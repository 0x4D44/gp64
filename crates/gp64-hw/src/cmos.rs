//! PC CMOS policy around the machine-independent MC146818 RTC.
#[derive(Clone, Copy, Debug)]
pub struct CmosCheckpointState {
    pub index: u8,
    pub nmi_disabled: bool,
    pub ram: [u8; 128],
    pub periodic_accum_us: u64,
    pub last_second: u8,
    pub alarm_fired_this_second: bool,
    /// Status C pending-IRQ shadow (inner value of the `Cell<u8>`).
    pub pending_irq_flags: u8,
    pub uip_accum_us: u64,
    /// Cumulative emulated microseconds — the RTC clock accumulator.
    pub emulated_us: u64,
    /// Divider-driven clock; absent in legacy checkpoints.
    pub calendar_clock_us: Option<u64>,
    /// Guest-applied RTC offset in seconds (from time-register writes / INT 1Ah).
    pub time_offset_secs: i64,
    /// The calendar Status-B SET is holding still, when it is asserted.
    ///
    /// Not derivable from the rest of this struct: the hold is taken at the
    /// instant SET rises and `emulated_us` keeps climbing afterwards, so
    /// rebuilding it from the restored clock returns the LATER raw time and
    /// moves the guest-visible held calendar forward (MDD-BUG-KIL-01039).
    pub set_frozen_secs: Option<u64>,
    /// Independent calendar locations programmed while SET inhibits updates.
    pub set_calendar_fields: Option<gp64_rtc::RtcCalendarFields>,
    /// Independent weekday counter from the RTC calendar.
    pub day_of_week: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CmosDiagnosticSnapshot {
    pub selected_index: u8,
    pub nmi_disabled: bool,
    pub selected_effective_value: u8,
    pub status_a_raw: u8,
    pub status_a_effective: u8,
    pub status_b: u8,
    pub status_c_pending: u8,
    pub status_d: u8,
    pub irqf_pending: bool,
    pub periodic_pending: bool,
    pub alarm_pending: bool,
    pub update_ended_pending: bool,
    pub has_pending_interrupt: bool,
    pub periodic_interrupt_enabled: bool,
    pub periodic_period_us: Option<u64>,
    pub periodic_accum_us: u64,
    pub uip_accum_us: u64,
    pub update_in_progress: bool,
    pub emulated_us: u64,
    pub startup_epoch_secs: u64,
    pub time_offset_secs: i64,
    pub adjusted_seconds: u8,
    pub adjusted_minutes: u8,
    pub adjusted_hours: u8,
    pub adjusted_day_of_week: u8,
    pub adjusted_day: u8,
    pub adjusted_month: u8,
    pub adjusted_year: u16,
    pub encoded_seconds: u8,
    pub encoded_minutes: u8,
    pub encoded_hours: u8,
    pub encoded_day: u8,
    pub encoded_month: u8,
    pub encoded_year: u8,
    pub encoded_century: u8,
    pub event_reschedule_pending: bool,
}
use std::ops::{Deref, DerefMut};
use std::time::SystemTime;
pub mod regs {
    pub use gp64_rtc::regs::*;
    pub const CENTURY: u8 = 0x32;
    pub const CENTURY_PS2: u8 = 0x37;
    pub const DIAGNOSTIC: u8 = 0x0E;
    pub const SHUTDOWN: u8 = 0x0F;
    pub const FLOPPY_TYPES: u8 = 0x10;
    pub const HARD_DISK_TYPES: u8 = 0x12;
    pub const EQUIPMENT: u8 = 0x14;
    pub const BASE_MEMORY_LO: u8 = 0x15;
    pub const BASE_MEMORY_HI: u8 = 0x16;
    pub const EXT_MEMORY_LO: u8 = 0x17;
    pub const EXT_MEMORY_HI: u8 = 0x18;
}

/// PC CMOS defaults and NVRAM policy, backed by the shared RTC chip.
#[derive(Clone)]
pub struct Cmos {
    chip: gp64_rtc::Mc146818,
    nmi_disabled: bool,
}

impl Deref for Cmos {
    type Target = gp64_rtc::Mc146818;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.chip
    }
}
impl DerefMut for Cmos {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.chip
    }
}
impl Default for Cmos {
    fn default() -> Self {
        Self::new()
    }
}
impl Cmos {
    /// Construct with the host clock supplying the legacy PC boot epoch.
    pub fn new() -> Self {
        Self::new_with_fixed_boot_epoch(None)
    }
    /// `Some(secs)` pins the boot epoch; `None` samples the host clock once.
    pub fn new_with_fixed_boot_epoch(epoch: Option<u64>) -> Self {
        Self::new_with_fixed_boot_epoch_and_ram_size(epoch, 128 * 1024 * 1024)
    }
    /// Seed the PC BIOS layout with the supplied epoch and guest RAM size.
    pub fn new_with_fixed_boot_epoch_and_ram_size(epoch: Option<u64>, ram_size_bytes: u64) -> Self {
        let mut cmos = Self {
            chip: gp64_rtc::Mc146818::new_with_century_registers(
                Self::compute_boot_epoch_secs(epoch),
                [Some(regs::CENTURY), Some(regs::CENTURY_PS2)],
            ),
            nmi_disabled: false,
        };
        cmos.init_defaults();
        cmos.set_ram_size(ram_size_bytes);
        cmos
    }
    /// Initialize CMOS with sensible defaults
    fn init_defaults(&mut self) {
        // Status Register A: Normal operation, 32.768 KHz time base
        // Bits 6-4: Divider (010 = 32.768 KHz)
        // Bits 3-0: Rate selection (0110 = 1024 Hz)
        self.chip.nvram_mut()[regs::STATUS_A as usize] = 0x26;

        // Status Register B: 24-hour mode, BCD format, DST disabled
        // Bit 7: Update cycle inhibit (0 = normal)
        // Bit 6: Periodic interrupt enable (0 = disabled)
        // Bit 5: Alarm interrupt enable (0 = disabled)
        // Bit 4: Update-ended interrupt enable (0 = disabled)
        // Bit 3: Square wave enable (0 = disabled)
        // Bit 2: Date mode (0 = BCD)
        // Bit 1: 24/12 hour (1 = 24-hour)
        // Bit 0: DST enable (0 = disabled)
        self.chip.nvram_mut()[regs::STATUS_B as usize] = 0x02;

        // Status Register C: No pending interrupts
        self.chip.nvram_mut()[regs::STATUS_C as usize] = 0x00;

        // Status Register D: Battery OK (bit 7 = 1)
        self.chip.nvram_mut()[regs::STATUS_D as usize] = 0x80;

        // Diagnostic: Power on normally, no errors
        self.chip.nvram_mut()[regs::DIAGNOSTIC as usize] = 0x00;

        // Shutdown status: Normal POST
        self.chip.nvram_mut()[regs::SHUTDOWN as usize] = 0x00;

        // Floppy types: Drive A = 1.44MB (4), Drive B = none (0)
        // High nibble = drive A type, Low nibble = drive B type
        self.chip.nvram_mut()[regs::FLOPPY_TYPES as usize] = 0x40;

        // Hard disk types: High nibble = drive 0, Low nibble = drive 1
        // Type F = user-defined (parameters in extended CMOS area)
        self.chip.nvram_mut()[regs::HARD_DISK_TYPES as usize] = 0xF0;

        // Extended HD parameters for type F.
        // 0x19 = HD0 extended type, 0x1A = HD1 extended type,
        // 0x1B-0x23 = HD0 user-defined parameters (type 47).
        // 10MB disk: 306 cylinders, 4 heads, 17 sectors/track
        self.chip.nvram_mut()[0x19] = 0x2F; // HD0 extended type 47 (user-defined)
        self.chip.nvram_mut()[0x1A] = 0x00; // HD1 extended type (none)
        self.chip.nvram_mut()[0x1B] = 0x32; // Cylinders low (306 = 0x0132)
        self.chip.nvram_mut()[0x1C] = 0x01; // Cylinders high
        self.chip.nvram_mut()[0x1D] = 0x04; // Heads
        self.chip.nvram_mut()[0x1E] = 0xFF; // Write precomp low (0xFFFF = none)
        self.chip.nvram_mut()[0x1F] = 0xFF; // Write precomp high
        self.chip.nvram_mut()[0x20] = 0x00; // Control byte
        self.chip.nvram_mut()[0x21] = 0x32; // Landing zone low (306)
        self.chip.nvram_mut()[0x22] = 0x01; // Landing zone high
        self.chip.nvram_mut()[0x23] = 0x11; // Sectors per track (17)

        // Equipment byte:
        // Bits 7-6: Number of floppy drives - 1 (00 = 1 drive)
        // Bits 5-4: Video mode (00 = EGA/VGA)
        // Bit 3: Not used
        // Bit 2: Pointing device (1 = installed)
        // Bit 1: Math coprocessor (1 = installed)
        // Bit 0: Floppy drive(s) present (1 = yes)
        self.chip.nvram_mut()[regs::EQUIPMENT as usize] = 0x07; // Floppy, FPU, mouse

        // Base memory: 640KB (0x0280 in KB)
        self.chip.nvram_mut()[regs::BASE_MEMORY_LO as usize] = 0x80;
        self.chip.nvram_mut()[regs::BASE_MEMORY_HI as usize] = 0x02;

        // Century: 20 (for 2000s)
        self.chip.nvram_mut()[regs::CENTURY as usize] = 0x20;
    }

