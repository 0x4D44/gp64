//! BIOS F000 ROM layout constants shared across crates.
//!
//! These constants define regions of the F000 ROM segment that multiple
//! subsystems need to agree on. Centralising them here prevents silent drift
//! between call sites that gate on these ranges (for example, the
//! fast-interpreter F000 sentinel gate in `gp64-interp` and the HLE
//! dispatcher pass-through in `src/emulator/hle_dispatch.rs`).

/// Start of the DOS batch idle loop in the F000 ROM segment (inclusive).
///
/// The four-byte `STI / HLT / JMP` loop occupies `F000:1300..1304`, after
/// the video parameter table at `1200..1258` and below the fonts starting
/// at `D000`. In particular, it must stay clear of the 8x16 font at
/// `E800..F800`, which BIOS font-pointer services advertise to guests.
pub const DOS_IDLE_STUB_START: u16 = 0x1300;

/// End (exclusive) of the DOS batch idle loop.
pub const DOS_IDLE_STUB_END: u16 = 0x1304;

/// Start of the reserved assembler keyboard stub code range in the F000 ROM
/// segment (inclusive).
///
/// Offsets `F000:F800..F000:F900` contain real x86 instructions installed by
/// `Bios::install_keyboard_stubs()`. These bytes must execute in the
/// interpreter rather than being intercepted as HLE entry points so their
/// `STI`/`NOP`/`CLI` triples can open per-instruction IRQ1 delivery windows.
pub const STUB_CODE_RANGE_START: u16 = 0xF800;

/// End (exclusive) of the reserved assembler keyboard stub code range in the
/// F000 ROM segment.
///
/// See [`STUB_CODE_RANGE_START`] for details.
pub const STUB_CODE_RANGE_END: u16 = 0xF900;

/// Returns `true` when `cs:ip` lies inside the reserved keyboard stub range
/// (`F000:F800..F000:F900`).
///
/// Used by the fast-interpreter outer gate and the HLE dispatcher
/// pass-through to let stub bytes execute as ordinary real-mode code while
/// every other `F000:xxxx` address retains its HLE-entry behaviour.
#[inline]
pub fn is_in_stub_code_range(cs: u16, ip: u16) -> bool {
    cs == 0xF000 && (STUB_CODE_RANGE_START..STUB_CODE_RANGE_END).contains(&ip)
}

/// Start of the VBE window-function thunk in the F000 ROM segment
/// (inclusive).
///
/// `F000:F900..F000:F910` holds a six-byte far-callable routine
/// (`MOV AX,4F05h / INT 10h / RETF`) that the VBE mode-info block advertises
/// as `WinFuncPtr`, so callers can bank-switch without the INT 10h round trip.
///
/// It lives HERE, and not in `F000:FE00..FFFF`, because that range is filled
/// with two-byte `IRET`/`NOP` protected-mode stubs for all 256 vectors — the
/// stubs for vectors C0h..C2h land on `F000:FF80` and used to overwrite the
/// whole thunk with `CF 90 CF 90 CF 90`.
pub const VBE_WINDOW_FUNC_START: u16 = 0xF900;

/// End (exclusive) of the VBE window-function thunk.
///
/// See [`VBE_WINDOW_FUNC_START`].
pub const VBE_WINDOW_FUNC_END: u16 = 0xF910;

/// Returns `true` when `cs:ip` lies inside the VBE window-function thunk.
///
/// The thunk is real x86 code reached by a guest FAR CALL, so — like the
/// keyboard stubs — it must execute in the interpreter instead of being
/// intercepted as an HLE entry point. Without this the HLE dispatcher treated
/// the address as a wild BIOS address and synthesized an `IRET`, so the guest
/// never banked and the far-call stack shape was mismatched.
#[inline]
pub fn is_in_vbe_window_func_thunk(cs: u16, ip: u16) -> bool {
    cs == 0xF000 && (VBE_WINDOW_FUNC_START..VBE_WINDOW_FUNC_END).contains(&ip)
}

/// Start of the BIOS INT 8 IRQ0 ISR (inclusive).
pub const BIOS_INT8_ISR_START: u16 = 0xFEA5;

/// End (exclusive) of the BIOS INT 8 IRQ0 ISR. 16 bytes total: F000:FEA5
/// through F000:FEB4 inclusive.
pub const BIOS_INT8_ISR_END: u16 = 0xFEB5;

/// Offset of the BIOS-tick HLE trap inside the BIOS INT 8 ISR
/// (`BIOS_INT8_ISR_START + 1`). The byte at this address is `0xF1`
/// (ICEBP) — the HLE dispatcher intercepts before it executes.
pub const BIOS_INT8_TICK_TRAP_OFF: u16 = 0xFEA6;

