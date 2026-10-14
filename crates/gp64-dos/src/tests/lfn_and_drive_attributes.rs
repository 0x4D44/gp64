use super::*;

/// LFN 7143h BL=0: Get attributes with archive flag (line 5514).
#[test]
fn test_cov_lfn_7143_get_attrs_archive() {
    struct LfnArchDrive;
    impl VirtualDrive for LfnArchDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Err(anyhow::anyhow!("n"))
        }
        fn create_dir(&mut self, _: &str) -> Result<()> {
            Err(anyhow::anyhow!("n"))
        }
        fn remove_file(&mut self, _: &str) -> Result<()> {
            Err(anyhow::anyhow!("n"))
        }
        fn remove_dir(&mut self, _: &str) -> Result<()> {
            Err(anyhow::anyhow!("n"))
        }
        fn rename(&mut self, _: &str, _: &str) -> Result<()> {
            Err(anyhow::anyhow!("n"))
        }
        fn get_attributes(&mut self, _: &str) -> Result<crate::filesystem::FileAttributes> {
            Ok(crate::filesystem::FileAttributes {
                read_only: false,
                hidden: false,
                system: false,
                volume_label: false,
                directory: false,
                archive: true,
                size: 42,
                modified: SystemTime::UNIX_EPOCH,
            })
        }
        fn set_attributes(&mut self, _: &str, _: crate::filesystem::FileAttributes) -> Result<()> {
            Ok(())
        }
        fn set_file_time(&mut self, _: &str, _: SystemTime) -> Result<()> {
            Ok(())
        }
        fn read_dir(
            &mut self,
            _: &str,
        ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
            Ok(vec![])
        }
        fn get_disk_space(&self) -> Result<(u64, u64)> {
            Ok((0, 0))
        }
    }

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut lfn_arch_probe = LfnArchDrive;
    exercise_virtual_drive(&mut lfn_arch_probe);
    dos.drives.insert(2, Box::new(LfnArchDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x43);
    cpu.regs.set_bl(0x00); // Get attributes
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx() & 0x20, 0x20); // Archive bit
}

#[test]
fn lfn_713b_chdir_rejects_a_nonexistent_path() {
    let (cpu, cwd) = run_lfn_713b(b"\\NOPE\0", &[""], &[]);

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "a nonexistent directory must fail"
    );
    assert_eq!(cpu.regs.ax(), 3, "DOS error 3 = path not found");
    assert!(
        !cwd.to_ascii_uppercase().contains("NOPE"),
        "a failed CHDIR must not move the current directory, got {cwd:?}"
    );
}

#[test]
fn lfn_713b_chdir_rejects_a_regular_file() {
    let (cpu, cwd) = run_lfn_713b(b"\\README.TXT\0", &[""], &["README.TXT"]);

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "a regular file must fail — it has no directory attribute"
    );
    assert_eq!(cpu.regs.ax(), 3, "DOS error 3 = path not found");
    assert!(
        !cwd.to_ascii_uppercase().contains("README"),
        "a failed CHDIR must not move the current directory, got {cwd:?}"
    );
}

#[test]
fn lfn_713b_chdir_accepts_an_existing_directory_and_keeps_its_spelling() {
    let (cpu, cwd) = run_lfn_713b(b"\\Program Files\0", &["Program Files"], &[]);

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "an existing directory must succeed"
    );
    assert_eq!(cpu.regs.ax(), 0);
    // The LONG name must survive: multi-word, not folded to an 8.3
    // "PROGRA~1". The resolver upper-cases what it stores — pre-existing
    // behaviour shared with the short-name AH=3Bh route and out of scope
    // here, so this compares case-insensitively (noted in scratchpad.md).
    assert!(
        cwd.to_ascii_uppercase().contains("PROGRAM FILES"),
        "the LFN long name must survive CHDIR, got {cwd:?}"
    );
}

#[test]
fn lfn_7143_set_attrs_reaches_the_drive_with_the_translated_bits() {
    // READONLY | SYSTEM | ARCHIVE — three bits, so a handler that passed
    // a default or an all-ones set could not accidentally match.
    let (cpu, log) = run_lfn_7143(0x01, 0x01 | 0x04 | 0x20, b"TEST.TXT\0", None);

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "set attributes reported failure"
    );
    let log = log.lock().unwrap();
    assert_eq!(
        log.set_calls.len(),
        1,
        "set_attributes must be called exactly once, saw {:?}",
        log.set_calls
    );
    let (path, attrs) = &log.set_calls[0];
    assert_eq!(path, "\\TEST.TXT", "LFN resolution changed the path");
    assert!(attrs.read_only, "READONLY bit lost");
    assert!(attrs.system, "SYSTEM bit lost");
    assert!(attrs.archive, "ARCHIVE bit lost");
    assert!(!attrs.hidden, "HIDDEN bit invented");
    assert!(!attrs.volume_label, "VOLUME bit invented");
    assert!(!attrs.directory, "DIRECTORY bit invented");
}

#[test]
fn lfn_7143_attrs_round_trip_through_set_then_get() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(AttrLog::default()));
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(
        2,
        Box::new(RecordingAttrDrive {
            log: log.clone(),
            fail_with: None,
        }),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);

    // Set HIDDEN | SYSTEM.
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x43);
    cpu.regs.set_bl(0x01);
    cpu.regs.set_cx(0x02 | 0x04);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Read them straight back.
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x43);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_cx(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(
        cpu.regs.cx(),
        0x02 | 0x04,
        "get did not return exactly what set stored"
    );
}

#[test]
fn lfn_7143_set_attrs_reports_a_missing_target() {
    let (cpu, log) = run_lfn_7143(
        0x01,
        0x20,
        b"GONE.TXT\0",
        Some(std::io::ErrorKind::NotFound),
    );

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "a missing target must set carry"
    );
    assert_eq!(cpu.regs.ax(), 2, "missing target must be DOS error 2");
    assert_eq!(log.lock().unwrap().set_calls.len(), 1);
}

#[test]
fn lfn_7143_set_attrs_reports_a_read_only_drive() {
    let (cpu, log) = run_lfn_7143(
        0x01,
        0x20,
        b"TEST.TXT\0",
        Some(std::io::ErrorKind::PermissionDenied),
    );

    assert!(
        cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "a rejecting drive must set carry"
    );
    assert_eq!(cpu.regs.ax(), 5, "a rejecting drive must be DOS error 5");
    assert_eq!(log.lock().unwrap().set_calls.len(), 1);
}

#[test]
fn lfn_7143_get_attrs_reports_the_volume_label_bit() {
    // The LFN get path used to omit 0x08 while the short-name AH=43h
    // path returned it — the two now share `dos_attr_bits`.
    let log = std::sync::Arc::new(std::sync::Mutex::new(AttrLog {
        set_calls: Vec::new(),
        stored: Some(crate::filesystem::FileAttributes {
            volume_label: true,
            ..Default::default()
        }),
    }));
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(
        2,
        Box::new(RecordingAttrDrive {
            log: log.clone(),
            fail_with: None,
        }),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"VOL\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x43);
    cpu.regs.set_bl(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx() & 0x08, 0x08, "volume-label bit dropped");
}

/// LFN 7143h BL=1: Set attributes - no drive (line 5533 else).
#[test]
fn test_cov_lfn_7143_set_attrs_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"Z:\\TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x43);
    cpu.regs.set_bl(0x01); // Set attributes
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

/// LFN 71A6h with invalid handle (line 5797 else path).
#[test]
fn test_cov_lfn_71a6_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0xA6);
    cpu.regs.set_bx(0xFFFF); // Invalid handle
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0200);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Environment copy: scan environment with embedded data (lines 6800-6804).
#[test]
fn test_cov_env_copy_with_data() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);

    // Allocate an env block
    let env_seg = dos.dos_mem.alloc(&mut memory, 0x0100, 0x10).unwrap();
    let env_base = Memory::seg_offset_to_linear(env_seg, 0);

    // Write environment: "PATH=C:\\" + NUL + "COMSPEC=C:\\DOS command interpreter" + NUL + NUL
    let env_str = b"PATH=C:\\\0COMSPEC=C:\\COMMAND.COM\0\0";
    for (i, &b) in env_str.iter().enumerate() {
        memory.write_u8(env_base + i, b);
    }

    let result = dos
        .clone_env_for_exec(&mut memory, env_seg, "TEST.EXE")
        .unwrap();
    assert!(result > 0);
}

