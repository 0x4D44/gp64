//! Fixed PC device set and port decoding.
use crate::joystick::Joystick;
use crate::pic::PicIrqSource;
use crate::serial::SerialPorts;
use crate::{Cmos, Keyboard, PcSpeaker, Pic, Pit, SoundBlaster, SystemControl, Vga};

/// Sound Blaster base port and interrupt line.
const SB_BASE: u16 = 0x220;
const SB_IRQ: u8 = 7;
/// PC hardware decoded by gp64. Absent devices return the open-bus value.
pub struct IoBus {
    pub vga: Vga,
    pub keyboard: Keyboard,
    pub pit: Pit,
    pub pic: Pic,
    pub cmos: Cmos,
    pub speaker: PcSpeaker,
    pub system: SystemControl,
    pub serial: SerialPorts,
    pub joystick: Joystick,
    pub opl2: gp64_opl2::Opl2,
    pub sound_blaster: SoundBlaster,
    cycle: u64,
}
impl Default for IoBus {
    fn default() -> Self {
        Self::new()
    }
}
impl IoBus {
    pub fn new() -> Self {
        Self {
            vga: Vga::new(),
            keyboard: Keyboard::new(),
            pit: Pit::new(),
            pic: Pic::new(),
            cmos: Cmos::new(),
            speaker: PcSpeaker::new(),
            system: SystemControl::new(),
            serial: SerialPorts::new_with_present_mask([true, true, false, false]),
            joystick: Joystick::new(),
            opl2: gp64_opl2::Opl2::new(),
            sound_blaster: SoundBlaster::new(),
            cycle: 0,
        }
    }
    /// Timestamp for the next access. The machine supplies a monotonic cycle clock.
    pub fn set_cycle(&mut self, cycle: u64) {
        self.cycle = cycle;
        self.pit.total_cycles = cycle;
        self.speaker.set_cycle(cycle);
    }
    fn sync_sound_blaster_irq(&mut self) {
        self.pic.set_irq_source(
            SB_IRQ,
            PicIrqSource::SoundBlaster,
            self.sound_blaster.irq_asserted(),
        );
    }
    pub fn read_u8(&mut self, port: u16) -> u8 {
        match port {
            0x20 | 0x21 | 0xa0 | 0xa1 => {
                if port & 1 == 0 {
                    self.pic.read_command(u8::from(port >= 0xa0))
                } else {
                    self.pic.read_data(u8::from(port >= 0xa0))
                }
            }
            0x40..=0x43 => {
                if port < 0x43 {
                    self.pit.read_counter((port - 0x40) as u8)
                } else {
                    0xff
                }
            }
            0x60 => self.keyboard.read_port60(&mut self.pic),
            0x61 => {
                (self.keyboard.read_control() & 0xd0)
                    | (u8::from(self.pit.get_out(2)) << 5)
                    | (self.speaker.read_control() & 15)
            }
            0x64 => self.keyboard.read_status(),
            0x70 => self.cmos.read_index(),
            0x71 => self.cmos.read_data(),
            0x92 => self.system.read_port_92(),
            0x201 => self.joystick.read(),
            0x388..=0x389 => self.opl2.read_port(self.cycle, (port - 0x388) as u8),
            p if is_sb_fm_port(p) => self.opl2.read_port(self.cycle, (p & 1) as u8),
            p if is_sb_dsp_port(p) => {
                let value = self.sound_blaster.read(p - SB_BASE);
                self.sync_sound_blaster_irq();
                value
            }
            0x3b4 | 0x3b5 | 0x3ba | 0x3c0..=0x3cf | 0x3d4..=0x3d5 | 0x3d8..=0x3d9 | 0x3da => {
                self.vga.read_port(port)
            }
            0x2f8..=0x2ff | 0x3f8..=0x3ff => self.serial.read(port).unwrap_or(0xff),
            _ => 0xff,
        }
    }
    pub fn write_u8(&mut self, port: u16, value: u8) {
        match port {
            0x20 | 0x21 | 0xa0 | 0xa1 => {
                if port & 1 == 0 {
                    self.pic.write_command(u8::from(port >= 0xa0), value)
                } else {
                    self.pic.write_data(u8::from(port >= 0xa0), value)
                }
            }
            0x40..=0x43 => {
                self.pit.write(port, value);
                if port == 0x42 || (port == 0x43 && value >> 6 == 2 && value & 0x30 != 0) {
                    let state = self.pit.diagnostic_snapshot().channels[2];
                    self.speaker.update_pit_state(
                        self.pit.get_divisor(2),
                        state.loaded.then_some(state.effective_divisor),
                    );
                }
            }
            0x60 => self.keyboard.write_data(value),
            0x61 => {
                self.keyboard.write_control(value);
                self.speaker.write_control(value);
                self.pit.set_gate(2, value & 1 != 0);
            }
            0x64 => self.keyboard.write_command(value),
            0x70 => self.cmos.write_index(value),
            0x71 => match self.cmos.write_data(value) {
                Some(true) => self.pic.raise_irq(8),
                Some(false) => self.pic.lower_irq(8),
                None => {}
            },
            0x92 => self.system.write_port_92(value),
            0x201 => self.joystick.write(value),
            0x388..=0x389 => self
                .opl2
                .write_port(self.cycle, (port - 0x388) as u8, value),
            p if is_sb_fm_port(p) => self.opl2.write_port(self.cycle, (p & 1) as u8, value),
            p if is_sb_dsp_port(p) => {
                self.sound_blaster.write(p - SB_BASE, value);
                self.sync_sound_blaster_irq();
            }
            0x3b4 | 0x3b5 | 0x3ba | 0x3c0..=0x3cf | 0x3d4..=0x3d5 | 0x3d8..=0x3d9 | 0x3da => {
                self.vga.write_port(port, value)
            }
            0x2f8..=0x2ff | 0x3f8..=0x3ff => {
                self.serial.write(port, value);
            }
            _ => {}
        }
    }
}

