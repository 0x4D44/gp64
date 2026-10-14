use super::*;
use crate::tests::create_test_io;
use std::collections::BTreeSet;
use std::path::Path;
use tempfile::TempDir;

const GUEST_PATH_SEG: u16 = 0x2000;
const GUEST_PATH_OFF: u16 = 0x0100;
const GUEST_DATA_SEG: u16 = 0x3000;
const GUEST_DATA_OFF: u16 = 0x0200;

struct Fixture {
    dos: Dos,
    cpu: gp64_cpu::Cpu,
    memory: Memory,
    vga: crate::Vga,
    keyboard: crate::Keyboard,
    _temp_dir: TempDir,
}

impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        Self::new_with_dirs(files, &[])
    }

    fn new_with_dirs(files: &[(&str, &[u8])], dirs: &[&str]) -> Self {
        let temp_dir = TempDir::new().unwrap();
        Self::new_with_temp_dir(files, dirs, temp_dir)
    }

    fn new_in_scratch(files: &[(&str, &[u8])]) -> Self {
        let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let temp_dir = tempfile::Builder::new()
            .prefix(&format!("lfn-delete-{}-", std::process::id()))
            .tempdir_in(scratch)
            .unwrap();
        Self::new_with_temp_dir(files, &[], temp_dir)
    }

    fn new_with_temp_dir(files: &[(&str, &[u8])], dirs: &[&str], temp_dir: TempDir) -> Self {
        for directory in dirs {
            std::fs::create_dir_all(temp_dir.path().join(directory)).unwrap();
        }
        for (name, contents) in files {
            std::fs::write(temp_dir.path().join(name), contents).unwrap();
        }

        let mut dos = Dos::new();
        dos.drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(
                temp_dir.path().to_path_buf(),
            )),
        );
        let (vga, keyboard) = create_test_io();
        Self {
            dos,
            cpu: gp64_cpu::Cpu::new(),
            memory: Memory::new(),
            vga,
            keyboard,
            _temp_dir: temp_dir,
        }
    }

    fn write_path(&mut self, path: &str) {
        let address = Memory::seg_offset_to_linear(GUEST_PATH_SEG, GUEST_PATH_OFF);
        for (index, byte) in path.bytes().chain(std::iter::once(0)).enumerate() {
            self.memory.write_u8(address + index, byte);
        }
    }

    fn set_legacy_attributes(&mut self, path: &str, attributes: u16) -> (u16, bool) {
        self.write_path(path);
        self.cpu.regs.set_ax(0x4301);
        self.cpu.regs.set_cx(attributes);
        self.cpu.regs.ds = GUEST_PATH_SEG;
        self.cpu.regs.set_dx(GUEST_PATH_OFF);
        self.dos
            .int21(
                &mut self.cpu,
                &mut self.memory,
                &mut self.vga,
                &mut self.keyboard,
                &mut crate::Pic::new(),
            )
            .unwrap();
        (
            self.cpu.regs.ax(),
            self.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        )
    }

    fn delete_file(&mut self, path: &str, wildcard_mode: u16, attributes: u16) -> (u16, bool) {
        self.write_path(path);
        self.cpu.regs.set_ax(0x7141);
        self.cpu.regs.set_cx(attributes);
        self.cpu.regs.set_si(wildcard_mode);
        self.cpu.regs.ds = GUEST_PATH_SEG;
        self.cpu.regs.set_dx(GUEST_PATH_OFF);
        self.dos
            .int21(
                &mut self.cpu,
                &mut self.memory,
                &mut self.vga,
                &mut self.keyboard,
                &mut crate::Pic::new(),
            )
            .unwrap();
        (
            self.cpu.regs.ax(),
            self.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        )
    }

    fn host_file_exists(&self, path: &str) -> bool {
        self._temp_dir.path().join(path).is_file()
    }

    fn find_first(&mut self, pattern: &str) -> (u16, bool) {
        self.find_first_with_time_format(pattern, 0)
    }

    fn find_first_with_time_format(&mut self, pattern: &str, time_format: u16) -> (u16, bool) {
        self.write_path(pattern);
        self.cpu.regs.set_ax(0x714E);
        self.cpu.regs.set_cx(0);
        self.cpu.regs.set_si(time_format);
        self.cpu.regs.ds = GUEST_PATH_SEG;
        self.cpu.regs.set_dx(GUEST_PATH_OFF);
        self.cpu.regs.es = GUEST_DATA_SEG;
        self.cpu.regs.set_di(GUEST_DATA_OFF);
        self.dos
            .int21(
                &mut self.cpu,
                &mut self.memory,
                &mut self.vga,
                &mut self.keyboard,
                &mut crate::Pic::new(),
            )
            .unwrap();
        (
            self.cpu.regs.ax(),
            self.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        )
    }

    fn find_next(&mut self, handle: u16, time_format: u16) -> (u16, bool) {
        self.cpu.regs.set_ax(0x714F);
        self.cpu.regs.set_bx(handle);
        self.cpu.regs.set_si(time_format);
        self.cpu.regs.es = GUEST_DATA_SEG;
        self.cpu.regs.set_di(GUEST_DATA_OFF);
        self.dos
            .int21(
                &mut self.cpu,
                &mut self.memory,
                &mut self.vga,
                &mut self.keyboard,
                &mut crate::Pic::new(),
            )
            .unwrap();
        (
            self.cpu.regs.ax(),
            self.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        )
    }

    fn find_close(&mut self, handle: u16) -> (u16, bool) {
        self.cpu.regs.set_ax(0x71A1);
        self.cpu.regs.set_bx(handle);
        self.dos
            .int21(
                &mut self.cpu,
                &mut self.memory,
                &mut self.vga,
                &mut self.keyboard,
                &mut crate::Pic::new(),
            )
            .unwrap();
        (
            self.cpu.regs.ax(),
            self.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        )
    }

    fn find_data_address(&self) -> usize {
        Memory::seg_offset_to_linear(GUEST_DATA_SEG, GUEST_DATA_OFF)
    }

    fn read_c_string(&self, offset: usize, max_len: usize) -> String {
        let address = self.find_data_address() + offset;
        (0..max_len)
            .map(|index| self.memory.read_u8(address + index))
            .take_while(|byte| *byte != 0)
            .map(char::from)
            .collect()
    }
}

