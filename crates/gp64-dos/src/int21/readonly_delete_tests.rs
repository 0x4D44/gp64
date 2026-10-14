//! MDD-BUG-KILN-00101, cluster 3: a read-only file must survive AH=41h.
//!
//! DOS refuses to unlink a read-only file with error 5 (access denied).
//! AH=43h AL=01 clears that attribute before AH=41h may delete the file.
//! These cases construct files and choose attributes directly from that contract.

use super::*;
use crate::tests::create_test_io;
use tempfile::TempDir;

/// `Dos::drives` holds C: under key 2.
const DRIVES_KEY_C: u8 = 2;
const GUEST_SEG: u16 = 0x2000;
const GUEST_OFF: u16 = 0x0100;

/// DOS error 5 — access denied.
const ERROR_ACCESS_DENIED: u16 = 5;
/// AH=43h AL=01 attribute bits: FILE_ATTRIBUTE_READONLY / _NORMAL.
const ATTR_READONLY: u16 = 0x01;
const ATTR_NORMAL: u16 = 0x00;

struct Fixture {
    dos: Dos,
    cpu: gp64_cpu::Cpu,
    memory: Memory,
    vga: crate::Vga,
    keyboard: crate::Keyboard,
    temp_dir: TempDir,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(name), b"data").unwrap();

        let mut dos = Dos::new();
        dos.drives.insert(
            DRIVES_KEY_C,
            Box::new(crate::filesystem::HostDrive::new(
                temp_dir.path().to_path_buf(),
            )),
        );

        let (vga, keyboard) = create_test_io();
        Fixture {
            dos,
            cpu: gp64_cpu::Cpu::new(),
            memory: Memory::new(),
            vga,
            keyboard,
            temp_dir,
        }
    }

    fn exists(&self, name: &str) -> bool {
        self.temp_dir.path().join(name).exists()
    }

    /// Point DS:DX at an ASCIIZ copy of `path`.
    fn write_path(&mut self, path: &str) {
        let base = Memory::seg_offset_to_linear(GUEST_SEG, GUEST_OFF);
        for (i, byte) in path.as_bytes().iter().enumerate() {
            self.memory.write_u8(base + i, *byte);
        }
        self.memory.write_u8(base + path.len(), 0);
    }

    /// Run one INT 21h call with DS:DX on the path. Returns `(ax, carry)`.
    fn int21(&mut self, ah: u8, al: u8, cx: u16, path: &str) -> (u16, bool) {
        self.write_path(path);
        self.cpu.regs.set_ah(ah);
        self.cpu.regs.set_al(al);
        self.cpu.regs.set_cx(cx);
        self.cpu.regs.ds = GUEST_SEG;
        self.cpu.regs.set_dx(GUEST_OFF);
        let result = self.dos.int21(
            &mut self.cpu,
            &mut self.memory,
            &mut self.vga,
            &mut self.keyboard,
            &mut crate::Pic::new(),
        );
        assert!(result.is_ok(), "int21 AH={ah:#04x} failed: {result:?}");
        (
            self.cpu.regs.ax(),
            self.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        )
    }

    fn delete(&mut self, path: &str) -> (u16, bool) {
        self.int21(0x41, 0, 0, path)
    }

    fn set_attributes(&mut self, path: &str, attrs: u16) -> (u16, bool) {
        self.int21(0x43, 0x01, attrs, path)
    }

    fn call_with_registers(&mut self) -> (u16, bool) {
        self.dos
            .int21(
                &mut self.cpu,
                &mut self.memory,
                &mut self.vga,
                &mut self.keyboard,
                &mut crate::Pic::new(),
            )
            .expect("INT 21h dispatch");
        (
            self.cpu.regs.ax(),
            self.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        )
    }

    fn create(&mut self, ah: u8, path: &str, attributes: u16) -> u16 {
        self.write_path(path);
        self.cpu.regs.set_ax(u16::from(ah) << 8);
        self.cpu.regs.set_cx(attributes);
        self.cpu.regs.ds = GUEST_SEG;
        self.cpu.regs.set_dx(GUEST_OFF);
        if ah == 0x6C {
            self.cpu.regs.set_bx(2); // read/write, compatibility sharing
            self.cpu.regs.set_dx(0x10); // create if absent, fail if present
            self.cpu.regs.set_si(GUEST_OFF); // AH=6Ch uses DS:SI
        }
        let (handle, carry) = self.call_with_registers();
        assert!(!carry, "AH={ah:02X} create failed: AX={handle}");
        if ah == 0x6C {
            // CX is an output action code, so the original attribute word must
            // be retained separately before the handler reports creation.
            assert_eq!(self.cpu.regs.cx(), 2, "AH=6Ch must report created");
        }
        handle
    }

    fn close(&mut self, handle: u16) {
        self.cpu.regs.set_ax(0x3E00);
        self.cpu.regs.set_bx(handle);
        assert_eq!(self.call_with_registers(), (0, false));
    }

    fn transfer(&mut self, ah: u8, handle: u16) -> (u16, bool) {
        self.cpu.regs.set_ax(u16::from(ah) << 8);
        self.cpu.regs.set_bx(handle);
        self.cpu.regs.set_cx(4);
        self.cpu.regs.ds = GUEST_SEG;
        self.cpu.regs.set_dx(0x0400);
        self.call_with_registers()
    }

    fn write_and_read_created_handle(&mut self, handle: u16) {
        let buffer = Memory::seg_offset_to_linear(GUEST_SEG, 0x0400);
        self.memory.write_bytes_guest(buffer, b"SAVE");
        assert_eq!(self.transfer(0x40, handle), (4, false));
        self.cpu.regs.set_ax(0x4200);
        self.cpu.regs.set_bx(handle);
        self.cpu.regs.set_cx(0);
        self.cpu.regs.set_dx(0);
        assert_eq!(self.call_with_registers(), (0, false));
        self.memory.write_bytes_guest(buffer, &[0; 4]);
        assert_eq!(self.transfer(0x3F, handle), (4, false));
        let mut contents = [0; 4];
        self.memory.read_bytes(buffer, &mut contents);
        assert_eq!(&contents, b"SAVE");
    }

    fn assert_host_attributes(&self, name: &str, read_only: bool) {
        let path = self.temp_dir.path().join(name);
        let permissions = std::fs::metadata(&path).unwrap().permissions();
        assert_eq!(permissions.readonly(), read_only);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if read_only {
                assert_eq!(permissions.mode() & 0o222, 0);
            } else {
                assert_ne!(permissions.mode() & 0o200, 0);
            }
        }
        assert_eq!(std::fs::read(path).unwrap(), b"SAVE");
    }
}

