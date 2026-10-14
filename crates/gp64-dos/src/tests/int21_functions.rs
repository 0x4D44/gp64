use super::*;

#[test]
fn test_int21_get_version_ah30() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x30);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Should return version 6.22 (AL=6 major, AH=22 minor)
    assert_eq!(cpu.regs.al(), 6);
    assert_eq!(cpu.regs.ah(), 22);
}

#[test]
fn test_int21_lfn_open_ex_rejects_reserved_access_mode() {
    // MDD-BUG-FLUXHOMEARPA-00017: LFN extended open (AX=716Ch) must reject the
    // reserved access modes 3, 5, 6 and 7 with CF=1 and AX=000Ch (invalid access)
    // BEFORE resolving the drive or allocating a handle. Mode 4 is the documented
    // read-only/no-last-access-update mode and must remain valid.
    for access in 3u16..=7 {
        let mut dos = Dos::new();
        let mut cpu = gp64_cpu::Cpu::new();
        let mut memory = Memory::new();
        let (mut vga, mut keyboard) = create_test_io();

        // Filename at DS:SI (DS=0, SI=0x100).
        for (i, &b) in b"C:\\FOO.TXT\0".iter().enumerate() {
            memory.write_u8(0x100 + i, b);
        }
        cpu.regs.ds = 0;
        cpu.regs.set_si(0x100);
        cpu.regs.set_ax(0x716C);
        cpu.regs.set_bx(access); // BL = open_mode (reserved access bits), BH = 0
        cpu.regs.set_dx(0x0001); // action: open if exists

        let result = dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        );
        assert!(result.is_ok());
        if access == 4 {
            assert_eq!(cpu.regs.ax(), 2, "mode 4 must reach ordinary path errors");
            assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
            continue;
        }
        assert!(
            cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "access {access}: reserved mode must set CF"
        );
        assert_eq!(
            cpu.regs.ax(),
            12,
            "access {access}: expect AX=000Ch (invalid access code)"
        );
    }
}

#[test]
fn test_int21_get_current_drive_ah19() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x19);
    dos.path_resolver.set_current_drive(2); // C:

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    assert_eq!(cpu.regs.al(), 2); // C: drive
}

#[test]
fn test_int21_get_drive_allocation_data_default_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x1C);
    cpu.regs.set_dl(0); // default drive

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 8);
    assert_eq!(cpu.regs.cx(), 512);
}

#[test]
fn test_int21_get_drive_allocation_data_explicit_drive() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x1C);
    cpu.regs.set_dl(3); // C:

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 8);
    assert_eq!(cpu.regs.cx(), 512);
}

#[test]
fn test_int21_set_dta_ah1a() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x1A);
    cpu.regs.ds = 0x5000;
    cpu.regs.set_dx(0x0100);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    assert_eq!(dos.dta_segment, 0x5000);
    assert_eq!(dos.dta_offset, 0x0100);
}

#[test]
fn test_int21_get_dta_ah2f() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.set_dta(0x3000, 0x0200);
    cpu.regs.es = 0x2222;
    cpu.regs.es_cache = gp64_cpu::SegmentCache::null_flat_usable();
    cpu.regs.es_cache.base = u32::from(cpu.regs.es) << 4;
    let old_cache = cpu.regs.es_cache;
    cpu.regs.set_ah(0x2F);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    assert_eq!(cpu.regs.es, 0x3000);
    assert_eq!(cpu.regs.bx(), 0x0200);
    let mut expected_cache = old_cache;
    expected_cache.base = 0x30000;
    assert_eq!(
        cpu.regs.es_cache, expected_cache,
        "AH=2Fh must update only the hidden ES base in real mode"
    );
}

#[test]
fn test_int21_disk_reset_ah0d() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x0D);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Should just return successfully (no-op)
}

#[test]
fn test_int21_select_disk_ah0e() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x0E);
    cpu.regs.set_dl(3); // D:

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    assert_eq!(dos.path_resolver.get_current_drive(), 3);
    assert_eq!(cpu.regs.al(), 26); // Number of logical drives
}

