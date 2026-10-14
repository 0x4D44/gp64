//! This process's CPU usage for the window title, as a share of one core,
//! the way `top` reports it. The emulator is mostly one thread, so one core
//! is its limit; other threads can take the total past 100%.
use std::time::{Duration, Instant};

/// How often the reading refreshes.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// Turns successive CPU-time readings into a usage percentage.
pub struct Meter {
    last: Option<(Instant, Duration)>,
}
impl Meter {
    pub fn new() -> Self {
        Self { last: None }
    }
    /// Takes a reading. The first call only sets the baseline.
    pub fn sample(&mut self, now: Instant) -> Option<u32> {
        let cpu = process_time()?;
        let usage = self
            .last
            .and_then(|(then, used)| usage(cpu.checked_sub(used)?, now - then));
        self.last = Some((now, cpu));
        usage
    }
}

/// Rounds CPU time over wall time to a whole percentage of one core.
fn usage(cpu: Duration, wall: Duration) -> Option<u32> {
    if wall.is_zero() {
        return None;
    }
    let share = cpu.as_secs_f64() / wall.as_secs_f64();
    // The cast saturates; durations are never negative.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((share * 100.0).round() as u32)
}

/// User plus kernel time used by every thread of this process.
#[cfg(windows)]
fn process_time() -> Option<Duration> {
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        fn GetProcessTimes(
            process: *mut core::ffi::c_void,
            creation: *mut u64,
            exit: *mut u64,
            kernel: *mut u64,
            user: *mut u64,
        ) -> i32;
    }
    let (mut creation, mut exit, mut kernel, mut user) = (0, 0, 0, 0);
    // SAFETY: the pseudo-handle names this process; each out pointer is a
    // FILETIME-sized u64 that outlives the call.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    // FILETIME counts 100 ns units.
    (ok != 0).then(|| Duration::from_nanos((kernel + user).saturating_mul(100)))
}
/// User plus system time used by every thread of this process.
#[cfg(unix)]
fn process_time() -> Option<Duration> {
    unsafe extern "C" {
        fn clock() -> core::ffi::c_long;
    }
    // POSIX fixes CLOCKS_PER_SEC at one million.
    // SAFETY: clock takes no arguments and reads this process's CPU time.
    let ticks = unsafe { clock() };
    u64::try_from(ticks).ok().map(Duration::from_micros)
}
#[cfg(not(any(windows, unix)))]
fn process_time() -> Option<Duration> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_a_share_of_one_core() {
        let second = Duration::from_secs(1);
        assert_eq!(usage(second / 4, second), Some(25));
        assert_eq!(usage(second, second), Some(100));
        assert_eq!(usage(Duration::ZERO, second), Some(0));
        assert_eq!(usage(second * 3 / 2, second), Some(150));
        assert_eq!(usage(second, Duration::ZERO), None);
    }

    #[test]
    fn meter_sees_this_process_burn_cpu() {
        let mut meter = Meter::new();
        let start = Instant::now();
        assert_eq!(meter.sample(start), None);
        let mut spin = 0u64;
        while start.elapsed() < Duration::from_millis(300) {
            spin = std::hint::black_box(spin.wrapping_add(1));
        }
        let busy = meter.sample(Instant::now()).unwrap();
        assert!(
            busy >= 20,
            "a spinning thread should fill most of a core, got {busy}%"
        );
    }
}
