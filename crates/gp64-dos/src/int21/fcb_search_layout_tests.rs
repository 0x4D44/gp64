//! MDD-BUG-CRUCIBLE-00447: FCB Find First/Next must write the MS-DOS 3.3
//! search-result layout into the DTA.
//!
//! Both AH=11h and AH=12h used to serialize the result inline and both got it
//! wrong the same way: attribute at DTA+21, size at DTA+28, no time or date,
//! and no extended-FCB prefix. Those offsets corrupt unrelated bytes of the
//! directory entry and leave the metadata fields holding whatever the guest
//! last put there.
//!
//! The reference (Function 11h) defines a NORMAL result as one drive byte plus
//! a 32-byte directory entry, and an EXTENDED result as a seven-byte prefix
//! plus that same normal result. Every offset asserted below is a SPEC
//! LITERAL, deliberately not the production constant, so a change to the
//! constants cannot silently move the layout.
//!
//! Reference: https://www.pcjs.org/documents/books/mspl13/msdos/dosref33/

use crate::filesystem::HostDrive;
use crate::{Dos, FindEntry};
use gp64_memory::Memory;

/// Fill the DTA region with a sentinel so any byte the serializer does NOT
/// write is visible as untouched.
const SENTINEL: u8 = 0xE7;
const DTA_SEG: u16 = 0x3000;
const DTA_LINEAR: usize = (DTA_SEG as usize) << 4;
/// Enough room for the extended result plus slack to prove nothing runs over.
const PROBE_LEN: usize = 64;

fn known_entry() -> FindEntry {
    FindEntry {
        name: "REPORT.DAT".to_string(),
        size: 0x1234_5678,
        attr: 0x21,    // ARCHIVE | READONLY
        time: 0x5A_3C, // distinctive, non-zero
        date: 0x2B_15, // distinctive, non-zero
    }
}

fn sentinel_memory() -> Memory {
    let mut memory = Memory::new();
    for i in 0..PROBE_LEN {
        memory.write_u8(DTA_LINEAR + i, SENTINEL);
    }
    memory
}

#[test]
fn normal_fcb_search_result_matches_the_msdos_layout_crucible447() {
    let mut memory = sentinel_memory();
    let entry = known_entry();

    let written = Dos::fcb_write_search_result(
        &mut memory,
        DTA_LINEAR,
        false,
        0,
        3, // drive C:
        &entry.name,
        &entry,
    );

    // One drive byte + a 32-byte directory entry.
    assert_eq!(written, 33, "a normal result is 1 + 32 bytes");

    let at = |off: usize| memory.read_u8(DTA_LINEAR + off);
    assert_eq!(at(0), 3, "drive byte");
    assert_eq!(
        (1..9).map(at).collect::<Vec<_>>(),
        b"REPORT  ".to_vec(),
        "8-character name, space padded"
    );
    assert_eq!(
        (9..12).map(at).collect::<Vec<_>>(),
        b"DAT".to_vec(),
        "3-character extension"
    );
    assert_eq!(at(12), 0x21, "attribute at DTA+12 (directory entry +11)");
    assert_eq!(
        memory.read_u16(DTA_LINEAR + 23),
        0x5A3C,
        "time at DTA+23 (directory entry +22)"
    );
    assert_eq!(
        memory.read_u16(DTA_LINEAR + 25),
        0x2B15,
        "date at DTA+25 (directory entry +24)"
    );
    assert_eq!(
        memory.read_u32(DTA_LINEAR + 29),
        0x1234_5678,
        "size at DTA+29 (directory entry +28)"
    );

    // The old layout put the attribute here and the size at +28. Both must now
    // be defined bytes of the directory entry, not the metadata they used to
    // hold — this is what proves the offsets actually moved.
    assert_eq!(at(21), 0, "DTA+21 is a reserved byte, not the attribute");
    assert_ne!(
        memory.read_u32(DTA_LINEAR + 28),
        0x1234_5678,
        "DTA+28 must no longer hold the size"
    );

    // Nothing past the 33-byte result was touched.
    for off in 33..PROBE_LEN {
        assert_eq!(
            at(off),
            SENTINEL,
            "byte {off} is past the result and must be untouched"
        );
    }
}

