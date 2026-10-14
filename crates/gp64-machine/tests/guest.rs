use gp64_bios::Bios;
use gp64_machine::{InterruptAction, Machine, RunExit};

const TICKS: usize = 0x500;
fn machine(code: &[u8], rate: u64) -> Machine {
    let mut m = Machine::with_rate(rate);
    m.cpu.regs.cs = 0;
    m.cpu.regs.cs_cache.base = 0;
    m.cpu.regs.ds = 0;
    m.cpu.regs.ds_cache.base = 0;
    m.cpu.regs.ss = 0;
    m.cpu.regs.ss_cache.base = 0;
    m.cpu.regs.set_sp(0xff00);
    m.cpu.regs.set_ip(0x1000);
    m.memory.ram[0x1000..0x1000 + code.len()].copy_from_slice(code);
    m
}
fn ticks(m: &Machine) -> u16 {
    u16::from_le_bytes([m.memory.ram[TICKS], m.memory.ram[TICKS + 1]])
}
fn install_irq(m: &mut Machine) {
    // IRQ0: push ax; inc word [0500h]; mov al,20h; out 20h,al; pop ax; iret
    let handler = [
        0x50, 0xff, 0x06, 0x00, 0x05, 0xb0, 0x20, 0xe6, 0x20, 0x58, 0xcf,
    ];
    m.memory.ram[0x800..0x800 + handler.len()].copy_from_slice(&handler);
    m.memory.ram[8 * 4..8 * 4 + 4].copy_from_slice(&[0x00, 0x08, 0, 0]);
}
fn pit_program(divisor: u16, halt: bool) -> Vec<u8> {
    let [lo, hi] = divisor.to_le_bytes();
    let mut code = vec![
        0xfa, // cli
        0xb0, 0xfe, 0xe6, 0x21, // mov al,feh; out 21h,al (only IRQ0 enabled)
        0xb0, 0x34, 0xe6, 0x43, // mov al,34h; out 43h,al (ch0, lo/hi, rate generator)
        0xb0, lo, 0xe6, 0x40, // mov al,lo; out 40h,al
        0xb0, hi, 0xe6, 0x40, // mov al,hi; out 40h,al
        0xfb, // sti
    ];
    if halt {
        code.extend([0xf4, 0xeb, 0xfd]);
    }
    // hlt; jmp hlt
    else {
        code.extend([0x90, 0xeb, 0xfd]);
    } // nop; jmp nop
    code
}
fn run(m: &mut Machine, cycles: u64) {
    assert!(matches!(m.run_for(cycles), RunExit::BudgetUsed));
}

#[test]
fn text_planar_routing_guest_out_mov_roundtrip() {
    fn indexed(code: &mut Vec<u8>, port: u16, index: u8, value: u8) {
        let [lo, hi] = port.to_le_bytes();
        // MOV DX,port; MOV AL,index; OUT DX,AL; INC DX; MOV AL,value; OUT DX,AL.
        code.extend([0xba, lo, hi, 0xb0, index, 0xee, 0x42, 0xb0, value, 0xee]);
    }

    let initial = [0x11, 0x22, 0x33, 0x44];
    for mask in [0x0c, 0, 4, 0x0f] {
        let mut code = vec![0xfa]; // CLI: isolate the synthetic guest from IRQ handlers.
        indexed(&mut code, 0x3c4, 4, 6); // Sequential CPU addressing, chain four disabled.
        indexed(&mut code, 0x3c4, 2, mask);
        for (index, value) in [(1, 0), (3, 0), (4, 2), (5, 0), (6, 4), (8, 0xff)] {
            indexed(&mut code, 0x3ce, index, value);
        }
        // MOV AX,A000h; MOV DS,AX; MOV AL,5Ah; MOV [0000h],AL.
        code.extend([0xb8, 0, 0xa0, 0x8e, 0xd8, 0xb0, 0x5a, 0xa2, 0, 0]);
        for plane in 0..4 {
            indexed(&mut code, 0x3ce, 4, plane);
            // Read the selected plane; store the result through CS, whose base remains zero.
            code.extend([0xa0, 0, 0, 0x2e, 0xa2, plane, 6]);
        }
        indexed(&mut code, 0x3c4, 4, 2); // Restore ordinary odd/even text addressing.
        indexed(&mut code, 0x3c4, 2, 3);
        indexed(&mut code, 0x3ce, 5, 0x10);
        indexed(&mut code, 0x3ce, 6, 0x0e);
        // MOV AX,B800h; MOV DS,AX; MOV AX,1E41h; MOV [0000h],AX;
        // MOV AX,[0000h]; MOV CS:[0610h],AX; HLT.
        code.extend([
            0xb8, 0, 0xb8, 0x8e, 0xd8, 0xb8, 0x41, 0x1e, 0xa3, 0, 0, 0xa1, 0, 0, 0x2e, 0xa3, 0x10,
            6, 0xf4,
        ]);

        let mut m = machine(&code, 20_000_000);
        assert!(m.io.vga.set_mode_bios(0x03));
        m.memory
            .apply_vga_pipeline(&m.io.vga.build_pipeline_state());
        for (plane, value) in initial.into_iter().enumerate() {
            m.memory.plane_mut(plane)[0] = value;
        }
        m.memory.ram[0xa0000] = 0xee;
        run(&mut m, 20_000);
        assert!(m.cpu.halted, "guest did not finish, mask={mask:x}");
        assert_eq!(m.io.vga.mode(), 0x03);
        for (plane, old) in initial.into_iter().enumerate() {
            let expected = if mask & (1 << plane) != 0 { 0x5a } else { old };
            assert_eq!(
                m.memory.plane(plane)[0],
                expected,
                "plane={plane}, mask={mask:x}"
            );
            assert_eq!(
                m.memory.ram[0x600 + plane],
                expected,
                "guest read plane={plane}, mask={mask:x}"
            );
        }
        assert_eq!(&m.memory.ram[0xb8000..0xb8002], &[0x41, 0x1e]);
        assert_eq!(&m.memory.ram[0x610..0x612], &[0x41, 0x1e]);
    }
}

