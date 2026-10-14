use super::*;

#[test]
fn test_coverage_exercise_virtual_file_test_doubles() {
    let mut failing_read = FailingReadFile;
    exercise_virtual_file(&mut failing_read);

    let mut cursor_file = CursorFile::new(vec![1, 2, 3, 4]);
    exercise_virtual_file(&mut cursor_file);

    let mut seek_error = SeekErrorFile;
    exercise_virtual_file(&mut seek_error);

    let mut set_len_error = SetLenErrorFile::new();
    exercise_virtual_file(&mut set_len_error);

    let mut partial_write = PartialWriteFile::new(2);
    exercise_virtual_file(&mut partial_write);

    let mut error_write = ErrorWriteFile;
    exercise_virtual_file(&mut error_write);
}

#[test]
fn test_coverage_exercise_virtual_drive_test_doubles() {
    let mut disk_space = DiskSpaceDrive::new(1_000_000, 500_000);
    exercise_virtual_drive(&mut disk_space);

    let mut error_disk_space = ErrorDiskSpaceDrive;
    exercise_virtual_drive(&mut error_disk_space);

    let mut deny_write = DenyWriteDrive;
    exercise_virtual_drive(&mut deny_write);

    let mut read_only = ReadOnlyDrive;
    exercise_virtual_drive(&mut read_only);

    let mut not_found = NotFoundDrive;
    exercise_virtual_drive(&mut not_found);

    let mut no_such_file = NoSuchFileDrive;
    exercise_virtual_drive(&mut no_such_file);

    let mut all_attrs = AllAttributesDrive;
    exercise_virtual_drive(&mut all_attrs);

    let mut failing_read_drive = FailingReadDrive;
    exercise_virtual_drive(&mut failing_read_drive);

    let mut listing_ok = ListingDrive::new(vec![(
        "FILE.TXT".to_string(),
        crate::filesystem::FileAttributes::default(),
    )]);
    exercise_virtual_drive(&mut listing_ok);

    let mut listing_fail_remove = ListingDrive::new(vec![]).with_remove_ok(false);
    exercise_virtual_drive(&mut listing_fail_remove);

    let mut listing_fail_rename = ListingDrive::new(vec![]).with_rename_ok(false);
    exercise_virtual_drive(&mut listing_fail_rename);

    let mut attr_error = AttrErrorDrive;
    exercise_virtual_drive(&mut attr_error);
}

#[test]
fn test_dos_new() {
    let dos = Dos::new();
    assert_eq!(dos.path_resolver.get_current_drive(), 2); // C:
    assert_eq!(dos.version_major, 6);
    assert_eq!(dos.version_minor, 22);
    assert_eq!(dos.dta_segment, 0x0100);
    assert_eq!(dos.dta_offset, 0x0080);
    assert_eq!(dos.current_psp, 0x0100);
}

#[test]
fn test_dos_default() {
    let dos = Dos::default();
    assert_eq!(dos.path_resolver.get_current_drive(), 2);
}

#[test]
fn test_env_segment() {
    let mut dos = Dos::new();
    dos.env_segment = 0x1234;
    assert_eq!(dos.env_segment(), 0x1234);
}

#[test]
fn test_current_psp() {
    let dos = Dos::new();
    assert_eq!(dos.current_psp(), 0x0100);
}

#[test]
fn test_parse_trace_ah_empty_returns_none() {
    let _lock = env_lock();

    assert!(parse_trace_ah().is_none());
}

#[test]
fn test_set_current_psp() {
    let mut dos = Dos::new();
    dos.set_current_psp(0x2000);
    assert_eq!(dos.current_psp(), 0x2000);
    // DTA should be reset to PSP:0080
    assert_eq!(dos.dta_segment, 0x2000);
    assert_eq!(dos.dta_offset, 0x0080);
}

#[test]
fn test_set_env_program_name_returns_when_no_space() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    dos.init_environment(&mut memory, &SoundEnvConfig::default());

    let env_seg = dos.env_segment;
    let max_bytes = dos
        .dos_mem
        .block_end_segment(env_seg)
        .map(|end_seg| end_seg.saturating_sub(env_seg) as usize * 16)
        .unwrap_or(0);
    assert!(max_bytes >= 3);

    let env_base = Memory::seg_offset_to_linear(env_seg, 0);
    for i in 0..max_bytes {
        memory.write_u8(env_base + i, b'A');
    }
    memory.write_u8(env_base + max_bytes - 2, 0);
    memory.write_u8(env_base + max_bytes - 1, 0);

    dos.set_env_program_name(&mut memory, "PROG.EXE");

    assert_eq!(memory.read_u8(env_base + max_bytes - 3), b'A');
    assert_eq!(memory.read_u8(env_base + max_bytes - 2), 0);
    assert_eq!(memory.read_u8(env_base + max_bytes - 1), 0);
}

#[test]
fn test_set_env_program_name_no_double_nul_stops_at_limit() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    dos.init_environment(&mut memory, &SoundEnvConfig::default());

    let env_seg = dos.env_segment;
    let max_bytes = dos
        .dos_mem
        .block_end_segment(env_seg)
        .map(|end_seg| end_seg.saturating_sub(env_seg) as usize * 16)
        .unwrap_or(0);
    assert!(max_bytes >= 4);

    let env_base = Memory::seg_offset_to_linear(env_seg, 0);
    for i in 0..max_bytes {
        memory.write_u8(env_base + i, b'A');
    }

    dos.set_env_program_name(&mut memory, "PROG.EXE");

    assert_eq!(memory.read_u8(env_base + max_bytes - 1), b'A');
    assert_eq!(memory.read_u8(env_base + max_bytes - 2), b'A');
}

#[test]
fn test_set_env_program_name_truncates_to_max_bytes() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    dos.init_environment(&mut memory, &SoundEnvConfig::default());

    let env_seg = dos.env_segment;
    let max_bytes = dos
        .dos_mem
        .block_end_segment(env_seg)
        .map(|end_seg| end_seg.saturating_sub(env_seg) as usize * 16)
        .unwrap_or(0);
    assert!(max_bytes >= 8);

    let env_base = Memory::seg_offset_to_linear(env_seg, 0);
    for i in 0..max_bytes {
        memory.write_u8(env_base + i, b'A');
    }
    let double_nul = max_bytes - 6;
    memory.write_u8(env_base + double_nul, 0);
    memory.write_u8(env_base + double_nul + 1, 0);

    dos.set_env_program_name(&mut memory, "PROGRAM.EXE");

    assert_eq!(memory.read_u16(env_base + double_nul + 2), 1);
    assert_eq!(memory.read_u8(env_base + max_bytes - 2), b'P');
    assert_eq!(memory.read_u8(env_base + max_bytes - 1), 0);
}

#[test]
fn test_create_psp() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    dos.create_psp(&mut memory, 0x1000, 0x9000, 0x0500, None, &[]);

    let base = Memory::seg_offset_to_linear(0x1000, 0);

    // Check INT 20h at start
    assert_eq!(memory.read_u8(base), 0xCD);
    assert_eq!(memory.read_u8(base + 1), 0x20);

    // Check memory end segment
    assert_eq!(memory.read_u16(base + 2), 0x9000);

    // Check environment segment
    assert_eq!(memory.read_u16(base + 0x2C), 0x0500);

    // Check command tail (empty)
    assert_eq!(memory.read_u8(base + 0x80), 0); // length
    assert_eq!(memory.read_u8(base + 0x81), 0x0D); // CR terminator
}

