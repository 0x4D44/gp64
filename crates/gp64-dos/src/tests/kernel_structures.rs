use super::*;

#[test]
fn test_kernel_lol_active_clock_and_con_stay_ff() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    // activeClock (+0x08) and activeCon (+0x0C) are never written by
    // the DOS model; they stay 0xFFFFFFFF from the initial memset of the
    // InfoBlock segment. We mirror the quirk.
    let base = Memory::seg_offset_to_linear(LOL_SEGMENT, LOL_OFFSET);
    assert_eq!(memory.read_u32(base + 0x08), 0xFFFF_FFFF);
    assert_eq!(memory.read_u32(base + 0x0C), 0xFFFF_FFFF);
}

#[test]
fn test_kernel_lol_mem_alloc_scan_start_sentinel() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    // the DOS model stores the stale `DOS_MEM_START = 0x0158` in LoL+0x68
    // before the actual first MCB is computed, and never updates it.
    // We match that behavior so programs see the same sentinel.
    let base = Memory::seg_offset_to_linear(LOL_SEGMENT, LOL_OFFSET);
    assert_eq!(memory.read_u16(base + 0x68), 0x0158);
}

#[test]
fn test_kernel_sda_psp_alias_address() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    // PSP field at the segment:offset address `InfoBlock_seg:0x0330`.
    // This is the load-bearing reason the InfoBlock segment is
    // exactly 0x32 paragraphs in size: the SDA follows immediately
    // at InfoBlock_seg + 0x32 = 0x00A3, so SDA+0x10 aliases
    // InfoBlock_seg:0x0330.
    let magic_linear = Memory::seg_offset_to_linear(INFOBLOCK_SEG, 0x0330);
    let sda_psp_linear = Memory::seg_offset_to_linear(SDA_SEG, 0x0010);
    assert_eq!(magic_linear, sda_psp_linear);

    // InfoBlock at 0x0071 (linear 0x710), +0x0330 -> linear 0x0A40.
    assert_eq!(magic_linear, 0x0A40);

    // Reads zero after init (we don't yet mirror current_psp into SDA).
    assert_eq!(memory.read_u16(sda_psp_linear), 0);

    // Strengthen the alias check: a write at SDA_SEG:0x10 must be
    // observable at INFOBLOCK_SEG:0x0330 — proves the segments
    // actually overlap in physical memory and the SDA alias lookup
    // would see the SDA's current_psp through the InfoBlock segment.
    let sentinel: u16 = 0xABCD;
    memory.write_u16(sda_psp_linear, sentinel);
    let readback = memory.read_u16(magic_linear);
    assert_eq!(
        readback, sentinel,
        "SDA+0x10 must alias INFOBLOCK_SEG:0x0330 for SDA alias lookup"
    );
}

#[test]
#[allow(clippy::identity_op)] // keep structure offsets explicit for readability
fn test_kernel_sda_drive_crit_error_byte() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    let base = Memory::seg_offset_to_linear(SDA_SEG, 0);
    assert_eq!(memory.read_u8(base + 0x02), 0xFF);
    // Other SDA fields stay zero (crit_error_flag, inDOS_flag, etc.)
    assert_eq!(memory.read_u8(base + 0x00), 0);
    assert_eq!(memory.read_u8(base + 0x01), 0);
    assert_eq!(memory.read_u8(base + 0x03), 0);
}

#[test]
#[allow(clippy::identity_op)] // keep structure offsets explicit for readability
fn test_kernel_primary_sft_header_and_entries() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    // Primary SFT lives at InfoBlock_seg:00CC.
    let base = Memory::seg_offset_to_linear(INFOBLOCK_SEG, 0x00CC);
    // next_ptr -> SECSFT_SEG:0000
    assert_eq!(memory.read_u16(base + 0x00), 0x0000);
    assert_eq!(memory.read_u16(base + 0x02), SECSFT_SEG);
    // entry_count = 5
    assert_eq!(memory.read_u16(base + 0x04), 5);

    // 5 entries, each 59 bytes, header is 6 bytes. Filename DWORDs
    // at entry+0x20: CON CON CON AUX PRN.
    let names: [&[u8; 4]; 5] = [b"CON ", b"CON ", b"CON ", b"AUX ", b"PRN "];
    for (i, name) in names.iter().enumerate() {
        let entry = base + 6 + i * 59;
        assert_eq!(memory.read_u8(entry + 0x20), name[0]);
        assert_eq!(memory.read_u8(entry + 0x21), name[1]);
        assert_eq!(memory.read_u8(entry + 0x22), name[2]);
        assert_eq!(memory.read_u8(entry + 0x23), name[3]);
    }
}

#[test]
#[allow(clippy::identity_op)] // keep structure offsets explicit for readability
fn test_kernel_secondary_sft_header() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    let base = Memory::seg_offset_to_linear(SECSFT_SEG, 0);
    assert_eq!(memory.read_u32(base + 0x00), 0xFFFF_FFFF);
    // 200 total files - 5 primary SFT entries = 195
    assert_eq!(memory.read_u16(base + 0x04), DOS_FILES - 5);
    assert_eq!(memory.read_u16(base + 0x04), 195);
}