#[test]
fn test_int21_select_disk_invalid_drive_no_change() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.path_resolver.set_current_drive(2); // C:

    cpu.regs.set_ah(0x0E);
    cpu.regs.set_dl(30); // Invalid drive

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(dos.path_resolver.get_current_drive(), 2);
    assert_eq!(cpu.regs.al(), 26);
}

#[test]
fn test_int21_get_psp_ah62() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.current_psp = 0x1234;
    cpu.regs.set_ah(0x62);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    assert_eq!(cpu.regs.bx(), 0x1234);
}

#[test]
fn test_int21_get_interrupt_vector_ah35() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Set up an interrupt vector at IVT offset
    memory.write_u16(0x21 * 4, 0x1234); // offset
    memory.write_u16(0x21 * 4 + 2, 0x5678); // segment

    // Pre-seed ES with a *different* real-mode segment so es_cache.base is a
    // stale non-zero value (mimics a program whose ES held its PSP before
    // the call). The handler must refresh the cache base, not just the
    // selector — otherwise real-mode ES-override accesses read the wrong
    // linear address.
    cpu.regs.es = 0x0700;
    cpu.regs.es_cache = gp64_cpu::SegmentCache::real_mode(0x0700);

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

    assert_eq!(cpu.regs.bx(), 0x1234);
    assert_eq!(cpu.regs.es, 0x5678);
    // The descriptor-cache base must track the new selector (0x5678 << 4),
    // not remain at the stale 0x7000.
    assert_eq!(
        cpu.regs.es_cache.base,
        0x5678 << 4,
        "INT 21h/35h must refresh es_cache.base in real mode"
    );
}

#[test]
fn test_int21_get_vector_ah35_preserves_unreal_es_limit() {
    // After the guest establishes unreal mode, ES's cache has a 4 GB limit.
    // INT 21h/35h refreshes its base while preserving that cached limit;
    // replacing the cache with real-mode defaults would truncate it to 64 KB.
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Synthetic divide-error vector in the architectural BIOS ROM segment.
    memory.write_u16(0, 0x1234); // offset
    memory.write_u16(2, 0xF000); // segment

    // Unreal-mode ES: real-mode base, flat 4 GB cached limit.
    cpu.regs.es = 0x0000;
    let mut flat = gp64_cpu::SegmentCache::real_mode(0x0000);
    flat.set_limit(0xFFFFF);
    flat.set_granularity(true);
    cpu.regs.es_cache = flat;
    assert_eq!(cpu.regs.es_cache.effective_limit(), 0xFFFF_FFFF);

    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x00);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    assert_eq!(cpu.regs.es, 0xF000);
    assert_eq!(
        cpu.regs.es_cache.base, 0xF0000,
        "cache base must track the returned selector"
    );
    assert_eq!(
        cpu.regs.es_cache.effective_limit(),
        0xFFFF_FFFF,
        "INT 21h/35h must not truncate an unreal-mode flat ES limit"
    );
}

#[test]
fn test_int21_get_vector_ah35_simulated_real_mode_preserves_es_cache() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    enter_dpmi_16bit(&mut dos, &mut cpu, &mut memory);
    assert!(cpu.is_protected_mode());
    dos.set_simulated_real_mode_hle(true);

    memory.write_u16(0x21 * 4, 0x1234);
    memory.write_u16(0x21 * 4 + 2, 0x5678);
    cpu.regs.es = 0x2222;
    cpu.regs.es_cache = gp64_cpu::SegmentCache::null_flat_usable();
    cpu.regs.es_cache.base = u32::from(cpu.regs.es) << 4;
    let old_cache = cpu.regs.es_cache;
    cpu.regs.set_ah(0x35);
    cpu.regs.set_al(0x21);

    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();

    assert_eq!(cpu.regs.es, 0x5678);
    assert_eq!(cpu.regs.bx(), 0x1234);
    let mut expected_cache = old_cache;
    expected_cache.base = 0x56780;
    assert_eq!(
        cpu.regs.es_cache, expected_cache,
        "simulated real mode must not treat a raw DOS segment as a PM selector"
    );
}

