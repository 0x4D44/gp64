use super::*;
use gp64_cpu::{Cpu, Flags};
use gp64_memory::Memory;

// Helper: setup paging (identity map first 1MB) — 32-bit protected mode
fn setup_paging(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 = 0x8000_0001;
    cpu.regs.cpl = 0;
    cpu.regs.cr3 = 0x10000;
    mem.write_u32_physical(0x10000, 0x11007);
    for page in 0..256u32 {
        let pte_addr = 0x11000 + (page as usize) * 4;
        mem.write_u32_physical(pte_addr, (page << 12) | 0x07);
    }
    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0x9B;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = true;
    cpu.regs.cs_cache.present = true;
    cpu.cs_bits = 32;
    cpu.regs.ds = 0x0010;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.access = 0x93;
    cpu.regs.ds_cache.is_code = false;
    cpu.regs.ds_cache.big = true;
    cpu.regs.ds_cache.present = true;
    cpu.regs.ss = 0x0018;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.access = 0x93;
    cpu.regs.ss_cache.is_code = false;
    cpu.regs.ss_cache.big = true;
    cpu.regs.ss_cache.present = true;
    cpu.regs.es = 0x0020;
    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0xFFFFF;
    cpu.regs.es_cache.granularity = true;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.access = 0x93;
    cpu.regs.es_cache.is_code = false;
    cpu.regs.es_cache.big = true;
    cpu.regs.es_cache.present = true;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

#[test]
fn test_lfs_descriptor_table_page_fault_maps_to_pf() {
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xB4, 0x05, 0x00, 0x30, 0x00, 0x00]);
    cpu.regs.gdtr.base = 0x0012_0000;
    cpu.regs.gdtr.limit = 0x0017;
    cpu.regs.eax = 0xCAFE_BABE;
    cpu.regs.fs = 0x0020;
    mem.ram[0x3000..0x3004].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    mem.ram[0x3004..0x3006].copy_from_slice(&0x0010u16.to_le_bytes());

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    match result {
        RunResult::Exception {
            vector,
            error_code,
            fault_addr,
            ..
        } => {
            assert_eq!(vector, 14);
            assert_eq!(error_code, 0);
            assert_eq!(fault_addr, Some(0x0012_0010));
        }
        other => panic!("expected descriptor-table #PF from LFS, got {other:?}"),
    }
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.fs, 0x0020);
    assert_eq!(
        cpu.regs.eax, 0xCAFE_BABE,
        "EAX must not be modified on LFS descriptor-table #PF"
    );
}

// ========================================================================
// Target 4: REP bulk paging paths
// ========================================================================
// The paging bulk path triggers when: paging enabled, DF=0 (forward),
// REP prefix present, and count is large enough to exceed per-element
// threshold. Source/destination spanning page boundaries forces the
// page-at-a-time chunked copy loop.

// 1. REP MOVSB with 256 bytes crossing a page boundary
#[test]
fn test_paging_bulk_rep_movsb_page_crossing() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);
    // Source at 0x3FF0, spans page boundary at 0x4000 (16 bytes on one page,
    // 240 bytes on the next page). Avoids page table area at 0x10000+.
    for i in 0u32..256 {
        mem.ram[(0x3FF0 + i) as usize] = (i & 0xFF) as u8;
    }
    cpu.regs.esi = 0x3FF0; // source crosses 0x4000 boundary
    cpu.regs.edi = 0x5000; // destination well within a page
    cpu.regs.ecx = 256;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 256);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ecx, 0,
        "ECX should be 0 after completing all iterations"
    );
    assert_eq!(cpu.regs.esi, 0x3FF0 + 256, "ESI should advance by 256");
    assert_eq!(cpu.regs.edi, 0x5000 + 256, "EDI should advance by 256");
    // Verify all bytes copied correctly
    for i in 0u32..256 {
        let expected = (i & 0xFF) as u8;
        assert_eq!(
            mem.ram[(0x5000 + i) as usize],
            expected,
            "byte {} mismatch: got {:#x}, expected {:#x}",
            i,
            mem.ram[(0x5000 + i) as usize],
            expected
        );
    }
}

