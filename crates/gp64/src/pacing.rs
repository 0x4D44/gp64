//! A monotonic clock is injected as elapsed `Duration`; no sleeping or wall clock lives here.
use std::time::Duration;

pub struct Pacer {
    rate: u64,
    last: Duration,
    target: u64,
    fraction: u128,
}
impl Pacer {
    pub fn new(rate: u64, now: Duration, cycle: u64) -> Self {
        assert!(rate > 0);
        Self {
            rate,
            last: now,
            target: cycle,
            fraction: 0,
        }
    }
    /// Budget for the next approximately 1 ms slice. Actual instruction cycles
    /// are supplied on the next call, so instruction overshoot cannot accumulate.
    /// Zero until half a slice is due: a caught-up loop would otherwise run
    /// slices of a few cycles, spending its time on per-slice overhead that
    /// the capacity estimate then mistakes for guest work.
    pub fn budget(&mut self, now: Duration, cycle: u64) -> u64 {
        let elapsed = now
            .saturating_sub(self.last)
            .min(Duration::from_millis(100));
        self.last = now.max(self.last);
        let scaled = elapsed.as_nanos() * u128::from(self.rate) + self.fraction;
        self.fraction = scaled % 1_000_000_000;
        self.target = self.target.saturating_add((scaled / 1_000_000_000) as u64);
        let cap = self.cap();
        if self.target.saturating_sub(cycle) > cap {
            self.target = cycle.saturating_add(cap);
        }
        let due = self.target.saturating_sub(cycle);
        let slice = (self.rate / 1000).max(1);
        if due < slice.div_ceil(2) {
            return 0;
        }
        due.min(slice)
    }
    /// Moves the pacing reference by `cycles`: positive runs the guest ahead
    /// of real time, negative holds it back. Run-ahead is limited so pending
    /// debt stays within 80% of the stall cap. Returns the shift applied.
    pub fn shift(&mut self, cycles: i64, cycle: u64) -> i64 {
        if cycles >= 0 {
            let room = (self.cap() * 4 / 5).saturating_sub(self.target.saturating_sub(cycle));
            let applied = cycles.unsigned_abs().min(room);
            self.target += applied;
            applied as i64
        } else {
            let applied = cycles.unsigned_abs().min(self.target);
            self.target -= applied;
            -(applied as i64)
        }
    }
    fn cap(&self) -> u64 {
        (self.rate / 10).max(1)
    }
}

/// Host output fill seen by the pacer at an epoch.
#[derive(Clone, Copy)]
pub struct Fill {
    /// Samples queued for the device.
    pub queued: usize,
    /// Fill the device side aims for.
    pub target: usize,
    /// Device sample rate.
    pub rate: u32,
    /// Total samples ever pushed; unchanged means the guest went quiet.
    pub pushed: u64,
    /// The device has begun playing.
    pub started: bool,
}

/// Each epoch moves the pacing reference by `(target - queued) / REFILL_TAU`
/// of the epoch's real time: up to 25% ahead to refill after a stall, up to
/// 10% behind to drain. This also absorbs device clock drift. Running
/// ahead needs measured headroom: never while the guest is behind real
/// time, and only the drift allowance while the host is near full load,
/// so it cannot deepen an overload. Guest time is cycles, so the shift is
/// never visible to the guest.
const REFILL_EPOCH: Duration = Duration::from_millis(16);
const REFILL_TAU_MS: f64 = 250.0;
const REFILL_AHEAD_MAX: f64 = 0.25;
const REFILL_BEHIND_MAX: f64 = 0.10;
const REFILL_MAX_LOAD: f64 = 0.97;
const REFILL_MIN_REALTIME: f64 = 0.98;
/// Run-ahead still allowed with the load gate closed: enough to track a
/// device clock 1000 ppm fast, too little to deepen an overload.
const REFILL_DRIFT_ALLOWANCE: f64 = 0.001;
const REFILL_EPOCHS: usize = 16;