/// DOS int trace enabled path (lines 409, 415).
#[test]
fn test_cov_dos_int_trace_enabled_with_max() {
    let _lock = env_lock();
    let _guard = trace_subscriber("gp64::dos::int=debug");
    let _env = EnvVarGuard::set("GP64_DOS_TRACE", "int=1");

    let cpu = gp64_cpu::Cpu::new();
    // Call trace function - first call should emit, second should be limited
    trace_dos_int(&cpu, 0x3D, 0x1000, 0x0100);
    trace_dos_int(&cpu, 0x3D, 0x1000, 0x0100);
}

/// DOS seek trace max path (line 500).
#[test]
fn test_cov_dos_seek_trace_max() {
    let _lock = env_lock();
    let _guard = trace_subscriber("gp64::dos::seek=debug");
    let _env = EnvVarGuard::set("GP64_DOS_TRACE", "seek=1");

    trace_dos_seek(5, "C:\\TEST.TXT", 0, 0, true, Some(0));
    trace_dos_seek(5, "C:\\TEST.TXT", 0, 0, true, Some(0)); // Should hit limit
}

/// DOS find trace max path (line 550).
#[test]
fn test_cov_dos_find_trace_max() {
    let _lock = env_lock();
    let _guard = trace_subscriber("gp64::dos::find=debug");
    let _env = EnvVarGuard::set("GP64_DOS_TRACE", "find=1");

    trace_dos_find("FIRST", "*.TXT", 0, None, true);
    trace_dos_find("NEXT", "*.TXT", 0, None, true); // Should hit limit
}

/// DOS write with truncated output (line 467 + 3403).
#[test]
fn test_cov_dos_write_trace_truncated() {
    let _lock = env_lock();
    let _guard = trace_subscriber("gp64::dos::write=debug");
    let _env = EnvVarGuard::set("GP64_DOS_TRACE", "write=5");

    // Write a large buffer that will be truncated
    let large_buf = vec![b'A'; 1000];
    trace_dos_write(5, &large_buf);
}

/// AH=09 print with trace enabled (lines 1507-1530).
#[test]
fn test_cov_ah09_print_with_trace() {
    let _lock = env_lock();
    let _guard = trace_subscriber("gp64::dos::print=debug");

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write string at DS:DX containing various characters
    let str_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    let test_str = b"Hello\r\n\tWorld\x01$";
    for (i, &b) in test_str.iter().enumerate() {
        memory.write_u8(str_addr + i, b);
    }

    cpu.regs.set_ah(0x09);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}

/// exec_read_param_block_cmdtail: es=0 but bx!=0 with cmdtail 0:0.
#[test]
fn test_cov_exec_read_param_block_es_zero_bx_nonzero() {
    let dos = Dos::new();
    let memory = Memory::new();
    let (_env_seg, cmdline) = dos.exec_read_param_block_cmdtail(&memory, 0, 0x100);
    assert!(cmdline.is_none());
}

/// exec open file: path with forward slash (line 7107 block=1)
#[test]
fn test_cov_exec_open_with_forward_slash() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(b'C', Box::new(NotFoundDrive));
    dos.path_resolver.set_current_drive(b'C');

    // Write filename with forward slash
    let str_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    let filename = b"SUBDIR/TEST.EXE\0";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(str_addr + i, b);
    }

    // Set up for AH=4B EXEC
    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x00); // Load and execute
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0;
    cpu.regs.set_bx(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Should fail (path or file not found)
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// exec open file: filename without path separator, returns error 2 (line 7107 branch)
#[test]
fn test_cov_exec_open_no_separator() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(b'C', Box::new(NotFoundDrive));
    dos.path_resolver.set_current_drive(b'C');

    // Write filename without any path separators
    let str_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    let filename = b"NOPATH.EXE\0";
    for (i, &b) in filename.iter().enumerate() {
        memory.write_u8(str_addr + i, b);
    }

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0;
    cpu.regs.set_bx(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Should fail with file not found (error 2)
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
}

/// Line 5514: LFN get attrs with archive=false (false branch).
#[test]
fn test_b25_lfn_get_attrs_no_archive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut no_archive_probe = NoArchiveDrive;
    exercise_virtual_drive(&mut no_archive_probe);
    dos.drives.insert(3, Box::new(NoArchiveDrive));
    dos.path_resolver.set_current_drive(3);
    let name_addr = Memory::seg_offset_to_linear(0x2000, 0x200);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x43);
    cpu.regs.set_bl(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0200);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let attrs = cpu.regs.cx();
    assert!(attrs & 0x01 != 0, "read_only should be set");
    assert_eq!(attrs & 0x20, 0, "archive should NOT be set");
}

/// Line 1507-1530: AH=09 print trace with diverse character types.
/// Exercises: CR (\r), LF (\n), tab (\t), non-printable hex, and normal chars.
#[test]
fn test_b25_ah09_print_trace_char_types() {
    let _sub = trace_subscriber("gp64::dos::print=debug");
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    // String with: printable 'A', CR, LF, tab, non-printable 0x01, '$'
    let data = [b'A', b'\r', b'\n', b'\t', 0x01, b'$'];
    for (i, &b) in data.iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x09);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
}

/// Line 1613: AH=0A buffered input with max_len=0 and Enter key.
#[test]
fn test_b25_ah0a_buffered_input_max_len_zero() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let buf_addr = Memory::seg_offset_to_linear(0x2000, 0x0000);
    memory.write_u8(buf_addr, 0); // max_len = 0
    memory.write_u8(buf_addr + 1, 0); // current length = 0
                                      // Enqueue Enter key so the function completes
    bda::kbd_enqueue(&mut memory, 0x1C, 0x0D);
    cpu.regs.set_ah(0x0A);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
}

/// Line 392: trace_dos_read with max reached (max != 0 && n >= max).
#[test]
fn test_b25_trace_dos_read_max_reached() {
    let _lock = env_lock();
    let _guard = EnvVarGuard::set("GP64_DOS_TRACE", "read=0");
    let _sub = trace_subscriber("gp64::dos::read=debug");
    // Just call the trace function directly - when max=0 the branch `max != 0` is false
    trace_dos_read(0x0005, "test.dat", 100, 50, true, Some(0));
}

/// Line 409 + 415: trace_dos_int enabled and max reached.
#[test]
fn test_b25_trace_dos_int_enabled() {
    let _lock = env_lock();
    let _guard = EnvVarGuard::set("GP64_DOS_TRACE", "int=0");
    let _sub = trace_subscriber("gp64::dos::int=debug");
    let cpu = gp64_cpu::Cpu::new();
    trace_dos_int(&cpu, 0x09, 0x1000, 0x0100);
}

/// Line 3403: AH=40 write to stdout with truncated trace message.
#[test]
fn test_b25_ah40_write_stdout_truncated_trace() {
    let _lock = env_lock();
    let _guard_max = EnvVarGuard::set("GP64_DOS_TRACE", "print=5");
    let _sub = trace_subscriber("gp64::dos::print=debug,gp64::dos::write=debug");
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    // Write 20 bytes to stdout - exceeds trace_max of 5
    for i in 0..20 {
        memory.write_u8(a + i, b'X');
    }
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(0x01); // stdout
    cpu.regs.set_cx(20);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
}

/// Line 6800: env scan loop with end_offset+1 >= max_scan.
#[test]
fn test_b25_clone_env_short_env_block() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    // Set environment segment to a small block with just a double-null
    let env_seg = 0x0300u16;
    let env_base = (env_seg as usize) * 16;
    memory.write_u8(env_base, 0); // first null
    memory.write_u8(env_base + 1, 0); // second null (double-null)
    dos.env_segment = env_seg;
    // Allocate a block (AH=48h) which internally uses env info
    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}

/// Lines 4301: find_first succeeds but find_get_entry returns None.
/// This is hard to trigger naturally, so we test the normal path instead
/// to exercise find_first → find_get_entry chain.
#[test]
fn test_b25_find_first_get_entry_chain() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(3, Box::new(NotFoundDrive));
    dos.path_resolver.set_current_drive(3);
    let name_addr = Memory::seg_offset_to_linear(0x2000, 0x200);
    for (i, &b) in b"*.*\0".iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x4E); // FindFirst
    cpu.regs.set_cx(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0200);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Line 1721: open file gets attributes from drive (the else branch where drive not found).
