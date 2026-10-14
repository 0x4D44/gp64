//! A path call on a fitted floppy drive with no media (MDD-BUG-FLU-03064).
//!
//! INT 21h 3Dh on a fitted, media-less A: drive
//! is DOS's drive-not-ready critical error, extended error 15h, for the
//! session's handler. Only a session that fills `not_ready_drives` (16-bit)
//! sees it; plain DOS keeps answering an unmounted drive with "invalid
//! drive" (0Fh), so a DOS program probing A: is unchanged.

use super::*;

const PATH_SEG: u16 = 0x2000;

fn call_on_a(dos: &mut Dos, ah: u8) -> (Cpu, Memory, Int21Disposition) {
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let path = Memory::seg_offset_to_linear(PATH_SEG, 0);
    for (i, &b) in b"A:\\CRIT.DAT\0".iter().enumerate() {
        memory.write_u8(path + i, b);
    }
    cpu.regs.set_ah(ah);
    cpu.regs.set_al(0);
    cpu.regs.set_cx(0);
    cpu.regs.ds = PATH_SEG;
    cpu.regs.set_dx(0);
    let disposition = dos
        .int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        )
        .expect("INT 21h runs");
    (cpu, memory, disposition)
}

fn indos(memory: &Memory) -> u8 {
    memory.read_u8(Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET))
}

#[test]
fn open_and_create_on_a_not_ready_drive_raise_drive_not_ready() {
    for ah in [0x3D, 0x3C] {
        let mut dos = Dos::new();
        dos.not_ready_drives.insert(0);
        let (_, memory, disposition) = call_on_a(&mut dos, ah);
        match disposition {
            Int21Disposition::CriticalError {
                ah_function,
                drive,
                error_code,
                is_write,
                entry_snapshot,
            } => {
                assert_eq!(ah_function, ah);
                assert_eq!(drive, 0, "A:");
                assert_eq!(error_code, 0x15, "drive not ready");
                assert!(!is_write);
                assert_eq!(
                    entry_snapshot.ah(),
                    ah,
                    "the registers the call came in with"
                );
                assert_eq!(entry_snapshot.dx(), 0);
            }
            other => panic!("AH={ah:02X}: expected a critical error, got {other:?}"),
        }
        assert_eq!(dos.extended_error.0, 0x15, "AH=59h reports drive not ready");
        assert_eq!(
            indos(&memory),
            1,
            "DOS stays entered until the critical-error handler is done"
        );
    }
}

#[test]
fn plain_dos_still_answers_an_unmounted_floppy_with_invalid_drive() {
    for ah in [0x3D, 0x3C] {
        let mut dos = Dos::new();
        let (cpu, memory, disposition) = call_on_a(&mut dos, ah);
        assert_eq!(disposition, Int21Disposition::Iret, "AH={ah:02X}");
        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 0x0F, "invalid drive");
        assert_eq!(dos.extended_error.0, 0x0F);
        assert_eq!(indos(&memory), 0);
    }
}
