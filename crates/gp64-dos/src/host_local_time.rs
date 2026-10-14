//! Host local-time offsets for DOS file timestamps.
//!
//! DOS stores and reports file dates and times in LOCAL time, with no zone
//! field. Host file metadata arrives as UTC instants (`SystemTime`), so every
//! conversion between the two goes through the offset the host's time zone
//! had AT THAT INSTANT — what C `localtime()` reports, and what the DOS model uses
//! for host-directory mounts (MDD-BUG-NMI-00208). The emulated DOS clock is
//! seeded from the same host local time (`get_system_date`/`get_host_time`),
//! so file times and the DOS clock agree.
//!
//! This is host file metadata, not a guest-visible clock: nothing here reads
//! the current host time.
//!
//! The pure `_with` functions take the offset source as a parameter so tests
//! are independent of the machine's time zone.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds east of UTC in effect on the host at Unix instant `unix_secs`.
pub(crate) fn host_utc_offset_at(unix_secs: i64) -> i64 {
    #[cfg(windows)]
    {
        windows_offset_at(unix_secs).unwrap_or(0)
    }
    #[cfg(not(windows))]
    {
        let t = unix_secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::localtime_r(&t, &mut tm) };
        if result.is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    }
}

#[cfg(windows)]
fn windows_offset_at(unix_secs: i64) -> Option<i64> {
    #[repr(C)]
    #[derive(Default)]
    struct Systemtime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    extern "system" {
        fn SystemTimeToTzSpecificLocalTime(
            time_zone: *const std::ffi::c_void,
            universal: *const Systemtime,
            local: *mut Systemtime,
        ) -> i32;
    }

    let days = unix_secs.div_euclid(86_400);
    let day_secs = unix_secs.rem_euclid(86_400);
    let (year, month, day) = crate::Dos::days_to_ymd(days);
    // SYSTEMTIME covers 1601..=30827; outside that there is no zone data.
    let year = u16::try_from(year)
        .ok()
        .filter(|y| (1601..=30827).contains(y))?;
    let universal = Systemtime {
        year,
        month: month as u16,
        day_of_week: (days + 4).rem_euclid(7) as u16,
        day: day as u16,
        hour: (day_secs / 3_600) as u16,
        minute: (day_secs % 3_600 / 60) as u16,
        second: (day_secs % 60) as u16,
        milliseconds: 0,
    };
    let mut local = Systemtime::default();
    // SAFETY: both pointers reference live, correctly laid-out SYSTEMTIMEs; a
    // null zone pointer selects the active time zone.
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &universal, &mut local) };
    if ok == 0 {
        return None;
    }
    let local_secs = crate::Dos::ymd_to_days(
        i32::from(local.year),
        u32::from(local.month),
        u32::from(local.day),
    ) * 86_400
        + i64::from(local.hour) * 3_600
        + i64::from(local.minute) * 60
        + i64::from(local.second);
    Some(local_secs - unix_secs)
}

/// Local civil seconds (a local wall-clock reading counted from 1970-01-01
/// 00:00) for the UTC instant `unix_secs`.
pub(crate) fn utc_to_local_with(unix_secs: i64, offset_at: impl Fn(i64) -> i64) -> i64 {
    unix_secs + offset_at(unix_secs)
}

/// The UTC instant for local civil seconds, resolved the way `mktime()` does:
/// the offset is the one in effect at the resulting instant. A reading inside
/// a spring-forward gap lands one gap-width later; an ambiguous fall-back
/// reading resolves to the later (standard-time) instant.
pub(crate) fn local_to_utc_with(local_secs: i64, offset_at: impl Fn(i64) -> i64) -> i64 {
    let first_guess = local_secs - offset_at(local_secs);
    local_secs - offset_at(first_guess)
}

/// Whole seconds since the Unix epoch, flooring pre-epoch instants.
pub(crate) fn unix_secs(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => {
            let d = e.duration();
            -(d.as_secs() as i64) - i64::from(d.subsec_nanos() > 0)
        }
    }
}

/// Local civil seconds for a host `SystemTime`.
pub(crate) fn systemtime_to_local_secs_with(
    time: SystemTime,
    offset_at: impl Fn(i64) -> i64,
) -> i64 {
    utc_to_local_with(unix_secs(time), offset_at)
}

/// The host `SystemTime` for local civil seconds; pre-epoch results clamp to
/// the epoch, as the DOS conversions always have.
pub(crate) fn local_secs_to_systemtime_with(
    local_secs: i64,
    offset_at: impl Fn(i64) -> i64,
) -> SystemTime {
    let secs = local_to_utc_with(local_secs, offset_at);
    UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64)
}

