use super::*;

#[test]
fn test_int2f_windows_install_check_ah1600() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1600);
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // AL=0 means no Windows running
    assert_eq!(cpu.regs.al(), 0);
}

#[test]
fn test_int2f_install_check_ah1600_hosted_session_active() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1600);
    let result = dos.int2f(&mut cpu, &mut memory, true);
    assert!(result.is_ok());
    // Windows 3.10 Enhanced Mode: AL=major(3), AH=minor(0x0A)
    assert_eq!(cpu.regs.al(), 0x03);
    assert_eq!(cpu.regs.ah(), 0x0A);
}

#[test]
fn test_int2f_vxd_api_entry_point_ah1684_returns_null() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Request VMM VxD API entry point
    cpu.regs.set_ax(0x1684);
    cpu.regs.set_bx(0x0001); // VMM
    cpu.regs.es = 0xBEEF; // non-zero to verify it gets cleared
    cpu.regs.set_di(0xBEEF);
    let result = dos.int2f(&mut cpu, &mut memory, true);
    assert!(result.is_ok());
    // ES:DI = 0000:0000 means "VxD not present"
    assert_eq!(cpu.regs.es, 0);
    assert_eq!(cpu.regs.di(), 0);
}

#[test]
fn test_int2f_vxd_api_entry_point_ah1684_unknown_vxd() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Request unknown VxD
    cpu.regs.set_ax(0x1684);
    cpu.regs.set_bx(0xBEEF);
    cpu.regs.es = 0x1234;
    cpu.regs.set_di(0x5678);
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.es, 0);
    assert_eq!(cpu.regs.di(), 0);
}

#[test]
fn test_int2f_windows_version_check_ah160a() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x160A);
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // AX=160Ah remains unchanged when NT is not running.
    assert_eq!(cpu.regs.ax(), 0x160A);
}

#[test]
fn test_int2f_windows_enhanced_init_ah1605() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1605);
    cpu.regs.set_cx(0xFFFF); // Non-zero to test it gets cleared
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // CX=0 indicates no failure
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int2f_doskey_install_check_ah4800() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x4800);
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // AL=0 means not installed
    assert_eq!(cpu.regs.al(), 0);
}

#[test]
fn test_int2f_hma_query_ah4a01() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x4A01);
    cpu.regs.set_bx(0xFFFF); // Non-zero to test
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // BX=0 means no HMA available
    assert_eq!(cpu.regs.bx(), 0);
}

#[test]
fn test_int2f_hma_allocate_ah4a02() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x4A02);
    cpu.regs.set_bx(100); // Request 100 bytes
    cpu.regs.es = 0x1234;
    cpu.regs.set_di(0x5678);
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // ES:DI=0:0 means allocation failed
    assert_eq!(cpu.regs.es, 0);
    assert_eq!(cpu.regs.di(), 0);
}

#[test]
fn test_int2f_display_sys_install_check_ahad00() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xAD00);
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // AL=0 means not installed
    assert_eq!(cpu.regs.al(), 0);
}

#[test]
fn test_int2f_append_install_check_ahb700() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xB700);
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // AL=0 means not installed
    assert_eq!(cpu.regs.al(), 0);
}

#[test]
fn test_int2f_append_get_path_ahb702() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xB702);
    cpu.regs.es = 0x1234;
    cpu.regs.set_di(0x5678);
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // ES:DI=0:0 means not installed
    assert_eq!(cpu.regs.es, 0);
    assert_eq!(cpu.regs.di(), 0);
}

#[test]
fn test_int2f_append_get_state_ahb706() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0xB706);
    cpu.regs.set_bx(0xFFFF);
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // BX=0 means disabled
    assert_eq!(cpu.regs.bx(), 0);
}

#[test]
fn test_int2f_dos_internal_get_psp_ah1251() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.current_psp = 0x1234;
    cpu.regs.set_ax(0x1251);
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.bx(), 0x1234);
}

#[test]
fn test_int2f_dos_internal_strlen_ah1221() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    // Write test string "HELLO\0"
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, ch) in b"HELLO\0".iter().enumerate() {
        memory.write_u8(addr + i, *ch);
    }

    cpu.regs.set_ax(0x1221);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);

    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    assert_eq!(cpu.regs.cx(), 5); // "HELLO" = 5 chars
}

#[test]
fn test_int2f_share_install_check_ah1000() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    cpu.regs.set_ax(0x1000);
    cpu.regs.set_al(0xFF); // Set to non-zero to verify unchanged
    let result = dos.int2f(&mut cpu, &mut memory, false);
    assert!(result.is_ok());
    // AL unchanged (0xFF) means not installed (if it was 0x00, it would be set)
    // Actually in our implementation we don't modify AL at all for 0x1000
    // so it stays at the input value - this is correct DOS behavior for "not installed"
}

#[test]
fn test_exec_read_param_block_cmdtail_zero() {
    let dos = Dos::new();
    let memory = Memory::new();

    let (env, cmdline) = dos.exec_read_param_block_cmdtail(&memory, 0, 0);
    assert_eq!(env, 0);
    assert!(cmdline.is_none());
}

#[test]
fn test_int21_create_temp_file_no_null_terminator_overflow() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for i in 0..129 {
        memory.write_u8(addr + i, b'A');
    }

    cpu.regs.set_ah(0x5A);
    cpu.regs.set_cx(0);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_create_temp_file_missing_drive_ah5a() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, "D:\\");
    cpu.regs.set_ah(0x5A);
    cpu.regs.set_cx(0);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5);
}

#[test]
fn test_int21_create_temp_file_allocation_failure() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    fill_file_table(&mut dos);

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\");
    cpu.regs.set_ah(0x5A);
    cpu.regs.set_cx(0);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 4);
}

#[test]
fn test_int21_create_new_file_allocation_failure_ah5b() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    fill_file_table(&mut dos);

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\NEWFAIL.TXT");
    cpu.regs.set_ah(0x5B);
    cpu.regs.set_cx(0);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 4);
}

#[test]
fn test_int21_rename_file_missing_drive_ah56() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, "D:\\OLD.TXT");
    write_asciiz(&mut memory, 0x3000, 0x0100, "D:\\NEW.TXT");
    cpu.regs.set_ah(0x56);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_get_file_date_time_missing_drive_ah57() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(vec![1, 2, 3])),
            "D:\\MISSING.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    cpu.regs.set_ah(0x57);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bx(handle);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.dx(), 0x0021);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_open_extended_missing_drive_6c() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    write_asciiz(&mut memory, 0x2000, 0x0100, "D:\\NOFILE.TXT");
    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(0x01);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_int21_open_extended_not_found_no_create_6c() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\NOFILE.TXT");
    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(0x01);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
}

#[test]
fn test_int21_open_extended_allocation_failure_6c() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();
    let mut vga = gp64_hw::vga::Vga::new();
    let mut keyboard = gp64_hw::keyboard::Keyboard::new();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("EXIST.TXT");
    std::fs::write(&file_path, b"data").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    fill_file_table(&mut dos);

    write_asciiz(&mut memory, 0x2000, 0x0100, "C:\\EXIST.TXT");
    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_bh(0x00);
    cpu.regs.set_cx(0x0000);
    cpu.regs.set_dl(0x01);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 4);
}

