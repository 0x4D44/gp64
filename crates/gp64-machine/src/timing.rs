//! Cycle deadlines. Each device has one replaceable slot, so reprogramming cannot leave stale events.
use gp64_hw::IoBus;
use gp64_opl2::SAMPLE_RATE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Pit(u8),
    Scanline,
    Uart,
    Audio,
    Rtc,
    Keyboard,
}

#[derive(Clone, Copy)]
struct Deadline {
    event: Event,
    cycle: u64,
}

/// A chip-rate sample at an absolute emulated CPU cycle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioSample {
    pub cycle: u64,
    pub speaker: f32,
    pub opl2: i16,
}

pub(crate) struct Timeline {
    pub rate: u64,
    pub now: u64,
    events: Vec<Deadline>,
    pit_remainder: [u128; 3],
    pit_rising: [bool; 3],
    scan_remainder: u128,
    rtc_remainder: u128,
    keyboard_remainder: u128,
    rtc_us_remainder: u128,
    guest_us: u64,
    audio_start: u64,
    pub frames: u64,
    pub render: Vec<(u64, Option<u32>)>,
    pub audio: Vec<AudioSample>,
}
impl Timeline {
    pub fn new(rate: u64, io: &mut IoBus) -> Self {
        io.pit.set_emulated_speed(rate);
        io.vga.set_emulated_ips(rate);
        io.serial.retime_for_speed_change(rate);
        io.opl2.set_clock(0, rate);
        let mut result = Self {
            rate,
            now: 0,
            events: Vec::new(),
            pit_remainder: [0; 3],
            pit_rising: [false; 3],
            scan_remainder: 0,
            rtc_remainder: 0,
            keyboard_remainder: 0,
            rtc_us_remainder: 0,
            guest_us: 0,
            audio_start: 0,
            frames: 0,
            render: Vec::new(),
            audio: Vec::new(),
        };
        for channel in 0..3 {
            result.arm_pit(io, channel);
        }
        result.next_scanline(io, 0);
        result.next_audio(io);
        result.next_rtc(io, 0);
        result.next_keyboard(0);
        result
    }
    fn schedule(&mut self, event: Event, cycle: Option<u64>) {
        self.events.retain(|d| d.event != event);
        if let Some(cycle) = cycle {
            self.events.push(Deadline { event, cycle });
        }
    }
    pub fn next(&self) -> Option<u64> {
        self.events.iter().map(|d| d.cycle).min()
    }
    /// Elapsed guest microseconds and their fractional numerator/denominator.
    pub(crate) fn guest_time(&self) -> (u64, u128, u64) {
        (self.guest_us, self.rtc_us_remainder, self.rate)
    }
    fn interval(numerator: u128, denominator: u128, remainder: &mut u128) -> u64 {
        let total = numerator + *remainder;
        *remainder = total % denominator;
        (total / denominator) as u64
    }
    fn pit_interval(&mut self, io: &IoBus, channel: u8) -> u64 {
        let state = io.pit.diagnostic_snapshot().channels[channel as usize];
        let divisor = state.effective_divisor;
        let rising = self.pit_rising[channel as usize];
        let ticks = match state.mode {
            2 => {
                if rising {
                    1
                } else {
                    divisor.saturating_sub(1).max(1)
                }
            }
            3 => {
                if rising {
                    (divisor / 2).max(1)
                } else {
                    divisor.div_ceil(2)
                }
            }
            4 | 5 => {
                if rising {
                    1
                } else {
                    divisor
                }
            }
            _ => divisor,
        };
        Self::interval(
            u128::from(ticks) * u128::from(self.rate),
            u128::from(io.pit.pit_hz()),
            &mut self.pit_remainder[channel as usize],
        )
    }
    fn arm_pit(&mut self, io: &mut IoBus, channel: u8) {
        let state = io.pit.diagnostic_snapshot().channels[channel as usize];
        if channel == 0 {
            io.pic.lower_irq(0);
        }
        self.pit_remainder[channel as usize] = 0;
        self.pit_rising[channel as usize] = !matches!(state.mode, 2..=5);
        let cycle = if state.loaded && state.counting && !state.terminal_count_reached {
            Some(self.now + self.pit_interval(io, channel))
        } else {
            None
        };
        self.schedule(Event::Pit(channel), cycle);
    }
    fn next_scanline(&mut self, io: &IoBus, base: u64) {
        let denominator = u128::from(io.vga.refresh_millihz().max(1))
            * u128::from(io.vga.total_scanlines_cached.max(1));
        let delay = Self::interval(
            u128::from(self.rate) * 1000,
            denominator,
            &mut self.scan_remainder,
        );
        self.schedule(Event::Scanline, Some(base + delay));
    }
    fn next_audio(&mut self, io: &IoBus) {
        self.schedule(Event::Audio, Some(io.opl2.next_sample_cycle()));
    }
    fn next_keyboard(&mut self, base: u64) {
        // The 8042 transfer cadence is independent of the programmable RTC.
        let delay =
            Self::interval(u128::from(self.rate), 1000, &mut self.keyboard_remainder).max(1);
        self.schedule(Event::Keyboard, Some(base.saturating_add(delay)));
    }
    fn next_rtc(&mut self, io: &IoBus, base: u64) {
        if io.cmos.is_divider_reset() {
            self.schedule(Event::Rtc, None);
            return;
        }
        let frequency = io.cmos.get_periodic_frequency_hz().unwrap_or(1024).max(1);
        let delay = Self::interval(
            u128::from(self.rate),
            u128::from(frequency),
            &mut self.rtc_remainder,
        );
        self.schedule(Event::Rtc, Some(base + delay));
    }
    fn uart(&mut self, io: &mut IoBus) {
        let deadline = io.serial.ports[..2]
            .iter()
            .filter_map(gp64_hw::serial::SerialPort::next_deadline)
            .min();
        self.schedule(Event::Uart, deadline);
        let (irq4, irq3) = io.serial.check_interrupts();
        io.pic
            .set_irq_source(4, gp64_hw::pic::PicIrqSource::Serial, irq4);
        io.pic
            .set_irq_source(3, gp64_hw::pic::PicIrqSource::Serial, irq3);
        io.serial.event_reschedule_pending = false;
    }
    /// Advance devices at event timestamps, never at the caller's late instruction boundary.
    pub fn advance(&mut self, io: &mut IoBus, target: u64) {
        while let Some((index, deadline)) = self
            .events
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, d)| d.cycle <= target)
            .min_by_key(|(_, d)| d.cycle)
        {
            self.events.remove(index);
            self.advance_continuous(io, deadline.cycle);
            match deadline.event {
                Event::Pit(channel) => {
                    let rising = self.pit_rising[channel as usize];
                    if channel == 0 && !rising {
                        io.pic.lower_irq(0);
                    }
                    let mode = io.pit.get_mode(channel);
                    if rising {
                        if channel == 0 {
                            io.pic.raise_irq(0);
                        }
                        if matches!(mode, 0 | 1 | 4 | 5) {
                            let mut state = io.pit.save_channel_state(channel as usize);
                            state.terminal_count_reached = true;
                            state.out = true;
                            io.pit.restore_channel_state(channel as usize, &state);
                            continue;
                        }
                        if mode != 3 {
                            io.pit.track_time(channel as usize, deadline.cycle);
                            io.pit.apply_deferred_reload(channel as usize);
                        }
                    }
                    if mode == 3 {
                        io.pit
                            .mode3_half_cycle_boundary(channel as usize, deadline.cycle, rising);
                    }
                    self.pit_rising[channel as usize] = !rising;
                    let delay = self.pit_interval(io, channel);
                    self.schedule(Event::Pit(channel), Some(deadline.cycle + delay));
                }
                Event::Scanline => {
                    let total = io.vga.total_scanlines_cached.max(1);
                    io.vga.scanline_counter = (io.vga.scanline_counter + 1) % total;
                    if io.vga.scanline_counter == 0 {
                        self.frames += 1;
                        self.render.push((deadline.cycle, None));
                        io.vga.frame_start_cycle = deadline.cycle;
                        io.vga.cycles_since_vblank = 0;
                        io.vga.signal_vretrace_start();
                        if io.vga.take_vretrace_irq_pending() {
                            // The display interrupt uses slave IRQ1 on the cascaded PIC.
                            io.pic.raise_irq(9);
                        }
                    } else {
                        let line = gp64_hw::Vga::compute_visible_line(
                            io.vga.cycles_since_vblank,
                            io.vga.timing_retrace_cycles,
                            io.vga.timing_cycles_per_frame,
                            io.vga.visible_scanlines_cached,
                        );
                        if io.vga.cycles_since_vblank >= io.vga.timing_retrace_cycles {
                            self.render.push((deadline.cycle, Some(line)));
                        }
                    }
                    self.next_scanline(io, deadline.cycle);
                }
                Event::Uart => self.uart(io),
                Event::Audio => {
                    let mut speaker = [0.0; 2];
                    io.speaker.generate_samples_in_window(
                        self.audio_start,
                        deadline.cycle,
                        1,
                        SAMPLE_RATE,
                        &mut speaker,
                    );
                    let mut samples = Vec::new();
                    io.opl2.render_until(deadline.cycle, &mut samples);
                    for opl2 in samples {
                        self.audio.push(AudioSample {
                            cycle: deadline.cycle,
                            speaker: speaker[0],
                            opl2,
                        });
                    }
                    self.audio_start = deadline.cycle;
                    self.next_audio(io);
                }
                Event::Keyboard => {
                    io.keyboard.stage_output(&mut io.pic);
                    self.next_keyboard(deadline.cycle);
                }
                Event::Rtc => {
                    // Continuous advancement already owns elapsed time. This
                    // deadline delivers only the divider's periodic event.
                    if io.cmos.tick_periodic(0) {
                        io.pic.raise_irq(8);
                    }
                    self.next_rtc(io, deadline.cycle);
                }
            }
        }
        self.advance_continuous(io, target);
    }
    fn advance_continuous(&mut self, io: &mut IoBus, target: u64) {
        let elapsed = target.saturating_sub(self.now);
        io.vga.tick(elapsed);
        io.serial.advance_to_cycle(target);
        let us = u128::from(elapsed) * 1_000_000 + self.rtc_us_remainder;
        self.rtc_us_remainder = us % u128::from(self.rate);
        let elapsed_us = (us / u128::from(self.rate)) as u64;
        self.guest_us = self.guest_us.saturating_add(elapsed_us);
        if elapsed_us != 0 && io.cmos.advance_clock(elapsed_us) {
            io.pic.raise_irq(8);
        }
        self.now = target;
        io.set_cycle(target);
    }
    /// Device writes invalidate their old deadline before the next instruction.
    pub fn after_write(&mut self, io: &mut IoBus, port: u16, value: u8) {
        if matches!(port, 0x3b5 | 0x3d5) && io.vga.get_crtc_index() == 0x11 && value & 0x10 == 0 {
            io.pic.lower_irq(9);
        }
        if (0x40..=0x43).contains(&port) || port == 0x61 {
            let snapshot = io.pit.diagnostic_snapshot();
            for channel in 0..3 {
                let state = snapshot.channels[channel];
                if (port == 0x40 + channel as u16
                    && state.base_cycles == self.now
                    // A Mode 3 boundary also rebases time. A pending or
                    // incomplete count write at that timestamp keeps its phase.
                    && !(state.mode == 3
                        && (state.deferred_reload.is_some() || state.write_state != 0)))
                    || (port == 0x43 && usize::from(value >> 6) == channel && value & 0x30 != 0)
                    || (port == 0x61 && channel == 2)
                {
                    self.arm_pit(io, channel as u8);
                }
            }
            io.pit.event_reschedule_pending = false;
            if io.pit.speaker_reload_updated {
                // IoBus already propagated the loaded state and effective count.
                io.pit.speaker_reload_updated = false;
            }
        }
        let mode_changed = io.vga.take_mode_change_pending();
        if let Some(period) = io.vga.pending_timing_cycles_per_frame.take() {
            io.vga.apply_timing_cycles_per_frame(period);
            self.scan_remainder = 0;
            self.next_scanline(io, self.now);
        } else if mode_changed {
            self.scan_remainder = 0;
            self.next_scanline(io, self.now);
        }
        if port == 0x71 && io.cmos.event_reschedule_pending {
            io.cmos.event_reschedule_pending = false;
            self.rtc_remainder = 0;
            self.next_rtc(io, self.now);
        }
        if port == 0x71 && io.cmos.peek_pending_flags() & 0x80 == 0 {
            io.pic.lower_irq(8);
        }
        if io.serial.event_reschedule_pending {
            self.uart(io);
        }
    }
    pub fn retime(&mut self, io: &mut IoBus, rate: u64) {
        let old = self.rate;
        let rescale = |cycles: u128| cycles * u128::from(rate) / u128::from(old);
        for deadline in &mut self.events {
            deadline.cycle =
                self.now + rescale(u128::from(deadline.cycle - self.now)).max(1) as u64;
        }
        for remainder in &mut self.pit_remainder {
            *remainder = rescale(*remainder);
        }
        self.rtc_us_remainder = rescale(self.rtc_us_remainder);
        io.vga.cycles_since_vblank = rescale(u128::from(io.vga.cycles_since_vblank)) as u64;
        self.keyboard_remainder = rescale(self.keyboard_remainder);
        self.scan_remainder = rescale(self.scan_remainder);
        self.rtc_remainder = rescale(self.rtc_remainder);
        self.rate = rate;
        io.pit.set_emulated_speed(rate);
        io.vga.set_emulated_ips(rate);
        if let Some(period) = io.vga.pending_timing_cycles_per_frame.take() {
            io.vga.apply_timing_cycles_per_frame(period);
        }
        io.serial.retime_for_speed_change(rate);
        io.opl2.set_clock(self.now, rate);
        self.next_audio(io);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;
    use gp64_types::IoPort;

    fn counter(io: &mut IoBus) -> u16 {
        io.pit.write_command(0); // latch channel 0
        u16::from_le_bytes([io.pit.read_counter(0), io.pit.read_counter(0)])
    }

    fn pit_write(io: &mut IoBus, timeline: &mut Timeline, port: u16, value: u8) {
        io.write_u8(port, value);
        timeline.after_write(io, port, value);
    }

    fn pit_deadline(timeline: &Timeline, channel: u8) -> u64 {
        let deadlines: Vec<_> = timeline
            .events
            .iter()
            .filter(|d| d.event == Event::Pit(channel))
            .collect();
        assert_eq!(deadlines.len(), 1, "one replaceable PIT event");
        deadlines[0].cycle
    }

    fn periodic_pit(mode: u8, divisor: u16) -> (IoBus, Timeline) {
        let mut io = IoBus::new();
        let rate = u64::from(io.pit.pit_hz());
        let mut timeline = Timeline::new(rate, &mut io);
        pit_write(&mut io, &mut timeline, 0x43, 0x30 | (mode << 1));
        for value in divisor.to_le_bytes() {
            pit_write(&mut io, &mut timeline, 0x40, value);
        }
        (io, timeline)
    }

    #[test]
    fn mode3_halfcycle_reload_high_and_low_phase() {
        for (old, new, write, boundary, rising, next) in [
            (100u16, 200u16, 25, 50, false, 150),
            (100, 200, 75, 100, true, 200),
            (101, 201, 25, 51, false, 151),
            (101, 200, 75, 101, true, 201),
            (100, 201, 25, 50, false, 150),
            (100, 201, 75, 100, true, 201),
        ] {
            let (mut io, mut timeline) = periodic_pit(3, old);
            timeline.advance(&mut io, write);
            let original_deadline = pit_deadline(&timeline, 0);
            let original_count = counter(&mut io);
            for value in new.to_le_bytes() {
                pit_write(&mut io, &mut timeline, 0x40, value);
            }
            assert_eq!(pit_deadline(&timeline, 0), original_deadline);
            assert_eq!(counter(&mut io), original_count);
            timeline.advance(&mut io, boundary - 1);
            assert_eq!(io.pit.get_deferred_reload(0), Some(new));
            assert_eq!(io.pic.get_irr() & 1, 0);
            timeline.advance(&mut io, boundary);
            assert_eq!(
                io.pit.get_deferred_reload(0),
                None,
                "transfer at either half-cycle boundary"
            );
            assert_eq!(pit_deadline(&timeline, 0), next);
            assert_eq!(counter(&mut io), new & !1);
            assert_eq!(io.pit.get_out(0), rising);
            assert_eq!(io.pic.get_irr() & 1, u8::from(rising));
            assert_eq!(
                io.pic.diagnostic_snapshot().master.irq_line & 1,
                u8::from(rising)
            );
            if rising {
                io.pic.acknowledge_irq(0);
                io.write_u8(0x20, 0x20);
            }
            timeline.advance(&mut io, boundary + 1);
            assert_eq!(counter(&mut io), (new & !1) - 2);
            assert_eq!(io.pit.get_out(0), rising);
            timeline.advance(&mut io, next - 1);
            assert_eq!(io.pit.get_out(0), rising);
            assert_eq!(io.pic.get_irr() & 1, 0);
            timeline.advance(&mut io, next);
            assert_eq!(io.pit.get_out(0), !rising);
            assert_eq!(counter(&mut io), new & !1);
            assert_eq!(io.pic.get_irr() & 1, u8::from(!rising));
            assert_eq!(
                pit_deadline(&timeline, 0),
                next + if rising {
                    u64::from(new / 2)
                } else {
                    u64::from(new.div_ceil(2))
                }
            );
        }
    }

    #[test]
    fn mode3_halfcycle_reload_exact_boundary_and_partial_pair() {
        for split in [false, true] {
            let (mut io, mut timeline) = periodic_pit(3, 100);
            timeline.advance(&mut io, 50);
            pit_write(&mut io, &mut timeline, 0x40, 200);
            assert_eq!(
                pit_deadline(&timeline, 0),
                100,
                "first byte must preserve LOW phase"
            );
            if split {
                timeline.advance(&mut io, 100);
                assert_eq!(counter(&mut io), 100);
                assert!(io.pit.get_out(0));
                timeline.advance(&mut io, 110);
            }
            pit_write(&mut io, &mut timeline, 0x40, 0);
            let boundary = if split { 150 } else { 100 };
            assert_eq!(pit_deadline(&timeline, 0), boundary);
            timeline.advance(&mut io, boundary);
            assert_eq!(io.pit.get_deferred_reload(0), None);
            assert_eq!(counter(&mut io), 200);
            assert_eq!(io.pit.get_out(0), !split);
            assert_eq!(pit_deadline(&timeline, 0), boundary + 100);
        }
    }

    #[test]
    fn mode2_halfcycle_reload_still_waits_for_full_period() {
        let (mut io, mut timeline) = periodic_pit(2, 100);
        timeline.advance(&mut io, 25);
        for value in [200, 0] {
            pit_write(&mut io, &mut timeline, 0x40, value);
        }
        timeline.advance(&mut io, 99);
        assert_eq!(io.pit.get_deferred_reload(0), Some(200));
        assert_eq!(counter(&mut io), 1);
        assert!(!io.pit.get_out(0));
        assert_eq!(pit_deadline(&timeline, 0), 100);
        assert_eq!(io.pic.get_irr() & 1, 0);
        timeline.advance(&mut io, 100);
        assert_eq!(io.pit.get_deferred_reload(0), None);
        assert_eq!(counter(&mut io), 200);
        assert!(io.pit.get_out(0));
        assert_eq!(pit_deadline(&timeline, 0), 299);
        assert_eq!(io.pic.get_irr() & 1, 1);
    }

    fn terminal_readback(mode: u8, bcd: bool) {
        let mut io = IoBus::new();
        let rate = u64::from(io.pit.pit_hz());
        let mut timeline = Timeline::new(rate, &mut io);
        let channel = if mode == 0 { 0 } else { 2 };
        let port = 0x40 + u16::from(channel);
        let control = (channel << 6) | 0x30 | (mode << 1) | u8::from(bcd);
        for (port, value) in [(0x43, control), (port, 16), (port, 0)] {
            io.write_u8(port, value);
            timeline.after_write(&mut io, port, value);
        }
        if mode == 1 {
            for value in [0, 1] {
                io.write_u8(0x61, value);
                timeline.after_write(&mut io, 0x61, value);
            }
        }
        let deadline = timeline
            .events
            .iter()
            .find(|event| event.event == Event::Pit(channel))
            .expect("loaded counter has a terminal deadline")
            .cycle;
        timeline.advance(&mut io, deadline);
        assert!(
            io.pit
                .save_channel_state(channel as usize)
                .terminal_count_reached
        );
        let modulus = if bcd { 10_000 } else { 65_536 };
        let wrapped = if bcd { 0x9996 } else { 0xfffc };
        for (elapsed, expected) in [(0, 0), (4, wrapped), (modulus, 0), (modulus + 4, wrapped)] {
            timeline.advance(&mut io, deadline + elapsed);
            io.write_u8(0x43, channel << 6); // guest counter latch
            let count = u16::from_le_bytes([io.read_u8(port), io.read_u8(port)]);
            assert_eq!(
                count, expected,
                "mode {mode}, BCD {bcd}, clocks after terminal {elapsed}"
            );
            assert!(io.pit.get_out(channel), "OUT remains high after terminal");
            if channel == 2 {
                assert_ne!(io.read_u8(0x61) & 0x20, 0);
            }
            assert!(timeline
                .events
                .iter()
                .all(|event| event.event != Event::Pit(channel)));
            if channel == 0 {
                assert_eq!(
                    io.pic.diagnostic_snapshot().master.irq_line & 1,
                    u8::from(elapsed == 0)
                );
                assert_eq!(io.pic.get_irr() & 1, u8::from(elapsed == 0));
                if elapsed == 0 {
                    io.pic.acknowledge_irq(0);
                    io.write_u8(0x20, 0x20); // guest non-specific EOI
                    io.pic.lower_irq(0);
                }
            }
        }
        // Count-only replacement preserves the completed shot until a fresh
        // trigger (mode 1), while a completed mode 0 write starts a new count.
        for value in [32, 0] {
            io.write_u8(port, value);
            timeline.after_write(&mut io, port, value);
        }
        if mode == 1 {
            assert!(io.pit.save_channel_state(2).terminal_count_reached);
            assert!(timeline
                .events
                .iter()
                .all(|event| event.event != Event::Pit(2)));
            io.write_u8(0x43, channel << 6);
            assert_eq!(
                u16::from_le_bytes([io.read_u8(port), io.read_u8(port)]),
                wrapped
            );
            for value in [0, 1] {
                io.write_u8(0x61, value);
                timeline.after_write(&mut io, 0x61, value);
            }
        }
        assert!(
            !io.pit
                .save_channel_state(channel as usize)
                .terminal_count_reached
        );
        assert!(!io.pit.get_out(channel));
        io.write_u8(0x43, channel << 6);
        assert_eq!(u16::from_le_bytes([io.read_u8(port), io.read_u8(port)]), 32);
        assert!(timeline
            .events
            .iter()
            .any(|event| event.event == Event::Pit(channel)));
    }

    #[test]
    fn programmed_zero_reload_reaches_timeline_audio_windows() {
        for (control, raw) in [(0xb6, 0u16), (0xb7, 0), (0xb6, 1_193)] {
            let mut io = IoBus::new();
            let mut timeline = Timeline::new(20_000_000, &mut io);
            for (port, value) in [
                (0x43, control),
                (0x42, raw as u8),
                (0x42, (raw >> 8) as u8),
                (0x61, 3),
            ] {
                io.write_u8(port, value);
                timeline.after_write(&mut io, port, value);
            }
            timeline.advance(&mut io, 2_000_000);
            assert!(
                timeline.audio.iter().any(|sample| sample.speaker > 0.0),
                "programmed reload must reach host audio with positive polarity"
            );
            assert!(
                timeline.audio.iter().any(|sample| sample.speaker < 0.0),
                "programmed reload must reach host audio with negative polarity"
            );
            for gate in [1, 2, 0] {
                io.write_u8(0x61, gate);
                timeline.after_write(&mut io, 0x61, gate);
                let old_len = timeline.audio.len();
                let target = timeline.now + 200_000;
                timeline.advance(&mut io, target);
                // The first queued sample can straddle the write cycle;
                // subsequent explicit windows lie wholly after the gate-off.
                assert!(timeline.audio.len() > old_len + 1);
                assert!(timeline.audio[old_len + 1..]
                    .iter()
                    .all(|sample| sample.speaker == 0.0));
            }
        }
        let mut io = IoBus::new();
        let mut timeline = Timeline::new(20_000_000, &mut io);
        io.write_u8(0x61, 3);
        timeline.after_write(&mut io, 0x61, 3);
        timeline.advance(&mut io, 2_000_000);
        assert!(!timeline.audio.is_empty());
        assert!(timeline.audio.iter().all(|sample| sample.speaker == 0.0));
    }

    #[test]
    fn pit_terminal_readback_mode0_binary() {
        terminal_readback(0, false);
    }

    #[test]
    fn pit_terminal_readback_mode0_bcd() {
        terminal_readback(0, true);
    }

    #[test]
    fn pit_terminal_readback_mode1_binary() {
        terminal_readback(1, false);
    }

    #[test]
    fn pit_terminal_readback_mode1_bcd() {
        terminal_readback(1, true);
    }

    #[test]
    fn set_assertion_via_cmos_ports_lowers_pending_uf_irq8() {
        let mut io = IoBus::new();
        io.cmos = gp64_hw::cmos::Cmos::new_with_fixed_boot_epoch(Some(1_718_452_800));
        let mut state = io.cmos.save_state();
        state.ram[gp64_hw::cmos::regs::STATUS_B as usize] = 0x12; // UIE + unrelated bit 1
        state.pending_irq_flags = 0x90; // IRQF + pending UF
        io.cmos.restore_state(state);
        assert_eq!(
            io.cmos.peek_pending_flags() & 0x80,
            0x80,
            "precondition: pending UF with UIE asserts IRQF before SET"
        );
        io.pic.raise_irq(8);
        assert_eq!(
            io.pic.diagnostic_snapshot().slave.irq_line & 1,
            1,
            "precondition: IRQ8 is physically asserted before SET"
        );
        let mut timeline = Timeline::new(20_000_000, &mut io);

        io.write_u8(0x70, gp64_hw::cmos::regs::STATUS_B);
        io.write_u8(0x71, 0x92); // assert SET through the guest port
        timeline.after_write(&mut io, 0x71, 0x92);

        io.write_u8(0x70, gp64_hw::cmos::regs::STATUS_B);
        let status_b = io.read_u8(0x71);
        let irq8_line = io.pic.diagnostic_snapshot().slave.irq_line & 1;
        let observed = (status_b, io.cmos.peek_pending_flags(), irq8_line);

        assert_eq!(
            observed,
            (0x82, 0x10, 0),
            "SET through ports 70h/71h must preserve UF and lower the physical IRQ8 line"
        );
    }

    #[test]
    fn repeated_clock_changes_preserve_audio_display_and_rtc_time() {
        let mut io = IoBus::new();
        let mut timeline = Timeline::new(20_000_000, &mut io);
        let mut cycle = 0;
        let mut elapsed_ns = 0u128;
        // All automatically selected clock rates, visited repeatedly. The
        // off-sample boundary exercises rational OPL phases across changes.
        for round in 0..4 {
            for exponent in 0..10 {
                for mantissa in 8..16 {
                    let rate = 125_000 * mantissa * (1 << exponent);
                    timeline.retime(&mut io, rate);
                    let span = rate / 1000 + round;
                    cycle += span;
                    elapsed_ns += u128::from(span) * 1_000_000_000 / u128::from(rate);
                    timeline.advance(&mut io, cycle);
                }
            }
        }
        let audio = &timeline.audio;
        assert!(audio.windows(2).all(|pair| pair[0].cycle < pair[1].cycle));
        let expected = u128::from(SAMPLE_RATE) * elapsed_ns / 1_000_000_000;
        assert!((audio.len() as i64 - expected as i64).abs() <= 2);
        // VGA stays on its hardware refresh cadence, not the selected CPU rate.
        let frames = u128::from(io.vga.refresh_millihz()) * elapsed_ns / 1_000_000_000_000;
        assert!((timeline.frames as i64 - frames as i64).abs() <= 1);
        assert!(
            (io.cmos.save_state().emulated_us as i64 - (elapsed_ns / 1000) as i64).abs() < 2000
        );
    }

    #[test]
    fn retime_preserves_pit_count_out_and_irq_deadline() {
        let mut io = IoBus::new();
        let rate = u64::from(io.pit.pit_hz()) * 4;
        let mut timeline = Timeline::new(rate, &mut io);
        timeline.advance(&mut io, 10_000);
        for (port, value) in [(0x43, 0x30), (0x40, 0xe8), (0x40, 0x03)] {
            io.write_u8(port, value);
            timeline.after_write(&mut io, port, value);
        }
        timeline.advance(&mut io, 12_000); // half of a 1000-tick one-shot
        assert_eq!(counter(&mut io), 500);
        assert!(!io.pit.get_out(0));
        timeline.retime(&mut io, rate / 2);
        assert_eq!(counter(&mut io), 500);
        assert!(!io.pit.get_out(0));
        let deadline = timeline
            .events
            .iter()
            .find(|d| d.event == Event::Pit(0))
            .unwrap()
            .cycle;
        assert_eq!(deadline, 13_000);
        timeline.advance(&mut io, deadline - 1);
        assert_eq!(counter(&mut io), 1);
        assert!(!io.pit.get_out(0));
        assert_eq!(io.pic.diagnostic_snapshot().master.irq_line & 1, 0);
        timeline.advance(&mut io, deadline);
        assert_eq!(counter(&mut io), 0);
        assert!(io.pit.get_out(0));
        assert_ne!(io.pic.diagnostic_snapshot().master.irq_line & 1, 0);
    }

    #[test]
    fn triggered_count_only_preserves_timeline_deadline() {
        for mode in [1, 5] {
            let mut io = IoBus::new();
            let rate = u64::from(io.pit.pit_hz());
            let mut timeline = Timeline::new(rate, &mut io);
            for (port, value) in [
                (0x43, 0xb0 | (mode << 1)),
                (0x42, 40),
                (0x42, 0),
                (0x61, 0),
                (0x61, 1),
            ] {
                io.write_u8(port, value);
                timeline.after_write(&mut io, port, value);
            }
            let deadline = timeline
                .events
                .iter()
                .find(|d| d.event == Event::Pit(2))
                .unwrap()
                .cycle;
            assert_eq!(deadline, 40);
            timeline.advance(&mut io, 20);
            for value in [90, 0] {
                io.write_u8(0x42, value);
                timeline.after_write(&mut io, 0x42, value);
            }
            assert_eq!(
                timeline
                    .events
                    .iter()
                    .find(|d| d.event == Event::Pit(2))
                    .unwrap()
                    .cycle,
                deadline,
                "count write must preserve original event deadline"
            );
            timeline.advance(&mut io, 40);
            assert_eq!(io.pit.get_out(2), mode == 1);
            timeline.advance(&mut io, 41);
            assert!(io.pit.get_out(2));
            assert!(io.pit.save_channel_state(2).terminal_count_reached);
        }
    }

    fn mode0_first_byte_timeline(write_cycle: u64) {
        let mut io = IoBus::new();
        let rate = u64::from(io.pit.pit_hz());
        let mut timeline = Timeline::new(rate, &mut io);
        for (port, value) in [(0x43, 0x30), (0x40, 40), (0x40, 0)] {
            io.write_u8(port, value);
            timeline.after_write(&mut io, port, value);
        }
        timeline.advance(&mut io, write_cycle);
        if write_cycle >= 40 {
            assert_ne!(io.pic.get_irr() & 1, 0);
            io.pic.acknowledge_irq(0);
            io.write_u8(0x20, 0x20);
            io.pic.lower_irq(0);
        }
        io.write_u8(0x40, 90);
        timeline.after_write(&mut io, 0x40, 90);
        if write_cycle < 40 {
            assert!(
                timeline.events.iter().all(|d| d.event != Event::Pit(0)),
                "first byte must cancel the old IRQ deadline"
            );
        } else {
            assert!(!io.pit.get_out(0), "first byte must lower terminal OUT");
        }
        let frozen = counter(&mut io);
        timeline.advance(&mut io, 100_000);
        assert_eq!(counter(&mut io), frozen);
        assert!(!io.pit.get_out(0));
        assert_eq!(io.pic.get_irr() & 1, 0);
        assert_eq!(io.pic.diagnostic_snapshot().master.irq_line & 1, 0);
        assert!(timeline.events.iter().all(|d| d.event != Event::Pit(0)));
        io.write_u8(0x40, 0);
        timeline.after_write(&mut io, 0x40, 0);
        assert_eq!(
            timeline
                .events
                .iter()
                .find(|d| d.event == Event::Pit(0))
                .unwrap()
                .cycle,
            100_090
        );
        assert!(!io.pit.save_channel_state(0).terminal_count_reached);
        timeline.advance(&mut io, 100_089);
        assert_eq!(io.pic.get_irr() & 1, 0);
        timeline.advance(&mut io, 100_090);
        assert_ne!(io.pic.get_irr() & 1, 0);
        assert!(io.pit.get_out(0));
        assert!(timeline.events.iter().all(|d| d.event != Event::Pit(0)));
    }

    #[test]
    fn mode0_first_byte_cancels_active_timeline_irq() {
        mode0_first_byte_timeline(20);
    }

    #[test]
    fn mode0_first_byte_after_terminal_rearms_new_irq() {
        mode0_first_byte_timeline(44);
    }

    #[test]
    fn rtc_divider_reset_removes_deadlines_and_releases_fresh_phase() {
        for reset in [0x66, 0x76] {
            let mut io = IoBus::new();
            io.cmos = gp64_hw::cmos::Cmos::new_with_fixed_boot_epoch(Some(0));
            let mut timeline = Timeline::new(1_000_000, &mut io);
            timeline.advance(&mut io, 123_456);
            io.write_u8(0x70, 0x0a);
            io.write_u8(0x71, reset);
            timeline.after_write(&mut io, 0x71, reset);
            assert!(
                timeline.events.iter().all(|d| d.event != Event::Rtc),
                "reset must remove RTC deadline"
            );
            timeline.retime(&mut io, 2_000_000);
            timeline.advance(&mut io, 6_123_456);
            assert_eq!(io.cmos.get_rtc_time_bcd(), (0, 0, 0));
            assert!(timeline.events.iter().all(|d| d.event != Event::Rtc));
            io.write_u8(0x71, 0x26);
            timeline.after_write(&mut io, 0x71, 0x26);
            assert_eq!(rtc_deadline(&timeline), timeline.now + 1953);
            let release = timeline.now;
            timeline.advance(&mut io, release + 999_999);
            assert_eq!(io.cmos.get_rtc_time_bcd(), (0, 0, 0));
            timeline.advance(&mut io, release + 1_000_000);
            assert_eq!(io.cmos.get_rtc_time_bcd(), (0, 0, 1));
            io.write_u8(0x71, 0x20);
            timeline.after_write(&mut io, 0x71, 0x20);
            assert_eq!(
                rtc_deadline(&timeline),
                timeline.now + 1953,
                "normal rate zero retains calendar polling"
            );
        }
    }

    fn rtc_deadline(timeline: &Timeline) -> u64 {
        timeline
            .events
            .iter()
            .find(|deadline| deadline.event == Event::Rtc)
            .expect("RTC deadline is scheduled")
            .cycle
    }

    fn rtc_uip_timing_read_at(
        io: &mut IoBus,
        timeline: &mut Timeline,
        register: u8,
        timestamp: u64,
    ) -> u8 {
        let mut fraction = 0;
        guest_write_byte(io, timeline, &mut fraction, 0x70, register);
        Bus {
            devices: io,
            clock: timeline,
            delay: 0,
            accessed: false,
            timestamp,
            fraction: &mut fraction,
        }
        .read_u8(0x71)
    }

    fn rtc_uip_timing_rate(rate_select: u8) {
        use gp64_hw::cmos::regs;
        let mut io = IoBus::new();
        io.cmos = gp64_hw::cmos::Cmos::new_with_fixed_boot_epoch(Some(0));
        let mut timeline = Timeline::new(1_000_000, &mut io);
        let mut fraction = 0;
        if rate_select == 6 {
            assert_eq!(
                rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_A, 0),
                0x26
            );
        } else {
            guest_write_byte(&mut io, &mut timeline, &mut fraction, 0x70, regs::STATUS_A);
            guest_write_byte(
                &mut io,
                &mut timeline,
                &mut fraction,
                0x71,
                0x20 | rate_select,
            );
        }
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_A, 999_755) & 0x80,
            0
        );
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_A, 999_756) & 0x80,
            0x80,
            "active bus must expose UIP independently of the periodic rate"
        );
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::SECONDS, 999_999),
            0
        );
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_A, 1_000_000) & 0x80,
            0
        );
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::SECONDS, 1_000_000),
            1
        );
        assert_eq!(
            io.cmos.save_state().emulated_us,
            1_000_000,
            "elapsed time is counted once"
        );
        let flags = rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_C, 1_000_000);
        assert_eq!(flags & 0x40, if rate_select == 0 { 0 } else { 0x40 });
        assert_eq!(
            flags & 0x10,
            0x10,
            "the completed calendar update latches UF"
        );
        let next = rtc_deadline(&timeline);
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_C, next - 1) & 0x40,
            0,
            "continuous clock advancement must not generate PF before its deadline"
        );
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_C, next) & 0x40,
            if rate_select == 0 { 0 } else { 0x40 }
        );
    }

    #[test]
    fn rtc_uip_timing_default_rate() {
        rtc_uip_timing_rate(6);
    }

    #[test]
    fn rtc_uip_timing_low_rate() {
        rtc_uip_timing_rate(15);
    }

    #[test]
    fn rtc_uip_timing_disabled_periodic_rate() {
        rtc_uip_timing_rate(0);
    }

    #[test]
    fn rtc_uip_timing_set_suppresses_update() {
        use gp64_hw::cmos::regs;
        let mut io = IoBus::new();
        io.cmos = gp64_hw::cmos::Cmos::new_with_fixed_boot_epoch(Some(0));
        let mut timeline = Timeline::new(1_000_000, &mut io);
        let mut fraction = 0;
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_A, 999_756) & 0x80,
            0x80
        );
        guest_write_byte(&mut io, &mut timeline, &mut fraction, 0x70, regs::STATUS_B);
        guest_write_byte(&mut io, &mut timeline, &mut fraction, 0x71, 0x82);
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_A, 999_999) & 0x80,
            0
        );
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::SECONDS, 1_000_100),
            0
        );
        let flags = rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_C, 1_000_100);
        assert_eq!(
            flags & 0x50,
            0x40,
            "SET suppresses updates while preserving PF"
        );
        assert_eq!(io.cmos.save_state().emulated_us, 1_000_100);
    }

    #[test]
    fn rtc_uip_timing_retime_preserves_fraction() {
        use gp64_hw::cmos::regs;
        let mut io = IoBus::new();
        io.cmos = gp64_hw::cmos::Cmos::new_with_fixed_boot_epoch(Some(0));
        let mut timeline = Timeline::new(3_000_000, &mut io);
        let before = 999_755 * 3 + 2; // 999755 + 2/3 microseconds
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_A, before) & 0x80,
            0
        );
        timeline.retime(&mut io, 7_000_000);
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_A, before + 3) & 0x80,
            0x80,
            "retiming retains the fractional microsecond at the UIP boundary"
        );
        assert_eq!(io.cmos.save_state().emulated_us, 999_756);
        assert_eq!(io.cmos.save_state().uip_accum_us, 999_756);
    }

    #[test]
    fn rtc_uip_timing_fractional_periodic_deadlines() {
        use gp64_hw::cmos::regs;
        let mut io = IoBus::new();
        io.cmos = gp64_hw::cmos::Cmos::new_with_fixed_boot_epoch(Some(0));
        let mut timeline = Timeline::new(20_000_000, &mut io);
        let first = rtc_deadline(&timeline);
        assert_eq!(first, 19_531);
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_C, first - 1) & 0x40,
            0
        );
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_C, first) & 0x40,
            0x40
        );
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_C, first + 20) & 0x40,
            0
        );
        assert_eq!(
            io.cmos.save_state().emulated_us,
            977,
            "time advances between fractional periodic deadlines without another PF"
        );
        let second = rtc_deadline(&timeline);
        assert_eq!(second, 39_062);
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_C, second - 1) & 0x40,
            0
        );
        assert_eq!(
            rtc_uip_timing_read_at(&mut io, &mut timeline, regs::STATUS_C, second) & 0x40,
            0x40
        );
        assert_eq!(io.cmos.save_state().emulated_us, 1953);
    }

    fn guest_write_byte(
        io: &mut IoBus,
        timeline: &mut Timeline,
        fraction: &mut u64,
        port: u16,
        value: u8,
    ) {
        let timestamp = timeline.now;
        let mut bus = Bus {
            devices: io,
            clock: timeline,
            delay: 0,
            accessed: false,
            timestamp,
            fraction,
        };
        bus.write_u8(port, value);
    }

    fn guest_read_byte(
        io: &mut IoBus,
        timeline: &mut Timeline,
        fraction: &mut u64,
        port: u16,
    ) -> u8 {
        let timestamp = timeline.now;
        let mut bus = Bus {
            devices: io,
            clock: timeline,
            delay: 0,
            accessed: false,
            timestamp,
            fraction,
        };
        bus.read_u8(port)
    }

    #[test]
    fn guest_enabling_pending_periodic_flag_asserts_irq8_before_next_tick() {
        let mut io = IoBus::new();
        io.cmos = gp64_hw::cmos::Cmos::new_with_fixed_boot_epoch(Some(0));
        for register in [
            gp64_hw::cmos::regs::SECONDS_ALARM,
            gp64_hw::cmos::regs::MINUTES_ALARM,
            gp64_hw::cmos::regs::HOURS_ALARM,
        ] {
            io.write_u8(0x70, register);
            io.write_u8(0x71, 0x01);
        }
        let mut timeline = Timeline::new(20_000_000, &mut io);
        let mut fraction = 0;
        guest_write_byte(&mut io, &mut timeline, &mut fraction, 0x21, 0xfb);
        guest_write_byte(&mut io, &mut timeline, &mut fraction, 0xa1, 0xfe);
        let first_rtc = rtc_deadline(&timeline);
        timeline.advance(&mut io, first_rtc);

        let pending = io.cmos.peek_pending_flags();
        assert_eq!(
            pending & 0x70,
            0x40,
            "the normal first RTC deadline latches PF"
        );
        assert_eq!(pending & 0x80, 0, "PIE is disabled before the guest write");
        assert_eq!(io.pic.diagnostic_snapshot().slave.irq_line & 1, 0);

        guest_write_byte(
            &mut io,
            &mut timeline,
            &mut fraction,
            0x70,
            gp64_hw::cmos::regs::STATUS_B,
        );
        guest_write_byte(&mut io, &mut timeline, &mut fraction, 0x71, 0x42);
        assert_eq!(
            timeline.now, first_rtc,
            "enabling PIE does not wait for another tick"
        );
        let snapshot = io.pic.diagnostic_snapshot();
        assert_eq!(
            snapshot.slave.irq_line & 1,
            1,
            "the guest write asserts IRQ8"
        );
        assert_eq!(snapshot.slave.irr & 1, 1, "the PIC latches slave IRQ0");
        assert_eq!(
            io.pic.get_interrupt(),
            Some(0x70),
            "the RTC vector is ready immediately"
        );

        let second_rtc = rtc_deadline(&timeline);
        assert!(second_rtc > first_rtc);
        timeline.advance(&mut io, second_rtc);
        assert_eq!(io.pic.diagnostic_snapshot().slave.irr & 1, 0);
        guest_write_byte(
            &mut io,
            &mut timeline,
            &mut fraction,
            0x70,
            gp64_hw::cmos::regs::STATUS_B,
        );
        guest_write_byte(&mut io, &mut timeline, &mut fraction, 0x71, 0x42);
        let snapshot = io.pic.diagnostic_snapshot();
        assert_eq!(snapshot.slave.irq_line & 1, 1);
        assert_eq!(
            snapshot.slave.irr & 1,
            0,
            "the second tick and identical B write add no edge"
        );

        guest_write_byte(
            &mut io,
            &mut timeline,
            &mut fraction,
            0x70,
            gp64_hw::cmos::regs::STATUS_C,
        );
        let status_c = guest_read_byte(&mut io, &mut timeline, &mut fraction, 0x71);
        assert_eq!(
            status_c & 0xc0,
            0xc0,
            "Status C reports and clears pending PF/IRQF"
        );
        assert_eq!(io.cmos.peek_pending_flags(), 0);
        assert_eq!(io.pic.diagnostic_snapshot().slave.irq_line & 1, 0);
    }
}
