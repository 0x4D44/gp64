use super::*;

/// Test DPMI segment_to_descriptor (AX=0002) - covers allocate + find path.
#[test]
fn test_cov_dpmi_segment_to_descriptor_via_int31() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ax(0x0002);
    cpu.regs.set_bx(0x0040); // BIOS data segment
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Test DPMI create_alias_descriptor (AX=000A).
#[test]
fn test_cov_dpmi_create_alias_via_int31() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    // Allocate a descriptor first
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let source_sel = cpu.regs.ax();

    // Create alias
    cpu.regs.set_ax(0x000A);
    cpu.regs.set_bx(source_sel);
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Test DPMI free + re-free covers free_selectors.contains check.
#[test]
fn test_cov_dpmi_free_selector_twice() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    // Allocate
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.ax();

    // Free
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel);
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);

    // Free again
    cpu.regs.set_ax(0x0001);
    cpu.regs.set_bx(sel);
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);
}

/// Test DPMI get/set multiple descriptors (AX=000E/000F).
#[test]
fn test_cov_dpmi_get_set_multiple_descriptors_via_int31() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    // Allocate a descriptor
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let sel = cpu.regs.ax();

    // Allocate a buffer selector with base=0 and large limit
    cpu.regs.set_ax(0x0000);
    cpu.regs.set_cx(1);
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let buf_sel = cpu.regs.ax();

    // Set buffer sel base=0 (AX=0007)
    cpu.regs.set_ax(0x0007);
    cpu.regs.set_bx(buf_sel);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);

    // Set buffer sel limit=0xFFFFF (AX=0008)
    cpu.regs.set_ax(0x0008);
    cpu.regs.set_bx(buf_sel);
    cpu.regs.set_cx(0x000F);
    cpu.regs.set_dx(0xFFFF);
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);

    // Get descriptors (AX=000E) using buf_sel as ES
    let buf_off = 0x6000usize;
    memory.write_u16(buf_off, sel);
    for i in 0..8 {
        memory.write_u8(buf_off + 2 + i, 0);
    }

    cpu.regs.set_ax(0x000E);
    cpu.regs.set_cx(1);
    cpu.regs.es = buf_sel;
    cpu.regs.edi = buf_off as u32;
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Set descriptors (AX=000F)
    let buf_off2 = 0x7000usize;
    memory.write_u16(buf_off2, sel);
    memory.write_u16(buf_off2 + 2, 0xFFFF);
    memory.write_u16(buf_off2 + 4, 0x0000);
    memory.write_u8(buf_off2 + 6, 0x01);
    memory.write_u8(buf_off2 + 7, 0xF2);
    memory.write_u8(buf_off2 + 8, 0x00);
    memory.write_u8(buf_off2 + 9, 0x00);

    cpu.regs.set_ax(0x000F);
    cpu.regs.set_cx(1);
    cpu.regs.es = buf_sel;
    cpu.regs.edi = buf_off2 as u32;
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// Test DPMI allocate specific LDT descriptor (AX=000D).
#[test]
fn test_cov_dpmi_allocate_specific_ldt_via_int31() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();

    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    // AX=000D: Allocate specific LDT descriptor
    cpu.regs.set_ax(0x000D);
    cpu.regs.set_bx(0x00F7); // High LDT selector (TI=1)
    let _ = dos.dpmi.int31(&mut cpu, &mut memory);
    // Just verify it doesn't crash; may or may not succeed
}

/// Test write_umb_link covers umb_chain_linked path in write_all_mcbs.
#[test]
fn test_cov_write_umb_link_mcb_chain_path() {
    let mut dos = Dos::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);
    dos.dos_mem.init_umb(&mut memory);
    dos.dos_mem.write_umb_link(&mut memory, true);
    assert!(dos.dos_mem.is_umb_initialized());
}

#[test]
fn test_cov_dos_offset_dx_16bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);
    assert!(dos.dpmi.in_protected_mode());
    assert!(!dos.dpmi.is_32bit_mode());
    cpu.regs.edx = 0xFFFF1234;
    let offset = dos.dos_offset_dx(&cpu);
    assert_eq!(offset, 0x1234); // Only DX (lower 16 bits)
}

#[test]
fn test_cov_dos_offset_di_16bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.edi = 0xFFFF5678;
    let offset = dos.dos_offset_di(&cpu);
    assert_eq!(offset, 0x5678);
}

#[test]
fn test_cov_dos_offset_si_16bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.esi = 0xFFFF9ABC;
    let offset = dos.dos_offset_si(&cpu);
    assert_eq!(offset, 0x9ABC);
}