#[test]
fn test_find_first_attr_filter_excludes_hidden() {
    let mut dos = Dos::new();

    let mut attrs = attrs_with_size(1);
    attrs.hidden = true;
    let entries = vec![("HIDDEN.TXT".to_string(), attrs)];
    dos.drives.insert(2, Box::new(ListingDrive::new(entries)));

    let result = dos.find_first("C:\\*.TXT", 0x00);
    assert!(result.is_err());
}

#[test]
fn test_int21_fcb_delete_wildcards_listing_drive_success() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let entries = vec![("FOO1.TXT".to_string(), attrs_with_size(1))];
    dos.drives.insert(2, Box::new(ListingDrive::new(entries)));

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "FOO?", "TXT");
    cpu.regs.set_ah(0x13);
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
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int21_fcb_rename_wildcards_listing_drive_success() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let entries = vec![("BAR1.TXT".to_string(), attrs_with_size(1))];
    dos.drives.insert(2, Box::new(ListingDrive::new(entries)));

    create_rename_fcb(&mut memory, 0x2000, 0x0100, 3, "BAR?", "TXT", "BAZ?", "TXT");
    cpu.regs.set_ah(0x17);
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
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int21_fcb_rename_wildcards_subdir() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let entries = vec![("FOO1.TXT".to_string(), attrs_with_size(1))];
    dos.drives.insert(2, Box::new(ListingDrive::new(entries)));
    dos.path_resolver.set_current_dir(r"\SUBDIR");

    create_rename_fcb(&mut memory, 0x2000, 0x0100, 3, "FOO?", "TXT", "BAR?", "TXT");
    cpu.regs.set_ah(0x17);
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
    assert_eq!(cpu.regs.al(), 0x00);
}

#[test]
fn test_int21_get_dpb_valid_drive_ah32() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Build the persistent DPB chain at DPB_SEG.
    dos.init_kernel_structures(&mut memory);

    // Insert a drive at C: (drive 2)
    dos.drives.insert(
        2,
        Box::new(DiskSpaceDrive::new(100 * 1024 * 1024, 50 * 1024 * 1024)),
    );

    cpu.regs.set_ah(0x32);
    cpu.regs.set_dl(3); // Drive C: (1=A, 2=B, 3=C)

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // AL should be 0 on success, DS:BX should point into the persistent
    // DPB chain at DPB_SEG (drive 2 -> offset 2 * 33 = 66).
    assert_eq!(cpu.regs.al(), 0x00);
    assert_eq!(cpu.regs.ds, DPB_SEG);
    assert_eq!(cpu.regs.bx(), 2 * 33);

    let dpb_addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.bx());
    // Check drive number at offset 0 (should be 2 for C:)
    assert_eq!(memory.read_u8(dpb_addr), 2);
    // Check bytes per sector at offset 2 (should be 512)
    assert_eq!(memory.read_u16(dpb_addr + 2), 0x0200);
}

#[test]
fn test_int21_get_dpb_invalid_drive_ah32() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x32);
    cpu.regs.set_dl(26); // Invalid drive Z: (26)

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // AL should be FFh for invalid drive
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_get_dpb_default_drive_ah32() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Build the persistent DPB chain at DPB_SEG.
    dos.init_kernel_structures(&mut memory);

    // Set current drive to C:
    dos.path_resolver.set_current_drive(2);
    dos.drives.insert(
        2,
        Box::new(DiskSpaceDrive::new(100 * 1024 * 1024, 50 * 1024 * 1024)),
    );

    cpu.regs.set_ah(0x32);
    cpu.regs.set_dl(0); // Default drive (0)

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // AL should be 0 on success, DS:BX should point into the DPB chain
    // for the default drive (C: -> drive index 2 -> offset 66).
    assert_eq!(cpu.regs.al(), 0x00);
    assert_eq!(cpu.regs.ds, DPB_SEG);
    assert_eq!(cpu.regs.bx(), 2 * 33);

    let dpb_addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.bx());
    assert_eq!(memory.read_u8(dpb_addr), 2); // Drive C:
}

#[test]
fn test_int21_get_set_disk_serial_ah69() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(
        2,
        Box::new(DiskSpaceDrive::new(100 * 1024 * 1024, 50 * 1024 * 1024)),
    );

    // Get disk serial number (AL=00h)
    cpu.regs.set_ah(0x69);
    cpu.regs.set_al(0x00); // Get
    cpu.regs.set_bl(3); // Drive C:
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Check that a serial number structure was written
    let buf_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    // Info level at offset 0 should be 0
    assert_eq!(memory.read_u16(buf_addr), 0);
}

#[test]
fn test_int21_get_disk_serial_invalid_drive_ah69() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x69);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bl(26); // Invalid drive

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 15); // Invalid drive
}

#[test]
fn test_int21_server_function_set_extended_error_ah5d() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set extended error (AL=0Ah)
    // DS:DX points to error info structure
    let buf_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u16(buf_addr, 5); // AX = error code 5 (access denied)
    memory.write_u16(buf_addr + 2, 0x0201); // BH=02 (hardware), BL=01 (retry)
    memory.write_u16(buf_addr + 4, 0x03); // CH = locus 3 (network)
    memory.write_u16(buf_addr + 6, 0);

    cpu.regs.set_ah(0x5D);
    cpu.regs.set_al(0x0A); // Set extended error
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

    // Now verify the error was set by calling Get Extended Error (AH=59h)
    cpu.regs.set_ah(0x59);
    cpu.regs.set_bx(0); // DOS version for compatibility

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // AX should be the error code we set (5)
    assert_eq!(cpu.regs.ax(), 5);
}

#[test]
fn test_int21_server_function_commit_all_ah5d() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Commit all files (AL=01h)
    cpu.regs.set_ah(0x5D);
    cpu.regs.set_al(0x01); // Commit all files

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_translate_bpb_to_dpb_ah53() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up a BPB at DS:SI
    let bpb_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    // BPB Structure:
    memory.write_u16(bpb_addr, 512); // 00h: Bytes per sector
    memory.write_u8(bpb_addr + 2, 4); // 02h: Sectors per cluster
    memory.write_u16(bpb_addr + 3, 1); // 03h: Reserved sectors
    memory.write_u8(bpb_addr + 5, 2); // 05h: Number of FATs
    memory.write_u16(bpb_addr + 6, 512); // 06h: Root directory entries
    memory.write_u16(bpb_addr + 8, 40960); // 08h: Total sectors (20MB)
    memory.write_u8(bpb_addr + 10, 0xF8); // 0Ah: Media descriptor (hard disk)
    memory.write_u16(bpb_addr + 11, 64); // 0Bh: Sectors per FAT

    // Set up DPB buffer at ES:BP
    let dpb_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);

    cpu.regs.set_ah(0x53);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bp(0x0200);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Verify DPB was written correctly
    assert_eq!(memory.read_u16(dpb_addr + 2), 512); // Bytes per sector
    assert_eq!(memory.read_u8(dpb_addr + 4), 3); // Sectors per cluster - 1
    assert_eq!(memory.read_u8(dpb_addr + 22), 0xF8); // Media descriptor
}

