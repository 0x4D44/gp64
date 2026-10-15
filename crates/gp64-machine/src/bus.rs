use crate::timing::Timeline;
use gp64_hw::IoBus;
use gp64_types::IoPort;

/// A borrowed execution view of the fixed hardware bus. Timing lives in the machine.
pub(crate) struct Bus<'a> {
    pub devices: &'a mut IoBus,
    pub clock: &'a mut Timeline,
    pub delay: u64,
    pub horizon: u64,
    pub poll_boundary_wanted: bool,
    pub timestamp: u64,
    pub fraction: &'a mut u64,
}
impl Bus<'_> {
    fn rtc_clock_irq_enabled(&self) -> bool {
        // Update/alarm edges come from continuous clock catch-up, rather
        // than the periodic divider's queued deadline.
        self.devices.cmos.nvram()[usize::from(gp64_hw::cmos::regs::STATUS_B)] & 0x30 != 0
    }
    fn charge(&mut self) {
        self.clock
            .advance(self.devices, self.timestamp.max(self.clock.now));
        let total = self.clock.rate + *self.fraction;
        self.delay += total / 1_000_000;
        *self.fraction = total % 1_000_000;
    }
    fn read_byte(&mut self, port: u16) -> u8 {
        let value = self.devices.read_u8(port);
        if port == 0x71 && self.devices.cmos.peek_pending_flags() & 0x80 == 0 {
            self.devices.pic.lower_irq(8);
        }
        if self.devices.serial.event_reschedule_pending {
            self.clock.after_write(self.devices, port, 0);
        }
        value
    }
    fn write_byte(&mut self, port: u16, value: u8) {
        let gate_changed = port != 0x61
            || self.devices.pit.diagnostic_snapshot().channels[2].gate != (value & 1 != 0);
        self.devices.write_u8(port, value);
        if gate_changed {
            self.clock.after_write(self.devices, port, value);
        }
    }
}
impl IoPort for Bus<'_> {
    fn set_cpu_cycles(&mut self, cycle: u64) {
        self.timestamp = cycle;
    }
    fn io_delay_total(&self) -> u64 {
        self.delay
    }
    fn interrupt_poll_wanted(&self) -> bool {
        self.poll_boundary_wanted
            || self.devices.pic.has_pending_irq()
            || self.rtc_clock_irq_enabled()
    }
    fn should_break_batch(&self) -> bool {
        self.devices.pic.deliverability_dirty()
            || self.rtc_clock_irq_enabled()
            || self.clock.next().is_some_and(|next| next < self.horizon)
            || self.devices.vga.pipeline_dirty()
            || self.devices.vga.has_pending_raster_writes()
            || self.devices.system.has_pending_action()
            || self.devices.keyboard.has_pending_a20()
            || self.devices.keyboard.has_pending_guest_reset()
    }
    fn read_u8(&mut self, port: u16) -> u8 {
        self.charge();
        self.read_byte(port)
    }
    fn read_u16(&mut self, port: u16) -> u16 {
        self.charge();
        u16::from_le_bytes([self.read_byte(port), self.read_byte(port.wrapping_add(1))])
    }
    fn read_u32(&mut self, port: u16) -> u32 {
        self.charge();
        u32::from_le_bytes(std::array::from_fn(|i| {
            self.read_byte(port.wrapping_add(i as u16))
        }))
    }
    fn write_u8(&mut self, port: u16, value: u8) {
        self.charge();
        self.write_byte(port, value);
    }
    fn write_u16(&mut self, port: u16, value: u16) {
        self.charge();
        for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.write_byte(port.wrapping_add(i as u16), byte);
        }
    }
    fn write_u32(&mut self, port: u16, value: u32) {
        self.charge();
        for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.write_byte(port.wrapping_add(i as u16), byte);
        }
    }
    fn vga_pipeline_dirty(&self) -> bool {
        self.devices.vga.pipeline_dirty()
    }
    fn take_vga_pipeline_state(&mut self) -> gp64_types::VgaPipelineState {
        let state = self.devices.vga.build_pipeline_state();
        self.devices.vga.clear_pipeline_dirty();
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_break_batch_tracks_machine_service_signals() {
        for signal in 0..7 {
            let mut devices = IoBus::new();
            let mut clock = Timeline::new(100_000_000, &mut devices);
            devices.vga.clear_pipeline_dirty();
            devices.pic.take_deliverability_dirty();
            let horizon = clock.next().unwrap();
            let mut fraction = 0;
            let mut bus = Bus {
                devices: &mut devices,
                clock: &mut clock,
                delay: 0,
                horizon,
                poll_boundary_wanted: false,
                timestamp: 0,
                fraction: &mut fraction,
            };
            bus.read_u8(0x3da);
            assert!(!bus.should_break_batch(), "status read, signal={signal}");
            match signal {
                0 => bus.devices.pic.raise_irq(0),
                1 => {
                    // An early PIT deadline remains a break reason even after
                    // consuming the PIC mark from reprogramming channel zero.
                    bus.write_u8(0x43, 0x10);
                    bus.write_u8(0x40, 1);
                    bus.devices.pic.take_deliverability_dirty();
                    assert!(bus.clock.next().unwrap() < horizon);
                }
                2 => bus.devices.vga.mark_pipeline_dirty(),
                3 => bus.devices.system.write_port_92(0),
                4 => {
                    bus.devices.keyboard.write_command(0xd1);
                    bus.devices.keyboard.write_data(0xdd);
                }
                5 => bus.devices.keyboard.write_command(0xfe),
                6 => {
                    let cycle = bus.devices.vga.timing_cycles_per_frame / 2;
                    bus.devices.vga.latch_or_apply_raster_write(0x3c9, 0, cycle);
                    assert!(bus.devices.vga.has_pending_raster_writes());
                }
                _ => unreachable!(),
            }
            assert!(bus.should_break_batch(), "service signal={signal}");
        }
    }
}
