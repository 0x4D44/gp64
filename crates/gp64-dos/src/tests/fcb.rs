use super::*;

/// MDD-BUG-KILN-00233 (fault 1 of 2): resolve the guest's FCB pointer
/// through the segment descriptor, not by a real-mode `ds << 4`.
///
/// Under a protected-mode guest — Windows 3.1's File Manager is the
/// witness — `DS` is a selector whose descriptor base bears no relation to
/// `selector << 4`, so the old shift read whatever happened to sit at the
/// low-memory address instead of the caller's FCB. Here the real FCB names
/// `TARGET.TXT` and a decoy naming `DECOY.TXT` sits exactly where the shift
/// would have landed; the search must find TARGET.
#[test]
fn fcb_find_first_resolves_the_fcb_through_the_segment_descriptor() {
    use tempfile::TempDir;

    const SELECTOR: u16 = 0x0080;
    const BASE: u32 = 0x0003_0000;
    const OFFSET: u16 = 0x0100;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("TARGET.TXT"), b"target").unwrap();
    std::fs::write(temp_dir.path().join("DECOY.TXT"), b"decoy").unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    cpu.regs.cr0 |= 1;
    cpu.regs.ds = SELECTOR;
    cpu.regs.ds_cache.base = BASE;
    cpu.regs.ds_cache.limit = 0xFFFF;
    cpu.regs.ds_cache.cached_effective_limit = 0xFFFF;
    cpu.regs.ds_cache.present = true;

    let pm_addr = BASE as usize + OFFSET as usize;
    let real_mode_addr = ((SELECTOR as usize) << 4) + OFFSET as usize;
    assert_ne!(pm_addr, real_mode_addr);
    create_test_fcb_at(&mut memory, pm_addr, 3, "TARGET", "TXT");
    create_test_fcb_at(&mut memory, real_mode_addr, 3, "DECOY", "TXT");

    dos.set_dta_pm(SELECTOR, 0x0400, BASE);

    cpu.regs.set_ah(0x11);
    cpu.regs.set_dx(OFFSET);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();

    assert_eq!(cpu.regs.al(), 0x00, "AH=11h should have found a match");
    let name: String = (0..8)
        .map(|i| memory.read_u8(dos.dta_linear() + Dos::FCB_SEARCH_NAME + i) as char)
        .collect();
    assert_eq!(
        name.trim_end(),
        "TARGET",
        "AH=11h read the FCB from the real-mode address, not the descriptor base"
    );
}

/// MDD-BUG-KILN-00233 (fault 2 of 2): an extended FCB's search attribute
/// selects what the search may match.
///
/// `0x08` alone means "the volume label and nothing else". The old code
/// passed a blanket `0xFF` to `find_first`, so a volume-label search
/// matched the first ordinary FILE instead and its 8.3 name was serialized
/// into the caller's 11-byte label field — which is what File Manager drew
/// as the drive's label.
#[test]
fn fcb_find_first_honours_an_extended_fcbs_volume_label_search_attribute() {
    use tempfile::TempDir;

    const SEGMENT: u16 = 0x2000;
    const OFFSET: u16 = 0x0100;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("ORDINARY.TXT"), b"file").unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    // Extended FCB: 0xFF marker, five reserved bytes, then the search
    // attribute, then the standard FCB body.
    let fcb = Memory::seg_offset_to_linear(SEGMENT, OFFSET);
    memory.write_u8(fcb, 0xFF);
    for i in 1..6 {
        memory.write_u8(fcb + i, 0);
    }
    memory.write_u8(fcb + Dos::FCB_EXTENDED_ATTR, 0x08);
    create_test_fcb_at(
        &mut memory,
        fcb + Dos::FCB_EXTENDED_PREFIX,
        3,
        "????????",
        "???",
    );

    dos.set_dta(0x3000, 0x0000);

    cpu.regs.set_ah(0x11);
    cpu.regs.ds = SEGMENT;
    cpu.regs.set_dx(OFFSET);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();

    // The subject of this test is unchanged: a 0x08 search must not match an
    // ordinary file. What changed is the outcome when there is no label to
    // find. This directory holds only ORDINARY.TXT and, being host-backed,
    // carries no volume-label directory entry — so real DOS reports no
    // match. It used to report a match only because `find_first`
    // synthesized a constant `GP64` label, which is the defect
    // MDD-BUG-ANVIL-00137 removed; asserting "a match was found" here was
    // asserting the hack. Not-found is the stronger statement of the same
    // rule, because matching ORDINARY would still fail it.
    //
    // The positive direction — a drive that DOES carry a label still
    // reports it — is pinned by
    // `find_first_still_reports_a_real_volume_label_anvil137`.
    assert_ne!(
        cpu.regs.al(),
        0x00,
        "a volume-label search of a directory with no label must not report a match"
    );
}

#[test]
fn test_int21_parse_filename_into_fcb_skips_separators_and_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src = b"  ;,=C:TEST?.TXT\0";
    let src_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    for (i, &b) in src.iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x01); // Skip separators
    cpu.regs.ds = 0x1000;
    cpu.regs.set_si(0x0000);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x01);

    let fcb_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    assert_eq!(memory.read_u8(fcb_addr), 3);

    let mut name = Vec::new();
    for i in 0..8 {
        name.push(memory.read_u8(fcb_addr + 1 + i));
    }
    let mut ext = Vec::new();
    for i in 0..3 {
        ext.push(memory.read_u8(fcb_addr + 9 + i));
    }
    assert_eq!(String::from_utf8_lossy(&name).trim_end(), "TEST?");
    assert_eq!(String::from_utf8_lossy(&ext).trim_end(), "TXT");
}

#[test]
fn test_int21_parse_filename_into_fcb_no_drive_no_ext() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src = b"README\0";
    let src_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    for (i, &b) in src.iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_si(0x0000);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00);

    let fcb_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    assert_eq!(memory.read_u8(fcb_addr), 0);
    assert_eq!(memory.read_u8(fcb_addr + 1), b'R');
    assert_eq!(memory.read_u8(fcb_addr + 9), b' ');
}

#[test]
fn test_int21_parse_filename_into_fcb_invalid_drive_prefix() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src = b"1:FILE.TXT\0";
    let src_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    for (i, &b) in src.iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_si(0x0000);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    let fcb_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    assert_eq!(memory.read_u8(fcb_addr), 0);
    assert_eq!(memory.read_u8(fcb_addr + 1), b'1');
}

/// MDD-BUG-CRU-01391: `*` fills the rest of its field with `?`, as MS-DOS
/// 6.22 yields `"????????TXT"` for `*.TXT`, rather than a literal `*`.
#[test]
fn test_int21_parse_filename_into_fcb_expands_star_to_question_marks() {
    let (al, si, fcb) = int21_parse_fcb(b"*.TXT\0", 0x00);
    assert_eq!(al, 0x01, "wildcard present");
    assert_eq!(si, 5, "SI advances past the parsed text");
    assert_eq!(fcb[0], 0);
    assert_eq!(&fcb[1..12], b"????????TXT", "* expands to ? for the field");

    let (al, _, fcb) = int21_parse_fcb(b"C:AB*.T*\0", 0x00);
    assert_eq!(al, 0x01);
    assert_eq!(fcb[0], 3);
    assert_eq!(&fcb[1..12], b"AB??????T??");
}

