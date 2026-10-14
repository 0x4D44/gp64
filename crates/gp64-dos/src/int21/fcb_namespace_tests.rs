//! MDD-BUG-CRUCIBLE-00446: FCB wildcard delete (AH=13h) and rename (AH=17h)
//! must work in the DOS 8.3 namespace, not the host namespace.
//!
//! The invariant these tests pin is "whatever Find First shows the guest, the
//! guest can then delete and rename". A host-backed drive can hold names DOS
//! cannot express, and the guest only ever learns the assigned 8.3 alias — so
//! matching an FCB pattern against the raw host name makes such a file
//! permanently undeletable and unrenameable from a DOS program.
//!
//! The alias is deliberately obtained from the emulator's own Find First
//! rather than recomputed here: the short-name *source* is host-dependent
//! (NTFS supplies it on Windows, the emulator's own assignment otherwise), so
//! hardcoding `LONGFI~1.TXT` would pin the wrong thing and could pass or fail
//! for reasons unrelated to this bug.

use super::*;
use crate::tests::{create_test_fcb, create_test_io};
use tempfile::TempDir;

/// A host name DOS cannot express: a space, and a 13-character base.
const LONG_HOST_NAME: &str = "Long Filename.txt";
/// A host name that is already a legal 8.3 name, so DOS shows it verbatim.
const SHORT_HOST_NAME: &str = "SHORT.TXT";

/// FCB drive byte 3 == C:, which `Dos::drives` holds under key 2.
const FCB_DRIVE_C: u8 = 3;
const DRIVES_KEY_C: u8 = 2;

struct Fixture {
    dos: Dos,
    cpu: gp64_cpu::Cpu,
    memory: Memory,
    vga: crate::Vga,
    keyboard: crate::Keyboard,
    temp_dir: TempDir,
}

impl Fixture {
    /// A C: drive holding one long host name and one already-8.3 name.
    fn new() -> Self {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(LONG_HOST_NAME), b"long").unwrap();
        std::fs::write(temp_dir.path().join(SHORT_HOST_NAME), b"short").unwrap();

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

    fn host_path(&self, name: &str) -> std::path::PathBuf {
        self.temp_dir.path().join(name)
    }

    /// Every name Find First reports for `C:*.*`, in DOS's own namespace.
    fn dos_visible_names(&mut self) -> Vec<String> {
        let mut names = Vec::new();
        let Ok(search_id) = self.dos.find_first("C:*.*", 0xFF) else {
            return names;
        };
        while let Some(entry) = self.dos.find_get_entry(search_id) {
            names.push(entry.name.clone());
            self.dos.find_advance(search_id);
        }
        names
    }

    /// The 8.3 alias DOS shows for `LONG_HOST_NAME`, asserted to differ from
    /// the host name — that difference is the precondition the whole bug rests
    /// on, so a fixture that failed to produce an alias must fail loudly
    /// rather than silently make the test vacuous.
    fn long_name_alias(&mut self) -> String {
        let names = self.dos_visible_names();
        assert_eq!(
            names.len(),
            2,
            "fixture should expose exactly the two files it created, got {names:?}"
        );
        let alias = names
            .iter()
            .find(|name| *name != SHORT_HOST_NAME)
            .unwrap_or_else(|| panic!("no alias for the long name among {names:?}"))
            .clone();
        assert_ne!(
            alias, LONG_HOST_NAME,
            "precondition: DOS must show an alias, not the raw host name"
        );
        assert!(
            names.iter().any(|name| name == SHORT_HOST_NAME),
            "precondition: an already-8.3 host name is shown verbatim, got {names:?}"
        );
        alias
    }

    /// Run one INT 21h call with `DS:DX` pointing at the FCB at 2000:0100.
    fn int21(&mut self, ah: u8) -> u8 {
        self.cpu.regs.set_ah(ah);
        self.cpu.regs.ds = 0x2000;
        self.cpu.regs.set_dx(0x0100);
        let result = self.dos.int21(
            &mut self.cpu,
            &mut self.memory,
            &mut self.vga,
            &mut self.keyboard,
            &mut crate::Pic::new(),
        );
        assert!(result.is_ok(), "int21 AH={ah:#04x} failed: {result:?}");
        self.cpu.regs.al()
    }

    fn write_fcb(&mut self, name: &str, ext: &str) {
        create_test_fcb(&mut self.memory, 0x2000, 0x0100, FCB_DRIVE_C, name, ext);
    }

    /// Write the AH=17h destination pattern, which lives 8+3 bytes past the
    /// source name inside the same FCB.
    fn write_rename_target(&mut self, name: &str, ext: &str) {
        let base = Memory::seg_offset_to_linear(0x2000, 0x0100);
        for i in 0..8 {
            let ch = name.as_bytes().get(i).copied().unwrap_or(b' ');
            self.memory.write_u8(base + 17 + i, ch);
        }
        for i in 0..3 {
            let ch = ext.as_bytes().get(i).copied().unwrap_or(b' ');
            self.memory.write_u8(base + 25 + i, ch);
        }
    }
}