#[test]
fn cga_packed_routing_guest_bios_and_scanout_roundtrip() {
    for mode in [0x04, 0x05, 0x06] {
        let (pixels_per_byte, framebuffer_width, packed_bytes, rgb_by_color) = match mode {
            0x04 => (
                4,
                320,
                [0xe4, 0x39, 0x4e, 0x93],
                [0x000000, 0x55ffff, 0xff55ff, 0xffffff],
            ),
            0x05 => (
                4,
                320,
                [0xe4, 0x39, 0x4e, 0x93],
                [0x000000, 0x55ffff, 0xff5555, 0xffffff],
            ),
            0x06 => (
                8,
                640,
                [0xb2, 0x4d, 0xe1, 0x36],
                [0x000000, 0xffffff, 0x000000, 0x000000],
            ),
            _ => unreachable!(),
        };
        let sample_width = pixels_per_byte * 2;
        let scratch_base = 0x700usize;
        let mut code = vec![0xfa]; // CLI: isolate the guest from IRQ handlers.

        // Select the CGA BIOS mode, then write two packed bytes to each bank
        // through ordinary guest MOV instructions at B800:0000 and B800:2000.
        code.extend([0xb8, mode, 0x00, 0xcd, 0x10]);
        code.extend([0xb8, 0x00, 0xb8, 0x8e, 0xd8]); // mov ax,b800h; mov ds,ax
        for (index, value) in packed_bytes.into_iter().enumerate() {
            let offset = match index {
                0 => 0x0000u16,
                1 => 0x0001,
                2 => 0x2000,
                3 => 0x2001,
                _ => unreachable!(),
            };
            let [lo, hi] = offset.to_le_bytes();
            code.extend([0xc6, 0x06, lo, hi, value]); // mov byte [disp16],imm8
        }

        // Read each pixel in two adjacent packed bytes on both scanlines via
        // the BIOS. Save AL through CS so DS can remain the B800 segment.
        for y in 0..2u16 {
            for x in 0..sample_width as u16 {
                code.extend([0xb8, 0x00, 0x0d]); // mov ax,0d00h
                code.extend([0xbb, 0x00, 0x00]); // mov bx,0 (page 0)
                let [x_lo, x_hi] = x.to_le_bytes();
                let [y_lo, y_hi] = y.to_le_bytes();
                code.extend([0xb9, x_lo, x_hi, 0xba, y_lo, y_hi, 0xcd, 0x10]);
                let slot = scratch_base + y as usize * sample_width + x as usize;
                let [slot_lo, slot_hi] = (slot as u16).to_le_bytes();
                code.extend([0x2e, 0xa2, slot_lo, slot_hi]); // mov cs:[disp16],al
            }
        }
        code.push(0xf4); // hlt

        let mut m = machine(&code, 20_000_000);
        let mut bios = Bios::new();
        bios.init_ivt(&mut m.memory, false);
        let exit = m.run_until_with_hook(m.cycle().saturating_add(500_000), |m, vector| {
            if vector == 0x10 {
                bios.int10(&mut m.cpu, &mut m.memory, &mut m.io.vga)
                    .expect("INT 10h handler");
                InterruptAction::Handled
            } else {
                InterruptAction::Deliver
            }
        });
        assert!(
            matches!(exit, RunExit::BudgetUsed | RunExit::Halted),
            "guest failed while running in mode {mode:02x}: {exit:?}"
        );
        assert!(m.cpu.halted, "guest did not finish in mode {mode:02x}");
        assert_eq!(m.io.vga.mode(), mode);

        for (index, expected_byte) in packed_bytes.into_iter().enumerate() {
            let offset = match index {
                0 => 0x0000usize,
                1 => 0x0001,
                2 => 0x2000,
                3 => 0x2001,
                _ => unreachable!(),
            };
            assert_eq!(
                m.memory.ram[0xb8000 + offset],
                expected_byte,
                "guest MOV packed byte in mode {mode:02x}, bank byte {index}"
            );
        }

        let mut expected_pixels = Vec::with_capacity(2 * sample_width);
        for y in 0..2usize {
            for x in 0..sample_width {
                let byte = packed_bytes[y * 2 + x / pixels_per_byte];
                let shift = if pixels_per_byte == 4 {
                    6 - (x % pixels_per_byte) * 2
                } else {
                    7 - (x % pixels_per_byte)
                };
                expected_pixels.push((byte >> shift) & if pixels_per_byte == 4 { 3 } else { 1 });
            }
        }

        let actual_pixels = &m.memory.ram[scratch_base..scratch_base + expected_pixels.len()];
        assert_eq!(
            actual_pixels, expected_pixels,
            "BIOS AH=0D did not read the guest's packed pixels and neighbors in mode {mode:02x}"
        );

        for y in 0..2usize {
            m.io.vga.render_scanline(y as u32, &mut m.memory);
            for x in 0..sample_width {
                let expected_color = rgb_by_color[expected_pixels[y * sample_width + x] as usize];
                assert_eq!(
                    m.io.vga.framebuffer()[y * framebuffer_width + x],
                    expected_color,
                    "scanout mode {mode:02x}, x={x}, y={y}"
                );
            }
        }
    }
}