/// MDD-BUG-CRU-01391: parsing stops at an AH=29h invalid character
/// (`:;,=+<>|"[]/\`), leaving SI on it, instead of copying it through.
#[test]
fn test_int21_parse_filename_into_fcb_stops_at_invalid_char() {
    let (al, si, fcb) = int21_parse_fcb(b"AB;CD.TXT\0", 0x00);
    assert_eq!(al, 0x00);
    assert_eq!(si, 2, "SI stops on the ';'");
    assert_eq!(&fcb[1..12], b"AB         ");

    let (_, si, fcb) = int21_parse_fcb(b"NAME.EXT+X\0", 0x00);
    assert_eq!(si, 8, "SI stops on the '+'");
    assert_eq!(&fcb[1..12], b"NAME    EXT");
}

#[test]
fn test_fcb_is_extended_normal() {
    let memory = Memory::new();
    // Normal FCB (doesn't start with 0xFF)
    assert!(!Dos::fcb_is_extended(
        &memory,
        Memory::seg_offset_to_linear(0x1000, 0x0000)
    ));
}

#[test]
fn test_fcb_is_extended_true() {
    let mut memory = Memory::new();
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    memory.write_u8(addr, 0xFF);
    assert!(Dos::fcb_is_extended(
        &memory,
        Memory::seg_offset_to_linear(0x1000, 0x0000)
    ));
}

#[test]
fn test_fcb_base_addr_normal() {
    let memory = Memory::new();
    let fcb = Memory::seg_offset_to_linear(0x1000, 0x0100);
    assert_eq!(Dos::fcb_base_addr(&memory, fcb), fcb);
}

#[test]
fn test_fcb_base_addr_extended() {
    let mut memory = Memory::new();
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u8(addr, 0xFF);
    assert_eq!(Dos::fcb_base_addr(&memory, addr), addr + 7);
}

#[test]
fn test_fcb_read_filename_simple() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TESTFILE", "TXT");

    let filename = dos.fcb_read_filename(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(filename, "TESTFILE.TXT");
}

#[test]
fn test_fcb_read_filename_no_extension() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TESTFILE", "   ");

    let filename = dos.fcb_read_filename(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(filename, "TESTFILE");
}

#[test]
fn test_fcb_read_filename_short() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "C");

    let filename = dos.fcb_read_filename(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(filename, "TEST.C");
}

#[test]
fn test_fcb_read_filename_skips_space_and_nul() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);

    memory.write_u8(addr + Dos::FCB_DRIVE, 1);

    let name_bytes = [b'F', b'O', b'O', b' ', 0, b'B', b'A', b'R'];
    for (i, &b) in name_bytes.iter().enumerate() {
        memory.write_u8(addr + Dos::FCB_FILENAME + i, b);
    }

    let ext_bytes = [b'T', b' ', 0];
    for (i, &b) in ext_bytes.iter().enumerate() {
        memory.write_u8(addr + Dos::FCB_EXTENSION + i, b);
    }

    let filename = dos.fcb_read_filename(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(filename, "FOOBAR.T");
}

#[test]
fn test_fcb_bytes_to_filename_trims_and_uppercases() {
    let name_bytes = *b"foo     ";
    let ext_bytes = *b"tx ";
    let filename = Dos::fcb_bytes_to_filename(&name_bytes, &ext_bytes);
    assert_eq!(filename, "FOO.TX");
}

#[test]
fn test_fcb_bytes_to_filename_skips_nul_bytes() {
    let name_bytes = [b'A', 0, b'B', b' ', b' ', b' ', b' ', b' '];
    let ext_bytes = [b'C', 0, b' '];
    let filename = Dos::fcb_bytes_to_filename(&name_bytes, &ext_bytes);
    assert_eq!(filename, "AB.C");
}

#[test]
fn test_apply_rename_pattern_keeps_and_replaces() {
    let new_name_bytes = *b"??NEW???";
    let new_ext_bytes = *b"?X?";
    let renamed = Dos::apply_rename_pattern("REPORT.TXT", &new_name_bytes, &new_ext_bytes);
    assert_eq!(renamed, "RENEWT.TXT");
}

#[test]
fn test_apply_rename_pattern_skips_missing_chars() {
    let new_name_bytes = *b"??      ";
    let new_ext_bytes = *b"?? ";
    let renamed = Dos::apply_rename_pattern("A.T", &new_name_bytes, &new_ext_bytes);
    assert_eq!(renamed, "A.T");
}

#[test]
fn test_apply_rename_pattern_ignores_nul_bytes() {
    let new_name_bytes = [0, b'N', b' ', b' ', b' ', b' ', b' ', b' '];
    let new_ext_bytes = [0, b'X', b' '];
    let renamed = Dos::apply_rename_pattern("FILE.TXT", &new_name_bytes, &new_ext_bytes);
    assert_eq!(renamed, "N.X");
}

#[test]
fn test_fcb_bytes_to_filename_preserves_wildcards() {
    let name_bytes = *b"ab?*    ";
    let ext_bytes = *b"t? ";
    let filename = Dos::fcb_bytes_to_filename(&name_bytes, &ext_bytes);
    assert_eq!(filename, "AB?*.T?");
}

#[test]
fn test_fcb_bytes_to_filename_no_extension() {
    let name_bytes = *b"readme  ";
    let ext_bytes = *b"   ";
    let filename = Dos::fcb_bytes_to_filename(&name_bytes, &ext_bytes);
    assert_eq!(filename, "README");
}

#[test]
fn test_apply_rename_pattern_drops_extension() {
    let new_name_bytes = *b"NEW     ";
    let new_ext_bytes = *b"   ";
    let renamed = Dos::apply_rename_pattern("REPORT.TXT", &new_name_bytes, &new_ext_bytes);
    assert_eq!(renamed, "NEW");
}

#[test]
fn test_apply_rename_pattern_keeps_original_extension() {
    let new_name_bytes = *b"NEW?????";
    let new_ext_bytes = *b"???";
    let renamed = Dos::apply_rename_pattern("REPORT.TXT", &new_name_bytes, &new_ext_bytes);
    assert_eq!(renamed, "NEWORT.TXT");
}

#[test]
fn test_apply_rename_pattern_no_original_extension() {
    let new_name_bytes = *b"NEW?????";
    let new_ext_bytes = *b"???";
    let renamed = Dos::apply_rename_pattern("README", &new_name_bytes, &new_ext_bytes);
    assert_eq!(renamed, "NEWDME");
}

#[test]
fn test_create_rename_fcb_pads_extension() {
    let mut memory = Memory::new();
    create_rename_fcb(&mut memory, 0x1000, 0x0100, 3, "OLD", "TXT", "NEW", "X");

    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    assert_eq!(memory.read_u8(addr + 25), b'X');
    assert_eq!(memory.read_u8(addr + 26), b' ');
    assert_eq!(memory.read_u8(addr + 27), b' ');
}