/// FM chip aliases on the card: base+0..3 and base+8..9.
fn is_sb_fm_port(port: u16) -> bool {
    matches!(port.wrapping_sub(SB_BASE), 0..=3 | 8 | 9)
}

/// DSP ports: reset, read data, write command/data, read status.
fn is_sb_dsp_port(port: u16) -> bool {
    matches!(port.wrapping_sub(SB_BASE), 0x6 | 0xa | 0xc | 0xe)
}

#[cfg(test)]
mod pic_poll_tests {
    use super::IoBus;

    fn command_port(slave: bool) -> u16 {
        if slave {
            0xa0
        } else {
            0x20
        }
    }

    fn data_port(slave: bool) -> u16 {
        command_port(slave) + 1
    }

    fn poll_irq(hardware: &mut IoBus, command_port: u16) -> u8 {
        hardware.write_u8(command_port, 0x0c);
        hardware.read_u8(command_port)
    }

    fn read_isr(hardware: &mut IoBus, command_port: u16) -> u8 {
        hardware.write_u8(command_port, 0x0b);
        hardware.read_u8(command_port)
    }

    fn exercise_init_selector_ports(slave: bool) {
        let command = command_port(slave);
        let data = data_port(slave);
        let irq = if slave { 13 } else { 5 };
        let pending = 1 << (irq % 8);
        let mut hardware = IoBus::new();

        hardware.write_u8(command, 0x0b); // OCW3: select ISR.
        hardware.write_u8(command, 0x11); // ICW1: cascaded, ICW4 follows.
        hardware.write_u8(data, if slave { 0x28 } else { 0x20 }); // ICW2
        hardware.write_u8(data, if slave { 0x02 } else { 0x04 }); // ICW3
        hardware.write_u8(data, 0x01); // ICW4
        hardware.write_u8(data, !pending); // Unmask only the line under test.
        hardware.pic.raise_irq(irq);
        assert_eq!(
            hardware.read_u8(command),
            pending,
            "initialized PIC {slave} command port reads a fresh unmasked request from IRR"
        );

        hardware.write_u8(command, 0x0b); // OCW3: select ISR again.
        assert_eq!(hardware.read_u8(command), 0, "PIC {slave} ISR is empty");
        hardware.write_u8(command, 0x0a); // OCW3: select IRR again.
        assert_eq!(
            hardware.read_u8(command),
            pending,
            "PIC {slave} IRR remains pending"
        );
    }