#[test]
#[allow(clippy::identity_op)] // keep structure offsets explicit for readability
fn test_kernel_device_chain_walk() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    // NUL device header (embedded in LoL at +0x22) next pointer
    // should land on CONDRV_SEG:0000.
    let nul_next_linear = Memory::seg_offset_to_linear(LOL_SEGMENT, LOL_OFFSET) + 0x22;
    let nul_next_off = memory.read_u16(nul_next_linear);
    let nul_next_seg = memory.read_u16(nul_next_linear + 2);
    assert_eq!(nul_next_off, 0x0000);
    assert_eq!(nul_next_seg, CONDRV_SEG);

    // CON.next = 0xFFFFFFFF (end of chain).
    let con_base = Memory::seg_offset_to_linear(CONDRV_SEG, 0);
    assert_eq!(memory.read_u32(con_base + 0x00), 0xFFFF_FFFF);
    // attributes = char | special | stdout | stdin
    assert_eq!(memory.read_u16(con_base + 0x04), 0x8013);
    // strategy+interrupt conflated DWORD matches the DOS model
    assert_eq!(memory.read_u32(con_base + 0x06), 0xFFFF_FFFF);

    let mut name = [0u8; 8];
    for (i, byte) in name.iter_mut().enumerate() {
        *byte = memory.read_u8(con_base + 0x0A + i);
    }
    assert_eq!(&name, b"CON     ");
}

#[test]
#[allow(clippy::identity_op)] // keep structure offsets explicit for readability
fn test_kernel_cds_stub_letter() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    let base = Memory::seg_offset_to_linear(CDS_SEG, 0);
    assert_eq!(memory.read_u8(base + 0x00), b'C');
    assert_eq!(memory.read_u8(base + 0x01), b':');
    assert_eq!(memory.read_u8(base + 0x02), b'\\');
    assert_eq!(memory.read_u8(base + 0x03), 0x00);
}

#[test]
#[allow(clippy::identity_op)] // keep structure offsets explicit for readability
fn test_kernel_fcb_sft_header() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    let base = Memory::seg_offset_to_linear(FCBSFT_SEG, 0);
    assert_eq!(memory.read_u32(base + 0x00), 0xFFFF_FFFF);
    assert_eq!(memory.read_u16(base + 0x04), DOS_FCBS);
    assert_eq!(memory.read_u16(base + 0x04), 100);
}

#[test]
#[allow(clippy::identity_op)] // keep structure offsets explicit for readability
fn test_kernel_dpb_chain_walk() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    for n in 0..(DOS_DRIVES as usize) {
        let entry = Memory::seg_offset_to_linear(DPB_SEG, 0) + n * 33;
        assert_eq!(memory.read_u8(entry + 0x00), n as u8);
        assert_eq!(memory.read_u8(entry + 0x01), n as u8);
        assert_eq!(memory.read_u16(entry + 0x02), 0x0200);
        assert_eq!(memory.read_u16(entry + 0x06), 0x0001);
        assert_eq!(memory.read_u8(entry + 0x17), 0x00);
        assert_eq!(memory.read_u16(entry + 0x1F), 0xFFFF);

        if n + 1 < DOS_DRIVES as usize {
            assert_eq!(memory.read_u16(entry + 0x19), ((n as u16) + 1) * 33);
            assert_eq!(memory.read_u16(entry + 0x1B), DPB_SEG);
        } else {
            // Last DPB: next pointer is 0xFFFFFFFF (end of chain).
            assert_eq!(memory.read_u32(entry + 0x19), 0xFFFF_FFFF);
        }
    }
}

#[test]
#[allow(clippy::identity_op)] // keep structure offsets explicit for readability
fn test_kernel_disk_buffer_head() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    let base = Memory::seg_offset_to_linear(DISKBUF_SEG, 0);
    assert_eq!(memory.read_u16(base + 0x00), 0xFFFF);
    assert_eq!(memory.read_u16(base + 0x02), 0xFFFF);
    assert_eq!(memory.read_u8(base + 0x04), 0xFF);
    assert_eq!(memory.read_u8(base + 0x0A), 0x01);
    assert_eq!(memory.read_u32(base + 0x0D), 0xFFFF_FFFF);
}

#[test]
fn test_kernel_first_mcb_segment_is_0x0700() {
    let dos = Dos::new();
    assert_eq!(dos.dos_mem.first_mcb_segment(), 0x0700);
}

#[test]
fn test_kernel_ihseg_region_is_zero() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    dos.init_kernel_structures(&mut memory);

    // DOS_IHSEG is a reserved 1-paragraph region that stays all
    // zeros — `init_kernel_structures` explicitly clears it.
    let base = Memory::seg_offset_to_linear(DOS_IHSEG, 0);
    for i in 0..0x10 {
        assert_eq!(memory.read_u8(base + i), 0);
    }
}

#[test]
fn test_int21_ah32_returns_dpb_chain_entry_for_drive_c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.init_kernel_structures(&mut memory);
    dos.drives
        .insert(2, Box::new(DiskSpaceDrive::new(1_000_000, 500_000)));

    cpu.regs.set_ah(0x32);
    cpu.regs.set_dl(3); // Drive C: (1-based)

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Drive 2 -> offset 2 * 33 = 66
    assert_eq!(cpu.regs.al(), 0x00);
    assert_eq!(cpu.regs.ds, DPB_SEG);
    assert_eq!(cpu.regs.bx(), 66);
}