/// Split an 8.3 name into the space-padded base and extension an FCB carries.
fn split_83(name: &str) -> (String, String) {
    match name.split_once('.') {
        Some((base, ext)) => (base.to_string(), ext.to_string()),
        None => (name.to_string(), String::new()),
    }
}

#[test]
fn fcb_wildcard_delete_matches_the_dos_visible_alias() {
    let mut fx = Fixture::new();
    let alias = fx.long_name_alias();
    let (base, ext) = split_83(&alias);

    // A pattern that matches the alias but cannot match the host name: the
    // host base is 13 characters with a space in it.
    let pattern_base = format!("{}?", &base[..base.len() - 1]);
    fx.write_fcb(&pattern_base, &ext);

    assert_eq!(
        fx.int21(0x13),
        0x00,
        "deleting {pattern_base}.{ext} (alias {alias}) should report success"
    );
    assert!(
        !fx.host_path(LONG_HOST_NAME).exists(),
        "the host file behind the alias should be gone"
    );
    assert!(
        fx.host_path(SHORT_HOST_NAME).exists(),
        "a non-matching file must be untouched"
    );
}

#[test]
fn fcb_wildcard_delete_still_matches_an_ordinary_short_name() {
    let mut fx = Fixture::new();
    fx.write_fcb("SHOR?", "TXT");

    assert_eq!(fx.int21(0x13), 0x00);
    assert!(!fx.host_path(SHORT_HOST_NAME).exists());
    assert!(
        fx.host_path(LONG_HOST_NAME).exists(),
        "the long-named file does not match SHOR?.TXT and must survive"
    );
}

#[test]
fn fcb_wildcard_delete_reports_no_match_for_the_raw_host_name() {
    // The host name is not in the DOS namespace at all, so a pattern derived
    // from it must miss — proving the match really moved namespaces rather
    // than merely widening to accept both.
    let mut fx = Fixture::new();
    fx.write_fcb("Long Fi", "txt");

    assert_eq!(fx.int21(0x13), 0xFF);
    assert!(fx.host_path(LONG_HOST_NAME).exists());
    assert!(fx.host_path(SHORT_HOST_NAME).exists());
}

#[test]
fn fcb_wildcard_rename_matches_the_alias_and_derives_the_target_from_it() {
    let mut fx = Fixture::new();
    let alias = fx.long_name_alias();
    let (base, ext) = split_83(&alias);

    let pattern_base = format!("{}?", &base[..base.len() - 1]);
    fx.write_fcb(&pattern_base, &ext);
    fx.write_rename_target("RENAMED", "DAT");

    assert_eq!(
        fx.int21(0x17),
        0x00,
        "renaming {pattern_base}.{ext} (alias {alias}) should report success"
    );
    assert!(
        !fx.host_path(LONG_HOST_NAME).exists(),
        "the host file behind the alias should have moved"
    );
    assert!(
        fx.host_path("RENAMED.DAT").exists(),
        "the destination is the literal replacement pattern"
    );
    assert!(fx.host_path(SHORT_HOST_NAME).exists());
}

#[test]
fn fcb_wildcard_rename_carries_alias_characters_through_a_wildcard_target() {
    // The '?' placeholders in the destination copy characters from the SOURCE
    // name. Those must come from the alias the guest can see, not from the
    // host name — otherwise the destination is spelled out of characters the
    // guest never had.
    let mut fx = Fixture::new();
    let alias = fx.long_name_alias();
    let (base, ext) = split_83(&alias);

    let pattern_base = format!("{}?", &base[..base.len() - 1]);
    fx.write_fcb(&pattern_base, &ext);
    // Keep the whole base, force the extension to BAK.
    fx.write_rename_target("????????", "BAK");

    assert_eq!(fx.int21(0x17), 0x00);
    let expected = format!("{base}.BAK");
    assert!(
        fx.host_path(&expected).exists(),
        "expected {expected} from alias {alias}; directory now holds {:?}",
        std::fs::read_dir(fx.temp_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );
    assert!(!fx.host_path(LONG_HOST_NAME).exists());
}

#[test]
fn fcb_wildcard_rename_still_matches_an_ordinary_short_name() {
    let mut fx = Fixture::new();
    fx.write_fcb("SHOR?", "TXT");
    fx.write_rename_target("PLAIN", "DAT");

    assert_eq!(fx.int21(0x17), 0x00);
    assert!(!fx.host_path(SHORT_HOST_NAME).exists());
    assert!(fx.host_path("PLAIN.DAT").exists());
    assert!(fx.host_path(LONG_HOST_NAME).exists());
}