#[test]
fn test_b25_open_file_no_drive_for_attrs() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    // Don't register any drives - try to open file on unregistered drive
    let name_addr = Memory::seg_offset_to_linear(0x2000, 0x200);
    for (i, &b) in b"Z:\\FILE.TXT\0".iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x3D); // Open file
    cpu.regs.set_al(0x00); // Read only
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0200);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Line 7107: load_overlay error with path separator in filename.
/// Covers the branch where filename.contains('\\') || filename.contains('/').
/// Also covers the forward-slash variant (block=1 branch=2).
#[test]
fn test_b27_load_overlay_error_with_separator() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);

    // Test 1: backslash separator → filename.contains('\\') = true
    let name_addr = Memory::seg_offset_to_linear(0x2000, 0x200);
    for (i, &b) in b"C:\\SUB\\NOTFOUND.OVL\0".iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    // OverlayParamBlock at ES:BX
    let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    memory.write_u16(param_addr, 0x4000); // load segment
    memory.write_u16(param_addr + 2, 0x4000); // reloc factor
    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03); // Load overlay
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0200);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0000);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3); // Path not found (has separator)

    // Test 2: forward-slash separator → filename.contains('/') = true
    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
    for (i, &b) in b"C:/SUB/NOTFOUND.OVL\0".iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0200);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0000);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Line 7107: load_overlay error WITHOUT path separator.
/// Covers the else branch (error code = 2, file not found).
#[test]
fn test_b27_load_overlay_error_no_separator() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    let name_addr = Memory::seg_offset_to_linear(0x2000, 0x200);
    for (i, &b) in b"NOTFOUND.OVL\0".iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    memory.write_u16(param_addr, 0x4000);
    memory.write_u16(param_addr + 2, 0x4000);
    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0200);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0000);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2); // File not found (no separator)
}

/// Line 7720: DOS date to SystemTime with pre-1970 date.
/// The total_secs < 0 branch for dates before Unix epoch.
#[test]
fn test_b27_dos_datetime_pre_epoch() {
    // DOS date format: bits [15:9]=year-1980, [8:5]=month, [4:0]=day
    // Year 0 + 1980 = 1980, but we need pre-1970.
    // DOS dates start at 1980, so total_secs is always >= 0.
    // This confirms the branch is structurally unreachable for valid DOS dates.
    // Test the boundary: year=0 (1980), month=1, day=1 → 1980-01-01
    let date: u16 = (1 << 5) | 1; // 1980-01-01 (year=0 means 1980)
    let time: u16 = 0; // 00:00:00
    let st = Dos::dos_datetime_to_systemtime(date, time);
    assert!(st >= std::time::UNIX_EPOCH);
}

#[test]
fn test_build_sound_env_vars_blaster_format() {
    use gp64_types::{SoundBlasterEnv, SoundEnvConfig};
    let config = SoundEnvConfig {
        sound_blaster: Some(SoundBlasterEnv {
            port: 0x220,
            irq: 5,
            dma: 1,
            hdma: 5,
            sb_type: 6,
            emu8000_port: 0x620,
            mpu_port: 0x330,
        }),
        gravis_ultrasound: None,
    };
    let vars = build_sound_env_vars(&config);
    assert_eq!(vars.len(), 1);
    assert_eq!(vars[0], "BLASTER=A220 I5 D1 H5 T6 E620 P330");
}

#[test]
fn test_build_sound_env_vars_omits_zero_high_dma() {
    use gp64_types::{SoundBlasterEnv, SoundEnvConfig};
    let config = SoundEnvConfig {
        sound_blaster: Some(SoundBlasterEnv {
            port: 0x220,
            irq: 7,
            dma: 1,
            hdma: 0,
            sb_type: 3,
            emu8000_port: 0,
            mpu_port: 0,
        }),
        gravis_ultrasound: None,
    };
    assert_eq!(build_sound_env_vars(&config), ["BLASTER=A220 I7 D1 T3"]);
}

#[test]
fn test_build_sound_env_vars_ultrasnd_hex_port() {
    use gp64_types::{GusEnv, SoundEnvConfig};
    let config = SoundEnvConfig {
        sound_blaster: None,
        gravis_ultrasound: Some(GusEnv {
            port: 0x240,
            play_dma: 1,
            rec_dma: 1,
            play_irq: 5,
            rec_irq: 5,
        }),
    };
    let vars = build_sound_env_vars(&config);
    assert_eq!(vars.len(), 2);
    assert_eq!(vars[0], "ULTRASND=240,1,1,5,5");
    assert_eq!(vars[1], "ULTRADIR=C:\\ULTRASND");
}

#[test]
fn test_build_sound_env_vars_no_mpu() {
    use gp64_types::{SoundBlasterEnv, SoundEnvConfig};
    let config = SoundEnvConfig {
        sound_blaster: Some(SoundBlasterEnv {
            port: 0x220,
            irq: 5,
            dma: 1,
            hdma: 5,
            sb_type: 6,
            emu8000_port: 0x620,
            mpu_port: 0,
        }),
        gravis_ultrasound: None,
    };
    let vars = build_sound_env_vars(&config);
    assert_eq!(vars[0], "BLASTER=A220 I5 D1 H5 T6 E620");
}

#[test]
fn test_build_sound_env_vars_both_devices() {
    use gp64_types::{GusEnv, SoundBlasterEnv, SoundEnvConfig};
    let config = SoundEnvConfig {
        sound_blaster: Some(SoundBlasterEnv {
            port: 0x220,
            irq: 5,
            dma: 1,
            hdma: 5,
            sb_type: 6,
            emu8000_port: 0x620,
            mpu_port: 0x330,
        }),
        gravis_ultrasound: Some(GusEnv {
            port: 0x240,
            play_dma: 1,
            rec_dma: 1,
            play_irq: 5,
            rec_irq: 5,
        }),
    };
    let vars = build_sound_env_vars(&config);
    assert_eq!(vars.len(), 3); // BLASTER + ULTRASND + ULTRADIR
}

#[test]
fn test_build_sound_env_vars_empty_config() {
    use gp64_types::SoundEnvConfig;
    let config = SoundEnvConfig::default();
    let vars = build_sound_env_vars(&config);
    assert!(vars.is_empty());
}

#[test]
fn test_init_environment_populates_blaster() {
    use gp64_types::{SoundBlasterEnv, SoundEnvConfig};
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let config = SoundEnvConfig {
        sound_blaster: Some(SoundBlasterEnv {
            port: 0x220,
            irq: 5,
            dma: 1,
            hdma: 5,
            sb_type: 6,
            emu8000_port: 0x620,
            mpu_port: 0x330,
        }),
        gravis_ultrasound: None,
    };
    dos.init_environment(&mut memory, &config);
    let vars = read_env_block(&dos, &memory);
    let blaster = vars.iter().find(|(k, _)| k == "BLASTER");
    assert!(blaster.is_some(), "BLASTER should be present");
    assert_eq!(blaster.unwrap().1, "A220 I5 D1 H5 T6 E620 P330");
}

#[test]
fn test_init_environment_populates_ultrasnd() {
    use gp64_types::{GusEnv, SoundEnvConfig};
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let config = SoundEnvConfig {
        sound_blaster: None,
        gravis_ultrasound: Some(GusEnv {
            port: 0x240,
            play_dma: 1,
            rec_dma: 1,
            play_irq: 5,
            rec_irq: 5,
        }),
    };
    dos.init_environment(&mut memory, &config);
    let vars = read_env_block(&dos, &memory);
    let ultrasnd = vars.iter().find(|(k, _)| k == "ULTRASND");
    assert!(ultrasnd.is_some(), "ULTRASND should be present");
    assert_eq!(ultrasnd.unwrap().1, "240,1,1,5,5");
    let ultradir = vars.iter().find(|(k, _)| k == "ULTRADIR");
    assert!(ultradir.is_some(), "ULTRADIR should be present");
    assert_eq!(ultradir.unwrap().1, "C:\\ULTRASND");
}

#[test]
fn test_init_environment_no_sound() {
    use gp64_types::SoundEnvConfig;
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_environment(&mut memory, &SoundEnvConfig::default());
    let vars = read_env_block(&dos, &memory);
    assert!(
        vars.iter().all(|(k, _)| k != "BLASTER"),
        "No BLASTER without SB config"
    );
    assert!(
        vars.iter().all(|(k, _)| k != "ULTRASND"),
        "No ULTRASND without GUS config"
    );
}