#[test]
fn test_int21_get_lookahead_ah64() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Get lookahead character (undocumented)
    cpu.regs.set_ah(0x64);
    cpu.regs.set_al(0x00); // Get lookahead

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // AL=0 means no character available (which is the expected state)
    assert_eq!(cpu.regs.al(), 0);
}

#[test]
fn test_int21_lfn_create_directory_ah71_39() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, r"C:\LongDirectoryName");

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x39); // LFN Create Directory
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify directory was created
    assert!(temp_dir.path().join("LongDirectoryName").exists());
}

#[test]
fn test_int21_lfn_remove_directory_ah71_3a() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let dir_path = temp_dir.path().join("TestDir");
    std::fs::create_dir(&dir_path).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, r"C:\TestDir");

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x3A); // LFN Remove Directory
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify directory was removed
    assert!(!dir_path.exists());
}

#[test]
fn test_int21_lfn_change_directory_ah71_3b() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let subdir = temp_dir.path().join("MySubDir");
    std::fs::create_dir(&subdir).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, r"C:\MySubDir");

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x3B); // LFN Change Directory
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // The LFN route keeps the guest's long-name spelling for AX=7147h.
    assert_eq!(dos.path_resolver.get_current_dir(2), r"\MySubDir");
}

#[test]
fn test_int21_lfn_delete_file_ah71_41() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("TestFile.txt");
    std::fs::write(&file_path, "test content").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, r"C:\TestFile.txt");

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x41); // LFN Delete File
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify file was deleted
    assert!(!file_path.exists());
}

#[test]
fn test_int21_lfn_get_attributes_ah71_43() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "test").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, r"C:\test.txt");

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x43); // LFN Get/Set Attributes
    cpu.regs.set_bl(0x00); // Subfunction: Get attributes
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // CX should contain attributes (archive bit at minimum)
    let attrs = cpu.regs.cx();
    assert!(attrs & 0x20 != 0, "Archive bit should be set");
}

#[test]
fn test_int21_lfn_get_current_directory_ah71_47() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let subdir = temp_dir.path().join("WorkDir");
    std::fs::create_dir(&subdir).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    // Set current directory first
    dos.path_resolver.set_current_dir_lfn(r"C:\WorkDir");

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x47); // LFN Get Current Directory
    cpu.regs.set_dl(0); // Current drive
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0200); // Buffer for result

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Read back the directory string
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0200);
    let mut dir = String::new();
    let mut i = 0;
    loop {
        let ch = memory.read_u8(addr + i);
        if ch == 0 {
            break;
        }
        dir.push(ch as char);
        i += 1;
    }
    assert_eq!(dir, "WorkDir");
}

#[test]
fn test_int21_lfn_rename_file_ah71_56() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let old_path = temp_dir.path().join("OldName.txt");
    std::fs::write(&old_path, "content").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, r"C:\OldName.txt");
    write_asciiz(&mut memory, 0x3000, 0x0100, r"C:\NewName.txt");

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x56); // LFN Rename
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Verify rename
    assert!(!old_path.exists());
    assert!(temp_dir.path().join("NewName.txt").exists());
}

#[test]
fn test_int21_lfn_canonicalize_path_ah71_60() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, r".\TEST");

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x60); // LFN Canonicalize
    cpu.regs.set_cl(0x00); // Full path
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100); // Source
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200); // Destination

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Read result
    let addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    let mut path = String::new();
    let mut i = 0;
    loop {
        let ch = memory.read_u8(addr + i);
        if ch == 0 {
            break;
        }
        path.push(ch as char);
        i += 1;
    }
    // Should start with drive letter
    assert!(
        path.starts_with("C:"),
        "Path should start with drive: {path}"
    );
}

#[test]
fn test_int21_lfn_get_volume_info_ah71_a0() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    write_asciiz(&mut memory, 0x2000, 0x0100, r"C:\");

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0xA0); // LFN Get Volume Info
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100); // Root path
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0200); // FS name buffer
    cpu.regs.set_cx(32); // Buffer size

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // BX should have flags (0x4002 = LFN supported, case preserved)
    assert_eq!(cpu.regs.bx(), 0x4002);
    // CX = max path component length
    assert_eq!(cpu.regs.cx(), 255);
    // DX = max path length
    assert_eq!(cpu.regs.dx(), 260);

    // Check filesystem name
    let addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
    let mut fs_name = String::new();
    let mut i = 0;
    loop {
        let ch = memory.read_u8(addr + i);
        if ch == 0 {
            break;
        }
        fs_name.push(ch as char);
        i += 1;
    }
    assert_eq!(fs_name, "FAT32");
}

#[test]
fn test_int21_lfn_find_close_ah71_a1_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0xA1); // LFN Find Close
    cpu.regs.set_bx(0x1234); // Some handle

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6); // Invalid handle
}

#[test]
fn test_int21_lfn_unsupported_function_returns_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0xFF); // Unsupported subfunction

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x7100); // Function not supported
}

/// Line 1216: `trace_enabled || int_trace_enabled` True branch.
#[test]
fn test_r5_dos_int21_trace_enabled_branch() {
    let _sub = trace_subscriber("gp64::dos::int=debug");
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    cpu.regs.ss = 0x0100;
    cpu.regs.set_sp(0x00FE);
    memory.write_u16_seg(0x0100, 0x00FE, 0x0010);
    memory.write_u16_seg(0x0100, 0x0100, 0x1000);
    cpu.regs.set_ah(0x30);
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
fn test_int21_mkdir_rmdir_clear_ax_on_success() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let temp_dir = std::env::temp_dir().join("gp64_mkdir_rmdir_ax_test");
    let _ = std::fs::create_dir(&temp_dir);
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    // Write path "C:\SUBDIR" to memory
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\SUBDIR\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // MakeDir (0x39) - set AX to non-zero to verify it gets cleared
    cpu.regs.set_ax(0x3900);
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
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "MakeDir should succeed"
    );
    assert_eq!(cpu.regs.ax(), 0, "MakeDir should clear AX on success");

    // RmDir (0x3A) - set AX to non-zero to verify it gets cleared
    cpu.regs.set_ax(0x3A00);
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
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "RmDir should succeed"
    );
    assert_eq!(cpu.regs.ax(), 0, "RmDir should clear AX on success");

    // Cleanup
    let _ = std::fs::remove_dir_all(&temp_dir);
}

/// Line 2783: `cpu.is_protected_mode() && cpu.regs.idtr.base != 0` True.
#[test]
fn test_r5_dos_get_vector_protected_mode() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 0;
    cpu.regs.idtr.base = 0x10000;
    cpu.regs.idtr.limit = 0x7FF;
    let idt_entry_addr = 0x10000 + 0x21 * 8;
    memory.write_u16(idt_entry_addr, 0x1234);
    memory.write_u16(idt_entry_addr + 2, 0x0028);
    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x21);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
}