    #[test]
    fn programmed_zero_reload_guest_io_preserves_count_and_gating() {
        for (control, raw, effective) in [
            (0xb6, 0u16, 65_536),
            (0xb7, 0, 10_000),
            (0xb6, 1_193, 1_193),
        ] {
            let mut io = IoBus::new();
            io.write_u8(0x61, 3);
            assert!(!io.speaker.is_active(), "startup is unprogrammed");
            io.write_u8(0x43, control);
            io.write_u8(0x42, raw as u8);
            assert!(!io.speaker.is_active(), "incomplete reload remains silent");
            io.write_u8(0x42, (raw >> 8) as u8);
            assert_eq!(
                io.pit.diagnostic_snapshot().channels[2].effective_divisor,
                effective
            );
            assert!(
                io.speaker.is_active(),
                "complete guest reload enables square wave"
            );
            assert_eq!(io.speaker.get_divisor(), raw);
            assert_eq!(io.speaker.get_frequency(), (1_193_182 / effective) as u32);
            let mut samples = vec![0.0; 8_820];
            io.speaker
                .generate_samples_in_window(0, 100_000, 4_410, 44_100, &mut samples);
            assert!(samples.iter().any(|sample| *sample > 0.0));
            assert!(samples.iter().any(|sample| *sample < 0.0));
            for gate in [1, 2, 0] {
                io.set_cycle(100_000);
                io.write_u8(0x61, gate);
                assert_eq!(io.read_u8(0x61) & 3, gate);
                assert!(!io.speaker.is_active());
                samples.fill(0.0);
                io.speaker.generate_samples_in_window(
                    100_000,
                    200_000,
                    4_410,
                    44_100,
                    &mut samples,
                );
                assert!(samples.iter().all(|sample| *sample == 0.0));
            }
            io.write_u8(0x61, 3);
            assert!(io.speaker.is_active());
            io.write_u8(0x43, control);
            assert!(
                !io.speaker.is_active(),
                "control word requires a fresh completed reload"
            );
        }
    }

    #[test]
    fn pic_init_selector_resets_master_through_ports() {
        exercise_init_selector_ports(false);
    }

    #[test]
    fn pic_init_selector_resets_slave_through_ports() {
        exercise_init_selector_ports(true);
    }

    fn exercise_special_mask_nested_eoi(
        slave: bool,
        priority_base: u8,
        outer_irq: u8,
        inner_irq: u8,
        lower_irq: u8,
        eoi: u8,
        expected_priority_base: u8,
    ) {
        let mut hardware = IoBus::new();
        let command_port = command_port(slave);
        let data_port = data_port(slave);
        let irq_offset = if slave { 8 } else { 0 };

        hardware.write_u8(data_port, 0);
        if priority_base != 7 {
            hardware.write_u8(command_port, 0xc0 | priority_base);
        }

        hardware.pic.raise_irq(irq_offset + outer_irq);
        assert_eq!(
            poll_irq(&mut hardware, command_port),
            0x80 | outer_irq,
            "PIC {slave} must acknowledge the outer IRQ"
        );
        hardware.pic.lower_irq(irq_offset + outer_irq);

        hardware.write_u8(data_port, 1 << outer_irq);
        hardware.write_u8(command_port, 0x68);
        hardware.pic.raise_irq(irq_offset + inner_irq);
        assert_eq!(
            poll_irq(&mut hardware, command_port),
            0x80 | inner_irq,
            "PIC {slave} must admit the lower-priority nested IRQ"
        );
        assert_eq!(
            read_isr(&mut hardware, command_port),
            (1 << outer_irq) | (1 << inner_irq),
            "PIC {slave} must have both handlers in service before EOI"
        );

        hardware.write_u8(command_port, eoi);
        assert_eq!(
            read_isr(&mut hardware, command_port),
            1 << outer_irq,
            "PIC {slave} EOI must clear the unmasked nested handler"
        );
        let snapshot = hardware.pic.diagnostic_snapshot();
        let chip = if slave {
            snapshot.slave
        } else {
            snapshot.master
        };
        assert_eq!(
            chip.priority_base, expected_priority_base,
            "PIC {slave} must retain the expected rotation after EOI"
        );

        hardware.pic.raise_irq(irq_offset + lower_irq);
        assert_eq!(
            poll_irq(&mut hardware, command_port),
            0x80 | lower_irq,
            "PIC {slave} must admit the next lower-priority IRQ after nested EOI"
        );
    }