    /// Update legacy CMOS extended-memory registers for the guest-visible RAM.
    ///
    /// Registers 0x17:0x18 and 0x30:0x31 report extended memory between 1 MiB
    /// and 16 MiB in KiB. Registers 0x34:0x35 report memory above 16 MiB in
    /// 64 KiB units, using the extended CMOS memory-size convention.
    pub fn set_ram_size(&mut self, ram_size_bytes: u64) {
        const MIB: u64 = 1024 * 1024;
        const BASE_MEMORY_KB: u16 = 640;
        const LOW_EXT_LIMIT_BYTES: u64 = 16 * MIB;

        self.chip.nvram_mut()[regs::BASE_MEMORY_LO as usize] = (BASE_MEMORY_KB & 0xFF) as u8;
        self.chip.nvram_mut()[regs::BASE_MEMORY_HI as usize] = (BASE_MEMORY_KB >> 8) as u8;

        let low_ext_bytes = ram_size_bytes
            .saturating_sub(MIB)
            .min(LOW_EXT_LIMIT_BYTES - MIB);
        let low_ext_kb = (low_ext_bytes / 1024).min(u16::MAX as u64) as u16;
        self.chip.nvram_mut()[regs::EXT_MEMORY_LO as usize] = (low_ext_kb & 0xFF) as u8;
        self.chip.nvram_mut()[regs::EXT_MEMORY_HI as usize] = (low_ext_kb >> 8) as u8;
        self.chip.nvram_mut()[0x30] = self.chip.nvram_mut()[regs::EXT_MEMORY_LO as usize];
        self.chip.nvram_mut()[0x31] = self.chip.nvram_mut()[regs::EXT_MEMORY_HI as usize];

        let high_ext_units = (ram_size_bytes.saturating_sub(LOW_EXT_LIMIT_BYTES) / (64 * 1024))
            .min(u16::MAX as u64) as u16;
        self.chip.nvram_mut()[0x34] = (high_ext_units & 0xFF) as u8;
        self.chip.nvram_mut()[0x35] = (high_ext_units >> 8) as u8;
    }

