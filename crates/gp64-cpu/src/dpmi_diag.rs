//! DPMI exception reentry tracking.
//!
//! Counts nested DPMI exception dispatches so an exception loop fails loudly
//! instead of recursing forever. Extracted from `cpu.rs`.

/// Tracing target for DPMI diagnostics
pub const DPMI_DIAG_TARGET: &str = "gp64::cpu::dpmi_diag";

/// Threshold for reentry detection (number of nested exceptions)
pub const DPMI_EXCEPTION_REENTRY_THRESHOLD: u8 = 8;

/// Runtime state for DPMI exception reentry tracking.
#[derive(Default)]
pub struct DpmiExceptionDiagState {
    pub reentry_depth: u8,
}

impl DpmiExceptionDiagState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.reentry_depth = 0;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_reset_clears_reentry_depth() {
        let mut state = DpmiExceptionDiagState::new();
        state.reentry_depth = 5;
        state.reset();
        assert_eq!(state.reentry_depth, 0);
    }
}