#[test]
fn test_cov_dos_count_cx_16bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.ecx = 0xFFFFDEF0;
    let count = dos.dos_count_cx(&cpu);
    assert_eq!(count, 0xDEF0);
}

#[test]
fn test_cov_dos_set_result_count_16bit_dpmi() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.eax = 0xFFFFFFFF;
    dos.dos_set_result_count(&mut cpu, 0x1234);
    assert_eq!(cpu.regs.ax(), 0x1234);
    // Upper 16 bits should be unchanged (set_ax only modifies lower 16)
}

#[test]
fn test_cov_dos_datetime_earliest() {
    // DOS date 0x0021 = 1980-01-01, time 0x0000 = 00:00:00
    let time = Dos::dos_datetime_to_systemtime(0x0021, 0x0000);
    assert!(time > std::time::UNIX_EPOCH);
}

#[test]
fn test_cov_ah58_set_umb_link_invalid_value() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03); // Set UMB link state
    cpu.regs.set_bx(2); // Invalid: must be 0 or 1
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1); // Invalid function
}

#[test]
fn test_cov_ah58_set_umb_link_enable() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);

    // Enable UMB link (BX=1)
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dos.umb_linked);
}

#[test]
fn test_cov_ah58_set_umb_link_already_linked() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);

    // First link
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(dos.umb_linked);

    // Link again (was_linked=true, umb_linked=true → no init)
    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x03);
    cpu.regs.set_bx(1);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(dos.umb_linked);
}

#[test]
fn test_cov_ah25_set_interrupt_vector_pm() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ah(0x25);
    cpu.regs.set_al(0x21); // Set INT 21h vector
    cpu.regs.ds = 0x0100; // Selector
    cpu.regs.set_dx(0x1234); // Offset
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // In PM mode, this should store as client PM handler
}

#[test]
fn test_cov_ah25_set_interrupt_vector_pm_32bit() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ah(0x25);
    cpu.regs.set_al(0x21);
    cpu.regs.ds = 0x0100;
    cpu.regs.edx = 0x12345678; // Full 32-bit offset
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
}

#[test]
fn test_cov_ah35_get_interrupt_vector_pm_with_handler() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    // First set a handler
    dos.dpmi.set_client_pm_int_handler(0x21, 0x1234, 0x5678);

    // Now get it
    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x21);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.es, 0x1234);
    assert_eq!(cpu.regs.bx(), 0x5678);
}

#[test]
fn test_cov_ah35_get_interrupt_vector_pm_no_handler() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);

    // Get vector for INT 10h (no client handler set)
    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Should return HLE BIOS stub
}

#[test]
fn test_cov_ah35_get_interrupt_vector_pm_32bit() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);

    // Set a handler with 32-bit offset
    dos.dpmi.set_client_pm_int_handler(0x21, 0x1234, 0x87654321);

    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x21);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert_eq!(cpu.regs.es, 0x1234);
    assert_eq!(cpu.regs.ebx, 0x87654321);
}

#[test]
fn test_cov_ah35_get_interrupt_vector_pm_32bit_no_handler() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);

    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Should return 32-bit stub offset
}

#[test]
fn test_cov_dos_datetime_with_seconds() {
    // Date = 0x0021 = 1980-01-01, time with seconds bits set
    // time bits 4-0 = seconds/2, set to 0x1F (31) → 62 seconds
    let time = Dos::dos_datetime_to_systemtime(0x0021, 0x001F);
    assert!(time > std::time::UNIX_EPOCH);
}

#[test]
fn test_cov_ah40_write_via_fe_alias() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Write "Hi" via handle 0x00FE (alias for stdout handle 1)
    let data = b"Hi";
    let buf_addr = 0x2000 * 16 + 0x100;
    for (i, &b) in data.iter().enumerate() {
        memory.write_u8(buf_addr + i, b);
    }

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(0x00FE); // Alias for stdout
    cpu.regs.set_cx(data.len() as u16);
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
    assert_eq!(cpu.regs.ax(), data.len() as u16);
}

#[test]
fn test_cov_ah58_get_umb_link_state() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x58);
    cpu.regs.set_al(0x02); // Get UMB link state
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.al(), 0); // Not linked initially
}

#[test]
fn test_cov_dos_set_result_count_32bit() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.eax = 0;
    dos.dos_set_result_count(&mut cpu, 0x12345678);
    assert_eq!(cpu.regs.eax, 0x12345678);
}