fn assert_create_attributes_and_handle_access(ah: u8) {
    // MS-DOS 4.0 Programmer's Reference, Create Handle (3Ch), Create New
    // File (5Bh), Extended Open/Create (6Ch): creation allows I/O and the
    // requested attributes control later opens after the handle is closed.
    // https://www.pcjs.org/documents/books/mspl13/msdos/dosref40/
    for attributes in [ATTR_NORMAL, ATTR_READONLY] {
        // AH=5Bh/6Ch must see an absent target; retain the existing fixture's
        // seed under another name rather than deleting it to arrange the test.
        let mut fx = Fixture::new("SEED.TMP");
        const NAME: &str = "CREATE.TMP";
        const PATH: &str = "C:\\CREATE.TMP";
        let handle = fx.create(ah, PATH, attributes);
        fx.write_and_read_created_handle(handle);
        fx.close(handle);
        fx.assert_host_attributes(NAME, attributes == ATTR_READONLY);

        let (reopened, carry) = fx.int21(0x3D, 0, 0, PATH);
        assert!(!carry, "read-only reopen failed: AX={reopened}");
        let buffer = Memory::seg_offset_to_linear(GUEST_SEG, 0x0400);
        fx.memory.write_bytes_guest(buffer, &[0; 4]);
        assert_eq!(fx.transfer(0x3F, reopened), (4, false));
        let mut contents = [0; 4];
        fx.memory.read_bytes(buffer, &mut contents);
        assert_eq!(&contents, b"SAVE");
        fx.close(reopened);
        fx.assert_host_attributes(NAME, attributes == ATTR_READONLY);

        let (write_reopened, carry) = fx.int21(0x3D, 1, 0, PATH);
        if attributes == ATTR_READONLY {
            assert!(carry, "write reopen of read-only file must fail");
            assert_eq!(write_reopened, ERROR_ACCESS_DENIED);
        } else {
            assert!(!carry, "write reopen failed: AX={write_reopened}");
            assert_eq!(fx.transfer(0x40, write_reopened), (4, false));
            fx.close(write_reopened);
        }
        fx.assert_host_attributes(NAME, attributes == ATTR_READONLY);
        assert!(!fx.set_attributes(PATH, ATTR_NORMAL).1);
    }
}