#[test]
fn test_fcb_read_drive_default() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 0, "TEST", "TXT");

    // Drive 0 means default (C: = 3)
    let drive = dos.fcb_read_drive(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(drive, 3); // 2 (current drive C:) + 1
}

#[test]
fn test_fcb_read_drive_explicit() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 1, "TEST", "TXT");

    let drive = dos.fcb_read_drive(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(drive, 1); // A:
}

/// MDD-BUG-CRU-01392: an FCB drive byte past `Z:` names no drive. Every
/// FCB call that builds a path from it reports AL=FFh with "invalid
/// drive", instead of overflowing `drive - 1 + b'A'` (a debug-build abort
/// for `0xC0..=0xFF`) or wrapping to a relative name on the current drive.
#[test]
fn fcb_calls_reject_a_drive_byte_beyond_z() {
    let base = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for ah in [0x0F, 0x11, 0x13, 0x16, 0x17] {
        for drive in [27, 0xC0, 0xC5, 0xFE] {
            let mut dos = Dos::new();
            let mut cpu = gp64_cpu::Cpu::new();
            let mut memory = Memory::new();
            let (mut vga, mut keyboard) = create_test_io();
            create_test_fcb(&mut memory, 0x2000, 0x0100, drive, "TEST", "TXT");
            assert_eq!(dos.fcb_drive_letter(&memory, base), None);
            cpu.regs.set_ah(ah);
            cpu.regs.ds = 0x2000;
            cpu.regs.set_dx(0x0100);
            let result = dos.int21(
                &mut cpu,
                &mut memory,
                &mut vga,
                &mut keyboard,
                &mut Pic::new(),
            );
            assert!(result.is_ok(), "AH={ah:02X}h drive={drive:#04x}");
            assert_eq!(cpu.regs.al(), 0xFF, "AH={ah:02X}h drive={drive:#04x}");
            assert_eq!(
                dos.extended_error.0, 15,
                "AH={ah:02X}h drive={drive:#04x} reports invalid drive"
            );
        }
    }

    // An extended FCB (0xFF marker) carries its drive at +7, where 0xFF
    // itself is a plain out-of-range drive byte.
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    memory.write_u8(base, 0xFF);
    create_test_fcb_at(
        &mut memory,
        base + Dos::FCB_EXTENDED_PREFIX,
        0xFF,
        "TEST",
        "TXT",
    );
    cpu.regs.set_ah(0x0F);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
    assert_eq!(dos.extended_error.0, 15);
}

#[test]
fn fcb_drive_letter_spans_a_to_z() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    let base = Memory::seg_offset_to_linear(0x1000, 0x0100);
    for (drive, letter) in [(0, Some('C')), (1, Some('A')), (26, Some('Z')), (27, None)] {
        create_test_fcb(&mut memory, 0x1000, 0x0100, drive, "TEST", "TXT");
        assert_eq!(dos.fcb_drive_letter(&memory, base), letter, "drive {drive}");
    }
}

/// MDD-BUG-CRU-01392: AH=1Ch with DL past `Z:` reports AL=FFh rather than
/// overflowing `b'A' + dl - 1` for DL >= 0xBF.
#[test]
fn int21_1c_rejects_a_drive_beyond_z() {
    for dl in [27, 0xBF, 0xC0, 0xFF] {
        let mut dos = Dos::new();
        let mut cpu = gp64_cpu::Cpu::new();
        let mut memory = Memory::new();
        let (mut vga, mut keyboard) = create_test_io();
        cpu.regs.set_ah(0x1C);
        cpu.regs.set_dl(dl);
        let result = dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        );
        assert!(result.is_ok(), "DL={dl:#04x}");
        assert_eq!(cpu.regs.al(), 0xFF, "DL={dl:#04x}");
    }
}

#[test]
fn test_fcb_set_get_handle() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x1000, 0x0100),
        42,
    );
    let handle = dos.fcb_get_handle(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(handle, 42);
}

#[test]
fn test_int21_parse_filename_skips_separators_and_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"  ;,=C:FILE.TXT\0".iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x01);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00);

    let fcb_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    assert_eq!(memory.read_u8(fcb_addr), 3);

    let mut name = [0u8; 8];
    for (i, byte) in name.iter_mut().enumerate() {
        *byte = memory.read_u8(fcb_addr + 1 + i);
    }
    assert_eq!(&name, b"FILE    ");

    let mut ext = [0u8; 3];
    for (i, byte) in ext.iter_mut().enumerate() {
        *byte = memory.read_u8(fcb_addr + 9 + i);
    }
    assert_eq!(&ext, b"TXT");
}

#[test]
fn test_int21_parse_filename_wildcard_sets_al() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"B:TE?T.TX*\0".iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x01);

    let fcb_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    assert_eq!(memory.read_u8(fcb_addr), 2);
}

#[test]
fn test_int21_parse_filename_skips_separators_null_terminates() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u8(src_addr, 0);

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x01);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    let fcb_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    assert_eq!(memory.read_u8(fcb_addr), 0);
    assert_eq!(memory.read_u8(fcb_addr + 1), b' ');
    assert_eq!(memory.read_u8(fcb_addr + 9), b' ');
}

#[test]
fn test_int21_parse_filename_terminates_on_space() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"FILE NAME\0".iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    let fcb_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    let mut name = [0u8; 8];
    for (i, byte) in name.iter_mut().enumerate() {
        *byte = memory.read_u8(fcb_addr + 1 + i);
    }
    assert_eq!(&name, b"FILE    ");
}

#[test]
fn test_int21_parse_filename_terminates_on_tab() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"FILE\tNAME\0".iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_int21_parse_filename_terminates_on_cr() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"FILE\rNAME\0".iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_int21_parse_filename_terminates_on_lf() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"FILE\nNAME\0".iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

#[test]
fn test_fcb_set_file_size() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    dos.fcb_set_file_size(
        &mut memory,
        Memory::seg_offset_to_linear(0x1000, 0x0100),
        0x12345678,
    );
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    let size = memory.read_u32(addr + Dos::FCB_FILE_SIZE);
    assert_eq!(size, 0x12345678);
}

#[test]
fn test_fcb_get_file_size() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u32(addr + Dos::FCB_FILE_SIZE, 0xCAFEBABE);

    let size = dos.fcb_get_file_size(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(size, 0xCAFEBABE);
}

#[test]
fn test_fcb_record_size_default() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    // Record size 0 should return 128 as default
    let size = dos.fcb_get_record_size(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(size, 128);
}

#[test]
fn test_fcb_set_get_record_size() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    dos.fcb_set_record_size(
        &mut memory,
        Memory::seg_offset_to_linear(0x1000, 0x0100),
        256,
    );
    let size = dos.fcb_get_record_size(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(size, 256);
}

