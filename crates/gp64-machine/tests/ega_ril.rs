//! Synthetic BIOS register-interface requests and VGA memory accesses.
use gp64_bios::Bios;
use gp64_machine::{Machine, RunExit};

fn bios_call(machine: &mut Machine) {
    Bios::new()
        .int10(&mut machine.cpu, &mut machine.memory, &mut machine.io.vga)
        .unwrap();
}

fn advance(machine: &mut Machine) {
    assert!(matches!(machine.run_for(0), RunExit::BudgetUsed));
    assert!(!machine.io.vga.pipeline_dirty());
}

fn setup() -> Machine {
    let mut machine = Machine::new();
    machine.cpu.regs.set_ax(0x000d);
    bios_call(&mut machine);
    advance(&mut machine);
    machine
}

fn single(machine: &mut Machine, group: u16, register: u8, value: u8) {
    machine.cpu.regs.set_ah(0xf1);
    machine.cpu.regs.set_dx(group);
    machine.cpu.regs.set_bl(register);
    machine.cpu.regs.set_bh(value);
    bios_call(machine);
}

fn request(machine: &mut Machine, service: u8, group: u16, register: u8, value: u8) {
    const BUFFER: usize = 0x600;
    if service == 0xf1 {
        single(machine, group, register, value);
        return;
    }
    machine.cpu.regs.es = 0;
    machine.cpu.regs.set_bx(BUFFER as u16);
    machine.cpu.regs.set_dx(group);
    match service {
        0xf3 => {
            machine.cpu.regs.set_ch(register);
            machine.cpu.regs.set_cl(1);
            machine.memory.write_u8(BUFFER, value);
        }
        0xf5 => {
            machine.cpu.regs.set_cx(1);
            machine.memory.write_u16(BUFFER, group);
            machine.memory.write_u8(BUFFER + 2, register);
            machine.memory.write_u8(BUFFER + 3, value);
        }
        0xf6 => {
            // Define a full default group, then revert it after its definition
            // has left the live register state and memory pipeline unchanged.
            let mut defaults = match group {
                0x08 => machine.io.vga.get_sequencer_regs()[..5].to_vec(),
                0x10 => machine.io.vga.get_graphics_regs()[..9].to_vec(),
                _ => unreachable!(),
            };
            defaults[usize::from(register)] = value;
            for (index, byte) in defaults.into_iter().enumerate() {
                machine.memory.write_u8(BUFFER + index, byte);
            }
            machine.cpu.regs.set_ah(0xf7);
            bios_call(machine);
            assert!(!machine.io.vga.pipeline_dirty());
            let live = if group == 0x08 {
                machine.io.vga.get_sequencer_regs()[usize::from(register)]
            } else {
                machine.io.vga.get_graphics_regs()[usize::from(register)]
            };
            assert_ne!(live, value);
        }
        _ => unreachable!(),
    }
    machine.cpu.regs.set_ah(service);
    bios_call(machine);
}

fn map_mask_behavior(service: u8) {
    let mut machine = setup();
    request(&mut machine, service, 0x08, 2, 1);
    advance(&mut machine);
    // CPU-visible VGA write must use the BIOS-selected map mask.
    machine.cpu.regs.set_al(0x5a);
    guest_access(&mut machine, 0, 0x88);
    assert_eq!(machine.memory.read_video_planar(0, 0), 0x5a);
    for plane in 1..4 {
        assert_eq!(machine.memory.read_video_planar(0, plane), 0);
    }
}

fn guest_access(machine: &mut Machine, offset: u16, opcode: u8) {
    // mov es:[di],al (88) or mov al,es:[di] (8a); hlt.
    let code = [0x26, opcode, 0x05, 0xf4];
    machine.memory.ram[0x1000..0x1000 + code.len()].copy_from_slice(&code);
    machine.cpu.regs.cs = 0;
    machine.cpu.regs.cs_cache.base = 0;
    machine.cpu.regs.set_ip(0x1000);
    machine.cpu.regs.es = 0xa000;
    machine.cpu.regs.es_cache.base = 0xa0000;
    machine.cpu.regs.set_di(offset);
    machine.cpu.interrupts_enabled = false;
    assert!(matches!(machine.run_for(100), RunExit::BudgetUsed));
    assert!(machine.cpu.halted);
}

fn read_plane_behavior(service: u8) {
    let mut machine = setup();
    machine.memory.write_video_planar(1, 0x35, 1);
    machine.memory.write_video_planar(1, 0xa6, 4);
    request(&mut machine, service, 0x10, 4, 2);
    advance(&mut machine);
    // CPU-visible VGA read must use the BIOS-selected graphics read plane.
    guest_access(&mut machine, 1, 0x8a);
    assert_eq!(machine.cpu.regs.al(), 0xa6);
}