    #[test]
    fn special_mask_non_specific_eoi_clears_unmasked_nested_irq_and_admits_lower_irq() {
        for slave in [false, true] {
            exercise_special_mask_nested_eoi(slave, 7, 3, 4, 5, 0x20, 7);
        }
    }

    #[test]
    fn special_mask_rotate_non_specific_eoi_clears_nested_irq_and_rotates_to_it() {
        for slave in [false, true] {
            exercise_special_mask_nested_eoi(slave, 4, 6, 7, 0, 0xa0, 7);
        }
    }

    #[test]
    fn special_mask_non_specific_eoi_preserves_masked_only_isr_and_priority() {
        for slave in [false, true] {
            for (priority_base, outer_irq) in [(7, 3), (4, 6)] {
                for eoi in [0x20, 0xa0] {
                    let mut hardware = IoBus::new();
                    let command_port = command_port(slave);
                    let data_port = data_port(slave);
                    let irq_offset = if slave { 8 } else { 0 };

                    hardware.write_u8(data_port, 0);
                    if priority_base != 7 {
                        hardware.write_u8(command_port, 0xc0 | priority_base);
                    }
                    hardware.pic.raise_irq(irq_offset + outer_irq);
                    assert_eq!(
                        poll_irq(&mut hardware, command_port),
                        0x80 | outer_irq,
                        "PIC {slave} must acknowledge the sole in-service IRQ"
                    );
                    hardware.pic.lower_irq(irq_offset + outer_irq);
                    hardware.write_u8(data_port, 1 << outer_irq);
                    hardware.write_u8(command_port, 0x68);

                    hardware.write_u8(command_port, eoi);
                    assert_eq!(
                        read_isr(&mut hardware, command_port),
                        1 << outer_irq,
                        "PIC {slave} EOI {eoi:#04x} must not clear a masked-only ISR"
                    );
                    let snapshot = hardware.pic.diagnostic_snapshot();
                    let chip = if slave {
                        snapshot.slave
                    } else {
                        snapshot.master
                    };
                    assert_eq!(
                        chip.priority_base, priority_base,
                        "PIC {slave} EOI {eoi:#04x} must not rotate without an eligible ISR"
                    );
                }
            }
        }
    }

    #[test]
    fn poll_port_bus_irq1_returns_irq_number_and_establishes_service() {
        let mut hardware = IoBus::new();
        hardware.pic.raise_irq(1);
        hardware.write_u8(0x20, 0x0c);
        assert_eq!(hardware.read_u8(0x20), 0x81);
        let snapshot = hardware.pic.diagnostic_snapshot();
        assert_eq!(snapshot.master.irr, 0);
        assert_eq!(snapshot.master.isr, 2);
        assert_eq!(hardware.read_u8(0x20), 0);
    }