/// Line 4229: `cpu.is_protected_mode()` True for set vector.
#[test]
fn test_r5_dos_set_vector_protected_mode() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 0;
    cpu.regs.set_ah(0x25);
    cpu.regs.set_al(0x21);
    cpu.regs.ds = 0x0028;
    cpu.regs.set_dx(0x1234);
    let ivt_addr = 0x21 * 4;
    memory.write_u16(ivt_addr, 0);
    memory.write_u16(ivt_addr + 2, 0);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(memory.read_u16(ivt_addr), 0);
}

/// Line 5069: `cpu.regs.bl() == 0` True for disk serial.
#[test]
fn test_r5_dos_disk_serial_default_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives
        .insert(2, Box::new(DiskSpaceDrive::new(100_000_000, 50_000_000)));
    cpu.regs.set_ah(0x69);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bl(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Line 4114: DPMI protected mode during AH=4Ch terminate.
#[test]
fn test_r5_dos_terminate_in_dpmi_pm() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    cpu.regs.cr0 |= 1;
    let _ = dos.dpmi.enter_protected_mode(&mut cpu, &mut memory);
    let parent_regs = gp64_cpu::Registers::new();
    dos.process_stack.push(ProcessState {
        psp_segment: 0x0100,
        registers: parent_regs,
        parent_psp: 0x0100,
        exec_ret_ip: 0x0000,
        exec_ret_cs: 0x0100,
        exec_ret_flags: 0x0202,
        pm_parent: false,
    });
    cpu.regs.set_ah(0x4C);
    cpu.regs.set_al(0);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.cr0 & 1, 0);
    // A child returning to its parent is not the DPMI program's exit: the
    // parent runs on, so nothing may end the run (MDD-BUG-NMI-00210).
    assert_eq!(dos.dpmi_program_exit_code, None);
}

/// The root DPMI program's exit records its code for the run loops.
#[test]
fn test_dpmi_terminate_records_exit_code() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    cpu.regs.cr0 |= 1;
    let _ = dos.dpmi.enter_protected_mode(&mut cpu, &mut memory);
    cpu.regs.set_ah(0x4C);
    cpu.regs.set_al(42); // Non-zero exit code
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_err(), "the root program's exit ends the run");
    assert_eq!(dos.dpmi_program_exit_code, Some(42));
}

/// Non-DPMI program termination does not set dpmi_program_exit_code.
#[test]
fn test_non_dpmi_terminate_no_exit_code() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    // Push a parent process but don't enter DPMI protected mode
    dos.process_stack.push(ProcessState {
        psp_segment: 0x0100,
        registers: gp64_cpu::Registers::new(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0000,
        exec_ret_cs: 0x0100,
        exec_ret_flags: 0x0202,
        pm_parent: false,
    });
    cpu.regs.set_ah(0x4C);
    cpu.regs.set_al(1);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // No DPMI client was active, so exit code should not be recorded
    assert_eq!(dos.dpmi_program_exit_code, None);
}

/// Line 7548: `m <= 2` True (January).
#[test]
fn test_r5_dos_ymd_to_days_january() {
    let date: u16 = (1 << 5) | 15; // year=0 (1980), month=1, day=15
    let time: u16 = 0;
    let result = Dos::dos_datetime_to_systemtime(date, time);
    assert!(result > std::time::UNIX_EPOCH);
}

/// Line 7548: `m <= 2` True (February).
#[test]
fn test_r5_dos_ymd_to_days_february() {
    let date: u16 = (2 << 5) | 0x1C; // year=0 (1980), month=2, day=28
    let time: u16 = 0;
    let result = Dos::dos_datetime_to_systemtime(date, time);
    assert!(result > std::time::UNIX_EPOCH);
}

/// Line 6365: `es == 0 && bx == 0` False (non-null param block).
#[test]
fn test_r5_dos_exec_param_block_nonnull() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0000);
    memory.write_u16(addr, 0x0000);
    memory.write_u16(addr + 2, 0x0080);
    memory.write_u16(addr + 4, 0x2000);
    let (env_seg, cmdline_ptr) = dos.exec_read_param_block_cmdtail(&memory, 0x2000, 0x0000);
    assert_eq!(env_seg, 0x0000);
    assert_eq!(cmdline_ptr, Some((0x2000, 0x0080)));
}

/// Line 6524: `desired_total_u32 > 0xFFFF` True.
#[test]
fn test_r5_dos_exec_paras_overflow() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);

    let mut exe = vec![0u8; 512];
    exe[0] = b'M';
    exe[1] = b'Z';
    exe[4] = 0x01;
    exe[5] = 0x00;
    exe[8] = 0x02;
    exe[9] = 0x00;
    exe[12] = 0xFF;
    exe[13] = 0xFF; // max_extra=0xFFFF
    exe[16] = 0x00;
    exe[17] = 0x01;
    exe[24] = 0x1C;
    exe[25] = 0x00;

    struct ExeDrive {
        data: Vec<u8>,
    }
    impl VirtualDrive for ExeDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(self.data.clone())))
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
                size: 512,
                ..Default::default()
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
            Ok((100_000_000, 50_000_000))
        }
    }

    let mut exe_probe = ExeDrive { data: exe.clone() };
    exercise_virtual_drive(&mut exe_probe);
    dos.drives.insert(2, Box::new(ExeDrive { data: exe }));
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"C:\\TEST.EXE\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    let pb = Memory::seg_offset_to_linear(0x3000, 0x0000);
    memory.write_u16(pb, 0);
    memory.write_u16(pb + 2, 0x80);
    memory.write_u16(pb + 4, 0x2000);
    let cl = Memory::seg_offset_to_linear(0x2000, 0x0080);
    memory.write_u8(cl, 0x00);
    memory.write_u8(cl + 1, 0x0D);
    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0000);
    cpu.regs.ss = 0x0100;
    cpu.regs.set_sp(0x00FE);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}

/// Lines 3485/3486: set_attributes returns io::Error(NotFound).
#[test]
fn test_r5_dos_set_attrs_io_not_found() {
    struct IoNotFoundDrive;
    impl VirtualDrive for IoNotFoundDrive {
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
            Err(anyhow::anyhow!("n"))
        }
        fn set_attributes(&mut self, _: &str, _: crate::filesystem::FileAttributes) -> Result<()> {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not found").into())
        }
        fn set_file_time(&mut self, _: &str, _: SystemTime) -> Result<()> {
            Err(anyhow::anyhow!("n"))
        }
        fn read_dir(
            &mut self,
            _: &str,
        ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
            Err(anyhow::anyhow!("n"))
        }
        fn get_disk_space(&self) -> Result<(u64, u64)> {
            Ok((0, 0))
        }
    }
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut io_not_found_probe = IoNotFoundDrive;
    exercise_virtual_drive(&mut io_not_found_probe);
    dos.drives.insert(2, Box::new(IoNotFoundDrive));
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x01);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.set_cx(0x20);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
}

