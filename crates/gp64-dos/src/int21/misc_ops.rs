//! Miscellaneous operations: time/date, disk info, interrupts for INT 21h

use super::*;

impl Dos {
    /// Return DS pointing to DOS-owned media/DBCS metadata, translating it
    /// for a protected-mode DPMI caller as for other DOS data pointers.
    pub(crate) fn return_metadata_segment(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let (segment, protected_mode) = if self.dpmi_client_int21(cpu) {
            self.dpmi
                .data_selector_for_segment(cpu, memory, METADATA_SEG)
                .map_or((METADATA_SEG, false), |selector| (selector, true))
        } else {
            (METADATA_SEG, false)
        };
        cpu.return_data_seg_for_hle(
            Some(memory),
            gp64_cpu::DataSegReg::DS,
            segment,
            protected_mode,
        );
    }

    /// Get drive allocation data for INT 21h/1Bh and 1Ch
    /// Returns allocation info: sectors/cluster, bytes/sector, total clusters
    pub(crate) fn get_drive_allocation_data(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        drive_letter: char,
    ) {
        let drive_num = (drive_letter.to_ascii_uppercase() as u8).saturating_sub(b'A');
        if self.drives.contains_key(&drive_num) {
            // Use realistic values for a hard drive
            // AL = sectors per cluster
            // CX = bytes per sector
            // DX = total clusters
            // DS:BX = pointer to media ID byte (FAT ID)
            cpu.regs.set_al(8); // 8 sectors per cluster (4KB clusters)
            cpu.regs.set_cx(512); // 512 bytes per sector
            cpu.regs.set_dx(0xFFFF); // Max clusters (simulate large drive)

            let media_id_addr = Memory::seg_offset_to_linear(METADATA_SEG, MEDIA_ID_OFFSET);
            memory.write_u8(media_id_addr, 0xF8); // Hard disk media ID
            self.return_metadata_segment(cpu, memory);
            cpu.regs.set_bx(MEDIA_ID_OFFSET);
        } else {
            // Invalid drive
            cpu.regs.set_al(0xFF);
        }
    }

    /// Get InDOS Flag Address (INT 21h AH=34h)
    /// Returns: ES:BX = pointer to InDOS flag byte
    pub fn get_indos_flag_address(&self, cpu: &mut Cpu) {
        cpu.return_data_seg_for_hle(
            None::<&Memory>,
            gp64_cpu::DataSegReg::ES,
            INDOS_SEGMENT,
            false,
        );
        cpu.regs.set_bx(INDOS_OFFSET);
        // Note: The byte at ES:BX-1 is the critical error flag
    }