#[test]
fn test_int21_set_interrupt_vector_ah25() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x25);
    cpu.regs.set_al(0x21);
    cpu.regs.ds = 0xAAAA;
    cpu.regs.set_dx(0xBBBB);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Check IVT was updated
    assert_eq!(memory.read_u16(0x21 * 4), 0xBBBB);
    assert_eq!(memory.read_u16(0x21 * 4 + 2), 0xAAAA);
}

#[test]
fn test_int21_get_disk_free_space_ah36() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x36);
    cpu.regs.set_dl(0); // Default drive

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // Should return some stub values
    // AX = sectors per cluster, BX = available clusters
    // CX = bytes per sector, DX = total clusters
    assert!(cpu.regs.ax() > 0);
}

#[test]
fn test_int21_get_disk_free_space_drive_ah36() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(
        2,
        Box::new(DiskSpaceDrive::new(256 * 1024 * 1024, 64 * 1024 * 1024)),
    );
    dos.path_resolver.set_current_drive(2);

    cpu.regs.set_ah(0x36);
    cpu.regs.set_dl(0); // Default drive

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), 64);
    assert_eq!(cpu.regs.cx(), 512);
    assert!(cpu.regs.bx() > 0);
    assert!(cpu.regs.dx() > 0);
}

#[test]
fn test_int21_get_disk_free_space_missing_drive_ah36() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x36);
    cpu.regs.set_dl(1); // A: (not mounted)

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), 0xFFFF);
}

#[test]
fn test_int21_get_disk_free_space_error_ah36() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.drives.insert(0, Box::new(ErrorDiskSpaceDrive));
    dos.path_resolver.set_current_drive(0);

    cpu.regs.set_ah(0x36);
    cpu.regs.set_dl(0); // Default drive

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.ax(), 0xFFFF);
}

#[test]
fn test_int21_character_output_ah02() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x02);
    cpu.regs.set_dl(b'A');

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Character should be output to VGA
}

#[test]
fn test_int21_ctrl_break_check_ah33() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // AL=0 - get ctrl-break flag
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x00);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // DL should contain the ctrl-break flag
}

#[test]
fn test_int21_ctrl_break_set_ah33() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // AL=1 - set ctrl-break flag
    cpu.regs.set_ah(0x33);
    cpu.regs.set_al(0x01);
    cpu.regs.set_dl(0x01);

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
fn test_int21_check_input_status_ah0b() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x0B);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // AL = 0 if no char available, 0xFF if available
}

#[test]
fn test_int21_check_input_status_ah0b_with_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x1E, b'A');

    cpu.regs.set_ah(0x0B);
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
fn test_int21_ioctl_get_device_info_ah44_00() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bx(0); // STDIN handle

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // DX should contain device info
}

#[test]
fn test_int21_ioctl_get_device_info_ems_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x00);
    cpu.regs.set_bx(0x00FF); // EMS device handle

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.dx(), 0x80C0);
}

#[test]
fn test_int21_ioctl_set_device_info_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x44);
    cpu.regs.set_al(0x01); // Set device info
    cpu.regs.set_bx(5); // Non-standard handle

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 1);
}

#[test]
fn test_int21_allocate_memory_ah48() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Initialize memory manager
    dos.dos_mem.init(&mut memory);

    cpu.regs.set_ah(0x48);
    cpu.regs.set_bx(0x0010); // Request 16 paragraphs

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    // Result depends on memory availability
    assert!(result.is_ok());
}