#[test]
fn ah3c_create_attributes_preserve_creation_handle_access() {
    assert_create_attributes_and_handle_access(0x3C);
}

#[test]
fn ah5b_create_attributes_preserve_creation_handle_access() {
    assert_create_attributes_and_handle_access(0x5B);
}

#[test]
fn ah6c_create_attributes_preserve_creation_handle_access_and_action_code() {
    assert_create_attributes_and_handle_access(0x6C);

    let mut fx = Fixture::new("SEED.TMP");
    const NAME: &str = "OPEN.TMP";
    const PATH: &str = "C:\\OPEN.TMP";
    let handle = fx.create(0x6C, PATH, ATTR_NORMAL);
    fx.write_and_read_created_handle(handle);
    fx.close(handle);
    fx.write_path(PATH);
    fx.cpu.regs.set_ax(0x6C00);
    fx.cpu.regs.set_bx(2);
    fx.cpu.regs.set_cx(ATTR_READONLY);
    fx.cpu.regs.ds = GUEST_SEG;
    fx.cpu.regs.set_si(GUEST_OFF);
    fx.cpu.regs.set_dx(1); // open existing; CX attributes apply only to create
    let (reopened, carry) = fx.call_with_registers();
    assert!(!carry, "plain extended open failed: AX={reopened}");
    assert_eq!(fx.cpu.regs.cx(), 1, "action must be opened, not created");
    fx.write_and_read_created_handle(reopened);
    fx.close(reopened);
    fx.assert_host_attributes(NAME, false);
}

#[test]
fn saved_file_can_be_marked_readonly_and_restored_to_owner_writable() {
    // Independently constructed DOS operation case: create and write a file,
    // rename it to an unused name, then toggle its read-only attribute.
    let mut fx = Fixture::new("SEED.DAT");
    const NAME: &str = "RENAMED.DAT";
    const PATH: &str = "C:\\RENAMED.DAT";
    const TEMP: &str = "C:\\CREATED.TMP";
    let temporary = fx.create(0x5B, TEMP, ATTR_NORMAL);
    fx.write_and_read_created_handle(temporary);
    fx.close(temporary);

    fx.write_path(TEMP);
    let destination_offset = 0x0800;
    let destination_addr = Memory::seg_offset_to_linear(GUEST_SEG, destination_offset);
    fx.memory
        .write_bytes_guest(destination_addr, PATH.as_bytes());
    fx.memory.write_u8(destination_addr + PATH.len(), 0);
    fx.cpu.regs.set_ax(0x5600);
    fx.cpu.regs.ds = GUEST_SEG;
    fx.cpu.regs.set_dx(GUEST_OFF);
    fx.cpu.regs.es = GUEST_SEG;
    fx.cpu.regs.set_di(destination_offset);
    assert!(!fx.call_with_registers().1, "rename created file");
    assert!(!fx.exists("CREATED.TMP"));
    fx.assert_host_attributes(NAME, false);
    assert!(!fx.set_attributes(PATH, 0x21).1);
    fx.assert_host_attributes(NAME, true);
    assert!(!fx.set_attributes(PATH, 0x20).1);
    fx.assert_host_attributes(NAME, false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::metadata(fx.temp_dir.path().join(NAME))
            .unwrap()
            .permissions();
        assert_eq!(permissions.mode() & 0o222, 0o200);
    }
}