#[test]
fn test_fcb_random_record() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    // Set random record number
    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    memory.write_u32(addr + Dos::FCB_RANDOM_RECORD, 12345);

    let record = dos.fcb_get_random_record(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(record, 12345);
}

#[test]
fn test_fcb_set_datetime() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    dos.fcb_set_datetime(
        &mut memory,
        Memory::seg_offset_to_linear(0x1000, 0x0100),
        0x1234,
        0x5678,
    );

    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    assert_eq!(memory.read_u16(addr + Dos::FCB_FILE_DATE), 0x1234);
    assert_eq!(memory.read_u16(addr + Dos::FCB_FILE_TIME), 0x5678);
}

#[test]
fn test_fcb_set_get_current_block_and_record() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    dos.fcb_set_current_block(
        &mut memory,
        Memory::seg_offset_to_linear(0x1000, 0x0100),
        0x0042,
    );
    dos.fcb_set_current_record(
        &mut memory,
        Memory::seg_offset_to_linear(0x1000, 0x0100),
        0x7A,
    );

    let block = dos.fcb_get_current_block(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    let record = dos.fcb_get_current_record(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(block, 0x0042);
    assert_eq!(record, 0x7A);
}

#[test]
fn test_fcb_advance_position_increments_record() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    dos.fcb_set_current_block(&mut memory, Memory::seg_offset_to_linear(0x1000, 0x0100), 2);
    dos.fcb_set_current_record(&mut memory, Memory::seg_offset_to_linear(0x1000, 0x0100), 5);

    dos.fcb_advance_position(&mut memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(
        dos.fcb_get_current_block(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100)),
        2
    );
    assert_eq!(
        dos.fcb_get_current_record(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100)),
        6
    );
}

#[test]
fn test_fcb_advance_position_wraps_block() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    dos.fcb_set_current_block(&mut memory, Memory::seg_offset_to_linear(0x1000, 0x0100), 2);
    dos.fcb_set_current_record(
        &mut memory,
        Memory::seg_offset_to_linear(0x1000, 0x0100),
        127,
    );

    dos.fcb_advance_position(&mut memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(
        dos.fcb_get_current_block(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100)),
        3
    );
    assert_eq!(
        dos.fcb_get_current_record(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100)),
        0
    );
}

#[test]
fn test_fcb_set_drive_extended() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    let base_offset = 0x0100;
    let ext_offset = base_offset + 7;
    let base_addr = Memory::seg_offset_to_linear(0x1000, base_offset);
    memory.write_u8(base_addr, 0xFF);
    create_test_fcb(&mut memory, 0x1000, ext_offset, 0, "TEST", "TXT");

    dos.fcb_set_drive(
        &mut memory,
        Memory::seg_offset_to_linear(0x1000, base_offset),
        4,
    );

    let addr = Memory::seg_offset_to_linear(0x1000, ext_offset);
    assert_eq!(memory.read_u8(addr + Dos::FCB_DRIVE), 4);
}

#[test]
fn test_fcb_set_drive_normal() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 0, "TEST", "TXT");

    dos.fcb_set_drive(&mut memory, Memory::seg_offset_to_linear(0x1000, 0x0100), 5);

    let addr = Memory::seg_offset_to_linear(0x1000, 0x0100);
    assert_eq!(memory.read_u8(addr + Dos::FCB_DRIVE), 5);
}

#[test]
fn test_fcb_set_random_record() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    create_test_fcb(&mut memory, 0x1000, 0x0100, 3, "TEST", "TXT");

    dos.fcb_set_random_record(
        &mut memory,
        Memory::seg_offset_to_linear(0x1000, 0x0100),
        0xDEADBEEF,
    );
    let record = dos.fcb_get_random_record(&memory, Memory::seg_offset_to_linear(0x1000, 0x0100));
    assert_eq!(record, 0xDEADBEEF);
}

#[test]
fn test_get_path_from_env_none() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.env_segment = 0x2000;
    write_env_block(&mut memory, 0x2000, &["COMSPEC=C:\\COMMAND.COM"]);

    assert!(dos.get_path_from_env(&memory).is_none());
}

#[test]
fn test_get_path_from_env_returns_value() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.env_segment = 0x2000;
    write_env_block(
        &mut memory,
        0x2000,
        &["COMSPEC=C:\\COMMAND.COM", "Path=C:\\DOS;C:\\BIN"],
    );

    let path = dos.get_path_from_env(&memory).expect("expected PATH");
    assert_eq!(path, "C:\\DOS;C:\\BIN");
}

#[test]
fn test_try_read_file_by_dos_name_searches_path_with_separator() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    let temp_dir = TempDir::new().unwrap();
    let bin_dir = temp_dir.path().join("BIN");
    std::fs::create_dir(&bin_dir).unwrap();
    let file_path = bin_dir.join("TEST.COM");
    std::fs::write(&file_path, [0xC3u8]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    dos.env_segment = 0x2000;
    write_env_block(&mut memory, 0x2000, &[r"PATH=C:\BIN"]);

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    for (i, &b) in b"TEST\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_ok());
    let (_path, data) = result.unwrap();
    assert_eq!(data, vec![0xC3]);
}

#[test]
fn test_try_read_file_by_dos_name_path_has_trailing_separator() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    let temp_dir = TempDir::new().unwrap();
    let bin_dir = temp_dir.path().join("BIN");
    std::fs::create_dir(&bin_dir).unwrap();
    let file_path = bin_dir.join("TAIL.COM");
    std::fs::write(&file_path, [0x90u8]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    dos.env_segment = 0x2000;
    write_env_block(&mut memory, 0x2000, &["PATH=C:\\BIN\\"]);

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    for (i, &b) in b"TAIL\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_ok());
    let (_path, data) = result.unwrap();
    assert_eq!(data, vec![0x90]);
}

#[test]
fn test_try_read_file_by_dos_name_path_trailing_forward_slash() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    let temp_dir = TempDir::new().unwrap();
    let bin_dir = temp_dir.path().join("BIN");
    std::fs::create_dir(&bin_dir).unwrap();
    let file_path = bin_dir.join("FSLASH.COM");
    std::fs::write(&file_path, [0x90u8]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    dos.env_segment = 0x2000;
    write_env_block(&mut memory, 0x2000, &["PATH=C:/BIN/"]);

    write_asciiz(&mut memory, 0x3000, 0x0100, "FSLASH");

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_ok());
    let (_path, data) = result.unwrap();
    assert_eq!(data, vec![0x90]);
}

#[test]
fn test_try_read_file_by_dos_name_read_error_returns_not_found() {
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.drives.insert(2, Box::new(FailingReadDrive));

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    for (i, &b) in b"PROG.COM\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn test_try_read_file_by_dos_name_access_denied_preserved() {
    // MDD-BUG-FLUXHOMEARPA-00018: an EXEC target that exists but is unreadable
    // (a directory or a permission-denied file) must surface PermissionDenied,
    // not be collapsed to NotFound (which callers map to DOS file-not-found).
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.drives.insert(2, Box::new(AccessDeniedDrive));

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    for (i, &b) in b"PROG.COM\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "an access-denied EXEC target must not be reported as not-found"
    );
}