// 1b. REP MOVSB with paging and forward-overlapping physical ranges must
// this exact pattern for self-referential back-buffer expansion under V86
// paging.
#[test]
fn test_paging_bulk_rep_movsb_overlap_smears() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);
    for i in 0u32..258 {
        mem.ram[(0x3000 + i) as usize] = (i & 0xFF) as u8;
    }
    cpu.regs.esi = 0x3000;
    cpu.regs.edi = 0x3002;
    cpu.regs.ecx = 256;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 256);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.esi, 0x3000 + 256);
    assert_eq!(cpu.regs.edi, 0x3002 + 256);
    for i in 0u32..258 {
        let expected = if i % 2 == 0 { 0x00 } else { 0x01 };
        assert_eq!(
            mem.ram[(0x3000 + i) as usize],
            expected,
            "byte {} mismatch",
            i
        );
    }
}

// 2. REP MOVSD with 64 dwords (256 bytes) crossing page boundary
#[test]
fn test_paging_bulk_rep_movsd_page_crossing() {
    // F3 A5 = REP MOVSD (32-bit default in paging mode)
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA5]);
    // Source at 0xFF0, 4 dwords fit before page boundary at 0x1000,
    // then 60 dwords on next page
    for i in 0u32..64 {
        let val = 0xDEAD_0000u32.wrapping_add(i);
        let base = (0xFF0 + i * 4) as usize;
        let bytes = val.to_le_bytes();
        mem.ram[base] = bytes[0];
        mem.ram[base + 1] = bytes[1];
        mem.ram[base + 2] = bytes[2];
        mem.ram[base + 3] = bytes[3];
    }
    cpu.regs.esi = 0xFF0; // source crosses 0x1000 boundary
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 64;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 64);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.esi, 0xFF0 + 256);
    assert_eq!(cpu.regs.edi, 0x5000 + 256);
    for i in 0u32..64 {
        let expected = 0xDEAD_0000u32.wrapping_add(i);
        let base = (0x5000 + i * 4) as usize;
        let got = u32::from_le_bytes([
            mem.ram[base],
            mem.ram[base + 1],
            mem.ram[base + 2],
            mem.ram[base + 3],
        ]);
        assert_eq!(got, expected, "dword {} mismatch", i);
    }
}

// 3. REP STOSB with 256 bytes crossing a page boundary
#[test]
fn test_paging_bulk_rep_stosb_page_crossing() {
    // F3 AA = REP STOSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAA]);
    cpu.regs.set_al(0xBB);
    cpu.regs.edi = 0x4FF0; // destination crosses 0x5000 boundary
    cpu.regs.ecx = 256;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 256);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.edi, 0x4FF0 + 256);
    for i in 0u32..256 {
        assert_eq!(
            mem.ram[(0x4FF0 + i) as usize],
            0xBB,
            "byte {} should be 0xBB, got {:#x}",
            i,
            mem.ram[(0x4FF0 + i) as usize]
        );
    }
    // Byte after fill region should be untouched
    assert_eq!(mem.ram[0x4FF0 + 256], 0x00);
}

// 4. REP STOSD with 64 dwords crossing page boundary
#[test]
fn test_paging_bulk_rep_stosd_page_crossing() {
    // F3 AB = REP STOSD (32-bit default in paging mode)
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAB]);
    cpu.regs.eax = 0xCAFE_BABE;
    cpu.regs.edi = 0xFF0; // destination crosses 0x1000 boundary
    cpu.regs.ecx = 64;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 64);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.edi, 0xFF0 + 256);
    for i in 0u32..64 {
        let base = (0xFF0 + i * 4) as usize;
        let got = u32::from_le_bytes([
            mem.ram[base],
            mem.ram[base + 1],
            mem.ram[base + 2],
            mem.ram[base + 3],
        ]);
        assert_eq!(got, 0xCAFE_BABE, "dword {} mismatch: got {:#x}", i, got);
    }
}