    /// Initialize InDOS flag to 0 during DOS startup
    pub fn init_indos_flag(memory: &mut Memory) {
        let indos_addr = Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET);
        let crit_addr = Memory::seg_offset_to_linear(INDOS_SEGMENT, CRITICAL_ERROR_OFFSET);
        memory.write_u8(indos_addr, 0);
        memory.write_u8(crit_addr, 0);
    }

    /// Called at start of INT 21h handler - increment InDOS counter
    /// TSRs check this flag to avoid calling DOS functions when DOS is busy
    pub fn enter_dos(memory: &mut Memory) {
        let addr = Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET);
        let val = memory.read_u8(addr);
        memory.write_u8(addr, val.saturating_add(1));
    }

    /// Called at end of INT 21h handler - decrement InDOS counter
    pub fn leave_dos(memory: &mut Memory) {
        let addr = Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET);
        let val = memory.read_u8(addr);
        memory.write_u8(addr, val.saturating_sub(1));
    }

    /// Convert a host `SystemTime` (a UTC instant) to DOS packed date and time.
    /// DOS file times are LOCAL time: the host zone's offset at that instant
    /// applies, as `localtime()` does (MDD-BUG-NMI-00208).
    pub fn systemtime_to_dos_datetime(time: std::time::SystemTime) -> (u16, u16) {
        Self::systemtime_to_dos_datetime_with(time, crate::host_local_time::host_utc_offset_at)
    }

    /// [`Self::systemtime_to_dos_datetime`] with an explicit zone-offset source.
    pub(crate) fn systemtime_to_dos_datetime_with(
        time: std::time::SystemTime,
        offset_at: impl Fn(i64) -> i64,
    ) -> (u16, u16) {
        Self::local_secs_to_dos_datetime(crate::host_local_time::systemtime_to_local_secs_with(
            time, offset_at,
        ))
    }

    /// Pack local civil seconds (a wall-clock reading counted from 1970-01-01)
    /// into DOS date and time. Pre-1970 readings clamp to 1970-01-01 00:00.
    /// DOS date: bits 15-9 = year-1980, bits 8-5 = month, bits 4-0 = day
    /// DOS time: bits 15-11 = hours, bits 10-5 = minutes, bits 4-0 = seconds/2
    pub(crate) fn local_secs_to_dos_datetime(local_secs: i64) -> (u16, u16) {
        let secs = local_secs.max(0);
        let days = secs / 86400;
        let day_secs = (secs % 86400) as u32;

        let (year, month, day) = Self::days_to_ymd(days);

        let hours = day_secs / 3600;
        let minutes = (day_secs % 3600) / 60;
        let seconds = day_secs % 60;

        // DOS year is offset from 1980, max 127 years
        let dos_year = if year >= 1980 { year - 1980 } else { 0 };
        let dos_date = ((dos_year.min(127) as u16) << 9) | ((month as u16) << 5) | (day as u16);
        let dos_time = ((hours as u16) << 11) | ((minutes as u16) << 5) | ((seconds / 2) as u16);

        (dos_date, dos_time)
    }

    /// Build DOS packed date+time from the emulated clock (BDA ticks + cached date).
    pub(crate) fn emulated_dos_datetime(&self, memory: &gp64_memory::Memory) -> (u16, u16) {
        use gp64_types::bda::BDA_TICK_COUNT_ADDR;
        let ticks = memory.read_u32(BDA_TICK_COUNT_ADDR);
        let (hours, minutes, seconds, _hundredths) = super::bda_ticks_to_time(ticks);
        let (year, month, day, _dow) = self.emulated_date;
        let dos_year = year.saturating_sub(1980);
        let dos_date = ((dos_year.min(127)) << 9) | ((month as u16) << 5) | (day as u16);
        let dos_time = ((hours as u16) << 11) | ((minutes as u16) << 5) | ((seconds / 2) as u16);
        (dos_date, dos_time)
    }

    /// Convert DOS packed (local) date and time to a host `SystemTime`, the
    /// inverse of [`Self::systemtime_to_dos_datetime`].
    pub(crate) fn dos_datetime_to_systemtime(date: u16, time: u16) -> std::time::SystemTime {
        Self::dos_datetime_to_systemtime_with(
            date,
            time,
            crate::host_local_time::host_utc_offset_at,
        )
    }

    /// [`Self::dos_datetime_to_systemtime`] with an explicit zone-offset source.
    pub(crate) fn dos_datetime_to_systemtime_with(
        date: u16,
        time: u16,
        offset_at: impl Fn(i64) -> i64,
    ) -> std::time::SystemTime {
        crate::host_local_time::local_secs_to_systemtime_with(
            Self::dos_datetime_to_local_secs(date, time),
            offset_at,
        )
    }

    /// Local civil seconds for DOS packed date and time.
    /// DOS date: bits 15-9 = year-1980, bits 8-5 = month, bits 4-0 = day
    /// DOS time: bits 15-11 = hours, bits 10-5 = minutes, bits 4-0 = seconds/2
    pub(crate) fn dos_datetime_to_local_secs(date: u16, time: u16) -> i64 {
        // Extract date components
        let year = 1980 + ((date >> 9) & 0x7F) as i32;
        let month = ((date >> 5) & 0x0F) as u32;
        let day = (date & 0x1F) as u32;

        // Extract time components
        let hours = ((time >> 11) & 0x1F) as i64;
        let minutes = ((time >> 5) & 0x3F) as i64;
        let seconds = ((time & 0x1F) * 2) as i64;

        let days = Self::ymd_to_days(year, month.max(1), day.max(1));
        days * 86400 + hours * 3600 + minutes * 60 + seconds
    }

    /// Advances the cached DOS date by the BIOS midnight rollovers counted in
    /// the BDA flag at 0040:0070 and clears it, as DOS does when it reads the
    /// clock for AH=2Ah/2Ch (the DOS model: INT 1Ah AH=00h, then `DOS_AddDays(AL)`).
    /// Without it the date stayed at its boot value (MDD-BUG-CRU-01394).
    pub(crate) fn consume_midnight_rollovers(&mut self, memory: &mut Memory) {
        let flag = gp64_types::bda::BDA_MIDNIGHT_FLAG_ADDR;
        let rollovers = memory.read_u8(flag);
        if rollovers == 0 {
            return;
        }
        memory.write_u8(flag, 0);
        let (year, month, day, _) = self.emulated_date;
        let days = Self::ymd_to_days(i32::from(year), u32::from(month), u32::from(day))
            + i64::from(rollovers);
        let (year, month, day) = Self::days_to_ymd(days);
        let (year, month, day) = (year as u16, month as u8, day as u8);
        self.emulated_date = (year, month, day, day_of_week(year, month, day));
    }

    /// Convert year/month/day to days since Unix epoch (1970-01-01)
    pub(crate) fn ymd_to_days(year: i32, month: u32, day: u32) -> i64 {
        // Algorithm: count days from year 0, then subtract days until 1970
        // Use Gregorian calendar rules

        let y = year as i64;
        let m = month as i64;
        let d = day as i64;

        // Adjust for months (treat Jan/Feb as months 13/14 of previous year)
        let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };

        // Days from year 0 to start of this year (Gregorian calendar)
        let days_from_year = y * 365 + y / 4 - y / 100 + y / 400;

        // Days from start of year to start of this month
        // expression: (153 * (m - 3) + 2) / 5 gives days from March 1
        let days_from_month = (153 * (m - 3) + 2) / 5;

        // Total days from year 0 to this date
        let total_days = days_from_year + days_from_month + d - 1;

        // Days from year 0 to 1970-01-01
        // 1970 adjusted: y=1969, m=13 (Jan=13 of 1969)
        let epoch_days = {
            let ey = 1969_i64;
            let em = 13_i64;
            let ed = 1_i64;
            let days_y = ey * 365 + ey / 4 - ey / 100 + ey / 400;
            let days_m = (153 * (em - 3) + 2) / 5;
            days_y + days_m + ed - 1
        };

        total_days - epoch_days
    }

    #[allow(dead_code)] // Reserved for future use
    pub(crate) fn dos_date_from_duration_result(
        result: Result<std::time::Duration, std::time::SystemTimeError>,
    ) -> (u16, u8, u8, u8) {
        match result {
            Ok(duration) => {
                let days = duration.as_secs() / 86400;
                let (year, month, day) = Self::days_to_ymd(days as i64);
                let day_of_week = ((days + 4) % 7) as u8; // Jan 1, 1970 was Thursday
                (year as u16, month as u8, day as u8, day_of_week)
            }
            Err(_) => (2025, 1, 1, 3),
        }
    }

    /// Convert days since Unix epoch to (year, month, day)
    /// Uses Howard Hinnant's algorithm for civil date conversion
    pub(crate) fn days_to_ymd(days: i64) -> (i32, u32, u32) {
        // Days from Unix epoch (1970-01-01) shifted to March 1, year 0
        let z = days + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = (z - era * 146097) as u32; // Day of era [0, 146096]
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // Year of era [0, 399]
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // Day of year [0, 365]
        let mp = (5 * doy + 2) / 153; // Month [0, 11]
        let d = doy - (153 * mp + 2) / 5 + 1; // Day [1, 31]
        let m = if mp < 10 { mp + 3 } else { mp - 9 }; // Month [1, 12]
        let y = if m <= 2 { y + 1 } else { y };
        (y as i32, m, d)
    }

    /// Trigger Ctrl-C interrupt (INT 23h)
    /// Called when Ctrl-C is detected during character input with ctrl_break_check enabled
    pub(crate) fn trigger_ctrl_c(cpu: &mut Cpu, memory: &mut Memory) {
        // INT 23h Ctrl-C handler
        // Push flags, CS, IP for interrupt return
        let flags = cpu.regs.flags.bits();
        cpu.regs.set_sp(cpu.regs.sp().wrapping_sub(2));
        memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), flags);
        cpu.regs.set_sp(cpu.regs.sp().wrapping_sub(2));
        memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), cpu.regs.cs);
        cpu.regs.set_sp(cpu.regs.sp().wrapping_sub(2));
        memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), cpu.regs.ip());

        // Load INT 23h vector
        let vector_addr = 0x23 * 4;
        let new_ip = memory.read_u16(vector_addr);
        let new_cs = memory.read_u16(vector_addr + 2);
        cpu.regs.set_ip(new_ip);
        cpu.regs.cs = new_cs;
        cpu.regs.cs_cache = gp64_cpu::SegmentCache::real_mode(new_cs);

        // Clear IF and TF as per interrupt semantics
        cpu.regs
            .flags
            .remove(gp64_cpu::Flags::INTERRUPT | gp64_cpu::Flags::TRAP);
    }
}