/// Part 1 (S3) — the critical regression guard. Pre-fills the PSP region
/// with 0xCF (BSS-init pattern, commit 14833c91b) then calls `create_psp`
/// and asserts every reserved-byte region documented in RBIL is now zero.
///
/// If a future engineer reorders `create_psp` so the zero-fill happens
/// AFTER field writes (or removes it entirely), the reserved-region
/// assertions below fail.
#[test]
fn test_create_psp_overwrites_bss_init_fill() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let segment: u16 = 0x1000;

    // Pre-fill the PSP region with 0xCF (the BSS-init policy pattern).
    let base = Memory::seg_offset_to_linear(segment, 0);
    for off in 0..0x100usize {
        memory.write_u8(base + off, 0xCF);
    }

    dos.create_psp(&mut memory, segment, 0x9000, 0x0500, None, &[]);

    // PSP:0x04 — reserved byte (1 byte).
    assert_eq!(
        memory.read_u8(base + 0x04),
        0,
        "PSP:0x04 must be zero (not 0xCF BSS leak)"
    );

    // PSP:0x2E..0x31 — SS:SP at last INT 21h (undocumented; we leave
    // zero per HLD D3). 4 bytes.
    for off in 0x2Eusize..=0x31 {
        assert_eq!(
            memory.read_u8(base + off),
            0,
            "PSP:0x{off:02X} must be zero (not 0xCF BSS leak)",
        );
    }

    // PSP:0x38..0x3F — previous PSP (DOS 4.0+) + reserved. 8 bytes.
    for off in 0x38usize..=0x3F {
        assert_eq!(
            memory.read_u8(base + off),
            0,
            "PSP:0x{off:02X} must be zero (not 0xCF BSS leak)",
        );
    }

    // PSP:0x40..0x4F — DOS version (PSP:0x40-0x41) + reserved (14 bytes).
    for off in 0x40usize..=0x4F {
        assert_eq!(
            memory.read_u8(base + off),
            0,
            "PSP:0x{off:02X} must be zero (not 0xCF BSS leak)",
        );
    }

    // PSP:0x53..0x5B — reserved 9 bytes.
    for off in 0x53usize..=0x5B {
        assert_eq!(
            memory.read_u8(base + off),
            0,
            "PSP:0x{off:02X} must be zero (not 0xCF BSS leak)",
        );
    }

    // PSP:0x5C..0x6B — default FCB 1 (16 bytes).
    for off in 0x5Cusize..=0x6B {
        assert_eq!(
            memory.read_u8(base + off),
            0,
            "PSP:0x{off:02X} (FCB1) must be zero (not 0xCF BSS leak)",
        );
    }

    // PSP:0x6C..0x7B — default FCB 2 (16 bytes).
    for off in 0x6Cusize..=0x7B {
        assert_eq!(
            memory.read_u8(base + off),
            0,
            "PSP:0x{off:02X} (FCB2) must be zero (not 0xCF BSS leak)",
        );
    }

    // PSP:0x7C..0x7F — unused 4 bytes.
    for off in 0x7Cusize..=0x7F {
        assert_eq!(
            memory.read_u8(base + off),
            0,
            "PSP:0x{off:02X} must be zero (not 0xCF BSS leak)",
        );
    }

    // Cmdtail length and CR (empty cmdtail).
    assert_eq!(memory.read_u8(base + 0x80), 0, "cmdtail length");
    assert_eq!(memory.read_u8(base + 0x81), 0x0D, "cmdtail CR terminator");

    // PSP:0x82..0xFF — cmdtail/DTA region. Must also be zero (covered
    // by the 256-byte zero-fill; no field writes touch this range when
    // args is empty).
    for off in 0x82usize..0x100 {
        assert_eq!(
            memory.read_u8(base + off),
            0,
            "PSP:0x{off:02X} (cmdtail/DTA region) must be zero (not 0xCF BSS leak)",
        );
    }
}

/// HLD Part 1 — pins that the zero-fill does NOT shadow field writes.
/// After `create_psp`, the explicit-field bytes (INT 20h stub, FAR CALL
/// trampoline, JFT, env-seg, JFT size/ptr, INT 21h trampoline) must
/// still have their RBIL-defined values, not zero.
#[test]
fn test_create_psp_preserves_field_writes_after_zero_fill() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let segment: u16 = 0x1000;
    let mem_end: u16 = 0x9000;
    let env_seg: u16 = 0x0500;

    // Pre-fill with a marker byte to make field-write detection
    // unambiguous (separately from the BSS-init test which uses 0xCF).
    let base = Memory::seg_offset_to_linear(segment, 0);
    for off in 0..0x100usize {
        memory.write_u8(base + off, 0xAA);
    }

    dos.create_psp(&mut memory, segment, mem_end, env_seg, None, &[]);

    // INT 20h stub at PSP:0x00-0x01.
    assert_eq!(memory.read_u8(base), 0xCD);
    assert_eq!(memory.read_u8(base + 1), 0x20);

    // Memory-end segment at PSP:0x02-0x03.
    assert_eq!(memory.read_u16(base + 2), mem_end);

    // CALL 5 entry at PSP:0x05-0x09. PSP:0006 carries the CP/M
    // bytes-available-in-segment word, not a 0x0050 offset
    // (MDD-BUG-KILN-00645).
    assert_eq!(memory.read_u8(base + 0x05), 0x9A);
    assert_eq!(
        memory.read_u16(base + 0x06),
        (u32::from(mem_end - segment) * 16).min(0xFFFF) as u16
    );
    assert_eq!(memory.read_u16(base + 0x08), 0x000C);

    // Default JFT at PSP:0x18 (root process: standard handles 00-04, then 0xFF).
    assert_eq!(memory.read_u8(base + 0x18), 0x00); // stdin
    assert_eq!(memory.read_u8(base + 0x19), 0x01); // stdout
    assert_eq!(memory.read_u8(base + 0x1A), 0x02); // stderr
    assert_eq!(memory.read_u8(base + 0x1B), 0x03); // stdaux
    assert_eq!(memory.read_u8(base + 0x1C), 0x04); // stdprn
    for i in 5..20 {
        assert_eq!(memory.read_u8(base + 0x18 + i), 0xFF);
    }

    // Env segment at PSP:0x2C-0x2D.
    assert_eq!(memory.read_u16(base + 0x2C), env_seg);

    // JFT size at PSP:0x32-0x33 (20 = 0x14).
    assert_eq!(memory.read_u16(base + 0x32), 20);

    // JFT pointer at PSP:0x34-0x37 (0x0018:segment).
    assert_eq!(memory.read_u16(base + 0x34), 0x0018);
    assert_eq!(memory.read_u16(base + 0x36), segment);

    // INT 21h/RETF trampoline at PSP:0x50-0x52.
    assert_eq!(memory.read_u8(base + 0x50), 0xCD);
    assert_eq!(memory.read_u8(base + 0x51), 0x21);
    assert_eq!(memory.read_u8(base + 0x52), 0xCB);

    // Cmdtail at PSP:0x80-0x81 (empty cmdtail).
    assert_eq!(memory.read_u8(base + 0x80), 0);
    assert_eq!(memory.read_u8(base + 0x81), 0x0D);
}

/// MDD-BUG-KILN-00645: PSP:0006 must carry the CP/M-heritage "bytes
/// available in the first segment" word, `min(0xFFFF, (top - psp) * 16)`.
///
/// It used to be a hardcoded `0x0050` — the offset half of a
/// `call far <this psp>:0050`. Any CP/M-heritage program sizing its own
/// segment therefore read **80 bytes** and concluded the machine was full;
/// The field must describe the segment capacity independently of CALL-5.
#[test]
fn psp_reports_bytes_available_in_segment_not_the_call5_offset() {
    let cases = [
        // Independently chosen span of twice the 64 KiB capacity: saturate.
        (0x1200u16, 0x3200u16, 0xFFFFu16),
        // A span that does NOT saturate, so the arithmetic itself is pinned
        // rather than only the clamp: 0x0100 paragraphs * 16 = 4096 bytes.
        (0x1000, 0x1100, 0x1000),
        // Exactly at the clamp boundary: 0x1000 paragraphs * 16 = 65536,
        // one past what the field can hold.
        (0x2000, 0x3000, 0xFFFF),
        // Degenerate: no room above the PSP at all.
        (0x4000, 0x4000, 0x0000),
    ];

    for (segment, mem_end, expected) in cases {
        let mut dos = Dos::new();
        let mut memory = Memory::new();
        let base = Memory::seg_offset_to_linear(segment, 0);
        dos.create_psp(&mut memory, segment, mem_end, 0x0500, None, &[]);

        assert_eq!(
            memory.read_u16(base + 0x06),
            expected,
            "PSP {segment:#06X}..{mem_end:#06X}: PSP:0006 must be the \
             bytes-available word, not a CALL-5 offset"
        );
        assert_ne!(
            memory.read_u16(base + 0x06),
            0x0050,
            "PSP {segment:#06X}: the hardcoded 0x0050 is the defect itself"
        );

        // The opcode byte and the dispatch segment are the rest of the
        // MS-DOS layout; PSP:0006 overlaps the far call's offset operand
        // deliberately, and on real DOS the size field wins.
        assert_eq!(memory.read_u8(base + 0x05), 0x9A, "CALL FAR opcode");
        assert_eq!(
            memory.read_u16(base + 0x08),
            0x000C,
            "CP/M dispatch segment"
        );
    }
}

/// HLD Part 0 — pins that `dump_psp_bytes` returns the 256 bytes at
/// `psp_seg:0`.
#[test]
fn test_dump_psp_bytes_returns_256_bytes() {
    let mut memory = Memory::new();
    let psp_seg: u16 = 0x2000;
    let base = Memory::seg_offset_to_linear(psp_seg, 0);

    // Fill the PSP region with a predictable pattern.
    for off in 0..0x100usize {
        memory.write_u8(base + off, (off & 0xFF) as u8);
    }

    let bytes = dump_psp_bytes(&memory, psp_seg);
    assert_eq!(bytes.len(), 0x100, "dump must return 256 bytes");
    for (i, &b) in bytes.iter().enumerate() {
        assert_eq!(b, (i & 0xFF) as u8, "byte {i} content mismatch");
    }
}

/// HLD Part 0 — pins `dump_env_strings` walks ASCIIZ entries up to the
/// double-NUL boundary.
#[test]
fn test_dump_env_strings_walks_until_double_nul() {
    let mut memory = Memory::new();
    let env_seg: u16 = 0x3000;
    let base = Memory::seg_offset_to_linear(env_seg, 0);

    // Write: "PATH=C:\\DOS\0" + "PROMPT=$P$G\0" + "\0" (double-NUL boundary).
    let strings: &[&[u8]] = &[b"PATH=C:\\DOS", b"PROMPT=$P$G"];
    let mut off = 0;
    for s in strings {
        for &b in *s {
            memory.write_u8(base + off, b);
            off += 1;
        }
        memory.write_u8(base + off, 0);
        off += 1;
    }
    memory.write_u8(base + off, 0); // Double-NUL terminator.

    let result = dump_env_strings(&memory, env_seg);
    assert_eq!(result.len(), 2, "two env entries");
    assert_eq!(result[0], "PATH=C:\\DOS");
    assert_eq!(result[1], "PROMPT=$P$G");
}