#[test]
fn pit_repeated_irq_exact() {
    let mut m = machine(&pit_program(1193, false), 20_000_000);
    install_irq(&mut m);
    run(&mut m, 4_000_000); // 200 ms: floor(0.2 * 1193182 / 1193) = 200
    assert_eq!(ticks(&m), 200);
    assert_eq!(m.io.pic.get_isr(), 0); // guest EOI really released the input
}
#[test]
fn pit_reprogram_mid_run() {
    let mut m = machine(&pit_program(1193, true), 20_000_000);
    install_irq(&mut m);
    run(&mut m, 4_000_000);
    assert_eq!(ticks(&m), 200);
    let code = pit_program(2386, true);
    m.memory.ram[0x1200..0x1200 + code.len()].copy_from_slice(&code);
    m.cpu.regs.set_ip(0x1200);
    m.cpu.halted = false;
    run(&mut m, 4_000_000); // next 200 ms: 100 ticks at the new divisor
    assert_eq!(ticks(&m), 300);
}
#[test]
fn hlt_wakes_without_losing_ticks() {
    let mut halted = machine(&pit_program(1193, true), 20_000_000);
    let mut spinning = machine(&pit_program(1193, false), 20_000_000);
    install_irq(&mut halted);
    install_irq(&mut spinning);
    for _ in 0..100 {
        run(&mut halted, 40_000);
        run(&mut spinning, 40_000);
    }
    assert_eq!(ticks(&halted), 200);
    assert_eq!(ticks(&halted), ticks(&spinning));
    assert!(halted.cpu.halted);
}
#[test]
fn retrace_poll_counts_current_mode_frames() {
    // mov dx,3dah; xor bl,bl;
    // poll: in al,dx; and al,8; cmp al,bl; je poll;
    // mov bl,al; test al,al; jz poll; inc word [0500h]; jmp poll
    let code = [
        0xba, 0xda, 0x03, 0x30, 0xdb, 0xec, 0x24, 0x08, 0x38, 0xd8, 0x74, 0xf9, 0x88, 0xc3, 0x84,
        0xc0, 0x74, 0xf3, 0xff, 0x06, 0x00, 0x05, 0xeb, 0xed,
    ];
    for mode in [0x03, 0x13, 0x12] {
        let mut m = machine(&code, 20_000_000);
        assert!(m.io.vga.set_mode_bios(mode));
        let hz = m.io.vga.refresh_millihz();
        run(&mut m, 10_000_000); // 500 ms, starting inside the first retrace pulse
        assert_eq!(u64::from(ticks(&m)), u64::from(hz) / 2000 + 1);
        assert_eq!(m.completed_frames(), u64::from(hz) / 2000);
    }
}

