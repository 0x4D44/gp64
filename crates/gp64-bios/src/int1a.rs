//! INT 1Ah - Time/clock services

use anyhow::Result;
use gp64_cpu::Cpu;
use gp64_hw::cmos::Cmos;
use gp64_hw::timer::Pit;
use gp64_memory::Memory;
use gp64_types::bda::{BDA_MIDNIGHT_FLAG_ADDR, BDA_TICK_COUNT_ADDR};
use gp64_types::gremlin_tape::{
    RtcTapeCursor, TapeOutcome, TapeRequest, TapeSource, UndiscriminatedTag,
};

use super::Bios;

impl Bios {
    /// INT 1Ah - Time services
    ///
    /// `pit` and `rtc_tape` are Gremlin Step 2 hookups. `pit` is only
    /// read for the current cumulative tick (used as the tape lookup
    /// key); `rtc_tape` is `None` on the production hot path.
    ///
    /// ### Tape-mode poisoning on coverage gap / mismatch (HLD Step 2)
    ///
    /// When a tape is active (`rtc_tape.is_some()`) and
    /// [`RtcTapeCursor::next_response`] returns `CoverageGap` or
    /// `Mismatch` for an `AH=00h/02h/04h` lookup, the handler MUST
    /// NOT fall through to the host-derived CMOS / BDA values: those
    /// paths transitively read `Cmos::startup_epoch_secs`, which is
    /// seeded from `SystemTime::now()` unless
    /// `--fixed-rtc-epoch` is also set. That would silently
    /// leak host wall-clock into a replay run — a direct violation
    /// of HLD §"RTC/port/boot-environment determinism": *"It never
    /// reads host hardware in tape mode."*
    ///
    /// Instead we substitute the documented poisoned-status pattern:
    /// `AL=0xFF` (AH=00h) and `CH=CL=DH=DL=0xFF` (AH=02h/04h), with
    /// `CF=1`. The poisoned `AL=0xFF` on AH=00h is treated as a
    /// poisoning indicator only — we do NOT clear the BDA midnight
    /// flag the way a real `AL != 0` would, because no real midnight
    /// rollover occurred.
    ///
    /// When the tape is NOT active (`rtc_tape.is_none()`), the
    /// pre-tape behaviour is preserved verbatim — host clock reads
    /// continue to flow through.
    #[allow(clippy::too_many_arguments)]
    pub fn int1a(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        cmos: &mut Cmos,
        pit: &Pit,
        rtc_tape: Option<&mut RtcTapeCursor>,
    ) -> Result<()> {
        let ah = cpu.regs.ah();
        tracing::debug!(
            ah = format_args!("{:02X}", ah),
            al = format_args!("{:02X}", cpu.regs.al()),
            ax = format_args!("{:04X}", cpu.regs.ax()),
            bx = format_args!("{:04X}", cpu.regs.bx()),
            cx = format_args!("{:04X}", cpu.regs.cx()),
            dx = format_args!("{:04X}", cpu.regs.dx()),
            di = format_args!("{:04X}", cpu.regs.di()),
            si = format_args!("{:04X}", cpu.regs.si()),
            "INT 1Ah entry"
        );

        // BIOS Data Area tick count at 0040:006C
        let tick_addr = BDA_TICK_COUNT_ADDR;

        // Gremlin Step 2: consult the tape cursor once at INT 1Ah
        // entry. The tape is consumed only for the AH=00h and
        // AH=02h-05h sub-functions; other AH values fall through
        // untouched. We compute the tick + expected source up-front
        // so the per-arm code can stay tidy.
        let current_pit_tick = pit.cumulative_ticks();
        let expected_request = match ah {
            0x00 => Some(TapeRequest::from(UndiscriminatedTag::Int1aAh00)),
            0x02..=0x05 => Some(TapeRequest::Int1aAh0205 { ah }),
            _ => None,
        };
        // `tape_active` records that a tape was supplied at INT 1Ah
        // entry. We use it on the AH=00h/02h/04h fallback arms to
        // distinguish "tape active but no entry for us" (poisoned —
        // must NOT touch host hardware) from "no tape at all"
        // (pre-tape behaviour, host reads allowed).
        let tape_active = rtc_tape.is_some();
        let tape_outcome = match (expected_request, rtc_tape) {
            (Some(request), Some(cursor)) => Some(cursor.next_response(request, current_pit_tick)),
            _ => None,
        };

        match ah {
            // Get system time - read from BIOS data area
            0x00 => {
                if let Some(TapeOutcome::Hit(TapeSource::Int1aAh00 { al, cx, dx })) =
                    tape_outcome.as_ref()
                {
                    // HLD §"Side-effects MUST be preserved": when
                    // AL=1 (i.e. midnight has rolled over), the BIOS
                    // clears the BDA midnight flag. We preserve that
                    // even under tape replay by basing the clear on
                    // the recorded AL value.
                    cpu.regs.set_al(*al);
                    if *al != 0 {
                        memory.write_u8(BDA_MIDNIGHT_FLAG_ADDR, 0);
                    }
                    cpu.regs.set_cx(*cx);
                    cpu.regs.set_dx(*dx);
                } else if tape_active {
                    // Tape active but cursor returned CoverageGap or
                    // Mismatch. Substitute poisoned values; do NOT
                    // read host BDA tick or clear midnight flag.
                    // Poisoned AL=0xFF is a status indicator, not a
                    // real midnight signal.
                    cpu.regs.set_al(0xFF);
                    cpu.regs.set_cx(0xFFFF);
                    cpu.regs.set_dx(0xFFFF);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else {
                    // No tape — pre-tape behaviour, host reads allowed.
                    let tick_count = memory.read_u32(tick_addr);
                    let midnight_flag = memory.read_u8(BDA_MIDNIGHT_FLAG_ADDR);
                    cpu.regs.set_al(u8::from(midnight_flag != 0));
                    memory.write_u8(BDA_MIDNIGHT_FLAG_ADDR, 0);
                    cpu.regs.set_cx((tick_count >> 16) as u16);
                    cpu.regs.set_dx(tick_count as u16);
                }
            }

            // Set system time - write to BIOS data area
            0x01 => {
                let tick_count = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
                memory.write_u32(tick_addr, tick_count);
                self.tick_count = tick_count;
                memory.write_u8(BDA_MIDNIGHT_FLAG_ADDR, 0);
            }

            // Get real-time clock time - read from CMOS RTC
            0x02 => {
                if let Some(TapeOutcome::Hit(TapeSource::Int1aAh0205 {
                    ah: _,
                    ch,
                    cl,
                    dh,
                    dl,
                    cf,
                })) = tape_outcome.as_ref()
                {
                    cpu.regs.set_ch(*ch);
                    cpu.regs.set_cl(*cl);
                    cpu.regs.set_dh(*dh);
                    cpu.regs.set_dl(*dl);
                    cpu.regs.flags.set(gp64_cpu::Flags::CARRY, *cf);
                } else if tape_active {
                    // Tape active but coverage gap / mismatch:
                    // substitute poisoned values; do NOT call
                    // `cmos.get_rtc_time_bcd()` (that path reads
                    // `startup_epoch_secs` which is host-clock-derived
                    // unless --fixed-rtc-epoch is also set).
                    cpu.regs.set_ch(0xFF);
                    cpu.regs.set_cl(0xFF);
                    cpu.regs.set_dh(0xFF);
                    cpu.regs.set_dl(0xFF);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else {
                    let (hours_bcd, mins_bcd, secs_bcd) = cmos.get_rtc_time_bcd();
                    cpu.regs.set_ch(hours_bcd);
                    cpu.regs.set_cl(mins_bcd);
                    cpu.regs.set_dh(secs_bcd);
                    cpu.regs.set_dl(0); // No DST
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                }
            }

            // Set real-time clock time - write through CMOS RTC
            0x03 => {
                // CH = hours (BCD), CL = minutes (BCD), DH = seconds (BCD), DL = DST flag
                let hours = cpu.regs.ch();
                let mins = cpu.regs.cl();
                let secs = cpu.regs.dh();
                let dst = cpu.regs.dl();
                tracing::debug!(
                    hours = format_args!("{:02X}", hours),
                    mins = format_args!("{:02X}", mins),
                    secs = format_args!("{:02X}", secs),
                    dst = dst,
                    "INT 1Ah AH=03h: Set RTC time"
                );
                if let Some(TapeOutcome::Hit(TapeSource::Int1aAh0205 { cf, .. })) =
                    tape_outcome.as_ref()
                {
                    cmos.set_rtc_time_bcd(hours, mins, secs);
                    cpu.regs.flags.set(gp64_cpu::Flags::CARRY, *cf);
                } else if tape_active {
                    // Tape active but coverage gap / mismatch: fail WITHOUT
                    // touching CMOS. Writing first and only then reading the
                    // tape outcome mutated RTC state a replay never authorised
                    // and still reported success (MDD-BUG-KILN-00277). The
                    // setters also preserve the unspecified half of the clock,
                    // so a stray set-time would have pulled the host-derived
                    // date into the run — the same leak the AH=02h/04h read
                    // arms above refuse.
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else {
                    cmos.set_rtc_time_bcd(hours, mins, secs);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                }
            }

            // Get real-time clock date - read from CMOS RTC
            0x04 => {
                if let Some(TapeOutcome::Hit(TapeSource::Int1aAh0205 {
                    ah: _,
                    ch,
                    cl,
                    dh,
                    dl,
                    cf,
                })) = tape_outcome.as_ref()
                {
                    cpu.regs.set_ch(*ch);
                    cpu.regs.set_cl(*cl);
                    cpu.regs.set_dh(*dh);
                    cpu.regs.set_dl(*dl);
                    cpu.regs.flags.set(gp64_cpu::Flags::CARRY, *cf);
                } else if tape_active {
                    // Tape active but coverage gap / mismatch:
                    // poisoned values. Do NOT call
                    // `cmos.get_rtc_date_bcd()` — see AH=02h above
                    // for the host-clock leak rationale.
                    cpu.regs.set_ch(0xFF);
                    cpu.regs.set_cl(0xFF);
                    cpu.regs.set_dh(0xFF);
                    cpu.regs.set_dl(0xFF);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else {
                    let (century_bcd, year_bcd, month_bcd, day_bcd) = cmos.get_rtc_date_bcd();
                    cpu.regs.set_ch(century_bcd);
                    cpu.regs.set_cl(year_bcd);
                    cpu.regs.set_dh(month_bcd);
                    cpu.regs.set_dl(day_bcd);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                }
            }

            // Set real-time clock date - write through CMOS RTC
            0x05 => {
                // CH = century (BCD), CL = year (BCD), DH = month (BCD), DL = day (BCD)
                let century_bcd = cpu.regs.ch();
                let year_bcd = cpu.regs.cl();
                let month_bcd = cpu.regs.dh();
                let day_bcd = cpu.regs.dl();
                tracing::debug!(
                    century = format_args!("{:02X}", century_bcd),
                    year = format_args!("{:02X}", year_bcd),
                    month = format_args!("{:02X}", month_bcd),
                    day = format_args!("{:02X}", day_bcd),
                    "INT 1Ah AH=05h: Set RTC date"
                );
                if let Some(TapeOutcome::Hit(TapeSource::Int1aAh0205 { cf, .. })) =
                    tape_outcome.as_ref()
                {
                    cmos.set_rtc_date_bcd(century_bcd, year_bcd, month_bcd, day_bcd);
                    cpu.regs.flags.set(gp64_cpu::Flags::CARRY, *cf);
                } else if tape_active {
                    // Tape active but coverage gap / mismatch: fail WITHOUT
                    // touching CMOS (MDD-BUG-KILN-00277). See AH=03h above.
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else {
                    cmos.set_rtc_date_bcd(century_bcd, year_bcd, month_bcd, day_bcd);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                }
            }

            // Set RTC alarm
            0x06 => {
                // CH = hours (BCD), CL = minutes (BCD), DH = seconds (BCD)
                // Program CMOS alarm registers and enable alarm interrupt.
                // BIOS alarm busy check: AIE already set means an
                // alarm is in use, so fail without changing the existing alarm.
                cmos.write_index(gp64_hw::cmos::regs::STATUS_B);
                if (cmos.read_data() & 0x20) != 0 {
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let hours = cpu.regs.ch();
                let mins = cpu.regs.cl();
                let secs = cpu.regs.dh();
                cmos.set_alarm_bcd(hours, mins, secs);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Reset/disable RTC alarm
            0x07 => {
                cmos.disable_alarm();
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Set RTC interval timer (stub)
            0x08 => {
                // This function would set up an RTC-based interval timer
                // In our emulation, we just acknowledge the request
                tracing::debug!("INT 1Ah 08h: Set RTC interval timer (stub)");
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Read RTC interval timer counter (stub)
            0x09 => {
                // This function would read the RTC interval timer counter
                // We return 0 (timer not active / no intervals elapsed)
                tracing::debug!("INT 1Ah 09h: Get RTC interval timer (stub)");
                cpu.regs.set_cx(0);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // PCI BIOS services
            _ => {
                tracing::warn!(ah = format_args!("{:02X}", ah), "Unhandled INT 1Ah");
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }
        }

        Ok(())
    }
}