#[test]
fn test_try_read_file_by_dos_name_path_open_error() {
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.drives.insert(2, Box::new(DenyWriteDrive));
    dos.env_segment = 0x2000;
    write_env_block(&mut memory, 0x2000, &["PATH=C:\\BIN"]);

    write_asciiz(&mut memory, 0x3000, 0x0100, "OPENFAIL");

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn test_try_read_file_by_dos_name_path_read_failure() {
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.drives.insert(2, Box::new(FailingReadDrive));
    dos.env_segment = 0x2000;
    write_env_block(&mut memory, 0x2000, &["PATH=C:\\BIN"]);

    write_asciiz(&mut memory, 0x3000, 0x0100, "READFAIL");

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn test_try_read_file_by_dos_name_missing_drive() {
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    for (i, &b) in b"D:\\MISSING.EXE\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn test_try_read_file_by_dos_name_forward_slash_has_path() {
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    write_asciiz(&mut memory, 0x3000, 0x0100, "C:/NOPE/PROG.EXE");

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn test_try_read_file_by_dos_name_path_empty_entry_skipped() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    let temp_dir = TempDir::new().unwrap();
    let bin_dir = temp_dir.path().join("BIN");
    std::fs::create_dir(&bin_dir).unwrap();
    let file_path = bin_dir.join("EMPTY.COM");
    std::fs::write(&file_path, [0x90u8]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    dos.env_segment = 0x2000;
    write_env_block(&mut memory, 0x2000, &["PATH=;C:\\BIN"]);

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    for (i, &b) in b"EMPTY\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_ok());
}

#[test]
fn test_try_read_file_by_dos_name_path_missing_drive_skipped() {
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.env_segment = 0x2000;
    write_env_block(&mut memory, 0x2000, &["PATH=D:\\BIN"]);

    let addr = Memory::seg_offset_to_linear(0x3000, 0x0100);
    for (i, &b) in b"NOPATH\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    let result = dos.try_read_file_by_dos_name(&cpu, &memory, 0x3000, 0x0100);
    assert!(result.is_err());
}

#[test]
fn test_get_drive_allocation_data_valid_drive() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let temp_dir = std::env::temp_dir().join("gp64_alloc_drive");
    let _ = std::fs::create_dir(&temp_dir);
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    dos.get_drive_allocation_data(&mut cpu, &mut memory, 'C');

    assert_eq!(cpu.regs.al(), 8);
    assert_eq!(cpu.regs.cx(), 512);
    assert_eq!(cpu.regs.dx(), 0xFFFF);
    assert_eq!(cpu.regs.ds, METADATA_SEG);
    assert_eq!(cpu.regs.bx(), MEDIA_ID_OFFSET);
    let media_id_addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.bx());
    assert_eq!(memory.read_u8(media_id_addr), 0xF8);

    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_get_drive_allocation_data_invalid_drive() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.get_drive_allocation_data(&mut cpu, &mut memory, 'Z');

    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_set_env_program_name_writes_after_env() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_environment(&mut memory, &SoundEnvConfig::default());

    dos.set_env_program_name(&mut memory, "MYPROG.EXE");

    let env_base = Memory::seg_offset_to_linear(dos.env_segment, 0);
    let mut offset = 0usize;
    loop {
        let b = memory.read_u8(env_base + offset);
        if b == 0 {
            let next = memory.read_u8(env_base + offset + 1);
            if next == 0 {
                offset += 2;
                break;
            }
        }
        offset += 1;
    }

    let count = memory.read_u16(env_base + offset);
    assert_eq!(count, 1);
    offset += 2;

    let mut name = Vec::new();
    loop {
        let b = memory.read_u8(env_base + offset);
        offset += 1;
        if b == 0 {
            break;
        }
        name.push(b);
    }
    assert_eq!(String::from_utf8(name).unwrap(), "MYPROG.EXE");
}

#[test]
fn test_set_env_program_name_no_env_segment_is_noop() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.env_segment = 0;
    memory.write_u8(0x0000, 0xAA);

    dos.set_env_program_name(&mut memory, "SHOULDNOTWRITE.EXE");

    assert_eq!(memory.read_u8(0x0000), 0xAA);
}

#[test]
fn test_try_read_file_by_dos_name_current_dir() {
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();
    let temp_dir = std::env::temp_dir().join("gp64_try_read_current");
    let _ = std::fs::create_dir(&temp_dir);
    let test_file = temp_dir.join("TEST.EXE");
    std::fs::write(&test_file, b"ok").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "TEST");
    let (path, data) = dos
        .try_read_file_by_dos_name(&cpu, &memory, 0x2000, 0x0100)
        .expect("file read");

    assert!(path.to_string_lossy().ends_with(r"\TEST.EXE"));
    assert_eq!(data, b"ok");

    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_try_read_file_dotted_parent_dir_probes_extensions() {
    // MDD-BUG-CRUCIBLE-00061: a dotted PARENT directory must not suppress
    // .COM/.EXE probing of an extensionless basename. `C:\DIR.V1\PROG` must still
    // find synthetic-program even though the parent component "DIR.V1" contains a dot.
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();

    let temp_dir = TempDir::new().unwrap();
    let sub = temp_dir.path().join("DIR.V1");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("PROG.COM"), b"ok").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, r"C:\DIR.V1\PROG");
    let (path, data) = dos
        .try_read_file_by_dos_name(&cpu, &memory, 0x2000, 0x0100)
        .expect("dotted parent dir + extensionless basename must probe PROG.COM");

    assert!(
        path.to_string_lossy().to_uppercase().ends_with("PROG.COM"),
        "must resolve to the probed .COM, got {path:?}"
    );
    assert_eq!(data, b"ok");
}