#[test]
fn retrace_interrupt_uses_slave_irq9_and_guest_acknowledges() {
    // Enable the cascade and slave IRQ1, arm CR11, then wait for interrupts.
    let code = [
        0xfa, 0xb0, 0xfb, 0xe6, 0x21, 0xb0, 0xfd, 0xe6, 0xa1, 0xba, 0xd4, 0x03, 0xb0, 0x11, 0xee,
        0x42, 0xb0, 0x10, 0xee, 0xfb, 0xf4, 0xeb, 0xfd,
    ];
    let mut m = machine(&code, 20_000_000);
    // Count, acknowledge CR11 by clearing bit 4, rearm, EOI slave then master.
    let handler = [
        0x50, 0x52, 0xff, 0x06, 0, 5, 0xba, 0xd4, 3, 0xb0, 0x11, 0xee, 0x42, 0xb0, 0, 0xee, 0xb0,
        0x10, 0xee, 0xb0, 0x20, 0xe6, 0xa0, 0xe6, 0x20, 0x5a, 0x58, 0xcf,
    ];
    m.memory.ram[0x800..0x800 + handler.len()].copy_from_slice(&handler);
    let vector = m.io.pic.diagnostic_snapshot().slave.vector_offset + 1;
    m.memory.write_u32(usize::from(vector) * 4, 0x800);
    run(&mut m, 10_000_000);
    assert!(m.completed_frames() > 1);
    assert_eq!(u64::from(ticks(&m)), m.completed_frames());
    let pic = m.io.pic.diagnostic_snapshot();
    assert_eq!(pic.slave.irq_line & 2, 0);
    assert_eq!(pic.slave.isr, 0);
    assert_eq!(pic.master.isr, 0);
}
#[test]
fn adlib_detection_in_loop_delay() {
    // mov dx,388h; mov al,4; out dx,al; inc dx; mov al,60h; out dx,al;
    // mov al,80h; out dx,al; dec dx; in al,dx; mov [0500h],al;
    // program timer1 preset FFh, start (control 21h); delay 200 INs; read status.
    let code = [
        0xba, 0x88, 0x03, 0xb0, 4, 0xee, 0x42, 0xb0, 0x60, 0xee, 0xb0, 0x80, 0xee, 0x4a, 0xec,
        0xa2, 0x00, 0x05, 0xb0, 2, 0xee, 0x42, 0xb0, 0xff, 0xee, 0x4a, 0xb0, 4, 0xee, 0x42, 0xb0,
        0x21, 0xee, 0x4a, 0xb9, 200, 0, 0xec, 0xe2, 0xfd, 0xec, 0xa2, 0x01, 0x05, 0xfa, 0xf4,
    ];
    let mut m = machine(&code, 20_000_000);
    run(&mut m, 20_000);
    assert_eq!(&m.memory.ram[TICKS..TICKS + 2], &[0, 0xc0]);
}
#[test]
fn open_bus_returns_ff() {
    // mov dx,1234h; in al,dx; mov [0500h],al; cli; hlt
    let code = [0xba, 0x34, 0x12, 0xec, 0xa2, 0x00, 0x05, 0xfa, 0xf4];
    let mut m = machine(&code, 20_000_000);
    run(&mut m, 1000);
    assert_eq!(m.memory.ram[TICKS], 0xff);
}
#[test]
fn sound_blaster_dsp_handshake_through_ports() {
    let mut m = machine(&[0xf4], 20_000_000);
    m.io.write_u8(0x226, 1);
    m.io.write_u8(0x226, 0);
    assert_eq!(m.io.read_u8(0x22e) & 0x80, 0x80);
    assert_eq!(m.io.read_u8(0x22a), 0xaa);
    m.io.write_u8(0x22c, 0xe1);
    assert_eq!(m.io.read_u8(0x22a), 2);
    assert_eq!(m.io.read_u8(0x22a), 1);
}
#[test]
fn sound_blaster_irq_line_follows_dsp_and_status_read() {
    let mut m = machine(&[0xf4], 20_000_000);
    m.io.write_u8(0x22c, 0xf2);
    assert_ne!(m.io.pic.get_irr() & 0x80, 0);
    assert!(m.io.sound_blaster.irq_asserted());
    m.io.read_u8(0x22e);
    assert!(!m.io.sound_blaster.irq_asserted());
}
#[test]
fn opl_timer_flag_visible_at_every_fm_port_alias() {
    for status_port in [0x388u16, 0x220, 0x222, 0x228] {
        let mut m = machine(&[0xf4], 20_000_000);
        let (index, data) = match status_port {
            0x388 => (0x388, 0x389),
            port => (port, port + 1),
        };
        let write = |m: &mut Machine, reg: u8, value: u8| {
            m.io.write_u8(index, reg);
            m.io.write_u8(data, value);
        };
        write(&mut m, 4, 0x60); // reset timers and flags
        write(&mut m, 4, 0x80);
        assert_eq!(m.io.read_u8(status_port) & 0xe0, 0, "port {status_port:#x}");
        write(&mut m, 2, 0xff);
        write(&mut m, 4, 0x21); // start timer 1
        m.io.set_cycle(2_000_000);
        assert_eq!(
            m.io.read_u8(status_port) & 0xe0,
            0xc0,
            "port {status_port:#x}"
        );
    }
}
#[test]
fn same_guest_time_at_two_cpu_rates() {
    for rate in [10_000_000, 20_000_000] {
        let mut m = machine(&pit_program(1193, true), rate);
        install_irq(&mut m);
        run(&mut m, rate); // one emulated second, irrespective of instruction throughput
        assert_eq!(ticks(&m), 1000);
    }
}