    /// Stamp CMOS register 0x10 (floppy drive types) to the supplied encoded
    /// byte. Upper nibble = drive 0 type, lower nibble = drive 1 type.
    ///
    /// The boot path uses this to advertise the `--boot-floppy` drive to firmware.
    /// The nibble value for a 1.44 MB 3.5" drive is 4; no-drive is 0. Callers pass the fully-packed byte (e.g. `0x40` for "drive 0 =
    /// 1.44 MB, drive 1 = none").
    pub fn set_floppy_types(&mut self, value: u8) {
        self.chip.nvram_mut()[regs::FLOPPY_TYPES as usize] = value;
    }

    /// Update CMOS equipment register 0x14 for the reported number of floppy
    /// drives while preserving unrelated equipment bits.
    pub fn set_floppy_drive_count(&mut self, count: u8) {
        let equipment = &mut self.chip.nvram_mut()[regs::EQUIPMENT as usize];
        *equipment &= !0xC1;
        match count {
            0 => {}
            1 => *equipment |= 0x01,
            _ => *equipment |= 0x41,
        }
    }

    /// Return the IBM AT shutdown-status byte at CMOS register 0x0F.
    pub fn shutdown_status(&self) -> u8 {
        self.chip.nvram()[regs::SHUTDOWN as usize]
    }

