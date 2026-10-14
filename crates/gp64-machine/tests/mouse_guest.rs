//! INT33 acceptance programs assembled here from ordinary real-mode instructions.
use gp64_machine::{DosMachine, DosRunExit};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Drive(PathBuf);
impl Drive {
    fn launch(bytes: &[u8]) -> (Self, DosMachine) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch")
            .join(format!(
                "mouse-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("MOUSE.COM"), bytes).unwrap();
        let pc = DosMachine::launch(&path, "MOUSE.COM", "").unwrap();
        (Self(path), pc)
    }
}
impl Drop for Drive {
    fn drop(&mut self) {
        fs::remove_file(self.0.join("MOUSE.COM")).unwrap();
        fs::remove_dir(&self.0).unwrap();
    }
}
fn run(pc: &mut DosMachine, cycles: u64) {
    assert!(matches!(pc.run_for(cycles).unwrap(), DosRunExit::Running));
}
fn address(pc: &DosMachine, offset: usize) -> usize {
    usize::from(pc.dos.current_psp()) * 16 + offset
}

#[test]
fn far_callbacks_preserve_registers_defer_with_cli_and_do_not_nest() {
    // Install a handler at 0180, retain ordinary registers, then spin with CLI.
    let mut code = vec![
        0x31, 0xc0, 0xcd, 0x33, // reset
        0x0e, 0x07, // push cs / pop es
        0xb8, 0x0c, 0, 0xb9, 0x7f, 0, 0xba, 0x80, 1, 0xcd, 0x33, 0xb8, 0x34, 0x12, 0xbb, 0x78,
        0x56, 0xb9, 0xbc, 0x9a, 0xba, 0xf0, 0xde, 0xbe, 0x11, 0x11, 0xbf, 0x22, 0x22, 0xfa, 0xeb,
        0xfe, // CLI: no callbacks until host resumes at STI below
        0xfb, 0xeb, 0xfe,
    ];
    let sti = 0x100 + code.len() as u16 - 3;
    code.resize(0x80, 0x90);
    // Handler writes ABI registers through ES, then waits with interrupts enabled.
    // A second event must remain queued while this callback is live.
    code.extend_from_slice(&[
        0x26, 0xff, 0x06, 0x00, 0x03, // inc word es:[0300] (calls)
        0x26, 0xa3, 0x02, 0x03, // event
        0x26, 0x89, 0x1e, 0x04, 0x03, // buttons
        0x26, 0x89, 0x0e, 0x06, 0x03, 0x26, 0x89, 0x16, 0x08, 0x03, 0x26, 0x89, 0x36, 0x0a, 0x03,
        0x26, 0x89, 0x3e, 0x0c, 0x03, 0x9c, 0x58, 0x26, 0xa3, 0x10, 0x03, 0xfb, 0x26, 0x80, 0x3e,
        0x0e, 0x03, 0, 0x74, 0xf8, // wait for host gate
        0xb8, 0xff, 0xff, 0xbb, 0xff, 0xff, 0x8e, 0xd8, 0xf9, 0xfd, 0xcb,
    ]);
    let (_drive, mut pc) = Drive::launch(&code);
    run(&mut pc, 2000);
    let base = address(&pc, 0x300);
    let before = pc.machine.cpu.regs.clone();
    pc.mouse_move(10, 6);
    pc.mouse_button(0, true);
    run(&mut pc, 2000);
    assert_eq!(
        pc.machine.memory.read_u16(base),
        0,
        "CLI must defer callback"
    );
    pc.machine.cpu.regs.set_ip(sti);
    run(&mut pc, 2000);
    assert_eq!(pc.machine.memory.read_u16(base), 1);
    assert_eq!(pc.machine.memory.read_u16(base + 2), 3);
    assert_eq!(pc.machine.memory.read_u16(base + 4), 1);
    assert_eq!(pc.machine.memory.read_u16(base + 6), 10);
    assert_eq!(pc.machine.memory.read_u16(base + 8), 3);
    assert_eq!(pc.machine.memory.read_u16(base + 10), 10);
    assert_eq!(pc.machine.memory.read_u16(base + 12), 6);
    assert_ne!(pc.machine.memory.read_u16(base + 16) & 0x200, 0);
    assert_eq!(pc.machine.memory.read_u16(base + 16) & 0x400, 0);
    pc.mouse_move(2, 2);
    run(&mut pc, 2000);
    assert_eq!(
        pc.machine.memory.read_u16(base),
        1,
        "callback must not reenter after STI"
    );
    pc.machine.memory.write_u8(base + 14, 1);
    run(&mut pc, 2000);
    assert_eq!(pc.machine.memory.read_u16(base), 2);
    assert_eq!(pc.machine.memory.read_u16(base + 10), 12);
    assert_eq!(pc.machine.memory.read_u16(base + 12), 8);
    let after = &pc.machine.cpu.regs;
    assert_eq!(
        (
            after.ax(),
            after.bx(),
            after.cx(),
            after.dx(),
            after.si(),
            after.di(),
            after.ds,
            after.sp()
        ),
        (
            before.ax(),
            before.bx(),
            before.cx(),
            before.dx(),
            before.si(),
            before.di(),
            before.ds,
            before.sp()
        )
    );
    assert!(!after
        .flags
        .contains(gp64_cpu::Flags::CARRY | gp64_cpu::Flags::DIRECTION));
}

#[test]
fn mode13_cursor_masks_hotspot_counter_and_background_restore() {
    // Fill framebuffer with index 55, select a one-pixel XOR cursor, and show it.
    let mut code = vec![
        0xb8, 0x13, 0, 0xcd, 0x10, 0xb8, 0x00, 0xa0, 0x8e, 0xc0, 0x31, 0xff, 0xb9, 0x00, 0xfa,
        0xb0, 0x55, 0xf3, 0xaa, 0x31, 0xc0, 0xcd, 0x33, // reset
        0xb8, 1, 0, 0xcd, 0x33, // show default arrow
        0x26, 0xa0, 0, 0, // AL = framebuffer origin (ES is A000)
        0x0e, 0x07, 0x26, 0xa2, 0, 3, // store through program segment
        0xb8, 2, 0, 0xcd, 0x33, // hide restores filled background
        0xb8, 4, 0, 0xb9, 20, 0, 0xba, 10, 0, 0xcd, 0x33, 0x0e, 0x07, 0xb8, 9, 0, 0xbb, 1, 0, 0xb9,
        1, 0, 0xba, 0x80, 1, 0xcd, 0x33, 0xb8, 1, 0, 0xcd, 0x33, 0xeb, 0xfe,
    ];
    code.resize(0x80, 0);
    for _ in 0..16 {
        code.extend_from_slice(&0xffffu16.to_le_bytes());
    }
    code.extend_from_slice(&0x8000u16.to_le_bytes());
    code.resize(0xc0, 0);
    // Two hides require two shows; the final hide restores backing pixels.
    let control = 0x100 + code.len() as u16;
    code.extend_from_slice(&[
        0xb8, 2, 0, 0xcd, 0x33, 0xb8, 2, 0, 0xcd, 0x33, 0xb8, 1, 0, 0xcd, 0x33, 0xeb, 0xfe, 0xb8,
        1, 0, 0xcd, 0x33, 0xeb, 0xfe, 0xb8, 2, 0, 0xcd, 0x33, 0xeb, 0xfe,
    ]);
    let (_drive, mut pc) = Drive::launch(&code);
    run(&mut pc, 400_000);
    let old = 0xa0000 + 9 * 320 + 9;
    assert_eq!(pc.machine.memory.read_u8(address(&pc, 0x300)), 0);
    assert_eq!(pc.machine.memory.read_u8(0xa0000), 0x55);

    assert_eq!(pc.machine.memory.read_u8(old), 0xaa);
    assert_eq!(pc.machine.memory.read_u8(old + 1), 0x55);
    run(&mut pc, 1_000_000);
    let completed = pc.machine.io.vga.completed_framebuffer().to_vec();
    assert!(completed.iter().any(|&pixel| pixel != 0));
    run(&mut pc, 300_000);
    assert_eq!(pc.machine.io.vga.completed_framebuffer(), completed);

    pc.mouse_move(4, 4);
    let new = 0xa0000 + 11 * 320 + 11;
    assert_eq!(pc.machine.memory.read_u8(old), 0x55);
    assert_eq!(pc.machine.memory.read_u8(new), 0xaa);
    pc.machine.cpu.regs.set_ip(control);
    run(&mut pc, 2000);
    assert_eq!(pc.machine.memory.read_u8(new), 0x55);
    pc.machine.cpu.regs.set_ip(control + 17);
    run(&mut pc, 2000);
    assert_eq!(pc.machine.memory.read_u8(new), 0xaa);
    pc.machine.cpu.regs.set_ip(control + 24);
    run(&mut pc, 2000);
    assert_eq!(pc.machine.memory.read_u8(new), 0x55);
}

#[test]
fn guest_ranges_ratios_signed_motion_and_counter_reset() {
    let mut code = vec![0x31, 0xc0, 0xcd, 0x33];
    for (function, x, y) in [
        (7u16, 10u16, 100u16),
        (8, 20, 80),
        (0xf, 16, 32),
        (4, 0, 500),
    ] {
        code.push(0xb8);
        code.extend_from_slice(&function.to_le_bytes());
        code.push(0xb9);
        code.extend_from_slice(&x.to_le_bytes());
        code.push(0xba);
        code.extend_from_slice(&y.to_le_bytes());
        code.extend_from_slice(&[0xcd, 0x33]);
    }
    let query = 0x100 + code.len() as u16;
    code.extend_from_slice(&[
        0xb8, 3, 0, 0xcd, 0x33, 0x89, 0x0e, 0, 3, 0x89, 0x16, 2, 3, 0xb8, 0xb, 0, 0xcd, 0x33, 0x89,
        0x0e, 4, 3, 0x89, 0x16, 6, 3, 0xeb, 0xfe,
    ]);
    let (_drive, mut pc) = Drive::launch(&code);
    run(&mut pc, 2000);
    let base = address(&pc, 0x300);
    assert_eq!(pc.machine.memory.read_u16(base), 10);
    assert_eq!(pc.machine.memory.read_u16(base + 2), 80);
    pc.mouse_move(1, -1);
    pc.mouse_move(1, -3);
    pc.machine.cpu.regs.set_ip(query);
    run(&mut pc, 2000);
    assert_eq!(pc.machine.memory.read_u16(base), 11);
    assert_eq!(pc.machine.memory.read_u16(base + 2), 79);
    assert_eq!(pc.machine.memory.read_u16(base + 4), 2);
    assert_eq!(pc.machine.memory.read_u16(base + 6), (-4i16) as u16);
    pc.machine.cpu.regs.set_ip(query);
    run(&mut pc, 2000);
    assert_eq!(pc.machine.memory.read_u16(base + 4), 0);
    assert_eq!(pc.machine.memory.read_u16(base + 6), 0);
    pc.mouse_move(i32::MAX, i32::MIN);
    pc.machine.cpu.regs.set_ip(query);
    run(&mut pc, 2000);
    assert_eq!(pc.machine.memory.read_u16(base), 100);
    assert_eq!(pc.machine.memory.read_u16(base + 2), 20);
    assert_eq!(pc.machine.memory.read_u16(base + 4), i16::MAX as u16);
    assert_eq!(pc.machine.memory.read_u16(base + 6), i16::MIN as u16);
}