#[test]
fn test_try_read_file_by_dos_name_path_search() {
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();
    let temp_dir = std::env::temp_dir().join("gp64_try_read_path");
    let bin_dir = temp_dir.join("BIN");
    let _ = std::fs::create_dir_all(&bin_dir);
    let test_file = bin_dir.join("HELLO.COM");
    std::fs::write(&test_file, b"path").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    dos.env_segment = 0x2000;
    write_env_block(&mut memory, 0x2000, &["PATH=C:\\BIN"]);

    write_asciiz(&mut memory, 0x2000, 0x0200, "HELLO");
    let (path, data) = dos
        .try_read_file_by_dos_name(&cpu, &memory, 0x2000, 0x0200)
        .expect("path search");

    assert!(path.to_string_lossy().ends_with(r"\BIN\HELLO.COM"));
    assert_eq!(data, b"path");

    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_try_read_file_by_dos_name_with_path_skips_env_search() {
    let mut dos = Dos::new();
    let cpu = Cpu::new();
    let mut memory = Memory::new();
    let temp_dir = std::env::temp_dir().join("gp64_try_read_path_skip");
    let bin_dir = temp_dir.join("BIN");
    let _ = std::fs::create_dir_all(&bin_dir);
    let test_file = bin_dir.join("HELLO.COM");
    std::fs::write(&test_file, b"path").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    dos.env_segment = 0x2000;
    write_env_block(&mut memory, 0x2000, &["PATH=C:\\BIN"]);

    write_asciiz(&mut memory, 0x2000, 0x0300, "C:\\MISSING\\HELLO");
    let err = dos
        .try_read_file_by_dos_name(&cpu, &memory, 0x2000, 0x0300)
        .expect_err("expected not found");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_exec_read_param_block_cmdtail_none() {
    let dos = Dos::new();
    let memory = Memory::new();
    let (env_seg, cmdline) = dos.exec_read_param_block_cmdtail(&memory, 0, 0);
    assert_eq!(env_seg, 0);
    assert!(cmdline.is_none());
}

#[test]
fn test_exec_read_param_block_cmdtail_reads_values() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    let es = 0x2000;
    let bx = 0x0100;
    let addr = Memory::seg_offset_to_linear(es, bx);
    memory.write_u16(addr, 0x1234);
    memory.write_u16(addr + 2, 0x0040);
    memory.write_u16(addr + 4, 0x3000);

    let (env_seg, cmdline) = dos.exec_read_param_block_cmdtail(&memory, es, bx);
    assert_eq!(env_seg, 0x1234);
    assert_eq!(cmdline, Some((0x3000, 0x0040)));
}

#[test]
fn test_exec_read_param_block_cmdtail_zero_far_ptr_means_none() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    let es = 0x2000;
    let bx = 0x0100;
    let addr = Memory::seg_offset_to_linear(es, bx);
    memory.write_u16(addr, 0x1234);
    memory.write_u16(addr + 2, 0x0000);
    memory.write_u16(addr + 4, 0x0000);
    memory.write_u8(0, 5);

    let (env_seg, cmdline) = dos.exec_read_param_block_cmdtail(&memory, es, bx);
    assert_eq!(env_seg, 0x1234);
    assert!(cmdline.is_none());
}

#[test]
fn test_exec_copy_cmdtail_to_psp_copies_bytes() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    let src_seg = 0x2000;
    let src_off = 0x0100;
    let src_addr = Memory::seg_offset_to_linear(src_seg, src_off);
    memory.write_u8(src_addr, 3);
    memory.write_u8(src_addr + 1, b'A');
    memory.write_u8(src_addr + 2, b'B');
    memory.write_u8(src_addr + 3, b'C');
    memory.write_u8(src_addr + 4, 0x0D);

    dos.exec_copy_cmdtail_to_psp(&mut memory, 0x3000, Some((src_seg, src_off)));

    let dst_addr = Memory::seg_offset_to_linear(0x3000, 0x80);
    assert_eq!(memory.read_u8(dst_addr), 3);
    assert_eq!(memory.read_u8(dst_addr + 1), b'A');
    assert_eq!(memory.read_u8(dst_addr + 2), b'B');
    assert_eq!(memory.read_u8(dst_addr + 3), b'C');
    assert_eq!(memory.read_u8(dst_addr + 4), 0x0D);
}

#[test]
fn test_exec_copy_cmdtail_to_psp_defaults_when_missing() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.exec_copy_cmdtail_to_psp(&mut memory, 0x3000, None);

    let dst_addr = Memory::seg_offset_to_linear(0x3000, 0x80);
    assert_eq!(memory.read_u8(dst_addr), 0);
    assert_eq!(memory.read_u8(dst_addr + 1), 0x0D);
}

#[test]
#[allow(clippy::identity_op)] // keep structure offsets explicit for readability
fn test_init_kernel_structures_writes_lol_fields() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    dos.init_kernel_structures(&mut memory);

    let base = Memory::seg_offset_to_linear(LOL_SEGMENT, LOL_OFFSET);
    assert_eq!(
        memory.read_u16(base.wrapping_sub(2)),
        dos.dos_mem.first_mcb_segment()
    );
    // firstDPB -> DPB_SEG:0000
    assert_eq!(memory.read_u16(base + 0x00), 0x0000);
    assert_eq!(memory.read_u16(base + 0x02), DPB_SEG);
    // firstFileTable -> INFOBLOCK_SEG:00CC
    assert_eq!(memory.read_u16(base + 0x04), 0x00CC);
    assert_eq!(memory.read_u16(base + 0x06), INFOBLOCK_SEG);
    // Max sector length
    assert_eq!(memory.read_u16(base + 0x10), 0x0200);
    // CDS pointer
    assert_eq!(memory.read_u16(base + 0x16), 0x0000);
    assert_eq!(memory.read_u16(base + 0x18), CDS_SEG);
    // FCB table pointer
    assert_eq!(memory.read_u16(base + 0x1A), 0x0000);
    assert_eq!(memory.read_u16(base + 0x1C), FCBSFT_SEG);
    // Block devices: 0 immediately after init (the DOS model
    // `dos_classes.cpp:93`). The AH=52h handler refreshes this
    // from the live mounted-drive count on every call.
    assert_eq!(memory.read_u8(base + 0x20), 0);
    assert_eq!(memory.read_u8(base + 0x21), DOS_LASTDRIVE);
    // NUL device header at +22h: next pointer, then attributes, then name
    assert_eq!(memory.read_u16(base + 0x22), 0x0000);
    assert_eq!(memory.read_u16(base + 0x24), CONDRV_SEG);
    assert_eq!(memory.read_u16(base + 0x26), 0x8004);
    assert_eq!(memory.read_u16(base + 0x28), 0x0000);
    assert_eq!(memory.read_u16(base + 0x2A), 0x0000);

    let mut name = [0u8; 8];
    for (i, byte) in name.iter_mut().enumerate() {
        *byte = memory.read_u8(base + 0x2C + i);
    }
    assert_eq!(&name, b"NUL     ");

    // BUFFERS and boot drive
    assert_eq!(memory.read_u16(base + 0x3F), DOS_BUFFERS_X);
    assert_eq!(memory.read_u16(base + 0x41), DOS_BUFFERS_Y);
    assert_eq!(memory.read_u8(base + 0x43), 3);
    assert_eq!(memory.read_u8(base + 0x44), 1);
    // Disk buffer head pointer
    assert_eq!(memory.read_u16(base + 0x47), 0x0000);
    assert_eq!(memory.read_u16(base + 0x49), DISKBUF_SEG);
}
fn write_env_block(memory: &mut Memory, segment: u16, entries: &[&str]) {
    let mut offset = 0usize;
    let base = Memory::seg_offset_to_linear(segment, 0);
    for entry in entries {
        for &b in entry.as_bytes() {
            memory.write_u8(base + offset, b);
            offset += 1;
        }
        memory.write_u8(base + offset, 0);
        offset += 1;
    }
    memory.write_u8(base + offset, 0);
}