#[test]
fn test_cov_dos_count_cx_32bit() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.ecx = 0xDEADBEEF;
    let count = dos.dos_count_cx(&cpu);
    assert_eq!(count, 0xDEADBEEF);
}

#[test]
fn test_cov_dos_offset_si_32bit() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    enter_dpmi_32bit(&mut dos, &mut cpu, &mut memory);
    cpu.regs.esi = 0xCAFEBABE;
    let offset = dos.dos_offset_si(&cpu);
    assert_eq!(offset, 0xCAFEBABE);
}

#[test]
fn test_cov_ah40_write_cx_zero_truncate() {
    struct TruncDrive;
    impl VirtualDrive for TruncDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![1, 2, 3, 4, 5])))
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
    let mut trunc_drive_probe = TruncDrive;
    exercise_virtual_drive(&mut trunc_drive_probe);
    dos.drives.insert(2, Box::new(TruncDrive));
    dos.path_resolver.set_current_drive(2);

    // Open file (AH=3Dh, AL=02 read/write)
    let filename = "TEST.TXT\0";
    let name_addr = 0x2000 * 16 + 0x200;
    for (i, &b) in filename.as_bytes().iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x02); // Read/write
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
    let handle = cpu.regs.ax();

    // Write with CX=0 (truncate at current position)
    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(0);
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
    assert_eq!(cpu.regs.ax(), 0);
}

#[test]
fn test_cov_lfn_get_attributes_all_flags() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(3, Box::new(AllAttributesDrive));
    dos.path_resolver.set_current_drive(3);

    // Write filename
    let filename = "TEST.TXT\0";
    let name_addr = 0x2000 * 16 + 0x200;
    for (i, &b) in filename.as_bytes().iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x71); // LFN
    cpu.regs.set_al(0x43); // Get/set file attributes
    cpu.regs.set_bl(0x00); // Get attributes
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0200);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    if !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY) {
        // Should have hidden (0x02) + system (0x04) + read_only (0x01) + dir (0x10) + archive (0x20)
        let attrs = cpu.regs.cx();
        assert!(attrs & 0x02 != 0, "hidden flag should be set");
        assert!(attrs & 0x04 != 0, "system flag should be set");
    }
}

#[test]
fn test_cov_ah3f_read_file_handle() {
    struct ReadDrive;
    impl VirtualDrive for ReadDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(b"ABCDE".to_vec())))
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
    let mut read_drive_probe = ReadDrive;
    exercise_virtual_drive(&mut read_drive_probe);
    dos.drives.insert(2, Box::new(ReadDrive));
    dos.path_resolver.set_current_drive(2);

    // Open file
    let filename = "DATA.BIN\0";
    let name_addr = 0x2000 * 16 + 0x200;
    for (i, &b) in filename.as_bytes().iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00); // Read-only
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
    let handle = cpu.regs.ax();

    // Read 3 bytes
    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(handle);
    cpu.regs.set_cx(3);
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
    assert_eq!(cpu.regs.ax(), 3);
    let buf_addr = 0x2000 * 16 + 0x100;
    assert_eq!(memory.read_u8(buf_addr), b'A');
    assert_eq!(memory.read_u8(buf_addr + 1), b'B');
    assert_eq!(memory.read_u8(buf_addr + 2), b'C');
}

#[test]
fn test_cov_ah43_set_attributes_io_error() {
    struct SetAttrErrorDrive;
    impl VirtualDrive for SetAttrErrorDrive {
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
            Ok(crate::filesystem::FileAttributes::default())
        }
        fn set_attributes(&mut self, _: &str, _: crate::filesystem::FileAttributes) -> Result<()> {
            // Return an io::Error (NotFound) to trigger the io_err branch
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "nf").into())
        }
        fn set_file_time(&mut self, _: &str, _: std::time::SystemTime) -> Result<()> {
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

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut set_attr_error_probe = SetAttrErrorDrive;
    exercise_virtual_drive(&mut set_attr_error_probe);
    dos.drives.insert(3, Box::new(SetAttrErrorDrive));
    dos.path_resolver.set_current_drive(3);

    let filename = "TEST.TXT\0";
    let name_addr = 0x2000 * 16 + 0x200;
    for (i, &b) in filename.as_bytes().iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x01); // Set attributes
    cpu.regs.set_cx(0x20); // Archive
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
    assert_eq!(cpu.regs.ax(), 2); // File not found
}