/// HLD Part 0 — pins `dump_env_strings` returns empty for `env_seg=0`.
#[test]
fn test_dump_env_strings_zero_seg_returns_empty() {
    let memory = Memory::new();
    let result = dump_env_strings(&memory, 0);
    assert!(result.is_empty());
}

/// HLD Part 0 — pins `dump_env_progname` walks past double-NUL, WORD
/// count, then reads ASCIIZ progname.
#[test]
fn test_dump_env_progname_walks_double_nul_then_word_then_asciiz() {
    let mut memory = Memory::new();
    let env_seg: u16 = 0x3000;
    let base = Memory::seg_offset_to_linear(env_seg, 0);

    // Layout: "PATH=A\0" + "\0" (double-NUL) + word(1) + "synthetic-program\0".
    let mut off = 0;
    for &b in b"PATH=A" {
        memory.write_u8(base + off, b);
        off += 1;
    }
    memory.write_u8(base + off, 0); // String 1 terminator.
    off += 1;
    memory.write_u8(base + off, 0); // Double-NUL.
    off += 1;
    memory.write_u16(base + off, 1); // WORD count = 1.
    off += 2;
    for &b in b"FOO.EXE" {
        memory.write_u8(base + off, b);
        off += 1;
    }
    memory.write_u8(base + off, 0); // Progname terminator.

    let result = dump_env_progname(&memory, env_seg);
    assert_eq!(result, Some("FOO.EXE".to_string()));
}

/// HLD Part 0 — pins `dump_env_progname` returns None when the double-NUL
/// boundary isn't found within the scan cap (malformed env block).
/// Synthesises a malformed env block by filling the full 32 KiB scan
/// window with non-zero bytes so the scan exhausts without finding a
/// `\0\0` boundary.
#[test]
fn test_dump_env_progname_no_double_nul_returns_none() {
    let mut memory = Memory::new();
    let env_seg: u16 = 0x3000;
    let base = Memory::seg_offset_to_linear(env_seg, 0);

    // Fill the entire 32 KiB scan window with non-zero bytes — no NUL
    // anywhere in scan range, simulating a corrupt env block. The
    // `dump_env_progname` impl caps its scan at 32 KiB.
    for off in 0..32 * 1024usize {
        memory.write_u8(base + off, 0xCF);
    }

    let result = dump_env_progname(&memory, env_seg);
    assert_eq!(result, None);
}

/// HLD Part 0 — pins `dump_env_progname` returns None for `env_seg=0`.
#[test]
fn test_dump_env_progname_zero_seg_returns_none() {
    let memory = Memory::new();
    let result = dump_env_progname(&memory, 0);
    assert_eq!(result, None);
}

#[test]
fn test_create_psp_with_args() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    dos.create_psp(
        &mut memory,
        0x1000,
        0x9000,
        0x0500,
        None,
        &["arg1".to_string(), "arg2".to_string()],
    );

    let base = Memory::seg_offset_to_linear(0x1000, 0);

    // Command tail should be " arg1 arg2"
    let len = memory.read_u8(base + 0x80);
    assert_eq!(len, 10); // " arg1 arg2" = 10 chars

    // Check content
    assert_eq!(memory.read_u8(base + 0x81), b' ');
    assert_eq!(memory.read_u8(base + 0x82), b'a');
    assert_eq!(memory.read_u8(base + 0x83), b'r');
    assert_eq!(memory.read_u8(base + 0x84), b'g');
    assert_eq!(memory.read_u8(base + 0x85), b'1');
}

#[test]
fn test_create_psp_file_handles() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    dos.create_psp(&mut memory, 0x1000, 0x9000, 0x0500, None, &[]);

    let base = Memory::seg_offset_to_linear(0x1000, 0);

    // Check standard file handles (correct SFT mapping)
    assert_eq!(memory.read_u8(base + 0x18), 0x00); // stdin  -> SFT 0
    assert_eq!(memory.read_u8(base + 0x19), 0x01); // stdout -> SFT 1
    assert_eq!(memory.read_u8(base + 0x1A), 0x02); // stderr -> SFT 2
    assert_eq!(memory.read_u8(base + 0x1B), 0x03); // stdaux -> SFT 3
    assert_eq!(memory.read_u8(base + 0x1C), 0x04); // stdprn -> SFT 4

    // Rest should be 0xFF (closed)
    for i in 5..20 {
        assert_eq!(memory.read_u8(base + 0x18 + i), 0xFF);
    }
}

#[test]
fn test_create_psp_jft_inheritance_from_parent() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    // Create parent PSP with default JFT (root process, no parent)
    let parent_seg = 0x1000;
    dos.create_psp(&mut memory, parent_seg, 0x9000, 0x0500, None, &[]);

    let parent_base = Memory::seg_offset_to_linear(parent_seg, 0);

    // Open a real file whose SFT index the parent's JFT names
    use crate::filesystem::VirtualFile;
    use std::io::{Cursor, Read, Seek, SeekFrom, Write};

    #[derive(Default)]
    struct MemFile {
        cursor: Cursor<Vec<u8>>,
    }
    impl Read for MemFile {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.cursor.read(buf)
        }
    }
    impl Write for MemFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.cursor.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.cursor.flush()
        }
    }
    impl Seek for MemFile {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.cursor.seek(pos)
        }
    }
    impl VirtualFile for MemFile {
        fn set_len(&mut self, size: u64) -> anyhow::Result<()> {
            self.cursor.get_mut().resize(size as usize, 0);
            Ok(())
        }
        fn sync_all(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    // Allocate a file in the file table (gets handle 5, SFT index 0)
    let handle = dos
        .file_table
        .allocate(
            Box::new(MemFile::default()),
            String::from(r"C:\TEST.DAT"),
            parent_seg,
        )
        .unwrap();
    let sft_idx = dos.file_table.get_sft_index(handle).unwrap();
    assert_eq!(dos.file_table.get_sft(handle).unwrap().ref_count, 1);

    // Clear the parent's standard handle JFT entries that collide with our SFT index.
    // In real DOS, SFT indices 0-4 would be console/AUX/PRN device entries that
    // always exist. In our test, those SFT slots are empty, so only our file at
    // sft_idx matters. Write the file's SFT index into parent JFT slot 5.
    memory.write_u8(parent_base + 0x18 + 5, sft_idx as u8);

    // Also set JFT slot 7 to an arbitrary non-0xFF value to test copy
    memory.write_u8(parent_base + 0x18 + 7, 0x42);

    // Create child PSP inheriting from parent
    let child_seg = 0x2000;
    dos.create_psp(
        &mut memory,
        child_seg,
        0x9000,
        0x0500,
        Some(parent_seg),
        &[],
    );

    let child_base = Memory::seg_offset_to_linear(child_seg, 0);

    // Child's JFT should be a copy of parent's JFT
    for i in 0..20 {
        let parent_val = memory.read_u8(parent_base + 0x18 + i);
        let child_val = memory.read_u8(child_base + 0x18 + i);
        assert_eq!(
            parent_val, child_val,
            "JFT slot {} mismatch: parent={:02X} child={:02X}",
            i, parent_val, child_val
        );
    }

    // Parent and child share the one handle table, so inheriting the JFT
    // bytes takes no SFT reference.
    assert_eq!(dos.file_table.get_sft(handle).unwrap().ref_count, 1);
}

#[test]
fn test_create_psp_root_process_gets_default_jft() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    // Root process: no parent
    dos.create_psp(&mut memory, 0x1000, 0x9000, 0x0500, None, &[]);

    let base = Memory::seg_offset_to_linear(0x1000, 0);

    // Should get default JFT: stdin=0, stdout=1, stderr=2, stdaux=3, stdprn=4
    assert_eq!(memory.read_u8(base + 0x18), 0x00);
    assert_eq!(memory.read_u8(base + 0x19), 0x01);
    assert_eq!(memory.read_u8(base + 0x1A), 0x02);
    assert_eq!(memory.read_u8(base + 0x1B), 0x03);
    assert_eq!(memory.read_u8(base + 0x1C), 0x04);
    for i in 5..20 {
        assert_eq!(memory.read_u8(base + 0x18 + i), 0xFF);
    }
}