#[test]
fn lfn_find_first_returns_long_name_and_short_alias() {
    let mut fixture = Fixture::new(&[("Long File Name.txt", b"hello")]);

    let (handle, carry) = fixture.find_first(r"C:\*.TXT");

    assert!(!carry, "LFN FindFirst failed with AX={handle:#06X}");
    assert_ne!(handle, 0, "zero is not a valid LFN search handle");
    let data = fixture.find_data_address();
    assert_eq!(fixture.memory.read_u32(data), 0x20);
    assert_eq!(fixture.memory.read_u32(data + 28), 0);
    assert_eq!(fixture.memory.read_u32(data + 32), 5);
    assert_eq!(fixture.read_c_string(44, 260), "Long File Name.txt");
    assert_eq!(fixture.read_c_string(304, 14), "LONGFI~1.TXT");
    for offset in [4usize, 12, 20] {
        assert_ne!(fixture.memory.read_u32(data + offset), 0);
    }
}

#[test]
fn lfn_find_next_walks_entries_and_close_releases_handle() {
    let mut fixture = Fixture::new(&[
        ("Long File Name.txt", b"hello"),
        ("Second File.txt", b"world!"),
    ]);

    let (handle, carry) = fixture.find_first(r"C:\*.TXT");
    assert!(!carry);
    let first = fixture.read_c_string(44, 260);

    let (next_ax, carry) = fixture.find_next(handle, 0);
    assert!(!carry, "LFN FindNext failed with AX={next_ax:#06X}");
    assert_eq!(next_ax & 0xFF00, 0x4F00);
    let second = fixture.read_c_string(44, 260);
    assert_ne!(first, second);
    assert_eq!(
        BTreeSet::from([first, second]),
        BTreeSet::from([
            "Long File Name.txt".to_string(),
            "Second File.txt".to_string()
        ])
    );

    let (end_ax, carry) = fixture.find_next(handle, 0);
    assert!(carry);
    assert_eq!(end_ax, 18, "FindNext must report no more files");

    let (close_ax, carry) = fixture.find_close(handle);
    assert!(!carry);
    assert_eq!(close_ax, 0);
    let (invalid_ax, carry) = fixture.find_close(handle);
    assert!(carry);
    assert_eq!(invalid_ax, 6, "closed handles must become invalid");
}