/// Run AH=29h over `src` with AL=`al`; returns (AL, SI, the 12 FCB bytes).
fn int21_parse_fcb(src: &[u8], al: u8) -> (u8, u16, [u8; 12]) {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let src_addr = Memory::seg_offset_to_linear(0x1000, 0x0000);
    for (i, &b) in src.iter().enumerate() {
        memory.write_u8(src_addr + i, b);
    }

    cpu.regs.set_ah(0x29);
    cpu.regs.set_al(al);
    cpu.regs.ds = 0x1000;
    cpu.regs.set_si(0x0000);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200);

    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();

    let fcb_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    let mut fcb = [0u8; 12];
    for (i, b) in fcb.iter_mut().enumerate() {
        *b = memory.read_u8(fcb_addr + i);
    }
    (cpu.regs.al(), cpu.regs.si(), fcb)
}

const FCB_POSITION_SEGMENT: u16 = 0x2000;
const FCB_POSITION_OFFSET: u16 = 0x0100;
const FCB_POSITION_DTA_SEGMENT: u16 = 0x3000;
const FCB_POSITION_RECORD_SIZE: u16 = 4;

fn fcb_position_records(count: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(count * usize::from(FCB_POSITION_RECORD_SIZE));
    for index in 0..count {
        let index = index as u16;
        let [low, high] = index.to_le_bytes();
        data.extend([low, high, !low, !high]);
    }
    data
}

fn install_fcb_position_file(
    dos: &mut Dos,
    memory: &mut Memory,
    file: Box<dyn crate::filesystem::VirtualFile>,
    file_size: u32,
    extended: bool,
) -> (usize, u16) {
    let fcb_addr = Memory::seg_offset_to_linear(FCB_POSITION_SEGMENT, FCB_POSITION_OFFSET);
    let body_addr = if extended {
        memory.write_u8(fcb_addr, 0xFF);
        for offset in 1..6 {
            memory.write_u8(fcb_addr + offset, 0);
        }
        memory.write_u8(fcb_addr + Dos::FCB_EXTENDED_ATTR, 0x20);
        fcb_addr + Dos::FCB_EXTENDED_PREFIX
    } else {
        fcb_addr
    };
    create_test_fcb_at(memory, body_addr, 3, "SEQFCB", "DAT");

    let handle = dos
        .file_table
        .allocate_with_sharing(
            file,
            String::from(r"C:\SEQFCB.DAT"),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READWRITE,
        )
        .expect("test SFT handle");
    dos.fcb_set_handle(memory, fcb_addr, handle);
    dos.fcb_set_record_size(memory, fcb_addr, FCB_POSITION_RECORD_SIZE);
    dos.fcb_set_file_size(memory, fcb_addr, file_size);
    dos.set_dta(FCB_POSITION_DTA_SEGMENT, 0);
    (fcb_addr, handle)
}

fn call_fcb_position_service(
    dos: &mut Dos,
    cpu: &mut gp64_cpu::Cpu,
    memory: &mut Memory,
    function: u8,
) {
    cpu.regs.set_ax(u16::from(function) << 8);
    cpu.regs.ds = FCB_POSITION_SEGMENT;
    cpu.regs.set_dx(FCB_POSITION_OFFSET);
    let (mut vga, mut keyboard) = create_test_io();
    dos.int21(cpu, memory, &mut vga, &mut keyboard, &mut Pic::new())
        .expect("INT 21h FCB service");
}

fn assert_fcb_backend_position(dos: &mut Dos, handle: u16, expected: u64) {
    assert_eq!(
        dos.file_table.get_sft(handle).expect("SFT entry").position,
        expected,
        "SFT position"
    );
    let backend_position = dos
        .file_table
        .get_sft_mut(handle)
        .expect("SFT entry")
        .file
        .stream_position()
        .expect("backend position");
    assert_eq!(backend_position, expected, "backend position");
}

fn dta_bytes(memory: &Memory, count: usize) -> Vec<u8> {
    let dta_addr = Memory::seg_offset_to_linear(FCB_POSITION_DTA_SEGMENT, 0);
    (0..count)
        .map(|offset| memory.read_u8(dta_addr + offset))
        .collect()
}

#[test]
fn fcb_sequential_read_repositions_after_random_read() {
    let records = fcb_position_records(130);
    let file_size = records.len() as u32;
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (fcb_addr, handle) = install_fcb_position_file(
        &mut dos,
        &mut memory,
        Box::new(CursorFile::new(records)),
        file_size,
        false,
    );
    dos.fcb_set_random_record(&mut memory, fcb_addr, 3);

    call_fcb_position_service(&mut dos, &mut cpu, &mut memory, 0x21);
    assert_eq!(cpu.regs.al(), 0x00, "random read succeeds");
    assert_eq!(dta_bytes(&memory, 4), [0x03, 0x00, 0xFC, 0xFF]);
    assert_fcb_backend_position(&mut dos, handle, 16);

    dos.fcb_set_current_block(&mut memory, fcb_addr, 1);
    dos.fcb_set_current_record(&mut memory, fcb_addr, 1);
    let dta_addr = Memory::seg_offset_to_linear(FCB_POSITION_DTA_SEGMENT, 0);
    for offset in 0..8 {
        memory.write_u8(dta_addr + offset, 0xA5);
    }
    call_fcb_position_service(&mut dos, &mut cpu, &mut memory, 0x14);

    assert_eq!(cpu.regs.al(), 0x00, "sequential read succeeds");
    assert_eq!(
        dta_bytes(&memory, 4),
        [0x81, 0x00, 0x7E, 0xFF],
        "block 1, record 1 is byte offset 516"
    );
    assert_eq!(
        dta_bytes(&memory, 8)[4..],
        [0xA5, 0xA5, 0xA5, 0xA5],
        "bytes beyond the record remain untouched"
    );
    assert_eq!(dos.fcb_get_current_block(&memory, fcb_addr), 1);
    assert_eq!(dos.fcb_get_current_record(&memory, fcb_addr), 2);
    assert_fcb_backend_position(&mut dos, handle, 520);
}

#[test]
fn fcb_sequential_read_rolls_record_127_into_next_block() {
    let records = fcb_position_records(130);
    let file_size = records.len() as u32;
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (fcb_addr, handle) = install_fcb_position_file(
        &mut dos,
        &mut memory,
        Box::new(CursorFile::new(records)),
        file_size,
        false,
    );
    dos.fcb_set_current_block(&mut memory, fcb_addr, 0);
    dos.fcb_set_current_record(&mut memory, fcb_addr, 127);

    call_fcb_position_service(&mut dos, &mut cpu, &mut memory, 0x14);

    assert_eq!(cpu.regs.al(), 0x00, "sequential read succeeds");
    assert_eq!(
        dta_bytes(&memory, 4),
        [0x7F, 0x00, 0x80, 0xFF],
        "record 127 is byte offset 508"
    );
    assert_eq!(dos.fcb_get_current_block(&memory, fcb_addr), 1);
    assert_eq!(dos.fcb_get_current_record(&memory, fcb_addr), 0);
    assert_fcb_backend_position(&mut dos, handle, 512);
}