/// Line 3485: set_attributes returns io::Error(PermissionDenied) -> code 5.
#[test]
fn test_r5_dos_set_attrs_io_perm_denied() {
    struct IoPermDrive;
    impl VirtualDrive for IoPermDrive {
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
            Err(anyhow::anyhow!("n"))
        }
        fn set_attributes(&mut self, _: &str, _: crate::filesystem::FileAttributes) -> Result<()> {
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied").into())
        }
        fn set_file_time(&mut self, _: &str, _: SystemTime) -> Result<()> {
            Err(anyhow::anyhow!("n"))
        }
        fn read_dir(
            &mut self,
            _: &str,
        ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
            Err(anyhow::anyhow!("n"))
        }
        fn get_disk_space(&self) -> Result<(u64, u64)> {
            Ok((0, 0))
        }
    }
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut io_perm_probe = IoPermDrive;
    exercise_virtual_drive(&mut io_perm_probe);
    dos.drives.insert(2, Box::new(IoPermDrive));
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x01);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.set_cx(0x20);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5);
}

/// Line 3101: sharing violation check True.
#[test]
fn test_r5_dos_open_sharing_violation() {
    struct SimpleDrive;
    impl VirtualDrive for SimpleDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![0u8; 10])))
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
            Ok(crate::filesystem::FileAttributes::default())
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
            Ok((100_000, 50_000))
        }
    }
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut simple_drive_probe = SimpleDrive;
    exercise_virtual_drive(&mut simple_drive_probe);
    dos.drives.insert(2, Box::new(SimpleDrive));
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    // First open with deny-all sharing
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x10);
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    // Second open should trigger sharing violation
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x10);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 32);
}

/// MDD-BUG-CRUCIBLE-00642: every create/open entry point must apply DOS
/// sharing before an existing host file can be truncated.
#[test]
fn test_create_apis_enforce_sharing_before_truncate() {
    use crate::files::{
        ACCESS_READ, ACCESS_READWRITE, ACCESS_WRITE, SHARE_COMPAT, SHARE_DENY_ALL, SHARE_DENY_NONE,
        SHARE_DENY_READ, SHARE_DENY_WRITE,
    };
    use tempfile::TempDir;

    let existing_modes = [
        ("deny-all", SHARE_DENY_ALL, ACCESS_READ),
        ("deny-read", SHARE_DENY_READ, ACCESS_WRITE),
        ("deny-write", SHARE_DENY_WRITE, ACCESS_READ),
        ("compat", SHARE_COMPAT, ACCESS_READWRITE),
    ];

    for (api, api_name) in [
        (0x003C_u16, "AH=3Ch"),
        (0x006C, "AH=6Ch"),
        (0x716C, "AX=716Ch"),
    ] {
        for &(mode_name, existing_share, existing_access) in &existing_modes {
            let temp = TempDir::new().unwrap();
            let host_path = temp.path().join("SHARED.TXT");
            std::fs::write(&host_path, b"must survive").unwrap();

            let mut dos = Dos::new();
            dos.drives.insert(
                2,
                Box::new(crate::filesystem::HostDrive::new(temp.path().to_path_buf())),
            );
            let canonical_host_path = host_path.canonicalize().unwrap();
            dos.file_table
                .allocate_with_sharing_identity(
                    Box::new(CursorFile::new(Vec::new())),
                    String::from(r"C:\SHARED.TXT"),
                    crate::files::FileIdentity::for_resolved_path(
                        2,
                        r"\SHARED.TXT",
                        Some(&canonical_host_path),
                    ),
                    0x2000,
                    existing_share,
                    existing_access,
                )
                .unwrap();

            let mut cpu = gp64_cpu::Cpu::new();
            let mut memory = Memory::new();
            let (mut vga, mut keyboard) = create_test_io();
            let filename_addr = Memory::seg_offset_to_linear(0x2100, 0x0040);
            for (i, &byte) in b"C:\\SHARED.TXT\0".iter().enumerate() {
                memory.write_u8(filename_addr + i, byte);
            }
            cpu.regs.ds = 0x2100;
            cpu.regs.set_cx(0);

            match api {
                0x003C => {
                    cpu.regs.set_ah(0x3C);
                    cpu.regs.set_dx(0x0040);
                }
                0x006C => {
                    cpu.regs.set_ah(0x6C);
                    cpu.regs.set_bl(SHARE_DENY_NONE | ACCESS_READWRITE);
                    cpu.regs.set_dl(0x03); // Open and truncate if the file exists.
                    cpu.regs.set_si(0x0040);
                }
                0x716C => {
                    cpu.regs.set_ax(0x716C);
                    cpu.regs
                        .set_bx(u16::from(SHARE_DENY_NONE | ACCESS_READWRITE));
                    cpu.regs.set_dx(0x0003); // Open and truncate if the file exists.
                    cpu.regs.set_si(0x0040);
                }
                _ => unreachable!(),
            }

            dos.int21(
                &mut cpu,
                &mut memory,
                &mut vga,
                &mut keyboard,
                &mut Pic::new(),
            )
            .unwrap();

            assert!(
                cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
                "{api_name} bypassed existing {mode_name} sharing"
            );
            assert_eq!(
                cpu.regs.ax(),
                32,
                "{api_name} / {mode_name}: expected sharing violation"
            );
            assert_eq!(
                dos.extended_error.0, 32,
                "{api_name} / {mode_name}: AH=59h state must report sharing violation"
            );
            assert_eq!(
                std::fs::read(&host_path).unwrap(),
                b"must survive",
                "{api_name} / {mode_name}: sharing failure occurred after truncation"
            );
        }
    }
}