#[test]
fn test_int21_ah32_returns_ff_for_invalid_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.init_kernel_structures(&mut memory);

    cpu.regs.set_ah(0x32);
    // 0xFF (1-based) wraps to drive index 0xFE — far outside the
    // mounted drive set, so the handler must report failure.
    cpu.regs.set_dl(0xFF);

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

#[test]
fn test_int21_ah52_refreshes_block_devices_count() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Initialize kernel structures: blockDevices starts at 0 per
    // the DOS model (`dos_classes.cpp:93`), and the AH=52h handler
    // refreshes it from the current mounted-drive count on every
    // call (`dos.cpp:2433`).
    dos.init_environment(&mut memory, &SoundEnvConfig::default());

    // Sanity-check the initial post-init value before any AH=52h
    // call: should be 0, not the live drive count.
    let lol_base_initial = Memory::seg_offset_to_linear(LOL_SEGMENT, LOL_OFFSET);
    assert_eq!(memory.read_u8(lol_base_initial + 0x20), 0);

    let initial_drives = dos.drives.len() as u8;

    cpu.regs.set_ah(0x52);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    let base = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
    assert_eq!(memory.read_u8(base + 0x20), initial_drives);

    // Add a new drive and re-issue AH=52h — the count should track.
    dos.drives
        .insert(5, Box::new(DiskSpaceDrive::new(1_000_000, 500_000)));
    cpu.regs.set_ah(0x52);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(memory.read_u8(base + 0x20), dos.drives.len() as u8);
}

#[test]
fn test_save_and_restore_psp_vectors() {
    let dos = Dos::new();
    let mut memory = Memory::new();
    let psp_seg = 0x2000;

    memory.write_u32(0x22 * 4, 0xAAAA_1111);
    memory.write_u32(0x23 * 4, 0xBBBB_2222);
    memory.write_u32(0x24 * 4, 0xCCCC_3333);

    dos.save_psp_vectors(&mut memory, psp_seg);

    let psp_base = Memory::seg_offset_to_linear(psp_seg, 0);
    assert_eq!(memory.read_u32(psp_base + 0x0A), 0xAAAA_1111);
    assert_eq!(memory.read_u32(psp_base + 0x0E), 0xBBBB_2222);
    assert_eq!(memory.read_u32(psp_base + 0x12), 0xCCCC_3333);

    memory.write_u32(0x22 * 4, 0x1111_0000);
    memory.write_u32(0x23 * 4, 0x2222_0000);
    memory.write_u32(0x24 * 4, 0x3333_0000);

    dos.restore_psp_vectors(&mut memory, psp_seg);

    assert_eq!(memory.read_u32(0x22 * 4), 0xAAAA_1111);
    assert_eq!(memory.read_u32(0x23 * 4), 0xBBBB_2222);
    assert_eq!(memory.read_u32(0x24 * 4), 0xCCCC_3333);
}

#[test]
fn test_int21_fcb_open_close() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Create a temp file
    let temp_dir = std::env::temp_dir().join("gp64_fcb_test1");
    let _ = std::fs::create_dir(&temp_dir);
    let test_file = temp_dir.join("FCBTEST.TXT");
    let _ = std::fs::write(&test_file, "Hello FCB World!");

    // Mount temp dir as C:
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    // Set up FCB at DS:DX
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "FCBTEST", "TXT");

    // FCB Open (AH=0F)
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
    assert_eq!(cpu.regs.al(), 0x00, "FCB Open should succeed");

    // Verify FCB was updated
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    let record_size = memory.read_u16(addr + Dos::FCB_RECORD_SIZE);
    assert_eq!(record_size, 128, "Record size should be set to 128");

    let file_size = memory.read_u32(addr + Dos::FCB_FILE_SIZE);
    assert_eq!(file_size, 16, "File size should be 16 bytes");
    let handle = dos.fcb_get_handle(&memory, Memory::seg_offset_to_linear(0x2000, 0x0100));
    assert!(
        dos.file_table.get_sft(handle).unwrap().allows_write(),
        "FCB opens are read/write and must synchronize on close"
    );

    // FCB Close (AH=10)
    cpu.regs.set_ah(0x10);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00, "FCB Close should succeed");

    // Cleanup
    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_int21_fcb_open_missing_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 5, "MISSING", "TXT");

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
}

#[test]
fn test_int21_fcb_open_get_attributes_error_uses_zero_metadata() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(FailingReadDrive));

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "ATTRERR", "TXT");

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
    assert_eq!(cpu.regs.al(), 0x00);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    assert_eq!(memory.read_u32(addr + Dos::FCB_FILE_SIZE), 0);
    assert_eq!(memory.read_u16(addr + Dos::FCB_FILE_DATE), 0);
    assert_eq!(memory.read_u16(addr + Dos::FCB_FILE_TIME), 0);
}

#[test]
fn test_int21_fcb_open_too_many_files() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(2, Box::new(FailingReadDrive));

    // Grow the JFT to its maximum and fill every user handle (5..255).
    dos.file_table.set_handle_count(255);
    for i in 0..(255 - 5) {
        let handle = dos.file_table.allocate(
            Box::new(FailingReadFile),
            format!("C:\\FILL{i}.TXT"),
            dos.current_psp(),
        );
        assert!(handle.is_some());
    }

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "TOOMANY", "TXT");

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
}