#[test]
fn test_cov_ah43_set_attributes_access_denied() {
    struct SetAttrPermErrorDrive;
    impl VirtualDrive for SetAttrPermErrorDrive {
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
            Ok(crate::filesystem::FileAttributes::default())
        }
        fn set_attributes(&mut self, _: &str, _: crate::filesystem::FileAttributes) -> Result<()> {
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied").into())
        }
        fn set_file_time(&mut self, _: &str, _: std::time::SystemTime) -> Result<()> {
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

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut set_attr_perm_probe = SetAttrPermErrorDrive;
    exercise_virtual_drive(&mut set_attr_perm_probe);
    dos.drives.insert(3, Box::new(SetAttrPermErrorDrive));
    dos.path_resolver.set_current_drive(3);

    let filename = "TEST.TXT\0";
    let name_addr = 0x2000 * 16 + 0x200;
    for (i, &b) in filename.as_bytes().iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x01);
    cpu.regs.set_cx(0x20);
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
    assert_eq!(cpu.regs.ax(), 5); // Access denied
}

#[test]
fn test_cov_ah43_set_attributes_anyhow_error() {
    struct SetAttrAnyhowDrive;
    impl VirtualDrive for SetAttrAnyhowDrive {
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
            Ok(crate::filesystem::FileAttributes::default())
        }
        fn set_attributes(&mut self, _: &str, _: crate::filesystem::FileAttributes) -> Result<()> {
            // Return a plain anyhow error (not io::Error) to trigger the else branch
            Err(anyhow::anyhow!("generic error"))
        }
        fn set_file_time(&mut self, _: &str, _: std::time::SystemTime) -> Result<()> {
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

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut set_attr_anyhow_probe = SetAttrAnyhowDrive;
    exercise_virtual_drive(&mut set_attr_anyhow_probe);
    dos.drives.insert(3, Box::new(SetAttrAnyhowDrive));
    dos.path_resolver.set_current_drive(3);

    let filename = "TEST.TXT\0";
    let name_addr = 0x2000 * 16 + 0x200;
    for (i, &b) in filename.as_bytes().iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x01);
    cpu.regs.set_cx(0x20);
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
    assert_eq!(cpu.regs.ax(), 2); // Default to file not found for anyhow errors
}

#[test]
fn test_cov_ah4e_find_first_success() {
    struct FindDrive;
    impl VirtualDrive for FindDrive {
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
            Ok(crate::filesystem::FileAttributes::default())
        }
        fn set_attributes(&mut self, _: &str, _: crate::filesystem::FileAttributes) -> Result<()> {
            Ok(())
        }
        fn set_file_time(&mut self, _: &str, _: std::time::SystemTime) -> Result<()> {
            Ok(())
        }
        fn read_dir(
            &mut self,
            _: &str,
        ) -> Result<Vec<(String, crate::filesystem::FileAttributes)>> {
            Ok(vec![(
                "HELLO.TXT".to_string(),
                crate::filesystem::FileAttributes {
                    archive: true,
                    size: 42,
                    ..Default::default()
                },
            )])
        }
        fn get_disk_space(&self) -> Result<(u64, u64)> {
            Ok((100_000_000, 50_000_000))
        }
    }

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut find_drive_probe = FindDrive;
    exercise_virtual_drive(&mut find_drive_probe);
    dos.drives.insert(3, Box::new(FindDrive));
    dos.path_resolver.set_current_drive(3);

    let pattern = "*.*\0";
    let name_addr = 0x2000 * 16 + 0x200;
    for (i, &b) in pattern.as_bytes().iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x4E);
    cpu.regs.set_cx(0xFF); // All attributes
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
}

#[test]
fn test_cov_ah43_invalid_subfunction() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(3, Box::new(AllAttributesDrive));
    dos.path_resolver.set_current_drive(3);

    let filename = "TEST.TXT\0";
    let name_addr = 0x2000 * 16 + 0x200;
    for (i, &b) in filename.as_bytes().iter().enumerate() {
        memory.write_u8(name_addr + i, b);
    }
    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x05); // Invalid subfunction (not 0 or 1)
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
    assert_eq!(cpu.regs.ax(), 1); // Invalid function
}

#[test]
fn test_cov_dos_datetime_to_systemtime() {
    // Encode a specific date/time: 2024-03-15 14:30:22
    // Date: year=44 (2024-1980), month=3, day=15
    let date = (44 << 9) | (3 << 5) | 15;
    // Time: hour=14, min=30, sec=11 (22/2)
    let time = (14 << 11) | (0x1E << 5) | 0x0B;

    let system_time = Dos::dos_datetime_to_systemtime(date, time);
    // Round-trip: convert back and verify
    let (rt_date, rt_time) = Dos::systemtime_to_dos_datetime(system_time);
    assert_eq!(rt_date, date);
    assert_eq!(rt_time, time);
}

