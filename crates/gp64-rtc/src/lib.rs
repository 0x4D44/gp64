//! MC146818-compatible RTC and raw battery-backed storage.
//! The caller supplies the boot epoch; guest elapsed time drives the clock.
//!
//! Indexed register access is exposed without machine address decoding.
//! It contains 64-128 bytes of battery-backed RAM, with the first 14 bytes
//! used for RTC time/date and status registers.
//!
//! ## RTC interrupts
//!
//! The RTC can generate three types of interrupts:
//! - **Periodic (PIE)**: Fires at programmable rate (2Hz to 8192Hz)
//! - **Alarm (AIE)**: Fires when time matches alarm registers
//! - **Update-ended (UIE)**: Fires once per second after time update
//!
//! All three sources share the interrupt output. Status C indicates which fired.
//! Reading Status C clears the interrupt flags.

use std::cell::Cell;

/// Standard CMOS register addresses
pub mod regs {
    pub const SECONDS: u8 = 0x00;
    pub const SECONDS_ALARM: u8 = 0x01;
    pub const MINUTES: u8 = 0x02;
    pub const MINUTES_ALARM: u8 = 0x03;
    pub const HOURS: u8 = 0x04;
    pub const HOURS_ALARM: u8 = 0x05;
    pub const DAY_OF_WEEK: u8 = 0x06;
    pub const DAY_OF_MONTH: u8 = 0x07;
    pub const MONTH: u8 = 0x08;
    pub const YEAR: u8 = 0x09;
    pub const STATUS_A: u8 = 0x0A;
    pub const STATUS_B: u8 = 0x0B;
    pub const STATUS_C: u8 = 0x0C;
    pub const STATUS_D: u8 = 0x0D;
}

/// `last_second` sentinel: no second has been observed yet, so the next tick
/// syncs the tracker without claiming an update cycle completed. Not a valid
/// BCD or binary seconds value, so it cannot collide with a real reading.
const NO_PREVIOUS_SECOND: u8 = 0xFF;

/// Independently programmable calendar locations held while Status-B SET is asserted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtcCalendarFields {
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub day: u8,
    pub month: u8,
    pub year: u16,
}

impl RtcCalendarFields {
    fn from_epoch(epoch: u64) -> Self {
        let (seconds, minutes, hours, _, day, month, year) =
            Mc146818::compute_time_components(epoch);
        Self {
            seconds,
            minutes,
            hours,
            day,
            month,
            year,
        }
    }

    /// Project onto an epoch without changing the independent held fields.
    /// SET clear commits this projection using the existing day-clamp policy.
    fn normalized_epoch(self) -> u64 {
        Mc146818::time_to_epoch_secs(
            self.seconds,
            self.minutes,
            self.hours,
            self.day.min(days_in_month(self.month, self.year)),
            self.month,
            self.year,
        )
        .max(0) as u64
    }
}

/// A lossless snapshot of the mutable CMOS/RTC device state for time-machine
/// checkpoints. Captures the RTC interrupt/timing accumulators, the Status C
/// pending-IRQ shadow, alarm suppression, and the emulated-clock state that the
/// old `(index, nmi, ram)` tuple dropped. `startup_epoch_secs` is intentionally
/// omitted: it is set once at construction and never mutated, so a same-process
/// in-memory restore already holds the correct value. MDD-BUG-CRUCIBLE-00031.
#[derive(Clone, Copy, Debug)]
pub struct RtcCheckpointState {
    pub index: u8,
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
    /// Independent fields; an intermediate date need not fit a calendar epoch.
    pub set_calendar_fields: Option<RtcCalendarFields>,
    /// Independent weekday counter, advancing only at calendar midnights.
    pub day_of_week: u8,
}

enum PeriodicTick {
    None,
    Elapsed,
    Event,
}

/// CMOS RAM and RTC controller
#[derive(Clone)]
pub struct Mc146818 {
    /// Currently selected register index
    index: u8,
    /// Caller-selected century extensions; absent slots remain raw NVRAM.
    century_registers: [Option<u8>; 2],
    /// CMOS RAM (128 bytes for extended CMOS)
    ram: [u8; 128],

    // --- RTC Interrupt State ---
    /// Periodic interrupt accumulator (microseconds since last periodic IRQ)
    periodic_accum_us: u64,
    /// Last second value for update-ended edge detection
    last_second: u8,
    /// Alarm fired flag (reset when second changes, prevents repeated firing)
    alarm_fired_this_second: bool,
    /// Pending interrupt flags (Status C shadow)
    /// Bit 7: IRQF (any interrupt active)
    /// Bit 6: PF (periodic interrupt)
    /// Bit 5: AF (alarm interrupt)
    /// Bit 4: UF (update-ended interrupt)
    /// Uses Cell for interior mutability (read_data clears flags)
    pending_irq_flags: Cell<u8>,

    // --- UIP (Update-In-Progress) simulation ---
    /// Microseconds accumulated for UIP timing within each second
    uip_accum_us: u64,

    // --- Time offset for writable RTC ---
    /// Offset in seconds applied to system time when reading RTC.
    /// Allows programs to set the RTC time by writing to time registers.
    time_offset_secs: i64,

    /// Cumulative emulated microseconds since the CMOS was created.
    /// Monotonic elapsed-time accounting, including time spent in divider reset.
    /// Accumulated by clock advancement and elapsed-time tick callers.
    emulated_us: u64,
    /// Calendar divider time, stopped by DV reset even while emulated time advances.
    calendar_clock_us: u64,

    /// Epoch projection of the calendar held while Status-B SET is asserted.
    /// Independent fields below preserve intermediate dates that cannot be
    /// represented as an epoch (MDD-BUG-CRUCIBLE-00552).
    ///
    /// The MC146818A's SET bit aborts and inhibits update cycles so firmware can
    /// read or program a coherent calendar image. `emulated_us` keeps running —
    /// SET halts the *calendar*, not the oscillator, and the periodic divider is
    /// explicitly independent of it — so the held value is what every calendar
    /// read returns while the bit is set. Calendar locations use the independent
    /// fields when present, and this projection remains checkpoint-compatible.
    ///
    /// On SET clear, `time_offset_secs` is re-anchored so the calendar resumes
    /// from this held (or newly programmed) value rather than jumping forward by
    /// however long SET was asserted. That is the whole point of the bit: a BIOS
    /// writes SET, programs the fields, clears SET, and time continues from what
    /// it wrote.
    set_frozen_secs: Option<u64>,
    /// Authoritative calendar locations under SET, including partial date writes.
    set_calendar_fields: Option<RtcCalendarFields>,

    /// Caller-supplied boot epoch (seconds since Unix epoch).
    startup_epoch_secs: u64,

    /// Independent calendar location, seeded once and writable by the guest.
    day_of_week: u8,

    // --- Event-driven scheduling ---
    /// Set when Status Register A (rate select) or Status Register B (PIE)
    /// are written, signalling that the RTC periodic event period may have changed.
    /// The emulator loop checks this and reschedules the RtcPeriodic event.
    pub event_reschedule_pending: bool,
}

/// Side-effect-free CMOS/RTC diagnostic snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtcDiagnosticSnapshot {
    pub selected_index: u8,
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

impl Mc146818 {
    /// Construct from the caller's Unix epoch in seconds. Time advances only through ticks.
    pub fn new(startup_epoch_secs: u64) -> Self {
        Self::new_with_century_registers(startup_epoch_secs, [None, None])
    }

    /// Construct with caller-selected century register extensions.
    /// Addresses beyond the RTC register bank are otherwise raw NVRAM.
    ///
    /// # Panics
    /// Panics for repeated addresses or addresses outside 0x0e..0x80.
    pub fn new_with_century_registers(
        startup_epoch_secs: u64,
        century_registers: [Option<u8>; 2],
    ) -> Self {
        assert!(
            century_registers
                .iter()
                .flatten()
                .all(|&address| (0x0e..0x80).contains(&address)),
            "century registers must be in raw NVRAM"
        );
        assert!(
            century_registers[0].is_none() || century_registers[0] != century_registers[1],
            "century register addresses must be distinct"
        );
        let mut chip = Self {
            index: 0,
            century_registers,
            ram: [0; 128],
            periodic_accum_us: 0,
            last_second: NO_PREVIOUS_SECOND, // triggers the first sync, not a UF
            alarm_fired_this_second: false,
            pending_irq_flags: Cell::new(0),
            uip_accum_us: 0,
            time_offset_secs: 0,
            emulated_us: 0,
            calendar_clock_us: 0,
            set_frozen_secs: None,
            set_calendar_fields: None,
            startup_epoch_secs,
            day_of_week: Self::compute_time_components(startup_epoch_secs).3,
            event_reschedule_pending: false,
        };
        chip.ram[regs::STATUS_A as usize] = 0x26;
        chip.ram[regs::STATUS_B as usize] = 0x02;
        chip.ram[regs::STATUS_D as usize] = 0x80;
        chip
    }

    /// Raw battery-backed register storage; the caller owns its NVRAM layout.
    #[inline]
    pub fn nvram(&self) -> &[u8; 128] {
        &self.ram
    }

    /// Raw storage for machine defaults and persisted NVRAM overlays.
    #[inline]
    pub fn nvram_mut(&mut self) -> &mut [u8; 128] {
        &mut self.ram
    }

    /// Select a register address. The caller owns bus index masking.
    #[inline]
    pub fn write_index(&mut self, value: u8) {
        self.index = value;
    }

    /// Read the selected register address.
    #[inline]
    pub fn read_index(&self) -> u8 {
        self.index
    }

    /// Write the selected data register.
    ///
    /// Returns the RTC interrupt output's transition, if the write moved it:
    /// `Some(true)` when IRQF rose, `Some(false)` when it fell, `None` when it
    /// did not change. Only a Status B write can move it — enabling a source
    /// whose flag is already latched asserts IRQF, and disabling the last
    /// enabled one deasserts it — and the MC146818's IRQ pin follows IRQF at
    /// once, so the caller drives IRQ8 from this (MDD-BUG-KIL-01040).
    pub fn write_data(&mut self, value: u8) -> Option<bool> {
        let idx = self.index as usize;
        let pending_before = self.pending_irq_flags.get();
        let divider_was_reset = self.is_divider_reset();
        if idx < self.ram.len() {
            // Don't allow writing to read-only RTC time registers in normal operation
            // (they're maintained by the RTC itself)
            match self.index {
                regs::SECONDS
                | regs::MINUTES
                | regs::HOURS
                | regs::DAY_OF_WEEK
                | regs::DAY_OF_MONTH
                | regs::MONTH
                | regs::YEAR => {
                    // Allow writing to time registers by adjusting time_offset_secs.
                    self.apply_time_register_write(self.index, value);
                }
                reg if self.century_registers.contains(&Some(reg)) => {
                    self.apply_time_register_write(reg, value);
                }
                regs::STATUS_C | regs::STATUS_D => {
                    // Status C and Status D are read-only.
                }
                _ => {
                    self.ram[idx] = value;
                }
            }

            // Signal RTC event reschedule when rate select or PIE bit may have changed
            if idx == regs::STATUS_A as usize || idx == regs::STATUS_B as usize {
                self.event_reschedule_pending = true;
            }
            if idx == regs::STATUS_A as usize {
                if self.is_divider_reset() {
                    self.periodic_accum_us = 0;
                    self.uip_accum_us = 0;
                } else if divider_was_reset {
                    // MC146818A: the first update begins half a second after
                    // divider reset is released, regardless of the former phase.
                    self.calendar_clock_us =
                        self.calendar_clock_us / 1_000_000 * 1_000_000 + 500_000;
                    self.uip_accum_us = 500_000;
                    self.last_second = self.get_adjusted_time().0;
                }
            }
            if idx == regs::STATUS_B as usize {
                // MDD-BUG-CRUCIBLE-00552: act on the SET (bit 7) edge.
                //
                // Asserting it holds the calendar at the instant of the write, so
                // firmware can read a coherent image or program the fields without
                // a carry landing between two register writes.
                //
                // Clearing it re-anchors `time_offset_secs` on the held (or newly
                // programmed) value, so the calendar resumes from what the guest
                // wrote instead of jumping forward by the duration SET was held.
                // Without this the bit would be a no-op with extra steps.
                self.apply_set_bit_edge();
                // An enable bit may have moved under a pending flag, so IRQF has
                // to be re-derived (MDD-BUG-CRUCIBLE-00621). This layer has no
                // PIC handle, so the transition is returned for the bus to
                // drive IRQ8 with (MDD-BUG-KIL-01040).
                self.refresh_irqf();
            }
        }
        let irqf_before = pending_before & 0x80 != 0;
        let irqf_after = self.pending_irq_flags.get() & 0x80 != 0;
        (irqf_before != irqf_after).then_some(irqf_after)
    }

    /// Read the selected data register.
    pub fn read_data(&self) -> u8 {
        let idx = self.index as usize;

        // RTC time registers are derived from system time
        match self.index {
            regs::SECONDS => self.get_rtc_seconds(),
            regs::MINUTES => self.get_rtc_minutes(),
            regs::HOURS => self.get_rtc_hours(),
            regs::DAY_OF_WEEK => self.get_rtc_day_of_week(),
            regs::DAY_OF_MONTH => self.get_rtc_day(),
            regs::MONTH => self.get_rtc_month(),
            regs::YEAR => self.get_rtc_year(),
            reg if self.century_registers.contains(&Some(reg)) => self.get_rtc_century(),
            regs::STATUS_A => self.effective_status_a(),
            regs::STATUS_C => {
                // Reading Status C clears interrupt flags (clear-on-read)
                let flags = self.pending_irq_flags.get();
                self.pending_irq_flags.set(0);
                flags
            }
            regs::STATUS_D => self.effective_status_d(),
            _ => {
                if idx < self.ram.len() {
                    self.ram[idx]
                } else {
                    0xFF
                }
            }
        }
    }

    fn effective_status_a(&self) -> u8 {
        let base = self.ram[regs::STATUS_A as usize] & 0x7F;
        // MDD-BUG-CRUCIBLE-00552: SET aborts the update cycle, so UIP reads
        // inactive for as long as the bit is asserted. `uip_accum_us` keeps
        // accumulating (the oscillator does not stop) — only the reported bit is
        // suppressed, so clearing SET resumes mid-phase rather than from zero.
        if self.set_asserted() || self.is_divider_reset() {
            return base;
        }
        if self.uip_accum_us >= 999_756 {
            base | 0x80
        } else {
            base
        }
    }

    fn effective_status_d(&self) -> u8 {
        self.ram[regs::STATUS_D as usize] & 0x80
    }

    fn peek_effective_register(&self, reg: u8) -> u8 {
        let idx = reg as usize;
        match reg {
            regs::SECONDS => self.get_rtc_seconds(),
            regs::MINUTES => self.get_rtc_minutes(),
            regs::HOURS => self.get_rtc_hours(),
            regs::DAY_OF_WEEK => self.get_rtc_day_of_week(),
            regs::DAY_OF_MONTH => self.get_rtc_day(),
            regs::MONTH => self.get_rtc_month(),
            regs::YEAR => self.get_rtc_year(),
            reg if self.century_registers.contains(&Some(reg)) => self.get_rtc_century(),
            regs::STATUS_A => self.effective_status_a(),
            regs::STATUS_C => self.pending_irq_flags.get(),
            regs::STATUS_D => self.effective_status_d(),
            _ => self.ram.get(idx).copied().unwrap_or(0xFF),
        }
    }

    /// Convert decimal to BCD
    fn to_bcd(value: u8) -> u8 {
        ((value / 10) << 4) | (value % 10)
    }

    /// Convert BCD to decimal
    fn from_bcd(bcd: u8) -> u8 {
        (bcd >> 4) * 10 + (bcd & 0x0F)
    }

    /// Get emulated RTC time adjusted by `time_offset_secs`.
    ///
    /// Derives from `startup_epoch_secs + calendar_clock_us / 1_000_000 + time_offset_secs`.
    /// The host clock is NOT consulted — time advances purely in emulated time,
    /// driven by `tick()`. CMOS has an internal
    /// counter advanced by a per-second callback.
    fn get_adjusted_time(&self) -> (u8, u8, u8, u8, u8, u8, u16) {
        if let Some(fields) = self.set_calendar_fields {
            return (
                fields.seconds,
                fields.minutes,
                fields.hours,
                self.day_of_week,
                fields.day,
                fields.month,
                fields.year,
            );
        }
        let (seconds, minutes, hours, _, day, month, year) =
            Self::compute_time_components(self.effective_epoch_secs());
        (seconds, minutes, hours, self.day_of_week, day, month, year)
    }

    /// The calendar the guest actually sees: the held value while Status-B SET
    /// is asserted, otherwise the freely-running clock
    /// (MDD-BUG-CRUCIBLE-00552).
    fn effective_epoch_secs(&self) -> u64 {
        self.set_frozen_secs
            .unwrap_or_else(|| self.raw_adjusted_epoch_secs())
    }

    /// Whether Status-B bit 7 (SET) is currently asserted.
    fn set_asserted(&self) -> bool {
        (self.ram[regs::STATUS_B as usize] & 0x80) != 0
    }

    /// Reconcile the held calendar with the current Status-B SET bit.
    ///
    /// Idempotent: it compares the bit against whether a hold already exists, so
    /// a write that leaves SET unchanged does nothing and cannot re-freeze at a
    /// later instant.
    fn apply_set_bit_edge(&mut self) {
        match (self.set_asserted(), self.set_frozen_secs) {
            // Rising edge — abort the update cycle and hold the calendar here.
            (true, None) => {
                // The MC146818 clears UIE when SET rises. Preserve the other
                // Status-B controls so IRQF can be requalified below.
                self.ram[regs::STATUS_B as usize] &= !0x10;
                let frozen = self.raw_adjusted_epoch_secs();
                self.set_frozen_secs = Some(frozen);
                self.set_calendar_fields = Some(RtcCalendarFields::from_epoch(frozen));
            }
            // Falling edge — resume from the held/programmed value.
            (false, Some(frozen)) => {
                let frozen = self
                    .set_calendar_fields
                    .take()
                    .map_or(frozen, RtcCalendarFields::normalized_epoch);
                self.set_frozen_secs = None;
                let raw = self.raw_adjusted_epoch_secs();
                self.time_offset_secs += frozen as i64 - raw as i64;
                // The visible calendar is continuous across this edge (that is
                // what the re-anchor buys), so `last_second` still matches and no
                // spurious update-ended edge is manufactured. Re-sync anyway so
                // the invariant does not depend on the arithmetic above.
                let (seconds, _, _, _, _, _, _) = self.get_adjusted_time();
                self.last_second = seconds;
            }
            _ => {}
        }
    }