#[test]
fn test_int21_fcb_find_first_and_next() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file1 = temp_dir.path().join("MATCH01.TXT");
    let file2 = temp_dir.path().join("MATCH02.TXT");
    std::fs::write(&file1, b"A").unwrap();
    std::fs::write(&file2, b"BB").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    dos.set_dta(0x3000, 0x0000);
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "MATCH??", "TXT");

    cpu.regs.set_ah(0x11);
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

    let dta_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    // MDD-BUG-CRUCIBLE-00447: size lives at DTA+29 (directory entry +28),
    // not +28. This read used to sit at +28 and the test only compared the
    // two reads to each other, so it stayed green on the wrong offset.
    let size_first = memory.read_u32(dta_addr + 29);

    cpu.regs.set_ah(0x12);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00);

    let size_next = memory.read_u32(dta_addr + 29);
    // Assert the ACTUAL sizes, not merely that they differ: the files are
    // 1 and 2 bytes, so a wrong offset now fails instead of comparing two
    // equally-wrong reads.
    let mut sizes = [size_first, size_next];
    sizes.sort_unstable();
    assert_eq!(sizes, [1, 2], "FCB search must report the real file sizes");
}

#[test]
fn test_int21_fcb_find_first_no_match() {
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

    dos.set_dta(0x3000, 0x0000);
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "NOMATCH", "TXT");

    cpu.regs.set_ah(0x11);
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
}

#[test]
fn test_int21_fcb_find_first_no_extension() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("NOEXT");
    std::fs::write(&file_path, b"data").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    dos.set_dta(0x3000, 0x0000);
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "NOEXT", "");

    cpu.regs.set_ah(0x11);
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
fn test_int21_fcb_find_next_no_more_entries() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("ONLY.TXT");
    std::fs::write(&file_path, b"one").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    dos.set_dta(0x3000, 0x0000);
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "ONLY", "TXT");

    cpu.regs.set_ah(0x11);
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

    cpu.regs.set_ah(0x12);
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

#[test]
fn test_int21_fcb_delete_single_file_success() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("DELETE.TXT");
    std::fs::write(&file_path, b"delete").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "DELETE", "TXT");

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
    assert!(!file_path.exists());
}

#[test]
fn test_int21_fcb_delete_single_file_missing_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 1, "MISSING", "TXT");

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
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_int21_fcb_delete_wildcards_success_root() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file1 = temp_dir.path().join("DEL01.TXT");
    let file2 = temp_dir.path().join("DEL02.TXT");
    let keep = temp_dir.path().join("KEEP.TXT");
    let dir_match = temp_dir.path().join("DEL99.TXT");
    std::fs::write(&file1, b"one").unwrap();
    std::fs::write(&file2, b"two").unwrap();
    std::fs::write(&keep, b"keep").unwrap();
    std::fs::create_dir(&dir_match).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "DEL??", "TXT");

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
    assert!(!file1.exists());
    assert!(!file2.exists());
    assert!(keep.exists());
    assert!(dir_match.is_dir());
}

#[test]
fn test_int21_fcb_delete_wildcards_subdir() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let subdir = temp_dir.path().join("SUBDIR");
    std::fs::create_dir(&subdir).unwrap();
    let file1 = subdir.join("DEL01.TXT");
    let file2 = subdir.join("DEL02.TXT");
    std::fs::write(&file1, b"one").unwrap();
    std::fs::write(&file2, b"two").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );
    dos.path_resolver.set_current_dir(r"\SUBDIR");

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "DEL??", "TXT");

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
    assert!(!file1.exists());
    assert!(!file2.exists());
}

#[test]
fn test_int21_fcb_delete_wildcards_no_match() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let keep = temp_dir.path().join("KEEP.TXT");
    std::fs::write(&keep, b"keep").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "MISS??", "TXT");

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
    assert_eq!(cpu.regs.al(), 0xFF);
    assert!(keep.exists());
}

#[test]
fn test_int21_fcb_sequential_write_success() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(Vec::new())),
            "C:\\SEQWR.DAT".to_string(),
            dos.current_psp(),
        )
        .expect("handle allocation failed");

    dos.set_dta(0x3000, 0x0000);
    let dta_addr = Memory::seg_offset_to_linear(dos.dta_segment, dos.dta_offset);
    for (i, b) in [0xAAu8, 0xBB, 0xCC, 0xDD].iter().enumerate() {
        memory.write_u8(dta_addr + i, *b);
    }

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "SEQWR", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_file_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);

    cpu.regs.set_ah(0x15);
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
    assert_eq!(
        dos.fcb_get_file_size(&memory, Memory::seg_offset_to_linear(0x2000, 0x0100)),
        4
    );
}

#[test]
fn test_int21_fcb_sequential_write_partial_disk_full() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(PartialWriteFile::new(2)),
            "C:\\SEQPAR.DAT".to_string(),
            dos.current_psp(),
        )
        .expect("handle allocation failed");

    dos.set_dta(0x3000, 0x0000);
    let dta_addr = Memory::seg_offset_to_linear(dos.dta_segment, dos.dta_offset);
    for (i, b) in [0xAAu8, 0xBB, 0xCC, 0xDD].iter().enumerate() {
        memory.write_u8(dta_addr + i, *b);
    }

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "SEQPAR", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);

    cpu.regs.set_ah(0x15);
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
    assert_eq!(cpu.regs.al(), 0x01);
}