#[test]
fn ril_single_updates_map_mask() {
    map_mask_behavior(0xf1);
}

#[test]
fn ril_range_updates_map_mask() {
    map_mask_behavior(0xf3);
}

#[test]
fn ril_set_updates_map_mask() {
    map_mask_behavior(0xf5);
}

#[test]
fn ril_defaults_update_map_mask() {
    map_mask_behavior(0xf6);
}

#[test]
fn ril_single_updates_read_plane() {
    read_plane_behavior(0xf1);
}

#[test]
fn ril_range_updates_read_plane() {
    read_plane_behavior(0xf3);
}

#[test]
fn ril_set_updates_read_plane() {
    read_plane_behavior(0xf5);
}

#[test]
fn ril_defaults_update_read_plane() {
    read_plane_behavior(0xf6);
}

fn timing_behavior(group: u16, register: u8, value: u8, index_port: u16, data_port: u16) {
    let mut bios = setup();
    let mut ports = setup();
    let before = bios.io.vga.refresh_millihz();
    if group == 0 {
        // Port writes obey CR11 protection; the BIOS bulk setter deliberately
        // restores raw state, so unlock only the port oracle before its write.
        let unlocked = ports.io.vga.get_crtc_regs()[0x11] & 0x7f;
        ports.io.vga.write_port(index_port, 0x11);
        ports.io.vga.write_port(data_port, unlocked);
    }
    if index_port != 0 {
        ports.io.vga.write_port(index_port, register);
    }
    ports.io.vga.write_port(data_port, value);
    advance(&mut ports);
    assert_ne!(ports.io.vga.refresh_millihz(), before);
    single(&mut bios, group, register, value);
    advance(&mut bios);
    assert_eq!(
        bios.io.vga.refresh_millihz(),
        ports.io.vga.refresh_millihz()
    );
    assert_eq!(
        bios.io.vga.timing_cycles_per_frame,
        ports.io.vga.timing_cycles_per_frame
    );
    assert_eq!(bios.io.vga.pending_timing_cycles_per_frame, None);
}

#[test]
fn ril_sequencer_updates_frame_timing() {
    timing_behavior(0x08, 1, 0x01, 0x3c4, 0x3c5);
}

#[test]
fn ril_crtc_updates_frame_timing() {
    timing_behavior(0x00, 0, 0x75, 0x3d4, 0x3d5);
}

#[test]
fn ril_misc_updates_frame_timing() {
    let value = setup().io.vga.get_misc_output() ^ 4;
    timing_behavior(0x20, 0, value, 0, 0x3c2);
}

fn transient_mode13() -> Machine {
    let mut machine = setup();
    machine.cpu.regs.set_ax(0x0013);
    bios_call(&mut machine);
    // Prepare the chain-4-disabled mode-set transient with port writes. It
    // remains linear until CRTC scan addressing or attribute mode changes.
    let memory_mode = machine.io.vga.get_sequencer_regs()[4] & !8;
    machine.io.vga.write_port(0x3c4, 4);
    machine.io.vga.write_port(0x3c5, memory_mode);
    machine.io.vga.write_port(0x3c4, 2);
    machine.io.vga.write_port(0x3c5, 1);
    assert!(!machine.io.vga.is_mode_x());
    advance(&mut machine);
    machine
}

fn assert_planar_write(mut machine: Machine) {
    advance(&mut machine);
    machine.cpu.regs.set_al(0x5a);
    guest_access(&mut machine, 0, 0x88);
    assert_eq!(machine.memory.read_video_planar(0, 0), 0x5a);
    for plane in 1..4 {
        assert_eq!(machine.memory.read_video_planar(0, plane), 0);
    }
}

#[test]
fn ril_attribute_updates_memory_dispatch() {
    let mut machine = transient_mode13();
    let value = machine.io.vga.get_attribute_regs()[0x10] & !0x40;
    single(&mut machine, 0x18, 0x10, value);
    assert!(!machine.io.vga.is_256_color_mode());
    assert_planar_write(machine);
}

#[test]
fn ril_crtc_updates_memory_dispatch() {
    let mut machine = transient_mode13();
    let value = machine.io.vga.get_crtc_regs()[9] & !0x1f;
    single(&mut machine, 0, 9, value);
    assert!(machine.io.vga.is_mode_x());
    assert_planar_write(machine);
}