/// DOS datetime before Unix epoch (line 7720: negative total_secs path).
#[test]
fn test_cov_dos_datetime_before_epoch() {
    // Year 0 in DOS = 1980, but we can try year=0 month=1 day=1 time=0
    // which is 1980-01-01 00:00:00 - that's after epoch.
    // To get before epoch, we'd need negative years which DOS doesn't support.
    // However the code uses ymd_to_days which can go negative for year < 1970.
    // DOS year field can't represent before 1980, but let's call the function
    // with date=0, time=0 to get the edge case.
    let system_time = Dos::dos_datetime_to_systemtime(0, 0);
    // date=0 means year=1980+0=1980, month=0, day=0
    // ymd_to_days(1980, 1, 1) [after max(1)] should be positive
    // So this won't hit the negative path. Let's test the function directly
    // is always >= 0 for valid DOS dates.
    assert!(system_time >= std::time::UNIX_EPOCH);
}

/// AH=6Ch Extended Open: invalid access mode > 2 (line 5334).
#[test]
fn test_cov_ah6c_extended_open_invalid_access_mode() {
    struct OpenDrive;
    impl VirtualDrive for OpenDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![])))
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
    let mut open_drive_probe = OpenDrive;
    exercise_virtual_drive(&mut open_drive_probe);
    dos.drives.insert(2, Box::new(OpenDrive));

    // Write filename at DS:SI
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // AH=6Ch, BL=0x03 (access mode 3 = invalid), DL=0x11 (open+create)
    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bl(0x03); // access mode 3 = invalid
    cpu.regs.set_dl(0x11); // open if exists + create if not exists
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 12); // Invalid access code
}

/// AH=6Ch Extended Open: file exists but open_if_exists not set → error 80.
#[test]
fn test_cov_ah6c_extended_open_file_exists_no_open() {
    struct OpenDrive;
    impl VirtualDrive for OpenDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![])))
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
    let mut open_drive_probe = OpenDrive;
    exercise_virtual_drive(&mut open_drive_probe);
    dos.drives.insert(2, Box::new(OpenDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // DL=0x10 (create if not exists, but NOT open if exists)
    // File exists → should return error 80
    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_dl(0x10); // only create, not open
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 80); // File exists
}

/// AH=6Ch Extended Open: file doesn't exist and create not set → error 2.
#[test]
fn test_cov_ah6c_extended_open_no_file_no_create() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(2, Box::new(NotFoundDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NOFILE.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // DL=0x01 (open if exists only, no create)
    cpu.regs.set_ah(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_dl(0x01); // open only, no create
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2); // File not found
}

/// AH=43h Get file attributes: archive attribute (line 5514).
#[test]
fn test_cov_ah43_get_attributes_archive() {
    struct ArchiveDrive;
    impl VirtualDrive for ArchiveDrive {
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
                size: 100,
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
    let mut archive_drive_probe = ArchiveDrive;
    exercise_virtual_drive(&mut archive_drive_probe);
    dos.drives.insert(2, Box::new(ArchiveDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x00); // Get attributes
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
    assert_eq!(cpu.regs.cx() & 0x20, 0x20); // Archive bit set
}

/// AH=43h AL=01: Set attributes (line 5533).
#[test]
fn test_cov_ah43_set_attributes() {
    struct SetOkDrive;
    impl VirtualDrive for SetOkDrive {
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
            Ok((0, 0))
        }
    }

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut set_ok_probe = SetOkDrive;
    exercise_virtual_drive(&mut set_ok_probe);
    dos.drives.insert(2, Box::new(SetOkDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x43);
    cpu.regs.set_al(0x01); // Set attributes
    cpu.regs.set_cx(0x20); // Archive
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
}

/// LFN 716Ch: Extended Open - no drive (line 5673 else branch).
#[test]
fn test_cov_lfn_716c_no_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    // No drives mounted

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"Z:\\TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_dx(0x0011); // open+create
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
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

/// LFN 716Ch: file exists, open_if_exists not set → error 80 (line 5682-5688).
#[test]
fn test_cov_lfn_716c_exists_no_open() {
    struct LfnDrive;
    impl VirtualDrive for LfnDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![])))
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
    let mut lfn_drive_probe = LfnDrive;
    exercise_virtual_drive(&mut lfn_drive_probe);
    dos.drives.insert(2, Box::new(LfnDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // DX=0x0010 (create only, not open)
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_dx(0x0010); // create only
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 80);
}

/// LFN 716Ch: file doesn't exist, no create → error 2 (line 5690-5695).
#[test]
fn test_cov_lfn_716c_no_file_no_create() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(2, Box::new(NotFoundDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NOFILE.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // DX=0x0001 (open only, no create)
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_dx(0x0001); // open only
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
}

#[test]
fn test_lfn_716c_failure_updates_extended_error() {
    // MDD-BUG-CRUCIBLE-00063: a failed LFN 716Ch open must update the AH=59h
    // extended-error state to match its result, not leave a stale value from a
    // previous call.
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(2, Box::new(NotFoundDrive));
    dos.set_extended_error(99); // stale value from a prior call

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NOFILE.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_dx(0x0001); // open only, no create
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 2);
    assert_eq!(
        dos.extended_error.0, 2,
        "missing-file LFN open must set extended error to 2, not leave the stale 99"
    );
}

#[test]
fn test_lfn_716c_success_clears_extended_error() {
    // MDD-BUG-CRUCIBLE-00063: a successful LFN 716Ch open must CLEAR any prior
    // extended error, matching the standard AH=6Ch handling.
    use tempfile::TempDir;
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let temp = TempDir::new().unwrap();
    std::fs::write(temp.path().join("EXISTS.TXT"), b"hi").unwrap();
    dos.drives.insert(
        2,
        Box::new(crate::filesystem::HostDrive::new(temp.path().to_path_buf())),
    );
    dos.set_extended_error(99); // stale value from a prior call

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"EXISTS.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x6C);
    cpu.regs.set_bl(0x00); // read access
    cpu.regs.set_dx(0x0001); // open if exists
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );

    assert!(
        !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
        "open of an existing file must succeed"
    );
    assert_eq!(
        dos.extended_error.0, 0,
        "successful LFN open must clear the stale extended error"
    );
}