#[test]
fn test_int21_fcb_sequential_write_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(ErrorWriteFile),
            "C:\\SEQERR.DAT".to_string(),
            dos.current_psp(),
        )
        .expect("handle allocation failed");

    dos.set_dta(0x3000, 0x0000);
    let dta_addr = Memory::seg_offset_to_linear(dos.dta_segment, dos.dta_offset);
    for (i, b) in [0xAAu8, 0xBB, 0xCC, 0xDD].iter().enumerate() {
        memory.write_u8(dta_addr + i, *b);
    }

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "SEQERR", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);

    cpu.regs.set_ah(0x15);
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
    assert_eq!(cpu.regs.al(), 0x01);
}

#[test]
fn test_int21_fcb_sequential_write_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.set_dta(0x3000, 0x0000);
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "SEQBAD", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        99,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);

    cpu.regs.set_ah(0x15);
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
    assert_eq!(cpu.regs.al(), 0x01);
}

#[test]
fn test_int21_fcb_rename_single_file_success() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let old_path = temp_dir.path().join("OLDNAME.TXT");
    let new_path = temp_dir.path().join("NEWNAME.TXT");
    std::fs::write(&old_path, b"data").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    create_rename_fcb(
        &mut memory,
        0x2000,
        0x0100,
        3,
        "OLDNAME",
        "TXT",
        "NEWNAME",
        "TXT",
    );

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
    assert!(!old_path.exists());
    assert!(new_path.exists());
}

#[test]
fn test_int21_fcb_rename_wildcards_success() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file1 = temp_dir.path().join("OLD01.TXT");
    let file2 = temp_dir.path().join("OLD02.TXT");
    let dir_match = temp_dir.path().join("OLD99.TXT");
    std::fs::write(&file1, b"one").unwrap();
    std::fs::write(&file2, b"two").unwrap();
    std::fs::create_dir(&dir_match).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    create_rename_fcb(
        &mut memory,
        0x2000,
        0x0100,
        3,
        "OLD??",
        "TXT",
        "NEW??",
        "TXT",
    );

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
    assert!(!file1.exists());
    assert!(!file2.exists());
    assert!(temp_dir.path().join("NEW01.TXT").exists());
    assert!(temp_dir.path().join("NEW02.TXT").exists());
    assert!(dir_match.is_dir());
}

#[test]
fn test_int21_fcb_rename_wildcards_no_match() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let keep = temp_dir.path().join("KEEP.TXT");
    std::fs::write(&keep, b"keep").unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    create_rename_fcb(
        &mut memory,
        0x2000,
        0x0100,
        3,
        "MISS??",
        "TXT",
        "NEW??",
        "TXT",
    );

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
    assert_eq!(cpu.regs.al(), 0xFF);
    assert!(keep.exists());
}

#[test]
fn test_int21_fcb_random_read_partial_record() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(vec![1, 2, 3, 4])),
            "C:\\RAND.DAT".to_string(),
            dos.current_psp(),
        )
        .expect("handle allocation failed");

    dos.set_dta(0x3000, 0x0000);
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RAND", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 8);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);

    cpu.regs.set_ah(0x21);
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
    assert_eq!(cpu.regs.al(), 0x03);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_fcb_random_read_read_error_sets_carry() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(FailingReadFile),
            "C:\\RANDERR.DAT".to_string(),
            dos.current_psp(),
        )
        .expect("handle allocation failed");

    dos.set_dta(0x3000, 0x0000);
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RANDERR", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 8);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);

    cpu.regs.set_ah(0x21);
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
    assert_eq!(cpu.regs.al(), 0x01);
}

#[test]
fn test_int21_fcb_random_read_seek_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(SeekErrorFile),
            "C:\\RANDSEEK.DAT".to_string(),
            dos.current_psp(),
        )
        .expect("handle allocation failed");

    dos.set_dta(0x3000, 0x0000);
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RANDSEEK", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 8);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 1);

    cpu.regs.set_ah(0x21);
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
    assert_eq!(cpu.regs.al(), 0x01);
}

#[test]
fn test_int21_fcb_random_write_success() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(Vec::new())),
            "C:\\RANDWR.DAT".to_string(),
            dos.current_psp(),
        )
        .expect("handle allocation failed");

    dos.set_dta(0x3000, 0x0000);
    let dta_addr = Memory::seg_offset_to_linear(dos.dta_segment, dos.dta_offset);
    for (i, b) in [0x10u8, 0x11, 0x12, 0x13].iter().enumerate() {
        memory.write_u8(dta_addr + i, *b);
    }

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RANDWR", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);

    cpu.regs.set_ah(0x22);
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
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_fcb_random_write_partial_sets_carry() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(PartialWriteFile::new(2)),
            "C:\\RANDPAR.DAT".to_string(),
            dos.current_psp(),
        )
        .expect("handle allocation failed");

    dos.set_dta(0x3000, 0x0000);
    let dta_addr = Memory::seg_offset_to_linear(dos.dta_segment, dos.dta_offset);
    for (i, b) in [0x10u8, 0x11, 0x12, 0x13].iter().enumerate() {
        memory.write_u8(dta_addr + i, *b);
    }

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RANDPAR", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);

    cpu.regs.set_ah(0x22);
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
    assert_eq!(cpu.regs.al(), 0x01);
}

