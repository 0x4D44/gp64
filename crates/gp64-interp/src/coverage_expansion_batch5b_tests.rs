use super::*;
use gp64_cpu::{Cpu, CpuType, Flags};
use gp64_memory::Memory;

// Helper: setup paging (identity map first 1MB, 32-bit PM flat model)
fn setup_paging(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 = 0x8000_0001; // PE + PG
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

// ========================================================================
// Target 3: 0F-prefix instructions with paging
// ========================================================================

// 1. MOVZX r32, byte [mem] (0F B6) with paging
#[test]
fn test_paging_movzx_r32_byte_mem() {
    // MOVZX EAX, byte [0x5000] = 0F B6 05 00 50 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xB6, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0xFE;
    cpu.regs.eax = 0xDEADBEEF; // upper bits should be cleared
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x000000FE); // zero-extended from byte
}

// 2. MOVZX r32, word [mem] (0F B7) with paging
#[test]
fn test_paging_movzx_r32_word_mem() {
    // MOVZX EAX, word [0x5000] = 0F B7 05 00 50 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xB7, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000..0x5002].copy_from_slice(&0xBEEFu16.to_le_bytes());
    cpu.regs.eax = 0xDEADDEAD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0000BEEF); // zero-extended from word
}

// 3. MOVSX r32, byte [mem] (0F BE) with paging
#[test]
fn test_paging_movsx_r32_byte_mem() {
    // MOVSX EAX, byte [0x5000] = 0F BE 05 00 50 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBE, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x80; // -128 as signed byte
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFFFF80); // sign-extended
}

// 4. MOVSX r32, word [mem] (0F BF) with paging
#[test]
fn test_paging_movsx_r32_word_mem() {
    // MOVSX EAX, word [0x5000] = 0F BF 05 00 50 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBF, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000..0x5002].copy_from_slice(&0x8001u16.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF8001); // sign-extended from word
}

// 5. BSF r32, [mem] (0F BC) with paging - scan forward
#[test]
fn test_paging_bsf_r32_mem() {
    // BSF EAX, dword [0x5000] = 0F BC 05 00 50 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBC, 0x05, 0x00, 0x50, 0x00, 0x00]);
    // Value 0x00000040 => bit 6 is lowest set bit
    mem.ram[0x5000..0x5004].copy_from_slice(&0x00000040u32.to_le_bytes());
    cpu.regs.eax = 0xFFFFFFFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 6); // bit 6 is the first set bit
    assert!(!cpu.regs.flags.contains(Flags::ZERO)); // ZF=0 when source != 0
}

// 6. BSR r32, [mem] (0F BD) with paging - scan reverse
#[test]
fn test_paging_bsr_r32_mem() {
    // BSR EAX, dword [0x5000] = 0F BD 05 00 50 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBD, 0x05, 0x00, 0x50, 0x00, 0x00]);
    // Value 0x00010000 => bit 16 is highest set bit
    mem.ram[0x5000..0x5004].copy_from_slice(&0x00010000u32.to_le_bytes());
    cpu.regs.eax = 0xFFFFFFFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 16); // bit 16 is the highest set bit
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// 7. BT [mem], r32 (0F A3) with paging - bit test
#[test]
fn test_paging_bt_mem_r32() {
    // BT dword [0x5000], EAX = 0F A3 05 00 50 00 00
    // ModR/M: mod=00, reg=EAX(0), r/m=disp32(5) => 0x05
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xA3, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000..0x5004].copy_from_slice(&0x00000100u32.to_le_bytes()); // bit 8 set
    cpu.regs.eax = 8; // test bit 8
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // bit 8 was set
}

// 8. BTS [mem], r32 (0F AB) with paging - bit test and set
#[test]
fn test_paging_bts_mem_r32() {
    // BTS dword [0x5000], EAX = 0F AB 05 00 50 00 00
    // ModR/M: mod=00, reg=EAX(0), r/m=disp32(5) => 0x05
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xAB, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000..0x5004].copy_from_slice(&0x00000000u32.to_le_bytes()); // all clear
    cpu.regs.eax = 5; // set bit 5
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was 0 before
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val & (1 << 5), 1 << 5); // bit 5 now set
}