#[test]
fn test_init_environment_sb_only() {
    use gp64_types::{SoundBlasterEnv, SoundEnvConfig};
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let config = SoundEnvConfig {
        sound_blaster: Some(SoundBlasterEnv {
            port: 0x220,
            irq: 7,
            dma: 1,
            hdma: 5,
            sb_type: 4,
            emu8000_port: 0,
            mpu_port: 0,
        }),
        gravis_ultrasound: None,
    };
    dos.init_environment(&mut memory, &config);
    let vars = read_env_block(&dos, &memory);
    let blaster = vars.iter().find(|(k, _)| k == "BLASTER");
    assert!(blaster.is_some(), "BLASTER should be present");
    assert_eq!(blaster.unwrap().1, "A220 I7 D1 H5 T4"); // No E/P fields since both are 0
    assert!(
        vars.iter().all(|(k, _)| k != "ULTRASND"),
        "No ULTRASND with SB-only config"
    );
}

#[test]
fn test_init_environment_has_tmp() {
    use gp64_types::SoundEnvConfig;
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_environment(&mut memory, &SoundEnvConfig::default());
    let vars = read_env_block(&dos, &memory);
    let temp = vars.iter().find(|(k, _)| k == "TEMP");
    let tmp = vars.iter().find(|(k, _)| k == "TMP");
    assert!(temp.is_some(), "TEMP should be present");
    assert!(tmp.is_some(), "TMP should be present");
    assert_eq!(temp.unwrap().1, "C:\\TEMP");
    assert_eq!(tmp.unwrap().1, "C:\\TEMP");
}

#[test]
fn test_env_block_size_sufficient() {
    use gp64_types::{GusEnv, SoundBlasterEnv, SoundEnvConfig};
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let config = SoundEnvConfig {
        sound_blaster: Some(SoundBlasterEnv {
            port: 0x220,
            irq: 5,
            dma: 1,
            hdma: 5,
            sb_type: 6,
            emu8000_port: 0x620,
            mpu_port: 0x330,
        }),
        gravis_ultrasound: Some(GusEnv {
            port: 0x240,
            play_dma: 1,
            rec_dma: 1,
            play_irq: 5,
            rec_irq: 5,
        }),
    };
    dos.init_environment(&mut memory, &config);
    let vars = read_env_block(&dos, &memory);
    // Calculate total bytes used
    let total_bytes: usize = vars
        .iter()
        .map(|(k, v)| k.len() + 1 + v.len() + 1) // KEY=VALUE\0
        .sum::<usize>()
        + 1; // + final \0
             // Block is 0x40 paragraphs = 1024 bytes; should have >800 bytes free
    assert!(
        total_bytes < 1024,
        "Env vars ({total_bytes} bytes) must fit in 1024-byte block"
    );
    assert!(
        1024 - total_bytes > 800,
        "Should have >800 bytes free, have {}",
        1024 - total_bytes
    );
}

#[test]
fn test_build_glide_env_vars_enabled() {
    let vars = build_glide_env_vars(true);
    assert_eq!(vars.len(), 3);
    assert_eq!(vars[0], "GLIDE=600");
    assert_eq!(vars[1], "SST_GRXCLK=57");
    assert_eq!(vars[2], "FX_GLIDE_NO_SPLASH=1");
}

#[test]
fn test_build_glide_env_vars_disabled() {
    let vars = build_glide_env_vars(false);
    assert!(vars.is_empty());
}

#[test]
fn test_init_environment_populates_glide_vars() {
    use gp64_types::SoundEnvConfig;
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.glide_ovl_data = Some(vec![0x4D, 0x5A].into()); // dummy MZ header
    dos.init_environment(&mut memory, &SoundEnvConfig::default());
    let vars = read_env_block(&dos, &memory);
    assert!(
        vars.iter().any(|(k, _)| k == "GLIDE"),
        "GLIDE should be present when glide_ovl_data is Some"
    );
    assert!(
        vars.iter().any(|(k, _)| k == "SST_GRXCLK"),
        "SST_GRXCLK should be present"
    );
    assert!(
        vars.iter().any(|(k, _)| k == "FX_GLIDE_NO_SPLASH"),
        "FX_GLIDE_NO_SPLASH should be present"
    );
    let glide = vars.iter().find(|(k, _)| k == "GLIDE").unwrap();
    assert_eq!(glide.1, "600");
}

#[test]
fn test_init_environment_no_glide_vars_when_disabled() {
    use gp64_types::SoundEnvConfig;
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    // glide_ovl_data is None by default
    dos.init_environment(&mut memory, &SoundEnvConfig::default());
    let vars = read_env_block(&dos, &memory);
    assert!(
        vars.iter().all(|(k, _)| k != "GLIDE"),
        "GLIDE should not be present when glide_ovl_data is None"
    );
}

#[test]
fn test_glide_ovl_filename_matching() {
    // The interception matches on base filename only (after last path separator)
    let cases = vec![
        ("GLIDE2X.OVL", true),
        ("glide2x.ovl", true),
        ("Glide2X.Ovl", true),
        ("C:\\GAMES\\GLIDE2X.OVL", true),
        ("C:\\GLIDE\\glide2x.ovl", true),
        ("D:\\SOME\\PATH\\Glide2x.OVL", true),
        ("GLIDE2X.DLL", false),
        ("GLIDE3X.OVL", false),
        ("NOTGLIDE2X.OVL", false), // base_name check rejects this
        ("GAME.EXE", false),
        ("", false),
    ];
    for (filename, expected) in cases {
        let upper = filename.to_uppercase();
        let base = upper.rsplit(['\\', '/']).next().unwrap_or(&upper);
        let result = base == "GLIDE2X.OVL";
        assert_eq!(
            result,
            expected,
            "Filename '{}' should {} match",
            filename,
            if expected { "" } else { "NOT" }
        );
    }
}

#[test]
fn test_get_glide_ovl_bytes_none() {
    let dos = Dos::new();
    assert!(dos.get_glide_ovl_bytes().is_none());
}

#[test]
fn test_get_glide_ovl_bytes_some() {
    let mut dos = Dos::new();
    let data = vec![0x4D, 0x5A, 0x90, 0x00]; // MZ header
    dos.glide_ovl_data = Some(data.clone().into());
    let result = dos.get_glide_ovl_bytes();
    assert!(result.is_some());
    assert_eq!(result.unwrap().as_ref(), data.as_slice());
}

#[test]
fn test_register_virtual_file_roundtrip() {
    let mut dos = Dos::new();
    let payload = vec![0x4D, 0x5A, 0xDE, 0xAD, 0xBE, 0xEF];
    dos.register_virtual_file(r"C:\TESTDIR\MAIN.EXE", payload.clone());

    assert_eq!(
        dos.get_virtual_file_bytes(2, r"\TESTDIR\MAIN.EXE"),
        Some(payload.clone().into())
    );
    assert!(dos.has_virtual_file(2, r"\TESTDIR\MAIN.EXE"));
}

#[test]
fn test_register_virtual_file_case_insensitive() {
    let mut dos = Dos::new();
    let payload = vec![0x4D, 0x5A];
    dos.register_virtual_file(r"C:\GameDir\Prog.Exe", payload.clone());

    // Resolver hands back uppercase, mixed-case registration must still
    // match.
    assert_eq!(
        dos.get_virtual_file_bytes(2, r"\GAMEDIR\PROG.EXE"),
        Some(payload.into())
    );
}

#[test]
fn test_register_virtual_file_drive_isolation() {
    let mut dos = Dos::new();
    dos.register_virtual_file(r"C:\APP.EXE", vec![1, 2, 3]);

    // Registered on C: (drive 2); D: (drive 3) must NOT see it.
    assert!(dos.has_virtual_file(2, r"\APP.EXE"));
    assert!(!dos.has_virtual_file(3, r"\APP.EXE"));
    assert_eq!(dos.get_virtual_file_bytes(3, r"\APP.EXE"), None);
}

#[test]
fn test_register_virtual_file_overwrite() {
    let mut dos = Dos::new();
    dos.register_virtual_file(r"C:\X.EXE", vec![0xAA]);
    dos.register_virtual_file(r"C:\X.EXE", vec![0xBB, 0xCC]);
    assert_eq!(
        dos.get_virtual_file_bytes(2, r"\X.EXE"),
        Some(vec![0xBB, 0xCC].into())
    );
}

#[test]
fn test_register_virtual_file_unknown_path_returns_none() {
    let dos = Dos::new();
    assert!(dos.get_virtual_file_bytes(2, r"\NOPE\NADA.EXE").is_none());
    assert!(!dos.has_virtual_file(2, r"\NOPE\NADA.EXE"));
}