#[test]
fn test_int21_fcb_random_write_error_sets_carry() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(ErrorWriteFile),
            "C:\\RANDERR.DAT".to_string(),
            dos.current_psp(),
        )
        .expect("handle allocation failed");

    dos.set_dta(0x3000, 0x0000);
    let dta_addr = Memory::seg_offset_to_linear(dos.dta_segment, dos.dta_offset);
    for (i, b) in [0x10u8, 0x11, 0x12, 0x13].iter().enumerate() {
        memory.write_u8(dta_addr + i, *b);
    }

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RANDERR", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);

    cpu.regs.set_ah(0x22);
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
    assert_eq!(cpu.regs.al(), 0x01);
}

#[test]
fn test_int21_fcb_random_write_seek_fail_sets_carry() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(SeekErrorFile),
            "C:\\RANDSEEK.DAT".to_string(),
            dos.current_psp(),
        )
        .expect("handle allocation failed");

    dos.set_dta(0x3000, 0x0000);
    let dta_addr = Memory::seg_offset_to_linear(dos.dta_segment, dos.dta_offset);
    for (i, b) in [0x10u8, 0x11, 0x12, 0x13].iter().enumerate() {
        memory.write_u8(dta_addr + i, *b);
    }

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RANDSEEK", "DAT");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 1);

    cpu.regs.set_ah(0x22);
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
    assert_eq!(cpu.regs.al(), 0x01);
}

#[test]
fn test_int21_fcb_get_file_size_record_size_zero() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("SIZE.TXT");
    std::fs::write(&file_path, vec![0u8; 256]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "SIZE", "TXT");

    cpu.regs.set_ah(0x23);
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
    assert_eq!(
        dos.fcb_get_random_record(&memory, Memory::seg_offset_to_linear(0x2000, 0x0100)),
        2
    );
}

#[test]
fn test_int21_fcb_get_file_size_nonzero_record_size() {
    use tempfile::TempDir;

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("SIZEREC.TXT");
    std::fs::write(&file_path, vec![0u8; 128]).unwrap();

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "SIZEREC", "TXT");
    dos.fcb_set_record_size(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        64,
    );

    cpu.regs.set_ah(0x23);
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
    assert_eq!(
        dos.fcb_get_random_record(&memory, Memory::seg_offset_to_linear(0x2000, 0x0100)),
        2
    );
}

#[test]
fn test_int21_fcb_get_file_size_missing_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 1, "SIZE", "TXT");

    cpu.regs.set_ah(0x23);
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
}

#[test]
fn test_int21_fcb_close_invalid_standard_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "TEST", "TXT");
    dos.fcb_set_handle(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 3);

    cpu.regs.set_ah(0x10);
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
}

#[test]
fn test_int21_fcb_close_invalid_unallocated_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "TEST", "TXT");
    dos.fcb_set_handle(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 5);

    cpu.regs.set_ah(0x10);
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
}

#[test]
fn test_int21_fcb_open_not_found() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up FCB for non-existent file
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "NOEXIST", "XXX");

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
    assert_eq!(
        cpu.regs.al(),
        0xFF,
        "FCB Open should fail for non-existent file"
    );
}

#[test]
fn test_int21_fcb_create_file() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = std::env::temp_dir().join("gp64_fcb_create");
    let _ = std::fs::create_dir(&temp_dir);
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "FCBMAKE", "TXT");

    cpu.regs.set_ah(0x16);
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
    assert_eq!(cpu.regs.al(), 0x00, "FCB Create should succeed");

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    let handle = memory.read_u16(addr + Dos::FCB_RESERVED);
    assert!(handle > 4);

    cpu.regs.set_ah(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00, "FCB Close should succeed");

    let _ = std::fs::remove_file(temp_dir.join("FCBMAKE.TXT"));
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_int21_fcb_rename_file() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let temp_dir = std::env::temp_dir().join("gp64_fcb_rename");
    let _ = std::fs::create_dir(&temp_dir);
    let old_path = temp_dir.join("OLDNAME.TXT");
    let _ = std::fs::write(&old_path, b"rename");

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "OLDNAME", "TXT");

    let base = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NEWNAME ".iter().enumerate() {
        memory.write_u8(base + 17 + i, b);
    }
    for (i, &b) in b"TXT".iter().enumerate() {
        memory.write_u8(base + 25 + i, b);
    }

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
    assert_eq!(cpu.regs.al(), 0x00, "FCB Rename should succeed");
    assert!(temp_dir.join("NEWNAME.TXT").exists());

    let _ = std::fs::remove_file(temp_dir.join("NEWNAME.TXT"));
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_int21_fcb_set_random_record() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "RANDOM", "DAT");
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u16(addr + Dos::FCB_CURRENT_BLOCK, 2);
    memory.write_u8(addr + Dos::FCB_CURRENT_RECORD, 5);

    cpu.regs.set_ah(0x24);
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

    let random_record = memory.read_u32(addr + Dos::FCB_RANDOM_RECORD);
    assert_eq!(random_record, 2 * 128 + 5);
}