    #[test]
    fn poll_port_bus_acknowledges_master_and_slave_independently() {
        let mut hardware = IoBus::new();
        hardware.write_u8(0x21, 0);
        hardware.write_u8(0xa1, 0);
        hardware.pic.raise_irq(11);
        hardware.write_u8(0x20, 0x0c);
        hardware.write_u8(0xa0, 0x0c);
        assert_eq!(hardware.read_u8(0x20), 0x82);
        assert_eq!(hardware.pic.diagnostic_snapshot().slave.isr, 0);
        assert_eq!(hardware.read_u8(0xa1), 0x83);
        let snapshot = hardware.pic.diagnostic_snapshot();
        assert_eq!(snapshot.master.isr, 4);
        assert_eq!(snapshot.slave.isr, 8);
        assert_eq!(hardware.read_u8(0xa1), 0);
    }
}

#[cfg(test)]
mod pit_reprogram_latch_port_tests {
    use super::IoBus;

    #[test]
    fn guest_ports_read_back_the_new_count_after_mode_reprogram() {
        let mut hardware = IoBus::new();
        hardware.set_cycle(0);

        hardware.write_u8(0x43, 0x30); // channel 0, LSB/MSB, Mode 0
        hardware.write_u8(0x40, 0x34);
        hardware.write_u8(0x40, 0x12);
        hardware.write_u8(0x43, 0x00); // ordinary latch of 0x1234

        hardware.write_u8(0x43, 0x30); // same mode/access control word
        hardware.write_u8(0x40, 0x78);
        hardware.write_u8(0x40, 0x56);
        hardware.write_u8(0x43, 0xD2); // read-back count latch for channel 0

        let lsb = u16::from(hardware.read_u8(0x40));
        let msb = u16::from(hardware.read_u8(0x40));
        assert_eq!((msb << 8) | lsb, 0x5678);
    }
}

#[cfg(test)]
mod rtc_status_d_port_tests {
    use super::IoBus;
    use crate::cmos::regs;

    #[test]
    fn guest_ports_cannot_change_status_d() {
        let mut hardware = IoBus::new();
        hardware.write_u8(0x70, regs::STATUS_D);

        for value in 0_u8..=u8::MAX {
            hardware.write_u8(0x71, value);
            assert_eq!(
                hardware.read_u8(0x71),
                0x80,
                "Status D changed after guest wrote {value:#04x}"
            );
        }
    }
}

#[cfg(test)]
mod rtc_irq_transition_port_tests {
    use super::IoBus;
    use crate::cmos::regs;
    use crate::Cmos;

    fn write_cmos_register(hardware: &mut IoBus, register: u8, value: u8) {
        hardware.write_u8(0x70, register);
        hardware.write_u8(0x71, value);
    }