// 9. BTR [mem], r32 (0F B3) with paging - bit test and reset
#[test]
fn test_paging_btr_mem_r32() {
    // BTR dword [0x5000], EAX = 0F B3 05 00 50 00 00
    // ModR/M: mod=00, reg=EAX(0), r/m=disp32(5) => 0x05
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xB3, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000..0x5004].copy_from_slice(&0x00000080u32.to_le_bytes()); // bit 7 set
    cpu.regs.eax = 7; // reset bit 7
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // was 1 before reset
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val & (1 << 7), 0); // bit 7 now cleared
}

// 10. BTC [mem], r32 (0F BB) with paging - bit test and complement
#[test]
fn test_paging_btc_mem_r32() {
    // BTC dword [0x5000], EAX = 0F BB 05 00 50 00 00
    // ModR/M: mod=00, reg=EAX(0), r/m=disp32(5) => 0x05
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBB, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000..0x5004].copy_from_slice(&0x00000004u32.to_le_bytes()); // bit 2 set
    cpu.regs.eax = 2; // complement bit 2
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // was 1 before complement
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val & (1 << 2), 0); // bit 2 now cleared (complemented)
}

// 11. IMUL r32, [mem] (0F AF) with paging - 2-operand multiply
#[test]
fn test_paging_imul_r32_mem() {
    // IMUL EAX, dword [0x5000] = 0F AF 05 00 50 00 00
    // ModR/M: mod=00, reg=EAX(0), r/m=disp32(5) => 0x05
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xAF, 0x05, 0x00, 0x50, 0x00, 0x00]);
    cpu.regs.eax = 7;
    mem.ram[0x5000..0x5004].copy_from_slice(&6u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 42); // 7 * 6 = 42
}

// 12. CMPXCHG [mem], r32 (0F B1) with paging - compare and exchange (match)
#[test]
fn test_paging_cmpxchg_mem_r32_equal() {
    // CMPXCHG dword [0x5000], ECX = 0F B1 0D 00 50 00 00
    // ModR/M: mod=00, reg=ECX(1), r/m=disp32(5) => 0x0D
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xB1, 0x0D, 0x00, 0x50, 0x00, 0x00]);
    cpu.cpu_type = CpuType::I80486;
    cpu.regs.eax = 0x12345678; // comparand
    mem.ram[0x5000..0x5004].copy_from_slice(&0x12345678u32.to_le_bytes()); // matches EAX
    cpu.regs.ecx = 0xDEADBEEF; // replacement
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // Equal: [mem] = ECX, ZF=1
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val, 0xDEADBEEF);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// 12b. CMPXCHG [mem], r32 (0F B1) with paging - compare and exchange (no match)
#[test]
fn test_paging_cmpxchg_mem_r32_not_equal() {
    // CMPXCHG dword [0x5000], ECX = 0F B1 0D 00 50 00 00
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xB1, 0x0D, 0x00, 0x50, 0x00, 0x00]);
    cpu.cpu_type = CpuType::I80486;
    cpu.regs.eax = 0x12345678;
    mem.ram[0x5000..0x5004].copy_from_slice(&0xAAAABBBBu32.to_le_bytes()); // != EAX
    cpu.regs.ecx = 0xDEADBEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // Not equal: EAX = [mem], [mem] unchanged, ZF=0
    assert_eq!(cpu.regs.eax, 0xAAAABBBB);
    let val = u32::from_le_bytes(mem.ram[0x5000..0x5004].try_into().unwrap());
    assert_eq!(val, 0xAAAABBBB); // unchanged
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// Target 5: MUL/DIV with paging
// ========================================================================

// 1. MUL byte [mem] (F6 /4) - unsigned byte multiply from paged mem
#[test]
fn test_paging_mul_byte_mem() {
    // MUL byte [0x5000] = F6 25 00 50 00 00
    // ModR/M: mod=00, reg=4 (/4), r/m=disp32(5) => 0x25
    let (mut cpu, mut mem) = setup_paging(&[0xF6, 0x25, 0x00, 0x50, 0x00, 0x00]);
    cpu.regs.eax = 10; // AL = 10
    mem.ram[0x5000] = 25;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax & 0xFFFF, 250); // AX = AL * src = 10 * 25 = 250
}

