//! MDD-BUG-CRUCIBLE-00441: `SftEntry.position` must track the live file
//! cursor, because that field is what a snapshot serializes.
//!
//! The FCB paths (AH=14h/21h/22h) already maintain it. The handle paths did
//! not: AH=3Fh read, AH=40h write and AH=42h seek moved only the underlying
//! `VirtualFile` cursor. A checkpoint taken after ordinary file I/O therefore
//! recorded whatever the position was at *open* time — normally zero — so a
//! restore rewound the handle. Later reads repeated data already consumed and
//! later writes landed on the wrong bytes, and a duplicated handle inherited
//! the same stale metadata.
//!
//! These tests assert the field against the cursor the operation actually
//! left behind, so they fail if any of the three stops syncing.

use super::*;
use crate::tests::create_test_io;
use tempfile::TempDir;

const DRIVES_KEY_C: u8 = 2;
const FILE_BODY: &[u8] = b"0123456789ABCDEF";

struct Fixture {
    dos: Dos,
    cpu: gp64_cpu::Cpu,
    memory: Memory,
    vga: crate::Vga,
    keyboard: crate::Keyboard,
    pic: crate::Pic,
    _temp_dir: TempDir,
}

impl Fixture {
    /// A C: drive holding one 16-byte file, already open for read/write.
    fn new() -> (Self, u16) {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("DATA.BIN"), FILE_BODY).unwrap();

        let mut dos = Dos::new();
        dos.drives.insert(
            DRIVES_KEY_C,
            Box::new(crate::filesystem::HostDrive::new(
                temp_dir.path().to_path_buf(),
            )),
        );

        let (vga, keyboard) = create_test_io();
        let mut fixture = Fixture {
            dos,
            cpu: gp64_cpu::Cpu::new(),
            memory: Memory::new(),
            vga,
            keyboard,
            pic: crate::Pic::new(),
            _temp_dir: temp_dir,
        };

        let handle = fixture.open_read_write();
        (fixture, handle)
    }

    fn int21(&mut self) {
        self.dos
            .int21(
                &mut self.cpu,
                &mut self.memory,
                &mut self.vga,
                &mut self.keyboard,
                &mut self.pic,
            )
            .expect("int21 dispatch");
    }

    /// AH=3Dh open `C:\DATA.BIN` for read/write, returning the handle.
    fn open_read_write(&mut self) -> u16 {
        let path_addr: u32 = 0x2000;
        for (i, b) in b"C:\\DATA.BIN\0".iter().enumerate() {
            self.memory.write_u8(path_addr as usize + i, *b);
        }
        self.cpu.regs.set_ax(0x3D02); // AH=3Dh, AL=02 read/write
        self.cpu.regs.ds = 0;
        self.cpu.regs.set_dx(path_addr as u16);
        self.int21();
        assert!(
            !self.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "fixture could not open the test file"
        );
        self.cpu.regs.ax()
    }

    /// The position a snapshot would record for `handle`.
    fn snapshot_position(&self, handle: u16) -> u64 {
        self.dos
            .file_table
            .get_sft(handle)
            .expect("handle must have a live SFT entry")
            .position
    }

    /// Where the file cursor actually is — the value the snapshot must match.
    fn live_cursor(&mut self, handle: u16) -> u64 {
        use std::io::Seek;
        self.dos
            .file_table
            .get_sft_mut(handle)
            .expect("handle must have a live SFT entry")
            .file
            .stream_position()
            .expect("stream position")
    }

    fn assert_position_tracks_cursor(&mut self, handle: u16, what: &str) {
        let cursor = self.live_cursor(handle);
        let recorded = self.snapshot_position(handle);
        assert_eq!(
            recorded, cursor,
            "after {what}: a snapshot would record position {recorded} for a \
             handle whose cursor is at {cursor}, so a restore would rewind it"
        );
    }
}

/// AH=3Fh — an ordinary read.
#[test]
fn read_updates_the_snapshot_visible_position() {
    let (mut fx, handle) = Fixture::new();

    fx.cpu.regs.set_ax(0x3F00);
    fx.cpu.regs.set_bx(handle);
    fx.cpu.regs.set_cx(6);
    fx.cpu.regs.ds = 0;
    fx.cpu.regs.set_dx(0x3000);
    fx.int21();

    assert_eq!(fx.cpu.regs.ax(), 6, "the read should have returned 6 bytes");
    assert_eq!(fx.live_cursor(handle), 6, "the cursor should have advanced");
    fx.assert_position_tracks_cursor(handle, "AH=3Fh read");
}