#[test]
fn extended_fcb_search_result_carries_its_prefix_crucible447() {
    let mut memory = sentinel_memory();
    let entry = known_entry();
    let search_attr = 0x16;

    let written = Dos::fcb_write_search_result(
        &mut memory,
        DTA_LINEAR,
        true,
        search_attr,
        3,
        &entry.name,
        &entry,
    );

    assert_eq!(
        written,
        7 + 33,
        "an extended result is a 7-byte prefix + 33"
    );

    let at = |off: usize| memory.read_u8(DTA_LINEAR + off);
    assert_eq!(at(0), 0xFF, "extended marker");
    for off in 1..6 {
        assert_eq!(at(off), 0, "prefix byte {off} is reserved and zeroed");
    }
    assert_eq!(
        at(6),
        search_attr,
        "search attribute echoed into the prefix"
    );

    // The normal result follows, shifted by exactly the prefix length.
    assert_eq!(at(7), 3, "drive byte after the prefix");
    assert_eq!(
        (8..16).map(at).collect::<Vec<_>>(),
        b"REPORT  ".to_vec(),
        "name after the prefix"
    );
    assert_eq!(at(7 + 12), 0x21, "attribute at prefix + 12");
    assert_eq!(memory.read_u16(DTA_LINEAR + 7 + 23), 0x5A3C, "time");
    assert_eq!(memory.read_u16(DTA_LINEAR + 7 + 25), 0x2B15, "date");
    assert_eq!(memory.read_u32(DTA_LINEAR + 7 + 29), 0x1234_5678, "size");

    for off in (7 + 33)..PROBE_LEN {
        assert_eq!(at(off), SENTINEL, "byte {off} must be untouched");
    }
}

/// A name with no extension must still space-pad the extension field rather
/// than leaving the guest's previous bytes there.
#[test]
fn extensionless_name_pads_the_extension_field_crucible447() {
    let mut memory = sentinel_memory();
    let entry = FindEntry {
        name: "README".to_string(),
        ..known_entry()
    };

    Dos::fcb_write_search_result(&mut memory, DTA_LINEAR, false, 0, 3, &entry.name, &entry);

    let at = |off: usize| memory.read_u8(DTA_LINEAR + off);
    assert_eq!((1..9).map(at).collect::<Vec<_>>(), b"README  ".to_vec());
    assert_eq!(
        (9..12).map(at).collect::<Vec<_>>(),
        b"   ".to_vec(),
        "an absent extension must be spaces, not sentinel leftovers"
    );
}

/// End to end through INT 21h AH=11h, so the handler is proven to use the
/// serializer rather than merely the serializer being correct.
#[test]
fn int21_fcb_find_first_writes_the_spec_layout_crucible447() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("DATA.BIN"), vec![0u8; 300]).unwrap();

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = sentinel_memory();
    let (mut vga, mut keyboard) = crate::tests::create_test_io();

    dos.drives
        .insert(2, Box::new(HostDrive::new(temp_dir.path().to_path_buf())));
    dos.set_dta(DTA_SEG, 0x0000);
    crate::tests::create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "DATA", "BIN");

    cpu.regs.set_ah(0x11);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut gp64_hw::Pic::new(),
    )
    .expect("int21 must succeed");
    assert_eq!(cpu.regs.al(), 0x00, "the file must be found");

    assert_eq!(
        memory.read_u32(DTA_LINEAR + 29),
        300,
        "size must land at DTA+29 through the real handler"
    );
    assert_eq!(
        memory.read_u8(DTA_LINEAR + 21),
        0,
        "DTA+21 must be a reserved byte, not the attribute"
    );
    for off in 33..PROBE_LEN {
        assert_eq!(
            memory.read_u8(DTA_LINEAR + off),
            SENTINEL,
            "byte {off} is past the result and must be untouched"
        );
    }
}
