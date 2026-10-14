use crate::timing::Timeline;
use gp64_hw::IoBus;
use gp64_types::IoPort;

/// A borrowed execution view of the fixed hardware bus. Timing lives in the machine.
pub(crate) struct Bus<'a> {
    pub devices: &'a mut IoBus,
    pub clock: &'a mut Timeline,
    pub delay: u64,
    pub accessed: bool,
    pub timestamp: u64,
    pub fraction: &'a mut u64,
}
impl Bus<'_> {
    fn charge(&mut self) {
        self.accessed = true;
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
    fn should_break_batch(&self) -> bool {
        self.accessed
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
