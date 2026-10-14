//! env progname handoff.md` Parts 1+2 end-to-end via `exec_program` and
//! `exec_load_only`.
//!
//! These tests drive `exec_program` with a hand-built EXEC parameter block
//! whose FCB-1/FCB-2 pointers reference known parent-buffer FCB bytes; they
//! then assert the child PSP at PSP:0x5C / 0x6C contains those exact bytes,
//! and that PSP:0x53..=0x7F is otherwise zero (no 0xCF BSS leak). This pins
//! the byte-for-byte child PSP layout against future regressions.

use gp64_cpu::{Cpu, Flags};
use gp64_dos::Dos;
use gp64_memory::Memory;
use tempfile::TempDir;

/// Helper: write an ASCIIZ string at `seg:off` ending in a NUL byte.
fn write_asciiz(memory: &mut Memory, seg: u16, off: u16, s: &str) {
    let base = Memory::seg_offset_to_linear(seg, off);
    for (i, b) in s.bytes().enumerate() {
        memory.write_u8(base + i, b);
    }
    memory.write_u8(base + s.len(), 0);
}

/// Helper: pre-fill the entire conventional memory region with 0xCF so the
/// allocator's free conventional blocks contain a non-zero BSS-init-style
/// pattern. After `create_psp` runs as part of `exec_program`, the child
/// PSP's reserved bytes must be ZERO (not 0xCF) — which is what these
/// tests verify.
fn pre_fill_conventional_with_cf(memory: &mut Memory) {
    // Pre-fill from 0x10000 (top of low memory after IVT/BDA) up through
    // 0x9FFFF (end of conventional). This covers all PSP-segment ranges
    // the allocator may hand out.
    for addr in 0x10000usize..0xA0000 {
        memory.write_u8(addr, 0xCF);
    }
}

/// Allocate a parent-owned scratch block via `dos_mem.alloc` so the
/// region's MCB is marked owned (not free). This prevents `exec_program`'s
/// BSS-init pass (which fills free conventional MCBs with 0xCF) from
/// wiping the test's pre-populated param block + FCB source bytes. In
/// real DOS, EXEC param blocks always live in the parent's allocated
/// memory; this helper mirrors that.
fn alloc_parent_scratch(dos: &mut Dos, memory: &mut Memory, paragraphs: u16) -> u16 {
    let parent_psp = dos.current_psp();
    dos.dos_mem
        .alloc(memory, parent_psp, paragraphs)
        .expect("scratch alloc")
}