    /// The freely-running clock, ignoring any SET hold.
    fn raw_adjusted_epoch_secs(&self) -> u64 {
        let emulated_secs = self.startup_epoch_secs + self.calendar_clock_us / 1_000_000;
        (emulated_secs as i64 + self.time_offset_secs).max(0) as u64
    }

    /// Move the RTC by `delta` seconds.
    ///
    /// While SET is asserted the held calendar moves too, so a guest that
    /// programs the time registers under SET reads back what it just wrote and
    /// resumes from there when it clears the bit. Re-anchoring on SET clear uses
    /// the held value, so shifting `time_offset_secs` as well is harmless and
    /// keeps the two representations in step.
    fn shift_rtc_epoch(&mut self, delta: i64) {
        self.time_offset_secs += delta;
        if let Some(frozen) = self.set_frozen_secs {
            let shifted = (frozen as i64 + delta).max(0) as u64;
            self.set_frozen_secs = Some(shifted);
            self.set_calendar_fields = Some(RtcCalendarFields::from_epoch(shifted));
        }
    }

    /// Apply a write to a time register by adjusting the time offset.
    fn apply_time_register_write(&mut self, reg: u8, value: u8) {
        let is_binary = (self.ram[regs::STATUS_B as usize] & 0x04) != 0;

        // Decode the written value to binary
        let decoded = if is_binary {
            value
        } else {
            Self::from_bcd(value & 0x7F) | (value & 0x80)
        };

        let (cur_sec, cur_min, cur_hr, _dow, cur_day, cur_mon, cur_year) = self.get_adjusted_time();

        let (new_sec, new_min, new_hr, new_day, new_mon, new_year) = match reg {
            regs::SECONDS => (decoded, cur_min, cur_hr, cur_day, cur_mon, cur_year),
            regs::MINUTES => (cur_sec, decoded, cur_hr, cur_day, cur_mon, cur_year),
            regs::HOURS => {
                let is_24h = (self.ram[regs::STATUS_B as usize] & 0x02) != 0;
                let h = if is_24h {
                    decoded
                } else {
                    let pm = (decoded & 0x80) != 0;
                    let h12 = decoded & 0x7F;
                    if h12 == 12 {
                        if pm {
                            12
                        } else {
                            0
                        }
                    } else if pm {
                        h12 + 12
                    } else {
                        h12
                    }
                };
                (cur_sec, cur_min, h, cur_day, cur_mon, cur_year)
            }
            regs::DAY_OF_MONTH => (cur_sec, cur_min, cur_hr, decoded, cur_mon, cur_year),
            regs::MONTH => (cur_sec, cur_min, cur_hr, cur_day, decoded, cur_year),
            regs::YEAR => {
                let century = cur_year / 100;
                let new_year = century * 100 + decoded as u16;
                (cur_sec, cur_min, cur_hr, cur_day, cur_mon, new_year)
            }
            reg if self.century_registers.contains(&Some(reg)) => {
                let year_2digit = cur_year % 100;
                let new_year = (decoded as u16) * 100 + year_2digit;
                (cur_sec, cur_min, cur_hr, cur_day, cur_mon, new_year)
            }
            regs::DAY_OF_WEEK => {
                // Only the documented 1..=7 range defines a calendar value.
                if (1..=7).contains(&decoded) {
                    self.day_of_week = decoded;
                }
                return;
            }
            _ => return,
        };

        if self.set_calendar_fields.is_some() {
            self.hold_calendar_fields(RtcCalendarFields {
                seconds: new_sec,
                minutes: new_min,
                hours: new_hr,
                day: new_day,
                month: new_mon,
                year: new_year,
            });
            return;
        }

        // Clamp the day into the target month before converting to an epoch.
        //
        // A real MC146818 is a register file: each date field stands alone, so a
        // BIOS may write month before day and leave a transiently impossible
        // date (31 June) that the following day write repairs. We model the RTC
        // as an offset from a monotonic epoch instead, and `time_to_epoch_secs`
        // turns an over-long day into a carry — 31 June silently becomes 1 July,
        // so the month the guest just wrote is lost. The BIOS date-setting handler
        // writes century, year, month, then day, so on the 31st of a month a
        // set-date reads back the OLD month (MDD-BUG-FLUXHOMEARPA-00032).
        //
        // Clamping keeps the invariant that matters — writing one field never
        // silently moves another — and the day write that follows restores the
        // intended day. It also covers 29 February when the year write moves to
        // a non-leap year.
        let new_day = new_day.min(days_in_month(new_mon, new_year));

        let cur_epoch =
            Self::time_to_epoch_secs(cur_sec, cur_min, cur_hr, cur_day, cur_mon, cur_year);
        let new_epoch =
            Self::time_to_epoch_secs(new_sec, new_min, new_hr, new_day, new_mon, new_year);
        self.shift_rtc_epoch(new_epoch - cur_epoch);
    }

    fn hold_calendar_fields(&mut self, fields: RtcCalendarFields) {
        self.set_frozen_secs = Some(fields.normalized_epoch());
        self.set_calendar_fields = Some(fields);
    }

    /// Convert time components to approximate seconds since epoch (signed, handles pre-1970).
    fn time_to_epoch_secs(sec: u8, min: u8, hr: u8, day: u8, mon: u8, year: u16) -> i64 {
        let mut days: i64 = 0;
        if year >= 1970 {
            for y in 1970..year {
                days += if is_leap_year(y) { 366 } else { 365 };
            }
        } else {
            for y in year..1970 {
                days -= if is_leap_year(y) { 366 } else { 365 };
            }
        }
        let mon = mon.clamp(1, 12);
        let days_in_month = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for m in 1..mon {
            let dim = if m == 2 && is_leap_year(year) {
                29
            } else {
                days_in_month[(m - 1) as usize] as i64
            };
            days += dim;
        }
        days += (day.max(1) - 1) as i64;
        days * 86400 + hr as i64 * 3600 + min as i64 * 60 + sec as i64
    }

    fn compute_time_components(secs: u64) -> (u8, u8, u8, u8, u8, u8, u16) {
        let time_of_day = secs % 86400;

        // Calculate time components
        // Note: This is a simplified calculation that doesn't handle all edge cases
        let hours = (time_of_day / 3600) as u8;
        let minutes = ((time_of_day % 3600) / 60) as u8;
        let seconds = (time_of_day % 60) as u8;

        // Calculate date (simplified - doesn't account for leap seconds)
        // Days since 1970-01-01
        let days_since_epoch = secs / 86400;
        let mut days = days_since_epoch as i64;
        let mut year = 1970u16;

        loop {
            let days_in_year = if is_leap_year(year) { 366 } else { 365 };
            if days < days_in_year {
                break;
            }
            days -= days_in_year;
            year += 1;
        }

        let mut month = 1u8;
        let days_in_month = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for (m, &dim) in days_in_month.iter().enumerate() {
            let dim = if m == 1 && is_leap_year(year) {
                29
            } else {
                dim
            };
            if days < dim as i64 {
                break;
            }
            days -= dim as i64;
            month += 1;
        }

        let day = (days + 1) as u8;

        // Day of week (0 = Sunday, 1 = Monday, etc.)
        // Jan 1, 1970 was a Thursday (4)
        let dow = ((days_since_epoch + 4) % 7) as u8 + 1; // CMOS uses 1-7

        (seconds, minutes, hours, dow, day, month, year)
    }

    /// Read RTC time as BCD values, suitable for INT 1Ah AH=02h.
    ///
    /// Returns `(hours_bcd, minutes_bcd, seconds_bcd)` using the adjusted
    /// time (respecting any offset applied via register writes).
    /// Values are always returned in BCD format regardless of Status B setting.
    pub fn get_rtc_time_bcd(&self) -> (u8, u8, u8) {
        let (seconds, minutes, hours, _, _, _, _) = self.get_adjusted_time();
        (
            Self::to_bcd(hours),
            Self::to_bcd(minutes),
            Self::to_bcd(seconds),
        )
    }

    /// Read RTC date as BCD values, suitable for INT 1Ah AH=04h.
    ///
    /// Returns `(century_bcd, year_bcd, month_bcd, day_bcd)` using the
    /// adjusted time (respecting any offset applied via register writes).
    /// Values are always returned in BCD format regardless of Status B setting.
    pub fn get_rtc_date_bcd(&self) -> (u8, u8, u8, u8) {
        let (_, _, _, _, day, month, year) = self.get_adjusted_time();
        let century = (year / 100) as u8;
        let year_2digit = (year % 100) as u8;
        (
            Self::to_bcd(century),
            Self::to_bcd(year_2digit),
            Self::to_bcd(month),
            Self::to_bcd(day),
        )
    }

    /// Set RTC time from BCD values, suitable for INT 1Ah AH=03h.
    ///
    /// Adjusts the internal time offset so subsequent reads return the
    /// specified time. This routes through the same offset mechanism as
    /// direct CMOS register writes.
    pub fn set_rtc_time_bcd(&mut self, hours_bcd: u8, minutes_bcd: u8, seconds_bcd: u8) {
        let hours = Self::from_bcd(hours_bcd);
        let minutes = Self::from_bcd(minutes_bcd);
        let seconds = Self::from_bcd(seconds_bcd);

        if let Some(mut fields) = self.set_calendar_fields {
            fields.hours = hours;
            fields.minutes = minutes;
            fields.seconds = seconds;
            self.hold_calendar_fields(fields);
            return;
        }
        let (_s, _m, _h, _dow, day, mon, year) = self.get_adjusted_time();
        let cur_epoch = Self::time_to_epoch_secs(_s, _m, _h, day, mon, year);
        let new_epoch = Self::time_to_epoch_secs(seconds, minutes, hours, day, mon, year);
        self.shift_rtc_epoch(new_epoch - cur_epoch);
    }

    /// Set RTC date from BCD values, suitable for INT 1Ah AH=05h.
    ///
    /// Computes the full target date at once to avoid intermediate states
    /// (e.g., century=19 with year=26 → 1926) that can't be represented
    /// as Unix epoch values.
    pub fn set_rtc_date_bcd(&mut self, century_bcd: u8, year_bcd: u8, month_bcd: u8, day_bcd: u8) {
        let century = Self::from_bcd(century_bcd);
        let year_2digit = Self::from_bcd(year_bcd);
        let month = Self::from_bcd(month_bcd);
        let day = Self::from_bcd(day_bcd);
        let full_year = century as u16 * 100 + year_2digit as u16;

        if let Some(mut fields) = self.set_calendar_fields {
            // The full-date setter keeps its existing atomic normalization contract.
            let date = RtcCalendarFields::from_epoch(
                Self::time_to_epoch_secs(0, 0, 0, day, month, full_year).max(0) as u64,
            );
            fields.day = date.day;
            fields.month = date.month;
            fields.year = date.year;
            self.hold_calendar_fields(fields);
            return;
        }
        let (sec, min, hr, _dow, _d, _m, cur_year) = self.get_adjusted_time();
        let cur_epoch = Self::time_to_epoch_secs(sec, min, hr, _d, _m, cur_year);
        let new_epoch = Self::time_to_epoch_secs(sec, min, hr, day, month, full_year);
        self.shift_rtc_epoch(new_epoch - cur_epoch);
    }

    fn get_rtc_seconds(&self) -> u8 {
        let (seconds, _, _, _, _, _, _) = self.get_adjusted_time();
        if (self.ram[regs::STATUS_B as usize] & 0x04) != 0 {
            seconds // Binary mode
        } else {
            Self::to_bcd(seconds) // BCD mode
        }
    }

    fn get_rtc_minutes(&self) -> u8 {
        let (_, minutes, _, _, _, _, _) = self.get_adjusted_time();
        if (self.ram[regs::STATUS_B as usize] & 0x04) != 0 {
            minutes
        } else {
            Self::to_bcd(minutes)
        }
    }

    /// Encode a 0-23 hour the way the guest has programmed the RTC to report
    /// it: Status-B bit 1 selects 24-hour over 12-hour-with-bit-7-as-PM, and
    /// bit 2 selects binary over BCD.
    ///
    /// This is the single encoder for both ordinary hour reads and alarm
    /// comparison. The alarm register holds whatever the guest wrote, so it is
    /// only comparable against a current hour in the very same encoding.
    fn encode_rtc_hour(&self, hours24: u8) -> u8 {
        let is_24hour = (self.ram[regs::STATUS_B as usize] & 0x02) != 0;
        let is_binary = (self.ram[regs::STATUS_B as usize] & 0x04) != 0;

        let hours = if is_24hour {
            hours24
        } else {
            // 12-hour mode: 1-12 with bit 7 set for PM, so midnight is 12 AM
            // and noon is 12 PM.
            let h12 = if hours24 == 0 {
                12
            } else if hours24 > 12 {
                hours24 - 12
            } else {
                hours24
            };
            if hours24 >= 12 {
                h12 | 0x80 // Set PM bit
            } else {
                h12
            }
        };

        if is_binary {
            hours
        } else {
            Self::to_bcd(hours & 0x7F) | (hours & 0x80)
        }
    }

    fn get_rtc_hours(&self) -> u8 {
        let (_, _, hours, _, _, _, _) = self.get_adjusted_time();
        self.encode_rtc_hour(hours)
    }

    fn get_rtc_day_of_week(&self) -> u8 {
        let (_, _, _, dow, _, _, _) = self.get_adjusted_time();
        dow // 1-7, Sunday = 1
    }

    fn get_rtc_day(&self) -> u8 {
        let (_, _, _, _, day, _, _) = self.get_adjusted_time();
        if (self.ram[regs::STATUS_B as usize] & 0x04) != 0 {
            day
        } else {
            Self::to_bcd(day)
        }
    }

    fn get_rtc_month(&self) -> u8 {
        let (_, _, _, _, _, month, _) = self.get_adjusted_time();
        if (self.ram[regs::STATUS_B as usize] & 0x04) != 0 {
            month
        } else {
            Self::to_bcd(month)
        }
    }

    fn get_rtc_year(&self) -> u8 {
        let (_, _, _, _, _, _, year) = self.get_adjusted_time();
        let year_2digit = (year % 100) as u8;
        if (self.ram[regs::STATUS_B as usize] & 0x04) != 0 {
            year_2digit
        } else {
            Self::to_bcd(year_2digit)
        }
    }

    fn get_rtc_century(&self) -> u8 {
        let (_, _, _, _, _, _, year) = self.get_adjusted_time();
        let century = (year / 100) as u8;
        if (self.ram[regs::STATUS_B as usize] & 0x04) != 0 {
            century
        } else {
            Self::to_bcd(century)
        }
    }

    // =========================================================================
    // RTC Interrupt Support
    // =========================================================================

    /// Tick the RTC. Call from emulator loop.
    ///
    /// # Arguments
    /// * `elapsed_us` - Microseconds since last tick
    ///
    /// # Returns
    /// `true` if IRQ8 should be raised
    ///
    /// # Notes
    /// For 8192 Hz periodic interrupts, this should be called at least every
    /// 122µs. In practice, calling once per frame (~16ms) is sufficient for
    /// most applications using lower rates (1024 Hz or less).
    #[inline]
    pub fn tick(&mut self, elapsed_us: u64) -> bool {
        self.tick_internal(elapsed_us, PeriodicTick::Elapsed)
    }

    /// Advance calendar time and UIP independently of the periodic divider.
    ///
    /// Scheduler callers deliver PF separately with [`Self::tick_periodic`].
    /// This path retains update/alarm flag handling and returns an IRQ8 edge,
    /// but neither advances the periodic accumulator nor generates PF.
    #[inline]
    pub fn advance_clock(&mut self, elapsed_us: u64) -> bool {
        self.tick_internal(elapsed_us, PeriodicTick::None)
    }

    /// Advance the RTC by one scheduler-generated periodic interval.
    ///
    /// The event scheduler already carries the fractional interval remainder,
    /// so this path must raise PF for each enabled periodic event instead of
    /// comparing a rounded microsecond value against the period table. The
    /// ordinary [`Self::tick`] path remains available for callers that advance
    /// the RTC by an arbitrary elapsed duration.
    #[inline]
    pub fn tick_periodic(&mut self, elapsed_us: u64) -> bool {
        self.tick_internal(elapsed_us, PeriodicTick::Event)
    }