/// Audio fill controller. Time is injected; nothing here reads a clock.
pub struct Refill {
    rate: u64,
    /// Fractional cycles of shift not yet applied.
    credit: f64,
    /// Recent epochs: (real ns, busy ns, guest cycles).
    epochs: [(u64, u64, u64); REFILL_EPOCHS],
    epoch_count: usize,
    epoch_next: usize,
    busy: Duration,
    last_at: Option<Duration>,
    last_cycle: u64,
    last_pushed: u64,
}
impl Refill {
    pub fn new(rate: u64) -> Self {
        assert!(rate > 0);
        Self {
            rate,
            credit: 0.0,
            epochs: [(0, 0, 0); REFILL_EPOCHS],
            epoch_count: 0,
            epoch_next: 0,
            busy: Duration::ZERO,
            last_at: None,
            last_cycle: 0,
            last_pushed: 0,
        }
    }
    /// Execution share of real time and guest speed relative to real time
    /// over the retained epochs.
    fn load_and_realtime(&self) -> (f64, f64) {
        let (mut real, mut busy, mut cycles) = (0u64, 0u64, 0u64);
        for &(r, b, c) in &self.epochs[..self.epoch_count] {
            real += r;
            busy += b;
            cycles += c;
        }
        let real = real.max(1) as f64;
        (
            busy as f64 / real,
            cycles as f64 * 1e9 / (self.rate as f64 * real),
        )
    }
    /// Call once per loop with the time spent executing since the last call.
    /// Returns the cycles to pass to [`Pacer::shift`] (zero between epochs).
    pub fn tick(&mut self, now: Duration, busy: Duration, cycle: u64, fill: Option<Fill>) -> i64 {
        self.busy += busy;
        let Some(previous) = self.last_at else {
            self.last_at = Some(now);
            self.last_cycle = cycle;
            self.busy = Duration::ZERO;
            self.last_pushed = fill.map_or(0, |fill| fill.pushed);
            return 0;
        };
        let real = now.saturating_sub(previous);
        if real < REFILL_EPOCH {
            return 0;
        }
        self.last_at = Some(now);
        self.epochs[self.epoch_next] = (
            real.as_nanos() as u64,
            std::mem::take(&mut self.busy).as_nanos() as u64,
            cycle.saturating_sub(self.last_cycle),
        );
        self.last_cycle = cycle;
        self.epoch_next = (self.epoch_next + 1) % REFILL_EPOCHS;
        self.epoch_count = (self.epoch_count + 1).min(REFILL_EPOCHS);
        let Some(fill) = fill else {
            self.credit = 0.0;
            return 0;
        };
        let pushed = fill.pushed != self.last_pushed;
        self.last_pushed = fill.pushed;
        if fill.target == 0 || fill.rate == 0 || !fill.started || !pushed {
            self.credit = 0.0;
            return 0;
        }
        let error_ms = (fill.target as f64 - fill.queued as f64) * 1000.0 / f64::from(fill.rate);
        let mut speed = (error_ms / REFILL_TAU_MS).clamp(-REFILL_BEHIND_MAX, REFILL_AHEAD_MAX);
        if speed > 0.0 {
            let (load, realtime) = self.load_and_realtime();
            if realtime < REFILL_MIN_REALTIME {
                // Behind real time: running ahead is impossible, and debt
                // added now would only be capped away later.
                speed = 0.0;
            } else if load >= REFILL_MAX_LOAD {
                // Keeping up without headroom: track device drift only.
                speed = speed.min(REFILL_DRIFT_ALLOWANCE);
            }
        }
        self.credit += speed * real.as_secs_f64() * self.rate as f64;
        let whole = self.credit.trunc();
        self.credit -= whole;
        whole as i64
    }
}
/// Sustainable CPU capacity, measured independently of intentional pacing sleeps.
/// Rates share a small common denominator so exact OPL2 phases remain representable
/// through arbitrarily many changes. The ladder is at most 12.5% apart.
const AUTO_EPOCH: Duration = Duration::from_millis(250);
const AUTO_STALL: Duration = Duration::from_millis(100);
const AUTO_MIN: u64 = 1_000_000;
const AUTO_MAX: u64 = 960_000_000;

fn sustainable_rate(capacity: u64) -> u64 {
    let mut best = AUTO_MIN;
    let mut unit = 125_000;
    while unit <= AUTO_MAX / 8 {
        for mantissa in 8..16 {
            let candidate = unit * mantissa;
            if candidate <= capacity && candidate <= AUTO_MAX {
                best = best.max(candidate);
            }
        }
        unit *= 2;
    }
    best
}