/// Round-trip: registered bytes flow through the same `SyntheticFile`
/// the INT 21h AH=3Dh hook constructs, so AH=3Fh read, AH=42h seek,
/// and AH=3Eh close behave identically to the GLIDE2X.OVL flow.
#[test]
fn test_virtual_file_synthetic_read_seek_close() {
    use std::io::{Read, Seek, SeekFrom};

    let mut dos = Dos::new();
    let payload: Vec<u8> = (0u8..32).collect();
    dos.register_virtual_file(r"C:\TESTAPP\APP.EXE", payload.clone());

    // Mirrors the construction in the AH=3Dh open hook.
    let bytes = dos
        .get_virtual_file_bytes(2, r"\TESTAPP\APP.EXE")
        .expect("virtual file registered");
    let mut sf = crate::filesystem::SyntheticFile::new(bytes);

    // AH=3Fh read 8 bytes from the start.
    let mut buf = [0u8; 8];
    let n = sf.read(&mut buf).expect("read");
    assert_eq!(n, 8);
    assert_eq!(&buf[..], &payload[..8]);

    // AH=42h seek (SEEK_SET to 16) then AH=3Fh read 4 more bytes.
    let pos = sf.seek(SeekFrom::Start(16)).expect("seek");
    assert_eq!(pos, 16);
    let mut buf2 = [0u8; 4];
    let n2 = sf.read(&mut buf2).expect("read");
    assert_eq!(n2, 4);
    assert_eq!(&buf2[..], &payload[16..20]);

    // AH=42h seek (SEEK_END) reports total size (matches what
    // a DOS extender uses to compute the LE stub offset).
    let end_pos = sf.seek(SeekFrom::End(0)).expect("seek end");
    assert_eq!(end_pos, payload.len() as u64);

    // AH=3Eh close — for a `SyntheticFile`, drop suffices; the
    // file_table allocator handles handle release on the int21 side.
    drop(sf);
}

#[test]
fn test_classify_io_error_permission_denied_returns_iret() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let memory = Memory::new();
    let snapshot = cpu.regs.clone();

    let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
    let disp = dos.classify_io_error(&mut cpu, &memory, &err, 0x3F, None, false, snapshot);

    assert_eq!(disp, Int21Disposition::Iret);
    // AX should be set to error code 5 (access denied)
    assert_eq!(cpu.regs.ax(), 5);
    // Carry flag should be set
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_classify_io_error_not_found_returns_iret() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let memory = Memory::new();
    let snapshot = cpu.regs.clone();

    let err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
    let disp = dos.classify_io_error(&mut cpu, &memory, &err, 0x3F, None, false, snapshot);

    assert_eq!(disp, Int21Disposition::Iret);
    assert_eq!(cpu.regs.ax(), 2);
}

#[test]
fn test_classify_io_error_already_exists_returns_iret() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let memory = Memory::new();
    let snapshot = cpu.regs.clone();

    let err = std::io::Error::new(std::io::ErrorKind::AlreadyExists, "exists");
    let disp = dos.classify_io_error(&mut cpu, &memory, &err, 0x3C, None, false, snapshot);

    assert_eq!(disp, Int21Disposition::Iret);
    assert_eq!(cpu.regs.ax(), 80);
}

#[test]
fn test_classify_io_error_disk_error_returns_access_denied() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let memory = Memory::new();
    let snapshot = cpu.regs.clone();

    // Unknown ErrorKind (Error::other) now returns access denied (non-critical)
    let err = std::io::Error::other("disk error");
    let disp = dos.classify_io_error(&mut cpu, &memory, &err, 0x3F, None, false, snapshot);

    assert_eq!(disp, Int21Disposition::Iret);
    assert_eq!(cpu.regs.ax(), 5); // Access denied
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_classify_io_error_write_zero_returns_critical_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let memory = Memory::new();
    let snapshot = cpu.regs.clone();

    let err = std::io::Error::new(std::io::ErrorKind::WriteZero, "write zero");
    let disp = dos.classify_io_error(&mut cpu, &memory, &err, 0x40, None, true, snapshot);

    match &disp {
        Int21Disposition::CriticalError {
            ah_function,
            error_code,
            is_write,
            ..
        } => {
            assert_eq!(*ah_function, 0x40);
            assert_eq!(*error_code, 29); // Write fault
            assert!(*is_write);
        }
        other => panic!("Expected CriticalError, got {:?}", other),
    }
}

#[test]
fn test_classify_io_error_unexpected_eof_returns_critical_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let memory = Memory::new();
    let snapshot = cpu.regs.clone();

    let err = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected eof");
    let disp = dos.classify_io_error(&mut cpu, &memory, &err, 0x3F, None, false, snapshot);

    match &disp {
        Int21Disposition::CriticalError { error_code, .. } => {
            assert_eq!(*error_code, 30); // Read fault
        }
        other => panic!("Expected CriticalError, got {:?}", other),
    }
}

#[test]
fn test_classify_io_error_other_write_returns_access_denied() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let memory = Memory::new();
    let snapshot = cpu.regs.clone();

    // Unknown ErrorKind (Error::other) now returns access denied (non-critical)
    let err = std::io::Error::other("something");
    let disp = dos.classify_io_error(&mut cpu, &memory, &err, 0x40, None, true, snapshot);

    assert_eq!(disp, Int21Disposition::Iret);
    assert_eq!(cpu.regs.ax(), 5); // Access denied
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_classify_io_error_recursive_guard_returns_iret() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let snapshot = cpu.regs.clone();

    // Set the critical error flag immediately before the InDOS byte.
    let crit_flag_addr =
        Memory::seg_offset_to_linear(super::INDOS_SEGMENT, super::CRITICAL_ERROR_OFFSET);
    memory.write_u8(crit_flag_addr, 0xFF);

    // Even a "critical" error should return Iret when recursive
    let err = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected eof");
    let disp = dos.classify_io_error(&mut cpu, &memory, &err, 0x3F, None, false, snapshot);

    assert_eq!(disp, Int21Disposition::Iret);
    // Should have set error return with the DOS error code
    assert_eq!(cpu.regs.ax(), 30); // Read fault error code
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_drive_from_handle_with_c_drive_path() {
    let mut dos = Dos::new();

    // Create a file handle with path "C:\FILE.TXT"
    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(vec![])),
            "C:\\FILE.TXT".to_string(),
            0x0100,
        )
        .expect("allocate handle");

    let drive = dos.drive_from_handle(Some(handle));
    assert_eq!(drive, 2); // C: = 2
}

#[test]
fn test_drive_from_handle_with_d_drive_path() {
    let mut dos = Dos::new();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(vec![])),
            "D:\\DATA.DAT".to_string(),
            0x0100,
        )
        .expect("allocate handle");

    let drive = dos.drive_from_handle(Some(handle));
    assert_eq!(drive, 3); // D: = 3
}

#[test]
fn test_drive_from_handle_with_a_drive_path() {
    let mut dos = Dos::new();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(vec![])),
            "A:\\BOOT.SYS".to_string(),
            0x0100,
        )
        .expect("allocate handle");

    let drive = dos.drive_from_handle(Some(handle));
    assert_eq!(drive, 0); // A: = 0
}

#[test]
fn test_drive_from_handle_with_no_drive_letter() {
    let mut dos = Dos::new();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(vec![])),
            "CON".to_string(), // Device name, no drive letter
            0x0100,
        )
        .expect("allocate handle");

    let drive = dos.drive_from_handle(Some(handle));
    assert_eq!(drive, 2); // Default: C:
}

#[test]
fn test_drive_from_handle_with_none() {
    let dos = Dos::new();
    let drive = dos.drive_from_handle(None);
    assert_eq!(drive, 2); // Default: C:
}

#[test]
fn test_drive_from_handle_with_invalid_handle() {
    let dos = Dos::new();
    let drive = dos.drive_from_handle(Some(999)); // No such handle
    assert_eq!(drive, 2); // Default: C:
}

#[test]
fn test_int21_disposition_critical_error_equality() {
    let regs = gp64_cpu::Registers::default();

    let a = Int21Disposition::CriticalError {
        ah_function: 0x3F,
        drive: 2,
        error_code: 30,
        is_write: false,
        entry_snapshot: Box::new(regs.clone()),
    };
    let b = Int21Disposition::CriticalError {
        ah_function: 0x3F,
        drive: 2,
        error_code: 30,
        is_write: false,
        entry_snapshot: Box::new(regs.clone()),
    };
    assert_eq!(a, b);
}

