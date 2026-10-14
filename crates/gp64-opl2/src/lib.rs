//! YM3812 melodic synthesis, cycle-driven timers and timestamped chip-rate audio.
//! Percussion mode (0xBD bit 5) is stored but not synthesised.

mod synth;

pub use synth::{CHANNELS, OPERATORS, SAMPLE_RATE};

/// Exact fractional tick phase, including across CPU clock changes.
#[derive(Clone, Copy, Debug)]
struct Phase {
    numerator: u128,
    denominator: u128,
}

impl Phase {
    const fn new() -> Self {
        Self {
            numerator: 0,
            denominator: 1,
        }
    }

    fn advance(&mut self, cycles: u64, rate: u64, clock: u64) -> u128 {
        let clock = u128::from(clock);
        let common = gcd(self.denominator, clock);
        let scale = clock / common;
        let denominator = self
            .denominator
            .checked_mul(scale)
            .expect("clock phase denominator overflow");
        let ticks = u128::from(cycles) * u128::from(rate);
        let numerator = self
            .numerator
            .checked_mul(scale)
            .and_then(|n| {
                (ticks % clock)
                    .checked_mul(self.denominator / common)
                    .and_then(|t| n.checked_add(t))
            })
            .expect("clock phase numerator overflow");
        let whole = ticks / clock + numerator / denominator;
        let remainder = numerator % denominator;
        let reduction = gcd(remainder, denominator);
        self.numerator = remainder / reduction;
        self.denominator = denominator / reduction;
        whole
    }
}

const fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

#[derive(Clone, Copy, Debug)]
struct Timer {
    preset: u8,
    count: u16,
    started: bool,
    masked: bool,
    flag: bool,
    phase: Phase,
}

impl Timer {
    const fn new() -> Self {
        Self {
            preset: 0,
            count: 0,
            started: false,
            masked: false,
            flag: false,
            phase: Phase::new(),
        }
    }

    fn advance(&mut self, cycles: u64, rate: u64, clock: u64) {
        if !self.started {
            return;
        }
        let ticks = self.phase.advance(cycles, rate, clock);
        let remaining = u128::from(256 - self.count);
        if ticks < remaining {
            self.count += ticks as u16;
        } else {
            self.flag |= !self.masked;
            let period = 256 - u128::from(self.preset);
            self.count = u16::from(self.preset) + ((ticks - remaining) % period) as u16;
        }
    }

    fn control(&mut self, started: bool, masked: bool) {
        // MASK forces the corresponding YM3812 flag to zero without stopping it.
        self.masked = masked;
        if masked {
            self.flag = false;
        }
        // Only a stopped-to-started transition loads the preset and resets phase.
        if started && !self.started {
            self.count = u16::from(self.preset);
            self.phase = Phase::new();
        }
        self.started = started;
    }
}

/// YM3812 with a monotonic CPU cycle timeline and a drainable PCM queue.
///
/// All accesses advance timers and render complete samples before applying a
/// register change. The frontend drains the queue and resamples at its host rate.
/// Cycle timestamps must never decrease; a zero CPU clock is invalid.
/// Fractions are exact; an unrepresentable rational phase after repeated changes
/// between unrelated clock rates panics rather than silently losing precision.
#[derive(Clone, Debug)]
pub struct Opl2 {
    synth: synth::Synth,
    timers: [Timer; 2],
    clock: u64,
    cycle: u64,
    audio_phase: Phase,
    address: u8,
    samples: Vec<i16>,
}

impl Default for Opl2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Opl2 {
    /// Creates a reset YM3812 on a 20 MHz CPU timeline.
    pub fn new() -> Self {
        Self::new_with_clock(20_000_000)
    }

    /// Creates a reset YM3812 with the given CPU cycles per emulated second.
    pub fn new_with_clock(cycles_per_second: u64) -> Self {
        assert!(cycles_per_second != 0, "CPU clock must be nonzero");
        Self {
            synth: synth::Synth::new(),
            timers: [Timer::new(); 2],
            clock: cycles_per_second,
            cycle: 0,
            audio_phase: Phase::new(),
            address: 0,
            samples: Vec::new(),
        }
    }