    #[test]
    fn pending_periodic_irqf_status_b_transitions_drive_irq8() {
        let mut hardware = IoBus::new();
        hardware.cmos = Cmos::new_with_fixed_boot_epoch(Some(0));
        for register in [regs::SECONDS_ALARM, regs::MINUTES_ALARM, regs::HOURS_ALARM] {
            write_cmos_register(&mut hardware, register, 0x01);
        }
        hardware.write_u8(0x21, 0xfb); // Unmask only the master cascade input.
        hardware.write_u8(0xa1, 0xfe); // Unmask slave IRQ0 (IRQ8).

        assert!(!hardware.cmos.tick_periodic(2_000));
        let pending = hardware.cmos.peek_pending_flags();
        assert_eq!(
            pending & 0x70,
            0x40,
            "a disabled periodic event must latch PF"
        );
        assert_eq!(
            pending & 0x80,
            0,
            "PIE is disabled before Status B enables it"
        );

        write_cmos_register(&mut hardware, regs::STATUS_B, 0x42);
        let snapshot = hardware.pic.diagnostic_snapshot();
        assert_eq!(
            snapshot.slave.irq_line & 1,
            1,
            "enabling pending PF asserts IRQ8"
        );
        assert_eq!(snapshot.slave.irr & 1, 1, "the PIC latches the IRQ8 edge");

        assert_eq!(hardware.pic.get_interrupt(), Some(0x70));
        assert_eq!(hardware.pic.diagnostic_snapshot().slave.irr & 1, 0);
        assert!(!hardware.cmos.tick_periodic(2_000));
        write_cmos_register(&mut hardware, regs::STATUS_B, 0x42);
        let snapshot = hardware.pic.diagnostic_snapshot();
        assert_eq!(snapshot.slave.irq_line & 1, 1, "IRQ8 remains level-held");
        assert_eq!(
            snapshot.slave.irr & 1,
            0,
            "an identical write and later tick add no edge"
        );

        write_cmos_register(&mut hardware, regs::STATUS_B, 0x02);
        let snapshot = hardware.pic.diagnostic_snapshot();
        assert_eq!(snapshot.slave.irq_line & 1, 0, "disabling PIE lowers IRQ8");
        assert_eq!(
            hardware.cmos.peek_pending_flags() & 0x70,
            0x40,
            "PF stays latched"
        );
        assert_eq!(hardware.cmos.peek_pending_flags() & 0x80, 0);

        write_cmos_register(&mut hardware, regs::STATUS_B, 0x42);
        let snapshot = hardware.pic.diagnostic_snapshot();
        assert_eq!(
            snapshot.slave.irq_line & 1,
            1,
            "re-enabling PIE reasserts IRQ8"
        );
        assert_eq!(
            snapshot.slave.irr & 1,
            1,
            "the new IRQ8 edge reaches the PIC"
        );
    }
}

#[cfg(test)]
mod triggered_count_only_tests {
    use super::IoBus;

    #[test]
    fn triggered_count_only_guest_channel2_out_and_gate() {
        for mode in [1, 5] {
            let mut io = IoBus::new();
            io.write_u8(0x43, 0xb0 | (mode << 1));
            io.write_u8(0x42, 40);
            io.write_u8(0x42, 0);
            io.write_u8(0x61, 0);
            io.write_u8(0x61, 1);
            io.set_cycle(20);
            io.write_u8(0x42, 90);
            io.write_u8(0x42, 0);
            io.set_cycle(40);
            assert_eq!(
                io.read_u8(0x61) & 0x20 != 0,
                mode == 1,
                "guest must observe original terminal edge"
            );
            io.set_cycle(41);
            assert_ne!(io.read_u8(0x61) & 0x20, 0);
            io.write_u8(0x61, 0);
            io.write_u8(0x61, 1);
            io.set_cycle(130);
            assert_eq!(io.read_u8(0x61) & 0x20 != 0, mode == 5);
            io.set_cycle(131);
            assert_eq!(io.read_u8(0x61) & 0x20 != 0, mode == 1);
        }
    }
}

#[cfg(test)]
mod rtc_set_fields_tests {
    use super::IoBus;
    use crate::{cmos::regs, Cmos};

    fn write(io: &mut IoBus, reg: u8, value: u8) {
        io.write_u8(0x70, reg | 0x80);
        io.write_u8(0x71, value);
    }

    fn read(io: &mut IoBus, reg: u8) -> u8 {
        io.write_u8(0x70, reg | 0x80);
        io.read_u8(0x71)
    }