/// End-to-end test: `exec_program` with a param block whose FCB-1 and
/// FCB-2 pointers reference known parent FCB buffers. After EXEC, the
/// child PSP must contain those exact 16 bytes at PSP:0x5C and 0x6C.
#[test]
fn test_exec_program_copies_fcbs_from_param_block() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);
    pre_fill_conventional_with_cf(&mut memory);

    // Set up a host drive C: with a trivial COM file.
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("CHILD.COM");
    std::fs::write(&file_path, [0xC3u8]).unwrap(); // RETN — innocuous
    dos.drives.insert(
        2,
        Box::new(gp64_dos::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    // Allocate a parent-owned scratch segment so its content survives the
    // BSS-init pass that exec_program runs over free conventional blocks.
    let scratch_seg = alloc_parent_scratch(&mut dos, &mut memory, 0x40);

    // Program path at scratch_seg:0x0100 — "synthetic-program\0".
    write_asciiz(&mut memory, scratch_seg, 0x0100, "CHILD.COM");

    // Param block at scratch_seg:0x0010.
    // Offset 0x00: env_seg = 0 (inherit parent).
    // Offset 0x02: cmdtail far ptr = 0:0 (no cmdtail).
    // Offset 0x06: FCB1 far ptr = scratch_seg:0x0080.
    // Offset 0x0A: FCB2 far ptr = scratch_seg:0x0090.
    let pb_addr = Memory::seg_offset_to_linear(scratch_seg, 0x0010);
    memory.write_u16(pb_addr, 0); // env_seg
    memory.write_u32(pb_addr + 2, 0); // cmdtail far ptr 0:0
    memory.write_u16(pb_addr + 0x06, 0x0080); // FCB1 offset
    memory.write_u16(pb_addr + 0x08, scratch_seg); // FCB1 segment
    memory.write_u16(pb_addr + 0x0A, 0x0090); // FCB2 offset
    memory.write_u16(pb_addr + 0x0C, scratch_seg); // FCB2 segment

    // FCB1 buffer at scratch_seg:0x0080 — known 16-byte pattern.
    let fcb1_src = Memory::seg_offset_to_linear(scratch_seg, 0x0080);
    for i in 0..16usize {
        memory.write_u8(fcb1_src + i, 0xA0 + i as u8);
    }
    // FCB2 buffer at scratch_seg:0x0090 — different 16-byte pattern.
    let fcb2_src = Memory::seg_offset_to_linear(scratch_seg, 0x0090);
    for i in 0..16usize {
        memory.write_u8(fcb2_src + i, 0xB0 + i as u8);
    }

    // Set up CPU regs for INT 21h/4Bh AL=00 EXEC:
    //   DS:DX = ASCIIZ program path.
    //   ES:BX = param block.
    cpu.regs.ds = scratch_seg;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = scratch_seg;
    cpu.regs.set_bx(0x0010);
    // Stack for save_frame — exec_program reads SS:SP for the return-frame
    // save. Reuse scratch_seg as the stack — it's in parent-owned memory.
    cpu.regs.ss = scratch_seg;
    cpu.regs.set_sp(0x0200);
    memory.write_u16_seg(scratch_seg, 0x0200, 0x1111);
    memory.write_u16_seg(scratch_seg, 0x0202, 0x2222);
    memory.write_u16_seg(scratch_seg, 0x0204, 0x0202);

    memory.write_u8(0, 5);
    memory.write_u8(1, b'X');

    let result = dos.exec_program(&mut cpu, &mut memory);
    assert!(result.is_ok(), "exec_program returned error: {:?}", result);
    assert!(
        !cpu.regs.flags.contains(Flags::CARRY),
        "EXEC should succeed (CARRY should be clear). AX={:04X}",
        cpu.regs.ax()
    );

    // After EXEC, current PSP is the child's PSP.
    let child_psp = dos.current_psp();
    let psp_base = Memory::seg_offset_to_linear(child_psp, 0);

    assert_eq!(memory.read_u8(psp_base + 0x80), 0, "cmdtail length");
    assert_eq!(
        memory.read_u8(psp_base + 0x81),
        0x0D,
        "cmdtail CR terminator"
    );

    // FCB1 copied to PSP:0x5C..=0x6B byte-for-byte.
    for i in 0..16usize {
        let actual = memory.read_u8(psp_base + 0x5C + i);
        let expected = 0xA0 + i as u8;
        assert_eq!(
            actual,
            expected,
            "PSP:0x{:02X} (FCB1 byte {i}): expected 0x{expected:02X}, got 0x{actual:02X}",
            0x5C + i,
        );
    }
    // FCB2 copied to PSP:0x6C..=0x7B byte-for-byte.
    for i in 0..16usize {
        let actual = memory.read_u8(psp_base + 0x6C + i);
        let expected = 0xB0 + i as u8;
        assert_eq!(
            actual,
            expected,
            "PSP:0x{:02X} (FCB2 byte {i}): expected 0x{expected:02X}, got 0x{actual:02X}",
            0x6C + i,
        );
    }

    // Reserved bytes around the FCBs MUST be zero (not 0xCF leak).
    for off in 0x53usize..=0x5B {
        assert_eq!(
            memory.read_u8(psp_base + off),
            0,
            "PSP:0x{off:02X} (reserved) must be zero",
        );
    }
    for off in 0x7Cusize..=0x7F {
        assert_eq!(
            memory.read_u8(psp_base + off),
            0,
            "PSP:0x{off:02X} (unused) must be zero",
        );
    }
}

/// End-to-end test: `exec_program` with a param block whose FCB pointers
/// are both 0:0 (parent could not parse). The child PSP's FCB slots must
/// be all zero (not 0xCF BSS leak).
#[test]
fn test_exec_program_zero_param_block_fcbs_zero_fills() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);
    pre_fill_conventional_with_cf(&mut memory);

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("CHILD.COM");
    std::fs::write(&file_path, [0xC3u8]).unwrap();
    dos.drives.insert(
        2,
        Box::new(gp64_dos::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let scratch_seg = alloc_parent_scratch(&mut dos, &mut memory, 0x40);
    write_asciiz(&mut memory, scratch_seg, 0x0100, "CHILD.COM");

    // Param block at scratch_seg:0x0010 — all FCB pointers 0:0.
    let pb_addr = Memory::seg_offset_to_linear(scratch_seg, 0x0010);
    memory.write_u16(pb_addr, 0); // env_seg
    memory.write_u32(pb_addr + 2, 0); // cmdtail 0:0
    memory.write_u32(pb_addr + 0x06, 0); // FCB1 0:0
    memory.write_u32(pb_addr + 0x0A, 0); // FCB2 0:0

    cpu.regs.ds = scratch_seg;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = scratch_seg;
    cpu.regs.set_bx(0x0010);
    cpu.regs.ss = scratch_seg;
    cpu.regs.set_sp(0x0200);
    memory.write_u16_seg(scratch_seg, 0x0200, 0x1111);
    memory.write_u16_seg(scratch_seg, 0x0202, 0x2222);
    memory.write_u16_seg(scratch_seg, 0x0204, 0x0202);

    let result = dos.exec_program(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(Flags::CARRY));

    let child_psp = dos.current_psp();
    let psp_base = Memory::seg_offset_to_linear(child_psp, 0);

    // PSP:0x5C..=0x7B (FCB1 + FCB2 + unused) — all zero.
    for off in 0x5Cusize..=0x7B {
        assert_eq!(
            memory.read_u8(psp_base + off),
            0,
            "PSP:0x{off:02X} must be zero (0:0 sentinel → zero FCB)",
        );
    }
}

/// End-to-end test: EXEC an MZ-format executable with FCB pointers and
/// verify the same FCB-copy behaviour. Mirrors `exec_load_exe`'s threading
/// path (separate from `exec_load_com`).
#[test]
fn test_exec_program_mz_copies_fcbs_from_param_block() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);
    pre_fill_conventional_with_cf(&mut memory);

    // Minimal MZ executable (1 header paragraph, 1 image paragraph, no relocs).
    let mut exe = vec![0u8; 544];
    exe[0] = b'M';
    exe[1] = b'Z';
    exe[2] = 32; // bytes in last page
    exe[4] = 2; // pages
    exe[6] = 0; // reloc count
    exe[8] = 32; // header paragraphs
    exe[24] = 28; // reloc table offset
    exe[512] = 0xCD; // INT 20h
    exe[513] = 0x20;

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("CHILD.EXE");
    std::fs::write(&file_path, &exe).unwrap();
    dos.drives.insert(
        2,
        Box::new(gp64_dos::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    let scratch_seg = alloc_parent_scratch(&mut dos, &mut memory, 0x40);
    write_asciiz(&mut memory, scratch_seg, 0x0100, "CHILD.EXE");

    let pb_addr = Memory::seg_offset_to_linear(scratch_seg, 0x0010);
    memory.write_u16(pb_addr, 0);
    memory.write_u32(pb_addr + 2, 0);
    memory.write_u16(pb_addr + 0x06, 0x0080);
    memory.write_u16(pb_addr + 0x08, scratch_seg);
    memory.write_u16(pb_addr + 0x0A, 0x0090);
    memory.write_u16(pb_addr + 0x0C, scratch_seg);

    let fcb1_src = Memory::seg_offset_to_linear(scratch_seg, 0x0080);
    for i in 0..16usize {
        memory.write_u8(fcb1_src + i, 0xE0 + i as u8);
    }
    let fcb2_src = Memory::seg_offset_to_linear(scratch_seg, 0x0090);
    for i in 0..16usize {
        memory.write_u8(fcb2_src + i, 0xF0 + i as u8);
    }

    cpu.regs.ds = scratch_seg;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = scratch_seg;
    cpu.regs.set_bx(0x0010);
    cpu.regs.ss = scratch_seg;
    cpu.regs.set_sp(0x0200);
    memory.write_u16_seg(scratch_seg, 0x0200, 0x1111);
    memory.write_u16_seg(scratch_seg, 0x0202, 0x2222);
    memory.write_u16_seg(scratch_seg, 0x0204, 0x0202);

    let result = dos.exec_program(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(
        !cpu.regs.flags.contains(Flags::CARRY),
        "EXEC failed AX={:04X}",
        cpu.regs.ax()
    );

    let child_psp = dos.current_psp();
    let psp_base = Memory::seg_offset_to_linear(child_psp, 0);

    for i in 0..16usize {
        assert_eq!(
            memory.read_u8(psp_base + 0x5C + i),
            0xE0 + i as u8,
            "PSP:0x{:02X} (FCB1 byte {i})",
            0x5C + i,
        );
    }
    for i in 0..16usize {
        assert_eq!(
            memory.read_u8(psp_base + 0x6C + i),
            0xF0 + i as u8,
            "PSP:0x{:02X} (FCB2 byte {i})",
            0x6C + i,
        );
    }
}

/// End-to-end test: AH=4Bh AL=01h ("load without exec") also reads FCB
/// pointers from its param block and copies them into the child PSP. This
/// is the option-(a) self-contained pattern (R5 in the HLD).
#[test]
fn test_exec_load_only_copies_fcbs_from_param_block() {
    let mut dos = Dos::new();
    let mut cpu = Cpu::new();
    let mut memory = Memory::new();

    dos.dos_mem.init(&mut memory);
    pre_fill_conventional_with_cf(&mut memory);

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("LOADONLY.COM");
    std::fs::write(&file_path, [0xC3u8]).unwrap();
    dos.drives.insert(
        2,
        Box::new(gp64_dos::filesystem::HostDrive::new(
            temp_dir.path().to_path_buf(),
        )),
    );

    write_asciiz(&mut memory, 0x2000, 0x0100, "LOADONLY.COM");

    // AL=01 param block: env_seg, cmdtail, FCB1, FCB2, then SS:SP/CS:IP result slots.
    let pb_addr = Memory::seg_offset_to_linear(0x3000, 0x0010);
    memory.write_u16(pb_addr, 0);
    memory.write_u32(pb_addr + 2, 0);
    memory.write_u16(pb_addr + 0x06, 0x0080);
    memory.write_u16(pb_addr + 0x08, 0x3000);
    memory.write_u16(pb_addr + 0x0A, 0x0090);
    memory.write_u16(pb_addr + 0x0C, 0x3000);

    let fcb1_src = Memory::seg_offset_to_linear(0x3000, 0x0080);
    for i in 0..16usize {
        memory.write_u8(fcb1_src + i, 0xD0 + i as u8);
    }
    let fcb2_src = Memory::seg_offset_to_linear(0x3000, 0x0090);
    for i in 0..16usize {
        memory.write_u8(fcb2_src + i, 0xE0 + i as u8);
    }

    cpu.regs.ds = 0x2000;
    cpu.regs.set_dx(0x0100);
    cpu.regs.es = 0x3000;
    cpu.regs.set_bx(0x0010);

    let result = dos.exec_load_only(&mut cpu, &mut memory);
    assert!(result.is_ok());
    assert!(!cpu.regs.flags.contains(Flags::CARRY));

    let child_psp = dos.current_psp();
    let psp_base = Memory::seg_offset_to_linear(child_psp, 0);

    for i in 0..16usize {
        assert_eq!(memory.read_u8(psp_base + 0x5C + i), 0xD0 + i as u8);
        assert_eq!(memory.read_u8(psp_base + 0x6C + i), 0xE0 + i as u8);
    }
}