    /// Advances through `cycle` with the previous clock, then changes the clock.
    /// Fractional timer ticks and audio samples survive the change exactly.
    pub fn set_clock(&mut self, cycle: u64, cycles_per_second: u64) {
        assert!(cycles_per_second != 0, "CPU clock must be nonzero");
        self.advance_to(cycle);
        self.clock = cycles_per_second;
    }

    /// Advances the timers and queues every complete chip sample through `cycle`.
    /// Repeating a timestamp has no effect. Earlier timestamps panic.
    pub fn advance_to(&mut self, cycle: u64) {
        let elapsed = cycle
            .checked_sub(self.cycle)
            .expect("OPL2 cycle timestamps must be monotonic");
        self.timers[0].advance(elapsed, 12_500, self.clock);
        self.timers[1].advance(elapsed, 3_125, self.clock);
        let count = self
            .audio_phase
            .advance(elapsed, u64::from(SAMPLE_RATE), self.clock);
        for _ in 0..count {
            self.samples.push(self.synth.next_sample());
        }
        self.cycle = cycle;
    }

    /// Absolute cycle at which the next complete chip sample is due.
    /// This projects the exact phase onto the CPU clock, including a runtime rate change.
    /// Panics if either exact projection product overflows `u128`.
    pub fn next_sample_cycle(&self) -> u64 {
        let remaining = self.audio_phase.denominator - self.audio_phase.numerator;
        let numerator = remaining
            .checked_mul(u128::from(self.clock))
            .expect("OPL2 sample deadline numerator overflow");
        let denominator = self
            .audio_phase
            .denominator
            .checked_mul(u128::from(SAMPLE_RATE))
            .expect("OPL2 sample deadline denominator overflow");
        let delay = numerator.div_ceil(denominator);
        self.cycle.saturating_add(delay as u64)
    }

    /// Reads IRQ and timer flags; the five low YM3812 status bits are zero.
    pub fn read_status(&mut self, cycle: u64) -> u8 {
        self.advance_to(cycle);
        let flags = (u8::from(self.timers[0].flag) << 6) | (u8::from(self.timers[1].flag) << 5);
        flags | if flags != 0 { 0x80 } else { 0 }
    }

    /// Reads port index 0 (status) or 1 (write-only data, open bus).
    pub fn read_port(&mut self, cycle: u64, port_index: u8) -> u8 {
        assert!(port_index <= 1, "OPL2 has two ports");
        let status = self.read_status(cycle);
        if port_index == 0 {
            status
        } else {
            0xFF
        }
    }

    /// Writes port index 0 (address latch) or 1 (latched register data).
    pub fn write_port(&mut self, cycle: u64, port_index: u8, value: u8) {
        assert!(port_index <= 1, "OPL2 has two ports");
        self.advance_to(cycle);
        if port_index == 0 {
            self.address = value;
        } else {
            self.write(cycle, self.address, value);
        }
    }

    /// Renders through the timestamp, then applies a YM3812 register write.
    pub fn write(&mut self, cycle: u64, register: u8, value: u8) {
        self.advance_to(cycle);
        match register {
            0x02 => self.timers[0].preset = value,
            0x03 => self.timers[1].preset = value,
            0x04 if value & 0x80 != 0 => {
                for timer in &mut self.timers {
                    timer.flag = false;
                }
            }
            0x04 => {
                self.timers[0].control(value & 1 != 0, value & 0x40 != 0);
                self.timers[1].control(value & 2 != 0, value & 0x20 != 0);
            }
            _ => self.synth.write_register(register, value),
        }
    }

    /// Appends queued chip-rate samples to `output`, emptying the queue.
    pub fn drain_samples(&mut self, output: &mut Vec<i16>) {
        output.append(&mut self.samples);
    }

    /// Renders through `cycle` and appends all queued samples to `output`.
    pub fn render_until(&mut self, cycle: u64, output: &mut Vec<i16>) {
        self.advance_to(cycle);
        self.drain_samples(output);
    }
}

#[cfg(test)]
mod tests;
