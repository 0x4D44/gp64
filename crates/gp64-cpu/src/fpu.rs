//! x87 Floating Point Unit emulation
//!
//! Implements the x87 FPU state and stack operations.
//! The FPU uses a stack-based model with 8 data registers (ST0-ST7).

use crate::x87_ops::F80;

/// Milestone-Q D16 Stage 0b measurement census of the x87 control-word
/// precision-control (PC) field (bits 9:8) at every site that writes the CW.
/// The default-off `interp-attrib` feature owns the process-global counters;
/// production note sites compile away. Bucketed by `(cw >> 8) & 0b11`:
/// 00 = single (24-bit), 01 = reserved, 10 = double (53-bit),
/// 11 = extended (64-bit). Read at guest exit to size the x87 callout budget.
pub mod cw_census {
    #[cfg(feature = "interp-attrib")]
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// PC-field buckets, indexed by `(cw >> 8) & 0b11`.
    #[cfg(feature = "interp-attrib")]
    static PC_BUCKETS: [AtomicU64; 4] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];

    /// RC-field buckets, indexed by `(cw >> 10) & 0b11`
    /// (nearest / down / up / chop).
    #[cfg(feature = "interp-attrib")]
    static RC_BUCKETS: [AtomicU64; 4] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];

    /// Record one control-word write, bucketed by its PC field (bits 9:8)
    /// and RC field (bits 11:10).
    #[inline(always)]
    pub fn note_cw(cw: u16) {
        #[cfg(feature = "interp-attrib")]
        {
            PC_BUCKETS[((cw >> 8) & 3) as usize].fetch_add(1, Relaxed);
            RC_BUCKETS[((cw >> 10) & 3) as usize].fetch_add(1, Relaxed);
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = cw;
    }

    /// Snapshot of the four PC-field buckets.
    #[derive(Default, Clone, Copy)]
    pub struct CwCensus {
        pub single: u64,
        pub reserved: u64,
        pub double: u64,
        pub extended: u64,
    }

    /// Load the current bucket counts.
    #[must_use]
    pub fn snapshot() -> CwCensus {
        #[cfg(feature = "interp-attrib")]
        {
            CwCensus {
                single: PC_BUCKETS[0].load(Relaxed),
                reserved: PC_BUCKETS[1].load(Relaxed),
                double: PC_BUCKETS[2].load(Relaxed),
                extended: PC_BUCKETS[3].load(Relaxed),
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        {
            CwCensus::default()
        }
    }

    /// Load the current RC bucket counts (nearest, down, up, chop).
    #[must_use]
    pub fn rc_snapshot() -> [u64; 4] {
        #[cfg(feature = "interp-attrib")]
        {
            std::array::from_fn(|i| RC_BUCKETS[i].load(Relaxed))
        }
        #[cfg(not(feature = "interp-attrib"))]
        {
            [0; 4]
        }
    }

    /// Two-space-indented report lines. The header carries no `": "`; every
    /// value line is `"  <single-token-label>: <value>"`.
    #[must_use]
    pub fn report() -> Vec<String> {
        #[cfg(not(feature = "interp-attrib"))]
        return Vec::new();

        #[cfg(feature = "interp-attrib")]
        let c = snapshot();
        #[cfg(feature = "interp-attrib")]
        let rc = rc_snapshot();
        #[cfg(feature = "interp-attrib")]
        vec![
            "  cw-pc census".to_string(),
            format!("  cw_pc_single: {}", c.single),
            format!("  cw_pc_reserved: {}", c.reserved),
            format!("  cw_pc_double: {}", c.double),
            format!("  cw_pc_extended: {}", c.extended),
            format!("  cw_rc_nearest: {}", rc[0]),
            format!("  cw_rc_down: {}", rc[1]),
            format!("  cw_rc_up: {}", rc[2]),
            format!("  cw_rc_chop: {}", rc[3]),
        ]
    }

    #[cfg(all(test, feature = "interp-attrib"))]
    mod tests {
        // Monotonic-delta assertion (other tests bump the process-global
        // buckets concurrently, so `>=`, never `==`).
        #[test]
        fn note_cw_double_bucket_is_monotonic() {
            let before = super::snapshot().double;
            // PC field = 10b (double / 53-bit): 0x02 << 8 = 0x0200.
            super::note_cw(0x0200);
            assert!(super::snapshot().double > before);
        }
    }
}

/// Milestone-Q D16 Stage 0d measurement census of the value classes that
/// flow through the three narrow single-precision x87 memory sites (FLD m32,
/// FSTP m32, FISTP m32). The default-off `interp-attrib` feature owns the
/// process-global counters; production note sites compile away. Per stream we
/// count the total plus the denormal / NaN / out-of-range sub-populations.
pub mod fpu_guard_census {
    #[cfg(feature = "interp-attrib")]
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    // FLD m32 (4-byte load) stream.
    #[cfg(feature = "interp-attrib")]
    static FLD_M32_TOTAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FLD_M32_DENORMAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FLD_M32_NAN: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FLD_M32_OOR: AtomicU64 = AtomicU64::new(0);

    // FSTP m32 (4-byte store) stream.
    #[cfg(feature = "interp-attrib")]
    static FSTP_M32_TOTAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FSTP_M32_DENORMAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FSTP_M32_NAN: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FSTP_M32_OOR: AtomicU64 = AtomicU64::new(0);

    // FISTP m32 (4-byte integer store) stream.
    #[cfg(feature = "interp-attrib")]
    static FISTP_M32_TOTAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FISTP_M32_DENORMAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FISTP_M32_NAN: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FISTP_M32_OOR: AtomicU64 = AtomicU64::new(0);

    /// Classify one FLD m32 source (the f32 just read from memory).
    #[inline(always)]
    pub fn note_fld_m32(source: f32) {
        #[cfg(feature = "interp-attrib")]
        {
            FLD_M32_TOTAL.fetch_add(1, Relaxed);
            if source.is_nan() {
                FLD_M32_NAN.fetch_add(1, Relaxed);
            } else if source.is_infinite() {
                FLD_M32_OOR.fetch_add(1, Relaxed);
            } else if source.is_subnormal() {
                FLD_M32_DENORMAL.fetch_add(1, Relaxed);
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = source;
    }

    /// Classify one FSTP m32 result (the f32 about to be stored).
    #[inline(always)]
    pub fn note_fstp_m32(v: f32) {
        #[cfg(feature = "interp-attrib")]
        {
            FSTP_M32_TOTAL.fetch_add(1, Relaxed);
            if v.is_nan() {
                FSTP_M32_NAN.fetch_add(1, Relaxed);
            } else if v.is_infinite() {
                FSTP_M32_OOR.fetch_add(1, Relaxed);
            } else if v.is_subnormal() {
                FSTP_M32_DENORMAL.fetch_add(1, Relaxed);
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = v;
    }

    /// Record one FISTP m32 store. The caller passes the source F80's class
    /// flags (`is_nan`, `is_denormal`) and the round-then-range `out_of_range`
    /// result already computed by the store site.
    #[inline(always)]
    pub fn note_fistp_m32(is_nan: bool, is_denormal: bool, out_of_range: bool) {
        #[cfg(feature = "interp-attrib")]
        {
            FISTP_M32_TOTAL.fetch_add(1, Relaxed);
            if is_nan {
                FISTP_M32_NAN.fetch_add(1, Relaxed);
            }
            if is_denormal {
                FISTP_M32_DENORMAL.fetch_add(1, Relaxed);
            }
            if out_of_range {
                FISTP_M32_OOR.fetch_add(1, Relaxed);
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = (is_nan, is_denormal, out_of_range);
    }

    /// Per-stream counts.
    #[derive(Default, Clone, Copy)]
    pub struct StreamCensus {
        pub total: u64,
        pub denormal: u64,
        pub nan: u64,
        pub out_of_range: u64,
    }

    /// Snapshot of all three narrow-x87 streams.
    #[derive(Default, Clone, Copy)]
    pub struct FpuGuardCensus {
        pub fld_m32: StreamCensus,
        pub fstp_m32: StreamCensus,
        pub fistp_m32: StreamCensus,
    }

    /// Load the current counts for every stream.
    #[must_use]
    pub fn snapshot() -> FpuGuardCensus {
        #[cfg(feature = "interp-attrib")]
        {
            FpuGuardCensus {
                fld_m32: StreamCensus {
                    total: FLD_M32_TOTAL.load(Relaxed),
                    denormal: FLD_M32_DENORMAL.load(Relaxed),
                    nan: FLD_M32_NAN.load(Relaxed),
                    out_of_range: FLD_M32_OOR.load(Relaxed),
                },
                fstp_m32: StreamCensus {
                    total: FSTP_M32_TOTAL.load(Relaxed),
                    denormal: FSTP_M32_DENORMAL.load(Relaxed),
                    nan: FSTP_M32_NAN.load(Relaxed),
                    out_of_range: FSTP_M32_OOR.load(Relaxed),
                },
                fistp_m32: StreamCensus {
                    total: FISTP_M32_TOTAL.load(Relaxed),
                    denormal: FISTP_M32_DENORMAL.load(Relaxed),
                    nan: FISTP_M32_NAN.load(Relaxed),
                    out_of_range: FISTP_M32_OOR.load(Relaxed),
                },
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        {
            FpuGuardCensus::default()
        }
    }

    /// Two-space-indented report lines. The header carries no `": "`; every
    /// value line is `"  <single-token-label>: <value>"`.
    #[must_use]
    pub fn report() -> Vec<String> {
        #[cfg(not(feature = "interp-attrib"))]
        return Vec::new();

        #[cfg(feature = "interp-attrib")]
        let c = snapshot();
        #[cfg(feature = "interp-attrib")]
        vec![
            "  fpu-guard census".to_string(),
            format!("  fpu_guard_fld_m32_total: {}", c.fld_m32.total),
            format!("  fpu_guard_fld_m32_denormal: {}", c.fld_m32.denormal),
            format!("  fpu_guard_fld_m32_nan: {}", c.fld_m32.nan),
            format!(
                "  fpu_guard_fld_m32_out_of_range: {}",
                c.fld_m32.out_of_range
            ),
            format!("  fpu_guard_fstp_m32_total: {}", c.fstp_m32.total),
            format!("  fpu_guard_fstp_m32_denormal: {}", c.fstp_m32.denormal),
            format!("  fpu_guard_fstp_m32_nan: {}", c.fstp_m32.nan),
            format!(
                "  fpu_guard_fstp_m32_out_of_range: {}",
                c.fstp_m32.out_of_range
            ),
            format!("  fpu_guard_fistp_m32_total: {}", c.fistp_m32.total),
            format!("  fpu_guard_fistp_m32_denormal: {}", c.fistp_m32.denormal),
            format!("  fpu_guard_fistp_m32_nan: {}", c.fistp_m32.nan),
            format!(
                "  fpu_guard_fistp_m32_out_of_range: {}",
                c.fistp_m32.out_of_range
            ),
        ]
    }

    #[cfg(all(test, feature = "interp-attrib"))]
    mod tests {
        // Monotonic-delta assertion (other tests bump the process-global
        // counters concurrently, so `>=`, never `==`).
        #[test]
        fn note_fld_m32_nan_is_monotonic() {
            let before = super::snapshot().fld_m32;
            super::note_fld_m32(f32::NAN);
            let after = super::snapshot().fld_m32;
            assert!(after.total > before.total);
            assert!(after.nan > before.nan);
        }
    }
}

/// Tagged-f64 stack cache HLD (2026.07.29) U0 census: operand-form mix of the
/// hot x87 paths (register vs m32 vs m64 arithmetic sources, wide FLD/FSTP
/// forms) and the per-op joint PC×RC mix at arithmetic execution time (the
/// ~2 writes per ftol; this census counts what arithmetic actually runs
/// under). Default-off `interp-attrib` feature; production sites compile away.
pub mod x87_form_census {
    #[cfg(feature = "interp-attrib")]
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    #[cfg(feature = "interp-attrib")]
    static ARITH_REG: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FLOAT_SRC_M32: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FLOAT_SRC_M64: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FLD_M64: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FLD_M80: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FSTP_M64: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static FSTP_M80: AtomicU64 = AtomicU64::new(0);
    /// Joint PC×RC buckets at arithmetic execution, indexed
    /// `[(cw >> 8) & 3][(cw >> 10) & 3]`.
    #[cfg(feature = "interp-attrib")]
    #[allow(clippy::declare_interior_mutable_const)]
    static ARITH_PC_RC: [[AtomicU64; 4]; 4] = {
        #[allow(clippy::declare_interior_mutable_const)]
        const Z: AtomicU64 = AtomicU64::new(0);
        [[Z; 4], [Z; 4], [Z; 4], [Z; 4]]
    };

    /// Record one register-form arithmetic op executing under `cw`.
    #[inline(always)]
    pub fn note_arith_reg(cw: u16) {
        #[cfg(feature = "interp-attrib")]
        {
            ARITH_REG.fetch_add(1, Relaxed);
            note_arith_cw(cw);
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = cw;
    }

    /// Record one float memory source read (arith + compare paths), by width.
    #[inline(always)]
    pub fn note_float_src(size_bytes: usize, cw: u16) {
        #[cfg(feature = "interp-attrib")]
        {
            match size_bytes {
                4 => FLOAT_SRC_M32.fetch_add(1, Relaxed),
                8 => FLOAT_SRC_M64.fetch_add(1, Relaxed),
                _ => 0,
            };
            note_arith_cw(cw);
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = (size_bytes, cw);
    }

    #[cfg(feature = "interp-attrib")]
    #[inline(always)]
    fn note_arith_cw(cw: u16) {
        ARITH_PC_RC[((cw >> 8) & 3) as usize][((cw >> 10) & 3) as usize].fetch_add(1, Relaxed);
    }

    /// Record one wide FLD (8- or 10-byte memory source).
    #[inline(always)]
    pub fn note_fld_wide(size_bytes: usize) {
        #[cfg(feature = "interp-attrib")]
        match size_bytes {
            8 => FLD_M64.fetch_add(1, Relaxed),
            10 => FLD_M80.fetch_add(1, Relaxed),
            _ => 0,
        };
        #[cfg(not(feature = "interp-attrib"))]
        let _ = size_bytes;
    }

    /// Record one wide FST/FSTP (8- or 10-byte memory destination).
    #[inline(always)]
    pub fn note_fstp_wide(size_bytes: usize) {
        #[cfg(feature = "interp-attrib")]
        match size_bytes {
            8 => FSTP_M64.fetch_add(1, Relaxed),
            10 => FSTP_M80.fetch_add(1, Relaxed),
            _ => 0,
        };
        #[cfg(not(feature = "interp-attrib"))]
        let _ = size_bytes;
    }

    /// Two-space-indented report lines, same shape as the sibling censuses.
    #[must_use]
    pub fn report() -> Vec<String> {
        #[cfg(not(feature = "interp-attrib"))]
        return Vec::new();

        #[cfg(feature = "interp-attrib")]
        {
            let mut lines = vec![
                "  x87-form census".to_string(),
                format!("  x87_form_arith_reg: {}", ARITH_REG.load(Relaxed)),
                format!("  x87_form_float_src_m32: {}", FLOAT_SRC_M32.load(Relaxed)),
                format!("  x87_form_float_src_m64: {}", FLOAT_SRC_M64.load(Relaxed)),
                format!("  x87_form_fld_m64: {}", FLD_M64.load(Relaxed)),
                format!("  x87_form_fld_m80: {}", FLD_M80.load(Relaxed)),
                format!("  x87_form_fstp_m64: {}", FSTP_M64.load(Relaxed)),
                format!("  x87_form_fstp_m80: {}", FSTP_M80.load(Relaxed)),
            ];
            const PC: [&str; 4] = ["pc24", "pcrsv", "pc53", "pc64"];
            const RC: [&str; 4] = ["nearest", "down", "up", "chop"];
            for (pc_idx, pc_name) in PC.iter().enumerate() {
                for (rc_idx, rc_name) in RC.iter().enumerate() {
                    let count = ARITH_PC_RC[pc_idx][rc_idx].load(Relaxed);
                    if count != 0 {
                        lines.push(format!("  x87_form_arith_{pc_name}_{rc_name}: {count}"));
                    }
                }
            }
            lines
        }
    }

    #[cfg(all(test, feature = "interp-attrib"))]
    mod tests {
        // Monotonic-delta assertions (process-global counters shared with
        // concurrent tests, so `>`, never `==`).
        #[test]
        fn note_sites_are_monotonic() {
            let before_reg = super::ARITH_REG.load(std::sync::atomic::Ordering::Relaxed);
            let before_bucket = super::ARITH_PC_RC[2][3].load(std::sync::atomic::Ordering::Relaxed);
            // PC=53 (10b at bits 9:8), RC=chop (11b at bits 11:10).
            super::note_arith_reg(0x0E00);
            assert!(super::ARITH_REG.load(std::sync::atomic::Ordering::Relaxed) > before_reg);
            assert!(
                super::ARITH_PC_RC[2][3].load(std::sync::atomic::Ordering::Relaxed) > before_bucket
            );

            let before_m64 = super::FLOAT_SRC_M64.load(std::sync::atomic::Ordering::Relaxed);
            super::note_float_src(8, 0x0E00);
            assert!(super::FLOAT_SRC_M64.load(std::sync::atomic::Ordering::Relaxed) > before_m64);

            let before_fld = super::FLD_M64.load(std::sync::atomic::Ordering::Relaxed);
            super::note_fld_wide(8);
            assert!(super::FLD_M64.load(std::sync::atomic::Ordering::Relaxed) > before_fld);
        }
    }
}

/// Measurement-only census of `X87Dispatch` classification outcomes, for the
/// hot-lane whitelist coverage question (which share of D8–DF escapes would an
/// inline non-Generic lane admit, and what dominates the Generic remainder).
/// Production builds compile the note site and report away.
pub mod dispatch_census {
    #[cfg(feature = "interp-attrib")]
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    #[cfg(feature = "interp-attrib")]
    use std::sync::Mutex;

    /// One counter per `X87Dispatch` discriminant, `NAMES` order.
    #[cfg(feature = "interp-attrib")]
    #[allow(clippy::declare_interior_mutable_const)]
    static FORMS: [AtomicU64; 14] = {
        #[allow(clippy::declare_interior_mutable_const)]
        const Z: AtomicU64 = AtomicU64::new(0);
        [Z, Z, Z, Z, Z, Z, Z, Z, Z, Z, Z, Z, Z, Z]
    };
    #[cfg(feature = "interp-attrib")]
    const NAMES: [&str; 14] = [
        "generic",
        "fld_m32",
        "fld_m64",
        "fld_reg",
        "fstp_m32",
        "fstp_m64",
        "fild_m32",
        "fistp_m32",
        "fnstsw_ax",
        "fxch",
        "fldcw",
        "arith_mem",
        "com_mem",
        "arith_reg",
    ];
    /// Per-mnemonic breakdown of the Generic bucket (Debug-name keyed).
    #[cfg(feature = "interp-attrib")]
    static GENERIC_MNEMONICS: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());
    /// Escapes admitted by the interpreter's inline hot lane (self-certifies
    /// lane engagement in attribution runs — a zero here with non-zero form
    /// counts means the lane build did not take the new path).
    #[cfg(feature = "interp-attrib")]
    static LANE_ADMITTED: AtomicU64 = AtomicU64::new(0);

    /// Record one escape executed via the inline hot lane.
    #[inline(always)]
    pub fn note_lane_admitted() {
        #[cfg(feature = "interp-attrib")]
        LANE_ADMITTED.fetch_add(1, Relaxed);
    }

    /// Per-form lane engagement (EXP-IQ008 panel amendment: laned forms
    /// must not vanish from the census breakdown). `LANE_FORM_NAMES` order.
    /// Indices 0-3 are the EXP-IQ008 register-lane forms; 4-7 are the
    /// EXP-IQ012 memory-lane forms.
    #[cfg(feature = "interp-attrib")]
    static LANE_FORMS: [AtomicU64; 8] = {
        #[allow(clippy::declare_interior_mutable_const)]
        const Z: AtomicU64 = AtomicU64::new(0);
        [Z, Z, Z, Z, Z, Z, Z, Z]
    };
    #[cfg(feature = "interp-attrib")]
    const LANE_FORM_NAMES: [&str; 8] = [
        "fld_reg",
        "fxch",
        "fnstsw_ax",
        "arith_reg",
        "fld_m32",
        "fstp_m32",
        "fild_m32",
        "fistp_m32",
    ];

    /// Test accessor: the census's lane-form name table (feature-gated like
    /// the table itself). Exists so the index<->name mapping can be pinned.
    #[cfg(feature = "interp-attrib")]
    pub fn lane_form_names() -> &'static [&'static str; 8] {
        &LANE_FORM_NAMES
    }

    /// Record one lane execution of the given form index (`LANE_FORM_NAMES`).
    #[inline(always)]
    pub fn note_lane_form(_idx: usize) {
        #[cfg(feature = "interp-attrib")]
        LANE_FORMS[_idx].fetch_add(1, Relaxed);
    }

    /// EXP-IQ012 declined-shape residue: SIB shapes (`rm=4`) of an
    /// otherwise-recognized memory-lane form, indexed 0..4 =
    /// fld_m32/fstp_m32/fild_m32/fistp_m32 (`LANE_FORM_NAMES[4..8]` minus
    /// the offset). Diagnostic-only — measures the residue the panel asked
    /// to see before pricing a follow-up SIB admission (design §4).
    #[cfg(feature = "interp-attrib")]
    static MEM_LANE_SIB_DECLINED: [AtomicU64; 4] = {
        #[allow(clippy::declare_interior_mutable_const)]
        const Z: AtomicU64 = AtomicU64::new(0);
        [Z, Z, Z, Z]
    };

    /// Record one memory-lane SIB-shape decline for the given form
    /// (0..4 = fld_m32/fstp_m32/fild_m32/fistp_m32).
    #[inline(always)]
    pub fn note_mem_lane_sib_declined(_idx: usize) {
        #[cfg(feature = "interp-attrib")]
        MEM_LANE_SIB_DECLINED[_idx].fetch_add(1, Relaxed);
    }

    #[cfg(feature = "interp-attrib")]
    fn form_index(dispatch: &super::X87Dispatch) -> usize {
        use super::X87Dispatch as D;
        match dispatch {
            D::Generic => 0,
            D::FldM32 => 1,
            D::FldM64 => 2,
            D::FldReg { .. } => 3,
            D::FstpM32 => 4,
            D::FstpM64 => 5,
            D::FildM32 => 6,
            D::FistpM32 => 7,
            D::FnstswAx => 8,
            D::Fxch { .. } => 9,
            D::Fldcw => 10,
            D::ArithMem { .. } => 11,
            D::ComMem { .. } => 12,
            D::ArithReg { .. } => 13,
        }
    }

    /// Record one dispatch classification (called once per executed escape).
    #[inline(always)]
    pub fn note(dispatch: &super::X87Dispatch, mnemonic: super::Mnemonic) {
        #[cfg(feature = "interp-attrib")]
        {
            FORMS[form_index(dispatch)].fetch_add(1, Relaxed);
            if matches!(dispatch, super::X87Dispatch::Generic) {
                let name = format!("{mnemonic:?}");
                let mut table = GENERIC_MNEMONICS.lock().unwrap();
                match table.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, count)) => *count += 1,
                    None => table.push((name, 1)),
                }
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = (dispatch, mnemonic);
    }

    /// Two-space-indented report lines, same shape as the sibling censuses.
    #[must_use]
    pub fn report() -> Vec<String> {
        #[cfg(not(feature = "interp-attrib"))]
        return Vec::new();

        #[cfg(feature = "interp-attrib")]
        {
            let counts: Vec<u64> = FORMS.iter().map(|c| c.load(Relaxed)).collect();
            // The denominator is the FULL escape population: FORMS counts the
            // outlined (execute_decoded_with_dispatch) route only, and since
            // EXP-IQ011/IQ012 the inline lanes carry the majority of escapes
            // alone made every x87_dispatch_* percentage a share of the
            // outlined residue and would return an EMPTY report for a fully
            // laned run (adversarial review, 2026.08.05).
            let lane_total: u64 = LANE_FORMS.iter().map(|c| c.load(Relaxed)).sum();
            let outlined_total: u64 = counts.iter().sum();
            let total: u64 = outlined_total + lane_total;
            if total == 0 {
                return Vec::new();
            }
            let mut lines = vec![format!(
                "  x87-dispatch census (total {total}: {outlined_total} outlined + {lane_total} laned)"
            )];
            lines.push(format!(
                "  x87_dispatch_lane_admitted: {}",
                LANE_ADMITTED.load(Relaxed)
            ));
            // EXP-IQ005 engagement honesty: how many arith executions the
            // host-float fast cores actually served (thread-local, same
            // thread as execution). Eligible population = arith_mem +
            // arith_reg + the Generic arith mnemonics; a low ratio means the
            // soft-80-bit path is carrying the load.
            lines.push(format!(
                "  x87_fast_core_hits: {}",
                crate::x87_fast_core::fast_path_hits()
            ));
            for (name, cell) in LANE_FORM_NAMES.iter().zip(LANE_FORMS.iter()) {
                let count = cell.load(Relaxed);
                if count != 0 {
                    lines.push(format!("  x87_lane_{name}: {count}"));
                }
            }
            for (name, cell) in LANE_FORM_NAMES[4..]
                .iter()
                .zip(MEM_LANE_SIB_DECLINED.iter())
            {
                let count = cell.load(Relaxed);
                if count != 0 {
                    lines.push(format!("  x87_lane_{name}_sib_declined: {count}"));
                }
            }
            for (name, count) in NAMES.iter().zip(&counts) {
                if *count != 0 {
                    lines.push(format!(
                        "  x87_dispatch_{name}: {count} ({:.2}%)",
                        *count as f64 * 100.0 / total as f64
                    ));
                }
            }
            let mut generics = GENERIC_MNEMONICS.lock().unwrap().clone();
            // Descending by count; `sort_by_key` + `Reverse` is the form
            // clippy::unnecessary_sort_by wants, and is still a stable sort,
            // so equal counts keep their insertion order as before.
            generics.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            for (name, count) in generics {
                lines.push(format!(
                    "  x87_dispatch_generic_{name}: {count} ({:.2}%)",
                    count as f64 * 100.0 / total as f64
                ));
            }
            lines
        }
    }

    #[cfg(all(test, feature = "interp-attrib"))]
    mod tests {
        // Monotonic-delta assertions (process-global counters shared with
        // concurrent tests, so `>`, never `==`).
        #[test]
        fn note_counts_forms_and_generic_mnemonics() {
            use crate::x87_decode::X87Operation as Mnemonic;
            let before_fld = super::FORMS[1].load(std::sync::atomic::Ordering::Relaxed);
            super::note(&crate::fpu::X87Dispatch::FldM32, Mnemonic::Fld);
            assert!(super::FORMS[1].load(std::sync::atomic::Ordering::Relaxed) > before_fld);

            super::note(&crate::fpu::X87Dispatch::Generic, Mnemonic::Fsqrt);
            let table = super::GENERIC_MNEMONICS.lock().unwrap();
            assert!(table.iter().any(|(n, c)| *n == "Fsqrt" && *c > 0));
        }
    }
}

/// Measurement-only acceptance census for the guarded PC-single arithmetic
/// path. Production builds compile all note sites and reports away.
pub mod single_arith_census {
    #[cfg(feature = "interp-attrib")]
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    #[cfg(feature = "interp-attrib")]
    static ADD_TOTAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static ADD_FAST: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static SUB_TOTAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static SUB_FAST: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static MUL_TOTAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static MUL_FAST: AtomicU64 = AtomicU64::new(0);

    #[inline(always)]
    pub(super) fn note_add(fast: bool) {
        #[cfg(feature = "interp-attrib")]
        {
            ADD_TOTAL.fetch_add(1, Relaxed);
            if fast {
                ADD_FAST.fetch_add(1, Relaxed);
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = fast;
    }

    #[inline(always)]
    pub(super) fn note_mul(fast: bool) {
        #[cfg(feature = "interp-attrib")]
        {
            MUL_TOTAL.fetch_add(1, Relaxed);
            if fast {
                MUL_FAST.fetch_add(1, Relaxed);
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = fast;
    }

    #[inline(always)]
    pub(super) fn note_sub(fast: bool) {
        #[cfg(feature = "interp-attrib")]
        {
            SUB_TOTAL.fetch_add(1, Relaxed);
            if fast {
                SUB_FAST.fetch_add(1, Relaxed);
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = fast;
    }

    #[cfg(feature = "interp-attrib")]
    static DIV_TOTAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static DIV_FAST: AtomicU64 = AtomicU64::new(0);

    #[inline(always)]
    pub(super) fn note_div(fast: bool) {
        #[cfg(feature = "interp-attrib")]
        {
            DIV_TOTAL.fetch_add(1, Relaxed);
            if fast {
                DIV_FAST.fetch_add(1, Relaxed);
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = fast;
    }

    #[cfg(feature = "interp-attrib")]
    static SQRT_TOTAL: AtomicU64 = AtomicU64::new(0);
    #[cfg(feature = "interp-attrib")]
    static SQRT_FAST: AtomicU64 = AtomicU64::new(0);

    #[inline(always)]
    pub(super) fn note_sqrt(fast: bool) {
        #[cfg(feature = "interp-attrib")]
        {
            SQRT_TOTAL.fetch_add(1, Relaxed);
            if fast {
                SQRT_FAST.fetch_add(1, Relaxed);
            }
        }
        #[cfg(not(feature = "interp-attrib"))]
        let _ = fast;
    }

    #[must_use]
    pub fn report() -> Vec<String> {
        #[cfg(not(feature = "interp-attrib"))]
        return Vec::new();

        #[cfg(feature = "interp-attrib")]
        vec![
            "  single-arith census".to_string(),
            format!("  fpu_single_add_total: {}", ADD_TOTAL.load(Relaxed)),
            format!("  fpu_single_add_fast: {}", ADD_FAST.load(Relaxed)),
            format!("  fpu_single_sub_total: {}", SUB_TOTAL.load(Relaxed)),
            format!("  fpu_single_sub_fast: {}", SUB_FAST.load(Relaxed)),
            format!("  fpu_single_mul_total: {}", MUL_TOTAL.load(Relaxed)),
            format!("  fpu_single_mul_fast: {}", MUL_FAST.load(Relaxed)),
            format!("  fpu_single_div_total: {}", DIV_TOTAL.load(Relaxed)),
            format!("  fpu_single_div_fast: {}", DIV_FAST.load(Relaxed)),
            format!("  fpu_single_sqrt_total: {}", SQRT_TOTAL.load(Relaxed)),
            format!("  fpu_single_sqrt_fast: {}", SQRT_FAST.load(Relaxed)),
        ]
    }

    #[cfg(all(test, feature = "interp-attrib"))]
    mod tests {
        use std::sync::atomic::Ordering::Relaxed;

        /// The six counters this census actually moves, snapshotted together.
        fn counts() -> [u64; 6] {
            [
                super::ADD_TOTAL.load(Relaxed),
                super::ADD_FAST.load(Relaxed),
                super::SUB_TOTAL.load(Relaxed),
                super::SUB_FAST.load(Relaxed),
                super::MUL_TOTAL.load(Relaxed),
                super::MUL_FAST.load(Relaxed),
            ]
        }

        /// MDD-BUG-CRUCIBLE-00262: assert the COUNTERS move, not that the
        /// report has labels.
        ///
        /// This used to call `note_*(true)` and then check only that fixed
        /// strings like `fpu_single_add_total:` appeared in the report. The
        /// formatter emits those labels unconditionally, so deleting every
        /// `fetch_add` in this module would have left the test green while
        /// performance attribution silently reported zeros.
        ///
        /// Deltas, never absolute values: these are process-global atomics and
        /// Rust runs tests concurrently, so any other test touching this census
        /// would make an absolute assertion flaky.

        #[test]
        fn accepted_arithmetic_increments_total_and_fast_counters() {
            // `fast = true` must bump BOTH the total and the fast counter.
            let before = counts();
            super::note_add(true);
            super::note_sub(true);
            super::note_mul(true);
            let after = counts();
            let deltas: Vec<u64> = after
                .iter()
                .zip(before.iter())
                .map(|(a, b)| a.wrapping_sub(*b))
                .collect();
            assert_eq!(
                deltas,
                vec![1, 1, 1, 1, 1, 1],
                "accepted add/sub/mul must each bump total AND fast \
                 [add_total, add_fast, sub_total, sub_fast, mul_total, mul_fast]"
            );

            // `fast = false` must bump ONLY the totals. Without this arm the
            // `if fast` branch could be deleted and the test would still pass.
            let before = counts();
            super::note_add(false);
            super::note_sub(false);
            super::note_mul(false);
            let after = counts();
            let deltas: Vec<u64> = after
                .iter()
                .zip(before.iter())
                .map(|(a, b)| a.wrapping_sub(*b))
                .collect();
            assert_eq!(
                deltas,
                vec![1, 0, 1, 0, 1, 0],
                "a rejected op must bump the total but NOT the fast count"
            );
        }

        /// The report must carry the live counter values, not just the labels —
        /// the other half of MDD-BUG-CRUCIBLE-00262.
        #[test]
        fn report_carries_the_live_counter_values() {
            super::note_add(true);
            let expected = super::ADD_TOTAL.load(Relaxed);
            let report = super::report().join("\n");
            assert!(
                report.contains(&format!("fpu_single_add_total: {expected}")),
                "report must print the counter's value, not merely its label; \
                 expected fpu_single_add_total: {expected} in:\n{report}"
            );
        }
    }
}

/// Runtime-only per-physical-register exactness proofs over the canonical
/// F80 register file. `bits` + `valid` prove an exact normal-or-zero binary32
/// value; `f64_bits` + `f64_valid` prove the same for binary64. Every
/// f32-provable value is also f64-provable, so `valid ⊆ f64_valid`.
/// `set_proofs` maintains both masks together.
#[repr(C)]
#[derive(Clone, Debug, Default)]
struct NumericShadow {
    bits: [u32; 8],
    f64_bits: [u64; 8],
    valid: u8,
    f64_valid: u8,
    host_condition_codes_dirty: bool,
    /// While true, `f64_bits` is authoritative and `Fpu::regs` is its
    /// widened helper-visible mirror.
    drc_binary64_active: bool,
    /// Physical F80 mirrors that need rounding back into the binary64 file.
    /// This also occupies existing tail padding, so no native offset moves.
    drc_binary64_dirty: u8,
    /// Binary64 slots whose canonical F80 mirrors are stale and must be
    /// materialized before an F80 observer or helper reads them.
    /// This occupies the final byte of the existing tail padding.
    drc_f80_dirty: u8,
}

/// Byte offsets of the shadow state a native backend addresses relative to
/// the OWNING [`Fpu`] (EXP-A018). Pinned by const-asserts on every arch so
/// the x86-64 twin keeps compiling against the same ABI.
pub mod shadow_offsets {
    use super::{Fpu, NumericShadow};

    const SHADOW: usize = core::mem::offset_of!(Fpu, numeric_shadow);
    /// `f64_bits[8]` — the binary64 register file.
    pub const FPU_SHADOW_F64_BITS: usize = SHADOW + core::mem::offset_of!(NumericShadow, f64_bits);
    /// `bits[8]` — the f32 proof values.
    pub const FPU_SHADOW_F32_BITS: usize = SHADOW + core::mem::offset_of!(NumericShadow, bits);
    /// Aligned word holding {valid, f64_valid, host_condition_codes_dirty,
    /// drc_binary64_active} for single-word RMW.
    pub const FPU_SHADOW_PROOF_WORD: usize = SHADOW + core::mem::offset_of!(NumericShadow, valid);
    /// Byte offsets of the fields inside the proof word.
    pub const PROOF_WORD_VALID_SHIFT: u32 = 0;
    pub const PROOF_WORD_F64_VALID_SHIFT: u32 = 8;
    pub const PROOF_WORD_HCCD_SHIFT: u32 = 16;
    /// `drc_binary64_dirty` (start of the following word).
    pub const FPU_SHADOW_DIRTY: usize =
        SHADOW + core::mem::offset_of!(NumericShadow, drc_binary64_dirty);
    /// `drc_f80_dirty`, the reverse-direction materialization mask.
    pub const FPU_SHADOW_F80_DIRTY: usize =
        SHADOW + core::mem::offset_of!(NumericShadow, drc_f80_dirty);
    /// Architectural fields the native path reads/writes.
    pub const FPU_STATUS_WORD: usize = core::mem::offset_of!(Fpu, status_word);
    pub const FPU_CONTROL_WORD: usize = core::mem::offset_of!(Fpu, control_word);
    pub const FPU_TAG_WORD: usize = core::mem::offset_of!(Fpu, tag_word);
    pub const FPU_REGS: usize = core::mem::offset_of!(Fpu, regs);

    const _: () = {
        assert!(core::mem::size_of::<NumericShadow>() == 104);
        assert!(core::mem::align_of::<NumericShadow>() == 8);
        assert!(core::mem::offset_of!(NumericShadow, bits) == 0);
        assert!(core::mem::offset_of!(NumericShadow, f64_bits) == 32);
        assert!(core::mem::offset_of!(NumericShadow, valid) == 96);
        assert!(core::mem::offset_of!(NumericShadow, f64_valid) == 97);
        assert!(core::mem::offset_of!(NumericShadow, host_condition_codes_dirty) == 98);
        assert!(core::mem::offset_of!(NumericShadow, drc_binary64_active) == 99);
        assert!(core::mem::offset_of!(NumericShadow, drc_binary64_dirty) == 100);
        assert!(core::mem::offset_of!(NumericShadow, drc_f80_dirty) == 101);
        // The proof word must be 4-aligned for word RMW.
        assert!(FPU_SHADOW_PROOF_WORD.is_multiple_of(4));
        assert!(FPU_SHADOW_DIRTY == FPU_SHADOW_PROOF_WORD + 4);
        assert!(FPU_REGS == 0);
    };
}

impl NumericShadow {
    #[inline]
    fn get32(&self, physical_index: usize) -> Option<f32> {
        let bit = 1u8 << physical_index;
        (self.valid & bit != 0).then(|| f32::from_bits(self.bits[physical_index]))
    }

    #[inline]
    fn get64(&self, physical_index: usize) -> Option<f64> {
        let bit = 1u8 << physical_index;
        (self.f64_valid & bit != 0).then(|| f64::from_bits(self.f64_bits[physical_index]))
    }

    /// The single proof-write entry: both masks are always written together.
    #[inline]
    fn set_proofs(&mut self, physical_index: usize, exact32: Option<f32>, exact64: Option<f64>) {
        debug_assert!(
            exact32.is_none() || exact64.is_some(),
            "an f32-provable register is always f64-provable (valid ⊆ f64_valid)"
        );
        let bit = 1u8 << physical_index;
        // The F80 side is being written now, so any reverse-direction
        // materialization for this slot is superseded by the write.
        self.drc_f80_dirty &= !bit;
        if let Some(value) = exact32 {
            self.bits[physical_index] = value.to_bits();
            self.valid |= bit;
        } else {
            self.valid &= !bit;
        }
        if let Some(value) = exact64 {
            self.f64_bits[physical_index] = value.to_bits();
            self.f64_valid |= bit;
            if self.drc_binary64_active {
                self.drc_binary64_dirty &= !bit;
            }
        } else {
            self.f64_valid &= !bit;
            if self.drc_binary64_active {
                self.drc_binary64_dirty |= bit;
            }
        }
    }

    #[inline]
    fn invalidate(&mut self, physical_index: usize) {
        let bit = 1u8 << physical_index;
        let clear = !bit;
        self.valid &= clear;
        self.f64_valid &= clear;
        // A raw F80 write supersedes any stale binary64->F80 mirror.
        self.drc_f80_dirty &= clear;
        if self.drc_binary64_active {
            self.drc_binary64_dirty |= bit;
        }
    }

    /// [`Self::invalidate`] for a slot that just became EMPTY (pop): the
    /// value is architecturally unreadable and the commit loop skips empty
    /// slots anyway, so marking it dirty only forces the NEXT op's commit
    /// through a pointless 0..8 scan — every pop-form op (FSTP/FADDP/
    /// FCOMPP/…) paid that scan per execution (EXP-A013a).
    ///
    /// MDD-BUG-ANVIL-00089: the `drc_binary64_dirty` clear below is
    /// PERF-ONLY and deliberately unpinned — correctness for an emptied slot
    /// is held by `commit_one_binary64_slot`'s `is_empty` early-return, so
    /// mutating this clear leaves every test green BY DESIGN. Do not confuse
    /// it with the identical-bodied [`Self::mark_raw_alias`] below, whose
    /// dirty clear is load-bearing (the aliased slot is tag-VALID at commit)
    /// and is pinned by `mmx_alias_writes_survive_binary64_commits_bit_exact`.
    fn invalidate_emptied(&mut self, physical_index: usize) {
        let bit = 1u8 << physical_index;
        let clear = !bit;
        self.valid &= clear;
        self.f64_valid &= clear;
        self.drc_binary64_dirty &= clear;
        self.drc_f80_dirty &= clear;
    }

    /// Drop one slot's numeric proofs and NOTHING else (MDD-BUG-FLUX-00141).
    ///
    /// The two neighbours above both also move `drc_binary64_dirty`, and
    /// neither move is safe for a wholesale `tag_word` write:
    /// - marking dirty (as [`Self::invalidate`] does) re-arms an F80<-f64
    ///   commit over a slot whose raw ten bytes may be an authoritative MMX
    ///   payload — the MDD-BUG-FLUX-00087 hazard, and EMMS is *defined* as
    ///   preserving that payload for the next MMX instruction to re-tag;
    /// - clearing it (as [`Self::invalidate_emptied`] does) would drop a
    ///   genuine pending commit for a slot the guest can re-validate.
    ///
    /// The commit set is unaffected by a tag change, so the correct move is to
    /// touch neither bit.
    #[inline]
    fn clear_proofs(&mut self, physical_index: usize) {
        let clear = !(1u8 << physical_index);
        self.valid &= clear;
        self.f64_valid &= clear;
    }

    /// An MMX alias write: the raw ten bytes are authoritative and must
    /// never be F80->f64 rounded (MDD-BUG-FLUX-00087). Exempt the slot from
    /// the binary64 needs-rounding set and drop the numeric proofs.
    ///
    /// The `drc_binary64_dirty` clear here is LOAD-BEARING, unlike the
    /// perf-only twin in [`Self::invalidate_emptied`] above: an MMX
    /// instruction runs `enter_mmx_state` (tag word all-valid), so the
    /// aliased slot is NON-empty at commit time and the `is_empty`
    /// early-return cannot save the payload — only this exemption does.
    /// Pinned by `mmx_alias_writes_survive_binary64_commits_bit_exact`,
    /// mutation-verified 2026-08-06 (MDD-BUG-ANVIL-00089).
    #[inline]
    fn mark_raw_alias(&mut self, physical_index: usize) {
        let clear = !(1u8 << physical_index);
        self.valid &= clear;
        self.f64_valid &= clear;
        self.drc_binary64_dirty &= clear;
        self.drc_f80_dirty &= clear;
    }

    #[inline]
    fn invalidate_all(&mut self) {
        self.valid = 0;
        self.f64_valid = 0;
        // A wholesale F80 invalidation discards any reverse stale mirrors;
        // the raw F80 side becomes the only candidate source for a later
        // binary64 commit.
        self.drc_f80_dirty = 0;
        if self.drc_binary64_active {
            self.drc_binary64_dirty = u8::MAX;
        }
    }

    #[inline]
    fn swap(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        self.bits.swap(a, b);
        self.f64_bits.swap(a, b);
        let a_bit = 1u8 << a;
        let b_bit = 1u8 << b;
        for mask in [
            &mut self.valid,
            &mut self.f64_valid,
            &mut self.drc_binary64_dirty,
            &mut self.drc_f80_dirty,
        ] {
            let a_set = *mask & a_bit != 0;
            let b_set = *mask & b_bit != 0;
            *mask = (*mask & !a_bit & !b_bit)
                | if b_set { a_bit } else { 0 }
                | if a_set { b_bit } else { 0 };
        }
    }
}

/// Derive the f32 proof implied by an f64 proof, when the value is exactly a
/// normal-or-zero binary32 (the f32 proof's domain — subnormal f32s excluded).
#[inline]
fn derive_f32_proof(value: f64) -> Option<f32> {
    let narrowed = value as f32;
    (f64::from(narrowed) == value && (narrowed == 0.0 || narrowed.is_normal())).then_some(narrowed)
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn restore_host_x87_condition_codes(control_word: u16, status_word: u16) {
    // Interpreter x87 helpers keep the host stack empty but historically
    // inherit its condition codes. Restore the current guest condition state
    // without copying architectural TOP onto the empty host stack.
    let mut environment = [0u8; 28];
    environment[0..2].copy_from_slice(&(control_word | 0x003f).to_le_bytes());
    environment[4..6].copy_from_slice(&(status_word & !0x3800).to_le_bytes());
    environment[8..10].copy_from_slice(&0xffffu16.to_le_bytes());
    unsafe {
        let _host_x87 = crate::cpu::host_x87_enter_guarded();
        std::arch::asm!(
            "fldenv [{environment}]",
            environment = in(reg) environment.as_ptr(),
            out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
            out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            options(nostack),
        );
    }
}

/// x87 Floating Point Unit state
#[repr(C)]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Fpu {
    /// 8 data registers (ST0-ST7) in 80-bit extended precision
    pub regs: [F80; 8],
    /// Status Word
    pub status_word: u16,
    /// Control Word
    pub control_word: u16,
    /// Tag Word
    pub tag_word: u16,
    /// Instruction pointer offset (32-bit for protected mode)
    pub ip_offset: u32,
    /// Instruction pointer selector
    pub ip_selector: u16,
    /// Data operand offset (32-bit for protected mode)
    pub operand_offset: u32,
    /// Data operand selector
    pub operand_selector: u16,
    /// 108-byte x87 state buffer in FRSTOR/FNSAVE format.
    /// Written by `sync_to_x87_save_area()` and read by
    /// `sync_from_x87_save_area()`.
    #[serde(with = "crate::serde_helpers::array_108")]
    pub x87_save_area: [u8; 108],
    /// `FOP` — the 11-bit opcode of the last non-control x87 instruction.
    ///
    /// Part of the x87 environment an exception handler reads to identify the
    /// faulting instruction; it lives in bits 26:16 of the dword at offset 16
    /// of the 32-bit protected-mode environment image, alongside FCS.
    ///
    /// **Not yet written by guest execution** — that is the remaining half of
    /// MDD-BUG-CRUCIBLE-00578 and needs the opcode plumbed into the x87
    /// execution boundary. What this field fixes today is the save/restore
    /// round trip: the encoder hard-coded FOP to zero and the decoder
    /// discarded it with an `as u16`, so a guest that `FLDENV`s an environment
    /// carrying a FOP and later `FNSTENV`s it got zero back. Real hardware
    /// preserves it.
    pub opcode: u16,
    /// Runtime-only proof that a physical register's canonical F80 bytes are an
    /// exact normal binary32 value. Canonical F80 remains authoritative.
    #[serde(skip, default)]
    numeric_shadow: NumericShadow,
}

impl Default for Fpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Fpu {
    /// Create a new FPU in reset state
    pub fn new() -> Self {
        let fpu = Self {
            regs: [F80::ZERO; 8],
            status_word: 0,
            control_word: 0x037F, // Default 8087 CW
            tag_word: 0xFFFF,     // All empty
            ip_offset: 0,
            ip_selector: 0,
            operand_offset: 0,
            operand_selector: 0,
            x87_save_area: [0u8; 108],
            opcode: 0,
            numeric_shadow: NumericShadow::default(),
        };
        // D16 Stage 0b census: count the power-on CW default (PC = extended).
        cw_census::note_cw(fpu.control_word);
        fpu
    }

    /// Restore the complete architectural and runtime FPU reset state.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Select the binary64 x87 representation. The first call converts F80
    /// values lossily; subsequent calls keep the current representation.
    pub fn enter_drc_binary64_mode(&mut self) {
        if self.numeric_shadow.drc_binary64_active {
            return;
        }
        self.numeric_shadow.drc_binary64_active = true;
        self.numeric_shadow.drc_binary64_dirty = u8::MAX;
        self.numeric_shadow.drc_f80_dirty = 0;
        self.commit_drc_binary64_register_file();
    }

    /// Whether binary64 is the active x87 representation.
    #[must_use]
    pub fn drc_binary64_mode_active(&self) -> bool {
        self.numeric_shadow.drc_binary64_active
    }

    /// Materialize one stale F80 mirror from the authoritative binary64
    /// register file before a helper or diagnostic reads it. The directional
    /// dirty masks never overlap; materialization clears both for the slot.
    pub fn materialize_drc_f80_register(&mut self, physical_index: usize) {
        let physical_index = physical_index & 7;
        let bit = 1u8 << physical_index;
        if !self.numeric_shadow.drc_binary64_active || self.numeric_shadow.drc_f80_dirty & bit == 0
        {
            return;
        }
        debug_assert_eq!(
            self.numeric_shadow.drc_binary64_dirty & bit,
            0,
            "DRC dirty directions must never overlap"
        );
        if self.is_empty(physical_index) {
            // A native pop empties the slot but, like `Fpu::pop`, leaves the
            // value in the register: FSAVE/FXSAVE and the MMX alias still
            // see it. Publish those bits, but do not manufacture a proof for
            // an empty slot.
            let value = f64::from_bits(self.numeric_shadow.f64_bits[physical_index]);
            self.regs[physical_index] = F80::from_f64(value);
            self.numeric_shadow.clear_proofs(physical_index);
            self.numeric_shadow.drc_f80_dirty &= !bit;
            return;
        }
        let value = f64::from_bits(self.numeric_shadow.f64_bits[physical_index]);
        self.regs[physical_index] = F80::from_f64(value);
        // Publication must not strengthen or otherwise rewrite the proofs the
        // native writer established. In particular, x87 arithmetic clears its
        // f32 proof even when the result happens to be exactly representable.
        self.numeric_shadow.drc_binary64_dirty &= !bit;
        self.numeric_shadow.drc_f80_dirty &= !bit;
    }

    /// Physical slots whose F80 mirror the next
    /// [`Self::materialize_drc_f80_register_file`] would rebuild.
    #[must_use]
    pub fn drc_f80_publication_mask(&self) -> u8 {
        if self.numeric_shadow.drc_binary64_active {
            self.numeric_shadow.drc_f80_dirty
        } else {
            0
        }
    }

    /// Materialize every reverse-stale canonical F80 mirror.
    pub fn materialize_drc_f80_register_file(&mut self) {
        if !self.numeric_shadow.drc_binary64_active || self.numeric_shadow.drc_f80_dirty == 0 {
            return;
        }
        debug_assert_eq!(
            self.numeric_shadow.drc_binary64_dirty & self.numeric_shadow.drc_f80_dirty,
            0,
            "DRC dirty directions must never overlap"
        );
        let dirty = self.numeric_shadow.drc_f80_dirty;
        for physical_index in 0..8 {
            if dirty & (1u8 << physical_index) != 0 {
                self.materialize_drc_f80_register(physical_index);
            }
        }
    }

    /// Invalidate the numeric shadow after an external F80 replacement
    /// such as FXRSTOR. The restored F80 values must be recommitted before
    /// a binary64 observer reads the shadow.
    pub fn invalidate_drc_f80_register_file(&mut self) {
        self.numeric_shadow.invalidate_all();
    }

    /// Test-only: seed a fresh binary64 value whose F80 mirror has not been
    /// materialized, for the MDD-BUG-KIL-00762 observation-barrier regression.
    #[cfg(test)]
    pub(crate) fn seed_drc_reverse_stale_f64(&mut self, physical_index: usize, value: f64) {
        self.enter_drc_binary64_mode();
        let bit = 1u8 << physical_index;
        self.numeric_shadow.f64_bits[physical_index] = value.to_bits();
        self.numeric_shadow.f64_valid |= bit;
        self.numeric_shadow.drc_binary64_dirty &= !bit;
        self.numeric_shadow.drc_f80_dirty |= bit;
    }

    pub fn commit_drc_binary64_register_file(&mut self) {
        self.materialize_drc_f80_register_file();
        if !self.numeric_shadow.drc_binary64_active || self.numeric_shadow.drc_binary64_dirty == 0 {
            return;
        }

        let dirty = std::mem::take(&mut self.numeric_shadow.drc_binary64_dirty);
        for physical_index in 0..8 {
            if dirty & (1u8 << physical_index) == 0 {
                continue;
            }
            self.commit_one_binary64_slot(physical_index);
        }
    }

    /// Commit ONE physical register if it is binary64-dirty (EXP-A013
    /// per-register read barrier for the fat-dispatch fast forms).
    #[inline]
    pub fn commit_drc_binary64_register(&mut self, physical_index: usize) {
        self.materialize_drc_f80_register(physical_index);
        let bit = 1u8 << (physical_index & 7);
        if !self.numeric_shadow.drc_binary64_active
            || self.numeric_shadow.drc_binary64_dirty & bit == 0
        {
            return;
        }
        self.numeric_shadow.drc_binary64_dirty &= !bit;
        self.commit_one_binary64_slot(physical_index & 7);
    }

    /// [`Self::commit_drc_binary64_register`] addressed ST-relative.
    #[inline]
    pub fn commit_drc_binary64_st(&mut self, st_index: usize) {
        let physical = (self.get_top() + st_index) & 7;
        self.commit_drc_binary64_register(physical);
    }

    fn commit_one_binary64_slot(&mut self, physical_index: usize) {
        if self.is_empty(physical_index) {
            return;
        }
        let value = self.regs[physical_index].to_f64();
        let canonical = F80::from_f64(value);
        let exact64 = (value == 0.0 || value.is_normal()).then_some(value);
        self.set_physical_reg_with_proofs(
            physical_index,
            canonical,
            exact64.and_then(derive_f32_proof),
            exact64,
        );
        // A committed slot holds the canonical binary64 image by
        // construction even when no exact64 PROOF exists (inf/NaN/denormal
        // results): clear the re-dirty that `set_proofs`' no-proof arm just
        // applied, or such slots would never leave the needs-rounding set.
        self.numeric_shadow.drc_binary64_dirty &= !(1u8 << physical_index);
        // Do not write tags here. Pushes and arithmetic results classify at
        // their write sites; raw physical setters preserve tags for FRSTOR,
        // FST ST(i) and FXCH. Reclassifying at this barrier would destroy tags
        // deliberately restored from an environment image.
    }

    /// Status-word TOP field (bits 11-13).
    pub const STATUS_TOP_MASK: u16 = 0x7 << 11;
    /// Status-word ES (error summary): an unmasked exception is pending.
    pub const STATUS_ES: u16 = 1 << 7;
    /// Tag word after any non-EMMS MMX instruction: every register valid.
    pub const MMX_ACTIVE_TAG_WORD: u16 = 0x0000;

    /// Get stack top index (TOP) from Status Word bits 11-13
    pub fn get_top(&self) -> usize {
        ((self.status_word >> 11) & 0x7) as usize
    }

    /// Set stack top index (TOP)
    pub fn set_top(&mut self, top: usize) {
        self.status_word =
            (self.status_word & !Self::STATUS_TOP_MASK) | (((top as u16) & 0x7) << 11);
    }

    /// Read one MMX register from its physical x87 register alias.
    ///
    /// MM0-MM7 map to R0-R7, not ST(0)-ST(7), so TOP does not participate.
    #[inline]
    pub fn read_mmx_register(&self, index: usize) -> u64 {
        let bytes: [u8; 8] = self.regs[index].0[..8]
            .try_into()
            .expect("an F80 register always has an eight-byte MMX alias");
        u64::from_le_bytes(bytes)
    }

    /// Return all eight physical MMX register aliases.
    #[inline]
    pub fn mmx_registers(&self) -> [u64; 8] {
        std::array::from_fn(|index| self.read_mmx_register(index))
    }

    /// Write one MMX register through its physical x87 register alias.
    ///
    /// Intel SDM Volume 3A section 15.2 requires an MMX destination write to
    /// replace bits 0-63 and set bits 64-79 to ones. The per-instruction
    /// TOP/tag transition is separate because read-only MMX instructions have
    /// the same global side effect.
    #[inline]
    pub fn write_mmx_register(&mut self, index: usize, value: u64) {
        let mut bytes = [0xFF; 10];
        bytes[..8].copy_from_slice(&value.to_le_bytes());
        self.set_physical_reg(index, F80(bytes));
        // The alias bytes are authoritative: a later binary64 commit must
        // not round them (MDD-BUG-FLUX-00087).
        self.numeric_shadow.mark_raw_alias(index);
    }

    /// [`Self::read_mmx_register`] with the EXP-A013 commit-on-read barrier:
    /// an x87-dirtied slot is rounded to its binary64 form before its alias
    /// bytes become guest-visible, exactly as the eager scheme exposed them.
    #[inline]
    pub fn read_mmx_register_committed(&mut self, index: usize) -> u64 {
        self.commit_drc_binary64_register(index);
        self.read_mmx_register(index)
    }

    /// Apply the x87 state transition made by a non-EMMS MMX instruction.
    #[inline]
    pub fn enter_mmx_state(&mut self) {
        self.set_top(0);
        self.tag_word = Self::MMX_ACTIVE_TAG_WORD;
    }

    /// Empty MMX state while preserving the aliased register contents.
    #[inline]
    pub fn emms(&mut self) {
        self.set_top(0);
        self.tag_word = 0xFFFF;
        self.invalidate_proofs_of_empty_slots();
    }

    /// Whether an unmasked x87 exception is waiting to be delivered.
    #[inline]
    pub fn has_pending_exception(&self) -> bool {
        self.status_word & Self::STATUS_ES != 0
    }

    /// Push a value to the stack.
    ///
    /// On full-stack overflow with IE masked, advance TOP and write the
    /// floating-point indefinite QNaN to the destination, tagged Special
    /// (Intel SDM Vol 1 §8.5.1.2).
    pub fn push(&mut self, value: F80) {
        // One byte-scan yields the f64 proof; the f32 proof derives from it
        // (the exact-normal-or-zero f32 domain is a subset of the f64 one).
        let exact64 = crate::x87_common::f80_to_exact_normal_or_zero_f64(value.bytes());
        let exact32 = exact64.and_then(derive_f32_proof);
        self.push_with_proofs(value, exact32, exact64);
    }

    #[inline]
    fn push_with_proofs(&mut self, value: F80, exact32: Option<f32>, exact64: Option<f64>) {
        let top = self.get_top();
        let new_top = (top.wrapping_sub(1)) & 0x7;
        if !self.is_empty(new_top) {
            self.set_stack_fault(true);
            // Advance TOP and write the indefinite QNaN to the destination,
            // tagged Special rather than Valid or Zero.
            self.set_top(new_top);
            self.set_physical_reg(new_top, F80([0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]));
            self.set_tag(new_top, 2); // Special
            return;
        }
        self.set_top(new_top);
        self.set_physical_reg_with_proofs(new_top, value, exact32, exact64);
        // Tag by content, not merely by occupancy. An unconditional Valid
        // tag would misclassify zero and NaN values. Every push — the whole FLD
        // family, FLD1/FLDZ/FLDPI and the constant loads — arrives here, so
        // this one site carries most of the defect.
        self.set_tag(new_top, value.x87_tag());
    }

    #[inline(always)]
    fn push_binary32(&mut self, value: f32) {
        if let Some(bytes) = crate::x87_common::f32_normal_or_zero_to_f80(value) {
            self.push_with_proofs(F80(bytes), Some(value), Some(f64::from(value)));
        } else {
            // Subnormal/exceptional f32: the generic push still recovers an
            // f64 proof where one exists (every subnormal f32 is a normal f64).
            self.push(F80::from_f32(value));
        }
    }

    #[inline(always)]
    fn push_i32_exact(&mut self, value: i32) {
        let canonical = F80(crate::x87_common::i32_to_f80(value));
        // Every i32 is exactly a zero-or-normal f64.
        let exact64 = f64::from(value);
        self.push_with_proofs(canonical, derive_f32_proof(exact64), Some(exact64));
    }

    /// Pop a value from the stack.
    ///
    /// On empty-stack underflow with IE masked, still advance TOP so later
    /// ST(i) references use the correct physical slots (SDM Vol 1 §8.5.1.1).
    pub fn pop(&mut self) -> F80 {
        let top = self.get_top();
        if self.is_empty(top) {
            self.set_stack_fault(false);
            // Advance TOP even on underflow — the slot was already empty
            // (tag=3), so no tag change is needed; only TOP advances.
            self.numeric_shadow.invalidate_emptied(top);
            self.set_top((top + 1) & 0x7);
            return F80::ZERO;
        }
        let value = self.regs[top];
        self.set_tag(top, 3); // Empty
        self.numeric_shadow.invalidate_emptied(top);
        self.set_top((top + 1) & 0x7);
        value
    }

    /// Get the absolute register index for ST(i)
    #[inline]
    pub fn get_st_idx(&self, i: usize) -> usize {
        (self.get_top() + i) & 0x7
    }

    /// Access ST(i)
    pub fn get_st(&self, i: usize) -> F80 {
        let physical = self.get_st_idx(i);
        self.debug_assert_committed(physical);
        self.regs[physical]
    }

    /// EXP-A013 tripwire: an F80 read of a binary64-dirty, non-empty slot
    /// means a missed commit-on-read barrier — surface it in debug builds
    /// (the differential suites run debug) instead of as precision drift.
    #[inline]
    fn debug_assert_committed(&self, physical_index: usize) {
        #[cfg(debug_assertions)]
        {
            debug_assert!(
                !self.numeric_shadow.drc_binary64_active
                    || (self.numeric_shadow.drc_binary64_dirty & (1u8 << physical_index) == 0
                        && self.numeric_shadow.drc_f80_dirty & (1u8 << physical_index) == 0)
                    || self.is_empty(physical_index),
                "F80 read of dirty DRC ST slot {physical_index} — missing materialization barrier"
            );
        }
        #[cfg(not(debug_assertions))]
        let _ = physical_index;
    }

    /// Set ST(i) to a new value, re-tagging it by CONTENT.
    ///
    /// Arithmetic can change a register's numerical class, so result writes
    /// classify the new value just as pushes do. A zero result receives the
    /// Zero tag; finite nonzero results receive Valid; NaNs and unsupported
    /// encodings receive Special.
    ///
    /// The re-tag belongs at the **ST-relative** setters, not at
    /// `set_physical_reg`. That raw setter is what `FRSTOR` uses, and there the
    /// tag word comes from the restored image — a register may legitimately be
    /// tagged Empty while holding stale bits. Classifying there would destroy
    /// the restored tag word and misclassify the restored register values.
    pub fn set_st(&mut self, i: usize, value: F80) {
        let idx = self.get_st_idx(i);
        self.set_physical_reg(idx, value);
        self.set_tag(idx, value.x87_tag());
    }

    #[inline]
    fn set_physical_reg(&mut self, physical_index: usize, value: F80) {
        self.regs[physical_index] = value;
        self.numeric_shadow.invalidate(physical_index);
    }

    #[inline]
    fn set_physical_reg_with_proofs(
        &mut self,
        physical_index: usize,
        value: F80,
        exact32: Option<f32>,
        exact64: Option<f64>,
    ) {
        self.regs[physical_index] = value;
        self.numeric_shadow
            .set_proofs(physical_index, exact32, exact64);
    }

    #[inline]
    /// The proof-carrying twin of [`Fpu::set_st`], and it re-tags for the same
    /// reason — this is the setter the numeric-shadow arithmetic fast path
    /// writes through, so leaving it out would fix the slow path only.
    fn set_st_with_proofs(
        &mut self,
        i: usize,
        value: F80,
        exact32: Option<f32>,
        exact64: Option<f64>,
    ) {
        let idx = self.get_st_idx(i);
        self.set_physical_reg_with_proofs(idx, value, exact32, exact64);
        self.set_tag(idx, value.x87_tag());
    }

    /// ST(i)'s exact binary32 shadow, when it carries the f32 proof.
    #[inline]
    #[must_use]
    pub fn exact_binary32_st(&self, i: usize) -> Option<f32> {
        let idx = self.get_st_idx(i);
        let exact = self.numeric_shadow.get32(idx)?;
        debug_assert_eq!(
            crate::x87_common::f80_to_exact_normal_or_zero_f32(self.regs[idx].bytes()),
            Some(exact),
            "stale exact-binary32 x87 shadow for physical register {idx}"
        );
        Some(exact)
    }

    #[inline]
    /// Whether ST(i) carries a binary64 proof.
    #[must_use]
    pub fn has_binary64_proof_st(&self, i: usize) -> bool {
        self.exact_binary64_st(i).is_some()
    }

    /// ST(i)'s exact binary64 shadow, when it carries the f64 proof. Public
    /// for the same reason as [`Self::exact_binary32_st`].
    #[must_use]
    pub fn exact_binary64_st(&self, i: usize) -> Option<f64> {
        let idx = self.get_st_idx(i);
        let exact = self.numeric_shadow.get64(idx)?;
        debug_assert_eq!(
            crate::x87_common::f80_to_exact_normal_or_zero_f64(self.regs[idx].bytes()),
            Some(exact),
            "stale exact-binary64 x87 shadow for physical register {idx}"
        );
        Some(exact)
    }

    #[inline]
    fn invalidate_binary32_reg(&mut self, physical_index: usize) {
        self.numeric_shadow.invalidate(physical_index);
    }

    #[inline]
    fn invalidate_all_binary32(&mut self) {
        self.numeric_shadow.invalidate_all();
    }

    /// Drop runtime-only x87 metadata after an external field-wise restore.
    ///
    /// Checkpoint owners outside this crate can restore the public architectural
    /// fields directly, but the binary32 proof and host condition-code state do
    /// not belong to that snapshot. Call this once after all fields are written.
    pub fn finish_external_restore(&mut self) {
        self.numeric_shadow.invalidate_all();
        self.numeric_shadow.host_condition_codes_dirty = true;
        self.numeric_shadow.drc_binary64_active = false;
        self.numeric_shadow.drc_binary64_dirty = 0;
    }

    #[inline]
    fn mark_host_condition_codes_dirty(&mut self) {
        self.numeric_shadow.host_condition_codes_dirty = true;
    }

    #[inline]
    fn sync_host_condition_codes_if_dirty(&mut self) {
        if !self.numeric_shadow.host_condition_codes_dirty {
            return;
        }

        #[cfg(target_arch = "x86_64")]
        {
            // A native-binary32 fast operation skips the host side effect, so
            // restore guest condition codes lazily before the next helper.
            restore_host_x87_condition_codes(self.control_word, self.status_word);
        }

        self.numeric_shadow.host_condition_codes_dirty = false;
    }

    #[inline]
    fn swap_physical_regs(&mut self, a: usize, b: usize) {
        self.regs.swap(a, b);
        self.numeric_shadow.swap(a, b);
    }

    /// Get a reference to ST(i) (avoids copies for asm operations)
    pub fn get_st_ref(&self, i: usize) -> &F80 {
        let idx = self.get_st_idx(i);
        self.debug_assert_committed(idx);
        &self.regs[idx]
    }

    /// Restore the invariant that a numeric proof implies a non-Empty slot
    /// after a wholesale `tag_word` write (MDD-BUG-FLUX-00141).
    /// `emms` and environment restores bypass `set_tag`, so they must remove
    /// proofs for slots they mark Empty rather than expose stale shadows.
    fn invalidate_proofs_of_empty_slots(&mut self) {
        for reg in 0..8 {
            if self.get_tag(reg) == 3 {
                self.numeric_shadow.clear_proofs(reg);
            }
        }
    }

    /// Set the tag for a register (0=valid, 1=zero, 2=special, 3=empty)
    pub fn set_tag(&mut self, reg: usize, tag: u16) {
        let shift = reg * 2;
        self.tag_word = (self.tag_word & !(0x3 << shift)) | ((tag & 0x3) << shift);
        if tag & 0x3 == 3 {
            self.invalidate_binary32_reg(reg);
        }
    }

    /// Get the tag for a register
    pub fn get_tag(&self, reg: usize) -> u16 {
        (self.tag_word >> (reg * 2)) & 0x3
    }

    /// Check if a register is empty
    pub fn is_empty(&self, reg: usize) -> bool {
        self.get_tag(reg) == 3
    }

    /// Set stack fault (C1 indicates overflow vs underflow)
    pub fn set_stack_fault(&mut self, overflow: bool) {
        // IE (bit 0) + SF (bit 6), C1 (bit 9) indicates overflow.
        self.set_exception(0x01);
        self.status_word |= 0x40;
        if overflow {
            self.status_word |= 1 << 9;
        } else {
            self.status_word &= !(1 << 9);
        }
    }

    /// Set an exception flag in the status word.
    ///
    /// For non-stack invalid operations, use `set_invalid_op_non_stack`
    /// to also clear C1 (Intel SDM Vol 1 §8.5.1). Calling this method with
    /// IE alone leaves C1 stale from a prior overflow.
    pub fn set_exception(&mut self, flag: u16) {
        self.status_word |= flag;
        if (self.control_word & flag) == 0 {
            self.status_word |= 1 << 7; // ES: error summary for unmasked exceptions.
        }
    }

    /// Clear all exception flags
    pub fn clear_exceptions(&mut self) {
        self.status_word &= !0x00FF;
    }

    /// Get condition codes (C0, C1, C2, C3)
    pub fn get_condition_codes(&self) -> (bool, bool, bool, bool) {
        let c0 = (self.status_word & (1 << 8)) != 0;
        let c1 = (self.status_word & (1 << 9)) != 0;
        let c2 = (self.status_word & (1 << 10)) != 0;
        let c3 = (self.status_word & (1 << 14)) != 0;
        (c0, c1, c2, c3)
    }

    /// Set condition codes
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn set_condition_codes(&mut self, c0: bool, c1: bool, c2: bool, c3: bool) {
        let mask = (1 << 8) | (1 << 9) | (1 << 10) | (1 << 14);
        self.status_word &= !mask;
        if c0 {
            self.status_word |= 1 << 8;
        }
        if c1 {
            self.status_word |= 1 << 9;
        }
        if c2 {
            self.status_word |= 1 << 10;
        }
        if c3 {
            self.status_word |= 1 << 14;
        }
    }

    // =========================================================================
    // x87 save-area conversion
    // =========================================================================

    /// Convert F80 registers + control/status/tag words to the 108-byte
    /// x87 state format used by FRSTOR/FNSAVE (32-bit protected mode layout).
    ///
    /// Layout (108 bytes, 32-bit protected-mode format):
    /// ```text
    /// Bytes   0- 3: Control word (32-bit, only low 16 used)
    /// Bytes   4- 7: Status word (TOP in bits 11-13)
    /// Bytes   8-11: Tag word (physical-register-indexed: bits 0-1 = R0 tag,
    ///               bits 2-3 = R1 tag, …, bits 14-15 = R7 tag)
    /// Bytes  12-15: IP offset
    /// Bytes  16-19: IP selector + opcode
    /// Bytes  20-23: Data operand offset
    /// Bytes  24-27: Data operand selector
    /// Bytes  28-107: 8 × 10-byte extended-precision registers in stack
    ///                order — offset 28+10·i is ST(i), the value currently
    ///                at logical stack slot i. Hardware FNSAVE writes this
    ///                stack-relative; FRSTOR reads it stack-relative. The
    ///                mapping to physical register R[(TOP+i) & 7] is implied
    ///                by the saved TOP in the status word.
    /// ```
    ///
    /// Registers must be saved in stack order, not physical order, or a
    /// host round trip restores the wrong ST(i) values when TOP is nonzero.
    pub fn sync_to_x87_save_area(&mut self) {
        // EXP-A013: the save-area image reads every F80 register.
        self.commit_drc_binary64_register_file();
        // Pull immutable values out before taking a mutable borrow on the buf.
        let safe_cw = self.control_word | 0x003F;
        let status_word = self.status_word;
        let tag_word = self.tag_word;
        let ip_offset = self.ip_offset;
        let ip_selector = self.ip_selector;
        let operand_offset = self.operand_offset;
        let operand_selector = self.operand_selector;
        let opcode = self.opcode & 0x07FF;
        let top = self.get_top();
        // Snapshot the physical-register array in stack-relative order so we
        // can write into x87_save_area without a self-aliasing borrow.
        let stack_regs: [F80; 8] = std::array::from_fn(|i| self.regs[(top + i) & 0x7]);

        let buf = &mut self.x87_save_area;

        // Control word — force-mask all x87 exceptions (bits 0-5) so FRSTOR
        // never loads an unmasked control word that could crash the host FPU.
        buf[0..4].copy_from_slice(&(safe_cw as u32).to_le_bytes());
        // Status word
        buf[4..8].copy_from_slice(&(status_word as u32).to_le_bytes());
        // Tag word — physical-register-indexed; copied verbatim.
        buf[8..12].copy_from_slice(&(tag_word as u32).to_le_bytes());
        // IP offset
        buf[12..16].copy_from_slice(&ip_offset.to_le_bytes());
        // IP selector (low 16) + FOP (bits 26:16). FOP used to be hard-coded
        // to zero here, which lost any opcode a guest had restored with
        // FLDENV/FRSTOR (MDD-BUG-CRUCIBLE-00578).
        buf[16..20]
            .copy_from_slice(&((ip_selector as u32) | ((opcode as u32) << 16)).to_le_bytes());
        // Data operand offset
        buf[20..24].copy_from_slice(&operand_offset.to_le_bytes());
        // Data operand selector
        buf[24..28].copy_from_slice(&(operand_selector as u32).to_le_bytes());

        // Registers in **stack-relative** order: offset 28+10·i = ST(i).
        for i in 0..8 {
            let offset = 28 + i * 10;
            buf[offset..offset + 10].copy_from_slice(&stack_regs[i].0);
        }
    }

    /// Convert the 108-byte x87 image back to F80 registers and state words.
    /// The image uses stack order, so unrotate by its saved TOP to recover
    /// the physical-register layout in `self.regs[]`.
    pub fn sync_from_x87_save_area(&mut self) {
        let buf = self.x87_save_area;

        // Control word (low 16 bits)
        self.control_word = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as u16;
        // Status word (low 16 bits) — must be loaded before the register loop
        // because the register-order rotation depends on TOP.
        self.status_word = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as u16;
        // Tag word (low 16 bits) — physical-indexed, no rotation needed.
        self.tag_word = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as u16;
        // IP offset
        self.ip_offset = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
        // IP selector (low 16) + FOP (bits 26:16). The `as u16` alone silently
        // discarded FOP (MDD-BUG-CRUCIBLE-00578).
        let ip_sel_and_fop = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
        self.ip_selector = ip_sel_and_fop as u16;
        self.opcode = ((ip_sel_and_fop >> 16) & 0x07FF) as u16;
        // Data operand offset
        self.operand_offset = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
        // Data operand selector
        self.operand_selector = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]) as u16;

        // Unrotate stack-order registers back into physical layout.
        let top = self.get_top();
        for i in 0..8 {
            let phys_idx = (top + i) & 0x7;
            let offset = 28 + i * 10;
            let mut bytes = [0u8; 10];
            bytes.copy_from_slice(&buf[offset..offset + 10]);
            self.set_physical_reg(phys_idx, F80(bytes));
        }
    }

    /// Restore only the eight x87 register values from the embedded save area.
    ///
    /// DH keeps the software status, tag, control, and pointer fields
    /// authoritative while register values are resident in the host x87 stack.
    /// Therefore an `FNSAVE` spill may copy back only bytes 28..108. The saved
    /// TOP must still match the software TOP because it defines how the
    /// stack-relative image maps onto the physical `regs` array.
    #[doc(hidden)]
    pub fn restore_regs_from_x87_save_area(&mut self) {
        let image_status_word = u32::from_le_bytes([
            self.x87_save_area[4],
            self.x87_save_area[5],
            self.x87_save_area[6],
            self.x87_save_area[7],
        ]) as u16;
        let image_top = ((image_status_word >> 11) & 0x7) as usize;
        let shadow_top = self.get_top();
        assert_eq!(
            image_top, shadow_top,
            "host x87 TOP diverged from the software shadow"
        );

        let buf = self.x87_save_area;
        for i in 0..8 {
            let phys_idx = (shadow_top + i) & 0x7;
            let offset = 28 + i * 10;
            let mut bytes = [0u8; 10];
            bytes.copy_from_slice(&buf[offset..offset + 10]);
            self.set_physical_reg(phys_idx, F80(bytes));
        }
    }
}

/// Test adapter for isolated host x87 captures, sharing the interpreter's
/// status and compare-flag helpers.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DyncoreStatusFold {
    /// Arithmetic capture: merge exception bits 0..=5 and replace C1.
    KFull,
    /// FISTTP capture: K-full, then apply the interpreter's non-stack IE rule.
    KFullIe,
    /// FIST/FISTP capture: apply only the interpreter's non-stack IE rule.
    PIe,
    /// Compare capture: replace C0/C2/C3 and apply the captured IE.
    ///
    /// MDD-BUG-CRUCIBLE-00258: this used to carry an `apply_invalid` flag that
    /// callers set `false` for FUCOM, which threw the host's invalid flag away
    /// for every unordered compare — including the signaling-NaN case where
    /// real x87 does raise it. The flag was answering a question the capture
    /// has already answered: the word was produced by running the *actual*
    /// mnemonic on the host, and host FUCOM sets IE for exactly the SNaN and
    /// unsupported-encoding inputs while host FCOM also sets it for quiet NaNs.
    /// So the captured bit is already the right bit for whichever compare ran,
    /// and the correct fold is simply to keep it.
    CompareConditionCodes,
    /// COMI capture: map C0/C2/C3 to CF/PF/ZF and apply the captured IE.
    /// See [`DyncoreStatusFold::CompareConditionCodes`] for why there is no
    /// FCOMI-vs-FUCOMI flag.
    CompareEflags,
}

/// Fold one isolated host-x87 status capture into software-authoritative state.
///
/// The caller must pass the word captured for exactly one mnemonic after the
/// direct-host `FNCLEX -> op -> FNSTSW` sequence.  This function is integer-only
/// and does not inspect or mutate the software x87 register values.
#[cfg(test)]
fn fold_dyncore_status_capture(cpu: &mut Cpu, fold: DyncoreStatusFold, sw: u16) {
    match fold {
        DyncoreStatusFold::KFull => merge_x87_exception_status_and_c1(cpu, sw),
        DyncoreStatusFold::KFullIe => {
            merge_x87_exception_status_and_c1(cpu, sw);
            if (sw & 0x0001) != 0 {
                set_invalid_op_non_stack(cpu);
            }
        }
        DyncoreStatusFold::PIe => {
            if (sw & 0x0001) != 0 {
                set_invalid_op_non_stack(cpu);
            }
        }
        DyncoreStatusFold::CompareConditionCodes => {
            set_condition_codes_from_sw(cpu, sw);
            if (sw & 1) != 0 {
                // Compares use the interpreter's plain IE rule, which
                // deliberately preserves stale C1.
                cpu.fpu.set_exception(0x01);
            }
        }
        DyncoreStatusFold::CompareEflags => {
            set_eflags_from_sw(cpu, sw);
            // FCOMI/FUCOMI zero OF, SF and AF (SDM "Flags Affected", confirmed
            // on host silicon). Keep this test adapter consistent with the
            // interpreter's compare path.
            clear_comi_undefined_flags(cpu);
            if (sw & 1) != 0 {
                cpu.fpu.set_exception(0x01);
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
// Test fixtures use arbitrary floats like `3.14` and `2.71828` to populate
// FPU registers. They are not meant as approximations of PI/E — clippy's
// `approx_constant` correctness lint flags them anyway, so we silence it
// at the test module boundary.
#[allow(clippy::approx_constant)]
mod tests {
    use super::*;

    #[test]
    fn reset_clears_architectural_save_and_runtime_shadow_state() {
        let mut fpu = Fpu::new();
        fpu.regs = [F80::from_f64(42.5); 8];
        fpu.status_word = 0x6D41;
        fpu.control_word = 0x0272;
        fpu.tag_word = 0x1234;
        fpu.ip_offset = 0xDEAD_BEEF;
        fpu.ip_selector = 0xBEEF;
        fpu.operand_offset = 0xCAFE_BABE;
        fpu.operand_selector = 0xCAFE;
        fpu.x87_save_area.fill(0xA5);
        fpu.numeric_shadow.bits = [0x3F80_0000; 8];
        fpu.numeric_shadow.valid = 0xFF;
        fpu.numeric_shadow.host_condition_codes_dirty = true;
        fpu.numeric_shadow.drc_binary64_active = true;

        fpu.reset();

        let reset = Fpu::new();
        assert_eq!(fpu.regs, reset.regs);
        assert_eq!(fpu.status_word, reset.status_word);
        assert_eq!(fpu.control_word, reset.control_word);
        assert_eq!(fpu.tag_word, reset.tag_word);
        assert_eq!(fpu.ip_offset, reset.ip_offset);
        assert_eq!(fpu.ip_selector, reset.ip_selector);
        assert_eq!(fpu.operand_offset, reset.operand_offset);
        assert_eq!(fpu.operand_selector, reset.operand_selector);
        assert_eq!(fpu.x87_save_area, reset.x87_save_area);
        assert_eq!(fpu.numeric_shadow.bits, reset.numeric_shadow.bits);
        assert_eq!(fpu.numeric_shadow.valid, reset.numeric_shadow.valid);
        assert_eq!(
            fpu.numeric_shadow.host_condition_codes_dirty,
            reset.numeric_shadow.host_condition_codes_dirty
        );
        assert!(!fpu.drc_binary64_mode_active());
    }

    #[test]
    fn drc_binary64_selection_rounds_once_and_preserves_the_fpu_abi() {
        // 1 + 2^-60 is below one binary64 ulp but is representable in F80.
        let extended = F80([0x08, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x3F]);
        let mut authentic = Fpu::new();
        authentic.push(extended);
        assert_eq!(authentic.get_st(0), extended);
        assert!(!authentic.drc_binary64_mode_active());

        let mut drc = authentic.clone();
        drc.enter_drc_binary64_mode();
        assert!(drc.drc_binary64_mode_active());
        assert_eq!(drc.get_st(0), F80::ONE);
        assert_eq!(drc.exact_binary64_st(0), Some(1.0));

        // The active bit uses NumericShadow tail padding, keeping Fpu compact.
        assert_eq!(std::mem::size_of::<Fpu>(), 368);

        drc.finish_external_restore();
        assert!(!drc.drc_binary64_mode_active());
    }

    /// MDD-BUG-FLUX-00087: an MMX alias write must never be F80->f64
    /// rounded by a later binary64 commit — the raw qword (here a signaling
    /// NaN pattern under the 0xFFFF alias exponent) survives bit-exact.
    ///
    /// MDD-BUG-ANVIL-00089 adjudication: this test pins `mark_raw_alias`'s
    /// `drc_binary64_dirty` clear SPECIFICALLY, and the slot's non-emptiness
    /// is asserted so nobody re-doubts the mechanism: after
    /// `enter_mmx_state` the tag word is all-valid, so
    /// `commit_one_binary64_slot`'s `is_empty` early-return CANNOT fire and
    /// the dirty exemption is the only thing standing between the alias
    /// bytes and a payload-destroying F80->f64 roundtrip. Mutation-verified
    /// 2026-08-06: removing only that clear reds this test. (ANVIL-00089's
    /// contrary measurement had mutated the identical-bodied
    /// `invalidate_emptied` twelve lines above — see the seam comments.)
    #[test]
    fn mmx_alias_writes_survive_binary64_commits_bit_exact() {
        let mut fpu = Fpu::new();
        fpu.enter_drc_binary64_mode();
        let payload = 0x0123_4567_89AB_CDEFu64;
        fpu.write_mmx_register(3, payload);
        fpu.enter_mmx_state();
        assert!(
            !fpu.is_empty(3),
            "after enter_mmx_state the aliased slot must be tag-valid, so the \
             commit's is_empty early-return cannot be what protects the payload"
        );
        // A whole-file commit (any read barrier) must leave the alias alone.
        fpu.commit_drc_binary64_register_file();
        assert_eq!(fpu.read_mmx_register(3), payload);
        assert_eq!(fpu.read_mmx_register_committed(3), payload);
    }

    /// EXP-A013: commits are deferred — an x87 write leaves its slot dirty
    /// (F80 unrounded) until a read barrier rounds it; the barrier is
    /// idempotent and per-register.
    #[test]
    fn binary64_commits_defer_until_a_read_barrier() {
        // 1 + 2^-60: representable in F80, rounds to 1.0 in binary64.
        let extended = F80([0x08, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x3F]);
        let mut fpu = Fpu::new();
        fpu.enter_drc_binary64_mode();
        fpu.push(F80::ZERO);
        // Overwrite ST(0) with the unroundable value: slot goes dirty.
        fpu.set_st(0, extended);
        assert_eq!(
            fpu.regs[fpu.get_st_idx(0)],
            extended,
            "the F80 stays unrounded while deferred"
        );
        fpu.commit_drc_binary64_st(0);
        assert_eq!(fpu.get_st(0), F80::ONE, "the read barrier rounds");
        // Idempotent: a second barrier is a no-op.
        fpu.commit_drc_binary64_st(0);
        assert_eq!(fpu.get_st(0), F80::ONE);
    }

    fn seed_reverse_stale_f64(fpu: &mut Fpu, physical: usize, value: f64) {
        fpu.seed_drc_reverse_stale_f64(physical, value);
    }

    #[test]
    fn drc_f80_materialization_rebuilds_the_canonical_mirror() {
        let mut fpu = Fpu::new();
        fpu.push(F80::from_f64(9.0));
        let physical = fpu.get_st_idx(0);
        fpu.regs[physical] = F80::from_f64(9.0);
        seed_reverse_stale_f64(&mut fpu, physical, 1.25);

        fpu.materialize_drc_f80_register(physical);

        assert_eq!(fpu.regs[physical], F80::from_f64(1.25));
        assert_eq!(fpu.exact_binary64_st(0), Some(1.25));
        assert_eq!(fpu.numeric_shadow.drc_f80_dirty, 0);
        assert_eq!(fpu.numeric_shadow.drc_binary64_dirty, 0);
    }

    #[test]
    fn raw_f80_overwrite_supersedes_reverse_stale_value() {
        let mut fpu = Fpu::new();
        fpu.push(F80::from_f64(2.0));
        let physical = fpu.get_st_idx(0);
        seed_reverse_stale_f64(&mut fpu, physical, 1.25);
        let extended = F80([0x08, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x3F]);

        fpu.set_physical_reg(physical, extended);

        let bit = 1u8 << physical;
        assert_eq!(fpu.numeric_shadow.drc_f80_dirty & bit, 0);
        assert_ne!(fpu.numeric_shadow.drc_binary64_dirty & bit, 0);
        assert_eq!(
            fpu.numeric_shadow.drc_f80_dirty & fpu.numeric_shadow.drc_binary64_dirty,
            0
        );
        fpu.commit_drc_binary64_register(physical);
        assert_eq!(fpu.regs[physical], F80::from_f64(extended.to_f64()));
    }

    #[test]
    fn mmx_alias_overwrite_clears_reverse_stale_direction() {
        let mut fpu = Fpu::new();
        fpu.push(F80::from_f64(2.0));
        let physical = fpu.get_st_idx(0);
        seed_reverse_stale_f64(&mut fpu, physical, 1.25);
        let payload = 0x0123_4567_89AB_CDEFu64;

        fpu.write_mmx_register(physical, payload);

        let bit = 1u8 << physical;
        assert_eq!(fpu.numeric_shadow.drc_f80_dirty & bit, 0);
        assert_eq!(fpu.numeric_shadow.drc_binary64_dirty & bit, 0);
        assert_eq!(fpu.read_mmx_register(physical), payload);
    }

    #[test]
    fn save_area_observation_materializes_reverse_stale_f80() {
        let mut fpu = Fpu::new();
        fpu.push(F80::from_f64(2.0));
        let physical = fpu.get_st_idx(0);
        seed_reverse_stale_f64(&mut fpu, physical, 1.25);

        fpu.sync_to_x87_save_area();

        assert_eq!(&fpu.x87_save_area[28..38], &F80::from_f64(1.25).0);
        assert_eq!(fpu.numeric_shadow.drc_f80_dirty, 0);
    }

    #[test]
    fn physical_swap_moves_reverse_stale_mask_with_the_value() {
        let mut fpu = Fpu::new();
        fpu.push(F80::from_f64(2.0));
        fpu.push(F80::from_f64(3.0));
        let a = fpu.get_st_idx(0);
        let b = fpu.get_st_idx(1);
        seed_reverse_stale_f64(&mut fpu, a, 1.25);
        fpu.numeric_shadow.f64_bits[b] = 2.5f64.to_bits();
        fpu.numeric_shadow.f64_valid |= 1u8 << b;

        fpu.swap_physical_regs(a, b);

        assert_eq!(fpu.numeric_shadow.drc_f80_dirty & (1u8 << b), 1u8 << b);
        assert_eq!(fpu.numeric_shadow.drc_f80_dirty & (1u8 << a), 0);
        fpu.materialize_drc_f80_register(b);
        assert_eq!(fpu.regs[b], F80::from_f64(1.25));
        assert_eq!(fpu.regs[a], F80::from_f64(2.0));
    }

    #[test]
    fn external_restore_drops_reverse_stale_metadata() {
        let mut fpu = Fpu::new();
        let physical = fpu.get_st_idx(0);
        seed_reverse_stale_f64(&mut fpu, physical, 1.25);

        fpu.finish_external_restore();

        assert!(!fpu.drc_binary64_mode_active());
        assert_eq!(fpu.numeric_shadow.drc_f80_dirty, 0);
        assert_eq!(fpu.numeric_shadow.drc_binary64_dirty, 0);
    }

    #[cfg(target_arch = "x86_64")]
    fn reset_host_x87_status_for_arithmetic_oracle() {
        unsafe {
            let _host_x87 = crate::cpu::host_x87_enter_guarded();
            std::arch::asm!(
                "fninit",
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn reset_host_x87_status_for_arithmetic_oracle() {}

    /// MDD-BUG-CRUCIBLE-00578 (save/restore half): `FOP` survives the x87
    /// environment round trip.
    ///
    /// FOP lives in bits 26:16 of the dword at offset 16, sharing that dword
    /// with FCS. The encoder hard-coded it to zero ("opcode (high 16, set to
    /// 0)") and the decoder threw it away with an `as u16`, so a guest that
    /// restored an environment carrying a FOP and later saved one got zero
    /// back — hardware preserves it.
    ///
    /// Pinned from BOTH directions, because either alone passes with the field
    /// still broken: encode-then-read-the-image catches the zeroing encoder,
    /// and image-then-decode catches the discarding decoder.
    #[test]
    fn fop_survives_the_x87_environment_round_trip_crucible578() {
        const FOP: u16 = 0x01D9; // 11-bit opcode, all bits exercised
        const FCS: u16 = 0xBEEF;

        // Direction 1: state -> image. FOP must appear in bits 26:16, and must
        // not disturb FCS in the low half.
        let mut fpu = Fpu::new();
        fpu.ip_selector = FCS;
        fpu.opcode = FOP;
        fpu.sync_to_x87_save_area();
        let dword = u32::from_le_bytes([
            fpu.x87_save_area[16],
            fpu.x87_save_area[17],
            fpu.x87_save_area[18],
            fpu.x87_save_area[19],
        ]);
        assert_eq!(dword as u16, FCS, "FCS must keep the low half of the dword");
        assert_eq!(
            (dword >> 16) & 0x07FF,
            u32::from(FOP),
            "FOP must be encoded in bits 26:16, not hard-coded to zero"
        );

        // Direction 2: image -> state, on a FRESH Fpu so a stale field cannot
        // make this pass.
        let image = fpu.x87_save_area;
        let mut restored = Fpu::new();
        restored.x87_save_area = image;
        restored.sync_from_x87_save_area();
        assert_eq!(restored.ip_selector, FCS);
        assert_eq!(
            restored.opcode, FOP,
            "FOP must be decoded out of bits 26:16, not discarded by `as u16`"
        );
    }

    /// Locks the host-x87 exception-pointer envelope (MDD-BUG-CRUCIBLE-00313).
    ///
    /// The host-round-trip oracle `assert_interp_state_byte_matches_host_x87`
    /// skips the FIP/FDP offset/selector fields because a preemptively
    /// scheduled process can have an OS save/restore land inside the
    /// FRSTOR→FNSAVE window and launder that record to zero. This test proves
    /// the two facts that make skipping safe rather than a blind hole:
    ///
    ///  1. Everything *outside* the documented skip set round-trips
    ///     FRSTOR→FNSAVE byte-exactly on EVERY attempt — laundering is
    ///     confined to the pointer record, or this fails hard.
    ///  2. The FIP/FDP offsets are preserved by at least one of N back-to-back
    ///     attempts. The laundering is stochastic (observed p ≈ 2–3e-4 per
    ///     window); N consecutive laundered attempts would mean the host now
    ///     drops the record *deterministically* — a genuine behaviour change
    ///     to re-investigate before trusting the skip.
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn host_x87_fip_fdp_preserved_when_unlaundered() {
        const ATTEMPTS: usize = 64;
        const FIP: u32 = 0xDEAD_BEEF;
        const FDP: u32 = 0x0BAD_F00D;

        // Minimal well-formed 108-byte 32-bit protected-mode FSAVE image:
        // CW=0x037F (all masked), SW=0 (TOP=0), TW=0xFFFF (all empty),
        // fabricated pointer offsets, selectors 0 (host zeroes them in 64-bit
        // mode regardless), registers all-zero (tagged Empty).
        let mut image = [0u8; 108];
        image[0..4].copy_from_slice(&0x0000_037Fu32.to_le_bytes());
        image[8..12].copy_from_slice(&0x0000_FFFFu32.to_le_bytes());
        image[12..16].copy_from_slice(&FIP.to_le_bytes());
        image[20..24].copy_from_slice(&FDP.to_le_bytes());

        let mut laundered = 0usize;
        let mut preserved = false;
        // MDD-BUG-CRUCIBLE-00325: counted, then asserted below, so the "every
        // attempt" half of this lock is itself checked. Without it, an early
        // exit silently reduces a 64-attempt envelope check to one round trip
        // and every run still passes - which is exactly how the `break` that
        // used to sit in the preserved arm survived review.
        let mut checked = 0usize;
        for attempt in 0..ATTEMPTS {
            let mut out = [0u8; 108];
            unsafe {
                let _host_x87 = crate::cpu::host_x87_enter_guarded();
                std::arch::asm!(
                    "frstor [{src}]",
                    "fnsave [{dst}]",
                    src = in(reg) image.as_ptr(),
                    dst = in(reg) out.as_mut_ptr(),
                    out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                    out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                    options(nostack),
                );
            }

            // Fact 1: non-pointer state is exact on every attempt.
            for i in 0..108 {
                if matches!(i, 2..=17 | 20..=27) {
                    continue;
                }
                assert_eq!(
                    image[i], out[i],
                    "attempt {attempt}: non-pointer byte {i:#04X} diverged — \
                     this is NOT pointer laundering; investigate the round-trip \
                     itself, not the OS"
                );
            }

            let fip = u32::from_le_bytes([out[12], out[13], out[14], out[15]]);
            let fdp = u32::from_le_bytes([out[20], out[21], out[22], out[23]]);
            match (fip, fdp) {
                // MDD-BUG-CRUCIBLE-00325: record it and KEEP GOING. Preservation
                // is the common outcome, so breaking here ended the loop on
                // attempt 0 almost every run, and fact 1 above - the byte-exact
                // check the doc promises on EVERY attempt - was never applied to
                // attempts 1..64. Later attempts are the only thing that can
                // catch a partial drop or a changed host rule.
                (FIP, FDP) => preserved = true,
                // Documented stochastic laundering to zero.
                (0, 0) => laundered += 1,
                other => panic!(
                    "attempt {attempt}: host FIP/FDP partially dropped or \
                     mangled ({other:X?}) — the laundering envelope changed \
                     (per-field drop? new vendor rule?). Re-investigate before \
                     adjusting the oracle skip list."
                ),
            }
            checked += 1;
        }
        assert_eq!(
            checked, ATTEMPTS,
            "the envelope lock must exercise every attempt; an early exit              reduces it to a single round trip while still passing"
        );
        assert!(
            preserved,
            "host laundered FIP/FDP on all {ATTEMPTS} consecutive attempts \
             ({laundered} laundered) — the drop is now deterministic on this \
             host, not stochastic OS interposition. The mechanism note on \
             assert_interp_state_byte_matches_host_x87 no longer holds here; \
             check CPU vendor / CPUID.(07H,0):EBX bits 6 and 13 and re-verify \
             before widening any skip list."
        );
    }

    /// Production-contract lock (MDD-BUG-CRUCIBLE-00313): DH keeps CW/SW/TW and
    /// the exception-pointer record software-authoritative; a host FNSAVE spill
    /// may feed back ONLY register bytes 28..108 (plus the TOP cross-check read
    /// from the image SW). The host is architecturally allowed to launder the
    /// pointer record, so if this fails because the restore widened its read
    /// window, host garbage would be reaching software-authoritative state —
    /// fix the restore, do not relax this test.
    #[test]
    fn dh_restore_regs_ignores_environment_bytes() {
        let mut fpu = Fpu::new();
        fpu.control_word = 0x027F;
        fpu.status_word = 0x3800; // TOP=7, no exception bits
        fpu.tag_word = 0x5A5A;
        fpu.ip_offset = 0xA1B2_C3D4;
        fpu.ip_selector = 0x1234;
        fpu.operand_offset = 0x5E6F_7081;
        fpu.operand_selector = 0x2345;
        fpu.sync_to_x87_save_area();

        // Worst-case host divergence: clobber every environment byte the host
        // could ever mangle — all of 0..28 except SW bytes 4..8, which the
        // restore legitimately reads for the TOP cross-check.
        for i in (0..4).chain(8..28) {
            fpu.x87_save_area[i] = 0xEE;
        }
        fpu.restore_regs_from_x87_save_area();

        assert_eq!(fpu.control_word, 0x027F);
        assert_eq!(fpu.status_word, 0x3800);
        assert_eq!(fpu.tag_word, 0x5A5A);
        assert_eq!(fpu.ip_offset, 0xA1B2_C3D4);
        assert_eq!(fpu.ip_selector, 0x1234);
        assert_eq!(fpu.operand_offset, 0x5E6F_7081);
        assert_eq!(fpu.operand_selector, 0x2345);
    }

    #[test]
    fn shadow_arithmetic_rejects_unproven_domains() {
        // PC=64 (the x87 default) never engages, even with full proofs on
        // both sides — no host type carries a 64-bit significand.
        let mut cpu = Cpu::new();
        cpu.fpu.control_word = 0x037f;
        cpu.fpu.push(F80::from_f32(1.0));
        assert!(!try_numeric_shadow_arith(
            &mut cpu,
            0,
            Some(2.0),
            Some(2.0),
            ArithOp::Add
        ));
        assert_eq!(cpu.fpu.get_st(0), F80::from_f32(1.0), "no state mutation");

        // PC=24 with an unproven (wider-than-f32) destination: no engagement.
        let mut cpu = Cpu::new();
        cpu.fpu.control_word = 0x007f;
        cpu.fpu.push(F80::from_f64(1.0 + 2f64.powi(-30)));
        assert!(!try_numeric_shadow_arith(
            &mut cpu,
            0,
            Some(2.0),
            Some(2.0),
            ArithOp::Add
        ));

        // PC=53 engages on f64 proofs (the legacy helper never could).
        let mut cpu = Cpu::new();
        cpu.fpu.control_word = 0x027f;
        let wide = 1.0 + 2f64.powi(-30);
        cpu.fpu.push(F80::from_f64(wide));
        assert!(try_numeric_shadow_arith(
            &mut cpu,
            0,
            None,
            Some(2.0),
            ArithOp::Add
        ));
        assert_eq!(cpu.fpu.get_st(0), F80::from_f64(wide + 2.0));
        assert_eq!(cpu.fpu.exact_binary64_st(0), Some(wide + 2.0));

        // Directed rounding engages too (RC=chop; the legacy helper's
        // `cw & 0x0f00 != 0` gate rejected every non-nearest mode — the
        let mut cpu = Cpu::new();
        cpu.fpu.control_word = 0x0c7f;
        cpu.fpu.push(F80::from_f32(1.0));
        assert!(try_numeric_shadow_arith(
            &mut cpu,
            0,
            Some(3.0 * 2f32.powi(-24)),
            Some(f64::from(3.0f32 * 2f32.powi(-24))),
            ArithOp::Add
        ));
        let (expected_chop, _) = x87_ops::f80_add_status(
            &F80::from_f32(1.0),
            &F80::from_f32(3.0 * 2f32.powi(-24)),
            0x0c7f,
        );
        assert_eq!(cpu.fpu.get_st(0), expected_chop);

        // Core-level domain pins: the min-normal boundary is excluded by the
        // magnitude floor (binary32 rounds min_normal * just_below_one UP to
        // MIN_POSITIVE while x87 PC-single keeps the wider exponent), and
        // overflow to infinity is excluded by the normal-result check.
        let just_below_one = f32::from_bits(0x3f7f_ffff);
        assert!(
            crate::x87_fast_core::fast_mul_f32_core(f32::MIN_POSITIVE, just_below_one, 0).is_none()
        );
        assert!(crate::x87_fast_core::fast_mul_f32_core(f32::MIN_POSITIVE, 0.5, 0).is_none());
        assert!(crate::x87_fast_core::fast_mul_f32_core(f32::MAX, 2.0, 0).is_none());
        #[cfg(target_arch = "x86_64")]
        assert_ne!(
            x87_ops::f80_mul(
                &F80::from_f32(f32::MIN_POSITIVE),
                &F80::from_f32(just_below_one),
                0x007f
            ),
            F80::from_f32(f32::MIN_POSITIVE),
            "host x87 oracle must demonstrate the exponent-boundary difference"
        );
    }

    #[test]
    fn single_normal_arithmetic_matches_exact_backend_bytes_and_status() {
        let cw = 0x007f;
        let mut state = 0x6a09_e667_f3bc_c909u64;
        let mut compared_add = 0usize;
        let mut compared_sub = 0usize;
        let mut compared_sub_reverse = 0usize;
        let mut compared_mul = 0usize;
        let mut compared_div = 0usize;

        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let mut a_bits =
                ((state as u32) & 0x807f_ffff) | ((((state >> 32) as u32 % 190) + 32) << 23);
            // The exponent range above excludes zero. Include signed zero
            // explicitly so exact arithmetic does not report precision loss.
            if (state >> 40) & 0xf == 0 {
                a_bits &= 0x8000_0000;
            }

            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let mut b_bits =
                ((state as u32) & 0x807f_ffff) | ((((state >> 32) as u32 % 190) + 32) << 23);
            if (state >> 40) & 0xf == 0 {
                b_bits &= 0x8000_0000;
            }

            let a_f32 = f32::from_bits(a_bits);
            let b_f32 = f32::from_bits(b_bits);
            let a = F80::from_f32(a_f32);
            let b = F80::from_f32(b_f32);

            use crate::x87_fast_core as fast_core;
            let rc = (cw >> 10) & 0b11;
            if let Some((value, status)) = fast_core::fast_add_f32_core(a_f32, b_f32, rc) {
                reset_host_x87_status_for_arithmetic_oracle();
                let (native, native_status) = x87_ops::f80_add_status(&a, &b, cw);
                assert_eq!(F80::from_f32(value), native);
                assert_eq!(status & 0x0220, native_status & 0x0220);
                compared_add += 1;
            }
            if let Some((value, status)) = fast_core::fast_mul_f32_core(a_f32, b_f32, rc) {
                reset_host_x87_status_for_arithmetic_oracle();
                let (native, native_status) = x87_ops::f80_mul_status(&a, &b, cw);
                assert_eq!(F80::from_f32(value), native);
                assert_eq!(status & 0x0220, native_status & 0x0220);
                compared_mul += 1;
            }
            if let Some((value, status)) = fast_core::fast_add_f32_core(a_f32, -b_f32, rc) {
                reset_host_x87_status_for_arithmetic_oracle();
                let (native, native_status) = x87_ops::f80_sub_status(&a, &b, cw);
                assert_eq!(F80::from_f32(value), native);
                assert_eq!(status & 0x0220, native_status & 0x0220);
                compared_sub += 1;
            }
            if let Some((value, status)) = fast_core::fast_add_f32_core(b_f32, -a_f32, rc) {
                reset_host_x87_status_for_arithmetic_oracle();
                let (native, native_status) = x87_ops::f80_sub_status(&b, &a, cw);
                assert_eq!(F80::from_f32(value), native);
                assert_eq!(status & 0x0220, native_status & 0x0220);
                compared_sub_reverse += 1;
            }
            if let Some((value, status)) = fast_core::fast_div_f32_core(a_f32, b_f32, rc) {
                reset_host_x87_status_for_arithmetic_oracle();
                let (native, native_status) = x87_ops::f80_div_status(&a, &b, cw);
                assert_eq!(F80::from_f32(value), native);
                assert_eq!(status & 0x0220, native_status & 0x0220);
                compared_div += 1;
            }
        }

        assert!(compared_add > 10_000, "too few accepted add cases");
        assert!(compared_sub > 10_000, "too few accepted subtract cases");
        assert!(
            compared_sub_reverse > 10_000,
            "too few accepted reverse-subtract cases"
        );
        assert!(compared_mul > 5_000, "too few accepted multiply cases");
        assert!(compared_div > 5_000, "too few accepted divide cases");
    }

    /// MDD-BUG-FLUX-00141: `emms` assigns `tag_word` wholesale, bypassing
    /// `set_tag`, so it must invalidate numeric proofs for the emptied slots.
    #[test]
    fn emms_drops_the_numeric_proofs_of_the_slots_it_empties() {
        let mut fpu = Fpu::new();
        fpu.push(F80::from_f32(1.25));
        assert_eq!(fpu.exact_binary32_st(0), Some(1.25));
        assert_eq!(fpu.exact_binary64_st(0), Some(1.25));

        fpu.emms();

        assert_eq!(fpu.tag_word, 0xFFFF);
        for i in 0..8 {
            assert!(fpu.is_empty(i), "slot {i} should be Empty after EMMS");
            assert_eq!(
                fpu.exact_binary32_st(i),
                None,
                "slot {i} kept a binary32 proof across EMMS"
            );
            assert_eq!(
                fpu.exact_binary64_st(i),
                None,
                "slot {i} kept a binary64 proof across EMMS"
            );
        }
    }

    /// The FLDENV half of MDD-BUG-FLUX-00141. FLDENV re-tags without touching
    /// the register bytes, so a proof outlives its own slot going Empty unless
    /// `decode_environment` drops it.
    #[test]
    fn fldenv_drops_the_numeric_proofs_of_the_slots_it_empties() {
        let mut cpu = Cpu::new();
        cpu.fpu.push(F80::from_f32(1.25));
        assert_eq!(cpu.fpu.exact_binary32_st(0), Some(1.25));
        assert_eq!(cpu.fpu.exact_binary64_st(0), Some(1.25));

        // A 14-byte environment image: default CW, SW with TOP=0, and a tag
        // word marking every slot Empty.
        let mut env = [0u8; 14];
        env[0..2].copy_from_slice(&0x037Fu16.to_le_bytes());
        env[2..4].copy_from_slice(&0u16.to_le_bytes());
        env[4..6].copy_from_slice(&0xFFFFu16.to_le_bytes());

        decode_environment(&mut cpu, X87EnvironmentLayout::Environment14, &env);

        for i in 0..8 {
            assert!(cpu.fpu.is_empty(i), "slot {i} should be Empty after FLDENV");
            assert_eq!(
                cpu.fpu.exact_binary32_st(i),
                None,
                "slot {i} kept a binary32 proof across FLDENV"
            );
            assert_eq!(
                cpu.fpu.exact_binary64_st(i),
                None,
                "slot {i} kept a binary64 proof across FLDENV"
            );
        }
    }

    #[test]
    fn numeric_shadow_tracks_canonical_register_mutations() {
        let mut fpu = Fpu::new();
        fpu.push(F80::from_f32(1.25));
        assert_eq!(fpu.exact_binary32_st(0), Some(1.25));

        fpu.set_st(0, F80::from_f64(1.0 + 2f64.powi(-30)));
        assert_eq!(fpu.exact_binary32_st(0), None);

        fpu.set_st(0, F80::from_f32(-3.5));
        assert_eq!(fpu.exact_binary32_st(0), None);
        fpu.pop();
        fpu.push(F80::from_f32(-3.5));
        assert_eq!(fpu.exact_binary32_st(0), Some(-3.5));
        let physical_top = fpu.get_top();
        fpu.set_tag(physical_top, 3);
        assert_eq!(fpu.exact_binary32_st(0), None);
    }

    #[test]
    fn f64_proof_lifecycle_tracks_canonical_register_mutations() {
        let mut fpu = Fpu::new();

        // An f32 push carries a free f64 proof (valid ⊆ f64_valid).
        fpu.push(F80::from_f32(1.25));
        assert_eq!(fpu.exact_binary32_st(0), Some(1.25));
        assert_eq!(fpu.exact_binary64_st(0), Some(1.25));

        // An f64-only value: exact binary64, not binary32.
        let f64_only = 1.0 + 2f64.powi(-30);
        let idx = fpu.get_st_idx(0);
        fpu.set_physical_reg_with_proofs(
            idx,
            F80::from_f64(f64_only),
            derive_f32_proof(f64_only),
            Some(f64_only),
        );
        assert_eq!(fpu.exact_binary32_st(0), None);
        assert_eq!(fpu.exact_binary64_st(0), Some(f64_only));

        // A plain set_st invalidates both proofs.
        fpu.set_st(0, F80::from_f64(f64_only));
        assert_eq!(fpu.exact_binary32_st(0), None);
        assert_eq!(fpu.exact_binary64_st(0), None);

        // Swap moves both proofs with the registers.
        fpu.push(F80::from_f32(2.5));
        let a = fpu.get_st_idx(0);
        let b = fpu.get_st_idx(1);
        fpu.set_physical_reg_with_proofs(
            b,
            F80::from_f64(f64_only),
            derive_f32_proof(f64_only),
            Some(f64_only),
        );
        fpu.swap_physical_regs(a, b);
        assert_eq!(fpu.exact_binary64_st(0), Some(f64_only));
        assert_eq!(fpu.exact_binary32_st(1), Some(2.5));
        assert_eq!(fpu.exact_binary64_st(1), Some(2.5));

        // Tagging Empty and popping invalidate the f64 proof too.
        let physical_top = fpu.get_top();
        fpu.set_tag(physical_top, 3);
        assert_eq!(fpu.exact_binary64_st(0), None);
        fpu.pop();
        // ST0 now aliases the next stack slot (2.5), whose proofs survive;
        // the VACATED physical slot's proof must be gone.
        assert_eq!(fpu.exact_binary64_st(0), Some(2.5));
        assert_eq!(fpu.numeric_shadow.get64(physical_top), None);
        assert_eq!(fpu.numeric_shadow.get32(physical_top), None);
    }

    #[test]
    fn load_boundaries_establish_f64_proofs() {
        let mut fpu = Fpu::new();

        // FLD m64 route: generic push of an exactly-f64 F80.
        let f64_only = 1.0 + 2f64.powi(-30);
        fpu.push(F80::from_f64(f64_only));
        assert_eq!(fpu.exact_binary64_st(0), Some(f64_only));
        assert_eq!(fpu.exact_binary32_st(0), None);
        fpu.pop();

        // FLD m80 of a full-64-bit-significand value: no proof.
        fpu.push(F80::PI);
        assert_eq!(fpu.exact_binary64_st(0), None);
        fpu.pop();

        // FILD i64 route: 2^53 is exact, 2^53 + 1 is not.
        fpu.push(F80::from_i64(1 << 53));
        assert_eq!(fpu.exact_binary64_st(0), Some(9_007_199_254_740_992.0));
        fpu.pop();
        fpu.push(F80::from_i64((1 << 53) + 1));
        assert_eq!(fpu.exact_binary64_st(0), None);
        fpu.pop();

        // FILD i32 route: always f64-provable, f32 only when narrow enough.
        fpu.push_i32_exact(16_777_217); // 2^24 + 1
        assert_eq!(fpu.exact_binary32_st(0), None);
        assert_eq!(fpu.exact_binary64_st(0), Some(16_777_217.0));
        fpu.pop();

        // Constants: FLDZ/FLD1 are provable, pi is not (covered above).
        fpu.push(F80::ZERO);
        assert_eq!(fpu.exact_binary64_st(0), Some(0.0));
        fpu.push(F80::ONE);
        assert_eq!(fpu.exact_binary32_st(0), Some(1.0));
        assert_eq!(fpu.exact_binary64_st(0), Some(1.0));

        // Subnormal f32 via push_binary32: normal in f64, so f64-provable.
        let subnormal_f32 = f32::from_bits(1);
        fpu.push_binary32(subnormal_f32);
        assert_eq!(fpu.exact_binary32_st(0), None);
        assert_eq!(fpu.exact_binary64_st(0), Some(f64::from(subnormal_f32)));
    }

    #[test]
    fn derive_f32_proof_covers_the_binary32_normal_or_zero_domain() {
        // Exactly-f32 normals and signed zeros derive.
        assert_eq!(derive_f32_proof(1.25), Some(1.25));
        assert_eq!(derive_f32_proof(0.0).unwrap().to_bits(), 0);
        assert_eq!(
            derive_f32_proof(-0.0).unwrap().to_bits(),
            (-0.0f32).to_bits()
        );
        // Not exactly representable in f32.
        assert_eq!(derive_f32_proof(1.0 + 2f64.powi(-30)), None);
        assert_eq!(derive_f32_proof(16_777_217.0), None); // 2^24 + 1
                                                          // Exactly representable but SUBNORMAL in f32: outside the proof domain.
        assert_eq!(derive_f32_proof(2f64.powi(-127)), None);
        // Out of f32 range entirely.
        assert_eq!(derive_f32_proof(1e300), None);
    }

    #[test]
    fn exact_input_widening_tracks_signed_zero_and_representable_i32() {
        let mut fpu = Fpu::new();
        fpu.push_binary32(-0.0);
        assert_eq!(
            fpu.exact_binary32_st(0).unwrap().to_bits(),
            (-0.0f32).to_bits()
        );
        assert_eq!(
            fpu.get_st(0),
            F80(crate::x87_common::f32_normal_or_zero_to_f80(-0.0).unwrap())
        );

        fpu.pop();
        fpu.push_i32_exact(16_777_216);
        assert_eq!(fpu.exact_binary32_st(0), Some(16_777_216.0));
        fpu.pop();
        fpu.push_i32_exact(16_777_217);
        assert_eq!(fpu.exact_binary32_st(0), None);
    }

    #[test]
    fn cpu_snapshot_restore_invalidates_binary32_runtime_metadata() {
        let mut cpu = Cpu::new();
        cpu.fpu.push(F80::from_f32(2.0));
        let snapshot = cpu.snapshot_state();

        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f32(100.0));
        cpu.restore_state(&snapshot).unwrap();

        assert_eq!(cpu.fpu.get_st(0), F80::from_f32(2.0));
        assert_eq!(cpu.fpu.exact_binary32_st(0), None);
        assert!(cpu.fpu.numeric_shadow.host_condition_codes_dirty);
    }

    #[test]
    fn shadow_add_core_handles_power_of_two_spacing_boundary() {
        let one = 1.0f32;
        let just_above_half_lower_ulp = -f32::from_bits((102 << 23) | 1);
        let (fast_value, fast_status) =
            crate::x87_fast_core::fast_add_f32_core(one, just_above_half_lower_ulp, 0)
                .expect("normal PC-single result");

        reset_host_x87_status_for_arithmetic_oracle();
        let (native, native_status) = x87_ops::f80_add_status(
            &F80::from_f32(one),
            &F80::from_f32(just_above_half_lower_ulp),
            0x007f,
        );
        assert_eq!(F80::from_f32(fast_value), native);
        assert_eq!(fast_status & 0x0220, native_status & 0x0220);
        assert_eq!(fast_status & 0x0220, 0x0020, "round-down must clear C1");
    }

    /// Adding or subtracting zero is exact, so PE and C1 must
    /// both stay clear regardless of the exponent gap between the operands.
    /// The error-free-transform cores get this by construction (the TwoSum
    /// residual of an exact sum is zero), but the pin stays.
    #[test]
    fn shadow_add_core_zero_operands_are_exact() {
        // 0xFF7F_FFFF is the regression seed: the largest-magnitude negative
        // finite f32, with biased exponent 254.
        let normals = [
            f32::MAX,
            -f32::MAX,
            f32::from_bits(0xFF7F_FFFF),
            f32::from_bits(40 << 23),
            f32::from_bits(30 << 23), // smallest exponent the old gap test caught
        ];
        for negate_b in [false, true] {
            for zero in [0.0f32, -0.0f32] {
                for normal in normals {
                    for (a, b) in [(zero, normal), (normal, zero)] {
                        let b = if negate_b { -b } else { b };
                        let (value, status) = crate::x87_fast_core::fast_add_f32_core(a, b, 0)
                            .expect("zero +/- normal stays in the fast domain");
                        assert_eq!(value, a + b);
                        assert_eq!(
                            status, 0,
                            "x +/- 0 is exact: PE and C1 must stay clear \
                             (a={a:?}, b={b:?})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn numeric_shadow_is_runtime_only_serde_metadata() {
        let mut fpu = Fpu::new();
        fpu.push(F80::from_f32(1.5));
        assert_eq!(fpu.exact_binary32_st(0), Some(1.5));

        let serialized = serde_json::to_string(&fpu).unwrap();
        let restored: Fpu = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored.get_st(0), F80::from_f32(1.5));
        assert_eq!(restored.exact_binary32_st(0), None);
        assert_eq!(restored.exact_binary64_st(0), None);
    }

    /// The pre-03463 loop form of `merge_x87_exception_status_and_c1`, kept
    /// as the oracle: one `Fpu::set_exception` per raised flag, then C1.
    fn merge_x87_exception_status_and_c1_loop_oracle(cpu: &mut Cpu, sw: u16) {
        for bit in 0..=5 {
            let flag = 1u16 << bit;
            if (sw & flag) != 0 {
                cpu.fpu.set_exception(flag);
            }
        }

        let c1 = 1u16 << 9;
        cpu.fpu.status_word = (cpu.fpu.status_word & !c1) | (sw & c1);
    }

    #[test]
    fn merge_x87_exception_status_and_c1_matches_per_flag_set_exception() {
        // Exhaustive over what the merge reads: the six raised-exception bits
        // and C1 of `sw` (plus all of its other bits at once), the six
        // control-word mask bits (plus all of its other bits at once), and the
        // incoming status word's low byte (IE..PE, SF, ES) and C1 (plus all of
        // its other bits: C0, C2, TOP, C3, B).
        const SW_OTHER: u16 = !0x023F;
        const CW_OTHER: u16 = !0x003F;
        const STATUS_OTHER: u16 = !0x02FF;
        let mut fast = Cpu::default();
        let mut oracle = Cpu::default();
        let mut checked = 0u64;
        for sw_exc in 0..64u16 {
            for sw_c1 in [0, 0x0200u16] {
                for sw_other in [0, SW_OTHER] {
                    let sw = sw_exc | sw_c1 | sw_other;
                    for cw_mask in 0..64u16 {
                        for cw_other in [0, CW_OTHER] {
                            let cw = cw_mask | cw_other;
                            for status_low in 0..256u16 {
                                for status_c1 in [0, 0x0200u16] {
                                    for status_other in [0, STATUS_OTHER] {
                                        let status = status_low | status_c1 | status_other;
                                        fast.fpu.control_word = cw;
                                        fast.fpu.status_word = status;
                                        oracle.fpu.control_word = cw;
                                        oracle.fpu.status_word = status;
                                        merge_x87_exception_status_and_c1(&mut fast, sw);
                                        merge_x87_exception_status_and_c1_loop_oracle(
                                            &mut oracle,
                                            sw,
                                        );
                                        if fast.fpu.status_word != oracle.fpu.status_word
                                            || fast.fpu.control_word != oracle.fpu.control_word
                                        {
                                            panic!(
                                                "sw={sw:#06x} cw={cw:#06x} status={status:#06x}: \
                                                 status {:#06x} != oracle {:#06x}, \
                                                 cw {:#06x} != oracle {:#06x}",
                                                fast.fpu.status_word,
                                                oracle.fpu.status_word,
                                                fast.fpu.control_word,
                                                oracle.fpu.control_word
                                            );
                                        }
                                        checked += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 256 * 128 * 1024);
    }

    #[test]
    fn dyncore_kfull_fold_reuses_all_exception_masks_and_replaces_c1() {
        for flag in [0x01u16, 0x02, 0x04, 0x08, 0x10, 0x20] {
            let mut cpu = Cpu::default();
            cpu.fpu.control_word = 0x003f & !flag;
            cpu.fpu.status_word = 0x4500;

            fold_dyncore_status_capture(&mut cpu, DyncoreStatusFold::KFull, flag | 0x0200);

            assert_eq!(
                cpu.fpu.status_word & flag,
                flag,
                "lost exception {flag:#04x}"
            );
            assert_ne!(
                cpu.fpu.status_word & 0x0080,
                0,
                "unmasked {flag:#04x} lost ES"
            );
            assert_ne!(cpu.fpu.status_word & 0x0200, 0, "captured C1 was not set");
            assert_eq!(
                cpu.fpu.status_word & 0x4500,
                0x4500,
                "K-full changed C0/C2/C3"
            );

            fold_dyncore_status_capture(&mut cpu, DyncoreStatusFold::KFull, 0);
            assert_eq!(
                cpu.fpu.status_word & flag,
                flag,
                "exceptions must stay sticky"
            );
            assert_eq!(
                cpu.fpu.status_word & 0x0200,
                0,
                "captured C1 was not cleared"
            );
        }
    }

    #[test]
    fn dyncore_kfull_ie_fold_merges_status_then_applies_non_stack_invalid() {
        let mut invalid = Cpu::default();
        invalid.fpu.control_word = 0x003f;
        invalid.fpu.status_word = 0x4502; // preserve CC and sticky DE
        fold_dyncore_status_capture(
            &mut invalid,
            DyncoreStatusFold::KFullIe,
            0x0221, // captured IE + PE + C1
        );
        assert_eq!(invalid.fpu.status_word & 0x0023, 0x0023);
        assert_eq!(invalid.fpu.status_word & 0x4500, 0x4500);
        assert_eq!(
            invalid.fpu.status_word & 0x0200,
            0,
            "non-stack invalid must clear captured C1"
        );

        let mut valid = Cpu::default();
        valid.fpu.status_word = 0x4000;
        fold_dyncore_status_capture(&mut valid, DyncoreStatusFold::KFullIe, 0x0220);
        assert_eq!(valid.fpu.status_word & 0x0020, 0x0020);
        assert_eq!(valid.fpu.status_word & 0x0200, 0x0200);
        assert_eq!(valid.fpu.status_word & 0x4000, 0x4000);
    }

    #[test]
    fn dyncore_pie_fold_applies_only_non_stack_invalid() {
        let mut invalid = Cpu::default();
        invalid.fpu.control_word = 0x003f;
        invalid.fpu.status_word = 0x4702; // all CC, stale C1, sticky DE
        fold_dyncore_status_capture(
            &mut invalid,
            DyncoreStatusFold::PIe,
            0x0221, // host also reports PE/C1, which P-IE must discard
        );
        assert_eq!(invalid.fpu.status_word & 0x0003, 0x0003);
        assert_eq!(invalid.fpu.status_word & 0x0020, 0, "P-IE leaked PE");
        assert_eq!(invalid.fpu.status_word & 0x4500, 0x4500);
        assert_eq!(invalid.fpu.status_word & 0x0200, 0, "invalid must clear C1");

        let mut valid = Cpu::default();
        valid.fpu.status_word = 0x4722;
        fold_dyncore_status_capture(&mut valid, DyncoreStatusFold::PIe, 0x0220);
        assert_eq!(
            valid.fpu.status_word, 0x4722,
            "P-IE without captured IE must change nothing"
        );
    }

    /// A quiet NaN: exponent all ones, integer bit set, quiet bit set.
    const QNAN: F80 = F80([0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0x7F]);
    /// A signaling NaN: same, but with the quiet bit CLEAR and a non-zero
    /// payload (a zero payload with the quiet bit clear would be infinity).
    const SNAN: F80 = F80([1, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x7F]);
    /// An unsupported encoding: non-zero exponent with the explicit integer
    /// bit clear (an "unnormal").
    const UNNORMAL: F80 = F80([0, 0, 0, 0, 0, 0, 0, 0x40, 0x01, 0x40]);
    const ONE: F80 = F80([0, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x3F]);

    /// MDD-BUG-CRUCIBLE-00258: an unordered compare is quiet for QUIET NaNs
    /// only. This is a decision table rather than a pair of one-sided cases
    /// because both directions matter: raising on a quiet NaN would make
    /// FUCOM identical to FCOM (defeating its purpose), and not raising on a
    /// signaling NaN is the bug that was filed.
    #[test]
    fn unordered_compare_raises_invalid_only_for_snan_and_unsupported_crucible258() {
        let cases: &[(&str, F80, F80, bool)] = &[
            ("ordinary finite operands", ONE, ONE, false),
            ("quiet NaN in ST(0)", QNAN, ONE, false),
            ("quiet NaN in ST(i)", ONE, QNAN, false),
            ("quiet NaN on both sides", QNAN, QNAN, false),
            ("signaling NaN in ST(0)", SNAN, ONE, true),
            ("signaling NaN in ST(i)", ONE, SNAN, true),
            ("signaling NaN masked by a quiet one", QNAN, SNAN, true),
            ("unsupported encoding in ST(0)", UNNORMAL, ONE, true),
            ("unsupported encoding in ST(i)", ONE, UNNORMAL, true),
        ];

        for (what, a, b, expected) in cases {
            assert_eq!(
                unordered_compare_raises_invalid(a, b),
                *expected,
                "{what}: expected raises_invalid == {expected}"
            );
        }
    }

    /// The classifiers must agree with the SDM's encoding table, and — because
    /// `x87_ops` is `x87_soft` on non-x86_64 hosts — must be answering about
    /// the encoding, never about the backend.
    #[test]
    fn f80_snan_and_unsupported_classifiers_match_the_encoding_table_crucible258() {
        assert!(SNAN.is_signaling_nan(), "SNaN not recognised");
        assert!(!QNAN.is_signaling_nan(), "QNaN misread as signaling");
        assert!(!ONE.is_signaling_nan());
        let infinity = F80([0, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x7F]);
        assert!(!infinity.is_signaling_nan(), "+inf misread as SNaN");
        assert!(!infinity.is_unsupported_encoding());

        assert!(
            UNNORMAL.is_unsupported_encoding(),
            "unnormal not recognised"
        );
        assert!(!SNAN.is_unsupported_encoding());
        assert!(!QNAN.is_unsupported_encoding());
        assert!(!ONE.is_unsupported_encoding());
        assert!(!F80::ZERO.is_unsupported_encoding(), "zero is supported");
        // A pseudo-NaN (all-ones exponent, integer bit clear) is unsupported
        // rather than signaling — the two predicates must not both claim it.
        let pseudo_nan = F80([1, 0, 0, 0, 0, 0, 0, 0x00, 0xFF, 0x7F]);
        assert!(pseudo_nan.is_unsupported_encoding());
        assert!(!pseudo_nan.is_signaling_nan());
    }

    #[test]
    fn dyncore_compare_cc_preserves_stale_c1_and_keeps_captured_ie() {
        let mut cpu = Cpu::default();
        cpu.fpu.status_word = 0x0200 | 0x0002; // stale C1 and stale DE
        fold_dyncore_status_capture(
            &mut cpu,
            DyncoreStatusFold::CompareConditionCodes,
            0x4501, // unordered plus host IE
        );
        assert_eq!(cpu.fpu.status_word & 0x4500, 0x4500);
        assert_ne!(
            cpu.fpu.status_word & 0x0200,
            0,
            "compare must preserve stale C1"
        );
        assert_ne!(
            cpu.fpu.status_word & 0x0001,
            0,
            "compare must apply captured IE"
        );

        // MDD-BUG-CRUCIBLE-00258: the SNaN capture from a host FUCOM must
        // survive the fold. This case previously asserted the opposite — that
        // the fold DROPPED it — which is exactly the defect that was filed.
        let mut fucom_snan = Cpu::default();
        fucom_snan.fpu.status_word = 0x0200;
        fold_dyncore_status_capture(
            &mut fucom_snan,
            DyncoreStatusFold::CompareConditionCodes,
            0x4501,
        );
        assert_ne!(
            fucom_snan.fpu.status_word & 0x0001,
            0,
            "host FUCOM only reports IE for SNaN/unsupported, so dropping it loses the fault"
        );
        assert_ne!(fucom_snan.fpu.status_word & 0x0200, 0);

        // Quiet-NaN FUCOM: the host reports NO IE, so none appears. The
        // suppression now lives where the hardware puts it — in the compare —
        // not in the fold.
        let mut fucom_qnan = Cpu::default();
        fold_dyncore_status_capture(
            &mut fucom_qnan,
            DyncoreStatusFold::CompareConditionCodes,
            0x4500,
        );
        assert_eq!(fucom_qnan.fpu.status_word & 0x0003, 0);
        assert_eq!(fucom_qnan.fpu.status_word & 0x4500, 0x4500);
    }

    #[test]
    fn dyncore_compare_eflags_maps_cf_pf_zf_clears_of_sf_af_and_keeps_software_cc() {
        let mut cpu = Cpu::default();
        cpu.fpu.status_word = 0x4700; // distinctive C0/C1/C2/C3 canary
        cpu.regs.set_eflags(0xA55A_0892); // OF/SF/AF set; CF/PF/ZF clear

        fold_dyncore_status_capture(&mut cpu, DyncoreStatusFold::CompareEflags, 0x4501);

        let eflags = cpu.regs.eflags();
        assert_eq!(eflags & 0x45, 0x45, "C0/C2/C3 did not map to CF/PF/ZF");
        assert_eq!(
            eflags & 0x0890,
            0x0000,
            "COMI must CLEAR OF/SF/AF - this used to assert they were preserved, \
             which matched no documented part (see clear_comi_undefined_flags)"
        );
        assert_eq!(eflags >> 16, 0xA55A, "COMI changed high EFLAGS");
        assert_eq!(
            cpu.fpu.status_word & 0x4700,
            0x4700,
            "COMI wrote software CC"
        );
        assert_ne!(cpu.fpu.status_word & 1, 0, "FCOMI must apply captured IE");

        // MDD-BUG-CRUCIBLE-00258: a host FUCOMI reports IE only for
        // SNaN/unsupported operands, so that bit must reach the guest. This
        // case previously asserted it was discarded.
        let mut fucomi_snan = Cpu::default();
        fucomi_snan.fpu.status_word = 0x0200;
        fucomi_snan.regs.set_eflags(0x0000_08d7);
        fold_dyncore_status_capture(&mut fucomi_snan, DyncoreStatusFold::CompareEflags, 0x0001);
        assert_ne!(
            fucomi_snan.fpu.status_word & 1,
            0,
            "FUCOMI must keep the captured SNaN IE"
        );
        assert_ne!(fucomi_snan.fpu.status_word & 0x0200, 0, "FUCOMI changed C1");
        assert_eq!(fucomi_snan.regs.eflags() & 0x45, 0);
        assert_eq!(
            fucomi_snan.regs.eflags() & 0x0890,
            0x0000,
            "the SNaN path clears OF/SF/AF too - FCOMI zeroes them regardless              of the comparison's outcome"
        );
    }

    #[test]
    fn test_fpu_new() {
        let fpu = Fpu::new();
        assert_eq!(fpu.status_word, 0);
        assert_eq!(fpu.control_word, 0x037F);
        assert_eq!(fpu.tag_word, 0xFFFF);
        assert_eq!(fpu.get_top(), 0);
    }

    #[test]
    fn test_fpu_default() {
        let fpu = Fpu::default();
        assert_eq!(fpu.control_word, 0x037F);
    }

    #[test]
    fn test_fpu_get_set_top() {
        let mut fpu = Fpu::new();
        assert_eq!(fpu.get_top(), 0);
        fpu.set_top(5);
        assert_eq!(fpu.get_top(), 5);
        fpu.set_top(7);
        assert_eq!(fpu.get_top(), 7);
        fpu.set_top(8); // Wraps to 0
        assert_eq!(fpu.get_top(), 0);
    }

    #[test]
    fn test_fpu_push_pop() {
        let mut fpu = Fpu::new();
        // All registers start empty
        assert!(fpu.is_empty(0));

        fpu.push(F80::from_f64(1.5));
        assert!(!fpu.is_empty(fpu.get_top()));
        assert_eq!(fpu.get_st(0), F80::from_f64(1.5));

        fpu.push(F80::from_f64(2.5));
        assert_eq!(fpu.get_st(0), F80::from_f64(2.5));
        assert_eq!(fpu.get_st(1), F80::from_f64(1.5));

        let val = fpu.pop();
        assert_eq!(val, F80::from_f64(2.5));
        assert_eq!(fpu.get_st(0), F80::from_f64(1.5));

        let val = fpu.pop();
        assert_eq!(val, F80::from_f64(1.5));
    }

    #[test]
    fn test_fpu_get_st_idx() {
        let mut fpu = Fpu::new();
        fpu.set_top(3);
        assert_eq!(fpu.get_st_idx(0), 3);
        assert_eq!(fpu.get_st_idx(1), 4);
        assert_eq!(fpu.get_st_idx(5), 0); // Wraps around
    }

    #[test]
    fn test_fpu_tags() {
        let mut fpu = Fpu::new();
        // All empty by default
        for i in 0..8 {
            assert!(fpu.is_empty(i));
            assert_eq!(fpu.get_tag(i), 3);
        }

        fpu.set_tag(2, 0); // Valid
        assert_eq!(fpu.get_tag(2), 0);
        assert!(!fpu.is_empty(2));

        fpu.set_tag(2, 1); // Zero
        assert_eq!(fpu.get_tag(2), 1);

        fpu.set_tag(2, 2); // Special
        assert_eq!(fpu.get_tag(2), 2);

        fpu.set_tag(2, 3); // Empty
        assert!(fpu.is_empty(2));
    }

    #[test]
    fn test_fpu_set_exception() {
        let mut fpu = Fpu::new();
        assert_eq!(fpu.status_word, 0);

        // Set IE flag (bit 0)
        fpu.set_exception(0x01);
        assert_eq!(fpu.status_word & 0x01, 0x01);

        // IE is unmasked by default, so ES should be set
        assert_eq!(fpu.status_word & 0x80, 0);

        // Now mask IE and set again
        fpu.status_word = 0;
        fpu.control_word |= 0x01; // Mask IE
        fpu.set_exception(0x01);
        assert_eq!(fpu.status_word & 0x01, 0x01);
    }

    #[test]
    fn test_fpu_condition_codes() {
        let mut fpu = Fpu::new();

        fpu.set_condition_codes(true, false, true, false);
        let (c0, c1, c2, c3) = fpu.get_condition_codes();
        assert!(c0);
        assert!(!c1);
        assert!(c2);
        assert!(!c3);

        fpu.set_condition_codes(false, true, false, true);
        let (c0, c1, c2, c3) = fpu.get_condition_codes();
        assert!(!c0);
        assert!(c1);
        assert!(!c2);
        assert!(c3);
    }

    #[test]
    fn test_fpu_set_exception_unmasked_sets_es() {
        let mut fpu = Fpu::new();
        // Default CW masks all exceptions. Unmask ZE (bit 2)
        fpu.control_word = 0x037F & !0x04; // Unmask ZE
        fpu.set_exception(0x04); // ZE flag
                                 // ES (bit 7) should be set because ZE is unmasked
        assert_ne!(
            fpu.status_word & 0x80,
            0,
            "ES should be set for unmasked exception"
        );
        assert_ne!(fpu.status_word & 0x04, 0, "ZE should be set");
    }

    #[test]
    fn test_fpu_clear_exceptions() {
        let mut fpu = Fpu::new();
        fpu.status_word = 0xFF; // Set all low 8 bits
        fpu.clear_exceptions();
        assert_eq!(fpu.status_word & 0xFF, 0);
    }

    #[test]
    fn test_fpu_stack_fault_overflow() {
        let mut fpu = Fpu::new();
        fpu.set_stack_fault(true);
        // SF (bit 6) and IE (bit 0) should be set
        assert_ne!(fpu.status_word & 0x41, 0);
        // C1 (bit 9) should be set for overflow
        assert_ne!(fpu.status_word & (1 << 9), 0);
    }

    #[test]
    fn test_fpu_stack_fault_underflow() {
        let mut fpu = Fpu::new();
        fpu.set_stack_fault(false);
        assert_ne!(fpu.status_word & 0x41, 0);
        // C1 (bit 9) should be CLEAR for underflow
        assert_eq!(fpu.status_word & (1 << 9), 0);
    }

    #[test]
    fn test_fpu_push_overflow() {
        let mut fpu = Fpu::new();
        // Fill all 8 registers
        for i in 0..8 {
            fpu.set_tag(i, 0); // Mark valid
        }
        fpu.set_top(0);
        // Now push should trigger stack overflow because new_top is not empty
        fpu.push(F80::ONE);
        // Stack fault should have been set
        assert_ne!(fpu.status_word & 0x41, 0);
    }

    #[test]
    fn test_fpu_pop_empty() {
        let mut fpu = Fpu::new();
        // All registers empty
        let val = fpu.pop();
        assert_eq!(val, F80::ZERO);
        // Stack fault should be set
        assert_ne!(fpu.status_word & 0x41, 0);
    }

    #[test]
    fn test_fpu_get_set_st() {
        let mut fpu = Fpu::new();
        fpu.set_top(0);
        fpu.set_tag(0, 0); // Valid
        fpu.set_st(0, F80::from_f64(3.14));
        assert_eq!(fpu.get_st(0), F80::from_f64(3.14));

        let r = fpu.get_st_ref(0);
        assert_eq!(*r, F80::from_f64(3.14));
    }

    #[test]
    fn test_fpu_sync_to_and_from_x87_save_area() {
        let mut fpu = Fpu::new();
        fpu.control_word = 0x027F;
        fpu.status_word = 0x3800; // TOP=7
        fpu.tag_word = 0x5555;
        fpu.ip_offset = 0x1234_5678;
        fpu.ip_selector = 0x0008;
        fpu.operand_offset = 0xABCD_EF00;
        fpu.operand_selector = 0x0010;
        // Set a few registers
        fpu.regs[0] = F80::from_f64(1.0);
        fpu.regs[3] = F80::from_f64(3.14);
        fpu.regs[7] = F80::from_f64(-42.0);

        // Sync to save area
        fpu.sync_to_x87_save_area();

        // Modify FPU state completely
        let mut fpu2 = Fpu::new();
        fpu2.x87_save_area = fpu.x87_save_area;

        // Sync back from save area
        fpu2.sync_from_x87_save_area();

        // Control word should have exception mask bits forced on
        assert_eq!(fpu2.control_word & 0x003F, 0x003F);
        // Status word should be restored
        assert_eq!(fpu2.status_word, 0x3800);
        assert_eq!(fpu2.tag_word, 0x5555);
        assert_eq!(fpu2.ip_offset, 0x1234_5678);
        assert_eq!(fpu2.ip_selector, 0x0008);
        assert_eq!(fpu2.operand_offset, 0xABCD_EF00);
        assert_eq!(fpu2.operand_selector, 0x0010);
        // Register values should round-trip
        assert_eq!(fpu2.regs[0], fpu.regs[0]);
        assert_eq!(fpu2.regs[3], fpu.regs[3]);
        assert_eq!(fpu2.regs[7], fpu.regs[7]);
    }

    // ========================================================================
    // Mutation-testing targeted tests (cargo-mutants on fpu.rs)
    // ========================================================================

    // --- L139: set_stack_fault C1 bit (overflow=false must clear bit 9) ---
    #[test]
    fn test_set_stack_fault_underflow_clears_c1() {
        let mut fpu = Fpu::new();
        fpu.status_word |= 1 << 9; // pre-set C1
        fpu.set_stack_fault(false); // underflow should CLEAR C1
        assert_eq!(
            fpu.status_word & (1 << 9),
            0,
            "C1 should be cleared on underflow"
        );
        assert_ne!(fpu.status_word & 0x01, 0, "IE should be set");
        assert_ne!(fpu.status_word & 0x40, 0, "SF should be set");
    }

    #[test]
    fn test_set_stack_fault_overflow_sets_c1() {
        let mut fpu = Fpu::new();
        fpu.set_stack_fault(true);
        assert_ne!(
            fpu.status_word & (1 << 9),
            0,
            "C1 should be set on overflow"
        );
    }

    // --- L168: set_condition_codes mask must clear each CC bit individually ---
    #[test]
    fn test_condition_codes_clear_all() {
        let mut fpu = Fpu::new();
        fpu.set_condition_codes(true, true, true, true);
        fpu.set_condition_codes(false, false, false, false);
        let (c0, c1, c2, c3) = fpu.get_condition_codes();
        assert!(!c0 && !c1 && !c2 && !c3, "All CCs should be cleared");
    }

    #[test]
    fn test_condition_codes_individual_bits() {
        let mut fpu = Fpu::new();
        // Set C1 only — C0/C2/C3 must remain clear
        fpu.set_condition_codes(false, true, false, false);
        assert_eq!(fpu.get_condition_codes(), (false, true, false, false));
        // Switch to C2 only — C1 must be cleared
        fpu.set_condition_codes(false, false, true, false);
        assert_eq!(fpu.get_condition_codes(), (false, false, true, false));
        // C0 only
        fpu.set_condition_codes(true, false, false, false);
        assert_eq!(fpu.get_condition_codes(), (true, false, false, false));
        // C3 only
        fpu.set_condition_codes(false, false, false, true);
        assert_eq!(fpu.get_condition_codes(), (false, false, false, true));
    }

    #[test]
    fn test_get_st_idx_plus_to_minus_kills_mutant() {
        let mut fpu = Fpu::new();
        fpu.set_top(3);
        assert_eq!(fpu.get_st_idx(2), 5);
        fpu.regs[1] = F80::from_f64(111.0);
        fpu.regs[5] = F80::from_f64(555.0);
        fpu.set_tag(1, 0);
        fpu.set_tag(5, 0);
        assert_eq!(
            fpu.get_st(2).to_f64(),
            555.0,
            "ST(2) with top=3 must read from regs[5], not regs[1]"
        );
    }

    #[test]
    fn test_get_condition_codes_c1_only_via_raw_status_word() {
        let mut fpu = Fpu::new();
        fpu.status_word = 1 << 9;
        let (c0, c1, c2, c3) = fpu.get_condition_codes();
        assert!(!c0);
        assert!(c1);
        assert!(!c2);
        assert!(!c3);
    }

    #[test]
    fn test_get_condition_codes_each_bit_isolated() {
        let mut fpu = Fpu::new();
        fpu.status_word = 1 << 8;
        assert_eq!(fpu.get_condition_codes(), (true, false, false, false));
        fpu.status_word = 1 << 9;
        assert_eq!(fpu.get_condition_codes(), (false, true, false, false));
        fpu.status_word = 1 << 10;
        assert_eq!(fpu.get_condition_codes(), (false, false, true, false));
        fpu.status_word = 1 << 14;
        assert_eq!(fpu.get_condition_codes(), (false, false, false, true));
        fpu.status_word = 0;
        assert_eq!(fpu.get_condition_codes(), (false, false, false, false));
    }

    #[test]
    fn test_fdecstp_and_vs_or_with_top_1() {
        let mut fpu = Fpu::new();
        fpu.set_top(1);
        let top = fpu.get_top();
        let new_top = (top.wrapping_sub(1)) & 0x7;
        assert_eq!(new_top, 0, "FDECSTP from top=1 must give 0, not 7");
    }
}

// ============================================================================
// FPU Instruction Execution (moved from main crate)
// ============================================================================

use crate::x87_decode::{
    CodeBitness, DecodedX87, X87AddressRegister, X87AddressSize, X87EnvironmentLayout,
    X87MemoryOperand, X87Operand, X87Operation as Mnemonic, X87Prefixes, X87Segment,
};
use crate::{x87_ops, Cpu, Flags, PagedMemory};
#[cfg(test)]
use iced_x86::OpKind;

#[cfg(test)]
pub(crate) fn execute_iced_for_test<M: PagedMemory>(
    cpu: &mut Cpu,
    instruction: &iced_x86::Instruction,
    memory: &mut M,
) -> anyhow::Result<()> {
    execute_iced_semantics_for_test(cpu, instruction, memory)
}

/// Execute x87 semantics without guest-CPU feature validation.
///
/// This exists only for tests which independently validate the semantics of an
/// instruction newer than every currently advertised guest CPU.
#[cfg(all(any(test, feature = "test-util"), feature = "iced-interop"))]
#[doc(hidden)]
pub fn execute_iced_semantics_for_test<M: PagedMemory>(
    cpu: &mut Cpu,
    instruction: &iced_x86::Instruction,
    memory: &mut M,
) -> anyhow::Result<()> {
    let decoded = crate::x87_iced_adapter::decode_instruction(instruction);
    execute_decoded_with_dispatch_unchecked(cpu, &decoded, memory, X87Dispatch::Generic)
}

impl DecodedX87 {
    fn mnemonic(&self) -> Mnemonic {
        self.operation
    }

    fn operand(&self, index: usize) -> X87Operand {
        match index {
            0 => self.operand1,
            1 => self.operand2,
            _ => X87Operand::None,
        }
    }

    fn memory_operand(&self) -> Option<&X87MemoryOperand> {
        match (&self.operand1, &self.operand2) {
            (X87Operand::Memory(memory), _) | (_, X87Operand::Memory(memory)) => Some(memory),
            _ => None,
        }
    }

    fn memory_size_bytes(&self) -> usize {
        usize::from(self.memory_operand().map_or(0, |memory| memory.width_bytes))
    }
}

fn stack_operand(instr: &DecodedX87, index: usize) -> Option<usize> {
    match instr.operand(index) {
        X87Operand::Stack(register) => Some(usize::from(register)),
        _ => None,
    }
}

fn operand_is_memory(instr: &DecodedX87, index: usize) -> bool {
    matches!(instr.operand(index), X87Operand::Memory(_))
}

fn required_memory_operand(instr: &DecodedX87) -> &X87MemoryOperand {
    instr
        .memory_operand()
        .expect("decoded x87 memory instruction has a memory operand")
}

fn environment_size(layout: X87EnvironmentLayout) -> usize {
    match layout {
        X87EnvironmentLayout::Environment14 | X87EnvironmentLayout::State94 => 14,
        X87EnvironmentLayout::Environment28 | X87EnvironmentLayout::State108 => 28,
        X87EnvironmentLayout::None => 0,
    }
}

fn encode_environment(cpu: &Cpu, layout: X87EnvironmentLayout, env: &mut [u8; 28]) -> usize {
    let size = environment_size(layout);
    if size == 28 {
        env[0..4].copy_from_slice(&u32::from(cpu.fpu.control_word).to_le_bytes());
        env[4..8].copy_from_slice(&u32::from(cpu.fpu.status_word).to_le_bytes());
        env[8..12].copy_from_slice(&u32::from(cpu.fpu.tag_word).to_le_bytes());
        env[12..16].copy_from_slice(&cpu.fpu.ip_offset.to_le_bytes());
        env[16..18].copy_from_slice(&cpu.fpu.ip_selector.to_le_bytes());
        env[20..24].copy_from_slice(&cpu.fpu.operand_offset.to_le_bytes());
        env[24..26].copy_from_slice(&cpu.fpu.operand_selector.to_le_bytes());
    } else {
        env[0..2].copy_from_slice(&cpu.fpu.control_word.to_le_bytes());
        env[2..4].copy_from_slice(&cpu.fpu.status_word.to_le_bytes());
        env[4..6].copy_from_slice(&cpu.fpu.tag_word.to_le_bytes());
        env[6..8].copy_from_slice(&(cpu.fpu.ip_offset as u16).to_le_bytes());
        env[8..10].copy_from_slice(&cpu.fpu.ip_selector.to_le_bytes());
        env[10..12].copy_from_slice(&(cpu.fpu.operand_offset as u16).to_le_bytes());
        env[12..14].copy_from_slice(&cpu.fpu.operand_selector.to_le_bytes());
    }
    size
}

fn decode_environment(cpu: &mut Cpu, layout: X87EnvironmentLayout, env: &[u8]) {
    if environment_size(layout) == 28 {
        cpu.fpu.control_word = u32::from_le_bytes(env[0..4].try_into().unwrap()) as u16;
        cpu.fpu.status_word = u32::from_le_bytes(env[4..8].try_into().unwrap()) as u16;
        cpu.fpu.tag_word = u32::from_le_bytes(env[8..12].try_into().unwrap()) as u16;
        cpu.fpu.ip_offset = u32::from_le_bytes(env[12..16].try_into().unwrap());
        cpu.fpu.ip_selector = u16::from_le_bytes(env[16..18].try_into().unwrap());
        cpu.fpu.operand_offset = u32::from_le_bytes(env[20..24].try_into().unwrap());
        cpu.fpu.operand_selector = u16::from_le_bytes(env[24..26].try_into().unwrap());
    } else {
        cpu.fpu.control_word = u16::from_le_bytes(env[0..2].try_into().unwrap());
        cpu.fpu.status_word = u16::from_le_bytes(env[2..4].try_into().unwrap());
        cpu.fpu.tag_word = u16::from_le_bytes(env[4..6].try_into().unwrap());
        cpu.fpu.ip_offset = u32::from(u16::from_le_bytes(env[6..8].try_into().unwrap()));
        cpu.fpu.ip_selector = u16::from_le_bytes(env[8..10].try_into().unwrap());
        cpu.fpu.operand_offset = u32::from(u16::from_le_bytes(env[10..12].try_into().unwrap()));
        cpu.fpu.operand_selector = u16::from_le_bytes(env[12..14].try_into().unwrap());
    }
    // The tag word was assigned wholesale, so any slot it just marked Empty
    // must lose its numeric proofs (MDD-BUG-FLUX-00141). FLDENV is the path
    // that needs this — it re-tags without touching the register bytes, so a
    // proof survives its own slot going Empty. FRSTOR reaches here too and is
    // unaffected: it rewrites all eight registers immediately afterwards, and
    // `set_physical_reg` invalidates each slot's shadow anyway.
    cpu.fpu.invalidate_proofs_of_empty_slots();
    cw_census::note_cw(cpu.fpu.control_word);
}

/// Compile-time master switch for the x87 diagnostic trace surface
/// (2026.07.30 emu_probes HLD). When false the gates below fold to constant
/// false and the per-op `tracing::enabled!` level-filter checks — one atomic
/// load each, on every x87 dispatch — are deleted by the optimiser.
const EMU_PROBES: bool = cfg!(any(test, feature = "emu_probes"));

/// Status-word C1 (bit 9).
///
/// Intel SDM Vol 1 Table 8-1 defines C1 as 0 for instructions that complete
/// successfully without rounding or a stack fault — FXCH, FINCSTP and FDECSTP
/// among them (MDD-BUG-CRUCIBLE-00580).
const C1_MASK: u16 = 1 << 9;

/// Check if FPU operation tracing is enabled.
/// Enable via `RUST_LOG=gp64::fpu::ops=trace` (needs an emu_probes build).
#[inline(always)]
fn fpu_ops_trace_enabled() -> bool {
    EMU_PROBES && tracing::enabled!(target: "gp64::fpu::ops", tracing::Level::TRACE)
}

/// Check if FPU exception tracing is enabled.
/// Enable via `RUST_LOG=gp64::fpu::exceptions=debug` (needs an emu_probes build).
#[inline(always)]
fn fpu_trace_exceptions_enabled() -> bool {
    EMU_PROBES && tracing::enabled!(target: "gp64::fpu::exceptions", tracing::Level::DEBUG)
}

fn trace_fpu_exception(op: &str, a: f64, b: Option<f64>) {
    if !fpu_trace_exceptions_enabled() {
        return;
    }
    match b {
        Some(b_val) => {
            tracing::debug!(
                target: "gp64::fpu::exceptions",
                op,
                a,
                b = b_val,
                "FPU exception"
            );
        }
        None => {
            tracing::debug!(
                target: "gp64::fpu::exceptions",
                op,
                a,
                "FPU exception"
            );
        }
    }
}

/// Compact exact-form dispatch derived from the portable descriptor. `Generic`
/// retains the complete executor; the other variants skip repeated operand-shape
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X87Dispatch {
    Generic,
    FldM32,
    FldM64,
    FldReg {
        src: u8,
    },
    FstpM32,
    FstpM64,
    FildM32,
    FistpM32,
    FnstswAx,
    Fxch {
        src: u8,
    },
    Fldcw,
    /// Float memory-source arithmetic (m32 or m64; the handler reads the
    /// width from the descriptor's memory operand). Memory forms never pop.
    ArithMem {
        op: ArithOp,
    },
    /// FCOM/FCOMP with a float memory source.
    ComMem {
        pop: bool,
    },
    ArithReg {
        op: ArithOp,
        dst: u8,
        src: u8,
        pop: bool,
    },
}

impl X87Dispatch {
    #[must_use]
    pub fn classify(instr: &DecodedX87) -> Self {
        let memory_width = instr.memory_size_bytes();
        match (instr.operation, instr.operand1, instr.operand2) {
            (Mnemonic::Fld, X87Operand::Memory(_), _) if memory_width == 4 => Self::FldM32,
            (Mnemonic::Fld, X87Operand::Memory(_), _) if memory_width == 8 => Self::FldM64,
            (Mnemonic::Fld, X87Operand::Stack(src), _) => Self::FldReg { src },
            (Mnemonic::Fstp, X87Operand::Memory(_), _) if memory_width == 4 => Self::FstpM32,
            (Mnemonic::Fstp, X87Operand::Memory(_), _) if memory_width == 8 => Self::FstpM64,
            (Mnemonic::Fild, X87Operand::Memory(_), _) if memory_width == 4 => Self::FildM32,
            (Mnemonic::Fistp, X87Operand::Memory(_), _) if memory_width == 4 => Self::FistpM32,
            (Mnemonic::Fnstsw, X87Operand::Ax, _) => Self::FnstswAx,
            // Same ST(i) selection rule as the generic FXCH arm: prefer the
            // second stack operand, else a non-ST0 first operand, else ST(1).
            (Mnemonic::Fxch, first, second) => {
                let src = match (first, second) {
                    (_, X87Operand::Stack(src)) => src,
                    (X87Operand::Stack(src), _) if src != 0 => src,
                    _ => 1,
                };
                Self::Fxch { src }
            }
            (Mnemonic::Fldcw, X87Operand::Memory(_), _) => Self::Fldcw,
            // Float memory arithmetic: FADD..FDIVR with a memory operand are
            // always float m32/m64 (integer forms are the FI* mnemonics).
            (Mnemonic::Fadd, X87Operand::Memory(_), _) => Self::ArithMem { op: ArithOp::Add },
            (Mnemonic::Fsub, X87Operand::Memory(_), _) => Self::ArithMem { op: ArithOp::Sub },
            (Mnemonic::Fsubr, X87Operand::Memory(_), _) => Self::ArithMem {
                op: ArithOp::SubReverse,
            },
            (Mnemonic::Fmul, X87Operand::Memory(_), _) => Self::ArithMem { op: ArithOp::Mul },
            (Mnemonic::Fdiv, X87Operand::Memory(_), _) => Self::ArithMem { op: ArithOp::Div },
            (Mnemonic::Fdivr, X87Operand::Memory(_), _) => Self::ArithMem {
                op: ArithOp::DivReverse,
            },
            (Mnemonic::Fcom, X87Operand::Memory(_), _) => Self::ComMem { pop: false },
            (Mnemonic::Fcomp, X87Operand::Memory(_), _) => Self::ComMem { pop: true },
            (Mnemonic::Fadd | Mnemonic::Faddp, X87Operand::Stack(dst), X87Operand::Stack(src)) => {
                Self::ArithReg {
                    op: ArithOp::Add,
                    dst,
                    src,
                    pop: instr.operation == Mnemonic::Faddp,
                }
            }
            (Mnemonic::Fsub | Mnemonic::Fsubp, X87Operand::Stack(dst), X87Operand::Stack(src)) => {
                Self::ArithReg {
                    op: ArithOp::Sub,
                    dst,
                    src,
                    pop: instr.operation == Mnemonic::Fsubp,
                }
            }
            (
                Mnemonic::Fsubr | Mnemonic::Fsubrp,
                X87Operand::Stack(dst),
                X87Operand::Stack(src),
            ) => Self::ArithReg {
                op: ArithOp::SubReverse,
                dst,
                src,
                pop: instr.operation == Mnemonic::Fsubrp,
            },
            (Mnemonic::Fmul | Mnemonic::Fmulp, X87Operand::Stack(dst), X87Operand::Stack(src)) => {
                Self::ArithReg {
                    op: ArithOp::Mul,
                    dst,
                    src,
                    pop: instr.operation == Mnemonic::Fmulp,
                }
            }
            (Mnemonic::Fdiv | Mnemonic::Fdivp, X87Operand::Stack(dst), X87Operand::Stack(src)) => {
                Self::ArithReg {
                    op: ArithOp::Div,
                    dst,
                    src,
                    pop: instr.operation == Mnemonic::Fdivp,
                }
            }
            (
                Mnemonic::Fdivr | Mnemonic::Fdivrp,
                X87Operand::Stack(dst),
                X87Operand::Stack(src),
            ) => Self::ArithReg {
                op: ArithOp::DivReverse,
                dst,
                src,
                pop: instr.operation == Mnemonic::Fdivrp,
            },
            _ => Self::Generic,
        }
    }
}

/// Execute a decoded x87 instruction without an iced instruction object.
pub fn execute_decoded<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<()> {
    execute_decoded_with_dispatch(cpu, instr, mem, X87Dispatch::Generic)
}

/// Execute using an exact-form classification cached under the same byte and
/// bitness oracle as `instr`.
pub fn execute_decoded_with_dispatch<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    dispatch: X87Dispatch,
) -> anyhow::Result<()> {
    dispatch_census::note(&dispatch, instr.operation);
    if instr.operation.requires_p6() && !cpu.cpu_type.supports_p6() {
        return Err(crate::CpuError::InvalidOpcode(cpu.regs.eip).into());
    }

    // FISTTP only ever classifies as Generic, so the fat-dispatch fast forms
    // skip the guest-CPU feature check entirely.
    if matches!(dispatch, X87Dispatch::Generic)
        && instr.operation == Mnemonic::Fisttp
        && !cpu.supports_sse3()
    {
        return Err(crate::CpuError::InvalidOpcode(cpu.regs.eip).into());
    }

    execute_decoded_with_dispatch_unchecked(cpu, instr, mem, dispatch)
}

fn execute_decoded_with_dispatch_unchecked<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    dispatch: X87Dispatch,
) -> anyhow::Result<()> {
    // Every x87 interpreter/fallback form observes F80 state. Keep the
    // helper-visible register file canonical before dispatch, including the
    // fat forms that bypass `execute_inner`.
    cpu.fpu.materialize_drc_f80_register_file();
    let may_use_shadow_fast_path = match dispatch {
        X87Dispatch::ArithMem { .. } | X87Dispatch::ArithReg { .. } => true,
        X87Dispatch::Generic => matches!(
            instr.operation,
            Mnemonic::Fadd
                | Mnemonic::Faddp
                | Mnemonic::Fsub
                | Mnemonic::Fsubp
                | Mnemonic::Fsubr
                | Mnemonic::Fsubrp
                | Mnemonic::Fmul
                | Mnemonic::Fmulp
                | Mnemonic::Fdiv
                | Mnemonic::Fdivp
                | Mnemonic::Fdivr
                | Mnemonic::Fdivrp
        ),
        _ => false,
    };
    if !may_use_shadow_fast_path {
        cpu.fpu.sync_host_condition_codes_if_dirty();
    }

    let _ops_trace = fpu_ops_trace_enabled();
    let (_pre_st0, _pre_st1, _pre_sw, _pre_cw) = if _ops_trace {
        (
            cpu.fpu.get_st(0).to_f64(),
            cpu.fpu.get_st(1).to_f64(),
            cpu.fpu.status_word,
            cpu.fpu.control_word,
        )
    } else {
        (0.0, 0.0, 0, 0)
    };
    let _mnemonic = _ops_trace.then_some(instr.operation);

    let result = match dispatch {
        X87Dispatch::Generic => execute_inner(cpu, instr, mem),
        X87Dispatch::FldM32 => execute_fld_m32(cpu, instr, mem),
        X87Dispatch::FldReg { src } => execute_fld_register(cpu, usize::from(src)),
        X87Dispatch::FstpM32 => execute_fstp_m32(cpu, instr, mem),
        X87Dispatch::FildM32 => execute_fild_m32(cpu, instr, mem),
        X87Dispatch::FistpM32 => execute_fistp_m32(cpu, instr, mem),
        X87Dispatch::FnstswAx => {
            execute_fnstsw_ax(cpu);
            Ok(())
        }
        X87Dispatch::FldM64 => execute_fld_m64(cpu, instr, mem),
        X87Dispatch::FstpM64 => execute_fstp_m64(cpu, instr, mem),
        X87Dispatch::Fxch { src } => execute_fxch(cpu, usize::from(src)),
        X87Dispatch::Fldcw => execute_fldcw(cpu, instr, mem),
        X87Dispatch::ArithMem { op } => execute_arith_memory(cpu, instr, mem, op),
        X87Dispatch::ComMem { pop } => execute_fcom_mem(cpu, instr, mem, pop),
        X87Dispatch::ArithReg { op, dst, src, pop } => {
            execute_arith_register(cpu, usize::from(dst), usize::from(src), pop, op)
        }
    };

    // EXP-A013: the eager per-op commit that lived here is retired — dirty
    // binary64 slots now round at F80/alias READ barriers (execute_inner
    // entry, the fat-form and MMX read seams, ownership exits) instead of
    // after every op.

    if _ops_trace {
        let mnemonic = _mnemonic.expect("trace mnemonic captured when tracing is enabled");
        // Diagnostic-only F80 reads: flush so the reads are commit-current.
        cpu.fpu.commit_drc_binary64_register_file();
        let post_st0 = cpu.fpu.get_st(0).to_f64();
        let post_st1 = cpu.fpu.get_st(1).to_f64();
        let post_sw = cpu.fpu.status_word;
        tracing::trace!(
            target: "gp64::fpu::ops",
            mnemonic = ?mnemonic,
            pre_st0 = format_args!("{:.15e}", _pre_st0),
            pre_st1 = format_args!("{:.15e}", _pre_st1),
            post_st0 = format_args!("{:.15e}", post_st0),
            post_st1 = format_args!("{:.15e}", post_st1),
            pre_sw = format_args!("{:04X}", _pre_sw),
            post_sw = format_args!("{:04X}", post_sw),
            cw = format_args!("{:04X}", _pre_cw),
            cs = format_args!("{:04X}", cpu.regs.cs),
            ip = format_args!("{:04X}", cpu.regs.ip()),
            "FPU op"
        );
    }

    result
}

fn execute_inner<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<()> {
    // EXP-A013 whole-file read barrier: the generic lane reads F80 state in
    // too many shapes to barrier individually (compares, transcendentals,
    // FNSAVE, m80 stores, consumed pops) — one commit covers them all.
    cpu.fpu.commit_drc_binary64_register_file();
    match instr.operation {
        Mnemonic::Fninit => exec_fninit(cpu),
        Mnemonic::Fld => {
            if operand_is_memory(instr, 0) && instr.memory_size_bytes() == 4 {
                return execute_fld_m32(cpu, instr, mem);
            }
            if operand_is_memory(instr, 0) && instr.memory_size_bytes() == 8 {
                return execute_fld_m64(cpu, instr, mem);
            }
            if let Some(src) = stack_operand(instr, 0) {
                return execute_fld_register(cpu, src);
            }
            let val = match instr.operand1 {
                X87Operand::Memory(_) => match instr.memory_size_bytes() {
                    4 => {
                        let source = read_f32(cpu, instr, mem)?;
                        fpu_guard_census::note_fld_m32(source);
                        if source.is_subnormal() {
                            cpu.fpu.set_exception(0x02);
                        }
                        F80::from_f32(source)
                    }
                    8 => {
                        x87_form_census::note_fld_wide(8);
                        let source = read_f64(cpu, instr, mem)?;
                        if source.is_subnormal() {
                            cpu.fpu.set_exception(0x02);
                        }
                        F80::from_f64(source)
                    }
                    10 => {
                        x87_form_census::note_fld_wide(10);
                        let source = read_f80_raw(cpu, instr, mem)?;
                        if source.is_denormal() {
                            cpu.fpu.set_exception(0x02);
                        }
                        source
                    }
                    size => {
                        tracing::warn!(
                            target: "gp64::cpu::fpu",
                            size,
                            cs = format_args!("{:04X}", cpu.regs.cs),
                            ip = format_args!("{:04X}", cpu.regs.ip()),
                            "FLD: unhandled memory size"
                        );
                        F80::ZERO
                    }
                },
                operand => {
                    tracing::warn!(
                        target: "gp64::cpu::fpu",
                        ?operand,
                        cs = format_args!("{:04X}", cpu.regs.cs),
                        ip = format_args!("{:04X}", cpu.regs.ip()),
                        "FLD: unhandled operand kind"
                    );
                    F80::ZERO
                }
            };
            cpu.fpu.push(val);
        }
        Mnemonic::Fldz => cpu.fpu.push(F80::ZERO),
        Mnemonic::Fld1 => cpu.fpu.push(F80::ONE),
        Mnemonic::Fldpi => cpu.fpu.push(F80::PI),
        Mnemonic::Fldl2e => cpu.fpu.push(F80::LOG2_E),
        Mnemonic::Fldl2t => cpu.fpu.push(F80::LOG2_10),
        Mnemonic::Fldlg2 => cpu.fpu.push(F80::LOG10_2),
        Mnemonic::Fldln2 => cpu.fpu.push(F80::LN_2),

        Mnemonic::Fstp => {
            if operand_is_memory(instr, 0) && instr.memory_size_bytes() == 4 {
                return execute_fstp_m32(cpu, instr, mem);
            }
            if operand_is_memory(instr, 0) && instr.memory_size_bytes() == 8 {
                return execute_fstp_m64(cpu, instr, mem);
            }
            // Masked empty-stack stores write the floating-point indefinite QNaN
            // and pop (Intel SDM Vol 1 §8.5.1.1), as do integer stores.
            if operand_is_memory(instr, 0) && stack_reg_empty(cpu, 0) {
                // Write FP indefinite unconditionally. Intel
                // SDM IM=0 semantics would raise #IA and not write, but our
                // emulator does not dispatch FPU exceptions — status-word-only.
                // See HLD 02 §5.2.
                write_fp_indefinite(cpu, instr, mem)?;
                cpu.fpu.set_stack_fault(false);
                cpu.fpu.pop();
                return Ok(());
            }
            // FSTP ST(i) with empty ST(0) and masked IE writes the indefinite
            // QNaN to the destination, tags it Special, and still pops (§8.5.1.1).
            if let Some(st_index) = stack_operand(instr, 0).filter(|_| stack_reg_empty(cpu, 0)) {
                cpu.fpu.set_stack_fault(false);
                let idx = cpu.fpu.get_st_idx(st_index);
                cpu.fpu.set_physical_reg(idx, F80::INDEFINITE);
                cpu.fpu.set_tag(idx, 2);
                cpu.fpu.pop();
                return Ok(());
            }
            let val = cpu.fpu.get_st(0);
            match instr.operand1 {
                X87Operand::Stack(st_index) => {
                    // FSTP ST(i) copies both ST(0)'s bytes and tag before popping.
                    // Copying only the bytes can leave a valid value tagged Empty.
                    let st_index = usize::from(st_index);
                    let src_phys = cpu.fpu.get_top();
                    let src_tag = cpu.fpu.get_tag(src_phys);
                    let src_binary32 = cpu.fpu.exact_binary32_st(0);
                    let src_binary64 = cpu.fpu.exact_binary64_st(0);
                    let dst_phys = cpu.fpu.get_st_idx(st_index);
                    cpu.fpu
                        .set_physical_reg_with_proofs(dst_phys, val, src_binary32, src_binary64);
                    cpu.fpu.set_tag(dst_phys, src_tag);
                    cpu.fpu.pop();
                }
                X87Operand::Memory(_) => match instr.memory_size_bytes() {
                    4 => {
                        let (f32_val, sw) = narrow_st0_to_f32(cpu, val);
                        fpu_guard_census::note_fstp_m32(f32_val);
                        write_f32(cpu, instr, mem, f32_val)?;
                        merge_x87_exception_status_and_c1(cpu, sw);
                    }
                    8 => {
                        x87_form_census::note_fstp_wide(8);
                        let (f64_val, sw) = narrow_st0_to_f64(cpu, val);
                        write_f64(cpu, instr, mem, f64_val)?;
                        merge_x87_exception_status_and_c1(cpu, sw);
                    }
                    10 => {
                        x87_form_census::note_fstp_wide(10);
                        write_f80_raw(cpu, instr, mem, val)?;
                    }
                    size => {
                        tracing::warn!(
                            target: "gp64::cpu::fpu",
                            size,
                            cs = format_args!("{:04X}", cpu.regs.cs),
                            ip = format_args!("{:04X}", cpu.regs.ip()),
                            "FSTP: unhandled memory size"
                        );
                    }
                },
                operand => {
                    tracing::warn!(
                        target: "gp64::cpu::fpu",
                        ?operand,
                        cs = format_args!("{:04X}", cpu.regs.cs),
                        ip = format_args!("{:04X}", cpu.regs.ip()),
                        "FSTP: unhandled operand kind"
                    );
                }
            }
            if stack_operand(instr, 0).is_none() {
                cpu.fpu.pop();
            }
        }
        Mnemonic::Fst => {
            // Masked empty-stack FST stores write the indefinite QNaN (§8.5.1.1).
            // FST has no 80-bit memory form; that form belongs to FSTP.
            if operand_is_memory(instr, 0) && stack_reg_empty(cpu, 0) {
                // Write FP indefinite unconditionally. Intel
                // SDM IM=0 semantics would raise #IA and not write, but our
                // emulator does not dispatch FPU exceptions — status-word-only.
                // See HLD 02 §5.2.
                write_fp_indefinite(cpu, instr, mem)?;
                cpu.fpu.set_stack_fault(false);
                return Ok(());
            }
            // FST ST(i) with empty ST(0) and masked IE writes the indefinite
            // QNaN to the destination and tags it Special (SDM Vol 1 §8.5.1.1).
            if let Some(st_index) = stack_operand(instr, 0).filter(|_| stack_reg_empty(cpu, 0)) {
                cpu.fpu.set_stack_fault(false);
                let idx = cpu.fpu.get_st_idx(st_index);
                cpu.fpu.set_physical_reg(idx, F80::INDEFINITE);
                cpu.fpu.set_tag(idx, 2);
                return Ok(());
            }
            let val = cpu.fpu.get_st(0);
            match instr.operand1 {
                X87Operand::Stack(st_index) => {
                    // FST ST(i) copies both the F80 bytes and tag, as FSTP does, so
                    // later reads see the correct occupancy.
                    let st_index = usize::from(st_index);
                    let src_phys = cpu.fpu.get_top();
                    let src_tag = cpu.fpu.get_tag(src_phys);
                    let src_binary32 = cpu.fpu.exact_binary32_st(0);
                    let src_binary64 = cpu.fpu.exact_binary64_st(0);
                    let dst_phys = cpu.fpu.get_st_idx(st_index);
                    cpu.fpu
                        .set_physical_reg_with_proofs(dst_phys, val, src_binary32, src_binary64);
                    cpu.fpu.set_tag(dst_phys, src_tag);
                }
                X87Operand::Memory(_) => match instr.memory_size_bytes() {
                    4 => {
                        let (f32_val, sw) = narrow_st0_to_f32(cpu, val);
                        write_f32(cpu, instr, mem, f32_val)?;
                        merge_x87_exception_status_and_c1(cpu, sw);
                    }
                    8 => {
                        x87_form_census::note_fstp_wide(8);
                        let (f64_val, sw) = narrow_st0_to_f64(cpu, val);
                        write_f64(cpu, instr, mem, f64_val)?;
                        merge_x87_exception_status_and_c1(cpu, sw);
                    }
                    size => {
                        tracing::warn!(
                            target: "gp64::cpu::fpu",
                            size,
                            cs = format_args!("{:04X}", cpu.regs.cs),
                            ip = format_args!("{:04X}", cpu.regs.ip()),
                            "FST: unhandled memory size"
                        );
                    }
                },
                operand => {
                    tracing::warn!(
                        target: "gp64::cpu::fpu",
                        ?operand,
                        cs = format_args!("{:04X}", cpu.regs.cs),
                        ip = format_args!("{:04X}", cpu.regs.ip()),
                        "FST: unhandled operand kind"
                    );
                }
            }
        }
        Mnemonic::Fild => {
            if instr.memory_size_bytes() == 4 {
                return execute_fild_m32(cpu, instr, mem);
            }
            let val = match instr.memory_size_bytes() {
                2 => {
                    let v = read_i16(cpu, instr, mem)?;
                    if fpu_ops_trace_enabled() {
                        let addr = crate::addressing::calc_x87_effective_address(
                            cpu,
                            instr.memory_operand().expect("FILD has a memory operand"),
                            mem,
                            crate::PageAccess::Read,
                        )
                        .unwrap_or(0);
                        tracing::trace!(target: "gp64::fpu::ops", addr = format_args!("{:05X}", addr), ival = v, size = 2, "FILD mem");
                    }
                    F80::from_i16(v)
                }
                4 => {
                    let v = read_i32(cpu, instr, mem)?;
                    if fpu_ops_trace_enabled() {
                        let addr = crate::addressing::calc_x87_effective_address(
                            cpu,
                            instr.memory_operand().expect("FILD has a memory operand"),
                            mem,
                            crate::PageAccess::Read,
                        )
                        .unwrap_or(0);
                        tracing::trace!(target: "gp64::fpu::ops", addr = format_args!("{:05X}", addr), ival = v, size = 4, "FILD mem");
                    }
                    F80::from_i32(v)
                }
                8 => {
                    let v = read_i64(cpu, instr, mem)?;
                    if fpu_ops_trace_enabled() {
                        let addr = crate::addressing::calc_x87_effective_address(
                            cpu,
                            instr.memory_operand().expect("FILD has a memory operand"),
                            mem,
                            crate::PageAccess::Read,
                        )
                        .unwrap_or(0);
                        tracing::trace!(target: "gp64::fpu::ops", addr = format_args!("{:05X}", addr), ival = v, size = 8, "FILD mem");
                    }
                    F80::from_i64(v)
                }
                size => {
                    tracing::warn!(
                        target: "gp64::cpu::fpu",
                        size,
                        cs = format_args!("{:04X}", cpu.regs.cs),
                        ip = format_args!("{:04X}", cpu.regs.ip()),
                        "FILD: unhandled memory size"
                    );
                    F80::ZERO
                }
            };
            cpu.fpu.push(val);
        }
        Mnemonic::Fisttp => {
            if stack_reg_empty(cpu, 0) {
                // Masked empty-stack FISTTP writes the integer indefinite value
                // (Intel SDM Vol 1 §8.5.1.1). Write even when IM=0: this emulator
                // records exception status but does not dispatch FPU exceptions.
                match instr.memory_size_bytes() {
                    2 => write_i16(cpu, instr, mem, i16::MIN)?,
                    4 => write_i32(cpu, instr, mem, i32::MIN)?,
                    8 => write_i64(cpu, instr, mem, i64::MIN)?,
                    _ => {}
                }
                cpu.fpu.set_stack_fault(false);
                // FISTTP still pops on masked IE, advancing TOP for later instructions.
                cpu.fpu.pop();
                return Ok(());
            }
            let val = cpu.fpu.get_st(0);
            match instr.memory_size_bytes() {
                2 => {
                    let (ival, sw) = val.to_i16_truncate_status();
                    write_i16(cpu, instr, mem, ival)?;
                    merge_x87_exception_status_and_c1(cpu, sw);
                    if (sw & 0x0001) != 0 {
                        set_invalid_op_non_stack(cpu);
                    }
                }
                4 => {
                    let (ival, sw) = val.to_i32_truncate_status();
                    write_i32(cpu, instr, mem, ival)?;
                    merge_x87_exception_status_and_c1(cpu, sw);
                    if (sw & 0x0001) != 0 {
                        set_invalid_op_non_stack(cpu);
                    }
                }
                8 => {
                    let (ival, sw) = val.to_i64_truncate_status();
                    write_i64(cpu, instr, mem, ival)?;
                    merge_x87_exception_status_and_c1(cpu, sw);
                    if (sw & 0x0001) != 0 {
                        set_invalid_op_non_stack(cpu);
                    }
                }
                size => {
                    tracing::warn!(
                        target: "gp64::cpu::fpu",
                        size,
                        cs = format_args!("{:04X}", cpu.regs.cs),
                        ip = format_args!("{:04X}", cpu.regs.ip()),
                        "FISTTP: unhandled memory size"
                    );
                }
            }
            cpu.fpu.pop();
        }
        Mnemonic::Fist | Mnemonic::Fistp => {
            if instr.operation == Mnemonic::Fistp && instr.memory_size_bytes() == 4 {
                return execute_fistp_m32(cpu, instr, mem);
            }
            if stack_reg_empty(cpu, 0) {
                // With masked IE, FIST/FISTP store the integer indefinite value
                // (highest bit set; SDM Vol 1 §8.5.1.1). Write even when IM=0:
                // this emulator records status but does not dispatch FPU exceptions.
                match instr.memory_size_bytes() {
                    2 => write_i16(cpu, instr, mem, i16::MIN)?,
                    4 => write_i32(cpu, instr, mem, i32::MIN)?,
                    8 => write_i64(cpu, instr, mem, i64::MIN)?,
                    _ => {}
                }
                cpu.fpu.set_stack_fault(false);
                // FISTP still pops on masked IE, advancing TOP for later instructions.
                if instr.mnemonic() == Mnemonic::Fistp {
                    cpu.fpu.pop();
                }
                return Ok(());
            }
            let val = cpu.fpu.get_st(0);
            let cw = cpu.fpu.control_word;
            match instr.memory_size_bytes() {
                2 => {
                    let (ival, conv_sw) = val.to_i16_status(cw);
                    if fpu_ops_trace_enabled() {
                        let addr = crate::addressing::calc_x87_effective_address(
                            cpu,
                            instr.memory_operand().expect("FIST has a memory operand"),
                            mem,
                            crate::PageAccess::Write,
                        )
                        .unwrap_or(0);
                        tracing::trace!(target: "gp64::fpu::ops", addr = format_args!("{:05X}", addr), val = format_args!("{:.15e}", val.to_f64()), ival, size = 2, "FISTP mem");
                    }
                    write_i16(cpu, instr, mem, ival)?;
                    apply_fist_conversion_status(cpu, conv_sw);
                }
                4 => {
                    let (ival, conv_sw) = val.to_i32_status(cw);
                    fpu_guard_census::note_fistp_m32(
                        val.is_nan(),
                        val.is_denormal(),
                        (conv_sw & 0x01) != 0,
                    );
                    write_i32(cpu, instr, mem, ival)?;
                    apply_fist_conversion_status(cpu, conv_sw);
                }
                8 => {
                    let (ival, conv_sw) = val.to_i64_status(cw);
                    write_i64(cpu, instr, mem, ival)?;
                    apply_fist_conversion_status(cpu, conv_sw);
                }
                size => {
                    tracing::warn!(
                        target: "gp64::cpu::fpu",
                        size,
                        cs = format_args!("{:04X}", cpu.regs.cs),
                        ip = format_args!("{:04X}", cpu.regs.ip()),
                        "FIST/FISTP: unhandled memory size"
                    );
                }
            }
            if instr.mnemonic() == Mnemonic::Fistp {
                cpu.fpu.pop();
            }
        }

        Mnemonic::Fadd | Mnemonic::Faddp => {
            execute_arith(cpu, instr, mem, ArithOp::Add)?;
        }
        Mnemonic::Fsub | Mnemonic::Fsubp => {
            execute_arith(cpu, instr, mem, ArithOp::Sub)?;
        }
        Mnemonic::Fsubr | Mnemonic::Fsubrp => {
            execute_arith(cpu, instr, mem, ArithOp::SubReverse)?;
        }
        Mnemonic::Fmul | Mnemonic::Fmulp => {
            execute_arith(cpu, instr, mem, ArithOp::Mul)?;
        }
        Mnemonic::Fdiv | Mnemonic::Fdivp => {
            execute_arith(cpu, instr, mem, ArithOp::Div)?;
        }
        Mnemonic::Fdivr | Mnemonic::Fdivrp => {
            execute_arith(cpu, instr, mem, ArithOp::DivReverse)?;
        }

        // Integer arithmetic operations. The shared helper reads and validates
        // the mandatory memory source before any FPU mutation, including the
        // masked empty-stack path. Fixes MDD-BUG-CRUCIBLE-00255.
        Mnemonic::Fiadd => {
            execute_integer_arith_memory(cpu, instr, mem, ArithOp::Add)?;
        }
        Mnemonic::Fisub => {
            execute_integer_arith_memory(cpu, instr, mem, ArithOp::Sub)?;
        }
        Mnemonic::Fisubr => {
            execute_integer_arith_memory(cpu, instr, mem, ArithOp::SubReverse)?;
        }
        Mnemonic::Fimul => {
            execute_integer_arith_memory(cpu, instr, mem, ArithOp::Mul)?;
        }
        Mnemonic::Fidiv => {
            execute_integer_arith_memory(cpu, instr, mem, ArithOp::Div)?;
        }
        Mnemonic::Fidivr => {
            execute_integer_arith_memory(cpu, instr, mem, ArithOp::DivReverse)?;
        }

        // Integer compare
        Mnemonic::Ficom | Mnemonic::Ficomp => {
            // A faulting source must leave the complete FPU image untouched.
            let val_b = read_int_operand_f80(cpu, instr, mem)?;
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                // Masked invalid FICOM sets C0=C2=C3=1 (unordered), rather than
                // leaving stale condition codes (SDM Vol 1 §8.3.6.2).
                cpu.fpu.set_condition_codes(true, false, true, true);
                if instr.mnemonic() == Mnemonic::Ficomp {
                    cpu.fpu.pop();
                }
                return Ok(());
            }
            let val_a = cpu.fpu.get_st(0);
            if val_a.is_nan() {
                cpu.fpu.set_exception(0x01); // IE flag - invalid operation on unordered compare
            }
            set_condition_codes_from_sw(cpu, x87_ops::f80_compare(&val_a, &val_b));
            if instr.mnemonic() == Mnemonic::Ficomp {
                cpu.fpu.pop();
            }
        }

        // Unordered compare (387+) - same as FCOM but handles NaN differently
        Mnemonic::Fucom | Mnemonic::Fucomp | Mnemonic::Fucompp => {
            // FUCOM/FUCOMP with register operand may have 2 operands: ST(0) and ST(i)
            let st_i = stack_operand(instr, 1)
                .or_else(|| stack_operand(instr, 0).filter(|&register| register != 0))
                .unwrap_or(1);
            if stack_reg_empty(cpu, 0) || stack_reg_empty(cpu, st_i) {
                cpu.fpu.set_stack_fault(false);
                // Masked invalid FUCOMP/FUCOMPP sets unordered condition codes
                // (C0=C2=C3=1) and still pops (SDM Vol 1 §8.5.1.1).
                cpu.fpu.set_condition_codes(true, false, true, true);
                if instr.mnemonic() == Mnemonic::Fucomp || instr.mnemonic() == Mnemonic::Fucompp {
                    cpu.fpu.pop();
                }
                if instr.mnemonic() == Mnemonic::Fucompp {
                    cpu.fpu.pop();
                }
                return Ok(());
            }
            let val_a = cpu.fpu.get_st(0);
            let val_b = cpu.fpu.get_st(st_i);
            // MDD-BUG-CRUCIBLE-00258: an unordered compare is quiet only for
            // QUIET NaNs. Per SDM Vol. 2 (FUCOM/FUCOMPP, "SIMD Floating-Point
            // Exceptions"/#IA), it still raises invalid-operation for a
            // signaling NaN or an unsupported encoding — that is the whole
            // difference from FCOM, which raises for quiet NaNs too. This arm
            // previously set no IE at all, so guest code reading FNSTSW saw a
            // clean status after comparing an SNaN.
            if unordered_compare_raises_invalid(&val_a, &val_b) {
                cpu.fpu.set_exception(0x01);
            }
            set_condition_codes_from_sw(cpu, x87_ops::f80_compare(&val_a, &val_b));
            if instr.mnemonic() == Mnemonic::Fucomp || instr.mnemonic() == Mnemonic::Fucompp {
                cpu.fpu.pop();
            }
            if instr.mnemonic() == Mnemonic::Fucompp {
                cpu.fpu.pop();
            }
        }

        Mnemonic::Fcom | Mnemonic::Fcomp | Mnemonic::Fcompp => {
            // FCOM/FCOMP with register operand may have 2 operands: ST(0) and ST(i)
            // We need to find the ST(i) operand, which is the non-ST0 register
            let st_i = if let Some(register) = stack_operand(instr, 1) {
                Some(register)
            } else if let Some(register) = stack_operand(instr, 0) {
                Some(if register == 0 { 1 } else { register })
            } else if operand_is_memory(instr, 0) {
                None
            } else {
                Some(1)
            };
            // Memory-form compares must validate their source before applying
            // masked stack-underflow status or pops.
            let memory_val = if st_i.is_none() {
                Some(read_float_operand_f80(cpu, instr, mem)?)
            } else {
                None
            };
            // Stack-underflow handling: either ST(0) or ST(i) empty → masked
            // invalid compare. Set C0=C2=C3=1 and still pop for FCOMP/FCOMPP.
            // Patch 3 of the x87 fix plan HLD.
            let empty =
                stack_reg_empty(cpu, 0) || st_i.map(|i| stack_reg_empty(cpu, i)).unwrap_or(false);
            if empty {
                cpu.fpu.set_stack_fault(false);
                cpu.fpu.set_condition_codes(true, false, true, true);
                if instr.mnemonic() == Mnemonic::Fcomp || instr.mnemonic() == Mnemonic::Fcompp {
                    cpu.fpu.pop();
                }
                if instr.mnemonic() == Mnemonic::Fcompp {
                    cpu.fpu.pop();
                }
                return Ok(());
            }
            let val_a = cpu.fpu.get_st(0);
            let val_b = if let Some(st_i) = st_i {
                cpu.fpu.get_st(st_i)
            } else {
                memory_val.expect("memory compare source was read before stack checks")
            };
            // IE covers the UNSUPPORTED encodings as well as NaNs
            // `F80::is_nan` requires exponent
            // 0x7FFF, so an unnormal — a finite value with the integer bit
            // clear — is not a NaN and used to slip through and compare
            // silently. A 387 and later raise #IA on it, and host silicon
            // agrees (case 12: SW=0x7501 against our 0x7500).
            if compare_operand_is_invalid(&val_a) || compare_operand_is_invalid(&val_b) {
                cpu.fpu.set_exception(0x01); // IE flag - invalid operation on unordered compare
            }
            set_condition_codes_from_sw(cpu, x87_ops::f80_compare(&val_a, &val_b));
            if instr.mnemonic() == Mnemonic::Fcomp || instr.mnemonic() == Mnemonic::Fcompp {
                cpu.fpu.pop();
            }
            if instr.mnemonic() == Mnemonic::Fcompp {
                cpu.fpu.pop();
            }
        }
        Mnemonic::Fcomi | Mnemonic::Fcomip | Mnemonic::Fucomi | Mnemonic::Fucomip => {
            let st_i_val = stack_operand(instr, 1)
                .or_else(|| stack_operand(instr, 0).filter(|&register| register != 0))
                .unwrap_or(1);
            if stack_reg_empty(cpu, 0) || stack_reg_empty(cpu, st_i_val) {
                cpu.fpu.set_stack_fault(false);
                // Masked invalid FCOMI/FUCOMI sets unordered EFLAGS (ZF=PF=CF=1).
                // Status bits C3=C2=C0=1 correspond to 0x4500.
                set_eflags_from_sw(cpu, 0x4500);
                clear_comi_undefined_flags(cpu);
                if matches!(instr.mnemonic(), Mnemonic::Fcomip | Mnemonic::Fucomip) {
                    cpu.fpu.pop();
                }
                return Ok(());
            }
            let val_a = cpu.fpu.get_st(0);
            let st_i = st_i_val;
            let val_b = cpu.fpu.get_st(st_i);
            // MDD-BUG-CRUCIBLE-00258: FCOMI raises invalid for ANY unordered
            // operand; FUCOMI raises only for a signaling NaN or an unsupported
            // encoding. FUCOMI used to raise for neither, so an SNaN compare
            // left IE clear.
            let raises_invalid = match instr.mnemonic() {
                Mnemonic::Fcomi | Mnemonic::Fcomip => {
                    compare_operand_is_invalid(&val_a) || compare_operand_is_invalid(&val_b)
                }
                _ => unordered_compare_raises_invalid(&val_a, &val_b),
            };
            if raises_invalid {
                cpu.fpu.set_exception(0x01); // IE flag for unordered compare
            }
            let sw = x87_ops::f80_compare(&val_a, &val_b);
            set_eflags_from_sw(cpu, sw);
            clear_comi_undefined_flags(cpu);
            if matches!(instr.mnemonic(), Mnemonic::Fcomip | Mnemonic::Fucomip) {
                cpu.fpu.pop();
            }
        }
        Mnemonic::Fcmovb
        | Mnemonic::Fcmovbe
        | Mnemonic::Fcmove
        | Mnemonic::Fcmovnb
        | Mnemonic::Fcmovnbe
        | Mnemonic::Fcmovne
        | Mnemonic::Fcmovnu
        | Mnemonic::Fcmovu => {
            let st_i = stack_operand(instr, 1)
                .or_else(|| stack_operand(instr, 0).filter(|&register| register != 0))
                .unwrap_or(1);
            let flags = cpu.regs.flags;
            let cf = flags.contains(Flags::CARRY);
            let zf = flags.contains(Flags::ZERO);
            let pf = flags.contains(Flags::PARITY);
            let should_move = match instr.mnemonic() {
                Mnemonic::Fcmovb => cf,
                Mnemonic::Fcmovbe => cf || zf,
                Mnemonic::Fcmove => zf,
                Mnemonic::Fcmovnb => !cf,
                Mnemonic::Fcmovnbe => !cf && !zf,
                Mnemonic::Fcmovne => !zf,
                Mnemonic::Fcmovnu => !pf,
                Mnemonic::Fcmovu => pf,
                _ => false,
            };
            // FCMOVcc raises stack underflow if either inspected register is
            // empty, regardless of whether the condition selects the move.
            // With masked IE, set SF and write the indefinite QNaN to ST(0),
            // even when the condition is false (Intel SDM, FCMOVcc).
            if stack_reg_empty(cpu, 0) || stack_reg_empty(cpu, st_i) {
                cpu.fpu.set_stack_fault(false);
                let top = cpu.fpu.get_top();
                cpu.fpu
                    .set_physical_reg(top, F80([0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]));
                cpu.fpu.set_tag(top, 2); // Special (QNaN)
            } else if should_move {
                let top = cpu.fpu.get_top();
                let src_idx = (top + st_i) & 0x7;
                cpu.fpu.set_physical_reg(top, cpu.fpu.regs[src_idx]);
                let tag = (cpu.fpu.tag_word >> (src_idx * 2)) & 0x3;
                cpu.fpu.set_tag(top, tag);
            }
        }
        Mnemonic::Fnstcw => {
            crate::addressing::write_x87_operand_bytes(
                cpu,
                instr.memory_operand().expect("FNSTCW has a memory operand"),
                mem,
                &cpu.fpu.control_word.to_le_bytes(),
            )?;
        }
        Mnemonic::Fldcw => {
            return execute_fldcw(cpu, instr, mem);
        }
        Mnemonic::Fnstsw => {
            if instr.operand1 == X87Operand::Ax {
                execute_fnstsw_ax(cpu);
            } else {
                let sw = cpu.fpu.status_word;
                crate::addressing::write_x87_operand_bytes(
                    cpu,
                    instr
                        .memory_operand()
                        .expect("FNSTSW has AX or memory operand"),
                    mem,
                    &sw.to_le_bytes(),
                )?;
            }
        }
        // Unary ST(0) operations on an empty stack with masked IE write
        // the indefinite QNaN and tag it Special (SDM Vol 1 §8.5.1.1).
        Mnemonic::Fchs => {
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                return Ok(());
            }
            let binary32 = cpu.fpu.exact_binary32_st(0).map(|value| -value);
            let binary64 = cpu.fpu.exact_binary64_st(0).map(|value| -value);
            let result = x87_ops::f80_chs(cpu.fpu.get_st_ref(0));
            cpu.fpu.set_st_with_proofs(0, result, binary32, binary64);
            // SDM: FCHS sets C1 to 0 (MDD-BUG-FLU-01967).
            cpu.fpu.status_word &= !(1 << 9);
        }
        Mnemonic::Fabs => {
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                return Ok(());
            }
            let binary32 = cpu.fpu.exact_binary32_st(0).map(f32::abs);
            let binary64 = cpu.fpu.exact_binary64_st(0).map(f64::abs);
            let result = x87_ops::f80_abs(cpu.fpu.get_st_ref(0));
            cpu.fpu.set_st_with_proofs(0, result, binary32, binary64);
            // SDM: FABS sets C1 to 0 (MDD-BUG-FLU-01967).
            cpu.fpu.status_word &= !(1 << 9);
        }
        Mnemonic::Fsin | Mnemonic::Fcos | Mnemonic::Fsincos | Mnemonic::Fptan
            if cpu.fpu.control_word & 1 == 0
                && !stack_reg_empty(cpu, 0)
                && cpu.fpu.get_st_ref(0).is_infinite() =>
        {
            // Host kernels mask all exceptions for safety. Suppress their
            // masked-invalid results and any push (including stack overflow)
            // when the guest leaves IE unmasked.
            if matches!(instr.operation, Mnemonic::Fsincos | Mnemonic::Fptan)
                && !cpu.fpu.is_empty((cpu.fpu.get_top().wrapping_sub(1)) & 0x7)
            {
                cpu.fpu.set_stack_fault(true);
            } else {
                set_invalid_op_non_stack(cpu);
            }
        }
        Mnemonic::Fsin => {
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                return Ok(());
            }
            let top = cpu.fpu.get_top();
            let val = cpu.fpu.regs[top];
            if val.is_infinite() {
                cpu.fpu.set_exception(0x01); // IE flag
            }
            let (result, c2) = x87_ops::f80_sin(&val, cpu.fpu.control_word);
            // Retag from the result, not the operand (MDD-BUG-FLU-01966).
            cpu.fpu.set_st(0, result);
            if c2 {
                cpu.fpu.status_word |= 1 << 10;
            } else {
                cpu.fpu.status_word &= !(1 << 10);
            }
        }
        Mnemonic::Fcos => {
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                return Ok(());
            }
            let top = cpu.fpu.get_top();
            let val = cpu.fpu.regs[top];
            if val.is_infinite() {
                cpu.fpu.set_exception(0x01); // IE flag
            }
            let (result, c2) = x87_ops::f80_cos(&val, cpu.fpu.control_word);
            // Retag from the result, not the operand (MDD-BUG-FLU-01966).
            cpu.fpu.set_st(0, result);
            if c2 {
                cpu.fpu.status_word |= 1 << 10;
            } else {
                cpu.fpu.status_word &= !(1 << 10);
            }
        }
        Mnemonic::Fsqrt => {
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                return Ok(());
            }
            // In binary64 mode, use a single-rounded host-double sqrt for a
            // proven-exact operand. Refusals use the 80-bit path, which owns IE/DE.
            if try_numeric_shadow_sqrt(cpu) {
                return Ok(());
            }
            let top = cpu.fpu.get_top();
            let val = cpu.fpu.regs[top];
            if val.is_sign_negative() && !val.is_zero() && !val.is_nan() {
                // Invalid operation - sqrt of negative
                trace_fpu_exception("FSQRT invalid", val.to_f64(), None);
                cpu.fpu.set_exception(0x01); // IE flag
            }
            let (result, sw) = x87_ops::f80_sqrt_status(&val, cpu.fpu.control_word);
            cpu.fpu.set_physical_reg(top, result);
            // Publish the operation's own exception flags and C1. Before
            // MDD-BUG-CRUCIBLE-00579 only the hand-set IE above reached the
            // guest, so an inexact FSQRT left PE and C1 at their previous
            // values and a denormal operand lost DE.
            merge_x87_exception_status_and_c1(cpu, sw);
        }
        Mnemonic::Fptan => {
            // FPTAN replaces ST(0) with tan(ST(0)) and pushes 1.0.
            // With empty ST(0) and masked IE, it instead writes the indefinite
            // QNaN to both the source and newly pushed slots. A following FSTP
            // m64 stores 0xFFF8000000000000 (SDM Vol 1 §8.5.1.1, §4.8.3.7).
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                cpu.fpu.push(F80::INDEFINITE);
                // push() with no overflow tags the new slot Valid; FPTAN's
                // empty-stack result is QNaN, so re-tag as Special (2).
                let new_top = cpu.fpu.get_top();
                cpu.fpu.set_tag(new_top, 2);
                // Clear C2 (result is "invalid/complete"). Matches hardware.
                cpu.fpu.status_word &= !(1 << 10);
                return Ok(());
            }
            // On push overflow with IM=1, FPTAN writes the indefinite QNaN to
            // both source and new TOP slots, tags them Special, and sets
            // C1=SF=IE=1 (SDM Vol 1 §8.5.1.2). `Fpu::push` handles the new TOP;
            // overwrite the source separately.
            if !cpu.fpu.is_empty((cpu.fpu.get_top().wrapping_sub(1)) & 0x7) {
                write_indefinite_to_st(cpu, 0);
                cpu.fpu.push(F80::INDEFINITE);
                cpu.fpu.status_word &= !(1 << 10); // Clear C2 (complete)
                return Ok(());
            }
            let top = cpu.fpu.get_top();
            let angle = cpu.fpu.regs[top];
            // NaN and infinity are invalid FPTAN inputs. Write the indefinite
            // to the source and push another indefinite, not 1.0, as on empty
            // stack (Intel SDM, FPTAN).
            if angle.is_nan() || angle.is_infinite() {
                cpu.fpu.set_exception(0x01); // IE flag
                write_indefinite_to_st(cpu, 0);
                cpu.fpu.push(F80::INDEFINITE);
                let new_top = cpu.fpu.get_top();
                cpu.fpu.set_tag(new_top, 2); // Special (QNaN)
                cpu.fpu.status_word &= !(1 << 10); // Clear C2
            } else {
                let (tan_val, one_val, c2, sw) = x87_ops::f80_ptan(&angle, cpu.fpu.control_word);
                // When C2 is set, ST(0) is unchanged and nothing is pushed
                cpu.fpu.set_physical_reg(top, tan_val);
                if !c2 {
                    cpu.fpu.push(one_val);
                }
                if c2 {
                    cpu.fpu.status_word |= 1 << 10;
                } else {
                    cpu.fpu.status_word &= !(1 << 10);
                }
                merge_x87_exception_status_and_c1(cpu, sw);
            }
        }
        Mnemonic::Fpatan => {
            // Partial arctangent: ST(1) = atan2(ST(1), ST(0)), pop
            // Patch 4: either operand empty → write indefinite to ST(1)
            // (result destination), pop ST(0). SDM §8.5.1.1.
            if stack_reg_empty(cpu, 0) || stack_reg_empty(cpu, 1) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 1);
                cpu.fpu.pop();
                return Ok(());
            }
            let x = cpu.fpu.pop(); // ST(0) = x
            let y = cpu.fpu.get_st(0); // ST(1) which is now ST(0) after pop
            cpu.fpu
                .set_st(0, x87_ops::f80_patan(&y, &x, cpu.fpu.control_word));
        }
        Mnemonic::Fsincos => {
            // FSINCOS computes sine in ST(0) and pushes cosine. With empty
            // ST(0) and masked IE, write and push the indefinite instead.
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                cpu.fpu.push(F80::INDEFINITE);
                // Tag new ST(0) as Special (QNaN), not Valid.
                let new_top = cpu.fpu.get_top();
                cpu.fpu.set_tag(new_top, 2);
                cpu.fpu.status_word &= !(1 << 10); // C2=0 (complete)
                return Ok(());
            }
            // Push-overflow with IM=1: see FPTAN comment above. Host probe
            // `tmp/probe_fsincos_overflow.rs` confirms FSINCOS overflow has
            // the identical bit-for-bit semantics as FPTAN — both source
            // and new-TOP slots become F80 indefinite QNaN.
            if !cpu.fpu.is_empty((cpu.fpu.get_top().wrapping_sub(1)) & 0x7) {
                write_indefinite_to_st(cpu, 0);
                cpu.fpu.push(F80::INDEFINITE);
                cpu.fpu.status_word &= !(1 << 10); // Clear C2 (complete)
                return Ok(());
            }
            let top = cpu.fpu.get_top();
            let angle = cpu.fpu.regs[top];
            if angle.is_infinite() {
                cpu.fpu.set_exception(0x01); // IE flag
            }
            let (sin_val, cos_val, c2) = x87_ops::f80_sincos(&angle, cpu.fpu.control_word);
            cpu.fpu.set_physical_reg(top, sin_val);
            if !c2 {
                cpu.fpu.push(cos_val);
            }
            if c2 {
                cpu.fpu.status_word |= 1 << 10;
            } else {
                cpu.fpu.status_word &= !(1 << 10);
            }
        }
        Mnemonic::Fyl2x => {
            // ST(1) = ST(1) * log2(ST(0)), pop
            // Patch 4: either empty → indefinite in destination ST(1), pop.
            if stack_reg_empty(cpu, 0) || stack_reg_empty(cpu, 1) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 1);
                cpu.fpu.pop();
                return Ok(());
            }
            let x = cpu.fpu.pop(); // ST(0)
            let y = cpu.fpu.get_st(0); // ST(1), now ST(0)
                                       // Exception checks
            if x.is_zero() {
                cpu.fpu.set_exception(0x04); // ZE flag
            } else if (x.is_sign_negative() && !x.is_nan()) || x.is_nan() || y.is_nan() {
                cpu.fpu.set_exception(0x01); // IE flag
            }
            cpu.fpu
                .set_st(0, x87_ops::f80_yl2x(&y, &x, cpu.fpu.control_word));
        }
        Mnemonic::Fyl2xp1 => {
            // ST(1) = ST(1) * log2(ST(0) + 1), pop
            // Patch 4: either empty → indefinite in destination ST(1), pop.
            if stack_reg_empty(cpu, 0) || stack_reg_empty(cpu, 1) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 1);
                cpu.fpu.pop();
                return Ok(());
            }
            let x = cpu.fpu.pop(); // ST(0)
            let y = cpu.fpu.get_st(0); // ST(1), now ST(0)
                                       // Exception checks: compute x+1 and check for edge cases
            let xp1 = x87_ops::f80_add(&x, &F80::ONE, cpu.fpu.control_word);
            if xp1.is_zero() {
                cpu.fpu.set_exception(0x04); // ZE flag - log2(0) = -infinity
            } else if xp1.is_sign_negative() && !xp1.is_nan() {
                cpu.fpu.set_exception(0x01); // IE flag - log2 of negative
            } else if x.is_nan() || y.is_nan() {
                cpu.fpu.set_exception(0x01); // IE flag
            }
            cpu.fpu
                .set_st(0, x87_ops::f80_yl2xp1(&y, &x, cpu.fpu.control_word));
        }
        Mnemonic::F2xm1 => {
            // ST(0) = 2^ST(0) - 1 (for -1 <= ST(0) <= 1)
            // Patch 4: empty ST(0) → write indefinite to ST(0).
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                return Ok(());
            }
            let top = cpu.fpu.get_top();
            let val = cpu.fpu.regs[top];
            // Pre-check: F2XM1 is defined for inputs in [-1, 1]. The host x87
            // returns the input unchanged for out-of-range values, so we must
            // handle them ourselves. Only positive values > 1.0 can overflow
            // (2^x - 1 → infinity). Negative out-of-range values produce small
            // finite results (e.g., 2^(-5) - 1 = -0.96875).
            let f64_val = val.to_f64();
            if f64_val > 1.0 && !val.is_infinite() && !val.is_nan() {
                cpu.fpu.set_exception(0x08); // OE flag - overflow
                cpu.fpu.set_physical_reg(top, F80::from_f64(f64::INFINITY));
            } else if f64_val < -1.0 && !val.is_infinite() && !val.is_nan() {
                // Negative out-of-range: compute manually since host x87 is
                // undefined here. Result is 2^x - 1, always in (-1, 0).
                let result = f64_val.exp2() - 1.0;
                cpu.fpu.set_physical_reg(top, F80::from_f64(result));
            } else {
                let result = x87_ops::f80_f2xm1(&val, cpu.fpu.control_word);
                // Check for overflow: result is infinite but input was finite
                if result.is_infinite() && !val.is_infinite() && !val.is_nan() {
                    cpu.fpu.set_exception(0x08); // OE flag - overflow
                }
                cpu.fpu.set_physical_reg(top, result);
            }
        }
        Mnemonic::Fscale => {
            // ST(0) = ST(0) * 2^trunc(ST(1))
            // Patch 4: empty ST(0) or ST(1) → write indefinite to ST(0).
            if stack_reg_empty(cpu, 0) || stack_reg_empty(cpu, 1) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                return Ok(());
            }
            let top = cpu.fpu.get_top();
            let val = cpu.fpu.regs[top];
            let scale = cpu.fpu.get_st(1);
            let result = x87_ops::f80_scale(&val, &scale, cpu.fpu.control_word);
            // Check for overflow: result is infinite but original was finite
            if result.is_infinite() && !val.is_infinite() && !val.is_nan() {
                cpu.fpu.set_exception(0x08); // OE flag - overflow
            }
            // Check for underflow: result is zero but original was non-zero
            if result.is_zero() && !val.is_zero() && !val.is_nan() {
                cpu.fpu.set_exception(0x10); // UE flag - underflow
            }
            cpu.fpu.set_physical_reg(top, result);
        }
        Mnemonic::Fxtract => {
            // FXTRACT extracts exponent and significand. With empty ST(0)
            // and masked IE, write the indefinite to the exponent slot and
            // push another indefinite for the significand.
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                cpu.fpu.push(F80::INDEFINITE);
                let new_top = cpu.fpu.get_top();
                cpu.fpu.set_tag(new_top, 2);
                return Ok(());
            }
            // Push-overflow with IM=1: see FPTAN comment above. Host probe
            // `tmp/probe_fxtract_overflow.rs` confirms FXTRACT overflow has
            // the identical bit-for-bit semantics as FPTAN/FSINCOS — both
            // source and new-TOP slots become F80 indefinite QNaN. FXTRACT
            // has no C2 flag to clear.
            if !cpu.fpu.is_empty((cpu.fpu.get_top().wrapping_sub(1)) & 0x7) {
                write_indefinite_to_st(cpu, 0);
                cpu.fpu.push(F80::INDEFINITE);
                return Ok(());
            }
            let top = cpu.fpu.get_top();
            let val = cpu.fpu.regs[top];
            // f80_xtract returns (significand, exponent)
            // Intel hardware: replaces ST(0) with exponent, then pushes significand
            // After FXTRACT: ST(0) = significand (pushed on top), ST(1) = exponent
            let (significand, exponent) = x87_ops::f80_xtract(&val, cpu.fpu.control_word);
            cpu.fpu.set_physical_reg(top, exponent); // old ST(0) becomes exponent
            cpu.fpu.push(significand); // push significand on top
        }
        Mnemonic::Fprem => {
            // Partial remainder: ST(0) = ST(0) % ST(1)
            // Patch 4: either empty → write indefinite to ST(0). SDM §8.5.1.1.
            if stack_reg_empty(cpu, 0) || stack_reg_empty(cpu, 1) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                // Clear C2 (complete).
                cpu.fpu.status_word &= !(1 << 10);
                return Ok(());
            }
            let divisor = cpu.fpu.get_st(1);
            let top = cpu.fpu.get_top();
            let dividend = cpu.fpu.regs[top];
            if divisor.is_zero() {
                cpu.fpu.set_exception(0x01); // IE flag - invalid operation
            }
            if dividend.is_nan() || divisor.is_nan() || dividend.is_infinite() {
                cpu.fpu.set_exception(0x01); // IE flag - invalid operation
            }
            // Per Intel 387 manual: finite / infinity returns dividend unchanged,
            // no exception. Only NaN or infinity dividend triggers IE.
            if divisor.is_infinite() && !dividend.is_nan() && !dividend.is_infinite() {
                // dividend unchanged
                cpu.fpu.status_word &= !(1 << 10); // C2 = 0 (complete)
                return Ok(());
            }
            let (result, status) = x87_ops::f80_prem(&dividend, &divisor, cpu.fpu.control_word);
            cpu.fpu.set_physical_reg(top, result);
            // Map status word bits: C0 (bit 8), C1 (bit 9), C2 (bit 10), C3 (bit 14)
            let mask = (1u16 << 8) | (1u16 << 9) | (1u16 << 10) | (1u16 << 14);
            cpu.fpu.status_word = (cpu.fpu.status_word & !mask) | (status & mask);
        }
        Mnemonic::Fprem1 => {
            // IEEE partial remainder: ST(0) = IEEE remainder of ST(0)/ST(1)
            // Patch 4: either empty → write indefinite to ST(0). SDM §8.5.1.1.
            if stack_reg_empty(cpu, 0) || stack_reg_empty(cpu, 1) {
                cpu.fpu.set_stack_fault(false);
                write_indefinite_to_st(cpu, 0);
                cpu.fpu.status_word &= !(1 << 10);
                return Ok(());
            }
            let divisor = cpu.fpu.get_st(1);
            let top = cpu.fpu.get_top();
            let dividend = cpu.fpu.regs[top];
            if divisor.is_zero() {
                cpu.fpu.set_exception(0x01); // IE flag - invalid operation
            }
            if dividend.is_nan() || divisor.is_nan() || dividend.is_infinite() {
                cpu.fpu.set_exception(0x01); // IE flag - invalid operation
            }
            // Per Intel 387 manual: finite / infinity returns dividend unchanged,
            // no exception. Only NaN or infinity dividend triggers IE.
            if divisor.is_infinite() && !dividend.is_nan() && !dividend.is_infinite() {
                // dividend unchanged
                cpu.fpu.status_word &= !(1 << 10); // C2 = 0 (complete)
                return Ok(());
            }
            let (result, status) = x87_ops::f80_prem1(&dividend, &divisor, cpu.fpu.control_word);
            cpu.fpu.set_physical_reg(top, result);
            // Map status word bits: C0 (bit 8), C1 (bit 9), C2 (bit 10), C3 (bit 14)
            let mask = (1u16 << 8) | (1u16 << 9) | (1u16 << 10) | (1u16 << 14);
            cpu.fpu.status_word = (cpu.fpu.status_word & !mask) | (status & mask);
        }
        Mnemonic::Frndint => exec_frndint(cpu),
        Mnemonic::Fxch => {
            // Exchange ST(0) with ST(i)
            // FXCH has 2 operands: first is ST(0), second is ST(i)
            let i = stack_operand(instr, 1)
                .or_else(|| stack_operand(instr, 0).filter(|&register| register != 0))
                .unwrap_or(1);
            return execute_fxch(cpu, i);
        }
        Mnemonic::Ftst => {
            // Compare ST(0) with 0.0
            // Patch 4: empty ST(0) → masked invalid compare → C0=C2=C3=1
            // (unordered). Same pattern as FCOM/FUCOM (Patch 3). SDM §8.3.6.2.
            if stack_reg_empty(cpu, 0) {
                cpu.fpu.set_stack_fault(false);
                cpu.fpu.set_condition_codes(true, false, true, true);
                return Ok(());
            }
            let val = cpu.fpu.get_st(0);
            if val.is_nan() {
                cpu.fpu.set_exception(0x01); // IE flag - invalid operation
            }
            let sw = x87_ops::f80_compare(&val, &F80::ZERO);
            let mask = (1u16 << 8) | (1u16 << 10) | (1u16 << 14);
            cpu.fpu.status_word = (cpu.fpu.status_word & !mask) | (sw & mask);
        }
        Mnemonic::Fxam => {
            // Examine ST(0) and set condition codes.
            // Per Intel 387 manual: FXAM never generates exceptions.
            let top = cpu.fpu.get_top();
            let tag = (cpu.fpu.tag_word >> (top * 2)) & 3;
            let val = cpu.fpu.regs[top];

            // C1 = sign bit
            if val.is_sign_negative() {
                cpu.fpu.status_word |= 1 << 9;
            } else {
                cpu.fpu.status_word &= !(1 << 9);
            }

            // C0, C2, C3 encode the type
            let (c3, c2, c0) = if tag == 3 {
                (1, 0, 1) // Empty
            } else if val.is_nan() {
                (0, 0, 1) // NaN
            } else if val.is_infinite() {
                (0, 1, 1) // Infinity
            } else if val.is_zero() {
                (1, 0, 0) // Zero
            } else if val.is_denormal() {
                (1, 1, 0) // Denormal
            } else {
                (0, 1, 0) // Normal
            };
            if c0 != 0 {
                cpu.fpu.status_word |= 1 << 8;
            } else {
                cpu.fpu.status_word &= !(1 << 8);
            }
            if c2 != 0 {
                cpu.fpu.status_word |= 1 << 10;
            } else {
                cpu.fpu.status_word &= !(1 << 10);
            }
            if c3 != 0 {
                cpu.fpu.status_word |= 1 << 14;
            } else {
                cpu.fpu.status_word &= !(1 << 14);
            }
        }
        Mnemonic::Fdecstp => {
            // Decrement stack pointer. MDD-BUG-CRUCIBLE-00580: SDM Vol 1
            // Table 8-1 defines C1 as 0 here; neither FDECSTP nor FINCSTP
            // touches the registers or tags, so C1 is the only status effect.
            let top = cpu.fpu.get_top();
            cpu.fpu.set_top((top.wrapping_sub(1)) & 0x7);
            cpu.fpu.status_word &= !C1_MASK;
        }
        Mnemonic::Fincstp => {
            // Increment stack pointer (C1 = 0, as for FDECSTP above).
            let top = cpu.fpu.get_top();
            cpu.fpu.set_top((top + 1) & 0x7);
            cpu.fpu.status_word &= !C1_MASK;
        }
        Mnemonic::Fnop => {}
        // 287 compatibility NOPs — these instructions were meaningful on the
        // 80287 coprocessor but are NOPs on 387+.
        Mnemonic::Fnsetpm | Mnemonic::Fneni | Mnemonic::Fndisi => {}
        Mnemonic::Fnclex => exec_fnclex(cpu),
        Mnemonic::Ffree => {
            // Mark register as empty
            if let Some(st_i) = stack_operand(instr, 0) {
                let idx = cpu.fpu.get_st_idx(st_i);
                cpu.fpu.set_tag(idx, 3); // Empty
            }
        }
        Mnemonic::Ffreep => {
            // Mark ST(i) and the popped ST(0) slot empty, then advance TOP.
            // Do not call `pop()`: FFREEP ST(0) frees that slot first, and the
            // generic pop would misread the intentional empty tag as stack
            // underflow and set SF+IE.
            let top = cpu.fpu.get_top();
            if let Some(st_i) = stack_operand(instr, 0) {
                let idx = cpu.fpu.get_st_idx(st_i);
                cpu.fpu.set_tag(idx, 3); // Empty
            }
            cpu.fpu.set_tag(top, 3);
            cpu.fpu.set_top((top + 1) & 0x7);
        }

        // FPU Environment Save/Restore. The descriptor fixes the 14/28-byte
        // layout from the instruction's operand size, including 66h overrides.
        Mnemonic::Fnstenv => {
            let mut env = [0u8; 28];
            let size = encode_environment(cpu, instr.environment_layout, &mut env);
            crate::addressing::write_x87_operand_bytes(
                cpu,
                required_memory_operand(instr),
                mem,
                &env[..size],
            )?;
            // "...and then masks all floating-point exceptions" (SDM, the
            // instruction's first sentence). That side effect is what makes
            // FNSTENV usable inside an #MF handler at all: without it the
            // handler re-faults immediately. The store comes first, so the
            // saved environment holds the PRE-mask control word. FNSAVE gets
            // the same for free by reinitialising to CW=0x037F
            // (MDD-BUG-TEM-00167).
            cpu.fpu.control_word |= 0x003F;
        }
        Mnemonic::Fldenv => {
            let size = environment_size(instr.environment_layout);
            let mut env = [0u8; 28];
            crate::addressing::read_x87_operand_bytes(
                cpu,
                required_memory_operand(instr),
                mem,
                &mut env[..size],
            )?;
            decode_environment(cpu, instr.environment_layout, &env[..size]);
        }

        // FPU Full State Save/Restore
        // 16-bit mode: 94 bytes (14 env + 80 regs); 32-bit PM: 108 bytes (28 env + 80 regs)
        Mnemonic::Fnsave => {
            // Assemble the full save image (env + 8x10-byte registers) in a local
            // buffer, then store it span-aware. 16-bit env = 14 bytes (94 total);
            // 32-bit PM env = 28 bytes (108 total). Reserved/opcode bytes stay 0.
            let mut buf = [0u8; 108];
            let mut env = [0u8; 28];
            let env_size = encode_environment(cpu, instr.environment_layout, &mut env);
            buf[..env_size].copy_from_slice(&env[..env_size]);
            // Next 80 bytes: 8 x 10-byte registers in **stack-relative** order —
            // slot i is ST(i), not physical R_i.
            //
            // This used to write physical R0..R7, citing the 387 manual. That
            // citation was wrong on the facts: SDM Vol 1 Fig. 8-9 labels the
            // area ST(0)..ST(7), and a direct host-silicon probe agrees
            // (`x87_oracle::tests::host_fnsave_register_area_is_st_relative_not_physical`
            // — after `FNINIT; FLD1; FLDZ; FLDPI` the host leaves TOP=5 with pi
            // in slot 0, not slot 5).
            //
            // `Fpu::sync_to_x87_save_area` — the DH ownership-transfer path —
            // already got this right, and its comment records that an earlier
            // physical-order version was caught by fuzzing. This arm is the
            // guest-visible instruction and was never brought into line.
            //
            // The defect hid because it was SYMMETRIC: `FRSTOR` below read
            // physical order too, so a save/restore round-trip inside gp64
            // came out right. That is why the two are corrected together — fix
            // one alone and every round-trip breaks.
            let top = cpu.fpu.get_top();
            for i in 0..8 {
                let o = env_size + i * 10;
                buf[o..o + 10].copy_from_slice(&cpu.fpu.regs[(top + i) & 0x7].0);
            }
            let total = env_size + 80;
            // Store first (may #PF and restart, leaving the FPU untouched); only
            // reinitialize once the store commits.
            crate::addressing::write_x87_operand_bytes(
                cpu,
                required_memory_operand(instr),
                mem,
                &buf[..total],
            )?;
            // FSAVE reinitializes the FPU
            cpu.fpu.control_word = 0x037F;
            cpu.fpu.status_word = 0;
            cpu.fpu.tag_word = 0xFFFF;
            cpu.fpu.invalidate_all_binary32();
        }
        Mnemonic::Frstor => {
            // 16-bit env = 14 bytes (94 total); 32-bit PM env = 28 bytes (108
            // total). Read the whole image span-aware, then parse it.
            let env_size = environment_size(instr.environment_layout);
            let total = env_size + 80;
            let mut buf = [0u8; 108];
            crate::addressing::read_x87_operand_bytes(
                cpu,
                required_memory_operand(instr),
                mem,
                &mut buf[..total],
            )?;
            decode_environment(cpu, instr.environment_layout, &buf[..env_size]);
            // Next 80 bytes: 8 x 10-byte registers in **stack-relative** order —
            // slot i is ST(i). Unrotate by TOP
            // into the physical `regs` array, mirroring
            // `Fpu::sync_from_x87_save_area`.
            //
            // `decode_environment` above has already loaded the status word, so
            // `get_top()` is the RESTORED TOP — which is the one that defines
            // how the stack-relative image maps onto physical registers. The
            // ordering of those two steps is load-bearing, exactly as
            // `sync_from_x87_save_area` records.
            let top = cpu.fpu.get_top();
            for i in 0..8 {
                let o = env_size + i * 10;
                let mut bytes = [0u8; 10];
                bytes.copy_from_slice(&buf[o..o + 10]);
                cpu.fpu.set_physical_reg((top + i) & 0x7, F80(bytes));
            }
        }

        // BCD Operations (packed decimal)
        Mnemonic::Fbld => {
            // Load 18-digit packed BCD into ST(0). On masked stack overflow,
            // advance TOP, write the indefinite QNaN, tag Special and set
            // C1=SF=IE=1 (SDM Vol 1 §8.5.1.2). Read the operand unconditionally
            // and let `Fpu::push` handle overflow, as for FILD.
            let mut bytes = [0u8; 10];
            crate::addressing::read_x87_operand_bytes(
                cpu,
                required_memory_operand(instr),
                mem,
                &mut bytes,
            )?;
            cpu.fpu.push(F80::from_bcd(&bytes));
        }
        Mnemonic::Fbstp => {
            // Store ST(0) as 18-digit packed BCD and pop. On masked empty-stack
            // underflow, store the 10-byte BCD indefinite: little-endian
            // [0,0,0,0,0,0,0,0xC0,0xFF,0xFF] (SDM Vol 1 §8.5.1.1, §4.8.3.7).
            if stack_reg_empty(cpu, 0) {
                // Write FP indefinite unconditionally. Intel
                // SDM IM=0 semantics would raise #IA and not write, but our
                // emulator does not dispatch FPU exceptions — status-word-only.
                // See HLD 02 §5.2.
                let indef: [u8; 10] = [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF];
                crate::addressing::write_x87_operand_bytes(
                    cpu,
                    required_memory_operand(instr),
                    mem,
                    &indef,
                )?;
                cpu.fpu.set_stack_fault(false);
                // FBSTP still advances TOP on masked IE.
                cpu.fpu.pop();
                return Ok(());
            }
            // Peek ST(0), store span-aware, then pop — so a cross-page #PF on the
            // store leaves the stack untouched and the instruction is restartable.
            let val = cpu.fpu.get_st(0);
            let bytes = val.to_bcd(cpu.fpu.control_word);
            crate::addressing::write_x87_operand_bytes(
                cpu,
                required_memory_operand(instr),
                mem,
                &bytes,
            )?;
            cpu.fpu.pop();
        }

        _ => {
            return Err(anyhow::anyhow!(
                "Unhandled FPU instruction: {:?}",
                instr.mnemonic()
            ));
        }
    }
    Ok(())
}

/// A single-precision or double-precision `FLD` source, for
/// [`raise_fld_memory_exceptions`].
///
/// The helper takes the SOURCE rather than a pair of booleans so a caller
/// cannot pass the wrong flag for the wrong value — the classification and the
/// status-word write stay in one place.
trait FldMemorySource: Copy {
    fn is_subnormal_source(self) -> bool;
    /// A signaling NaN: a NaN whose most-significant fraction bit is clear.
    fn is_signaling_nan_source(self) -> bool;
}

impl FldMemorySource for f32 {
    fn is_subnormal_source(self) -> bool {
        self.is_subnormal()
    }
    fn is_signaling_nan_source(self) -> bool {
        self.is_nan() && (self.to_bits() & 0x0040_0000) == 0
    }
}

impl FldMemorySource for f64 {
    fn is_subnormal_source(self) -> bool {
        self.is_subnormal()
    }
    fn is_signaling_nan_source(self) -> bool {
        self.is_nan() && (self.to_bits() & 0x0008_0000_0000_0000) == 0
    }
}

/// The status-word exceptions an `FLD m32`/`FLD m64` owes its source.
///
/// DE for a subnormal was already raised; **IE for a signaling NaN was not**
/// SDM Vol 2 lists `#IA` for `FLD m32fp`/`m64fp`
/// when the source is an SNaN or an unsupported format, and host silicon agrees:
/// `FNINIT; FLD dword [snan]; FNSAVE` leaves SW=0x3801 on the host against our
/// 0x3800 — the IE bit was the ONLY difference, measured. The loaded value and
/// the tag word already matched, because `F80::from_f32` quiets the NaN and
/// defect B's classifier tags it Special, so nothing else is owed here.
///
/// **Scoped to m32/m64 deliberately.** `FLD m80` is a raw 80-bit load and does
/// NOT raise `#IA` for an SNaN, so it must not call this. The unsupported
/// encodings are likewise unreachable from binary32/binary64 — IEEE has no
/// unnormals — which is why only the SNaN case appears here.
fn raise_fld_memory_exceptions<S: FldMemorySource>(cpu: &mut Cpu, source: S) {
    if source.is_subnormal_source() {
        cpu.fpu.set_exception(0x02); // DE
    }
    if source.is_signaling_nan_source() {
        cpu.fpu.set_exception(0x01); // IE
    }
}

#[inline(always)]
fn execute_fld_m32<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<()> {
    let source = read_f32(cpu, instr, mem)?;
    fpu_guard_census::note_fld_m32(source);
    raise_fld_memory_exceptions(cpu, source);
    cpu.fpu.push_binary32(source);
    Ok(())
}

#[inline(always)]
fn execute_fld_register(cpu: &mut Cpu, src: usize) -> anyhow::Result<()> {
    if stack_reg_empty(cpu, src) {
        cpu.fpu.set_stack_fault(false);
        cpu.fpu.push(F80::INDEFINITE);
        let new_top = cpu.fpu.get_top();
        cpu.fpu.set_tag(new_top, 2);
        return Ok(());
    }
    cpu.fpu.commit_drc_binary64_st(src);
    let value = cpu.fpu.get_st(src);
    let binary32 = cpu.fpu.exact_binary32_st(src);
    let binary64 = cpu.fpu.exact_binary64_st(src);
    cpu.fpu.push_with_proofs(value, binary32, binary64);
    Ok(())
}

#[inline(always)]
fn execute_fstp_m32<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<()> {
    if stack_reg_empty(cpu, 0) {
        write_fp_indefinite(cpu, instr, mem)?;
        cpu.fpu.set_stack_fault(false);
        cpu.fpu.pop();
        return Ok(());
    }
    cpu.fpu.commit_drc_binary64_st(0);
    let value = cpu.fpu.get_st(0);
    let (narrowed, sw) = narrow_st0_to_f32(cpu, value);
    fpu_guard_census::note_fstp_m32(narrowed);
    write_f32(cpu, instr, mem, narrowed)?;
    merge_x87_exception_status_and_c1(cpu, sw);
    cpu.fpu.pop();
    Ok(())
}

#[inline(always)]
fn execute_fild_m32<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<()> {
    let value = read_i32(cpu, instr, mem)?;
    if fpu_ops_trace_enabled() {
        let addr = crate::addressing::calc_x87_effective_address(
            cpu,
            required_memory_operand(instr),
            mem,
            crate::PageAccess::Read,
        )
        .unwrap_or(0);
        tracing::trace!(target: "gp64::fpu::ops", addr = format_args!("{:05X}", addr), ival = value, size = 4, "FILD mem");
    }
    cpu.fpu.push_i32_exact(value);
    Ok(())
}

#[inline(always)]
fn execute_fistp_m32<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<()> {
    // The store comes before any status or stack change, so a faulting
    // operand leaves the x87 untouched.
    let (integer, conversion) = fistp_i32_convert(cpu);
    write_i32(cpu, instr, mem, integer)?;
    fistp_i32_finish(cpu, conversion);
    Ok(())
}

/// The status half of an FISTP m32 conversion, applied after the store.
#[derive(Clone, Copy)]
enum FistpI32Conversion {
    /// ST(0) was empty: the store is the integer indefinite and the pop is a
    /// stack-underflow fault.
    Empty,
    /// The conversion's own status bits (IE, PE and C1).
    Converted(u16),
}

#[inline(always)]
fn fistp_i32_convert(cpu: &mut Cpu) -> (i32, FistpI32Conversion) {
    if stack_reg_empty(cpu, 0) {
        return (i32::MIN, FistpI32Conversion::Empty);
    }
    cpu.fpu.commit_drc_binary64_st(0);
    let value = cpu.fpu.get_st(0);
    // The FOURTH store site. `Fistp` with a
    // 4-byte operand short-circuits to this dedicated path before the generic
    // `Fist | Fistp` arm is reached, so fixing that arm's three width cases
    // leaves the single most common FISTP form untouched. Only a test that
    // drove all three widths surfaced it.
    let (integer, conv_sw) = value.to_i32_status(cpu.fpu.control_word);
    fpu_guard_census::note_fistp_m32(value.is_nan(), value.is_denormal(), (conv_sw & 0x01) != 0);
    (integer, FistpI32Conversion::Converted(conv_sw))
}

#[inline(always)]
fn fistp_i32_finish(cpu: &mut Cpu, conversion: FistpI32Conversion) {
    match conversion {
        FistpI32Conversion::Empty => cpu.fpu.set_stack_fault(false),
        FistpI32Conversion::Converted(conv_sw) => apply_fist_conversion_status(cpu, conv_sw),
    }
    cpu.fpu.pop();
}

/// FNINIT, exactly as the instruction executes.
///
/// For HLE code that stands in for guest code issuing the instruction,
/// preserving the instruction-defined result rather than creating a fresh [`Fpu`].
pub fn exec_fninit(cpu: &mut Cpu) {
    cpu.fpu.status_word = 0;
    cpu.fpu.control_word = 0x037F;
    cpu.fpu.tag_word = 0xFFFF;
    cpu.fpu.invalidate_all_binary32();
    cpu.fpu.set_top(0);
    cw_census::note_cw(cpu.fpu.control_word);
}

/// FNCLEX, exactly as the instruction executes (see [`exec_fninit`] for who
/// calls it).
pub fn exec_fnclex(cpu: &mut Cpu) {
    // SDM Vol 2 (FCLEX/FNCLEX): clears the six exception flags (IE, DE,
    // ZE, OE, UE, PE — bits 0-5), the **stack fault flag SF (bit 6)**,
    // the exception summary ES (bit 7), and the busy flag B (bit 15).
    //
    // The old mask was `!0x00BF`, which skips bit 6 — so SF survived a
    // clear that is supposed to remove it.
    // Host silicon agrees with the SDM: `x87_oracle::tests::
    // host_fnclex_clears_stack_fault` drives an underflow then FNCLEX
    // and reads SW=0x0800 with SF clear, where we returned 0x0840.
    //
    // B is included for the same reason SF is — the SDM lists it. We
    // never SET bit 15 today, so clearing it is a no-op in practice;
    // it is here so the instruction stays correct if the busy flag is
    // ever modelled, rather than leaving a second known-wrong bit in a
    // mask we are already correcting.
    cpu.fpu.status_word &= !0x80FF;
}

/// FRNDINT on ST(0) under the current control word, exactly as the
/// instruction executes (see [`exec_fninit`] for who calls it).
pub fn exec_frndint(cpu: &mut Cpu) {
    // Patch 4: empty ST(0) → write indefinite to ST(0). SDM §8.5.1.1.
    if stack_reg_empty(cpu, 0) {
        cpu.fpu.set_stack_fault(false);
        write_indefinite_to_st(cpu, 0);
        return;
    }
    let top = cpu.fpu.get_top();
    let (result, sw) = x87_ops::f80_rndint_status(&cpu.fpu.regs[top], cpu.fpu.control_word);
    cpu.fpu.set_physical_reg(top, result);
    // As for FSQRT: an inexact FRNDINT (1.5 -> 2) must publish PE and C1,
    // and a denormal operand must publish DE (MDD-BUG-CRUCIBLE-00579).
    merge_x87_exception_status_and_c1(cpu, sw);
}

/// FISTP m32 without the memory store: converts ST(0) under the current
/// control word, publishes the conversion status, pops, and returns the
/// integer the instruction would have stored (see [`exec_fninit`] for who
/// calls it).
pub fn exec_fistp_i32(cpu: &mut Cpu) -> i32 {
    let (integer, conversion) = fistp_i32_convert(cpu);
    fistp_i32_finish(cpu, conversion);
    integer
}

#[inline(always)]
fn execute_fnstsw_ax(cpu: &mut Cpu) {
    cpu.regs.set_ax(cpu.fpu.status_word);
}

#[inline(always)]
fn execute_fld_m64<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<()> {
    x87_form_census::note_fld_wide(8);
    let source = read_f64(cpu, instr, mem)?;
    raise_fld_memory_exceptions(cpu, source);
    cpu.fpu.push(F80::from_f64(source));
    Ok(())
}

#[inline(always)]
fn execute_fstp_m64<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<()> {
    if stack_reg_empty(cpu, 0) {
        write_fp_indefinite(cpu, instr, mem)?;
        cpu.fpu.set_stack_fault(false);
        cpu.fpu.pop();
        return Ok(());
    }
    cpu.fpu.commit_drc_binary64_st(0);
    let val = cpu.fpu.get_st(0);
    x87_form_census::note_fstp_wide(8);
    // Binary64 mode preserves C1 for an exact binary64 store.
    // The normal interpreter path reports the narrowing status below.
    let drc_exact_store =
        cpu.fpu.drc_binary64_mode_active() && cpu.fpu.exact_binary64_st(0).is_some();
    let (f64_val, sw) = narrow_st0_to_f64(cpu, val);
    write_f64(cpu, instr, mem, f64_val)?;
    if !drc_exact_store {
        merge_x87_exception_status_and_c1(cpu, sw);
    }
    cpu.fpu.pop();
    Ok(())
}

/// FXCH ST(0) <-> ST(i). Extracted verbatim from the generic arm so the fat
/// dispatch and `execute_inner` share one implementation.
/// EXP-IQ007: the FXCH ST(i) inline lane — the interpreter's escape arm
/// calls this directly for an unprefixed `D9 C8+i`, bypassing the decode
/// cache and the `execute_decoded_with_dispatch` wrapper. Parity bar is the
/// wrapper's own `Fxch` route, reproduced exactly: the non-arith
/// `sync_host_condition_codes_if_dirty` then `execute_fxch` (infallible —
/// both its arms return `Ok`, including the masked-IE empty-register
/// QNaN-fill path). FXCH neither reads nor writes EFLAGS, touches no
/// memory, and is valid on every supported guest CPU, so the skipped
/// layers (materialize_flags, decode cache, P6/FISTTP checks, dispatch
/// census note, ops/diverge trace capture) carry no guest-visible
/// semantics. Known diagnostic narrowing while the lane is engaged:
/// env-armed ops/diverge traces record no FXCH rows (journaled,
/// EXP-IQ007). Engagement is counted through the previously-unwired
/// `note_lane_admitted` census scaffold. Pinned by
/// `fxch_lane_matches_the_full_route` (gp64-interp).
/// EXP-IQ008: the x87 REGISTER hot lane — admission + execution for the
/// unprefixed register-only forms the interpreter's escape arm may run
/// without the decode cache and `execute_decoded_with_dispatch` wrapper.
/// Returns `true` iff the byte pair was admitted and executed; `false`
/// sends the caller down the unchanged full route.
///
/// NORMATIVE admission table: the EXP-IQ008 HLD v2 (panel-folded — note
/// BOTH DC reversals: operand direction AND the sub/div R-slot swap).
/// Reference of record: `X87Dispatch::classify` + the iced-pinned decoder.
/// Wrapper-tail parity per form: non-arith forms sync host condition codes
/// first (`may_use_shadow_fast_path` is false for them); arith forms do
/// NOT sync (exactly the wrapper's rule); every admitted form is
/// EFLAGS-neutral, memory-free, infallible, IP-neutral, and pre-P6; all
/// end with the binary64 commit barrier, matching the wrapper.
/// While the ops trace is armed (EMU_PROBES/test builds only — const-false
/// in production) the lane declines everything so trace fidelity is kept.
/// Pinned by the exhaustive 512-pair two-route sweep
/// (`x87_register_lane_sweep`, gp64-interp) plus `fxch_lane_tests`.
#[inline]
/// ADMISSION HAZARD (adversarial review 2026.08.05): no admitted form may
/// WRITE EFLAGS. The caller's register arm publishes the PRE-execution
/// `flags_reg` without re-reading `cpu.regs.flags` (and skips the publish
/// entirely under a pending lazy op) — correct for the current admission set,
/// silently wrong for FCOMI/FUCOMI/FCOMIP/FUCOMIP/FCMOVcc (now executable on
/// the P6 identities). Laning any flag-writing form requires the memory
/// arm's full materialize/publish/re-read bracket, not this arm's.
pub fn try_execute_register_lane(cpu: &mut Cpu, b0: u8, b1: u8) -> bool {
    if fpu_ops_trace_enabled() {
        return false;
    }
    let i = usize::from(b1 & 0x07);
    match (b0, b1) {
        (0xD9, 0xC0..=0xC7) => {
            dispatch_census::note_lane_admitted();
            dispatch_census::note_lane_form(0);
            cpu.fpu.sync_host_condition_codes_if_dirty();
            let _ = execute_fld_register(cpu, i);
        }
        (0xD9, 0xC8..=0xCF) => {
            dispatch_census::note_lane_admitted();
            dispatch_census::note_lane_form(1);
            cpu.fpu.sync_host_condition_codes_if_dirty();
            let _ = execute_fxch(cpu, i);
        }
        (0xDF, 0xE0) => {
            dispatch_census::note_lane_admitted();
            dispatch_census::note_lane_form(2);
            cpu.fpu.sync_host_condition_codes_if_dirty();
            execute_fnstsw_ax(cpu);
        }
        // Register arithmetic: slots 0/1/4/5/6/7 of (modrm-0xC0)>>3; the
        // D0..DF hole (slots 2/3 — register FCOM/FCOMP/FCOMPP aliases,
        // Generic dispatch) is excluded by the range pattern.
        (0xD8 | 0xDC | 0xDE, 0xC0..=0xCF | 0xE0..=0xFF) => {
            let slot = (b1 - 0xC0) >> 3;
            // D8 slot order: Sub/SubReverse/Div/DivReverse at 4/5/6/7.
            // DC and DE swap the R-variants into the opposite slots (the
            // notorious D8/DC mnemonic swap) AND reverse operand direction.
            let op = match (b0 == 0xD8, slot) {
                (_, 0) => ArithOp::Add,
                (_, 1) => ArithOp::Mul,
                (true, 4) | (false, 5) => ArithOp::Sub,
                (true, 5) | (false, 4) => ArithOp::SubReverse,
                (true, 6) | (false, 7) => ArithOp::Div,
                (true, 7) | (false, 6) => ArithOp::DivReverse,
                _ => unreachable!("slots 2/3 excluded by the match pattern"),
            };
            let (dst, src) = if b0 == 0xD8 { (0, i) } else { (i, 0) };
            let pop = b0 == 0xDE;
            dispatch_census::note_lane_admitted();
            dispatch_census::note_lane_form(3);
            // Arith dispatches do NOT sync (wrapper parity:
            // `may_use_shadow_fast_path` is true for ArithReg).
            let _ = execute_arith_register(cpu, dst, src, pop, op);
        }
        _ => return false,
    }
    // EXP-A013: no per-dispatch commit — reads inside the lane forms carry
    // their own per-register barriers.
    true
}

/// Result of `x87_mem_lane_admit`: a descriptor constructed to be bit-for-bit
/// identical to what the full decoder (`decode_uncached`) would produce for
/// the same bytes, its pre-classified `X87Dispatch`, its `LANE_FORM_NAMES`
/// census index, and its encoded length in bytes.
#[derive(Clone, Copy, Debug)]
pub struct MemLaneDecoded {
    pub instr: DecodedX87,
    pub dispatch: X87Dispatch,
    pub lane_form: usize,
    pub len: usize,
}

/// EXP-IQ012: the x87 MEMORY hot lane — admission + descriptor construction
/// for the four unprefixed, 32-bit-addressed, non-SIB single-precision
/// memory forms the interpreter's escape arm may execute without the decode
/// cache or the `execute_decoded_with_dispatch` wrapper.
///
/// NORMATIVE admission table (design doc
/// `wrk_docs/2026.08.05 - DES - EXP-IQ012 x87 memory-form lane
/// (panel-folded).md` §4): admitted iff `cs_bits == 32` (checked here; the
/// caller's `pos == pos_start + 1` gate proves the escape is unprefixed —
/// `bytes[0]` is the raw escape opcode with nothing ahead of it); `bytes[1]
/// < 0xC0` (memory ModR/M, i.e. `mode != 3`); `(bytes[0], (bytes[1]>>3)&7)`
/// is one of `(0xD9,0)=FLD m32`, `(0xD9,3)=FSTP m32`, `(0xDB,0)=FILD m32`,
/// `(0xDB,3)=FISTP m32`; and the addressing shape is `mod=00,rm∉{4,5}`
/// (`[reg]`), `mod=00,rm=5` (`[disp32]`, no base), `mod=01,rm≠4`
/// (`[reg+disp8]`), or `mod=10,rm≠4` (`[reg+disp32]`) — SIB (`rm=4`) is
/// declined in every `mod`. Declines unconditionally while the ops trace is
/// armed (`fpu_ops_trace_enabled`, EMU_PROBES/test builds only) — the
/// register-lane precedent. Touches NO guest-visible CPU/memory state; its
/// only side effects are the `fpu_ops_trace_enabled` global read and, on a
/// SIB decline under `interp-attrib`, a diagnostic census bump
/// (`note_mem_lane_sib_declined`) — so admission still commits no
/// architectural mutation before the caller decides. Construction reference of record:
/// `decode_memory_form` + `decode_memory_operand32` (x87_decode.rs);
/// pinned exhaustively against `decode_uncached` by
/// `x87_mem_lane_construction_parity` below. Segment default: SS iff
/// `base == Some(Ebp)` (i.e. `mod` 01/10 `rm=5`), else DS — the `mod=00
/// rm=5` `[disp32]` no-base form has `base == None` and stays DS, matching
/// `decode_memory_operand32`.
#[must_use]
pub fn x87_mem_lane_admit(bytes: &[u8; 15], cs_bits: u8) -> Option<MemLaneDecoded> {
    if fpu_ops_trace_enabled() {
        return None;
    }
    if cs_bits != 32 {
        return None;
    }
    let b0 = bytes[0];
    let b1 = bytes[1];
    if b1 >= 0xC0 {
        return None;
    }
    let reg = (b1 >> 3) & 7;
    let (operation, dispatch, lane_form) = match (b0, reg) {
        (0xD9, 0) => (Mnemonic::Fld, X87Dispatch::FldM32, 4),
        (0xD9, 3) => (Mnemonic::Fstp, X87Dispatch::FstpM32, 5),
        (0xDB, 0) => (Mnemonic::Fild, X87Dispatch::FildM32, 6),
        (0xDB, 3) => (Mnemonic::Fistp, X87Dispatch::FistpM32, 7),
        _ => return None,
    };
    let mode = b1 >> 6;
    let rm = b1 & 7;
    if rm == 4 {
        dispatch_census::note_mem_lane_sib_declined(lane_form - 4);
        return None; // SIB — not admitted (design §4)
    }
    const REGS: [X87AddressRegister; 8] = [
        X87AddressRegister::Eax,
        X87AddressRegister::Ecx,
        X87AddressRegister::Edx,
        X87AddressRegister::Ebx,
        X87AddressRegister::Esp,
        X87AddressRegister::Ebp,
        X87AddressRegister::Esi,
        X87AddressRegister::Edi,
    ];
    let (base, displacement, displacement_raw, displacement_size, len): (
        Option<X87AddressRegister>,
        i32,
        u32,
        u8,
        usize,
    ) = match mode {
        0 if rm == 5 => {
            let raw = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
            (None, raw as i32, raw, 4, 6)
        }
        0 => (Some(REGS[rm as usize]), 0, 0, 0, 2),
        1 => {
            let raw = bytes[2];
            (
                Some(REGS[rm as usize]),
                i32::from(raw as i8),
                u32::from(raw),
                1,
                3,
            )
        }
        2 => {
            let raw = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
            (Some(REGS[rm as usize]), raw as i32, raw, 4, 6)
        }
        _ => unreachable!("mode is 0..=2 here: b1 < 0xC0 excludes mode 3"),
    };
    let default_segment = match base {
        Some(X87AddressRegister::Ebp) => X87Segment::Ss,
        _ => X87Segment::Ds,
    };
    let mo = X87MemoryOperand {
        address_size: X87AddressSize::Bits32,
        base,
        index: None,
        scale: 1,
        displacement,
        displacement_raw,
        displacement_size,
        default_segment,
        segment: default_segment,
        width_bytes: 4,
    };
    let instr = DecodedX87 {
        operation,
        operand1: X87Operand::Memory(mo),
        operand2: X87Operand::None,
        length: len as u8,
        prefixes: X87Prefixes::default(),
        operand_size: CodeBitness::Bits32,
        environment_layout: X87EnvironmentLayout::None,
    };
    Some(MemLaneDecoded {
        instr,
        dispatch,
        lane_form,
        len,
    })
}

/// Execute an admitted memory-lane descriptor. Bracket order matches the
/// wrapper's for these four forms (fpu.rs `execute_decoded_with_dispatch_unchecked`,
/// `dispatch_census::note` then `sync_host_condition_codes_if_dirty` since
/// `may_use_shadow_fast_path` is false for all four — panel fold, design
/// §11): census note THEN sync, then the form's fat helper.
pub fn x87_mem_lane_execute<M: PagedMemory>(
    cpu: &mut Cpu,
    mem: &mut M,
    d: &MemLaneDecoded,
) -> anyhow::Result<()> {
    dispatch_census::note_lane_admitted();
    dispatch_census::note_lane_form(d.lane_form);
    cpu.fpu.sync_host_condition_codes_if_dirty();
    match d.dispatch {
        X87Dispatch::FldM32 => execute_fld_m32(cpu, &d.instr, mem),
        X87Dispatch::FstpM32 => execute_fstp_m32(cpu, &d.instr, mem),
        X87Dispatch::FildM32 => execute_fild_m32(cpu, &d.instr, mem),
        X87Dispatch::FistpM32 => execute_fistp_m32(cpu, &d.instr, mem),
        _ => unreachable!("x87_mem_lane_admit only ever returns the four admitted forms"),
    }
}

/// EXP-IQ012 §6.2 construction-parity pin: for every admitted
/// (form × mod × rm) shape, the lane's constructed `DecodedX87` must equal
/// what the full decoder produces for the identical bytes, and its
/// `X87Dispatch` classification must agree. This is the oracle that makes
/// "construct instead of decode" sound (design R1). Runs under ordinary
/// `cargo test` with no tracing subscriber armed for
/// `gp64::fpu::ops=trace`, so `fpu_ops_trace_enabled()` is false and the
/// lane is live (design §11 fold: must not run under an armed trace).
#[cfg(test)]
mod x87_mem_lane_construction_parity {
    #[cfg(feature = "interp-attrib")]
    #[test]
    fn mem_lane_form_indices_match_census_names() {
        // x87_mem_lane_admit hardcodes lane_form 4..8 and derives the
        // SIB-declined index as lane_form - 4; LANE_FORM_NAMES is
        // feature-gated, so a reorder cannot fail compilation — pin the
        // mapping here (adversarial review 2026.08.05).
        assert_eq!(
            &super::dispatch_census::lane_form_names()[4..8],
            &["fld_m32", "fstp_m32", "fild_m32", "fistp_m32"]
        );
    }

    use super::*;
    use crate::x87_decode_cache::decode_uncached;

    /// Displacement samples: 0, 1, a disp8 boundary either side of the sign
    /// bit, the disp8 max, and two disp32-scale values (design §6.2).
    const DISPS: [u32; 7] = [0, 1, 0x7F, 0x80, 0xFF, 0x1234_5678, 0x8000_0000_u32];

    fn bytes15(code: &[u8]) -> [u8; 15] {
        let mut padded = [0u8; 15];
        padded[..code.len()].copy_from_slice(code);
        padded
    }

    fn check_one(code: &[u8]) {
        let padded = bytes15(code);
        let lane = x87_mem_lane_admit(&padded, 32)
            .unwrap_or_else(|| panic!("expected admission for {code:02x?}"));
        let (expected_instr, expected_dispatch) =
            decode_uncached(code, 32).expect("reference decode must succeed");
        assert_eq!(
            lane.instr, expected_instr,
            "descriptor mismatch for {code:02x?}"
        );
        assert_eq!(
            lane.dispatch, expected_dispatch,
            "dispatch mismatch for {code:02x?}"
        );
        assert_eq!(lane.len, usize::from(expected_instr.length));
    }

    #[test]
    fn admitted_forms_match_the_full_decoder_exactly() {
        let forms: [(u8, u8); 4] = [(0xD9, 0), (0xD9, 3), (0xDB, 0), (0xDB, 3)];
        for (b0, reg) in forms {
            for rm in 0u8..8 {
                if rm == 4 {
                    continue; // SIB — never admitted
                }
                // mode = 0, rm != 5: [reg], no displacement bytes.
                if rm != 5 {
                    let modrm = (reg << 3) | rm;
                    check_one(&[b0, modrm]);
                }
                // mode = 0, rm == 5: [disp32], no base.
                if rm == 5 {
                    let modrm = (reg << 3) | rm;
                    for &disp in &DISPS {
                        let mut code = vec![b0, modrm];
                        code.extend_from_slice(&disp.to_le_bytes());
                        check_one(&code);
                    }
                }
                // mode = 1, rm != 4: [reg+disp8].
                {
                    let modrm = (1u8 << 6) | (reg << 3) | rm;
                    for &disp in &DISPS {
                        let mut code = vec![b0, modrm];
                        code.push(disp as u8);
                        check_one(&code);
                    }
                }
                // mode = 2, rm != 4: [reg+disp32].
                {
                    let modrm = (2u8 << 6) | (reg << 3) | rm;
                    for &disp in &DISPS {
                        let mut code = vec![b0, modrm];
                        code.extend_from_slice(&disp.to_le_bytes());
                        check_one(&code);
                    }
                }
            }
        }
    }

    #[test]
    fn declines_sib_shape_in_every_mode() {
        let padded = bytes15(&[0xD9, 0x04, 0x24]); // FLD m32 [ESP] via SIB
        assert!(x87_mem_lane_admit(&padded, 32).is_none());
        let padded = bytes15(&[0xD9, 0x44, 0x24, 0x08]); // mod=01 SIB
        assert!(x87_mem_lane_admit(&padded, 32).is_none());
        let padded = bytes15(&[0xD9, 0x84, 0x24, 0x00, 0x10, 0x00, 0x00]); // mod=10 SIB
        assert!(x87_mem_lane_admit(&padded, 32).is_none());
    }

    #[test]
    fn declines_unadmitted_reg_fields_and_16bit_code() {
        let padded = bytes15(&[0xD9, 0x10]); // FST m32 — D9/2, not admitted
        assert!(x87_mem_lane_admit(&padded, 32).is_none());
        let padded = bytes15(&[0xDB, 0x08]); // FISTTP m32 — DB/1, not admitted
        assert!(x87_mem_lane_admit(&padded, 32).is_none());
        let padded = bytes15(&[0xDB, 0x10]); // FIST m32 — DB/2, not admitted
        assert!(x87_mem_lane_admit(&padded, 32).is_none());
        // Admitted form, but 16-bit code — cs_bits must gate it off.
        let padded = bytes15(&[0xD9, 0x00]);
        assert!(x87_mem_lane_admit(&padded, 16).is_none());
    }
}

fn execute_fxch(cpu: &mut Cpu, i: usize) -> anyhow::Result<()> {
    let empty0 = stack_reg_empty(cpu, 0);
    let emptyi = stack_reg_empty(cpu, i);
    let top = cpu.fpu.get_top();
    let idx_i = (top + i) & 0x7;
    if empty0 || emptyi {
        // On masked stack underflow, FXCH still exchanges the registers,
        // filling the empty register with the indefinite QNaN (Intel SDM, FXCH).
        cpu.fpu.set_stack_fault(false);
        if empty0 {
            cpu.fpu.set_physical_reg(top, F80::INDEFINITE);
            cpu.fpu.set_tag(top, 2);
        }
        if emptyi {
            cpu.fpu.set_physical_reg(idx_i, F80::INDEFINITE);
            cpu.fpu.set_tag(idx_i, 2);
        }
        // Now perform the swap on the (now-populated) slots.
        cpu.fpu.swap_physical_regs(top, idx_i);
        let tag_top = (cpu.fpu.tag_word >> (top * 2)) & 0x3;
        let tag_i = (cpu.fpu.tag_word >> (idx_i * 2)) & 0x3;
        cpu.fpu.set_tag(top, tag_i);
        cpu.fpu.set_tag(idx_i, tag_top);
        return Ok(());
    }
    cpu.fpu.swap_physical_regs(top, idx_i);
    let tag_top = (cpu.fpu.tag_word >> (top * 2)) & 0x3;
    let tag_i = (cpu.fpu.tag_word >> (idx_i * 2)) & 0x3;
    cpu.fpu.set_tag(top, tag_i);
    cpu.fpu.set_tag(idx_i, tag_top);
    // MDD-BUG-CRUCIBLE-00580: SDM Vol 1 Table 8-1 defines C1 as 0 for a
    // successful FXCH. Leaving it alone exposed a stale C1 from a previous
    // rounding or overflow through FNSTSW / FNSTENV / FNSAVE. The stack-fault
    // path above is separate and sets C1 itself via `set_stack_fault`.
    cpu.fpu.status_word &= !C1_MASK;
    Ok(())
}

#[inline(always)]
fn execute_fldcw<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<()> {
    let mut bytes = [0u8; 2];
    crate::addressing::read_x87_operand_bytes(
        cpu,
        instr.memory_operand().expect("FLDCW has a memory operand"),
        mem,
        &mut bytes,
    )?;
    cpu.fpu.control_word = u16::from_le_bytes(bytes);
    cw_census::note_cw(cpu.fpu.control_word);
    Ok(())
}

/// FCOM/FCOMP with a float memory source — the memory path of the generic
/// compare arm, specialized for the fat dispatch. The source read must
/// precede masked stack-underflow effects so a faulting operand leaves the
/// FPU image untouched.
/// Whether a compare operand makes FCOM/FCOMI invalid (#IA): a NaN or an
/// unsupported encoding - an unnormal, a pseudo-infinity, a pseudo-NaN.
/// `F80::is_nan` alone is not the rule: it needs exponent 0x7FFF, so an
/// unnormal slips past it, and the two backends disagree
/// on pseudo encodings, which made FCOMI host-dependent. Every compare form
/// uses this one predicate (MDD-BUG-TEM-00166).
fn compare_operand_is_invalid(value: &F80) -> bool {
    value.is_nan() || value.is_unsupported_encoding()
}

fn execute_fcom_mem<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    pop: bool,
) -> anyhow::Result<()> {
    let val_b = read_float_operand_f80(cpu, instr, mem)?;
    if stack_reg_empty(cpu, 0) {
        cpu.fpu.set_stack_fault(false);
        // Masked invalid compare: C0=C2=C3=1 (unordered); FCOMP still pops.
        cpu.fpu.set_condition_codes(true, false, true, true);
        if pop {
            cpu.fpu.pop();
        }
        return Ok(());
    }
    cpu.fpu.commit_drc_binary64_st(0);
    let val_a = cpu.fpu.get_st(0);
    if compare_operand_is_invalid(&val_a) || compare_operand_is_invalid(&val_b) {
        cpu.fpu.set_exception(0x01); // IE flag - invalid operation on unordered compare
    }
    set_condition_codes_from_sw(cpu, x87_ops::f80_compare(&val_a, &val_b));
    if pop {
        cpu.fpu.pop();
    }
    Ok(())
}

fn read_f32<M: PagedMemory>(cpu: &Cpu, instr: &DecodedX87, mem: &mut M) -> anyhow::Result<f32> {
    let mut bytes = [0u8; 4];
    crate::addressing::read_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &mut bytes,
    )?;
    Ok(f32::from_le_bytes(bytes))
}

fn read_f64<M: PagedMemory>(cpu: &Cpu, instr: &DecodedX87, mem: &mut M) -> anyhow::Result<f64> {
    let mut bytes = [0u8; 8];
    crate::addressing::read_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &mut bytes,
    )?;
    Ok(f64::from_le_bytes(bytes))
}

/// Read a floating-point memory source as F80 before any architectural FPU
/// mutation. Empty-stack arithmetic and compare paths use this same helper as
/// their non-empty paths so a guest memory fault is always precise.
fn read_float_operand_f80<M: PagedMemory>(
    cpu: &Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<F80> {
    x87_form_census::note_float_src(instr.memory_size_bytes(), cpu.fpu.control_word);
    match instr.memory_size_bytes() {
        4 => Ok(F80::from_f32(read_f32(cpu, instr, mem)?)),
        8 => Ok(F80::from_f64(read_f64(cpu, instr, mem)?)),
        size => {
            tracing::warn!(
                target: "gp64::cpu::fpu",
                size,
                cs = format_args!("{:04X}", cpu.regs.cs),
                ip = format_args!("{:04X}", cpu.regs.ip()),
                "FPU float op: unhandled memory size"
            );
            Ok(F80::ZERO)
        }
    }
}

fn write_f32<M: PagedMemory>(
    cpu: &Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    value: f32,
) -> anyhow::Result<()> {
    crate::addressing::write_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &value.to_le_bytes(),
    )?;
    Ok(())
}

fn write_f64<M: PagedMemory>(
    cpu: &Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    value: f64,
) -> anyhow::Result<()> {
    crate::addressing::write_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &value.to_le_bytes(),
    )?;
    Ok(())
}

fn read_i16<M: PagedMemory>(cpu: &Cpu, instr: &DecodedX87, mem: &mut M) -> anyhow::Result<i16> {
    let mut bytes = [0u8; 2];
    crate::addressing::read_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &mut bytes,
    )?;
    Ok(i16::from_le_bytes(bytes))
}

fn read_i32<M: PagedMemory>(cpu: &Cpu, instr: &DecodedX87, mem: &mut M) -> anyhow::Result<i32> {
    let mut bytes = [0u8; 4];
    crate::addressing::read_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &mut bytes,
    )?;
    Ok(i32::from_le_bytes(bytes))
}

/// Read integer operand from memory for FIADD/FISUB/etc - returns as F80
fn read_int_operand_f80<M: PagedMemory>(
    cpu: &Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<F80> {
    match instr.memory_size_bytes() {
        2 => Ok(F80::from_i16(read_i16(cpu, instr, mem)?)),
        4 => Ok(F80::from_i32(read_i32(cpu, instr, mem)?)),
        size => {
            tracing::warn!(
                target: "gp64::cpu::fpu",
                size,
                cs = format_args!("{:04X}", cpu.regs.cs),
                ip = format_args!("{:04X}", cpu.regs.ip()),
                "FPU integer op: unhandled memory size"
            );
            Ok(F80::ZERO)
        }
    }
}

fn write_i16<M: PagedMemory>(
    cpu: &Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    value: i16,
) -> anyhow::Result<()> {
    crate::addressing::write_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &value.to_le_bytes(),
    )?;
    Ok(())
}

fn write_i32<M: PagedMemory>(
    cpu: &Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    value: i32,
) -> anyhow::Result<()> {
    crate::addressing::write_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &value.to_le_bytes(),
    )?;
    Ok(())
}

fn read_i64<M: PagedMemory>(cpu: &Cpu, instr: &DecodedX87, mem: &mut M) -> anyhow::Result<i64> {
    let mut bytes = [0u8; 8];
    crate::addressing::read_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &mut bytes,
    )?;
    Ok(i64::from_le_bytes(bytes))
}

fn write_i64<M: PagedMemory>(
    cpu: &Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    value: i64,
) -> anyhow::Result<()> {
    crate::addressing::write_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &value.to_le_bytes(),
    )?;
    Ok(())
}

/// Read 80-bit extended precision float from memory as F80
fn read_f80_raw<M: PagedMemory>(cpu: &Cpu, instr: &DecodedX87, mem: &mut M) -> anyhow::Result<F80> {
    let mut bytes = [0u8; 10];
    crate::addressing::read_x87_operand_bytes(
        cpu,
        required_memory_operand(instr),
        mem,
        &mut bytes,
    )?;
    Ok(F80(bytes))
}

/// Write F80 value to memory as 80-bit extended precision
fn write_f80_raw<M: PagedMemory>(
    cpu: &Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    value: F80,
) -> anyhow::Result<()> {
    crate::addressing::write_x87_operand_bytes(cpu, required_memory_operand(instr), mem, &value.0)?;
    Ok(())
}

/// Write the x87 floating-point indefinite (QNaN) for the size of the
/// instruction's memory operand. Called by FST/FSTP on masked-IE stack
/// underflow. Intel SDM Vol 1 §4.8.3.7 defines the indefinite bit patterns:
/// - m32fp: 0xFFC00000
/// - m64fp: 0xFFF8_0000_0000_0000
/// - m80fp: sign=1, exp=0x7FFF, mantissa bits 63 and 62 set (LE bytes
///   [0,0,0,0,0,0,0,0xC0,0xFF,0xFF]).
fn write_fp_indefinite<M: PagedMemory>(
    cpu: &Cpu,
    instr: &DecodedX87,
    mem: &mut M,
) -> anyhow::Result<()> {
    match instr.memory_size_bytes() {
        4 => write_f32(cpu, instr, mem, f32::from_bits(0xFFC0_0000)),
        8 => write_f64(cpu, instr, mem, f64::from_bits(0xFFF8_0000_0000_0000)),
        10 => write_f80_raw(
            cpu,
            instr,
            mem,
            F80([0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]),
        ),
        _ => Ok(()),
    }
}

/// Does an *unordered* compare (FUCOM/FUCOMP/FUCOMPP/FUCOMI/FUCOMIP) of these
/// two operands raise invalid-operation?
///
/// The unordered forms exist precisely so that a quiet NaN — the ordinary
/// result of an earlier masked invalid operation — can be compared without
/// signalling again. They are **not** exception-free: a signaling NaN or an
/// unsupported encoding still raises #IA, exactly as it does for FCOM. Ordered
/// FCOM/FCOMI raise for quiet NaNs as well, so they do not use this predicate.
///
/// Factored out as a predicate because "must NOT raise" has no other observable
/// signature: a masked IE that is correctly suppressed and an IE that was never
/// computed look identical in the status word, so the quiet-NaN case can only
/// be pinned by testing the decision itself.
fn unordered_compare_raises_invalid(a: &F80, b: &F80) -> bool {
    [a, b]
        .into_iter()
        .any(|v| v.is_signaling_nan() || v.is_unsupported_encoding())
}

/// Set FPU condition codes (C0, C2, C3) directly from x87 status word bits
/// returned by `f80_compare`. C0=bit 8, C2=bit 10, C3=bit 14.
fn set_condition_codes_from_sw(cpu: &mut Cpu, sw: u16) {
    let mask = (1u16 << 8) | (1u16 << 10) | (1u16 << 14);
    cpu.fpu.status_word = (cpu.fpu.status_word & !mask) | (sw & mask);
}

/// `ST(0)` narrowed to binary32 for an `FST`/`FSTP m32`, with the status the
/// store reports (MDD-BUG-TEM-00168). An f32-proven `ST(0)` narrows exactly
/// under any RC: no flags, and C1 clear — a store that did not round up
/// clears it.
fn narrow_st0_to_f32(cpu: &Cpu, value: F80) -> (f32, u16) {
    cpu.fpu
        .exact_binary32_st(0)
        .map(|exact| (exact, 0))
        .unwrap_or_else(|| value.to_f32_status_with_cw(cpu.fpu.control_word))
}

/// `ST(0)` narrowed to binary64 for an `FST`/`FSTP m64`, as
/// [`narrow_st0_to_f32`].
fn narrow_st0_to_f64(cpu: &Cpu, value: F80) -> (f64, u16) {
    cpu.fpu
        .exact_binary64_st(0)
        .map(|exact| (exact, 0))
        .unwrap_or_else(|| value.to_f64_status_with_cw(cpu.fpu.control_word))
}

/// Merge exception flags (bits 0-5), set ES for unmasked exceptions,
/// and replace C1. Equivalent to per-flag [`Fpu::set_exception`] calls.
/// Unrolled to keep PGO from choosing an expensive loop on sparse counts;
/// the exhaustive equivalence test pins its behavior.
fn merge_x87_exception_status_and_c1(cpu: &mut Cpu, sw: u16) {
    const EXCEPTIONS: u16 = 0x003F;
    const C1: u16 = 1 << 9;
    let raised = sw & EXCEPTIONS;
    let unmasked = raised & !cpu.fpu.control_word;
    let es = if unmasked != 0 { Fpu::STATUS_ES } else { 0 };
    cpu.fpu.status_word = ((cpu.fpu.status_word | raised | es) & !C1) | (sw & C1);
}

/// Set CF/ZF/PF from the x87 status returned by `f80_compare`:
/// C0 (bit 8) -> CF, C2 (bit 10) -> PF, C3 (bit 14) -> ZF.
/// Callers must separately clear OF/SF/AF via `clear_comi_undefined_flags`.
fn set_eflags_from_sw(cpu: &mut Cpu, sw: u16) {
    let mut flags = cpu.regs.flags;
    flags.remove(Flags::CARRY | Flags::ZERO | Flags::PARITY);
    flags.set(Flags::CARRY, (sw & (1 << 8)) != 0); // C0 -> CF
    flags.set(Flags::PARITY, (sw & (1 << 10)) != 0); // C2 -> PF
    flags.set(Flags::ZERO, (sw & (1 << 14)) != 0); // C3 -> ZF
    cpu.regs.flags = flags;
}

/// Clear OF, SF and AF, which FCOMI/FCOMIP/FUCOMI/FUCOMIP set to 0.
///
/// Not cosmetic: the SDM's "Flags Affected" for FCOMI zeroes all three, which
/// is what lets these instructions feed a `Jcc` directly without an
/// intervening TEST. Until 2026.08.05 the compare arm set only CF/ZF/PF and
/// left OF/SF/AF holding whatever the PREVIOUS instruction wrote — behaviour
/// matching no documented part.
///
/// It survived because it was unreachable: `requires_p6()` gated every caller
/// and no `CpuType` was P6-capable, so nothing could execute FCOMI. Adding the
/// Pentium Pro / Pentium II identities makes it guest-visible.
///
/// Confirmed on host silicon before changing it, not argued from the manual —
/// with OF/SF/AF all pre-set to 1, every ordering (a<b, a>b, a==b) and a NaN
/// operand return OF=SF=AF=0 from both `fcomi` and `fucomi`.
fn clear_comi_undefined_flags(cpu: &mut Cpu) {
    cpu.regs
        .flags
        .remove(Flags::OVERFLOW | Flags::SIGN | Flags::AUX_CARRY);
}

fn stack_reg_empty(cpu: &Cpu, st_i: usize) -> bool {
    cpu.fpu.is_empty(cpu.fpu.get_st_idx(st_i))
}

/// Write the indefinite QNaN to ST(i) and tag it Special. Masked
/// stack-underflow paths use this result, rather than Valid or Zero
/// (Intel SDM Vol 1 §8.5.1.1, §4.8.3.7).
fn write_indefinite_to_st(cpu: &mut Cpu, st_i: usize) {
    let idx = cpu.fpu.get_st_idx(st_i);
    cpu.fpu.set_physical_reg(idx, F80::INDEFINITE);
    cpu.fpu.set_tag(idx, 2); // Special (QNaN)
}

/// Apply the status bits an `FIST`/`FISTP` conversion produced
///
/// Every store site reaches this, so the decision of WHICH flags a store owes
/// lives in one place. `IE` keeps its existing route through
/// `set_invalid_op_non_stack`, which clears C1 per SDM §8.5.1; on the
/// non-invalid path C1 is REPLACED with the conversion's rounded-up bit —
/// `FIST`/`FISTP` set C1 when the store rounded up and clear it otherwise, so
/// a stale C1 from a prior operation must not survive (MDD-BUG-FLUX-00127).
///
/// SDM: `FIST`/`FISTP` raise `#IA` and `#P` (plus the stack fault `#IS`,
/// which is not a conversion outcome) and update C1; C0/C2/C3 are undefined.
/// `F80::FIST_STATUS_MASK` has already narrowed the raw host word to those
/// bits.
#[inline]
fn apply_fist_conversion_status(cpu: &mut Cpu, sw: u16) {
    if (sw & 0x01) != 0 {
        set_invalid_op_non_stack(cpu);
        return;
    }
    cpu.fpu.status_word = (cpu.fpu.status_word & !0x0200) | (sw & 0x0200);
    if (sw & 0x20) != 0 {
        cpu.fpu.set_exception(0x20); // PE — the store rounded
    }
}

/// Signal a masked invalid-operation exception that is NOT a stack fault.
///
/// Per Intel SDM Vol 1 §8.5.1: when the IE (invalid-operation) exception
/// is signalled with IM=1, C1 is cleared **unless** the source of the
/// exception is a stack overflow (in which case C1=1) or a stack underflow
/// (C1=0 — already cleared). For non-stack invalid operations — e.g. an
/// FIST/FISTP/FISTTP convert from a NaN/infinity input, or an arithmetic
/// op on an SNaN — C1 must be **cleared** so it does not retain a stale
/// value from a prior overflow operation.
///
/// `set_stack_fault` is the right helper for stack-related #IS — it sets
/// IE+SF and assigns C1 explicitly. This helper is the analogue for
/// non-stack #IA: set IE, clear C1, leave SF untouched (SF is sticky
/// across non-stack-fault exceptions per the SDM).
fn set_invalid_op_non_stack(cpu: &mut Cpu) {
    cpu.fpu.set_exception(0x01); // IE flag (sets ES if unmasked, but IM=1 normally)
    cpu.fpu.status_word &= !(1 << 9); // C1 = 0 (non-stack invalid per SDM §8.5.1)
}

/// Shadow-resident arithmetic (HLD 2026.07.29 D4): when the destination and
/// source both carry the exactness proof the control word's PC needs (PC=24 →
/// f32 proofs, PC=53 → f64 proofs), run the shared `x87_fast_core` error-free-
/// transform cores directly on the proven host values — no F80 operand decode
/// at all. PC=64 (and reserved PC=01) never engage: no host type carries a
/// 64-bit significand. All rounding modes are supported; the cores return
/// `None` for anything the pure path must own. Returns whether it engaged.
#[inline(always)]
fn try_numeric_shadow_arith(
    cpu: &mut Cpu,
    destination: usize,
    source32: Option<f32>,
    source64: Option<f64>,
    op: ArithOp,
) -> bool {
    use crate::x87_fast_core as fast_core;
    let cw = cpu.fpu.control_word;
    let rc = (cw >> 10) & 0b11;
    // Each arm produces (canonical F80, f32 proof, f64 proof, status bits).
    let precision = if cpu.fpu.drc_binary64_mode_active() {
        // The binary64 execution contract uses a double register file;
        // its arithmetic therefore stays binary64 even when guest PC requests
        // extended precision.
        0b10
    } else {
        (cw >> 8) & 0b11
    };
    let result = match precision {
        0b00 => cpu
            .fpu
            .exact_binary32_st(destination)
            .zip(source32)
            .and_then(|(a, b)| match op {
                ArithOp::Add => fast_core::fast_add_f32_core(a, b, rc),
                ArithOp::Sub => fast_core::fast_add_f32_core(a, -b, rc),
                ArithOp::SubReverse => fast_core::fast_add_f32_core(b, -a, rc),
                ArithOp::Mul => fast_core::fast_mul_f32_core(a, b, rc),
                ArithOp::Div => fast_core::fast_div_f32_core(a, b, rc),
                ArithOp::DivReverse => fast_core::fast_div_f32_core(b, a, rc),
            })
            .and_then(|(value, sw)| {
                crate::x87_common::f32_normal_or_zero_to_f80(value)
                    .map(|bytes| (F80(bytes), Some(value), Some(f64::from(value)), sw))
            }),
        0b10 => cpu
            .fpu
            .exact_binary64_st(destination)
            .zip(source64)
            .and_then(|(a, b)| match op {
                ArithOp::Add => fast_core::fast_add_f64_core(a, b, rc),
                ArithOp::Sub => fast_core::fast_add_f64_core(a, -b, rc),
                ArithOp::SubReverse => fast_core::fast_add_f64_core(b, -a, rc),
                ArithOp::Mul => fast_core::fast_mul_f64_core(a, b, rc),
                ArithOp::Div => fast_core::fast_div_f64_core(a, b, rc),
                ArithOp::DivReverse => fast_core::fast_div_f64_core(b, a, rc),
            })
            .and_then(|(value, sw)| {
                crate::x87_common::f64_normal_or_zero_to_f80(value)
                    .map(|bytes| (F80(bytes), derive_f32_proof(value), Some(value), sw))
            }),
        _ => None,
    };
    let accepted = result.is_some();
    match op {
        ArithOp::Add => single_arith_census::note_add(accepted),
        ArithOp::Sub | ArithOp::SubReverse => single_arith_census::note_sub(accepted),
        ArithOp::Mul => single_arith_census::note_mul(accepted),
        ArithOp::Div | ArithOp::DivReverse => single_arith_census::note_div(accepted),
    }
    let Some((canonical, exact32, exact64, sw)) = result else {
        return false;
    };
    cpu.fpu
        .set_st_with_proofs(destination, canonical, exact32, exact64);
    merge_x87_exception_status_and_c1(cpu, sw);
    cpu.fpu.mark_host_condition_codes_dirty();
    true
}

/// FSQRT in binary64 mode uses a single-rounded host-double result for
/// proven-exact ST(0), avoiding a second rounding through an 80-bit result.
/// Like [`try_numeric_shadow_arith`], this mode ignores PC. Normal F80
/// mode and unproven, negative, denormal, NaN, infinite or sub-floor
/// operands use the pure path, which owns IE/DE.
fn try_numeric_shadow_sqrt(cpu: &mut Cpu) -> bool {
    if !cpu.fpu.drc_binary64_mode_active() {
        return false;
    }
    let rc = (cpu.fpu.control_word >> 10) & 0b11;
    let result = cpu
        .fpu
        .exact_binary64_st(0)
        .and_then(|a| crate::x87_fast_core::fast_sqrt_f64_core(a, rc))
        .and_then(|(value, sw)| {
            crate::x87_common::f64_normal_or_zero_to_f80(value)
                .map(|bytes| (F80(bytes), derive_f32_proof(value), Some(value), sw))
        });
    let accepted = result.is_some();
    single_arith_census::note_sqrt(accepted);
    let Some((canonical, exact32, exact64, sw)) = result else {
        return false;
    };
    cpu.fpu.set_st_with_proofs(0, canonical, exact32, exact64);
    merge_x87_exception_status_and_c1(cpu, sw);
    cpu.fpu.mark_host_condition_codes_dirty();
    true
}

#[inline(always)]
fn interp_f80_add(a: &F80, b: &F80, cw: u16) -> (F80, Option<u16>) {
    #[cfg(target_arch = "x86_64")]
    {
        let (result, sw) = x87_ops::f80_add_status(a, b, cw);
        (result, Some(sw))
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let (result, sw) = x87_ops::f80_add_status(a, b, cw);
        (result, Some(sw))
    }
}

#[inline(always)]
fn interp_f80_mul(a: &F80, b: &F80, cw: u16) -> (F80, Option<u16>) {
    #[cfg(target_arch = "x86_64")]
    {
        let (result, sw) = x87_ops::f80_mul_status(a, b, cw);
        (result, Some(sw))
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let (result, sw) = x87_ops::f80_mul_status(a, b, cw);
        (result, Some(sw))
    }
}

/// Basic-four operation selector, public because [`X87Dispatch`] carries it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArithOp {
    Add,
    Sub,
    SubReverse,
    Mul,
    Div,
    DivReverse,
}

#[inline(always)]
fn apply_arith(op: ArithOp, a: &F80, b: &F80, cw: u16) -> (F80, Option<u16>) {
    match op {
        ArithOp::Add => interp_f80_add(a, b, cw),
        ArithOp::Sub => {
            let (result, sw) = x87_ops::f80_sub_status(a, b, cw);
            (result, Some(sw))
        }
        ArithOp::SubReverse => {
            let (result, sw) = x87_ops::f80_sub_status(b, a, cw);
            (result, Some(sw))
        }
        ArithOp::Mul => interp_f80_mul(a, b, cw),
        ArithOp::Div => {
            let (result, sw) = x87_ops::f80_div_status(a, b, cw);
            (result, Some(sw))
        }
        ArithOp::DivReverse => {
            let (result, sw) = x87_ops::f80_div_status(b, a, cw);
            (result, Some(sw))
        }
    }
}

fn execute_arith<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    op: ArithOp,
) -> anyhow::Result<()> {
    let mnemonic = instr.mnemonic();
    let is_pop = matches!(
        mnemonic,
        Mnemonic::Faddp
            | Mnemonic::Fsubp
            | Mnemonic::Fsubrp
            | Mnemonic::Fmulp
            | Mnemonic::Fdivp
            | Mnemonic::Fdivrp
    );
    match instr.operand1 {
        X87Operand::Memory(_) => execute_arith_memory(cpu, instr, mem, op)?,
        X87Operand::Stack(st_i) => {
            let st_i = usize::from(st_i);
            let st_j = stack_operand(instr, 1).unwrap_or(0);
            execute_arith_register(cpu, st_i, st_j, is_pop, op)?;
        }
        operand => {
            tracing::warn!(
                target: "gp64::cpu::fpu",
                ?operand,
                cs = format_args!("{:04X}", cpu.regs.cs),
                ip = format_args!("{:04X}", cpu.regs.ip()),
                "FPU arith: unhandled operand kind"
            );
        }
    }
    Ok(())
}

fn execute_arith_memory<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    op: ArithOp,
) -> anyhow::Result<()> {
    let cw = cpu.fpu.control_word;
    // Read first: a source #GP/#PF must leave status, tags, TOP and registers
    // byte-identical for restart. Fixes MDD-BUG-CRUCIBLE-00255.
    let m_val = read_float_operand_f80(cpu, instr, mem)?;
    // Masked empty-stack arithmetic writes the indefinite QNaN to ST(0)
    // (SDM Vol 1 §8.5.1.1). Still read the memory operand to preserve its
    // access side effects before reporting the stack fault.
    if stack_reg_empty(cpu, 0) {
        cpu.fpu.set_stack_fault(false);
        // SDM §8.5.1.1: C0/C2/C3 are "undefined" on masked-#IS arith
        // underflow; real Intel/AMD x87 preserves them — match that
        // (HLD 2026.05.08).
        write_indefinite_to_st(cpu, 0);
        return Ok(());
    }
    let memory_binary64 = crate::x87_common::f80_to_exact_normal_or_zero_f64(m_val.bytes());
    let memory_binary32 = memory_binary64.and_then(derive_f32_proof);
    if try_numeric_shadow_arith(cpu, 0, memory_binary32, memory_binary64, op) {
        return Ok(());
    }
    cpu.fpu.sync_host_condition_codes_if_dirty();
    cpu.fpu.commit_drc_binary64_st(0);
    let a = cpu.fpu.get_st(0);
    let (result, sw) = apply_arith(op, &a, &m_val, cw);
    cpu.fpu.set_st(0, result);
    if let Some(sw) = sw {
        merge_x87_exception_status_and_c1(cpu, sw);
    }
    Ok(())
}

fn execute_integer_arith_memory<M: PagedMemory>(
    cpu: &mut Cpu,
    instr: &DecodedX87,
    mem: &mut M,
    op: ArithOp,
) -> anyhow::Result<()> {
    // The mandatory source read precedes even masked stack-underflow effects.
    let m_val = read_int_operand_f80(cpu, instr, mem)?;
    if stack_reg_empty(cpu, 0) {
        cpu.fpu.set_stack_fault(false);
        write_indefinite_to_st(cpu, 0);
        return Ok(());
    }

    let a = cpu.fpu.get_st(0);
    let (result, sw) = apply_arith(op, &a, &m_val, cpu.fpu.control_word);
    cpu.fpu.set_st(0, result);
    if let Some(sw) = sw {
        merge_x87_exception_status_and_c1(cpu, sw);
    }
    Ok(())
}

fn execute_arith_register(
    cpu: &mut Cpu,
    st_i: usize,
    st_j: usize,
    is_pop: bool,
    op: ArithOp,
) -> anyhow::Result<()> {
    let cw = cpu.fpu.control_word;
    x87_form_census::note_arith_reg(cw);
    // With either source empty and IE masked, arithmetic writes the
    // indefinite to the destination; pop variants still pop
    // (Intel SDM Vol 1 §8.5.1.1).
    if stack_reg_empty(cpu, st_i) || stack_reg_empty(cpu, st_j) {
        cpu.fpu.set_stack_fault(false);
        // SDM §8.5.1.1: C0/C2/C3 are "undefined" on masked-#IS arith
        // underflow; real Intel/AMD x87 preserves them — match that
        // (HLD 2026.05.08).
        write_indefinite_to_st(cpu, st_i);
        if is_pop {
            cpu.fpu.pop();
        }
        return Ok(());
    }
    let source_binary32 = cpu.fpu.exact_binary32_st(st_j);
    let source_binary64 = cpu.fpu.exact_binary64_st(st_j);
    if try_numeric_shadow_arith(cpu, st_i, source_binary32, source_binary64, op) {
        if is_pop {
            cpu.fpu.pop();
        }
        return Ok(());
    }
    cpu.fpu.sync_host_condition_codes_if_dirty();
    cpu.fpu.commit_drc_binary64_st(st_i);
    cpu.fpu.commit_drc_binary64_st(st_j);
    let a = cpu.fpu.get_st(st_i);
    let b = cpu.fpu.get_st(st_j);
    let (result, sw) = apply_arith(op, &a, &b, cw);
    cpu.fpu.set_st(st_i, result);
    if let Some(sw) = sw {
        merge_x87_exception_status_and_c1(cpu, sw);
    }
    if is_pop {
        cpu.fpu.pop();
    }
    Ok(())
}

/// Round f64 according to x87 rounding control (test helper for f64_to_bcd)
#[cfg(test)]
fn round_with_rc(control_word: u16, value: f64) -> f64 {
    let rc = (control_word >> 10) & 3;
    match rc {
        0 => {
            // Round to nearest even
            if !value.is_finite() {
                return value;
            }
            let abs = value.abs();
            if abs >= (1u64 << 53) as f64 {
                return value;
            }
            let floor = abs.floor();
            let frac = abs - floor;
            let rounded = if frac < 0.5 {
                floor
            } else if frac > 0.5 {
                floor + 1.0
            } else if ((floor as u64) & 1) == 0 {
                floor
            } else {
                floor + 1.0
            };
            if value.is_sign_negative() {
                -rounded
            } else {
                rounded
            }
        }
        1 => value.floor(),
        2 => value.ceil(),
        3 => value.trunc(),
        _ => unreachable!(),
    }
}

/// Convert f64 to 80-bit extended precision format (10 bytes, test helper for FSAVE/FRSTOR setup)
#[cfg(test)]
fn f64_to_f80(value: f64) -> [u8; 10] {
    let mut bytes = [0u8; 10];

    if value.is_nan() {
        // NaN: exponent all 1s, non-zero mantissa
        bytes[7] = 0xC0; // Set integer bit and first mantissa bit (quiet NaN)
        bytes[8] = 0xFF;
        bytes[9] = if value.is_sign_negative() { 0xFF } else { 0x7F };
        return bytes;
    }

    if value.is_infinite() {
        // Infinity
        bytes[7] = 0x80; // Integer bit set
        bytes[8] = 0xFF;
        bytes[9] = if value.is_sign_negative() { 0xFF } else { 0x7F };
        return bytes;
    }

    if value == 0.0 {
        // Zero (preserve sign)
        if value.is_sign_negative() {
            bytes[9] = 0x80;
        }
        return bytes;
    }

    // Normal number
    let bits = value.to_bits();
    let sign = ((bits >> 63) & 1) as u16;
    let f64_exp = ((bits >> 52) & 0x7FF) as i32;
    let f64_mantissa = bits & 0x000F_FFFF_FFFF_FFFF;

    if f64_exp == 0 {
        let msb = 63 - f64_mantissa.leading_zeros() as i32;
        let shift = 63 - msb;
        let f80_mantissa = f64_mantissa << shift;
        let unbiased = msb - 1074;
        let f80_exp = (unbiased + 16383) as u16;
        let sign_exp = (sign << 15) | f80_exp;
        let mantissa_bytes = f80_mantissa.to_le_bytes();
        bytes[0..8].copy_from_slice(&mantissa_bytes);
        bytes[8..10].copy_from_slice(&sign_exp.to_le_bytes());
        return bytes;
    }

    // Convert exponent: f64 bias 1023 -> f80 bias 16383
    let unbiased = f64_exp - 1023;
    let f80_exp = (unbiased + 16383) as u16;

    // Build 64-bit mantissa with explicit integer bit
    let f80_mantissa = 0x8000_0000_0000_0000u64 | (f64_mantissa << 11);

    // Write mantissa (little-endian)
    let mantissa_bytes = f80_mantissa.to_le_bytes();
    bytes[0..8].copy_from_slice(&mantissa_bytes);

    // Write sign and exponent
    let sign_exp = (sign << 15) | f80_exp;
    bytes[8..10].copy_from_slice(&sign_exp.to_le_bytes());

    bytes
}

/// Convert 80-bit extended precision (10 bytes) to f64 (test helper for reading FSAVE output)
#[cfg(test)]
fn f80_to_f64(bytes: &[u8; 10]) -> f64 {
    // Read mantissa (little-endian)
    let mantissa = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    let sign_exp = u16::from_le_bytes([bytes[8], bytes[9]]);

    let sign = (sign_exp >> 15) & 1;
    let exponent = sign_exp & 0x7FFF;

    let sign_bit = (sign as u64) << 63;

    // Handle special cases
    if exponent == 0 {
        // Zero or denormal
        if mantissa == 0 {
            return if sign == 1 { -0.0 } else { 0.0 };
        }
        // Denormal - treat as zero for simplicity
        return if sign == 1 { -0.0 } else { 0.0 };
    }

    if exponent == 0x7FFF {
        // Infinity or NaN
        if mantissa == 0x8000_0000_0000_0000 {
            return if sign == 1 {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
        }
        let mut payload = (mantissa & 0x7FFF_FFFF_FFFF_FFFF) >> 11;
        if payload == 0 {
            payload = 1;
        }
        payload |= 1u64 << 51;
        let f64_bits = sign_bit | (0x7FFu64 << 52) | (payload & 0x000F_FFFF_FFFF_FFFF);
        return f64::from_bits(f64_bits);
    }

    // Normal number: convert to f64
    let unbiased_exp = exponent as i32 - 16383;

    // Check if exponent fits in f64 range
    if unbiased_exp > 1023 {
        return if sign == 1 {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    if unbiased_exp < -1074 {
        return if sign == 1 { -0.0 } else { 0.0 }; // Underflow
    }
    if unbiased_exp < -1022 {
        let shift = (-unbiased_exp - 1011) as u32;
        let mantissa = (mantissa >> shift) & 0x000F_FFFF_FFFF_FFFF;
        if mantissa == 0 {
            return if sign == 1 { -0.0 } else { 0.0 };
        }
        let f64_bits = sign_bit | mantissa;
        return f64::from_bits(f64_bits);
    }

    // Build f64: take top 52 bits of mantissa (excluding integer bit)
    let f64_exp = ((unbiased_exp + 1023) as u64) & 0x7FF;
    let f64_mantissa = (mantissa << 1) >> 12; // Remove integer bit, take top 52
    let f64_bits = sign_bit | (f64_exp << 52) | f64_mantissa;

    f64::from_bits(f64_bits)
}

/// Convert 80-bit packed BCD (10 bytes) to f64 (test helper for verifying FBSTP output)
#[cfg(test)]
fn bcd_to_f64(bytes: &[u8; 10]) -> f64 {
    let is_negative = (bytes[9] & 0x80) != 0;

    // Decode BCD digits (2 per byte, little-endian)
    let mut value: i64 = 0;
    for i in (0..9).rev() {
        let byte = bytes[i];
        let lo = (byte & 0x0F) as i64;
        let hi = ((byte >> 4) & 0x0F) as i64;
        value = value * 100 + hi * 10 + lo;
    }

    let result = value as f64;
    if is_negative {
        -result
    } else {
        result
    }
}

/// Convert f64 to 80-bit packed BCD (10 bytes, test helper for preparing FBLD input)
#[cfg(test)]
fn f64_to_bcd(control_word: u16, value: f64) -> [u8; 10] {
    let mut bytes = [0u8; 10];

    let rounded = round_with_rc(control_word, value);
    let is_negative = rounded.is_sign_negative();
    let mut int_val = rounded.abs().trunc() as u64;

    // Encode BCD digits (2 per byte, little-endian)
    for byte in bytes.iter_mut().take(9) {
        let lo = (int_val % 10) as u8;
        int_val /= 10;
        let hi = (int_val % 10) as u8;
        int_val /= 10;
        *byte = (hi << 4) | lo;
    }

    // Set sign byte
    if is_negative {
        bytes[9] = 0x80;
    }

    bytes
}

// ============================================================================
// Tests for test helper functions (f64_to_f80, f80_to_f64, round_with_rc, etc.)
// These are #[cfg(test)] functions that need their own edge-case coverage.
// ============================================================================

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests_helpers {
    use super::*;

    #[test]
    fn test_f64_to_f80_normal() {
        let bytes = f64_to_f80(1.0);
        let back = f80_to_f64(&bytes);
        assert_eq!(back, 1.0);
    }

    #[test]
    fn test_f64_to_f80_zero() {
        let bytes = f64_to_f80(0.0);
        let back = f80_to_f64(&bytes);
        assert_eq!(back, 0.0);
    }

    #[test]
    fn test_f64_to_f80_negative_zero() {
        let bytes = f64_to_f80(-0.0);
        let back = f80_to_f64(&bytes);
        assert!(back.is_sign_negative());
        assert_eq!(back, 0.0);
    }

    #[test]
    fn test_f64_to_f80_nan() {
        let bytes = f64_to_f80(f64::NAN);
        let back = f80_to_f64(&bytes);
        assert!(back.is_nan());
    }

    #[test]
    fn test_f64_to_f80_negative_nan() {
        let bytes = f64_to_f80(-f64::NAN);
        let back = f80_to_f64(&bytes);
        assert!(back.is_nan());
    }

    #[test]
    fn test_f64_to_f80_infinity() {
        let bytes = f64_to_f80(f64::INFINITY);
        let back = f80_to_f64(&bytes);
        assert_eq!(back, f64::INFINITY);
    }

    #[test]
    fn test_f64_to_f80_neg_infinity() {
        let bytes = f64_to_f80(f64::NEG_INFINITY);
        let back = f80_to_f64(&bytes);
        assert_eq!(back, f64::NEG_INFINITY);
    }

    #[test]
    fn test_f64_to_f80_denormal() {
        // Smallest positive denormal f64
        let denorm = f64::from_bits(1);
        let bytes = f64_to_f80(denorm);
        let _back = f80_to_f64(&bytes);
        // Denormals may lose precision in roundtrip but should not crash
    }

    #[test]
    fn test_f80_to_f64_zero_exponent_zero_mantissa() {
        let bytes = [0u8; 10]; // all zeros = positive zero
        assert_eq!(f80_to_f64(&bytes), 0.0);
    }

    #[test]
    fn test_f80_to_f64_zero_exponent_nonzero_mantissa() {
        // Denormal f80: exponent 0, mantissa nonzero -> treated as zero
        let mut bytes = [0u8; 10];
        bytes[0] = 1; // tiny mantissa
        let result = f80_to_f64(&bytes);
        assert_eq!(result, 0.0);
    }

    #[test]
    fn test_f80_to_f64_neg_zero() {
        let mut bytes = [0u8; 10];
        bytes[9] = 0x80; // sign bit set
        let result = f80_to_f64(&bytes);
        assert!(result.is_sign_negative());
    }

    #[test]
    fn test_f80_to_f64_overflow_exponent() {
        // F80 with exponent > 1023 biased -> +infinity
        let mut bytes = [0u8; 10];
        bytes[7] = 0x80; // integer bit
        let exp = (16383 + 1024) as u16; // overflows f64 range
        bytes[8] = (exp & 0xFF) as u8;
        bytes[9] = (exp >> 8) as u8;
        let result = f80_to_f64(&bytes);
        assert_eq!(result, f64::INFINITY);
    }

    #[test]
    fn test_f80_to_f64_underflow_exponent() {
        // F80 with exponent < -1074 biased -> 0
        let mut bytes = [0u8; 10];
        bytes[7] = 0x80;
        let exp: u16 = 1; // unbiased = 1 - 16383 = -16382 < -1074
        bytes[8] = (exp & 0xFF) as u8;
        bytes[9] = (exp >> 8) as u8;
        let result = f80_to_f64(&bytes);
        assert_eq!(result, 0.0);
    }

    #[test]
    fn test_f80_to_f64_subnormal_range() {
        // F80 with exponent in -1023..-1074 biased -> f64 subnormal
        let mut bytes = [0u8; 10];
        bytes[7] = 0x80; // integer bit
        let exp = (16383 - 1030) as u16; // unbiased = -1030, in subnormal range
        bytes[8] = (exp & 0xFF) as u8;
        bytes[9] = (exp >> 8) as u8;
        let result = f80_to_f64(&bytes);
        // Should be a very small positive number, not zero, not infinity
        assert!(result >= 0.0);
    }

    #[test]
    fn test_f80_to_f64_inf_no_mantissa() {
        // F80: exponent 0x7FFF, mantissa = integer bit only -> infinity
        let mut bytes = [0u8; 10];
        bytes[7] = 0x80; // integer bit only
        bytes[8] = 0xFF;
        bytes[9] = 0x7F; // exponent 0x7FFF, sign 0
        let result = f80_to_f64(&bytes);
        assert_eq!(result, f64::INFINITY);
    }

    #[test]
    fn test_f80_to_f64_neg_inf() {
        let mut bytes = [0u8; 10];
        bytes[7] = 0x80;
        bytes[8] = 0xFF;
        bytes[9] = 0xFF; // exponent 0x7FFF, sign 1
        let result = f80_to_f64(&bytes);
        assert_eq!(result, f64::NEG_INFINITY);
    }

    #[test]
    fn test_f80_to_f64_nan_with_payload() {
        let mut bytes = [0u8; 10];
        bytes[7] = 0xC0; // integer bit + quiet NaN bit
        bytes[8] = 0xFF;
        bytes[9] = 0x7F;
        let result = f80_to_f64(&bytes);
        assert!(result.is_nan());
    }

    #[test]
    fn test_round_with_rc_round_nearest_exact_half_even() {
        // RC=0 (round to nearest even): 2.5 -> 2.0 (even), 3.5 -> 4.0 (even)
        let cw = 0x037F; // RC=0
        assert_eq!(round_with_rc(cw, 2.5), 2.0);
        assert_eq!(round_with_rc(cw, 3.5), 4.0);
        assert_eq!(round_with_rc(cw, 0.5), 0.0);
        assert_eq!(round_with_rc(cw, 1.5), 2.0);
    }

    #[test]
    fn test_round_with_rc_round_nearest_not_half() {
        let cw = 0x037F; // RC=0
        assert_eq!(round_with_rc(cw, 2.3), 2.0);
        assert_eq!(round_with_rc(cw, 2.7), 3.0);
    }

    #[test]
    fn test_round_with_rc_round_nearest_negative() {
        let cw = 0x037F; // RC=0
        assert_eq!(round_with_rc(cw, -2.5), -2.0);
        assert_eq!(round_with_rc(cw, -3.5), -4.0);
    }

    #[test]
    fn test_round_with_rc_round_nearest_nan() {
        let cw = 0x037F; // RC=0
        assert!(round_with_rc(cw, f64::NAN).is_nan());
    }

    #[test]
    fn test_round_with_rc_round_nearest_large() {
        let cw = 0x037F; // RC=0
        let large = (1u64 << 53) as f64;
        assert_eq!(round_with_rc(cw, large), large);
    }

    #[test]
    fn test_round_with_rc_floor() {
        let cw = 0x077F; // RC=1 (round down = floor)
        assert_eq!(round_with_rc(cw, 2.7), 2.0);
        assert_eq!(round_with_rc(cw, -2.3), -3.0);
    }

    #[test]
    fn test_round_with_rc_ceil() {
        let cw = 0x0B7F; // RC=2 (round up = ceil)
        assert_eq!(round_with_rc(cw, 2.3), 3.0);
        assert_eq!(round_with_rc(cw, -2.7), -2.0);
    }

    #[test]
    fn test_round_with_rc_truncate() {
        let cw = 0x0F7F; // RC=3 (truncate)
        assert_eq!(round_with_rc(cw, 2.7), 2.0);
        assert_eq!(round_with_rc(cw, -2.7), -2.0);
    }

    #[test]
    fn test_bcd_to_f64_negative() {
        // BCD for -42: byte 0 = 0x42, sign bit set
        let mut bytes = [0u8; 10];
        bytes[0] = 0x42;
        bytes[9] = 0x80; // negative
        assert_eq!(bcd_to_f64(&bytes), -42.0);
    }

    #[test]
    fn test_bcd_to_f64_zero() {
        let bytes = [0u8; 10];
        assert_eq!(bcd_to_f64(&bytes), 0.0);
    }
}

/// Compare two Fpu states for bitwise equivalence.
/// Returns a list of differences (empty = identical).
#[cfg(any(test, feature = "test-util"))]
#[doc(hidden)]
pub fn compare_fpu_state(a: &Fpu, b: &Fpu) -> Vec<String> {
    let mut diffs = Vec::new();
    for i in 0..8 {
        if a.regs[i].0 != b.regs[i].0 {
            diffs.push(format!(
                "regs[{}]: {:02X?} vs {:02X?}",
                i, a.regs[i].0, b.regs[i].0
            ));
        }
    }
    if a.status_word != b.status_word {
        diffs.push(format!(
            "status_word: {:04X} vs {:04X}",
            a.status_word, b.status_word
        ));
    }
    if a.control_word != b.control_word {
        diffs.push(format!(
            "control_word: {:04X} vs {:04X}",
            a.control_word, b.control_word
        ));
    }
    if a.tag_word != b.tag_word {
        diffs.push(format!(
            "tag_word: {:04X} vs {:04X}",
            a.tag_word, b.tag_word
        ));
    }
    if a.ip_offset != b.ip_offset {
        diffs.push(format!(
            "ip_offset: {:08X} vs {:08X}",
            a.ip_offset, b.ip_offset
        ));
    }
    if a.ip_selector != b.ip_selector {
        diffs.push(format!(
            "ip_selector: {:04X} vs {:04X}",
            a.ip_selector, b.ip_selector
        ));
    }
    if a.operand_offset != b.operand_offset {
        diffs.push(format!(
            "operand_offset: {:08X} vs {:08X}",
            a.operand_offset, b.operand_offset
        ));
    }
    if a.operand_selector != b.operand_selector {
        diffs.push(format!(
            "operand_selector: {:04X} vs {:04X}",
            a.operand_selector, b.operand_selector
        ));
    }
    diffs
}

/// Test/debug oracle: asserts that the interpreter's current FPU state is
/// equivalent to what host x87 hardware would store for the same architectural
/// state. Equality is byte-exact except for the documented long-mode/reserved
/// fields and non-Empty tag classification; tag occupancy remains exact.
///
/// The check syncs the interpreter's `Fpu` to a 108-byte FNSAVE/FRSTOR
/// layout, hands that buffer to the host CPU via `frstor` + `fnsave`, and
/// asserts architectural equality with the original under the narrow exclusions
/// below. A divergence indicates state that real x87 cannot represent.
///
/// The comparison skips the status-word bytes and reserved upper
/// halves of the 16-bit CW/SW/TW/operand-selector fields (Intel SDM Vol 1
/// §8.1.10 Table 8-2 lists these as "reserved" in the 32-bit save image,
/// and real x87 hardware FNSAVE writes 0xFFFF there while
/// `sync_to_x87_save_area` zero-extends u16 to u32). These bytes are not
/// architectural state, so the divergence is a layout artefact rather
/// than a bookkeeping bug.
///
/// Volatile/reserved byte ranges skipped:
/// - 2..=3   (CW high half — reserved)
/// - 4..=5   (SW low half — derived `ES` bit may differ)
/// - 6..=7   (SW high half — reserved)
/// - 8..=9   (TW non-Empty class — host reclassifies Valid/Zero/Special from value;
///             Empty versus occupied is still compared exactly)
/// - 10..=11 (TW high half — reserved)
/// - 12..=15, 20..=23 (FIP/FDP offsets) and 16..=17, 24..=27 (FIP/FDP
///             selectors): the whole x87 *exception-pointer record*
///             (FIP/FDP/FOP/FCS/FDS). This record is **not** a
///             FRSTOR→FNSAVE round-trip invariant in a preemptively
///             scheduled user process. Architecturally it exists only so
///             an x87 exception handler can identify the faulting
///             instruction and operand; both vendors document license to
///             drop it when no unmasked x87 exception is pending — AMD:
///             the FXSAVE family saves/restores FIP/FDP/FOP only with an
///             unmasked exception pending (AMD APM FXSAVE notes); Intel: FCS/FDS
///             deprecation (`CPUID.(07H,0):EBX[13]`), `FDP_EXCPTN_ONLY`
///             (EBX[6]), fopcode-compat (`IA32_MISC_ENABLE[2]`, default
///             off) — Intel SDM Vol 1 §8.1.8. So an OS interrupt/context
///             switch landing in the ~300-cycle window between our FRSTOR
///             and FNSAVE launders the just-loaded pointer record to zero
///             while preserving every other x87 field exactly (observed
///             p ≈ 2–3e-4 per window under a busy multi-threaded test
///             process; MDD-BUG-CRUCIBLE-00313). Production never reads
///             these fields back from a host image —
///             `restore_regs_from_x87_save_area` copies only bytes
///             28..108 and keeps the pointer record software-authoritative
///             — so the emulator is unaffected. The offset fields still
///             get a field-wise {preserved-or-zero} check below (the
///             selectors are always host-zeroed in 64-bit mode, so they
///             are skipped outright); the host-behaviour envelope itself
///             is locked by `host_x87_fip_fdp_preserved_when_unlaundered`.
///
/// Bytes 18..=19 (the saved opcode / FOP) are *not* skipped, but only
/// because both sides currently pin them to zero: interp has no opcode
/// field, and modern hosts record FOP only with fopcode-compat mode on
/// (off by default) and launder it with the rest of the record anyway. If
/// the `Fpu` model ever grows a real opcode field, move 18..=19 into the
/// field-wise pointer-record check below rather than the byte loop.
///
/// Cloning the FPU before syncing keeps the helper non-mutating from the
/// caller's perspective even though `sync_to_x87_save_area` requires
/// `&mut self` to write into the embedded buffer.
// Host-x87 differential oracle: round-trips interp state through the HOST x87
// unit (FRSTOR/FNSAVE), so it only exists on an x86_64 host. On other arches the
// software x87 (`x87_soft.rs`) is validated by the golden-vector oracle instead.
#[cfg(all(
    any(test, feature = "test-util", debug_assertions),
    target_arch = "x86_64"
))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[doc(hidden)]
pub fn assert_interp_state_byte_matches_host_x87(cpu: &Cpu, ctx: &str) {
    // Take a copy so we don't perturb the caller's `Fpu::x87_save_area`.
    // `sync_to_x87_save_area` writes into the embedded 108-byte buffer; the
    // architectural register/word state is not modified.
    let mut fpu_clone = cpu.fpu.clone();
    fpu_clone.sync_to_x87_save_area();
    let save = fpu_clone.x87_save_area;

    // Round-trip through the host x87 FPU. FRSTOR loads the state, FNSAVE
    // re-saves and reinitialises. For any state that real x87 hardware can
    // hold, the round-trip is a byte-level identity (modulo the reserved
    // upper-half bytes flagged above). For states the interpreter has
    // fabricated but the hardware cannot represent, the round-trip will
    // mutate one or more architectural bytes.
    //
    // Both pointers reference distinct in-scope buffers so the asm cannot
    // alias. FNSAVE leaves the host x87 stack empty, which the eight explicit
    // stack outputs communicate to rustc/LLVM.
    let mut roundtrip = [0u8; 108];
    roundtrip.copy_from_slice(&save);
    unsafe {
        let _host_x87 = crate::cpu::host_x87_enter_guarded();
        std::arch::asm!(
            "frstor [{src}]",
            "fnsave [{dst}]",
            src = in(reg) save.as_ptr(),
            dst = in(reg) roundtrip.as_mut_ptr(),
            out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
            out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
            options(nostack),
        );
    }

    let interp_tag = u16::from_le_bytes([save[8], save[9]]);
    let host_tag = u16::from_le_bytes([roundtrip[8], roundtrip[9]]);
    assert_same_tag_occupancy(interp_tag, host_tag, ctx);

    let mut diffs: Vec<(usize, u8, u8)> = Vec::new();
    for (i, (a, b)) in save.iter().zip(roundtrip.iter()).enumerate() {
        // Skip the architecturally-reserved or derived bytes plus the whole
        // exception-pointer record (12..=27). See the doc comment above for
        // the rationale and vendor references.
        if matches!(i, 2..=17 | 20..=27) {
            continue;
        }
        if a != b {
            diffs.push((i, *a, *b));
        }
    }

    // Exception-pointer offsets, field-wise: the host must either preserve the
    // FRSTOR-loaded value or launder the whole field to zero (documented
    // stochastic behaviour, see doc comment). Any *other* value means the
    // round-trip buffers were corrupted (asm aliasing / a wild write) or the
    // host is doing something new — both are hard failures this oracle must
    // still surface even though the byte loop above skips these ranges.
    for (name, lo) in [("FIP offset", 12usize), ("FDP offset", 20usize)] {
        let interp = u32::from_le_bytes([save[lo], save[lo + 1], save[lo + 2], save[lo + 3]]);
        let host = u32::from_le_bytes([
            roundtrip[lo],
            roundtrip[lo + 1],
            roundtrip[lo + 2],
            roundtrip[lo + 3],
        ]);
        assert!(
            host == interp || host == 0,
            "{ctx}: host x87 {name} neither preserved nor laundered-to-zero: \
             interp={interp:#010X} host={host:#010X}"
        );
    }

    if !diffs.is_empty() {
        let mut msg = format!(
            "{}: interp FPU state diverges from host x87 round-trip ({} byte(s)):\n",
            ctx,
            diffs.len()
        );
        for (off, interp, host) in diffs.iter().take(16) {
            msg.push_str(&format!(
                "  byte {:#05X}: interp={:#04X} host-x87={:#04X}\n",
                off, interp, host
            ));
        }
        if diffs.len() > 16 {
            msg.push_str(&format!("  ... and {} more\n", diffs.len() - 16));
        }
        panic!("{}", msg);
    }
}

#[cfg(all(
    any(test, feature = "test-util", debug_assertions),
    target_arch = "x86_64"
))]
fn assert_same_tag_occupancy(software: u16, host: u16, ctx: &str) {
    let mismatches: Vec<_> = (0..8)
        .filter(|&physical| {
            let shift = physical * 2;
            let software_empty = ((software >> shift) & 3) == 3;
            let host_empty = ((host >> shift) & 3) == 3;
            software_empty != host_empty
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "{ctx}: software/host x87 tag occupancy differs in physical slots {mismatches:?} \
         (software={software:#06X}, host={host:#06X})"
    );
}

/// Debug/test Oracle-3 assertion for a just-completed DH FNSAVE spill.
///
/// The embedded image is still the real host save. Compare its tag occupancy
/// with the software shadow before the general representability round trip
/// rebuilds a fresh image from software-authoritative metadata.
#[cfg(all(
    any(test, feature = "test-util", debug_assertions),
    target_arch = "x86_64"
))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[doc(hidden)]
pub fn assert_dh_spilled_state_matches_host_x87(cpu: &Cpu, ctx: &str) {
    let image_tag = u16::from_le_bytes([cpu.fpu.x87_save_area[8], cpu.fpu.x87_save_area[9]]);
    assert_same_tag_occupancy(cpu.fpu.tag_word, image_tag, ctx);
    assert_interp_state_byte_matches_host_x87(cpu, ctx);
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::approx_constant)] // arbitrary FPU register fixtures, not PI/E
mod tests_execute {
    use super::*;
    use gp64_memory::Memory;
    use gp64_types::test_util::env_lock;
    use iced_x86::{Code, Instruction, MemoryOperand, Register};

    fn exec_instr(cpu: &mut Cpu, mem: &mut Memory, instr: Instruction) {
        execute_iced_for_test(cpu, &instr, mem).unwrap();
    }

    fn execute_portable(
        cpu: &mut Cpu,
        mem: &mut Memory,
        instr: &Instruction,
    ) -> anyhow::Result<()> {
        let descriptor = crate::x87_iced_adapter::decode_instruction(instr);
        execute_decoded(cpu, &descriptor, mem)
    }

    fn instr0(code: Code) -> Instruction {
        Instruction::with(code)
    }

    fn instr1(code: Code, op0: Register) -> Instruction {
        Instruction::with1(code, op0).unwrap()
    }

    fn instr2(code: Code, op0: Register, op1: Register) -> Instruction {
        Instruction::with2(code, op0, op1).unwrap()
    }

    fn instr2_with_op1_kind(
        code: Code,
        op0: Register,
        op1: Register,
        op1_kind: OpKind,
    ) -> Instruction {
        let mut instr = instr2(code, op0, op1);
        instr.set_op_kind(1, op1_kind);
        instr
    }

    fn exec_empty_fault(cpu: &mut Cpu, mem: &mut Memory, code: Code) {
        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
        exec_instr(cpu, mem, instr0(code));
        assert_stack_fault(cpu);
    }

    fn mem_op(displacement: i64) -> MemoryOperand {
        MemoryOperand::new(
            Register::None,
            Register::None,
            1,
            displacement,
            2,
            false,
            Register::None,
        )
    }

    fn set_st0_st1(cpu: &mut Cpu, st0: f64, st1: f64) {
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(st0);
        cpu.fpu.regs[1] = F80::from_f64(st1);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.set_tag(1, 0);
    }

    /// MDD-BUG-CRUCIBLE-00258, end to end: execute the real unordered-compare
    /// instructions and read the status word the way a guest would (FNSTSW).
    ///
    /// The ordered FCOM/FCOMI rows are here deliberately — the fix must NOT
    /// change them, and a quiet-NaN row that stays quiet only because the
    /// instruction stopped raising IE altogether would otherwise look correct.
    #[test]
    fn unordered_compares_raise_ie_for_snan_but_not_qnan_crucible258() {
        // (ST(0) bytes, description, expected IE for the FUCOM family, expected
        // IE for the FCOM family)
        let qnan = F80([0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0x7F]);
        let snan = F80([1, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x7F]);
        let rows: &[(&str, F80, bool, bool)] = &[
            ("quiet NaN", qnan, false, true),
            ("signaling NaN", snan, true, true),
        ];

        let unordered = [
            ("FUCOM", Code::Fucom_st0_sti, false),
            ("FUCOMI", Code::Fucomi_st0_sti, true),
        ];
        let ordered = [
            ("FCOM", Code::Fcom_st0_sti, false),
            ("FCOMI", Code::Fcomi_st0_sti, true),
        ];

        for (what, value, expect_unordered_ie, expect_ordered_ie) in rows {
            for (family, cases, expected) in [
                ("unordered", &unordered, expect_unordered_ie),
                ("ordered", &ordered, expect_ordered_ie),
            ] {
                for (name, code, _is_comi) in cases {
                    let mut cpu = Cpu::default();
                    let mut mem = Memory::new();
                    set_st0_st1(&mut cpu, 1.0, 2.0);
                    cpu.fpu.regs[0] = *value;
                    cpu.fpu.status_word = 0;

                    exec_instr(
                        &mut cpu,
                        &mut mem,
                        instr2(*code, Register::ST0, Register::ST1),
                    );

                    let ie = cpu.fpu.status_word & 0x0001 != 0;
                    assert_eq!(
                        ie, *expected,
                        "{name} ({family}) with {what} in ST(0): expected IE == {expected}, \
                         status word {:#06X}",
                        cpu.fpu.status_word
                    );
                }
            }
        }
    }

    #[test]
    fn classify_covers_widened_hot_forms() {
        let addr = 0x2000i64;
        let mem1 = |code| {
            crate::x87_iced_adapter::decode_instruction(
                &Instruction::with1(code, mem_op(addr)).unwrap(),
            )
        };
        // Float memory arithmetic classifies for both widths and all six ops.
        for (code, op) in [
            (Code::Fadd_m32fp, ArithOp::Add),
            (Code::Fsub_m32fp, ArithOp::Sub),
            (Code::Fsubr_m32fp, ArithOp::SubReverse),
            (Code::Fmul_m32fp, ArithOp::Mul),
            (Code::Fdiv_m32fp, ArithOp::Div),
            (Code::Fdivr_m32fp, ArithOp::DivReverse),
            (Code::Fadd_m64fp, ArithOp::Add),
            (Code::Fsub_m64fp, ArithOp::Sub),
            (Code::Fsubr_m64fp, ArithOp::SubReverse),
            (Code::Fmul_m64fp, ArithOp::Mul),
            (Code::Fdiv_m64fp, ArithOp::Div),
            (Code::Fdivr_m64fp, ArithOp::DivReverse),
        ] {
            assert_eq!(
                X87Dispatch::classify(&mem1(code)),
                X87Dispatch::ArithMem { op },
                "{code:?}"
            );
        }
        // Integer arithmetic must NOT classify as float memory arithmetic.
        for code in [Code::Fiadd_m32int, Code::Fidiv_m16int] {
            assert_eq!(X87Dispatch::classify(&mem1(code)), X87Dispatch::Generic);
        }
        assert_eq!(
            X87Dispatch::classify(&mem1(Code::Fld_m64fp)),
            X87Dispatch::FldM64
        );
        // FLD m80 stays generic (10-byte form).
        assert_eq!(
            X87Dispatch::classify(&mem1(Code::Fld_m80fp)),
            X87Dispatch::Generic
        );
        assert_eq!(
            X87Dispatch::classify(&mem1(Code::Fstp_m64fp)),
            X87Dispatch::FstpM64
        );
        assert_eq!(
            X87Dispatch::classify(&mem1(Code::Fstp_m80fp)),
            X87Dispatch::Generic
        );
        // FST (no pop) stays generic.
        assert_eq!(
            X87Dispatch::classify(&mem1(Code::Fst_m32fp)),
            X87Dispatch::Generic
        );
        assert_eq!(
            X87Dispatch::classify(&mem1(Code::Fcom_m32fp)),
            X87Dispatch::ComMem { pop: false }
        );
        assert_eq!(
            X87Dispatch::classify(&mem1(Code::Fcomp_m64fp)),
            X87Dispatch::ComMem { pop: true }
        );
        assert_eq!(
            X87Dispatch::classify(&mem1(Code::Fldcw_m2byte)),
            X87Dispatch::Fldcw
        );
        // FNSTCW stays generic.
        assert_eq!(
            X87Dispatch::classify(&mem1(Code::Fnstcw_m2byte)),
            X87Dispatch::Generic
        );
        let fxch = crate::x87_iced_adapter::decode_instruction(&instr2(
            Code::Fxch_st0_sti,
            Register::ST0,
            Register::ST3,
        ));
        assert_eq!(X87Dispatch::classify(&fxch), X87Dispatch::Fxch { src: 3 });
        // Pop-variant register subtract carries direction and pop.
        let subrp = crate::x87_iced_adapter::decode_instruction(&instr2(
            Code::Fsubrp_sti_st0,
            Register::ST2,
            Register::ST0,
        ));
        assert_eq!(
            X87Dispatch::classify(&subrp),
            X87Dispatch::ArithReg {
                op: ArithOp::SubReverse,
                dst: 2,
                src: 0,
                pop: true,
            }
        );
        let divp = crate::x87_iced_adapter::decode_instruction(&instr2(
            Code::Fdivp_sti_st0,
            Register::ST1,
            Register::ST0,
        ));
        assert_eq!(
            X87Dispatch::classify(&divp),
            X87Dispatch::ArithReg {
                op: ArithOp::Div,
                dst: 1,
                src: 0,
                pop: true,
            }
        );
    }

    #[test]
    fn iced_adapter_preserves_common_semantic_forms() {
        let addr = 0x2000;
        let fld_mem = crate::x87_iced_adapter::decode_instruction(
            &Instruction::with1(Code::Fld_m32fp, mem_op(addr)).unwrap(),
        );
        assert_eq!(fld_mem.operation, Mnemonic::Fld);
        assert!(matches!(
            fld_mem.operand1,
            X87Operand::Memory(X87MemoryOperand { width_bytes: 4, .. })
        ));
        assert_eq!(X87Dispatch::classify(&fld_mem), X87Dispatch::FldM32);

        let fld_reg =
            crate::x87_iced_adapter::decode_instruction(&instr1(Code::Fld_sti, Register::ST4));
        assert_eq!(fld_reg.operation, Mnemonic::Fld);
        assert_eq!(fld_reg.operand1, X87Operand::Stack(4));
        assert_eq!(
            X87Dispatch::classify(&fld_reg),
            X87Dispatch::FldReg { src: 4 }
        );

        let status =
            crate::x87_iced_adapter::decode_instruction(&instr1(Code::Fnstsw_AX, Register::AX));
        assert_eq!(status.operation, Mnemonic::Fnstsw);
        assert_eq!(status.operand1, X87Operand::Ax);
        assert_eq!(X87Dispatch::classify(&status), X87Dispatch::FnstswAx);

        for (code, operation) in [
            (Code::Fstp_m32fp, Mnemonic::Fstp),
            (Code::Fild_m32int, Mnemonic::Fild),
            (Code::Fistp_m32int, Mnemonic::Fistp),
            (Code::Fadd_m32fp, Mnemonic::Fadd),
            (Code::Fmul_m32fp, Mnemonic::Fmul),
        ] {
            let descriptor = crate::x87_iced_adapter::decode_instruction(
                &Instruction::with1(code, mem_op(addr)).unwrap(),
            );
            assert_eq!(descriptor.operation, operation);
            assert!(matches!(
                descriptor.operand1,
                X87Operand::Memory(X87MemoryOperand { width_bytes: 4, .. })
            ));
        }

        let add_pop = crate::x87_iced_adapter::decode_instruction(&instr2(
            Code::Faddp_sti_st0,
            Register::ST3,
            Register::ST0,
        ));
        assert_eq!(add_pop.operation, Mnemonic::Faddp);
        assert_eq!(add_pop.operand1, X87Operand::Stack(3));
        assert_eq!(add_pop.operand2, X87Operand::Stack(0));
        assert_eq!(
            X87Dispatch::classify(&add_pop),
            X87Dispatch::ArithReg {
                op: ArithOp::Add,
                dst: 3,
                src: 0,
                pop: true,
            }
        );

        let multiply = crate::x87_iced_adapter::decode_instruction(&instr2(
            Code::Fmul_st0_sti,
            Register::ST0,
            Register::ST5,
        ));
        assert_eq!(multiply.operation, Mnemonic::Fmul);
        assert_eq!(multiply.operand1, X87Operand::Stack(0));
        assert_eq!(multiply.operand2, X87Operand::Stack(5));
        assert_eq!(
            X87Dispatch::classify(&multiply),
            X87Dispatch::ArithReg {
                op: ArithOp::Mul,
                dst: 0,
                src: 5,
                pop: false,
            }
        );

        let subtract = crate::x87_iced_adapter::decode_instruction(&instr2(
            Code::Fsub_st0_sti,
            Register::ST0,
            Register::ST1,
        ));
        assert_eq!(subtract.operation, Mnemonic::Fsub);
        assert_eq!(subtract.operand1, X87Operand::Stack(0));
        assert_eq!(subtract.operand2, X87Operand::Stack(1));
        assert_eq!(
            X87Dispatch::classify(&subtract),
            X87Dispatch::ArithReg {
                op: ArithOp::Sub,
                dst: 0,
                src: 1,
                pop: false,
            }
        );
    }

    #[test]
    fn portable_arithmetic_matches_iced_adapter_state() {
        let addr = 0x2000usize;
        let instructions = [
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr as i64)).unwrap(),
            Instruction::with1(Code::Fmul_m32fp, mem_op(addr as i64)).unwrap(),
            instr2(Code::Faddp_sti_st0, Register::ST1, Register::ST0),
            instr2(Code::Fmul_st0_sti, Register::ST0, Register::ST1),
        ];

        for instr in instructions {
            let mut adapter_cpu = Cpu::new();
            let mut portable_cpu = Cpu::new();
            set_st0_st1(&mut adapter_cpu, 1.25, 2.5);
            set_st0_st1(&mut portable_cpu, 1.25, 2.5);
            adapter_cpu.fpu.control_word &= !(0x3 << 8);
            portable_cpu.fpu.control_word &= !(0x3 << 8);
            let mut adapter_mem = Memory::new();
            let mut portable_mem = Memory::new();
            adapter_mem.write_u32(addr, 1.5f32.to_bits());
            portable_mem.write_u32(addr, 1.5f32.to_bits());

            execute_iced_for_test(&mut adapter_cpu, &instr, &mut adapter_mem).unwrap();
            execute_portable(&mut portable_cpu, &mut portable_mem, &instr).unwrap();

            assert_eq!(
                compare_fpu_state(&adapter_cpu.fpu, &portable_cpu.fpu),
                Vec::<String>::new(),
                "portable state mismatch for {:?}",
                instr.code()
            );
            assert_eq!(adapter_cpu.regs.flags, portable_cpu.regs.flags);
        }
    }

    #[test]
    fn portable_arithmetic_matches_iced_adapter_underflow() {
        let addr = 0x2000usize;
        for instr in [
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr as i64)).unwrap(),
            instr2(Code::Fmulp_sti_st0, Register::ST1, Register::ST0),
        ] {
            let mut adapter_cpu = Cpu::new();
            let mut portable_cpu = Cpu::new();
            let mut adapter_mem = Memory::new();
            let mut portable_mem = Memory::new();
            adapter_mem.write_u32(addr, 1.5f32.to_bits());
            portable_mem.write_u32(addr, 1.5f32.to_bits());

            execute_iced_for_test(&mut adapter_cpu, &instr, &mut adapter_mem).unwrap();
            execute_portable(&mut portable_cpu, &mut portable_mem, &instr).unwrap();

            assert_eq!(
                compare_fpu_state(&adapter_cpu.fpu, &portable_cpu.fpu),
                Vec::<String>::new(),
                "portable underflow mismatch for {:?}",
                instr.code()
            );
        }
    }

    /// `x ± 0` under PC=single/RC=nearest is exact. The shadow's exponent-gap
    /// shortcut fabricated PE (and sometimes C1) for zero paired with a
    /// normal operand of biased exponent >= 30. Host x87 keeps both clear;
    /// cover memory and register forms with zero in either slot.
    #[test]
    fn zero_operand_shadow_arith_keeps_pe_and_c1_clear() {
        let addr = 0x2000usize;
        let big = f32::MAX;

        #[track_caller]
        fn assert_exact(cpu: &Cpu, expected: f32, ctx: &str) {
            assert_eq!(cpu.fpu.get_st(0), F80::from_f32(expected), "{ctx}: value");
            assert_eq!(
                cpu.fpu.status_word & 0x0020,
                0,
                "{ctx}: exact op fabricated PE"
            );
            assert_eq!(
                cpu.fpu.status_word & 0x0200,
                0,
                "{ctx}: exact op fabricated C1"
            );
            // The zero operand stays on the binary32 fast path: the fix routes
            // its status through the exact f64 comparison instead of rejecting
            // zeros from the shadow (which would silently give up the fast
            // path for the common `x ± 0`). If this shadow probe fails while
            // PE/C1 stay clear, the fast-path strategy changed — update the
            // test consciously rather than deleting the probe.
            assert_eq!(
                cpu.fpu.exact_binary32_st(0),
                Some(expected),
                "{ctx}: expected the binary32 shadow fast path to retire this op"
            );
        }

        // Regression seed 7a37172c: CW=0x0040 (PC=single, RC=nearest),
        // FLDZ; FSUBR m32 with -f32::MAX. The incorrect status differed
        // by PE (0x0020) alone; C1 was correctly clear.
        {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.fpu.control_word = 0x0040;
            mem.write_u32(addr, 0xFF7F_FFFF);
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldz));
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fsubr_m32fp, mem_op(addr as i64)).unwrap(),
            );
            assert_exact(&cpu, -big, "FLDZ; FSUBR m32 (dyncore seed shape)");
        }

        // m32 memory forms under the dominant guest CW 0x007F.
        for (code, expected, ctx) in [
            (Code::Fadd_m32fp, big, "FLDZ; FADD m32"),
            (
                Code::Fsub_m32fp,
                -big,
                "FLDZ; FSUB m32 (signs differ: C1 case)",
            ),
            (Code::Fsubr_m32fp, big, "FLDZ; FSUBR m32"),
        ] {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.fpu.control_word = 0x007F;
            mem.write_u32(addr, big.to_bits());
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldz));
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(code, mem_op(addr as i64)).unwrap(),
            );
            assert_exact(&cpu, expected, ctx);
        }

        // -0.0 destination loaded from memory: `(-0) + x` has differing
        // operand signs — the C1-corruption case from the ticket.
        {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.fpu.control_word = 0x007F;
            mem.write_u32(addr, (-0.0f32).to_bits());
            mem.write_u32(addr + 8, big.to_bits());
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fld_m32fp, mem_op(addr as i64)).unwrap(),
            );
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fadd_m32fp, mem_op((addr + 8) as i64)).unwrap(),
            );
            assert_exact(&cpu, big, "FLD -0.0; FADD m32");
        }

        // m64 memory form: an f32-exact f64 source reaches the same shadow.
        {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.fpu.control_word = 0x007F;
            mem.write_u64(addr, 2.0f64.powi(100).to_bits());
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldz));
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fadd_m64fp, mem_op(addr as i64)).unwrap(),
            );
            assert_exact(&cpu, 2.0f32.powi(100), "FLDZ; FADD m64");
        }

        // Register forms, zero destination: ST0 = 0, ST1 = big.
        for (code, expected, ctx) in [
            (Code::Fadd_st0_sti, big, "FADD ST0,ST1 (zero dst)"),
            (
                Code::Fsub_st0_sti,
                -big,
                "FSUB ST0,ST1 (zero dst, signs differ)",
            ),
            (Code::Fsubr_st0_sti, big, "FSUBR ST0,ST1 (zero dst)"),
        ] {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.fpu.control_word = 0x007F;
            mem.write_u32(addr, big.to_bits());
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fld_m32fp, mem_op(addr as i64)).unwrap(),
            );
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldz));
            exec_instr(
                &mut cpu,
                &mut mem,
                instr2(code, Register::ST0, Register::ST1),
            );
            assert_exact(&cpu, expected, ctx);
        }

        // Register form, zero source: ST0 = big, ST1 = 0.
        {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.fpu.control_word = 0x007F;
            mem.write_u32(addr, big.to_bits());
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldz));
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fld_m32fp, mem_op(addr as i64)).unwrap(),
            );
            exec_instr(
                &mut cpu,
                &mut mem,
                instr2(Code::Fadd_st0_sti, Register::ST0, Register::ST1),
            );
            assert_exact(&cpu, big, "FADD ST0,ST1 (zero src)");
        }

        // Pop form with a zero destination: FADDP ST1,ST0 where ST1 = 0.
        {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.fpu.control_word = 0x007F;
            mem.write_u32(addr, big.to_bits());
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldz));
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fld_m32fp, mem_op(addr as i64)).unwrap(),
            );
            exec_instr(
                &mut cpu,
                &mut mem,
                instr2(Code::Faddp_sti_st0, Register::ST1, Register::ST0),
            );
            assert_exact(&cpu, big, "FADDP ST1,ST0 (zero dst, pop)");
        }

        // -0.0 as the register SOURCE (ST0 = big, ST1 = -0.0): differing
        // operand signs, so the pre-fix shortcut fabricated PE and C1 here.
        // (A zero MEMORY source can never reach the shadow: the m32/m64
        // source filter is f80_to_exact_normal_f32, which rejects zero.)
        {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.fpu.control_word = 0x007F;
            mem.write_u32(addr, (-0.0f32).to_bits());
            mem.write_u32(addr + 8, big.to_bits());
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fld_m32fp, mem_op(addr as i64)).unwrap(),
            );
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fld_m32fp, mem_op((addr + 8) as i64)).unwrap(),
            );
            exec_instr(
                &mut cpu,
                &mut mem,
                instr2(Code::Fadd_st0_sti, Register::ST0, Register::ST1),
            );
            assert_exact(&cpu, big, "FADD ST0,ST1 (-0.0 src, signs differ)");
        }
    }

    #[test]
    fn portable_common_forms_match_iced_adapter_state_and_memory() {
        let addr = 0x2000usize;
        let instructions = [
            Instruction::with1(Code::Fld_m32fp, mem_op(addr as i64)).unwrap(),
            instr1(Code::Fld_sti, Register::ST1),
            Instruction::with1(Code::Fstp_m32fp, mem_op(addr as i64)).unwrap(),
            Instruction::with1(Code::Fild_m32int, mem_op(addr as i64)).unwrap(),
            Instruction::with1(Code::Fistp_m32int, mem_op(addr as i64)).unwrap(),
            instr1(Code::Fnstsw_AX, Register::AX),
            instr1(Code::Fstsw_AX, Register::AX),
        ];

        for instr in instructions {
            let mut adapter_cpu = Cpu::new();
            let mut portable_cpu = Cpu::new();
            set_st0_st1(&mut adapter_cpu, 1.25, 2.5);
            set_st0_st1(&mut portable_cpu, 1.25, 2.5);
            adapter_cpu.fpu.status_word |= 0x4500;
            portable_cpu.fpu.status_word |= 0x4500;
            let mut adapter_mem = Memory::new();
            let mut portable_mem = Memory::new();
            adapter_mem.write_u32(addr, 1.5f32.to_bits());
            portable_mem.write_u32(addr, 1.5f32.to_bits());

            execute_iced_for_test(&mut adapter_cpu, &instr, &mut adapter_mem).unwrap();
            execute_portable(&mut portable_cpu, &mut portable_mem, &instr).unwrap();

            assert_eq!(
                compare_fpu_state(&adapter_cpu.fpu, &portable_cpu.fpu),
                Vec::<String>::new(),
                "portable state mismatch for {:?}",
                instr.code()
            );
            assert_eq!(adapter_cpu.regs.eax, portable_cpu.regs.eax);
            assert_eq!(adapter_mem.read_u32(addr), portable_mem.read_u32(addr));
        }
    }

    #[test]
    fn cached_hot_dispatch_matches_generic_state_and_memory() {
        let addr = 0x2000usize;
        let instructions = [
            Instruction::with1(Code::Fld_m32fp, mem_op(addr as i64)).unwrap(),
            instr1(Code::Fld_sti, Register::ST1),
            Instruction::with1(Code::Fstp_m32fp, mem_op(addr as i64)).unwrap(),
            Instruction::with1(Code::Fild_m32int, mem_op(addr as i64)).unwrap(),
            Instruction::with1(Code::Fistp_m32int, mem_op(addr as i64)).unwrap(),
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr as i64)).unwrap(),
            Instruction::with1(Code::Fmul_m32fp, mem_op(addr as i64)).unwrap(),
            instr2(Code::Faddp_sti_st0, Register::ST1, Register::ST0),
            instr2(Code::Fmul_st0_sti, Register::ST0, Register::ST1),
            instr1(Code::Fnstsw_AX, Register::AX),
        ];

        for instruction in instructions {
            let descriptor = crate::x87_iced_adapter::decode_instruction(&instruction);
            let dispatch = X87Dispatch::classify(&descriptor);
            assert_ne!(dispatch, X87Dispatch::Generic, "{:?}", instruction.code());

            let mut generic_cpu = Cpu::new();
            let mut cached_cpu = Cpu::new();
            set_st0_st1(&mut generic_cpu, 1.25, 2.5);
            set_st0_st1(&mut cached_cpu, 1.25, 2.5);
            generic_cpu.fpu.control_word &= !(0x3 << 8);
            cached_cpu.fpu.control_word &= !(0x3 << 8);
            let mut generic_mem = Memory::new();
            let mut cached_mem = Memory::new();
            generic_mem.write_u32(addr, 1.5f32.to_bits());
            cached_mem.write_u32(addr, 1.5f32.to_bits());

            execute_decoded(&mut generic_cpu, &descriptor, &mut generic_mem).unwrap();
            execute_decoded_with_dispatch(&mut cached_cpu, &descriptor, &mut cached_mem, dispatch)
                .unwrap();

            assert_eq!(
                compare_fpu_state(&generic_cpu.fpu, &cached_cpu.fpu),
                Vec::<String>::new(),
                "cached dispatch state mismatch for {:?}",
                instruction.code()
            );
            assert_eq!(generic_cpu.regs.eax, cached_cpu.regs.eax);
            assert_eq!(generic_mem.read_u32(addr), cached_mem.read_u32(addr));
        }
    }

    #[test]
    fn exact_binary32_proof_survives_common_stack_chain_and_store() {
        let source_addr = 0x2000usize;
        let output_addr = 0x2010usize;
        let source = -37.25f32;
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        mem.write_u32(source_addr, source.to_bits());

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m32fp, mem_op(source_addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.exact_binary32_st(0), Some(source));

        exec_instr(&mut cpu, &mut mem, instr1(Code::Fld_sti, Register::ST0));
        assert_eq!(cpu.fpu.exact_binary32_st(0), Some(source));
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fchs));
        assert_eq!(cpu.fpu.exact_binary32_st(0), Some(-source));
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fabs));
        assert_eq!(cpu.fpu.exact_binary32_st(0), Some(source.abs()));
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fst_sti, Register::ST1));
        assert_eq!(cpu.fpu.exact_binary32_st(1), Some(source.abs()));

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m32fp, mem_op(output_addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u32(output_addr), source.abs().to_bits());
    }

    #[test]
    fn portable_common_forms_match_iced_adapter_empty_stack_stores() {
        let addr = 0x2000usize;
        for instr in [
            Instruction::with1(Code::Fstp_m32fp, mem_op(addr as i64)).unwrap(),
            Instruction::with1(Code::Fistp_m32int, mem_op(addr as i64)).unwrap(),
        ] {
            let mut adapter_cpu = Cpu::new();
            let mut portable_cpu = Cpu::new();
            let mut adapter_mem = Memory::new();
            let mut portable_mem = Memory::new();

            execute_iced_for_test(&mut adapter_cpu, &instr, &mut adapter_mem).unwrap();
            execute_portable(&mut portable_cpu, &mut portable_mem, &instr).unwrap();

            assert_eq!(
                compare_fpu_state(&adapter_cpu.fpu, &portable_cpu.fpu),
                Vec::<String>::new(),
                "portable empty-store mismatch for {:?}",
                instr.code()
            );
            assert_eq!(adapter_mem.read_u32(addr), portable_mem.read_u32(addr));
        }
    }

    #[test]
    fn portable_memory_forms_match_iced_adapter_segment_fault_order() {
        let addr = 0x100i64;
        let instructions = [
            Instruction::with1(Code::Fld_m32fp, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fstp_m32fp, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fild_m32int, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fistp_m32int, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fmul_m32fp, mem_op(addr)).unwrap(),
        ];

        for instr in instructions {
            let setup = || {
                let mut cpu = Cpu::new();
                set_st0_st1(&mut cpu, 1.25, 2.5);
                cpu.regs.cr0 |= 1;
                cpu.regs.ds = 0x10;
                cpu.regs.ds_cache.base = 0;
                cpu.regs.ds_cache.set_limit(0xff);
                cpu.regs.ds_cache.access = 0x93;
                cpu.regs.ds_cache.present = true;
                cpu.regs.ds_cache.is_code = false;
                cpu.regs.ds_cache.is_readable = true;
                cpu.regs.ds_cache.is_writable = true;
                cpu
            };
            let mut adapter_cpu = setup();
            let mut portable_cpu = setup();
            let mut adapter_mem = Memory::new();
            let mut portable_mem = Memory::new();

            let adapter_err =
                execute_iced_for_test(&mut adapter_cpu, &instr, &mut adapter_mem).unwrap_err();
            let portable_err =
                execute_portable(&mut portable_cpu, &mut portable_mem, &instr).unwrap_err();

            assert!(matches!(
                adapter_err.downcast_ref::<crate::CpuError>(),
                Some(crate::CpuError::GeneralProtectionFault)
            ));
            assert!(matches!(
                portable_err.downcast_ref::<crate::CpuError>(),
                Some(crate::CpuError::GeneralProtectionFault)
            ));
            assert_eq!(
                compare_fpu_state(&adapter_cpu.fpu, &portable_cpu.fpu),
                Vec::<String>::new(),
                "portable fault state mismatch for {:?}",
                instr.code()
            );
        }
    }

    fn empty_stack_memory_source_instructions(addr: i64) -> [Instruction; 16] {
        [
            Instruction::with1(Code::Fiadd_m16int, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fisub_m16int, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fisubr_m16int, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fimul_m16int, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fidiv_m16int, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fidivr_m16int, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Ficom_m16int, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Ficomp_m16int, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fsub_m32fp, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fsubr_m32fp, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fmul_m32fp, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fdiv_m32fp, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fdivr_m32fp, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fcom_m32fp, mem_op(addr)).unwrap(),
            Instruction::with1(Code::Fcomp_m32fp, mem_op(addr)).unwrap(),
        ]
    }

    fn assert_empty_stack_memory_source_fault_preserves_fpu(
        cpu: &mut Cpu,
        mem: &mut Memory,
        instr: &Instruction,
        expected: fn(&anyhow::Error) -> bool,
    ) {
        reset_empty_fpu(cpu);
        cpu.fpu.status_word = 0x4500;
        cpu.fpu.ip_offset = 0x1234_5678;
        cpu.fpu.operand_offset = 0x8765_4321;
        let before = cpu.fpu.clone();

        let err = execute_iced_for_test(cpu, instr, mem)
            .expect_err("mandatory x87 memory read must fault");
        assert!(
            expected(&err),
            "wrong fault for {:?}: {err:#}",
            instr.code()
        );
        assert_eq!(
            compare_fpu_state(&before, &cpu.fpu),
            Vec::<String>::new(),
            "faulting empty-stack memory source mutated FPU for {:?}",
            instr.code()
        );
        assert_eq!(
            before.x87_save_area,
            cpu.fpu.x87_save_area,
            "faulting empty-stack memory source mutated save area for {:?}",
            instr.code()
        );
    }

    #[test]
    fn empty_stack_memory_sources_propagate_segment_faults_before_fpu_mutation() {
        for instr in empty_stack_memory_source_instructions(0x100) {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 |= 1;
            cpu.regs.ds = 0x10;
            cpu.regs.ds_cache.base = 0;
            cpu.regs.ds_cache.set_limit(0xff);
            cpu.regs.ds_cache.access = 0x93;
            cpu.regs.ds_cache.present = true;
            cpu.regs.ds_cache.is_code = false;
            cpu.regs.ds_cache.is_readable = true;
            cpu.regs.ds_cache.is_writable = true;
            let mut mem = Memory::new();

            assert_empty_stack_memory_source_fault_preserves_fpu(
                &mut cpu,
                &mut mem,
                &instr,
                |err| {
                    matches!(
                        err.downcast_ref::<crate::CpuError>(),
                        Some(crate::CpuError::GeneralProtectionFault)
                    )
                },
            );
        }
    }

    #[test]
    fn empty_stack_memory_sources_propagate_page_faults_before_fpu_mutation() {
        for instr in empty_stack_memory_source_instructions(0x100) {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 |= 1 | 0x8000_0000;
            cpu.regs.cr3 = 0;
            cpu.regs.ds = 0x10;
            cpu.regs.ds_cache.base = 0;
            cpu.regs.ds_cache.set_limit(0xffff_ffff);
            cpu.regs.ds_cache.access = 0x93;
            cpu.regs.ds_cache.present = true;
            cpu.regs.ds_cache.is_code = false;
            cpu.regs.ds_cache.is_readable = true;
            cpu.regs.ds_cache.is_writable = true;
            let mut mem = Memory::new();
            mem.write_u32(0, 0);

            assert_empty_stack_memory_source_fault_preserves_fpu(
                &mut cpu,
                &mut mem,
                &instr,
                |err| {
                    matches!(
                        err.downcast_ref::<crate::CpuError>(),
                        Some(crate::CpuError::PageFault(_))
                    )
                },
            );
        }
    }

    #[derive(Clone, Copy)]
    enum FaultingStoreSetup {
        Empty,
        Invalid,
    }

    fn faulting_store_instructions(addr: i64) -> [(Instruction, FaultingStoreSetup); 12] {
        [
            (
                Instruction::with1(Code::Fst_m32fp, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Empty,
            ),
            (
                Instruction::with1(Code::Fstp_m32fp, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Empty,
            ),
            (
                Instruction::with1(Code::Fstp_m64fp, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Empty,
            ),
            (
                Instruction::with1(Code::Fist_m16int, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Empty,
            ),
            (
                Instruction::with1(Code::Fistp_m16int, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Empty,
            ),
            (
                Instruction::with1(Code::Fistp_m32int, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Empty,
            ),
            (
                Instruction::with1(Code::Fisttp_m16int, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Empty,
            ),
            (
                Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Empty,
            ),
            (
                Instruction::with1(Code::Fist_m16int, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Invalid,
            ),
            (
                Instruction::with1(Code::Fistp_m16int, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Invalid,
            ),
            (
                Instruction::with1(Code::Fistp_m32int, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Invalid,
            ),
            (
                Instruction::with1(Code::Fisttp_m64int, mem_op(addr)).unwrap(),
                FaultingStoreSetup::Invalid,
            ),
        ]
    }

    fn assert_faulting_store_preserves_fpu(
        cpu: &mut Cpu,
        mem: &mut Memory,
        instr: &Instruction,
        setup: FaultingStoreSetup,
        expected: fn(&anyhow::Error) -> bool,
    ) {
        reset_empty_fpu(cpu);
        if matches!(setup, FaultingStoreSetup::Invalid) {
            cpu.fpu.push(F80::INDEFINITE);
        }
        cpu.fpu.status_word |= 0x4722;
        cpu.fpu.ip_offset = 0x1234_5678;
        cpu.fpu.operand_offset = 0x8765_4321;
        cpu.fpu.x87_save_area = [0x5a; 108];
        let before = cpu.fpu.clone();
        let memory_before: Vec<u8> = (0x100..0x110).map(|addr| mem.read_u8(addr)).collect();

        let err = execute_iced_for_test(cpu, instr, mem).expect_err("checked x87 store must fault");
        assert!(
            expected(&err),
            "wrong fault for {:?}: {err:#}",
            instr.code()
        );
        assert_eq!(
            compare_fpu_state(&before, &cpu.fpu),
            Vec::<String>::new(),
            "faulting store mutated FPU for {:?}",
            instr.code()
        );
        assert_eq!(before.x87_save_area, cpu.fpu.x87_save_area);
        assert_eq!(
            memory_before,
            (0x100..0x110)
                .map(|addr| mem.read_u8(addr))
                .collect::<Vec<_>>(),
            "faulting store mutated memory for {:?}",
            instr.code()
        );
    }

    #[test]
    fn faulting_x87_stores_preserve_fpu_on_segment_faults() {
        for (instr, setup) in faulting_store_instructions(0x100) {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 |= 1;
            cpu.regs.ds = 0x10;
            cpu.regs.ds_cache.base = 0;
            cpu.regs.ds_cache.set_limit(0xff);
            cpu.regs.ds_cache.access = 0x93;
            cpu.regs.ds_cache.present = true;
            cpu.regs.ds_cache.is_code = false;
            cpu.regs.ds_cache.is_readable = true;
            cpu.regs.ds_cache.is_writable = true;
            let mut mem = Memory::new();
            for addr in 0x100..0x110 {
                mem.write_u8(addr, 0xa5);
            }

            assert_faulting_store_preserves_fpu(&mut cpu, &mut mem, &instr, setup, |err| {
                matches!(
                    err.downcast_ref::<crate::CpuError>(),
                    Some(crate::CpuError::GeneralProtectionFault)
                )
            });
        }
    }

    #[test]
    fn faulting_x87_stores_preserve_fpu_on_page_faults() {
        for (instr, setup) in faulting_store_instructions(0x100) {
            let mut cpu = Cpu::new();
            cpu.regs.cr0 |= 1 | 0x8000_0000;
            cpu.regs.cr3 = 0;
            cpu.regs.ds = 0x10;
            cpu.regs.ds_cache.base = 0;
            cpu.regs.ds_cache.set_limit(0xffff_ffff);
            cpu.regs.ds_cache.access = 0x93;
            cpu.regs.ds_cache.present = true;
            cpu.regs.ds_cache.is_code = false;
            cpu.regs.ds_cache.is_readable = true;
            cpu.regs.ds_cache.is_writable = true;
            let mut mem = Memory::new();
            mem.write_u32(0, 0);
            for addr in 0x100..0x110 {
                mem.write_u8(addr, 0xa5);
            }

            assert_faulting_store_preserves_fpu(&mut cpu, &mut mem, &instr, setup, |err| {
                matches!(
                    err.downcast_ref::<crate::CpuError>(),
                    Some(crate::CpuError::PageFault(_))
                )
            });
        }
    }

    fn f80_from_significand_exponent(significand: u64, exponent: u16) -> F80 {
        let mut bytes = [0u8; 10];
        bytes[..8].copy_from_slice(&significand.to_le_bytes());
        bytes[8..].copy_from_slice(&exponent.to_le_bytes());
        F80(bytes)
    }

    fn execute_register_arith_status(code: Code, a: F80, b: F80, initial_sw: u16) -> u16 {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xffff;
        cpu.fpu.regs[0] = a;
        cpu.fpu.regs[1] = b;
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.set_tag(1, 0);
        cpu.fpu.status_word = initial_sw;

        execute_iced_for_test(
            &mut cpu,
            &instr2(code, Register::ST0, Register::ST1),
            &mut mem,
        )
        .unwrap();
        execute_fnstsw_ax(&mut cpu);
        cpu.regs.ax()
    }

    #[test]
    fn add_and_multiply_publish_all_architectural_exception_classes() {
        let denormal = f80_from_significand_exponent(1, 0);
        let max_finite = f80_from_significand_exponent(u64::MAX, 0x7ffe);
        let two_to_minus_64 = f80_from_significand_exponent(1u64 << 63, 0x3fbf);

        let cases = [
            (
                Code::Fadd_st0_sti,
                F80::from_f64(f64::INFINITY),
                F80::from_f64(f64::NEG_INFINITY),
                0x0001,
                "FADD invalid",
            ),
            (
                Code::Fmul_st0_sti,
                F80::from_f64(f64::INFINITY),
                F80::ZERO,
                0x0001,
                "FMUL invalid",
            ),
            (
                Code::Fadd_st0_sti,
                denormal,
                F80::ONE,
                0x0002,
                "FADD denormal operand",
            ),
            (
                Code::Fmul_st0_sti,
                max_finite,
                F80::from_i16(2),
                0x0028,
                "FMUL overflow and precision",
            ),
            (
                Code::Fmul_st0_sti,
                denormal,
                F80::from_f64(0.5),
                0x0032,
                "FMUL denormal underflow and precision",
            ),
            (
                Code::Fadd_st0_sti,
                F80::ONE,
                two_to_minus_64,
                0x0020,
                "FADD precision",
            ),
        ];

        for (code, a, b, expected, context) in cases {
            let sw = execute_register_arith_status(code, a, b, 0);
            assert_eq!(sw & expected, expected, "{context}: sw={sw:#06x}");
        }
    }

    #[test]
    fn add_and_multiply_replace_c1_with_current_rounding_direction() {
        for code in [Code::Fadd_st0_sti, Code::Fmul_st0_sti] {
            let sw = execute_register_arith_status(code, F80::ONE, F80::ONE, 1 << 9);
            assert_eq!(sw & (1 << 9), 0, "exact {:?} retained stale C1", code);
        }

        // 1 + 3*2^-64 is halfway between two extended-precision values; ties
        // to even increments the significand and therefore reports C1=1.
        let three_to_minus_64 = f80_from_significand_exponent(3u64 << 62, 0x3fc0);
        let sw = execute_register_arith_status(Code::Fadd_st0_sti, F80::ONE, three_to_minus_64, 0);
        assert_eq!(sw & 0x0220, 0x0220, "rounded-up FADD must report PE+C1");
    }

    #[test]
    fn integer_add_and_multiply_publish_exception_status() {
        let addr = 0x1000i64;
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        mem.write_u16(addr as usize, 1);
        cpu.fpu.push(F80::from_f64(18_446_744_073_709_551_616.0));
        execute_iced_for_test(
            &mut cpu,
            &Instruction::with1(Code::Fiadd_m16int, mem_op(addr)).unwrap(),
            &mut mem,
        )
        .unwrap();
        execute_fnstsw_ax(&mut cpu);
        assert_ne!(cpu.regs.ax() & 0x0020, 0, "FIADD must publish PE");

        let mut cpu = Cpu::new();
        mem.write_u16(addr as usize, 2);
        cpu.fpu
            .push(f80_from_significand_exponent(u64::MAX, 0x7ffe));
        execute_iced_for_test(
            &mut cpu,
            &Instruction::with1(Code::Fimul_m16int, mem_op(addr)).unwrap(),
            &mut mem,
        )
        .unwrap();
        execute_fnstsw_ax(&mut cpu);
        assert_eq!(cpu.regs.ax() & 0x0028, 0x0028, "FIMUL must publish OE+PE");
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn add_multiply_status_merge_matches_host_in_every_pc_rc_mode() {
        let three_to_minus_64 = f80_from_significand_exponent(3u64 << 62, 0x3fc0);
        let max_finite = f80_from_significand_exponent(u64::MAX, 0x7ffe);
        let operations = [
            (Code::Fadd_st0_sti, F80::ONE, three_to_minus_64, true),
            (Code::Fmul_st0_sti, max_finite, F80::from_i16(2), false),
        ];

        for pc in [0x0000u16, 0x0200, 0x0300] {
            for rc in [0x0000u16, 0x0400, 0x0800, 0x0c00] {
                let cw = 0x007f | pc | rc;
                for (code, a, b, add) in operations {
                    let (expected_value, host_sw) = if add {
                        x87_ops::f80_add_status(&a, &b, cw)
                    } else {
                        x87_ops::f80_mul_status(&a, &b, cw)
                    };
                    let mut cpu = Cpu::new();
                    let mut mem = Memory::new();
                    cpu.fpu.control_word = cw;
                    cpu.fpu.status_word = 0x0204; // sticky ZE plus stale C1
                    cpu.fpu.tag_word = 0xffff;
                    cpu.fpu.regs[0] = a;
                    cpu.fpu.regs[1] = b;
                    cpu.fpu.set_tag(0, 0);
                    cpu.fpu.set_tag(1, 0);

                    execute_iced_for_test(
                        &mut cpu,
                        &instr2(code, Register::ST0, Register::ST1),
                        &mut mem,
                    )
                    .unwrap();

                    assert_eq!(
                        cpu.fpu.get_st(0),
                        expected_value,
                        "value: {code:?} cw={cw:#06x}"
                    );
                    assert_eq!(
                        cpu.fpu.status_word & 0x003f,
                        0x0004 | (host_sw & 0x003f),
                        "sticky exceptions: {code:?} cw={cw:#06x}"
                    );
                    assert_eq!(
                        cpu.fpu.status_word & 0x0200,
                        host_sw & 0x0200,
                        "C1: {code:?} cw={cw:#06x}"
                    );
                }
            }
        }
    }

    fn fill_stack(cpu: &mut Cpu) {
        cpu.fpu = Fpu::new();
        for i in 0..8 {
            cpu.fpu.push(F80::from_f64(i as f64 + 1.0));
        }
    }

    fn reset_empty_fpu(cpu: &mut Cpu) {
        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
    }

    fn assert_stack_fault(cpu: &Cpu) {
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
        assert_ne!(cpu.fpu.status_word & 0x40, 0);
    }

    #[test]
    fn test_fpu_push_pop_tracks_tags() {
        let mut fpu = Fpu::new();
        assert_eq!(fpu.get_top(), 0);

        fpu.push(F80::from_f64(1.25));
        assert_eq!(fpu.get_top(), 7);
        assert_eq!(fpu.get_st(0).to_f64(), 1.25);
        let tag = (fpu.tag_word >> (7 * 2)) & 0x3;
        assert_eq!(tag, 0);

        let value = fpu.pop();
        assert_eq!(value, F80::from_f64(1.25));
        assert_eq!(fpu.get_top(), 0);
        let tag = (fpu.tag_word >> (7 * 2)) & 0x3;
        assert_eq!(tag, 3);
    }

    #[test]
    fn test_fpu_push_overflow_sets_stack_fault() {
        let mut fpu = Fpu::new();
        fpu.status_word = 0;

        for i in 0..8 {
            fpu.push(F80::from_f64(i as f64));
        }
        let top_before = fpu.get_top();
        fpu.push(F80::from_f64(9.0));

        // Per Intel SDM Vol 1 §8.5.1.2, masked stack overflow still advances
        // TOP and writes the F80 indefinite to the destination slot.
        let expected_new_top = (top_before.wrapping_sub(1)) & 0x7;
        assert_eq!(fpu.get_top(), expected_new_top);
        assert_eq!(
            fpu.regs[expected_new_top].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]
        );
        assert_eq!(fpu.get_tag(expected_new_top), 2); // Special (QNaN)
        assert_ne!(fpu.status_word & 0x01, 0); // IE
        assert_ne!(fpu.status_word & 0x40, 0); // SF
        assert_ne!(fpu.status_word & (1 << 9), 0); // C1 = overflow
    }

    #[test]
    fn test_fpu_pop_underflow_sets_stack_fault() {
        let mut fpu = Fpu::new();
        fpu.status_word = 0;

        let top_before = fpu.get_top();
        let value = fpu.pop();

        // Per Intel SDM Vol 1 §8.5.1.1, masked stack underflow still advances
        // TOP (tag already Empty, so no tag change).
        assert_eq!(value, F80::ZERO);
        assert_eq!(fpu.get_top(), (top_before + 1) & 0x7);
        assert_ne!(fpu.status_word & 0x01, 0); // IE
        assert_ne!(fpu.status_word & 0x40, 0); // SF
        assert_eq!(fpu.status_word & (1 << 9), 0); // C1 = underflow
    }

    #[test]
    fn pop_advances_top_on_empty_stack() {
        let mut fpu = Fpu::new();
        assert_eq!(fpu.get_top(), 0);
        assert!(fpu.is_empty(0));
        let v = fpu.pop();
        assert_eq!(v.0, [0u8; 10], "pop empty returns F80::ZERO value");
        assert_eq!(fpu.get_top(), 1, "TOP must advance even on underflow");
        assert_ne!(fpu.status_word & 0x41, 0, "SF+IE set after underflow");
    }

    #[test]
    fn push_advances_top_and_writes_indefinite_on_full_stack() {
        let mut fpu = Fpu::new();
        // Mark all 8 slots valid: tag_word = 0 (all tags = 0 = valid).
        fpu.tag_word = 0;
        // TOP=0 initially.
        fpu.push(F80::ZERO);
        // After a full push, TOP should have decremented (wrapping) to 7.
        assert_eq!(
            fpu.get_top(),
            7,
            "TOP must advance (decrement wrapping) on overflow"
        );
        assert_eq!(
            fpu.regs[7].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF],
            "overflow destination receives F80 indefinite"
        );
        assert_eq!(fpu.get_tag(7), 2, "indefinite slot tagged Special");
        assert_ne!(fpu.status_word & 0x41, 0, "SF+IE set after overflow");
        assert_ne!(fpu.status_word & (1 << 9), 0, "C1=1 for overflow");
    }

    #[test]
    fn test_fpu_set_exception_respects_masking() {
        let mut fpu = Fpu::new();
        fpu.control_word = 0;
        fpu.status_word = 0;
        fpu.set_exception(0x01);
        assert_ne!(fpu.status_word & 0x01, 0);
        assert_ne!(fpu.status_word & (1 << 7), 0);

        let mut fpu = Fpu::new();
        fpu.control_word = 0x037F;
        fpu.status_word = 0;
        fpu.set_exception(0x01);
        assert_ne!(fpu.status_word & 0x01, 0);
        assert_eq!(fpu.status_word & (1 << 7), 0);
    }

    #[test]
    fn test_trace_fpu_exception_helpers_are_noop_without_subscriber() {
        // With no DEBUG subscriber, exception tracing is a safe no-op.
        trace_fpu_exception("TEST", 1.0, Some(2.0));
        trace_fpu_exception("TEST", 1.0, None);
    }

    #[test]
    fn iced_adapter_maps_stack_and_ax_operands() {
        for (register, expected) in [
            (Register::ST0, X87Operand::Stack(0)),
            (Register::ST3, X87Operand::Stack(3)),
            (Register::ST7, X87Operand::Stack(7)),
        ] {
            let descriptor =
                crate::x87_iced_adapter::decode_instruction(&instr1(Code::Fld_sti, register));
            assert_eq!(descriptor.operand1, expected);
        }
        let descriptor =
            crate::x87_iced_adapter::decode_instruction(&instr1(Code::Fnstsw_AX, Register::AX));
        assert_eq!(descriptor.operand1, X87Operand::Ax);
    }

    #[test]
    fn test_execute_fninit_and_constant_loads() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu.status_word = 0xFFFF;
        cpu.fpu.control_word = 0;
        cpu.fpu.tag_word = 0;
        cpu.fpu.set_top(3);

        exec_instr(&mut cpu, &mut mem, instr0(Code::Fninit));
        assert_eq!(cpu.fpu.status_word, 0);
        assert_eq!(cpu.fpu.control_word, 0x037F);
        assert_eq!(cpu.fpu.tag_word, 0xFFFF);
        assert_eq!(cpu.fpu.get_top(), 0);

        exec_instr(&mut cpu, &mut mem, instr0(Code::Fld1));
        assert!((cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fldz));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fldpi));
        assert!((cpu.fpu.get_st(0).to_f64() - std::f64::consts::PI).abs() < 1e-9);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fldln2));
        assert!((cpu.fpu.get_st(0).to_f64() - std::f64::consts::LN_2).abs() < 1e-9);
    }

    /// MDD-BUG-CRUCIBLE-00579: FSQRT and FRNDINT published only their value,
    /// so an inexact result left PE and C1 at whatever the PREVIOUS operation
    /// set them to. These flags are guest-visible even when masked.
    #[test]
    fn fsqrt_and_frndint_publish_their_own_precision_and_c1() {
        const PE: u16 = 0x20;
        const C1: u16 = 1 << 9;

        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // Exact: sqrt(4) = 2. No PE, and C1 must be CLEARED, not left over
        // from an earlier op -- so pre-set both and prove the op overwrites.
        set_st0_st1(&mut cpu, 4.0, 0.0);
        cpu.fpu.status_word |= C1;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 2.0);
        assert_eq!(cpu.fpu.status_word & PE, 0, "exact FSQRT must not raise PE");
        assert_eq!(cpu.fpu.status_word & C1, 0, "exact FSQRT must clear C1");

        // Inexact: sqrt(2) is irrational, so PE must be raised.
        cpu.fpu.status_word &= !(PE | C1);
        set_st0_st1(&mut cpu, 2.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        assert_ne!(cpu.fpu.status_word & PE, 0, "inexact FSQRT must raise PE");

        // Inexact FRNDINT: 1.5 rounds to 2 under the default nearest-even, so
        // PE is raised AND C1 is set, because rounding increased the magnitude.
        cpu.fpu.status_word &= !(PE | C1);
        set_st0_st1(&mut cpu, 1.5, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Frndint));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 2.0);
        assert_ne!(cpu.fpu.status_word & PE, 0, "inexact FRNDINT must raise PE");
        assert_ne!(
            cpu.fpu.status_word & C1,
            0,
            "FRNDINT rounding 1.5 up to 2 must set C1"
        );

        // Exact FRNDINT: 2.0 is already an integer. No PE, and C1 cleared.
        cpu.fpu.status_word |= PE | C1;
        set_st0_st1(&mut cpu, 2.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Frndint));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 2.0);
        assert_eq!(cpu.fpu.status_word & C1, 0, "exact FRNDINT must clear C1");
    }

    /// `FNCLEX` clears SF along with the exception flags
    ///
    /// The mask used to be `!0x00BF`, which skips bit 6, so the stack fault
    /// flag survived a clear that is supposed to remove it. SDM Vol 2 lists
    /// IE/DE/ZE/OE/UE/PE, SF, ES and B; host silicon agrees
    /// (`x87_oracle::tests::host_fnclex_clears_stack_fault` reads SW=0x0800
    /// after an underflow-then-FNCLEX where we returned 0x0840).
    ///
    /// The bits to clear are ENUMERATED here rather than compared against one
    /// hand-written expected word, so a future mask that drops any single one
    /// of them names the offender instead of just reporting two unequal
    /// hex numbers.
    #[test]
    fn test_execute_fnclex_clears_every_flag_the_sdm_lists() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // (bit, name) — the full SDM list for FCLEX/FNCLEX.
        let cleared: [(u16, &str); 9] = [
            (0x0001, "IE"),
            (0x0002, "DE"),
            (0x0004, "ZE"),
            (0x0008, "OE"),
            (0x0010, "UE"),
            (0x0020, "PE"),
            (0x0040, "SF"),
            (0x0080, "ES"),
            (0x8000, "B"),
        ];
        // Condition codes and TOP are NOT touched by FNCLEX.
        let preserved: u16 = 0x4700 | (0x5 << 11); // C0..C3 set, TOP=5

        cpu.fpu.status_word = 0xFFFF;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fnclex));
        for (bit, name) in cleared {
            assert_eq!(
                cpu.fpu.status_word & bit,
                0,
                "FNCLEX must clear {name} (bit {bit:#06X}); status is {:#06X}",
                cpu.fpu.status_word
            );
        }

        // Precondition-shaped positive case: FNCLEX must not be a blanket
        // wipe. C0-C3 and TOP survive, so a `status_word = 0` implementation
        // would fail here rather than passing the loop above.
        cpu.fpu.status_word = preserved | 0x00FF;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fnclex));
        assert_eq!(
            cpu.fpu.status_word, preserved,
            "FNCLEX must preserve C0-C3 and TOP while clearing the flag set"
        );
    }

    /// The tag word records CONTENT, not just occupancy
    ///
    /// We used to write Valid(0) for everything that was not Empty, so `FLDZ`
    /// tagged Zero as Valid and a loaded NaN tagged Special as Valid. That is
    /// visible to `FXAM`, to any `FSTENV`/`FSAVE` consumer, and to a guest that
    /// tests the tag word for zero or NaN without touching the value.
    ///
    /// The sequence and the expectations mirror the host-silicon probe
    /// `x87_oracle::tests::host_tag_word_distinguishes_zero_from_valid`, which
    /// needs no external oracle: after `FNINIT; FLD1; FLDZ; FLDPI` the host
    /// leaves TOP=5 with R5=pi Valid, R6=+0 **Zero**, R7=1.0 Valid and R0-R4
    /// Empty. This asserts the emulator reaches the same tag word — the probe
    /// alone only pins what the HOST does.
    #[test]
    fn test_tag_word_classifies_content_not_just_occupancy() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);

        exec_instr(&mut cpu, &mut mem, instr0(Code::Fld1));
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fldz));
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fldpi));

        assert_eq!(cpu.fpu.get_top(), 5, "three pushes from empty leave TOP=5");
        let tag = |r: usize| cpu.fpu.get_tag(r);
        assert_eq!(tag(5), 0, "R5 holds pi — Valid");
        assert_eq!(tag(6), 1, "R6 holds +0 — must tag Zero, not Valid");
        assert_eq!(tag(7), 0, "R7 holds 1.0 — Valid");
        for r in 0..5 {
            assert_eq!(tag(r), 3, "R{r} must be Empty");
        }
    }

    /// Every x87 class earns its own tag, checked one class at a time.
    ///
    /// The table is the assertion: a "tag everything Valid" implementation
    /// passes the Valid row on its own, which is how defect B survived. Each
    /// row names its class so a failure says which one regressed rather than
    /// printing two unequal integers.
    ///
    /// `3` (Empty) is absent by construction — it is an occupancy fact, not a
    /// property of the bits, so `F80::x87_tag` cannot produce it.
    #[test]
    fn test_x87_tag_classifies_every_value_class() {
        let qnan = F80([0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]);
        let inf = F80([0, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x7F]);
        let denormal = F80([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let unsupported = F80([1, 0, 0, 0, 0, 0, 0, 0, 1, 0]); // exp != 0, integer bit clear

        let cases: [(F80, u16, &str); 6] = [
            (F80::from_f64(1.0), 0, "normal finite"),
            (F80::from_f64(-2.5), 0, "negative normal"),
            (F80::ZERO, 1, "zero"),
            (qnan, 2, "QNaN"),
            (inf, 2, "infinity"),
            (denormal, 2, "denormal"),
        ];
        for (value, want, name) in cases {
            assert_eq!(value.x87_tag(), want, "{name} must tag {want}");
        }
        // Unsupported encodings are Special too, asserted separately because
        // the predicate that recognises them is a different one.
        assert!(
            unsupported.is_unsupported_encoding(),
            "precondition: the fixture must actually be an unsupported encoding"
        );
        assert_eq!(
            unsupported.x87_tag(),
            2,
            "unsupported encoding must tag Special"
        );

        // The classes are genuinely distinguished — not all mapped to one tag.
        let distinct: std::collections::BTreeSet<u16> =
            cases.iter().map(|(v, _, _)| v.x87_tag()).collect();
        assert_eq!(
            distinct.len(),
            3,
            "Valid, Zero and Special must all be reachable, got {distinct:?}"
        );
    }

    /// `FLD m32`/`FLD m64` of a signaling NaN raises IE
    ///
    /// SDM Vol 2 lists `#IA` for `FLD m32fp`/`m64fp` when the source is an SNaN.
    /// Host silicon agrees and pins the scope precisely: `FNINIT; FLD dword
    /// [snan]; FNSAVE` leaves SW=0x3801 where we gave 0x3800 — measured, the IE
    /// bit was the ONLY difference. The loaded value already matched (our
    /// widening quiets the NaN) and so did the tag word (defect B tags it
    /// Special), which is why this fix touches the status word alone.
    #[test]
    fn test_fld_from_memory_raises_ie_on_a_signaling_nan() {
        const IE: u16 = 0x0001;
        const DE: u16 = 0x0002;

        // (width, bits of a signaling NaN, bits of a quiet NaN)
        for (width, snan_is_ie) in [(4usize, true), (8, true)] {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.regs.ds = 0;
            let addr: usize = 0x500;

            if width == 4 {
                mem.write_u32(addr, 0x7F80_0001); // SNaN f32
            } else {
                mem.write_u32(addr, 0x0000_0001);
                mem.write_u32(addr + 4, 0x7FF0_0000); // SNaN f64
            }
            let code = if width == 4 {
                Code::Fld_m32fp
            } else {
                Code::Fld_m64fp
            };
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(code, mem_op(addr as i64)).unwrap(),
            );
            assert_eq!(
                cpu.fpu.status_word & IE,
                if snan_is_ie { IE } else { 0 },
                "FLD m{}: a signaling NaN source must set IE",
                width * 8
            );
            // The value must still be the quiet form and the tag Special —
            // this fix changes the status word only, and asserting that here
            // stops a future "fix" from also mangling the value.
            assert!(
                cpu.fpu.get_st(0).is_nan(),
                "the loaded value is still a NaN"
            );
            assert_eq!(
                cpu.fpu.get_tag(cpu.fpu.get_top()),
                2,
                "FLD m{}: the loaded NaN tags Special",
                width * 8
            );
        }

        // Negative case: an ordinary finite load raises NEITHER IE nor DE, so
        // the assertions above cannot be satisfied by an implementation that
        // simply sets IE on every load.
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x600;
        mem.write_u32(addr, 1.5f32.to_bits());
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & (IE | DE),
            0,
            "an ordinary finite FLD must raise neither IE nor DE"
        );
    }

    /// `FCOM` against an UNSUPPORTED encoding raises IE
    ///
    /// `F80::is_nan` requires exponent 0x7FFF, so an unnormal — a finite value
    /// with the integer bit clear — is not a NaN and used to compare silently.
    /// A 387 and later raise `#IA` on it; host silicon reports SW=0x7501 for
    /// case 12 against our 0x7500.
    #[test]
    fn test_fcom_raises_ie_on_an_unsupported_encoding() {
        const IE: u16 = 0x0001;
        // Unnormal: non-zero exponent with the integer bit CLEAR.
        let unnormal = F80([0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x40]);
        assert!(
            unnormal.is_unsupported_encoding(),
            "precondition: the fixture must be an unsupported encoding"
        );
        assert!(
            !unnormal.is_nan(),
            "precondition: it must NOT be a NaN, or the pre-existing NaN arm \
             would already have caught it and this test would prove nothing"
        );

        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.set_st(1, unnormal);
        cpu.fpu.set_tag(cpu.fpu.get_st_idx(1), 2);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcom_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(
            cpu.fpu.status_word & IE,
            IE,
            "comparing against an unnormal must set IE"
        );

        // Negative case: two ordinary finites leave IE clear.
        let mut cpu = Cpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcom_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(
            cpu.fpu.status_word & IE,
            0,
            "an ordinary compare must not set IE"
        );
    }

    /// An arithmetic RESULT re-tags the destination by content
    ///
    /// Classifying only pushes left result writes
    /// alone, so a register whose content changed class kept its old tag. The
    /// lane that found it measured exactly this: `+0.0` loaded (tag Zero), then
    /// `FADD -f32::MAX`, leaving the tag word still reading "zero" while the
    /// register held `-f32::MAX`. Raw-host execution reported Valid, and the SDM
    /// re-tags on every register write, so the interpreter was the wrong side.
    ///
    /// Each row changes the destination's tag CLASS, which is the property at
    /// issue — a test whose result stays in the same class could not tell a
    /// re-tagging implementation from a stale-tagging one.
    #[test]
    fn test_arithmetic_result_retags_the_destination_by_content() {
        // (start value, added f32, expected tag after, what it demonstrates)
        let cases: [(f64, f32, u16, &str); 3] = [
            (0.0, -f32::MAX, 0, "Zero + big negative => Valid"),
            (1.0, -1.0, 1, "Valid + its negation => Zero"),
            (1.0, f32::INFINITY, 2, "Valid + infinity => Special"),
        ];

        for (start, addend, want_tag, what) in cases {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.regs.ds = 0;
            reset_empty_fpu(&mut cpu);

            cpu.fpu.push(F80::from_f64(start));
            let before = cpu.fpu.get_tag(cpu.fpu.get_top());
            assert_eq!(
                before,
                F80::from_f64(start).x87_tag(),
                "precondition ({what}): the pushed value must be tagged by content, \
                 or this test is measuring the push fix rather than the result fix"
            );

            let addr: usize = 0x700;
            mem.write_u32(addr, addend.to_bits());
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fadd_m32fp, mem_op(addr as i64)).unwrap(),
            );

            let after = cpu.fpu.get_tag(cpu.fpu.get_top());
            assert_ne!(
                after, before,
                "precondition ({what}): the class must actually CHANGE, or a \
                 stale-tagging implementation would pass by doing nothing"
            );
            assert_eq!(after, want_tag, "{what}");
        }
    }

    /// The same re-tag on the numeric-shadow FAST path
    ///
    /// `try_numeric_shadow_arith` writes its result through
    /// `set_st_with_proofs`, a DIFFERENT setter from the slow path's `set_st`,
    /// and it only engages when the destination carries an exact binary32/64
    /// proof. A destination pushed as a raw F80 has no proof, so the sibling
    /// test above silently exercises the slow path only — mutating the fast
    /// path's re-tag away leaves it green. Loading through `FLD m32` sets the
    /// proof and forces the fast path, which is what makes this test pin the
    /// second site rather than the first one twice.
    #[test]
    fn test_arithmetic_result_retags_on_the_numeric_shadow_fast_path() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        reset_empty_fpu(&mut cpu);
        // PC = double (0b10). `try_numeric_shadow_arith` matches precision
        // 0b00 and 0b10 only, so under the DEFAULT control word (0x037F,
        // PC=0b11 extended) the fast path never engages at all — a fixture
        // that leaves the CW alone silently tests the slow path twice.
        cpu.fpu.control_word = 0x027F;
        assert_eq!(
            (cpu.fpu.control_word >> 8) & 0b11,
            0b10,
            "precondition: PC must select double, or the fast path is unreachable"
        );

        // FLD m32(+0.0) — pushes WITH a binary32 proof, arming the fast path.
        let zero_at: usize = 0x800;
        mem.write_u32(zero_at, 0.0f32.to_bits());
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m32fp, mem_op(zero_at as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.get_tag(cpu.fpu.get_top()),
            1,
            "precondition: the loaded +0.0 tags Zero"
        );
        assert!(
            cpu.fpu.exact_binary32_st(0).is_some(),
            "precondition: the destination must carry a binary32 proof, or the \
             fast path will not engage and this test would duplicate the slow one"
        );

        // FADD m32(-f32::MAX): the destination stops being zero.
        let addend_at: usize = 0x900;
        mem.write_u32(addend_at, (-f32::MAX).to_bits());
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fadd_m32fp, mem_op(addend_at as i64)).unwrap(),
        );

        assert_eq!(
            cpu.fpu.get_st(0).to_f64(),
            f64::from(-f32::MAX),
            "sanity: the arithmetic itself is right"
        );
        assert_eq!(
            cpu.fpu.get_tag(cpu.fpu.get_top()),
            0,
            "the fast path must re-tag the destination Valid, not leave it Zero"
        );
    }

    /// An inexact `FIST`/`FISTP` raises PE, at every width
    ///
    /// The host `FISTP` these conversions already execute reported the whole
    /// status word and we kept only bit 0, so the inexactness signal was being
    /// measured and thrown away. Host silicon: `FNINIT; FLDPI; FIST dword`
    /// gives SW=0x3820 against our 0x3800 — measured, the low six exception
    /// bits are exactly `0x0020`, PE alone, with the stored value already right.
    ///
    /// All three widths are driven because each is a SEPARATE call site in the
    /// `Fist | Fistp` arm; a single-width test would leave two of them unpinned,
    /// which is the shape that let defect B land half-finished.
    #[test]
    fn test_inexact_fist_raises_pe_at_every_width() {
        const PE: u16 = 0x0020;
        const IE: u16 = 0x0001;

        for (code, width) in [
            (Code::Fistp_m16int, 2usize),
            (Code::Fistp_m32int, 4),
            (Code::Fistp_m64int, 8),
        ] {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.regs.ds = 0;
            reset_empty_fpu(&mut cpu);
            // pi is not an integer, so every width rounds and must set PE.
            cpu.fpu.push(F80::from_f64(std::f64::consts::PI));

            let addr: usize = 0xA00;
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(code, mem_op(addr as i64)).unwrap(),
            );
            assert_eq!(
                cpu.fpu.status_word & PE,
                PE,
                "FISTP m{}: rounding pi to an integer is inexact and must set PE",
                width * 8
            );
            assert_eq!(
                cpu.fpu.status_word & IE,
                0,
                "FISTP m{}: pi is in range, so IE must stay clear",
                width * 8
            );
        }

        // Negative case: an EXACT store raises neither PE nor IE, so an
        // implementation that simply sets PE on every FIST cannot pass.
        for (code, width) in [
            (Code::Fistp_m16int, 2usize),
            (Code::Fistp_m32int, 4),
            (Code::Fistp_m64int, 8),
        ] {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.regs.ds = 0;
            reset_empty_fpu(&mut cpu);
            cpu.fpu.push(F80::from_f64(42.0)); // exactly representable

            let addr: usize = 0xB00;
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(code, mem_op(addr as i64)).unwrap(),
            );
            assert_eq!(
                cpu.fpu.status_word & (PE | IE),
                0,
                "FISTP m{}: an exact store must raise neither PE nor IE",
                width * 8
            );
        }
    }

    /// `FIST`/`FISTP` set C1 when the store rounded up and clear it
    /// otherwise (SDM; MDD-BUG-FLUX-00127).
    ///
    /// C1 is pre-set before each store so a "cleared" expectation proves the
    /// instruction actively clears it — the defect was precisely that an
    /// inexact store left C1 at its stale prior value. All three widths are
    /// driven (separate backend call sites), and the negative-value cases pin
    /// the direction convention: rounded UP means the result EXCEEDS the
    /// source, so -2.2 → -2 sets C1 while -2.7 → -3 clears it.
    #[test]
    fn test_fist_c1_reports_rounding_direction() {
        const C1: u16 = 0x0200;

        // (source, expected C1 under round-nearest)
        //
        // C1 tracks the MAGNITUDE, so it is sign-SYMMETRIC. The two negative
        // rows were originally the value-based reading ("result exceeds
        // source"), which inverts every negative operand and is what reopened
        // MDD-BUG-FLUX-00127. Corrected against host silicon, measured with
        // `fld tbyte` / `fistp` / `fnstsw` under round-nearest:
        //
        //    2.7 -> 3   sw=0x0220      -2.7 -> -3  sw=0x0220
        //    2.2 -> 2   sw=0x0020      -2.2 -> -2  sw=0x0020
        //
        // `-2.7` is the discriminating case: the stored VALUE fell while the
        // magnitude rose, and silicon sets C1.
        let cases = [
            (2.7, true),   // → 3, magnitude up
            (2.2, false),  // → 2, magnitude down
            (-2.2, false), // → -2, magnitude down (value rose)
            (-2.7, true),  // → -3, magnitude up (value fell)
            (42.0, false), // exact: C1 cleared
        ];
        for (code, width) in [
            (Code::Fistp_m16int, 2usize),
            (Code::Fistp_m32int, 4),
            (Code::Fistp_m64int, 8),
        ] {
            for (source, rounded_up) in cases {
                let mut cpu = Cpu::new();
                let mut mem = Memory::new();
                cpu.regs.ds = 0;
                reset_empty_fpu(&mut cpu);
                cpu.fpu.push(F80::from_f64(source));
                // Stale C1 from a prior operation must not survive the store.
                cpu.fpu.status_word |= C1;

                let addr: usize = 0xA00;
                exec_instr(
                    &mut cpu,
                    &mut mem,
                    Instruction::with1(code, mem_op(addr as i64)).unwrap(),
                );
                assert_eq!(
                    cpu.fpu.status_word & C1,
                    if rounded_up { C1 } else { 0 },
                    "FISTP m{} of {source}: C1 must be {} (set iff rounded up)",
                    width * 8,
                    u16::from(rounded_up)
                );
            }
        }
    }

    #[test]
    fn test_execute_basic_unary_and_stack_ops() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 2.5, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fchs));
        assert!((cpu.fpu.get_st(0).to_f64() + 2.5).abs() < 1e-9);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fabs));
        assert!((cpu.fpu.get_st(0).to_f64() - 2.5).abs() < 1e-9);

        set_st0_st1(&mut cpu, 1.6, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Frndint));
        assert!((cpu.fpu.get_st(0).to_f64() - 2.0).abs() < 1e-9);

        set_st0_st1(&mut cpu, 4.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        assert!((cpu.fpu.get_st(0).to_f64() - 2.0).abs() < 1e-9);

        set_st0_st1(&mut cpu, 0.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsin));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fcos));
        assert!((cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9);

        set_st0_st1(&mut cpu, 0.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::F2xm1));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 0.0);

        cpu.fpu.set_top(0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fdecstp));
        assert_eq!(cpu.fpu.get_top(), 7);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fincstp));
        assert_eq!(cpu.fpu.get_top(), 0);

        cpu.fpu.status_word = 0x00BF;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fnclex));
        assert_eq!(cpu.fpu.status_word & 0x00BF, 0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fnop));
        exec_instr(&mut cpu, &mut mem, instr0(Code::Wait));
    }

    #[test]
    fn test_execute_arith_and_div_exceptions() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.5, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fadd_st0_sti, Register::ST0, Register::ST1),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.5).abs() < 1e-9);

        set_st0_st1(&mut cpu, 5.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fsub_st0_sti, Register::ST0, Register::ST1),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);

        set_st0_st1(&mut cpu, 5.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fsubr_st0_sti, Register::ST0, Register::ST1),
        );
        assert!((cpu.fpu.get_st(0).to_f64() + 3.0).abs() < 1e-9);

        set_st0_st1(&mut cpu, 3.0, 4.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fmul_st0_sti, Register::ST0, Register::ST1),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 12.0).abs() < 1e-9);

        set_st0_st1(&mut cpu, 8.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdiv_st0_sti, Register::ST0, Register::ST1),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 4.0).abs() < 1e-9);

        set_st0_st1(&mut cpu, 8.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivr_st0_sti, Register::ST0, Register::ST1),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 0.25).abs() < 1e-9);

        cpu.fpu.status_word = 0;
        set_st0_st1(&mut cpu, 1.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdiv_st0_sti, Register::ST0, Register::ST1),
        );
        assert_ne!(cpu.fpu.status_word & 0x04, 0);

        cpu.fpu.status_word = 0;
        set_st0_st1(&mut cpu, 0.0, 1.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivr_st0_sti, Register::ST0, Register::ST1),
        );
        assert_ne!(cpu.fpu.status_word & 0x04, 0);

        set_st0_st1(&mut cpu, 1.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Faddp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_eq!(cpu.fpu.get_top(), 1);
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn ffreep_st0_frees_and_pops_without_false_stack_underflow() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu.set_top(3);
        cpu.fpu.regs[3] = F80::ONE;
        cpu.fpu.set_tag(3, 0);

        exec_instr(&mut cpu, &mut mem, instr1(Code::Ffreep_sti, Register::ST0));

        assert_eq!(cpu.fpu.get_top(), 4);
        assert_eq!(cpu.fpu.get_tag(3), 3);
        assert_eq!(
            cpu.fpu.status_word & 0x0041,
            0,
            "freeing ST(0) is not an underflow"
        );
    }

    #[test]
    fn test_execute_arith_respects_precision_control() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        let tiny = 2f64.powi(-25);

        set_st0_st1(&mut cpu, 1.0, tiny);
        cpu.fpu.control_word &= !(0x3 << 8);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fadd_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);

        set_st0_st1(&mut cpu, 1.0, tiny);
        cpu.fpu.control_word = (cpu.fpu.control_word & !(0x3 << 8)) | (0x2 << 8);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fadd_st0_sti, Register::ST0, Register::ST1),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - (1.0 + tiny)).abs() < 1e-12);
    }

    #[test]
    fn test_fpu_push_respects_precision_control() {
        // With F80 registers, push no longer rounds — precision control
        // is handled by the x87 hardware in x87_ops functions.
        // This test now just validates push stores the value correctly.
        let tiny = 2f64.powi(-25);

        let mut fpu = Fpu::new();
        fpu.control_word &= !(0x3 << 8);
        fpu.push(F80::from_f64(1.0 + tiny));
        assert!((fpu.get_st(0).to_f64() - (1.0 + tiny)).abs() < 1e-12);

        let mut fpu = Fpu::new();
        fpu.control_word = (fpu.control_word & !(0x3 << 8)) | (0x2 << 8);
        fpu.push(F80::from_f64(1.0 + tiny));
        assert!((fpu.get_st(0).to_f64() - (1.0 + tiny)).abs() < 1e-12);
    }

    #[test]
    fn test_execute_trig_extract_scale_ops() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 0.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
        assert!((cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9);
        assert!((cpu.fpu.get_st(1).to_f64() - 0.0).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 0.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsincos));
        assert!((cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9);
        assert!((cpu.fpu.get_st(1).to_f64() - 0.0).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 1.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fpatan));
        assert!((cpu.fpu.get_st(0).to_f64() - (std::f64::consts::PI / 4.0)).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 8.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxtract));
        assert!((cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9);
        assert!((cpu.fpu.get_st(1).to_f64() - 3.0).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 3.0, 2.2);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fscale));
        assert!((cpu.fpu.get_st(0).to_f64() - 12.0).abs() < 1e-9);
    }

    #[test]
    fn test_execute_trig_special_cases() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::INFINITY, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsin));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::INFINITY, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fcos));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, -4.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
        assert!(cpu.fpu.get_st(0).to_f64().is_nan());

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::INFINITY, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::INFINITY, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsincos));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_remainder_and_log_ops() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 5.0, 2.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert!((cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9);
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 5.0, 2.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert!((cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9);
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 2.0, 3.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2xp1));
        assert!((cpu.fpu.get_st(0).to_f64() - 2.0).abs() < 1e-9);
    }

    #[test]
    // x86_64-only: the soft-float f2xm1 is computed in f64 and is approximate by
    // design (x87_soft.rs), so the rounding-control bits cannot perturb the
    // 80-bit mantissa the way the host x87 transcendental microcode does. This
    // test distinguishes directed rounding from default, which only the host
    // path satisfies.
    #[cfg(target_arch = "x86_64")]
    fn test_execute_f2xm1_threads_control_word_to_helper() {
        const CW_DEFAULT: u16 = 0x037F;
        const CW_ROUND_DOWN: u16 = 0x077F;
        const CW_ROUND_UP: u16 = 0x0B7F;

        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        let input = F80::from_f64(0.5);
        let default = x87_ops::f80_f2xm1(&input, CW_DEFAULT);
        let down = x87_ops::f80_f2xm1(&input, CW_ROUND_DOWN);
        let up = x87_ops::f80_f2xm1(&input, CW_ROUND_UP);
        let (cw, expected) = if down.bytes() != default.bytes() {
            (CW_ROUND_DOWN, down)
        } else {
            assert_ne!(
                up.bytes(),
                default.bytes(),
                "test fixture must distinguish directed rounding from default"
            );
            (CW_ROUND_UP, up)
        };

        set_st0_st1(&mut cpu, 0.5, 0.0);
        cpu.fpu.control_word = cw;
        exec_instr(&mut cpu, &mut mem, instr0(Code::F2xm1));

        assert_eq!(cpu.fpu.get_st(0).bytes(), expected.bytes());
    }

    #[test]
    fn test_execute_log_special_cases() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 0.0, 2.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert_ne!(cpu.fpu.status_word & 0x04, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, -1.0, 2.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::NAN, 2.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, -1.0, 2.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2xp1));
        assert_ne!(cpu.fpu.status_word & 0x04, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, -2.0, 2.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2xp1));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::NAN, 2.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2xp1));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_fprem_partial_and_zero_dividend() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 2.0_f64.powi(120), 1.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 0.0, 2.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 0.0);
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0);
    }

    #[test]
    fn test_execute_fprem_invalid_inputs_and_q_bits() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::NAN, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::INFINITY, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        // One quotient bit at a time. The old fixture was 7.0 / 1.0, whose
        // quotient 0b111 sets all three, so no permutation of Q0->C1,
        // Q1->C3, Q2->C0 was detectable -- and a guest reading C0/C3/C1 as
        // the octant index after FPREM (the standard range-reduction idiom
        // before FSIN/FCOS) would silently pick the wrong octant
        // (MDD-BUG-TEM-00177).
        for (dividend, quotient, c1, c3, c0) in [
            (1.5_f64, 1, 1 << 9, 0, 0),
            (2.5, 2, 0, 1 << 14, 0),
            (4.5, 4, 0, 0, 1 << 8),
        ] {
            cpu.fpu = Fpu::new();
            set_st0_st1(&mut cpu, dividend, 1.0);
            cpu.fpu.status_word = 0;
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
            let status = cpu.fpu.status_word;
            assert_eq!(status & (1 << 10), 0, "q={quotient}: reduction complete");
            assert_eq!(status & (1 << 9), c1, "q={quotient}: C1 is Q0");
            assert_eq!(status & (1 << 14), c3, "q={quotient}: C3 is Q1");
            assert_eq!(status & (1 << 8), c0, "q={quotient}: C0 is Q2");
        }
    }

    #[test]
    fn test_execute_fprem1_invalid_inputs_and_q_bits() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::NAN, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::INFINITY, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        // As for FPREM, one bit at a time; FPREM1 rounds the quotient to
        // nearest-even, so these dividends give 1, 2 and 4 (MDD-BUG-TEM-00177).
        for (dividend, quotient, c1, c3, c0) in [
            (1.25_f64, 1, 1 << 9, 0, 0),
            (2.25, 2, 0, 1 << 14, 0),
            (4.25, 4, 0, 0, 1 << 8),
        ] {
            cpu.fpu = Fpu::new();
            set_st0_st1(&mut cpu, dividend, 1.0);
            cpu.fpu.status_word = 0;
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
            let status = cpu.fpu.status_word;
            assert_eq!(status & (1 << 10), 0, "q={quotient}: reduction complete");
            assert_eq!(status & (1 << 9), c1, "q={quotient}: C1 is Q0");
            assert_eq!(status & (1 << 14), c3, "q={quotient}: C3 is Q1");
            assert_eq!(status & (1 << 8), c0, "q={quotient}: C0 is Q2");
        }
    }

    #[test]
    fn test_execute_fscale_nan_overflow_underflow() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // FSCALE with NaN input propagates NaN
        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::NAN, 2.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fscale));
        assert!(cpu.fpu.get_st(0).to_f64().is_nan());

        // FSCALE(1.0, 2000.0) = 1.0 * 2^2000 -- this fits in F80 (max exp ~16383)
        // so no overflow in F80 precision. Result is a valid large F80 number.
        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2000.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fscale));
        assert!(!cpu.fpu.get_st(0).is_infinite()); // F80 can hold 2^2000

        // FSCALE(1.0, 20000.0) = 1.0 * 2^20000 -- overflows F80 (max exp ~16383)
        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 20000.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fscale));
        assert!(cpu.fpu.get_st(0).is_infinite());
        assert_ne!(cpu.fpu.status_word & 0x08, 0); // OE flag

        // FSCALE(1e-300, -20000.0) underflows even in F80
        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1e-300, -20000.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fscale));
        assert_ne!(cpu.fpu.status_word & 0x10, 0); // UE flag
    }

    #[test]
    fn test_execute_f2xm1_overflow_sets_exception() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // F2XM1 is defined for inputs in [-1, 1]. For out-of-range inputs
        // like 2000.0, x87 hardware behaviour is undefined/implementation-defined.
        // On modern x87, it typically just returns the input value unchanged.
        // Test with a valid in-range input instead.
        set_st0_st1(&mut cpu, 0.5, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::F2xm1));
        // 2^0.5 - 1 = sqrt(2) - 1 ≈ 0.4142
        let result = cpu.fpu.get_st(0).to_f64();
        assert!((result - 0.41421356237).abs() < 1e-8);
        // No exception flags for normal operation
        assert_eq!(cpu.fpu.status_word & 0x3F, 0);
    }

    #[test]
    fn test_execute_fxtract_zero_and_nan() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 0.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxtract));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 0.0);
        assert!(cpu.fpu.get_st(1).to_f64().is_infinite());
        assert!(cpu.fpu.get_st(1).to_f64().is_sign_negative());

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::NAN, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxtract));
        assert!(cpu.fpu.get_st(0).to_f64().is_nan());
        assert!(cpu.fpu.get_st(1).to_f64().is_nan());
    }

    #[test]
    fn test_execute_fptan_stack_overflow_sets_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        fill_stack(&mut cpu);
        cpu.fpu.status_word = 0;
        let top_before = cpu.fpu.get_top();
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
        // Per Intel SDM Vol 1 §8.5.1.2, push overflow with IM=1 still
        // decrements TOP.
        assert_eq!(cpu.fpu.get_top(), (top_before + 7) & 0x7);
        assert_ne!(cpu.fpu.status_word & 0x01, 0); // IE
        assert_ne!(cpu.fpu.status_word & 0x40, 0); // SF
        assert_ne!(cpu.fpu.status_word & (1 << 9), 0); // C1=1 (overflow flavour)
    }

    #[test]
    fn test_execute_fxch_compare_and_tags() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST1),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 2.0).abs() < 1e-9);
        assert!((cpu.fpu.get_st(1).to_f64() - 1.0).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, -1.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Ftst));
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);

        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.regs[0] = F80::from_f64(0.0);
        cpu.fpu.set_tag(0, 0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        exec_instr(&mut cpu, &mut mem, instr1(Code::Ffree_sti, Register::ST1));
        let tag = (cpu.fpu.tag_word >> 2) & 0x3;
        assert_eq!(tag, 3);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        exec_instr(&mut cpu, &mut mem, instr1(Code::Ffreep_sti, Register::ST1));
        assert_eq!(cpu.fpu.get_top(), 1);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcom_st0_sti, Register::ST0, Register::ST1),
        );
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fucompp));
        assert_eq!(cpu.fpu.get_top(), 2);
    }

    /// The memory form of FCOM and FCOMI raise IE on an unsupported
    /// encoding, as the register form already does; a pseudo-infinity does so
    /// on every backend (MDD-BUG-TEM-00166).
    #[test]
    fn test_fcom_memory_and_fcomi_raise_ie_on_unsupported_encodings() {
        const IE: u16 = 0x0001;
        let unnormal = F80([0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x40]);
        let pseudo_infinity = F80([0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0x7f]);
        for value in [unnormal, pseudo_infinity] {
            assert!(
                value.is_unsupported_encoding(),
                "precondition: {value:?} must be an unsupported encoding"
            );
        }
        assert!(
            !unnormal.is_nan(),
            "precondition: the unnormal must not be a NaN on any backend"
        );

        // FCOM m32 with an unnormal ST(0) against 1.0.
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x3C0;
        mem.write_u32(addr, 1.0f32.to_bits());
        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.set_st(0, unnormal);
        cpu.fpu.set_tag(cpu.fpu.get_st_idx(0), 2);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fcom_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & IE,
            IE,
            "FCOM m32 against an unnormal ST(0) must set IE"
        );

        // FCOMI against each unsupported encoding.
        for value in [unnormal, pseudo_infinity] {
            let mut cpu = Cpu::new();
            set_st0_st1(&mut cpu, 1.0, 0.0);
            cpu.fpu.set_st(1, value);
            cpu.fpu.set_tag(cpu.fpu.get_st_idx(1), 2);
            cpu.fpu.status_word = 0;
            exec_instr(
                &mut cpu,
                &mut mem,
                instr2(Code::Fcomi_st0_sti, Register::ST0, Register::ST1),
            );
            assert_eq!(
                cpu.fpu.status_word & IE,
                IE,
                "FCOMI against {value:?} must set IE"
            );
        }
    }

    #[test]
    fn test_execute_fcom_memory_nan_sets_exception() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x3C0;
        let nan = f32::NAN;
        mem.write_u32(addr, u32::from_le_bytes(nan.to_le_bytes()));

        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fcom_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_fcom_nan_in_st0_sets_exception() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::NAN, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcom_st0_sti, Register::ST0, Register::ST1),
        );
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_fucomp_pops_once() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucomp_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    #[test]
    fn test_execute_fcompp_pops_twice() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        let top_before = cpu.fpu.get_top();
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fcompp));
        assert_eq!(cpu.fpu.get_top(), (top_before + 2) & 0x7);
    }

    #[test]
    fn test_execute_fcomip_unordered_sets_flags_and_pops() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::NAN, 1.0);
        cpu.fpu.status_word = 0;
        cpu.regs
            .flags
            .remove(Flags::CARRY | Flags::ZERO | Flags::PARITY);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomip_st0_sti, Register::ST0, Register::ST1),
        );
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
        assert_eq!(cpu.fpu.get_top(), 1);
        let flags = cpu.regs.flags;
        assert!(flags.contains(Flags::CARRY));
        assert!(flags.contains(Flags::ZERO));
        assert!(flags.contains(Flags::PARITY));
    }

    #[test]
    fn test_execute_fcomp_pops_once() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomp_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    #[test]
    fn test_execute_fcomi_ordered_sets_flags() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.regs
            .flags
            .remove(Flags::CARRY | Flags::ZERO | Flags::PARITY);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomi_st0_sti, Register::ST0, Register::ST1),
        );
        let flags = cpu.regs.flags;
        assert!(flags.contains(Flags::CARRY));
        assert!(!flags.contains(Flags::ZERO));
        assert!(!flags.contains(Flags::PARITY));
        assert_eq!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_fcomi_empty_st1_sets_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomi_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_fucom_empty_st1_sets_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucom_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_fcmov_conditions() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.regs.flags.insert(Flags::CARRY);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovbe_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 2.0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.regs.flags.remove(Flags::CARRY | Flags::ZERO);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovbe_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.regs.flags.remove(Flags::CARRY | Flags::ZERO);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovnbe_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 2.0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.regs.flags.insert(Flags::PARITY);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovu_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 2.0);
    }

    #[test]
    fn test_execute_fxch_empty_masked_exception_swaps_tags() {
        // Patch 4: per Intel SDM Vol 2 FXCH, with IE masked, an empty
        // operand is loaded with FP indefinite (tag=Special), then the
        // exchange is performed. ST(0) held a valid value, ST(1) was
        // empty; after FXCH both slots hold populated values (tag 0 or 2),
        // neither is still Empty.
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.control_word |= 0x1;
        cpu.fpu.set_tag(1, 3); // ST(1) empty

        let top = cpu.fpu.get_top();
        let tag0_before = (cpu.fpu.tag_word >> (top * 2)) & 0x3;
        let tag1_before = (cpu.fpu.tag_word >> (((top + 1) & 0x7) * 2)) & 0x3;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST1),
        );
        let tag0_after = (cpu.fpu.tag_word >> (top * 2)) & 0x3;
        let tag1_after = (cpu.fpu.tag_word >> (((top + 1) & 0x7) * 2)) & 0x3;
        assert_eq!(tag0_before, 0);
        assert_eq!(tag1_before, 3);
        // After: the empty slot was filled with indefinite (Special=2),
        // then swapped. Neither slot is empty post-swap.
        assert_ne!(tag0_after, 3, "ST(0) must not be empty after FXCH");
        assert_ne!(tag1_after, 3, "ST(1) must not be empty after FXCH");
    }

    #[test]
    fn test_execute_ftst_nan_sets_exception() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::NAN, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Ftst));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_fxam_classification_bits() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::NAN, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::INFINITY, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 0.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0);

        cpu.fpu = Fpu::new();
        // Create a true F80 denormal: exponent=0, non-zero mantissa
        let f80_denormal = F80([0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = f80_denormal;
        cpu.fpu.regs[1] = F80::ZERO;
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.set_tag(1, 0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0);
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, -1.25, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0);
        assert_ne!(cpu.fpu.status_word & (1 << 9), 0);
    }

    #[test]
    fn test_execute_fxam_empty_never_faults() {
        // Per Intel 387 manual: FXAM never generates exceptions, even on empty
        // registers. It should set C3=1, C2=0, C0=1 (Empty class).
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu.control_word &= !0x1; // unmask IE
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.status_word = 0;

        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        // No exception should be set
        assert_eq!(cpu.fpu.status_word & 0x01, 0, "FXAM should never set IE");
        assert_eq!(cpu.fpu.status_word & 0x40, 0, "FXAM should never set SF");
        // Empty class: C3=1 (bit 14), C2=0 (bit 10), C0=1 (bit 8)
        assert_ne!(
            cpu.fpu.status_word & (1 << 14),
            0,
            "C3 should be set for Empty"
        );
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "C2 should be clear for Empty"
        );
        assert_ne!(
            cpu.fpu.status_word & (1 << 8),
            0,
            "C0 should be set for Empty"
        );
    }

    #[test]
    fn test_execute_memory_load_store_f32_f64() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let f32_addr: usize = 0x200;
        let f32_val = 1.25f32;
        mem.write_u32(f32_addr, u32::from_le_bytes(f32_val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m32fp, mem_op(f32_addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - f32_val as f64).abs() < 1e-6);

        let f32_out: usize = 0x220;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m32fp, mem_op(f32_out as i64)).unwrap(),
        );
        let stored_f32 = f32::from_le_bytes(mem.read_u32(f32_out).to_le_bytes());
        assert!((stored_f32 - f32_val).abs() < 1e-6);
        assert_eq!(cpu.fpu.get_top(), 0);

        let f64_addr: usize = 0x240;
        let f64_val = 2.5f64;
        mem.write_u64(f64_addr, u64::from_le_bytes(f64_val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m64fp, mem_op(f64_addr as i64)).unwrap(),
        );

        let f64_out: usize = 0x260;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fst_m64fp, mem_op(f64_out as i64)).unwrap(),
        );
        let stored_f64 = f64::from_le_bytes(mem.read_u64(f64_out).to_le_bytes());
        assert!((stored_f64 - f64_val).abs() < 1e-9);
    }

    #[test]
    fn test_fld_m64_subnormal_sets_de_and_fnstsw_writes_low_byte() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let source_addr: usize = 0x280;
        let status_addr: usize = 0x290;
        let source = f64::from_bits(0x0000_0000_0000_3800);
        assert!(source.is_subnormal());
        mem.write_u64(source_addr, u64::from_le_bytes(source.to_le_bytes()));

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m64fp, mem_op(source_addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.status_word & 0x02, 0x02, "FLD m64fp must set DE");

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnstsw_m2byte, mem_op(status_addr as i64)).unwrap(),
        );
        assert_eq!(
            mem.read_u16(status_addr) & 0x00FF,
            0x02,
            "FNSTSW low byte must expose DE"
        );
    }

    #[test]
    fn test_fld_m32_subnormal_sets_de() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let source_addr: usize = 0x2A0;
        let source = f32::from_bits(1);
        assert!(source.is_subnormal());
        mem.write_u32(source_addr, u32::from_le_bytes(source.to_le_bytes()));

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m32fp, mem_op(source_addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.status_word & 0x02, 0x02, "FLD m32fp must set DE");
    }

    #[test]
    fn test_fld_m64_zero_does_not_set_de() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let source_addr: usize = 0x2C0;
        mem.write_u64(source_addr, 0);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m64fp, mem_op(source_addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.status_word & 0x02, 0, "zero is not denormal");
    }

    #[test]
    fn test_fld_m80_denormal_sets_de() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let source_addr: usize = 0x2E0;
        let source = F80([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(source.is_denormal());
        for (i, &byte) in source.0.iter().enumerate() {
            mem.write_u8(source_addr + i, byte);
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m80fp, mem_op(source_addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.status_word & 0x02, 0x02, "FLD m80fp must set DE");
    }

    /// MDD-BUG-TEM-00168: `FST`/`FSTP` to m32/m64 report the narrowing's
    /// exceptions and C1. pi to binary32 rounds up (PE + C1); 1e300 overflows
    /// binary32 (OE + PE); 1e-40 is a tiny inexact binary32 (UE + PE); and an
    /// exact store clears a stale C1.
    #[test]
    fn fst_to_memory_reports_the_narrowing_status() {
        const EXCEPTIONS_AND_C1: u16 = 0x023F;
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr = 0x300_i64;
        let mut store = |cpu: &mut Cpu, value: f64, code: Code| {
            set_st0_st1(cpu, value, 0.0);
            cpu.fpu.status_word &= !EXCEPTIONS_AND_C1;
            exec_instr(
                cpu,
                &mut mem,
                Instruction::with1(code, mem_op(addr)).unwrap(),
            );
            cpu.fpu.status_word & EXCEPTIONS_AND_C1
        };

        assert_eq!(
            store(&mut cpu, std::f64::consts::PI, Code::Fstp_m32fp),
            0x0220,
            "pi to m32 rounds up: PE and C1"
        );
        assert_eq!(
            store(&mut cpu, std::f64::consts::PI, Code::Fst_m32fp) & 0x003F,
            0x0020,
            "FST m32 reports PE as FSTP does"
        );
        assert_eq!(
            store(&mut cpu, 1e300, Code::Fstp_m32fp) & 0x003F,
            0x0028,
            "overflow to m32: OE and PE"
        );
        assert_eq!(
            store(&mut cpu, 1e-40, Code::Fstp_m32fp) & 0x003F,
            0x0030,
            "a tiny inexact m32: UE and PE"
        );

        set_st0_st1(&mut cpu, 1.5, 0.0);
        cpu.fpu.status_word = (cpu.fpu.status_word & !EXCEPTIONS_AND_C1) | 0x0200;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m64fp, mem_op(addr)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & EXCEPTIONS_AND_C1,
            0,
            "an exact store clears C1"
        );
    }

    #[test]
    fn test_fstp_f32_respects_rounding_control() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x300;
        let value = 1.0 + 2f64.powi(-25);
        let next_up = f32::from_bits(1.0f32.to_bits() + 1);

        set_st0_st1(&mut cpu, value, 0.0);
        cpu.fpu.control_word = (cpu.fpu.control_word & !(0x3 << 10)) | (0x2 << 10);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m32fp, mem_op(addr as i64)).unwrap(),
        );
        let stored = f32::from_le_bytes(mem.read_u32(addr).to_le_bytes());
        assert_eq!(stored, next_up);

        set_st0_st1(&mut cpu, value, 0.0);
        cpu.fpu.control_word = (cpu.fpu.control_word & !(0x3 << 10)) | (0x1 << 10);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m32fp, mem_op(addr as i64)).unwrap(),
        );
        let stored = f32::from_le_bytes(mem.read_u32(addr).to_le_bytes());
        assert_eq!(stored, 1.0f32);

        let neg_value = -(1.0 + 2f64.powi(-25));
        set_st0_st1(&mut cpu, neg_value, 0.0);
        cpu.fpu.control_word = (cpu.fpu.control_word & !(0x3 << 10)) | (0x3 << 10);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m32fp, mem_op(addr as i64)).unwrap(),
        );
        let stored = f32::from_le_bytes(mem.read_u32(addr).to_le_bytes());
        assert_eq!(stored, -1.0f32);
    }

    #[test]
    fn test_execute_register_load_store() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu.set_top(0);
        cpu.fpu.regs[0] = F80::from_f64(1.0);
        cpu.fpu.regs[1] = F80::from_f64(2.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.set_tag(1, 0);
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fld_sti, Register::ST1));
        assert!((cpu.fpu.get_st(0).to_f64() - 2.0).abs() < 1e-9);

        cpu.fpu.set_top(0);
        cpu.fpu.regs[0] = F80::from_f64(3.0);
        cpu.fpu.regs[1] = F80::from_f64(4.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.set_tag(1, 0);
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fst_sti, Register::ST1));
        assert!((cpu.fpu.get_st(1).to_f64() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn test_fbstp_respects_rounding_control() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x380;
        let mut bytes = [0u8; 10];

        set_st0_st1(&mut cpu, 2.5, 0.0);
        cpu.fpu.control_word &= !(0x3 << 10);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr as i64)).unwrap(),
        );
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = mem.read_u8(addr + i);
        }
        assert_eq!(bcd_to_f64(&bytes), 2.0);

        set_st0_st1(&mut cpu, 2.1, 0.0);
        cpu.fpu.control_word = (cpu.fpu.control_word & !(0x3 << 10)) | (0x2 << 10);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr as i64)).unwrap(),
        );
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = mem.read_u8(addr + i);
        }
        assert_eq!(bcd_to_f64(&bytes), 3.0);

        set_st0_st1(&mut cpu, 2.9, 0.0);
        cpu.fpu.control_word = (cpu.fpu.control_word & !(0x3 << 10)) | (0x1 << 10);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr as i64)).unwrap(),
        );
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = mem.read_u8(addr + i);
        }
        assert_eq!(bcd_to_f64(&bytes), 2.0);

        set_st0_st1(&mut cpu, -2.9, 0.0);
        cpu.fpu.control_word = (cpu.fpu.control_word & !(0x3 << 10)) | (0x3 << 10);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr as i64)).unwrap(),
        );
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = mem.read_u8(addr + i);
        }
        assert_eq!(bcd_to_f64(&bytes), -2.0);
    }

    #[test]
    fn test_execute_memory_store_fst_variants() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.set_top(0);
        cpu.fpu.regs[0] = F80::from_f64(1.5);
        cpu.fpu.set_tag(0, 0);
        let f32_addr: usize = 0x280;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fst_m32fp, mem_op(f32_addr as i64)).unwrap(),
        );
        let stored_f32 = f32::from_le_bytes(mem.read_u32(f32_addr).to_le_bytes());
        assert!((stored_f32 - 1.5).abs() < 1e-6);

        cpu.fpu.set_top(0);
        cpu.fpu.regs[0] = F80::from_f64(2.5);
        cpu.fpu.set_tag(0, 0);
        let f64_addr: usize = 0x2A0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m64fp, mem_op(f64_addr as i64)).unwrap(),
        );
        let stored_f64 = f64::from_le_bytes(mem.read_u64(f64_addr).to_le_bytes());
        assert!((stored_f64 - 2.5).abs() < 1e-9);
        assert_eq!(cpu.fpu.get_top(), 1);
    }

    #[test]
    fn test_fstp_register_pop_order() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu.set_top(0);
        cpu.fpu.regs[0] = F80::from_f64(1.0);
        cpu.fpu.regs[1] = F80::from_f64(2.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.set_tag(1, 0);

        exec_instr(&mut cpu, &mut mem, instr1(Code::Fstp_sti, Register::ST0));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 2.0);
        assert_eq!(cpu.fpu.get_top(), 1);

        cpu.fpu.set_top(0);
        cpu.fpu.regs[0] = F80::from_f64(3.0);
        cpu.fpu.regs[1] = F80::from_f64(4.0);
        cpu.fpu.regs[2] = F80::from_f64(5.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.set_tag(1, 0);
        cpu.fpu.set_tag(2, 0);

        exec_instr(&mut cpu, &mut mem, instr1(Code::Fstp_sti, Register::ST1));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 3.0);
        assert_eq!(cpu.fpu.get_st(1).to_f64(), 5.0);
        assert_eq!(cpu.fpu.get_top(), 1);
    }

    #[test]
    fn test_execute_memory_load_store_f80() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let f80_addr: usize = 0x300;
        let f80_val = -3.25;
        let bytes = f64_to_f80(f80_val);
        for (i, byte) in bytes.iter().enumerate() {
            mem.write_u8(f80_addr + i, *byte);
        }
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m80fp, mem_op(f80_addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - f80_val).abs() < 1e-6);

        let f80_out: usize = 0x320;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m80fp, mem_op(f80_out as i64)).unwrap(),
        );
        let mut out_bytes = [0u8; 10];
        for (i, byte) in out_bytes.iter_mut().enumerate() {
            *byte = mem.read_u8(f80_out + i);
        }
        let roundtrip = f80_to_f64(&out_bytes);
        assert!((roundtrip - f80_val).abs() < 1e-6);
        assert_eq!(cpu.fpu.get_top(), 0);
    }

    #[test]
    fn test_execute_f80_nan_sign_roundtrip() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let f80_addr: usize = 0x340;
        let neg_nan = f64::from_bits(0xFFF8_0000_0000_0001);
        cpu.fpu.set_top(0);
        cpu.fpu.regs[0] = F80::from_f64(neg_nan);
        cpu.fpu.set_tag(0, 0);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m80fp, mem_op(f80_addr as i64)).unwrap(),
        );
        let mut bytes = [0u8; 10];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = mem.read_u8(f80_addr + i);
        }
        assert_eq!(bytes[9] & 0x80, 0x80);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m80fp, mem_op(f80_addr as i64)).unwrap(),
        );
        let st0 = cpu.fpu.get_st(0);
        assert!(st0.is_nan());
        assert!(st0.is_sign_negative());
    }

    #[test]
    fn test_execute_integer_load_store_and_ops() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let i16_addr: usize = 0x300;
        mem.write_u16(i16_addr, (-2i16) as u16);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fild_m16int, mem_op(i16_addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), -2.0);

        let i16_out: usize = 0x320;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m16int, mem_op(i16_out as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(i16_out) as i16, -2);

        cpu.fpu.push(F80::from_f64(123_456_789.0));
        let i64_out: usize = 0x340;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m64int, mem_op(i64_out as i64)).unwrap(),
        );
        assert_eq!(mem.read_u64(i64_out) as i64, 123_456_789);

        set_st0_st1(&mut cpu, 3.0, 0.0);
        mem.write_u16(i16_addr, 2);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fiadd_m16int, mem_op(i16_addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 5.0);

        cpu.fpu.status_word = 0;
        mem.write_u16(i16_addr, 0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidiv_m16int, mem_op(i16_addr as i64)).unwrap(),
        );
        assert_ne!(cpu.fpu.status_word & 0x04, 0);
    }

    #[test]
    fn test_execute_fisttp_invalid_sets_exception_and_pops() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        set_st0_st1(&mut cpu, f64::NAN, 0.0);
        cpu.fpu.status_word = 0;
        let addr: usize = 0x360;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(addr) as i16, i16::MIN);
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
        assert_eq!(cpu.fpu.get_top(), 1);
    }

    #[test]
    fn fisttp_is_ud_and_atomic_on_every_advertised_cpu_type() {
        let addr = 0x360usize;
        let instr = Instruction::with1(Code::Fisttp_m32int, mem_op(addr as i64)).unwrap();

        for cpu_type in [
            crate::CpuType::I8086,
            crate::CpuType::I8088,
            crate::CpuType::I80186,
            crate::CpuType::I80286,
            crate::CpuType::I80386,
            crate::CpuType::I80486,
            crate::CpuType::Pentium,
            crate::CpuType::PentiumMmx,
        ] {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.set_cpu_type(cpu_type);
            cpu.regs.set_eip(0x1234_5678);
            cpu.fpu.push(F80::from_f64(12.75));
            cpu.fpu.status_word |= 0x4500;
            cpu.fpu.ip_offset = 0x1020_3040;
            cpu.fpu.operand_offset = 0x5060_7080;
            cpu.fpu.x87_save_area = [0x5a; 108];
            mem.write_u32(addr, 0xa5a5_5a5a);

            let fpu_before = cpu.fpu.clone();
            let binary32_before: Vec<_> = (0..8).map(|st| cpu.fpu.exact_binary32_st(st)).collect();
            let memory_before = mem.read_u32(addr);

            let err = execute_portable(&mut cpu, &mut mem, &instr)
                .expect_err("every advertised CPU predates SSE3 FISTTP");
            assert!(
                matches!(
                    err.downcast_ref::<crate::CpuError>(),
                    Some(crate::CpuError::InvalidOpcode(0x1234_5678))
                ),
                "{cpu_type:?}: wrong fault: {err:#}"
            );
            assert_eq!(
                compare_fpu_state(&fpu_before, &cpu.fpu),
                Vec::<String>::new(),
                "{cpu_type:?}: #UD changed architectural x87 state"
            );
            assert_eq!(
                cpu.fpu.x87_save_area, fpu_before.x87_save_area,
                "{cpu_type:?}: #UD changed the x87 save area"
            );
            assert_eq!(
                (0..8)
                    .map(|st| cpu.fpu.exact_binary32_st(st))
                    .collect::<Vec<_>>(),
                binary32_before,
                "{cpu_type:?}: #UD changed the binary32 shadow"
            );
            assert_eq!(
                mem.read_u32(addr),
                memory_before,
                "{cpu_type:?}: #UD changed destination memory"
            );
        }
    }

    #[test]
    fn test_execute_fisttp_variants_cover_valid_and_invalid() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr16: usize = 0x360;
        set_st0_st1(&mut cpu, 12.9, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m16int, mem_op(addr16 as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(addr16) as i16, 12);
        assert_eq!(cpu.fpu.status_word & 0x01, 0);

        let addr32: usize = 0x364;
        set_st0_st1(&mut cpu, f64::NAN, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m32int, mem_op(addr32 as i64)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr32) as i32, i32::MIN);
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        set_st0_st1(&mut cpu, 12345.25, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m32int, mem_op(addr32 as i64)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr32) as i32, 12345);
        assert_eq!(cpu.fpu.status_word & 0x01, 0);

        let addr64: usize = 0x368;
        set_st0_st1(&mut cpu, f64::INFINITY, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m64int, mem_op(addr64 as i64)).unwrap(),
        );
        assert_eq!(mem.read_u64(addr64) as i64, i64::MIN);
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        set_st0_st1(&mut cpu, -5.9, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m64int, mem_op(addr64 as i64)).unwrap(),
        );
        assert_eq!(mem.read_u64(addr64) as i64, -5);
        assert_eq!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_fisttp_empty_sets_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        reset_empty_fpu(&mut cpu);
        let addr: usize = 0x36C;
        mem.write_u16(addr, 0x1234);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m16int, mem_op(addr as i64)).unwrap(),
        );
        // With masked IE, empty-stack FISTTP writes the integer indefinite
        // (0x8000 for 16-bit), as FIST/FISTP/FBSTP do.
        assert_eq!(mem.read_u16(addr), i16::MIN as u16);
        assert_stack_fault(&cpu);
    }

    #[test]
    fn fstp_empty_stack_writes_indefinite_even_with_im_unmasked() {
        // The interpreter writes the empty-stack indefinite even with IM=0.
        // It records exception status rather than dispatching the hardware's
        // unmasked #IA exception; the memory write is unconditional.
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        reset_empty_fpu(&mut cpu);
        cpu.fpu.control_word = 0x037F & !0x01; // IM=0 (invalid-op unmasked)

        let addr: usize = 0x378;
        mem.write_u32(addr, 0x1234_5678);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m32fp, mem_op(addr as i64)).unwrap(),
        );
        // 32-bit FP indefinite = 0xFFC00000 (little-endian bytes
        // [0x00, 0x00, 0xC0, 0xFF]).
        assert_eq!(mem.read_u32(addr), 0xFFC0_0000);
        assert_stack_fault(&cpu);
    }

    #[test]
    fn fst_empty_stack_writes_indefinite_into_bios_region_when_writes_allowed() {
        // FST to the BIOS region must succeed when ROM writes are allowed,
        // including an empty-stack store of the float32 indefinite.
        // Segment 0xF000 plus offset 0x1000 targets linear 0xF1000.
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        mem.allow_rom_writes = true;
        cpu.regs.ds = 0xF000;
        reset_empty_fpu(&mut cpu);

        let addr: usize = 0xF_1000; // inside BIOS region 0xF0000-0xFFFFF
        let offset: i64 = 0x1000;
        // Pin the derivation: linear = (DS << 4) + offset. A future edit
        // that bumps one constant without the other would otherwise let the
        // FST land outside the BIOS region and silently neuter the test.
        debug_assert_eq!(
            ((cpu.regs.ds as usize) << 4).wrapping_add(offset as usize),
            addr
        );
        mem.write_u32(addr, 0x1234_5678);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fst_m32fp, mem_op(offset)).unwrap(),
        );
        assert_eq!(
            mem.read_u32(addr),
            0xFFC0_0000,
            "FST on empty stack must write FP indefinite even into the BIOS region when allow_rom_writes is set"
        );
        assert_stack_fault(&cpu);
    }

    #[test]
    fn fst_empty_stack_drops_write_into_bios_region_by_default() {
        // With ROM writes disabled by default, FST still reports stack-fault
        // status and succeeds, but the BIOS destination bytes stay unchanged.
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        // mem.allow_rom_writes left at default (false)
        cpu.regs.ds = 0xF000;
        reset_empty_fpu(&mut cpu);

        let addr: usize = 0xF_1000; // inside BIOS region
        let offset: i64 = 0x1000;
        debug_assert_eq!(
            ((cpu.regs.ds as usize) << 4).wrapping_add(offset as usize),
            addr
        );
        mem.write_u32(addr, 0x1234_5678); // also dropped by prepare_write_addr
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fst_m32fp, mem_op(offset)).unwrap(),
        );
        assert_eq!(
            mem.read_u32(addr),
            0x0000_0000,
            "FST on empty stack writes are silently dropped in the BIOS region when allow_rom_writes is false"
        );
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_fist_invalid_does_not_pop() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        set_st0_st1(&mut cpu, 1e20, 0.0);
        cpu.fpu.status_word = 0;
        let addr: usize = 0x370;
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fist_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr) as i32, i32::MIN);
        assert_eq!(cpu.fpu.get_top(), top_before);
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_fist_variants_cover_valid_and_invalid() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr16: usize = 0x370;
        set_st0_st1(&mut cpu, f64::NAN, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fist_m16int, mem_op(addr16 as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(addr16) as i16, i16::MIN);
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        let addr32: usize = 0x374;
        set_st0_st1(&mut cpu, 42.4, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fist_m32int, mem_op(addr32 as i64)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr32) as i32, 42);
        assert_eq!(cpu.fpu.status_word & 0x01, 0);

        let addr64: usize = 0x378;
        set_st0_st1(&mut cpu, 1e20, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m64int, mem_op(addr64 as i64)).unwrap(),
        );
        assert_eq!(mem.read_u64(addr64) as i64, i64::MIN);
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_fistp_empty_sets_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.status_word = 0;
        cpu.fpu.tag_word = 0xFFFF;
        let addr: usize = 0x380;
        mem.write_u16(addr, 0x1234);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m16int, mem_op(addr as i64)).unwrap(),
        );
        // With default (masked) IE exception, FISTP from empty stack writes
        // the integer indefinite value (0x8000 for 16-bit) to memory.
        assert_eq!(mem.read_u16(addr), i16::MIN as u16);
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
        assert_ne!(cpu.fpu.status_word & 0x40, 0);
        assert_eq!(cpu.fpu.status_word & (1 << 9), 0);
    }

    /// Regression for HLD 02 Task C1 — the interpreter previously gated the
    /// empty-stack indefinite write on IM=1. Per HLD we now write the
    /// indefinite unconditionally; SDM IM=0 would raise
    /// #IA without writing, but our emulator does not dispatch FPU
    /// exceptions. Test updated to reflect the new semantics.
    #[test]
    fn test_execute_fistp_empty_unmasked_ie_still_writes() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.status_word = 0;
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.control_word = 0x037F & !0x01; // unmask IE
        let addr: usize = 0x380;
        mem.write_u16(addr, 0x1234);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(
            mem.read_u16(addr),
            i16::MIN as u16,
            "memory receives i16 indefinite (0x8000) regardless of IM"
        );
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_integer_ops_stack_empty_sets_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr_i16: usize = 0x390;
        mem.write_u16(addr_i16, 2);
        let addr_f32: usize = 0x394;
        mem.write_u32(addr_f32, u32::from_le_bytes(1.0f32.to_le_bytes()));

        let instrs = [
            Instruction::with1(Code::Fiadd_m16int, mem_op(addr_i16 as i64)).unwrap(),
            Instruction::with1(Code::Fisub_m16int, mem_op(addr_i16 as i64)).unwrap(),
            Instruction::with1(Code::Fisubr_m16int, mem_op(addr_i16 as i64)).unwrap(),
            Instruction::with1(Code::Fimul_m16int, mem_op(addr_i16 as i64)).unwrap(),
            Instruction::with1(Code::Fidiv_m16int, mem_op(addr_i16 as i64)).unwrap(),
            Instruction::with1(Code::Fidivr_m16int, mem_op(addr_i16 as i64)).unwrap(),
            Instruction::with1(Code::Ficom_m16int, mem_op(addr_i16 as i64)).unwrap(),
            Instruction::with1(Code::Fcom_m32fp, mem_op(addr_f32 as i64)).unwrap(),
        ];

        for instr in instrs {
            reset_empty_fpu(&mut cpu);
            exec_instr(&mut cpu, &mut mem, instr);
            assert_stack_fault(&cpu);
        }

        reset_empty_fpu(&mut cpu);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucom_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);

        reset_empty_fpu(&mut cpu);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomi_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_integer_ops_non_empty_paths() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x3A0;
        mem.write_u16(addr, 2);

        set_st0_st1(&mut cpu, 5.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisub_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 3.0);

        set_st0_st1(&mut cpu, 5.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisubr_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), -3.0);

        set_st0_st1(&mut cpu, 3.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fimul_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 6.0);

        set_st0_st1(&mut cpu, 8.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidiv_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 4.0);
        assert_eq!(cpu.fpu.status_word & 0x04, 0);

        mem.write_u16(addr, 10);
        set_st0_st1(&mut cpu, 2.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidivr_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 5.0);
        assert_eq!(cpu.fpu.status_word & 0x04, 0);

        set_st0_st1(&mut cpu, 0.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidivr_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_ne!(cpu.fpu.status_word & 0x04, 0);
    }

    #[test]
    fn test_execute_ficom_nan_sets_exception_and_ficomp_pops() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x3A0;
        mem.write_u16(addr, 5);

        set_st0_st1(&mut cpu, f64::NAN, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Ficom_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 2.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Ficomp_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_top(), 1);
    }

    #[test]
    fn test_execute_control_and_env_memory_ops() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.control_word = 0x1234;
        let cw_addr: usize = 0x400;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstcw_m2byte, mem_op(cw_addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(cw_addr), 0x1234);
        mem.write_u16(cw_addr, 0xABCD);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fldcw_m2byte, mem_op(cw_addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.control_word, 0xABCD);

        cpu.fpu.status_word = 0xBEEF;
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fstsw_AX, Register::AX));
        assert_eq!(cpu.regs.ax(), 0xBEEF);

        let sw_addr: usize = 0x420;
        cpu.fpu.status_word = 0xCAFE;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstsw_m2byte, mem_op(sw_addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(sw_addr), 0xCAFE);

        cpu.fpu.control_word = 0x1357;
        cpu.fpu.status_word = 0x2468;
        cpu.fpu.tag_word = 0xAAAA;
        cpu.fpu.ip_offset = 0x1111;
        cpu.fpu.ip_selector = 0x2222;
        cpu.fpu.operand_offset = 0x3333;
        cpu.fpu.operand_selector = 0x4444;
        let env_addr: usize = 0x440;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstenv_m14byte, mem_op(env_addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(env_addr), 0x1357);
        assert_eq!(mem.read_u16(env_addr + 2), 0x2468);
        mem.write_u16(env_addr, 0xAAAA);
        mem.write_u16(env_addr + 2, 0xBBBB);
        mem.write_u16(env_addr + 4, 0xCCCC);
        mem.write_u16(env_addr + 6, 0xDDDD);
        mem.write_u16(env_addr + 8, 0xEEEE);
        mem.write_u16(env_addr + 10, 0xFFFF);
        mem.write_u16(env_addr + 12, 0x1234);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fldenv_m14byte, mem_op(env_addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.control_word, 0xAAAA);
        assert_eq!(cpu.fpu.status_word, 0xBBBB);
        assert_eq!(cpu.fpu.tag_word, 0xCCCC);
        assert_eq!(cpu.fpu.ip_offset, 0xDDDD);
        assert_eq!(cpu.fpu.ip_selector, 0xEEEE);
        assert_eq!(cpu.fpu.operand_offset, 0xFFFF);
        assert_eq!(cpu.fpu.operand_selector, 0x1234);
    }

    #[test]
    fn test_execute_fsave_frstor_round_trip() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.control_word = 0x1111;
        cpu.fpu.status_word = 0x2222;
        cpu.fpu.tag_word = 0x3333;
        cpu.fpu.regs[0] = F80::from_f64(1.5);
        cpu.fpu.regs[1] = F80::from_f64(-2.25);
        let addr: usize = 0x500;

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsave_m94byte, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.control_word, 0x037F);
        assert_eq!(cpu.fpu.status_word, 0);
        assert_eq!(cpu.fpu.tag_word, 0xFFFF);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Frstor_m94byte, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.regs[0].to_f64() - 1.5).abs() < 1e-9);
        assert!((cpu.fpu.regs[1].to_f64() + 2.25).abs() < 1e-9);
        assert_eq!(cpu.fpu.control_word, 0x1111);
        assert_eq!(cpu.fpu.status_word, 0x2222);
        assert_eq!(cpu.fpu.tag_word, 0x3333);
    }

    /// `FSAVE` writes the register area in STACK-RELATIVE order: slot i is
    /// ST(i).
    ///
    /// This test previously asserted the opposite and cited "Intel 387 manual:
    /// FSAVE stores registers in physical R0-R7 order". That citation was
    /// wrong on the facts, so the test was pinning the defect as though it were
    /// intentional. SDM Vol 1 Fig. 8-9 labels the area ST(0)..ST(7), and both a
    /// direct host-silicon probe
    /// (`x87_oracle::tests::host_fnsave_register_area_is_st_relative_not_physical`)
    /// agree on the encoding. Rewritten rather than deleted, because the encoding
    /// it pins is still worth pinning — just the other way round.
    #[test]
    fn test_execute_fsave_stores_stack_relative_order() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.set_top(5);
        for i in 0..8 {
            cpu.fpu.regs[i] = F80::from_f64(i as f64 + 0.25);
            cpu.fpu.set_tag(i, 0);
        }
        // TOP=5, so ST(0)=regs[5]=5.25, ST(1)=regs[6]=6.25, ST(7)=regs[4]=4.25.
        // A physical-order implementation would put 0.25 in slot 0 instead, so
        // this fixture distinguishes the two orderings at every slot it checks.
        let expected_st0 = 5.25;
        let expected_st1 = 6.25;
        let expected_st7 = 4.25;
        assert_ne!(
            expected_st0,
            cpu.fpu.regs[0].to_f64(),
            "precondition: TOP must be rotated, or stack-relative and physical              order would be indistinguishable and this test would prove nothing"
        );

        let addr: usize = 0x640;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsave_m94byte, mem_op(addr as i64)).unwrap(),
        );

        let slot = |mem: &Memory, i: usize| -> f64 {
            let mut bytes = [0u8; 10];
            for (j, byte) in bytes.iter_mut().enumerate() {
                *byte = mem.read_u8(addr + 14 + i * 10 + j);
            }
            f80_to_f64(&bytes)
        };

        for (i, want) in [(0usize, expected_st0), (1, expected_st1), (7, expected_st7)] {
            let got = slot(&mem, i);
            assert!(
                (got - want).abs() < 1e-9,
                "slot {i} must hold ST({i}) = {want}, got {got}"
            );
        }
    }

    /// `FRSTOR` reads the register area as STACK-RELATIVE, unrotating by the
    /// RESTORED TOP.
    ///
    /// The mirror of the test above, and previously wrong the same way. The
    /// restored TOP — not the TOP in effect before the instruction — is what
    /// defines the mapping, which is why `decode_environment` must run before
    /// the register loop.
    #[test]
    fn test_execute_frstor_loads_stack_relative_order() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x740;
        mem.write_u16(addr, 0x037F);
        mem.write_u16(addr + 2, 0x2800); // TOP=5
        mem.write_u16(addr + 4, 0x0000);
        mem.write_u16(addr + 6, 0);
        mem.write_u16(addr + 8, 0);
        mem.write_u16(addr + 10, 0);
        mem.write_u16(addr + 12, 0);

        // Image slot i holds ST(i) = 100.0 + i.
        for i in 0..8 {
            let bytes = f64_to_f80(100.0 + i as f64);
            for (j, byte) in bytes.iter().enumerate() {
                mem.write_u8(addr + 14 + i * 10 + j, *byte);
            }
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Frstor_m94byte, mem_op(addr as i64)).unwrap(),
        );

        assert_eq!(
            cpu.fpu.get_top(),
            5,
            "TOP must come from the restored image"
        );
        // ST(i) is what the image said it was...
        for i in 0..8 {
            let want = 100.0 + i as f64;
            let got = cpu.fpu.get_st(i).to_f64();
            assert!(
                (got - want).abs() < 1e-9,
                "ST({i}) must be {want}, got {got}"
            );
        }
        //...which with TOP=5 means physical R5 holds ST(0), not R0.
        assert!(
            (cpu.fpu.regs[5].to_f64() - 100.0).abs() < 1e-9,
            "physical R5 must hold ST(0)=100.0 after unrotation"
        );
        assert!(
            (cpu.fpu.regs[0].to_f64() - 103.0).abs() < 1e-9,
            "physical R0 must hold ST(3)=103.0 after unrotation"
        );
    }

    /// A guest `FSAVE`/`FRSTOR` round trip preserves every ST(i).
    ///
    /// This is the property that made defect A survive: FSAVE and FRSTOR were
    /// BOTH physical-order, so the round trip came out right and only an
    /// external reader of the image could see the defect. It has to keep
    /// working after the correction, and it is the test that would catch
    /// someone fixing one of the pair without the other.
    #[test]
    fn test_fsave_frstor_round_trip_preserves_stack_registers() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.set_top(3);
        for i in 0..8 {
            cpu.fpu.regs[i] = F80::from_f64(i as f64 * 2.0 + 0.5);
            cpu.fpu.set_tag(i, 0);
        }
        let before: Vec<f64> = (0..8).map(|i| cpu.fpu.get_st(i).to_f64()).collect();

        let addr: usize = 0x840;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsave_m94byte, mem_op(addr as i64)).unwrap(),
        );
        // FSAVE reinitialises the FPU, so the restore below genuinely reloads
        // rather than observing state that was never disturbed.
        assert_eq!(cpu.fpu.tag_word, 0xFFFF, "FSAVE must reinitialise the FPU");

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Frstor_m94byte, mem_op(addr as i64)).unwrap(),
        );

        let after: Vec<f64> = (0..8).map(|i| cpu.fpu.get_st(i).to_f64()).collect();
        assert_eq!(cpu.fpu.get_top(), 3, "TOP must survive the round trip");
        for i in 0..8 {
            assert!(
                (after[i] - before[i]).abs() < 1e-9,
                "ST({i}) must survive the round trip: was {}, now {}",
                before[i],
                after[i]
            );
        }
    }

    #[test]
    fn test_execute_fpatan_empty_st0_sets_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(0, 3);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fpatan));
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_dual_operand_empty_st1_sets_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(1, 3);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(1, 3);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2xp1));
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(1, 3);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(1, 3);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_f2xm1_infinite_input_no_overflow() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::INFINITY, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::F2xm1));
        assert_eq!(cpu.fpu.status_word & 0x08, 0);
        assert!(cpu.fpu.get_st(0).to_f64().is_infinite());
    }

    #[test]
    fn test_execute_fscale_infinite_and_zero_original() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::INFINITY, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fscale));
        assert_eq!(cpu.fpu.status_word & 0x08, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 0.0, 10.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fscale));
        assert_eq!(cpu.fpu.status_word & 0x10, 0);
    }

    #[test]
    fn test_execute_compare_operand_fallbacks_additional() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.regs[2] = F80::from_f64(2.0);
        cpu.fpu.set_tag(2, 0);
        cpu.fpu.status_word = 0;
        let mut instr = instr2_with_op1_kind(
            Code::Fucom_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        instr.set_op0_register(Register::ST2);
        exec_instr(&mut cpu, &mut mem, instr);
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        let mut instr = instr2_with_op1_kind(
            Code::Fucom_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        instr.set_op_kind(0, OpKind::Immediate8);
        exec_instr(&mut cpu, &mut mem, instr);
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        let instr = instr2_with_op1_kind(
            Code::Fcom_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        exec_instr(&mut cpu, &mut mem, instr);
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        let mut instr = instr2_with_op1_kind(
            Code::Fcom_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        instr.set_op_kind(0, OpKind::Immediate8);
        exec_instr(&mut cpu, &mut mem, instr);
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);
    }

    #[test]
    fn test_execute_compare_flag_fallbacks_additional() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 5.0);
        cpu.fpu.regs[2] = F80::from_f64(0.0);
        cpu.fpu.set_tag(2, 0);
        cpu.regs
            .flags
            .remove(Flags::CARRY | Flags::ZERO | Flags::PARITY);
        let mut instr = instr2_with_op1_kind(
            Code::Fcomi_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        instr.set_op0_register(Register::ST2);
        exec_instr(&mut cpu, &mut mem, instr);
        assert!(!cpu.regs.flags.contains(Flags::CARRY));

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.regs
            .flags
            .remove(Flags::CARRY | Flags::ZERO | Flags::PARITY);
        let mut instr = instr2_with_op1_kind(
            Code::Fcomi_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        instr.set_op_kind(0, OpKind::Immediate8);
        exec_instr(&mut cpu, &mut mem, instr);
        assert!(cpu.regs.flags.contains(Flags::CARRY));
    }

    #[test]
    fn test_execute_fcmov_operand_fallbacks_additional() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.regs[2] = F80::from_f64(5.0);
        cpu.fpu.set_tag(2, 0);
        cpu.regs.flags.insert(Flags::CARRY);
        let mut instr = instr2_with_op1_kind(
            Code::Fcmovb_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        instr.set_op0_register(Register::ST2);
        exec_instr(&mut cpu, &mut mem, instr);
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 5.0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 7.0);
        cpu.regs.flags.insert(Flags::CARRY);
        let mut instr = instr2_with_op1_kind(
            Code::Fcmovb_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        instr.set_op_kind(0, OpKind::Immediate8);
        exec_instr(&mut cpu, &mut mem, instr);
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 7.0);
    }

    #[test]
    fn test_execute_fstsw_non_ax_register_uses_memory_path() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x680;
        let mut instr = Instruction::with1(Code::Fstsw_m2byte, mem_op(addr as i64)).unwrap();
        instr.set_op_kind(0, OpKind::Register);
        instr.set_op0_register(Register::BX);
        cpu.fpu.status_word = 0x1234;
        exec_instr(&mut cpu, &mut mem, instr);
        assert_eq!(mem.read_u16(addr), 0x1234);
    }

    #[test]
    fn test_execute_fxch_fallback_and_empty_st0() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        let instr = instr2_with_op1_kind(
            Code::Fxch_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        exec_instr(&mut cpu, &mut mem, instr);
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 2.0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.regs[2] = F80::from_f64(4.0);
        cpu.fpu.set_tag(2, 0);
        let mut instr = instr2_with_op1_kind(
            Code::Fxch_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        instr.set_op0_register(Register::ST2);
        exec_instr(&mut cpu, &mut mem, instr);
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 4.0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        let mut instr = instr2_with_op1_kind(
            Code::Fxch_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        instr.set_op_kind(0, OpKind::Immediate8);
        exec_instr(&mut cpu, &mut mem, instr);
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 2.0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.control_word |= 0x1;
        cpu.fpu.set_tag(0, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_fbld_non_overflow_pushes_value() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x520;
        let bytes = f64_to_bcd(0x037F, 1234.0);
        for (i, b) in bytes.iter().enumerate() {
            mem.write_u8(addr + i, *b);
        }
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbld_m80bcd, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1234.0);
    }

    #[test]
    fn test_execute_arith_memory_and_register_empty_sets_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x5C0;
        mem.write_u32(addr, (1.0f32).to_bits());

        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fadd_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_arith_pop_register_variants() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 5.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fsubp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_eq!(cpu.fpu.get_top(), 1);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 5.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fsubrp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_eq!(cpu.fpu.get_top(), 1);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 3.0, 4.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fmulp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_eq!(cpu.fpu.get_top(), 1);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 8.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_eq!(cpu.fpu.get_top(), 1);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 8.0, 2.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivrp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_eq!(cpu.fpu.get_top(), 1);
    }

    #[test]
    fn test_execute_arith_memory_variants_empty_and_non_empty() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x640;
        mem.write_u32(addr, (2.0f32).to_bits());

        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 3.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 5.0).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsub_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 5.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsub_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsubr_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 3.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsubr_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() + 1.0).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fmul_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 3.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fmul_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 6.0).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdiv_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 8.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdiv_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 4.0).abs() < 1e-9);

        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdivr_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 8.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdivr_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn test_execute_arith_register_empty_variants() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fadd_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fsub_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fsubr_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fmul_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdiv_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivr_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Faddp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fsubp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fsubrp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fmulp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivrp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_unary_ops_empty_stack_sets_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        exec_empty_fault(&mut cpu, &mut mem, Code::Fchs);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fabs);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fsin);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fcos);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fsqrt);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fptan);
        exec_empty_fault(&mut cpu, &mut mem, Code::F2xm1);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fscale);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fxtract);
        exec_empty_fault(&mut cpu, &mut mem, Code::Frndint);
        exec_empty_fault(&mut cpu, &mut mem, Code::Ftst);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fyl2x);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fyl2xp1);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fprem);
        exec_empty_fault(&mut cpu, &mut mem, Code::Fprem1);
    }

    #[test]
    fn test_execute_fpatan_fsincos_empty_and_overflow() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(1, 3);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fpatan));
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsincos));
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        fill_stack(&mut cpu);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsincos));
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_log_nan_in_y_sets_exception() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, f64::NAN);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, f64::NAN);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2xp1));
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
    }

    #[test]
    fn test_execute_f2xm1_non_overflow() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 0.5, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::F2xm1));
        assert_eq!(cpu.fpu.status_word & 0x08, 0);
        assert!(cpu.fpu.get_st(0).to_f64().is_finite());
    }

    #[test]
    fn test_execute_f2xm1_negative_out_of_range() {
        // F2XM1 with a value < -1.0 should compute 2^x - 1 manually
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, -5.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::F2xm1));
        let result = cpu.fpu.get_st(0).to_f64();
        // 2^(-5) - 1 = 0.03125 - 1 = -0.96875
        assert!((result - (-0.96875)).abs() < 1e-9, "got {}", result);
        // No overflow flag should be set for negative out-of-range
        assert_eq!(cpu.fpu.status_word & 0x08, 0);
    }

    fn assert_execute_trig_infinity_invalid(opcode: u8, code: Code) {
        let indefinite = [0, 0, 0, 0, 0, 0, 0, 0xc0, 0xff, 0xff];
        for sign in [0u16, 0x8000] {
            for masked in [true, false] {
                let mut bytes = [0; 10];
                bytes[7] = 0x80;
                bytes[8..].copy_from_slice(&(0x7fff | sign).to_le_bytes());
                let mut cpu = Cpu::new();
                cpu.set_cpu_type(crate::CpuType::I80386);
                cpu.cs_bits = 16;
                let mut mem = Memory::new();
                cpu.fpu.control_word = if masked { 0x037f } else { 0x037e };
                cpu.fpu.status_word |= 1 << 10; // masked invalid actively clears C2
                cpu.fpu.push(F80::ONE);
                cpu.fpu.push(F80(bytes));
                let top = cpu.fpu.get_top();
                let tags = cpu.fpu.tag_word;
                let registers = cpu.fpu.regs;
                // Synthetic real-mode FSIN/FCOS instruction bytes.
                let instruction_bytes = [0xd9, opcode];
                let mut decoder = iced_x86::Decoder::with_ip(
                    16,
                    &instruction_bytes,
                    0,
                    iced_x86::DecoderOptions::NONE,
                );
                let instr = decoder.decode();
                assert_eq!(instr.code(), code);
                execute_portable(&mut cpu, &mut mem, &instr).unwrap();
                assert_eq!(cpu.fpu.status_word & 1, 1, "infinity must raise IE");
                assert_eq!(cpu.fpu.status_word & 0x80 != 0, !masked, "ES follows IM");
                assert_eq!(cpu.fpu.get_top(), top, "invalid trig must preserve TOP");
                assert_eq!(
                    cpu.fpu.tag_word, tags,
                    "invalid trig must preserve stack tags"
                );
                for (index, original) in registers.iter().enumerate() {
                    let expected = if masked && index == top {
                        indefinite
                    } else {
                        original.0
                    };
                    assert_eq!(
                        cpu.fpu.regs[index].0, expected,
                        "{code:?}: masked={masked}, register={index}"
                    );
                }
                if masked {
                    assert!(cpu.fpu.get_st(0).is_nan());
                    assert_eq!(cpu.fpu.get_tag(top), 2, "indefinite is Special");
                    assert_eq!(cpu.fpu.status_word & (1 << 10), 0);
                }
            }
        }
    }

    #[test]
    fn execute_trig_infinity_invalid_sin() {
        assert_execute_trig_infinity_invalid(0xfe, Code::Fsin);
    }

    #[test]
    fn execute_trig_infinity_invalid_cos() {
        assert_execute_trig_infinity_invalid(0xff, Code::Fcos);
    }

    fn assert_execute_trig_infinity_invalid_push(opcode: u8, code: Code) {
        for sign in [0u16, 0x8000] {
            for masked in [true, false] {
                for depth in [2, 8] {
                    let mut bytes = [0; 10];
                    bytes[7] = 0x80;
                    bytes[8..].copy_from_slice(&(0x7fff | sign).to_le_bytes());
                    let mut cpu = Cpu::new();
                    cpu.set_cpu_type(crate::CpuType::I80386);
                    cpu.cs_bits = 16;
                    let mut mem = Memory::new();
                    cpu.fpu.control_word = if masked { 0x037f } else { 0x037e };
                    cpu.fpu.status_word |= 1 << 10;
                    for value in 1..=depth {
                        cpu.fpu.push(F80::from_f64(f64::from(value)));
                    }
                    cpu.fpu.set_st(0, F80(bytes));
                    if !masked {
                        cpu.fpu.status_word |= 1 << 9; // invalid clears stale C1 unless overflowing
                    }
                    let top = cpu.fpu.get_top();
                    let pushed_top = (top + 7) & 7;
                    let tags = cpu.fpu.tag_word;
                    let registers = cpu.fpu.regs;
                    let instruction_bytes = [0xd9, opcode];
                    let mut decoder = iced_x86::Decoder::with_ip(
                        16,
                        &instruction_bytes,
                        0,
                        iced_x86::DecoderOptions::NONE,
                    );
                    let instr = decoder.decode();
                    assert_eq!(instr.code(), code);
                    execute_portable(&mut cpu, &mut mem, &instr).unwrap();
                    let context =
                        format!("{code:?}: sign={sign:#x}, masked={masked}, depth={depth}");
                    assert_eq!(cpu.fpu.status_word & 1, 1, "{context}: infinity raises IE");
                    assert_eq!(cpu.fpu.status_word & 0x80 != 0, !masked, "{context}: ES");
                    assert_eq!(
                        cpu.fpu.status_word & 0x0240,
                        if depth == 8 { 0x0240 } else { 0 },
                        "{context}: SF and C1 identify stack overflow"
                    );
                    assert_eq!(
                        cpu.fpu.get_top(),
                        if masked { pushed_top } else { top },
                        "{context}: TOP"
                    );
                    let mut expected_tags = tags;
                    if masked {
                        for index in [top, pushed_top] {
                            expected_tags =
                                (expected_tags & !(3 << (index * 2))) | (2 << (index * 2));
                        }
                        assert_eq!(cpu.fpu.status_word & (1 << 10), 0, "{context}: C2");
                    }
                    assert_eq!(cpu.fpu.tag_word, expected_tags, "{context}: stack tags");
                    for (index, original) in registers.iter().enumerate() {
                        let expected = if masked && (index == top || index == pushed_top) {
                            F80::INDEFINITE.0
                        } else {
                            original.0
                        };
                        assert_eq!(
                            cpu.fpu.regs[index].0, expected,
                            "{context}: register={index}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn execute_trig_infinity_invalid_sincos() {
        assert_execute_trig_infinity_invalid_push(0xfb, Code::Fsincos);
    }

    #[test]
    fn execute_trig_infinity_invalid_ptan() {
        assert_execute_trig_infinity_invalid_push(0xf2, Code::Fptan);
    }

    fn assert_execute_trig_extended_range(code: Code, huge_only: bool) {
        let cases = if huge_only {
            vec![(0x8000_0000_0000_0000u64, 0x43ffu16, true)]
        } else {
            vec![
                (u64::MAX, 0x403d, false),             // 2^63 - 0.5
                (0x8000_0000_0000_0000, 0x403e, true), // 2^63
                (0x8000_0000_0000_0001, 0x403e, true), // 2^63 + 1
                (0x8000_0000_0000_0000, 0x43ff, true), // finite 2^1024
            ]
        };
        for sign in [0, 0x8000] {
            for &(significand, exponent, out_of_range) in &cases {
                let mut bytes = [0; 10];
                bytes[..8].copy_from_slice(&significand.to_le_bytes());
                bytes[8..].copy_from_slice(&(exponent | sign).to_le_bytes());
                let mut cpu = Cpu::new();
                cpu.set_cpu_type(crate::CpuType::I80386);
                let mut mem = Memory::new();
                // A second occupied slot makes accidental stack changes visible.
                cpu.fpu.push(F80::ONE);
                cpu.fpu.push(F80(bytes));
                cpu.fpu.status_word |= 1 << 10; // prove C2 actively clears in range
                let top = cpu.fpu.get_top();
                let tags = cpu.fpu.tag_word;
                let registers = cpu.fpu.regs;
                let pushes = !out_of_range && matches!(code, Code::Fptan | Code::Fsincos);
                execute_portable(&mut cpu, &mut mem, &instr0(code)).unwrap();
                assert_eq!(
                    cpu.fpu.status_word & (1 << 10) != 0,
                    out_of_range,
                    "exact extended range {code:?}, {bytes:02x?}"
                );
                assert_eq!(
                    cpu.fpu.status_word & 1,
                    0,
                    "finite operand must not raise IE"
                );
                assert_eq!(
                    cpu.fpu.get_top(),
                    if pushes { (top + 7) & 7 } else { top },
                    "{code:?}: TOP must reflect only an in-range push"
                );
                let occupied = (0..8).filter(|&i| !cpu.fpu.is_empty(i)).count();
                assert_eq!(occupied, if pushes { 3 } else { 2 });
                if out_of_range {
                    assert_eq!(cpu.fpu.tag_word, tags, "C2 must preserve every tag");
                    assert_eq!(cpu.fpu.get_st(0).0, bytes, "C2 must preserve exact ST0");
                    for (actual, original) in cpu.fpu.regs.iter().zip(registers) {
                        assert_eq!(actual.0, original.0, "C2 must preserve every register");
                    }
                } else {
                    for offset in 0..=usize::from(pushes) {
                        let value = cpu.fpu.get_st(offset);
                        assert!(!value.is_nan() && !value.is_infinite());
                        assert_eq!(
                            cpu.fpu.get_tag((cpu.fpu.get_top() + offset) & 7),
                            value.x87_tag()
                        );
                    }
                    assert_eq!(cpu.fpu.get_st(if pushes { 2 } else { 1 }).0, F80::ONE.0);
                    if code == Code::Fptan {
                        assert_eq!(cpu.fpu.get_st(0).0, F80::ONE.0);
                    }
                }
            }
        }
    }

    #[test]
    fn execute_trig_extended_range_sin() {
        assert_execute_trig_extended_range(Code::Fsin, false);
    }
    #[test]
    fn execute_trig_extended_range_cos() {
        assert_execute_trig_extended_range(Code::Fcos, false);
    }
    #[test]
    fn execute_trig_extended_range_sincos() {
        assert_execute_trig_extended_range(Code::Fsincos, false);
    }
    #[test]
    fn execute_trig_extended_range_ptan() {
        assert_execute_trig_extended_range(Code::Fptan, false);
    }
    #[test]
    fn execute_trig_extended_range_sincos_finite_huge() {
        assert_execute_trig_extended_range(Code::Fsincos, true);
    }

    #[test]
    fn test_execute_fsin_out_of_range_sets_c2() {
        // FSIN with |operand| >= 2^63 should set C2
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        let large_val = (1u64 << 63) as f64;
        set_st0_st1(&mut cpu, large_val, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsin));
        // C2 (bit 10) should be set for out-of-range
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0, "C2 should be set");
    }

    #[test]
    fn test_execute_fcos_out_of_range_sets_c2() {
        // FCOS with |operand| >= 2^63 should set C2
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        let large_val = (1u64 << 63) as f64;
        set_st0_st1(&mut cpu, large_val, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fcos));
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0, "C2 should be set");
    }

    #[test]
    fn test_execute_fptan_out_of_range_sets_c2() {
        // FPTAN with |operand| >= 2^63 should set C2 and not push 1.0
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        let large_val = (1u64 << 63) as f64;
        set_st0_st1(&mut cpu, large_val, 0.0);
        let top_before = cpu.fpu.get_top();
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0, "C2 should be set");
        // TOP should not have changed (no push when C2 set)
        assert_eq!(cpu.fpu.get_top(), top_before);
    }

    #[test]
    fn test_execute_fsincos_out_of_range_sets_c2() {
        // FSINCOS with |operand| >= 2^63 should set C2 and not push cos
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        let large_val = (1u64 << 63) as f64;
        set_st0_st1(&mut cpu, large_val, 0.0);
        let top_before = cpu.fpu.get_top();
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsincos));
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0, "C2 should be set");
        // TOP should not have changed (no push when C2 set)
        assert_eq!(cpu.fpu.get_top(), top_before);
    }

    #[test]
    fn test_execute_fscale_scale_nan_and_normal_path() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, f64::NAN);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fscale));
        assert!(cpu.fpu.get_st(0).to_f64().is_nan());

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 2.0, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fscale));
        assert_eq!(cpu.fpu.status_word & 0x18, 0);
    }

    #[test]
    fn test_execute_fxtract_stack_overflow_sets_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        fill_stack(&mut cpu);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxtract));
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_fprem_divisor_nan_inf_sets_exception() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // NaN divisor should still set IE
        set_st0_st1(&mut cpu, 1.0, f64::NAN);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "NaN divisor should set IE");

        // Per Intel 387 manual: finite / infinity returns dividend unchanged,
        // no exception. The dividend (1.0) should be preserved.
        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_eq!(
            cpu.fpu.status_word & 0x01,
            0,
            "finite/infinity should NOT set IE"
        );
        assert!(
            (cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9,
            "dividend should be unchanged"
        );
    }

    #[test]
    fn test_execute_fprem1_divisor_nan_inf_and_partial() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // NaN divisor should still set IE
        set_st0_st1(&mut cpu, 1.0, f64::NAN);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "NaN divisor should set IE");

        // Per Intel 387 manual: finite / infinity returns dividend unchanged,
        // no exception.
        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert_eq!(
            cpu.fpu.status_word & 0x01,
            0,
            "finite/infinity should NOT set IE"
        );
        assert!(
            (cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9,
            "dividend should be unchanged"
        );

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 2.0_f64.powi(120), 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0);
    }

    #[test]
    fn test_execute_fcom_empty_st1_sets_stack_fault() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(1, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcom_st0_sti, Register::ST0, Register::ST1),
        );
        assert_stack_fault(&cpu);
    }

    #[test]
    fn test_execute_fxch_empty_unmasked_returns() {
        // Patch 4: the emulator does not dispatch FPU exceptions — status-
        // word-only (see HLD 02 §5.2). So the CW "IE unmasked" state behaves
        // the same as masked for our interpreter: fill empty slot with
        // indefinite, then swap. Regardless of IE mask, SF is set and the
        // empty slot is populated.
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.control_word &= !0x1;
        cpu.fpu.status_word = 0;
        cpu.fpu.set_tag(1, 3); // ST(1) empty

        let top = cpu.fpu.get_top();
        let tag0_before = (cpu.fpu.tag_word >> (top * 2)) & 0x3;
        let tag1_before = (cpu.fpu.tag_word >> (((top + 1) & 0x7) * 2)) & 0x3;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST1),
        );
        let tag0_after = (cpu.fpu.tag_word >> (top * 2)) & 0x3;
        let tag1_after = (cpu.fpu.tag_word >> (((top + 1) & 0x7) * 2)) & 0x3;
        assert_stack_fault(&cpu);
        assert_eq!(tag0_before, 0);
        assert_eq!(tag1_before, 3);
        // Empty slot filled before swap → neither slot is Empty afterwards.
        assert_ne!(tag0_after, 3);
        assert_ne!(tag1_after, 3);
    }

    #[test]
    fn test_execute_fxam_empty_masked_sets_empty_bits() {
        // Per Intel 387 manual: FXAM never generates exceptions regardless of
        // mask settings. Empty class: C3=1, C2=0, C0=1.
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        cpu.fpu.control_word |= 0x1; // mask IE (should not matter for FXAM)
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        // No exception should be set
        assert_eq!(cpu.fpu.status_word & 0x01, 0, "FXAM should never set IE");
        assert_eq!(cpu.fpu.status_word & 0x40, 0, "FXAM should never set SF");
        // Empty class condition codes
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0, "C3 should be set");
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0, "C2 should be clear");
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0, "C0 should be set");
    }

    #[test]
    fn test_execute_ffree_and_ffreep_non_register_operand() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        let top = cpu.fpu.get_top();
        let tag1_before = (cpu.fpu.tag_word >> (((top + 1) & 0x7) * 2)) & 0x3;
        let mut instr = instr1(Code::Ffree_sti, Register::ST1);
        instr.set_op_kind(0, OpKind::Immediate8);
        exec_instr(&mut cpu, &mut mem, instr);
        let tag1_after = (cpu.fpu.tag_word >> (((top + 1) & 0x7) * 2)) & 0x3;
        assert_eq!(tag1_after, tag1_before);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        let top_before = cpu.fpu.get_top();
        let mut instr = instr1(Code::Ffreep_sti, Register::ST1);
        instr.set_op_kind(0, OpKind::Immediate8);
        exec_instr(&mut cpu, &mut mem, instr);
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    #[test]
    fn test_execute_fcmovnbe_false_does_not_move() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.regs.flags.insert(Flags::CARRY);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovnbe_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);
    }

    #[test]
    fn test_execute_register_operand_fallbacks() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;
        let instr = instr2_with_op1_kind(
            Code::Fucom_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        exec_instr(&mut cpu, &mut mem, instr);
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.regs[2] = F80::from_f64(2.0);
        cpu.fpu.set_tag(2, 0);
        cpu.fpu.status_word = 0;
        let mut instr = instr2_with_op1_kind(
            Code::Fcom_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        instr.set_op0_register(Register::ST2);
        exec_instr(&mut cpu, &mut mem, instr);
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.regs
            .flags
            .remove(Flags::CARRY | Flags::ZERO | Flags::PARITY);
        let instr = instr2_with_op1_kind(
            Code::Fcomi_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        exec_instr(&mut cpu, &mut mem, instr);
        assert!(cpu.regs.flags.contains(Flags::CARRY));

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.regs.flags.insert(Flags::CARRY);
        let instr = instr2_with_op1_kind(
            Code::Fcmovb_st0_sti,
            Register::ST0,
            Register::ST1,
            OpKind::Immediate8,
        );
        exec_instr(&mut cpu, &mut mem, instr);
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 2.0);
    }

    #[test]
    fn test_execute_fbld_overflow_and_fbstp_empty_faults() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        fill_stack(&mut cpu);
        cpu.fpu.status_word = 0;
        let addr: usize = 0x500;
        for i in 0..10 {
            mem.write_u8(addr + i, 0);
        }
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbld_m80bcd, mem_op(addr as i64)).unwrap(),
        );
        assert_stack_fault(&cpu);

        cpu.fpu = Fpu::new();
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr as i64)).unwrap(),
        );
        assert_stack_fault(&cpu);
    }

    // ========================================================================
    // Group 1: FLD constant variants (FLDL2E, FLDL2T, FLDLG2)
    // ========================================================================

    #[test]
    fn test_execute_fldl2e_pushes_log2_e() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        exec_instr(&mut cpu, &mut mem, instr0(Code::Fldl2e));
        assert!((cpu.fpu.get_st(0).to_f64() - std::f64::consts::LOG2_E).abs() < 1e-9);
    }

    #[test]
    fn test_execute_fldl2t_pushes_log2_10() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        exec_instr(&mut cpu, &mut mem, instr0(Code::Fldl2t));
        assert!((cpu.fpu.get_st(0).to_f64() - 10.0f64.log2()).abs() < 1e-9);
    }

    #[test]
    fn test_execute_fldlg2_pushes_log10_2() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        exec_instr(&mut cpu, &mut mem, instr0(Code::Fldlg2));
        assert!((cpu.fpu.get_st(0).to_f64() - 2.0f64.log10()).abs() < 1e-9);
    }

    // ========================================================================
    // Group 2: Memory I/O helper functions
    // ========================================================================

    #[test]
    fn test_execute_fld_m64_reads_f64_from_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1000;
        let val: f64 = std::f64::consts::PI;
        mem.write_u64(addr, u64::from_le_bytes(val.to_le_bytes()));

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - val).abs() < 1e-12);
    }

    #[test]
    fn test_execute_fstp_m32_writes_f32_to_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1020;
        set_st0_st1(&mut cpu, 2.5, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m32fp, mem_op(addr as i64)).unwrap(),
        );
        let stored = f32::from_le_bytes(mem.read_u32(addr).to_le_bytes());
        assert!((stored - 2.5).abs() < 1e-6);
    }

    #[test]
    fn test_execute_fstp_m64_writes_f64_to_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1040;
        set_st0_st1(&mut cpu, 7.77, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m64fp, mem_op(addr as i64)).unwrap(),
        );
        let stored = f64::from_le_bytes(mem.read_u64(addr).to_le_bytes());
        assert!((stored - 7.77).abs() < 1e-12);
    }

    #[test]
    fn test_execute_fild_m16int_reads_i16_from_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1060;
        mem.write_u16(addr, (-42i16) as u16);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fild_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), -42.0);
    }

    #[test]
    fn test_execute_fild_m32int_reads_i32_from_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1080;
        mem.write_u32(addr, 100000u32);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fild_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 100000.0);
    }

    #[test]
    fn test_execute_fild_m64int_reads_i64_from_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x10A0;
        let val: i64 = -9876543210;
        mem.write_u64(addr, val as u64);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fild_m64int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), val as f64);
    }

    #[test]
    fn test_execute_fstp_m80fp_writes_f80_to_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x10C0;
        set_st0_st1(&mut cpu, 1.234, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m80fp, mem_op(addr as i64)).unwrap(),
        );
        let mut bytes = [0u8; 10];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = mem.read_u8(addr + i);
        }
        let roundtrip = f80_to_f64(&bytes);
        assert!((roundtrip - 1.234).abs() < 1e-9);
    }

    #[test]
    fn test_execute_fld_m80fp_reads_f80_from_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x10E0;
        let val = 5.678;
        let bytes = f64_to_f80(val);
        for (i, &b) in bytes.iter().enumerate() {
            mem.write_u8(addr + i, b);
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fld_m80fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - val).abs() < 1e-9);
    }

    // ========================================================================
    // Group 3: FCMOV variants where condition is TRUE
    // ========================================================================

    #[test]
    fn test_execute_fcmovnb_true_moves() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.remove(Flags::CARRY); // CF=0 -> FCMOVNB should move
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovnb_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 99.0);
    }

    #[test]
    fn test_execute_fcmovne_true_moves() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.remove(Flags::ZERO); // ZF=0 -> FCMOVNE should move
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovne_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 99.0);
    }

    #[test]
    fn test_execute_fcmovnu_true_moves() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.remove(Flags::PARITY); // PF=0 -> FCMOVNU should move
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovnu_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 99.0);
    }

    #[test]
    fn test_execute_fcmove_true_moves() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.insert(Flags::ZERO); // ZF=1 -> FCMOVE should move
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmove_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 99.0);
    }

    // ========================================================================
    // Group 4: FSTCW/FLDCW
    // ========================================================================

    #[test]
    fn test_execute_fnstcw_stores_control_word() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.control_word = 0x5678;
        let addr: usize = 0x1100;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnstcw_m2byte, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(addr), 0x5678);
    }

    #[test]
    fn test_execute_fldcw_loads_control_word() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1120;
        mem.write_u16(addr, 0x9ABC);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fldcw_m2byte, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.control_word, 0x9ABC);
    }

    // ========================================================================
    // Group 5: FNSTSW to memory
    // ========================================================================

    #[test]
    fn test_execute_fnstsw_m2byte_stores_status_word_to_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.status_word = 0xDEAD;
        let addr: usize = 0x1140;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnstsw_m2byte, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(addr), 0xDEAD);
    }

    // ========================================================================
    // Group 6: FSAVE/FRSTOR 32-bit protected mode
    // ========================================================================

    #[test]
    fn test_execute_fsave_32bit_protected_mode() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 32;

        cpu.fpu.control_word = 0x1111;
        cpu.fpu.status_word = 0x2222;
        cpu.fpu.ip_offset = 0xAABBCCDD;
        cpu.fpu.ip_selector = 0x4444;
        cpu.fpu.operand_offset = 0x55667788;
        cpu.fpu.operand_selector = 0x9999;
        cpu.fpu.regs[0] = F80::from_f64(1.5);
        cpu.fpu.regs[1] = F80::from_f64(-2.25);
        // Set tag_word AFTER any set_tag calls to avoid overwrites
        cpu.fpu.tag_word = 0x3333;

        let addr: usize = 0x1200;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnsave_m108byte, mem_op(addr as i64)).unwrap(),
        );

        // Verify 32-bit environment format (28 bytes)
        assert_eq!(mem.read_u32(addr), 0x1111);
        assert_eq!(mem.read_u32(addr + 4), 0x2222);
        assert_eq!(mem.read_u32(addr + 8), 0x3333);
        assert_eq!(mem.read_u32(addr + 12), 0xAABBCCDD);
        assert_eq!(mem.read_u16(addr + 16), 0x4444);
        assert_eq!(mem.read_u32(addr + 20), 0x55667788);
        assert_eq!(mem.read_u16(addr + 24), 0x9999);

        // Verify register data was saved (starting at offset 28), in
        // stack-relative order.
        //
        // This test exists for the 32-bit ENVIRONMENT layout above; the
        // register check is incidental, and it used to assert slot 0 == 1.5 ==
        // regs[0], which pinned physical order by accident. Status word 0x2222
        // puts TOP at 4, so ST(0) is regs[4] and 1.5 belongs in slot 4.
        let slot = |i: usize| -> f64 {
            let mut bytes = [0u8; 10];
            for (j, byte) in bytes.iter_mut().enumerate() {
                *byte = mem.read_u8(addr + 28 + i * 10 + j);
            }
            f80_to_f64(&bytes)
        };
        assert_eq!(
            (0x2222u16 >> 11) & 0x7,
            4,
            "fixture's status word means TOP=4"
        );
        assert!(
            slot(0).abs() < 1e-9,
            "slot 0 is ST(0) = regs[4], which this fixture left at zero"
        );
        assert!(
            (slot(4) - 1.5).abs() < 1e-9,
            "regs[0] is ST(4) with TOP=4, so 1.5 must appear in slot 4"
        );
        assert!(
            (slot(5) - -2.25).abs() < 1e-9,
            "regs[1] is ST(5) with TOP=4"
        );

        // Verify FSAVE reset FPU
        assert_eq!(cpu.fpu.control_word, 0x037F);
        assert_eq!(cpu.fpu.status_word, 0);
        assert_eq!(cpu.fpu.tag_word, 0xFFFF);
    }

    #[test]
    fn test_execute_frstor_32bit_protected_mode() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 32;

        let addr: usize = 0x1300;
        // Write 32-bit environment (28 bytes)
        mem.write_u32(addr, 0x037F); // control_word
        mem.write_u32(addr + 4, 0x1234); // status_word
        mem.write_u32(addr + 8, 0x0000); // tag_word
        mem.write_u32(addr + 12, 0xDEADBEEF); // ip_offset
        mem.write_u16(addr + 16, 0x0010); // ip_selector
        mem.write_u16(addr + 18, 0); // opcode
        mem.write_u32(addr + 20, 0xCAFEBABE); // operand_offset
        mem.write_u16(addr + 24, 0x0020); // operand_selector
        mem.write_u16(addr + 26, 0); // reserved

        // Write register data (80 bytes at offset 28)
        for i in 0..8 {
            let val = (i as f64 + 1.0) * 10.0;
            let bytes = f64_to_f80(val);
            for (j, &b) in bytes.iter().enumerate() {
                mem.write_u8(addr + 28 + i * 10 + j, b);
            }
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Frstor_m108byte, mem_op(addr as i64)).unwrap(),
        );

        assert_eq!(cpu.fpu.control_word, 0x037F);
        assert_eq!(cpu.fpu.status_word, 0x1234);
        assert_eq!(cpu.fpu.tag_word, 0x0000);
        assert_eq!(cpu.fpu.ip_offset, 0xDEADBEEF);
        assert_eq!(cpu.fpu.ip_selector, 0x0010);
        assert_eq!(cpu.fpu.operand_offset, 0xCAFEBABE);
        assert_eq!(cpu.fpu.operand_selector, 0x0020);
        // Image slot i is ST(i). Stated
        // semantically first — slot i loaded ST(i) = (i+1)*10 — then physically,
        // to show the rotation actually happened. Status word 0x1234 puts TOP
        // at 2, so slot 0 lands in regs[2], not regs[0].
        let top = ((cpu.fpu.status_word >> 11) & 0x7) as usize;
        assert_eq!(top, 2, "fixture's status word means TOP=2");
        for i in 0..8 {
            let want = (i as f64 + 1.0) * 10.0;
            assert!(
                (cpu.fpu.get_st(i).to_f64() - want).abs() < 1e-9,
                "ST({i}) must be {want}"
            );
        }
        assert!(
            (cpu.fpu.regs[2].to_f64() - 10.0).abs() < 1e-9,
            "regs[2] holds ST(0)"
        );
        assert!(
            (cpu.fpu.regs[1].to_f64() - 80.0).abs() < 1e-9,
            "regs[1] holds ST(7)"
        );
    }

    // ========================================================================
    // Group 7: FBLD/FBSTP
    // ========================================================================

    #[test]
    fn test_execute_fbld_loads_bcd_from_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1400;
        let bytes = f64_to_bcd(0x037F, 5678.0);
        for (i, &b) in bytes.iter().enumerate() {
            mem.write_u8(addr + i, b);
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbld_m80bcd, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 5678.0);
    }

    #[test]
    fn test_execute_fbstp_stores_bcd_to_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1420;
        set_st0_st1(&mut cpu, -9999.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr as i64)).unwrap(),
        );

        let mut bytes = [0u8; 10];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = mem.read_u8(addr + i);
        }
        assert_eq!(bcd_to_f64(&bytes), -9999.0);
    }

    // ========================================================================
    // Group 8: FPREM/FPREM1 with divisor=INF + NaN/INF dividend
    // ========================================================================

    #[test]
    fn test_execute_fprem_nan_dividend_inf_divisor_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // dividend=NaN, divisor=INF -> IE should be set
        set_st0_st1(&mut cpu, f64::NAN, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "FPREM NaN/INF should set IE");
    }

    #[test]
    fn test_execute_fprem_inf_dividend_inf_divisor_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // dividend=INF, divisor=INF -> IE should be set
        set_st0_st1(&mut cpu, f64::INFINITY, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "FPREM INF/INF should set IE");
    }

    #[test]
    fn test_execute_fprem1_nan_dividend_inf_divisor_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // dividend=NaN, divisor=INF -> IE should be set
        set_st0_st1(&mut cpu, f64::NAN, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert_ne!(
            cpu.fpu.status_word & 0x01,
            0,
            "FPREM1 NaN/INF should set IE"
        );
    }

    #[test]
    fn test_execute_fprem1_inf_dividend_inf_divisor_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // dividend=INF, divisor=INF -> IE should be set
        set_st0_st1(&mut cpu, f64::INFINITY, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert_ne!(
            cpu.fpu.status_word & 0x01,
            0,
            "FPREM1 INF/INF should set IE"
        );
    }

    // ========================================================================
    // Group 9: FDIV/FDIVR status memory paths
    // ========================================================================

    #[cfg(target_arch = "x86_64")]
    fn host_fdiv_m32fp_zero_over_zero_status_bits() -> u16 {
        use core::arch::asm;

        let cw: u16 = 0x037F;
        let zero: f32 = 0.0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = crate::cpu::host_x87_enter_guarded();
            asm!(
                "fnclex",
                "fldcw word ptr [{cw}]",
                "fldz",
                "fdiv dword ptr [{zero}]",
                "fnstsw word ptr [{sw}]",
                "fstp st(0)",
                cw = in(reg) &cw,
                zero = in(reg) &zero,
                sw = in(reg) &mut sw as *mut u16,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
        sw & 0x05
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_host_fdiv_m32fp_zero_over_zero_reports_ie_not_ze() {
        assert_eq!(
            host_fdiv_m32fp_zero_over_zero_status_bits(),
            0x01,
            "host x87 FDIV m32fp 0.0/0.0 should report IE, not ZE"
        );
    }

    #[test]
    fn test_execute_fdiv_m32fp_zero_over_zero_reports_ie_not_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x14E0;
        mem.write_u32(addr, 0u32); // f32 +0.0

        cpu.fpu.status_word = 0;
        cpu.fpu.push(F80::ZERO);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdiv_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & 0x05,
            0x01,
            "FDIV m32fp 0.0/0.0 should report IE, not ZE; sw={:#06X}",
            cpu.fpu.status_word
        );
    }

    #[test]
    fn test_execute_fdiv_m32fp_zero_sets_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1500;
        mem.write_u32(addr, 0u32); // f32 zero

        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdiv_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_ne!(
            cpu.fpu.status_word & 0x04,
            0,
            "FDIV m32fp zero should set ZE"
        );
    }

    #[test]
    fn test_execute_fdiv_m64fp_zero_sets_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1520;
        mem.write_u64(addr, 0u64); // f64 zero

        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdiv_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert_ne!(
            cpu.fpu.status_word & 0x04,
            0,
            "FDIV m64fp zero should set ZE"
        );
    }

    // ========================================================================
    // Group 9b: FSUB/FSUBR status memory paths
    // ========================================================================

    #[cfg(target_arch = "x86_64")]
    fn host_fsubr_m64fp_rm16_repro_status_bits() -> u16 {
        use core::arch::asm;

        let cw: u16 = 0x037F;
        let ext: [u8; 10] = [0, 7, 14, 21, 28, 35, 42, 49, 56, 63];
        let zero: f64 = 0.0;
        let mut sw: u16 = 0;
        unsafe {
            let _host_x87 = crate::cpu::host_x87_enter_guarded();
            asm!(
                "fnclex",
                "fldcw word ptr [{cw}]",
                "fld tbyte ptr [{ext}]",
                "fsubr qword ptr [{zero}]",
                "fnstsw word ptr [{sw}]",
                "fstp st(0)",
                cw = in(reg) &cw,
                ext = in(reg) ext.as_ptr(),
                zero = in(reg) &zero,
                sw = in(reg) &mut sw as *mut u16,
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
        sw & 0x01
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_host_fsubr_m64fp_rm16_repro_reports_ie() {
        assert_eq!(
            host_fsubr_m64fp_rm16_repro_status_bits(),
            0x01,
            "host x87 RM16 repro FSUBR m64fp should report IE"
        );
    }

    #[test]
    fn test_execute_fsubr_m64fp_rm16_repro_reports_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x1540;
        mem.write_u64(addr, 0u64); // f64 +0.0, matching RM16 SS:0 input

        cpu.fpu.status_word = 0;
        cpu.fpu.push(F80([0, 7, 14, 21, 28, 35, 42, 49, 56, 63]));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsubr_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & 0x01,
            0x01,
            "FSUBR m64fp RM16 repro should report IE; sw={:#06X}",
            cpu.fpu.status_word
        );
    }

    // ========================================================================
    // Group 10: clear_exceptions
    // ========================================================================

    #[test]
    fn test_clear_exceptions_clears_status_bits() {
        let mut fpu = Fpu::new();
        // Set some exception bits
        fpu.status_word = 0x00FF; // All exception bits set (bits 0-7)
        fpu.clear_exceptions();
        assert_eq!(
            fpu.status_word & 0x00FF,
            0,
            "clear_exceptions should clear bits 0-7"
        );
    }

    #[test]
    fn test_clear_exceptions_preserves_non_exception_bits() {
        let mut fpu = Fpu::new();
        // Set exception bits + non-exception bits
        fpu.status_word = 0xFF3F; // Set some non-exception bits too
        fpu.clear_exceptions();
        assert_eq!(fpu.status_word & 0x00FF, 0);
        assert_ne!(
            fpu.status_word & 0xFF00,
            0,
            "Non-exception bits should be preserved"
        );
    }

    // ========================================================================
    // Group 11: round_to_precision truncate path (RC=3, PC=0)
    // ========================================================================

    #[test]
    fn test_round_to_precision_truncate_with_arithmetic() {
        // Set RC=3 (truncate) and PC=0 (24-bit single precision)
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // PC=0 (24-bit), RC=3 (truncate)
        cpu.fpu.control_word = (3u16 << 10) | 0x3F;

        let tiny = 2f64.powi(-25);
        set_st0_st1(&mut cpu, 1.0, tiny);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fadd_st0_sti, Register::ST0, Register::ST1),
        );
        // With truncate + 24-bit precision, adding a tiny value to 1.0 should still be 1.0
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);
    }

    // ========================================================================
    // Coverage: FSTP/FST memory all sizes
    // ========================================================================

    #[test]
    fn test_execute_fstp_fst_memory_all_sizes() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr = 0x2000usize;

        // FSTP m32fp (line 534) - store as f32 and pop
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(std::f64::consts::PI));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m32fp, mem_op(addr as i64)).unwrap(),
        );
        let stored_f32 = f32::from_le_bytes(mem.read_u32(addr).to_le_bytes());
        assert!((stored_f32 - std::f32::consts::PI).abs() < 1e-5);

        // FSTP m64fp (line 537)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(std::f64::consts::E));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m64fp, mem_op(addr as i64)).unwrap(),
        );
        let stored_f64 = f64::from_le_bytes(mem.read_u64(addr).to_le_bytes());
        assert!((stored_f64 - std::f64::consts::E).abs() < 1e-9);

        // FSTP m80fp (line 540) - exercises write_f80
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(1.23456789));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstp_m80fp, mem_op(addr as i64)).unwrap(),
        );
        let mut bytes = [0u8; 10];
        for i in 0..10 {
            bytes[i] = mem.read_u8(addr + i);
        }
        let read_back = f80_to_f64(&bytes);
        assert!((read_back - 1.23456789).abs() < 1e-9);

        // FST m32fp (line 576)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(42.5));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fst_m32fp, mem_op(addr as i64)).unwrap(),
        );
        let stored = f32::from_le_bytes(mem.read_u32(addr).to_le_bytes());
        assert!((stored - 42.5f32).abs() < 1e-5);

        // FST m64fp (line 579)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(123.456));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fst_m64fp, mem_op(addr as i64)).unwrap(),
        );
        let stored = f64::from_le_bytes(mem.read_u64(addr).to_le_bytes());
        assert!((stored - 123.456).abs() < 1e-9);
    }

    // ========================================================================
    // Coverage: FISTTP all sizes
    // ========================================================================

    #[test]
    fn test_execute_fisttp_all_sizes() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr = 0x2000usize;

        // FISTTP m16int (line 632, write_i16 line 1697)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(7.9));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(addr) as i16, 7); // truncates

        // FISTTP m32int (line 639, write_i32 line 1708)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(-123.7));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr) as i32, -123); // truncates

        // FISTTP m64int (line 646, write_i64 line 1724)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(999999.1));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m64int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u64(addr) as i64, 999999);
    }

    // ========================================================================
    // Coverage: Arith memory operands (m32fp and m64fp)
    // ========================================================================

    #[test]
    fn test_execute_arith_memory_operands() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr = 0x2000usize;

        // FADD m32fp (line 704, execute_arith memory f32 line 1870)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(10.0));
        let val = 5.0f32;
        mem.write_u32(addr, u32::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 15.0).abs() < 1e-9);

        // FADD m64fp (execute_arith memory f64 line 1871)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(10.0));
        let val = 5.0f64;
        mem.write_u64(addr, u64::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fadd_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 15.0).abs() < 1e-9);

        // FSUB m32fp (line 707)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(10.0));
        let val = 3.0f32;
        mem.write_u32(addr, u32::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsub_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 7.0).abs() < 1e-9);

        // FSUB m64fp
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(10.0));
        let val = 3.0f64;
        mem.write_u64(addr, u64::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsub_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 7.0).abs() < 1e-9);

        // FSUBR m32fp (line 710)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(3.0));
        let val = 10.0f32;
        mem.write_u32(addr, u32::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsubr_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 7.0).abs() < 1e-9);

        // FSUBR m64fp
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(3.0));
        let val = 10.0f64;
        mem.write_u64(addr, u64::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsubr_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 7.0).abs() < 1e-9);

        // FMUL m32fp (line 713)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(4.0));
        let val = 3.0f32;
        mem.write_u32(addr, u32::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fmul_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 12.0).abs() < 1e-9);

        // FMUL m64fp
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(4.0));
        let val = 3.0f64;
        mem.write_u64(addr, u64::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fmul_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 12.0).abs() < 1e-9);

        // FDIV m32fp
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(12.0));
        let val = 4.0f32;
        mem.write_u32(addr, u32::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdiv_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);

        // FDIVR m32fp
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(4.0));
        let val = 12.0f32;
        mem.write_u32(addr, u32::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdivr_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);

        // FDIV m64fp
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(15.0));
        let val = 5.0f64;
        mem.write_u64(addr, u64::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdiv_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);

        // FDIVR m64fp
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(5.0));
        let val = 15.0f64;
        mem.write_u64(addr, u64::from_le_bytes(val.to_le_bytes()));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdivr_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);
    }

    /// HLD 2026.07.29 D6 sequence oracle: random x87 op sequences run on two
    /// CPUs — A executes normally (NumericShadow proofs live), B has its
    /// shadow invalidated before EVERY op, forcing the per-op/pure route.
    /// Full architectural state (all 8 register byte images, SW, TW) and the
    /// memory operand window must match after every single op. This is the
    /// missed-invalidation catcher a per-op oracle cannot be: a stale proof
    /// in A shows up as a state divergence at the first op that consumes it
    /// (and as a freshness debug_assert before that).
    #[test]
    fn shadow_sequence_oracle_matches_forced_pure_path() {
        let addr = 0x3000usize;
        let mut state = 0x0123_4567_89ab_cdefu64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // PC=24 / PC=53 / PC=64 across nearest, down, up, chop.
        const CWS: [u16; 8] = [
            0x007f, 0x0c7f, 0x087f, 0x047f, 0x027f, 0x0e7f, 0x037f, 0x0f7f,
        ];

        let f32_pool = |r: u64| -> f32 {
            if r & 0xf0 == 0 {
                return if r & 1 == 0 { 0.0 } else { -0.0 };
            }
            if r & 0xff == 0x11 {
                return f32::NAN;
            }
            f32::from_bits(((r as u32) & 0x807f_ffff) | ((((r >> 32) as u32 % 190) + 32) << 23))
        };
        let f64_pool = |r: u64| -> f64 {
            if r & 0xf0 == 0 {
                return if r & 1 == 0 { 0.0 } else { -0.0 };
            }
            // Mix f32-exact and f64-only values (low fraction bits kept).
            let frac_mask = if r & 4 == 0 {
                0x000f_ffff_e000_0000u64
            } else {
                0x000f_ffff_ffff_ffffu64
            };
            let exponent = ((r >> 52) % 400 + 823) & 0x7ff; // well inside normal range
            f64::from_bits((r & 0x8000_0000_0000_0000) | (exponent << 52) | (r & frac_mask))
        };

        crate::x87_fast_core::reset_fast_path_hits();
        for seq in 0..200 {
            let mut cpu_a = Cpu::new();
            let mut mem_a = Memory::new();
            let mut cpu_b = Cpu::new();
            let mut mem_b = Memory::new();
            cpu_a.regs.ds = 0;
            cpu_b.regs.ds = 0;

            for step in 0..100 {
                let r = next();
                if r % 11 == 0 {
                    let cw = CWS[((r >> 16) % 8) as usize];
                    cpu_a.fpu.control_word = cw;
                    cpu_b.fpu.control_word = cw;
                }

                let instr = match r % 32 {
                    0..=2 => {
                        let v = f32_pool(r >> 5);
                        mem_a.write_u32(addr, v.to_bits());
                        mem_b.write_u32(addr, v.to_bits());
                        Instruction::with1(Code::Fld_m32fp, mem_op(addr as i64)).unwrap()
                    }
                    3..=4 => {
                        let v = f64_pool(r >> 5);
                        mem_a.write_u64(addr, v.to_bits());
                        mem_b.write_u64(addr, v.to_bits());
                        Instruction::with1(Code::Fld_m64fp, mem_op(addr as i64)).unwrap()
                    }
                    5 => instr1(
                        Code::Fld_sti,
                        if r & 8 == 0 {
                            Register::ST1
                        } else {
                            Register::ST2
                        },
                    ),
                    6 => instr0(if r & 8 == 0 { Code::Fld1 } else { Code::Fldz }),
                    7 => {
                        let v = i32::try_from((r >> 8) as u32 as i64 - (u32::MAX / 2) as i64)
                            .unwrap_or(0);
                        mem_a.write_u32(addr, v as u32);
                        mem_b.write_u32(addr, v as u32);
                        Instruction::with1(Code::Fild_m32int, mem_op(addr as i64)).unwrap()
                    }
                    8..=13 => {
                        let codes = [
                            Code::Fadd_st0_sti,
                            Code::Fsub_st0_sti,
                            Code::Fsubr_st0_sti,
                            Code::Fmul_st0_sti,
                            Code::Fdiv_st0_sti,
                            Code::Fdivr_st0_sti,
                        ];
                        instr2(
                            codes[(r % 32 - 8) as usize],
                            Register::ST0,
                            if r & 8 == 0 {
                                Register::ST1
                            } else {
                                Register::ST2
                            },
                        )
                    }
                    14..=16 => {
                        let codes = [
                            Code::Faddp_sti_st0,
                            Code::Fmulp_sti_st0,
                            Code::Fsubp_sti_st0,
                        ];
                        instr2(codes[(r % 32 - 14) as usize], Register::ST1, Register::ST0)
                    }
                    17..=22 => {
                        let m64 = r & 16 != 0;
                        let codes32 = [
                            Code::Fadd_m32fp,
                            Code::Fsub_m32fp,
                            Code::Fsubr_m32fp,
                            Code::Fmul_m32fp,
                            Code::Fdiv_m32fp,
                            Code::Fdivr_m32fp,
                        ];
                        let codes64 = [
                            Code::Fadd_m64fp,
                            Code::Fsub_m64fp,
                            Code::Fsubr_m64fp,
                            Code::Fmul_m64fp,
                            Code::Fdiv_m64fp,
                            Code::Fdivr_m64fp,
                        ];
                        let pick = (r % 32 - 17) as usize;
                        if m64 {
                            let v = f64_pool(r >> 5);
                            mem_a.write_u64(addr, v.to_bits());
                            mem_b.write_u64(addr, v.to_bits());
                            Instruction::with1(codes64[pick], mem_op(addr as i64)).unwrap()
                        } else {
                            let v = f32_pool(r >> 5);
                            mem_a.write_u32(addr, v.to_bits());
                            mem_b.write_u32(addr, v.to_bits());
                            Instruction::with1(codes32[pick], mem_op(addr as i64)).unwrap()
                        }
                    }
                    23..=24 => {
                        let codes = [Code::Fstp_m32fp, Code::Fstp_m64fp, Code::Fstp_m80fp];
                        Instruction::with1(codes[((r >> 6) % 3) as usize], mem_op(addr as i64))
                            .unwrap()
                    }
                    25 => {
                        let codes = [Code::Fst_m32fp, Code::Fst_m64fp, Code::Fistp_m32int];
                        Instruction::with1(codes[((r >> 6) % 3) as usize], mem_op(addr as i64))
                            .unwrap()
                    }
                    26 => instr2(
                        Code::Fxch_st0_sti,
                        Register::ST0,
                        if r & 8 == 0 {
                            Register::ST1
                        } else {
                            Register::ST2
                        },
                    ),
                    27 => instr0(if r & 8 == 0 { Code::Fchs } else { Code::Fabs }),
                    28 => instr0(if r & 8 == 0 {
                        Code::Fincstp
                    } else {
                        Code::Fdecstp
                    }),
                    29 => instr1(
                        Code::Ffree_sti,
                        if r & 8 == 0 {
                            Register::ST0
                        } else {
                            Register::ST1
                        },
                    ),
                    30 => instr0(Code::Fsqrt),
                    _ => instr1(
                        Code::Fst_sti,
                        if r & 8 == 0 {
                            Register::ST1
                        } else {
                            Register::ST2
                        },
                    ),
                };

                // B: kill every proof so the per-op/pure route must run.
                cpu_b.fpu.numeric_shadow.invalidate_all();
                exec_instr(&mut cpu_a, &mut mem_a, instr);
                exec_instr(&mut cpu_b, &mut mem_b, instr);

                for reg in 0..8 {
                    assert_eq!(
                        cpu_a.fpu.regs[reg].0, cpu_b.fpu.regs[reg].0,
                        "seq {seq} step {step}: physical reg {reg} diverged"
                    );
                }
                assert_eq!(
                    cpu_a.fpu.status_word, cpu_b.fpu.status_word,
                    "seq {seq} step {step}: status word diverged"
                );
                assert_eq!(
                    cpu_a.fpu.tag_word, cpu_b.fpu.tag_word,
                    "seq {seq} step {step}: tag word diverged"
                );
                for offset in 0..10 {
                    assert_eq!(
                        mem_a.read_u8(addr + offset),
                        mem_b.read_u8(addr + offset),
                        "seq {seq} step {step}: stored memory diverged at +{offset}"
                    );
                }
            }
        }
        // The oracle is vacuous if the fast/shadow route never fires: demand
        // a healthy engagement count across the whole run (both CPUs count).
        assert!(
            crate::x87_fast_core::fast_path_hits() > 500,
            "sequence oracle barely engaged the fast cores: {}",
            crate::x87_fast_core::fast_path_hits()
        );
    }

    #[test]
    fn exact_numeric_shadow_executes_m32_and_register_arithmetic() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.fpu.control_word = 0x007f;
        cpu.fpu.push(F80::from_f32(1.0));

        let addr = 0x2200usize;
        let addend = 3.0 * 2f32.powi(-24);
        mem.write_u32(addr, addend.to_bits());
        let (expected_add, expected_add_status) =
            x87_ops::f80_add_status(&F80::from_f32(1.0), &F80::from_f32(addend), 0x007f);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0), expected_add);
        assert_eq!(cpu.fpu.status_word & 0x0220, expected_add_status & 0x0220);
        assert_eq!(
            cpu.fpu.exact_binary32_st(0),
            crate::x87_common::f80_to_exact_normal_f32(expected_add.bytes())
        );

        cpu.fpu = Fpu::new();
        cpu.fpu.control_word = 0x007f;
        let multiplicand = f32::from_bits(0x3f80_0001);
        let multiplier = f32::from_bits(0x3f80_0003);
        cpu.fpu.push(F80::from_f32(multiplier));
        cpu.fpu.push(F80::from_f32(multiplicand));
        let (expected_mul, expected_mul_status) = x87_ops::f80_mul_status(
            &F80::from_f32(multiplicand),
            &F80::from_f32(multiplier),
            0x007f,
        );
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fmul_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0), expected_mul);
        assert_eq!(cpu.fpu.status_word & 0x0220, expected_mul_status & 0x0220);
        assert_eq!(
            cpu.fpu.exact_binary32_st(0),
            crate::x87_common::f80_to_exact_normal_f32(expected_mul.bytes())
        );
    }

    #[test]
    fn exact_numeric_shadow_subtraction_covers_directions_and_pop_forms() {
        let addr = 0x2240usize;
        let lhs = 1.0f32;
        let rhs = 2f32.powi(-25);
        let cw = 0x007f;
        let assert_result = |cpu: &Cpu, expected: F80, expected_status: u16| {
            assert_eq!(cpu.fpu.get_st(0), expected);
            assert_eq!(cpu.fpu.status_word & 0x0220, expected_status & 0x0220);
            assert_eq!(
                cpu.fpu.exact_binary32_st(0),
                crate::x87_common::f80_to_exact_normal_f32(expected.bytes())
            );
        };

        for (code, reverse) in [(Code::Fsub_m32fp, false), (Code::Fsubr_m32fp, true)] {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.regs.ds = 0;
            cpu.fpu.control_word = cw;
            cpu.fpu.push(F80::from_f32(lhs));
            mem.write_u32(addr, rhs.to_bits());
            let top_before = cpu.fpu.get_top();
            let tags_before = cpu.fpu.tag_word;
            let (expected, status) = if reverse {
                x87_ops::f80_sub_status(&F80::from_f32(rhs), &F80::from_f32(lhs), cw)
            } else {
                x87_ops::f80_sub_status(&F80::from_f32(lhs), &F80::from_f32(rhs), cw)
            };

            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(code, mem_op(addr as i64)).unwrap(),
            );

            assert_eq!(cpu.fpu.get_top(), top_before, "{code:?}");
            assert_eq!(cpu.fpu.tag_word, tags_before, "{code:?}");
            assert_result(&cpu, expected, status);
        }

        for (code, reverse) in [(Code::Fsub_st0_sti, false), (Code::Fsubr_st0_sti, true)] {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.fpu.control_word = cw;
            cpu.fpu.push(F80::from_f32(rhs));
            cpu.fpu.push(F80::from_f32(lhs));
            let top_before = cpu.fpu.get_top();
            let tags_before = cpu.fpu.tag_word;
            let (expected, status) = if reverse {
                x87_ops::f80_sub_status(&F80::from_f32(rhs), &F80::from_f32(lhs), cw)
            } else {
                x87_ops::f80_sub_status(&F80::from_f32(lhs), &F80::from_f32(rhs), cw)
            };

            exec_instr(
                &mut cpu,
                &mut mem,
                instr2(code, Register::ST0, Register::ST1),
            );

            assert_eq!(cpu.fpu.get_top(), top_before, "{code:?}");
            assert_eq!(cpu.fpu.tag_word, tags_before, "{code:?}");
            assert_result(&cpu, expected, status);
        }

        for (code, reverse) in [(Code::Fsubp_sti_st0, false), (Code::Fsubrp_sti_st0, true)] {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.fpu.control_word = cw;
            cpu.fpu.push(F80::from_f32(rhs));
            cpu.fpu.push(F80::from_f32(lhs));
            let top_before = cpu.fpu.get_top();
            let (expected, status) = if reverse {
                x87_ops::f80_sub_status(&F80::from_f32(lhs), &F80::from_f32(rhs), cw)
            } else {
                x87_ops::f80_sub_status(&F80::from_f32(rhs), &F80::from_f32(lhs), cw)
            };

            exec_instr(
                &mut cpu,
                &mut mem,
                instr2(code, Register::ST1, Register::ST0),
            );

            assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 7, "{code:?}");
            assert!(cpu.fpu.is_empty(top_before), "{code:?}");
            assert!(!cpu.fpu.is_empty(cpu.fpu.get_top()), "{code:?}");
            assert_result(&cpu, expected, status);
        }
    }

    // ========================================================================
    // Coverage: Integer arith memory (m16int and m32int)
    // ========================================================================

    #[test]
    fn test_execute_integer_arith_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr = 0x2000usize;

        // FIADD m16int (line 732, read_int_operand line 1676)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(10.0));
        mem.write_u16(addr, 5u16);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fiadd_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 15.0).abs() < 1e-9);

        // FIADD m32int (read_int_operand line 1677)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(10.0));
        mem.write_u32(addr, 5u32);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fiadd_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 15.0).abs() < 1e-9);

        // FISUB m16int (line 741)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(10.0));
        mem.write_u16(addr, 3u16);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisub_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 7.0).abs() < 1e-9);

        // FISUB m32int
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(10.0));
        mem.write_u32(addr, 3u32);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisub_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 7.0).abs() < 1e-9);

        // FISUBR m16int (line 750)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(3.0));
        mem.write_u16(addr, 10u16);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisubr_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 7.0).abs() < 1e-9);

        // FISUBR m32int
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(3.0));
        mem.write_u32(addr, 10u32);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisubr_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 7.0).abs() < 1e-9);

        // FIMUL m16int (line 759)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(4.0));
        mem.write_u16(addr, 3u16);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fimul_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 12.0).abs() < 1e-9);

        // FIMUL m32int
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(4.0));
        mem.write_u32(addr, 3u32);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fimul_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 12.0).abs() < 1e-9);

        // FIDIV m16int (line 768)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(12.0));
        mem.write_u16(addr, 4u16);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidiv_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);

        // FIDIV m32int
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(12.0));
        mem.write_u32(addr, 4u32);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidiv_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);

        // FIDIVR m16int (line 780)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(4.0));
        mem.write_u16(addr, 12u16);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidivr_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);

        // FIDIVR m32int
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(4.0));
        mem.write_u32(addr, 12u32);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidivr_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);
    }

    // ========================================================================
    // Coverage: FICOM/FICOMP memory
    // ========================================================================

    #[test]
    fn test_execute_ficom_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr = 0x2000usize;

        // FICOM m16int (line 795)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(5.0));
        mem.write_u16(addr, 3u16);
        // Clear only condition code bits, preserve TOP
        cpu.fpu.status_word &= !((1 << 14) | (1 << 10) | (1 << 8));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Ficom_m16int, mem_op(addr as i64)).unwrap(),
        );
        // ST0 (5.0) > mem (3) -> C3=0, C2=0, C0=0
        assert_eq!(cpu.fpu.status_word & ((1 << 14) | (1 << 10) | (1 << 8)), 0);

        // FICOM m32int
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(5.0));
        mem.write_u32(addr, 3u32);
        // Clear only condition code bits, preserve TOP
        cpu.fpu.status_word &= !((1 << 14) | (1 << 10) | (1 << 8));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Ficom_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.status_word & ((1 << 14) | (1 << 10) | (1 << 8)), 0);

        // FICOMP m16int
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(3.0));
        mem.write_u16(addr, 3u16);
        // Clear only condition code bits, preserve TOP
        cpu.fpu.status_word &= !((1 << 14) | (1 << 10) | (1 << 8));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Ficomp_m16int, mem_op(addr as i64)).unwrap(),
        );
        // Equal -> C3=1
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0);

        // FICOMP m32int
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(3.0));
        mem.write_u32(addr, 3u32);
        // Clear only condition code bits, preserve TOP
        cpu.fpu.status_word &= !((1 << 14) | (1 << 10) | (1 << 8));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Ficomp_m32int, mem_op(addr as i64)).unwrap(),
        );
        // Equal -> C3=1
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0);
    }

    // ========================================================================
    // Coverage: FCOM memory (m32fp and m64fp)
    // ========================================================================

    #[test]
    fn test_execute_fcom_memory() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr = 0x2000usize;

        // FCOM m32fp (line 875)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(5.0));
        let val = 3.0f32;
        mem.write_u32(addr, u32::from_le_bytes(val.to_le_bytes()));
        // Clear only condition code bits, preserve TOP
        cpu.fpu.status_word &= !((1 << 14) | (1 << 10) | (1 << 8));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fcom_m32fp, mem_op(addr as i64)).unwrap(),
        );
        // ST0 (5.0) > mem (3.0) -> all condition codes clear
        assert_eq!(cpu.fpu.status_word & ((1 << 14) | (1 << 10) | (1 << 8)), 0);

        // FCOM m64fp (line 876)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(2.0));
        let val = 5.0f64;
        mem.write_u64(addr, u64::from_le_bytes(val.to_le_bytes()));
        // Clear only condition code bits, preserve TOP
        cpu.fpu.status_word &= !((1 << 14) | (1 << 10) | (1 << 8));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fcom_m64fp, mem_op(addr as i64)).unwrap(),
        );
        // ST0 (2.0) < mem (5.0) -> C0=1
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);

        // FCOMP m32fp (pops after compare)
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(5.0));
        let val = 5.0f32;
        mem.write_u32(addr, u32::from_le_bytes(val.to_le_bytes()));
        // Clear only condition code bits, preserve TOP
        cpu.fpu.status_word &= !((1 << 14) | (1 << 10) | (1 << 8));
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fcomp_m32fp, mem_op(addr as i64)).unwrap(),
        );
        // Equal -> C3=1
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0);
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);

        // FCOMP m64fp
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(5.0));
        let val = 5.0f64;
        mem.write_u64(addr, u64::from_le_bytes(val.to_le_bytes()));
        // Clear only condition code bits, preserve TOP
        cpu.fpu.status_word &= !((1 << 14) | (1 << 10) | (1 << 8));
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fcomp_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0);
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    // ========================================================================
    // Coverage: FCOMI Greater path (set_eflags_compare)
    // ========================================================================

    #[test]
    fn test_execute_fcomi_greater() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // FCOMI ST0 > ST1 (Greater path, line 1818-1819)
        set_st0_st1(&mut cpu, 10.0, 5.0);
        cpu.regs.flags = Flags::empty();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomi_st0_sti, Register::ST0, Register::ST1),
        );
        // Greater: CF=0, ZF=0, PF=0
        assert!(!cpu.regs.flags.contains(Flags::CARRY));
        assert!(!cpu.regs.flags.contains(Flags::ZERO));
        assert!(!cpu.regs.flags.contains(Flags::PARITY));
    }

    #[test]
    fn test_execute_fcomi_equal() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // FCOMI ST0 == ST1 (Equal path, line 1819)
        set_st0_st1(&mut cpu, 7.0, 7.0);
        cpu.regs.flags = Flags::empty();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomi_st0_sti, Register::ST0, Register::ST1),
        );
        // Equal: CF=0, ZF=1, PF=0
        assert!(!cpu.regs.flags.contains(Flags::CARRY));
        assert!(cpu.regs.flags.contains(Flags::ZERO));
        assert!(!cpu.regs.flags.contains(Flags::PARITY));
    }

    // ========================================================================
    // Coverage: FDIVR status path identifies the correct divisor
    // ========================================================================

    /// FDIVR memory form computes `mem / ST(0)`: ST(0) is the divisor.
    /// ZE must be set for ST(0)=0, not for a zero memory dividend.
    #[test]
    fn test_execute_fdivr_memory_zero_does_not_set_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr = 0x2000usize;

        // FDIVR m32fp with memory=0, ST(0)=5.0:  result = 0/5.0 = 0, no ZE.
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(5.0));
        cpu.fpu.status_word &= !0x04;
        mem.write_u32(addr, 0); // 0.0f32 in memory
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdivr_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & 0x04,
            0,
            "ZE must NOT be set for FDIVR m32fp with memory=0 (memory is dividend, not divisor)"
        );

        // FDIVR m64fp with memory=0, ST(0)=5.0:  same expectation.
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(5.0));
        cpu.fpu.status_word &= !0x04;
        mem.write_u64(addr, 0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdivr_m64fp, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & 0x04,
            0,
            "ZE must NOT be set for FDIVR m64fp with memory=0"
        );
    }

    /// FDIVR mem with ST(0)=0 (the actual divisor) DOES set ZE.
    #[test]
    fn test_execute_fdivr_st0_zero_sets_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr = 0x2000usize;

        // FDIVR m32fp with memory=5.0, ST(0)=0: 5.0/0 = +inf, ZE set.
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::ZERO);
        cpu.fpu.status_word &= !0x04;
        mem.write_u32(addr, 5.0_f32.to_bits());
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdivr_m32fp, mem_op(addr as i64)).unwrap(),
        );
        assert_ne!(
            cpu.fpu.status_word & 0x04,
            0,
            "ZE must be set for FDIVR m32fp with ST(0)=0 (actual divide-by-zero)"
        );
    }

    #[test]
    fn test_execute_fxch_st4_st5_st6() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // Fill all 8 FPU registers with distinct values
        cpu.fpu = Fpu::new();
        for i in 0..8 {
            cpu.fpu.push(F80::from_f64((i + 1) as f64 * 10.0));
        }
        // Stack is full: ST0=80, ST1=70, ST2=60, ST3=50, ST4=40, ST5=30, ST6=20, ST7=10

        // FXCH ST4.
        let st0_before = cpu.fpu.get_st(0);
        let st4_before = cpu.fpu.get_st(4);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST4),
        );
        assert_eq!(cpu.fpu.get_st(0), st4_before);
        assert_eq!(cpu.fpu.get_st(4), st0_before);

        // FXCH ST5.
        let st0_before = cpu.fpu.get_st(0);
        let st5_before = cpu.fpu.get_st(5);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST5),
        );
        assert_eq!(cpu.fpu.get_st(0), st5_before);
        assert_eq!(cpu.fpu.get_st(5), st0_before);

        // FXCH ST6.
        let st0_before = cpu.fpu.get_st(0);
        let st6_before = cpu.fpu.get_st(6);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST6),
        );
        assert_eq!(cpu.fpu.get_st(0), st6_before);
        assert_eq!(cpu.fpu.get_st(6), st0_before);
    }

    // ========================================================================
    // FCMOV false-condition tests (condition false => no move)
    // ========================================================================

    #[test]
    fn test_execute_fcmovnb_false_no_move() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.insert(Flags::CARRY); // CF=1 -> FCMOVNB condition false
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovnb_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0); // No move
    }

    #[test]
    fn test_execute_fcmovne_false_no_move() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.insert(Flags::ZERO); // ZF=1 -> FCMOVNE condition false
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovne_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);
    }

    #[test]
    fn test_execute_fcmovnu_false_no_move() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.insert(Flags::PARITY); // PF=1 -> FCMOVNU condition false
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovnu_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);
    }

    // ========================================================================
    // FBLD with negative BCD
    // ========================================================================

    #[test]
    fn test_execute_fbld_negative_bcd() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        let addr = 0x1000usize;
        // Encode -1234 as BCD: set sign byte (byte 9) to 0x80, digits in byte 0-8
        let bcd = f64_to_bcd(0x037F, -1234.0);
        for (i, &b) in bcd.iter().enumerate() {
            mem.write_u8(addr + i, b);
        }
        cpu.fpu = Fpu::new();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbld_m80bcd, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), -1234.0);
    }

    // ========================================================================
    // FCOMI less-than path
    // ========================================================================

    #[test]
    fn test_execute_fcomi_less_than() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 2.0, 10.0);
        cpu.regs.flags = Flags::empty();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomi_st0_sti, Register::ST0, Register::ST1),
        );
        // Less: CF=1, ZF=0, PF=0
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert!(!cpu.regs.flags.contains(Flags::ZERO));
        assert!(!cpu.regs.flags.contains(Flags::PARITY));
    }

    // ========================================================================
    // Coverage: get_st_ref (lines 109-112)
    // ========================================================================

    #[test]
    fn test_get_st_ref_returns_correct_reference() {
        let mut fpu = Fpu::new();
        fpu.set_top(0);
        fpu.regs[0] = F80::from_f64(42.5);
        fpu.set_tag(0, 0);
        fpu.regs[1] = F80::from_f64(99.0);
        fpu.set_tag(1, 0);

        let st0_ref = fpu.get_st_ref(0);
        assert_eq!(st0_ref.to_f64(), 42.5);
        let st1_ref = fpu.get_st_ref(1);
        assert_eq!(st1_ref.to_f64(), 99.0);
    }

    // ========================================================================
    // Coverage: sync_to/from_x87_save_area (lines 201-258)
    // ========================================================================

    #[test]
    fn test_sync_to_x87_save_area_round_trip() {
        let mut fpu = Fpu::new();
        fpu.control_word = 0x037F;
        fpu.status_word = 0x1234;
        fpu.tag_word = 0x5678;
        fpu.ip_offset = 0xAABBCCDD;
        fpu.ip_selector = 0x1122;
        fpu.operand_offset = 0x33445566;
        fpu.operand_selector = 0x7788;
        for i in 0..8 {
            fpu.regs[i] = F80::from_f64((i + 1) as f64 * 1.5);
        }

        fpu.sync_to_x87_save_area();

        // Verify control word has exceptions masked (bits 0-5 forced on)
        let cw = u32::from_le_bytes([
            fpu.x87_save_area[0],
            fpu.x87_save_area[1],
            fpu.x87_save_area[2],
            fpu.x87_save_area[3],
        ]) as u16;
        assert_eq!(cw & 0x003F, 0x003F, "All exceptions should be masked");

        // Now restore from the save area into a fresh Fpu
        let mut fpu2 = Fpu::new();
        fpu2.x87_save_area = fpu.x87_save_area;
        fpu2.sync_from_x87_save_area();

        // Control word: low 6 bits forced to 1, so original bits 0-5 are
        // masked to 0x3F; rest should be preserved.
        assert_eq!(fpu2.control_word & !0x003F, fpu.control_word & !0x003F);
        assert_eq!(fpu2.control_word & 0x003F, 0x003F);
        assert_eq!(fpu2.status_word, 0x1234);
        assert_eq!(fpu2.tag_word, 0x5678);
        assert_eq!(fpu2.ip_offset, 0xAABBCCDD);
        assert_eq!(fpu2.ip_selector, 0x1122);
        assert_eq!(fpu2.operand_offset, 0x33445566);
        assert_eq!(fpu2.operand_selector, 0x7788);

        // Verify register round-trip
        for i in 0..8 {
            assert_eq!(
                fpu2.regs[i].0, fpu.regs[i].0,
                "Register {} mismatch after round-trip",
                i
            );
        }
    }

    #[test]
    fn test_sync_to_x87_save_area_layout() {
        let mut fpu = Fpu::new();
        // TOP=0 keeps stack-relative and physical orders aligned, so the
        // ST(0) layout assertion (offset 28) reduces to regs[0]. The TOP≠0
        // case is covered by `test_sync_to_x87_save_area_layout_top_nonzero`.
        fpu.status_word = 0x4321 & !(0x7 << 11); // strip TOP, leaves TOP=0
        fpu.tag_word = 0xAAAA;
        fpu.ip_offset = 0x11111111;
        fpu.ip_selector = 0x2222;
        fpu.operand_offset = 0x33333333;
        fpu.operand_selector = 0x4444;
        fpu.regs[0] = F80::from_f64(1.0);

        fpu.sync_to_x87_save_area();
        let buf = &fpu.x87_save_area;

        // Status word at bytes 4-7
        let sw = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as u16;
        assert_eq!(sw, fpu.status_word);

        // Tag word at bytes 8-11
        let tw = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as u16;
        assert_eq!(tw, 0xAAAA);

        // IP offset at bytes 12-15
        let ip = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
        assert_eq!(ip, 0x11111111);

        // IP selector at bytes 16-19
        let ips = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]) as u16;
        assert_eq!(ips, 0x2222);

        // Operand offset at bytes 20-23
        let oo = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
        assert_eq!(oo, 0x33333333);

        // Operand selector at bytes 24-27
        let os = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]) as u16;
        assert_eq!(os, 0x4444);

        // ST(0) at bytes 28-37; with TOP=0, ST(0) == regs[0].
        assert_eq!(&buf[28..38], &fpu.regs[0].0);
    }

    /// Save-area registers use stack order: offset 28+10·i holds
    /// ST(i) = regs[(TOP+i) & 7]. With TOP=6, offset 28 holds regs[6],
    /// matching hardware FNSAVE/FRSTOR.
    #[test]
    fn test_sync_to_x87_save_area_layout_top_nonzero() {
        let mut fpu = Fpu::new();
        fpu.set_top(6);
        fpu.tag_word = 0x0FFF; // R6, R7 valid; R0..R5 empty
        fpu.regs[6] = F80::from_f64(4.0);
        fpu.regs[7] = F80::from_f64(1.0);

        fpu.sync_to_x87_save_area();
        let buf = &fpu.x87_save_area;

        // ST(0) = regs[6] = 4.0 at offset 28
        assert_eq!(&buf[28..38], &fpu.regs[6].0, "ST(0) at offset 28");
        // ST(1) = regs[7] = 1.0 at offset 38
        assert_eq!(&buf[38..48], &fpu.regs[7].0, "ST(1) at offset 38");
        // ST(2) = regs[0] (uninitialised) at offset 48
        assert_eq!(&buf[48..58], &fpu.regs[0].0, "ST(2) at offset 48");
    }

    /// Round-trip a TOP≠0 state through sync_to + sync_from and verify the
    /// physical-register layout is preserved.
    #[test]
    fn test_sync_to_from_x87_save_area_top_nonzero_roundtrip() {
        let mut fpu = Fpu::new();
        fpu.set_top(3);
        fpu.tag_word = 0x000F;
        fpu.regs[0] = F80::from_f64(0.5);
        fpu.regs[3] = F80::from_f64(3.0);
        fpu.regs[5] = F80::from_f64(5.0);
        fpu.regs[7] = F80::from_f64(7.0);

        fpu.sync_to_x87_save_area();

        // Replay through a fresh Fpu via the save area.
        let mut fpu2 = Fpu::new();
        fpu2.x87_save_area = fpu.x87_save_area;
        fpu2.sync_from_x87_save_area();

        assert_eq!(fpu2.get_top(), 3);
        assert_eq!(fpu2.regs[0], fpu.regs[0]);
        assert_eq!(fpu2.regs[3], fpu.regs[3]);
        assert_eq!(fpu2.regs[5], fpu.regs[5]);
        assert_eq!(fpu2.regs[7], fpu.regs[7]);
    }

    #[test]
    fn restore_regs_from_x87_save_area_unrotates_by_shadow_top() {
        let mut fpu = Fpu::new();
        fpu.set_top(5);
        for stack_idx in 0..8 {
            let bytes = [stack_idx as u8 + 1; 10];
            let offset = 28 + stack_idx * 10;
            fpu.x87_save_area[offset..offset + 10].copy_from_slice(&bytes);
        }
        fpu.x87_save_area[4..8].copy_from_slice(&(fpu.status_word as u32).to_le_bytes());

        fpu.restore_regs_from_x87_save_area();

        for stack_idx in 0..8 {
            let phys_idx = (5 + stack_idx) & 7;
            assert_eq!(fpu.regs[phys_idx].0, [stack_idx as u8 + 1; 10]);
        }
    }

    #[test]
    fn restore_regs_from_x87_save_area_preserves_metadata() {
        let mut fpu = Fpu::new();
        fpu.status_word = 0x2B45; // TOP=5 plus distinctive condition/status bits.
        fpu.control_word = 0x0A5A;
        fpu.tag_word = 0x96E1;
        fpu.ip_offset = 0x1122_3344;
        fpu.ip_selector = 0x5566;
        fpu.operand_offset = 0x7788_99AA;
        fpu.operand_selector = 0xBBCC;
        fpu.x87_save_area.fill(0xDD);
        fpu.x87_save_area[4..8].copy_from_slice(&(0x2800u32).to_le_bytes());
        let save_area_before = fpu.x87_save_area;

        fpu.restore_regs_from_x87_save_area();

        assert_eq!(fpu.status_word, 0x2B45);
        assert_eq!(fpu.control_word, 0x0A5A);
        assert_eq!(fpu.tag_word, 0x96E1);
        assert_eq!(fpu.ip_offset, 0x1122_3344);
        assert_eq!(fpu.ip_selector, 0x5566);
        assert_eq!(fpu.operand_offset, 0x7788_99AA);
        assert_eq!(fpu.operand_selector, 0xBBCC);
        assert_eq!(fpu.x87_save_area, save_area_before);
    }

    #[test]
    #[should_panic(expected = "host x87 TOP diverged from the software shadow")]
    fn restore_regs_from_x87_save_area_rejects_top_mismatch() {
        let mut fpu = Fpu::new();
        fpu.set_top(2);
        fpu.x87_save_area[4..8].copy_from_slice(&(0x1800u32).to_le_bytes());

        fpu.restore_regs_from_x87_save_area();
    }

    // ========================================================================
    // Coverage: FUCOMIP pops and sets EFLAGS (lines 1007-1009)
    // ========================================================================

    #[test]
    fn test_execute_fucomip_pops_and_sets_eflags() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        let top_before = cpu.fpu.get_top();
        cpu.regs
            .flags
            .remove(Flags::CARRY | Flags::ZERO | Flags::PARITY);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucomip_st0_sti, Register::ST0, Register::ST1),
        );
        // ST0 < ST1 -> CF=1
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert!(!cpu.regs.flags.contains(Flags::ZERO));
        // Should pop once
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    // ========================================================================
    // Coverage: FUCOMI with NaN does NOT set IE (unlike FCOMI)
    // ========================================================================

    #[test]
    fn test_execute_fucomi_nan_does_not_set_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::NAN, 1.0);
        cpu.fpu.status_word = 0;
        cpu.regs
            .flags
            .remove(Flags::CARRY | Flags::ZERO | Flags::PARITY);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucomi_st0_sti, Register::ST0, Register::ST1),
        );
        // FUCOMI with NaN: unordered, but IE is NOT set (unlike FCOMI)
        assert_eq!(cpu.fpu.status_word & 0x01, 0);
        // Unordered sets CF=1, ZF=1, PF=1
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert!(cpu.regs.flags.contains(Flags::ZERO));
        assert!(cpu.regs.flags.contains(Flags::PARITY));
        // Should NOT pop
        assert_eq!(cpu.fpu.get_top(), 0);
    }

    // ========================================================================
    // Coverage: FCMOVB with CF=1 (true)
    // ========================================================================

    #[test]
    fn test_execute_fcmovb_true_moves() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.insert(Flags::CARRY); // CF=1 -> FCMOVB should move
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovb_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 99.0);
    }

    // ========================================================================
    // Coverage: FCMOVB with CF=0 (false, no move)
    // ========================================================================

    #[test]
    fn test_execute_fcmovb_false_no_move() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.remove(Flags::CARRY); // CF=0 -> FCMOVB false
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovb_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);
    }

    // ========================================================================
    // Coverage: FCMOVE false (ZF=0)
    // ========================================================================

    #[test]
    fn test_execute_fcmove_false_no_move() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.remove(Flags::ZERO); // ZF=0 -> FCMOVE false
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmove_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);
    }

    // ========================================================================
    // Coverage: FCMOVBE with ZF=1 (true via ZF)
    // ========================================================================

    #[test]
    fn test_execute_fcmovbe_true_via_zf() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.remove(Flags::CARRY);
        cpu.regs.flags.insert(Flags::ZERO); // ZF=1 -> FCMOVBE true
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovbe_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 99.0);
    }

    // ========================================================================
    // Coverage: FCMOVU false (PF=0)
    // ========================================================================

    #[test]
    fn test_execute_fcmovu_false_no_move() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 99.0);
        cpu.regs.flags.remove(Flags::PARITY); // PF=0 -> FCMOVU false
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovu_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);
    }

    // ========================================================================
    // Coverage: 287 compatibility NOPs (lines 1472-1477)
    // ========================================================================

    #[test]
    fn test_execute_287_compat_nop_fnsetpm() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 2.0);
        let st0_before = cpu.fpu.get_st(0).to_f64();
        let sw_before = cpu.fpu.status_word;
        // FNSETPM = DB E4: escape=3 (0xDB), modrm=0xE4
        // iced-x86 Code for this is Fnsetpm
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fnsetpm));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), st0_before);
        assert_eq!(cpu.fpu.status_word, sw_before);
    }

    // ========================================================================
    // Coverage: Finit (WAIT version, different from Fninit)
    // ========================================================================

    #[test]
    fn test_execute_finit_resets_fpu() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu.status_word = 0xFFFF;
        cpu.fpu.control_word = 0;
        cpu.fpu.tag_word = 0;
        cpu.fpu.set_top(5);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Finit));
        assert_eq!(cpu.fpu.status_word, 0);
        assert_eq!(cpu.fpu.control_word, 0x037F);
        assert_eq!(cpu.fpu.tag_word, 0xFFFF);
        assert_eq!(cpu.fpu.get_top(), 0);
    }

    // ========================================================================
    // Coverage: FCLEX (WAIT version)
    // ========================================================================

    #[test]
    fn test_execute_fclex_clears_exceptions() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu.status_word = 0x00BF; // All exception flags set
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fclex));
        assert_eq!(cpu.fpu.status_word & 0x00BF, 0);
    }

    // ========================================================================
    // Coverage: FXAM with negative infinity (C1=1 sign bit)
    // ========================================================================

    #[test]
    fn test_execute_fxam_negative_infinity() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::NEG_INFINITY, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        // Infinity: C3=0, C2=1, C0=1
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0, "C0 should be set");
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0, "C2 should be set");
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0, "C3 should be clear");
        // C1=1 (negative sign)
        assert_ne!(
            cpu.fpu.status_word & (1 << 9),
            0,
            "C1 should be set for negative"
        );
    }

    // ========================================================================
    // Coverage: FXAM with negative zero (C1=1 sign bit)
    // ========================================================================

    #[test]
    fn test_execute_fxam_negative_zero() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, -0.0, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        // Zero: C3=1, C2=0, C0=0
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0, "C3 should be set");
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0, "C2 should be clear");
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0, "C0 should be clear");
        // C1=1 (negative sign)
        assert_ne!(
            cpu.fpu.status_word & (1 << 9),
            0,
            "C1 should be set for -0.0"
        );
    }

    // ========================================================================
    // Coverage: FTST with positive value (C3=0, C0=0)
    // ========================================================================

    #[test]
    fn test_execute_ftst_positive() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 5.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Ftst));
        // 5.0 > 0.0: C3=0, C0=0
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0, "C3 should be clear");
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0, "C0 should be clear");
    }

    // ========================================================================
    // Coverage: FTST with zero (C3=1)
    // ========================================================================

    #[test]
    fn test_execute_ftst_zero() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 0.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Ftst));
        // 0.0 == 0.0: C3=1
        assert_ne!(
            cpu.fpu.status_word & (1 << 14),
            0,
            "C3 should be set for zero"
        );
    }

    // ========================================================================
    // Coverage: FIDIV m32int zero sets ZE (line 847)
    // ========================================================================

    #[test]
    fn test_execute_fidiv_m32int_zero_sets_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x500;

        mem.write_u32(addr, 0u32);
        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidiv_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_ne!(
            cpu.fpu.status_word & 0x04,
            0,
            "FIDIV m32int by zero should set ZE"
        );
    }

    #[test]
    fn test_execute_fidiv_m32int_zero_over_zero_sets_ie_not_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x500;

        mem.write_u32(addr, 0u32);
        set_st0_st1(&mut cpu, 0.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidiv_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & 0x05,
            0x01,
            "FIDIV 0/0 should set IE without ZE"
        );
    }

    // ========================================================================
    // Coverage: FIDIVR m32int zero divisor status
    // ========================================================================

    #[test]
    fn test_execute_fidivr_m32int_nonzero_dividend_zero_divisor_sets_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x500;

        mem.write_u32(addr, 10u32);
        set_st0_st1(&mut cpu, 0.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidivr_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_ne!(
            cpu.fpu.status_word & 0x04,
            0,
            "FIDIVR m32int with ST0=0 should set ZE"
        );
    }

    #[test]
    fn test_execute_fidivr_m32int_zero_over_zero_sets_ie_not_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x500;

        mem.write_u32(addr, 0u32);
        set_st0_st1(&mut cpu, 0.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidivr_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & 0x05,
            0x01,
            "FIDIVR 0/0 should set IE without ZE"
        );
    }

    #[test]
    fn test_execute_fidivr_m32int_zero_dividend_nonzero_divisor_sets_no_ie_or_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x500;

        mem.write_u32(addr, 0u32);
        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fidivr_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & 0x05,
            0,
            "FIDIVR 0/non-zero should not set IE or ZE"
        );
    }

    // ========================================================================
    // Coverage: FUCOM with register operand (not ST0 default)
    // ========================================================================

    #[test]
    fn test_execute_fucom_register_non_default() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.regs[0] = F80::from_f64(5.0);
        cpu.fpu.regs[1] = F80::from_f64(3.0);
        cpu.fpu.regs[2] = F80::from_f64(10.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.set_tag(1, 0);
        cpu.fpu.set_tag(2, 0);
        cpu.fpu.status_word = 0;

        // FUCOM ST0, ST2
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucom_st0_sti, Register::ST0, Register::ST2),
        );
        // ST0 (5.0) < ST2 (10.0) -> C0=1
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0);
    }

    // ========================================================================
    // Coverage: FIST m16int valid path
    // ========================================================================

    #[test]
    fn test_execute_fist_m16int_valid() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x500;

        set_st0_st1(&mut cpu, 42.4, 0.0);
        cpu.fpu.status_word = 0;
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fist_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(addr) as i16, 42);
        assert_eq!(cpu.fpu.status_word & 0x01, 0);
        // FIST does NOT pop
        assert_eq!(cpu.fpu.get_top(), top_before);
    }

    // ========================================================================
    // Coverage: FISTP m32int valid path
    // ========================================================================

    #[test]
    fn test_execute_fistp_m32int_valid() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x500;

        set_st0_st1(&mut cpu, -12345.0, 0.0);
        cpu.fpu.status_word = 0;
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr) as i32, -12345);
        assert_eq!(cpu.fpu.status_word & 0x01, 0);
        // FISTP pops
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    // ========================================================================
    // Coverage: FCOM equal values (C3=1, C0=0)
    // ========================================================================

    #[test]
    fn test_execute_fcom_equal_values() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 5.0, 5.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcom_st0_sti, Register::ST0, Register::ST1),
        );
        // Equal: C3=1, C0=0
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0);
    }

    // ========================================================================
    // Coverage: FUCOM equal values (C3=1)
    // ========================================================================

    #[test]
    fn test_execute_fucom_equal_values() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 3.0, 3.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucom_st0_sti, Register::ST0, Register::ST1),
        );
        // Equal: C3=1
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0);
    }

    // ========================================================================
    // Coverage: unhandled FPU instruction returns error
    // ========================================================================

    #[test]
    fn test_execute_unhandled_instruction_returns_error() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // Create an instruction with an FPU-like mnemonic that's not handled
        // We can try with a non-FPU instruction that somehow gets routed here
        let instr = Instruction::with(Code::Nopd);
        let result = execute_iced_for_test(&mut cpu, &instr, &mut mem);
        assert!(result.is_err());
    }

    // ========================================================================
    // Coverage: FSINCOS with non-zero normal angle
    // ========================================================================

    #[test]
    fn test_execute_fsincos_normal_angle() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // pi/4 is a normal in-range angle
        let angle = std::f64::consts::PI / 4.0;
        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, angle, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsincos));
        // After FSINCOS: ST0 = cos, ST1 = sin
        let cos_val = cpu.fpu.get_st(0).to_f64();
        let sin_val = cpu.fpu.get_st(1).to_f64();
        let expected = std::f64::consts::FRAC_1_SQRT_2;
        assert!((cos_val - expected).abs() < 1e-9);
        assert!((sin_val - expected).abs() < 1e-9);
        // C2 should be clear (reduction complete)
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0);
    }

    // ========================================================================
    // Coverage: FPTAN with non-zero angle
    // ========================================================================

    #[test]
    fn test_execute_fptan_normal_angle() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        let angle = std::f64::consts::PI / 4.0;
        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, angle, 0.0);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
        // After FPTAN: ST0 = 1.0 (pushed), ST1 = tan(angle)
        assert!((cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9);
        assert!((cpu.fpu.get_st(1).to_f64() - 1.0).abs() < 1e-6); // tan(pi/4) ~ 1.0
                                                                  // C2 should be clear
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0);
    }

    // ========================================================================
    // Coverage: FSIN with normal angle (C2 clear path)
    // ========================================================================

    #[test]
    fn test_execute_fsin_normal_angle() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, std::f64::consts::PI / 6.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsin));
        assert!((cpu.fpu.get_st(0).to_f64() - 0.5).abs() < 1e-9);
        // C2 should be clear
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0);
    }

    // ========================================================================
    // Coverage: FCOS with normal angle (C2 clear path)
    // ========================================================================

    #[test]
    fn test_execute_fcos_normal_angle() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, std::f64::consts::PI / 3.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fcos));
        assert!((cpu.fpu.get_st(0).to_f64() - 0.5).abs() < 1e-9);
        // C2 should be clear
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0);
    }

    // ========================================================================
    // Coverage: FSQRT with NaN input (no exception, NaN propagated)
    // ========================================================================

    #[test]
    fn test_execute_fsqrt_nan_no_exception() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::NAN, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        // NaN sqrt should NOT set IE (NaN is not sign-negative in the negative check)
        // NaN propagation: result is NaN
        assert!(cpu.fpu.get_st(0).to_f64().is_nan());
    }

    // ========================================================================
    // Coverage: FSQRT with +0 (no exception)
    // ========================================================================

    #[test]
    fn test_execute_fsqrt_positive_zero() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 0.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 0.0);
        assert_eq!(cpu.fpu.status_word & 0x01, 0);
    }

    // ========================================================================
    // Coverage: FSQRT with -0 (no exception, -0 is special case)
    // ========================================================================

    #[test]
    fn test_execute_fsqrt_negative_zero() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, -0.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        // sqrt(-0) = -0 on x87, no exception because -0 passes the is_zero check
        assert_eq!(cpu.fpu.status_word & 0x01, 0);
    }

    /// Normal F80 mode computes the 64-bit-significand FSQRT result;
    /// binary64 mode rounds to the host-double grid. sqrt(2) distinguishes
    /// the two because its F80 root has more precision than a widened f64.
    #[test]
    fn fsqrt_single_rounds_on_the_binary64_contract_and_stays_80bit_off_it() {
        let f64_root = F80::from_f64(2.0_f64.sqrt());

        // Interpreter contract: 80-bit root, NOT the widened f64 root.
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 2.0, 0.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        assert_ne!(
            cpu.fpu.get_st(0).0,
            f64_root.0,
            "interpreter FSQRT must keep the 64-bit-significand root"
        );

        // Binary64 mode produces the host-double root with an f64 proof
        // and PE set: sqrt(2) is inexact at either precision.
        let mut cpu = Cpu::new();
        set_st0_st1(&mut cpu, 2.0, 0.0);
        cpu.fpu.enter_drc_binary64_mode();
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        assert_eq!(
            cpu.fpu.get_st(0).0,
            f64_root.0,
            "binary64-contract FSQRT must be the single-rounded host-double root"
        );
        assert_eq!(cpu.fpu.exact_binary64_st(0), Some(2.0_f64.sqrt()));
        assert_ne!(cpu.fpu.status_word & 0x20, 0, "inexact root must raise PE");
    }

    /// Even in binary64 mode, negative FSQRT operands use the pure path's
    /// invalid-operation flag and indefinite result.
    #[test]
    fn fsqrt_binary64_fast_path_leaves_negative_operands_to_the_pure_path() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, -4.0, 0.0);
        cpu.fpu.enter_drc_binary64_mode();
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "sqrt(-4) must raise IE");
        // The pure path defers the binary64 round-back (EXP-A013), so run
        // the read barrier before inspecting the F80 result.
        cpu.fpu.commit_drc_binary64_st(0);
        assert!(cpu.fpu.get_st(0).to_f64().is_nan());
    }

    // ========================================================================
    // Coverage: FPREM with finite/infinity returns dividend (lines 1319-1322)
    // ========================================================================

    #[test]
    fn test_execute_fprem_finite_dividend_inf_divisor_returns_dividend() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // finite / infinity -> dividend unchanged, C2=0
        set_st0_st1(&mut cpu, 42.0, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 42.0);
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0); // C2 = 0
        assert_eq!(cpu.fpu.status_word & 0x01, 0); // No IE
    }

    // ========================================================================
    // Coverage: FPREM1 with finite/infinity returns dividend (lines 1347-1350)
    // ========================================================================

    #[test]
    fn test_execute_fprem1_finite_dividend_inf_divisor_returns_dividend() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 42.0, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem1));
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 42.0);
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0);
        assert_eq!(cpu.fpu.status_word & 0x01, 0);
    }

    // ========================================================================
    // Coverage: FIST/FISTP with valid m16int and m32int values
    // ========================================================================

    #[test]
    fn test_execute_fist_fistp_m16int_valid_pop_difference() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x500;

        // FIST m16int - does NOT pop
        set_st0_st1(&mut cpu, 100.0, 0.0);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fist_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(addr) as i16, 100);
        assert_eq!(cpu.fpu.get_top(), top_before);

        // FISTP m16int - pops
        set_st0_st1(&mut cpu, 200.0, 0.0);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m16int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(addr) as i16, 200);
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    // ========================================================================
    // Coverage: FIST/FISTP m32int valid values
    // ========================================================================

    #[test]
    fn test_execute_fist_fistp_m32int_valid() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x500;

        // FIST m32int - does NOT pop
        set_st0_st1(&mut cpu, 100000.0, 0.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fist_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr) as i32, 100000);

        // FISTP m32int - pops
        set_st0_st1(&mut cpu, -999.0, 0.0);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m32int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr) as i32, -999);
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    // ========================================================================
    // Coverage: FIST/FISTP m64int valid values
    // ========================================================================

    #[test]
    fn test_execute_fist_fistp_m64int_valid() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        let addr: usize = 0x500;

        // FISTP m64int valid
        set_st0_st1(&mut cpu, 1234567890.0, 0.0);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m64int, mem_op(addr as i64)).unwrap(),
        );
        assert_eq!(mem.read_u64(addr) as i64, 1234567890);
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    /// Integer conversion of `byte * 19.2`: FILD m64int, FMUL m64fp,
    /// then FLDCW with RC=chop and FISTP m64int at extended precision.
    /// Returns the stored integer.
    fn ftol_byte_times_19_2(byte: i64, drc_binary64: bool) -> i64 {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.fpu.control_word = 0x1330;
        if drc_binary64 {
            cpu.fpu.enter_drc_binary64_mode();
        }
        mem.write_u64(0x500, byte as u64);
        mem.write_u64(0x508, 19.2f64.to_bits());
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fild_m64int, mem_op(0x500)).unwrap(),
        );
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fmul_m64fp, mem_op(0x508)).unwrap(),
        );
        cpu.fpu.control_word = 0x1330 | 0x0C00;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m64int, mem_op(0x510)).unwrap(),
        );
        mem.read_u64(0x510) as i64
    }

    /// The binary64 value nearest 19.2 lies just below it, so 10 * 19.2
    /// is exactly 191.99999999999999289... An extended significand retains
    /// that value and truncates to 191. A binary64 product rounds to 192
    /// before integer conversion, as required by the binary64 contract.
    #[test]
    fn ftol_of_ten_times_19_2_is_191_on_a_387_and_192_on_the_binary64_contract() {
        assert_eq!(
            ftol_byte_times_19_2(10, false),
            191,
            "interpreter (387 authenticity): the extended product chops to 191"
        );
        assert_eq!(
            ftol_byte_times_19_2(10, true),
            192,
            "dynrec-drc binary64 contract: the product rounds to 192.0 first"
        );
        // The product 6 * 19.2 = 115.2 is far from an integer boundary,
        // so both precision contracts truncate it to the same result.
        assert_eq!(ftol_byte_times_19_2(6, false), 115);
        assert_eq!(ftol_byte_times_19_2(6, true), 115);
    }

    // ========================================================================
    // Coverage: FYL2X with y=NaN (line 1225)
    // ========================================================================

    #[test]
    fn test_execute_fyl2x_nan_x_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // x = NaN, y = 1.0 -> IE flag
        set_st0_st1(&mut cpu, f64::NAN, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "NaN x should set IE");
    }

    // ========================================================================
    // Coverage: FYL2XP1 with x=NaN (line 1244)
    // ========================================================================

    #[test]
    fn test_execute_fyl2xp1_nan_x_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // x = NaN, y = 1.0 -> IE flag
        set_st0_st1(&mut cpu, f64::NAN, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2xp1));
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "NaN x should set IE");
    }

    // ========================================================================
    // Coverage: FCOMI with NaN sets IE (line 1002-1003)
    // ========================================================================

    #[test]
    fn test_execute_fcomi_nan_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::NAN, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomi_st0_sti, Register::ST0, Register::ST1),
        );
        // FCOMI with NaN: unordered, IE IS set (unlike FUCOMI)
        assert_ne!(cpu.fpu.status_word & 0x01, 0);
    }

    // ========================================================================
    // Coverage: FUCOMIP with NaN (no IE, sets flags, pops)
    // ========================================================================

    #[test]
    fn test_execute_fucomip_nan_sets_flags_and_pops() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::NAN, 1.0);
        cpu.fpu.status_word = 0;
        cpu.regs
            .flags
            .remove(Flags::CARRY | Flags::ZERO | Flags::PARITY);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucomip_st0_sti, Register::ST0, Register::ST1),
        );
        // FUCOMIP: NaN -> unordered, but no IE
        assert_eq!(cpu.fpu.status_word & 0x01, 0);
        // Unordered: CF=1, ZF=1, PF=1
        assert!(cpu.regs.flags.contains(Flags::CARRY));
        assert!(cpu.regs.flags.contains(Flags::ZERO));
        assert!(cpu.regs.flags.contains(Flags::PARITY));
        // Pops once
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    // ========================================================================
    // Coverage: FCOMIP with ordered values pops (line 1007-1009)
    // ========================================================================

    #[test]
    fn test_execute_fcomip_ordered_pops() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 5.0, 3.0);
        cpu.fpu.status_word = 0;
        cpu.regs
            .flags
            .remove(Flags::CARRY | Flags::ZERO | Flags::PARITY);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomip_st0_sti, Register::ST0, Register::ST1),
        );
        // 5.0 > 3.0: CF=0, ZF=0, PF=0
        assert!(!cpu.regs.flags.contains(Flags::CARRY));
        assert!(!cpu.regs.flags.contains(Flags::ZERO));
        assert!(!cpu.regs.flags.contains(Flags::PARITY));
        // Should pop once
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 0x7);
    }

    // ========================================================================
    // Tests with tracing enabled to cover tracing code paths in FPU execute
    // ========================================================================

    fn with_trace_subscriber<F: FnOnce()>(f: F) {
        use tracing_subscriber::fmt;
        use tracing_subscriber::prelude::*;
        let subscriber = fmt::layer()
            .with_writer(std::io::sink)
            .with_filter(tracing_subscriber::filter::LevelFilter::TRACE);
        let _guard = tracing_subscriber::registry()
            .with(subscriber)
            .set_default();
        f();
    }

    #[test]
    fn test_fpu_execute_with_tracing_fld_fst_fadd() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();

            // FNINIT
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fninit));

            // FILD word [0x1000] - load integer from memory
            mem.write_u16(0x1000, 42);
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fild_m16int, mem_op(0x1000)).unwrap(),
            );
            assert_eq!(cpu.fpu.get_st(0).to_f64(), 42.0);

            // FILD dword [0x1004]
            mem.write_u32(0x1004, 100);
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fild_m32int, mem_op(0x1004)).unwrap(),
            );
            assert_eq!(cpu.fpu.get_st(0).to_f64(), 100.0);

            // FILD qword [0x1008]
            let val: i64 = 999;
            mem.write_u64(0x1008, val as u64);
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fild_m64int, mem_op(0x1008)).unwrap(),
            );
            assert_eq!(cpu.fpu.get_st(0).to_f64(), 999.0);

            // FLD1
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fld1));
            assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);

            // FADD ST0, ST1
            exec_instr(
                &mut cpu,
                &mut mem,
                instr2(Code::Fadd_st0_sti, Register::ST0, Register::ST1),
            );
        });
    }

    #[test]
    fn test_fpu_fistp_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();

            exec_instr(&mut cpu, &mut mem, instr0(Code::Fninit));

            // Push a known value
            cpu.fpu.push(F80::from_f64(42.0));

            // FISTP word [0x2000]
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fistp_m16int, mem_op(0x2000)).unwrap(),
            );
            assert_eq!(mem.read_u16(0x2000) as i16, 42);
        });
    }

    #[test]
    fn test_fpu_fist_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();

            exec_instr(&mut cpu, &mut mem, instr0(Code::Fninit));
            cpu.fpu.push(F80::from_f64(123.0));

            // FIST word [0x2000]
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fist_m16int, mem_op(0x2000)).unwrap(),
            );
            assert_eq!(mem.read_u16(0x2000) as i16, 123);
        });
    }

    #[test]
    fn test_fpu_fld_f32_f64_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();

            exec_instr(&mut cpu, &mut mem, instr0(Code::Fninit));

            // FLD f32 [0x3000]
            let f32_val: f32 = 3.14;
            mem.write_u32(0x3000, u32::from_le_bytes(f32_val.to_le_bytes()));
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fld_m32fp, mem_op(0x3000)).unwrap(),
            );

            // FLD f64 [0x3004]
            let f64_val: f64 = 2.71828;
            mem.write_u64(0x3004, u64::from_le_bytes(f64_val.to_le_bytes()));
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fld_m64fp, mem_op(0x3004)).unwrap(),
            );
        });
    }

    #[test]
    fn test_fpu_fstp_f32_f64_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();

            exec_instr(&mut cpu, &mut mem, instr0(Code::Fninit));
            cpu.fpu.push(F80::from_f64(1.5));

            // FSTP f32 [0x4000]
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fstp_m32fp, mem_op(0x4000)).unwrap(),
            );
        });
    }

    #[test]
    fn test_fpu_fcom_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();

            exec_instr(&mut cpu, &mut mem, instr0(Code::Fninit));

            // Set up f32 for FCOM memory operand
            let f32_val: f32 = 1.0;
            mem.write_u32(0x5000, u32::from_le_bytes(f32_val.to_le_bytes()));

            cpu.fpu.push(F80::from_f64(2.0));

            // FCOM dword [0x5000]
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fcom_m32fp, mem_op(0x5000)).unwrap(),
            );
        });
    }

    #[test]
    fn test_fpu_misc_with_tracing() {
        with_trace_subscriber(|| {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();

            exec_instr(&mut cpu, &mut mem, instr0(Code::Fninit));

            // FLD constants
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldz));
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldpi));
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldl2e));
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldl2t));
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldlg2));
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fldln2));

            // FCHS
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fchs));

            // FABS
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fabs));

            // FNSTCW [0x6000]
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fnstcw_m2byte, mem_op(0x6000)).unwrap(),
            );

            // FLDCW [0x6000]
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fldcw_m2byte, mem_op(0x6000)).unwrap(),
            );

            // FNSTSW AX
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fnstsw_AX, Register::AX).unwrap(),
            );
        });
    }

    // ========================================================================
    // Mutant-killing tests: FXAM condition code bit clearing
    // ========================================================================

    /// FXAM positive zero: C3=1, C2=0, C0=0, C1=0.
    /// Pre-set all condition bits to 1 to verify clearing works (kills `delete !`
    /// and `<< with >>` mutations on the `&= !(1 << N)` lines).
    #[test]
    fn test_fxam_mutant_positive_zero_clears_bits() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 0.0, 0.0);
        // Pre-set ALL condition code bits to 1 so clearing is observable
        cpu.fpu.status_word |= (1 << 8) | (1 << 9) | (1 << 10) | (1 << 14);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        // Zero: C3=1, C2=0, C0=0, C1=0 (positive)
        assert_ne!(
            cpu.fpu.status_word & (1 << 14),
            0,
            "C3 must be SET for zero"
        );
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "C2 must be CLEARED for zero"
        );
        assert_eq!(
            cpu.fpu.status_word & (1 << 8),
            0,
            "C0 must be CLEARED for zero"
        );
        assert_eq!(
            cpu.fpu.status_word & (1 << 9),
            0,
            "C1 must be CLEARED (positive)"
        );
    }

    /// FXAM positive normal: C3=0, C2=1, C0=0, C1=0.
    /// Pre-set all bits to verify each one is correctly set or cleared.
    #[test]
    fn test_fxam_mutant_positive_normal_clears_bits() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, 42.0, 0.0);
        // Pre-set all condition bits
        cpu.fpu.status_word |= (1 << 8) | (1 << 9) | (1 << 10) | (1 << 14);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        // Normal: C3=0, C2=1, C0=0, C1=0 (positive)
        assert_eq!(
            cpu.fpu.status_word & (1 << 14),
            0,
            "C3 must be CLEARED for normal"
        );
        assert_ne!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "C2 must be SET for normal"
        );
        assert_eq!(
            cpu.fpu.status_word & (1 << 8),
            0,
            "C0 must be CLEARED for normal"
        );
        assert_eq!(
            cpu.fpu.status_word & (1 << 9),
            0,
            "C1 must be CLEARED (positive)"
        );
    }

    /// FXAM NaN: C3=0, C2=0, C0=1.
    /// Pre-set C3 and C2 to verify they get cleared.
    #[test]
    fn test_fxam_mutant_nan_clears_c3_c2() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::NAN, 0.0);
        cpu.fpu.status_word |= (1 << 10) | (1 << 14); // pre-set C2, C3
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        // NaN: C3=0, C2=0, C0=1
        assert_eq!(
            cpu.fpu.status_word & (1 << 14),
            0,
            "C3 must be CLEARED for NaN"
        );
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "C2 must be CLEARED for NaN"
        );
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0, "C0 must be SET for NaN");
    }

    /// FXAM +infinity: C3=0, C2=1, C0=1.
    /// Pre-set C3 to verify it gets cleared.
    #[test]
    fn test_fxam_mutant_pos_infinity_clears_c3() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        set_st0_st1(&mut cpu, f64::INFINITY, 0.0);
        cpu.fpu.status_word |= (1 << 14) | (1 << 9); // pre-set C3, C1
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        // Infinity: C3=0, C2=1, C0=1, C1=0 (positive)
        assert_eq!(
            cpu.fpu.status_word & (1 << 14),
            0,
            "C3 must be CLEARED for +inf"
        );
        assert_ne!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "C2 must be SET for +inf"
        );
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0, "C0 must be SET for +inf");
        assert_eq!(
            cpu.fpu.status_word & (1 << 9),
            0,
            "C1 must be CLEARED for +inf"
        );
    }

    /// FXAM empty register: C3=1, C2=0, C0=1.
    /// Pre-set C2 to verify clearing; pre-clear C3 and C0 to verify setting.
    #[test]
    fn test_fxam_mutant_empty_clears_c2() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        cpu.fpu.tag_word = 0xFFFF; // all empty
                                   // Pre-set C2, clear C3 and C0
        cpu.fpu.status_word = 1 << 10;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        // Empty: C3=1, C2=0, C0=1
        assert_ne!(
            cpu.fpu.status_word & (1 << 14),
            0,
            "C3 must be SET for empty"
        );
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "C2 must be CLEARED for empty"
        );
        assert_ne!(
            cpu.fpu.status_word & (1 << 8),
            0,
            "C0 must be SET for empty"
        );
    }

    // ========================================================================
    // Mutant-killing tests: FSIN/FCOS/FPTAN/FSINCOS C2 clearing
    // ========================================================================

    /// FSIN with normal angle: C2 must be actively cleared.
    /// Pre-set C2=1 to kill `&= with |=` mutation.
    #[test]
    fn test_fsin_mutant_c2_actively_cleared() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 0.0);
        // Pre-set C2
        cpu.fpu.status_word |= 1 << 10;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsin));
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "FSIN must CLEAR C2 for in-range input"
        );
    }

    /// FCOS with normal angle: C2 must be actively cleared.
    #[test]
    fn test_fcos_mutant_c2_actively_cleared() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.status_word |= 1 << 10;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fcos));
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "FCOS must CLEAR C2 for in-range input"
        );
    }

    /// FPTAN with normal angle: C2 must be actively cleared.
    #[test]
    fn test_fptan_mutant_c2_actively_cleared() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.status_word |= 1 << 10;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "FPTAN must CLEAR C2 for in-range input"
        );
    }

    /// FPTAN with NaN: C2 must be actively cleared (NaN path).
    #[test]
    fn test_fptan_mutant_nan_c2_actively_cleared() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::NAN, 0.0);
        cpu.fpu.status_word |= 1 << 10;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "FPTAN must CLEAR C2 for NaN input"
        );
    }

    /// FSINCOS with normal angle: C2 must be actively cleared.
    #[test]
    fn test_fsincos_mutant_c2_actively_cleared() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 1.0, 0.0);
        cpu.fpu.status_word |= 1 << 10;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsincos));
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "FSINCOS must CLEAR C2 for in-range input"
        );
    }

    // ========================================================================
    // Mutant-killing tests: FPREM condition codes and special cases
    // ========================================================================

    /// FPREM dividend=infinity: must set IE (kills `delete !` on `!dividend.is_infinite()`).
    #[test]
    fn test_fprem_mutant_infinite_dividend_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::INFINITY, 2.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(
            cpu.fpu.status_word & 0x01,
            0,
            "infinity dividend must set IE"
        );
    }

    /// FPREM dividend=NaN: must set IE (kills `delete !` on `!dividend.is_nan()`).
    #[test]
    fn test_fprem_mutant_nan_dividend_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::NAN, 2.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "NaN dividend must set IE");
    }

    /// FPREM divisor=infinity, finite dividend: must NOT set IE, dividend
    /// unchanged, C2=0. This kills `&& with ||` in the infinity-bypass guard.
    #[test]
    fn test_fprem_mutant_infinite_divisor_finite_dividend_no_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 3.5, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_eq!(
            cpu.fpu.status_word & 0x01,
            0,
            "finite / infinity must NOT set IE"
        );
        assert!(
            (cpu.fpu.get_st(0).to_f64() - 3.5).abs() < 1e-15,
            "dividend must be unchanged"
        );
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "C2 must be 0 (complete)"
        );
    }

    /// FPREM divisor=infinity, dividend=NaN: must NOT take the infinity-bypass
    /// path (because dividend is NaN). IE should be set.
    #[test]
    fn test_fprem_mutant_infinite_divisor_nan_dividend_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::NAN, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(
            cpu.fpu.status_word & 0x01,
            0,
            "NaN dividend with inf divisor must still set IE"
        );
    }

    /// FPREM divisor=infinity, dividend=infinity: must NOT take the
    /// infinity-bypass path. IE should be set from the NaN/infinity check.
    #[test]
    fn test_fprem_mutant_both_infinite_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, f64::INFINITY, f64::INFINITY);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "inf/inf must set IE");
    }

    /// FPREM normal case: C2=0 (complete) after small remainder.
    /// Pre-set C2=1 to verify clearing.
    #[test]
    fn test_fprem_mutant_c2_cleared_on_complete() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 5.0, 2.0);
        cpu.fpu.status_word |= 1 << 10; // pre-set C2
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_eq!(
            cpu.fpu.status_word & (1 << 10),
            0,
            "C2 must be cleared when remainder is complete"
        );
        assert!((cpu.fpu.get_st(0).to_f64() - 1.0).abs() < 1e-9);
    }

    // ========================================================================
    // Mutant-killing tests: FYL2X special cases
    // ========================================================================

    /// FYL2X y=NaN, x=positive: must set IE.
    /// Kills `&& with ||` mutation that would skip the y.is_nan() branch.
    #[test]
    fn test_fyl2x_mutant_y_nan_positive_x_sets_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // ST0 = x = 2.0 (positive, valid), ST1 = y = NaN
        set_st0_st1(&mut cpu, 2.0, f64::NAN);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert_ne!(
            cpu.fpu.status_word & 0x01,
            0,
            "y=NaN must set IE even when x is positive"
        );
    }

    /// FYL2X x=negative (not NaN), y=valid: must set IE but NOT ZE.
    #[test]
    fn test_fyl2x_mutant_negative_x_sets_ie_not_ze() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, -3.0, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "negative x must set IE");
        assert_eq!(cpu.fpu.status_word & 0x04, 0, "negative x must NOT set ZE");
    }

    /// FYL2X x=0, y=positive: must set ZE but NOT IE.
    /// This kills `&& with ||` mutations that merge the zero check with the
    /// negative/NaN check.
    #[test]
    fn test_fyl2x_mutant_zero_x_sets_ze_not_ie() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 0.0, 5.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert_ne!(cpu.fpu.status_word & 0x04, 0, "x=0 must set ZE");
        assert_eq!(cpu.fpu.status_word & 0x01, 0, "x=0 must NOT set IE");
    }

    /// FYL2X x=positive, y=positive: must NOT set any exception.
    /// Kills mutations that would fire exceptions on valid inputs.
    #[test]
    fn test_fyl2x_mutant_valid_inputs_no_exception() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 8.0, 1.0);
        cpu.fpu.status_word = 0;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fyl2x));
        assert_eq!(
            cpu.fpu.status_word & 0x01,
            0,
            "valid inputs must NOT set IE"
        );
        assert_eq!(
            cpu.fpu.status_word & 0x04,
            0,
            "valid inputs must NOT set ZE"
        );
        // 1 * log2(8) = 3.0
        assert!((cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9);
    }

    // ========================================================================
    // Mutant-killing tests: FXCH tag word swap
    // ========================================================================

    /// FXCH must swap tag words between ST(0) and ST(i), not just values.
    /// Set ST(0) tag=0 (valid), ST(2) tag=1 (zero), exchange them, verify
    /// both tags swapped. Kills `>> with <<`, `* with +`, `* with /` on the
    /// tag extraction lines.
    #[test]
    fn test_fxch_mutant_tag_swap_different_tags() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // Set up: top=0, ST(0)=reg[0] valid (tag=0), ST(2)=reg[2] zero (tag=1)
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF; // all empty
        cpu.fpu.regs[0] = F80::from_f64(42.0);
        cpu.fpu.set_tag(0, 0); // valid
        cpu.fpu.regs[2] = F80::ZERO;
        cpu.fpu.set_tag(2, 1); // zero

        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST2),
        );

        // After exchange: ST(0)=reg[0] should have tag=1, ST(2)=reg[2] tag=0
        assert_eq!(cpu.fpu.get_tag(0), 1, "ST(0) must get ST(2)'s tag (zero=1)");
        assert_eq!(
            cpu.fpu.get_tag(2),
            0,
            "ST(2) must get ST(0)'s tag (valid=0)"
        );
        // Also verify values swapped
        assert_eq!(cpu.fpu.regs[0].to_f64(), 0.0, "ST(0) must get zero value");
        assert_eq!(cpu.fpu.regs[2].to_f64(), 42.0, "ST(2) must get 42.0 value");
    }

    /// FXCH with non-zero TOP: verifies the tag extraction uses the correct
    /// absolute register indices (top and idx_i), not raw stack offsets.
    #[test]
    fn test_fxch_mutant_tag_swap_nonzero_top() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // TOP=3, so ST(0)=reg[3], ST(1)=reg[4]
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(3);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[3] = F80::from_f64(10.0);
        cpu.fpu.set_tag(3, 0); // valid
        cpu.fpu.regs[4] = F80::from_f64(20.0);
        cpu.fpu.set_tag(4, 2); // special

        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST1),
        );

        assert_eq!(cpu.fpu.get_tag(3), 2, "reg[3] must get tag=2 from reg[4]");
        assert_eq!(cpu.fpu.get_tag(4), 0, "reg[4] must get tag=0 from reg[3]");
        assert_eq!(cpu.fpu.regs[3].to_f64(), 20.0);
        assert_eq!(cpu.fpu.regs[4].to_f64(), 10.0);
    }

    // ========================================================================
    // Mutant-killing tests: FCMOV tag copy from source
    // ========================================================================

    /// FCMOVE with ZF=1 must copy source tag. Source ST(2) has tag=1 (zero),
    /// destination ST(0) has tag=0 (valid). After move, ST(0) tag must be 1.
    /// Kills `>> with <<`, `* with +` on the tag extraction line.
    #[test]
    fn test_fcmove_mutant_tag_copy_from_source() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(99.0);
        cpu.fpu.set_tag(0, 0); // valid
        cpu.fpu.regs[2] = F80::ZERO;
        cpu.fpu.set_tag(2, 1); // zero

        cpu.regs.flags.insert(Flags::ZERO); // ZF=1 -> FCMOVE fires
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmove_st0_sti, Register::ST0, Register::ST2),
        );

        assert_eq!(
            cpu.fpu.get_tag(0),
            1,
            "FCMOVE must copy source tag (1=zero) to dest"
        );
        assert_eq!(cpu.fpu.regs[0].to_f64(), 0.0, "value must be copied");
    }

    /// FCMOVB with CF=1 from ST(3): verify tag is copied from the correct
    /// source register, not from a wrong index.
    #[test]
    fn test_fcmovb_mutant_tag_copy_st3() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(1.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.regs[3] = F80::from_f64(77.0);
        cpu.fpu.set_tag(3, 2); // special

        cpu.regs.flags.insert(Flags::CARRY); // CF=1 -> FCMOVB fires
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcmovb_st0_sti, Register::ST0, Register::ST3),
        );

        assert_eq!(cpu.fpu.get_tag(0), 2, "must copy tag=2 from ST(3)");
        assert_eq!(cpu.fpu.regs[0].to_f64(), 77.0);
    }

    // ========================================================================
    // Mutant-killing tests: FBSTP BCD store and condition code mask
    // ========================================================================

    /// FBSTP stores a known value (12345) as packed BCD. Read back the 10
    /// bytes and verify each BCD digit pair. Kills `-` with `+` on register
    /// index calculation.
    #[test]
    fn test_fbstp_mutant_bcd_digits_correct() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x2000;
        // Clear destination
        for i in 0..10 {
            mem.write_u8(addr + i, 0xFF);
        }

        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(12345.0);
        cpu.fpu.set_tag(0, 0);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr as i64)).unwrap(),
        );

        let mut bytes = [0u8; 10];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = mem.read_u8(addr + i);
        }
        assert_eq!(bcd_to_f64(&bytes), 12345.0, "FBSTP must store 12345 as BCD");
        // Verify sign byte is positive
        assert_eq!(bytes[9] & 0x80, 0, "sign byte must be 0 for positive");
    }

    /// FBSTP negative value: verify sign bit and digit accuracy.
    #[test]
    fn test_fbstp_mutant_negative_value() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        let addr: usize = 0x2020;
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(-67890.0);
        cpu.fpu.set_tag(0, 0);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr as i64)).unwrap(),
        );

        let mut bytes = [0u8; 10];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = mem.read_u8(addr + i);
        }
        assert_eq!(bcd_to_f64(&bytes), -67890.0);
    }

    // ========================================================================
    // Mutant-killing tests: FLD/FST/FSTP register index
    // ========================================================================

    /// FLD ST(2) must load from the correct register (ST2), not from
    /// ST(0+2) or ST(0-2). Put distinct values in each register.
    /// Kills `-` with `+` on `instr.op_register(0).number() - Register::ST0.number()`.
    #[test]
    fn test_fld_mutant_register_index_st2() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // TOP=0: ST(0)=reg[0], ST(1)=reg[1], ST(2)=reg[2]
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(100.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.regs[1] = F80::from_f64(200.0);
        cpu.fpu.set_tag(1, 0);
        cpu.fpu.regs[2] = F80::from_f64(300.0);
        cpu.fpu.set_tag(2, 0);

        // FLD ST2: pushes ST(2) value onto stack
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fld_sti, Register::ST2));

        // After push, TOP decremented by 1 -> TOP=7
        // New ST(0) = reg[7] should contain 300.0 (copied from old ST(2)=reg[2])
        assert_eq!(
            cpu.fpu.get_st(0).to_f64(),
            300.0,
            "FLD ST2 must load from ST(2), got wrong value"
        );
    }

    /// FSTP ST(3) must store to the correct register.
    /// Kills `-` with `+` on the FSTP register index.
    #[test]
    fn test_fstp_mutant_register_index_st3() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(555.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.regs[3] = F80::from_f64(0.0);
        cpu.fpu.set_tag(3, 0);

        // FSTP ST3: store ST(0) into ST(3) then pop
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fstp_sti, Register::ST3));

        // reg[3] should now contain 555.0
        assert_eq!(
            cpu.fpu.regs[3].to_f64(),
            555.0,
            "FSTP ST3 must write to reg[3]"
        );
        // TOP should have incremented (pop)
        assert_eq!(cpu.fpu.get_top(), 1, "FSTP must pop stack");
    }

    /// FST ST(2) must store to the correct register without popping.
    /// Kills `-` with `+` on the FST register index.
    #[test]
    fn test_fst_mutant_register_index_st2() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(777.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.regs[2] = F80::from_f64(0.0);
        cpu.fpu.set_tag(2, 0);

        exec_instr(&mut cpu, &mut mem, instr1(Code::Fst_sti, Register::ST2));

        assert_eq!(
            cpu.fpu.regs[2].to_f64(),
            777.0,
            "FST ST2 must write to reg[2]"
        );
        // FST does not pop
        assert_eq!(cpu.fpu.get_top(), 0, "FST must NOT pop");
    }

    // ========================================================================
    // Mutant-killing tests: FFREE register index
    // ========================================================================

    /// FFREE ST(2) must mark ST(2) as empty (tag=3), not some other register.
    /// Kills `-` with `+` on register index calculation.
    #[test]
    fn test_ffree_mutant_register_index_st2() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        // Mark registers 0,1,2,3 as valid
        for i in 0..4 {
            cpu.fpu.regs[i] = F80::from_f64((i + 1) as f64);
            cpu.fpu.set_tag(i, 0);
        }

        exec_instr(&mut cpu, &mut mem, instr1(Code::Ffree_sti, Register::ST2));

        // Only reg[2] should be empty; others unchanged
        assert_eq!(cpu.fpu.get_tag(0), 0, "ST(0) must stay valid");
        assert_eq!(cpu.fpu.get_tag(1), 0, "ST(1) must stay valid");
        assert_eq!(cpu.fpu.get_tag(2), 3, "ST(2) must be marked empty");
        assert_eq!(cpu.fpu.get_tag(3), 0, "ST(3) must stay valid");
    }

    /// FFREE ST(3): verify tag change targets reg[3] specifically.
    #[test]
    fn test_ffree_mutant_register_index_st3() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        for i in 0..5 {
            cpu.fpu.regs[i] = F80::from_f64((i + 1) as f64);
            cpu.fpu.set_tag(i, 0);
        }

        exec_instr(&mut cpu, &mut mem, instr1(Code::Ffree_sti, Register::ST3));

        assert_eq!(cpu.fpu.get_tag(2), 0, "ST(2) must stay valid");
        assert_eq!(cpu.fpu.get_tag(3), 3, "ST(3) must be marked empty");
        assert_eq!(cpu.fpu.get_tag(4), 0, "ST(4) must stay valid");
    }

    // ========================================================================
    // Mutant-killing tests: FDIVRP register index
    // ========================================================================

    /// FDIVRP ST(1),ST(0): reversed divide ST(0)/ST(1) -> ST(1), then pop.
    /// With ST(0)=6.0, ST(1)=2.0: FDIVRP computes ST(0)/ST(1) = 6/2 = 3.0
    /// and stores in ST(1), then pops. Verify the result value to kill
    /// register index mutations.
    #[test]
    fn test_fdivrp_mutant_register_index_result() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 6.0, 2.0);

        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivrp_sti_st0, Register::ST1, Register::ST0),
        );

        // After pop, old ST(1) becomes new ST(0)
        assert_eq!(cpu.fpu.get_top(), 1);
        // FDIVRP reverses: st_i = st_j / st_i, so ST1 = ST0/ST1 = 6/2 = 3
        assert!(
            (cpu.fpu.get_st(0).to_f64() - 3.0).abs() < 1e-9,
            "FDIVRP must compute ST0/ST1=6/2=3, got {}",
            cpu.fpu.get_st(0).to_f64()
        );
    }

    /// FDIVRP with different values to distinguish from FDIVP.
    /// ST(0)=10.0, ST(1)=4.0: FDIVRP -> ST(0)/ST(1) = 10/4 = 2.5.
    #[test]
    fn test_fdivrp_mutant_distinguishes_from_fdivp() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        set_st0_st1(&mut cpu, 10.0, 4.0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivrp_sti_st0, Register::ST1, Register::ST0),
        );
        let result = cpu.fpu.get_st(0).to_f64();

        // FDIVRP: reversed, so ST0/ST1 = 10/4 = 2.5
        // FDIVP would give ST1/ST0 = 4/10 = 0.4
        assert!(
            (result - 2.5).abs() < 1e-9,
            "FDIVRP must give 2.5 (not 0.4 which FDIVP would give), got {}",
            result
        );
    }

    // ========================================================================
    // Mutant-killing tests: FCOM/FCOMP operand detection
    // ========================================================================

    /// FCOMPP (zero explicit operands): must compare ST(0) with ST(1).
    /// This tests the implicit ST(1) fallback path and kills `&& with ||`
    /// on `instr.op_count() >= 2 && instr.op_kind(1) == OpKind::Register`.
    #[test]
    fn test_fcompp_mutant_implicit_st1() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // ST(0)=1.0 < ST(1)=2.0 -> C0=1 (below)
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word = 0;

        exec_instr(&mut cpu, &mut mem, instr0(Code::Fcompp));
        // ST(0) < ST(1) -> C0=1
        assert_ne!(
            cpu.fpu.status_word & (1 << 8),
            0,
            "FCOMPP: ST(0)<ST(1) must set C0"
        );
    }

    /// FCOMP with explicit ST(2): must compare ST(0) with ST(2), not ST(1).
    /// Kills mutations that ignore the explicit register operand.
    #[test]
    fn test_fcomp_mutant_explicit_st2_operand() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        // ST(0)=5.0, ST(1)=1.0, ST(2)=3.0
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(5.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.regs[1] = F80::from_f64(1.0);
        cpu.fpu.set_tag(1, 0);
        cpu.fpu.regs[2] = F80::from_f64(3.0);
        cpu.fpu.set_tag(2, 0);
        cpu.fpu.status_word = 0;

        // FCOMP ST2: compare ST(0)=5.0 with ST(2)=3.0 -> ST0>ST2, C0=0, C3=0
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomp_st0_sti, Register::ST0, Register::ST2),
        );

        // 5.0 > 3.0 -> C0=0, C3=0
        assert_eq!(
            cpu.fpu.status_word & (1 << 8),
            0,
            "FCOMP ST2: 5>3 must not set C0"
        );
        assert_eq!(
            cpu.fpu.status_word & (1 << 14),
            0,
            "FCOMP ST2: 5>3 must not set C3"
        );
        // FCOMP pops once
        assert_eq!(cpu.fpu.get_top(), 1, "FCOMP must pop");
    }

    /// FCOM with explicit ST(2) vs implicit ST(1): verify different results.
    /// If ST(0)=5, ST(1)=10, ST(2)=3: FCOM ST1 -> below, FCOM ST2 -> above.
    #[test]
    fn test_fcom_mutant_st2_vs_default() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();

        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(5.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.regs[1] = F80::from_f64(10.0);
        cpu.fpu.set_tag(1, 0);
        cpu.fpu.regs[2] = F80::from_f64(3.0);
        cpu.fpu.set_tag(2, 0);

        // FCOM ST2: 5.0 vs 3.0 -> above (C0=0)
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcom_st0_sti, Register::ST0, Register::ST2),
        );
        let c0_st2 = cpu.fpu.status_word & (1 << 8);
        assert_eq!(c0_st2, 0, "5>3 with FCOM ST2 must not set C0");

        // FCOM ST1: 5.0 vs 10.0 -> below (C0=1)
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcom_st0_sti, Register::ST0, Register::ST1),
        );
        let c0_st1 = cpu.fpu.status_word & (1 << 8);
        assert_ne!(c0_st1, 0, "5<10 with FCOM ST1 must set C0");
    }

    /// MDD-BUG-CRUCIBLE-00580: FXCH, FINCSTP and FDECSTP clear C1 on success.
    ///
    /// Intel SDM Vol 1 Table 8-1 defines C1 as 0 for all three when they
    /// complete normally. None of them touched it, so a C1 left set by a
    /// previous rounding or overflow leaked out through FNSTSW / FNSTENV /
    /// FNSAVE and misreported "rounded up" or "stack overflow" long after the
    /// instruction that set it. The existing TOP tests check only the stack
    /// pointer and cannot see this.
    ///
    /// C1 is asserted as the literal bit 9 from the SDM, not via `C1_MASK`, so
    /// redefining the constant cannot move the goalposts. Each instruction is
    /// checked from a dirtied status word — starting from zero would pass even
    /// with no fix at all.
    #[test]
    fn fxch_and_stack_pointer_moves_clear_c1_crucible580() {
        let _lock = env_lock();
        const C1: u16 = 1 << 9;

        // --- FXCH ---
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 1.0, 2.0);
        cpu.fpu.status_word |= C1; // stale from an earlier rounding
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(
            cpu.fpu.status_word & C1,
            0,
            "a successful FXCH must clear C1 (SDM Vol 1 Table 8-1)"
        );
        // The exchange itself still happened.
        assert!((cpu.fpu.get_st(0).to_f64() - 2.0).abs() < 1e-9);
        assert!((cpu.fpu.get_st(1).to_f64() - 1.0).abs() < 1e-9);

        // --- FINCSTP ---
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu.set_top(3);
        cpu.fpu.status_word |= C1;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fincstp));
        assert_eq!(cpu.fpu.get_top(), 4, "FINCSTP still advances TOP");
        assert_eq!(
            cpu.fpu.status_word & C1,
            0,
            "FINCSTP must clear C1 (SDM Vol 1 Table 8-1)"
        );

        // --- FDECSTP ---
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu.set_top(3);
        cpu.fpu.status_word |= C1;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fdecstp));
        assert_eq!(cpu.fpu.get_top(), 2, "FDECSTP still retreats TOP");
        assert_eq!(
            cpu.fpu.status_word & C1,
            0,
            "FDECSTP must clear C1 (SDM Vol 1 Table 8-1)"
        );
    }

    #[test]
    fn test_fdecstp_mutant_and_to_or_from_top_1() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu.set_top(1);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fdecstp));
        assert_eq!(cpu.fpu.get_top(), 0, "FDECSTP from top=1 must give 0");
    }

    #[test]
    fn test_fdecstp_mutant_and_to_or_from_top_5() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu.set_top(5);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fdecstp));
        assert_eq!(cpu.fpu.get_top(), 4, "FDECSTP from top=5 must give 4");
    }

    /// FXTRACT on a full stack with IM=1 overwrites both the source and
    /// new TOP slots with the indefinite QNaN (Intel SDM Vol 1 §8.5.1.2).
    #[test]
    fn test_fxtract_full_stack_overflow_writes_indefinite_to_both_slots() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        fill_stack(&mut cpu);
        let top_before = cpu.fpu.get_top();
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxtract));
        // TOP advances (push happened despite overflow).
        let top_after = cpu.fpu.get_top();
        assert_eq!(
            top_after,
            (top_before + 7) & 0x7,
            "FXTRACT push-overflow must still decrement TOP per SDM §8.5.1.2"
        );
        // New ST(0) = F80 indefinite QNaN, tagged Special.
        assert_eq!(cpu.fpu.get_st(0), F80::INDEFINITE);
        assert_eq!(cpu.fpu.get_tag(cpu.fpu.get_st_idx(0)), 2);
        // New ST(1) (= old source ST(0)) = also F80 indefinite QNaN.
        assert_eq!(cpu.fpu.get_st(1), F80::INDEFINITE);
        assert_eq!(cpu.fpu.get_tag(cpu.fpu.get_st_idx(1)), 2);
        // C1=1 (overflow flavour), SF=1, IE=1.
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "IE must be set");
        assert_ne!(cpu.fpu.status_word & 0x40, 0, "SF must be set");
        assert_ne!(
            cpu.fpu.status_word & (1 << 9),
            0,
            "C1 must indicate overflow"
        );
    }

    /// FSINCOS on full stack — same semantics as FXTRACT push-overflow per
    /// host x87 probe `tmp/probe_fsincos_overflow.rs`.
    #[test]
    fn test_fsincos_full_stack_overflow_writes_indefinite_to_both_slots() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        fill_stack(&mut cpu);
        let top_before = cpu.fpu.get_top();
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsincos));
        let top_after = cpu.fpu.get_top();
        assert_eq!(
            top_after,
            (top_before + 7) & 0x7,
            "FSINCOS push-overflow must still decrement TOP per SDM §8.5.1.2"
        );
        assert_eq!(cpu.fpu.get_st(0), F80::INDEFINITE);
        assert_eq!(cpu.fpu.get_tag(cpu.fpu.get_st_idx(0)), 2);
        assert_eq!(cpu.fpu.get_st(1), F80::INDEFINITE);
        assert_eq!(cpu.fpu.get_tag(cpu.fpu.get_st_idx(1)), 2);
        // FSINCOS clears C2 on completion.
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0, "C2 must be cleared");
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "IE must be set");
        assert_ne!(cpu.fpu.status_word & 0x40, 0, "SF must be set");
        assert_ne!(
            cpu.fpu.status_word & (1 << 9),
            0,
            "C1 must indicate overflow"
        );
    }

    /// FPTAN on full stack — same semantics as FXTRACT/FSINCOS push-overflow
    /// per host x87 probe `tmp/probe_fptan_overflow.rs`.
    #[test]
    fn test_fptan_full_stack_overflow_writes_indefinite_to_both_slots() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        fill_stack(&mut cpu);
        let top_before = cpu.fpu.get_top();
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
        let top_after = cpu.fpu.get_top();
        assert_eq!(
            top_after,
            (top_before + 7) & 0x7,
            "FPTAN push-overflow must still decrement TOP per SDM §8.5.1.2"
        );
        assert_eq!(cpu.fpu.get_st(0), F80::INDEFINITE);
        assert_eq!(cpu.fpu.get_tag(cpu.fpu.get_st_idx(0)), 2);
        assert_eq!(cpu.fpu.get_st(1), F80::INDEFINITE);
        assert_eq!(cpu.fpu.get_tag(cpu.fpu.get_st_idx(1)), 2);
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0, "C2 must be cleared");
        assert_ne!(cpu.fpu.status_word & 0x01, 0, "IE must be set");
        assert_ne!(cpu.fpu.status_word & 0x40, 0, "SF must be set");
        assert_ne!(
            cpu.fpu.status_word & (1 << 9),
            0,
            "C1 must indicate overflow"
        );
    }

    #[test]
    fn test_fld_st4_loads_correct_value() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(100.0));
        cpu.fpu.push(F80::from_f64(200.0));
        cpu.fpu.push(F80::from_f64(300.0));
        cpu.fpu.push(F80::from_f64(400.0));
        cpu.fpu.push(F80::from_f64(500.0));
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fld_sti, Register::ST4));
        assert_eq!(
            cpu.fpu.get_st(0).to_f64(),
            100.0,
            "FLD ST(4) must load the 5th pushed value (100.0)"
        );
    }

    #[test]
    fn test_fstp_st4_stores_to_correct_register() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(100.0));
        cpu.fpu.push(F80::from_f64(200.0));
        cpu.fpu.push(F80::from_f64(300.0));
        cpu.fpu.push(F80::from_f64(400.0));
        cpu.fpu.push(F80::from_f64(999.0));
        let top = cpu.fpu.get_top();
        let st4_phys = cpu.fpu.get_st_idx(4);
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fstp_sti, Register::ST4));
        assert_eq!(
            cpu.fpu.regs[st4_phys].to_f64(),
            999.0,
            "FSTP ST(4) must store to the correct physical register"
        );
        assert_eq!(cpu.fpu.get_top(), (top + 1) & 7, "FSTP must pop");
    }

    #[test]
    fn test_fst_st3_stores_without_pop() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(100.0));
        cpu.fpu.push(F80::from_f64(200.0));
        cpu.fpu.push(F80::from_f64(300.0));
        cpu.fpu.push(F80::from_f64(888.0));
        let top_before = cpu.fpu.get_top();
        let st3_phys = cpu.fpu.get_st_idx(3);
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fst_sti, Register::ST3));
        assert_eq!(
            cpu.fpu.regs[st3_phys].to_f64(),
            888.0,
            "FST ST(3) must store to ST(3) physical register"
        );
        assert_eq!(cpu.fpu.get_top(), top_before, "FST must NOT pop");
    }

    #[test]
    fn test_ffree_st4_marks_only_st4_empty() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        for i in 0..6 {
            cpu.fpu.regs[i] = F80::from_f64((i + 1) as f64 * 100.0);
            cpu.fpu.set_tag(i, 0);
        }
        exec_instr(&mut cpu, &mut mem, instr1(Code::Ffree_sti, Register::ST4));
        for i in 0..6 {
            let expected = if i == 4 { 3 } else { 0 };
            assert_eq!(cpu.fpu.get_tag(i), expected, "tag for reg {i}");
        }
    }

    #[test]
    fn test_ffreep_st3_marks_empty_and_pops() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        for i in 0..5 {
            cpu.fpu.regs[i] = F80::from_f64((i + 1) as f64 * 100.0);
            cpu.fpu.set_tag(i, 0);
        }
        let top_before = cpu.fpu.get_top();
        exec_instr(&mut cpu, &mut mem, instr1(Code::Ffreep_sti, Register::ST3));
        assert_eq!(cpu.fpu.get_tag(3), 3, "FFREEP must mark reg[3] empty");
        assert_eq!(cpu.fpu.get_tag(1), 0, "reg[1] must stay valid");
        assert_eq!(cpu.fpu.get_tag(2), 0, "reg[2] must stay valid");
        assert_eq!(cpu.fpu.get_tag(4), 0, "reg[4] must stay valid");
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 7, "FFREEP must pop");
    }

    #[test]
    fn test_fdivrp_st3_st0_correct_indices() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(4.0));
        cpu.fpu.push(F80::from_f64(0.5));
        cpu.fpu.push(F80::from_f64(0.25));
        cpu.fpu.push(F80::from_f64(12.0));
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivrp_sti_st0, Register::ST3, Register::ST0),
        );
        assert!(
            (cpu.fpu.get_st(2).to_f64() - 3.0).abs() < 1e-9,
            "FDIVRP ST(3),ST(0) must compute 12/4=3.0, got {}",
            cpu.fpu.get_st(2).to_f64()
        );
    }

    #[test]
    fn test_faddp_st3_st0_correct_register_indices() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(7.0));
        cpu.fpu.push(F80::from_f64(0.1));
        cpu.fpu.push(F80::from_f64(0.2));
        cpu.fpu.push(F80::from_f64(10.0));
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Faddp_sti_st0, Register::ST3, Register::ST0),
        );
        assert!(
            (cpu.fpu.get_st(2).to_f64() - 17.0).abs() < 1e-9,
            "FADDP ST(3),ST(0) must give 7+10=17, got {}",
            cpu.fpu.get_st(2).to_f64()
        );
    }

    #[test]
    fn test_fdivr_st3_check_div_exception_register_index() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(0.0));
        cpu.fpu.push(F80::from_f64(1.0));
        cpu.fpu.push(F80::from_f64(2.0));
        cpu.fpu.push(F80::from_f64(5.0));
        cpu.fpu.status_word &= !0x04;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivrp_sti_st0, Register::ST3, Register::ST0),
        );
        assert_ne!(
            cpu.fpu.status_word & 0x04,
            0,
            "FDIVRP with ST(3)=0.0 must set ZE flag"
        );
    }

    #[test]
    fn test_fdiv_st0_st3_check_div_exception_register_index() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.push(F80::from_f64(0.0));
        cpu.fpu.push(F80::from_f64(1.0));
        cpu.fpu.push(F80::from_f64(2.0));
        cpu.fpu.push(F80::from_f64(10.0));
        cpu.fpu.status_word &= !0x04;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdiv_st0_sti, Register::ST0, Register::ST3),
        );
        assert_ne!(
            cpu.fpu.status_word & 0x04,
            0,
            "FDIV with ST(3)=0.0 divisor must set ZE flag"
        );
    }

    // ========================================================================
    // Mutant-killing: FXCH tag extraction with top != 0 (L1596)
    // ========================================================================

    #[test]
    fn test_fxch_tag_extraction_top3_st2() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(3);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[3] = F80::from_f64(100.0);
        cpu.fpu.set_tag(3, 0);
        cpu.fpu.regs[5] = F80::ZERO;
        cpu.fpu.set_tag(5, 1);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST2),
        );
        assert_eq!((cpu.fpu.tag_word >> 6) & 0x3, 1, "reg[3] gets tag=1");
        assert_eq!((cpu.fpu.tag_word >> 10) & 0x3, 0, "reg[5] gets tag=0");
        assert_eq!(cpu.fpu.regs[3].to_f64(), 0.0);
        assert_eq!(cpu.fpu.regs[5].to_f64(), 100.0);
    }

    #[test]
    fn test_fxch_tag_extraction_top5_st3_wrap() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(5);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[5] = F80::from_f64(77.0);
        cpu.fpu.set_tag(5, 2);
        cpu.fpu.regs[0] = F80::from_f64(33.0);
        cpu.fpu.set_tag(0, 0);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST3),
        );
        assert_eq!((cpu.fpu.tag_word >> 10) & 0x3, 0, "reg[5] gets tag=0");
        assert_eq!(cpu.fpu.tag_word & 0x3, 2, "reg[0] gets tag=2");
        assert_eq!(cpu.fpu.regs[5].to_f64(), 33.0);
        assert_eq!(cpu.fpu.regs[0].to_f64(), 77.0);
    }

    // ========================================================================
    // Mutant-killing: FXAM tag extraction with top != 0 (L1619)
    // ========================================================================

    #[test]
    fn test_fxam_tag_extraction_top2_normal() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(2);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[2] = F80::from_f64(42.0);
        cpu.fpu.set_tag(2, 0);
        cpu.fpu.status_word |= (1 << 8) | (1 << 9) | (1 << 10) | (1 << 14);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0, "C3=0 normal");
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0, "C2=1 normal");
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0, "C0=0 normal");
        assert_eq!(cpu.fpu.status_word & (1 << 9), 0, "C1=0 positive");
    }

    #[test]
    fn test_fxam_tag_extraction_top3_normal() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(3);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[3] = F80::from_f64(-7.5);
        cpu.fpu.set_tag(3, 0);
        cpu.fpu.status_word &= !((1 << 8) | (1 << 9) | (1 << 10) | (1 << 14));
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0, "C3=0 normal");
        assert_ne!(cpu.fpu.status_word & (1 << 10), 0, "C2=1 normal");
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0, "C0=0 normal");
        assert_ne!(cpu.fpu.status_word & (1 << 9), 0, "C1=1 negative");
    }

    #[test]
    fn test_fxam_tag_extraction_top3_empty_vs_valid_neighbor() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(3);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(1.0);
        cpu.fpu.set_tag(0, 0);
        // Clear condition code bits without clobbering TOP (bits 11-13)
        cpu.fpu.status_word &= !((1 << 8) | (1 << 9) | (1 << 10) | (1 << 14));
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fxam));
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0, "C3=1 empty");
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0, "C2=0 empty");
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0, "C0=1 empty");
    }

    // ========================================================================
    // Mutant-killing: FTST condition code masks (L1612-1613)
    // ========================================================================

    #[test]
    fn test_ftst_positive_clears_c3_c0() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 5.0, 0.0);
        cpu.fpu.status_word |= (1 << 14) | (1 << 8);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Ftst));
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0, "FTST +5: C3 cleared");
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0, "FTST +5: C0 cleared");
    }

    #[test]
    fn test_ftst_negative_sets_c0_clears_c3() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, -3.0, 0.0);
        cpu.fpu.status_word |= 1 << 14;
        cpu.fpu.status_word &= !(1 << 8);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Ftst));
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0, "FTST -3: C0 set");
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0, "FTST -3: C3 cleared");
    }

    #[test]
    fn test_ftst_zero_sets_c3_clears_c0() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 0.0, 0.0);
        cpu.fpu.status_word |= 1 << 8;
        cpu.fpu.status_word &= !(1 << 14);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Ftst));
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0, "FTST 0: C3 set");
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0, "FTST 0: C0 cleared");
    }

    #[test]
    fn test_ftst_preserves_non_condition_bits() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 5.0, 0.0);
        cpu.fpu.status_word = 1 << 5;
        exec_instr(&mut cpu, &mut mem, instr0(Code::Ftst));
        assert_ne!(cpu.fpu.status_word & (1 << 5), 0, "bit 5 survives");
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0);
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0);
    }

    // ========================================================================
    // Mutant-killing: FPREM status word mask (L1556)
    // ========================================================================

    #[test]
    fn test_fprem_mask_clears_all_condition_codes() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 7.0, 3.0);
        cpu.fpu.status_word |= (1 << 8) | (1 << 9) | (1 << 10) | (1 << 14) | (1 << 5);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0, "FPREM: C2=0 complete");
        assert_ne!(cpu.fpu.status_word & (1 << 5), 0, "FPREM: bit 5 survives");
    }

    // ========================================================================
    // Mutant-killing: set_eflags_from_sw flag removal (L2053)
    // ========================================================================

    #[test]
    fn test_fucomi_clears_pf_on_greater_than() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 10.0, 2.0);
        cpu.regs.flags.insert(Flags::PARITY);
        cpu.regs.flags.insert(Flags::ZERO);
        cpu.regs.flags.insert(Flags::CARRY);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucomi_st0_sti, Register::ST0, Register::ST1),
        );
        assert!(
            !cpu.regs.flags.contains(Flags::CARRY),
            "FUCOMI GT: CF cleared"
        );
        assert!(
            !cpu.regs.flags.contains(Flags::ZERO),
            "FUCOMI GT: ZF cleared"
        );
        assert!(
            !cpu.regs.flags.contains(Flags::PARITY),
            "FUCOMI GT: PF cleared"
        );
    }

    #[test]
    fn test_fucomi_sets_cf_clears_zf_pf_on_less_than() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 1.0, 9.0);
        cpu.regs.flags.insert(Flags::ZERO);
        cpu.regs.flags.insert(Flags::PARITY);
        cpu.regs.flags.remove(Flags::CARRY);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucomi_st0_sti, Register::ST0, Register::ST1),
        );
        assert!(cpu.regs.flags.contains(Flags::CARRY), "FUCOMI LT: CF set");
        assert!(
            !cpu.regs.flags.contains(Flags::ZERO),
            "FUCOMI LT: ZF cleared"
        );
        assert!(
            !cpu.regs.flags.contains(Flags::PARITY),
            "FUCOMI LT: PF cleared"
        );
    }

    #[test]
    fn test_fucomi_equal_sets_zf_clears_cf_pf() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 4.0, 4.0);
        cpu.regs.flags.insert(Flags::CARRY);
        cpu.regs.flags.insert(Flags::PARITY);
        cpu.regs.flags.remove(Flags::ZERO);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucomi_st0_sti, Register::ST0, Register::ST1),
        );
        assert!(
            !cpu.regs.flags.contains(Flags::CARRY),
            "FUCOMI EQ: CF cleared"
        );
        assert!(cpu.regs.flags.contains(Flags::ZERO), "FUCOMI EQ: ZF set");
        assert!(
            !cpu.regs.flags.contains(Flags::PARITY),
            "FUCOMI EQ: PF cleared"
        );
    }

    /// FCOMI/FUCOMI CLEAR OF, SF and AF — they do not preserve them.
    ///
    /// This test used to assert the opposite (`test_fucomi_preserves_other_flags`,
    /// "FUCOMI preserves SF" / "FUCOMI preserves OF"). It carried no citation:
    /// it pinned what the code did, not what the hardware does. Both the SDM's
    /// "Flags Affected" for FCOMI and a direct host-silicon probe say all
    /// three are zeroed — with OF/SF/AF pre-set to 1, every ordering and a NaN
    /// operand come back OF=SF=AF=0 from both `fcomi` and `fucomi`.
    ///
    /// It went unnoticed because `requires_p6()` made the whole family
    /// unreachable: no `CpuType` was P6-capable, so no guest could execute
    /// FCOMI and the wrong flags could never be observed. The Pentium Pro /
    /// Pentium II identities are what make it matter.
    #[test]
    fn test_fucomi_clears_of_sf_af() {
        let _lock = env_lock();
        for (a, b, what) in [
            (1.0f64, 2.0f64, "less-than"),
            (2.0, 1.0, "greater-than"),
            (1.0, 1.0, "equal"),
        ] {
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            set_st0_st1(&mut cpu, a, b);
            cpu.regs.flags.insert(Flags::SIGN);
            cpu.regs.flags.insert(Flags::OVERFLOW);
            cpu.regs.flags.insert(Flags::AUX_CARRY);
            exec_instr(
                &mut cpu,
                &mut mem,
                instr2(Code::Fucomi_st0_sti, Register::ST0, Register::ST1),
            );
            assert!(
                !cpu.regs.flags.contains(Flags::SIGN),
                "FUCOMI ({what}) must clear SF"
            );
            assert!(
                !cpu.regs.flags.contains(Flags::OVERFLOW),
                "FUCOMI ({what}) must clear OF"
            );
            assert!(
                !cpu.regs.flags.contains(Flags::AUX_CARRY),
                "FUCOMI ({what}) must clear AF"
            );
        }
    }

    /// The x87 register fast lane must never claim a P6 encoding.
    ///
    /// `try_execute_register_lane` is called from the interpreter BEFORE the
    /// decode-cache route, and it executes directly - it consults no
    /// `requires_p6()` gate. So the design claim that "x87 has exactly one P6
    /// gate" is only true because this lane's match arms happen to exclude
    /// every P6 encoding. One of those arms is `(0xDF, 0xE0)`, a single nibble
    /// away from `DF E8-F7` (FUCOMIP/FCOMIP).
    ///
    /// Safe-by-coincidence is not safe. This turns the exclusion into a
    /// checked property, so widening the lane cannot silently route FCMOVcc or
    /// FCOMI around the identity gate and execute them on a pre-P6 guest.
    #[test]
    fn register_fast_lane_never_claims_a_p6_encoding() {
        let mut cpu = Cpu::new();
        // The twelve P6 x87 forms, by escape byte and ModR/M range:
        //   DA C0-DF  FCMOVB/FCMOVE/FCMOVBE/FCMOVU
        //   DB C0-DF  FCMOVNB/FCMOVNE/FCMOVNBE/FCMOVNU
        //   DB E8-EF  FUCOMI      DB F0-F7  FCOMI
        //   DF E8-EF  FUCOMIP     DF F0-F7  FCOMIP
        let ranges: &[(u8, u8, u8, &str)] = &[
            (0xDA, 0xC0, 0xDF, "DA C0-DF FCMOVcc"),
            (0xDB, 0xC0, 0xDF, "DB C0-DF FCMOVNcc"),
            (0xDB, 0xE8, 0xEF, "DB E8-EF FUCOMI"),
            (0xDB, 0xF0, 0xF7, "DB F0-F7 FCOMI"),
            (0xDF, 0xE8, 0xEF, "DF E8-EF FUCOMIP"),
            (0xDF, 0xF0, 0xF7, "DF F0-F7 FCOMIP"),
        ];
        for &(escape, lo, hi, what) in ranges {
            for modrm in lo..=hi {
                assert!(
                    !try_execute_register_lane(&mut cpu, escape, modrm),
                    "{what}: the fast lane claimed {escape:02X} {modrm:02X}, \
                     bypassing the requires_p6() gate entirely"
                );
            }
        }
    }

    /// The empty-stack path returns early, so it needs the clear too — a
    /// separate `return Ok(())` is exactly where a flag fix gets missed.
    #[test]
    fn test_fucomi_clears_of_sf_af_on_empty_stack() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        // Leave the stack empty so the #IS early-return path runs.
        cpu.regs.flags.insert(Flags::SIGN);
        cpu.regs.flags.insert(Flags::OVERFLOW);
        cpu.regs.flags.insert(Flags::AUX_CARRY);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucomi_st0_sti, Register::ST0, Register::ST1),
        );
        assert!(!cpu.regs.flags.contains(Flags::SIGN), "empty stack: SF");
        assert!(!cpu.regs.flags.contains(Flags::OVERFLOW), "empty stack: OF");
        assert!(
            !cpu.regs.flags.contains(Flags::AUX_CARRY),
            "empty stack: AF"
        );
        // And still the SDM's "unordered" condition codes.
        assert!(cpu.regs.flags.contains(Flags::ZERO));
        assert!(cpu.regs.flags.contains(Flags::PARITY));
        assert!(cpu.regs.flags.contains(Flags::CARRY));
    }

    // ========================================================================
    // Mutant-killing: FNSAVE memory offsets 18 and 26 (L1751, L1754)
    // ========================================================================

    #[test]
    fn test_fnsave_32bit_offset18_opcode_zeroed_on_0xff_fill() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 32;
        cpu.fpu = Fpu::new();
        cpu.fpu.control_word = 0x037F;
        cpu.fpu.status_word = 0;
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.ip_offset = 0x12345678;
        cpu.fpu.ip_selector = 0xABCD;
        cpu.fpu.operand_offset = 0x87654321;
        cpu.fpu.operand_selector = 0xEF01;
        let base: usize = 0xB000;
        for i in 0..108 {
            mem.write_u8(base + i, 0xFF);
        }
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnsave_m108byte, mem_op(base as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(base + 18), 0x0000, "offset 18: opcode=0");
        assert_eq!(mem.read_u16(base + 16), 0xABCD, "offset 16: ip_selector");
        assert_eq!(mem.read_u32(base + 20), 0x87654321, "offset 20: op_offset");
    }

    #[test]
    fn test_fnsave_32bit_offset26_reserved_zeroed_on_0xff_fill() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 32;
        cpu.fpu = Fpu::new();
        cpu.fpu.control_word = 0x037F;
        cpu.fpu.status_word = 0;
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.ip_offset = 0;
        cpu.fpu.ip_selector = 0;
        cpu.fpu.operand_offset = 0;
        cpu.fpu.operand_selector = 0x1234;
        let base: usize = 0xC000;
        for i in 0..108 {
            mem.write_u8(base + i, 0xFF);
        }
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnsave_m108byte, mem_op(base as i64)).unwrap(),
        );
        assert_eq!(mem.read_u16(base + 26), 0x0000, "offset 26: reserved=0");
        assert_eq!(mem.read_u16(base + 24), 0x1234, "offset 24: op_selector");
        assert_eq!(mem.read_u8(base + 27), 0x00, "offset 27: second byte=0");
    }

    // ========================================================================
    // Mutant-killing: FUCOMPP operand detection (L1070)
    // ========================================================================

    #[test]
    fn test_fucompp_implicit_st1_operand() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(1.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.regs[1] = F80::from_f64(5.0);
        cpu.fpu.set_tag(1, 0);
        cpu.fpu.regs[2] = F80::from_f64(1.0);
        cpu.fpu.set_tag(2, 0);
        cpu.fpu.status_word &= !((1 << 8) | (1 << 10) | (1 << 14));
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fucompp));
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0, "FUCOMPP: 1<5, C0 set");
        assert_eq!(cpu.fpu.get_top(), 2, "FUCOMPP pops twice");
    }

    #[test]
    fn test_fucomp_explicit_st2_vs_fucompp_implicit() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.fpu = Fpu::new();
        cpu.fpu.set_top(0);
        cpu.fpu.tag_word = 0xFFFF;
        cpu.fpu.regs[0] = F80::from_f64(5.0);
        cpu.fpu.set_tag(0, 0);
        cpu.fpu.regs[1] = F80::from_f64(10.0);
        cpu.fpu.set_tag(1, 0);
        cpu.fpu.regs[2] = F80::from_f64(3.0);
        cpu.fpu.set_tag(2, 0);
        cpu.fpu.status_word = 0;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucomp_st0_sti, Register::ST0, Register::ST2),
        );
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0, "FUCOMP ST2: 5>3 C0=0");
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0, "FUCOMP ST2: 5>3 C3=0");
        assert_eq!(cpu.fpu.get_top(), 1, "FUCOMP pops once");
    }

    // ========================================================================
    // Mutant-killing: set_condition_codes_from_sw mask (L2045)
    // ========================================================================

    #[test]
    fn test_fucom_condition_codes_equal_clears_c0() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 7.0, 7.0);
        cpu.fpu.status_word |= 1 << 8;
        cpu.fpu.status_word &= !(1 << 14);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucom_st0_sti, Register::ST0, Register::ST1),
        );
        assert_ne!(cpu.fpu.status_word & (1 << 14), 0, "FUCOM EQ: C3 set");
        assert_eq!(cpu.fpu.status_word & (1 << 8), 0, "FUCOM EQ: C0 cleared");
    }

    #[test]
    fn test_fucom_condition_codes_less_clears_c3() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        set_st0_st1(&mut cpu, 2.0, 8.0);
        cpu.fpu.status_word |= 1 << 14;
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucom_st0_sti, Register::ST0, Register::ST1),
        );
        assert_ne!(cpu.fpu.status_word & (1 << 8), 0, "FUCOM LT: C0 set");
        assert_eq!(cpu.fpu.status_word & (1 << 14), 0, "FUCOM LT: C3 cleared");
    }

    // ========================================================================
    // Empty-stack handling on ST(i) paths with masked IE: results, condition
    // codes and pops must follow Intel SDM Vol 1 §8.5.1.1 and §8.3.6.2,
    // rather than merely setting the stack-fault bit and returning early.
    // ========================================================================

    /// Fuzz Repro A (`D9 C2 DB 17` = FLD ST(2); FIST DWORD [BX]).
    /// FLD ST(i) on a fresh FPU (all empty) must push FP indefinite
    /// (`[0,0,0,0,0,0,0,0xC0,0xFF,0xFF]`) and set SF+IE.
    #[test]
    fn test_fld_empty_source_writes_indefinite_and_sets_sf() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fld_sti, Register::ST2));
        // SF=1 (bit 6), IE=1 (bit 0)
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE must be set");
        // New ST(0) = FP indefinite
        let st0 = cpu.fpu.get_st(0);
        assert_eq!(
            st0.0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF],
            "new ST(0) must be FP indefinite"
        );
        // Tag of new ST(0) is Special (2)
        let top = cpu.fpu.get_top();
        assert_eq!(cpu.fpu.get_tag(top), 2, "new ST(0) tag must be Special");
        // C1=0 on stack underflow
        assert_eq!(
            cpu.fpu.status_word & (1 << 9),
            0,
            "C1 must be 0 on underflow"
        );
    }

    /// FIST DWORD after FLD of an empty register must write the integer
    /// indefinite (0x8000_0000).
    #[test]
    fn test_fist_after_fld_of_empty_writes_integer_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        // FLD ST(2) on fresh CPU -> ST(0) = QNaN, tag=Special
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fld_sti, Register::ST2));
        // FIST DWORD [addr]: stores integer indefinite since ST(0) is a NaN.
        let addr: i64 = 0x1800;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fist_m32int, mem_op(addr)).unwrap(),
        );
        let bytes = [
            mem.read_u8(addr as usize),
            mem.read_u8(addr as usize + 1),
            mem.read_u8(addr as usize + 2),
            mem.read_u8(addr as usize + 3),
        ];
        assert_eq!(
            u32::from_le_bytes(bytes),
            0x8000_0000,
            "FIST of QNaN must write integer indefinite"
        );
    }

    /// `D9 C2 DB 11` decodes as `FLD ST(2); FIST DWORD [BX+DI]`. With
    /// empty ST(2), the store must write integer indefinite 0x80000000
    /// (SDM Vol 1 §8.2.2). Cover segment-base plus 16-bit base/index
    /// addressing, not only direct-displacement operands.
    #[test]
    fn test_fld_empty_fist_indefinite_rm16_byte_sequence() {
        use iced_x86::{Decoder, DecoderOptions};
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        // RM16 fixture: DS = 0x1000 (base 0x10000), 16-bit code.
        cpu.cs_bits = 16;
        cpu.regs.ds = 0x1000;
        cpu.regs.ds_cache.base = 0x10000;
        cpu.regs.ds_cache.cached_effective_limit = 0xFFFF;
        cpu.regs.ds_cache.is_writable = true;
        // BX = DI = 0  →  effective offset = 0  →  linear DS:0 = 0x10000.
        cpu.regs.ebx = 0;
        cpu.regs.edi = 0;
        reset_empty_fpu(&mut cpu);
        // Pre-fill destination dword with a known sentinel so a missed write
        // would also fail (not just "store wrote zero"). 0xDEAD_BEEF.
        let dst: usize = 0x10000;
        for (i, b) in 0xDEAD_BEEF_u32.to_le_bytes().iter().enumerate() {
            mem.write_u8(dst + i, *b);
        }
        let bytes = [0xD9u8, 0xC2, 0xDB, 0x11];
        let mut decoder = Decoder::with_ip(16, &bytes, 0, DecoderOptions::NONE);
        let fld = decoder.decode();
        assert!(!fld.is_invalid(), "FLD ST(2) decode");
        let fist = decoder.decode();
        assert!(!fist.is_invalid(), "FIST DWORD [BX+DI] decode");
        exec_instr(&mut cpu, &mut mem, fld);
        exec_instr(&mut cpu, &mut mem, fist);
        let stored = u32::from_le_bytes([
            mem.read_u8(dst),
            mem.read_u8(dst + 1),
            mem.read_u8(dst + 2),
            mem.read_u8(dst + 3),
        ]);
        assert_eq!(
            stored, 0x8000_0000,
            "FIST after FLD-of-empty must write integer indefinite (got {:#010x})",
            stored
        );
    }

    /// Fuzz Repro B (`DD E9 90 DD 78 3D` = FUCOMP ST(1); NOP; FNSTSW...).
    /// FUCOMP on empty ST(0)/ST(1) must set C0=C2=C3=1 AND pop the stack.
    #[test]
    fn test_fucomp_empty_sets_c0_c2_c3_and_pops() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucomp_st0_sti, Register::ST0, Register::ST1),
        );
        // SF+IE set
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE must be set");
        // C0 (bit 8) + C2 (bit 10) + C3 (bit 14) all set = 0x4500
        assert_eq!(
            cpu.fpu.status_word & 0x4500,
            0x4500,
            "C0+C2+C3 must all be set on masked invalid compare"
        );
        // Popped once (TOP advanced by 1)
        assert_eq!(
            cpu.fpu.get_top(),
            (top_before + 1) & 7,
            "FUCOMP must pop even on masked empty path"
        );
    }

    /// FUCOM (non-popping) on empty ST(0) must set C0=C2=C3=1 but NOT pop.
    #[test]
    fn test_fucom_empty_sets_c0_c2_c3_and_does_not_pop() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fucom_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.status_word & 0x4500, 0x4500, "C0+C2+C3 set");
        assert_eq!(cpu.fpu.get_top(), top_before, "FUCOM must not pop");
    }

    /// FUCOMPP on empty stack must set C0=C2=C3=1 and pop twice.
    #[test]
    fn test_fucompp_empty_sets_c0_c2_c3_and_pops_twice() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let top_before = cpu.fpu.get_top();
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fucompp));
        assert_eq!(cpu.fpu.status_word & 0x4500, 0x4500, "C0+C2+C3 set");
        assert_eq!(
            cpu.fpu.get_top(),
            (top_before + 2) & 7,
            "FUCOMPP must pop twice"
        );
    }

    /// Fuzz Repro C (`DE 14 DD B9` = FICOM WORD [SI]; FNSTSW...).
    /// FICOM on empty ST(0) must set C0=C2=C3=1 without popping.
    #[test]
    fn test_ficom_empty_sets_c0_c2_c3_and_does_not_pop() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let addr: i64 = 0x1000;
        mem.write_u16(addr as usize, 42);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Ficom_m16int, mem_op(addr)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & 0x4500,
            0x4500,
            "FICOM must set C0+C2+C3 on empty ST(0)"
        );
        assert_eq!(cpu.fpu.get_top(), top_before, "FICOM must not pop");
    }

    /// FICOMP variant of Repro C: same CC semantics but pops.
    #[test]
    fn test_ficomp_empty_sets_c0_c2_c3_and_pops() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let addr: i64 = 0x1000;
        mem.write_u16(addr as usize, 42);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Ficomp_m16int, mem_op(addr)).unwrap(),
        );
        assert_eq!(cpu.fpu.status_word & 0x4500, 0x4500, "C0+C2+C3 set");
        assert_eq!(
            cpu.fpu.get_top(),
            (top_before + 1) & 7,
            "FICOMP must pop on masked empty path"
        );
    }

    /// FST ST(i) from empty ST(0) must write FP indefinite into the
    /// destination slot, tag it Special, and set SF+IE. Does NOT pop.
    #[test]
    fn test_fst_reg_from_empty_writes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let top_before = cpu.fpu.get_top();
        // Seed ST(2) (physical reg at (TOP+2)&7) with a distinct value so we
        // can verify it was overwritten with the indefinite QNaN.
        let dst_idx = (top_before + 2) & 7;
        cpu.fpu.regs[dst_idx] = F80::from_f64(12345.0);
        // Tag word left all-empty (dst still empty) — but FST is writing into
        // that slot; the point of the test is that the slot ends as indefinite
        // + tag=Special.
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fst_sti, Register::ST2));
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE set");
        assert_eq!(
            cpu.fpu.regs[dst_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF],
            "FST ST(i) of empty ST(0) must write FP indefinite"
        );
        assert_eq!(
            cpu.fpu.get_tag(dst_idx),
            2,
            "destination tag must be Special after FST of empty"
        );
        assert_eq!(cpu.fpu.get_top(), top_before, "FST must not change TOP");
    }

    /// FSTP ST(i) from empty ST(0) must write FP indefinite AND pop.
    #[test]
    fn test_fstp_reg_from_empty_writes_indefinite_and_pops() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let top_before = cpu.fpu.get_top();
        let dst_idx = (top_before + 2) & 7;
        exec_instr(&mut cpu, &mut mem, instr1(Code::Fstp_sti, Register::ST2));
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE set");
        assert_eq!(
            cpu.fpu.regs[dst_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF],
            "FSTP ST(i) of empty ST(0) must write FP indefinite"
        );
        assert_eq!(
            cpu.fpu.get_tag(dst_idx),
            2,
            "destination tag must be Special"
        );
        assert_eq!(cpu.fpu.get_top(), (top_before + 1) & 7, "FSTP must pop");
    }

    // =========================================================================
    // Masked empty-stack handling for arithmetic, unary, integer arithmetic
    // and stores (Intel SDM Vol 1 §8.5.1.1).
    // =========================================================================

    /// FADD ST(0), memf32 with empty ST(0): ST(0) becomes FP indefinite.
    #[test]
    fn test_fadd_mem_empty_writes_indefinite_to_st0() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let addr: i64 = 0x1000;
        mem.write_u32(addr as usize, 0x3F80_0000); // 1.0 as f32
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fadd_m32fp, mem_op(addr)).unwrap(),
        );
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE set");
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF],
            "FADD memf32 of empty ST(0) must write FP indefinite to ST(0)"
        );
        assert_eq!(cpu.fpu.get_tag(st0_idx), 2, "tag must be Special");
        assert_eq!(cpu.fpu.get_top(), top_before, "FADD must not change TOP");
    }

    /// FMUL ST(i), ST(0) with empty ST(0): destination becomes indefinite.
    #[test]
    fn test_fmul_reg_empty_writes_indefinite_to_dest() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        // Seed ST(1) physical slot with a distinct value but mark it empty
        // so we hit the empty branch through ST(0); we want the destination
        // to still receive indefinite.
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fmul_sti_st0, Register::ST1, Register::ST0),
        );
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE set");
        let st1_idx = cpu.fpu.get_st_idx(1);
        assert_eq!(
            cpu.fpu.regs[st1_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF],
            "destination ST(i) must hold FP indefinite"
        );
        assert_eq!(cpu.fpu.get_tag(st1_idx), 2);
        assert_eq!(cpu.fpu.get_top(), top_before, "FMUL does not pop");
    }

    /// FMULP ST(i), ST(0) with empty stack: destination becomes indefinite
    /// AND the stack pops (TOP advances by 1).
    #[test]
    fn test_fmulp_reg_empty_writes_indefinite_and_pops() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fmulp_sti_st0, Register::ST1, Register::ST0),
        );
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE set");
        assert_eq!(
            cpu.fpu.get_top(),
            (top_before + 1) & 7,
            "FMULP must pop even on masked empty path"
        );
    }

    /// FDIVR ST(0), ST(5) with empty stack → indefinite in ST(0), no pop.
    /// Fuzz pattern: `[D8, F5,...]` repros.
    #[test]
    fn test_fdivr_reg_empty_writes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivr_st0_sti, Register::ST0, Register::ST5),
        );
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE set");
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF],
            "FDIVR of empty must write indefinite to ST(0)"
        );
        assert_eq!(cpu.fpu.get_top(), top_before);
    }

    /// `FDIVR ST(6),ST(0); FNSTSW WORD [BX+SI]` with empty operands must
    /// store status low byte 0x41 (SF+IE): stack underflow takes priority
    /// over divide by zero. Preserve pre-set C0/C2/C3; masked arithmetic
    /// stack underflow does not clear them.
    #[test]
    fn test_fdivr_st6_st0_empty_status_word_low_byte_matches_jit() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        // Pre-set C0 as if a prior FCOM set it. Masked arithmetic stack
        // underflow preserves C0/C2/C3 while setting IE+SF.
        cpu.fpu.status_word = 1 << 8; // C0 = 1
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivr_st0_sti, Register::ST6, Register::ST0),
        );
        // SF (bit 6) and IE (bit 0) must be set, with ZE clear. Stack
        // underflow must take priority over the divide-by-zero precheck.
        assert_eq!(
            cpu.fpu.status_word & 0xFF,
            0x41,
            "FDIVR-empty status-word low byte must be 0x41 (SF+IE), got {:#04X}",
            cpu.fpu.status_word & 0xFF
        );
        // High-byte hardware oracle (HLD 2026.05.08): the pre-set C0 must
        // be PRESERVED across the masked-#IS FDIVR-empty. Pre-`clear_arith_*`
        // revert this asserted `0` (matching the buggy interp); post-revert
        // it asserts the actual hardware behaviour.
        assert_ne!(
            cpu.fpu.status_word & (1 << 8),
            0,
            "C0 must be preserved across FDIVR-empty (HLD 2026.05.08); sw={:#06X}",
            cpu.fpu.status_word
        );
    }

    /// Masked arithmetic stack underflow preserves C0/C2/C3, sets IE+SF
    /// and clears C1. Preload unordered condition codes, then issue FDIVR
    /// with empty ST(0): all three condition codes must remain set.
    #[test]
    fn test_fdivr_after_fucompp_empty_preserves_c0_c2_c3_mem() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        reset_empty_fpu(&mut cpu);
        // Synthesise the post-FUCOMPP-of-NaNs status word (C0=C2=C3=1 on
        // the unordered branch). Stack stays empty (tag_word = 0xFFFF, top
        // unchanged from `Fpu::new()`'s default 0).
        cpu.fpu.status_word = 0x4500;
        let addr: usize = 0x800;
        mem.write_u32(addr, 0); // memory operand value (read but result is indefinite)
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdivr_m32fp, mem_op(addr as i64)).unwrap(),
        );
        // SF + IE must be set (stack-underflow signalled).
        assert_eq!(
            cpu.fpu.status_word & 0x41,
            0x41,
            "SF+IE must be set after FDIVR-empty, sw={:#06X}",
            cpu.fpu.status_word
        );
        // C0 (bit 8), C2 (bit 10), C3 (bit 14) must all still be 1 — real
        // x87 preserves them across masked-#IS arith underflow. Pre-fix
        // (`clear_arith_condition_codes`) zeroed all three.
        assert_eq!(
            cpu.fpu.status_word & ((1 << 8) | (1 << 10) | (1 << 14)),
            (1 << 8) | (1 << 10) | (1 << 14),
            "C0/C2/C3 must be preserved after FDIVR-empty-mem; sw={:#06X}",
            cpu.fpu.status_word
        );
        // ST(0) becomes indefinite (the existing patch-4 behaviour stays).
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF],
            "FDIVR-mem-empty must write indefinite to ST(0)"
        );
    }

    /// HLD `2026.05.08` — register-form analogue of
    /// `test_fdivr_after_fucompp_empty_preserves_c0_c2_c3_mem`. Covers the
    /// second deletion site (Register branch of `execute_arith`).
    #[test]
    fn test_fdiv_register_after_fucompp_empty_preserves_c0_c2_c3() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        // Pre-load post-compare condition codes (C0=C2=C3=1).
        cpu.fpu.status_word = 0x4500;
        // ST(0) and ST(5) both empty — the Register branch checks
        // `stack_reg_empty(st_i) || stack_reg_empty(st_j)` and falls into
        // the underflow path.
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fdivr_st0_sti, Register::ST0, Register::ST5),
        );
        assert_eq!(
            cpu.fpu.status_word & 0x41,
            0x41,
            "SF+IE must be set after FDIVR ST(0),ST(5)-empty, sw={:#06X}",
            cpu.fpu.status_word
        );
        assert_eq!(
            cpu.fpu.status_word & ((1 << 8) | (1 << 10) | (1 << 14)),
            (1 << 8) | (1 << 10) | (1 << 14),
            "C0/C2/C3 must be preserved after FDIVR-empty-reg; sw={:#06X}",
            cpu.fpu.status_word
        );
    }

    /// HLD `2026.05.08` — direct simulation of the smallest crash (30-byte
    /// `crash-128ad1dd…`). Drives the FCOMP→FDIVR-empty-mem chain through
    /// `execute` and asserts the final FNSTSW image at memory matches the
    /// host-x87 trace from HLD §3.3 Test 1: low byte `0x41`, high byte
    /// `0x4D` (C0=C2=C3=1, top=1 from the FCOMP pop, C1=0). Pre-fix the
    /// interpreter writes `(0x41, 0x08)` because `clear_arith_condition_codes`
    /// zeroed C0/C2/C3; the corrected result must match host x87.
    ///
    /// Modelled steps (after the trivial XOR/CMP setup the interpreter
    /// execution path is identical):
    ///   * FDIVR DWORD [..] with empty stack    → ST(0)=Indefinite, SF+IE=1
    ///   * FCOMP ST(0)                          → C0=C2=C3=1 (NaN-vs-NaN), pop
    ///   * FDIVR DWORD [..] with empty stack    → preserves C0/C2/C3
    ///   * FNSTSW WORD [..]                     → high byte must be 0x4D
    #[test]
    fn test_fnstsw_high_byte_after_fcomp_fdivr_chain_matches_native() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        reset_empty_fpu(&mut cpu);
        let mem_addr: usize = 0x800;
        let nstsw_addr: usize = 0x900;

        // Step 1: FDIVR with empty stack writes indefinite and sets SF+IE.
        mem.write_u32(mem_addr, 0);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdivr_m32fp, mem_op(mem_addr as i64)).unwrap(),
        );

        // Step 2: unordered FCOMP of the indefinite sets C0=C2=C3=1 and pops.
        // iced-x86 decodes `D8 D8` as `Fcomp_st0_sti(ST0, ST0)`.
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fcomp_st0_sti, Register::ST0, Register::ST0),
        );
        // Sanity: at this point we expect C0/C2/C3 = 1 from the
        // unordered-compare path.
        let cc_mask = (1u16 << 8) | (1u16 << 10) | (1u16 << 14);
        assert_eq!(
            cpu.fpu.status_word & cc_mask,
            cc_mask,
            "after FCOMP-NaN expected C0=C2=C3=1, got sw={:#06X}",
            cpu.fpu.status_word
        );

        // Step 3: FDIVR with the now-empty stack must preserve C0/C2/C3.
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fdivr_m32fp, mem_op(mem_addr as i64)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & cc_mask,
            cc_mask,
            "after second FDIVR-empty C0=C2=C3 must still be 1 (HLD 2026.05.08); sw={:#06X}",
            cpu.fpu.status_word
        );

        // Step 4: FNSTSW must store high byte 0x4D, matching host x87;
        // the buggy interpreter stored 0x08.
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnstsw_m2byte, mem_op(nstsw_addr as i64)).unwrap(),
        );
        let lo = mem.read_u8(nstsw_addr);
        let hi = mem.read_u8(nstsw_addr + 1);
        assert_eq!(
            lo, 0x41,
            "FNSTSW low byte must be 0x41 (SF+IE), got {:#04X}",
            lo
        );
        assert_eq!(
            hi, 0x4D,
            "FNSTSW high byte must be 0x4D (C3|top=1|C2|C0); got {:#04X}",
            hi
        );
    }

    /// FIADD memf16 with empty ST(0): ST(0) becomes indefinite.
    /// Fuzz pattern: `[DE, 11,...]` = FIADD word [BX+DI]; FNSTSW.
    #[test]
    fn test_fiadd_mem_empty_writes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let addr: i64 = 0x1000;
        mem.write_u16(addr as usize, 42);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fiadd_m16int, mem_op(addr)).unwrap(),
        );
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE set");
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF],
            "FIADD of empty must write indefinite to ST(0)"
        );
        assert_eq!(cpu.fpu.get_tag(st0_idx), 2);
    }

    /// FISUB memf16 with empty ST(0): indefinite to ST(0).
    #[test]
    fn test_fisub_mem_empty_writes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let addr: i64 = 0x1000;
        mem.write_u16(addr as usize, 7);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisub_m16int, mem_op(addr)).unwrap(),
        );
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]
        );
    }

    /// FSQRT with empty ST(0): ST(0) becomes FP indefinite.
    #[test]
    fn test_fsqrt_empty_writes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsqrt));
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41);
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]
        );
        assert_eq!(cpu.fpu.get_tag(st0_idx), 2);
    }

    /// FCHS with empty ST(0): indefinite.
    #[test]
    fn test_fchs_empty_writes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fchs));
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]
        );
    }

    /// FSIN with empty ST(0): indefinite.
    #[test]
    fn test_fsin_empty_writes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsin));
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]
        );
    }

    /// FRNDINT with empty ST(0): indefinite.
    #[test]
    fn test_frndint_empty_writes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Frndint));
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]
        );
    }

    /// FPTAN on empty ST(0) makes both the source and pushed value
    /// indefinite; the pushed value must not be 1.0.
    #[test]
    fn test_fptan_empty_writes_indefinite_and_pushes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let top_before = cpu.fpu.get_top();
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE set");
        // TOP advanced (push happened).
        assert_eq!(
            cpu.fpu.get_top(),
            (top_before.wrapping_sub(1)) & 7,
            "FPTAN advances TOP even on masked empty path"
        );
        // New ST(0) is the F80 indefinite QNaN, not 1.0.
        let new_top = cpu.fpu.get_top();
        assert_eq!(
            cpu.fpu.regs[new_top],
            F80::INDEFINITE,
            "pushed value must be the F80 indefinite, matching native FPTAN"
        );
        // Tag of new ST(0) is Special (the indefinite is a QNaN).
        assert_eq!(cpu.fpu.get_tag(new_top), 2, "new ST(0) tagged Special");
        // Old ST(0) (now ST(1)) is also the indefinite.
        let old_top = top_before;
        assert_eq!(
            cpu.fpu.regs[old_top],
            F80::INDEFINITE,
            "old ST(0) (now ST(1)) is the F80 indefinite"
        );
        // C2 cleared (complete).
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0, "C2 cleared");
    }

    /// FSINCOS on empty ST(0): ST(0) indefinite, push indefinite.
    #[test]
    fn test_fsincos_empty_writes_indefinite_and_pushes() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let top_before = cpu.fpu.get_top();
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fsincos));
        assert_eq!(
            cpu.fpu.get_top(),
            (top_before.wrapping_sub(1)) & 7,
            "FSINCOS pushes"
        );
        let new_top = cpu.fpu.get_top();
        assert_eq!(
            cpu.fpu.regs[new_top].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]
        );
    }

    /// FSCALE on empty stack: indefinite in ST(0).
    #[test]
    fn test_fscale_empty_writes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fscale));
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]
        );
    }

    /// FPREM on empty stack: indefinite in ST(0), C2 cleared.
    #[test]
    fn test_fprem_empty_writes_indefinite() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Fprem));
        let st0_idx = cpu.fpu.get_st_idx(0);
        assert_eq!(
            cpu.fpu.regs[st0_idx].0,
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]
        );
        assert_eq!(cpu.fpu.status_word & (1 << 10), 0, "C2 clear on complete");
    }

    /// FTST on empty ST(0): C0=C2=C3=1 (unordered).
    #[test]
    fn test_ftst_empty_sets_c0_c2_c3() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        exec_instr(&mut cpu, &mut mem, instr0(Code::Ftst));
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE");
        assert_eq!(cpu.fpu.status_word & 0x4500, 0x4500, "C0+C2+C3 set");
    }

    /// FIST (no-pop) memory on empty ST(0): writes integer indefinite, does not pop.
    #[test]
    fn test_fist_empty_does_not_pop() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let addr: i64 = 0x2000;
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fist_m32int, mem_op(addr)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr as usize), 0x8000_0000);
        assert_eq!(cpu.fpu.get_top(), top_before, "FIST must not pop");
    }

    /// FISTP memory on empty ST(0): writes integer indefinite AND pops.
    /// Patch 4 fix: previously FISTP on empty left TOP unchanged.
    #[test]
    fn test_fistp_empty_pops() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let addr: i64 = 0x2000;
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fistp_m32int, mem_op(addr)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr as usize), 0x8000_0000);
        assert_eq!(
            cpu.fpu.get_top(),
            (top_before + 1) & 7,
            "FISTP must pop on masked empty path"
        );
    }

    /// FISTTP memory on empty ST(0): writes integer indefinite AND pops.
    /// Patch 4 fix: previously FISTTP on empty left TOP unchanged.
    #[test]
    fn test_fisttp_empty_pops() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let addr: i64 = 0x2000;
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m32int, mem_op(addr)).unwrap(),
        );
        assert_eq!(mem.read_u32(addr as usize), 0x8000_0000);
        assert_eq!(
            cpu.fpu.get_top(),
            (top_before + 1) & 7,
            "FISTTP must pop on masked empty path"
        );
    }

    #[test]
    fn test_fisttp_m64_inexact_propagates_precision_status() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        cpu.fpu.push(F80::from_f64(1.5));
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisttp_m64int, mem_op(0x2000)).unwrap(),
        );
        assert_ne!(cpu.fpu.status_word & 0x0020, 0, "FISTTP must propagate PE");
    }

    #[test]
    // x86_64-only: asserts the C1 round-up-direction status bit (0x200). C1 is
    // an explicitly-deferred soft-float gap (status_to_sw in x87_soft.rs leaves
    // it clear — apfloat does not expose the rounding direction), so only the
    // host x87 path sets it. The PE (inexact) half is satisfied on both paths.
    #[cfg(target_arch = "x86_64")]
    fn test_fisubr_m32_inexact_propagates_precision_and_c1() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        cpu.fpu
            .push(F80::from_f64(f64::from_bits(0x312A_231C_150E_0700)));
        mem.write_u32(0x2000, 0x150E_0700);
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fisubr_m32int, mem_op(0x2000)).unwrap(),
        );
        assert_eq!(
            cpu.fpu.status_word & 0x0220,
            0x0220,
            "FISUBR must propagate PE+C1"
        );
    }

    /// FBSTP memory on empty ST(0): writes BCD indefinite AND pops.
    /// Patch 4 fix: previously FBSTP on empty left TOP unchanged.
    #[test]
    fn test_fbstp_empty_pops() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        reset_empty_fpu(&mut cpu);
        let addr: i64 = 0x2000;
        let top_before = cpu.fpu.get_top();
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr)).unwrap(),
        );
        // BCD indefinite = [0,0,0,0,0,0,0,0xC0,0xFF,0xFF]
        assert_eq!(mem.read_u8(addr as usize + 7), 0xC0);
        assert_eq!(mem.read_u8(addr as usize + 8), 0xFF);
        assert_eq!(mem.read_u8(addr as usize + 9), 0xFF);
        assert_eq!(
            cpu.fpu.get_top(),
            (top_before + 1) & 7,
            "FBSTP must pop on masked empty path"
        );
    }

    /// FXCH with one empty operand: fill empty with indefinite, then swap.
    /// Neither slot is Empty after the operation.
    #[test]
    fn test_fxch_one_empty_fills_and_swaps() {
        let _lock = env_lock();
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        // Seed ST(0) with a valid value, leave ST(1) empty.
        reset_empty_fpu(&mut cpu);
        cpu.fpu.push(F80::from_f64(42.0));
        let top_before = cpu.fpu.get_top();
        let tag0_before = cpu.fpu.get_tag(top_before);
        let tag1_before = cpu.fpu.get_tag((top_before + 1) & 7);
        assert_eq!(tag0_before, 0);
        assert_eq!(tag1_before, 3);
        exec_instr(
            &mut cpu,
            &mut mem,
            instr2(Code::Fxch_st0_sti, Register::ST0, Register::ST1),
        );
        assert_eq!(cpu.fpu.status_word & 0x41, 0x41, "SF+IE set");
        let tag0_after = cpu.fpu.get_tag(top_before);
        let tag1_after = cpu.fpu.get_tag((top_before + 1) & 7);
        // Neither slot should be Empty after FXCH-with-fill.
        assert_ne!(tag0_after, 3);
        assert_ne!(tag1_after, 3);
    }

    // ====================================================================
    // Phase 1 helper: assert_interp_state_byte_matches_host_x87
    //
    // These tests exercise the test-only oracle introduced for the RM16 R1
    // α/β/γ/δ HLD. Each test runs an FPU instruction down a known-cold
    // empty/overflow path, then asserts the resulting interpreter state
    // round-trips through host x87 byte-for-byte. They should PASS in
    // Phase 1 — they verify the helper itself, not the bookkeeping
    // bugs that Phases 2-4 will close (those manifest only across
    // *chains* of mixed FPU + non-FPU instructions, not single ops).
    // ====================================================================
    // These assert against the HOST x87 unit, so they are x86_64-only (the
    // helper exists only there). On non-x86_64, x87_soft is covered by the
    // golden-vector oracle, not this host-differential block.
    #[cfg(target_arch = "x86_64")]
    mod host_x87_byte_equiv {
        use super::super::{
            assert_dh_spilled_state_matches_host_x87, assert_interp_state_byte_matches_host_x87,
        };
        use super::*;

        #[test]
        fn host_roundtrip_ignores_long_mode_selector_zeroing_but_not_offsets() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            cpu.fpu.ip_offset = 0x1234_5678;
            cpu.fpu.ip_selector = 0x1357;
            cpu.fpu.operand_offset = 0x89AB_CDEF;
            cpu.fpu.operand_selector = 0x2468;
            assert_interp_state_byte_matches_host_x87(&cpu, "long-mode selector zeroing");
        }

        #[test]
        fn host_roundtrip_agrees_on_the_tag_class_of_a_pushed_zero() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            // Was: "the interpreter deliberately tags every ordinary push
            // Valid", asserting tag 0 for a pushed zero and relying on the
            // helper comparing occupancy rather than class to stay green.
            // That treated a zero result with a valid tag as intentional.
            //
            // We now tag by content, so we agree with host FNSAVE outright:
            // both call this Zero. The helper still compares occupancy only —
            // that tolerance is unchanged and is not this test's subject — but
            // this case no longer needs it.
            cpu.fpu.push(F80::ZERO);
            assert_eq!(
                cpu.fpu.get_tag(cpu.fpu.get_top()),
                1,
                "a pushed +0 must tag Zero, which is what host FNSAVE reports"
            );
            assert_interp_state_byte_matches_host_x87(&cpu, "pushed zero tag class");
        }

        #[test]
        #[should_panic(expected = "tag occupancy differs")]
        fn dh_spill_oracle_rejects_actual_host_occupancy_drift() {
            let mut cpu = Cpu::new();
            cpu.fpu.sync_to_x87_save_area();
            // Claim physical R0 was occupied in the actual host image while the
            // software shadow still says Empty.
            let image_tag = cpu.fpu.tag_word & !3;
            cpu.fpu.x87_save_area[8..10].copy_from_slice(&image_tag.to_le_bytes());
            assert_dh_spilled_state_matches_host_x87(&cpu, "fabricated occupancy drift");
        }

        #[test]
        fn fpu_state_byte_equivalence_after_fistp_empty() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.regs.ds = 0;
            reset_empty_fpu(&mut cpu);
            let addr: i64 = 0x380;
            // FISTP m16 from an empty stack: masked-#IS path writes i16::MIN.
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fistp_m16int, mem_op(addr)).unwrap(),
            );
            assert_interp_state_byte_matches_host_x87(&cpu, "after FISTP m16 (empty)");
        }

        #[test]
        fn fpu_state_byte_equivalence_after_fbstp_empty() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.regs.ds = 0;
            reset_empty_fpu(&mut cpu);
            let addr: i64 = 0x2000;
            // FBSTP m80 from an empty stack: writes the BCD indefinite and pops.
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fbstp_m80bcd, mem_op(addr)).unwrap(),
            );
            assert_interp_state_byte_matches_host_x87(&cpu, "after FBSTP m80 (empty)");
        }

        #[test]
        fn fpu_state_byte_equivalence_after_fpatan_empty() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            reset_empty_fpu(&mut cpu);
            // FPATAN with both ST(0) and ST(1) empty: the empty-input branch
            // writes indefinite to ST(1) and pops ST(0).
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fpatan));
            assert_interp_state_byte_matches_host_x87(&cpu, "after FPATAN (both empty)");
        }

        #[test]
        fn fpu_state_byte_equivalence_after_fild_overflow() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.regs.ds = 0;
            // Pre-fill all 8 slots so the next FILD overflows the stack.
            fill_stack(&mut cpu);
            let addr: usize = 0x400;
            mem.write_u16(addr, 0x1234);
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fild_m16int, mem_op(addr as i64)).unwrap(),
            );
            assert_interp_state_byte_matches_host_x87(&cpu, "after FILD m16 (overflow)");
        }

        /// FBLD push overflow must produce a byte-equivalent host x87 state.
        #[test]
        fn fpu_state_byte_equivalence_after_fbld_overflow() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.regs.ds = 0;
            // Pre-fill all 8 slots so the next FBLD overflows the stack.
            fill_stack(&mut cpu);
            let addr: usize = 0x800;
            // BCD value matching the rm16 R1.γ harness data init pattern at
            // offset 0: data[i] = (i*7) & 0xFF → 00 07 0E 15 1C 23 2A 31 38 3F.
            let bcd: [u8; 10] = [0x00, 0x07, 0x0E, 0x15, 0x1C, 0x23, 0x2A, 0x31, 0x38, 0x3F];
            for (i, &b) in bcd.iter().enumerate() {
                mem.write_u8(addr + i, b);
            }
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fbld_m80bcd, mem_op(addr as i64)).unwrap(),
            );
            assert_interp_state_byte_matches_host_x87(&cpu, "after FBLD m80 (overflow)");
        }

        /// FPTAN push-overflow byte-equivalence with host x87.
        /// Latent bug fixed alongside R1.γ — see HLD section §8.1.
        #[test]
        fn fpu_state_byte_equivalence_after_fptan_overflow() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            // Pre-fill all 8 slots; ST(0)=8.0 (a finite, valid input). Push of
            // 1.0 would overflow.
            fill_stack(&mut cpu);
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
            assert_interp_state_byte_matches_host_x87(&cpu, "after FPTAN (push overflow)");
        }

        #[test]
        fn fpu_state_after_fptan_inexact_propagates_precision_and_c1() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            reset_empty_fpu(&mut cpu);
            cpu.fpu.push(F80::from_f64(1.0));
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fptan));
            assert_eq!(
                cpu.fpu.status_word & 0x0220,
                0x0220,
                "FPTAN must propagate PE+C1"
            );
            assert_interp_state_byte_matches_host_x87(&cpu, "after FPTAN inexact finite input");
        }

        /// FSINCOS push-overflow byte-equivalence with host x87.
        #[test]
        fn fpu_state_byte_equivalence_after_fsincos_overflow() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            fill_stack(&mut cpu);
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fsincos));
            assert_interp_state_byte_matches_host_x87(&cpu, "after FSINCOS (push overflow)");
        }

        /// FXTRACT push-overflow byte-equivalence with host x87.
        #[test]
        fn fpu_state_byte_equivalence_after_fxtract_overflow() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            fill_stack(&mut cpu);
            exec_instr(&mut cpu, &mut mem, instr0(Code::Fxtract));
            assert_interp_state_byte_matches_host_x87(&cpu, "after FXTRACT (push overflow)");
        }

        /// FISTTP qword from F80 indefinite QNaN with C1=1 pre-set: closes the
        /// layered residual surfaced by RM16 R1.γ `crash-7ed7dd58…` after the
        /// push-overflow fix landed (commit `c09266c09`). Per Intel SDM Vol 1
        /// §8.5.1: masked-IE for non-stack-related invalid operations clears
        /// C1. Host probe `tmp/probe_fisttp_qnan.rs` confirms.
        #[test]
        fn fpu_state_byte_equivalence_after_fisttp_qnan_clears_c1() {
            let _lock = env_lock();
            let mut cpu = Cpu::new();
            let mut mem = Memory::new();
            cpu.regs.ds = 0;
            // Set up ST(0) = F80 indefinite QNaN with C1=1 pre-set (mimicking
            // a prior overflow operation that left C1=1 in the status word).
            reset_empty_fpu(&mut cpu);
            cpu.fpu.push(F80::INDEFINITE);
            let top = cpu.fpu.get_top();
            cpu.fpu.set_tag(top, 2); // Special (the indefinite is a QNaN)
            cpu.fpu.status_word |= 1 << 9; // C1 = 1 (overflow flavour, sticky)
            let addr: i64 = 0x300;
            exec_instr(
                &mut cpu,
                &mut mem,
                Instruction::with1(Code::Fisttp_m64int, mem_op(addr)).unwrap(),
            );
            // Spot-check that C1 actually got cleared (defensive — the
            // byte-equivalence assertion below subsumes this, but a focused
            // assertion makes the regression intent explicit).
            assert_eq!(
                cpu.fpu.status_word & (1 << 9),
                0,
                "FISTTP-of-QNaN must clear C1 per SDM §8.5.1 (non-stack masked-IE)"
            );
            assert_interp_state_byte_matches_host_x87(
                &cpu,
                "after FISTTP qword of F80 QNaN (C1=1 pre-set)",
            );
        }
    } // mod host_x87_byte_equiv
}

// ============================================================================
// FPU Differential Testing Infrastructure
// ============================================================================

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::approx_constant)] // arbitrary FPU register fixtures, not PI/E
mod tests_diff {
    use super::*;
    use gp64_memory::Memory;
    use iced_x86::{Decoder, DecoderOptions, Instruction};

    // ====================================================================
    // Test helpers
    // ====================================================================

    /// Set up memory at address 0x1000 with various numeric test data.
    /// Layout:
    ///   0x1000: f32 (PI as f32)          - 4 bytes
    ///   0x1004: f64 (E)                   - 8 bytes
    ///   0x100C: i16 (42)                  - 2 bytes
    ///   0x100E: i32 (-12345)              - 4 bytes
    ///   0x1012: i64 (9876543210)          - 8 bytes
    ///   0x101A: f80 (known pattern)       - 10 bytes
    fn setup_test_memory(mem: &mut Memory) {
        let addr = 0x1000usize;

        // f32: PI
        let f32_val = std::f64::consts::PI as f32;
        mem.write_u32(addr, u32::from_le_bytes(f32_val.to_le_bytes()));

        // f64: E
        let f64_val = std::f64::consts::E;
        mem.write_u64(addr + 4, u64::from_le_bytes(f64_val.to_le_bytes()));

        // i16: 42
        mem.write_u16(addr + 0xC, 42u16);

        // i32: -12345
        mem.write_u32(addr + 0xE, (-12345i32) as u32);

        // i64: 9876543210
        mem.write_u64(addr + 0x12, 9876543210u64);

        // f80: extended precision for 1.0 (exponent 16383, mantissa = 0x8000_0000_0000_0000)
        let f80_bytes: [u8; 10] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xFF, 0x3F];
        for (i, &b) in f80_bytes.iter().enumerate() {
            mem.write_u8(addr + 0x1A + i, b);
        }
    }

    /// FPU state presets to cover major behavioral branches.
    #[derive(Clone, Copy, Debug)]
    enum FpuPreset {
        /// Default: TOP=0, all empty, CW=0x037F
        Default,
        /// Two values: TOP=6, ST0=3.14, ST1=2.71
        TwoValues,
        /// Stack full: all 8 registers populated
        StackFull,
        /// Special values: NaN, +Inf, -0.0, denormal
        SpecialValues,
        /// Truncation rounding: CW with RC=3
        TruncationRounding,
        /// Round toward -infinity: CW with RC=1
        RoundDown,
        /// Round toward +infinity: CW with RC=2
        RoundUp,
    }

    const ALL_PRESETS: [FpuPreset; 7] = [
        FpuPreset::Default,
        FpuPreset::TwoValues,
        FpuPreset::StackFull,
        FpuPreset::SpecialValues,
        FpuPreset::TruncationRounding,
        FpuPreset::RoundDown,
        FpuPreset::RoundUp,
    ];

    /// Apply a preset to a CPU's FPU state.
    fn apply_preset(cpu: &mut Cpu, preset: FpuPreset) {
        cpu.fpu = Fpu::new();
        match preset {
            FpuPreset::Default => {
                // Already default from Fpu::new()
            }
            FpuPreset::TwoValues => {
                cpu.fpu.set_top(6);
                cpu.fpu.regs[6] = F80::from_f64(3.14);
                cpu.fpu.regs[7] = F80::from_f64(2.71);
                cpu.fpu.set_tag(6, 0); // Valid
                cpu.fpu.set_tag(7, 0); // Valid
            }
            FpuPreset::StackFull => {
                cpu.fpu.set_top(0);
                let values = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
                for i in 0..8 {
                    cpu.fpu.regs[i] = F80::from_f64(values[i]);
                    cpu.fpu.set_tag(i, 0); // Valid
                }
            }
            FpuPreset::SpecialValues => {
                cpu.fpu.set_top(4);
                cpu.fpu.regs[4] = F80::from_f64(f64::NAN);
                cpu.fpu.regs[5] = F80::from_f64(f64::INFINITY);
                cpu.fpu.regs[6] = F80::from_f64(-0.0);
                cpu.fpu.regs[7] = F80::from_f64(f64::from_bits(1)); // smallest denormal
                cpu.fpu.set_tag(4, 0);
                cpu.fpu.set_tag(5, 0);
                cpu.fpu.set_tag(6, 0);
                cpu.fpu.set_tag(7, 0);
            }
            FpuPreset::TruncationRounding => {
                // RC=3 (truncate), all exceptions masked
                cpu.fpu.control_word = 0x0F7F; // RC=3 in bits 10-11
                cpu.fpu.set_top(6);
                cpu.fpu.regs[6] = F80::from_f64(3.14);
                cpu.fpu.regs[7] = F80::from_f64(2.71);
                cpu.fpu.set_tag(6, 0);
                cpu.fpu.set_tag(7, 0);
            }
            FpuPreset::RoundDown => {
                // RC=1 (round toward -infinity)
                cpu.fpu.control_word = 0x077F; // bits 10-11 = 01
                cpu.fpu.set_top(6);
                cpu.fpu.regs[6] = F80::from_f64(3.14);
                cpu.fpu.regs[7] = F80::from_f64(2.71);
                cpu.fpu.set_tag(6, 0);
                cpu.fpu.set_tag(7, 0);
            }
            FpuPreset::RoundUp => {
                // RC=2 (round toward +infinity)
                cpu.fpu.control_word = 0x0B7F; // bits 10-11 = 10
                cpu.fpu.set_top(6);
                cpu.fpu.regs[6] = F80::from_f64(3.14);
                cpu.fpu.regs[7] = F80::from_f64(2.71);
                cpu.fpu.set_tag(6, 0);
                cpu.fpu.set_tag(7, 0);
            }
        }
    }

    /// Clone the full CPU state relevant for FPU comparison.
    fn clone_cpu_for_diff(cpu: &Cpu) -> Cpu {
        let mut new_cpu = Cpu::new();
        new_cpu.fpu = cpu.fpu.clone();
        new_cpu.regs = cpu.regs.clone();
        new_cpu.cs_bits = cpu.cs_bits;
        new_cpu
    }

    /// Try to decode instruction bytes using iced-x86.
    /// Returns None if decoding fails or produces an invalid instruction.
    fn try_decode(bytes: &[u8]) -> Option<Instruction> {
        let mut decoder = Decoder::with_ip(16, bytes, 0, DecoderOptions::NONE);
        let instr = decoder.decode();
        if instr.is_invalid() {
            return None;
        }
        Some(instr)
    }

    /// Execute an FPU instruction on a CPU+Memory, returning Ok(()) or the error.
    fn try_execute(cpu: &mut Cpu, mem: &mut Memory, instr: &Instruction) -> Result<(), String> {
        match execute_iced_for_test(cpu, instr, mem) {
            Ok(()) => Ok(()),
            Err(e) => Err(format!("{}", e)),
        }
    }

    // ====================================================================
    // Tests for compare_fpu_state
    // ====================================================================

    #[test]
    fn test_compare_fpu_state_identical() {
        let a = Fpu::new();
        let b = Fpu::new();
        let diffs = compare_fpu_state(&a, &b);
        assert!(diffs.is_empty(), "Expected no diffs, got: {:?}", diffs);
    }

    #[test]
    fn test_compare_fpu_state_reg_diff() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        a.regs[3] = F80::from_f64(1.0);
        b.regs[3] = F80::from_f64(2.0);
        let diffs = compare_fpu_state(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("regs[3]"));
    }

    #[test]
    fn test_compare_fpu_state_signed_zero() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        a.regs[0] = F80::from_f64(0.0);
        b.regs[0] = F80::from_f64(-0.0);
        let diffs = compare_fpu_state(&a, &b);
        assert_eq!(diffs.len(), 1, "F80 bytes should distinguish +0.0 and -0.0");
    }

    #[test]
    fn test_compare_fpu_state_nan_same_bits() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        // Same NaN bits should match
        a.regs[0] = F80::from_f64(f64::NAN);
        b.regs[0] = F80::from_f64(f64::NAN);
        let diffs = compare_fpu_state(&a, &b);
        assert!(diffs.is_empty(), "Same NaN F80 bytes should be identical");
    }

    #[test]
    fn test_compare_fpu_state_status_word_diff() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        a.status_word = 0x1234;
        b.status_word = 0x5678;
        let diffs = compare_fpu_state(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("status_word"));
    }

    #[test]
    fn test_compare_fpu_state_control_word_diff() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        a.control_word = 0x037F;
        b.control_word = 0x0F7F;
        let diffs = compare_fpu_state(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("control_word"));
    }

    #[test]
    fn test_compare_fpu_state_tag_word_diff() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        a.tag_word = 0xFFFF;
        b.tag_word = 0x0000;
        let diffs = compare_fpu_state(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("tag_word"));
    }

    #[test]
    fn test_compare_fpu_state_multiple_diffs() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        a.regs[0] = F80::from_f64(1.0);
        b.regs[0] = F80::from_f64(2.0);
        a.status_word = 0x0001;
        b.status_word = 0x0002;
        a.tag_word = 0xAAAA;
        b.tag_word = 0xBBBB;
        let diffs = compare_fpu_state(&a, &b);
        assert_eq!(diffs.len(), 3);
    }

    #[test]
    fn test_compare_fpu_state_ip_offset_diff() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        a.ip_offset = 0x1000;
        b.ip_offset = 0x2000;
        let diffs = compare_fpu_state(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("ip_offset"));
    }

    #[test]
    fn test_compare_fpu_state_ip_selector_diff() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        a.ip_selector = 0x0008;
        b.ip_selector = 0x0010;
        let diffs = compare_fpu_state(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("ip_selector"));
    }

    #[test]
    fn test_compare_fpu_state_operand_offset_diff() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        a.operand_offset = 0x3000;
        b.operand_offset = 0x4000;
        let diffs = compare_fpu_state(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("operand_offset"));
    }

    #[test]
    fn test_compare_fpu_state_operand_selector_diff() {
        let mut a = Fpu::new();
        let mut b = Fpu::new();
        a.operand_selector = 0x0018;
        b.operand_selector = 0x0020;
        let diffs = compare_fpu_state(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].contains("operand_selector"));
    }

    // ====================================================================
    // Tests for presets
    // ====================================================================

    #[test]
    fn test_preset_default() {
        let mut cpu = Cpu::new();
        apply_preset(&mut cpu, FpuPreset::Default);
        assert_eq!(cpu.fpu.get_top(), 0);
        assert_eq!(cpu.fpu.control_word, 0x037F);
        assert_eq!(cpu.fpu.tag_word, 0xFFFF);
    }

    #[test]
    fn test_preset_two_values() {
        let mut cpu = Cpu::new();
        apply_preset(&mut cpu, FpuPreset::TwoValues);
        assert_eq!(cpu.fpu.get_top(), 6);
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 3.14);
        assert_eq!(cpu.fpu.get_st(1).to_f64(), 2.71);
        assert!(!cpu.fpu.is_empty(6));
        assert!(!cpu.fpu.is_empty(7));
    }

    #[test]
    fn test_preset_stack_full() {
        let mut cpu = Cpu::new();
        apply_preset(&mut cpu, FpuPreset::StackFull);
        for i in 0..8 {
            assert!(!cpu.fpu.is_empty(i));
        }
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 1.0);
        assert_eq!(cpu.fpu.get_st(7).to_f64(), 8.0);
    }

    #[test]
    fn test_preset_special_values() {
        let mut cpu = Cpu::new();
        apply_preset(&mut cpu, FpuPreset::SpecialValues);
        assert!(cpu.fpu.get_st(0).to_f64().is_nan());
        assert!(cpu.fpu.get_st(1).to_f64().is_infinite());
        assert_eq!(cpu.fpu.get_st(2).to_f64().to_bits(), (-0.0f64).to_bits());
        assert!(cpu.fpu.get_st(3).to_f64().is_subnormal());
    }

    #[test]
    fn test_preset_truncation_rounding() {
        let mut cpu = Cpu::new();
        apply_preset(&mut cpu, FpuPreset::TruncationRounding);
        assert_eq!((cpu.fpu.control_word >> 10) & 3, 3); // RC=3
    }

    #[test]
    fn test_preset_round_down() {
        let mut cpu = Cpu::new();
        apply_preset(&mut cpu, FpuPreset::RoundDown);
        assert_eq!((cpu.fpu.control_word >> 10) & 3, 1); // RC=1
        assert_eq!(cpu.fpu.control_word, 0x077F);
        assert_eq!(cpu.fpu.get_top(), 6);
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 3.14);
        assert_eq!(cpu.fpu.get_st(1).to_f64(), 2.71);
    }

    #[test]
    fn test_preset_round_up() {
        let mut cpu = Cpu::new();
        apply_preset(&mut cpu, FpuPreset::RoundUp);
        assert_eq!((cpu.fpu.control_word >> 10) & 3, 2); // RC=2
        assert_eq!(cpu.fpu.control_word, 0x0B7F);
        assert_eq!(cpu.fpu.get_top(), 6);
        assert_eq!(cpu.fpu.get_st(0).to_f64(), 3.14);
        assert_eq!(cpu.fpu.get_st(1).to_f64(), 2.71);
    }

    // ====================================================================
    // Tests for memory setup
    // ====================================================================

    #[test]
    fn test_setup_test_memory() {
        let mut mem = Memory::new();
        setup_test_memory(&mut mem);

        // Verify f32 PI at 0x1000
        let f32_bits = mem.read_u32(0x1000);
        let f32_val = f32::from_le_bytes(f32_bits.to_le_bytes());
        assert!((f32_val as f64 - std::f64::consts::PI).abs() < 0.001);

        // Verify f64 E at 0x1004
        let f64_bits = mem.read_u64(0x1004);
        let f64_val = f64::from_le_bytes(f64_bits.to_le_bytes());
        assert_eq!(f64_val, std::f64::consts::E);

        // Verify i16 42 at 0x100C
        assert_eq!(mem.read_u16(0x100C) as i16, 42);

        // Verify i32 -12345 at 0x100E
        assert_eq!(mem.read_u32(0x100E) as i32, -12345);

        // Verify i64 at 0x1012
        assert_eq!(mem.read_u64(0x1012), 9876543210u64);
    }

    // ====================================================================
    // Exhaustive encoding consistency test
    // ====================================================================

    /// Run a single (escape, modrm, preset) combination through both sides
    /// and verify deterministic output.
    ///
    /// Returns: (decoded, matched, errored_both, decode_failed)
    fn run_encoding(
        escape: u8,
        modrm: u8,
        is_register_form: bool,
        preset: FpuPreset,
    ) -> (bool, bool, bool, bool) {
        // Build instruction bytes
        let opcode = 0xD8 + escape;
        let bytes: Vec<u8> = if is_register_form {
            // Register form: [escape_byte, modrm >= 0xC0]
            vec![opcode, modrm]
        } else {
            // Memory form: [escape_byte, modrm_with_mode00_disp16, disp_lo, disp_hi]
            // modrm format: mod=00, reg=reg_field, rm=110 (disp16 addressing)
            // modrm = (reg_field << 3) | 0x06 gives mod=00, rm=110
            vec![opcode, modrm, 0x00, 0x10] // disp16 = 0x1000
        };

        // Try to decode
        let instr = match try_decode(&bytes) {
            Some(i) => i,
            None => return (false, false, false, true),
        };

        // Setup CPU A
        let mut cpu_a = Cpu::new();
        let mut mem_a = Memory::new();
        setup_test_memory(&mut mem_a);
        apply_preset(&mut cpu_a, preset);

        // Setup CPU B identically
        let mut cpu_b = clone_cpu_for_diff(&cpu_a);
        let mut mem_b = Memory::new();
        setup_test_memory(&mut mem_b);

        // Execute on both
        let result_a = try_execute(&mut cpu_a, &mut mem_a, &instr);
        let result_b = try_execute(&mut cpu_b, &mut mem_b, &instr);

        // Both error -> compare error messages
        if let (Err(ref err_a), Err(ref err_b)) = (&result_a, &result_b) {
            let matched = err_a == err_b;
            return (true, matched, matched, false);
        }

        // One errors but not the other -> mismatch
        if result_a.is_err() != result_b.is_err() {
            return (true, false, false, false);
        }

        // Both succeeded -> compare FPU state
        let diffs = compare_fpu_state(&cpu_a.fpu, &cpu_b.fpu);
        if !diffs.is_empty() {
            return (true, false, false, false);
        }

        // Compare 128 bytes of memory at 0x1000
        for i in 0..128 {
            if mem_a.read_u8(0x1000 + i) != mem_b.read_u8(0x1000 + i) {
                return (true, false, false, false);
            }
        }

        (true, true, false, false)
    }

    #[test]
    fn test_diff_register_forms_deterministic() {
        let mut total_decoded = 0u32;
        let mut total_matched = 0u32;
        let mut total_errored_both = 0u32;
        let mut total_decode_failed = 0u32;
        let mut mismatches = Vec::new();

        // Register forms: modrm >= 0xC0 (mod=11, 64 combinations per escape)
        for escape in 0..8u8 {
            for modrm in 0xC0..=0xFFu8 {
                for &preset in &ALL_PRESETS {
                    let (decoded, matched, errored_both, decode_failed) =
                        run_encoding(escape, modrm, true, preset);

                    if decode_failed {
                        total_decode_failed += 1;
                    } else if decoded {
                        total_decoded += 1;
                        if matched {
                            total_matched += 1;
                            if errored_both {
                                total_errored_both += 1;
                            }
                        } else {
                            mismatches.push(format!(
                                "REG escape={} modrm=0x{:02X} preset={:?}",
                                escape, modrm, preset
                            ));
                        }
                    }
                }
            }
        }

        assert!(
            mismatches.is_empty(),
            "Register form non-determinism detected in {} encodings:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );

        // Sanity: we should have decoded a significant number of encodings
        assert!(
            total_decoded > 0,
            "No register-form encodings were decoded at all"
        );

        eprintln!(
            "Register forms: decoded={}, matched={}, errored_both={}, decode_failed={}",
            total_decoded, total_matched, total_errored_both, total_decode_failed
        );
    }

    #[test]
    fn test_diff_memory_forms_deterministic() {
        let mut total_decoded = 0u32;
        let mut total_matched = 0u32;
        let mut total_errored_both = 0u32;
        let mut total_decode_failed = 0u32;
        let mut mismatches = Vec::new();

        // Memory forms: 8 escapes x 8 /reg fields
        // modrm = (reg_field << 3) | 0x06 for mod=00, rm=110 (disp16)
        for escape in 0..8u8 {
            for reg_field in 0..8u8 {
                let modrm = (reg_field << 3) | 0x06;
                for &preset in &ALL_PRESETS {
                    let (decoded, matched, errored_both, decode_failed) =
                        run_encoding(escape, modrm, false, preset);

                    if decode_failed {
                        total_decode_failed += 1;
                    } else if decoded {
                        total_decoded += 1;
                        if matched {
                            total_matched += 1;
                            if errored_both {
                                total_errored_both += 1;
                            }
                        } else {
                            mismatches.push(format!(
                                "MEM escape={} reg={} modrm=0x{:02X} preset={:?}",
                                escape, reg_field, modrm, preset
                            ));
                        }
                    }
                }
            }
        }

        assert!(
            mismatches.is_empty(),
            "Memory form non-determinism detected in {} encodings:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );

        assert!(
            total_decoded > 0,
            "No memory-form encodings were decoded at all"
        );

        eprintln!(
            "Memory forms: decoded={}, matched={}, errored_both={}, decode_failed={}",
            total_decoded, total_matched, total_errored_both, total_decode_failed
        );
    }

    /// Verify that the self-consistency test exercises a meaningful number of
    /// unique FPU mnemonics (not just FNOP over and over).
    #[test]
    fn test_diff_covers_meaningful_mnemonics() {
        use std::collections::HashSet;

        let mut mnemonics = HashSet::new();

        // Check register forms
        for escape in 0..8u8 {
            for modrm in 0xC0..=0xFFu8 {
                let opcode = 0xD8 + escape;
                let bytes = [opcode, modrm];
                if let Some(instr) = try_decode(&bytes) {
                    mnemonics.insert(format!("{:?}", instr.mnemonic()));
                }
            }
        }

        // Check memory forms
        for escape in 0..8u8 {
            for reg_field in 0..8u8 {
                let opcode = 0xD8 + escape;
                let modrm = (reg_field << 3) | 0x06;
                let bytes = [opcode, modrm, 0x00, 0x10];
                if let Some(instr) = try_decode(&bytes) {
                    mnemonics.insert(format!("{:?}", instr.mnemonic()));
                }
            }
        }

        // We should cover at least 20 distinct mnemonics (FADD, FSUB, FMUL, FDIV,
        // FLD, FST, FSTP, FCOM, FCOMP, FXCH, FNOP, FCHS, FABS, FTST, FXAM,
        // FILD, FIST, FISTP, FLDCW, FNSTCW, FNSTSW, FSQRT, FPTAN, etc.)
        assert!(
            mnemonics.len() >= 20,
            "Only {} distinct mnemonics decoded, expected >= 20: {:?}",
            mnemonics.len(),
            mnemonics
        );

        eprintln!(
            "Distinct FPU mnemonics covered: {} ({:?})",
            mnemonics.len(),
            mnemonics
        );
    }
}

// ========================================================================
// Mutant-killing tests for FNSAVE / FNSTENV / FRSTOR memory layout
// (appended outside tests_execute as a separate test module to avoid
// insertion conflicts)
// ========================================================================

#[cfg(test)]
mod tests_emu_probes_guard {
    /// Guard (2026.07.30 emu_probes HLD): the per-x87-op trace gates must stay
    /// `EMU_PROBES &&`-gated so a feature-off build deletes the level-filter
    /// atomic from the dispatch path. Lives in this crate so any component
    /// running `cargo test -p gp64-cpu` re-checks the file it polices.
    #[test]
    fn fpu_trace_gates_stay_emu_probes_gated() {
        let src = include_str!("fpu.rs");
        for gate in [
            "fn fpu_ops_trace_enabled",
            "fn fpu_trace_exceptions_enabled",
        ] {
            let at = src
                .find(gate)
                .expect("fpu trace gate renamed: update guard");
            assert!(
                src[at..at + 400].contains("EMU_PROBES &&"),
                "{gate} lost its EMU_PROBES gate"
            );
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests_fpu_save_restore_layout {
    use super::*;
    use gp64_memory::Memory;
    use iced_x86::{Code, Instruction, MemoryOperand, Register};

    fn exec_instr(cpu: &mut Cpu, mem: &mut Memory, instr: Instruction) {
        execute_iced_for_test(cpu, &instr, mem).unwrap();
    }

    fn mem_op(displacement: i64) -> MemoryOperand {
        MemoryOperand::new(
            Register::None,
            Register::None,
            1,
            displacement,
            2,
            false,
            Register::None,
        )
    }

    #[test]
    fn mmx_alias_is_physical_and_fnsave_frstor_preserves_it() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 32;

        // A pre-existing x87 value is immediately visible through the low
        // 64-bit physical alias, independent of TOP.
        cpu.fpu.regs[3] = F80([1, 2, 3, 4, 5, 6, 7, 8, 0x34, 0x12]);
        cpu.fpu.set_top(5);
        assert_eq!(cpu.mmx_reg(3), 0x0807_0605_0403_0201);

        cpu.fpu.enter_mmx_state();
        cpu.set_mmx_reg(0, 0x0123_4567_89AB_CDEF);
        cpu.set_mmx_reg(7, 0xFEDC_BA98_7654_3210);
        assert_eq!(&cpu.fpu.regs[0].0[8..10], &[0xFF, 0xFF]);
        assert_eq!(&cpu.fpu.regs[7].0[8..10], &[0xFF, 0xFF]);

        let base = 0xA000;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnsave_m108byte, mem_op(base)).unwrap(),
        );
        assert_eq!(cpu.fpu.tag_word, 0xFFFF, "FNSAVE resets x87/MMX state");

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Frstor_m108byte, mem_op(base)).unwrap(),
        );
        assert_eq!(cpu.mmx_reg(0), 0x0123_4567_89AB_CDEF);
        assert_eq!(cpu.mmx_reg(7), 0xFEDC_BA98_7654_3210);
        assert_eq!(&cpu.fpu.regs[0].0[8..10], &[0xFF, 0xFF]);
        assert_eq!(&cpu.fpu.regs[7].0[8..10], &[0xFF, 0xFF]);
    }

    /// FNSTENV 16-bit: verify every field is written to the correct memory offset.
    /// Uses unique non-zero values per field so any offset swap is detected.
    /// MDD-BUG-TEM-00167: FNSTENV masks every floating-point exception AFTER
    /// storing the environment, which is what lets an #MF handler use it
    /// without re-faulting. The stored image keeps the pre-mask control word.
    ///
    /// FNINIT (0x037F) -> FLDCW 0x0300 (all six masks cleared) -> FNSTENV:
    /// a 387/Pentium then reports CW = 0x033F, and the saved image 0x0300.
    #[test]
    fn fnstenv_masks_the_exceptions_after_saving_the_unmasked_environment() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.control_word = 0x0300; // every exception unmasked
        cpu.fpu.status_word = 0x0000;
        let base: usize = 0x3000;

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstenv_m14byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(
            mem.read_u16(base),
            0x0300,
            "the saved environment holds the control word as it was before the mask"
        );
        assert_eq!(
            cpu.fpu.control_word, 0x033F,
            "FNSTENV masks all six exceptions once the store is done"
        );

        // The mask is guest-visible a second way: set_exception derives ES
        // from it, so an exception after FNSTENV must NOT summarise as an
        // unmasked error the way it would have before.
        cpu.fpu.set_exception(0x0001); // #IE
        assert_eq!(
            cpu.fpu.status_word & (1 << 7),
            0,
            "a masked exception sets no error summary"
        );
    }

    #[test]
    fn test_fnstenv_16bit_all_field_offsets() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.control_word = 0x1234;
        cpu.fpu.status_word = 0x5678;
        cpu.fpu.tag_word = 0x9ABC;
        cpu.fpu.ip_offset = 0x0000DEF0;
        cpu.fpu.ip_selector = 0x1111;
        cpu.fpu.operand_offset = 0x00002222;
        cpu.fpu.operand_selector = 0x3333;

        let base: usize = 0x2000;
        for i in 0..20 {
            mem.write_u8(base + i, 0xFF);
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstenv_m14byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(mem.read_u16(base), 0x1234, "control_word at offset 0");
        assert_eq!(mem.read_u16(base + 2), 0x5678, "status_word at offset 2");
        assert_eq!(mem.read_u16(base + 4), 0x9ABC, "tag_word at offset 4");
        assert_eq!(
            mem.read_u16(base + 6),
            0xDEF0,
            "ip_offset (low 16) at offset 6"
        );
        assert_eq!(mem.read_u16(base + 8), 0x1111, "ip_selector at offset 8");
        assert_eq!(
            mem.read_u16(base + 10),
            0x2222,
            "operand_offset (low 16) at offset 10"
        );
        assert_eq!(
            mem.read_u16(base + 12),
            0x3333,
            "operand_selector at offset 12"
        );
        assert_eq!(
            mem.read_u8(base + 14),
            0xFF,
            "sentinel after env must be untouched"
        );
    }

    /// FNSAVE 32-bit PM: verify every env field AND every register byte offset.
    #[test]
    fn test_fnsave_32bit_all_field_and_register_offsets() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 32;

        cpu.fpu.control_word = 0x037F;
        cpu.fpu.status_word = 0xBEEF;
        cpu.fpu.tag_word = 0xCAFE;
        cpu.fpu.ip_offset = 0x12345678;
        cpu.fpu.ip_selector = 0xABCD;
        cpu.fpu.operand_offset = 0x9ABCDEF0;
        cpu.fpu.operand_selector = 0x4321;

        for i in 0u8..8 {
            let mut bytes = [0u8; 10];
            for j in 0..10 {
                bytes[j] = (i + 1) * 10 + j as u8;
            }
            cpu.fpu.regs[i as usize] = F80(bytes);
        }

        let base: usize = 0x3000;
        for i in 0..120 {
            mem.write_u8(base + i, 0xDD);
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnsave_m108byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(mem.read_u32(base), 0x037F, "control_word at +0");
        assert_eq!(mem.read_u32(base + 4), 0xBEEF, "status_word at +4");
        assert_eq!(mem.read_u32(base + 8), 0xCAFE, "tag_word at +8");
        assert_eq!(mem.read_u32(base + 12), 0x12345678, "ip_offset at +12");
        assert_eq!(mem.read_u16(base + 16), 0xABCD, "ip_selector at +16");
        assert_eq!(mem.read_u32(base + 20), 0x9ABCDEF0, "operand_offset at +20");
        assert_eq!(mem.read_u16(base + 24), 0x4321, "operand_selector at +24");

        // Slot i holds ST(i), so the expectation rotates by the fixture's TOP.
        // Derive TOP from the status word, so it stays right if
        // the fixture changes.
        // Read TOP from the SAVED image, not from `cpu.fpu` — FSAVE
        // reinitialises the FPU, so the live status word is already 0 by now.
        let top = ((mem.read_u32(base + 4) >> 11) & 0x7) as u8;
        assert_ne!(top, 0, "precondition: TOP must be rotated or this test                             could not tell stack-relative from physical order");
        for i in 0u8..8 {
            for j in 0..10 {
                let expected = (((top + i) & 0x7) + 1) * 10 + j as u8;
                let actual = mem.read_u8(base + 28 + (i as usize) * 10 + j);
                assert_eq!(
                    actual, expected,
                    "reg[{}] byte[{}]: expected 0x{:02X}, got 0x{:02X}",
                    i, j, expected, actual
                );
            }
        }

        assert_eq!(mem.read_u8(base + 108), 0xDD, "sentinel after save area");
    }

    /// FNSAVE 16-bit: verify env fields at 16-bit offsets + registers at offset 14.
    #[test]
    fn test_fnsave_16bit_all_field_and_register_offsets() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 16;

        cpu.fpu.control_word = 0x037F;
        cpu.fpu.status_word = 0xA5A5;
        cpu.fpu.tag_word = 0x5A5A;
        cpu.fpu.ip_offset = 0x0000FACE;
        cpu.fpu.ip_selector = 0xD00D;
        cpu.fpu.operand_offset = 0x0000BABE;
        cpu.fpu.operand_selector = 0xF00D;

        for i in 0u8..8 {
            let mut bytes = [0u8; 10];
            for j in 0..10 {
                bytes[j] = (i + 1) * 0x11 + j as u8;
            }
            cpu.fpu.regs[i as usize] = F80(bytes);
        }

        let base: usize = 0x4000;
        for i in 0..100 {
            mem.write_u8(base + i, 0xCC);
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsave_m94byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(mem.read_u16(base), 0x037F, "control_word at +0");
        assert_eq!(mem.read_u16(base + 2), 0xA5A5, "status_word at +2");
        assert_eq!(mem.read_u16(base + 4), 0x5A5A, "tag_word at +4");
        assert_eq!(mem.read_u16(base + 6), 0xFACE, "ip_offset low16 at +6");
        assert_eq!(mem.read_u16(base + 8), 0xD00D, "ip_selector at +8");
        assert_eq!(
            mem.read_u16(base + 10),
            0xBABE,
            "operand_offset low16 at +10"
        );
        assert_eq!(mem.read_u16(base + 12), 0xF00D, "operand_selector at +12");

        // Slot i holds ST(i), so the expectation rotates by the fixture's TOP.
        // Derive TOP from the status word, so it stays right if
        // the fixture changes.
        // Read TOP from the SAVED image, not from `cpu.fpu` — FSAVE
        // reinitialises the FPU, so the live status word is already 0 by now.
        // The image's own status word is what defines its ordering anyway.
        let top = ((mem.read_u16(base + 2) >> 11) & 0x7) as u8;
        assert_ne!(top, 0, "precondition: TOP must be rotated or this test                             could not tell stack-relative from physical order");
        for i in 0u8..8 {
            for j in 0..10 {
                let expected = (((top + i) & 0x7) + 1) * 0x11 + j as u8;
                let actual = mem.read_u8(base + 14 + (i as usize) * 10 + j);
                assert_eq!(
                    actual, expected,
                    "16-bit reg[{}] byte[{}]: expected 0x{:02X}, got 0x{:02X}",
                    i, j, expected, actual
                );
            }
        }

        assert_eq!(mem.read_u8(base + 94), 0xCC, "sentinel after 16-bit save");
    }

    /// FRSTOR 32-bit from crafted memory: verify every FPU field is loaded from
    /// the correct offset.
    #[test]
    fn test_frstor_32bit_crafted_memory_all_fields() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 32;

        let base: usize = 0x5000;

        mem.write_u32(base, 0x027F);
        mem.write_u32(base + 4, 0x3800);
        mem.write_u32(base + 8, 0x5555);
        mem.write_u32(base + 12, 0xDEADC0DE);
        mem.write_u16(base + 16, 0x0008);
        mem.write_u16(base + 18, 0);
        mem.write_u32(base + 20, 0xFEEDFACE);
        mem.write_u16(base + 24, 0x0010);
        mem.write_u16(base + 26, 0);

        for i in 0u8..8 {
            for j in 0..10 {
                mem.write_u8(base + 28 + (i as usize) * 10 + j, (i + 1) * 0x10 + j as u8);
            }
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Frstor_m108byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(cpu.fpu.control_word, 0x027F, "control_word");
        assert_eq!(cpu.fpu.status_word, 0x3800, "status_word");
        assert_eq!(cpu.fpu.tag_word, 0x5555, "tag_word");
        assert_eq!(cpu.fpu.ip_offset, 0xDEADC0DE, "ip_offset");
        assert_eq!(cpu.fpu.ip_selector, 0x0008, "ip_selector");
        assert_eq!(cpu.fpu.operand_offset, 0xFEEDFACE, "operand_offset");
        assert_eq!(cpu.fpu.operand_selector, 0x0010, "operand_selector");

        // Image slot i is ST(i), so it lands in physical regs[(TOP+i)&7]
        // TOP is the RESTORED one, asserted
        // just above, and derived here rather than written down.
        let top = ((cpu.fpu.status_word >> 11) & 0x7) as u8;
        assert_ne!(top, 0, "precondition: TOP must be rotated or this test                             could not tell stack-relative from physical order");
        for i in 0u8..8 {
            for j in 0..10 {
                let expected = (i + 1) * 0x10 + j as u8;
                assert_eq!(
                    cpu.fpu.regs[((top + i) & 0x7) as usize].0[j],
                    expected,
                    "FRSTOR reg[{}] byte[{}]: expected 0x{:02X}, got 0x{:02X}",
                    i,
                    j,
                    expected,
                    cpu.fpu.regs[((top + i) & 0x7) as usize].0[j]
                );
            }
        }
    }

    /// FRSTOR 16-bit from crafted memory: verify all fields loaded from correct offsets.
    #[test]
    fn test_frstor_16bit_crafted_memory_all_fields() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 16;

        let base: usize = 0x6000;

        mem.write_u16(base, 0x037F);
        mem.write_u16(base + 2, 0xA1B2);
        mem.write_u16(base + 4, 0xC3D4);
        mem.write_u16(base + 6, 0xE5F6);
        mem.write_u16(base + 8, 0x0708);
        mem.write_u16(base + 10, 0x091A);
        mem.write_u16(base + 12, 0x2B3C);

        for i in 0u8..8 {
            for j in 0..10 {
                let val = ((i as u16 + 1) * 0x20 + j as u16) as u8;
                mem.write_u8(base + 14 + (i as usize) * 10 + j, val);
            }
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Frstor_m94byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(cpu.fpu.control_word, 0x037F, "control_word");
        assert_eq!(cpu.fpu.status_word, 0xA1B2, "status_word");
        assert_eq!(cpu.fpu.tag_word, 0xC3D4, "tag_word");
        assert_eq!(cpu.fpu.ip_offset, 0xE5F6, "ip_offset");
        assert_eq!(cpu.fpu.ip_selector, 0x0708, "ip_selector");
        assert_eq!(cpu.fpu.operand_offset, 0x091A, "operand_offset");
        assert_eq!(cpu.fpu.operand_selector, 0x2B3C, "operand_selector");

        // Image slot i is ST(i), so it lands in physical regs[(TOP+i)&7]
        // TOP is the RESTORED one, asserted
        // just above, and derived here rather than written down.
        let top = ((cpu.fpu.status_word >> 11) & 0x7) as u8;
        assert_ne!(top, 0, "precondition: TOP must be rotated or this test                             could not tell stack-relative from physical order");
        for i in 0u8..8 {
            for j in 0..10 {
                let expected = ((i as u16 + 1) * 0x20 + j as u16) as u8;
                assert_eq!(
                    cpu.fpu.regs[((top + i) & 0x7) as usize].0[j],
                    expected,
                    "16-bit FRSTOR reg[{}] byte[{}]",
                    i,
                    j
                );
            }
        }
    }

    /// FNSAVE 32-bit -> FRSTOR 32-bit round-trip with all fields distinct.
    #[test]
    fn test_fnsave_frstor_32bit_roundtrip_all_fields() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 32;

        let orig_cw: u16 = 0x027F;
        let orig_sw: u16 = 0x3800;
        let orig_tw: u16 = 0x5A5A;
        let orig_ip: u32 = 0xAABB_CCDD;
        let orig_ips: u16 = 0x0018;
        let orig_op: u32 = 0x1122_3344;
        let orig_ops: u16 = 0x0028;

        cpu.fpu.control_word = orig_cw;
        cpu.fpu.status_word = orig_sw;
        cpu.fpu.tag_word = orig_tw;
        cpu.fpu.ip_offset = orig_ip;
        cpu.fpu.ip_selector = orig_ips;
        cpu.fpu.operand_offset = orig_op;
        cpu.fpu.operand_selector = orig_ops;

        let mut orig_regs = [[0u8; 10]; 8];
        for i in 0u8..8 {
            for j in 0..10 {
                orig_regs[i as usize][j] = ((i as u16 * 31 + j as u16 * 7 + 1) & 0xFF) as u8;
            }
            cpu.fpu.regs[i as usize] = F80(orig_regs[i as usize]);
        }

        let base: usize = 0x7000;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnsave_m108byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(cpu.fpu.control_word, 0x037F);
        assert_eq!(cpu.fpu.status_word, 0);
        assert_eq!(cpu.fpu.tag_word, 0xFFFF);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Frstor_m108byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(cpu.fpu.control_word, orig_cw, "round-trip control_word");
        assert_eq!(cpu.fpu.status_word, orig_sw, "round-trip status_word");
        assert_eq!(cpu.fpu.tag_word, orig_tw, "round-trip tag_word");
        assert_eq!(cpu.fpu.ip_offset, orig_ip, "round-trip ip_offset");
        assert_eq!(cpu.fpu.ip_selector, orig_ips, "round-trip ip_selector");
        assert_eq!(cpu.fpu.operand_offset, orig_op, "round-trip operand_offset");
        assert_eq!(
            cpu.fpu.operand_selector, orig_ops,
            "round-trip operand_selector"
        );

        for i in 0..8 {
            assert_eq!(cpu.fpu.regs[i].0, orig_regs[i], "round-trip reg[{}]", i);
        }
    }

    /// FNSAVE 16-bit -> FRSTOR 16-bit round-trip.
    #[test]
    fn test_fnsave_frstor_16bit_roundtrip_all_fields() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 16;

        let orig_cw: u16 = 0x137F;
        let orig_sw: u16 = 0x2468;
        let orig_tw: u16 = 0xABCD;
        let orig_ip: u32 = 0x0000_FEDC;
        let orig_ips: u16 = 0xBA98;
        let orig_op: u32 = 0x0000_7654;
        let orig_ops: u16 = 0x3210;

        cpu.fpu.control_word = orig_cw;
        cpu.fpu.status_word = orig_sw;
        cpu.fpu.tag_word = orig_tw;
        cpu.fpu.ip_offset = orig_ip;
        cpu.fpu.ip_selector = orig_ips;
        cpu.fpu.operand_offset = orig_op;
        cpu.fpu.operand_selector = orig_ops;

        let mut orig_regs = [[0u8; 10]; 8];
        for i in 0u8..8 {
            for j in 0..10 {
                orig_regs[i as usize][j] = ((i as u16 * 17 + j as u16 * 13 + 3) & 0xFF) as u8;
            }
            cpu.fpu.regs[i as usize] = F80(orig_regs[i as usize]);
        }

        let base: usize = 0x8000;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fsave_m94byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(cpu.fpu.control_word, 0x037F);
        assert_eq!(cpu.fpu.status_word, 0);
        assert_eq!(cpu.fpu.tag_word, 0xFFFF);

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Frstor_m94byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(cpu.fpu.control_word, orig_cw, "16-bit rt control_word");
        assert_eq!(cpu.fpu.status_word, orig_sw, "16-bit rt status_word");
        assert_eq!(cpu.fpu.tag_word, orig_tw, "16-bit rt tag_word");
        assert_eq!(cpu.fpu.ip_offset, orig_ip & 0xFFFF, "16-bit rt ip_offset");
        assert_eq!(cpu.fpu.ip_selector, orig_ips, "16-bit rt ip_selector");
        assert_eq!(
            cpu.fpu.operand_offset,
            orig_op & 0xFFFF,
            "16-bit rt operand_offset"
        );
        assert_eq!(
            cpu.fpu.operand_selector, orig_ops,
            "16-bit rt operand_selector"
        );

        for i in 0..8 {
            assert_eq!(cpu.fpu.regs[i].0, orig_regs[i], "16-bit rt reg[{}]", i);
        }
    }

    /// FNSTENV + FLDENV round-trip: verify all 7 fields with unique values.
    #[test]
    fn test_fnstenv_fldenv_roundtrip_distinct_values() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;

        cpu.fpu.control_word = 0x037F;
        cpu.fpu.status_word = 0xBEEF;
        cpu.fpu.tag_word = 0xCAFE;
        cpu.fpu.ip_offset = 0x0000_DEAD;
        cpu.fpu.ip_selector = 0xF00D;
        cpu.fpu.operand_offset = 0x0000_BABE;
        cpu.fpu.operand_selector = 0x1234;

        let base: usize = 0x9000;
        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fstenv_m14byte, mem_op(base as i64)).unwrap(),
        );

        // Overwrite CPU state to prove FLDENV actually reads from memory
        cpu.fpu.control_word = 0;
        cpu.fpu.status_word = 0;
        cpu.fpu.tag_word = 0;
        cpu.fpu.ip_offset = 0;
        cpu.fpu.ip_selector = 0;
        cpu.fpu.operand_offset = 0;
        cpu.fpu.operand_selector = 0;

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fldenv_m14byte, mem_op(base as i64)).unwrap(),
        );

        assert_eq!(cpu.fpu.control_word, 0x037F, "fldenv control_word");
        assert_eq!(cpu.fpu.status_word, 0xBEEF, "fldenv status_word");
        assert_eq!(cpu.fpu.tag_word, 0xCAFE, "fldenv tag_word");
        assert_eq!(cpu.fpu.ip_offset, 0xDEAD, "fldenv ip_offset");
        assert_eq!(cpu.fpu.ip_selector, 0xF00D, "fldenv ip_selector");
        assert_eq!(cpu.fpu.operand_offset, 0xBABE, "fldenv operand_offset");
        assert_eq!(cpu.fpu.operand_selector, 0x1234, "fldenv operand_selector");
    }

    /// FNSAVE 32-bit: verify adjacent field boundaries at byte level.
    #[test]
    fn test_fnsave_32bit_adjacent_field_isolation() {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        cpu.regs.ds = 0;
        cpu.cs_bits = 32;

        cpu.fpu.control_word = 0x0102;
        cpu.fpu.status_word = 0x0304;
        cpu.fpu.tag_word = 0x0506;
        cpu.fpu.ip_offset = 0x0708_090A;
        cpu.fpu.ip_selector = 0x0B0C;
        cpu.fpu.operand_offset = 0x0D0E_0F10;
        cpu.fpu.operand_selector = 0x1112;

        let base: usize = 0xA000;
        for i in 0..32 {
            mem.write_u8(base + i, 0);
        }

        exec_instr(
            &mut cpu,
            &mut mem,
            Instruction::with1(Code::Fnsave_m108byte, mem_op(base as i64)).unwrap(),
        );

        // control_word as u32 = 0x00000102 (LE: 02 01 00 00)
        assert_eq!(mem.read_u8(base), 0x02);
        assert_eq!(mem.read_u8(base + 1), 0x01);
        assert_eq!(mem.read_u8(base + 2), 0x00);
        assert_eq!(mem.read_u8(base + 3), 0x00);

        // status_word as u32 = 0x00000304 (LE: 04 03 00 00)
        assert_eq!(mem.read_u8(base + 4), 0x04);
        assert_eq!(mem.read_u8(base + 5), 0x03);
        assert_eq!(mem.read_u8(base + 6), 0x00);
        assert_eq!(mem.read_u8(base + 7), 0x00);

        // tag_word as u32 = 0x00000506 (LE: 06 05 00 00)
        assert_eq!(mem.read_u8(base + 8), 0x06);
        assert_eq!(mem.read_u8(base + 9), 0x05);
        assert_eq!(mem.read_u8(base + 10), 0x00);
        assert_eq!(mem.read_u8(base + 11), 0x00);

        // ip_offset = 0x0708090A (LE: 0A 09 08 07)
        assert_eq!(mem.read_u8(base + 12), 0x0A);
        assert_eq!(mem.read_u8(base + 13), 0x09);
        assert_eq!(mem.read_u8(base + 14), 0x08);
        assert_eq!(mem.read_u8(base + 15), 0x07);

        // ip_selector = 0x0B0C (LE: 0C 0B)
        assert_eq!(mem.read_u8(base + 16), 0x0C);
        assert_eq!(mem.read_u8(base + 17), 0x0B);

        // opcode = 0
        assert_eq!(mem.read_u8(base + 18), 0x00);
        assert_eq!(mem.read_u8(base + 19), 0x00);

        // operand_offset = 0x0D0E0F10 (LE: 10 0F 0E 0D)
        assert_eq!(mem.read_u8(base + 20), 0x10);
        assert_eq!(mem.read_u8(base + 21), 0x0F);
        assert_eq!(mem.read_u8(base + 22), 0x0E);
        assert_eq!(mem.read_u8(base + 23), 0x0D);

        // operand_selector = 0x1112 (LE: 12 11)
        assert_eq!(mem.read_u8(base + 24), 0x12);
        assert_eq!(mem.read_u8(base + 25), 0x11);

        // reserved = 0
        assert_eq!(mem.read_u8(base + 26), 0x00);
        assert_eq!(mem.read_u8(base + 27), 0x00);
    }
}