    fn tick_internal(&mut self, elapsed_us: u64, periodic_tick: PeriodicTick) -> bool {
        let status_b = self.ram[regs::STATUS_B as usize];
        let mut new_flags = self.pending_irq_flags.get();
        // The interrupt EDGE is IRQF's, not "any flag's" (MDD-BUG-CRUCIBLE-00621).
        // PF/UF/AF now latch whether or not their enable is set, so a disabled
        // source leaves flags non-zero and would otherwise suppress the next
        // genuine edge forever — or, read the other way, assert IRQ8 for an event
        // the guest has explicitly disabled.
        let had_irqf = (new_flags & 0x80) != 0;

        // Guest date/time programming does not change the independent weekday.
        // Only elapsed calendar midnights advance it; SET inhibits that calendar.
        let previous_day = self.effective_epoch_secs() / 86_400;
        self.emulated_us += elapsed_us;
        if self.is_divider_reset() {
            return false;
        }
        self.calendar_clock_us += elapsed_us;
        if !self.set_asserted() {
            let elapsed_days = self.effective_epoch_secs() / 86_400 - previous_day;
            self.day_of_week = ((u64::from(self.day_of_week - 1) + elapsed_days % 7) % 7 + 1) as u8;
        }

        // --- UIP timing ---
        // Track microseconds within the current second for UIP simulation.
        self.uip_accum_us += elapsed_us;
        if self.uip_accum_us >= 1_000_000 {
            self.uip_accum_us %= 1_000_000;
        }

        // --- Periodic flag (PF) ---
        // Latched whenever the rate generator ticks, INDEPENDENT of PIE. PIE
        // qualifies IRQF/IRQ8 only, below. Rate selection still gates it: with
        // no rate selected the divider produces no periodic event at all, which
        // is a different thing from the interrupt being disabled.
        match periodic_tick {
            PeriodicTick::Event if self.get_periodic_period_ratio_us().is_some() => {
                // The scheduler delivered exactly one period. Its fixed-point
                // cycle/us conversion already accounts for 976.5625us at
                // 1024Hz, so do not compare a rounded integer duration here.
                self.periodic_accum_us = 0;
                new_flags |= 0x40; // PF (periodic flag)
            }
            PeriodicTick::Elapsed => {
                if let Some(period_us) = self.get_periodic_period_us() {
                    self.periodic_accum_us += elapsed_us;
                    if self.periodic_accum_us >= period_us {
                        // For high frequencies, we might have accumulated multiple periods
                        // Just fire once and reset (don't try to catch up)
                        self.periodic_accum_us = 0;
                        new_flags |= 0x40; // PF (periodic flag)
                    }
                }
            }
            PeriodicTick::None | PeriodicTick::Event => {}
        }

        // Get current emulated time (in binary, regardless of BCD mode)
        let (seconds, minutes, hours, _, _, _, _) = self.get_adjusted_time();

        // MDD-BUG-CRUCIBLE-00552: SET inhibits the update cycle, and UF/AF are
        // products of that cycle — so neither latches while the bit is asserted.
        // PF is deliberately NOT gated: the periodic divider is independent of
        // SET on real silicon, and a guest that holds SET while relying on the
        // periodic interrupt must keep receiving it.
        //
        // The freeze alone would mostly achieve this (a held calendar never
        // changes second), but relying on that would make correct behaviour an
        // accident of the hold rather than a stated rule.
        let update_cycle_inhibited = self.set_asserted();

        // --- Update-ended flag (UF) ---
        // Latched once per second when the second changes, INDEPENDENT of UIE.
        //
        // `last_second` starts at the 0xFF sentinel meaning "no previous second
        // observed", so the first tick only SYNCS it. Latching UF there would
        // invent an update cycle that never ran — invisible while UF was gated on
        // UIE, and guest-visible the moment it is not.
        let mut update_cycle_completed = false;
        if !update_cycle_inhibited && seconds != self.last_second {
            let had_previous_second = self.last_second != NO_PREVIOUS_SECOND;
            // Reset alarm-fired flag when second changes
            self.alarm_fired_this_second = false;
            self.last_second = seconds;
            if had_previous_second {
                new_flags |= 0x10; // UF (update-ended flag)
                update_cycle_completed = true;
            }
        }

        // --- Alarm flag (AF) ---
        // The calendar update compares the new time against the alarm registers.
        // A first-tick sync or a same-second tick is not an update cycle.
        // AF latches independently of AIE.
        if update_cycle_completed
            && !self.alarm_fired_this_second
            && self.check_alarm_match(seconds, minutes, hours)
        {
            self.alarm_fired_this_second = true;
            new_flags |= 0x20; // AF (alarm flag)
        }

        new_flags = Self::qualify_irqf(new_flags, status_b);
        self.pending_irq_flags.set(new_flags);

        // Signal a new IRQ only on IRQF's rising edge. The RTC output is
        // level-held until Status C is read, so it must not re-assert while
        // IRQF is still set.
        !had_irqf && (new_flags & 0x80) != 0
    }

    /// Derive IRQF (bit 7) from the event flags and Status B's enable bits.
    ///
    /// The MC146818 latches PF, UF and AF when their events occur regardless of
    /// PIE, UIE and AIE; the enables qualify IRQF — and therefore IRQ8 — only
    /// (MDD-BUG-CRUCIBLE-00621). Polling Status C with interrupts disabled is a
    /// real and common idiom, and it used to lose every event here.
    ///
    /// The mask works because the three enables sit at the SAME bit positions in
    /// Status B as their flags do in Status C: PIE/PF = 0x40, AIE/AF = 0x20,
    /// UIE/UF = 0x10. That alignment is the part's, not a coincidence of ours.
    fn qualify_irqf(flags: u8, status_b: u8) -> u8 {
        if (flags & status_b & 0x70) != 0 {
            flags | 0x80
        } else {
            flags & !0x80
        }
    }

    /// Recompute IRQF against the current Status B, preserving the event flags.
    ///
    /// Needed wherever an enable bit can change while a flag is already pending:
    /// enabling a source with its flag set asserts IRQF, disabling the last
    /// enabled source clears it, and neither may disturb the latched flags.
    fn refresh_irqf(&self) {
        let status_b = self.ram[regs::STATUS_B as usize];
        let flags = self.pending_irq_flags.get();
        self.pending_irq_flags
            .set(Self::qualify_irqf(flags, status_b));
    }

    /// Check if the periodic interrupt enable (PIE) bit is set in Status Register B.
    /// PIE is bit 6 of register B.
    #[inline]
    pub fn is_periodic_interrupt_enabled(&self) -> bool {
        (self.ram[regs::STATUS_B as usize] & 0x40) != 0
    }

    /// Whether either hardware divider-reset pattern is selected.
    pub fn is_divider_reset(&self) -> bool {
        self.ram[regs::STATUS_A as usize] & 0x60 == 0x60
    }

    /// Get the RTC periodic frequency in hertz; divider reset returns `None`.
    #[inline]
    pub fn get_periodic_frequency_hz(&self) -> Option<u64> {
        if self.is_divider_reset() {
            return None;
        }
        let rate_sel = self.ram[regs::STATUS_A as usize] & 0x0F;
        match rate_sel {
            0 => None,
            1 => Some(256),
            2 => Some(128),
            3 => Some(8192),
            4 => Some(4096),
            5 => Some(2048),
            6 => Some(1024),
            7 => Some(512),
            8 => Some(256),
            9 => Some(128),
            10 => Some(64),
            11 => Some(32),
            12 => Some(16),
            13 => Some(8),
            14 => Some(4),
            15 => Some(2),
            _ => None,
        }
    }

    /// Get the exact periodic interval as `(microseconds numerator, frequency)`.
    ///
    /// For example, rate 6 is `1_000_000 / 1024` microseconds, or
    /// 976.5625us. Callers that schedule repeated events must carry the
    /// division remainder instead of using the rounded compatibility value
    /// returned by [`Self::get_periodic_period_us`].
    #[inline]
    pub fn get_periodic_period_ratio_us(&self) -> Option<(u64, u64)> {
        self.get_periodic_frequency_hz()
            .map(|frequency_hz| (1_000_000, frequency_hz))
    }

    /// Get the integer periodic interval; divider reset returns `None`.
    ///
    /// Rate selection is in Status A bits 3-0. This compatibility view keeps
    /// the legacy rounded values; event-driven timing uses
    /// [`Self::get_periodic_period_ratio_us`] for exact fractional periods.
    #[inline]
    pub fn get_periodic_period_us(&self) -> Option<u64> {
        if self.is_divider_reset() {
            return None;
        }
        let rate_sel = self.ram[regs::STATUS_A as usize] & 0x0F;
        match rate_sel {
            0 => None,          // Disabled
            1 => Some(3906),    // 256 Hz (3.90625 ms)
            2 => Some(7812),    // 128 Hz (7.8125 ms)
            3 => Some(122),     // 8192 Hz (122.07 µs) - minimum period
            4 => Some(244),     // 4096 Hz
            5 => Some(488),     // 2048 Hz
            6 => Some(977),     // 1024 Hz (default)
            7 => Some(1953),    // 512 Hz
            8 => Some(3906),    // 256 Hz
            9 => Some(7812),    // 128 Hz
            10 => Some(15625),  // 64 Hz
            11 => Some(31250),  // 32 Hz
            12 => Some(62500),  // 16 Hz
            13 => Some(125000), // 8 Hz
            14 => Some(250000), // 4 Hz
            15 => Some(500000), // 2 Hz
            _ => None,
        }
    }

    /// Check if current time matches alarm registers.
    ///
    /// Alarm registers use the MC146818 two-MSB "don't care" convention
    /// (0xC0-0xFF) for that field, allowing periodic alarms.
    fn check_alarm_match(&self, seconds: u8, minutes: u8, hours: u8) -> bool {
        let is_binary = (self.ram[regs::STATUS_B as usize] & 0x04) != 0;

        let alarm_sec = self.ram[regs::SECONDS_ALARM as usize];
        let alarm_min = self.ram[regs::MINUTES_ALARM as usize];
        let alarm_hr = self.ram[regs::HOURS_ALARM as usize];

        // Convert current time to match alarm format (BCD or binary). The hour
        // additionally honours Status-B bit 1: in 12-hour mode the guest wrote
        // a 1-12 value with bit 7 as PM, so a 24-hour comparison never matches.
        let (sec, min) = if is_binary {
            (seconds, minutes)
        } else {
            (Self::to_bcd(seconds), Self::to_bcd(minutes))
        };
        let hr = self.encode_rtc_hour(hours);

        // Two MSBs set = "don't care" for that field. Values 0x80-0xBF
        // compare as literal alarm values and do not match normal RTC fields.
        let sec_match = (alarm_sec & 0xC0) == 0xC0 || alarm_sec == sec;
        let min_match = (alarm_min & 0xC0) == 0xC0 || alarm_min == min;
        let hr_match = (alarm_hr & 0xC0) == 0xC0 || alarm_hr == hr;

        sec_match && min_match && hr_match
    }

    /// Program the RTC alarm registers using BCD-encoded values.
    /// Also enables the alarm interrupt (AIE) in Status B.
    pub fn set_alarm_bcd(&mut self, hours_bcd: u8, minutes_bcd: u8, seconds_bcd: u8) {
        self.ram[regs::HOURS_ALARM as usize] = hours_bcd;
        self.ram[regs::MINUTES_ALARM as usize] = minutes_bcd;
        self.ram[regs::SECONDS_ALARM as usize] = seconds_bcd;
        self.ram[regs::STATUS_B as usize] |= 0x20; // AIE
        self.alarm_fired_this_second = false;
        self.clear_alarm_pending();
    }

    /// Disable the RTC alarm interrupt (AIE) and clear any pending alarm flag.
    pub fn disable_alarm(&mut self) {
        self.ram[regs::STATUS_B as usize] &= !0x20;
        self.alarm_fired_this_second = false;
        self.clear_alarm_pending();
    }

    fn clear_alarm_pending(&self) {
        let flags = self.pending_irq_flags.get() & !0x20; // AF
        self.pending_irq_flags.set(flags);
        // Re-derive rather than clearing IRQF on "no flags left": a still-pending
        // PF or UF only holds IRQF if ITS enable is set (MDD-BUG-CRUCIBLE-00621).
        self.refresh_irqf();
    }

    /// Check if any interrupt is pending (for status queries)
    #[inline]
    pub fn has_pending_interrupt(&self) -> bool {
        self.pending_irq_flags.get() != 0
    }

    /// Get pending interrupt flags without clearing them
    #[inline]
    pub fn peek_pending_flags(&self) -> u8 {
        self.pending_irq_flags.get()
    }

    /// Capture CMOS/RTC state without performing any clear-on-read register access.
    pub fn diagnostic_snapshot(&self) -> RtcDiagnosticSnapshot {
        let status_c_pending = self.pending_irq_flags.get();
        let (
            adjusted_seconds,
            adjusted_minutes,
            adjusted_hours,
            adjusted_day_of_week,
            adjusted_day,
            adjusted_month,
            adjusted_year,
        ) = self.get_adjusted_time();

        RtcDiagnosticSnapshot {
            selected_index: self.index,
            selected_effective_value: self.peek_effective_register(self.index),
            status_a_raw: self.ram[regs::STATUS_A as usize],
            status_a_effective: self.effective_status_a(),
            status_b: self.ram[regs::STATUS_B as usize],
            status_c_pending,
            status_d: self.effective_status_d(),
            irqf_pending: (status_c_pending & 0x80) != 0,
            periodic_pending: (status_c_pending & 0x40) != 0,
            alarm_pending: (status_c_pending & 0x20) != 0,
            update_ended_pending: (status_c_pending & 0x10) != 0,
            has_pending_interrupt: status_c_pending != 0,
            periodic_interrupt_enabled: self.is_periodic_interrupt_enabled(),
            periodic_period_us: self.get_periodic_period_us(),
            periodic_accum_us: self.periodic_accum_us,
            uip_accum_us: self.uip_accum_us,
            update_in_progress: (self.effective_status_a() & 0x80) != 0,
            emulated_us: self.emulated_us,
            startup_epoch_secs: self.startup_epoch_secs,
            time_offset_secs: self.time_offset_secs,
            adjusted_seconds,
            adjusted_minutes,
            adjusted_hours,
            adjusted_day_of_week,
            adjusted_day,
            adjusted_month,
            adjusted_year,
            encoded_seconds: self.get_rtc_seconds(),
            encoded_minutes: self.get_rtc_minutes(),
            encoded_hours: self.get_rtc_hours(),
            encoded_day: self.get_rtc_day(),
            encoded_month: self.get_rtc_month(),
            encoded_year: self.get_rtc_year(),
            encoded_century: self.get_rtc_century(),
            event_reschedule_pending: self.event_reschedule_pending,
        }
    }

    /// Save CMOS state for checkpoint.
    ///
    /// Captures a LOSSLESS snapshot of all mutable RTC state, not just
    /// index/NMI/RAM: the interrupt/timing accumulators, the Status C pending-IRQ
    /// shadow, alarm suppression, and the emulated-clock state (`emulated_us` +
    /// `time_offset_secs`). The previous `(index, nmi, ram)` tuple dropped every
    /// one of those, so a time-machine restore resumed with a different
    /// guest-visible RTC clock and IRQ8/Status-C state. MDD-BUG-CRUCIBLE-00031.
    pub fn save_state(&self) -> RtcCheckpointState {
        RtcCheckpointState {
            index: self.index,
            ram: self.ram,
            periodic_accum_us: self.periodic_accum_us,
            last_second: self.last_second,
            alarm_fired_this_second: self.alarm_fired_this_second,
            pending_irq_flags: self.pending_irq_flags.get(),
            uip_accum_us: self.uip_accum_us,
            emulated_us: self.emulated_us,
            time_offset_secs: self.time_offset_secs,
            calendar_clock_us: Some(self.calendar_clock_us),
            set_frozen_secs: self.set_frozen_secs,
            set_calendar_fields: self.set_calendar_fields,
            day_of_week: self.day_of_week,
        }
    }

    /// Restore CMOS state from checkpoint.
    ///
    /// Restores the live RTC timing / IRQ / emulated-clock state instead of
    /// resetting it to zero, so a time-machine resume is byte-identical for RTC
    /// register reads and first-tick IRQ8/Status-C behavior. MDD-BUG-CRUCIBLE-00031.
    pub fn restore_state(&mut self, s: RtcCheckpointState) {
        self.index = s.index;
        self.day_of_week = s.day_of_week;
        self.ram = s.ram;
        self.periodic_accum_us = s.periodic_accum_us;
        self.last_second = s.last_second;
        self.alarm_fired_this_second = s.alarm_fired_this_second;
        self.pending_irq_flags.set(s.pending_irq_flags);
        self.uip_accum_us = s.uip_accum_us;
        self.emulated_us = s.emulated_us;
        self.calendar_clock_us = s.calendar_clock_us.unwrap_or(s.emulated_us);
        self.time_offset_secs = s.time_offset_secs;
        // MDD-BUG-CRUCIBLE-00552 restored the SET hold; MDD-BUG-KIL-01039 fixed
        // where it came from. Rebuilding it from the restored clock was only
        // right while no raw time had passed under SET: the hold is taken when
        // SET rises and `emulated_us` keeps climbing, so a checkpoint captured
        // some seconds later rebuilt a hold that far ahead of the calendar the
        // guest was actually reading. The hold now travels in the checkpoint.
        self.set_frozen_secs = match (self.set_asserted(), s.set_frozen_secs) {
            (true, Some(held)) => Some(held),
            // A checkpoint asserting SET with no hold cannot come from
            // `save_state`, but the fields are public and 00552's guarantee is
            // that SET never resumes a RUNNING calendar. Freeze here.
            (true, None) => Some(self.raw_adjusted_epoch_secs()),
            // Likewise a stale hold with SET clear must not hold the calendar.
            (false, _) => None,
        };
        self.set_calendar_fields = self.set_frozen_secs.map(|frozen| {
            s.set_calendar_fields
                .unwrap_or_else(|| RtcCalendarFields::from_epoch(frozen))
        });
        // Re-arm the host RtcPeriodic timer to match the restored Status A rate
        // and periodic phase (mirrors Pit::restore_controller_state, which also
        // forces a reschedule so the host timer tracks the restored device state).
        self.event_reschedule_pending = true;
    }

    // =========================================================================
    // RTC offset persistence (BIOS setup and CMOS NVRAM)
    // =========================================================================

    /// Current RTC offset in seconds (host-clock-at-boot + offset = guest RTC).
    ///
    /// Persisted in the `.nvram` sidecar so the guest clock survives across
    /// runs. The host I/O lives in the emulator orchestration layer
    /// (`src/emulator/nvram.rs`), not in this `#[derive(Clone)]` device.
    #[inline]
    pub fn time_offset_secs(&self) -> i64 {
        self.time_offset_secs
    }

    /// Overwrite the RTC offset in seconds. Used when loading a `.nvram`
    /// sidecar; subsequent RTC reads derive from the restored offset.
    #[inline]
    pub fn set_time_offset_secs(&mut self, secs: i64) {
        self.time_offset_secs = secs;
    }
}