/// A virtual-file AH=3Dh handle keeps its backend and SFT cursor after AH=3Fh
/// reaches EOF or receives a zero-length request beyond the file's end.
#[test]
fn virtual_file_eof_and_zero_length_reads_preserve_ah42_current_position() {
    use crate::files::SftBacking;
    use std::io::Seek;

    let mut dos = Dos::new();
    dos.register_virtual_file(r"C:\DATA.BIN", vec![1u8, 2, 3]);
    let (mut vga, mut keyboard) = create_test_io();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let mut pic = crate::Pic::new();

    for (index, byte) in b"C:\\DATA.BIN\0".iter().enumerate() {
        memory.write_u8(0x2000 + index, *byte);
    }
    cpu.regs.set_ax(0x3D00); // AH=3Dh, AL=0: read-only
    cpu.regs.ds = 0;
    cpu.regs.set_dx(0x2000);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=3Dh dispatch");
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "AH=3Dh must open the registered virtual file"
    );
    let handle = cpu.regs.ax();
    assert!(matches!(
        dos.file_table.get_sft(handle).unwrap().backing,
        SftBacking::VirtualFile(_)
    ));

    let assert_position = |dos: &mut Dos, expected: u64, stage: &str| {
        let entry = dos.file_table.get_sft_mut(handle).unwrap();
        let cursor = entry.file.stream_position().expect("backend position");
        assert_eq!(entry.position, expected, "SFT position {stage}");
        assert_eq!(cursor, expected, "backend position {stage}");
        assert_eq!(entry.position, cursor, "SFT/backend position {stage}");
    };

    // AH=42h SEEK_SET past the three-byte payload.
    cpu.regs.set_ax(0x4200);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(99);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=42h dispatch");
    assert_eq!(cpu.regs.ax(), 99);
    assert_position(&mut dos, 99, "after AH=42h SEEK_SET");

    // AH=3Fh with a positive count reaches EOF but must not rewind the cursor.
    cpu.regs.set_ax(0x3F00);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(1);
    cpu.regs.ds = 0;
    cpu.regs.set_dx(0x3000);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=3Fh EOF dispatch");
    assert_eq!(cpu.regs.ax(), 0, "AH=3Fh at EOF transfers no bytes");
    assert_position(&mut dos, 99, "after AH=3Fh EOF");

    // AH=42h SEEK_CUR 0 exposes the position through the DOS service result.
    cpu.regs.set_ax(0x4201);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=42h current-position dispatch");
    assert_eq!(((cpu.regs.dx() as u32) << 16) | cpu.regs.ax() as u32, 99);
    assert_position(&mut dos, 99, "after AH=42h SEEK_CUR 0");

    // AH=3Fh with CX=0 is a no-op even when its backend position is past EOF.
    cpu.regs.set_ax(0x3F00);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
    cpu.regs.ds = 0;
    cpu.regs.set_dx(0x3000);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=3Fh zero-length dispatch");
    assert_eq!(cpu.regs.ax(), 0, "AH=3Fh zero-length read returns zero");
    assert_position(&mut dos, 99, "after AH=3Fh zero-length read");

    cpu.regs.set_ax(0x4201);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    dos.int21(&mut cpu, &mut memory, &mut vga, &mut keyboard, &mut pic)
        .expect("AH=42h final current-position dispatch");
    assert_eq!(((cpu.regs.dx() as u32) << 16) | cpu.regs.ax() as u32, 99);
    assert_position(&mut dos, 99, "after final AH=42h SEEK_CUR 0");
}

/// AH=42h — a seek, where the new offset IS the result.
#[test]
fn seek_updates_the_snapshot_visible_position() {
    let (mut fx, handle) = Fixture::new();

    fx.cpu.regs.set_ax(0x4200); // AL=0: from start
    fx.cpu.regs.set_bx(handle);
    fx.cpu.regs.set_cx(0);
    fx.cpu.regs.set_dx(11);
    fx.int21();

    assert_eq!(fx.live_cursor(handle), 11, "the cursor should have moved");
    fx.assert_position_tracks_cursor(handle, "AH=42h seek");
}

/// AH=40h — an ordinary write.
#[test]
fn write_updates_the_snapshot_visible_position() {
    let (mut fx, handle) = Fixture::new();

    for (i, b) in b"XYZ".iter().enumerate() {
        fx.memory.write_u8(0x3000 + i, *b);
    }
    fx.cpu.regs.set_ax(0x4000);
    fx.cpu.regs.set_bx(handle);
    fx.cpu.regs.set_cx(3);
    fx.cpu.regs.ds = 0;
    fx.cpu.regs.set_dx(0x3000);
    fx.int21();

    assert_eq!(fx.cpu.regs.ax(), 3, "the write should have taken 3 bytes");
    assert_eq!(fx.live_cursor(handle), 3, "the cursor should have advanced");
    fx.assert_position_tracks_cursor(handle, "AH=40h write");
}

/// AH=40h with CX=0 — truncate at the cursor, which must still be recorded.
#[test]
fn zero_length_write_records_the_truncation_point() {
    let (mut fx, handle) = Fixture::new();

    // Seek somewhere non-zero first, then truncate there.
    fx.cpu.regs.set_ax(0x4200);
    fx.cpu.regs.set_bx(handle);
    fx.cpu.regs.set_cx(0);
    fx.cpu.regs.set_dx(8);
    fx.int21();

    fx.cpu.regs.set_ax(0x4000);
    fx.cpu.regs.set_bx(handle);
    fx.cpu.regs.set_cx(0);
    fx.int21();

    assert_eq!(
        fx.live_cursor(handle),
        8,
        "truncation leaves the cursor put"
    );
    fx.assert_position_tracks_cursor(handle, "AH=40h zero-length truncate");
}

/// A duplicated handle shares the SFT entry, so it must see the same
/// up-to-date position rather than inheriting stale metadata.
#[test]
fn a_duplicated_handle_sees_the_updated_position() {
    let (mut fx, handle) = Fixture::new();

    fx.cpu.regs.set_ax(0x3F00);
    fx.cpu.regs.set_bx(handle);
    fx.cpu.regs.set_cx(9);
    fx.cpu.regs.ds = 0;
    fx.cpu.regs.set_dx(0x3000);
    fx.int21();
    assert_eq!(fx.cpu.regs.ax(), 9, "the read should have returned 9 bytes");

    // AH=45h DUP
    fx.cpu.regs.set_ax(0x4500);
    fx.cpu.regs.set_bx(handle);
    fx.int21();
    assert!(
        !fx.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "DUP should succeed"
    );
    let dup = fx.cpu.regs.ax();

    assert_eq!(
        fx.snapshot_position(dup),
        9,
        "the duplicate must see the position the read left, not a stale one"
    );
    fx.assert_position_tracks_cursor(dup, "AH=45h duplicate after a read");
}