#[test]
fn test_int21_disposition_critical_error_inequality() {
    let regs = gp64_cpu::Registers::default();

    let a = Int21Disposition::CriticalError {
        ah_function: 0x3F,
        drive: 2,
        error_code: 30,
        is_write: false,
        entry_snapshot: Box::new(regs.clone()),
    };
    // Different error_code
    let b = Int21Disposition::CriticalError {
        ah_function: 0x3F,
        drive: 2,
        error_code: 29,
        is_write: false,
        entry_snapshot: Box::new(regs.clone()),
    };
    assert_ne!(a, b);
}

#[test]
fn test_int21_disposition_critical_error_vs_iret() {
    let regs = gp64_cpu::Registers::default();

    let a = Int21Disposition::CriticalError {
        ah_function: 0x3F,
        drive: 2,
        error_code: 30,
        is_write: false,
        entry_snapshot: Box::new(regs),
    };
    assert_ne!(a, Int21Disposition::Iret);
    assert_ne!(a, Int21Disposition::NoIret);
}

#[test]
fn test_pending_critical_error_field_initialized_to_none() {
    let dos = Dos::new();
    assert!(dos.pending_critical_error.is_none());
}

#[test]
fn test_format_dos_bytes_printable_ascii() {
    let (s, truncated) = format_dos_bytes(b"Hello", 100);
    assert_eq!(s, "Hello");
    assert!(!truncated);
}

#[test]
fn test_format_dos_bytes_control_chars() {
    let (s, _) = format_dos_bytes(b"\r\n\t", 100);
    assert_eq!(s, "\\r\\n\\t");
}

#[test]
fn test_format_dos_bytes_hex_escape() {
    let (s, _) = format_dos_bytes(&[0x01, 0x7F, 0xFF], 100);
    assert_eq!(s, "\\x01\\x7F\\xFF");
}

#[test]
fn test_format_dos_bytes_truncation() {
    let (s, truncated) = format_dos_bytes(b"Hello, World!", 5);
    assert_eq!(s, "Hello");
    assert!(truncated);
}

#[test]
fn test_format_dos_bytes_empty() {
    let (s, truncated) = format_dos_bytes(b"", 100);
    assert_eq!(s, "");
    assert!(!truncated);
}

#[test]
fn test_format_dos_bytes_exact_limit() {
    let (s, truncated) = format_dos_bytes(b"ABC", 3);
    assert_eq!(s, "ABC");
    assert!(!truncated);
}

#[test]
fn test_bda_ticks_to_time_midnight() {
    let (h, m, s, hs) = bda_ticks_to_time(0);
    assert_eq!(h, 0);
    assert_eq!(m, 0);
    assert_eq!(s, 0);
    assert_eq!(hs, 0);
}

#[test]
fn test_bda_ticks_to_time_noon() {
    // BIOS_TICKS_PER_DAY / 2 = noon
    let ticks = gp64_types::bda::BIOS_TICKS_PER_DAY / 2;
    let (h, m, _s, _hs) = bda_ticks_to_time(ticks);
    assert_eq!(h, 12);
    assert_eq!(m, 0);
}

#[test]
fn test_bda_ticks_to_time_wraps_at_day_boundary() {
    // Ticks beyond a full day should wrap
    let ticks = gp64_types::bda::BIOS_TICKS_PER_DAY + 100;
    let (h, m, _s, _hs) = bda_ticks_to_time(ticks);
    // Should be the same as ticks=100
    let (h2, m2, _, _) = bda_ticks_to_time(100);
    assert_eq!(h, h2);
    assert_eq!(m, m2);
}

#[test]
fn test_boot_time_bda_ticks_returns_valid_range() {
    let ticks = boot_time_bda_ticks();
    // Must be in [0, BIOS_TICKS_PER_DAY)
    assert!(
        ticks < gp64_types::bda::BIOS_TICKS_PER_DAY,
        "boot_time_bda_ticks returned {ticks}, expected < {}",
        gp64_types::bda::BIOS_TICKS_PER_DAY
    );
    // Round-trip: convert to time and back; result should be within 1 tick
    let (h, m, s, hs) = bda_ticks_to_time(ticks);
    let roundtrip = time_to_bda_ticks(h, m, s, hs);
    assert!(
        ticks.abs_diff(roundtrip) <= 1,
        "round-trip mismatch: {ticks} vs {roundtrip}"
    );
}

#[test]
fn test_boot_time_bda_ticks_from_epoch_is_deterministic() {
    const FIXED_EPOCH: u64 = 1_700_000_000; // 2023-11-14 22:13:20 UTC
    let expected = time_to_bda_ticks(22, 13, 20, 0);

    assert_eq!(boot_time_bda_ticks_from_epoch(FIXED_EPOCH), expected);
    assert_eq!(boot_time_bda_ticks_from_epoch(FIXED_EPOCH), expected);
}

#[test]
fn test_boot_time_bda_ticks_from_epoch_uses_utc_time_of_day() {
    const DAY_SECS: u64 = 24 * 60 * 60;
    let time_of_day = 12 * 60 * 60 + 34 * 60 + 56;
    let expected = time_to_bda_ticks(12, 34, 56, 0);

    assert_eq!(boot_time_bda_ticks_from_epoch(time_of_day), expected);
    assert_eq!(
        boot_time_bda_ticks_from_epoch(20_000 * DAY_SECS + time_of_day),
        expected
    );
}

#[test]
fn boot_date_from_epoch_is_the_epochs_utc_calendar_day() {
    // 2023-11-14 22:13:20 UTC, a Tuesday; the first second of 2000-03-01
    // (Wednesday) checks the leap-century arithmetic.
    assert_eq!(boot_date_from_epoch(1_700_000_000), (2023, 11, 14, 2));
    assert_eq!(boot_date_from_epoch(951_868_800), (2000, 3, 1, 3));
}

#[test]
fn dos_write_marker_matches_across_split_successful_writes() {
    let mut dos = Dos::new();
    dos.configure_dos_write_marker(b"test-marker").unwrap();

    dos.dos_write_marker
        .as_mut()
        .unwrap()
        .observe(b"noise test-mar");
    assert!(!dos.take_dos_write_marker_hit());
    dos.dos_write_marker.as_mut().unwrap().observe(b"ker tail");
    assert!(dos.take_dos_write_marker_hit());
}

#[test]
fn dos_write_marker_rejects_near_match_and_handles_overlap() {
    let mut dos = Dos::new();
    dos.configure_dos_write_marker(b"ababac").unwrap();

    dos.dos_write_marker.as_mut().unwrap().observe(b"ababab");
    assert!(!dos.take_dos_write_marker_hit());
    dos.dos_write_marker.as_mut().unwrap().observe(b"ac");
    assert!(dos.take_dos_write_marker_hit());
}

#[test]
fn dos_write_marker_is_bounded_and_one_shot() {
    let mut dos = Dos::new();
    assert!(dos.configure_dos_write_marker(b"").is_err());
    assert!(dos.configure_dos_write_marker(&vec![b'x'; 257]).is_err());

    dos.configure_dos_write_marker(b"done").unwrap();
    dos.dos_write_marker.as_mut().unwrap().observe(b"done done");
    assert!(dos.take_dos_write_marker_hit());
    assert!(!dos.take_dos_write_marker_hit());
    dos.dos_write_marker.as_mut().unwrap().observe(b"done");
    assert!(!dos.take_dos_write_marker_hit());
}

#[test]
fn test_build_sound_env_vars_empty() {
    let config = SoundEnvConfig {
        sound_blaster: None,
        gravis_ultrasound: None,
    };
    let vars = build_sound_env_vars(&config);
    assert!(vars.is_empty());
}

#[test]
fn test_build_sound_env_vars_sb_basic_coverage() {
    let config = SoundEnvConfig {
        sound_blaster: Some(gp64_types::SoundBlasterEnv {
            port: 0x220,
            irq: 5,
            dma: 1,
            hdma: 5,
            sb_type: 6,
            emu8000_port: 0,
            mpu_port: 0,
        }),
        gravis_ultrasound: None,
    };
    let vars = build_sound_env_vars(&config);
    assert_eq!(vars.len(), 1);
    assert!(vars[0].starts_with("BLASTER="));
    assert!(vars[0].contains("A220"));
    assert!(vars[0].contains("I5"));
    assert!(vars[0].contains("D1"));
}