#[test]
fn test_create_psp_inherited_handle_close_releases_the_file() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    use crate::filesystem::VirtualFile;
    use std::io::{Cursor, Read, Seek, SeekFrom, Write};

    #[derive(Default)]
    struct MemFile {
        cursor: Cursor<Vec<u8>>,
    }
    impl Read for MemFile {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.cursor.read(buf)
        }
    }
    impl Write for MemFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.cursor.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.cursor.flush()
        }
    }
    impl Seek for MemFile {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.cursor.seek(pos)
        }
    }
    impl VirtualFile for MemFile {
        fn set_len(&mut self, size: u64) -> anyhow::Result<()> {
            self.cursor.get_mut().resize(size as usize, 0);
            Ok(())
        }
        fn sync_all(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    // Create parent PSP
    let parent_seg = 0x1000;
    dos.create_psp(&mut memory, parent_seg, 0x9000, 0x0500, None, &[]);
    let parent_base = Memory::seg_offset_to_linear(parent_seg, 0);

    // Clear standard handle JFT entries so they don't collide with our SFT index.
    // In real DOS these would be valid console device SFT entries, but in this
    // unit test we only care about the file we're about to open.
    for i in 0..5 {
        memory.write_u8(parent_base + 0x18 + i, 0xFF);
    }

    // Open a file (handle 5, SFT index 0)
    let handle = dos
        .file_table
        .allocate(
            Box::new(MemFile::default()),
            String::from(r"C:\SHARED.DAT"),
            parent_seg,
        )
        .unwrap();
    let sft_idx = dos.file_table.get_sft_index(handle).unwrap();

    // Write SFT index into parent's JFT slot 5
    memory.write_u8(parent_base + 0x18 + 5, sft_idx as u8);

    // Create child with inheritance
    let child_seg = 0x2000;
    dos.create_psp(
        &mut memory,
        child_seg,
        0x9000,
        0x0500,
        Some(parent_seg),
        &[],
    );

    // Inheriting takes no reference: the one open handle is the only one.
    assert_eq!(dos.file_table.get_sft(handle).unwrap().ref_count, 1);

    // So closing it after the child ran releases the file. A reference
    // taken per EXEC was never released and left the SFT open forever,
    // refusing the next compatibility-mode open from another PSP.
    dos.file_table.free(handle);
    assert!(dos.file_table.get_sft(handle).is_none());
    assert_eq!(dos.file_table.get_sft_ref_count(sft_idx), None);
}

#[test]
fn test_init_environment() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    dos.init_environment(&mut memory, &SoundEnvConfig::default());

    // UMBs should be initialized and linked after boot
    assert!(dos.dos_mem.is_umb_initialized());
    assert!(dos.umb_linked);

    // Environment segment should be allocated
    assert!(dos.env_segment != 0);

    // Read environment strings
    let env_base = Memory::seg_offset_to_linear(dos.env_segment, 0);

    // First string should start with "PATH="
    let path_start = *b"PATH=";
    for (i, &expected) in path_start.iter().enumerate() {
        assert_eq!(memory.read_u8(env_base + i), expected);
    }
}

/// MDD-BUG-FLU-03380: a `--guest-env` variable larger than the default 1 KB
/// block grows the environment block. The guest MCB chain still walks to its
/// `Z` terminator, every string reads back, the program name still fits after
/// the double null, and room is left for later `SET`s. An empty value
/// deletes a default variable as `SET NAME=` does.
#[test]
fn test_init_environment_grows_for_a_large_guest_variable() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let big = "X".repeat(4096);
    dos.init_environment_with(
        &mut memory,
        &SoundEnvConfig::default(),
        &[
            ("include".to_string(), big.clone()),
            ("PROMPT".to_string(), String::new()),
        ],
    );

    let env_seg = dos.env_segment;
    let env_paras = dos.dos_mem.block_end_segment(env_seg).unwrap() - env_seg;
    assert!(
        env_paras > 0x40,
        "block grew past 1 KB: {env_paras:#x} paragraphs"
    );

    // Walk the MCB chain in guest memory from the first MCB to the 'Z' block.
    let mut mcb = dos.dos_mem.first_mcb_segment();
    let mut env_mcb_size = None;
    for _ in 0..4096 {
        let base = Memory::seg_offset_to_linear(mcb, 0);
        let marker = memory.read_u8(base);
        assert!(
            marker == b'M' || marker == b'Z',
            "bad MCB marker {marker:#x} at {mcb:#x}"
        );
        let size = memory.read_u16(base + 3);
        if mcb + 1 == env_seg {
            env_mcb_size = Some(size);
        }
        if marker == b'Z' {
            break;
        }
        mcb = mcb + size + 1;
    }
    assert_eq!(
        env_mcb_size,
        Some(env_paras),
        "the chain holds the grown block"
    );

    let env_base = Memory::seg_offset_to_linear(env_seg, 0);
    let mut strings = Vec::new();
    let mut offset = 0;
    loop {
        let mut entry = Vec::new();
        while memory.read_u8(env_base + offset) != 0 {
            entry.push(memory.read_u8(env_base + offset));
            offset += 1;
        }
        offset += 1;
        if entry.is_empty() {
            break;
        }
        strings.push(String::from_utf8(entry).unwrap());
    }
    assert!(strings.contains(&format!("INCLUDE={big}")));
    assert!(strings.iter().all(|entry| !entry.starts_with("PROMPT=")));

    let program = r"C:\TOOLS\BINNT\LINK.EXE";
    dos.set_env_program_name(&mut memory, program);
    assert_eq!(memory.read_u16(env_base + offset), 1);
    let name: Vec<u8> = (0..program.len())
        .map(|index| memory.read_u8(env_base + offset + 2 + index))
        .collect();
    assert_eq!(name, program.as_bytes());
    let used = offset + 2 + program.len() + 1;
    assert!(
        usize::from(env_paras) * 16 - used >= 128,
        "room left for later SETs"
    );
}

/// MDD-BUG-FLU-03529: the one replace-or-append rule both DOS environment
/// writers use. A name matches without ASCII case and keeps its place (taking
/// the upper-cased name), a new name is appended upper-cased, an empty value
/// deletes, and a high byte stays one byte.
#[test]
fn test_set_env_var_keeps_position_ignores_case_and_deletes_on_empty() {
    let mut vars: Vec<(Vec<u8>, Vec<u8>)> = vec![
        (b"PATH".to_vec(), b"C:\\DOS".to_vec()),
        (b"Mixed".to_vec(), b"1".to_vec()),
        (b"TEMP".to_vec(), b"C:\\TEMP".to_vec()),
    ];
    set_env_var(&mut vars, b"mixed", b"caf\xE9");
    set_env_var(&mut vars, b"new", b"x");
    set_env_var(&mut vars, b"temp", b"");
    assert_eq!(
        vars,
        [
            (b"PATH".to_vec(), b"C:\\DOS".to_vec()),
            (b"MIXED".to_vec(), b"caf\xE9".to_vec()),
            (b"NEW".to_vec(), b"x".to_vec()),
        ]
    );
    // Deleting a name that is not there changes nothing.
    set_env_var(&mut vars, b"ABSENT", b"");
    assert_eq!(vars.len(), 3);
}

#[test]
fn test_read_asciiz() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    // Write a test string
    let test_str = b"HELLO.TXT\0";
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in test_str.iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let result = dos.read_asciiz(&Cpu::new(), &memory, 0x2000, 0x0100);
    assert_eq!(result, "HELLO.TXT");
}

#[test]
fn test_read_asciiz_empty() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    // Write just a null terminator
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(addr, 0);

    let result = dos.read_asciiz(&Cpu::new(), &memory, 0x2000, 0x0100);
    assert_eq!(result, "");
}

#[test]
fn test_matches_pattern_star_star() {
    // Test *.* pattern matches any file
    assert!(Dos::matches_pattern_static("*.*", "test.txt"));
    assert!(Dos::matches_pattern_static("*.*", "readme"));
    assert!(Dos::matches_pattern_static("*.*", "file.com"));
}

#[test]
fn test_matches_pattern_specific() {
    // Test *.TXT pattern matching
    assert!(
        Dos::matches_pattern_static("*.TXT", "test.txt"),
        "Should match .TXT files"
    );
    assert!(
        !Dos::matches_pattern_static("*.TXT", "test.com"),
        "Should not match non-.TXT files"
    );
    assert!(
        Dos::matches_pattern_static("*.TXT", "FILE.TXT"),
        "Should match uppercase .TXT files"
    );
}

#[test]
fn test_matches_pattern_no_dot_and_trailing_dot() {
    assert!(Dos::matches_pattern_static("README", "README"));
    assert!(!Dos::matches_pattern_static("README", "README.TXT"));
    assert!(Dos::matches_pattern_static("README*", "README.TXT"));
    assert!(Dos::matches_pattern_static("README*", "README"));
    assert!(Dos::matches_pattern_static("README.", "README"));
    assert!(!Dos::matches_pattern_static("README.", "README.TXT"));
    assert!(Dos::matches_pattern_static("FOO.*", "FOO"));
    assert!(Dos::matches_pattern_static("*", "FILE.TXT"));
}

#[test]
fn test_matches_pattern_question_mark() {
    assert!(Dos::matches_pattern_static("F?LE.TXT", "FILE.TXT"));
    assert!(Dos::matches_pattern_static("FILE.T?T", "FILE.TXT"));
    assert!(Dos::matches_pattern_static("F*E.TXT", "FILE.TXT"));
    assert!(!Dos::matches_pattern_static("FILE.?", "FILE.TXT"));
}

#[test]
fn test_matches_pattern_extension_edge_cases() {
    assert!(Dos::matches_pattern_static("FILE", "FILE."));
    assert!(!Dos::matches_pattern_static("*.TXT", "FILE"));
}

#[test]
fn test_wildcard_match_star_middle() {
    assert!(Dos::wildcard_match("A*C", "AC"));
    assert!(Dos::wildcard_match("A*C", "ABC"));
    assert!(Dos::wildcard_match("A*C", "ABDC"));
    assert!(!Dos::wildcard_match("A*C", "AB"));
}