#[test]
fn lfn_find_matches_long_wildcards_and_short_aliases() {
    let mut fixture = Fixture::new(&[("Chapter One1.txt", b"one")]);

    // DOS 7 LFN matching applies the wildcard to the complete filename, so
    // `*One1*` matches a substring in the complete long filename.
    let (handle, carry) = fixture.find_first(r"C:\*One1*");
    assert!(!carry);
    assert_eq!(fixture.read_c_string(44, 260), "Chapter One1.txt");
    fixture.find_close(handle);

    let (handle, carry) = fixture.find_first(r"C:\CHAPTE~1.TXT");
    assert!(!carry);
    assert_eq!(fixture.read_c_string(44, 260), "Chapter One1.txt");
    fixture.find_close(handle);
}

#[test]
fn lfn_find_resolves_long_directory_components() {
    let mut fixture = Fixture::new_with_dirs(
        &[("Long Directory/Read Me.txt", b"contents")],
        &["Long Directory"],
    );

    let (handle, carry) = fixture.find_first(r"C:\Long Directory\*.TXT");
    assert!(!carry);
    assert_eq!(fixture.read_c_string(44, 260), "Read Me.txt");
    fixture.find_close(handle);
}

#[test]
fn lfn_find_supports_dos_datetime_records() {
    let mut fixture = Fixture::new(&[("Long File Name.txt", b"hello")]);

    let (handle, carry) = fixture.find_first_with_time_format(r"C:\*.TXT", 1);
    assert!(!carry);
    let data = fixture.find_data_address();
    for offset in [4usize, 12, 20] {
        assert_ne!(fixture.memory.read_u32(data + offset), 0);
        assert_eq!(fixture.memory.read_u32(data + offset + 4), 0);
    }
    fixture.find_close(handle);
}

#[test]
fn lfn_find_rejects_an_unknown_time_format() {
    let mut fixture = Fixture::new(&[("Long File Name.txt", b"hello")]);

    let (error, carry) = fixture.find_first_with_time_format(r"C:\*.TXT", 2);
    assert!(carry);
    assert_eq!(error, 1);
}

#[test]
fn lfn_find_reports_unicode_conversion_and_uses_underscore() {
    let mut fixture = Fixture::new(&[("A\u{2554}Name.txt", b"hello")]);

    let (handle, carry) = fixture.find_first(r"C:\*");
    assert!(!carry);
    assert_eq!(fixture.cpu.regs.cx(), 1, "primary name conversion flag");
    assert_eq!(fixture.read_c_string(44, 260), "A_Name.txt");
    fixture.find_close(handle);
}

#[test]
fn lfn_find_snapshot_restores_search_cursor() {
    let mut fixture = Fixture::new(&[
        ("First Long Name.txt", b"one"),
        ("Second Long Name.txt", b"two"),
    ]);

    let (handle, carry) = fixture.find_first(r"C:\*.TXT");
    assert!(!carry);
    let snapshot = fixture.dos.snapshot();

    let (_, carry) = fixture.find_next(handle, 0);
    assert!(!carry);
    let expected_second = fixture.read_c_string(44, 260);

    fixture.dos.restore(&snapshot);
    let (_, carry) = fixture.find_next(handle, 0);
    assert!(!carry);
    assert_eq!(fixture.read_c_string(44, 260), expected_second);
    fixture.find_close(handle);
}

#[test]
fn lfn_find_attribute_masks_follow_dos_rules() {
    let normal = crate::filesystem::FileAttributes {
        archive: true,
        ..Default::default()
    };
    let mut hidden = normal;
    hidden.hidden = true;
    let mut directory = normal;
    directory.directory = true;

    assert!(Dos::matches_lfn_attr_filter(&normal, 0x0000));
    assert!(!Dos::matches_lfn_attr_filter(&hidden, 0x0000));
    assert!(Dos::matches_lfn_attr_filter(&hidden, 0x0002));
    assert!(Dos::matches_lfn_attr_filter(&hidden, 0x0202));
    assert!(!Dos::matches_lfn_attr_filter(&hidden, 0x0200));
    assert!(!Dos::matches_lfn_attr_filter(&directory, 0x0000));
    assert!(Dos::matches_lfn_attr_filter(&directory, 0x0010));
}

#[test]
fn lfn_find_errors_distinguish_missing_path_and_file() {
    let mut fixture = Fixture::new(&[("present.txt", b"hello")]);

    let (path_ax, carry) = fixture.find_first(r"C:\Missing Directory\*.TXT");
    assert!(carry);
    assert_eq!(path_ax, 3);

    let (file_ax, carry) = fixture.find_first(r"C:\absent.txt");
    assert!(carry);
    assert_eq!(file_ax, 2);
}