#[test]
fn test_int21_terminate_with_return_code_ah4c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Push a process state. Set a known stack pointer on the parent's saved
    // registers so the INT-frame pop (SP += 6) performed by terminate_process
    // is observable after the child returns.
    let mut regs = gp64_cpu::Registers::new();
    regs.set_sp(0x8000);
    dos.process_stack.push(ProcessState {
        psp_segment: 0x1000,
        registers: regs.clone(),
        parent_psp: 0x0100,
        exec_ret_ip: 0x0200,
        exec_ret_cs: 0x3000,
        exec_ret_flags: 0x0202,
        pm_parent: false,
    });
    // EXEC points the child's PSP:0Ah at the return address.
    let child_psp = dos.current_psp();
    memory.write_u32(child_psp as usize * 16 + PSP_INT22_TERMINATE, 0x3000_0200);

    cpu.regs.set_ah(0x4C);
    cpu.regs.set_al(0x05); // Return code 5

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // Returns NoIret when returning to parent process.
    // Pin the child-return boundary that INT 20h / AH=00 / AH=4Ch all rely on:
    // parent CS:IP restored from exec_ret_cs/exec_ret_ip, the saved process PSP
    // made current, and the INT frame popped (parent SP + 6).
    assert_eq!(cpu.regs.cs, 0x3000, "parent CS (exec_ret_cs) restored");
    assert_eq!(cpu.regs.ip(), 0x0200, "parent IP (exec_ret_ip) restored");
    // terminate_process restores the saved ProcessState.psp_segment as the
    // current PSP (NOT parent_psp); for this frame that is 0x1000.
    assert_eq!(dos.current_psp(), 0x1000, "saved process PSP restored");
    assert_eq!(cpu.regs.sp(), 0x8006, "INT frame popped (parent SP + 6)");
}

#[test]
fn test_int21_terminate_with_return_code_no_parent_errors() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x4C);
    cpu.regs.set_al(0x01);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_err());
}

#[test]
fn test_int21_get_return_code_ah4d() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x4D);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // AH = termination type (0 = normal)
    assert_eq!(cpu.regs.ah(), 0);
}

#[test]
fn test_int21_direct_console_output_ah06() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(b'X'); // Output character

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());

    // MDD-BUG-CRUCIBLE-00448: observe the OUTPUT, not just that the
    // call returned Ok. These tests asserted dispatch success alone, so
    // deleting `vga.teletype`, passing it the wrong byte, or breaking the
    // cursor advance left every one of them green while DOS applications
    // silently lost AH=06h output.
    let mut cell = [0u8; 2];
    memory.read_bytes(0xB8000, &mut cell);
    assert_eq!(
        cell[0], b'X',
        "the character must reach VGA text memory, found {:02X?}",
        cell
    );
    assert_eq!(
        vga.get_cursor(0),
        (0, 1),
        "the cursor must advance one column past the written cell"
    );
}

#[test]
fn test_int21_direct_console_input_ah06_ff() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF); // Input

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // AL = 0 if no char available, ZF set
}

#[test]
fn test_int21_direct_console_input_ah06_with_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x2C, b'Z');

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), b'Z');
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
}

#[test]
fn test_int21_direct_console_input_ah06_extended_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x3B, 0);

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x3B);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
}

#[test]
fn test_int21_direct_console_input_ah06_extended_prefix_e0() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x4D, 0xE0);

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xE0);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));

    cpu.regs.set_ah(0x06);
    cpu.regs.set_dl(0xFF);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x4D);
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::ZERO));
}

#[test]
fn test_int21_direct_console_input_ah07_normal_key() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x2C, b'Z');

    cpu.regs.set_ah(0x07);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), b'Z');
}

#[test]
fn test_int21_direct_console_input_ah07_extended_prefix_e0() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    bda::kbd_enqueue(&mut memory, 0x4D, 0xE0);

    cpu.regs.set_ah(0x07);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xE0);

    cpu.regs.set_ah(0x07);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x4D);
}

#[test]
fn test_int21_direct_console_input_ah07_reads_from_bda() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Place key directly in BDA (as IRQ1 -> INT 09h would do).
    bda::kbd_enqueue(&mut memory, 0x1C, 0x0D);

    cpu.regs.set_ah(0x07);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x0D);
}