    #[test]
    fn rtc_set_fields_guest_ports_and_wrapper_checkpoint() {
        let mut io = IoBus::new();
        io.cmos = Cmos::new_with_fixed_boot_epoch(Some(0));
        io.cmos.set_rtc_date_bcd(0x20, 0x26, 0x02, 0x28);
        io.cmos.set_rtc_time_bcd(0x12, 0x00, 0x07);
        write(&mut io, regs::STATUS_B, 0x82);
        write(&mut io, regs::DAY_OF_MONTH, 0x31);
        io.cmos.tick(5_000_000);
        let saved = io.cmos.save_state();
        io.cmos = Cmos::new_with_fixed_boot_epoch(Some(0));
        io.cmos.restore_state(saved);
        assert_eq!(
            read(&mut io, regs::DAY_OF_MONTH),
            0x31,
            "port reads restore independent day"
        );
        assert_eq!(read(&mut io, regs::MONTH), 0x02);
        assert_eq!(io.read_u8(0x70) & 0x80, 0x80, "checkpoint keeps NMI mask");
        write(&mut io, regs::MONTH, 0x03);
        write(&mut io, regs::STATUS_B, 0x02);
        assert_eq!(read(&mut io, regs::DAY_OF_MONTH), 0x31);
        assert_eq!(read(&mut io, regs::MONTH), 0x03);
        assert_eq!(read(&mut io, regs::SECONDS), 0x07, "release has no catchup");
        io.cmos.tick(1_000_000);
        assert_eq!(read(&mut io, regs::SECONDS), 0x08);
    }
}

#[cfg(test)]
mod mode0_first_byte_tests {
    use super::IoBus;

    #[test]
    fn mode0_first_byte_guest_channel2_out_and_gate() {
        for write_cycle in [20, 44] {
            let mut io = IoBus::new();
            for (port, value) in [(0x43, 0xb0), (0x42, 40), (0x42, 0), (0x61, 1)] {
                io.write_u8(port, value);
            }
            io.set_cycle(write_cycle);
            assert_eq!(io.read_u8(0x61) & 0x20 != 0, write_cycle >= 40);
            io.write_u8(0x42, 90);
            io.set_cycle(100_000);
            assert_eq!(
                io.read_u8(0x61) & 0x20,
                0,
                "guest OUT must stay low while MSB is missing"
            );
            io.write_u8(0x61, 0);
            io.write_u8(0x61, 1);
            io.set_cycle(200_000);
            assert_eq!(io.read_u8(0x61) & 0x20, 0);
            io.write_u8(0x42, 0);
            io.set_cycle(200_089);
            assert_eq!(io.read_u8(0x61) & 0x20, 0);
            io.set_cycle(200_090);
            assert_ne!(io.read_u8(0x61) & 0x20, 0);
        }
    }
}

#[cfg(test)]
mod cga_port_routing_tests {
    use super::IoBus;
    use gp64_memory::Memory;

    #[test]
    fn bios_mode04_cga_registers_are_reachable_through_io_bus() {
        let mut io = IoBus::new();
        let mut memory = Memory::new();
        assert!(io.vga.set_mode_bios(0x04));
        memory.write_u8(0xB8000, 0xAA); // Four color-2 pixels.

        io.write_u8(0x3D9, 0x00);
        assert_eq!(io.read_u8(0x3D9), 0x00);
        io.vga.render_scanline(0, &mut memory);
        let palette_zero_pixel = io.vga.framebuffer()[0];
        assert_eq!(palette_zero_pixel, 0xAA0000);

        io.write_u8(0x3D9, 0x20);
        assert_eq!(io.read_u8(0x3D9), 0x20);
        io.vga.render_scanline(0, &mut memory);
        let palette_one_pixel = io.vga.framebuffer()[0];
        assert_eq!(palette_one_pixel, 0xAA00AA);
        assert_ne!(palette_zero_pixel, palette_one_pixel);

        let enabled_mode_control = io.vga.get_cga_mode_control();
        let disabled_mode_control = enabled_mode_control & !0x08;
        io.write_u8(0x3D8, disabled_mode_control);
        assert_eq!(io.read_u8(0x3D8), disabled_mode_control);
        io.vga.render_scanline(0, &mut memory);
        assert_ne!(io.vga.framebuffer()[0], palette_one_pixel);

        io.write_u8(0x3D8, enabled_mode_control);
        assert_eq!(io.read_u8(0x3D8), enabled_mode_control);
        io.vga.render_scanline(0, &mut memory);
        assert_eq!(io.vga.framebuffer()[0], palette_one_pixel);
    }
}