#[test]
fn lfn_delete_literal_rejects_read_only_files_and_allows_after_clear() {
    let mut fixture = Fixture::new_in_scratch(&[("LOCKED.TXT", b"contents")]);
    let (set_ax, set_carry) = fixture.set_legacy_attributes(r"C:\LOCKED.TXT", 0x01);
    let (delete_ax, delete_carry) = fixture.delete_file(r"C:\LOCKED.TXT", 0, 0);
    let preserved = fixture.host_file_exists("LOCKED.TXT");
    let (clear_ax, clear_carry) = fixture.set_legacy_attributes(r"C:\LOCKED.TXT", 0);
    let (writable_ax, writable_carry) = fixture.delete_file(r"C:\LOCKED.TXT", 0, 0);
    let deleted_after_clear = !fixture.host_file_exists("LOCKED.TXT");

    assert!(!set_carry, "AH=4301h failed with AX={set_ax:#06X}");
    assert!(
        delete_carry,
        "read-only literal deletion must return access denied (AX={delete_ax:#06X})"
    );
    assert_eq!(
        delete_ax, 5,
        "read-only literal deletion must return error 5"
    );
    assert!(preserved, "read-only literal deletion removed the file");
    assert!(
        !clear_carry,
        "clearing read-only failed with AX={clear_ax:#06X}"
    );
    assert!(
        !writable_carry,
        "cleared literal deletion failed with AX={writable_ax:#06X}"
    );
    assert_eq!(writable_ax, 0);
    assert!(
        deleted_after_clear,
        "writable literal deletion left the file"
    );
}

#[test]
fn lfn_delete_wildcard_rejects_matching_read_only_files() {
    let mut fixture = Fixture::new_in_scratch(&[("LOCKED.TXT", b"contents")]);
    let (set_ax, set_carry) = fixture.set_legacy_attributes(r"C:\LOCKED.TXT", 0x01);
    let (delete_ax, delete_carry) = fixture.delete_file(r"C:\*.TXT", 1, 0);
    let preserved = fixture.host_file_exists("LOCKED.TXT");
    let (clear_ax, clear_carry) = fixture.set_legacy_attributes(r"C:\LOCKED.TXT", 0);
    let (writable_ax, writable_carry) = fixture.delete_file(r"C:\*.TXT", 1, 0);
    let deleted_after_clear = !fixture.host_file_exists("LOCKED.TXT");

    assert!(!set_carry, "AH=4301h failed with AX={set_ax:#06X}");
    assert!(
        delete_carry,
        "read-only wildcard deletion must return access denied (AX={delete_ax:#06X})"
    );
    assert_eq!(
        delete_ax, 5,
        "read-only wildcard deletion must return error 5"
    );
    assert!(preserved, "read-only wildcard deletion removed the file");
    assert!(
        !clear_carry,
        "clearing read-only failed with AX={clear_ax:#06X}"
    );
    assert!(
        !writable_carry,
        "cleared wildcard deletion failed with AX={writable_ax:#06X}"
    );
    assert_eq!(writable_ax, 0);
    assert!(
        deleted_after_clear,
        "cleared wildcard deletion left the file"
    );
}

#[test]
fn lfn_delete_wildcard_keeps_no_match_and_writable_file_behavior() {
    let mut fixture = Fixture::new_in_scratch(&[("OPEN.TXT", b"contents")]);
    let (no_match_ax, no_match_carry) = fixture.delete_file(r"C:\*.BIN", 1, 0);
    let preserved_after_no_match = fixture.host_file_exists("OPEN.TXT");
    let (delete_ax, delete_carry) = fixture.delete_file(r"C:\*.TXT", 1, 0);
    let deleted = !fixture.host_file_exists("OPEN.TXT");

    assert!(no_match_carry, "a wildcard with no match must fail");
    assert_eq!(
        no_match_ax, 2,
        "a wildcard with no match must return error 2"
    );
    assert!(
        preserved_after_no_match,
        "a no-match wildcard changed the file"
    );
    assert!(
        !delete_carry,
        "writable wildcard deletion failed with AX={delete_ax:#06X}"
    );
    assert_eq!(delete_ax, 0);
    assert!(deleted, "writable wildcard deletion left the file");
}
