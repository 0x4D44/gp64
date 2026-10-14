use super::*;

fn completed_console_call(ax: u16, depth: u8) {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut pic = Pic::new();
    let indos = Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET);
    cpu.regs.ss = 0x2000;
    cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(0x2000);
    cpu.regs.set_sp(0x100);
    cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, 0x3000);
    memory.write_u8(indos, depth);
    memory.write_u8(Memory::seg_offset_to_linear(0x3000, 0x100), 0);
    for attempt in 0..3 {
        cpu.regs.set_ax(ax);
        cpu.regs.set_bx(0);
        cpu.regs.set_cx(0);
        cpu.regs.set_dx(if ax == 0x0c06 { 0xff } else { 0x100 });
        assert_eq!(
            dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
                .unwrap(),
            Int21Disposition::Iret
        );
        assert!(!cpu.halted, "immediate call must not wait");
        assert_eq!(
            memory.read_u8(indos),
            depth,
            "AX={ax:04x} attempt={attempt}"
        );
        if ax == 0x3f00 {
            assert_eq!(cpu.regs.ax(), 0);
            assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        }
    }
}

#[test]
fn indos_zero_length_buffered_input_completes() {
    for depth in [0, 3] {
        completed_console_call(0x0a00, depth);
    }
}

#[test]
fn indos_flush_subfunction_has_one_entry() {
    for depth in [0, 3] {
        completed_console_call(0x0c06, depth);
    }
}

#[test]
fn indos_zero_count_stdin_completes() {
    for depth in [0, 3] {
        completed_console_call(0x3f00, depth);
    }
}

#[test]
fn indos_retry_wait_releases_entry() {
    for skip in [false, true] {
        let mut dos = Dos::new();
        dos.skip_iret_sync = skip;
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        let (mut vga, mut keyboard) = create_test_io();
        let mut pic = Pic::new();
        let indos = Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET);
        memory.write_u8(indos, 3);
        cpu.regs.ss = 0x2000;
        cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(0x2000);
        cpu.regs.set_sp(0x100);
        cpu.regs.set_ip(0x202);
        memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), 0x202);
        cpu.regs.set_ax(0x3f00);
        cpu.regs.set_bx(0);
        cpu.regs.set_cx(1);
        cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, 0x3000);
        cpu.regs.set_dx(0x100);
        assert_eq!(
            dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
                .unwrap(),
            Int21Disposition::Iret
        );
        assert!(cpu.halted);
        assert_eq!(memory.read_u8(indos), 3, "retry skip={skip}");
        assert_eq!(
            if skip {
                cpu.regs.ip()
            } else {
                memory.read_u16_seg(cpu.regs.ss, cpu.regs.sp())
            },
            if skip { 0x202 } else { 0x200 }
        );
    }
}