#[test]
fn fcb_sequential_write_repositions_after_random_write() {
    let records = fcb_position_records(130);
    let file_size = records.len() as u32;
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (fcb_addr, handle) = install_fcb_position_file(
        &mut dos,
        &mut memory,
        Box::new(CursorFile::new(records)),
        file_size,
        true,
    );
    dos.fcb_set_random_record(&mut memory, fcb_addr, 3);
    let dta_addr = Memory::seg_offset_to_linear(FCB_POSITION_DTA_SEGMENT, 0);
    let random_data = [0xA1, 0xB2, 0xC3, 0xD4];
    for (offset, byte) in random_data.into_iter().enumerate() {
        memory.write_u8(dta_addr + offset, byte);
    }

    call_fcb_position_service(&mut dos, &mut cpu, &mut memory, 0x22);
    assert_eq!(cpu.regs.al(), 0x00, "random write succeeds");
    assert_fcb_backend_position(&mut dos, handle, 16);

    dos.fcb_set_current_block(&mut memory, fcb_addr, 1);
    dos.fcb_set_current_record(&mut memory, fcb_addr, 1);
    let sequential_data = [0x51, 0x62, 0x73, 0x84];
    for (offset, byte) in sequential_data.into_iter().enumerate() {
        memory.write_u8(dta_addr + offset, byte);
    }
    call_fcb_position_service(&mut dos, &mut cpu, &mut memory, 0x15);

    assert_eq!(cpu.regs.al(), 0x00, "sequential write succeeds");
    assert_eq!(dos.fcb_get_current_block(&memory, fcb_addr), 1);
    assert_eq!(dos.fcb_get_current_record(&memory, fcb_addr), 2);
    assert_fcb_backend_position(&mut dos, handle, 520);
    let extended_prefix = (0..Dos::FCB_EXTENDED_PREFIX)
        .map(|offset| memory.read_u8(fcb_addr + offset))
        .collect::<Vec<_>>();
    assert_eq!(extended_prefix, [0xFF, 0, 0, 0, 0, 0, 0x20]);

    let mut expected = fcb_position_records(130);
    expected[12..16].copy_from_slice(&random_data);
    expected[516..520].copy_from_slice(&sequential_data);
    let sft = dos.file_table.get_sft_mut(handle).expect("SFT entry");
    sft.file
        .seek(std::io::SeekFrom::Start(0))
        .expect("rewind memory file");
    let mut actual = vec![0; expected.len()];
    sft.file
        .read_exact(&mut actual)
        .expect("read back complete memory file");
    assert_eq!(
        actual, expected,
        "random and sequential writes land at their records"
    );
}

#[test]
fn fcb_sequential_read_uses_u64_for_the_maximum_position() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (fcb_addr, handle) = install_fcb_position_file(
        &mut dos,
        &mut memory,
        Box::new(CursorFile::new(Vec::new())),
        0,
        false,
    );
    dos.fcb_set_record_size(&mut memory, fcb_addr, u16::MAX);
    dos.fcb_set_current_block(&mut memory, fcb_addr, u16::MAX);
    dos.fcb_set_current_record(&mut memory, fcb_addr, 127);

    call_fcb_position_service(&mut dos, &mut cpu, &mut memory, 0x14);

    assert_eq!(cpu.regs.al(), 0x01, "empty file reports EOF");
    assert_fcb_backend_position(&mut dos, handle, 549_747_359_745);
    assert_eq!(dos.fcb_get_current_block(&memory, fcb_addr), u16::MAX);
    assert_eq!(dos.fcb_get_current_record(&memory, fcb_addr), 127);
}

struct FcbSeekFailureFile {
    reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    writes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl std::io::Read for FcbSeekFailureFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        use std::sync::atomic::Ordering;

        self.reads.fetch_add(1, Ordering::Relaxed);
        buffer.fill(0xC7);
        Ok(buffer.len())
    }
}

impl std::io::Write for FcbSeekFailureFile {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        use std::sync::atomic::Ordering;

        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Seek for FcbSeekFailureFile {
    fn seek(&mut self, _position: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::other("forced seek failure"))
    }
}

impl crate::filesystem::VirtualFile for FcbSeekFailureFile {
    fn set_len(&mut self, _size: u64) -> anyhow::Result<()> {
        Ok(())
    }

    fn sync_all(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[test]
fn fcb_sequential_read_seek_failure_skips_io_and_position_advance() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let reads = Arc::new(AtomicUsize::new(0));
    let writes = Arc::new(AtomicUsize::new(0));
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (fcb_addr, handle) = install_fcb_position_file(
        &mut dos,
        &mut memory,
        Box::new(FcbSeekFailureFile {
            reads: Arc::clone(&reads),
            writes,
        }),
        4,
        false,
    );
    dos.fcb_set_current_block(&mut memory, fcb_addr, 1);
    dos.fcb_set_current_record(&mut memory, fcb_addr, 1);
    let dta_addr = Memory::seg_offset_to_linear(FCB_POSITION_DTA_SEGMENT, 0);
    for offset in 0..8 {
        memory.write_u8(dta_addr + offset, 0xA5);
    }

    call_fcb_position_service(&mut dos, &mut cpu, &mut memory, 0x14);

    assert_eq!(cpu.regs.al(), 0x01, "seek failure reports an FCB error");
    assert_eq!(
        dos.extended_error.0, 30,
        "seek failure reports a read fault"
    );
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "failed seek must precede read"
    );
    assert_eq!(dta_bytes(&memory, 8), [0xA5; 8]);
    assert_eq!(dos.fcb_get_current_block(&memory, fcb_addr), 1);
    assert_eq!(dos.fcb_get_current_record(&memory, fcb_addr), 1);
    assert_eq!(dos.file_table.get_sft(handle).unwrap().position, 0);
}

#[test]
fn fcb_sequential_write_seek_failure_skips_io_and_position_advance() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let reads = Arc::new(AtomicUsize::new(0));
    let writes = Arc::new(AtomicUsize::new(0));
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (fcb_addr, handle) = install_fcb_position_file(
        &mut dos,
        &mut memory,
        Box::new(FcbSeekFailureFile {
            reads,
            writes: Arc::clone(&writes),
        }),
        4,
        false,
    );
    dos.fcb_set_current_block(&mut memory, fcb_addr, 1);
    dos.fcb_set_current_record(&mut memory, fcb_addr, 1);
    let dta_addr = Memory::seg_offset_to_linear(FCB_POSITION_DTA_SEGMENT, 0);
    for (offset, byte) in [0x51, 0x62, 0x73, 0x84].into_iter().enumerate() {
        memory.write_u8(dta_addr + offset, byte);
    }

    call_fcb_position_service(&mut dos, &mut cpu, &mut memory, 0x15);

    assert_eq!(cpu.regs.al(), 0x01, "seek failure reports an FCB error");
    assert_eq!(
        writes.load(Ordering::Relaxed),
        0,
        "failed seek must precede write"
    );
    assert_eq!(dos.fcb_get_current_block(&memory, fcb_addr), 1);
    assert_eq!(dos.fcb_get_current_record(&memory, fcb_addr), 1);
    assert_eq!(dos.file_table.get_sft(handle).unwrap().position, 0);
}