#[test]
fn runtime_clock_change_preserves_device_time() {
    let mut m = machine(&pit_program(1193, true), 20_000_000);
    install_irq(&mut m);
    run(&mut m, 5_000_000);
    assert_eq!(ticks(&m), 250);
    m.set_cycles_per_second(10_000_000);
    run(&mut m, 2_500_000);
    assert_eq!(ticks(&m), 500);
    let samples = m.take_audio();
    assert!((samples.len() as i64 - 24_858).abs() <= 1);
    assert!(samples.windows(2).all(|pair| pair[0].cycle < pair[1].cycle));
}
#[test]
fn frame_dimensions_palette_and_planes() {
    let mut m = machine(&[0xfa, 0xf4], 20_000_000); // cli; hlt
    assert!(m.io.vga.set_mode_bios(0x13));
    m.memory
        .apply_vga_pipeline(&m.io.vga.build_pipeline_state());
    m.io.vga.set_dac_reg(17, 63, 31, 7);
    m.io.vga.write_pixel(0, 3, 4, 17, &mut m.memory);
    let frame = m.frame();
    assert_eq!((frame.width, frame.height), (320, 200));
    let (r, g, b) = frame.palette[usize::from(frame.pixels[4 * 320 + 3])];
    assert_eq!((r, g, b), m.io.vga.get_dac_entries()[17]);
    assert!(m.io.vga.set_mode_bios(0x03));
    let frame = m.frame();
    assert_eq!((frame.width, frame.height), m.io.vga.dimensions());
    assert_ne!(frame.height, 200);
    assert_eq!(frame.pixels.len(), frame.width * frame.height);
    m.io.vga.set_dac_entries(&[(255, 0, 0); 256]);
    m.io.vga.write_port(0x3c4, 1);
    m.io.vga.write_port(0x3c5, 0x20); // sequencer clocking: blank display
    let frame = m.frame();
    assert!(frame
        .pixels
        .iter()
        .all(|&index| frame.palette[usize::from(index)] == (0, 0, 0)));
}
#[test]
fn typed_exception_and_software_interrupt_hook() {
    // xor ax,ax; div al (#DE, no vector installed)
    let mut m = machine(&[0x31, 0xc0, 0xf6, 0xf0], 20_000_000);
    assert!(matches!(
        m.run_for(1000),
        RunExit::UnhandledException { vector: 0, .. }
    ));
    // int 80h; mov [0500h],al; cli; hlt
    let mut m = machine(&[0xcd, 0x80, 0xa2, 0, 5, 0xfa, 0xf4], 20_000_000);
    assert!(matches!(
        m.run_until_with_hook(1000, |m, vector| {
            assert_eq!(vector, 0x80);
            m.cpu.regs.set_al(42);
            gp64_machine::InterruptAction::Handled
        }),
        RunExit::BudgetUsed
    ));
    assert_eq!(m.memory.ram[TICKS], 42);
    // int 80h (dispatcher asks to return to its caller)
    let mut m = machine(&[0xcd, 0x80], 20_000_000);
    assert!(matches!(
        m.run_until_with_hook(1000, |_, _| gp64_machine::InterruptAction::Yield),
        RunExit::SoftwareInterrupt { vector: 0x80 }
    ));
}
#[test]
fn speaker_short_note_is_retained() {
    // cli; program PIT2 mode3 divisor 1193; gate+speaker on; 2000 INs; gate off; hlt
    let code = [
        0xfa, 0xb0, 0xb6, 0xe6, 0x43, 0xb0, 0xa9, 0xe6, 0x42, 0xb0, 4, 0xe6, 0x42, 0xb0, 3, 0xe6,
        0x61, 0xb9, 0xd0, 7, 0xe4, 0x61, 0xe2, 0xfc, 0xb0, 0, 0xe6, 0x61, 0xf4,
    ];
    let mut m = machine(&code, 20_000_000);
    run(&mut m, 200_000); // 10 ms; the note occupies about 3 ms inside it
    let samples = m.take_audio();
    assert!(samples.iter().any(|sample| sample.speaker.abs() > 0.01));
    assert_eq!(samples.len(), 497);
    assert!(samples.last().unwrap().speaker.abs() < 0.01);
}
#[test]
fn guest_port92_updates_memory_a20() {
    // mov al,0; out 92h,al; cli; hlt
    let mut m = machine(&[0xb0, 0, 0xe6, 0x92, 0xfa, 0xf4], 20_000_000);
    assert!(m.memory.a20_enabled());
    run(&mut m, 1000);
    assert!(!m.memory.a20_enabled());
    m.io.serial.set_present(3, true); // generic UART collection cannot widen the fixed bus
    assert_eq!(m.io.read_u8(0x2e8), 0xff); // COM4 absent
}
#[test]
fn interpreter_stops_a_tight_loop_at_cycle_budget() {
    let mut m = machine(&[0x90, 0xeb, 0xfd], 20_000_000); // nop; jmp nop
    let result = gp64_interp::run_ext_charged_until_cycle_budget(
        &mut m.cpu,
        &mut m.memory,
        &mut (),
        &mut gp64_machine::MachinePolicy,
        1000,
        true,
        4,
    );
    assert_eq!(result.charged_cycles, 4);
    assert_eq!(result.result.executed(), 4);
}