struct Capacity {
    last: Duration,
    since: Duration,
    cycle: u64,
    busy: Duration,
}
impl Capacity {
    fn new(now: Duration, cycle: u64) -> Self {
        Self {
            last: now,
            since: now,
            cycle,
            busy: Duration::ZERO,
        }
    }
    fn sample(
        &mut self,
        now: Duration,
        busy: Duration,
        cycle: u64,
        rate: u64,
        ceiling: u64,
    ) -> Option<u64> {
        let gap = now.saturating_sub(self.last);
        self.last = now;
        if gap.saturating_sub(busy) > AUTO_STALL {
            // OS suspension/debugger stops are not sustainable execution cost.
            *self = Self::new(now, cycle);
            return None;
        }
        self.busy += busy;
        if now.saturating_sub(self.since) < AUTO_EPOCH {
            return None;
        }
        let cycles = cycle.saturating_sub(self.cycle);
        let busy = self.busy;
        *self = Self::new(now, cycle);
        if cycles == 0 || busy.is_zero() {
            return None;
        }
        // Keep 15% free for presentation, host scheduling and transient work.
        let capacity =
            (u128::from(cycles) * 850_000_000 / busy.as_nanos()).min(u128::from(AUTO_MAX)) as u64;
        let next = sustainable_rate(capacity.min(rate.saturating_mul(5) / 4).min(ceiling));
        (next != rate).then_some(next)
    }
}

