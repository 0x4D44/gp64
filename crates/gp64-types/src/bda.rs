//! BIOS Data Area (BDA) constants shared across crates.

/// BDA address of the floppy drive motor status byte (0040:003F).
///
/// Bits 0-3 are the motor-running flags for drives 0-3. Bits 4-5 select
/// the currently active drive, and the remaining bits are reserved /
/// reset-in-progress flags. The HLE INT 08h timer handler clears bits 0-3
/// when the shut-off counter at [`BDA_DISK_MOTOR_TIMEOUT_ADDR`] reaches
/// zero, matching the IBM PC/AT BIOS.
pub const BDA_DRIVE_RUNNING_ADDR: usize = 0x0040 * 16 + 0x003F;

/// BDA address of the floppy drive motor shut-off counter (0040:0040).
///
/// Decremented once per PIT tick by the BIOS INT 08h timer handler until
/// it reaches zero; on the tick that takes the counter to zero the motor
/// flags in [`BDA_DRIVE_RUNNING_ADDR`] are cleared. Software may use the
/// countdown as a coarse delay measured in BIOS timer ticks.
pub const BDA_DISK_MOTOR_TIMEOUT_ADDR: usize = 0x0040 * 16 + 0x0040;

/// BDA address of the 32-bit tick counter (0040:006C).
pub const BDA_TICK_COUNT_ADDR: usize = 0x0040 * 16 + 0x006C;

/// BDA address of the midnight rollover flag (0040:0070).
pub const BDA_MIDNIGHT_FLAG_ADDR: usize = 0x0040 * 16 + 0x0070;

/// Number of PIT ticks per day (18.2065 Hz * 86400 seconds = 1,573,040).
pub const BIOS_TICKS_PER_DAY: u32 = 0x0018_00B0;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_bda_drive_running_addr() {
        // 0040:003F = 0x43F
        assert_eq!(BDA_DRIVE_RUNNING_ADDR, 0x043F);
    }

    #[test]
    fn test_bda_disk_motor_timeout_addr() {
        // 0040:0040 = 0x440
        assert_eq!(BDA_DISK_MOTOR_TIMEOUT_ADDR, 0x0440);
    }

    #[test]
    fn test_bda_tick_count_addr() {
        // 0040:006C = 0x46C
        assert_eq!(BDA_TICK_COUNT_ADDR, 0x046C);
    }

    #[test]
    fn test_bda_midnight_flag_addr() {
        // 0040:0070 = 0x470
        assert_eq!(BDA_MIDNIGHT_FLAG_ADDR, 0x0470);
    }

    #[test]
    fn test_bios_ticks_per_day_value() {
        // 0x001800B0 = 1,573,040 (standard BIOS value)
        assert_eq!(BIOS_TICKS_PER_DAY, 0x0018_00B0);
        // Verify it's roughly 18.2 * 86400
        let expected = (18.2065 * 86400.0) as u32;
        assert!((BIOS_TICKS_PER_DAY as i64 - expected as i64).unsigned_abs() < 2000);
    }
}