#[test]
fn test_wildcard_match_star_before_unicode_non_match_does_not_panic() {
    assert!(Dos::wildcard_match("*É", "CAFÉ"));
    assert!(Dos::wildcard_match("CAF?", "CAFÉ"));
    assert!(!Dos::wildcard_match("*Z", "CAFÉ"));
}

#[test]
fn test_wildcard_match_repeated_stars_have_linear_step_bound() {
    let pattern = format!("{}Z", "*".repeat(4096));
    let text = "A".repeat(4096);
    let input_chars = pattern.chars().count() + text.chars().count();

    let (matched, steps) = Dos::wildcard_match_counted(&pattern, &text);

    assert!(!matched);
    assert!(
        steps <= input_chars + 1,
        "repeated-star non-match took {steps} steps for {input_chars} input characters"
    );
}

#[test]
fn test_wildcard_match_agrees_with_bounded_reference_matrix() {
    fn word(mut ordinal: usize, len: usize, alphabet: &[char]) -> String {
        let mut result = String::with_capacity(len);
        for _ in 0..len {
            result.push(alphabet[ordinal % alphabet.len()]);
            ordinal /= alphabet.len();
        }
        result
    }

    fn reference(pattern: &str, text: &str) -> bool {
        let pattern: Vec<char> = pattern.chars().collect();
        let text: Vec<char> = text.chars().collect();
        let mut rows = vec![vec![false; text.len() + 1]; pattern.len() + 1];
        rows[0][0] = true;
        for pattern_index in 1..=pattern.len() {
            if pattern[pattern_index - 1] == '*' {
                rows[pattern_index][0] = rows[pattern_index - 1][0];
            }
            for text_index in 1..=text.len() {
                rows[pattern_index][text_index] = match pattern[pattern_index - 1] {
                    '*' => {
                        rows[pattern_index - 1][text_index] || rows[pattern_index][text_index - 1]
                    }
                    '?' => rows[pattern_index - 1][text_index - 1],
                    literal => {
                        literal == text[text_index - 1] && rows[pattern_index - 1][text_index - 1]
                    }
                };
            }
        }
        rows[pattern.len()][text.len()]
    }

    const PATTERN_ALPHABET: &[char] = &['A', 'B', 'É', '?', '*'];
    const TEXT_ALPHABET: &[char] = &['A', 'B', 'É'];
    for pattern_len in 0usize..=4 {
        for pattern_ordinal in 0..PATTERN_ALPHABET
            .len()
            .pow(u32::try_from(pattern_len).unwrap())
        {
            let pattern = word(pattern_ordinal, pattern_len, PATTERN_ALPHABET);
            for text_len in 0usize..=4 {
                for text_ordinal in 0..TEXT_ALPHABET.len().pow(u32::try_from(text_len).unwrap()) {
                    let text = word(text_ordinal, text_len, TEXT_ALPHABET);
                    assert_eq!(
                        Dos::wildcard_match(&pattern, &text),
                        reference(&pattern, &text),
                        "pattern={pattern:?}, text={text:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn test_wildcard_match_question_mark() {
    assert!(Dos::wildcard_match("A?C", "ABC"));
    assert!(!Dos::wildcard_match("A?C", "AC"));
}

#[test]
fn test_wildcard_match_question_mark_no_text() {
    assert!(!Dos::wildcard_match("?", ""));
    assert!(!Dos::wildcard_match("A?", "A"));
}

#[test]
fn test_wildcard_match_pattern_exhausted_text_remaining() {
    assert!(!Dos::wildcard_match("ABC", "ABCD"));
}

#[test]
fn test_wildcard_match_exact_full() {
    assert!(Dos::wildcard_match("ABC", "ABC"));
}

#[test]
fn test_int21_disposition() {
    assert_eq!(Int21Disposition::Iret, Int21Disposition::Iret);
    assert_eq!(Int21Disposition::NoIret, Int21Disposition::NoIret);
    assert_ne!(Int21Disposition::Iret, Int21Disposition::NoIret);
}

#[test]
fn test_write_dta_entry() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    dos.set_dta(0x3000, 0x0080);

    let entry = FindEntry {
        name: "TEST.TXT".to_string(),
        size: 1234,
        attr: 0x20, // Archive
        time: 0x4000,
        date: 0x5000,
    };

    let search_id = 42; // Test search ID
    dos.write_dta_entry(&mut memory, search_id, &entry);

    let dta_addr = Memory::seg_offset_to_linear(0x3000, 0x0080);

    // Check search ID is stored at offset 0
    assert_eq!(memory.read_u32(dta_addr), search_id);

    // Check attribute
    assert_eq!(memory.read_u8(dta_addr + 0x15), 0x20);

    // Check time
    assert_eq!(memory.read_u16(dta_addr + 0x16), 0x4000);

    // Check date
    assert_eq!(memory.read_u16(dta_addr + 0x18), 0x5000);

    // Check size
    assert_eq!(memory.read_u32(dta_addr + 0x1A), 1234);

    // Check filename
    assert_eq!(memory.read_u8(dta_addr + 0x1E), b'T');
    assert_eq!(memory.read_u8(dta_addr + 0x1F), b'E');
    assert_eq!(memory.read_u8(dta_addr + 0x20), b'S');
    assert_eq!(memory.read_u8(dta_addr + 0x21), b'T');
}

#[test]
fn test_write_dta_entry_near_segment_end_skips() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    dos.set_dta(0x4000, 0xFFD6); // Past the safe boundary for a full DTA entry.

    let entry = FindEntry {
        name: "SKIP.TXT".to_string(),
        size: 1,
        attr: 0x20,
        time: 0x1234,
        date: 0x5678,
    };

    let dta_addr = Memory::seg_offset_to_linear(dos.dta_segment, dos.dta_offset);
    memory.write_u8(dta_addr, 0xAA);

    dos.write_dta_entry(&mut memory, 1, &entry);

    assert_eq!(memory.read_u8(dta_addr), 0xAA);
}

/// MDD-BUG-CRU-01393: a 32-bit client's DTA whose offset's low 16 bits
/// lie in 0xFFD6..=0xFFFF has no 64 KiB boundary, so the find record is
/// written rather than silently skipped.
#[test]
fn write_dta_entry_for_a_32bit_client_ignores_the_16bit_boundary() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.edx = 0x0002_FFF0;
    pm16_int21(&mut dos, &mut cpu, &mut memory, 0x1A00);
    assert_eq!(dos.dta_offset, 0xFFF0);

    let entry = FindEntry {
        name: "FLAT.DAT".to_string(),
        size: 1,
        attr: 0x20,
        time: 0x1234,
        date: 0x5678,
    };
    dos.write_dta_entry(&mut memory, 7, &entry);

    let dta = dos.dta_linear;
    assert_eq!(memory.read_u32(dta), 7, "search id");
    assert_eq!(memory.read_u8(dta + 0x1E), b'F', "file name");
}

#[test]
fn test_find_get_entry_invalid_id() {
    let mut dos = Dos::new();
    // Invalid search ID should return None
    assert!(dos.find_get_entry(0).is_none());
    assert!(dos.find_get_entry(999).is_none());
}

#[test]
fn test_cursor_file_set_len_grows_and_truncates() {
    let mut file = CursorFile::new(vec![1, 2]);
    file.set_len(4).unwrap();
    assert_eq!(file.cursor.get_ref().len(), 4);
    file.set_len(1).unwrap();
    assert_eq!(file.cursor.get_ref().len(), 1);
}

#[test]
fn test_partial_write_file_set_len_grows_and_truncates() {
    let mut file = PartialWriteFile::new(2);
    file.set_len(3).unwrap();
    assert_eq!(file.cursor.get_ref().len(), 3);
    file.set_len(1).unwrap();
    assert_eq!(file.cursor.get_ref().len(), 1);
}

#[test]
fn test_find_first_missing_drive_returns_err() {
    let mut dos = Dos::new();
    let result = dos.find_first("A:\\*.TXT", 0xFF);
    assert!(result.is_err());
}

#[test]
fn test_find_first_read_dir_error_returns_err() {
    let mut dos = Dos::new();
    dos.drives.insert(2, Box::new(NotFoundDrive));
    let result = dos.find_first("C:\\*.TXT", 0xFF);
    assert!(result.is_err());
}

/// A volume-label search on a drive with no label reports NO match.
///
/// A host-backed directory without a label must not manufacture a label entry.
#[test]
fn find_first_reports_no_volume_label_when_the_drive_has_none_anvil137() {
    let mut dos = Dos::new();
    dos.drives.insert(2, Box::new(NotFoundDrive));

    assert!(
        dos.find_first("C:\\*.*", 0x08).is_err(),
        "an unlabelled drive must report no volume label, not a fabricated one"
    );
}

/// The other half: a drive that really does carry a label still reports it,
/// so the fix cannot degrade into "never answer a volume-label search".
/// MDD-BUG-ANVIL-00137.
#[test]
fn find_first_still_reports_a_real_volume_label_anvil137() {
    let mut dos = Dos::new();
    dos.drives.insert(2, Box::new(LabelledDrive));

    let search_id = dos
        .find_first("C:\\*.*", 0x08)
        .expect("a drive carrying a label must answer the search");
    let entry = dos.find_get_entry(search_id).unwrap();

    assert_eq!(entry.name, "REALDISK");
    assert_eq!(entry.attr & 0x08, 0x08);
}

/// MDD-BUG-CRU-01407: a subdirectory search yields `.` and `..` (as
/// directories, first) before its files; the root yields neither, and a
/// search for `..` finds the parent entry.
#[test]
fn find_first_in_a_subdirectory_lists_dot_and_dot_dot_first() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let temp_dir = TempDir::new().unwrap();
    std::fs::create_dir(temp_dir.path().join("GAMES")).unwrap();
    for name in ["A.TXT", "B.TXT"] {
        std::fs::write(temp_dir.path().join("GAMES").join(name), b"x").unwrap();
    }
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let search_id = dos.find_first("C:\\GAMES\\*.*", 0x10).unwrap();
    let mut listed = Vec::new();
    while let Some(entry) = dos.find_get_entry(search_id) {
        listed.push((entry.name.to_string(), entry.attr & 0x10));
        dos.find_advance(search_id);
    }
    assert_eq!(
        listed,
        [
            (".".to_string(), 0x10),
            ("..".to_string(), 0x10),
            ("A.TXT".to_string(), 0),
            ("B.TXT".to_string(), 0),
        ]
    );

    let search_id = dos.find_first("C:\\GAMES\\..", 0x10).unwrap();
    assert_eq!(
        dos.find_get_entry(search_id).unwrap().name.to_string(),
        ".."
    );

    let search_id = dos.find_first("C:\\*.*", 0x10).unwrap();
    let root_first = dos.find_get_entry(search_id).unwrap();
    assert_eq!(
        root_first.name.to_string(),
        "GAMES",
        "the root has no dot entries"
    );
}

#[test]
fn test_find_first_wraps_search_id() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("WRAP.TXT");
    std::fs::write(&file_path, b"ok").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    dos.next_search_id = u32::MAX;

    let search_id = dos.find_first("C:\\WRAP.TXT", 0xFF).unwrap();
    assert_eq!(search_id, u32::MAX);
    assert_eq!(dos.next_search_id, 1);
}

