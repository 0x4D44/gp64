use super::*;

static FIXTURE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Fixture(std::path::PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(".scratch");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(format!(
            "console-alias-{}-{}-{}",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("DATA.BIN"), b"original").unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for name in ["DATA.BIN", "NEW.BIN"] {
            let path = self.0.join(name);
            if path.exists() {
                std::fs::remove_file(path).unwrap();
            }
        }
        std::fs::remove_dir(&self.0).unwrap();
    }
}

struct Guest {
    dos: Dos,
    cpu: Cpu,
    memory: Memory,
    vga: Vga,
    keyboard: Keyboard,
}

impl Guest {
    fn new(fixture: &Fixture) -> Self {
        let mut dos = Dos::new();
        dos.drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(fixture.0.clone())),
        );
        let (vga, keyboard) = create_test_io();
        let mut cpu = Cpu::new();
        let memory = Memory::new();
        cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, 0x2000);
        Self {
            dos,
            cpu,
            memory,
            vga,
            keyboard,
        }
    }

    fn call(&mut self, ax: u16, bx: u16, cx: u16, dx: u16) -> u16 {
        self.cpu.regs.set_ax(ax);
        self.cpu.regs.set_bx(bx);
        self.cpu.regs.set_cx(cx);
        self.cpu.regs.set_dx(dx);
        self.dos
            .int21(
                &mut self.cpu,
                &mut self.memory,
                &mut self.vga,
                &mut self.keyboard,
                &mut Pic::new(),
            )
            .unwrap();
        self.cpu.regs.ax()
    }

    fn ok(&mut self, ax: u16, bx: u16, cx: u16, dx: u16) -> u16 {
        let result = self.call(ax, bx, cx, dx);
        assert!(
            !self.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "AX={result:04x}"
        );
        result
    }

    fn path(&mut self, name: &str) {
        write_asciiz(&mut self.memory, 0x2000, 0x100, name);
    }

    fn assert_write_read(&mut self, handle: u16) {
        let addr = Memory::seg_offset_to_linear(0x2000, 0x200);
        for (i, byte) in b"updated!".iter().enumerate() {
            self.memory.write_u8(addr + i, *byte);
        }
        assert_eq!(self.ok(0x4000, handle, 8, 0x200), 8);
        self.assert_read(handle);
    }

    fn assert_read(&mut self, handle: u16) {
        let addr = Memory::seg_offset_to_linear(0x2000, 0x200);
        assert_eq!(self.ok(0x4200, handle, 0, 0), 0);
        for i in 0..8 {
            self.memory.write_u8(addr + i, 0);
        }
        assert_eq!(self.ok(0x3f00, handle, 8, 0x200), 8);
        let actual: Vec<_> = (0..8).map(|i| self.memory.read_u8(addr + i)).collect();
        assert_eq!(actual, b"updated!");
    }
}

#[test]
fn alias_occupancy_create_skips_saved_console() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    assert_eq!(guest.ok(0x4500, 1, 0, 0), 5);
    guest.path("NEW.BIN");
    let file = guest.ok(0x3c00, 0, 0, 0x100);
    assert_eq!(file, 6);
    guest.assert_write_read(file);
    guest.ok(0x3e00, file, 0, 0);
    assert_eq!(
        std::fs::read(fixture.0.join("NEW.BIN")).unwrap(),
        b"updated!"
    );
    assert_eq!(guest.dos.resolve_alias_handle(5), 0xfe);
    guest.ok(0x3e00, 5, 0, 0);
}

#[test]
fn alias_occupancy_open_skips_saved_console() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    assert_eq!(guest.ok(0x4500, 1, 0, 0), 5);
    guest.path("DATA.BIN");
    let file = guest.ok(0x3d02, 0, 0, 0x100);
    assert_eq!(file, 6);
    guest.assert_write_read(file);
    guest.ok(0x3e00, file, 0, 0);
    assert_eq!(
        std::fs::read(fixture.0.join("DATA.BIN")).unwrap(),
        b"updated!"
    );
    guest.ok(0x3e00, 5, 0, 0);
}

#[test]
fn alias_occupancy_file_dup_skips_saved_console() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    guest.path("DATA.BIN");
    assert_eq!(guest.ok(0x3d02, 0, 0, 0x100), 5);
    assert_eq!(guest.ok(0x4500, 1, 0, 0), 6);
    assert_eq!(guest.ok(0x4500, 5, 0, 0), 7);
    guest.assert_write_read(7);
    assert_eq!(guest.dos.file_table.get_sft(5).unwrap().position, 8);
    for handle in [5, 6, 7] {
        guest.ok(0x3e00, handle, 0, 0);
    }
}

#[test]
fn alias_occupancy_force_duplicate_replaces_standard_handles_without_leaks() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);

    for index in 0..300 {
        guest.path("DATA.BIN");
        let source = guest.ok(0x3d02, 0, 0, 0x100);
        assert_eq!(source, 5, "the closed source slot must remain reusable");

        let destination = (index % 5) as u16;
        guest.ok(0x4600, source, destination, 0);
        assert_eq!(
            guest.dos.file_table.get_sft(source).unwrap().ref_count,
            2,
            "AH=46h must make the standard handle share the source SFT"
        );
        assert_eq!(
            guest.dos.file_table.get_sft(destination).unwrap().ref_count,
            2
        );

        guest.ok(0x3e00, source, 0, 0);
        assert!(guest.dos.file_table.get_sft(source).is_none());
        assert_eq!(
            guest.dos.file_table.get_sft(destination).unwrap().ref_count,
            1,
            "closing the source must leave one reachable standard-handle reference"
        );
    }
}