    /// Clear the IBM AT shutdown-status byte after BIOS has consumed it.
    pub fn clear_shutdown_status(&mut self) {
        self.chip.nvram_mut()[regs::SHUTDOWN as usize] = 0x00;
    }

    // Explicit methods preserve PC call syntax and two-phase receiver borrows.
    #[inline]
    pub fn get_rtc_time_bcd(&self) -> (u8, u8, u8) {
        self.chip.get_rtc_time_bcd()
    }
    #[inline]
    pub fn get_rtc_date_bcd(&self) -> (u8, u8, u8, u8) {
        self.chip.get_rtc_date_bcd()
    }
    #[inline]
    pub fn set_rtc_time_bcd(&mut self, hours_bcd: u8, minutes_bcd: u8, seconds_bcd: u8) {
        self.chip
            .set_rtc_time_bcd(hours_bcd, minutes_bcd, seconds_bcd)
    }
    #[inline]
    pub fn set_rtc_date_bcd(&mut self, century_bcd: u8, year_bcd: u8, month_bcd: u8, day_bcd: u8) {
        self.chip
            .set_rtc_date_bcd(century_bcd, year_bcd, month_bcd, day_bcd)
    }
    #[inline]
    pub fn tick(&mut self, elapsed_us: u64) -> bool {
        self.chip.tick(elapsed_us)
    }
    #[inline]
    pub fn advance_clock(&mut self, elapsed_us: u64) -> bool {
        self.chip.advance_clock(elapsed_us)
    }
    #[inline]
    pub fn tick_periodic(&mut self, elapsed_us: u64) -> bool {
        self.chip.tick_periodic(elapsed_us)
    }
    #[inline]
    pub fn is_periodic_interrupt_enabled(&self) -> bool {
        self.chip.is_periodic_interrupt_enabled()
    }
    #[inline]
    pub fn get_periodic_frequency_hz(&self) -> Option<u64> {
        self.chip.get_periodic_frequency_hz()
    }
    #[inline]
    pub fn get_periodic_period_ratio_us(&self) -> Option<(u64, u64)> {
        self.chip.get_periodic_period_ratio_us()
    }
    #[inline]
    pub fn get_periodic_period_us(&self) -> Option<u64> {
        self.chip.get_periodic_period_us()
    }
    #[inline]
    pub fn set_alarm_bcd(&mut self, hours_bcd: u8, minutes_bcd: u8, seconds_bcd: u8) {
        self.chip.set_alarm_bcd(hours_bcd, minutes_bcd, seconds_bcd)
    }
    #[inline]
    pub fn disable_alarm(&mut self) {
        self.chip.disable_alarm()
    }
    #[inline]
    pub fn has_pending_interrupt(&self) -> bool {
        self.chip.has_pending_interrupt()
    }
    #[inline]
    pub fn peek_pending_flags(&self) -> u8 {
        self.chip.peek_pending_flags()
    }
    #[inline]
    pub fn time_offset_secs(&self) -> i64 {
        self.chip.time_offset_secs()
    }
    #[inline]
    pub fn set_time_offset_secs(&mut self, secs: i64) {
        self.chip.set_time_offset_secs(secs)
    }
    /// Read the PC index latch including its NMI mask bit.
    #[inline]
    pub fn read_index(&self) -> u8 {
        self.chip.read_index() | if self.nmi_disabled { 0x80 } else { 0 }
    }
    /// Save both chip state and the PC NMI mask.
    pub fn save_state(&self) -> CmosCheckpointState {
        let state = self.chip.save_state();
        CmosCheckpointState {
            index: state.index,
            nmi_disabled: self.nmi_disabled,
            ram: state.ram,
            periodic_accum_us: state.periodic_accum_us,
            last_second: state.last_second,
            alarm_fired_this_second: state.alarm_fired_this_second,
            pending_irq_flags: state.pending_irq_flags,
            uip_accum_us: state.uip_accum_us,
            emulated_us: state.emulated_us,
            time_offset_secs: state.time_offset_secs,
            calendar_clock_us: state.calendar_clock_us,
            set_frozen_secs: state.set_frozen_secs,
            set_calendar_fields: state.set_calendar_fields,
            day_of_week: state.day_of_week,
        }
    }
    /// Restore PC checkpoint state.
    pub fn restore_state(&mut self, state: CmosCheckpointState) {
        self.nmi_disabled = state.nmi_disabled;
        self.chip.restore_state(gp64_rtc::RtcCheckpointState {
            index: state.index,
            ram: state.ram,
            periodic_accum_us: state.periodic_accum_us,
            last_second: state.last_second,
            alarm_fired_this_second: state.alarm_fired_this_second,
            pending_irq_flags: state.pending_irq_flags,
            uip_accum_us: state.uip_accum_us,
            emulated_us: state.emulated_us,
            time_offset_secs: state.time_offset_secs,
            calendar_clock_us: state.calendar_clock_us,
            set_frozen_secs: state.set_frozen_secs,
            set_calendar_fields: state.set_calendar_fields,
            day_of_week: state.day_of_week,
        });
    }
    /// Peek the RTC and PC index state without clearing interrupt flags.
    pub fn diagnostic_snapshot(&self) -> CmosDiagnosticSnapshot {
        let snapshot = self.chip.diagnostic_snapshot();
        CmosDiagnosticSnapshot {
            selected_index: snapshot.selected_index,
            nmi_disabled: self.nmi_disabled,
            selected_effective_value: snapshot.selected_effective_value,
            status_a_raw: snapshot.status_a_raw,
            status_a_effective: snapshot.status_a_effective,
            status_b: snapshot.status_b,
            status_c_pending: snapshot.status_c_pending,
            status_d: snapshot.status_d,
            irqf_pending: snapshot.irqf_pending,
            periodic_pending: snapshot.periodic_pending,
            alarm_pending: snapshot.alarm_pending,
            update_ended_pending: snapshot.update_ended_pending,
            has_pending_interrupt: snapshot.has_pending_interrupt,
            periodic_interrupt_enabled: snapshot.periodic_interrupt_enabled,
            periodic_period_us: snapshot.periodic_period_us,
            periodic_accum_us: snapshot.periodic_accum_us,
            uip_accum_us: snapshot.uip_accum_us,
            update_in_progress: snapshot.update_in_progress,
            emulated_us: snapshot.emulated_us,
            startup_epoch_secs: snapshot.startup_epoch_secs,
            time_offset_secs: snapshot.time_offset_secs,
            adjusted_seconds: snapshot.adjusted_seconds,
            adjusted_minutes: snapshot.adjusted_minutes,
            adjusted_hours: snapshot.adjusted_hours,
            adjusted_day_of_week: snapshot.adjusted_day_of_week,
            adjusted_day: snapshot.adjusted_day,
            adjusted_month: snapshot.adjusted_month,
            adjusted_year: snapshot.adjusted_year,
            encoded_seconds: snapshot.encoded_seconds,
            encoded_minutes: snapshot.encoded_minutes,
            encoded_hours: snapshot.encoded_hours,
            encoded_day: snapshot.encoded_day,
            encoded_month: snapshot.encoded_month,
            encoded_year: snapshot.encoded_year,
            encoded_century: snapshot.encoded_century,
            event_reschedule_pending: snapshot.event_reschedule_pending,
        }
    }
    /// Select a PC CMOS register and update the NMI mask bit.
    #[inline]
    pub fn write_index(&mut self, value: u8) {
        self.nmi_disabled = value & 0x80 != 0;
        self.chip.write_index(value & 0x7f);
    }
    /// Write the selected register and return any RTC IRQ output transition.
    pub fn write_data(&mut self, value: u8) -> Option<bool> {
        self.chip.write_data(value)
    }
    /// Read the selected register (Status C clears its interrupt flags).
    pub fn read_data(&self) -> u8 {
        self.chip.read_data()
    }