/// The emulation loop's clock: a [`Pacer`] steered by a [`Refill`], with
/// pause handling. Resuming rebases both, so the
/// guest never tries to catch up time it was not meant to run.
pub struct Pacing {
    rate: u64,
    pacer: Pacer,
    refill: Refill,
    paused: bool,
    capacity: Capacity,
    ceiling: u64,
}
impl Pacing {
    pub fn new(rate: u64, now: Duration, cycle: u64) -> Self {
        Self {
            rate,
            pacer: Pacer::new(rate, now, cycle),
            refill: Refill::new(rate),
            paused: false,
            capacity: Capacity::new(now, cycle),
            ceiling: AUTO_MAX,
        }
    }
    /// Caps the automatic rate, as a slower host would; for measurement.
    pub fn set_ceiling(&mut self, ceiling: u64) {
        self.ceiling = ceiling.clamp(AUTO_MIN, AUTO_MAX);
    }
    pub fn rate(&self) -> u64 {
        self.rate
    }
    pub fn paused(&self) -> bool {
        self.paused
    }
    pub fn set_paused(&mut self, paused: bool, now: Duration, cycle: u64) {
        if self.paused && !paused {
            self.rebase(now, cycle);
        }
        self.paused = paused;
    }
    fn rebase(&mut self, now: Duration, cycle: u64) {
        self.pacer = Pacer::new(self.rate, now, cycle);
        self.refill = Refill::new(self.rate);
        self.capacity = Capacity::new(now, cycle);
    }
    /// Cycles to run now; always zero while paused. `busy` is the time spent
    /// executing since the last call.
    pub fn budget(&mut self, now: Duration, busy: Duration, cycle: u64, fill: Option<Fill>) -> u64 {
        if self.paused {
            return 0;
        }
        if let Some(rate) = self
            .capacity
            .sample(now, busy, cycle, self.rate, self.ceiling)
        {
            self.rate = rate;
            self.rebase(now, cycle);
        }
        let shift = self.refill.tick(now, busy, cycle, fill);
        if shift != 0 {
            self.pacer.shift(shift, cycle);
        }
        self.pacer.budget(now, cycle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caught_up_pacer_waits_for_half_a_slice() {
        // 100 MHz: a slice is 1 ms, 100,000 cycles.
        let mut pacer = Pacer::new(100_000_000, Duration::ZERO, 0);
        assert_eq!(pacer.budget(Duration::from_micros(1), 0), 0);
        assert_eq!(pacer.budget(Duration::from_micros(400), 0), 0);
        assert_eq!(pacer.budget(Duration::from_micros(600), 0), 60_000);
        assert_eq!(pacer.budget(Duration::from_millis(3), 0), 100_000);
    }
    #[test]
    fn automatic_capacity_climbs_then_falls_with_workload() {
        let mut pacing = Pacing::new(20_000_000, Duration::ZERO, 0);
        let mut cycle = 0;
        // A 100 MHz executor initially has ample headroom.
        for ms in 1..=250 {
            cycle += 20_000;
            pacing.budget(
                Duration::from_millis(ms),
                Duration::from_micros(200),
                cycle,
                None,
            );
        }
        assert_eq!(pacing.rate(), 24_000_000);
        // Higher instruction/device cost now yields only 10 MHz capacity.
        for ms in 251..=500 {
            cycle += 10_000;
            pacing.budget(
                Duration::from_millis(ms),
                Duration::from_millis(1),
                cycle,
                None,
            );
        }
        assert_eq!(pacing.rate(), 8_000_000);
        // Changing rate rebases old debt; it cannot trigger a catch-up burst.
        assert_eq!(
            pacing.budget(Duration::from_millis(500), Duration::ZERO, cycle, None),
            0
        );
        assert_eq!(
            pacing.budget(Duration::from_millis(501), Duration::ZERO, cycle, None),
            8_000
        );
    }

    #[test]
    fn slow_execution_after_idle_downshifts_instead_of_being_ignored_as_a_stall() {
        let mut pacing = Pacing::new(AUTO_MAX, Duration::ZERO, 0);
        // One formerly cheap 1ms slice takes 300ms once the guest resumes work.
        pacing.budget(
            Duration::from_millis(300),
            Duration::from_millis(300),
            AUTO_MAX / 1000,
            None,
        );
        assert_eq!(pacing.rate(), 2_500_000);
    }

    #[test]
    fn automatic_capacity_ignores_stalls_pause_and_zero_work() {
        let rate = 20_000_000;
        let mut pacing = Pacing::new(rate, Duration::ZERO, 0);
        pacing.budget(Duration::from_secs(5), Duration::from_micros(10), 100, None);
        assert_eq!(pacing.rate(), rate);
        pacing.set_paused(true, Duration::from_secs(5), 100);
        pacing.budget(Duration::from_secs(10), Duration::from_secs(5), 100, None);
        pacing.set_paused(false, Duration::from_secs(10), 100);
        assert_eq!(
            pacing.budget(Duration::from_secs(10), Duration::ZERO, 100, None),
            0
        );
        for ms in 10_001..=11_000 {
            pacing.budget(Duration::from_millis(ms), Duration::ZERO, 100, None);
        }
        assert_eq!(pacing.rate(), rate);
    }

    #[test]
    fn automatic_rate_has_a_bounded_exact_phase_ladder() {
        assert_eq!(sustainable_rate(0), AUTO_MIN);
        assert_eq!(sustainable_rate(u64::MAX), AUTO_MAX);
        let mut controller = Capacity::new(Duration::ZERO, 0);
        // An almost free executor still increases gradually and stays bounded.
        let mut rate = 20_000_000;
        let mut cycle = 0;
        for ms in 1..=20_000 {
            cycle += rate / 1000;
            if let Some(next) = controller.sample(
                Duration::from_millis(ms),
                Duration::from_nanos(1),
                cycle,
                rate,
                AUTO_MAX,
            ) {
                assert!(next <= rate * 5 / 4);
                rate = next;
            }
        }
        assert_eq!(rate, AUTO_MAX);
    }

    /// Runs every granted cycle from `from` to `to` in 1 ms host steps.
    fn run(pacing: &mut Pacing, guest: &mut u64, from: u64, to: u64) -> u64 {
        let before = *guest;
        for ms in from..=to {
            loop {
                let budget = pacing.budget(Duration::from_millis(ms), Duration::ZERO, *guest, None);
                if budget == 0 {
                    break;
                }
                *guest += budget;
            }
        }
        *guest - before
    }

    #[test]
    fn pause_stops_cycles_and_resume_does_not_catch_up() {
        let rate = 20_000_000;
        let mut pacing = Pacing::new(rate, Duration::ZERO, 0);
        let mut guest = 0;
        run(&mut pacing, &mut guest, 1, 1000);
        assert_eq!(guest, rate);
        pacing.set_paused(true, Duration::from_millis(1000), guest);
        assert!(pacing.paused());
        assert_eq!(run(&mut pacing, &mut guest, 1001, 6000), 0);
        pacing.set_paused(false, Duration::from_millis(6000), guest);
        // The first host millisecond after a five-second pause runs one
        // millisecond of guest time, not a burst.
        assert_eq!(run(&mut pacing, &mut guest, 6001, 6001), rate / 1000);
        assert_eq!(run(&mut pacing, &mut guest, 6002, 7000), rate - rate / 1000);
        // Pausing twice, or resuming while running, changes nothing.
        pacing.set_paused(false, Duration::from_millis(9000), guest);
        assert_eq!(run(&mut pacing, &mut guest, 7001, 7001), rate / 1000);
    }

    #[test]
    fn different_rates_start_without_a_burst() {
        let mut pacing = Pacing::new(20_000_000, Duration::ZERO, 0);
        let mut guest = 0;
        run(&mut pacing, &mut guest, 1, 100);
        pacing = Pacing::new(40_000_000, Duration::from_millis(100), guest);
        assert_eq!(run(&mut pacing, &mut guest, 101, 200), 4_000_000);
        pacing = Pacing::new(8_000_000, Duration::from_millis(200), guest);
        assert_eq!(run(&mut pacing, &mut guest, 201, 300), 800_000);
    }
    #[test]
    fn guest_tracks_host_and_fractional_cycles() {
        for rate in [20_000_000, 1_234_567] {
            let mut clock = Pacer::new(rate, Duration::ZERO, 0);
            let mut guest = 0;
            for us in (100..=1_000_000).step_by(100) {
                guest += clock.budget(Duration::from_micros(us), guest);
            }
            assert_eq!(guest, rate);
        }
    }
    #[test]
    fn stall_debt_is_capped_and_cannot_run_away() {
        let mut clock = Pacer::new(20_000_000, Duration::ZERO, 0);
        let now = Duration::from_millis(500);
        let mut guest = 0;
        for _ in 0..1000 {
            guest += clock.budget(now, guest);
        }
        assert_eq!(guest, 2_000_000);
        assert_eq!(clock.budget(now, guest), 0);
        guest += clock.budget(now + Duration::from_millis(1), guest);
        assert_eq!(guest, 2_020_000);
    }
    #[test]
    fn overshoot_and_existing_debt_are_bounded() {
        let mut clock = Pacer::new(20_000_000, Duration::ZERO, 0);
        assert_eq!(clock.budget(Duration::from_millis(1), 0), 20_000);
        assert_eq!(clock.budget(Duration::from_millis(1), 20_003), 0);
        assert_eq!(clock.budget(Duration::from_millis(2), 20_003), 19_997);
        let mut guest = 20_003;
        let now = Duration::from_millis(502);
        for _ in 0..1000 {
            guest += clock.budget(now, guest);
        }
        assert_eq!(guest - 20_003, 2_000_000);
    }

    const CPU: u64 = 20_000_000;
    const HOST: u32 = 48_000;
    const TARGET: usize = 2_400;

    fn fill(queued: usize, pushed: u64) -> Option<Fill> {
        Some(Fill {
            queued,
            target: TARGET,
            rate: HOST,
            pushed,
            started: true,
        })
    }
    /// One closed epoch of 16 ms after a priming call; `speed` is the guest's
    /// cycles per real time and `load` its busy share.
    fn one_epoch(speed: f64, load: f64, queued: usize) -> i64 {
        one_epoch_to(speed, load, queued, TARGET)
    }
    fn one_epoch_to(speed: f64, load: f64, queued: usize, target: usize) -> i64 {
        let mut refill = Refill::new(CPU);
        assert_eq!(
            refill.tick(Duration::ZERO, Duration::ZERO, 0, fill(0, 0)),
            0
        );
        let real = Duration::from_millis(16);
        let cycles = (speed * CPU as f64 * real.as_secs_f64()) as u64;
        // Between epochs nothing moves.
        assert_eq!(
            refill.tick(Duration::from_millis(15), Duration::ZERO, 0, fill(0, 1)),
            0
        );
        let mut at = fill(queued, 2).unwrap();
        at.target = target;
        refill.tick(real, real.mul_f64(load), cycles, Some(at))
    }

    #[test]
    fn refill_is_clamped_ahead_and_behind() {
        let epoch_cycles = CPU as f64 * 0.016;
        // 1 s short: far beyond the 25% run-ahead clamp.
        let ahead = one_epoch_to(1.0, 0.5, 0, 48_000);
        assert!((ahead as f64 - 0.25 * epoch_cycles).abs() <= 1.0, "{ahead}");
        let behind = one_epoch(1.0, 0.5, 48_000);
        assert!(
            (behind as f64 + 0.10 * epoch_cycles).abs() <= 1.0,
            "{behind}"
        );
        // Proportional inside the clamp: 10 ms short over a 250 ms constant;
        // the full 50 ms target missing asks for 20%.
        let empty = one_epoch(1.0, 0.5, 0);
        assert!((empty as f64 - 0.20 * epoch_cycles).abs() <= 1.0, "{empty}");
        let small = one_epoch(1.0, 0.5, TARGET - 480);
        assert!((small as f64 - 0.04 * epoch_cycles).abs() <= 1.0, "{small}");
    }

    #[test]
    fn refill_is_gated_under_load_and_without_fresh_audio() {
        let epoch_cycles = CPU as f64 * 0.016;
        // Near full load only the drift allowance may run ahead.
        let loaded = one_epoch_to(1.0, 0.99, 0, 48_000);
        assert!(
            (loaded as f64 - 0.001 * epoch_cycles).abs() <= 1.0,
            "{loaded}"
        );
        // Behind real time: no run-ahead at all, but draining still works.
        assert_eq!(one_epoch(0.5, 0.5, 0), 0);
        assert!(one_epoch(0.5, 0.99, 48_000) < 0);
        // A quiet guest, a device that has not started, or no device.
        let mut refill = Refill::new(CPU);
        let mut now = Duration::ZERO;
        let mut step = |fill: Option<Fill>| {
            now += Duration::from_millis(16);
            refill.tick(
                now,
                Duration::ZERO,
                CPU * now.as_millis() as u64 / 1000,
                fill,
            )
        };
        step(fill(0, 7));
        assert_eq!(step(fill(0, 7)), 0);
        let mut silent = fill(0, 8).unwrap();
        silent.started = false;
        assert_eq!(step(Some(silent)), 0);
        assert_eq!(step(None), 0);
        assert!(step(fill(0, 9)) > 0);
    }

    #[test]
    fn shift_bounds_run_ahead_debt() {
        let mut clock = Pacer::new(CPU, Duration::ZERO, 0);
        assert_eq!(clock.shift(10_000_000, 0), 1_600_000);
        assert_eq!(clock.shift(1, 0), 0);
        assert_eq!(clock.shift(-600_000, 0), -600_000);
        assert_eq!(clock.shift(-2_000_000, 0), -1_000_000);
        assert_eq!(clock.budget(Duration::from_millis(1), 0), 20_000);
    }

    struct Run {
        underrun_ms: u32,
        min_ms: f64,
        max_ms: f64,
        final_ms: f64,
    }
    /// Simulates the loop in 1 ms steps: the guest runs every cycle the
    /// pacer grants, its audio enters the queue, and a device whose clock is
    /// off by `ppm` drains it.
    fn simulate(start_ms: f64, seconds: u64, ppm: f64, load: f64) -> Run {
        let host = f64::from(HOST);
        let mut clock = Pacer::new(CPU, Duration::ZERO, 0);
        let mut refill = Refill::new(CPU);
        let (mut guest, mut produced, mut consumed) = (0u64, start_ms * host / 1000.0, 0.0);
        let mut run = Run {
            underrun_ms: 0,
            min_ms: f64::MAX,
            max_ms: 0.0,
            final_ms: 0.0,
        };
        for ms in 1..=seconds * 1000 {
            let now = Duration::from_millis(ms);
            let queued = produced - consumed;
            let busy = Duration::from_secs_f64(load / 1000.0);
            let shift = refill.tick(now, busy, guest, fill(queued as usize, produced as u64));
            clock.shift(shift, guest);
            let before = guest;
            loop {
                let budget = clock.budget(now, guest);
                if budget == 0 {
                    break;
                }
                guest += budget;
            }
            produced += (guest - before) as f64 * host / CPU as f64;
            consumed += host * (1.0 + ppm * 1e-6) / 1000.0;
            if consumed > produced {
                run.underrun_ms += 1;
                consumed = produced;
            }
            let queued_ms = (produced - consumed) * 1000.0 / host;
            if ms > 5_000 {
                run.min_ms = run.min_ms.min(queued_ms);
                run.max_ms = run.max_ms.max(queued_ms);
            }
            run.final_ms = queued_ms;
        }
        run
    }

    #[test]
    fn refill_converges_to_target_fill() {
        for start_ms in [10.0, 200.0] {
            let run = simulate(start_ms, 10, 0.0, 0.5);
            assert_eq!(run.underrun_ms, 0, "start {start_ms}");
            assert!(
                run.min_ms > 49.0 && run.max_ms < 51.0,
                "start {start_ms}: {}..{}",
                run.min_ms,
                run.max_ms
            );
        }
    }

    #[test]
    fn drift_allowance_tracks_a_skewed_device_with_the_load_gate_closed() {
        for ppm in [500.0, -500.0] {
            let run = simulate(50.0, 600, ppm, 0.99);
            assert_eq!(run.underrun_ms, 0, "{ppm} ppm");
            assert!(
                (run.final_ms - 50.0).abs() < 2.0 && run.max_ms < 60.0,
                "{ppm} ppm: {}..{} final {}",
                run.min_ms,
                run.max_ms,
                run.final_ms
            );
        }
    }
}