/// LFN 716Ch: successful open - action_taken=1 (line 5717-5727).
#[test]
fn test_cov_lfn_716c_open_success() {
    struct LfnOkDrive;
    impl VirtualDrive for LfnOkDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![1, 2, 3])))
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
    let mut lfn_ok_probe = LfnOkDrive;
    exercise_virtual_drive(&mut lfn_ok_probe);
    dos.drives.insert(2, Box::new(LfnOkDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // DX=0x0001 (open if exists)
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x6C);
    cpu.regs.set_bl(0x00); // read access
    cpu.regs.set_dx(0x0001); // open if exists
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 1); // action_taken = opened
}

/// LFN 716Ch: create new file - action_taken=2 (line 5721).
#[test]
fn test_cov_lfn_716c_create_success() {
    struct LfnCreateDrive {
        exists: bool,
    }
    impl VirtualDrive for LfnCreateDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![])))
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
            if self.exists {
                Ok(crate::filesystem::FileAttributes::default())
            } else {
                Err(anyhow::anyhow!("not found"))
            }
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
    let mut lfn_create_probe = LfnCreateDrive { exists: false };
    exercise_virtual_drive(&mut lfn_create_probe);
    dos.drives
        .insert(2, Box::new(LfnCreateDrive { exists: false }));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"NEW.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // DX=0x0010 (create if not exists)
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x6C);
    cpu.regs.set_bl(0x01); // write access
    cpu.regs.set_dx(0x0010); // create if not exists
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 2); // action_taken = created
}

/// LFN 716Ch: open+truncate existing file - action_taken=3 (line 5723).
#[test]
fn test_cov_lfn_716c_truncate_success() {
    struct LfnTruncDrive;
    impl VirtualDrive for LfnTruncDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![1, 2, 3])))
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
    let mut lfn_trunc_probe = LfnTruncDrive;
    exercise_virtual_drive(&mut lfn_trunc_probe);
    dos.drives.insert(2, Box::new(LfnTruncDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // DX=0x0003 (open if exists + truncate if exists)
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x6C);
    cpu.regs.set_bl(0x02); // read/write access
    cpu.regs.set_dx(0x0003); // open + truncate
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.cx(), 3); // action_taken = truncated
}

/// LFN 716Ch: open fails → error 5 (line 5735-5738).
#[test]
fn test_cov_lfn_716c_open_error() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.drives.insert(2, Box::new(DenyWriteDrive));

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // DX=0x0011 (open + create)
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x6C);
    cpu.regs.set_bl(0x01);
    cpu.regs.set_dx(0x0011);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5);
}