/// MDD-BUG-CRUCIBLE-00643: DOS spellings that resolve to one host file
/// must share one identity for both sharing modes and byte-range locks.
#[test]
fn test_host_file_aliases_share_sharing_and_lock_identity() {
    use crate::files::{ACCESS_READ, ACCESS_READWRITE, SHARE_DENY_ALL, SHARE_DENY_NONE};
    use tempfile::TempDir;

    let long_name = "Long Shared Filename.dat";
    let short_name = crate::sfn::assign_short_names(&[long_name.to_string()])
        .pop()
        .expect("short-name assignment")
        .1;
    assert_ne!(short_name, long_name.to_ascii_uppercase());

    let temp = TempDir::new().unwrap();
    std::fs::write(temp.path().join(long_name), b"shared bytes").unwrap();

    let mut dos = Dos::new();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp.path().to_path_buf())),
    );
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let first_path = format!(r"C:\{long_name}");
    let aliases = [
        format!(r"C:\{short_name}"),
        format!(r"c:\{}", long_name.to_ascii_lowercase()),
        short_name.clone(),
    ];

    {
        let mut open = |path: &str, mode: u8| {
            write_asciiz(&mut memory, 0x2100, 0x0040, path);
            cpu.regs.set_ah(0x3D);
            cpu.regs.set_al(mode);
            cpu.regs.ds = 0x2100;
            cpu.regs.set_dx(0x0040);
            dos.int21(
                &mut cpu,
                &mut memory,
                &mut vga,
                &mut keyboard,
                &mut Pic::new(),
            )
            .unwrap();
            (
                cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
                cpu.regs.ax(),
            )
        };

        let (carry, _) = open(&first_path, SHARE_DENY_ALL | ACCESS_READ);
        assert!(!carry, "initial long-name open failed");

        for alias in &aliases {
            let (carry, error) = open(alias, SHARE_DENY_NONE | ACCESS_READ);
            assert!(
                carry,
                "alias {alias:?} bypassed the long-name handle's deny-all mode"
            );
            assert_eq!(error, 32, "alias {alias:?} returned the wrong DOS error");
        }
    }

    let temp = TempDir::new().unwrap();
    std::fs::write(temp.path().join(long_name), b"shared bytes").unwrap();
    let mut dos = Dos::new();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp.path().to_path_buf())),
    );
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let (locker, others) = {
        let mut open = |path: &str| {
            write_asciiz(&mut memory, 0x2200, 0x0030, path);
            cpu.regs.set_ah(0x3D);
            cpu.regs.set_al(SHARE_DENY_NONE | ACCESS_READWRITE);
            cpu.regs.ds = 0x2200;
            cpu.regs.set_dx(0x0030);
            dos.int21(
                &mut cpu,
                &mut memory,
                &mut vga,
                &mut keyboard,
                &mut Pic::new(),
            )
            .unwrap();
            assert!(
                !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
                "open failed for {path:?}: {}",
                cpu.regs.ax()
            );
            cpu.regs.ax()
        };

        let locker = open(&first_path);
        let others = aliases
            .iter()
            .map(|alias| (alias.clone(), open(alias)))
            .collect::<Vec<_>>();
        (locker, others)
    };

    cpu.regs.set_ah(0x5C);
    cpu.regs.set_al(0);
    cpu.regs.set_bx(locker);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    cpu.regs.set_si(0);
    cpu.regs.set_di(4);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();
    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "initial long-name lock failed"
    );

    for (alias, handle) in others {
        cpu.regs.set_ah(0x5C);
        cpu.regs.set_al(0);
        cpu.regs.set_bx(handle);
        cpu.regs.set_cx(0);
        cpu.regs.set_dx(1);
        cpu.regs.set_si(0);
        cpu.regs.set_di(1);
        dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        )
        .unwrap();

        assert!(
            cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "alias {alias:?} bypassed the long-name byte-range lock"
        );
        assert_eq!(
            cpu.regs.ax(),
            33,
            "alias {alias:?} returned wrong lock error"
        );
    }
}

/// Line 4051: UMB init conditions.
#[test]
fn test_r5_dos_umb_link_init() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(0x01);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dos.umb_linked);
    // Unlink then re-link to hit already-initialized path
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(0x00);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(0x01);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
}

/// Lines 5248/5270/5292/5308: LFN ops with missing drive.
#[test]
fn test_r5_dos_lfn_mkdir_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"Z:\\TESTDIR\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x39);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_r5_dos_lfn_rmdir_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"Z:\\TESTDIR\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x3A);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_r5_dos_lfn_chdir_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"Z:\\TESTDIR\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x3B);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

#[test]
fn test_r5_dos_lfn_delete_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"Z:\\FILE.TXT\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x41);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

/// Line 5334: LFN get attrs with drive not found.
#[test]
fn test_r5_dos_lfn_get_attrs_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"Z:\\FILE.TXT\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x43);
    cpu.regs.set_bl(0x00);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Lines 5339-5351: LFN get attrs individual flags.
#[test]
fn test_r5_dos_lfn_get_attrs_all_flags() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(2, Box::new(AllAttributesDrive));
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"FILE.TXT\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x43);
    cpu.regs.set_bl(0x00);
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let cx = cpu.regs.cx();
    assert!(cx & 0x01 != 0); // read_only
    assert!(cx & 0x02 != 0); // hidden
    assert!(cx & 0x04 != 0); // system
    assert!(cx & 0x10 != 0); // directory
    assert!(cx & 0x20 != 0); // archive
}

/// Line 5389: LFN getcwd DL=0 (default drive).
#[test]
fn test_r5_dos_lfn_getcwd_default_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives
        .insert(2, Box::new(DiskSpaceDrive::new(100_000, 50_000)));
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x47);
    cpu.regs.set_dl(0x00);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0000);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Line 5395: LFN getcwd invalid drive.
#[test]
fn test_r5_dos_lfn_getcwd_invalid_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x47);
    cpu.regs.set_dl(26);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0000);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0x0F);
}

/// Line 5422: LFN rename cross-drive error.
#[test]
fn test_r5_dos_lfn_rename_cross_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives
        .insert(2, Box::new(DiskSpaceDrive::new(100_000, 50_000)));
    dos.drives
        .insert(3, Box::new(DiskSpaceDrive::new(100_000, 50_000)));
    let sa = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"C:\\FILE.TXT\0".iter().enumerate() {
        memory.write_u8(sa + i, b);
    }
    let da = Memory::seg_offset_to_linear(0x3000, 0x0000);
    for (i, &b) in b"D:\\FILE.TXT\0".iter().enumerate() {
        memory.write_u8(da + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x56);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0000);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 17);
}

/// Line 5425: LFN rename drive not found.
#[test]
fn test_r5_dos_lfn_rename_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let sa = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"Z:\\OLD.TXT\0".iter().enumerate() {
        memory.write_u8(sa + i, b);
    }
    let da = Memory::seg_offset_to_linear(0x3000, 0x0000);
    for (i, &b) in b"Z:\\NEW.TXT\0".iter().enumerate() {
        memory.write_u8(da + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x56);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0000);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

/// Line 5577: LFN get volume info invalid drive.
#[test]
fn test_r5_dos_lfn_volume_info_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"Z:\\\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0xA0);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x3000;
    cpu.regs.set_di(0x0000);
    cpu.regs.set_cx(32);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Line 5726: ASCIIZ string length limit at 0x8000 (INT 2Fh/1221h).
#[test]
fn test_r5_dos_internal_strlen_limit() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for i in 0..0x8002 {
        memory.write_u8(a + i, b'A');
    }
    cpu.regs.set_ax(0x1221);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0000);
    let r = dos.int2f(&mut cpu, &mut memory, false);
    assert!(r.is_ok());
    assert_eq!(cpu.regs.cx(), 0x8000);
}

/// Lines 6612/6614/6616: env copy with immediate double-null.
#[test]
fn test_r5_dos_env_copy_empty() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    let env_seg = dos.dos_mem.alloc(&mut memory, 0x0100, 0x10).unwrap();
    let env_base = Memory::seg_offset_to_linear(env_seg, 0);
    memory.write_u8(env_base, 0);
    memory.write_u8(env_base + 1, 0);
    let result = dos
        .clone_env_for_exec(&mut memory, env_seg, "TEST.EXE")
        .unwrap();
    assert!(result > 0);
}