/// Days in `month` (1-12) of `year`. Out-of-range months clamp to 31 so a
/// malformed write can never shorten the month below any legal day.
fn days_in_month(month: u8, year: u16) -> u8 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn is_leap_year(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    mod regs {
        pub use crate::regs::*;
        pub const CENTURY: u8 = 0x32;
        pub const CENTURY_PS2: u8 = 0x37;
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

    fn test_cmos(epoch: Option<u64>) -> Mc146818 {
        let epoch = test_time_override()
            .map(|(sec, min, hr, _, day, mon, year)| {
                Mc146818::time_to_epoch_secs(sec, min, hr, day, mon, year) as u64
            })
            .or(epoch)
            .unwrap_or(1_700_000_000);
        Mc146818::new_with_century_registers(epoch, [Some(regs::CENTURY), Some(regs::CENTURY_PS2)])
    }

    // -- Multi-register date writes (MDD-BUG-FLUXHOMEARPA-00032) ------------
    //
    // A real MC146818 holds each date field in its own register, so a BIOS may
    // write month before day and leave a transiently impossible date that the
    // day write repairs. Our offset model has to collapse components to an
    // epoch, where an over-long day carries into the next month and destroys
    // the month just written. The BIOS date-setting handler writes century, year,
    // month, then day, so this fired on the 31st of any month.

    /// Set a date the way INT 1A AH=05 does, in century/year/month/day register order.
    fn int1a_set_date(cmos: &mut Mc146818, century: u8, year: u8, month: u8, day: u8) {
        for (reg, value) in [
            (regs::CENTURY, century),
            (regs::YEAR, year),
            (regs::MONTH, month),
            (regs::DAY_OF_MONTH, day),
        ] {
            cmos.write_index(reg);
            cmos.write_data(value);
        }
    }

    fn read_reg(cmos: &mut Mc146818, reg: u8) -> u8 {
        cmos.write_index(reg);
        cmos.read_data()
    }

    #[test]
    fn set_date_from_the_31st_keeps_the_written_month_and_day() {
        // 2026-07-31 12:00:00 UTC — the 31st is what makes the month write
        // land on an impossible date.
        let mut cmos = test_cmos(Some(1_785_499_200));
        assert_eq!(read_reg(&mut cmos, regs::MONTH), 0x07, "boot epoch check");
        assert_eq!(
            read_reg(&mut cmos, regs::DAY_OF_MONTH),
            0x31,
            "boot epoch check"
        );

        int1a_set_date(&mut cmos, 0x20, 0x26, 0x06, 0x11);

        assert_eq!(read_reg(&mut cmos, regs::MONTH), 0x06, "month must persist");
        assert_eq!(
            read_reg(&mut cmos, regs::DAY_OF_MONTH),
            0x11,
            "day must persist"
        );
        assert_eq!(read_reg(&mut cmos, regs::YEAR), 0x26);
        assert_eq!(read_reg(&mut cmos, regs::CENTURY), 0x20);
    }

    #[test]
    fn set_date_from_a_leap_day_into_a_non_leap_year_keeps_the_written_month() {
        // 2024-02-29 12:00:00 UTC — the year write alone shortens February.
        let mut cmos = test_cmos(Some(1_709_208_000));
        assert_eq!(read_reg(&mut cmos, regs::MONTH), 0x02, "boot epoch check");
        assert_eq!(
            read_reg(&mut cmos, regs::DAY_OF_MONTH),
            0x29,
            "boot epoch check"
        );

        int1a_set_date(&mut cmos, 0x20, 0x25, 0x02, 0x14);

        assert_eq!(read_reg(&mut cmos, regs::MONTH), 0x02, "month must persist");
        assert_eq!(read_reg(&mut cmos, regs::DAY_OF_MONTH), 0x14);
        assert_eq!(read_reg(&mut cmos, regs::YEAR), 0x25);
    }

    /// The clamp must not disturb an ordinary set-date, where no intermediate
    /// state is impossible.
    #[test]
    fn set_date_from_a_safe_day_is_unaffected() {
        // 2026-07-10 12:00:00 UTC.
        let mut cmos = test_cmos(Some(1_783_684_800));
        assert_eq!(
            read_reg(&mut cmos, regs::DAY_OF_MONTH),
            0x10,
            "boot epoch check"
        );

        // A nearby date, as a real guest sets: large jumps run into a
        // separate, pre-existing fidelity limit in the epoch model (see
        // scratchpad.md), which is not what this test is about.
        int1a_set_date(&mut cmos, 0x20, 0x26, 0x09, 0x25);

        assert_eq!(read_reg(&mut cmos, regs::CENTURY), 0x20);
        assert_eq!(read_reg(&mut cmos, regs::YEAR), 0x26);
        assert_eq!(read_reg(&mut cmos, regs::MONTH), 0x09);
        assert_eq!(read_reg(&mut cmos, regs::DAY_OF_MONTH), 0x25);
    }

    /// A day longer than the month clamps rather than carrying into the next
    /// month — writing one field must never move another.
    #[test]
    fn an_over_long_day_clamps_instead_of_carrying_into_the_next_month() {
        let mut cmos = test_cmos(Some(1_783_684_800));
        cmos.write_index(regs::MONTH);
        cmos.write_data(0x06);
        cmos.write_index(regs::DAY_OF_MONTH);
        cmos.write_data(0x31);

        assert_eq!(
            read_reg(&mut cmos, regs::MONTH),
            0x06,
            "June must stay June"
        );
        assert_eq!(read_reg(&mut cmos, regs::DAY_OF_MONTH), 0x30);
    }

    #[test]
    fn days_in_month_covers_leap_years_and_out_of_range_months() {
        assert_eq!(days_in_month(1, 2026), 31);
        assert_eq!(days_in_month(2, 2026), 28);
        assert_eq!(days_in_month(2, 2024), 29);
        assert_eq!(days_in_month(2, 2000), 29, "2000 is a leap year");
        assert_eq!(days_in_month(2, 1900), 28, "1900 is not");
        assert_eq!(days_in_month(4, 2026), 30);
        assert_eq!(days_in_month(12, 2026), 31);
        assert_eq!(
            days_in_month(0, 2026),
            31,
            "a malformed month must not shorten"
        );
        assert_eq!(days_in_month(13, 2026), 31);
    }
    use super::*;

    #[test]
    fn test_diagnostic_snapshot_peeks_status_c_without_clearing() {
        let mut cmos = test_cmos(None);
        cmos.write_index(regs::STATUS_B);
        cmos.write_data(0x42); // 24-hour mode + periodic interrupt enable.
        assert!(cmos.tick(977));
        let pending = cmos.peek_pending_flags();
        assert_ne!(pending, 0);

        cmos.write_index(regs::STATUS_C);
        let snapshot = cmos.diagnostic_snapshot();

        assert_eq!(snapshot.selected_index, regs::STATUS_C);
        assert_eq!(snapshot.selected_effective_value, pending);
        assert_eq!(snapshot.status_c_pending, pending);
        assert!(snapshot.irqf_pending);
        assert!(snapshot.periodic_pending);
        assert!(snapshot.has_pending_interrupt);
        assert_eq!(cmos.peek_pending_flags(), pending);

        assert_eq!(cmos.read_data(), pending);
        assert_eq!(cmos.peek_pending_flags(), 0);
    }

    #[test]
    fn test_cmos_ram_readwrite() {
        let mut cmos = test_cmos(None);
        // Write to a general RAM location
        cmos.write_index(0x20);
        cmos.write_data(0x42);
        cmos.write_index(0x20);
        assert_eq!(cmos.read_data(), 0x42);
    }

    #[test]
    fn test_write_data_out_of_range_index_is_ignored() {
        let mut cmos = test_cmos(None);
        cmos.write_index(0x20);
        cmos.write_data(0x12);

        cmos.index = 0xFF;
        cmos.write_data(0x34);

        cmos.write_index(0x20);
        assert_eq!(cmos.read_data(), 0x12);
    }

    #[test]
    fn test_read_data_out_of_range_returns_ff() {
        let mut cmos = test_cmos(None);
        cmos.index = 0xFF;
        assert_eq!(cmos.read_data(), 0xFF);
    }

    #[test]
    fn test_bcd_conversion() {
        assert_eq!(Mc146818::to_bcd(0), 0x00);
        assert_eq!(Mc146818::to_bcd(9), 0x09);
        assert_eq!(Mc146818::to_bcd(10), 0x10);
        assert_eq!(Mc146818::to_bcd(59), 0x59);
        assert_eq!(Mc146818::to_bcd(99), 0x99);
    }

    #[test]
    fn test_rtc_time_read() {
        let mut cmos = test_cmos(None);
        // Just verify we can read time registers without panicking
        cmos.write_index(regs::SECONDS);
        let _ = cmos.read_data();
        cmos.write_index(regs::MINUTES);
        let _ = cmos.read_data();
        cmos.write_index(regs::HOURS);
        let _ = cmos.read_data();
    }

    #[test]
    fn test_status_d_battery() {
        let _cmos = test_cmos(None);
        // Status D bit 7 indicates valid battery/RAM
        let mut c = test_cmos(None);
        c.write_index(regs::STATUS_D);
        let status = c.read_data();
        assert_eq!(status & 0x80, 0x80);
    }

    #[test]
    fn test_status_d_guest_writes_are_ignored() {
        let mut cmos = test_cmos(None);
        cmos.write_index(regs::STATUS_D);

        for value in 0_u8..=u8::MAX {
            cmos.write_data(value);
            assert_eq!(
                cmos.read_data(),
                0x80,
                "Status D changed after {value:#04x}"
            );
        }

        cmos.write_index(0x40);
        cmos.write_data(0x5A);
        assert_eq!(cmos.read_data(), 0x5A);
        assert_eq!(cmos.nvram()[0x40], 0x5A);
    }

    #[test]
    fn test_status_d_reserved_bits_read_as_zero_and_checkpoint_is_raw() {
        for (raw_status_d, expected_status_d) in [(0x7F, 0x00), (0xFF, 0x80)] {
            let mut cmos = test_cmos(None);
            cmos.nvram_mut()[regs::STATUS_D as usize] = raw_status_d;
            cmos.write_index(regs::STATUS_D);

            assert_eq!(cmos.read_data(), expected_status_d);
            let snapshot = cmos.diagnostic_snapshot();
            assert_eq!(snapshot.selected_effective_value, expected_status_d);
            assert_eq!(snapshot.status_d, expected_status_d);

            for value in 0_u8..=u8::MAX {
                cmos.write_data(value);
                assert_eq!(cmos.read_data(), expected_status_d);
                assert_eq!(cmos.nvram()[regs::STATUS_D as usize], raw_status_d);
            }

            let checkpoint = cmos.save_state();
            assert_eq!(checkpoint.ram[regs::STATUS_D as usize], raw_status_d);
            let mut restored = test_cmos(None);
            restored.restore_state(checkpoint);
            assert_eq!(restored.nvram()[regs::STATUS_D as usize], raw_status_d);
            assert_eq!(restored.read_data(), expected_status_d);
        }
    }

    #[test]
    fn test_write_status_c_is_ignored() {
        let mut cmos = test_cmos(None);
        cmos.write_index(regs::STATUS_C);
        cmos.write_data(0xFF);
        cmos.write_index(regs::STATUS_C);
        assert_eq!(cmos.read_data(), 0x00);
    }

    #[test]
    fn test_status_a_uip_bit_reflects_timing() {
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_A as usize] = 0x26;

        // When uip_accum_us is below threshold, UIP should be clear
        cmos.uip_accum_us = 0;
        cmos.write_index(regs::STATUS_A);
        assert_eq!(cmos.read_data() & 0x80, 0x00);

        // When uip_accum_us is at/above 999756, UIP should be set
        cmos.uip_accum_us = 999_756;
        cmos.write_index(regs::STATUS_A);
        assert_eq!(cmos.read_data() & 0x80, 0x80);

        // Verify lower 7 bits are preserved
        assert_eq!(cmos.read_data() & 0x7F, 0x26);
    }

    #[test]
    fn test_write_time_registers_adjusts_offset() {
        let _guard = TestTimeGuard::set((30, 15, 10, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary

        // Write seconds = 45 (was 30)
        cmos.write_index(regs::SECONDS);
        cmos.write_data(45);

        // Read back should reflect the offset
        cmos.write_index(regs::SECONDS);
        let value = cmos.read_data();
        assert_eq!(value, 45);
    }

    #[test]
    fn test_write_hours_24h_binary_path() {
        let _guard = TestTimeGuard::set((0, 0, 1, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary

        cmos.write_index(regs::HOURS);
        cmos.write_data(21);

        cmos.write_index(regs::HOURS);
        assert_eq!(cmos.read_data(), 21);
    }

    #[test]
    fn test_write_hours_12h_bcd_midnight_and_noon_paths() {
        // 12-hour + BCD input: 12 AM should map to 00h.
        {
            let _guard = TestTimeGuard::set((0, 0, 1, 3, 12, 5, 2024));
            let mut cmos = test_cmos(None);
            cmos.ram[regs::STATUS_B as usize] = 0x00;
            cmos.write_index(regs::HOURS);
            cmos.write_data(0x12);
            cmos.ram[regs::STATUS_B as usize] = 0x06; // Read in 24-hour binary
            cmos.write_index(regs::HOURS);
            assert_eq!(cmos.read_data(), 0);
        }

        // 12-hour + BCD input: 12 PM should map to 12h.
        {
            let _guard = TestTimeGuard::set((0, 0, 1, 3, 12, 5, 2024));
            let mut cmos = test_cmos(None);
            cmos.ram[regs::STATUS_B as usize] = 0x00;
            cmos.write_index(regs::HOURS);
            cmos.write_data(0x92); // 12 + PM bit
            cmos.ram[regs::STATUS_B as usize] = 0x06;
            cmos.write_index(regs::HOURS);
            assert_eq!(cmos.read_data(), 12);
        }
    }

    #[test]
    fn test_write_hours_12h_bcd_pm_non_twelve_path() {
        let _guard = TestTimeGuard::set((0, 0, 1, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x00; // 12-hour + BCD input
        cmos.write_index(regs::HOURS);
        cmos.write_data(0x81); // 1 PM (BCD + PM bit)
        cmos.ram[regs::STATUS_B as usize] = 0x06; // Read back in 24-hour binary
        cmos.write_index(regs::HOURS);
        assert_eq!(cmos.read_data(), 13);
    }

    #[test]
    fn test_write_hours_12h_bcd_am_non_twelve_path() {
        let _guard = TestTimeGuard::set((0, 0, 1, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x00; // 12-hour + BCD input
        cmos.write_index(regs::HOURS);
        cmos.write_data(0x01); // 1 AM
        cmos.ram[regs::STATUS_B as usize] = 0x06; // Read back in 24-hour binary
        cmos.write_index(regs::HOURS);
        assert_eq!(cmos.read_data(), 1);
    }

    #[test]
    fn test_read_time_binary_mode() {
        let _guard = TestTimeGuard::set((0, 0, 0, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary

        // Advance emulated time to 15:07:42
        cmos.advance_clock((15 * 3600 + 7 * 60 + 42) * 1_000_000);

        cmos.write_index(regs::SECONDS);
        assert_eq!(cmos.read_data(), 42);
        cmos.write_index(regs::MINUTES);
        assert_eq!(cmos.read_data(), 7);
        cmos.write_index(regs::HOURS);
        assert_eq!(cmos.read_data(), 15);
        cmos.write_index(regs::DAY_OF_MONTH);
        assert_eq!(cmos.read_data(), 12);
        cmos.write_index(regs::MONTH);
        assert_eq!(cmos.read_data(), 5);
        cmos.write_index(regs::YEAR);
        assert_eq!(cmos.read_data(), 24);
        cmos.write_index(regs::CENTURY);
        assert_eq!(cmos.read_data(), 20);
    }

    #[test]
    fn test_read_date_bcd_mode() {
        let _guard = TestTimeGuard::set((5, 4, 3, 2, 23, 11, 1985));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x02; // 24-hour + BCD

        cmos.write_index(regs::DAY_OF_MONTH);
        assert_eq!(cmos.read_data(), 0x23);
        cmos.write_index(regs::MONTH);
        assert_eq!(cmos.read_data(), 0x11);
        cmos.write_index(regs::YEAR);
        assert_eq!(cmos.read_data(), 0x85);
        cmos.write_index(regs::CENTURY);
        assert_eq!(cmos.read_data(), 0x19);
    }

    #[test]
    fn test_read_ps2_century_alias_bcd_mode() {
        let _guard = TestTimeGuard::set((5, 4, 3, 2, 23, 11, 1985));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x02; // 24-hour + BCD

        cmos.write_index(regs::CENTURY_PS2);
        assert_eq!(cmos.read_data(), 0x19);
        assert_eq!(cmos.diagnostic_snapshot().selected_effective_value, 0x19);
    }

    #[test]
    fn test_read_hours_binary_24h() {
        let _guard = TestTimeGuard::set((0, 0, 14, 3, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary
        cmos.write_index(regs::HOURS);
        let value = cmos.read_data();
        assert_eq!(value, 14);
    }

    #[test]
    fn test_read_hours_bcd_12h_midnight() {
        let _guard = TestTimeGuard::set((0, 0, 0, 1, 1, 1, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x00; // 12-hour + BCD
        cmos.write_index(regs::HOURS);
        let value = cmos.read_data();
        assert_eq!(value, 0x12);
    }

    #[test]
    fn test_read_hours_bcd_12h_morning() {
        let _guard = TestTimeGuard::set((0, 0, 0, 1, 1, 1, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x00; // 12-hour + BCD
                                                  // Advance emulated time to 11:00:00
        cmos.advance_clock(11 * 3600 * 1_000_000);
        cmos.write_index(regs::HOURS);
        let value = cmos.read_data();
        assert_eq!(value, 0x11);
    }

    #[test]
    fn test_read_hours_bcd_12h_afternoon() {
        let _guard = TestTimeGuard::set((0, 0, 0, 1, 1, 1, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x00; // 12-hour + BCD
                                                  // Advance emulated time to 13:00:00
        cmos.advance_clock(13 * 3600 * 1_000_000);
        cmos.write_index(regs::HOURS);
        let value = cmos.read_data();
        assert_eq!(value, 0x81);
    }

    #[test]
    fn test_compute_time_components_leap_year_feb_29() {
        let secs = 789u64 * 86400;
        let (_, _, _, _, day, month, year) = Mc146818::compute_time_components(secs);
        assert_eq!(year, 1972);
        assert_eq!(month, 2);
        assert_eq!(day, 29);
    }

    #[test]
    fn test_compute_time_components_non_leap_february() {
        let secs = 396u64 * 86400;
        let (_, _, _, _, day, month, year) = Mc146818::compute_time_components(secs);
        assert_eq!(year, 1971);
        assert_eq!(month, 2);
        assert_eq!(day, 1);
    }

    #[test]
    fn test_time_to_epoch_secs_february_leap_handling() {
        let leap_jan = Mc146818::time_to_epoch_secs(0, 0, 0, 1, 1, 2024);
        let leap_march = Mc146818::time_to_epoch_secs(0, 0, 0, 1, 3, 2024);
        assert_eq!(leap_march - leap_jan, 60 * 86400);

        let non_leap_jan = Mc146818::time_to_epoch_secs(0, 0, 0, 1, 1, 2023);
        let non_leap_march = Mc146818::time_to_epoch_secs(0, 0, 0, 1, 3, 2023);
        assert_eq!(non_leap_march - non_leap_jan, 59 * 86400);
    }

    #[test]
    fn test_time_to_epoch_secs_invalid_month_does_not_panic() {
        // Month > 12 from invalid CMOS register writes must not cause
        // an array index out-of-bounds panic in time_to_epoch_secs.
        let _ = Mc146818::time_to_epoch_secs(0, 0, 0, 1, 13, 2024);
        let _ = Mc146818::time_to_epoch_secs(0, 0, 0, 1, 255, 2024);
        // Month 0 (below valid range) should also be clamped safely.
        let _ = Mc146818::time_to_epoch_secs(0, 0, 0, 1, 0, 2024);
    }

    #[test]
    fn test_write_invalid_month_register_does_not_panic() {
        // Writing an out-of-range month (13 in BCD = 0x13) via the CMOS
        // data port must not panic when the time offset is recalculated.
        let _guard = TestTimeGuard::set((0, 0, 12, 3, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary

        // Write month = 13 (invalid, should be clamped internally)
        cmos.write_index(regs::MONTH);
        cmos.write_data(13);

        // Reading any time register must also not panic
        cmos.write_index(regs::MONTH);
        let _ = cmos.read_data();
    }

    #[test]
    fn test_is_leap_year_century_and_regular_rules() {
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2023));
    }

    // =========================================================================
    // RTC Interrupt Tests
    // =========================================================================

    #[test]
    fn test_periodic_interrupt_disabled_by_default() {
        let mut cmos = test_cmos(None);
        // PIE bit should be clear by default
        assert_eq!(cmos.ram[regs::STATUS_B as usize] & 0x40, 0);
        // tick should not fire IRQ when PIE is disabled
        assert!(!cmos.tick(10_000)); // 10ms
    }

    #[test]
    fn test_periodic_interrupt_fires_at_default_rate() {
        let mut cmos = test_cmos(None);
        // Enable periodic interrupt (PIE = bit 6)
        cmos.ram[regs::STATUS_B as usize] |= 0x40;
        // Default rate (status_a = 0x26) is 1024 Hz = 977 µs period

        // Less than one period should not fire
        assert!(!cmos.tick(500));
        assert!(!cmos.has_pending_interrupt());

        // More than one period should fire
        assert!(cmos.tick(600));
        assert!(cmos.has_pending_interrupt());

        // Check flags: IRQF (0x80) + PF (0x40)
        assert_eq!(cmos.peek_pending_flags(), 0xC0);
    }

    #[test]
    fn test_periodic_interrupt_level_triggered() {
        let mut cmos = test_cmos(None);
        // Enable periodic interrupt
        cmos.ram[regs::STATUS_B as usize] |= 0x40;

        // Fire interrupt
        assert!(cmos.tick(1000)); // 1ms > 977µs period
        assert!(cmos.has_pending_interrupt());

        // Should not fire again while pending (level-triggered behavior)
        assert!(!cmos.tick(2000));
        assert!(cmos.has_pending_interrupt());

        // Reading Status C clears pending flags
        cmos.write_index(regs::STATUS_C);
        let flags = cmos.read_data();
        assert_eq!(flags, 0xC0); // IRQF + PF
        assert!(!cmos.has_pending_interrupt());

        // Now it should fire again
        assert!(cmos.tick(1000));
        assert!(cmos.has_pending_interrupt());
    }

    #[test]
    fn test_status_c_clear_on_read() {
        let mut cmos = test_cmos(None);
        // Enable periodic interrupt
        cmos.ram[regs::STATUS_B as usize] |= 0x40;

        // Fire interrupt
        assert!(cmos.tick(1000));
        assert_eq!(cmos.peek_pending_flags(), 0xC0);

        // First read should return flags and clear them
        cmos.write_index(regs::STATUS_C);
        let flags1 = cmos.read_data();
        assert_eq!(flags1, 0xC0);

        // Second read should return 0
        cmos.write_index(regs::STATUS_C);
        let flags2 = cmos.read_data();
        assert_eq!(flags2, 0x00);
    }

    #[test]
    fn test_periodic_rate_selection() {
        let mut cmos = test_cmos(None);
        // Enable periodic interrupt
        cmos.ram[regs::STATUS_B as usize] |= 0x40;

        // Set rate to 2 Hz (500,000 µs period) - rate = 15
        cmos.ram[regs::STATUS_A as usize] = 0x2F; // Keep divider bits, rate = 15

        // 100ms should not fire
        assert!(!cmos.tick(100_000));

        // 500ms total should fire
        assert!(cmos.tick(400_000));
        assert!(cmos.has_pending_interrupt());
    }

    #[test]
    fn test_periodic_rate_disabled() {
        let mut cmos = test_cmos(None);
        // Enable periodic interrupt
        cmos.ram[regs::STATUS_B as usize] |= 0x40;

        // Set rate to 0 (disabled)
        cmos.ram[regs::STATUS_A as usize] = 0x20; // Divider bits only, rate = 0

        // Even large time should not fire
        assert!(!cmos.tick(1_000_000)); // 1 second
        assert!(!cmos.has_pending_interrupt());
    }

    #[test]
    fn test_update_ended_interrupt_disabled_by_default() {
        let cmos = test_cmos(None);
        // UIE bit should be clear by default
        assert_eq!(cmos.ram[regs::STATUS_B as usize] & 0x10, 0);
    }

    #[test]
    fn test_update_ended_interrupt_sets_flags() {
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] |= 0x10;

        // Drive a REAL second boundary rather than re-arming the
        // `NO_PREVIOUS_SECOND` sentinel (MDD-BUG-CRUCIBLE-00621). This test used
        // to set the sentinel and `tick(0)`, asserting an update-ended flag after
        // ZERO microseconds — which no update cycle can produce. That only ever
        // "worked" because the sentinel makes the first tick look like a second
        // change whatever the elapsed time. The property being pinned (a second
        // boundary sets UF, and UIE promotes it to IRQF) is unchanged.
        assert!(
            !cmos.tick(0),
            "the first tick only syncs the second tracker"
        );
        assert_eq!(
            cmos.peek_pending_flags(),
            0,
            "syncing the tracker must not invent an update cycle"
        );

        let fired = cmos.tick(1_000_000);
        assert!(fired);
        let flags = cmos.peek_pending_flags();
        assert_eq!(flags & 0x10, 0x10);
        assert_eq!(flags & 0x80, 0x80);
    }

    #[test]
    fn test_alarm_interrupt_disabled_by_default() {
        let cmos = test_cmos(None);
        // AIE bit should be clear by default
        assert_eq!(cmos.ram[regs::STATUS_B as usize] & 0x20, 0);
    }

    #[test]
    fn first_periodic_tick_does_not_invent_alarm_or_update_cycle() {
        let mut cmos = Mc146818::new(0);
        cmos.ram[regs::STATUS_B as usize] |= 0x70; // AIE + UIE + PIE

        assert!(cmos.tick_periodic(976), "PIE still raises IRQ8 for PF");
        let flags = cmos.peek_pending_flags();
        assert_eq!(flags & 0x70, 0x40, "first sync must latch PF only");
        assert_eq!(flags & 0x80, 0x80, "PF with PIE must qualify IRQ8");
    }

    #[test]
    fn midsecond_alarm_waits_for_update_and_next_second_alarm_fires() {
        let mut current_second = Mc146818::new(0);
        current_second.ram[regs::STATUS_B as usize] = 0x12; // UIE, 24-hour BCD
        current_second.advance_clock(5_000_000); // First observation syncs second 5.
        current_second.advance_clock(500_000); // Program midway through second 5.
        current_second.set_alarm_bcd(0x00, 0x00, 0x05);

        current_second.tick_periodic(976);
        let flags = current_second.peek_pending_flags();
        assert_eq!(
            flags & 0x20,
            0,
            "an alarm programmed for the current second must wait for an update cycle"
        );

        assert!(
            current_second.advance_clock(499_024),
            "the next second's UF with UIE must raise IRQ8"
        );
        let flags = current_second.peek_pending_flags();
        assert_eq!(flags & 0x70, 0x50, "PF and UF latch, but AF does not");
        assert_eq!(flags & 0x80, 0x80);

        let mut next_second = Mc146818::new(0);
        next_second.ram[regs::STATUS_B as usize] = 0x12; // UIE, 24-hour BCD
        next_second.advance_clock(5_000_000); // Sync second 5.
        next_second.advance_clock(500_000); // Reach 5.5 seconds.
        next_second.set_alarm_bcd(0x00, 0x00, 0x06);

        assert!(
            next_second.advance_clock(500_000),
            "matching AF and UF with AIE/UIE must raise IRQ8 at second 6"
        );
        let flags = next_second.peek_pending_flags();
        assert_eq!(flags & 0x70, 0x30, "AF and UF latch at the update boundary");
        assert_eq!(flags & 0x80, 0x80);
    }

    #[test]
    fn wildcard_alarm_fires_at_update_and_status_c_ack_suppresses_relatch() {
        let mut cmos = Mc146818::new(0);
        cmos.ram[regs::STATUS_B as usize] = 0x12; // UIE, 24-hour BCD
        cmos.advance_clock(5_000_000); // Sync second 5.
        cmos.ram[regs::STATUS_B as usize] |= 0x20; // AIE
        cmos.ram[regs::SECONDS_ALARM as usize] = 0xC0;
        cmos.ram[regs::MINUTES_ALARM as usize] = 0xC0;
        cmos.ram[regs::HOURS_ALARM as usize] = 0xC0;

        assert!(
            !cmos.advance_clock(500_000),
            "a wildcard alarm must not fire midway through second 5"
        );
        assert_eq!(cmos.peek_pending_flags() & 0x20, 0);

        assert!(
            cmos.advance_clock(500_000),
            "wildcard AF and UF with AIE/UIE must raise IRQ8 at second 6"
        );
        assert_eq!(cmos.peek_pending_flags() & 0x70, 0x30);
        assert_eq!(cmos.peek_pending_flags() & 0x80, 0x80);

        cmos.write_index(regs::STATUS_C);
        assert_eq!(cmos.read_data() & 0x70, 0x30, "Status C reports AF and UF");
        assert_eq!(cmos.peek_pending_flags(), 0, "Status C clears all flags");

        assert!(
            !cmos.tick_periodic(976),
            "same-second PF is disabled for IRQ8"
        );
        assert_eq!(
            cmos.peek_pending_flags() & 0x70,
            0x40,
            "Status C acknowledgement must not relatch AF during the same second"
        );
    }

    #[test]
    fn test_alarm_wildcard_match() {
        let _guard = TestTimeGuard::set((0, 0, 0, 1, 1, 1, 2024));
        let mut c = test_cmos(None);
        c.ram[regs::SECONDS_ALARM as usize] = 0xC0; // Don't care (two MSBs set)
        c.ram[regs::MINUTES_ALARM as usize] = 0xC0;
        c.ram[regs::HOURS_ALARM as usize] = 0xC0;

        // Enable alarm interrupt
        c.ram[regs::STATUS_B as usize] |= 0x20;

        assert!(!c.tick(0), "first tick only syncs the second tracker");
        assert_eq!(c.peek_pending_flags() & 0x20, 0);

        // The wildcard matches when the next calendar update completes.
        assert!(c.tick(1_000_000));
        assert_eq!(c.peek_pending_flags() & 0x20, 0x20); // AF flag
    }

    #[test]
    fn test_alarm_0x80_to_0xbf_are_not_wildcards() {
        let _guard = TestTimeGuard::set((59, 59, 23, 1, 1, 1, 2024));
        let mut c = test_cmos(None);
        c.ram[regs::STATUS_B as usize] |= 0x20; // AIE, BCD mode
        c.ram[regs::SECONDS_ALARM as usize] = 0x80;
        c.ram[regs::MINUTES_ALARM as usize] = 0xC0;
        c.ram[regs::HOURS_ALARM as usize] = 0xC0;

        assert!(!c.tick(0), "first tick only syncs the second tracker");
        assert!(!c.tick(1_000_000), "0x80 must compare, not wildcard-match");
        assert_eq!(c.peek_pending_flags() & 0x20, 0x00);

        c.ram[regs::SECONDS_ALARM as usize] = 0xBF;
        assert!(!c.tick(1_000_000), "0xBF must compare, not wildcard-match");
        assert_eq!(c.peek_pending_flags() & 0x20, 0x00);

        c.ram[regs::SECONDS_ALARM as usize] = 0xC0;
        assert!(c.tick(1_000_000), "0xC0 is the first wildcard value");
        assert_eq!(c.peek_pending_flags() & 0x20, 0x20);
    }

    #[test]
    fn test_alarm_match_binary_mode() {
        let _guard = TestTimeGuard::set((59, 59, 23, 1, 1, 1, 2024));
        let mut c = test_cmos(None);
        // The clock advances from 23:59:59 through a real midnight update.
        c.ram[regs::STATUS_B as usize] |= 0x24; // AIE + binary mode
        c.ram[regs::SECONDS_ALARM as usize] = 0;
        c.ram[regs::MINUTES_ALARM as usize] = 0;
        c.ram[regs::HOURS_ALARM as usize] = 0;

        assert!(!c.tick(0), "first tick only syncs the second tracker");
        assert!(c.tick(1_000_000));
        assert_eq!(c.peek_pending_flags() & 0x20, 0x20);
    }

    #[test]
    fn test_alarm_match_bcd_mode() {
        let _guard = TestTimeGuard::set((59, 59, 23, 1, 1, 1, 2024));
        let mut c = test_cmos(None);
        // The clock advances from 23:59:59 through a real midnight update.
        c.ram[regs::STATUS_B as usize] |= 0x20; // AIE in BCD mode
        c.ram[regs::SECONDS_ALARM as usize] = Mc146818::to_bcd(0);
        c.ram[regs::MINUTES_ALARM as usize] = Mc146818::to_bcd(0);
        c.ram[regs::HOURS_ALARM as usize] = Mc146818::to_bcd(0);

        assert!(!c.tick(0), "first tick only syncs the second tracker");
        assert!(c.tick(1_000_000));
        assert_eq!(c.peek_pending_flags() & 0x20, 0x20);
    }

    #[test]
    fn test_alarm_skipped_when_already_fired() {
        let _guard = TestTimeGuard::set((59, 59, 0, 1, 1, 1, 2024));
        let mut c = test_cmos(None);
        c.ram[regs::STATUS_B as usize] |= 0x20;
        c.ram[regs::SECONDS_ALARM as usize] = 0x00;
        c.ram[regs::MINUTES_ALARM as usize] = 0x00;
        c.ram[regs::HOURS_ALARM as usize] = 0x01;

        assert!(!c.tick(0), "first tick only syncs the second tracker");
        assert!(c.tick(1_000_000), "the matching update must fire the alarm");
        c.write_index(regs::STATUS_C);
        assert_eq!(c.read_data() & 0x20, 0x20);

        let fired = c.tick(0);
        assert!(!fired);
        assert_eq!(c.peek_pending_flags(), 0);
    }

    #[test]
    fn test_alarm_mismatch_returns_false() {
        let mut c = test_cmos(None);
        c.ram[regs::SECONDS_ALARM as usize] = 0x00;
        c.ram[regs::MINUTES_ALARM as usize] = 0x00;
        c.ram[regs::HOURS_ALARM as usize] = 0x00;

        assert!(!c.check_alarm_match(1, 0, 0));
    }

    #[test]
    fn test_alarm_minute_mismatch_returns_false() {
        let mut c = test_cmos(None);
        c.ram[regs::STATUS_B as usize] = 0x04; // Binary mode
        c.ram[regs::SECONDS_ALARM as usize] = 10;
        c.ram[regs::MINUTES_ALARM as usize] = 22;
        c.ram[regs::HOURS_ALARM as usize] = 5;

        assert!(!c.check_alarm_match(10, 21, 5));
    }

    #[test]
    fn test_alarm_hour_mismatch_returns_false() {
        let mut c = test_cmos(None);
        c.ram[regs::STATUS_B as usize] = 0x04; // Binary mode
        c.ram[regs::SECONDS_ALARM as usize] = 10;
        c.ram[regs::MINUTES_ALARM as usize] = 21;
        c.ram[regs::HOURS_ALARM as usize] = 5;

        assert!(!c.check_alarm_match(10, 21, 6));
    }

    #[test]
    fn test_alarm_exact_match_returns_true() {
        let mut c = test_cmos(None);
        c.ram[regs::STATUS_B as usize] = 0x04; // Binary mode
        c.ram[regs::SECONDS_ALARM as usize] = 10;
        c.ram[regs::MINUTES_ALARM as usize] = 21;
        c.ram[regs::HOURS_ALARM as usize] = 5;

        assert!(c.check_alarm_match(10, 21, 5));
    }

    #[test]
    fn twelve_hour_alarm_matches_the_pm_encoded_hour() {
        // In 12-hour mode the guest programs the alarm hour as 1-12 with bit 7
        // as PM, so comparing it against a 24-hour value never matches: a 1 PM
        // alarm is 0x81, not 0x13. Midnight is 12 AM and noon is 12 PM.
        // (status_b, hour24, alarm register value)
        let cases: &[(u8, u8, u8)] = &[
            // 12-hour BCD: Status-B bits 1 and 2 both clear.
            (0x00, 0, 0x12),
            (0x00, 1, 0x01),
            (0x00, 11, 0x11),
            (0x00, 12, 0x92),
            (0x00, 13, 0x81),
            (0x00, 23, 0x91),
            // 12-hour binary: Status-B bit 2 set, bit 1 clear.
            (0x04, 0, 12),
            (0x04, 1, 1),
            (0x04, 11, 11),
            (0x04, 12, 12 | 0x80),
            (0x04, 13, 1 | 0x80),
            (0x04, 23, 11 | 0x80),
        ];
        for &(status_b, hour24, alarm) in cases {
            let mut c = test_cmos(None);
            c.ram[regs::STATUS_B as usize] = status_b;
            // Wildcard the other fields so only the hour is under test.
            c.ram[regs::SECONDS_ALARM as usize] = 0xC0;
            c.ram[regs::MINUTES_ALARM as usize] = 0xC0;
            c.ram[regs::HOURS_ALARM as usize] = alarm;
            assert!(
                c.check_alarm_match(0, 0, hour24),
                "status_b {status_b:#04x}: {hour24}:00 should match alarm {alarm:#04x}"
            );
        }
    }

    #[test]
    fn twelve_hour_alarm_distinguishes_am_from_pm() {
        // The PM bit is the only thing separating 1 AM from 1 PM, in either
        // encoding. Neither may fire at the other's hour.
        for status_b in [0x00u8, 0x04u8] {
            for (alarm, wrong_hour) in [(0x01u8, 13u8), (0x81u8, 1u8)] {
                let mut c = test_cmos(None);
                c.ram[regs::STATUS_B as usize] = status_b;
                c.ram[regs::SECONDS_ALARM as usize] = 0xC0;
                c.ram[regs::MINUTES_ALARM as usize] = 0xC0;
                c.ram[regs::HOURS_ALARM as usize] = alarm;
                assert!(
                    !c.check_alarm_match(0, 0, wrong_hour),
                    "status_b {status_b:#04x}: alarm {alarm:#04x} must not match {wrong_hour}:00"
                );
            }
        }
    }

    #[test]
    fn twenty_four_hour_alarm_hour_is_unencoded() {
        // Status-B bit 1 set: the hour is compared as-is, with no PM bit.
        for (status_b, hour24, alarm) in [(0x02u8, 13u8, 0x13u8), (0x06, 13, 13)] {
            let mut c = test_cmos(None);
            c.ram[regs::STATUS_B as usize] = status_b;
            c.ram[regs::SECONDS_ALARM as usize] = 0xC0;
            c.ram[regs::MINUTES_ALARM as usize] = 0xC0;
            c.ram[regs::HOURS_ALARM as usize] = alarm;
            assert!(
                c.check_alarm_match(0, 0, hour24),
                "status_b {status_b:#04x}: {hour24}:00 should match alarm {alarm:#04x}"
            );
        }
    }

    #[test]
    fn twelve_hour_pm_alarm_actually_fires() {
        // End to end through tick(): a 1 PM alarm programmed in 12-hour BCD
        // raises AF when the clock reaches 13:00:00.
        let _guard = TestTimeGuard::set((59, 59, 12, 1, 1, 1, 2024));
        let mut c = test_cmos(None);
        c.ram[regs::STATUS_B as usize] = 0x20; // AIE, 12-hour, BCD
        c.ram[regs::SECONDS_ALARM as usize] = 0x00;
        c.ram[regs::MINUTES_ALARM as usize] = 0x00;
        c.ram[regs::HOURS_ALARM as usize] = 0x81; // 1 PM

        assert!(!c.tick(0), "first tick only syncs the second tracker");
        assert!(c.tick(1_000_000));
        assert_eq!(c.peek_pending_flags() & 0x20, 0x20);
    }

    #[test]
    fn test_alarm_mismatch_does_not_fire() {
        let _guard = TestTimeGuard::set((9, 20, 3, 2, 1, 1, 2024));
        let mut c = test_cmos(None);
        c.ram[regs::STATUS_B as usize] = 0x26; // AIE + 24-hour + binary
        c.ram[regs::SECONDS_ALARM as usize] = 11;
        c.ram[regs::MINUTES_ALARM as usize] = 20;
        c.ram[regs::HOURS_ALARM as usize] = 3;

        assert!(!c.tick(0), "first tick only syncs the second tracker");
        let fired = c.tick(1_000_000);
        assert!(!fired);
        assert_eq!(c.peek_pending_flags() & 0x20, 0x00);
    }

    #[test]
    fn test_multiple_interrupt_types() {
        let mut cmos = test_cmos(None);
        // Enable periodic interrupt only
        cmos.ram[regs::STATUS_B as usize] |= 0x40;

        // Fire periodic
        assert!(cmos.tick(1000));
        let flags = cmos.peek_pending_flags();
        assert_eq!(flags & 0x40, 0x40); // PF
        assert_eq!(flags & 0x80, 0x80); // IRQF

        // Clear flags
        cmos.write_index(regs::STATUS_C);
        cmos.read_data();

        // Enable a wildcard alarm; it is compared when the next update completes.
        cmos.ram[regs::STATUS_B as usize] |= 0x20;
        cmos.ram[regs::SECONDS_ALARM as usize] = 0xC0;
        cmos.ram[regs::MINUTES_ALARM as usize] = 0xC0;
        cmos.ram[regs::HOURS_ALARM as usize] = 0xC0;

        // Both should be able to fire
        assert!(cmos.tick(1_000_000));
        let flags = cmos.peek_pending_flags();
        // At least periodic or alarm should fire
        assert!(flags & 0xE0 != 0); // Some interrupt flag set
    }

    #[test]
    fn test_disable_alarm_clears_irqf_only_when_no_other_pending_irqs() {
        let mut cmos = test_cmos(None);

        // AF + IRQF only: clearing alarm should clear IRQF too.
        cmos.pending_irq_flags.set(0xA0);
        cmos.disable_alarm();
        assert_eq!(cmos.peek_pending_flags(), 0x00);

        // PF + AF + IRQF: clearing alarm should preserve PF + IRQF.
        //
        // PIE must be set for that to hold (MDD-BUG-CRUCIBLE-00621): IRQF is the
        // ENABLE-QUALIFIED or of the flags, so a pending PF with PIE clear does
        // not hold the interrupt line up. This test previously left PIE clear and
        // still expected IRQF, which was the old "any latched flag implies IRQF"
        // rule. The property it pins — clearing the alarm must not drop IRQF
        // while another source still holds it — is unchanged.
        cmos.ram[regs::STATUS_B as usize] |= 0x40; // PIE
        cmos.pending_irq_flags.set(0xE0);
        cmos.disable_alarm();
        assert_eq!(cmos.peek_pending_flags(), 0xC0);

        // And with PIE clear the same pending PF must NOT hold IRQF up.
        cmos.ram[regs::STATUS_B as usize] &= !0x40;
        cmos.pending_irq_flags.set(0xE0);
        cmos.disable_alarm();
        assert_eq!(cmos.peek_pending_flags(), 0x40, "PF latched, but no IRQF");
    }

    /// MDD-BUG-CRUCIBLE-00621: PF latches with PIE clear, and raises no IRQ.
    ///
    /// Polling Status C with the interrupt disabled is a real and common idiom —
    /// the BIOS wait services use it. We used to drop the event entirely, so a
    /// poller saw nothing forever.
    #[test]
    fn periodic_flag_latches_with_pie_clear_and_raises_no_irq_crucible621() {
        let mut cmos = test_cmos(None);
        // PIE deliberately NOT set; the rate selector still runs the divider.
        assert_eq!(
            cmos.ram[regs::STATUS_B as usize] & 0x40,
            0,
            "PIE must be off"
        );

        assert!(
            !cmos.tick(1_000),
            "a disabled source must not assert the IRQ line"
        );
        let flags = cmos.peek_pending_flags();
        assert_eq!(flags & 0x40, 0x40, "PF latches regardless of PIE");
        assert_eq!(flags & 0x80, 0, "IRQF is qualified by PIE");

        // Status C still reports it, and still clears on read.
        cmos.write_index(regs::STATUS_C);
        assert_eq!(cmos.read_data() & 0x40, 0x40);
        cmos.write_index(regs::STATUS_C);
        assert_eq!(cmos.read_data(), 0x00);
    }

    /// MDD-BUG-CRUCIBLE-00621: the full flag/enable matrix for IRQF.
    ///
    /// IRQF is `(PF && PIE) || (AF && AIE) || (UF && UIE)`. Driven by setting the
    /// latched flags directly so each pair is exercised in isolation, including
    /// the mixed case where one source is enabled and another is not.
    #[test]
    fn irqf_is_the_enable_qualified_or_of_the_event_flags_crucible621() {
        const PF: u8 = 0x40;
        const AF: u8 = 0x20;
        const UF: u8 = 0x10;
        // Enables sit at the same bit positions in Status B.
        for (flags, enables, want_irqf) in [
            (PF, 0x00, false),
            (PF, PF, true),
            (AF, 0x00, false),
            (AF, AF, true),
            (UF, 0x00, false),
            (UF, UF, true),
            // Mixed: the pending source is not the enabled one.
            (PF, AF | UF, false),
            (AF, PF | UF, false),
            (UF, PF | AF, false),
            // Mixed: several pending, only one enabled.
            (PF | AF | UF, UF, true),
            (PF | AF, AF, true),
            (PF | AF | UF, 0x00, false),
        ] {
            let mut cmos = test_cmos(None);
            cmos.ram[regs::STATUS_B as usize] =
                (cmos.ram[regs::STATUS_B as usize] & !0x70) | enables;
            cmos.pending_irq_flags.set(flags);
            cmos.refresh_irqf();

            let got = cmos.peek_pending_flags();
            assert_eq!(
                got & 0x70,
                flags,
                "flags {flags:#04X} with enables {enables:#04X} must be preserved"
            );
            assert_eq!(
                (got & 0x80) != 0,
                want_irqf,
                "flags {flags:#04X} with enables {enables:#04X}: wrong IRQF"
            );
        }
    }

    /// MDD-BUG-CRUCIBLE-00621: enabling a source under a pending flag asserts
    /// IRQF, and disabling the last enabled source clears it — without ever
    /// disturbing the latched flags.
    #[test]
    fn status_b_write_recomputes_irqf_over_pending_flags_crucible621() {
        let mut cmos = test_cmos(None);
        cmos.pending_irq_flags.set(0x40); // PF latched, PIE clear, no IRQF
        assert_eq!(cmos.peek_pending_flags() & 0x80, 0);

        // Enable PIE through the port, as a guest would.
        cmos.write_index(regs::STATUS_B);
        let status_b = cmos.ram[regs::STATUS_B as usize];
        cmos.write_data(status_b | 0x40);
        let flags = cmos.peek_pending_flags();
        assert_eq!(
            flags & 0x40,
            0x40,
            "the latched flag must survive the write"
        );
        assert_eq!(flags & 0x80, 0x80, "enabling PIE asserts IRQF");

        // Disable it again: IRQF drops, PF stays latched until Status C is read.
        cmos.write_index(regs::STATUS_B);
        let status_b = cmos.ram[regs::STATUS_B as usize];
        cmos.write_data(status_b & !0x40);
        let flags = cmos.peek_pending_flags();
        assert_eq!(flags & 0x40, 0x40, "disabling must not clear the flag");
        assert_eq!(
            flags & 0x80,
            0,
            "disabling the last enabled source drops IRQF"
        );
    }

    #[test]
    fn test_has_pending_and_peek() {
        let mut cmos = test_cmos(None);
        assert!(!cmos.has_pending_interrupt());
        assert_eq!(cmos.peek_pending_flags(), 0);

        // Enable and fire periodic
        cmos.ram[regs::STATUS_B as usize] |= 0x40;
        cmos.tick(1000);

        assert!(cmos.has_pending_interrupt());
        assert_ne!(cmos.peek_pending_flags(), 0);

        // peek should not clear
        assert!(cmos.has_pending_interrupt());

        // read_data should clear
        cmos.write_index(regs::STATUS_C);
        cmos.read_data();
        assert!(!cmos.has_pending_interrupt());
    }

    #[test]
    fn test_periodic_rate_zero_returns_none() {
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_A as usize] = 0x20; // divider bits only, rate=0
        assert!(cmos.get_periodic_period_us().is_none());
    }

    #[test]
    fn test_periodic_rate_mapping_entries() {
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_A as usize] = 0x23; // rate=3
        assert_eq!(cmos.get_periodic_period_us(), Some(122));
        assert_eq!(cmos.get_periodic_frequency_hz(), Some(8192));
        assert_eq!(cmos.get_periodic_period_ratio_us(), Some((1_000_000, 8192)));
        cmos.ram[regs::STATUS_A as usize] = 0x2F; // rate=15
        assert_eq!(cmos.get_periodic_period_us(), Some(500_000));
    }

    #[test]
    fn test_periodic_event_tick_preserves_fractional_rtc_period() {
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] |= 0x40; // PIE

        // 1024Hz is 976.5625us. The event scheduler supplies the integer
        // sequence 976, 977, ... while carrying the fractional remainder.
        assert!(cmos.tick_periodic(976));
        cmos.write_index(regs::STATUS_C);
        cmos.read_data();
        assert!(cmos.tick_periodic(977));

        assert_eq!(cmos.diagnostic_snapshot().emulated_us, 1953);
        assert_eq!(cmos.peek_pending_flags() & 0x40, 0x40);
    }

    // =========================================================================
    // Additional coverage tests
    // =========================================================================

    #[test]
    fn test_read_day_of_week_via_port() {
        // Covers get_rtc_day_of_week() and the read_data match arm
        // 2024-05-12 is a Sunday (dow = 1)
        {
            let _guard = TestTimeGuard::set((0, 0, 0, 1, 12, 5, 2024));
            let mut cmos = test_cmos(None);
            cmos.write_index(regs::DAY_OF_WEEK);
            let dow = cmos.read_data();
            assert_eq!(dow, 1); // Sunday
        }

        // Wednesday (dow = 4) — 2024-05-15
        {
            let _guard = TestTimeGuard::set((0, 0, 0, 4, 15, 5, 2024));
            let mut cmos = test_cmos(None);
            cmos.write_index(regs::DAY_OF_WEEK);
            let dow = cmos.read_data();
            assert_eq!(dow, 4);
        }
    }

    #[test]
    fn test_write_minutes_register() {
        // Covers apply_time_register_write MINUTES arm
        let _guard = TestTimeGuard::set((0, 15, 10, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary

        // Write minutes = 45 (was 15)
        cmos.write_index(regs::MINUTES);
        cmos.write_data(45);

        // Read back
        cmos.write_index(regs::MINUTES);
        assert_eq!(cmos.read_data(), 45);
    }

    #[test]
    fn test_write_day_of_month_register() {
        // Covers apply_time_register_write DAY_OF_MONTH arm
        let _guard = TestTimeGuard::set((0, 0, 10, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary

        // Write day = 25 (was 12)
        cmos.write_index(regs::DAY_OF_MONTH);
        cmos.write_data(25);

        // Read back
        cmos.write_index(regs::DAY_OF_MONTH);
        assert_eq!(cmos.read_data(), 25);
    }

    #[test]
    fn test_write_year_register() {
        // Covers apply_time_register_write YEAR arm
        let _guard = TestTimeGuard::set((0, 0, 10, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary

        // Write year 2-digit = 99 (was 24, century stays 20 => year becomes 2099)
        cmos.write_index(regs::YEAR);
        cmos.write_data(99);

        // Read back year (2-digit)
        cmos.write_index(regs::YEAR);
        assert_eq!(cmos.read_data(), 99);

        // Century should still be 20
        cmos.write_index(regs::CENTURY);
        assert_eq!(cmos.read_data(), 20);
    }

    #[test]
    fn test_write_century_register() {
        // Covers apply_time_register_write CENTURY arm
        let _guard = TestTimeGuard::set((0, 0, 10, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary

        // Write century = 21 (was 20, year 2-digit stays 24 => year becomes 2124)
        cmos.write_index(regs::CENTURY);
        cmos.write_data(21);

        // Read back century
        cmos.write_index(regs::CENTURY);
        assert_eq!(cmos.read_data(), 21);

        // Year 2-digit should still be 24
        cmos.write_index(regs::YEAR);
        assert_eq!(cmos.read_data(), 24);
    }

    #[test]
    fn test_write_ps2_century_alias_updates_rtc_year() {
        let _guard = TestTimeGuard::set((0, 0, 10, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary

        cmos.write_index(regs::CENTURY_PS2);
        cmos.write_data(21);

        cmos.write_index(regs::CENTURY);
        assert_eq!(cmos.read_data(), 21);
        cmos.write_index(regs::CENTURY_PS2);
        assert_eq!(cmos.read_data(), 21);
        cmos.write_index(regs::YEAR);
        assert_eq!(cmos.read_data(), 24);
    }

    #[test]
    fn test_write_day_of_week_register_is_independent() {
        for mode in [0x02, 0x06] {
            let mut cmos = Mc146818::new(0);
            cmos.advance_clock(30 * 1_000_000);
            cmos.write_index(regs::STATUS_B);
            cmos.write_data(mode | 0x80);
            for weekday in 1..=7 {
                cmos.write_index(regs::DAY_OF_WEEK);
                cmos.write_data(weekday);
                assert_eq!(
                    cmos.read_data(),
                    weekday,
                    "weekday write must read back under SET"
                );
                assert_eq!(cmos.diagnostic_snapshot().adjusted_day_of_week, weekday);
            }
            // Out-of-range guest values leave the legal counter unchanged.
            for invalid in [0, 8] {
                cmos.write_data(invalid);
                assert_eq!(cmos.read_data(), 7, "illegal weekday write is ignored");
            }
            cmos.write_index(regs::SECONDS);
            assert_eq!(cmos.read_data(), if mode == 0x06 { 30 } else { 0x30 });
            assert_eq!(cmos.time_offset_secs(), 0);
        }
    }

    #[test]
    fn rtc_weekday_midnight_wrap_and_set_freeze() {
        let mut cmos = Mc146818::new(86_399);
        cmos.write_index(regs::DAY_OF_WEEK);
        cmos.write_data(7);
        cmos.advance_clock(1_000_000);
        assert_eq!(cmos.read_data(), 1, "weekday 7 wraps to 1 at midnight");
        cmos.advance_clock(10 * 86_400 * 1_000_000);
        assert_eq!(cmos.read_data(), 4, "all elapsed midnights advance weekday");
        cmos.write_index(regs::STATUS_B);
        cmos.write_data(0x82);
        cmos.advance_clock(2 * 86_400 * 1_000_000);
        cmos.write_index(regs::DAY_OF_WEEK);
        assert_eq!(
            cmos.read_data(),
            4,
            "SET freezes weekday across elapsed days"
        );
        cmos.write_index(regs::STATUS_B);
        cmos.write_data(0x02);
        cmos.advance_clock(86_400 * 1_000_000);
        cmos.write_index(regs::DAY_OF_WEEK);
        assert_eq!(cmos.read_data(), 5, "weekday resumes from held value");
    }

    #[test]
    fn rtc_weekday_date_writes_and_checkpoint_continuation() {
        for set in [false, true] {
            let mut cmos = Mc146818::new(0);
            cmos.write_index(regs::STATUS_B);
            cmos.write_data(if set { 0x82 } else { 0x02 });
            cmos.write_index(regs::DAY_OF_WEEK);
            cmos.write_data(7);
            for (reg, value) in [
                (regs::YEAR, 0x26),
                (regs::MONTH, 0x06),
                (regs::DAY_OF_MONTH, 0x11),
            ] {
                cmos.write_index(reg);
                cmos.write_data(value);
            }
            cmos.set_rtc_date_bcd(0x20, 0x25, 0x02, 0x14);
            cmos.set_rtc_time_bcd(0x23, 0x59, 0x59);
            cmos.write_index(regs::DAY_OF_WEEK);
            assert_eq!(
                cmos.read_data(),
                7,
                "date/time programming preserves weekday"
            );
            cmos.advance_clock(2 * 86_400 * 1_000_000);
            let mut restored = Mc146818::new(0);
            restored.restore_state(cmos.save_state());
            assert_eq!(
                restored.read_data(),
                cmos.read_data(),
                "checkpoint retains weekday"
            );
            for chip in [&mut cmos, &mut restored] {
                chip.write_index(regs::STATUS_B);
                chip.write_data(0x02);
                chip.advance_clock(1_000_000);
                chip.write_index(regs::DAY_OF_WEEK);
            }
            assert_eq!(
                restored.read_data(),
                if set { 1 } else { 3 },
                "restored weekday advances at next midnight"
            );
            assert_eq!(
                restored.read_data(),
                cmos.read_data(),
                "checkpoint preserves future weekday behavior"
            );
        }
    }

    #[test]
    fn test_get_rtc_time_bcd() {
        // Covers get_rtc_time_bcd()
        let _guard = TestTimeGuard::set((0, 0, 0, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);

        // Advance emulated time to 15:07:42
        cmos.advance_clock((15 * 3600 + 7 * 60 + 42) * 1_000_000);

        let (hours_bcd, minutes_bcd, seconds_bcd) = cmos.get_rtc_time_bcd();
        assert_eq!(hours_bcd, Mc146818::to_bcd(15)); // 0x15
        assert_eq!(minutes_bcd, Mc146818::to_bcd(7)); // 0x07
        assert_eq!(seconds_bcd, Mc146818::to_bcd(42)); // 0x42
    }

    #[test]
    fn test_get_rtc_date_bcd() {
        // Covers get_rtc_date_bcd()
        let _guard = TestTimeGuard::set((0, 0, 0, 3, 25, 12, 2024));
        let cmos = test_cmos(None);

        let (century_bcd, year_bcd, month_bcd, day_bcd) = cmos.get_rtc_date_bcd();
        assert_eq!(century_bcd, Mc146818::to_bcd(20)); // 0x20
        assert_eq!(year_bcd, Mc146818::to_bcd(24)); // 0x24
        assert_eq!(month_bcd, Mc146818::to_bcd(12)); // 0x12
        assert_eq!(day_bcd, Mc146818::to_bcd(25)); // 0x25
    }

    #[test]
    fn test_set_rtc_time_bcd() {
        // Covers set_rtc_time_bcd()
        let _guard = TestTimeGuard::set((0, 0, 0, 1, 1, 1, 2024));
        let mut cmos = test_cmos(None);

        // Set time to 15:07:42 in BCD
        cmos.set_rtc_time_bcd(
            Mc146818::to_bcd(15),
            Mc146818::to_bcd(7),
            Mc146818::to_bcd(42),
        );

        // Read back via get_rtc_time_bcd
        let (h, m, s) = cmos.get_rtc_time_bcd();
        assert_eq!(h, Mc146818::to_bcd(15));
        assert_eq!(m, Mc146818::to_bcd(7));
        assert_eq!(s, Mc146818::to_bcd(42));
    }

    #[test]
    fn test_set_rtc_date_bcd() {
        // Covers set_rtc_date_bcd()
        // Each call to apply_time_register_write computes offset against the
        // original system time, so multi-field writes don't round-trip exactly.
        // We verify the function executes correctly and modifies the offset.
        let _guard = TestTimeGuard::set((0, 0, 12, 3, 15, 6, 2024));
        let mut cmos = test_cmos(None);

        let before = cmos.time_offset_secs;
        cmos.set_rtc_date_bcd(
            Mc146818::to_bcd(20),
            Mc146818::to_bcd(25),
            Mc146818::to_bcd(6),
            Mc146818::to_bcd(15),
        );
        // The offset should have changed (year went from 24 to 25)
        assert_ne!(cmos.time_offset_secs, before);

        // Verify we can read back date values without panicking
        let (century, year, _month, _day) = cmos.get_rtc_date_bcd();
        // Century should still be 20
        assert_eq!(century, Mc146818::to_bcd(20));
        // Year should be 25
        assert_eq!(year, Mc146818::to_bcd(25));
    }

    #[test]
    fn test_periodic_rate_all_entries() {
        // Covers all periodic rate table entries
        let expected: [(u8, Option<u64>); 17] = [
            (0, None),
            (1, Some(3906)),
            (2, Some(7812)),
            (3, Some(122)),
            (4, Some(244)),
            (5, Some(488)),
            (6, Some(977)),
            (7, Some(1953)),
            (8, Some(3906)),
            (9, Some(7812)),
            (10, Some(15625)),
            (11, Some(31250)),
            (12, Some(62500)),
            (13, Some(125000)),
            (14, Some(250000)),
            (15, Some(500000)),
            (16, None), // _ => None (unreachable in practice, but we test the lookup)
        ];

        let mut cmos = test_cmos(None);
        for &(rate, expected_period) in &expected {
            // Rate occupies bits 3-0 of STATUS_A; preserve divider in bits 6-4
            cmos.ram[regs::STATUS_A as usize] = 0x20 | (rate & 0x0F);
            assert_eq!(
                cmos.get_periodic_period_us(),
                expected_period,
                "rate {} should map to {:?}",
                rate,
                expected_period
            );
        }
    }

    #[test]
    fn test_is_periodic_interrupt_enabled() {
        let mut cmos = test_cmos(None);
        // Default: PIE disabled
        assert!(!cmos.is_periodic_interrupt_enabled());

        // Enable PIE (bit 6 of Status B)
        cmos.ram[regs::STATUS_B as usize] |= 0x40;
        assert!(cmos.is_periodic_interrupt_enabled());

        // Disable PIE
        cmos.ram[regs::STATUS_B as usize] &= !0x40;
        assert!(!cmos.is_periodic_interrupt_enabled());
    }

    #[test]
    fn test_set_alarm_bcd() {
        // Covers set_alarm_bcd()
        let mut cmos = test_cmos(None);

        // Set alarm to 15:07:42 BCD
        cmos.set_alarm_bcd(
            Mc146818::to_bcd(15),
            Mc146818::to_bcd(7),
            Mc146818::to_bcd(42),
        );

        // Verify alarm registers
        assert_eq!(cmos.ram[regs::HOURS_ALARM as usize], Mc146818::to_bcd(15));
        assert_eq!(cmos.ram[regs::MINUTES_ALARM as usize], Mc146818::to_bcd(7));
        assert_eq!(cmos.ram[regs::SECONDS_ALARM as usize], Mc146818::to_bcd(42));

        // AIE should be enabled
        assert_ne!(cmos.ram[regs::STATUS_B as usize] & 0x20, 0);

        // alarm_fired_this_second should be reset
        assert!(!cmos.alarm_fired_this_second);

        // No pending alarm flag
        assert_eq!(cmos.peek_pending_flags() & 0x20, 0);
    }

    #[test]
    fn test_write_minutes_bcd_mode() {
        // Covers the BCD decode path of apply_time_register_write for MINUTES
        let _guard = TestTimeGuard::set((0, 15, 10, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        // BCD mode (default: STATUS_B bit 2 = 0)
        cmos.ram[regs::STATUS_B as usize] = 0x02; // 24-hour + BCD

        // Write minutes = 0x45 BCD (= 45 decimal)
        cmos.write_index(regs::MINUTES);
        cmos.write_data(0x45);

        // Read back in BCD mode
        cmos.write_index(regs::MINUTES);
        assert_eq!(cmos.read_data(), 0x45);
    }

    #[test]
    fn test_write_day_of_month_bcd_mode() {
        // Covers the BCD decode path for DAY_OF_MONTH
        let _guard = TestTimeGuard::set((0, 0, 10, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x02; // 24-hour + BCD

        // Write day = 0x25 BCD (= 25 decimal)
        cmos.write_index(regs::DAY_OF_MONTH);
        cmos.write_data(0x25);

        // Read back in BCD
        cmos.write_index(regs::DAY_OF_MONTH);
        assert_eq!(cmos.read_data(), 0x25);
    }

    #[test]
    fn test_write_year_bcd_mode() {
        // Covers the BCD decode path for YEAR register
        let _guard = TestTimeGuard::set((0, 0, 10, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x02; // 24-hour + BCD

        // Write year = 0x30 BCD (= 30 decimal, century 20 => 2030)
        cmos.write_index(regs::YEAR);
        cmos.write_data(0x30);

        // Read back in BCD
        cmos.write_index(regs::YEAR);
        assert_eq!(cmos.read_data(), 0x30);
    }

    #[test]
    fn test_write_century_bcd_mode() {
        // Covers the BCD decode path for CENTURY register
        let _guard = TestTimeGuard::set((0, 0, 10, 3, 12, 5, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x02; // 24-hour + BCD

        // Write century = 0x21 BCD (= 21 decimal, year 2-digit stays 24 => 2124)
        cmos.write_index(regs::CENTURY);
        cmos.write_data(0x21);

        // Read back in BCD
        cmos.write_index(regs::CENTURY);
        assert_eq!(cmos.read_data(), 0x21);
    }

    #[test]
    fn test_get_rtc_time_bcd_with_offset() {
        // Verify get_rtc_time_bcd respects time offset
        let _guard = TestTimeGuard::set((0, 0, 0, 1, 1, 1, 2024));
        let mut cmos = test_cmos(None);

        // Set time via register writes to create an offset
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary for writes
        cmos.write_index(regs::HOURS);
        cmos.write_data(23);
        cmos.write_index(regs::MINUTES);
        cmos.write_data(59);
        cmos.write_index(regs::SECONDS);
        cmos.write_data(58);

        // get_rtc_time_bcd always returns BCD regardless of STATUS_B
        let (h, m, s) = cmos.get_rtc_time_bcd();
        assert_eq!(h, Mc146818::to_bcd(23));
        assert_eq!(m, Mc146818::to_bcd(59));
        assert_eq!(s, Mc146818::to_bcd(58));
    }

    #[test]
    fn test_get_rtc_date_bcd_with_offset() {
        // Verify get_rtc_date_bcd respects time offset
        let _guard = TestTimeGuard::set((0, 0, 0, 1, 1, 1, 2024));
        let mut cmos = test_cmos(None);

        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary for writes
        cmos.write_index(regs::DAY_OF_MONTH);
        cmos.write_data(31);
        cmos.write_index(regs::MONTH);
        cmos.write_data(12);

        let (century, year, month, day) = cmos.get_rtc_date_bcd();
        assert_eq!(century, Mc146818::to_bcd(20));
        assert_eq!(year, Mc146818::to_bcd(24));
        assert_eq!(month, Mc146818::to_bcd(12));
        assert_eq!(day, Mc146818::to_bcd(31));
    }

    #[test]
    fn test_restored_time_offset_shifts_guest_visible_rtc() {
        // HLD §4.4 — a *restored* time_offset_secs (the value `load_nvram` applies
        // via `set_time_offset_secs`, src/emulator/nvram.rs) must actually shift the
        // guest-visible RTC, not merely round-trip through the getter. The sibling
        // tests drive the offset through register *writes*; this one drives it
        // through the exact setter the NVRAM load path calls, then reads the clock.
        // Tuple is (sec, min, hour, day_of_week, day, month, year); dow is ignored.
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024)); // 2024-06-15 12:00:00

        let mut cmos = test_cmos(None);
        // Baseline: no offset -> the pinned boot epoch.
        assert_eq!(
            cmos.get_rtc_date_bcd(),
            (
                Mc146818::to_bcd(20),
                Mc146818::to_bcd(24),
                Mc146818::to_bcd(6),
                Mc146818::to_bcd(15)
            ),
            "baseline date with zero offset"
        );
        assert_eq!(
            cmos.get_rtc_time_bcd(),
            (
                Mc146818::to_bcd(12),
                Mc146818::to_bcd(0),
                Mc146818::to_bcd(0)
            ),
            "baseline time with zero offset"
        );

        // Apply +1 day +1 hour through the NVRAM-restore setter.
        cmos.set_time_offset_secs(86_400 + 3_600);
        assert_eq!(
            cmos.time_offset_secs(),
            90_000,
            "the offset getter still round-trips the restored value"
        );

        // The guest-visible RTC must reflect the restored offset: 2024-06-16 13:00:00.
        assert_eq!(
            cmos.get_rtc_date_bcd(),
            (
                Mc146818::to_bcd(20),
                Mc146818::to_bcd(24),
                Mc146818::to_bcd(6),
                Mc146818::to_bcd(16)
            ),
            "a restored +1-day offset must advance the guest-visible RTC date"
        );
        assert_eq!(
            cmos.get_rtc_time_bcd(),
            (
                Mc146818::to_bcd(13),
                Mc146818::to_bcd(0),
                Mc146818::to_bcd(0)
            ),
            "a restored +1-hour offset must advance the guest-visible RTC time"
        );
    }

    #[test]
    fn test_set_alarm_bcd_clears_existing_alarm_flag() {
        // Verify set_alarm_bcd clears any previously pending alarm flag
        let mut cmos = test_cmos(None);
        // Simulate a pending alarm flag
        cmos.pending_irq_flags.set(0xA0); // IRQF + AF
        cmos.alarm_fired_this_second = true;

        cmos.set_alarm_bcd(0x12, 0x00, 0x00);

        // AF and IRQF should be cleared (no other interrupt sources)
        assert_eq!(cmos.peek_pending_flags() & 0x20, 0);
        assert!(!cmos.alarm_fired_this_second);
    }

    #[test]
    fn test_uip_accum_wraps_at_one_second() {
        // Verify UIP accumulator wraps properly in tick()
        let mut cmos = test_cmos(None);
        cmos.uip_accum_us = 999_900;

        // Tick 200us to push past 1_000_000
        cmos.tick(200);
        // Should have wrapped: 999_900 + 200 = 1_000_100 => 1_000_100 % 1_000_000 = 100
        assert_eq!(cmos.uip_accum_us, 100);
    }

    #[test]
    fn test_y2k_set_date_cross_century() {
        // Calendar rollover test: set date from 2026 to Dec 31, 1999
        // This crosses a century boundary and must round-trip correctly.
        let _guard = TestTimeGuard::set((0, 30, 23, 4, 26, 3, 2026));
        let mut cmos = test_cmos(None);

        cmos.set_rtc_date_bcd(
            Mc146818::to_bcd(19), // century = 19
            Mc146818::to_bcd(99), // year = 99
            Mc146818::to_bcd(12), // month = 12
            Mc146818::to_bcd(31), // day = 31
        );

        let (century, year, month, day) = cmos.get_rtc_date_bcd();
        assert_eq!(century, Mc146818::to_bcd(19), "century should be 19");
        assert_eq!(year, Mc146818::to_bcd(99), "year should be 99");
        assert_eq!(month, Mc146818::to_bcd(12), "month should be 12");
        assert_eq!(day, Mc146818::to_bcd(31), "day should be 31");
    }

    #[test]
    fn test_y2k_set_time_and_date() {
        // Full calendar rollover scenario: set time to 23:59:59, date to Dec 31, 1999
        let _guard = TestTimeGuard::set((0, 30, 23, 4, 26, 3, 2026));
        let mut cmos = test_cmos(None);

        // Set time before changing the date
        cmos.set_rtc_time_bcd(
            Mc146818::to_bcd(23),
            Mc146818::to_bcd(59),
            Mc146818::to_bcd(59),
        );
        // Then set date
        cmos.set_rtc_date_bcd(
            Mc146818::to_bcd(19),
            Mc146818::to_bcd(99),
            Mc146818::to_bcd(12),
            Mc146818::to_bcd(31),
        );

        let (h, m, s) = cmos.get_rtc_time_bcd();
        assert_eq!(h, Mc146818::to_bcd(23), "hours should be 23");
        assert_eq!(m, Mc146818::to_bcd(59), "minutes should be 59");
        assert_eq!(s, Mc146818::to_bcd(59), "seconds should be 59");

        let (century, year, month, day) = cmos.get_rtc_date_bcd();
        assert_eq!(century, Mc146818::to_bcd(19), "century should be 19");
        assert_eq!(year, Mc146818::to_bcd(99), "year should be 99");
        assert_eq!(month, Mc146818::to_bcd(12), "month should be 12");
        assert_eq!(day, Mc146818::to_bcd(31), "day should be 31");
    }

    #[test]
    fn test_y2k_rollover_with_tick() {
        // Simulate the full Calendar rollover test:
        // Set time to 23:59:59, date to Dec 31 1999, tick 2 seconds, verify rollover
        let _guard = TestTimeGuard::set((0, 30, 23, 4, 26, 3, 2026));
        let mut cmos = test_cmos(None);

        // Simulate some emulated time passing (as in a real emulator run)
        // Tick 5 seconds before the Y2K test
        for _ in 0..5120 {
            cmos.tick(977); // ~5 seconds at 1024 Hz
        }

        // Set the time and date
        cmos.set_rtc_time_bcd(
            Mc146818::to_bcd(23),
            Mc146818::to_bcd(59),
            Mc146818::to_bcd(59),
        );
        cmos.set_rtc_date_bcd(
            Mc146818::to_bcd(19),
            Mc146818::to_bcd(99),
            Mc146818::to_bcd(12),
            Mc146818::to_bcd(31),
        );

        // Verify initial state
        let (h, m, s) = cmos.get_rtc_time_bcd();
        assert_eq!(s, Mc146818::to_bcd(59), "initial seconds should be 59");
        assert_eq!(m, Mc146818::to_bcd(59), "initial minutes should be 59");
        assert_eq!(h, Mc146818::to_bcd(23), "initial hours should be 23");

        let (century, year, month, day) = cmos.get_rtc_date_bcd();
        assert_eq!(
            century,
            Mc146818::to_bcd(19),
            "initial century should be 19"
        );
        assert_eq!(year, Mc146818::to_bcd(99), "initial year should be 99");
        assert_eq!(month, Mc146818::to_bcd(12), "initial month should be 12");
        assert_eq!(day, Mc146818::to_bcd(31), "initial day should be 31");

        // Tick 2 seconds
        for _ in 0..2048 {
            cmos.tick(977);
        }

        // After 2 seconds, should have rolled over to Jan 1, 2000
        let (h, m, s) = cmos.get_rtc_time_bcd();
        let (century, year, month, day) = cmos.get_rtc_date_bcd();

        // Time should be 00:00:01 (1 second past midnight, ±1)
        assert_eq!(h, Mc146818::to_bcd(0), "hours should roll to 0");
        assert_eq!(m, Mc146818::to_bcd(0), "minutes should roll to 0");
        assert!(
            s <= Mc146818::to_bcd(3),
            "seconds should be near 0, got BCD {s:#x}"
        );

        // Date should be Jan 1, 2000
        assert_eq!(century, Mc146818::to_bcd(20), "century should roll to 20");
        assert_eq!(year, Mc146818::to_bcd(0), "year should roll to 00");
        assert_eq!(month, Mc146818::to_bcd(1), "month should roll to 1");
        assert_eq!(day, Mc146818::to_bcd(1), "day should roll to 1");
    }

    #[test]
    fn test_y2k_set_date_future_century() {
        // Setting date from 1985 to 2024 (forward cross-century)
        let _guard = TestTimeGuard::set((5, 4, 3, 2, 23, 11, 1985));
        let mut cmos = test_cmos(None);

        cmos.set_rtc_date_bcd(
            Mc146818::to_bcd(20),
            Mc146818::to_bcd(24),
            Mc146818::to_bcd(6),
            Mc146818::to_bcd(15),
        );

        let (century, year, month, day) = cmos.get_rtc_date_bcd();
        assert_eq!(century, Mc146818::to_bcd(20), "century should be 20");
        assert_eq!(year, Mc146818::to_bcd(24), "year should be 24");
        assert_eq!(month, Mc146818::to_bcd(6), "month should be 6");
        assert_eq!(day, Mc146818::to_bcd(15), "day should be 15");
    }

    // -------------------------------------------------------------------------
    // Firmware config block and RTC offset accessors
    // -------------------------------------------------------------------------

    #[test]
    fn test_time_offset_secs_roundtrip() {
        let mut cmos = test_cmos(None);
        assert_eq!(cmos.time_offset_secs(), 0);
        cmos.set_time_offset_secs(123_456_789);
        assert_eq!(cmos.time_offset_secs(), 123_456_789);
        cmos.set_time_offset_secs(-7);
        assert_eq!(cmos.time_offset_secs(), -7);
    }

    // =====================================================================
    // MDD-BUG-CRUCIBLE-00552 - Status-B SET must inhibit the update cycle.
    //
    // The MC146818A's SET bit aborts and inhibits update cycles so firmware
    // can read or program a coherent calendar image. Before this fix the bit
    // was stored through the generic register path and never consulted: the
    // calendar kept advancing, UIP kept cycling, and UF/AF kept latching.
    // =====================================================================

    /// Helper: assert SET via the guest-visible port path, not by poking `ram`,
    /// so the edge handling in `write_data` is what is under test.
    fn write_status_b(cmos: &mut Mc146818, value: u8) {
        cmos.write_index(regs::STATUS_B);
        cmos.write_data(value);
    }

    fn rtc_set_fields_order(
        initial: (u8, u8, u16),
        writes: &[(u8, u8)],
        expected: (u8, u8, u8, u8),
    ) {
        for binary in [false, true] {
            let (day, month, year) = initial;
            let epoch = Mc146818::time_to_epoch_secs(7, 8, 12, day, month, year) as u64;
            let mut cmos = test_cmos(Some(epoch));
            let mode = if binary { 0x06 } else { 0x02 };
            write_status_b(&mut cmos, mode | 0x80);
            let mut fields = [day, month, (year % 100) as u8, (year / 100) as u8];
            for &(reg, value) in writes {
                let encoded = if binary {
                    value
                } else {
                    Mc146818::to_bcd(value)
                };
                cmos.write_index(reg);
                cmos.write_data(encoded);
                assert_eq!(
                    read_reg(&mut cmos, reg),
                    encoded,
                    "independent field {reg} under SET"
                );
                fields[match reg {
                    regs::DAY_OF_MONTH => 0,
                    regs::MONTH => 1,
                    regs::YEAR => 2,
                    _ => 3,
                }] = value;
                for (field_reg, field) in
                    [regs::DAY_OF_MONTH, regs::MONTH, regs::YEAR, regs::CENTURY]
                        .into_iter()
                        .zip(fields)
                {
                    let encoded_field = if binary {
                        field
                    } else {
                        Mc146818::to_bcd(field)
                    };
                    assert_eq!(
                        read_reg(&mut cmos, field_reg),
                        encoded_field,
                        "write {reg} preserves independent field {field_reg}"
                    );
                }
                cmos.tick(2_000_000);
                write_status_b(&mut cmos, mode | 0x80);
                assert_eq!(
                    read_reg(&mut cmos, reg),
                    encoded,
                    "tick/repeated SET preserves field {reg}"
                );
            }
            assert_eq!(cmos.get_rtc_date_bcd(), expected);
            assert_eq!(cmos.get_rtc_time_bcd(), (0x12, 0x08, 0x07));
            write_status_b(&mut cmos, mode);
            assert_eq!(cmos.get_rtc_date_bcd(), expected, "release completed date");
            assert_eq!(
                cmos.get_rtc_time_bcd(),
                (0x12, 0x08, 0x07),
                "no SET catchup"
            );
            cmos.tick(1_000_000);
            assert_eq!(cmos.get_rtc_time_bcd(), (0x12, 0x08, 0x08));
        }
    }

    #[test]
    fn rtc_set_fields_day_before_month() {
        rtc_set_fields_order(
            (28, 2, 2026),
            &[(regs::DAY_OF_MONTH, 31), (regs::MONTH, 3)],
            (0x20, 0x26, 0x03, 0x31),
        );
    }

    #[test]
    fn rtc_set_fields_month_before_day() {
        rtc_set_fields_order(
            (31, 3, 2026),
            &[
                (regs::MONTH, 2),
                (regs::DAY_OF_MONTH, 28),
                (regs::MONTH, 3),
                (regs::DAY_OF_MONTH, 31),
            ],
            (0x20, 0x26, 0x03, 0x31),
        );
    }

    #[test]
    fn rtc_set_fields_leap_year_order() {
        rtc_set_fields_order(
            (28, 2, 2025),
            &[(regs::DAY_OF_MONTH, 29), (regs::YEAR, 24)],
            (0x20, 0x24, 0x02, 0x29),
        );
        rtc_set_fields_order(
            (29, 2, 2024),
            &[(regs::YEAR, 25), (regs::MONTH, 3)],
            (0x20, 0x25, 0x03, 0x29),
        );
    }

    #[test]
    fn rtc_set_fields_century_order() {
        rtc_set_fields_order(
            (29, 2, 2000),
            &[(regs::CENTURY_PS2, 21), (regs::YEAR, 4)],
            (0x21, 0x04, 0x02, 0x29),
        );
        rtc_set_fields_order(
            (28, 2, 2099),
            &[
                (regs::DAY_OF_MONTH, 29),
                (regs::YEAR, 0),
                (regs::CENTURY, 20),
            ],
            (0x20, 0x00, 0x02, 0x29),
        );
    }

    #[test]
    fn rtc_set_fields_midtransaction_checkpoint() {
        let epoch = Mc146818::time_to_epoch_secs(7, 8, 12, 28, 2, 2026) as u64;
        let mut cmos = test_cmos(Some(epoch));
        write_status_b(&mut cmos, 0x82);
        cmos.write_index(regs::DAY_OF_MONTH);
        cmos.write_data(0x31);
        cmos.tick(5_000_000);
        let mut restored = test_cmos(Some(epoch));
        restored.restore_state(cmos.save_state());
        assert_eq!(
            read_reg(&mut restored, regs::DAY_OF_MONTH),
            0x31,
            "checkpoint keeps impossible intermediate day"
        );
        restored.set_rtc_time_bcd(0x23, 0x59, 0x58);
        assert_eq!(
            read_reg(&mut restored, regs::DAY_OF_MONTH),
            0x31,
            "atomic time setter preserves partial date"
        );
        restored.write_index(regs::MONTH);
        restored.write_data(0x03);
        write_status_b(&mut restored, 0x02);
        assert_eq!(restored.get_rtc_date_bcd(), (0x20, 0x26, 0x03, 0x31));
        assert_eq!(restored.get_rtc_time_bcd(), (0x23, 0x59, 0x58));

        write_status_b(&mut restored, 0x82);
        restored.set_rtc_date_bcd(0x20, 0x24, 0x02, 0x29);
        assert_eq!(restored.get_rtc_date_bcd(), (0x20, 0x24, 0x02, 0x29));
        assert_eq!(
            restored.get_rtc_time_bcd(),
            (0x23, 0x59, 0x58),
            "atomic date setter preserves held time"
        );
        restored.tick(5_000_000);
        let mut missing_fields = restored.save_state();
        missing_fields.set_calendar_fields = None;
        let mut fallback = test_cmos(Some(epoch));
        fallback.restore_state(missing_fields);
        assert_eq!(fallback.get_rtc_date_bcd(), (0x20, 0x24, 0x02, 0x29));
        assert_eq!(
            fallback.get_rtc_time_bcd(),
            (0x23, 0x59, 0x58),
            "missing fields derive from captured hold"
        );

        let mut stale = restored.save_state();
        stale.ram[regs::STATUS_B as usize] &= !0x80;
        fallback.restore_state(stale);
        let before = fallback.get_rtc_seconds();
        fallback.tick(1_000_000);
        assert_ne!(
            fallback.get_rtc_seconds(),
            before,
            "stale held fields cannot freeze SET-clear clock"
        );

        // Releasing an unfinished calendar keeps the existing day-clamp policy.
        write_status_b(&mut restored, 0x82);
        restored.write_index(regs::YEAR);
        restored.write_data(0x25);
        assert_eq!(read_reg(&mut restored, regs::DAY_OF_MONTH), 0x29);
        write_status_b(&mut restored, 0x02);
        assert_eq!(restored.get_rtc_date_bcd(), (0x20, 0x25, 0x02, 0x28));
    }

    #[test]
    fn set_bit_holds_the_calendar_still() {
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // 24-hour + binary

        write_status_b(&mut cmos, 0x06 | 0x80); // assert SET
        let held = cmos.get_rtc_seconds();

        // Advance well past several second boundaries.
        for _ in 0..5 {
            cmos.tick(1_000_000);
        }

        assert_eq!(
            cmos.get_rtc_seconds(),
            held,
            "the calendar must not advance while SET is asserted"
        );
    }

    #[test]
    fn set_bit_forces_uip_inactive() {
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);

        // Land inside the UIP window: the bit asserts from 999_756us onward.
        cmos.tick(999_900);
        cmos.write_index(regs::STATUS_A);
        assert_ne!(
            cmos.read_data() & 0x80,
            0,
            "precondition: UIP is asserted at this phase, so the assertion below is meaningful"
        );

        write_status_b(&mut cmos, 0x80); // assert SET
        cmos.write_index(regs::STATUS_A);
        assert_eq!(
            cmos.read_data() & 0x80,
            0,
            "SET aborts the update cycle, so UIP must read inactive"
        );
    }

    #[test]
    fn set_bit_suppresses_update_ended_and_alarm_flags() {
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06;
        // Alarm on every second/minute/hour ("don't care" = 0xC0 in each).
        cmos.ram[regs::SECONDS_ALARM as usize] = 0xC0;
        cmos.ram[regs::MINUTES_ALARM as usize] = 0xC0;
        cmos.ram[regs::HOURS_ALARM as usize] = 0xC0;

        // Sync last_second, then clear whatever latched.
        cmos.tick(1_000_000);
        cmos.write_index(regs::STATUS_C);
        let _ = cmos.read_data();

        write_status_b(&mut cmos, 0x06 | 0x80); // assert SET
        for _ in 0..3 {
            cmos.tick(1_000_000);
        }

        cmos.write_index(regs::STATUS_C);
        let flags = cmos.read_data();
        assert_eq!(flags & 0x10, 0, "UF must not latch while SET is asserted");
        assert_eq!(flags & 0x20, 0, "AF must not latch while SET is asserted");
    }

    #[test]
    fn set_assertion_clears_uie_and_deasserts_pending_uf_irqf() {
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x12; // UIE + unrelated bit 1
        cmos.pending_irq_flags.set(0x10); // UF pending
        cmos.refresh_irqf();
        assert_eq!(
            cmos.peek_pending_flags() & 0x80,
            0x80,
            "precondition: pending UF with UIE asserts IRQF before SET"
        );

        cmos.write_index(regs::STATUS_B);
        let transition = cmos.write_data(0x92); // assert SET, preserving bit 1
        let observed = (cmos.read_data(), cmos.peek_pending_flags(), transition);

        assert_eq!(
            observed,
            (0x82, 0x10, Some(false)),
            "SET must clear UIE and IRQF while preserving UF and unrelated Status-B bits"
        );
    }

    #[test]
    fn set_bit_leaves_the_periodic_divider_running() {
        // The periodic divider is independent of SET on real silicon - this is
        // the control that stops the fix from over-reaching into PF.
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_A as usize] = 0x0F; // rate 15 -> 500ms

        write_status_b(&mut cmos, 0x80); // SET asserted, PIE off
        cmos.write_index(regs::STATUS_C);
        let _ = cmos.read_data(); // clear

        cmos.tick(600_000); // one periodic period

        cmos.write_index(regs::STATUS_C);
        assert_ne!(
            cmos.read_data() & 0x40,
            0,
            "PF must keep latching while SET is asserted"
        );
    }

    #[test]
    fn clearing_set_resumes_from_the_held_value_not_the_elapsed_one() {
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06;

        write_status_b(&mut cmos, 0x06 | 0x80);
        let held = cmos.get_rtc_seconds();
        let held_min = cmos.get_rtc_minutes();

        // Hold SET across a long stretch of emulated time.
        for _ in 0..10 {
            cmos.tick(1_000_000);
        }
        write_status_b(&mut cmos, 0x06); // clear SET

        assert_eq!(
            (cmos.get_rtc_seconds(), cmos.get_rtc_minutes()),
            (held, held_min),
            "on SET clear the calendar resumes from the held value, it does not jump forward"
        );

        // ...and it is running again.
        cmos.tick(1_000_000);
        assert_ne!(
            cmos.get_rtc_seconds(),
            held,
            "the calendar must resume advancing once SET is cleared"
        );
    }

    #[test]
    fn time_programmed_under_set_is_what_resumes() {
        // The bit's actual purpose: write SET, program the fields, clear SET.
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06; // binary mode

        write_status_b(&mut cmos, 0x06 | 0x80);
        cmos.write_index(regs::SECONDS);
        cmos.write_data(30);
        cmos.write_index(regs::MINUTES);
        cmos.write_data(45);

        assert_eq!(
            (cmos.get_rtc_seconds(), cmos.get_rtc_minutes()),
            (30, 45),
            "a field programmed under SET must read back immediately"
        );

        cmos.tick(2_000_000); // time passes, still held
        assert_eq!(cmos.get_rtc_seconds(), 30, "still held after the write");

        write_status_b(&mut cmos, 0x06);
        assert_eq!(
            (cmos.get_rtc_seconds(), cmos.get_rtc_minutes()),
            (30, 45),
            "clearing SET resumes from the PROGRAMMED value"
        );
    }

    #[test]
    fn a_redundant_set_write_does_not_re_freeze_later() {
        // `apply_set_bit_edge` must be idempotent: re-writing Status B with SET
        // already asserted must not move the hold forward.
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06;

        write_status_b(&mut cmos, 0x06 | 0x80);
        let held = cmos.set_frozen_secs.expect("SET asserted -> hold exists");

        cmos.tick(3_000_000);
        write_status_b(&mut cmos, 0x06 | 0x80); // redundant re-assert

        assert_eq!(
            cmos.set_frozen_secs,
            Some(held),
            "re-writing an already-asserted SET must not re-freeze at a later instant"
        );
    }

    /// The hold is taken when SET rises; raw time keeps running underneath it.
    /// A checkpoint captured some seconds later must restore the calendar the
    /// guest was READING, not the raw clock that had moved on — rebuilding it
    /// from the restored clock moved the held time forward by however long SET
    /// had been asserted (MDD-BUG-KIL-01039).
    #[test]
    fn a_checkpoint_taken_under_set_restores_the_time_it_held_not_the_raw_clock() {
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06;
        write_status_b(&mut cmos, 0x06 | 0x80);
        let held = cmos.get_rtc_seconds();

        // Raw time runs on under the hold, which is the whole point of SET.
        for _ in 0..5 {
            cmos.tick(1_000_000);
        }
        assert_eq!(
            cmos.get_rtc_seconds(),
            held,
            "SET still holds before capture"
        );

        let saved = cmos.save_state();
        let mut restored = test_cmos(None);
        restored.restore_state(saved);

        assert_eq!(
            restored.get_rtc_seconds(),
            held,
            "the restored hold is the captured calendar, not the raw clock five \
             seconds later"
        );

        // And it resumes from the held value when the guest clears SET, which is
        // the guest-visible consequence.
        write_status_b(&mut restored, 0x06);
        assert_eq!(
            restored.get_rtc_seconds(),
            held,
            "clearing SET resumes from the held calendar"
        );
    }

    /// `RtcCheckpointState`'s fields are public, so a checkpoint can assert SET
    /// while carrying no hold. MDD-BUG-CRUCIBLE-00552's guarantee — SET never
    /// resumes a RUNNING calendar — still applies to it.
    #[test]
    fn restoring_set_without_a_captured_hold_still_freezes_the_calendar() {
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06;
        write_status_b(&mut cmos, 0x06 | 0x80);
        cmos.tick(2_000_000);

        let mut saved = cmos.save_state();
        saved.set_frozen_secs = None;
        let mut restored = test_cmos(None);
        restored.restore_state(saved);

        let frozen = restored.get_rtc_seconds();
        for _ in 0..3 {
            restored.tick(1_000_000);
        }
        assert_eq!(
            restored.get_rtc_seconds(),
            frozen,
            "an asserted SET must hold the calendar even with no captured hold"
        );
    }

    /// The mirror case: a stale hold with SET clear must not hold the calendar.
    #[test]
    fn restoring_a_stale_hold_with_set_clear_leaves_the_clock_running() {
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06;

        let mut saved = cmos.save_state();
        saved.set_frozen_secs = Some(12_345);
        let mut restored = test_cmos(None);
        restored.restore_state(saved);

        let before = restored.get_rtc_seconds();
        for _ in 0..3 {
            restored.tick(1_000_000);
        }
        assert_ne!(
            restored.get_rtc_seconds(),
            before,
            "with SET clear the calendar must keep running"
        );
    }

    #[test]
    fn checkpoint_restore_rebuilds_the_set_hold() {
        // `RtcCheckpointState` deliberately does not carry the hold - it is
        // exactly derivable from the restored Status B plus the restored clock.
        // Without the rebuild, a checkpoint taken under SET would restore a
        // RUNNING calendar with SET asserted.
        let _guard = TestTimeGuard::set((0, 0, 12, 0, 15, 6, 2024));
        let mut cmos = test_cmos(None);
        cmos.ram[regs::STATUS_B as usize] = 0x06;
        write_status_b(&mut cmos, 0x06 | 0x80);
        let held = cmos.get_rtc_seconds();

        let saved = cmos.save_state();
        let mut restored = test_cmos(None);
        restored.restore_state(saved);

        assert!(
            restored.set_frozen_secs.is_some(),
            "the hold must be rebuilt from the restored SET bit"
        );
        for _ in 0..4 {
            restored.tick(1_000_000);
        }
        assert_eq!(
            restored.get_rtc_seconds(),
            held,
            "a restored SET must still hold the calendar still"
        );
    }

    #[test]
    fn century_register_map_rejects_rtc_bank_out_of_range_and_duplicates() {
        for mapping in [
            [Some(0x00), None],
            [Some(0x0a), None],
            [Some(0x0c), None],
            [Some(0x0d), None],
            [Some(0x80), None],
            [Some(0xff), None],
            [Some(0x32), Some(0x32)],
        ] {
            assert!(
                std::panic::catch_unwind(|| Mc146818::new_with_century_registers(0, mapping))
                    .is_err(),
                "invalid mapping accepted: {mapping:?}"
            );
        }
    }
}