#[test]
fn a_readonly_file_cannot_be_deleted_until_its_attribute_is_cleared() {
    const NAME: &str = "RDONLY.TXT";
    let mut fx = Fixture::new(NAME);
    let path = format!("C:\\{NAME}");

    // Precondition: the file exists and marking it read-only sticks.
    assert!(fx.exists(NAME), "fixture must create the file");
    let (_, carry) = fx.set_attributes(&path, ATTR_READONLY);
    assert!(!carry, "setting the read-only attribute must succeed");

    // Deletion must preserve the read-only file.
    let (ax, carry) = fx.delete(&path);
    assert!(carry, "deleting a read-only file must set CF");
    assert_eq!(
        ax, ERROR_ACCESS_DENIED,
        "deleting a read-only file must report error 5 (access denied)"
    );
    assert!(fx.exists(NAME), "the file must still be on disk");

    // Clearing the attribute makes deletion eligible.
    let (_, carry) = fx.set_attributes(&path, ATTR_NORMAL);
    assert!(!carry, "clearing the read-only attribute must succeed");

    // Deleting the now-writable file succeeds.
    let (ax, carry) = fx.delete(&path);
    assert!(
        !carry,
        "deleting a writable file must clear CF, got AX={ax}"
    );
    assert_eq!(ax, 0, "a successful delete reports AX=0");
    assert!(!fx.exists(NAME), "the file must be gone");
}

#[test]
fn deleting_a_writable_file_is_unaffected_by_the_readonly_guard() {
    const NAME: &str = "PLAIN.TXT";
    let mut fx = Fixture::new(NAME);
    let path = format!("C:\\{NAME}");

    let (ax, carry) = fx.delete(&path);
    assert!(!carry, "an ordinary delete must not regress, got AX={ax}");
    assert_eq!(ax, 0);
    assert!(!fx.exists(NAME));
}

// MDD-BUG-FLU-02877: the replace action is 2, not open (1) plus
// truncate. DX=0012h requests replacement with creation if absent.
#[test]
fn ah6c_replace_existing_truncates_without_open_action_bit() {
    for action in [0x02, 0x12] {
        let mut fx = Fixture::new("DEST.TXT");
        fx.write_path("C:\\DEST.TXT");
        fx.cpu.regs.set_ax(0x6C00);
        fx.cpu.regs.set_bx(0x0042); // read/write, deny none
        fx.cpu.regs.set_cx(ATTR_NORMAL);
        fx.cpu.regs.ds = GUEST_SEG;
        fx.cpu.regs.set_si(GUEST_OFF);
        fx.cpu.regs.set_dx(action);
        let (handle, carry) = fx.call_with_registers();
        assert!(!carry, "replace action {action:04X} failed: AX={handle}");
        assert_eq!(fx.cpu.regs.cx(), 3, "must report replacement");
        assert_eq!(
            std::fs::metadata(fx.temp_dir.path().join("DEST.TXT"))
                .unwrap()
                .len(),
            0
        );
        fx.write_and_read_created_handle(handle);
        fx.close(handle);
        assert_eq!(
            std::fs::read(fx.temp_dir.path().join("DEST.TXT")).unwrap(),
            b"SAVE"
        );
    }
}