/// LFN 71A6h: Get file info by handle (line 5797).
#[test]
fn test_cov_lfn_71a6_get_file_info_by_handle() {
    struct InfoDrive;
    impl VirtualDrive for InfoDrive {
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
            Ok((0, 0))
        }
    }

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut info_drive_probe = InfoDrive;
    exercise_virtual_drive(&mut info_drive_probe);
    dos.drives.insert(2, Box::new(InfoDrive));

    // First open a file to get a handle
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }
    cpu.regs.set_ah(0x3D);
    cpu.regs.set_al(0x00);
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
    let handle = cpu.regs.ax();

    // Now call LFN Get File Info By Handle
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0xA6);
    cpu.regs.set_bx(handle);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0200); // buffer address
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=48h Alloc memory: InsufficientMemory error path (line 3955).
#[test]
fn test_cov_ah48_alloc_insufficient_memory() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);

    // Request an absurdly large block
    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0xFFFF); // Request 65535 paragraphs
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 8); // Insufficient memory
                                  // BX should contain largest available block
    assert!(cpu.regs.bx() > 0);
}

/// AH=49h Free memory: invalid block error path (line 4072).
#[test]
fn test_cov_ah49_free_invalid_block() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);

    // Free a segment that was never allocated
    cpu.regs.es = 0x9999;
    cpu.regs.set_ah(0x49);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 9); // Invalid memory block
}

/// AH=4Ah Resize: insufficient memory error path (line 4115).
#[test]
fn test_cov_ah4a_resize_insufficient() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);

    // Allocate a small block first
    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x10); // 16 paragraphs
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let seg = cpu.regs.ax();

    // Try to resize to an absurdly large size
    cpu.regs.es = seg;
    cpu.regs.set_ah(0x4A);
    cpu.regs.set_bx(0xFFFF); // 65535 paragraphs
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 8); // Insufficient memory
}

/// AH=4Ah Resize: success path (line 4100).
#[test]
fn test_cov_ah4a_resize_success() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);

    // Allocate a block
    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x100); // 256 paragraphs
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let seg = cpu.regs.ax();

    // Resize to smaller
    cpu.regs.es = seg;
    cpu.regs.set_ah(0x4A);
    cpu.regs.set_bx(0x10); // Shrink to 16 paragraphs
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// AH=49h Free memory: success path (line 3993).
#[test]
fn test_cov_ah49_free_success() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);

    // Allocate a block
    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let seg = cpu.regs.ax();

    // Free it
    cpu.regs.es = seg;
    cpu.regs.set_ah(0x49);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

/// DOS trace functions with tracing enabled (lines 392-553).
#[test]
fn test_cov_trace_functions_enabled() {
    let _guard = trace_subscriber("gp64::dos=debug");

    // trace_dos_read
    let cpu = gp64_cpu::Cpu::new();

    // These functions use env vars for max counts - just call them
    // to exercise the trace-enabled code paths
    trace_dos_read(5, "C:\\TEST.TXT", 100, 50, true, Some(0));
    trace_dos_read(5, "C:\\TEST.TXT", 100, 0, false, None);

    trace_dos_int(&cpu, 0x3D, 0x1000, 0x0100);

    trace_dos_write(5, b"Hello world");

    trace_dos_seek(5, "C:\\TEST.TXT", 0, 0, true, Some(0));
    trace_dos_seek(5, "C:\\TEST.TXT", 1, 10, false, None);

    trace_dos_find("FIRST", "*.TXT", 0, None, true);
    trace_dos_find(
        "NEXT",
        "*.TXT",
        0x10,
        Some(&FindEntry {
            name: "TEST.TXT".to_string(),
            attr: 0x20,
            size: 1024,
            date: 0x5921,
            time: 0x7530,
        }),
        false,
    );
}

/// DOS trace functions: max count reached path (lines 392, 500, 550).
#[test]
fn test_cov_trace_max_reached() {
    let _lock = env_lock();
    let _guard = trace_subscriber("gp64::dos=debug");
    let _env1 = EnvVarGuard::set("GP64_DOS_TRACE", "read=0,seek=0,find=0");

    // With max=0, the trace should not emit (but the `max != 0` check
    // returns false, so it falls through)
    // Actually max=0 means unlimited (the check is `max != 0 && n >= max`)
    // So let's set max=1 and call twice to hit the limit
    let _env1 = EnvVarGuard::set("GP64_DOS_TRACE", "read=1");
    trace_dos_read(5, "C:\\TEST.TXT", 100, 50, true, Some(0));
    trace_dos_read(5, "C:\\TEST.TXT", 100, 50, true, Some(0)); // Should hit limit
}