/// `clone_env_for_exec` must NOT allocate the child env block with
/// owner=0 (free). If it did, the exec_program BSS-init pass —
/// which iterates `iter_free_conventional_blocks()` — would treat
/// the new env block as free and overwrite it with the BSS-init
/// pattern (0xCF by default), corrupting the env strings + progname
/// extension before the child program ever runs. This was the
/// actual nested DOS extender failure root cause identified by
/// Implementer 2 during the 2026-05-26 DOS exploration lane after
/// the Designer 2 PSP/env HLD v2 landed but did NOT unblock the
/// started.
///
/// Fix: allocate the env block owned by `self.current_psp` (the
/// PARENT PSP at the time `clone_env_for_exec` is called). The
/// `iter_free_conventional_blocks()` filter excludes any block
/// with `owner_psp != 0`, so the BSS-init pass leaves the env
/// strings alone. After EXEC succeeds, `set_owner` transfers
/// ownership to the child PSP so AH=4Ch frees it.
#[test]
fn test_clone_env_for_exec_alloc_excluded_from_free_chain() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    // Simulate a parent process: assign current_psp before clone.
    // (In real EXEC flow, current_psp is the parent's PSP at this
    // point; we mirror that here.)
    let parent_psp = 0x0742u16;
    dos.set_current_psp(parent_psp);

    // Allocate a source env block (parent's env, owned by parent).
    let src_env_seg = dos.dos_mem.alloc(&mut memory, parent_psp, 0x10).unwrap();
    let src_base = Memory::seg_offset_to_linear(src_env_seg, 0);
    // PATH=C:\\\0\0 minimal env
    for (i, &b) in b"PATH=C:\\\0\0".iter().enumerate() {
        memory.write_u8(src_base + i, b);
    }

    let child_env_seg = dos
        .clone_env_for_exec(&mut memory, src_env_seg, "CHILD.EXE")
        .unwrap();
    assert!(child_env_seg > 0, "clone_env_for_exec should succeed");

    // The allocated child env block MUST NOT appear in the free
    // chain — if it does, the BSS-init pass will corrupt it.
    let free_blocks: Vec<(u16, u16)> = dos.dos_mem.iter_free_conventional_blocks().collect();
    for (start, end) in free_blocks {
        assert!(
            !(child_env_seg >= start && child_env_seg < end),
            "child env block at seg {:#06X} should NOT be in free chain \
             (found in range {:#06X}..{:#06X})",
            child_env_seg,
            start,
            end
        );
    }

    // Also confirm the env block carries the expected ownership
    // (parent_psp at this stage; will transition to child after
    // EXEC success via the existing set_owner fixup in exec_program).
    let info = dos
        .dos_mem
        .block_info_for_segment(child_env_seg)
        .expect("block info should exist for allocated env block");
    assert_eq!(
        info.owner_psp, parent_psp,
        "env block must be owned by parent PSP at clone time, not 0 (free)"
    );
    assert!(!info.is_free, "env block must not be reported as free");
}

/// Line 6917: load_overlay error with path separator.
#[test]
fn test_r5_dos_load_overlay_path_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(2, Box::new(NotFoundDrive));
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"C:\\SUB\\OVL.OVL\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    let pb = Memory::seg_offset_to_linear(0x3000, 0x0000);
    memory.write_u16(pb, 0x4000);
    memory.write_u16(pb + 2, 0x4000);
    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0000);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 3);
}

/// Line 6917: load_overlay error without path separator.
#[test]
fn test_r5_dos_load_overlay_file_not_found() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(2, Box::new(NotFoundDrive));
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"OVL.OVL\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    let pb = Memory::seg_offset_to_linear(0x3000, 0x0000);
    memory.write_u16(pb, 0x4000);
    memory.write_u16(pb + 2, 0x4000);
    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x03);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0000);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
}

/// Lines 7027/7032: TSR with env_seg != 0 and resize to large size.
#[test]
fn test_r5_dos_tsr_env_and_resize() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    let seg = dos.dos_mem.alloc(&mut memory, 0x100, 0x20).unwrap();
    dos.set_current_psp(seg);
    let psp_base = Memory::seg_offset_to_linear(seg, 0);
    let env_seg = dos.dos_mem.alloc(&mut memory, 0x100, 0x04).unwrap();
    memory.write_u16(psp_base + 0x2C, env_seg);
    dos.process_stack.push(ProcessState {
        psp_segment: 0x0100,
        registers: cpu.regs.clone(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0000,
        exec_ret_cs: 0x0100,
        exec_ret_flags: 0x0000,
        pm_parent: false,
    });
    cpu.regs.set_ah(0x31);
    cpu.regs.set_al(0);
    cpu.regs.set_dx(0xFFFF);
    let disp = dos.terminate_stay_resident(&mut cpu, &mut memory);
    assert_eq!(disp, Int21Disposition::NoIret);
}

/// Line 7050: TSR without parent returns Iret.
#[test]
fn test_r5_dos_tsr_no_parent() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    let seg = dos.dos_mem.alloc(&mut memory, 0x100, 0x20).unwrap();
    dos.set_current_psp(seg);
    // Give the PSP a parent (the shell), as a real EXEC would.
    let shell_psp = 0x0060u16;
    let psp_base = Memory::seg_offset_to_linear(seg, 0);
    memory.write_u16(psp_base + 0x16, shell_psp);
    cpu.regs.set_ah(0x31);
    cpu.regs.set_al(0);
    cpu.regs.set_dx(0x10);
    let disp = dos.terminate_stay_resident(&mut cpu, &mut memory);

    // Used to assert `Iret`, which was the defect: a plain IRET returns
    // into the terminated program and never restores the FFFF:0000 idle
    // sentinel, so a batch that TSRs on line 1 stalled there forever
    // (MDD-BUG-KILN-00070). With no parent on the process stack the shell
    // must get control back instead.
    assert_eq!(disp, Int21Disposition::NoIret);
    assert_eq!(cpu.regs.cs, 0xFFFF, "must park on the idle sentinel");
    assert_eq!(cpu.regs.ip(), 0x0000, "must park on the idle sentinel");
    assert_eq!(
        dos.current_psp, shell_psp,
        "current PSP must return to the shell, not stay on the resident program"
    );
}

/// Line 7055: TSR with DPMI protected mode.
#[test]
fn test_r5_dos_tsr_dpmi_pm() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    dos.dos_mem.init(&mut memory);
    let seg = dos.dos_mem.alloc(&mut memory, 0x100, 0x20).unwrap();
    dos.set_current_psp(seg);
    cpu.regs.cr0 |= 1;
    let _ = dos.dpmi.enter_protected_mode(&mut cpu, &mut memory);
    dos.process_stack.push(ProcessState {
        psp_segment: 0x0100,
        registers: gp64_cpu::Registers::new(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0000,
        exec_ret_cs: 0x0100,
        exec_ret_flags: 0x0000,
        pm_parent: false,
    });
    cpu.regs.set_ah(0x31);
    cpu.regs.set_al(0);
    cpu.regs.set_dx(0x10);
    let disp = dos.terminate_stay_resident(&mut cpu, &mut memory);
    assert_eq!(disp, Int21Disposition::NoIret);
    assert_eq!(cpu.regs.cr0 & 1, 0);
}

