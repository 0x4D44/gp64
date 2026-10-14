//! Native Windows attributes must survive host enumeration and DOS searches.

use super::*;
use crate::tests::create_test_io;
use std::collections::BTreeMap;
use std::os::windows::fs::MetadataExt;
use tempfile::TempDir;

const PATH_SEG: u16 = 0x2000;
const PATH_OFF: u16 = 0x0100;
const DATA_SEG: u16 = 0x3000;
const DATA_OFF: u16 = 0x0200;
const FILES: [(&str, u16); 4] = [
    ("NORMAL.TXT", 0x20),
    ("HIDDEN.TXT", 0x22),
    ("SYSTEM.TXT", 0x24),
    ("BOTH.TXT", 0x26),
];

struct Fixture {
    dos: Dos,
    cpu: gp64_cpu::Cpu,
    memory: Memory,
    vga: crate::Vga,
    keyboard: crate::Keyboard,
    _root: TempDir,
    _overlay: Option<TempDir>,
}

impl Fixture {
    fn new(overlay: bool) -> Self {
        let root = TempDir::new().unwrap();
        let upper = overlay.then(|| TempDir::new().unwrap());
        for (name, _) in FILES {
            std::fs::write(root.path().join(name), b"root fixture").unwrap();
            if let Some(upper) = &upper {
                std::fs::write(upper.path().join(name), b"overlay fixture").unwrap();
            }
        }
        let mut drive = crate::filesystem::HostDrive::new(root.path().to_path_buf());
        if let Some(upper) = &upper {
            drive.set_write_overlay(upper.path().to_path_buf());
        }
        let mut dos = Dos::new();
        dos.drives.insert(2, Box::new(drive));
        let (vga, keyboard) = create_test_io();
        let mut fixture = Self {
            dos,
            cpu: gp64_cpu::Cpu::new(),
            memory: Memory::new(),
            vga,
            keyboard,
            _root: root,
            _overlay: upper,
        };
        for (name, attributes) in FILES {
            fixture.path(&format!(r"C:\{name}"));
            fixture.cpu.regs.set_cx(attributes);
            fixture.call(0x4301);
            assert!(!fixture.carry(), "AH=43 setter failed for {name}");
            fixture.call(0x4300);
            assert!(!fixture.carry(), "AH=43 getter failed for {name}");
            assert_eq!(fixture.cpu.regs.cx(), attributes, "AH=43 bits for {name}");
            let effective = fixture
                ._overlay
                .as_ref()
                .unwrap_or(&fixture._root)
                .path()
                .join(name);
            assert_eq!(
                std::fs::metadata(effective).unwrap().file_attributes() & 0x27,
                u32::from(attributes),
                "native Windows attributes for {name}"
            );
            if fixture._overlay.is_some() {
                assert_eq!(
                    std::fs::metadata(fixture._root.path().join(name))
                        .unwrap()
                        .file_attributes()
                        & 0x06,
                    0,
                    "AH=43 must update the overlay rather than its root shadow"
                );
            }
        }
        fixture
    }

    fn path(&mut self, path: &str) {
        let address = Memory::seg_offset_to_linear(PATH_SEG, PATH_OFF);
        for (index, byte) in path.bytes().chain([0]).enumerate() {
            self.memory.write_u8(address + index, byte);
        }
        self.cpu.regs.ds = PATH_SEG;
        self.cpu.regs.set_dx(PATH_OFF);
    }

    fn call(&mut self, ax: u16) {
        self.cpu.regs.set_ax(ax);
        self.dos
            .int21(
                &mut self.cpu,
                &mut self.memory,
                &mut self.vga,
                &mut self.keyboard,
                &mut crate::Pic::new(),
            )
            .unwrap();
    }