#[test]
fn test_int21_fcb_random_block_read_zero_count() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");

    cpu.regs.set_ah(0x27);
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
    assert_eq!(cpu.regs.al(), 0x00);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int21_fcb_random_block_read_partial_record() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.set_dta(0x3000, 0x0000);

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"ABCDEF".to_vec())),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x27);
    cpu.regs.set_cx(3);
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
    assert_eq!(cpu.regs.al(), 0x03);
    assert_eq!(cpu.regs.cx(), 2);
    assert_eq!(
        dos.fcb_get_random_record(&memory, Memory::seg_offset_to_linear(0x2000, 0x0100)),
        2
    );

    let dta_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    let mut out = Vec::new();
    for i in 0..6 {
        out.push(memory.read_u8(dta_addr + i));
    }
    assert_eq!(out.as_slice(), b"ABCDEF");
}

#[test]
fn test_int21_fcb_random_block_read_seek_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 1);

    let handle = dos
        .file_table
        .allocate(
            Box::new(SeekErrorFile),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x27);
    cpu.regs.set_cx(1);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int21_fcb_random_block_write_zero_count_truncates() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 2);
    dos.fcb_set_file_size(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        20,
    );

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(vec![0u8; 16])),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x28);
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
    assert_eq!(cpu.regs.al(), 0x00);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(
        dos.fcb_get_file_size(&memory, Memory::seg_offset_to_linear(0x2000, 0x0100)),
        8
    );
}

#[test]
fn test_int21_fcb_random_block_write_zero_count_set_len_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 2);
    dos.fcb_set_file_size(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        20,
    );

    let handle = dos
        .file_table
        .allocate(
            Box::new(SetLenErrorFile::new()),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x28);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(
        dos.fcb_get_file_size(&memory, Memory::seg_offset_to_linear(0x2000, 0x0100)),
        20,
        "failed truncate must preserve the FCB's prior file size"
    );
}

#[test]
fn test_int21_fcb_random_block_write_partial_sets_status() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.fcb_set_file_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);
    dos.set_dta(0x3000, 0x0000);

    let dta_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    for i in 0..12 {
        memory.write_u8(dta_addr + i, b'X');
    }

    let handle = dos
        .file_table
        .allocate(
            Box::new(PartialWriteFile::new(5)),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x28);
    cpu.regs.set_cx(3);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(
        dos.fcb_get_random_record(&memory, Memory::seg_offset_to_linear(0x2000, 0x0100)),
        1
    );
    assert_eq!(
        dos.fcb_get_file_size(&memory, Memory::seg_offset_to_linear(0x2000, 0x0100)),
        5
    );
}

#[test]
fn test_int21_fcb_random_block_write_error_sets_carry() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 0);

    let handle = dos
        .file_table
        .allocate(
            Box::new(ErrorWriteFile),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x28);
    cpu.regs.set_cx(2);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int21_fcb_random_block_write_seek_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "BLOCK", "DAT");
    dos.fcb_set_record_size(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 4);
    dos.fcb_set_random_record(&mut memory, Memory::seg_offset_to_linear(0x2000, 0x0100), 1);

    let handle = dos
        .file_table
        .allocate(
            Box::new(SeekErrorFile),
            "C:\\BLOCK.DAT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");
    dos.fcb_set_handle(
        &mut memory,
        Memory::seg_offset_to_linear(0x2000, 0x0100),
        handle,
    );

    cpu.regs.set_ah(0x28);
    cpu.regs.set_cx(1);
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
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_int21_fcb_sequential_read() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Create a temp file with known content
    let temp_dir = std::env::temp_dir().join("gp64_fcb_test2");
    let _ = std::fs::create_dir(&temp_dir);
    let test_file = temp_dir.join("FCBREAD.TXT");
    let test_data = b"This is test data for FCB sequential read testing.";
    let _ = std::fs::write(&test_file, test_data);

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    // Set up FCB
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "FCBREAD", "TXT");

    // Set DTA
    dos.set_dta(0x3000, 0x0000);

    // Open file
    cpu.regs.set_ah(0x0F);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00, "Open should succeed");

    // Sequential read (AH=14)
    cpu.regs.set_ah(0x14);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Verify DTA contains data (first 51 bytes = data + zeros for padding to 128)
    let dta_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    let first_byte = memory.read_u8(dta_addr);
    assert_eq!(first_byte, b'T', "First byte should be 'T'");

    // Close
    cpu.regs.set_ah(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Cleanup
    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_int21_set_dta_near_segment_boundary() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x1A);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0xFF90);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(dos.dta_segment, 0x2000);
    assert_eq!(dos.dta_offset, 0xFF90);
}

