use gp64_machine::{InterruptAction, Machine, RunExit};

fn machine(code: &[u8]) -> Machine {
    let mut machine = Machine::with_rate(100_000_000);
    machine.cpu.regs.cs = 0;
    machine.cpu.regs.cs_cache.base = 0;
    machine.cpu.regs.ds = 0;
    machine.cpu.regs.ds_cache.base = 0;
    machine.cpu.regs.es = 0;
    machine.cpu.regs.es_cache.base = 0;
    machine.cpu.regs.ss = 0;
    machine.cpu.regs.ss_cache.base = 0;
    machine.cpu.regs.set_sp(0xff00);
    machine.cpu.regs.set_ip(0x1000);
    machine.memory.ram[0x1000..0x1000 + code.len()].copy_from_slice(code);
    machine
}

fn finish(machine: &mut Machine, single_step: bool) {
    loop {
        let end = machine.cycle() + if single_step { 1 } else { 100_000_000 };
        let exit = machine.run_until_with_hook(end, |_, _| InterruptAction::Yield);
        match exit {
            RunExit::SoftwareInterrupt { vector: 0xf1 } => break,
            RunExit::BudgetUsed if single_step => {}
            other => panic!("unexpected guest exit: {other:?}"),
        }
        assert!(machine.cycle() < 100_000_000);
    }
}

#[test]
fn status_polling_batches_accesses_without_changing_retrace_or_cycles() {
    // CLI; CLD; MOV DX,3DAh; MOV DI,2000h; MOV CX,16384;
    // IN AL,DX; STOSB; LOOP; INT F1h.
    let code = [
        0xfa, 0xfc, 0xba, 0xda, 3, 0xbf, 0, 0x20, 0xb9, 0, 0x40, 0xec, 0xaa, 0xe2, 0xfc, 0xcd, 0xf1,
    ];
    let mut reference = machine(&code);
    let mut batched = machine(&code);
    finish(&mut reference, true);
    finish(&mut batched, false);
    let samples = &batched.memory.ram[0x2000..0x6000];
    assert_eq!(samples, &reference.memory.ram[0x2000..0x6000]);
    assert!(samples.iter().any(|value| value & 8 != 0));
    assert!(samples.iter().any(|value| value & 8 == 0));
    assert_eq!(batched.cycle(), reference.cycle());
    assert_eq!(batched.completed_frames(), reference.completed_frames());
    assert!(batched.completed_frames() > 0);
    assert_eq!(batched.take_audio(), reference.take_audio());
    assert_eq!(batched.slices().io, 0, "status reads need no I/O service");
    assert!(batched.slices().total < 4096, "{:?}", batched.slices());
}

#[test]
fn unmasking_pending_irq_stops_before_the_next_instruction() {
    // Poll an inert status port before unmasking IRQ0 with IF already set.
    // The handler records the interrupted IP, then INT F1h ends the fixture.
    let code = [
        0xba, 0xda, 3, 0xb9, 0, 1, 0xec, 0xe2, 0xfd, 0xb0, 0xfe, 0xe6, 0x21, 0x90, 0xcd, 0xf1,
    ];
    let mut reference = machine(&code);
    let mut batched = machine(&code);
    for machine in [&mut reference, &mut batched] {
        machine.io.pic.write_data(0, 0xff);
        machine.io.pic.raise_irq(0);
        machine.cpu.interrupts_enabled = true;
        machine.cpu.regs.flags.insert(gp64_cpu::Flags::INTERRUPT);
        machine.memory.ram[0x20..0x24].copy_from_slice(&[0, 8, 0, 0]);
        // POP AX; MOV [0500h],AX; INT F1h.
        machine.memory.ram[0x800..0x806].copy_from_slice(&[0x58, 0xa3, 0, 5, 0xcd, 0xf1]);
    }
    finish(&mut reference, true);
    finish(&mut batched, false);
    assert_eq!(&batched.memory.ram[0x500..0x502], &[0x0d, 0x10]);
    assert_eq!(
        &batched.memory.ram[0x500..0x502],
        &reference.memory.ram[0x500..0x502]
    );
    assert_eq!(batched.cycle(), reference.cycle());
    assert_eq!(batched.slices().io, 1);
}

#[test]
fn special_mask_mode_delivers_pending_irq_before_the_next_instruction() {
    // In IRQ0 with IF=1: MOV AL,68h; OUT 20h,AL; NOP; JMP NOP.
    let code = [0xb0, 0x68, 0xe6, 0x20, 0x90, 0xeb, 0xfd];
    let mut reference = machine(&code);
    let mut batched = machine(&code);
    for machine in [&mut reference, &mut batched] {
        machine.io.pic.raise_irq(0);
        assert_eq!(machine.io.pic.get_interrupt(), Some(8));
        machine.io.pic.write_data(0, 0xf7); // Mask ISR0; enable pending IRQ3.
        machine.io.pic.raise_irq(3);
        assert!(!machine.io.pic.has_pending_irq());
        machine.cpu.interrupts_enabled = true;
        machine.cpu.regs.flags.insert(gp64_cpu::Flags::INTERRUPT);
        machine.memory.ram[0x2c..0x30].copy_from_slice(&[0, 8, 0, 0]);
        // POP AX; MOV [0500h],AX; INT F1h records IRQ3's interrupted IP.
        machine.memory.ram[0x800..0x806].copy_from_slice(&[0x58, 0xa3, 0, 5, 0xcd, 0xf1]);
    }
    finish(&mut reference, true);
    finish(&mut batched, false);
    assert_eq!(&reference.memory.ram[0x500..0x502], &[4, 0x10]);
    assert_eq!(
        &batched.memory.ram[0x500..0x502],
        &reference.memory.ram[0x500..0x502]
    );
    assert_eq!(batched.cycle(), reference.cycle());
}