fn full_console_table_preflight(name: &str) {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    for expected in 5..20 {
        assert_eq!(guest.ok(0x4500, 1, 0, 0), expected);
    }
    guest.path(name);
    assert_eq!(guest.call(0x3c00, 0, 0, 0x100), 4);
    assert!(guest.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(!fixture.0.join("NEW.BIN").exists());
    assert_eq!(
        std::fs::read(fixture.0.join("DATA.BIN")).unwrap(),
        b"original"
    );
    for handle in 5..20 {
        guest.ok(0x3e00, handle, 0, 0);
    }
}

#[test]
fn alias_occupancy_full_console_table_preflights_create() {
    full_console_table_preflight("NEW.BIN");
}

#[test]
fn alias_occupancy_full_console_table_preflights_truncate() {
    full_console_table_preflight("DATA.BIN");
}

#[test]
fn alias_occupancy_mixed_table_rejects_file_dup_at_limit() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    guest.path("DATA.BIN");
    assert_eq!(guest.ok(0x3d02, 0, 0, 0x100), 5);
    for expected in 6..20 {
        assert_eq!(guest.ok(0x4500, 1, 0, 0), expected);
    }
    assert_eq!(guest.call(0x4500, 5, 0, 0), 4);
    assert!(guest.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(guest.dos.file_table.get_sft(5).unwrap().ref_count, 1);
    for handle in 5..20 {
        guest.ok(0x3e00, handle, 0, 0);
    }
}

#[test]
fn alias_occupancy_dup2_restore_console_and_release_saved_slot() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    assert_eq!(guest.ok(0x4500, 1, 0, 0), 5);
    // Use an explicit distinct slot so the restoration guard also runs on the old allocator.
    assert!(guest.dos.file_table.allocate_at_handle(
        6,
        Box::new(CursorFile::new(Vec::new())),
        "C:\\REDIRECT.BIN".into(),
        guest.dos.current_psp
    ));
    guest.ok(0x4600, 6, 1, 0);
    guest.ok(0x4600, 5, 1, 0);
    assert!(!guest.dos.file_table.is_valid(1));
    assert_eq!(guest.dos.resolve_alias_handle(1), 0xfe);
    guest.ok(0x3e00, 5, 0, 0);
    guest.path("DATA.BIN");
    assert_eq!(guest.ok(0x3d02, 0, 0, 0x100), 5);
    guest.assert_write_read(5);
    for handle in [5, 6] {
        guest.ok(0x3e00, handle, 0, 0);
    }
}

#[test]
fn alias_occupancy_bridge_reports_reserved_slot() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    assert_eq!(guest.ok(0x4500, 1, 0, 0), 5);
    assert!(!guest.dos.file_table.is_handle_free(5));
    guest.ok(0x3e00, 5, 0, 0);
    assert!(guest.dos.file_table.is_handle_free(5));
}

#[test]
fn alias_occupancy_bridge_rejects_reserved_slot() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    assert_eq!(guest.ok(0x4500, 1, 0, 0), 5);
    assert!(!guest.dos.file_table.allocate_at_handle(
        5,
        Box::new(CursorFile::new(Vec::new())),
        "C:\\BRIDGE.BIN".into(),
        guest.dos.current_psp
    ));
    assert_eq!(guest.dos.resolve_alias_handle(5), 0xfe);
    guest.ok(0x3e00, 5, 0, 0);
}

#[test]
fn alias_occupancy_explicit_file_replacement_clears_alias() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    assert_eq!(guest.ok(0x4500, 1, 0, 0), 5);
    assert!(guest.dos.file_table.insert_at(
        5,
        Box::new(CursorFile::new(b"updated!".to_vec())),
        "C:\\REPLACE.BIN".into(),
        guest.dos.current_psp
    ));
    assert_eq!(guest.dos.resolve_alias_handle(5), 5);
    // insert_at keeps its existing read-only default access contract.
    assert_eq!(guest.call(0x4000, 5, 1, 0x200), 5);
    assert!(guest.cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    guest.assert_read(5);
    guest.ok(0x3e00, 5, 0, 0);
}

#[test]
fn alias_occupancy_explicit_file_dup_replacement_clears_alias() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    assert_eq!(guest.ok(0x4500, 1, 0, 0), 5);
    assert!(!guest
        .dos
        .file_table
        .force_duplicate(99, 5, guest.dos.current_psp));
    assert_eq!(guest.dos.resolve_alias_handle(5), 0xfe);
    assert!(guest.dos.file_table.allocate_at_handle(
        6,
        Box::new(CursorFile::new(Vec::new())),
        "C:\\SOURCE.BIN".into(),
        guest.dos.current_psp
    ));
    assert!(guest
        .dos
        .file_table
        .force_duplicate(6, 5, guest.dos.current_psp));
    assert_eq!(guest.dos.resolve_alias_handle(5), 5);
    guest.assert_write_read(5);
    for handle in [5, 6] {
        guest.ok(0x3e00, handle, 0, 0);
    }
}

#[test]
fn alias_occupancy_snapshot_restores_aliases_and_limit() {
    let fixture = Fixture::new();
    let mut guest = Guest::new(&fixture);
    guest.ok(0x6700, 32, 0, 0);
    assert_eq!(guest.ok(0x4500, 1, 0, 0), 5);
    let snapshot = guest.dos.snapshot();
    guest.ok(0x3e00, 5, 0, 0);
    guest.ok(0x6700, 40, 0, 0);
    guest.dos.restore(&snapshot);
    assert_eq!(guest.dos.file_table.handle_limit(), 32);
    assert_eq!(guest.dos.resolve_alias_handle(5), 0xfe);
    assert_eq!(guest.ok(0x4500, 1, 0, 0), 6);
    for handle in [5, 6] {
        guest.ok(0x3e00, handle, 0, 0);
    }
}