#[test]
fn keyboard_transfers_do_not_depend_on_rtc_rate() {
    // Select 2Hz periodic RTC rate and disable PIE; only IRQ1 is unmasked.
    let code = [
        0xfa, 0xb0, 0xfd, 0xe6, 0x21, 0xb0, 0x0a, 0xe6, 0x70, 0xb0, 0x2f, 0xe6, 0x71, 0xb0, 0x0b,
        0xe6, 0x70, 0xb0, 0x02, 0xe6, 0x71, 0xfb, 0xf4, 0xeb, 0xfd,
    ];
    let mut m = machine(&code, 20_000_000);
    let handler = [
        0x50, 0x53, // preserve AX and BX
        0x8b, 0x1e, 0, 5, // BX = number of bytes delivered
        0xe4, 0x60, // consume current output byte
        0x88, 0x87, 0x10, 5, // [BX+0510] = scan code
        0xe4, 0x60, 0x88, 0x87, 0x20, 5, // chained read sees the same data latch
        0x43, 0x89, 0x1e, 0, 5, 0xb0, 0x20, 0xe6, 0x20, 0x5b, 0x58, 0xcf,
    ];
    m.memory.ram[0x800..0x800 + handler.len()].copy_from_slice(&handler);
    m.memory.write_u32(9 * 4, 0x800);
    run(&mut m, 1000);
    let codes = [0x1e, 0x9e, 0x30, 0xb0];
    m.io.keyboard
        .shared_state()
        .inject_scancode(&codes, 0x1e, false);
    run(&mut m, 100_000); // 5ms: enough for four 8042 transfers, less than a RTC period.
    assert_eq!(ticks(&m), 4);
    assert_eq!(&m.memory.ram[0x510..0x514], &codes);
    assert_eq!(&m.memory.ram[0x520..0x524], &codes);
    assert_eq!(m.io.pic.get_isr(), 0);
}

