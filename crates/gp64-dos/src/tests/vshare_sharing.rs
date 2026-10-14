//! VSHARE's sharing rules in a sharing session (MDD-BUG-FLU-03201).
//!
//! Independently constructed holder and caller-open cases: a held file
//! is read-only, deny-write, and caller-open files use explicit modes:
//! - a refused open answers AX=5, extended error 20h; a refused
//!   compatibility-mode open goes through the critical-error handler first;
//! - a delete of a file open in a sharing mode, or held, is the critical
//!   error; one the caller holds in compatibility mode goes through.
//!
//! Plain DOS (`vshare` off) keeps its own rules: error 32, no handler, and
//! deletes of open files go through.

use super::*;

const PATH_SEG: u16 = 0x2000;

struct Session {
    dos: Dos,
    _dir: tempfile::TempDir,
}

impl Session {
    /// C: on a fresh host directory holding HELD.TTF and OWN.DAT.
    fn new(vshare: bool) -> Self {
        let dir = tempfile::tempdir().expect("drive");
        std::fs::write(dir.path().join("HELD.TTF"), b"ttf").unwrap();
        std::fs::write(dir.path().join("OWN.DAT"), b"own").unwrap();
        let mut dos = Dos::new();
        dos.drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(dir.path().to_path_buf())),
        );
        dos.vshare = vshare;
        Session { dos, _dir: dir }
    }

    fn exists(&self, name: &str) -> bool {
        self._dir.path().join(name).exists()
    }

    /// INT 21h `ah` with AL=`al` on the path `name`: the registers after,
    /// and the disposition.
    fn call(&mut self, ah: u8, al: u8, name: &str) -> (Cpu, Int21Disposition) {
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        let (mut vga, mut keyboard) = create_test_io();
        let path = Memory::seg_offset_to_linear(PATH_SEG, 0);
        for (i, b) in name.bytes().chain([0]).enumerate() {
            memory.write_u8(path + i, b);
        }
        cpu.regs.set_ah(ah);
        cpu.regs.set_al(al);
        cpu.regs.set_cx(0);
        cpu.regs.ds = PATH_SEG;
        cpu.regs.set_dx(0);
        let disposition = self
            .dos
            .int21(
                &mut cpu,
                &mut memory,
                &mut vga,
                &mut keyboard,
                &mut Pic::new(),
            )
            .expect("INT 21h runs");
        (cpu, disposition)
    }

    /// AH=3Dh: Ok(handle) or Err(AX) when refused without the handler.
    fn open(&mut self, name: &str, mode: u8) -> Result<u16, u16> {
        let (cpu, disposition) = self.call(0x3D, mode, name);
        assert!(
            !matches!(disposition, Int21Disposition::CriticalError { .. }),
            "open {name} {mode:02X} raised the critical error"
        );
        if cpu.regs.flags.contains(gp64_cpu::Flags::CARRY) {
            Err(cpu.regs.ax())
        } else {
            Ok(cpu.regs.ax())
        }
    }

    fn close(&mut self, handle: u16) {
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        let (mut vga, mut keyboard) = create_test_io();
        cpu.regs.set_ah(0x3E);
        cpu.regs.set_bx(handle);
        self.dos
            .int21(
                &mut cpu,
                &mut memory,
                &mut vga,
                &mut keyboard,
                &mut Pic::new(),
            )
            .expect("INT 21h runs");
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    }

    fn ext_error(&self) -> u16 {
        self.dos.extended_error.0
    }
}

fn sharing_violation(disposition: &Int21Disposition, ah: u8) -> bool {
    matches!(
        disposition,
        Int21Disposition::CriticalError {
            ah_function,
            drive: 2,
            error_code: 0x20,
            is_write: false,
            ..
        } if *ah_function == ah
    )
}

#[test]
fn a_held_file_refuses_opens_and_deletes_under_vshare() {
    let mut session = Session::new(true);
    session.dos.set_held_files(["C:\\HELD.TTF"]);
    // Read opens pass when the sharing mode permits reading.
    for mode in [0x00, 0x20, 0x40] {
        let handle = session.open("C:\\HELD.TTF", mode).expect("opens");
        session.close(handle);
    }
    // Deny-all, deny-read and writes in a sharing mode: AX=5, ext 20h.
    for mode in [0x10, 0x30, 0x12, 0x22, 0x42] {
        assert_eq!(session.open("C:\\HELD.TTF", mode), Err(5), "{mode:02X}");
        assert_eq!(session.ext_error(), 0x20, "{mode:02X}");
    }
    // A compatibility read/write open and create: the critical error.
    let (_, disposition) = session.call(0x3D, 0x02, "C:\\HELD.TTF");
    assert!(sharing_violation(&disposition, 0x3D));
    let (_, disposition) = session.call(0x3C, 0x00, "C:\\HELD.TTF");
    assert!(sharing_violation(&disposition, 0x3C));
    // Deleting a held file raises the critical error and preserves the file.
    let (_, disposition) = session.call(0x41, 0x00, "C:\\HELD.TTF");
    assert!(sharing_violation(&disposition, 0x41));
    assert!(session.exists("HELD.TTF"));
    // Released: the delete goes through.
    session.dos.set_held_files([]);
    let (cpu, disposition) = session.call(0x41, 0x00, "C:\\HELD.TTF");
    assert_eq!(disposition, Int21Disposition::Iret);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(!session.exists("HELD.TTF"));
}

#[test]
fn deleting_ones_own_open_file_follows_its_sharing_mode_under_vshare() {
    // Own rows: compatibility read and read/write let the delete through;
    // deny-none, deny-write and deny-all refuse it.
    for (mode, deleted) in [
        (0x00, true),
        (0x02, true),
        (0x40, false),
        (0x20, false),
        (0x10, false),
    ] {
        let mut session = Session::new(true);
        let handle = session.open("C:\\OWN.DAT", mode).expect("opens");
        let (_, disposition) = session.call(0x41, 0x00, "C:\\OWN.DAT");
        assert_eq!(
            !sharing_violation(&disposition, 0x41),
            deleted,
            "{mode:02X}"
        );
        let _ = handle;
    }
}

#[test]
fn plain_dos_keeps_its_own_sharing_rules() {
    let mut session = Session::new(false);
    // Holds are a hosted component's: plain DOS never sees one.
    session.dos.set_held_files(["C:\\HELD.TTF"]);
    assert!(session.open("C:\\HELD.TTF", 0x12).is_ok());
    // A conflict answers error 32 with no handler.
    let _first = session.open("C:\\OWN.DAT", 0x12).expect("opens");
    assert_eq!(session.open("C:\\OWN.DAT", 0x12), Err(32));
    // A delete of an open file goes through.
    let (cpu, disposition) = session.call(0x41, 0x00, "C:\\OWN.DAT");
    assert_eq!(disposition, Int21Disposition::Iret);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}