// 5. REP MOVSB backward (DF=1) with 256 bytes and paging
// Backward direction falls to per-element path, but exercises the
// paging translate for each element individually.
#[test]
fn test_paging_rep_movsb_backward() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);
    // Fill source with recognizable pattern
    for i in 0u32..256 {
        mem.ram[(0x2000 + i) as usize] = ((i + 1) & 0xFF) as u8;
    }
    cpu.regs.esi = 0x2000 + 255; // start at end, moving backward
    cpu.regs.edi = 0x5000 + 255;
    cpu.regs.ecx = 256;
    cpu.regs.flags.insert(Flags::DIRECTION); // DF=1 (backward)

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 256);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.ecx, 0);
    // ESI should have decremented by 256
    assert_eq!(cpu.regs.esi, 0x2000 + 255 - 256);
    // EDI should have decremented by 256
    assert_eq!(cpu.regs.edi, 0x5000 + 255 - 256);
    // Verify the backward copy was correct
    for i in 0u32..256 {
        let expected = ((i + 1) & 0xFF) as u8;
        assert_eq!(
            mem.ram[(0x5000 + i) as usize],
            expected,
            "byte {} mismatch: got {:#x}, expected {:#x}",
            i,
            mem.ram[(0x5000 + i) as usize],
            expected
        );
    }
}

// 6. REP STOSB backward (DF=1) with 256 bytes and paging
#[test]
fn test_paging_rep_stosb_backward() {
    // F3 AA = REP STOSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAA]);
    cpu.regs.set_al(0xCC);
    cpu.regs.edi = 0x5000 + 255; // start at end, moving backward
    cpu.regs.ecx = 256;
    cpu.regs.flags.insert(Flags::DIRECTION); // DF=1 (backward)

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 256);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.edi, 0x5000 + 255 - 256);
    // Verify all 256 bytes filled with 0xCC
    for i in 0u32..256 {
        assert_eq!(
            mem.ram[(0x5000 + i) as usize],
            0xCC,
            "byte {} should be 0xCC, got {:#x}",
            i,
            mem.ram[(0x5000 + i) as usize]
        );
    }
}

// 7. REP MOVSW with 128 words crossing page boundary
#[test]
fn test_paging_bulk_rep_movsw_page_crossing() {
    // 66 F3 A5 = REP MOVSW (0x66 operand-size override to 16-bit in 32-bit mode)
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0xF3, 0xA5]);
    // Source at 0xFF0, crosses page boundary at 0x1000
    for i in 0u32..128 {
        let val = (0xA000u16).wrapping_add(i as u16);
        let base = (0xFF0 + i * 2) as usize;
        let bytes = val.to_le_bytes();
        mem.ram[base] = bytes[0];
        mem.ram[base + 1] = bytes[1];
    }
    cpu.regs.esi = 0xFF0; // source crosses 0x1000 boundary
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 128;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.esi, 0xFF0 + 256); // 128 words * 2 bytes
    assert_eq!(cpu.regs.edi, 0x5000 + 256);
    for i in 0u32..128 {
        let expected = (0xA000u16).wrapping_add(i as u16);
        let base = (0x5000 + i * 2) as usize;
        let got = u16::from_le_bytes([mem.ram[base], mem.ram[base + 1]]);
        assert_eq!(got, expected, "word {} mismatch", i);
    }
}

// 8. REP STOSW with 128 words crossing page boundary
#[test]
fn test_paging_bulk_rep_stosw_page_crossing() {
    // 66 F3 AB = REP STOSW (0x66 operand-size override to 16-bit in 32-bit mode)
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0xF3, 0xAB]);
    cpu.regs.eax = 0x0000_BEEF; // AX = 0xBEEF
    cpu.regs.edi = 0x6FF0; // destination crosses 0x7000 boundary
    cpu.regs.ecx = 128;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.edi, 0x6FF0 + 256); // 128 words * 2 bytes
    for i in 0u32..128 {
        let base = (0x6FF0 + i * 2) as usize;
        let got = u16::from_le_bytes([mem.ram[base], mem.ram[base + 1]]);
        assert_eq!(got, 0xBEEF, "word {} mismatch: got {:#x}", i, got);
    }
}