fn real_interrupt_failure_machine(path: u8) {
    let mut failures = Vec::new();
    for sp in [1, 3, 5, 0, 2, 4, 0x100] {
        // Independently written guests: INT 30h; loop, NOP; loop, and UD2.
        let code: &[u8] = match path {
            0 => &[0xCD, 0x30, 0xEB, 0xFE],
            1 => &[0x90, 0xEB, 0xFD],
            _ => &[0x0F, 0x0B],
        };
        let vector = match path {
            0 => 0x30,
            1 => 8,
            _ => 6,
        };
        let mut m = machine(code, 20_000_000);
        m.cpu.regs.ss = 0x200;
        m.cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(0x200);
        m.cpu.regs.set_sp(sp);
        m.memory.ram[0x2000..0x12000].fill(0xA5);
        // Handler marks AX then loops, making execution after failed delivery observable.
        m.memory.ram[0x800..0x805].copy_from_slice(&[0xB8, 0x34, 0x12, 0xEB, 0xFE]);
        m.memory.ram[usize::from(vector) * 4..usize::from(vector) * 4 + 4]
            .copy_from_slice(&[0, 8, 0, 0]);
        if path == 1 {
            m.cpu.regs.flags.set(gp64_cpu::Flags::INTERRUPT, true);
            m.cpu.interrupts_enabled = true;
            m.io.pic.write_data(0, 0xFE);
            m.io.pic.raise_irq(0);
        }
        let before_stack = m.memory.ram[0x2000..0x12000].to_vec();
        let flags = m.cpu.regs.flags.bits();
        let result = m.run_for(100);
        if matches!(sp, 1 | 3 | 5) {
            let RunExit::CpuFault(gp64_cpu::CpuError::TripleFault(info)) = result else {
                failures.push(format!(
                    "path {path}, SP {sp}: expected shutdown, got {result:?}"
                ));
                continue;
            };
            assert_eq!(info.vector, vector);
            assert_eq!(info.irq, None);
            assert_eq!(info.error_code, if path == 2 { Some(0) } else { None });
            assert_eq!(
                info.origin,
                match path {
                    0 => gp64_cpu::TripleFaultOrigin::SoftwareInterrupt,
                    1 => gp64_cpu::TripleFaultOrigin::HardwareIrq,
                    _ => gp64_cpu::TripleFaultOrigin::CpuException,
                }
            );
            assert_eq!(m.cpu.regs.sp(), sp);
            assert_eq!(m.cpu.regs.cs, 0);
            assert_eq!(m.cpu.regs.ip(), if path == 0 { 0x1002 } else { 0x1000 });
            assert_eq!(m.cpu.regs.ax(), 0, "handler executed after failed delivery");
            assert_eq!(m.cpu.regs.flags.bits(), flags);
            assert_eq!(m.memory.ram[0x2000..0x12000], before_stack);
        } else {
            assert!(matches!(result, RunExit::BudgetUsed), "{result:?}");
            assert_eq!(m.cpu.regs.ax(), 0x1234);
            assert_eq!(m.cpu.regs.sp(), sp.wrapping_sub(6));
            assert_eq!(m.cpu.regs.cs, 0);
            assert_eq!(m.cpu.regs.ip(), 0x803);
            let expected_ip = if path == 0 { 0x1002_u16 } else { 0x1000_u16 };
            let mut expected_stack = before_stack;
            for (i, word) in [expected_ip, 0, flags].into_iter().enumerate() {
                let start = usize::from(sp.wrapping_sub(6).wrapping_add(i as u16 * 2));
                expected_stack[start..start + 2].copy_from_slice(&word.to_le_bytes());
            }
            assert_eq!(m.memory.ram[0x2000..0x12000], expected_stack);
        }
        if path == 1 {
            // Delivery has acknowledged IRQ0; shutdown does not silently consume it and continue.
            m.io.pic.write_command(0, 0x0B);
            assert_eq!(m.io.pic.read_command(0) & 1, 1);
            m.io.pic.write_command(0, 0x0A);
            assert_eq!(m.io.pic.read_command(0) & 1, 0);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("; "));
}
#[test]
fn real_interrupt_failure_machine_software() {
    real_interrupt_failure_machine(0);
}
#[test]
fn real_interrupt_failure_machine_acknowledged_irq() {
    real_interrupt_failure_machine(1);
}
#[test]
fn real_interrupt_failure_machine_exception() {
    real_interrupt_failure_machine(2);
}

// Independently assembled tiny guests, with handlers that mark AX and loop.
fn real_idtr_guest(path: u8, old: u32, relocated: u32) -> Machine {
    let code: &[u8] = match path {
        0 => &[0x0f, 0x01, 0x1e, 0x00, 0x06, 0xcd, 0x30, 0xeb, 0xfe],
        1 => &[0x90, 0xeb, 0xfd],
        _ => &[0x0f, 0x0b],
    };
    let vector = match path {
        0 => 0x30,
        1 => 8,
        _ => 6,
    };
    let mut m = machine(code, 20_000_000);
    m.cpu.regs.idtr.base = 0x2000;
    m.cpu.regs.idtr.limit = 0x3ff;
    m.memory.write_u16(0x600, 0x3ff);
    m.memory.write_u32(0x602, 0x2000);
    m.memory.write_u32(vector * 4, old);
    m.memory.write_u32(0x2000 + vector * 4, relocated);
    m.memory.ram[0x800..0x805].copy_from_slice(&[0xb8, 0x11, 0x11, 0xeb, 0xfe]);
    m.memory.ram[0x900..0x905].copy_from_slice(&[0xb8, 0x22, 0x22, 0xeb, 0xfe]);
    if path == 1 {
        m.cpu.regs.flags.set(gp64_cpu::Flags::INTERRUPT, true);
        m.cpu.interrupts_enabled = true;
        m.io.pic.write_data(0, 0xfe);
        m.io.pic.raise_irq(0);
    }
    m
}

#[test]
fn real_idtr_machine_relocated_paths() {
    for path in 0..3 {
        for old in [0, 0x800] {
            let mut m = real_idtr_guest(path, old, 0x900);
            if path == 0 {
                m.cpu.regs.idtr.base = 0;
            }
            let result = m.run_for(100);
            assert!(
                matches!(result, RunExit::BudgetUsed),
                "path {path}: {result:?}"
            );
            assert_eq!(m.cpu.regs.ax(), 0x2222, "path {path}, old {old:#x}");
        }
        let mut m = real_idtr_guest(path, 0x800, 0);
        let result = m.run_for(100);
        assert!(
            match path {
                0 => matches!(result, RunExit::SoftwareInterrupt { vector: 0x30 }),
                1 => matches!(result, RunExit::UnhandledInterrupt { vector: 8 }),
                _ => matches!(result, RunExit::UnhandledException { vector: 6, .. }),
            },
            "path {path}: {result:?}"
        );
        assert_eq!(m.cpu.regs.ax(), 0);
    }
}

#[test]
fn real_idtr_machine_firmware_identity() {
    let mut m = real_idtr_guest(2, 0x900, 0x900);
    m.cpu.regs.idtr.base = 0;
    m.record_firmware_exception_vectors();
    m.cpu.regs.idtr.base = 0x2000;
    let result = m.run_for(100);
    assert!(matches!(result, RunExit::BudgetUsed), "{result:?}");
    assert_eq!(m.cpu.regs.ax(), 0x2222);
    let mut m = real_idtr_guest(2, 0x900, 0x900);
    m.record_firmware_exception_vectors();
    m.memory.write_u32(6 * 4, 0x800);
    assert!(matches!(
        m.run_for(100),
        RunExit::UnhandledException { vector: 6, .. }
    ));
    for cpu_type in [
        gp64_cpu::CpuType::I8086,
        gp64_cpu::CpuType::I8088,
        gp64_cpu::CpuType::I80186,
    ] {
        // XOR AX,AX; DIV AX raises #DE on every generation.
        let mut m = machine(&[0x31, 0xc0, 0xf7, 0xf0], 20_000_000);
        m.cpu.cpu_type = cpu_type;
        m.memory.write_u32(0, 0x900);
        m.memory.ram[0x900..0x905].copy_from_slice(&[0xb8, 0x22, 0x22, 0xeb, 0xfe]);
        m.record_firmware_exception_vectors();
        m.cpu.regs.idtr.base = 0x2000;
        m.cpu.regs.idtr.limit = 0;
        let result = m.run_for(100);
        assert!(
            matches!(result, RunExit::UnhandledException { vector: 0, .. }),
            "{cpu_type:?}: {result:?}"
        );
        assert_eq!(m.cpu.regs.ax(), 0);
    }
}

#[test]
fn real_idtr_machine_a20_split_vector() {
    for enabled in [false, true] {
        let mut m = real_idtr_guest(0, 0x800, 0);
        m.cpu.regs.idtr.base = 0xfffff - 0xc0;
        m.memory.write_u32(0x602, m.cpu.regs.idtr.base);
        m.memory.set_a20(true);
        m.memory.write_u8(0xfffff, 0);
        m.memory.write_u8(0x100000, 0x0a);
        m.memory.write_u16(0x100001, 0);
        m.memory.write_u8(0, 9);
        m.memory.write_u16(1, 0);
        m.memory.ram[0xa00..0xa05].copy_from_slice(&[0xb8, 0x33, 0x33, 0xeb, 0xfe]);
        m.memory.set_a20(enabled);
        let result = m.run_for(100);
        assert!(
            matches!(result, RunExit::BudgetUsed),
            "A20 {enabled}: {result:?}"
        );
        assert_eq!(m.cpu.regs.ax(), if enabled { 0x3333 } else { 0x2222 });
    }
}

#[test]
fn real_idtr_machine_limit_rescue_and_shutdown() {
    for path in 0..3 {
        let mut m = real_idtr_guest(path, 0x800, 0x900);
        m.cpu.regs.idtr.limit = if path == 0 { 0xc2 } else { 0x1a };
        m.memory.write_u16(0x600, m.cpu.regs.idtr.limit);
        m.memory.write_u32(0x2020, 0x900);
        let result = m.run_for(100);
        if path == 0 {
            assert!(matches!(result, RunExit::BudgetUsed), "{result:?}");
            assert_eq!(m.cpu.regs.ax(), 0x2222);
            assert_eq!(m.cpu.regs.sp(), 0xfefa);
        } else {
            assert!(
                matches!(
                    result,
                    RunExit::CpuFault(gp64_cpu::CpuError::TripleFault(_))
                ),
                "{result:?}"
            );
            assert_eq!(m.cpu.regs.sp(), 0xff00);
            assert_eq!(m.cpu.regs.ax(), 0);
        }
    }
}