#[test]
fn earlier_pit_deadline_delivers_irq_at_the_same_instruction() {
    // Load a long channel-zero one-shot, then replace it with a short one.
    let mut code = vec![
        0xb0, 0x30, 0xe6, 0x43, 0xb0, 0xff, 0xe6, 0x40, 0xe6, 0x40, 0xb0, 0x10, 0xe6, 0x43, 0xb0,
        4, 0xe6, 0x40,
    ];
    code.extend([0x90; 4096]);
    code.extend([0xcd, 0xf1]);
    let mut reference = machine(&code);
    let mut batched = machine(&code);
    for machine in [&mut reference, &mut batched] {
        machine.cpu.interrupts_enabled = true;
        machine.cpu.regs.flags.insert(gp64_cpu::Flags::INTERRUPT);
        machine.memory.ram[0x20..0x24].copy_from_slice(&[0, 8, 0, 0]);
        machine.memory.ram[0x800..0x806].copy_from_slice(&[0x58, 0xa3, 0, 5, 0xcd, 0xf1]);
    }
    finish(&mut reference, true);
    finish(&mut batched, false);
    let interrupted_ip = u16::from_le_bytes(reference.memory.ram[0x500..0x502].try_into().unwrap());
    assert!((0x1012..0x1412).contains(&interrupted_ip));
    assert!(
        reference.cycle() < 1000,
        "IRQ precedes the original audio deadline"
    );
    assert_eq!(
        &batched.memory.ram[0x500..0x502],
        &reference.memory.ram[0x500..0x502]
    );
    assert_eq!(batched.cycle(), reference.cycle());
}

#[test]
fn quiet_enable_polls_keep_the_pending_irq_shadow() {
    // CLI; MOV CX,4096; (STI; NOP; CLI; LOOP); unmask IRQ0; STI; NOP; INT F1h.
    let code = [
        0xfa, 0xb9, 0, 0x10, 0xfb, 0x90, 0xfa, 0xe2, 0xfb, 0xb0, 0xfe, 0xe6, 0x21, 0xfb, 0x90,
        0xcd, 0xf1,
    ];
    let mut reference = machine(&code);
    let mut batched = machine(&code);
    for machine in [&mut reference, &mut batched] {
        machine.io.pic.write_data(0, 0xff);
        machine.io.pic.raise_irq(0);
        machine.memory.ram[0x20..0x24].copy_from_slice(&[0, 8, 0, 0]);
        machine.memory.ram[0x800..0x806].copy_from_slice(&[0x58, 0xa3, 0, 5, 0xcd, 0xf1]);
    }
    finish(&mut reference, true);
    finish(&mut batched, false);
    assert_eq!(&batched.memory.ram[0x500..0x502], &[0x0f, 0x10]);
    assert_eq!(batched.cycle(), reference.cycle());
    assert!(batched.slices().total < 100, "{:?}", batched.slices());
}

#[test]
fn batched_io_delay_is_visible_to_the_timestamp_counter() {
    // CLI; IN AL,80h; RDTSC; MOV [0500h],EAX; INT F1h.
    let code = [0xfa, 0xe4, 0x80, 0x0f, 0x31, 0x66, 0xa3, 0, 5, 0xcd, 0xf1];
    let mut reference = machine(&code);
    let mut batched = machine(&code);
    for machine in [&mut reference, &mut batched] {
        machine.cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    }
    finish(&mut reference, true);
    finish(&mut batched, false);
    assert_eq!(
        &batched.memory.ram[0x500..0x504],
        &reference.memory.ram[0x500..0x504]
    );
    assert_eq!(batched.cycle(), reference.cycle());
}

#[test]
fn continuous_rtc_irq_keeps_enable_poll_delivery_points() {
    for code in [
        // Repeated enable/shadow polls.
        &[
            0xfa, 0xb9, 0, 0x10, 0xfb, 0x90, 0xfa, 0xe2, 0xfb, 0xcd, 0xf1,
        ][..],
        // A status read's I/O delay crosses the update-ended edge.
        &[0xba, 0xda, 3, 0xfb, 0x90, 0xec, 0xeb, 0xfd][..],
    ] {
        let mut reference = machine(code);
        let mut batched = machine(code);
        for machine in [&mut reference, &mut batched] {
            machine.io.cmos = gp64_hw::cmos::Cmos::new_with_fixed_boot_epoch(Some(0));
            machine.io.cmos.advance_clock(0);
            machine.io.cmos.advance_clock(999_999);
            machine.io.write_u8(0x70, gp64_hw::cmos::regs::STATUS_B);
            machine.io.write_u8(0x71, 0x12); // UIE, 24-hour clock.
            machine.io.pic.write_data(0, 0xfb);
            machine.io.pic.write_data(1, 0xfe);
            machine.memory.ram[0x1c0..0x1c4].copy_from_slice(&[0, 8, 0, 0]);
            machine.memory.ram[0x800..0x802].copy_from_slice(&[0xcd, 0xf1]);
        }
        finish(&mut reference, true);
        finish(&mut batched, false);
        assert_eq!(batched.cpu.regs.ip(), 0x802);
        assert_eq!(batched.cycle(), reference.cycle());
    }
}
