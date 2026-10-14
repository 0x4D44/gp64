//! This process's CPU usage for the window title, as a share of the whole
//! machine, the way Task Manager and Activity Monitor's total report it.
use std::time::{Duration, Instant};

/// How often the reading refreshes.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// Turns successive CPU-time readings into a usage percentage.
pub struct Meter {
    last: Option<(Instant, Duration)>,
    cores: u32,
}
impl Meter {
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self {
            last: None,
            cores: u32::try_from(cores).unwrap_or(u32::MAX),
        }
    }
    /// Takes a reading. The first call only sets the baseline.
    pub fn sample(&mut self, now: Instant) -> Option<u32> {
        let cpu = process_time()?;
        let usage = self
            .last
            .and_then(|(then, used)| usage(cpu.checked_sub(used)?, now - then, self.cores));
        self.last = Some((now, cpu));
        usage
    }
}

/// Rounds CPU time over wall time across `cores` to a whole percentage.
fn usage(cpu: Duration, wall: Duration, cores: u32) -> Option<u32> {
    if wall.is_zero() {
        return None;
    }
    let share = cpu.as_secs_f64() / wall.as_secs_f64() / f64::from(cores.max(1));
    // The cast is safe: the value is clamped to 0..=100.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((share * 100.0).round().clamp(0.0, 100.0) as u32)
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
    fn usage_is_a_share_of_every_core() {
        let second = Duration::from_secs(1);
        assert_eq!(usage(second, second, 4), Some(25));
        assert_eq!(usage(second * 2, second, 4), Some(50));
        assert_eq!(usage(Duration::ZERO, second, 4), Some(0));
        assert_eq!(usage(second * 9, second, 4), Some(100));
        assert_eq!(usage(second, Duration::ZERO, 4), None);
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
        assert!(busy > 0, "a spinning thread should register, got {busy}%");
    }
}