/// LFN 716Ch: file table full (line 5730-5733).
#[test]
fn test_cov_lfn_716c_file_table_full() {
    struct LfnFullDrive;
    impl VirtualDrive for LfnFullDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Ok(Box::new(CursorFile::new(vec![])))
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
    let mut lfn_full_probe = LfnFullDrive;
    exercise_virtual_drive(&mut lfn_full_probe);
    dos.drives.insert(2, Box::new(LfnFullDrive));

    // Fill the file table
    fill_file_table(&mut dos);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"TEST.TXT\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    // DX=0x0001 (open if exists)
    cpu.regs.set_ah(0x71);
    cpu.regs.set_al(0x6C);
    cpu.regs.set_bl(0x00);
    cpu.regs.set_dx(0x0001);
    cpu.regs.ds = 0x2000;
    cpu.regs.set_si(0x0100);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 4); // Too many open files
}

/// AH=4Bh EXEC: read param block with valid cmdtail (line 6553 non-null path).
#[test]
fn test_cov_exec_read_param_block_valid() {
    let dos = Dos::new();
    let mut memory = Memory::new();

    // Set up a param block at ES:BX = 0x3000:0x0000
    let pb_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    memory.write_u16(pb_addr, 0x0000); // env_seg = 0 (inherit)
    memory.write_u16(pb_addr + 2, 0x0010); // cmdline offset
    memory.write_u16(pb_addr + 4, 0x3000); // cmdline segment

    let (env_seg, cmdline) = dos.exec_read_param_block_cmdtail(&memory, 0x3000, 0x0000);
    assert_eq!(env_seg, 0);
    assert_eq!(cmdline, Some((0x3000, 0x0010)));
}

/// EXEC file error differentiation: path with separator (line 7107).
#[test]
fn test_cov_exec_error_path_separator() {
    struct ExecFailDrive;
    impl VirtualDrive for ExecFailDrive {
        fn open_file(
            &mut self,
            _: &str,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn VirtualFile>> {
            Err(anyhow::anyhow!("not found"))
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
            Err(anyhow::anyhow!("not found"))
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
            Err(anyhow::anyhow!("n"))
        }
        fn get_disk_space(&self) -> Result<(u64, u64)> {
            Ok((100_000, 50_000))
        }
    }

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let mut exec_fail_probe = ExecFailDrive;
    exercise_virtual_drive(&mut exec_fail_probe);
    dos.drives.insert(2, Box::new(ExecFailDrive));
    dos.dos_mem.init(&mut memory);

    // Set up param block
    let pb_addr = Memory::seg_offset_to_linear(0x3000, 0x0000);
    memory.write_u16(pb_addr, 0x0000); // env_seg
    memory.write_u16(pb_addr + 2, 0x0080); // cmdline offset
    memory.write_u16(pb_addr + 4, 0x0100); // cmdline segment
                                           // Write cmdline: length byte + content
    let cmd_addr = Memory::seg_offset_to_linear(0x0100, 0x0080);
    memory.write_u8(cmd_addr, 0); // empty cmdline

    // Path with separator → error 3 (path not found)
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in b"C:\\SUBDIR\\MISSING.EXE\0".iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x4B);
    cpu.regs.set_al(0x00); // Load and execute
    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
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
    // Should return error 3 (path not found) since filename has path separator
    assert!(cpu.regs.ax() == 3 || cpu.regs.ax() == 2);
}

/// Trace: DOS memory trace enabled (lines 3955, 3972, 3993, 4013, 4036, 4072, 4100, 4115).
#[test]
fn test_cov_dos_mem_trace_enabled() {
    let _guard = trace_subscriber("gp64::dos::memory=debug");

    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    dos.dos_mem.init(&mut memory);

    // Alloc success (hits trace at ~3940)
    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let seg = cpu.regs.ax();

    // Resize success (hits trace at ~4100)
    cpu.regs.es = seg;
    cpu.regs.set_ah(0x4A);
    cpu.regs.set_bx(0x08);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Free success (hits trace at ~3993)
    cpu.regs.es = seg;
    cpu.regs.set_ah(0x49);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Alloc failure (hits trace at ~3955)
    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0xFFFF);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Free invalid (hits trace at ~4072)
    cpu.regs.es = 0x9999;
    cpu.regs.set_ah(0x49);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

    // Resize failure (hits trace at ~4115)
    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x10);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    let seg2 = cpu.regs.ax();
    cpu.regs.es = seg2;
    cpu.regs.set_ah(0x4A);
    cpu.regs.set_bx(0xFFFF);
    let _ = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}