// 2. MUL dword [mem] (F7 /4) - unsigned dword multiply from paged mem
#[test]
fn test_paging_mul_dword_mem() {
    // MUL dword [0x5000] = F7 25 00 50 00 00
    // ModR/M: mod=00, reg=4 (/4), r/m=disp32(5) => 0x25
    let (mut cpu, mut mem) = setup_paging(&[0xF7, 0x25, 0x00, 0x50, 0x00, 0x00]);
    cpu.regs.eax = 100;
    mem.ram[0x5000..0x5004].copy_from_slice(&200u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // EAX = low 32 bits of 100 * 200 = 20000, EDX = high 32 bits = 0
    assert_eq!(cpu.regs.eax, 20000);
    assert_eq!(cpu.regs.edx, 0);
}

// 3. IMUL byte [mem] (F6 /5) - signed byte multiply from paged mem
#[test]
fn test_paging_imul_byte_mem() {
    // IMUL byte [0x5000] = F6 2D 00 50 00 00
    // ModR/M: mod=00, reg=5 (/5), r/m=disp32(5) => 0x2D
    let (mut cpu, mut mem) = setup_paging(&[0xF6, 0x2D, 0x00, 0x50, 0x00, 0x00]);
    cpu.regs.eax = 0xF6; // AL = -10 (signed byte)
    mem.ram[0x5000] = 3;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // AX = AL(signed) * src(signed) = (-10) * 3 = -30 = 0xFFE2
    assert_eq!(cpu.regs.eax & 0xFFFF, 0xFFE2);
}

// 4. DIV byte [mem] (F6 /6) - unsigned byte divide from paged mem
#[test]
fn test_paging_div_byte_mem() {
    // DIV byte [0x5000] = F6 35 00 50 00 00
    // ModR/M: mod=00, reg=6 (/6), r/m=disp32(5) => 0x35
    let (mut cpu, mut mem) = setup_paging(&[0xF6, 0x35, 0x00, 0x50, 0x00, 0x00]);
    cpu.regs.eax = 100; // AX = 100 (AH=0, AL=100)
    mem.ram[0x5000] = 7;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // AL = 100 / 7 = 14, AH = 100 % 7 = 2
    assert_eq!(cpu.regs.eax & 0xFF, 14); // quotient in AL
    assert_eq!((cpu.regs.eax >> 8) & 0xFF, 2); // remainder in AH
}

// 5. DIV dword [mem] (F7 /6) - unsigned dword divide from paged mem
#[test]
fn test_paging_div_dword_mem() {
    // DIV dword [0x5000] = F7 35 00 50 00 00
    // ModR/M: mod=00, reg=6 (/6), r/m=disp32(5) => 0x35
    let (mut cpu, mut mem) = setup_paging(&[0xF7, 0x35, 0x00, 0x50, 0x00, 0x00]);
    cpu.regs.eax = 1000;
    cpu.regs.edx = 0; // EDX:EAX = 1000
    mem.ram[0x5000..0x5004].copy_from_slice(&7u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // EAX = 1000 / 7 = 142, EDX = 1000 % 7 = 6
    assert_eq!(cpu.regs.eax, 142);
    assert_eq!(cpu.regs.edx, 6);
}

// 6. IDIV byte [mem] (F6 /7) - signed byte divide from paged mem
#[test]
fn test_paging_idiv_byte_mem() {
    // IDIV byte [0x5000] = F6 3D 00 50 00 00
    // ModR/M: mod=00, reg=7 (/7), r/m=disp32(5) => 0x3D
    let (mut cpu, mut mem) = setup_paging(&[0xF6, 0x3D, 0x00, 0x50, 0x00, 0x00]);
    // AX = -100 = 0xFF9C (signed 16-bit)
    cpu.regs.eax = 0xFF9C;
    mem.ram[0x5000] = 7; // divisor = 7
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // -100 / 7 = -14, -100 % 7 = -2
    // AL = quotient = -14 = 0xF2, AH = remainder = -2 = 0xFE
    assert_eq!(cpu.regs.eax & 0xFF, 0xF2u32); // -14 as u8
    assert_eq!((cpu.regs.eax >> 8) & 0xFF, 0xFEu32); // -2 as u8
}