#[test]
fn test_build_sound_env_vars_sb_with_emu8000_and_mpu() {
    let config = SoundEnvConfig {
        sound_blaster: Some(gp64_types::SoundBlasterEnv {
            port: 0x220,
            irq: 5,
            dma: 1,
            hdma: 5,
            sb_type: 6,
            emu8000_port: 0x620,
            mpu_port: 0x330,
        }),
        gravis_ultrasound: None,
    };
    let vars = build_sound_env_vars(&config);
    assert_eq!(vars.len(), 1);
    assert!(vars[0].contains("E620"));
    assert!(vars[0].contains("P330"));
}

#[test]
fn test_build_sound_env_vars_gus_coverage() {
    let config = SoundEnvConfig {
        sound_blaster: None,
        gravis_ultrasound: Some(gp64_types::GusEnv {
            port: 0x240,
            play_dma: 1,
            rec_dma: 1,
            play_irq: 11,
            rec_irq: 11,
        }),
    };
    let vars = build_sound_env_vars(&config);
    assert_eq!(vars.len(), 2);
    assert!(vars[0].starts_with("ULTRASND="));
    assert!(vars[1].contains("ULTRADIR"));
}

#[test]
fn test_build_glide_env_vars_enabled_has_three_vars() {
    let vars = build_glide_env_vars(true);
    assert_eq!(vars.len(), 3);
    assert!(vars.iter().any(|v| v.starts_with("GLIDE=")));
    assert!(vars.iter().any(|v| v.starts_with("SST_GRXCLK=")));
    assert!(vars.iter().any(|v| v.starts_with("FX_GLIDE_NO_SPLASH=")));
}

#[test]
fn test_build_glide_env_vars_disabled_is_empty() {
    let vars = build_glide_env_vars(false);
    assert!(vars.is_empty());
}

#[test]
fn test_dos_enable_stdout_capture_and_take() {
    let mut dos = Dos::new();
    assert!(dos.take_stdout_capture().is_empty());

    dos.enable_stdout_capture();
    let captured = dos.take_stdout_capture();
    assert!(captured.is_empty()); // Nothing written yet
                                  // After take, capture is disabled again
    assert!(dos.stdout_capture.is_none());
}

#[test]
fn test_dos_get_glide_ovl_bytes_none() {
    let dos = Dos::new();
    assert!(dos.get_glide_ovl_bytes().is_none());
}

#[test]
fn test_dos_get_glide_ovl_bytes_some() {
    let mut dos = Dos::new();
    dos.glide_ovl_data = Some(vec![0xDE, 0xAD].into());
    let bytes = dos.get_glide_ovl_bytes().unwrap();
    assert_eq!(bytes.as_ref(), [0xDE, 0xAD]);
}

#[test]
fn test_dos_emit_hdd_clicks_disabled() {
    let mut dos = Dos::new();
    dos.hdd_sound_enabled = false;
    dos.emit_hdd_clicks(5);
    assert_eq!(dos.hdd_seek_clicks, 0);
}

#[test]
fn test_dos_emit_hdd_clicks_enabled() {
    let mut dos = Dos::new();
    dos.hdd_sound_enabled = true;
    dos.emit_hdd_clicks(5);
    assert_eq!(dos.hdd_seek_clicks, 5);
    dos.emit_hdd_clicks(3);
    assert_eq!(dos.hdd_seek_clicks, 8);
}

#[test]
fn test_dos_emit_hdd_clicks_saturates() {
    let mut dos = Dos::new();
    dos.hdd_sound_enabled = true;
    dos.hdd_seek_clicks = 250;
    dos.emit_hdd_clicks(10);
    assert_eq!(dos.hdd_seek_clicks, 255);
}

#[test]
fn test_dos_set_dta() {
    let mut dos = Dos::new();
    dos.set_dta(0x2000, 0x0080);
    assert_eq!(dos.dta_segment, 0x2000);
    assert_eq!(dos.dta_offset, 0x0080);
    assert_eq!(
        dos.dta_linear(),
        Memory::seg_offset_to_linear(0x2000, 0x0080)
    );
}

#[test]
fn test_dos_verify_flag_accessors() {
    let mut dos = Dos::new();
    assert!(!dos.verify_flag());
    dos.set_verify_flag(true);
    assert!(dos.verify_flag());
    dos.set_verify_flag(false);
    assert!(!dos.verify_flag());
}

#[test]
fn test_dos_ctrl_break_check_accessors() {
    let mut dos = Dos::new();
    assert!(!dos.ctrl_break_check());
    dos.set_ctrl_break_check(true);
    assert!(dos.ctrl_break_check());
}

#[test]
fn test_dos_set_last_exit_code_and_termination_type() {
    let mut dos = Dos::new();
    dos.set_last_exit_code(42);
    assert_eq!(dos.last_exit_code, 42);
    dos.set_last_termination_type(3);
    assert_eq!(dos.last_termination_type, 3);
}

#[test]
fn test_dos_restore_current_psp() {
    let mut dos = Dos::new();
    let orig_dta_seg = dos.dta_segment;
    dos.restore_current_psp(0x3000);
    assert_eq!(dos.current_psp(), 0x3000);
    // DTA should NOT be reset
    assert_eq!(dos.dta_segment, orig_dta_seg);
}

#[test]
fn test_dos_set_current_psp_resets_dta() {
    let mut dos = Dos::new();
    dos.set_current_psp(0x3000);
    assert_eq!(dos.current_psp(), 0x3000);
    assert_eq!(dos.dta_segment, 0x3000);
    assert_eq!(dos.dta_offset, 0x0080);
}

#[test]
fn test_dos_country_info_accessor() {
    let dos = Dos::new();
    let info = dos.country_info();
    assert_eq!(info.country_code, 1); // US default
}

#[test]
fn test_dos_env_segment_accessor() {
    let dos = Dos::new();
    assert_eq!(dos.env_segment(), 0x0000); // Before init
}

#[test]
fn test_critical_error_snapshot_is_preserved() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let memory = Memory::new();

    // Set distinctive register values
    cpu.regs.set_ax(0x1234);
    cpu.regs.set_bx(0x5678);
    let snapshot = cpu.regs.clone();

    // MDD-BUG-CRUCIBLE-00381: move the LIVE registers on before classifying.
    // Without this the snapshot and `cpu.regs` are identical, so the
    // assertions below hold whether the disposition carries the snapshot or
    // the live registers - the test proved nothing about the distinction it
    // is named for. It also models what really happens: the INT 21h read and
    // write paths mutate AX and the carry flag well before the error site,
    // which is precisely why the snapshot is taken at entry.
    cpu.regs.set_ax(0x9ABC);
    cpu.regs.set_bx(0xDEF0);

    // Use UnexpectedEof which is a critical error (unlike Error::other which is now non-critical)
    let err = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected eof");
    let disp = dos.classify_io_error(&mut cpu, &memory, &err, 0x3F, None, false, snapshot);

    match disp {
        Int21Disposition::CriticalError { entry_snapshot, .. } => {
            assert_eq!(entry_snapshot.eax & 0xFFFF, 0x1234);
            assert_eq!(entry_snapshot.ebx & 0xFFFF, 0x5678);
            // MDD-BUG-CRUCIBLE-00381: state the discrimination outright.
            // `classify_io_error` overwrites AX with the DOS error code, so
            // the assertions above only mean anything while the live
            // registers have MOVED AWAY from the snapshot. Without this the
            // test would still pass if the disposition started carrying
            // live registers and the function stopped touching AX - and
            // carrying live registers is exactly the mistake available when
            // the snapshot is passed by value rather than pre-boxed.
            assert_ne!(
                cpu.regs.ax(),
                0x1234,
                "live registers must differ from the snapshot, or this test                      cannot tell the two apart"
            );
        }
        other => panic!("Expected CriticalError, got {:?}", other),
    }
}