/// Returns `true` when `cs:ip` lies inside the BIOS INT 8 ISR span
/// **excluding the HLE tick-trap byte at F000:FEA6**.
///
/// The fast-interpreter outer gate and the HLE dispatcher pass-through
/// both use this so the ISR's STI / PUSH DS / PUSH AX / PUSH DX / INT
/// 1Ch / CLI / EOI / POPs / IRET execute as ordinary real-mode code,
/// while the trap byte at F000:FEA6 still drops out to the dispatcher
/// to fire the host-side BIOS tick / floppy / PIT housekeeping.
///
/// See `wrk_docs/2026.05.21 - HLD - HLE BIOS INT 8 chain fix HLD-A v3.md`
/// §3.1 / §4.1.
#[inline]
pub fn is_in_bios_int8_isr_pass_through(cs: u16, ip: u16) -> bool {
    cs == 0xF000
        && (BIOS_INT8_ISR_START..BIOS_INT8_ISR_END).contains(&ip)
        && ip != BIOS_INT8_TICK_TRAP_OFF
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn stub_range_boundaries() {
        assert_eq!(STUB_CODE_RANGE_START, 0xF800);
        assert_eq!(STUB_CODE_RANGE_END, 0xF900);
        const { assert!(STUB_CODE_RANGE_START < STUB_CODE_RANGE_END) };
    }

    #[test]
    fn is_in_stub_code_range_matches_inclusive_start() {
        assert!(is_in_stub_code_range(0xF000, 0xF800));
    }

    #[test]
    fn is_in_stub_code_range_matches_interior() {
        assert!(is_in_stub_code_range(0xF000, 0xF8D0));
        assert!(is_in_stub_code_range(0xF000, 0xF8FF));
    }

    #[test]
    fn is_in_stub_code_range_excludes_exclusive_end() {
        assert!(!is_in_stub_code_range(0xF000, 0xF900));
    }

    #[test]
    fn is_in_stub_code_range_excludes_below_start() {
        assert!(!is_in_stub_code_range(0xF000, 0xF7FF));
        assert!(!is_in_stub_code_range(0xF000, 0x0000));
    }

    #[test]
    fn is_in_stub_code_range_excludes_other_segments() {
        // Non-ROM segments must not be treated as stubs even if the IP
        // happens to land inside the reserved offset range.
        assert!(!is_in_stub_code_range(0xF001, 0xF8D0));
        assert!(!is_in_stub_code_range(0, STUB_CODE_RANGE_START));
        assert!(!is_in_stub_code_range(0xF000 - 1, STUB_CODE_RANGE_START));
    }

    #[test]
    fn bios_int8_isr_constants_match_hld() {
        assert_eq!(BIOS_INT8_ISR_START, 0xFEA5);
        assert_eq!(BIOS_INT8_ISR_END, 0xFEB5);
        assert_eq!(BIOS_INT8_ISR_END - BIOS_INT8_ISR_START, 16);
        assert_eq!(BIOS_INT8_TICK_TRAP_OFF, BIOS_INT8_ISR_START + 1);
    }

    #[test]
    fn is_in_bios_int8_isr_pass_through_covers_isr_body() {
        // First byte (STI) must pass through to the interpreter.
        assert!(is_in_bios_int8_isr_pass_through(0xF000, 0xFEA5));
        // PUSH DS / PUSH AX / PUSH DX through to IRET (FEA7..FEB4).
        for ip in 0xFEA7..=0xFEB4u16 {
            assert!(
                is_in_bios_int8_isr_pass_through(0xF000, ip),
                "IP {ip:#06X} must pass through"
            );
        }
    }

    #[test]
    fn is_in_bios_int8_isr_pass_through_excludes_trap_byte() {
        // The 0xF1 trap byte at F000:FEA6 must NOT pass through — it must
        // drop out to the dispatcher so the host-side BIOS tick fires.
        assert!(!is_in_bios_int8_isr_pass_through(
            0xF000,
            BIOS_INT8_TICK_TRAP_OFF
        ));
    }

    #[test]
    fn is_in_bios_int8_isr_pass_through_excludes_outside_isr() {
        assert!(!is_in_bios_int8_isr_pass_through(0xF000, 0xFEA4));
        assert!(!is_in_bios_int8_isr_pass_through(0xF000, BIOS_INT8_ISR_END));
        assert!(!is_in_bios_int8_isr_pass_through(0xF000, 0x0120));
    }

    #[test]
    fn is_in_bios_int8_isr_pass_through_excludes_other_segments() {
        // Non-ROM segments and protected-mode code selectors must not match
        // even if the offset is inside the ISR range.
        assert!(!is_in_bios_int8_isr_pass_through(0xF001, 0xFEA7));
        assert!(!is_in_bios_int8_isr_pass_through(
            0,
            BIOS_INT8_TICK_TRAP_OFF + 1
        ));
        assert!(!is_in_bios_int8_isr_pass_through(
            0xF000 - 1,
            BIOS_INT8_TICK_TRAP_OFF + 1
        ));
    }

    // ---- VBE window-function thunk ----

    #[test]
    fn vbe_window_func_thunk_range_is_gated_on_segment_and_bounds() {
        // Inside, both ends.
        assert!(is_in_vbe_window_func_thunk(0xF000, VBE_WINDOW_FUNC_START));
        assert!(is_in_vbe_window_func_thunk(0xF000, VBE_WINDOW_FUNC_END - 1));
        // Exclusive upper bound, and one below the start.
        assert!(!is_in_vbe_window_func_thunk(0xF000, VBE_WINDOW_FUNC_END));
        assert!(!is_in_vbe_window_func_thunk(
            0xF000,
            VBE_WINDOW_FUNC_START - 1
        ));
        // Wrong segment must never match, so a protected-mode code selector
        // cannot alias the thunk.
        assert!(!is_in_vbe_window_func_thunk(0xF001, VBE_WINDOW_FUNC_START));
        assert!(!is_in_vbe_window_func_thunk(0x0000, VBE_WINDOW_FUNC_START));
    }

    /// The reserved ranges must stay disjoint: the thunk executes as real code,
    /// while `F000:FE00..FFFF` is filled with protected-mode IRET stubs.
    #[test]
    fn reserved_f000_ranges_do_not_overlap() {
        assert!(
            VBE_WINDOW_FUNC_START >= STUB_CODE_RANGE_END,
            "thunk overlaps the keyboard stub range"
        );
        assert!(
            VBE_WINDOW_FUNC_END <= BIOS_INT8_ISR_START,
            "thunk overlaps the INT 8 ISR"
        );
        assert!(
            VBE_WINDOW_FUNC_START < VBE_WINDOW_FUNC_END,
            "thunk range must be non-empty"
        );
    }
}