fn shift(time: SystemTime, delta_secs: i64) -> SystemTime {
    let magnitude = Duration::from_secs(delta_secs.unsigned_abs());
    let shifted = if delta_secs >= 0 {
        time.checked_add(magnitude)
    } else {
        time.checked_sub(magnitude)
    };
    shifted.unwrap_or(time)
}

/// Re-express a UTC `SystemTime` as a local wall-clock reading, keeping its
/// sub-second part (for the LFN hundredths fields).
pub(crate) fn systemtime_utc_to_local_with(
    time: SystemTime,
    offset_at: impl Fn(i64) -> i64,
) -> SystemTime {
    shift(time, offset_at(unix_secs(time)))
}

/// Inverse of [`systemtime_utc_to_local_with`].
pub(crate) fn systemtime_local_to_utc_with(
    time: SystemTime,
    offset_at: impl Fn(i64) -> i64,
) -> SystemTime {
    let local = unix_secs(time);
    shift(time, local_to_utc_with(local, offset_at) - local)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-03-29T01:00:00Z: the UK moves from GMT (+0) to BST (+1h).
    const UK_SPRING: i64 = 1_774_746_000;
    /// 2026-10-25T01:00:00Z: the UK moves from BST (+1h) back to GMT.
    const UK_AUTUMN: i64 = 1_792_890_000;

    fn uk_offset(unix_secs: i64) -> i64 {
        if (UK_SPRING..UK_AUTUMN).contains(&unix_secs) {
            3_600
        } else {
            0
        }
    }

    #[test]
    fn utc_to_local_uses_the_offset_in_effect_at_the_instant() {
        assert_eq!(utc_to_local_with(UK_SPRING - 1, uk_offset), UK_SPRING - 1);
        assert_eq!(utc_to_local_with(UK_SPRING, uk_offset), UK_SPRING + 3_600);
        assert_eq!(
            utc_to_local_with(UK_AUTUMN - 1, uk_offset),
            UK_AUTUMN + 3_599
        );
        assert_eq!(utc_to_local_with(UK_AUTUMN, uk_offset), UK_AUTUMN);
    }

    #[test]
    fn local_to_utc_inverts_every_unambiguous_reading_across_both_transitions() {
        // Every instant within two hours of each transition, except the hour
        // of summer time whose local readings repeat after the fall-back.
        for centre in [UK_SPRING, UK_AUTUMN] {
            for utc in (centre - 7_200..centre + 7_200).step_by(60) {
                if (UK_AUTUMN - 3_600..UK_AUTUMN).contains(&utc) {
                    continue;
                }
                let local = utc_to_local_with(utc, uk_offset);
                assert_eq!(local_to_utc_with(local, uk_offset), utc, "utc {utc}");
            }
        }
        // Negative and large offsets resolve too (UTC-5 -> -4, UTC+10 -> +11).
        for (before, after) in [(-18_000, -14_400), (36_000, 39_600)] {
            let offset = |t: i64| if t < UK_SPRING { before } else { after };
            for utc in (UK_SPRING - 7_200..UK_SPRING + 7_200).step_by(60) {
                let local = utc_to_local_with(utc, offset);
                assert_eq!(local_to_utc_with(local, offset), utc, "utc {utc}");
            }
        }
    }

    #[test]
    fn local_to_utc_resolves_gap_and_overlap_readings_like_mktime() {
        // 01:30 local on the spring day does not exist: it lands on 02:30 BST.
        assert_eq!(
            local_to_utc_with(UK_SPRING + 1_800, uk_offset),
            UK_SPRING + 1_800
        );
        // 01:30 local on the autumn day happens twice: the later (GMT) one wins.
        assert_eq!(
            local_to_utc_with(UK_AUTUMN + 1_800, uk_offset),
            UK_AUTUMN + 1_800
        );
    }

    /// The OS query answers with a real zone offset: within +-14 h and a
    /// whole number of quarter hours, never a garbage field.
    #[test]
    fn host_offset_is_a_plausible_zone_offset() {
        for instant in [0, UK_SPRING, UK_AUTUMN, 4_102_444_800] {
            let offset = host_utc_offset_at(instant);
            assert!(offset.abs() <= 14 * 3_600, "offset {offset}");
            assert_eq!(offset % 900, 0, "offset {offset}");
        }
    }

    #[test]
    fn systemtime_shift_round_trips_and_keeps_sub_seconds() {
        let utc = UNIX_EPOCH + Duration::new(UK_SPRING as u64 + 86_400, 250_000_000);
        let local = systemtime_utc_to_local_with(utc, uk_offset);
        assert_eq!(
            local,
            UNIX_EPOCH + Duration::new(UK_SPRING as u64 + 90_000, 250_000_000)
        );
        assert_eq!(systemtime_local_to_utc_with(local, uk_offset), utc);
    }
}