#[test]
fn test_int21_fcb_random_read() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Create a temp file with multiple records
    let temp_dir = std::env::temp_dir().join("gp64_fcb_test3");
    let _ = std::fs::create_dir(&temp_dir);
    let test_file = temp_dir.join("FCBRAND.DAT");

    // Create file with 256 bytes (2 records of 128 bytes each)
    let mut test_data = vec![0u8; 256];
    test_data[0] = b'A'; // First record starts with 'A'
    test_data[128] = b'B'; // Second record starts with 'B'
    let _ = std::fs::write(&test_file, &test_data);

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    // Set up FCB
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "FCBRAND", "DAT");

    // Set DTA
    dos.set_dta(0x3000, 0x0000);

    // Open file
    cpu.regs.set_ah(0x0F);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.al(), 0x00, "Open should succeed");

    // Set random record number to 1 (second record)
    let fcb_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u32(fcb_addr + Dos::FCB_RANDOM_RECORD, 1);

    // Random read (AH=21)
    cpu.regs.set_ah(0x21);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x00, "Random read should succeed");

    // Verify DTA contains second record
    let dta_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    let first_byte = memory.read_u8(dta_addr);
    assert_eq!(
        first_byte, b'B',
        "First byte should be 'B' (from second record)"
    );

    // Close
    cpu.regs.set_ah(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Cleanup
    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_int21_fcb_random_read_eof() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Create a small temp file
    let temp_dir = std::env::temp_dir().join("gp64_fcb_test4");
    let _ = std::fs::create_dir(&temp_dir);
    let test_file = temp_dir.join("FCBEOF.DAT");
    let _ = std::fs::write(&test_file, b"Short");

    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp_dir.clone())),
    );

    // Set up FCB
    create_test_fcb(&mut memory, 0x2000, 0x0100, 3, "FCBEOF", "DAT");

    // Set DTA
    dos.set_dta(0x3000, 0x0000);

    // Open file
    cpu.regs.set_ah(0x0F);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Set random record number to 100 (way past EOF)
    let fcb_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    memory.write_u32(fcb_addr + Dos::FCB_RANDOM_RECORD, 100);

    // Random read (AH=21) - should return EOF
    cpu.regs.set_ah(0x21);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(
        cpu.regs.al(),
        0x01,
        "Random read past EOF should return AL=1"
    );

    // Close
    cpu.regs.set_ah(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    // Cleanup
    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_dir(&temp_dir);
}

#[test]
fn test_int21_fcb_extended() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    // Create an extended FCB (starts with 0xFF)
    let segment = 0x2000u16;
    let offset = 0x0100u16;
    let ext_addr = Memory::seg_offset_to_linear(segment, offset);

    // Extended FCB header
    memory.write_u8(ext_addr, 0xFF); // Extended FCB marker
    memory.write_u8(ext_addr + 1, 0); // Reserved
    memory.write_u8(ext_addr + 2, 0);
    memory.write_u8(ext_addr + 3, 0);
    memory.write_u8(ext_addr + 4, 0);
    memory.write_u8(ext_addr + 5, 0);
    memory.write_u8(ext_addr + 6, 0x20); // Attribute (archive)

    // Normal FCB follows at offset+7
    let fcb_addr = ext_addr + 7;
    memory.write_u8(fcb_addr + Dos::FCB_DRIVE, 3);

    // Filename "EXTENDED"
    for (i, &b) in b"EXTENDED".iter().enumerate() {
        memory.write_u8(fcb_addr + Dos::FCB_FILENAME + i, b);
    }
    // Extension "TXT"
    for (i, &b) in b"TXT".iter().enumerate() {
        memory.write_u8(fcb_addr + Dos::FCB_EXTENSION + i, b);
    }

    // Verify extended FCB detection
    assert!(Dos::fcb_is_extended(
        &memory,
        Memory::seg_offset_to_linear(segment, offset)
    ));
    assert_eq!(
        Dos::fcb_base_addr(&memory, Memory::seg_offset_to_linear(segment, offset)),
        Memory::seg_offset_to_linear(segment, offset) + 7
    );

    // Verify filename reading works with extended FCB
    let filename = dos.fcb_read_filename(&memory, Memory::seg_offset_to_linear(segment, offset));
    assert_eq!(filename, "EXTENDED.TXT");
}

#[test]
fn test_int21_get_list_of_lists_ah52() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Initialize the DOS environment (which initializes LoL)
    dos.init_environment(&mut memory, &SoundEnvConfig::default());

    cpu.regs.set_ah(0x52);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Check that ES:BX points to the List of Lists
    assert_eq!(cpu.regs.es, super::LOL_SEGMENT);
    assert_eq!(cpu.regs.es_cache.base, u32::from(super::LOL_SEGMENT) << 4);
    assert_eq!(cpu.regs.bx(), super::LOL_OFFSET);

    // Verify the LoL structure is properly initialized
    let lol_base = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());

    // First MCB segment should be at offset -2
    let first_mcb = memory.read_u16(lol_base.wrapping_sub(2));
    assert_eq!(first_mcb, dos.dos_mem.first_mcb_segment());

    // Max bytes per sector at +0x10
    assert_eq!(memory.read_u16(lol_base + 0x10), 0x0200);

    // Number of block devices at +0x20 (refreshed by AH=52h)
    assert_eq!(memory.read_u8(lol_base + 0x20), dos.drives.len() as u8);

    // LASTDRIVE at +0x21 (the DOS model default: 1)
    assert_eq!(memory.read_u8(lol_base + 0x21), super::DOS_LASTDRIVE);

    // NUL device name at +0x2C (after next ptr, attributes, strategy, interrupt)
    let mut nul_name = [0u8; 8];
    for (i, byte) in nul_name.iter_mut().enumerate() {
        *byte = memory.read_u8(lol_base + 0x2C + i);
    }
    assert_eq!(&nul_name, b"NUL     ");
}