#[test]
fn test_int21_check_input_status_ah0b_reads_bda() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Place key directly in BDA (as IRQ1 -> INT 09h would do).
    bda::kbd_enqueue(&mut memory, 0x1C, 0x0D);

    cpu.regs.set_ah(0x0B);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0xFF);

    // Key should remain available for the subsequent read.
    cpu.regs.set_ah(0x07);
    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert_eq!(cpu.regs.al(), 0x0D);
}

#[test]
fn test_int21_flush_and_read_ah0c() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x0C);
    cpu.regs.set_al(0x00); // Just flush, no read

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
fn test_int21_get_current_directory_ah47() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x47);
    cpu.regs.set_dl(0); // Default drive
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
    // Buffer at DS:SI should contain current directory
}

#[test]
fn test_int21_dup_handle_ah45() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x45);
    cpu.regs.set_bx(0); // Duplicate STDIN

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // AX = new handle or error
}

#[test]
fn test_int21_dup_handle_ah45_no_free_alias() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Grow the JFT to its maximum and fill every user handle (5..255).
    dos.file_table.set_handle_count(255);
    for i in 5..0xFF {
        let handle = dos.file_table.allocate(
            Box::new(CursorFile::new(vec![])),
            format!("C:\\FILL{i:02X}.TXT"),
            dos.current_psp,
        );
        assert!(handle.is_some());
    }

    cpu.regs.set_ah(0x45);
    cpu.regs.set_bx(0); // Duplicate STDIN

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
fn test_int21_dup_handle_file_handle_ah45() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    let handle = dos
        .file_table
        .allocate(
            Box::new(CursorFile::new(b"DATA".to_vec())),
            "C:\\DUP.TXT".to_string(),
            dos.current_psp,
        )
        .expect("expected handle");

    cpu.regs.set_ah(0x45);
    cpu.regs.set_bx(handle);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let dup = cpu.regs.ax();
    assert_ne!(dup, handle);
    assert!(dos.file_table.is_valid(dup));
}

#[test]
fn test_int21_dup_handle_stdout_creates_alias() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x45);
    cpu.regs.set_bx(1); // Duplicate STDOUT

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    let dup = cpu.regs.ax();
    assert!(dup >= 5);

    let msg = b"OK";
    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    for (i, &b) in msg.iter().enumerate() {
        memory.write_u8(addr + i, b);
    }

    cpu.regs.set_ah(0x40);
    cpu.regs.set_bx(dup);
    cpu.regs.set_cx(msg.len() as u16);
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
    assert_eq!(cpu.regs.ax(), msg.len() as u16);
}

#[test]
fn test_wildcard_match_exact() {
    fn wildcard_match(pat: &str, text: &str) -> bool {
        let pat = pat.to_uppercase();
        let text = text.to_uppercase();
        let pat_chars: Vec<char> = pat.chars().collect();
        let text_chars: Vec<char> = text.chars().collect();

        fn matches(pat: &[char], text: &[char]) -> bool {
            if pat.is_empty() {
                return text.is_empty();
            }
            if text.is_empty() {
                return pat.iter().all(|&c| c == '*');
            }
            match pat[0] {
                '*' => matches(&pat[1..], text) || matches(pat, &text[1..]),
                '?' => matches(&pat[1..], &text[1..]),
                c if c == text[0] => matches(&pat[1..], &text[1..]),
                _ => false,
            }
        }
        matches(&pat_chars, &text_chars)
    }

    assert!(wildcard_match("*.TXT", "FILE.TXT"));
    assert!(wildcard_match("FILE.*", "FILE.TXT"));
    assert!(wildcard_match("*.*", "TEST.DAT"));
    assert!(!wildcard_match("*.COM", "FILE.TXT"));
    assert!(wildcard_match("F???.*", "FILE.TXT"));
}

#[test]
fn test_int21_close_file_ah3e_standard_handles() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Close STDIN (handle 0) - should succeed
    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(0);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // CF should be clear for standard handles
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
}