    fn carry(&self) -> bool {
        self.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY)
    }

    fn data_address(&self) -> usize {
        Memory::seg_offset_to_linear(DATA_SEG, DATA_OFF)
    }

    fn name(&self, offset: usize, max_length: usize) -> String {
        (0..max_length)
            .map(|index| self.memory.read_u8(self.data_address() + offset + index))
            .take_while(|byte| *byte != 0)
            .map(char::from)
            .collect()
    }

    fn search(&mut self, mask: u16, lfn: bool) -> BTreeMap<String, u32> {
        self.cpu.regs.ds = DATA_SEG;
        self.cpu.regs.set_dx(DATA_OFF);
        self.call(0x1A00);
        self.path(r"C:\*.TXT");
        self.cpu.regs.set_cx(mask);
        self.cpu.regs.set_si(0);
        self.cpu.regs.es = DATA_SEG;
        self.cpu.regs.set_di(DATA_OFF);
        self.call(if lfn { 0x714E } else { 0x4E00 });
        assert!(!self.carry(), "FindFirst failed for mask {mask:#04x}");
        let handle = self.cpu.regs.ax();
        let mut results = BTreeMap::new();
        loop {
            let (name, attributes) = if lfn {
                (
                    self.name(44, 260),
                    self.memory.read_u32(self.data_address()),
                )
            } else {
                (
                    self.name(0x1E, 13),
                    u32::from(self.memory.read_u8(self.data_address() + 0x15)),
                )
            };
            assert!(
                results.insert(name, attributes).is_none(),
                "duplicate result"
            );
            assert!(results.len() <= FILES.len(), "search did not terminate");
            self.cpu.regs.set_bx(handle);
            self.call(if lfn { 0x714F } else { 0x4F00 });
            if self.carry() {
                assert_eq!(self.cpu.regs.ax(), 18, "FindNext end of search");
                break;
            }
        }
        if lfn {
            self.cpu.regs.set_bx(handle);
            self.call(0x71A1);
            assert!(!self.carry(), "LFN search close failed");
        }
        results
    }
}

fn expected(mask: u16) -> BTreeMap<String, u32> {
    let entries: &[(&str, u32)] = match mask {
        0 => &[("NORMAL.TXT", 0x20)],
        2 => &[("NORMAL.TXT", 0x20), ("HIDDEN.TXT", 0x22)],
        4 => &[("NORMAL.TXT", 0x20), ("SYSTEM.TXT", 0x24)],
        6 => &[
            ("NORMAL.TXT", 0x20),
            ("HIDDEN.TXT", 0x22),
            ("SYSTEM.TXT", 0x24),
            ("BOTH.TXT", 0x26),
        ],
        _ => unreachable!(),
    };
    entries
        .iter()
        .map(|(name, attributes)| ((*name).to_string(), *attributes))
        .collect()
}

fn check_listing(overlay: bool) {
    let mut fixture = Fixture::new(overlay);
    let entries = fixture
        .dos
        .drives
        .get_mut(&2)
        .unwrap()
        .read_dir(r"\")
        .unwrap();
    let actual: BTreeMap<_, _> = entries
        .iter()
        .map(|(name, attributes)| (name.clone(), u32::from(Dos::attrs_to_dos_byte(attributes))))
        .collect();
    assert_eq!(actual, expected(6));
}

fn check_search(overlay: bool, lfn: bool) {
    let mut fixture = Fixture::new(overlay);
    for mask in [0, 2, 4, 6] {
        assert_eq!(
            fixture.search(mask, lfn),
            expected(mask),
            "mask {mask:#04x}"
        );
    }
}

#[test]
fn windows_search_attrs_root_listing() {
    check_listing(false);
}

#[test]
fn windows_search_attrs_overlay_listing() {
    check_listing(true);
}

#[test]
fn windows_search_attrs_root_legacy() {
    check_search(false, false);
}

#[test]
fn windows_search_attrs_overlay_legacy() {
    check_search(true, false);
}

#[test]
fn windows_search_attrs_root_lfn() {
    check_search(false, true);
}

#[test]
fn windows_search_attrs_overlay_lfn() {
    check_search(true, true);
}