// ========================================================================
// Target 6: Shift/rotate with paging (memory operands)
// ========================================================================
// These test shift and rotate instructions operating on memory operands
// with paging enabled. Each exercises the translate_write!/rmw path
// through the TLB.

// 9. SHL byte [mem], 1 (0xD0 /4)
#[test]
fn test_paging_shl_byte_mem_by1() {
    // D0 /4 with ModR/M for [0x5000]: D0 25 00 50 00 00
    // SHL byte [0x5000], 1
    let (mut cpu, mut mem) = setup_paging(&[0xD0, 0x25, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x81; // 1000_0001
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // SHL 0x81 by 1 = 0x02, CF=1 (bit 7 shifted out)
    assert_eq!(mem.ram[0x5000], 0x02);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// 10. SHR byte [mem], 1 (0xD0 /5)
#[test]
fn test_paging_shr_byte_mem_by1() {
    // D0 /5 with ModR/M for [0x5000]: D0 2D 00 50 00 00
    // SHR byte [0x5000], 1
    let (mut cpu, mut mem) = setup_paging(&[0xD0, 0x2D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x03; // 0000_0011
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // SHR 0x03 by 1 = 0x01, CF=1 (bit 0 shifted out)
    assert_eq!(mem.ram[0x5000], 0x01);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// 11. SAR byte [mem], 1 (0xD0 /7)
#[test]
fn test_paging_sar_byte_mem_by1() {
    // D0 /7 with ModR/M for [0x5000]: D0 3D 00 50 00 00
    // SAR byte [0x5000], 1
    let (mut cpu, mut mem) = setup_paging(&[0xD0, 0x3D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x80; // -128 signed
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // SAR 0x80 by 1 = 0xC0 (sign-extends), CF=0
    assert_eq!(mem.ram[0x5000], 0xC0);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

// 12. SHL dword [mem], CL (0xD3 /4)
#[test]
fn test_paging_shl_dword_mem_cl() {
    // D3 /4 with ModR/M for [0x5000]: D3 25 00 50 00 00
    // SHL dword [0x5000], CL
    let (mut cpu, mut mem) = setup_paging(&[0xD3, 0x25, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x01;
    mem.ram[0x5001] = 0x00;
    mem.ram[0x5002] = 0x00;
    mem.ram[0x5003] = 0x00; // 0x00000001
    cpu.regs.set_cl(4);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    assert_eq!(val, 0x10); // 1 << 4 = 16
}

// 13. SHR dword [mem], CL (0xD3 /5)
#[test]
fn test_paging_shr_dword_mem_cl() {
    // D3 /5 with ModR/M for [0x5000]: D3 2D 00 50 00 00
    // SHR dword [0x5000], CL
    let (mut cpu, mut mem) = setup_paging(&[0xD3, 0x2D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x00;
    mem.ram[0x5001] = 0x01;
    mem.ram[0x5002] = 0x00;
    mem.ram[0x5003] = 0x00; // 0x00000100
    cpu.regs.set_cl(4);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    assert_eq!(val, 0x10); // 0x100 >> 4 = 0x10
}

// 14. ROL byte [mem], 1 (0xD0 /0)
#[test]
fn test_paging_rol_byte_mem_by1() {
    // D0 /0 with ModR/M for [0x5000]: D0 05 00 50 00 00
    // ROL byte [0x5000], 1
    let (mut cpu, mut mem) = setup_paging(&[0xD0, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x81; // 1000_0001
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // ROL 0x81 by 1 = 0x03 (bit 7 rotates to bit 0), CF=1
    assert_eq!(mem.ram[0x5000], 0x03);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// 15. ROR dword [mem], CL (0xD3 /1)
#[test]
fn test_paging_ror_dword_mem_cl() {
    // D3 /1 with ModR/M for [0x5000]: D3 0D 00 50 00 00
    // ROR dword [0x5000], CL
    let (mut cpu, mut mem) = setup_paging(&[0xD3, 0x0D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x01;
    mem.ram[0x5001] = 0x00;
    mem.ram[0x5002] = 0x00;
    mem.ram[0x5003] = 0x00; // 0x00000001
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    // ROR 0x00000001 by 1 = 0x80000000
    assert_eq!(val, 0x80000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// 16. RCL dword [mem], 1 (0xD1 /2)
#[test]
fn test_paging_rcl_dword_mem_by1() {
    // D1 /2 with ModR/M for [0x5000]: D1 15 00 50 00 00
    // RCL dword [0x5000], 1
    let (mut cpu, mut mem) = setup_paging(&[0xD1, 0x15, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0xFF;
    mem.ram[0x5001] = 0xFF;
    mem.ram[0x5002] = 0xFF;
    mem.ram[0x5003] = 0x7F; // 0x7FFFFFFF
    cpu.regs.flags.insert(Flags::CARRY); // CF=1
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    // RCL 0x7FFFFFFF, 1 with CF=1: result = (0x7FFFFFFF << 1) | 1 = 0xFFFFFFFF, CF=0
    assert_eq!(val, 0xFFFFFFFF);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

// ========================================================================
// Cross-page #PF instruction-rollback tests
// (HLD: wrk_docs/2026.04.28 - HLD - cross-page PF instruction rollback.md)
//
// Intel SDM Vol 3 §6.6: when a fault is reported, the processor restores
// machine state to the state prior to the beginning of execution of the
// faulting instruction.  The post-decode #PF check raises #PF after the
// instruction has already executed, so ESP must be rolled back from a
// snapshot taken at the top of the outer loop.
// ========================================================================

// PUSH imm32 (68 imm32) placed at linear 0x0FFC so its 5th byte lands on
// page B (linear 0x1000), which is marked absent.  PUSH imm32 has no
// destination pre-flight: it commits ESP unconditionally, then the
// post-decode #PF check at 19199 fires because the instruction itself
// straddled into the absent page.  Without the ESP snapshot/restore, the
// push leaves ESP off by 4.
//
// (The CALL rel32 E8 handler at
// lib.rs ~9434 pre-flights the destination's translatability before
// pushing, which would consume the fault on a different code path
// and never exercise the post-decode rollback the PUSH imm32 test
// is designed to cover.)
#[test]
fn test_cross_page_push_imm32_pf_rolls_back_esp() {
    let (mut cpu, mut mem) = setup_paging(&[]);
    // Place PUSH imm32 at 0x0FFC: bytes [68, AA, BB, CC] on page A,
    // byte 5 at 0x1000 on absent page B (zero-filled by the fetcher).
    mem.ram[0x0FFC] = 0x68;
    mem.ram[0x0FFD] = 0xAA;
    mem.ram[0x0FFE] = 0xBB;
    mem.ram[0x0FFF] = 0xCC;
    // Mark page 1 (linear 0x1000-0x1FFF) absent: clear the present bit on
    // PTE[1] at 0x11004.  setup_paging maps PTE[i] to (i << 12) | 0x07.
    mem.write_u32_physical(0x11004, (1u32 << 12) | 0x06);
    cpu.regs.eip = 0x0FFC;
    let pre_esp = cpu.regs.esp;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    match result {
        RunResult::Exception {
            vector, fault_addr, ..
        } => {
            assert_eq!(
                vector, 14,
                "expected #PF (vector 14), got vector {}",
                vector
            );
            assert_eq!(
                fault_addr,
                Some(0x1000),
                "fault_addr should point at the absent page B start (0x1000)"
            );
        }
        other => panic!("expected RunResult::Exception, got {:?}", other),
    }
    assert_eq!(
        cpu.regs.eip, 0x0FFC,
        "EIP should be rolled back to the start of the faulting PUSH"
    );
    assert_eq!(
        cpu.regs.esp, pre_esp,
        "ESP must be rolled back to its pre-PUSH value (Intel SDM Vol 3 §6.6); \
         got 0x{:08X}, expected 0x{:08X}",
        cpu.regs.esp, pre_esp
    );
}

// Counter-test: a 1-byte PUSH EAX (0x50) at linear 0x0FFF lives entirely on
// page A; page B is absent but the instruction does not cross the boundary,
// so no #PF should fire and ESP should decrement normally.
#[test]
fn test_cross_page_single_byte_push_does_not_fault() {
    let (mut cpu, mut mem) = setup_paging(&[]);
    // PUSH EAX at the very last byte of page A.
    mem.ram[0x0FFF] = 0x50;
    // Mark page 1 absent (same as the PUSH imm32 test).
    mem.write_u32_physical(0x11004, (1u32 << 12) | 0x06);
    cpu.regs.eip = 0x0FFF;
    cpu.regs.eax = 0xDEAD_BEEF;
    let pre_esp = cpu.regs.esp;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "single-byte PUSH EAX on page A should not fault, got {:?}",
        result
    );
    // 32-bit stack mode: ESP decrements by 4.
    assert_eq!(
        cpu.regs.esp,
        pre_esp.wrapping_sub(4),
        "ESP should decrement by 4 for 32-bit PUSH EAX"
    );
    // EIP advances past the 1-byte instruction.
    assert_eq!(cpu.regs.eip, 0x1000, "EIP should advance to 0x1000");
}

#[test]
fn lgdt_base_read_page_fault_does_not_commit_gdtr() {
    // MDD-BUG-ANVIL-00009: LGDT must not commit GDTR until BOTH the limit and
    // base reads succeed. The 6-byte pseudo-descriptor straddles a page: the
    // limit (2B) is on present page 0, the base (4B) starts on absent page 1,
    // so the base read #PFs and GDTR (limit+base) must be left unchanged.
    // LGDT [0x0FFE] = 0F 01 15 FE 0F 00 00 (modrm 15 = /2 with disp32).
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0x01, 0x15, 0xFE, 0x0F, 0x00, 0x00]);
    mem.ram[0x0FFE] = 0xFF;
    mem.ram[0x0FFF] = 0x00; // pseudo-descriptor limit = 0x00FF at linear 0x0FFE
                            // Mark page 1 (linear 0x1000-0x1FFF) absent.
    mem.write_u32_physical(0x11004, (1u32 << 12) | 0x06);
    cpu.regs.gdtr.limit = 0xBEEF;
    cpu.regs.gdtr.base = 0xCAFE_0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector, fault_addr, ..
        } => {
            assert_eq!(
                vector, 14,
                "expected #PF on the base read, got vector {vector}"
            );
            assert_eq!(
                fault_addr,
                Some(0x1000),
                "fault should be the absent base page"
            );
        }
        other => panic!("expected #PF on the LGDT base read, got {other:?}"),
    }
    assert_eq!(
        cpu.regs.gdtr.limit, 0xBEEF,
        "GDTR limit must be unchanged on a faulting LGDT"
    );
    assert_eq!(
        cpu.regs.gdtr.base, 0xCAFE_0000,
        "GDTR base must be unchanged on a faulting LGDT"
    );
}

#[test]
fn lidt_base_read_page_fault_does_not_commit_idtr() {
    // MDD-BUG-ANVIL-00009 (LIDT arm): a faulting base read must leave IDTR
    // unchanged, same as LGDT.
    // LIDT [0x0FFE] = 0F 01 1D FE 0F 00 00 (modrm 1D = /3 with disp32).
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0x01, 0x1D, 0xFE, 0x0F, 0x00, 0x00]);
    mem.ram[0x0FFE] = 0xFF;
    mem.ram[0x0FFF] = 0x00; // pseudo-descriptor limit = 0x00FF at linear 0x0FFE
    mem.write_u32_physical(0x11004, (1u32 << 12) | 0x06);
    cpu.regs.idtr.limit = 0xBEEF;
    cpu.regs.idtr.base = 0xCAFE_0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector, fault_addr, ..
        } => {
            assert_eq!(
                vector, 14,
                "expected #PF on the base read, got vector {vector}"
            );
            assert_eq!(
                fault_addr,
                Some(0x1000),
                "fault should be the absent base page"
            );
        }
        other => panic!("expected #PF on the LIDT base read, got {other:?}"),
    }
    assert_eq!(
        cpu.regs.idtr.limit, 0xBEEF,
        "IDTR limit must be unchanged on a faulting LIDT"
    );
    assert_eq!(
        cpu.regs.idtr.base, 0xCAFE_0000,
        "IDTR base must be unchanged on a faulting LIDT"
    );
}

#[test]
fn cross_page_u32_store_pf_leaves_memory_unchanged() {
    // MDD-BUG-ANVIL-00010: a 4-byte store straddling a page boundary must not
    // partially write when a later byte faults. MOV [0x0FFE], EAX touches bytes
    // 0x0FFE/0x0FFF (present page 0) and 0x1000/0x1001 (absent page 1); the #PF
    // must leave the page-0 bytes unchanged (a restartable fault has no partial
    // side effects).
    // MOV [0x0FFE], EAX = 89 05 FE 0F 00 00 (modrm 05 = /0 disp32, reg EAX).
    let (mut cpu, mut mem) = setup_paging(&[0x89, 0x05, 0xFE, 0x0F, 0x00, 0x00]);
    mem.ram[0x0FFE] = 0xAA;
    mem.ram[0x0FFF] = 0xBB; // sentinels on the present page
                            // Mark page 1 (linear 0x1000-0x1FFF) absent.
    mem.write_u32_physical(0x11004, (1u32 << 12) | 0x06);
    cpu.regs.eax = 0xDEAD_BEEF; // LE bytes EF BE AD DE

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector, fault_addr, ..
        } => {
            assert_eq!(
                vector, 14,
                "expected #PF on the cross-page store, got {vector}"
            );
            assert_eq!(fault_addr, Some(0x1000), "fault should be the absent page");
        }
        other => panic!("expected #PF on the cross-page store, got {other:?}"),
    }
    assert_eq!(
        mem.ram[0x0FFE], 0xAA,
        "page-0 byte 0 must be unchanged when the store faults"
    );
    assert_eq!(
        mem.ram[0x0FFF], 0xBB,
        "page-0 byte 1 must be unchanged when the store faults"
    );
}

// Regression for MDD-00010: RET imm16 (C2 iw) at linear 0x0FFE has its
// immediate's high byte on the absent second fetch page. The stack return
// target is same-page, so the old path popped ESP, applied the imm16 stack
// adjustment from zero-filled bytes, and took branch_absolute! before the
// post-decode cross-page #PF check could run. Correct x86 behavior faults
// on instruction fetch before the RET commits any stack or control-flow
// side effects.
#[test]
fn test_cross_page_ret_imm16_branch_absolute_pf_before_pop() {
    let (mut cpu, mut mem) = setup_paging(&[]);
    mem.ram[0x0FFE] = 0xC2; // RET imm16
    mem.ram[0x0FFF] = 0x04; // low byte of imm16; high byte is on page B
    mem.write_u32_physical(0x11004, (1u32 << 12) | 0x06);
    cpu.regs.eip = 0x0FFE;

    let return_target = 0x0F00u32;
    let pre_esp = cpu.regs.esp;
    mem.ram[pre_esp as usize..pre_esp as usize + 4].copy_from_slice(&return_target.to_le_bytes());

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);

    match result {
        RunResult::Exception {
            vector,
            fault_addr,
            executed,
            ..
        } => {
            assert_eq!(vector, 14, "expected #PF, got vector {vector}");
            assert_eq!(fault_addr, Some(0x1000));
            assert_eq!(executed, 0, "faulting RET must not retire");
        }
        other => panic!("expected RunResult::Exception, got {other:?}"),
    }
    assert_eq!(cpu.regs.eip, 0x0FFE, "EIP should stay on the RET");
    assert_eq!(cpu.regs.esp, pre_esp, "RET must not pop before fetch #PF");
}