#[test]
fn test_int21_close_file_ah3e_ems_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(0x00FF);

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
fn test_int21_close_file_ah3e_con_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(0x00FE);

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
fn test_int21_close_file_ah3e_alias_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.file_table.std_handle_aliases[10] = Some(1);

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(10);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert!(dos.file_table.std_handle_aliases[10].is_none());
}

#[test]
fn test_int21_close_file_ah3e_invalid_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Close invalid handle
    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(99);

    let result = dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    );
    assert!(result.is_ok());
    // CF should be set for invalid handle
    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 6); // Invalid handle error
}

#[test]
fn test_int21_close_file_ah3e_reports_flush_failure_and_retains_handle() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(FailingSyncFile),
            r"C:\DIRTY.DAT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_WRITE,
        )
        .expect("test file handle");

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(handle);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();

    assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 5, "sync failure must be guest-visible");
    assert!(
        dos.file_table.is_valid(handle),
        "failed close must retain the dirty handle for retry"
    );
}

#[test]
fn test_int21_close_file_ah3e_read_only_handle_does_not_sync() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    let handle = dos
        .file_table
        .allocate_with_sharing(
            Box::new(FailingSyncFile),
            r"C:\READONLY.DAT".to_string(),
            dos.current_psp,
            crate::files::SHARE_COMPAT,
            crate::files::ACCESS_READ,
        )
        .expect("test file handle");

    cpu.regs.set_ah(0x3E);
    cpu.regs.set_bx(handle);
    dos.int21(
        &mut cpu,
        &mut memory,
        &mut vga,
        &mut keyboard,
        &mut Pic::new(),
    )
    .unwrap();

    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    assert_eq!(cpu.regs.ax(), 0);
    assert!(!dos.file_table.is_valid(handle));
}

#[test]
fn test_int21_read_stdin_ah3f() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0); // STDIN
    cpu.regs.set_cx(10);
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
    // Cooked-mode stdin blocks when no input is available;
    // AX is left unchanged (still holds AH=0x3F from setup).
    assert_eq!(cpu.regs.ax(), 0x3F00);
}

#[test]
fn test_int21_read_stdin_reads_buffered_bytes() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    // Pre-populate the cooked-mode line buffer (simulates a completed line).
    dos.stdin_line_buf = vec![b'A', b'B'];

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0); // STDIN
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
    assert_eq!(cpu.regs.ax(), 2);

    let addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    assert_eq!(memory.read_u8(addr), b'A');
    assert_eq!(memory.read_u8(addr + 1), b'B');
}

#[test]
fn test_int21_read_stdin_ctrl_c_triggers_interrupt() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.ctrl_break_check = true;
    cpu.regs.ss = 0x3000;
    cpu.regs.set_sp(0x0200);

    let vector_addr = 0x23 * 4;
    memory.write_u16(vector_addr, 0x1111);
    memory.write_u16(vector_addr + 2, 0x2222);

    bda::kbd_enqueue(&mut memory, 0x2E, 0x03);

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0); // STDIN
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
    assert_eq!(cpu.regs.ip(), 0x1111);
    assert_eq!(cpu.regs.cs, 0x2222);
}

#[test]
fn test_int21_read_stdin_ctrl_c_after_data_returns_bytes() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    dos.ctrl_break_check = true;

    // Pre-populate the cooked-mode line buffer with completed data.
    // In the real flow, 'A' followed by Enter would produce "A\r\n"
    // in stdin_line_buf.  Here we just test that buffered data is served.
    dos.stdin_line_buf = vec![b'A'];

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(0);
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
    assert_eq!(cpu.regs.ax(), 1);
    let buffer_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
    assert_eq!(memory.read_u8(buffer_addr), b'A');
}

#[test]
fn test_int21_read_invalid_handle_ah3f() {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();

    cpu.regs.set_ah(0x3F);
    cpu.regs.set_bx(99); // Invalid handle
    cpu.regs.set_cx(10);
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
    assert_eq!(cpu.regs.ax(), 6); // Invalid handle
}