#[test]
fn test_exec_read_param_block_null() {
    let dos = Dos::new();
    let memory = Memory::new();

    let (env_seg, cmdline_ptr) = dos.exec_read_param_block_cmdtail(&memory, 0, 0);
    assert_eq!(env_seg, 0);
    assert!(cmdline_ptr.is_none());
}

#[test]
fn test_exec_read_param_block() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    // Set up a parameter block at ES:BX
    let pb_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u16(pb_addr, 0x0500); // env segment
    memory.write_u16(pb_addr + 2, 0x0080); // cmdline offset
    memory.write_u16(pb_addr + 4, 0x1000); // cmdline segment

    let (env_seg, cmdline_ptr) = dos.exec_read_param_block_cmdtail(&memory, 0x2000, 0x0100);
    assert_eq!(env_seg, 0x0500);
    assert_eq!(cmdline_ptr, Some((0x1000, 0x0080)));
}

#[test]
fn test_exec_copy_cmdtail_to_psp() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    // Set up a command tail at source location
    let src = Memory::seg_offset_to_linear(0x1000, 0x0080);
    memory.write_u8(src, 5); // length
    memory.write_u8(src + 1, b' ');
    memory.write_u8(src + 2, b't');
    memory.write_u8(src + 3, b'e');
    memory.write_u8(src + 4, b's');
    memory.write_u8(src + 5, b't');
    memory.write_u8(src + 6, 0x0D);

    dos.exec_copy_cmdtail_to_psp(&mut memory, 0x2000, Some((0x1000, 0x0080)));

    // Check destination
    let dst = Memory::seg_offset_to_linear(0x2000, 0x80);
    assert_eq!(memory.read_u8(dst), 5); // length
    assert_eq!(memory.read_u8(dst + 1), b' ');
    assert_eq!(memory.read_u8(dst + 2), b't');
}

#[test]
fn test_exec_copy_cmdtail_to_psp_none() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    dos.exec_copy_cmdtail_to_psp(&mut memory, 0x2000, None);

    let dst = Memory::seg_offset_to_linear(0x2000, 0x80);
    assert_eq!(memory.read_u8(dst), 0); // length = 0
    assert_eq!(memory.read_u8(dst + 1), 0x0D); // CR terminator
}

/// HLD Part 2 — pins `exec_read_param_block_fcbs` returns both FCB
/// pointers when both are non-zero. Param block layout per RBIL:
/// `+06h` DWORD FCB1 (offset:segment), `+0Ah` DWORD FCB2.
#[test]
fn test_exec_read_param_block_fcbs_parses_both() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    let pb_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    // FCB1 far ptr (offset:segment).
    memory.write_u16(pb_addr + 0x06, 0x0010);
    memory.write_u16(pb_addr + 0x08, 0x3000);
    // FCB2 far ptr (offset:segment).
    memory.write_u16(pb_addr + 0x0A, 0x0020);
    memory.write_u16(pb_addr + 0x0C, 0x3000);

    let (fcb1, fcb2) = dos.exec_read_param_block_fcbs(&memory, 0x2000, 0x0100);
    assert_eq!(fcb1, Some((0x3000, 0x0010)));
    assert_eq!(fcb2, Some((0x3000, 0x0020)));
}

/// HLD Part 2 — pins the 0:0 sentinel convention. A 0:0 FCB pointer in
/// the param block means "parent could not parse a filename argument";
/// `exec_read_param_block_fcbs` returns `None` for that slot.
#[test]
fn test_exec_read_param_block_fcbs_treats_0_0_as_none() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    let pb_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    // FCB1 = 0:0 (sentinel).
    memory.write_u16(pb_addr + 0x06, 0x0000);
    memory.write_u16(pb_addr + 0x08, 0x0000);
    // FCB2 = 0x3000:0x0020 (real pointer).
    memory.write_u16(pb_addr + 0x0A, 0x0020);
    memory.write_u16(pb_addr + 0x0C, 0x3000);

    let (fcb1, fcb2) = dos.exec_read_param_block_fcbs(&memory, 0x2000, 0x0100);
    assert_eq!(fcb1, None);
    assert_eq!(fcb2, Some((0x3000, 0x0020)));
}

/// HLD Part 2 — pins the no-param-block case. An EXEC with es=0, bx=0
/// has no parameter block (internally-driven EXEC); both FCB pointers
/// must be `None`.
#[test]
fn test_exec_read_param_block_fcbs_es_bx_zero_returns_none_none() {
    let dos = Dos::new();
    let memory = Memory::new();

    let (fcb1, fcb2) = dos.exec_read_param_block_fcbs(&memory, 0, 0);
    assert_eq!(fcb1, None);
    assert_eq!(fcb2, None);
}

/// HLD Part 2 — pins `exec_copy_fcbs_to_psp` copies 16 bytes from each
/// parent FCB buffer to PSP:0x5C (FCB1) and PSP:0x6C (FCB2).
#[test]
fn test_exec_copy_fcbs_to_psp_copies_16_bytes() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    // Parent FCB1 buffer at 0x3000:0x0010 — fill with a known pattern.
    let fcb1_src = Memory::seg_offset_to_linear(0x3000, 0x0010);
    for i in 0..16usize {
        memory.write_u8(fcb1_src + i, 0xA0 + i as u8);
    }
    // Parent FCB2 buffer at 0x3000:0x0020 — different pattern.
    let fcb2_src = Memory::seg_offset_to_linear(0x3000, 0x0020);
    for i in 0..16usize {
        memory.write_u8(fcb2_src + i, 0xB0 + i as u8);
    }

    dos.exec_copy_fcbs_to_psp(
        &mut memory,
        0x4000,
        Some((0x3000, 0x0010)),
        Some((0x3000, 0x0020)),
    );

    // Verify PSP:0x5C..=0x6B == FCB1 pattern.
    let psp_base = Memory::seg_offset_to_linear(0x4000, 0);
    for i in 0..16usize {
        assert_eq!(
            memory.read_u8(psp_base + 0x5C + i),
            0xA0 + i as u8,
            "PSP:0x{:02X} (FCB1 byte {i})",
            0x5C + i,
        );
    }
    // Verify PSP:0x6C..=0x7B == FCB2 pattern.
    for i in 0..16usize {
        assert_eq!(
            memory.read_u8(psp_base + 0x6C + i),
            0xB0 + i as u8,
            "PSP:0x{:02X} (FCB2 byte {i})",
            0x6C + i,
        );
    }
}

/// HLD Part 2 — pins the no-FCB case. After `create_psp`'s zero-fill,
/// `exec_copy_fcbs_to_psp(None, None)` must leave PSP:0x5C..=0x7B as
/// zero (no overwrite).
#[test]
fn test_exec_copy_fcbs_to_psp_none_leaves_zero() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    // Use create_psp so PSP:0x5C..=0x7B is zeroed by Part 1's fill.
    dos.create_psp(&mut memory, 0x4000, 0x9000, 0x0500, None, &[]);

    // No-op call with both FCB pointers absent.
    dos.exec_copy_fcbs_to_psp(&mut memory, 0x4000, None, None);

    let psp_base = Memory::seg_offset_to_linear(0x4000, 0);
    for off in 0x5Cusize..=0x7B {
        assert_eq!(
            memory.read_u8(psp_base + off),
            0,
            "PSP:0x{off:02X} must remain zero with no parent FCB",
        );
    }
}