    fn compute_boot_epoch_secs(fixed_epoch_secs: Option<u64>) -> u64 {
        #[cfg(test)]
        if let Some((sec, min, hr, _, d, mon, y)) = test_time_override() {
            return test_epoch_secs(sec, min, hr, d, mon, y) as u64;
        }

        if let Some(secs) = fixed_epoch_secs {
            return secs;
        }

        use std::time::{Duration, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
    }

    // =========================================================================
    // Firmware config block (CMOS RAM 0x40..=0x4F, HLD §3 / §10)
    // =========================================================================
}
#[cfg(test)]
type TestTime = (u8, u8, u8, u8, u8, u8, u16);

#[cfg(test)]
thread_local! {
    static TEST_TIME: std::cell::RefCell<Option<TestTime>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct TestTimeGuard;

#[cfg(test)]
impl TestTimeGuard {
    fn set(time: (u8, u8, u8, u8, u8, u8, u16)) -> Self {
        TEST_TIME.with(|cell| {
            *cell.borrow_mut() = Some(time);
        });
        Self
    }
}

#[cfg(test)]
impl Drop for TestTimeGuard {
    fn drop(&mut self) {
        TEST_TIME.with(|cell| {
            *cell.borrow_mut() = None;
        });
    }
}

#[cfg(test)]
fn test_time_override() -> Option<TestTime> {
    TEST_TIME.with(|cell| *cell.borrow())
}

#[cfg(test)]
fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}
#[cfg(test)]
fn test_epoch_secs(sec: u8, min: u8, hr: u8, day: u8, mon: u8, year: u16) -> i64 {
    // Only the two original June 1, 2026 PC constructor test vectors use this helper.
    assert_eq!((day, mon, year), (1, 6, 2026));
    1_780_272_000 + hr as i64 * 3600 + min as i64 * 60 + sec as i64
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn checkpoint_preserves_programmed_weekday() {
        let mut cmos = Cmos::new_with_fixed_boot_epoch(Some(86_399));
        cmos.write_index(regs::STATUS_B);
        cmos.write_data(0x82);
        cmos.write_index(regs::DAY_OF_WEEK);
        cmos.write_data(7);
        let mut restored = Cmos::new_with_fixed_boot_epoch(Some(86_399));
        restored.restore_state(cmos.save_state());
        assert_eq!(
            restored.read_data(),
            7,
            "PC checkpoint retains programmed weekday"
        );
        restored.write_index(regs::STATUS_B);
        restored.write_data(0x02);
        restored.advance_clock(1_000_000);
        restored.write_index(regs::DAY_OF_WEEK);
        assert_eq!(
            restored.read_data(),
            1,
            "PC checkpoint retains weekday midnight behavior"
        );
    }

    #[test]
    fn test_cmos_defaults() {
        let cmos = Cmos::new();
        // Status D should indicate battery OK
        assert_eq!(cmos.chip.nvram()[regs::STATUS_D as usize] & 0x80, 0x80);
        // Base memory should be 640KB
        let base_mem = cmos.chip.nvram()[regs::BASE_MEMORY_LO as usize] as u16
            | ((cmos.chip.nvram()[regs::BASE_MEMORY_HI as usize] as u16) << 8);
        assert_eq!(base_mem, 640);
    }
    #[test]
    fn test_cmos_reports_configured_16mb_ram() {
        let cmos = Cmos::new_with_fixed_boot_epoch_and_ram_size(None, 16 * 1024 * 1024);

        let low_ext = cmos.chip.nvram()[regs::EXT_MEMORY_LO as usize] as u16
            | ((cmos.chip.nvram()[regs::EXT_MEMORY_HI as usize] as u16) << 8);
        let mirror = cmos.chip.nvram()[0x30] as u16 | ((cmos.chip.nvram()[0x31] as u16) << 8);
        let above_16mb = cmos.chip.nvram()[0x34] as u16 | ((cmos.chip.nvram()[0x35] as u16) << 8);

        assert_eq!(low_ext, 0x3C00);
        assert_eq!(mirror, low_ext);
        assert_eq!(above_16mb, 0);
    }
    #[test]
    fn test_cmos_reports_default_128mb_ram_above_16mb() {
        let cmos = Cmos::new();
        let above_16mb = cmos.chip.nvram()[0x34] as u16 | ((cmos.chip.nvram()[0x35] as u16) << 8);

        assert_eq!(above_16mb, 0x0700);
    }
    #[test]
    fn test_cmos_boot_time_uses_host_time_of_day() {
        let _guard = TestTimeGuard::set((56, 34, 12, 1, 1, 6, 2026));
        let cmos = Cmos::new();

        let (hours_bcd, minutes_bcd, seconds_bcd) = cmos.get_rtc_time_bcd();
        assert_eq!(hours_bcd, to_bcd(12));
        assert_eq!(minutes_bcd, to_bcd(34));
        assert_eq!(seconds_bcd, to_bcd(56));
    }
    #[test]
    fn test_fixed_boot_epoch_preserves_time_of_day() {
        let fixed_epoch = test_epoch_secs(17, 20, 5, 1, 6, 2026) as u64;
        let cmos = Cmos::new_with_fixed_boot_epoch(Some(fixed_epoch));

        let (hours_bcd, minutes_bcd, seconds_bcd) = cmos.get_rtc_time_bcd();
        assert_eq!(hours_bcd, to_bcd(5));
        assert_eq!(minutes_bcd, to_bcd(20));
        assert_eq!(seconds_bcd, to_bcd(17));
    }
    #[test]
    fn test_cmos_default_trait() {
        // Covers Default::default() -> Self::new()
        let cmos = Cmos::default();
        assert_eq!(cmos.chip.nvram()[regs::STATUS_D as usize] & 0x80, 0x80);
        let base_mem = cmos.chip.nvram()[regs::BASE_MEMORY_LO as usize] as u16
            | ((cmos.chip.nvram()[regs::BASE_MEMORY_HI as usize] as u16) << 8);
        assert_eq!(base_mem, 640);
    }

    #[test]
    fn test_cmos_index_write() {
        let mut cmos = Cmos::new();
        cmos.write_index(0x0A);
        assert_eq!(cmos.chip.read_index(), 0x0A);
        assert_eq!(cmos.read_index(), 0x0A);
        // NMI bit should be preserved on readback
        cmos.write_index(0x8A);
        assert_eq!(cmos.chip.read_index(), 0x0A);
        assert_eq!(cmos.read_index(), 0x8A);
    }

    #[test]
    fn set_assertion_through_cmos_wrapper_deasserts_irqf() {
        let mut cmos = Cmos::new_with_fixed_boot_epoch(Some(1_718_452_800));
        let mut state = cmos.save_state();
        state.ram[regs::STATUS_B as usize] = 0x12; // UIE + unrelated bit 1
        state.pending_irq_flags = 0x90; // IRQF + pending UF
        cmos.restore_state(state);
        assert_eq!(
            cmos.peek_pending_flags() & 0x80,
            0x80,
            "precondition: pending UF with UIE asserts IRQF before SET"
        );

        cmos.write_index(regs::STATUS_B);
        let transition = cmos.write_data(0x92);
        cmos.write_index(regs::STATUS_B);
        let observed = (cmos.read_data(), cmos.peek_pending_flags(), transition);

        assert_eq!(
            observed,
            (0x82, 0x10, Some(false)),
            "the PC CMOS register wrapper must expose SET's IRQF deassertion"
        );
    }

    #[test]
    fn test_cmos_divider_reset_checkpoint_preserves_clock_phase() {
        let mut cmos = Cmos::new_with_fixed_boot_epoch(Some(0));
        cmos.advance_clock(234_567);
        cmos.write_index(regs::STATUS_A);
        cmos.write_data(0x76);
        cmos.advance_clock(3_000_000);
        let state = cmos.save_state();
        let mut restored = Cmos::new_with_fixed_boot_epoch(Some(0));
        restored.restore_state(state);
        assert_eq!(restored.get_rtc_time_bcd(), (0, 0, 0));
        restored.advance_clock(2_000_000);
        restored.write_index(regs::STATUS_A);
        assert_eq!(restored.read_data(), 0x76);
        restored.write_data(0x26);
        restored.advance_clock(499_999);
        assert_eq!(restored.get_rtc_time_bcd(), (0, 0, 0));
        restored.advance_clock(1);
        assert_eq!(restored.get_rtc_time_bcd(), (0, 0, 1));
        let state = restored.save_state();
        cmos.restore_state(state);
        assert_eq!(cmos.save_state().calendar_clock_us, state.calendar_clock_us);
        assert_eq!(cmos.get_rtc_time_bcd(), restored.get_rtc_time_bcd());
    }

    #[test]
    fn test_checkpoint_roundtrip_preserves_live_rtc_state() {
        // MDD-BUG-CRUCIBLE-00031: the time-machine checkpoint must save+restore ALL
        // live RTC state — the timing accumulators, the Status C pending-IRQ shadow,
        // alarm suppression, and the emulated-clock state (emulated_us +
        // time_offset_secs) — not reset it to zero. The old save_state()/
        // restore_state() dropped every one, so a restored checkpoint resumed with a
        // different RTC clock and IRQ8/Status-C state.
        let epoch = 1_718_452_800; // 2024-06-15 12:00:00 UTC

        let mut cmos = Cmos::new_with_fixed_boot_epoch(Some(epoch));
        cmos.chip.nvram_mut()[regs::STATUS_B as usize] = 0x40 | 0x10; // PIE | UIE
        cmos.chip.nvram_mut()[regs::STATUS_A as usize] = 0x0F; // rate 15 -> 500 ms periodic period
        cmos.tick(1_100_000); // cross a second (UF) + fire periodic (PF): Status C latches
        cmos.tick(200_000); // leave a non-zero partial periodic phase
        let mut state = cmos.save_state();
        state.alarm_fired_this_second = true; // distinctive alarm-suppression latch
        state.time_offset_secs = 3600; // distinctive guest RTC offset
        cmos.restore_state(state);

        cmos.write_index(0x8A); // distinctive PC-owned NMI mask
        let saved = cmos.save_state();
        // Precondition: distinctive live state accumulated — every field below is
        // reset to zero / dropped by the OLD restore_state.
        assert_ne!(saved.emulated_us, 0, "clock advanced");
        assert_ne!(saved.periodic_accum_us, 0, "partial periodic phase");
        assert_ne!(saved.uip_accum_us, 0, "partial UIP phase");
        assert_ne!(saved.last_second, 0, "second edge advanced");
        assert_ne!(saved.pending_irq_flags, 0, "Status C flags pending");

        // Restore into a FRESH cmos (which starts fully zeroed).
        let mut restored = Cmos::new_with_fixed_boot_epoch(Some(epoch));
        restored.restore_state(saved);
        let after = restored.save_state();

        assert_eq!(
            after.emulated_us, saved.emulated_us,
            "emulated_us (RTC clock) must survive restore"
        );
        assert_eq!(
            after.time_offset_secs, saved.time_offset_secs,
            "time_offset_secs must survive restore"
        );
        assert_eq!(
            after.periodic_accum_us, saved.periodic_accum_us,
            "periodic accumulator must survive"
        );
        assert_eq!(
            after.uip_accum_us, saved.uip_accum_us,
            "UIP accumulator must survive"
        );
        assert_eq!(
            after.last_second, saved.last_second,
            "update-ended edge must survive"
        );
        assert_eq!(
            after.alarm_fired_this_second, saved.alarm_fired_this_second,
            "alarm suppression latch must survive"
        );
        assert_eq!(
            after.pending_irq_flags, saved.pending_irq_flags,
            "Status C pending-IRQ shadow must survive"
        );
        assert_eq!(after.index, saved.index);
        assert_eq!(after.nmi_disabled, saved.nmi_disabled);
        assert!(after.nmi_disabled);
        assert_eq!(restored.read_index(), 0x8A);
        assert_eq!(after.ram, saved.ram);
    }

    #[test]
    fn pc_century_aliases_follow_calendar_and_clamp_leap_days() {
        let mut cmos = Cmos::new_with_fixed_boot_epoch(Some(951_782_400)); // 2000-02-29
        for register in [regs::CENTURY, regs::CENTURY_PS2] {
            cmos.write_index(register);
            assert_eq!(cmos.read_data(), 0x20);
        }
        cmos.write_index(regs::CENTURY_PS2);
        cmos.write_data(0x21); // 2100 is not a leap year.
        assert_eq!(cmos.get_rtc_date_bcd(), (0x21, 0x00, 0x02, 0x28));
        for register in [regs::CENTURY, regs::CENTURY_PS2] {
            cmos.write_index(register);
            assert_eq!(cmos.read_data(), 0x21);
        }
        cmos.write_index(regs::CENTURY);
        cmos.write_data(0x20);
        cmos.write_index(regs::CENTURY_PS2);
        assert_eq!(cmos.read_data(), 0x20);
        assert_eq!(cmos.get_rtc_date_bcd(), (0x20, 0x00, 0x02, 0x28));
    }
}