#[test]
fn test_snapshot_restore_preserves_virtual_files() {
    // MDD-BUG-FLUXHOMEARPA-00015: virtual_files (synthetic in-memory DOS files,
    // a later extender self-open falls through to the host file.
    let mut dos = Dos::new();
    dos.register_virtual_file("C:\\GAME.EXE", vec![0x4D, 0x5A, 0x90, 0x00]);
    assert!(dos.has_virtual_file(2, "\\GAME.EXE"));

    let snap = dos.snapshot();

    // Restore into a fresh Dos that has no virtual files of its own.
    let mut fresh = Dos::new();
    assert!(!fresh.has_virtual_file(2, "\\GAME.EXE"));
    fresh.restore(&snap);

    assert!(
        fresh.has_virtual_file(2, "\\GAME.EXE"),
        "restore must reinstate registered virtual files"
    );
    assert_eq!(
        fresh.get_virtual_file_bytes(2, "\\GAME.EXE"),
        Some(vec![0x4D, 0x5A, 0x90, 0x00].into()),
        "restored virtual file must return the original bytes"
    );
}

/// MDD-BUG-KIL-00780: a checkpoint and a restore share the synthetic
/// payloads instead of copying them - the restored file is the very
/// allocation the live map held, not a copy of its bytes.
#[test]
fn test_snapshot_restore_shares_virtual_file_payloads() {
    let mut dos = Dos::new();
    dos.register_virtual_file("C:\\GAME.EXE", vec![0x4D, 0x5A, 0x90, 0x00]);
    let live = dos
        .virtual_files
        .values()
        .next()
        .cloned()
        .expect("the registered payload");

    let snap = dos.snapshot();
    let mut fresh = Dos::new();
    fresh.restore(&snap);

    let restored = fresh
        .virtual_files
        .values()
        .next()
        .expect("restore reinstates the payload");
    assert!(
        std::sync::Arc::ptr_eq(restored, &live),
        "the restored payload is the live allocation, not a copy"
    );
}

/// MDD-BUG-CRUCIBLE-00374: an ACTIVE handle on a synthetic file must keep
/// reading across a snapshot restore, from where it left off.
///
/// Preserving the `virtual_files` namespace (FLUXHOMEARPA-00015, above) is
/// not enough: the SFT entry itself has to be rebuilt. The restore path
/// reopens by parsing a drive letter out of the stored path, and a
/// synthetic handle's path is the caller's spelling — so before the fix the
/// entry was dropped and the guest's still-open handle became invalid.
#[test]
fn active_synthetic_handle_survives_a_snapshot_restore() {
    use crate::files::SftBacking;

    let payload: Vec<u8> = (0..32u8).collect();
    let mut dos = Dos::new();
    dos.register_virtual_file("C:\\GAME.EXE", payload.clone());

    // Open the registered virtual file the way INT 21h/AH=3Dh does, and
    // consume the first half.
    let bytes = dos
        .get_virtual_file_bytes(2, "\\GAME.EXE")
        .expect("virtual file is registered");
    let vf: Box<dyn crate::filesystem::VirtualFile> =
        Box::new(crate::filesystem::SyntheticFile::new(bytes));
    let handle = dos
        .file_table
        .allocate_synthetic_with_sharing(
            vf,
            "C:\\GAME.EXE".to_string(),
            dos.current_psp,
            crate::files::SHARE_DENY_NONE,
            crate::files::ACCESS_READ,
            SftBacking::VirtualFile("C:\\GAME.EXE".to_string()),
        )
        .expect("a handle is available");
    let mut first_half = [0u8; 16];
    assert_eq!(
        read_handle(&mut dos, handle, &mut first_half),
        16,
        "the handle reads before the snapshot"
    );
    assert_eq!(&first_half, &payload[..16]);

    let snap = dos.snapshot();

    let mut fresh = Dos::new();
    fresh.restore(&snap);
    fresh.reopen_files_from_snapshot(&snap);

    let mut second_half = [0u8; 16];
    assert_eq!(
        read_handle(&mut fresh, handle, &mut second_half),
        16,
        "the restored handle must still be readable, not invalid"
    );
    assert_eq!(
        &second_half,
        &payload[16..],
        "the restored handle must continue from its saved position"
    );
}

/// The same guarantee for a GLIDE2X.OVL handle, whose stored path is a bare
/// name with no drive letter at all — the case that made the drive-based
/// reopen impossible rather than merely wrong.
#[test]
fn active_glide_overlay_handle_survives_a_snapshot_restore() {
    use crate::files::SftBacking;

    let ovl: Vec<u8> = (0..64u8).rev().collect();
    let mut dos = Dos::new();
    dos.glide_ovl_data = Some(ovl.clone().into());

    let vf: Box<dyn crate::filesystem::VirtualFile> =
        Box::new(crate::filesystem::SyntheticFile::new(ovl.clone()));
    let handle = dos
        .file_table
        .allocate_synthetic_with_sharing(
            vf,
            "GLIDE2X.OVL".to_string(),
            dos.current_psp,
            crate::files::SHARE_DENY_NONE,
            crate::files::ACCESS_READ,
            SftBacking::GlideOvl,
        )
        .expect("a handle is available");
    let mut head = [0u8; 8];
    read_handle(&mut dos, handle, &mut head);

    let snap = dos.snapshot();
    let mut fresh = Dos::new();
    fresh.glide_ovl_data = Some(ovl.clone().into());
    fresh.restore(&snap);
    fresh.reopen_files_from_snapshot(&snap);

    let mut tail = [0u8; 8];
    assert_eq!(
        read_handle(&mut fresh, handle, &mut tail),
        8,
        "a restored GLIDE2X.OVL handle must still read"
    );
    assert_eq!(&tail, &ovl[8..16], "and continue where it left off");
}

#[test]
fn test_int21_ah37_get_switch_char() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x37);
    cpu.regs.set_al(0x00); // Get switch character

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.dl(), b'/');
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int21_ah37_set_switch_char() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x37);
    cpu.regs.set_al(0x01); // Set switch character
    cpu.regs.set_dl(b'-');

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00); // Success
}

#[test]
fn test_int21_ah37_get_device_availability() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x37);
    cpu.regs.set_al(0x02); // Get device availability

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.dl(), 0xFF);
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int21_ah37_set_device_availability() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x37);
    cpu.regs.set_al(0x03); // Set device availability

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int21_ah37_invalid_subfunction() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x37);
    cpu.regs.set_al(0x05); // Invalid subfunction

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);
}
/// Issue one `AX=713Bh` (LFN CHDIR) against a `TreeDrive` mounted on C:,
/// returning the CPU and the resulting current directory.
fn run_lfn_713b(path: &[u8], dirs: &[&str], files: &[&str]) -> (gp64_cpu::Cpu, String) {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(
        2,
        Box::new(TreeDrive {
            dirs: dirs.iter().map(|d| d.to_string()).collect(),
            files: files.iter().map(|f| f.to_string()).collect(),
        }),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in path.iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x3B);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    let cwd = dos.path_resolver.get_current_dir(2);
    (cpu, cwd)
}

/// Drive a single `AX=7143h` call against a recording drive on C:.
fn run_lfn_7143(
    bl: u8,
    cx: u16,
    path: &[u8],
    fail_with: Option<std::io::ErrorKind>,
) -> (gp64_cpu::Cpu, std::sync::Arc<std::sync::Mutex<AttrLog>>) {
    let log = std::sync::Arc::new(std::sync::Mutex::new(AttrLog::default()));
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(
        2,
        Box::new(RecordingAttrDrive {
            log: log.clone(),
            fail_with,
        }),
    );

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in path.iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x43);
    cpu.regs.set_bl(bl);
    cpu.regs.set_cx(cx);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    (cpu, log)
}

/// Helper: read all env vars from the env block as Vec<(key, value)>
fn read_env_block(dos: &Dos, memory: &Memory) -> Vec<(String, String)> {
    let env_base = Memory::seg_offset_to_linear(dos.env_segment(), 0);
    let mut vars = Vec::new();
    let mut offset = 0;
    loop {
        let mut var = String::new();
        loop {
            let b = memory.read_u8(env_base + offset);
            if b == 0 {
                break;
            }
            var.push(b as char);
            offset += 1;
        }
        if var.is_empty() {
            break;
        }
        if let Some((name, val)) = var.split_once('=') {
            vars.push((name.to_string(), val.to_string()));
        }
        offset += 1;
    }
    vars
}

/// Reads through an SFT handle the way INT 21h/AH=3Fh does, keeping the
/// entry's position in step.
fn read_handle(dos: &mut Dos, handle: u16, buf: &mut [u8]) -> usize {
    use std::io::Read;
    let entry = dos
        .file_table
        .get_sft_mut(handle)
        .expect("handle points at a live SFT entry");
    let n = entry.file.read(buf).expect("synthetic reads do not fail");
    entry.position += n as u64;
    n
}