/// HLD Part 2 — pins that one FCB present + one absent works correctly:
/// the present FCB is copied; the absent one's slot stays zero.
#[test]
fn test_exec_copy_fcbs_to_psp_mixed_some_and_none() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    // Pre-zero the PSP (simulates Part 1's create_psp zero-fill).
    dos.create_psp(&mut memory, 0x4000, 0x9000, 0x0500, None, &[]);

    let fcb1_src = Memory::seg_offset_to_linear(0x3000, 0x0010);
    for i in 0..16usize {
        memory.write_u8(fcb1_src + i, 0xC0 + i as u8);
    }

    dos.exec_copy_fcbs_to_psp(&mut memory, 0x4000, Some((0x3000, 0x0010)), None);

    let psp_base = Memory::seg_offset_to_linear(0x4000, 0);
    // FCB1 slot populated.
    for i in 0..16usize {
        assert_eq!(memory.read_u8(psp_base + 0x5C + i), 0xC0 + i as u8);
    }
    // FCB2 slot remains zero.
    for off in 0x6Cusize..=0x7B {
        assert_eq!(memory.read_u8(psp_base + off), 0);
    }
}

#[test]
fn test_exec_apply_relocations() {
    let mut memory = Memory::new();

    // Create fake EXE data with one relocation
    let mut data = vec![0u8; 100];
    // Relocation at offset 0 in segment 0 -> should fix address at load_segment:0000
    data[50] = 0x00; // offset low
    data[51] = 0x00; // offset high
    data[52] = 0x00; // segment low
    data[53] = 0x00; // segment high

    // Write initial value at load address
    let load_segment = 0x1000u16;
    let addr = Memory::seg_offset_to_linear(load_segment, 0);
    memory.write_u16(addr, 0x0010); // Original value

    Dos::exec_apply_relocations(&mut memory, &data, 50, 1, load_segment);

    // Value should be adjusted by load_segment
    let new_val = memory.read_u16(addr);
    assert_eq!(new_val, 0x0010 + load_segment);
}

#[test]
fn test_parse_trace_ah_none() {
    let _lock = env_lock();
    assert!(parse_trace_ah().is_none());
}

#[test]
fn test_dos_int_trace_enabled_env() {
    let _trace = trace_subscriber("gp64::dos::int=debug");
    assert!(dos_int_trace_enabled());
}

#[test]
fn test_dos_int_trace_max_env() {
    let _lock = env_lock();
    let _guard = EnvVarGuard::set("GP64_DOS_TRACE", "int=42");
    assert_eq!(dos_int_trace_max(), 42);
}

#[test]
fn test_dos_mem_trace_enabled_env() {
    let _trace = trace_subscriber("gp64::dos::memory=debug");
    assert!(dos_mem_trace_enabled());
}

#[test]
fn test_dos_read_trace_enabled_env() {
    let _trace = trace_subscriber("gp64::dos::read=debug");
    assert!(dos_read_trace_enabled());
}

#[test]
fn test_dos_seek_trace_enabled_env() {
    let _trace = trace_subscriber("gp64::dos::seek=debug");
    assert!(dos_seek_trace_enabled());
}

#[test]
fn test_dos_write_trace_enabled_env() {
    let _trace = trace_subscriber("gp64::dos::write=debug");
    assert!(dos_write_trace_enabled());
}

#[test]
fn test_dos_find_trace_enabled_env() {
    let _trace = trace_subscriber("gp64::dos::find=debug");
    assert!(dos_find_trace_enabled());
}

#[test]
fn test_should_trace_ah() {
    let _lock = env_lock();
    // The AH-range filter is permanently absent, so should_trace_ah always
    // returns true. We can verify by calling should_trace_ah with any value.
    let _ = should_trace_ah(0x00);
    let _ = should_trace_ah(0xFF);
}

#[test]
fn test_set_extended_error_mapping() {
    let mut dos = Dos::new();
    let cases = [
        (0u16, (0x00, 0x00, 0x00)),
        (1u16, (0x07, 0x04, 0x01)),
        (2u16, (0x08, 0x03, 0x02)),
        (3u16, (0x08, 0x03, 0x02)),
        (4u16, (0x01, 0x04, 0x01)),
        (5u16, (0x03, 0x04, 0x02)),
        (6u16, (0x07, 0x04, 0x01)),
        (7u16, (0x04, 0x05, 0x05)),
        (8u16, (0x01, 0x04, 0x05)),
        (9u16, (0x07, 0x04, 0x05)),
        (10u16, (0x07, 0x04, 0x05)),
        (11u16, (0x09, 0x04, 0x02)),
        (12u16, (0x07, 0x04, 0x02)),
        (13u16, (0x07, 0x04, 0x01)),
        (15u16, (0x07, 0x03, 0x02)),
        (16u16, (0x07, 0x04, 0x02)),
        (17u16, (0x02, 0x01, 0x02)),
        // MS-DOS v4.0/src/DOS/MS_TABLE.ASM ErrTab: NotFnd/User/Disk.
        (18u16, (0x08, 0x03, 0x02)),
        (19u16, (0x0B, 0x07, 0x02)),
        (20u16, (0x04, 0x05, 0x01)),
        (21u16, (0x05, 0x07, 0x02)),
        (32u16, (0x0A, 0x02, 0x02)),
        (33u16, (0x0A, 0x02, 0x02)),
        (80u16, (0x0C, 0x04, 0x02)),
        (0x1234u16, (0x0D, 0x04, 0x01)),
    ];

    for (code, (class, action, locus)) in cases {
        dos.set_extended_error(code);
        assert_eq!(dos.extended_error, (code, class, action, locus));
    }

    // Exercise the full documented DOS error-map ranges so all match arms stay covered.
    for code in 22u16..=90u16 {
        dos.set_extended_error(code);
        assert_eq!(dos.extended_error.0, code);
    }

    dos.clear_extended_error();
    assert_eq!(dos.extended_error, (0, 0, 0, 0));
}

#[test]
fn test_set_error_return_sets_carry_and_extended() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    dos.set_error_return(&mut cpu, 5);
    assert_eq!(cpu.regs.ax(), 5);
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(dos.extended_error, (5, 0x03, 0x04, 0x02));
}

#[test]
fn test_dos_trace_functions_exercise() {
    // Exercise trace functions - they now use tracing::enabled!() instead of env vars
    let _ = dos_trace_enabled();
    let _ = dos_print_trace_enabled();
    let _ = exec_trace_enabled();
    trace_dos_path("OPEN", 0x1234, 0x5678, "TEST.DAT");
}

#[test]
fn test_dos_config_defaults() {
    let _lock = env_lock();
    let _guard_max = EnvVarGuard::unset("GP64_DOS_TRACE");

    assert_eq!(dos_print_trace_max(), 200);
}

#[test]
fn test_dos_config_env_overrides() {
    let _lock = env_lock();
    let _guard_print_max = EnvVarGuard::set("GP64_DOS_TRACE", "print=42");

    assert_eq!(dos_print_trace_max(), 42);
}

#[test]
fn test_format_dos_bytes_handles_control_and_truncation() {
    let (s, truncated) = format_dos_bytes(b"A\n\t\x00", 10);
    assert_eq!(s, "A\\n\\t\\x00");
    assert!(!truncated);

    let (s, truncated) = format_dos_bytes(b"ABC", 2);
    assert_eq!(s, "AB");
    assert!(truncated);
}

#[test]
fn test_dos_resource_default_filter_accepts_nonempty_paths() {
    let _lock = env_lock();
    let _all = EnvVarGuard::unset("GP64_DOS_TRACE");

    assert!(dos_resource_path_matches(r"C:\TEST\DATA.BIN"));
    assert!(dos_resource_path_matches(r"c:\test\data.bin"));
    assert!(!dos_resource_path_matches(""));
}

#[test]
fn test_dos_resource_include_all_overrides_filter() {
    let _lock = env_lock();
    let _all = EnvVarGuard::set("GP64_DOS_TRACE", "resource_all=1");

    assert!(dos_resource_path_matches(r"C:\TEST\TOOL.EXE"));
}

#[test]
fn test_dos_resource_trace_env_flag_is_value_aware() {
    let _lock = env_lock();
    let _trace = EnvVarGuard::set("GP64_DOS_TRACE", "resource=0");
    assert!(!dos_resource_trace_env_enabled());

    let _trace = EnvVarGuard::set("GP64_DOS_TRACE", "resource=1");
    assert!(dos_resource_trace_env_enabled());
}

#[test]
fn test_dos_resource_event_cap_records_drop_state() {
    let state = DosResourceEventState::new();

    assert_eq!(state.next_event(1), Some(0));
    assert_eq!(state.next_event(1), None);
    assert_eq!(state.dropped_events(), 1);
}

#[test]
fn test_dos_resource_handle_fields_are_stable() {
    assert_eq!(dos_resource_handle_value(Some(5)), 5);
    assert_eq!(dos_resource_handle_hex(Some(5)), "0005");
    assert_eq!(dos_resource_handle_value(None), -1);
    assert_eq!(dos_resource_handle_hex(None), "NONE");
}

#[test]
fn test_find_first_nonexistent() {
    let mut dos = Dos::new();

    // Try to find a pattern that won't match anything
    let result = dos.find_first("ZZZZZZZZ.ZZZ", 0xFF);
    assert!(result.is_err());
}