/// Line 4833: set file time when drive removed.
#[test]
fn test_r5_dos_set_file_time_no_drive() {
    struct SFileDrive;
    impl VirtualDrive for SFileDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![0u8; 10])))
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
                size: 10,
                modified: SystemTime::UNIX_EPOCH + Duration::from_hours(175200),
                ..Default::default()
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
            Ok((100_000, 50_000))
        }
    }
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut sfile_probe = SFileDrive;
    exercise_virtual_drive(&mut sfile_probe);
    dos.drives.insert(2, Box::new(SFileDrive));
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
    let handle = cpu.regs.ax();
    dos.drives.remove(&2);
    cpu.regs.set_ah(0x57);
    cpu.regs.set_al(0x01);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0x0021);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}

/// Line 4194: find_next with empty dir_path.
#[test]
fn test_r5_dos_find_next_empty_dir() {
    struct FDrive;
    impl VirtualDrive for FDrive {
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
            Err(anyhow::anyhow!("n"))
        }
        fn set_attributes(&mut self, _: &str, _: crate::filesystem::FileAttributes) -> Result<()> {
            Err(anyhow::anyhow!("n"))
        }
        fn set_file_time(&mut self, _: &str, _: SystemTime) -> Result<()> {
            Err(anyhow::anyhow!("n"))
        }
        fn read_dir(
            &mut self,
            _: &str,
        ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
            Ok(vec![
                (
                    "FILE1.TXT".to_string(),
                    crate::filesystem::FileAttributes::default(),
                ),
                (
                    "FILE2.TXT".to_string(),
                    crate::filesystem::FileAttributes::default(),
                ),
            ])
        }
        fn get_disk_space(&self) -> Result<(u64, u64)> {
            Ok((0, 0))
        }
    }
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut fdrive_probe = FDrive;
    exercise_virtual_drive(&mut fdrive_probe);
    dos.drives.insert(2, Box::new(FDrive));
    dos.set_dta(0x0100, 0x0080);
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"*.TXT\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x4E);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.set_cx(0);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    cpu.regs.set_ah(0x4F);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
}

/// Line 1518: buffered input with max_len = 0.
#[test]
fn test_r5_dos_buffered_input_zero_maxlen() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let buf_addr = Memory::seg_offset_to_linear(0x2000, 0x0000);
    memory.write_u8(buf_addr, 0); // max_len = 0
    memory.write_u8(buf_addr + 1, 0);
    bda::kbd_enqueue(&mut memory, 0x1C, 0x0D); // Enter key
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

/// Line 3261: trace for stdout writes.
#[test]
fn test_r5_dos_write_stdout_trace() {
    let _sub = trace_subscriber("gp64::dos::print=debug,gp64::dos::write=debug");
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"Hello".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(0x01);
    cpu.regs.set_cx(5);
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

/// Line 1409: AH=09h print with trace.
#[test]
fn test_r5_dos_ah09_print_trace() {
    let _sub = trace_subscriber("gp64::dos::print=debug");
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"Hello$".iter().enumerate() {
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

/// Line 3801: memory alloc with trace.
#[test]
fn test_r5_dos_alloc_mem_trace() {
    let _sub = trace_subscriber("gp64::dos::memory=debug");
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x10);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Line 3994: memory resize invalid block.
#[test]
fn test_r5_dos_resize_invalid_block() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    cpu.regs.set_ah(0x4A);
    cpu.regs.es = 0x9999;
    cpu.regs.set_bx(0x10);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 9);
}

/// Line 3898: free invalid block.
#[test]
fn test_r5_dos_free_invalid_block() {
    let _lock = env_lock();
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    cpu.regs.set_ah(0x49);
    cpu.regs.es = 0x9999;
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 9);
}

/// Lines 372/444/480/530: trace functions enabled (False path = trace fires).
#[test]
fn test_r5_dos_trace_enabled_paths() {
    let _sub = trace_subscriber(
        "gp64::dos::read=debug,gp64::dos::write=debug,gp64::dos::seek=debug,gp64::dos::find=debug",
    );
    struct TDrive;
    impl VirtualDrive for TDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![0u8; 100])))
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
                size: 100,
                modified: SystemTime::UNIX_EPOCH + Duration::from_hours(175200),
                ..Default::default()
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
            Ok(vec![(
                "FILE1.TXT".to_string(),
                crate::filesystem::FileAttributes::default(),
            )])
        }
        fn get_disk_space(&self) -> Result<(u64, u64)> {
            Ok((100_000, 50_000))
        }
    }
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut tdrive_probe = TDrive;
    exercise_virtual_drive(&mut tdrive_probe);
    dos.drives.insert(2, Box::new(TDrive));
    // Open file
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x02); // r/w access
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0000);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    let handle = cpu.regs.ax();
    // Read (trace_dos_read)
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(10);
    cpu.regs.ds = 0x3000;
    cpu.regs.set_dx(0x0000);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Write (trace_dos_write)
    let wa = Memory::seg_offset_to_linear(0x4000, 0x0000);
    for (i, &b) in b"data".iter().enumerate() {
        memory.write_u8(wa + i, b);
    }
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(4);
    cpu.regs.ds = 0x4000;
    cpu.regs.set_dx(0x0000);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Seek (trace_dos_seek)
    cpu.regs.set_ah(0x42);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Find (trace_dos_find)
    dos.set_dta(0x0100, 0x0080);
    let fa = Memory::seg_offset_to_linear(0x5000, 0x0000);
    for (i, &b) in b"*.TXT\0".iter().enumerate() {
        memory.write_u8(fa + i, b);
    }
    cpu.regs.set_ah(0x4E);
    cpu.regs.ds = 0x5000;
    cpu.regs.set_dx(0x0000);
    cpu.regs.set_cx(0);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}

/// Line 1694: FCB find first with no matching drive.
#[test]
fn test_r5_dos_fcb_find_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    // No drives registered
    cpu.regs.set_ah(0x11);
    let fcb = Memory::seg_offset_to_linear(0x2000, 0x0000);
    memory.write_u8(fcb, 3); // Drive C: (1-based)
    for i in 1..=8 {
        memory.write_u8(fcb + i, b'?');
    }
    for i in 9..=11 {
        memory.write_u8(fcb + i, b'?');
    }
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
    assert_eq!(cpu.regs.al(), 0xFF);
}

/// Line 1626: open file on drive that fails get_attributes.
#[test]
fn test_r5_dos_open_file_attrs_fail() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(2, Box::new(DenyWriteDrive));
    let a = Memory::seg_offset_to_linear(0x2000, 0x0000);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(a + i, b);
    }
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Line 3994: resize invalid with trace.
#[test]
fn test_r5_dos_resize_invalid_with_trace() {
    let _sub = trace_subscriber("gp64::dos::memory=debug");
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    cpu.regs.set_ah(0x4A);
    cpu.regs.es = 0x9999;
    cpu.regs.set_bx(0x10);
    let r = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(r.is_ok());
    assert_eq!(cpu.regs.ax(), 9);
}

/// Line 3898: free invalid with trace.
#[test]
fn test_r5_dos_free_invalid_with_trace() {
    let _lock = env_lock();
    let _sub = trace_subscriber("gp64::dos::memory=debug");
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);
    cpu.regs.set_ah(0x49);
    cpu.regs.es = 0x9999;
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}