#[test]
fn test_find_entry_struct() {
    let entry = FindEntry {
        name: "TEST.TXT".to_string(),
        size: 100,
        attr: 0x20,
        time: 0x1234,
        date: 0x5678,
    };

    assert_eq!(entry.name, "TEST.TXT");
    assert_eq!(entry.size, 100);
    assert_eq!(entry.attr, 0x20);
    assert_eq!(entry.time, 0x1234);
    assert_eq!(entry.date, 0x5678);
}

#[test]
fn test_process_state() {
    let regs = gp64_cpu::Registers::new();
    let state = ProcessState {
        psp_segment: 0x1000,
        registers: regs.clone(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0200,
        exec_ret_cs: 0x3000,
        exec_ret_flags: 0x0202,
        pm_parent: false,
    };

    assert_eq!(state.psp_segment, 0x1000);
    assert_eq!(state.parent_psp, 0x0100);
    assert_eq!(state.exec_ret_ip, 0x0200);
    assert_eq!(state.exec_ret_cs, 0x3000);
    assert_eq!(state.exec_ret_flags, 0x0202);
}

#[test]
fn test_ah_range() {
    let range = AhRange {
        start: 0x10,
        end: 0x20,
    };
    assert_eq!(range.start, 0x10);
    assert_eq!(range.end, 0x20);
}

#[test]
fn test_ah_range_copy_clone() {
    let range = AhRange {
        start: 0x10,
        end: 0x20,
    };
    let range2 = range;
    let range3 = range;
    assert_eq!(range.start, range2.start);
    assert_eq!(range.end, range3.end);
}

#[test]
fn test_int21_disposition_debug() {
    let disp = Int21Disposition::Iret;
    assert_eq!(format!("{disp:?}"), "Iret");

    let disp2 = Int21Disposition::NoIret;
    assert_eq!(format!("{disp2:?}"), "NoIret");
}

#[test]
fn test_dos_version() {
    let dos = Dos::new();
    assert_eq!(dos.version_major, 6);
    assert_eq!(dos.version_minor, 22);
}

#[test]
fn test_create_psp_with_long_args() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    // Long command line that would overflow 127 bytes
    let long_args: Vec<String> = (0..20).map(|i| format!("arg{i}")).collect();
    dos.create_psp(&mut memory, 0x1000, 0x9000, 0x0500, None, &long_args);

    let base = Memory::seg_offset_to_linear(0x1000, 0);
    let len = memory.read_u8(base + 0x80);
    // Command tail should be truncated
    assert!(len <= 127);
}

#[test]
fn test_exec_copy_cmdtail_max_length() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    // Set up a command tail that's at max length
    let src = Memory::seg_offset_to_linear(0x1000, 0x0080);
    memory.write_u8(src, 126); // length
    for i in 1..=126 {
        memory.write_u8(src + i, b'A');
    }
    memory.write_u8(src + 127, 0x0D);

    dos.exec_copy_cmdtail_to_psp(&mut memory, 0x2000, Some((0x1000, 0x0080)));

    let dst = Memory::seg_offset_to_linear(0x2000, 0x80);
    let len = memory.read_u8(dst);
    assert!(len <= 126);
}

#[test]
fn test_get_process_stack_empty() {
    let dos = Dos::new();
    assert!(dos.process_stack.is_empty());
}

#[test]
fn test_file_table_access() {
    let dos = Dos::new();
    // File table should be accessible - just verify it exists
    let _file_table = &dos.file_table;
}

#[test]
fn test_path_resolver_access() {
    let mut dos = Dos::new();
    // Path resolver should be accessible
    let _resolved = dos.path_resolver.resolve("TEST.TXT");
}

#[test]
fn test_xms_access() {
    let dos = Dos::new();
    // XMS should be initialized
    let _xms = &dos.xms;
}

#[test]
fn test_dpmi_access() {
    let dos = Dos::new();
    // DPMI should be initialized
    let _dpmi = &dos.dpmi;
}

#[test]
fn test_dos_mem_access() {
    let dos = Dos::new();
    // DOS memory manager should be accessible
    let _mem = &dos.dos_mem;
}

#[test]
fn test_find_searches_empty() {
    let dos = Dos::new();
    assert!(dos.find_searches.is_empty());
    assert_eq!(dos.next_search_id, 1); // Starts at 1, 0 is invalid
    assert_eq!(dos.fcb_search_id, 0); // No active FCB search
    assert!(std::mem::size_of_val(&dos.find_searches) < std::mem::size_of::<FindSearchState>());
}

#[test]
fn test_find_search_state_keeps_entries_boxed() {
    assert!(std::mem::size_of::<FindSearchState>() < 1024);
}

#[test]
fn test_find_search_cleanup_on_exhaust() {
    use crate::filesystem::FileAttributes;
    use std::time::SystemTime;

    let mut dos = Dos::new();

    // Manually insert a search state with one entry
    let mut entries = FindSearchEntries::new_boxed();
    entries.push((
        FixedString::from("TEST.TXT"),
        FileAttributes {
            directory: false,
            size: 100,
            modified: SystemTime::UNIX_EPOCH,
            ..Default::default()
        },
    ));
    let state = FindSearchState {
        pattern: FixedString::from("*.TXT"),
        drive: 2,
        dir_path: FixedString::from("\\"),
        attr_filter: 0,
        entries,
        index: 0,
    };
    dos.find_searches.push((1, Box::new(state)));
    assert_eq!(dos.find_searches.len(), 1);

    // Get the first (and only) entry
    let entry = dos.find_get_entry(1);
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().name, "TEST.TXT");

    // Search still exists (not exhausted yet)
    assert_eq!(dos.find_searches.len(), 1);

    // Advance past the entry
    dos.find_advance(1);

    // Try to get next entry - should return None and clean up
    let entry2 = dos.find_get_entry(1);
    assert!(entry2.is_none());

    // Search should now be removed
    assert!(dos.find_searches.is_empty());
}

#[test]
fn test_dta_defaults() {
    let dos = Dos::new();
    assert_eq!(dos.dta_segment, 0x0100);
    assert_eq!(dos.dta_offset, 0x0080);
}

#[test]
fn test_find_entry_fields() {
    let entry = FindEntry {
        name: "TEST.TXT".to_string(),
        size: 100,
        attr: 0x20,
        time: 0x1234,
        date: 0x5678,
    };
    assert_eq!(entry.name, "TEST.TXT");
    assert_eq!(entry.size, 100);
    assert_eq!(entry.attr, 0x20);
}

#[test]
fn test_process_state_clone() {
    let regs = gp64_cpu::Registers::new();
    let state = ProcessState {
        psp_segment: 0x1000,
        registers: regs.clone(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0200,
        exec_ret_cs: 0x3000,
        exec_ret_flags: 0x0202,
        pm_parent: false,
    };
    let cloned = state.clone();
    assert_eq!(state.psp_segment, cloned.psp_segment);
}

#[test]
fn test_exec_apply_relocations_multiple() {
    let mut memory = Memory::new();

    // Create fake EXE data with two relocations
    let mut data = vec![0u8; 100];
    // Relocation 1 at offset 0 in segment 0
    data[50] = 0x00;
    data[51] = 0x00;
    data[52] = 0x00;
    data[53] = 0x00;
    // Relocation 2 at offset 4 in segment 0
    data[54] = 0x04;
    data[55] = 0x00;
    data[56] = 0x00;
    data[57] = 0x00;

    let load_segment = 0x1000u16;
    let addr1 = Memory::seg_offset_to_linear(load_segment, 0);
    let addr2 = Memory::seg_offset_to_linear(load_segment, 4);
    memory.write_u16(addr1, 0x0010);
    memory.write_u16(addr2, 0x0020);

    Dos::exec_apply_relocations(&mut memory, &data, 50, 2, load_segment);

    assert_eq!(memory.read_u16(addr1), 0x0010 + load_segment);
    assert_eq!(memory.read_u16(addr2), 0x0020 + load_segment);
}

#[test]
fn test_exec_apply_relocations_empty() {
    let mut memory = Memory::new();
    let data = vec![0u8; 100];
    let load_segment = 0x1000u16;

    // No relocations
    Dos::exec_apply_relocations(&mut memory, &data, 50, 0, load_segment);
    // Should not crash
}

#[test]
fn test_exec_load_com_insufficient_memory() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);
    let largest = dos.dos_mem.largest_free();
    let _ = dos.dos_mem.alloc(&mut memory, dos.current_psp, largest);

    let image = vec![0x90];
    let result = dos.exec_load_com(
        &mut cpu,
        &mut memory,
        &image,
        0x2000,
        None,
        None,
        None,
        dos.current_psp,
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 8);
}

#[test]
fn test_exec_load_exe_invalid_header() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    let exe_data = vec![0u8; 10];
    let result = dos.exec_load_exe(
        &mut cpu,
        &mut memory,
        &exe_data,
        0x2000,
        None,
        None,
        None,
        dos.current_psp,
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 11);
}

#[test]
fn test_read_asciiz_long_string() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    // Write a longer test string
    let test_str = b"C:\\GAMES\\TOOL\\EXTRA.EXE\0";
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in test_str.iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let result = dos.read_asciiz(&Cpu::new(), &memory, 0x2000, 0x0100);
    assert_eq!(result, "C:\\GAMES\\TOOL\\EXTRA.EXE");
}
