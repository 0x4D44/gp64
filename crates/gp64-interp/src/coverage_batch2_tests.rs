use super::test_helpers::setup;
use super::*;
use gp64_cpu::Flags;

// ========================================================================
// 32-bit MUL/IMUL/DIV/IDIV
// ========================================================================

#[test]
fn test_mul_r32() {
    // MUL EBX: 66 F7 E3 (operand-size prefix + F7 /4)
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xE3]);
    cpu.regs.eax = 0x00010000;
    cpu.regs.ebx = 0x00020000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // 0x10000 * 0x20000 = 0x200000000 → EAX=0x00000000, EDX=0x00000002
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert_eq!(cpu.regs.edx, 0x00000002);
}

#[test]
fn test_imul_r32_single() {
    // IMUL EBX (F6/F7 /5 in 32-bit): 66 F7 EB
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xEB]);
    cpu.regs.eax = 0xFFFFFFFF; // -1
    cpu.regs.ebx = 0x00000002; // 2
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // -1 * 2 = -2 → EAX=0xFFFFFFFE, EDX=0xFFFFFFFF
    assert_eq!(cpu.regs.eax, 0xFFFFFFFE);
    assert_eq!(cpu.regs.edx, 0xFFFFFFFF);
}

#[test]
fn test_div_r32() {
    // DIV EBX: 66 F7 F3
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xF3]);
    cpu.regs.eax = 7;
    cpu.regs.edx = 0;
    cpu.regs.ebx = 2;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 3); // quotient
    assert_eq!(cpu.regs.edx, 1); // remainder
}

#[test]
fn test_idiv_r32() {
    // IDIV EBX: 66 F7 FB
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xFB]);
    cpu.regs.eax = 0xFFFFFFF9; // -7 as u32
    cpu.regs.edx = 0xFFFFFFFF; // sign-extend to 64 bits
    cpu.regs.ebx = 2;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // -7 / 2 = -3 remainder -1
    assert_eq!(cpu.regs.eax as i32, -3);
    assert_eq!(cpu.regs.edx as i32, -1);
}

#[test]
fn test_div_r32_by_zero() {
    // DIV EBX: 66 F7 F3 with EBX=0
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xF3]);
    cpu.regs.eax = 1;
    cpu.regs.edx = 0;
    cpu.regs.ebx = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Exception { vector: 0, .. }));
}

// ========================================================================
// 32-bit NOT/NEG
// ========================================================================

#[test]
fn test_not_r32() {
    // NOT EAX: 66 F7 D0
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xD0]);
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, !0x12345678u32);
}

#[test]
fn test_neg_r32() {
    // NEG EAX: 66 F7 D8
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xD8]);
    cpu.regs.eax = 5;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax as i32, -5);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // NEG sets CF for non-zero
}

#[test]
fn test_neg_r32_zero() {
    // NEG EAX with 0: 66 F7 D8
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xD8]);
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // CF=0 for NEG 0
}

// ========================================================================
// 32-bit ALU: ADD/SUB/AND/OR/XOR/CMP EAX, imm32
// ========================================================================

#[test]
fn test_add_eax_imm32() {
    // ADD EAX, 0x12345678: 66 05 78 56 34 12
    let (mut cpu, mut mem) = setup(&[0x66, 0x05, 0x78, 0x56, 0x34, 0x12]);
    cpu.regs.eax = 1;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12345679);
}

#[test]
fn test_sub_eax_imm32() {
    // SUB EAX, 1: 66 2D 01 00 00 00
    let (mut cpu, mut mem) = setup(&[0x66, 0x2D, 0x01, 0x00, 0x00, 0x00]);
    cpu.regs.eax = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0F);
}

#[test]
fn test_and_eax_imm32() {
    // AND EAX, 0xFF00FF00: 66 25 00 FF 00 FF
    let (mut cpu, mut mem) = setup(&[0x66, 0x25, 0x00, 0xFF, 0x00, 0xFF]);
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12005600);
}

#[test]
fn test_or_eax_imm32() {
    // OR EAX, 0x00FF00FF: 66 0D FF 00 FF 00
    let (mut cpu, mut mem) = setup(&[0x66, 0x0D, 0xFF, 0x00, 0xFF, 0x00]);
    cpu.regs.eax = 0x12340000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12FF00FF);
}

#[test]
fn test_xor_eax_imm32() {
    // XOR EAX, 0xFFFFFFFF: 66 35 FF FF FF FF
    let (mut cpu, mut mem) = setup(&[0x66, 0x35, 0xFF, 0xFF, 0xFF, 0xFF]);
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xEDCBA987);
}

#[test]
fn test_cmp_eax_imm32() {
    // CMP EAX, 0x100: 66 3D 00 01 00 00
    let (mut cpu, mut mem) = setup(&[0x66, 0x3D, 0x00, 0x01, 0x00, 0x00]);
    cpu.regs.eax = 0x100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.eax, 0x100); // CMP doesn't modify dest
}

// ========================================================================
// 32-bit reg-reg ALU
// ========================================================================

#[test]
fn test_add_r32_r32() {
    // ADD EAX, EBX: 66 01 D8 (01 /r with modrm=D8: EBX→EAX)
    let (mut cpu, mut mem) = setup(&[0x66, 0x01, 0xD8]);
    cpu.regs.eax = 0x10000000;
    cpu.regs.ebx = 0x20000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x30000000);
}

#[test]
fn test_sub_r32_r32() {
    // SUB ECX, EDX: 66 29 D1
    let (mut cpu, mut mem) = setup(&[0x66, 0x29, 0xD1]);
    cpu.regs.ecx = 0x50000000;
    cpu.regs.edx = 0x10000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0x40000000);
}

#[test]
fn test_xor_r32_r32_self() {
    // XOR EAX, EAX: 66 31 C0
    let (mut cpu, mut mem) = setup(&[0x66, 0x31, 0xC0]);
    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// ADC/SBB 32-bit
// ========================================================================

#[test]
fn test_adc_r32_r32_carry_set() {
    // STC (F9) then ADC EAX, EBX (66 11 D8)
    let (mut cpu, mut mem) = setup(&[0xF9, 0x66, 0x11, 0xD8]);
    cpu.regs.eax = 1;
    cpu.regs.ebx = 2;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 4); // 1 + 2 + 1(carry)
}

#[test]
fn test_sbb_r32_r32_borrow_set() {
    // STC (F9) then SBB EAX, EBX (66 19 D8)
    let (mut cpu, mut mem) = setup(&[0xF9, 0x66, 0x19, 0xD8]);
    cpu.regs.eax = 10;
    cpu.regs.ebx = 3;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 6); // 10 - 3 - 1(borrow)
}

// ========================================================================
// ADC/SBB AL, imm8
// ========================================================================

#[test]
fn test_adc_al_imm8_carry_set() {
    // STC (F9) then ADC AL, 5 (14 05)
    let (mut cpu, mut mem) = setup(&[0xF9, 0x14, 0x05]);
    cpu.regs.set_al(10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.al(), 16); // 10 + 5 + 1
}

#[test]
fn test_sbb_al_imm8_borrow_set() {
    // STC (F9) then SBB AL, 3 (1C 03)
    let (mut cpu, mut mem) = setup(&[0xF9, 0x1C, 0x03]);
    cpu.regs.set_al(10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.al(), 6); // 10 - 3 - 1
}

// ========================================================================
// I/O instructions (OUT imm8, OUT DX)
// ========================================================================

#[test]
fn test_out_al_imm8() {
    // OUT 0x80, AL = E6 80
    let (mut cpu, mut mem) = setup(&[0xE6, 0x80]);
    cpu.regs.set_al(0x42);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_out_ax_imm8() {
    // OUT 0x80, AX = E7 80
    let (mut cpu, mut mem) = setup(&[0xE7, 0x80]);
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_out_dx_al() {
    // OUT DX, AL = EE
    let (mut cpu, mut mem) = setup(&[0xEE]);
    cpu.regs.set_dx(0x80);
    cpu.regs.set_al(0x55);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_out_dx_ax() {
    // OUT DX, AX = EF
    let (mut cpu, mut mem) = setup(&[0xEF]);
    cpu.regs.set_dx(0x80);
    cpu.regs.set_ax(0xBEEF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_in_eax_imm8_32bit() {
    // IN EAX, 0x40 = 66 E5 40
    let (mut cpu, mut mem) = setup(&[0x66, 0xE5, 0x40]);
    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // () io returns 0xFFFFFFFF for u32 reads
    assert_eq!(cpu.regs.eax, 0xFFFFFFFF);
}

#[test]
fn test_out_eax_imm8_32bit() {
    // OUT 0x40, EAX = 66 E7 40
    let (mut cpu, mut mem) = setup(&[0x66, 0xE7, 0x40]);
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0103);
}

#[test]
fn test_in_eax_dx_32bit() {
    // IN EAX, DX = 66 ED
    let (mut cpu, mut mem) = setup(&[0x66, 0xED]);
    cpu.regs.set_dx(0x40);
    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // () io returns 0xFFFFFFFF for u32 reads
    assert_eq!(cpu.regs.eax, 0xFFFFFFFF);
}

#[test]
fn test_out_eax_dx_32bit() {
    // OUT DX, EAX = 66 EF
    let (mut cpu, mut mem) = setup(&[0x66, 0xEF]);
    cpu.regs.set_dx(0x40);
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ========================================================================
// HLT (0xF4)
// ========================================================================

#[test]
fn test_hlt() {
    let (mut cpu, mut mem) = setup(&[0xF4]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Halt { .. }));
}

// ========================================================================
// INTO (0xCE) — INT 4 if OF set
// ========================================================================

#[test]
fn test_into_overflow_set() {
    let (mut cpu, mut mem) = setup(&[0xCE]);
    cpu.regs.flags.insert(Flags::OVERFLOW);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::SoftwareInterrupt { vector: 4, .. }
    ));
}

#[test]
fn test_into_overflow_clear() {
    let (mut cpu, mut mem) = setup(&[0xCE]);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { .. }));
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ========================================================================
// INT3 (0xCC)
// ========================================================================

#[test]
fn test_int3() {
    let (mut cpu, mut mem) = setup(&[0xCC]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::SoftwareInterrupt { vector: 3, .. }
    ));
}

// ========================================================================
// ICEBP (0xF1) — undocumented INT 1 as software interrupt
// ========================================================================

#[test]
fn test_icebp_returns_software_interrupt() {
    // ICEBP (0xF1) should generate a software interrupt with vector 1
    let (mut cpu, mut mem) = setup(&[0xF1]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::SoftwareInterrupt { vector: 1, .. }
    ));
}

#[test]
fn test_icebp_advances_eip() {
    // ICEBP is a 1-byte instruction; EIP should advance past it
    let (mut cpu, mut mem) = setup(&[0xF1]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ========================================================================
// NOP (0x90)
// ========================================================================

#[test]
fn test_nop() {
    let (mut cpu, mut mem) = setup(&[0x90, 0x90, 0x90]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.regs.eip, 0x0103);
}

// ========================================================================
// XCHG EAX, reg32 (0x91-0x97 with 0x66 prefix)
// ========================================================================

#[test]
fn test_xchg_eax_ecx_32bit() {
    // XCHG EAX, ECX: 66 91
    let (mut cpu, mut mem) = setup(&[0x66, 0x91]);
    cpu.regs.eax = 0x11111111;
    cpu.regs.ecx = 0x22222222;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x22222222);
    assert_eq!(cpu.regs.ecx, 0x11111111);
}

#[test]
fn test_xchg_eax_ebp_32bit() {
    // XCHG EAX, EBP: 66 95
    let (mut cpu, mut mem) = setup(&[0x66, 0x95]);
    cpu.regs.eax = 0xAAAAAAAA;
    cpu.regs.ebp = 0xBBBBBBBB;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xBBBBBBBB);
    assert_eq!(cpu.regs.ebp, 0xAAAAAAAA);
}

// ========================================================================
// XCHG r8, r8 — reg-reg ModR/M form (0x86)
// ========================================================================

#[test]
fn test_xchg_al_bl() {
    // XCHG AL, BL: 86 C3
    let (mut cpu, mut mem) = setup(&[0x86, 0xC3]);
    cpu.regs.set_al(0x11);
    cpu.regs.set_bl(0x22);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x22);
    assert_eq!(cpu.regs.bl(), 0x11);
}

// ========================================================================
// XCHG r32, r/m32 — reg-reg ModR/M form (0x87 with 0x66)
// ========================================================================

#[test]
fn test_xchg_eax_ebx_modrm() {
    // XCHG EAX, EBX: 66 87 C3
    let (mut cpu, mut mem) = setup(&[0x66, 0x87, 0xC3]);
    cpu.regs.eax = 0xAAAA0000;
    cpu.regs.ebx = 0x0000BBBB;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0000BBBB);
    assert_eq!(cpu.regs.ebx, 0xAAAA0000);
}

// ========================================================================
// MOV r/m8, r8 and MOV r8, r/m8 (0x88, 0x8A)
// ========================================================================

#[test]
fn test_mov_rm8_r8() {
    // MOV [0x200], AL: 88 06 00 02
    let (mut cpu, mut mem) = setup(&[0x88, 0x06, 0x00, 0x02]);
    cpu.regs.set_al(0xAB);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0xAB);
}

#[test]
fn test_mov_r8_rm8() {
    // MOV AL, [0x200]: 8A 06 00 02
    let (mut cpu, mut mem) = setup(&[0x8A, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0xCD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xCD);
}

// ========================================================================
// MOV r/m16, r16 and MOV r16, r/m16 (0x89, 0x8B)
// ========================================================================

#[test]
fn test_mov_rm16_r16() {
    // MOV [0x200], AX: 89 06 00 02
    let (mut cpu, mut mem) = setup(&[0x89, 0x06, 0x00, 0x02]);
    cpu.regs.set_ax(0xBEEF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0xBEEF);
}

#[test]
fn test_mov_r16_rm16() {
    // MOV AX, [0x200]: 8B 06 00 02
    let (mut cpu, mut mem) = setup(&[0x8B, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0xEF;
    mem.ram[0x201] = 0xBE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xBEEF);
}

// ========================================================================
// MOV r/m32, r32 and MOV r32, r/m32 (0x89, 0x8B with 0x66)
// ========================================================================

#[test]
fn test_mov_rm32_r32() {
    // MOV [0x200], EAX: 66 89 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x89, 0x06, 0x00, 0x02]);
    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0xDEADBEEF);
}

#[test]
fn test_mov_r32_rm32() {
    // MOV EAX, [0x200]: 66 8B 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x8B, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0x78;
    mem.ram[0x201] = 0x56;
    mem.ram[0x202] = 0x34;
    mem.ram[0x203] = 0x12;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12345678);
}

// ========================================================================
// MOV AL/AX/EAX, moffs (0xA0, 0xA1)
// ========================================================================

#[test]
fn test_mov_al_moffs() {
    // MOV AL, [0x300]: A0 00 03
    let (mut cpu, mut mem) = setup(&[0xA0, 0x00, 0x03]);
    mem.ram[0x300] = 0x42;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x42);
}

#[test]
fn test_mov_ax_moffs() {
    // MOV AX, [0x300]: A1 00 03
    let (mut cpu, mut mem) = setup(&[0xA1, 0x00, 0x03]);
    mem.ram[0x300] = 0xEF;
    mem.ram[0x301] = 0xBE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xBEEF);
}

// ========================================================================
// MOV moffs, AL/AX/EAX (0xA2, 0xA3)
// ========================================================================

#[test]
fn test_mov_moffs_al() {
    // MOV [0x300], AL: A2 00 03
    let (mut cpu, mut mem) = setup(&[0xA2, 0x00, 0x03]);
    cpu.regs.set_al(0x99);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x300], 0x99);
}

#[test]
fn test_mov_moffs_ax() {
    // MOV [0x300], AX: A3 00 03
    let (mut cpu, mut mem) = setup(&[0xA3, 0x00, 0x03]);
    cpu.regs.set_ax(0xCAFE);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x300], mem.ram[0x301]]), 0xCAFE);
}

// ========================================================================
// MOV r8, imm8 (0xB0-0xB7)
// ========================================================================

#[test]
fn test_mov_cl_imm8() {
    // MOV CL, 0x42: B1 42
    let (mut cpu, mut mem) = setup(&[0xB1, 0x42]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cl(), 0x42);
}

#[test]
fn test_mov_dh_imm8() {
    // MOV DH, 0xFF: B6 FF
    let (mut cpu, mut mem) = setup(&[0xB6, 0xFF]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.dh(), 0xFF);
}

#[test]
fn test_mov_bh_imm8() {
    // MOV BH, 0xAA: B7 AA
    let (mut cpu, mut mem) = setup(&[0xB7, 0xAA]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.bh(), 0xAA);
}

// ========================================================================
// MOV reg32, imm32 (0xB8-0xBF with 0x66)
// ========================================================================

#[test]
fn test_mov_eax_imm32() {
    // MOV EAX, 0xDEADBEEF: 66 B8 EF BE AD DE
    let (mut cpu, mut mem) = setup(&[0x66, 0xB8, 0xEF, 0xBE, 0xAD, 0xDE]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xDEADBEEF);
}

#[test]
fn test_mov_ecx_imm32() {
    // MOV ECX, 0x12345678: 66 B9 78 56 34 12
    let (mut cpu, mut mem) = setup(&[0x66, 0xB9, 0x78, 0x56, 0x34, 0x12]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0x12345678);
}

#[test]
fn test_mov_edi_imm32() {
    // MOV EDI, 0x11223344: 66 BF 44 33 22 11
    let (mut cpu, mut mem) = setup(&[0x66, 0xBF, 0x44, 0x33, 0x22, 0x11]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edi, 0x11223344);
}

// ========================================================================
// PUSH/POP reg16 (0x50-0x5F)
// ========================================================================

#[test]
fn test_push_pop_bx() {
    // PUSH BX (53), POP CX (59)
    let (mut cpu, mut mem) = setup(&[0x53, 0x59]);
    cpu.regs.set_bx(0xCAFE);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.cx(), 0xCAFE);
    assert_eq!(cpu.regs.sp(), 0x8000); // SP restored after push+pop
}

#[test]
fn test_push_pop_si() {
    // PUSH SI (56), POP DI (5F)
    let (mut cpu, mut mem) = setup(&[0x56, 0x5F]);
    cpu.regs.set_si(0x1234);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.di(), 0x1234);
}

// ========================================================================
// PUSH/POP imm (0x68, 0x6A)
// ========================================================================

#[test]
fn test_push_imm16() {
    // PUSH 0x1234: 68 34 12
    let (mut cpu, mut mem) = setup(&[0x68, 0x34, 0x12]);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.sp(), 0x7FFE);
    let val = u16::from_le_bytes([mem.ram[0x7FFE], mem.ram[0x7FFF]]);
    assert_eq!(val, 0x1234);
}

#[test]
fn test_push_imm8_sign_ext() {
    // PUSH -1 (sign-extended): 6A FF
    let (mut cpu, mut mem) = setup(&[0x6A, 0xFF]);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.sp(), 0x7FFE);
    let val = u16::from_le_bytes([mem.ram[0x7FFE], mem.ram[0x7FFF]]);
    assert_eq!(val, 0xFFFF);
}

// ========================================================================
// INC/DEC reg16 (0x40-0x4F)
// ========================================================================

#[test]
fn test_inc_ax() {
    // INC AX: 40
    let (mut cpu, mut mem) = setup(&[0x40]);
    cpu.regs.set_ax(0xFFFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0000);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_dec_bx() {
    // DEC BX: 4B
    let (mut cpu, mut mem) = setup(&[0x4B]);
    cpu.regs.set_bx(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.bx(), 0);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_inc_all_regs() {
    // INC AX, INC CX, INC DX, INC BX, INC SP, INC BP, INC SI, INC DI
    let (mut cpu, mut mem) = setup(&[0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47]);
    cpu.regs.set_ax(0);
    cpu.regs.set_cx(0);
    cpu.regs.set_dx(0);
    cpu.regs.set_bx(0);
    cpu.regs.esp = 100;
    cpu.regs.ebp = 0;
    cpu.regs.set_si(0);
    cpu.regs.set_di(0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 8);
    assert_eq!(cpu.regs.ax(), 1);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(cpu.regs.dx(), 1);
    assert_eq!(cpu.regs.bx(), 1);
    assert_eq!(cpu.regs.sp(), 101);
    assert_eq!(cpu.regs.bp(), 1);
    assert_eq!(cpu.regs.si(), 1);
    assert_eq!(cpu.regs.di(), 1);
}

#[test]
fn test_dec_all_regs() {
    // DEC AX..DI
    let (mut cpu, mut mem) = setup(&[0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F]);
    cpu.regs.set_ax(5);
    cpu.regs.set_cx(5);
    cpu.regs.set_dx(5);
    cpu.regs.set_bx(5);
    cpu.regs.esp = 100;
    cpu.regs.ebp = 5;
    cpu.regs.set_si(5);
    cpu.regs.set_di(5);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 8);
    assert_eq!(cpu.regs.ax(), 4);
    assert_eq!(cpu.regs.cx(), 4);
    assert_eq!(cpu.regs.dx(), 4);
    assert_eq!(cpu.regs.bx(), 4);
    assert_eq!(cpu.regs.sp(), 99);
    assert_eq!(cpu.regs.bp(), 4);
    assert_eq!(cpu.regs.si(), 4);
    assert_eq!(cpu.regs.di(), 4);
}

// ========================================================================
// INC/DEC reg32 (0x40-0x4F with 0x66)
// ========================================================================

#[test]
fn test_inc_eax_32bit() {
    // INC EAX: 66 40
    let (mut cpu, mut mem) = setup(&[0x66, 0x40]);
    cpu.regs.eax = 0xFFFFFFFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_dec_ecx_32bit() {
    // DEC ECX: 66 49
    let (mut cpu, mut mem) = setup(&[0x66, 0x49]);
    cpu.regs.ecx = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0x7FFFFFFF);
}

// ========================================================================
// INC/DEC r/m8 (0xFE /0, /1)
// ========================================================================

#[test]
fn test_inc_rm8_reg() {
    // INC CL: FE C1
    let (mut cpu, mut mem) = setup(&[0xFE, 0xC1]);
    cpu.regs.set_cl(0x0F);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cl(), 0x10);
}

#[test]
fn test_dec_rm8_reg() {
    // DEC DL: FE CA
    let (mut cpu, mut mem) = setup(&[0xFE, 0xCA]);
    cpu.regs.set_dl(0x00);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.dl(), 0xFF);
}

#[test]
fn test_inc_rm8_mem() {
    // INC byte [0x200]: FE 06 00 02
    let (mut cpu, mut mem) = setup(&[0xFE, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0xFE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0xFF);
}

#[test]
fn test_dec_rm8_mem() {
    // DEC byte [0x200]: FE 0E 00 02
    let (mut cpu, mut mem) = setup(&[0xFE, 0x0E, 0x00, 0x02]);
    mem.ram[0x200] = 0x01;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x00);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// INC/DEC r/m16 (0xFF /0, /1)
// ========================================================================

#[test]
fn test_inc_rm16_reg() {
    // INC AX: FF C0
    let (mut cpu, mut mem) = setup(&[0xFF, 0xC0]);
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1235);
}

#[test]
fn test_dec_rm16_reg() {
    // DEC CX: FF C9
    let (mut cpu, mut mem) = setup(&[0xFF, 0xC9]);
    cpu.regs.set_cx(0x1000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cx(), 0x0FFF);
}

// ========================================================================
// Group 1: 0x80, 0x81, 0x83 (ALU r/m, imm)
// ========================================================================

#[test]
fn test_add_rm8_imm8() {
    // ADD [0x200], 5: 80 06 00 02 05
    let (mut cpu, mut mem) = setup(&[0x80, 0x06, 0x00, 0x02, 0x05]);
    mem.ram[0x200] = 10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 15);
}

#[test]
fn test_sub_rm8_imm8() {
    // SUB CL, 3: 80 E9 03
    let (mut cpu, mut mem) = setup(&[0x80, 0xE9, 0x03]);
    cpu.regs.set_cl(10);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cl(), 7);
}

#[test]
fn test_and_rm8_imm8() {
    // AND AL, 0x0F: 80 E0 0F
    let (mut cpu, mut mem) = setup(&[0x80, 0xE0, 0x0F]);
    cpu.regs.set_al(0xFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x0F);
}

#[test]
fn test_or_rm8_imm8() {
    // OR AL, 0xF0: 80 C8 F0
    let (mut cpu, mut mem) = setup(&[0x80, 0xC8, 0xF0]);
    cpu.regs.set_al(0x0F);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xFF);
}

#[test]
fn test_xor_rm8_imm8() {
    // XOR AL, 0xFF: 80 F0 FF
    let (mut cpu, mut mem) = setup(&[0x80, 0xF0, 0xFF]);
    cpu.regs.set_al(0xAA);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x55);
}

#[test]
fn test_cmp_rm8_imm8() {
    // CMP AL, 0x42: 80 F8 42
    let (mut cpu, mut mem) = setup(&[0x80, 0xF8, 0x42]);
    cpu.regs.set_al(0x42);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// Group 1: 0x81 (ALU r/m16, imm16)
// ========================================================================

#[test]
fn test_add_rm16_imm16() {
    // ADD AX, 0x100: 81 C0 00 01
    let (mut cpu, mut mem) = setup(&[0x81, 0xC0, 0x00, 0x01]);
    cpu.regs.set_ax(0x200);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x300);
}

#[test]
fn test_or_rm16_imm16() {
    // OR BX, 0xFF00: 81 CB 00 FF
    let (mut cpu, mut mem) = setup(&[0x81, 0xCB, 0x00, 0xFF]);
    cpu.regs.set_bx(0x00FF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.bx(), 0xFFFF);
}

// ========================================================================
// Group 1: 0x83 (ALU r/m16, imm8 sign-extended)
// ========================================================================

#[test]
fn test_add_rm16_imm8_sign_ext() {
    // ADD AX, -1 (0xFF sign-extended to 0xFFFF): 83 C0 FF
    let (mut cpu, mut mem) = setup(&[0x83, 0xC0, 0xFF]);
    cpu.regs.set_ax(0x100);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFF); // 0x100 + (-1) = 0xFF
}

#[test]
fn test_sub_rm16_imm8_sign_ext() {
    // SUB CX, 5: 83 E9 05
    let (mut cpu, mut mem) = setup(&[0x83, 0xE9, 0x05]);
    cpu.regs.set_cx(100);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cx(), 95);
}

#[test]
fn test_cmp_rm16_imm8() {
    // CMP AX, 0: 83 F8 00
    let (mut cpu, mut mem) = setup(&[0x83, 0xF8, 0x00]);
    cpu.regs.set_ax(0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// Group 1: 0x83 with 32-bit operand (66 83)
// ========================================================================

#[test]
fn test_add_rm32_imm8_sign_ext() {
    // ADD EAX, -1: 66 83 C0 FF
    let (mut cpu, mut mem) = setup(&[0x66, 0x83, 0xC0, 0xFF]);
    cpu.regs.eax = 0x100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFF);
}

#[test]
fn test_sub_rm32_imm8() {
    // SUB EAX, 1: 66 83 E8 01
    let (mut cpu, mut mem) = setup(&[0x66, 0x83, 0xE8, 0x01]);
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x7FFFFFFF);
}

#[test]
fn test_and_rm32_imm8() {
    // AND EAX, 0x0F: 66 83 E0 0F
    let (mut cpu, mut mem) = setup(&[0x66, 0x83, 0xE0, 0x0F]);
    cpu.regs.eax = 0xFF00FFFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0F);
}

// ========================================================================
// TEST r/m8, r8 (0x84) and TEST r/m16, r16 (0x85)
// ========================================================================

#[test]
fn test_test_r8_r8() {
    // TEST AL, BL: 84 C3
    let (mut cpu, mut mem) = setup(&[0x84, 0xC3]);
    cpu.regs.set_al(0x0F);
    cpu.regs.set_bl(0xF0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO)); // no bits in common
}

#[test]
fn test_test_r16_r16() {
    // TEST AX, BX: 85 C3
    let (mut cpu, mut mem) = setup(&[0x85, 0xC3]);
    cpu.regs.set_ax(0xFF00);
    cpu.regs.set_bx(0x00FF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_test_r16_r16_nonzero() {
    // TEST AX, BX: 85 C3
    let (mut cpu, mut mem) = setup(&[0x85, 0xC3]);
    cpu.regs.set_ax(0x0F0F);
    cpu.regs.set_bx(0x0F0F);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// TEST AL, imm8 (0xA8) and TEST AX, imm16 (0xA9)
// ========================================================================

#[test]
fn test_test_al_imm8() {
    // TEST AL, 0x01: A8 01
    let (mut cpu, mut mem) = setup(&[0xA8, 0x01]);
    cpu.regs.set_al(0xFE);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO)); // 0xFE & 0x01 = 0
}

#[test]
fn test_test_ax_imm16() {
    // TEST AX, 0x8000: A9 00 80
    let (mut cpu, mut mem) = setup(&[0xA9, 0x00, 0x80]);
    cpu.regs.set_ax(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// TEST EAX, imm32 (0x66 0xA9)
// ========================================================================

#[test]
fn test_test_eax_imm32() {
    // TEST EAX, 0x80000000: 66 A9 00 00 00 80
    let (mut cpu, mut mem) = setup(&[0x66, 0xA9, 0x00, 0x00, 0x00, 0x80]);
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// TEST r/m32, r32 (0x66 0x85)
// ========================================================================

#[test]
fn test_test_r32_r32() {
    // TEST EAX, EBX: 66 85 C3
    let (mut cpu, mut mem) = setup(&[0x66, 0x85, 0xC3]);
    cpu.regs.eax = 0x12340000;
    cpu.regs.ebx = 0x00005678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// Shift/Rotate 32-bit (0x66 prefix)
// ========================================================================

#[test]
fn test_shl_eax_cl() {
    // SHL EAX, CL: 66 D3 E0
    let (mut cpu, mut mem) = setup(&[0x66, 0xD3, 0xE0]);
    cpu.regs.eax = 1;
    cpu.regs.set_cl(16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x10000);
}

#[test]
fn test_shr_eax_cl() {
    // SHR EAX, CL: 66 D3 E8
    let (mut cpu, mut mem) = setup(&[0x66, 0xD3, 0xE8]);
    cpu.regs.eax = 0x80000000;
    cpu.regs.set_cl(16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x8000);
}

#[test]
fn test_sar_eax_cl() {
    // SAR EAX, CL: 66 D3 F8
    let (mut cpu, mut mem) = setup(&[0x66, 0xD3, 0xF8]);
    cpu.regs.eax = 0x80000000;
    cpu.regs.set_cl(16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF8000); // sign-extended
}

#[test]
fn test_rol_eax_cl() {
    // ROL EAX, CL: 66 D3 C0
    let (mut cpu, mut mem) = setup(&[0x66, 0xD3, 0xC0]);
    cpu.regs.eax = 0x80000001;
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000003);
}

#[test]
fn test_ror_eax_cl() {
    // ROR EAX, CL: 66 D3 C8
    let (mut cpu, mut mem) = setup(&[0x66, 0xD3, 0xC8]);
    cpu.regs.eax = 0x00000001;
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x80000000);
}

#[test]
fn test_rcl_eax_cl() {
    // RCL EAX, CL: 66 D3 D0
    let (mut cpu, mut mem) = setup(&[0x66, 0xD3, 0xD0]);
    cpu.regs.eax = 0x80000000;
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_rcr_eax_cl() {
    // RCR EAX, CL: 66 D3 D8
    let (mut cpu, mut mem) = setup(&[0x66, 0xD3, 0xD8]);
    cpu.regs.eax = 1;
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// ========================================================================
// Shift 32-bit by imm8 (0x66 0xC1)
// ========================================================================

#[test]
fn test_shl_eax_imm8() {
    // SHL EAX, 8: 66 C1 E0 08
    let (mut cpu, mut mem) = setup(&[0x66, 0xC1, 0xE0, 0x08]);
    cpu.regs.eax = 0x00FF00FF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFF00FF00);
}

#[test]
fn test_shr_eax_imm8() {
    // SHR EAX, 8: 66 C1 E8 08
    let (mut cpu, mut mem) = setup(&[0x66, 0xC1, 0xE8, 0x08]);
    cpu.regs.eax = 0xFF00FF00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00FF00FF);
}

#[test]
fn test_sar_eax_imm8() {
    // SAR EAX, 4: 66 C1 F8 04
    let (mut cpu, mut mem) = setup(&[0x66, 0xC1, 0xF8, 0x04]);
    cpu.regs.eax = 0xF0000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFF000000);
}

// ========================================================================
// Shift by 1 (0xD0, 0xD1 for 8/16-bit)
// ========================================================================

#[test]
fn test_shl_al_1() {
    // SHL AL, 1: D0 E0
    let (mut cpu, mut mem) = setup(&[0xD0, 0xE0]);
    cpu.regs.set_al(0x80);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shr_al_1() {
    // SHR AL, 1: D0 E8
    let (mut cpu, mut mem) = setup(&[0xD0, 0xE8]);
    cpu.regs.set_al(0x01);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_sar_al_1_negative() {
    // SAR AL, 1: D0 F8 (with negative value)
    let (mut cpu, mut mem) = setup(&[0xD0, 0xF8]);
    cpu.regs.set_al(0x80);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xC0); // sign-extended
}

#[test]
fn test_shl_ax_1() {
    // SHL AX, 1: D1 E0
    let (mut cpu, mut mem) = setup(&[0xD1, 0xE0]);
    cpu.regs.set_ax(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shr_ax_1() {
    // SHR AX, 1: D1 E8
    let (mut cpu, mut mem) = setup(&[0xD1, 0xE8]);
    cpu.regs.set_ax(0x0001);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// ========================================================================
// Shift memory by CL (0xD2/0xD3)
// ========================================================================

#[test]
fn test_shl_mem8_cl_zero_count() {
    // SHL [0x200], CL with CL=0: D2 26 00 02
    let (mut cpu, mut mem) = setup(&[0xD2, 0x26, 0x00, 0x02]);
    mem.ram[0x200] = 0xFF;
    cpu.regs.set_cl(0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0xFF); // unchanged
}

// ========================================================================
// Shift by imm8 (0xC0/0xC1 for 8/16-bit)
// ========================================================================

#[test]
fn test_shl_al_imm8() {
    // SHL AL, 4: C0 E0 04
    let (mut cpu, mut mem) = setup(&[0xC0, 0xE0, 0x04]);
    cpu.regs.set_al(0x0F);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xF0);
}

#[test]
fn test_shr_al_imm8() {
    // SHR AL, 4: C0 E8 04
    let (mut cpu, mut mem) = setup(&[0xC0, 0xE8, 0x04]);
    cpu.regs.set_al(0xF0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x0F);
}

#[test]
fn test_shl_ax_imm8() {
    // SHL AX, 4: C1 E0 04
    let (mut cpu, mut mem) = setup(&[0xC1, 0xE0, 0x04]);
    cpu.regs.set_ax(0x0FFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFFF0);
}

#[test]
fn test_shr_ax_imm8() {
    // SHR AX, 4: C1 E8 04
    let (mut cpu, mut mem) = setup(&[0xC1, 0xE8, 0x04]);
    cpu.regs.set_ax(0xFFF0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0FFF);
}

// ========================================================================
// RCL/RCR 16-bit by 1
// ========================================================================

#[test]
fn test_rcl_ax_1() {
    // RCL AX, 1: D1 D0
    let (mut cpu, mut mem) = setup(&[0xD1, 0xD0]);
    cpu.regs.set_ax(0x8000);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0001); // CF rotated in as bit 0
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // old bit 15 is new CF
}

#[test]
fn test_rcr_ax_1() {
    // RCR AX, 1: D1 D8
    let (mut cpu, mut mem) = setup(&[0xD1, 0xD8]);
    cpu.regs.set_ax(0x0001);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x8000); // CF rotated in as bit 15
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // old bit 0 is new CF
}

/// A rotate whose masked count reduces to zero: the operand never changes, but
/// whether the FLAGS change depends on which rotate it is.
///
/// ROL/ROR write CF even though the rotate is a no-op — the SDM guards the CF
/// update on the masked count, not the reduced one, and silicon agrees (ROL AL,24
/// with AL=0x80 and CF=1 returns CF=0). RCL/RCR genuinely restore everything,
/// because a full 9-/17-bit rotation is the identity on both operand and carry.
///
/// Commit 1e1b04300 collapsed those two cases into one and returned early before
/// the CF write for all four, which is the bug this pins. It stayed green because
/// `Cpu::step()` resolved to the legacy oracle in test builds until 2026.08.18.
#[test]
fn test_effective_zero_byte_rotates_keep_the_operand_and_write_cf_only_for_rol_ror() {
    // (name, modrm, count, value, writes_cf)
    for (name, modrm, count, value, writes_cf) in [
        ("ROL AL, 8", 0xC0, 8, 0x01, true),
        ("ROL AL, 8 (CF cleared by the write)", 0xC0, 8, 0x80, true),
        ("ROR AL, 8", 0xC8, 8, 0x80, true),
        ("ROR AL, 8 (CF cleared by the write)", 0xC8, 8, 0x6D, true),
        ("RCL AL, 9", 0xD0, 9, 0xA5, false),
        ("RCR AL, 9", 0xD8, 9, 0xA5, false),
    ] {
        for carry_set in [false, true] {
            let (mut cpu, mut mem) = setup(&[0xC0, modrm, count]);
            cpu.regs.set_al(value);
            cpu.regs.flags = Flags::RESERVED1
                | Flags::PARITY
                | Flags::AUX_CARRY
                | Flags::ZERO
                | Flags::SIGN
                | Flags::DIRECTION
                | Flags::OVERFLOW;
            cpu.regs.flags.set(Flags::CARRY, carry_set);
            let flags_before = cpu.regs.flags;

            let mut io = ();
            let result = run(&mut cpu, &mut mem, &mut io, 1);

            assert!(
                matches!(result, RunResult::BudgetExhausted { executed: 1 }),
                "{name} with initial CF={carry_set} should execute, got {result:?}"
            );
            assert_eq!(
                cpu.regs.al(),
                value,
                "{name} with initial CF={carry_set} changed the operand"
            );

            // CF is the only flag that may move; everything else is untouched.
            let mut expected = flags_before;
            if writes_cf {
                let rol = name.starts_with("ROL");
                let bit = if rol {
                    value & 1 != 0
                } else {
                    value & 0x80 != 0
                };
                expected.set(Flags::CARRY, bit);
            }
            assert_eq!(
                cpu.regs.flags, expected,
                "{name} with initial CF={carry_set} produced the wrong flags"
            );
        }
    }
}

/// A rotate whose masked count reduces to zero: the operand never changes, but
/// whether the FLAGS change depends on which rotate it is.
///
/// ROL/ROR write CF even though the rotate is a no-op — the SDM guards the CF
/// update on the masked count, not the reduced one, and silicon agrees (ROL AL,24
/// with AL=0x80 and CF=1 returns CF=0). RCL/RCR genuinely restore everything,
/// because a full 9-/17-bit rotation is the identity on both operand and carry.
///
/// Commit 1e1b04300 collapsed those two cases into one and returned early before
/// the CF write for all four, which is the bug this pins. It stayed green because
/// `Cpu::step()` resolved to the legacy oracle in test builds until 2026.08.18.
#[test]
fn test_effective_zero_word_rotates_keep_the_operand_and_write_cf_only_for_rol_ror() {
    // (name, modrm, count, value, writes_cf)
    for (name, modrm, count, value, writes_cf) in [
        ("ROL AX, 16", 0xC0, 16, 0x0001, true),
        (
            "ROL AX, 16 (CF cleared by the write)",
            0xC0,
            16,
            0x8000,
            true,
        ),
        ("ROR AX, 16", 0xC8, 16, 0x8000, true),
        (
            "ROR AX, 16 (CF cleared by the write)",
            0xC8,
            16,
            0x0001,
            true,
        ),
        ("RCL AX, 17", 0xD0, 17, 0xA5A5, false),
        ("RCR AX, 17", 0xD8, 17, 0xA5A5, false),
    ] {
        for carry_set in [false, true] {
            let (mut cpu, mut mem) = setup(&[0xC1, modrm, count]);
            cpu.regs.set_ax(value);
            cpu.regs.flags = Flags::RESERVED1
                | Flags::PARITY
                | Flags::AUX_CARRY
                | Flags::ZERO
                | Flags::SIGN
                | Flags::DIRECTION
                | Flags::OVERFLOW;
            cpu.regs.flags.set(Flags::CARRY, carry_set);
            let flags_before = cpu.regs.flags;

            let mut io = ();
            let result = run(&mut cpu, &mut mem, &mut io, 1);

            assert!(
                matches!(result, RunResult::BudgetExhausted { executed: 1 }),
                "{name} with initial CF={carry_set} should execute, got {result:?}"
            );
            assert_eq!(
                cpu.regs.ax(),
                value,
                "{name} with initial CF={carry_set} changed the operand"
            );

            // CF is the only flag that may move; everything else is untouched.
            let mut expected = flags_before;
            if writes_cf {
                let rol = name.starts_with("ROL");
                let bit = if rol {
                    value & 1 != 0
                } else {
                    value & 0x8000 != 0
                };
                expected.set(Flags::CARRY, bit);
            }
            assert_eq!(
                cpu.regs.flags, expected,
                "{name} with initial CF={carry_set} produced the wrong flags"
            );
        }
    }
}

// ========================================================================
// MOVS/CMPS/STOS/LODS with direction flag backward
// ========================================================================

#[test]
fn test_movsb_backward() {
    // STD (FD), MOVSB (A4)
    let (mut cpu, mut mem) = setup(&[0xFD, 0xA4]);
    mem.ram[0x300] = 0xAB;
    cpu.regs.esi = 0x300;
    cpu.regs.edi = 0x400;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(mem.ram[0x400], 0xAB);
    assert_eq!(cpu.regs.si(), 0x2FF); // decremented
    assert_eq!(cpu.regs.di(), 0x3FF);
}

#[test]
fn test_movsw_forward() {
    // MOVSW: A5
    let (mut cpu, mut mem) = setup(&[0xA5]);
    mem.ram[0x300] = 0xEF;
    mem.ram[0x301] = 0xBE;
    cpu.regs.esi = 0x300;
    cpu.regs.edi = 0x400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x400], mem.ram[0x401]]), 0xBEEF);
    assert_eq!(cpu.regs.si(), 0x302);
    assert_eq!(cpu.regs.di(), 0x402);
}

#[test]
fn test_cmpsb_greater() {
    // CMPSB: A6 (when DS:SI > ES:DI)
    let (mut cpu, mut mem) = setup(&[0xA6]);
    mem.ram[0x300] = 0x10; // source
    mem.ram[0x400] = 0x05; // dest
    cpu.regs.esi = 0x300;
    cpu.regs.edi = 0x400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // 0x10 > 0x05
}

#[test]
fn test_cmpsd_equal() {
    // CMPSD: 66 A7
    let (mut cpu, mut mem) = setup(&[0x66, 0xA7]);
    let val: u32 = 0x12345678;
    let bytes = val.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x300 + i] = b;
        mem.ram[0x400 + i] = b;
    }
    cpu.regs.esi = 0x300;
    cpu.regs.edi = 0x400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_scasd_equal() {
    // SCASD: 66 AF
    let (mut cpu, mut mem) = setup(&[0x66, 0xAF]);
    let val: u32 = 0xDEADBEEF;
    let bytes = val.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x400 + i] = b;
    }
    cpu.regs.eax = 0xDEADBEEF;
    cpu.regs.edi = 0x400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// MOVSD (A5 with 66 prefix)
// ========================================================================

#[test]
fn test_movsd_forward() {
    // MOVSD: 66 A5
    let (mut cpu, mut mem) = setup(&[0x66, 0xA5]);
    let bytes = 0xDEADBEEFu32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x300 + i] = b;
    }
    cpu.regs.esi = 0x300;
    cpu.regs.edi = 0x400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x400],
        mem.ram[0x401],
        mem.ram[0x402],
        mem.ram[0x403],
    ]);
    assert_eq!(val, 0xDEADBEEF);
    assert_eq!(cpu.regs.si(), 0x304);
    assert_eq!(cpu.regs.di(), 0x404);
}

// ========================================================================
// LODSW/LODSD (AD with/without 66)
// ========================================================================

#[test]
fn test_lodsw_forward() {
    // LODSW: AD
    let (mut cpu, mut mem) = setup(&[0xAD]);
    mem.ram[0x300] = 0xEF;
    mem.ram[0x301] = 0xBE;
    cpu.regs.esi = 0x300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xBEEF);
    assert_eq!(cpu.regs.si(), 0x302);
}

#[test]
fn test_lodsd_forward() {
    // LODSD: 66 AD
    let (mut cpu, mut mem) = setup(&[0x66, 0xAD]);
    let bytes = 0x12345678u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x300 + i] = b;
    }
    cpu.regs.esi = 0x300;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12345678);
    assert_eq!(cpu.regs.si(), 0x304);
}

// ========================================================================
// STOSW/STOSD (AB with/without 66)
// ========================================================================

#[test]
fn test_stosw_forward() {
    // STOSW: AB
    let (mut cpu, mut mem) = setup(&[0xAB]);
    cpu.regs.set_ax(0xCAFE);
    cpu.regs.edi = 0x400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x400], mem.ram[0x401]]), 0xCAFE);
    assert_eq!(cpu.regs.di(), 0x402);
}

#[test]
fn test_stosd_forward() {
    // STOSD: 66 AB
    let (mut cpu, mut mem) = setup(&[0x66, 0xAB]);
    cpu.regs.eax = 0xDEADBEEF;
    cpu.regs.edi = 0x400;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x400],
        mem.ram[0x401],
        mem.ram[0x402],
        mem.ram[0x403],
    ]);
    assert_eq!(val, 0xDEADBEEF);
    assert_eq!(cpu.regs.di(), 0x404);
}

// ========================================================================
// REP string ops: REP CMPSB, REPNE CMPSB, REP CMPSD
// ========================================================================

#[test]
fn test_rep_cmpsb_equal_strings() {
    // REP CMPSB: F3 A6
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA6]);
    for i in 0..4u8 {
        mem.ram[0x300 + i as usize] = i + 1;
        mem.ram[0x400 + i as usize] = i + 1;
    }
    cpu.regs.esi = 0x300;
    cpu.regs.edi = 0x400;
    cpu.regs.set_cx(4);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 10);
    assert_eq!(cpu.regs.cx(), 0); // all compared
    assert!(cpu.regs.flags.contains(Flags::ZERO)); // all equal
}

#[test]
fn test_repne_cmpsb_find_match() {
    // REPNE CMPSB: F2 A6
    let (mut cpu, mut mem) = setup(&[0xF2, 0xA6]);
    mem.ram[0x300] = 1;
    mem.ram[0x301] = 2;
    mem.ram[0x302] = 3;
    mem.ram[0x400] = 0; // no match
    mem.ram[0x401] = 0; // no match
    mem.ram[0x402] = 3; // match!
    cpu.regs.esi = 0x300;
    cpu.regs.edi = 0x400;
    cpu.regs.set_cx(3);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// REP MOVSD
// ========================================================================

#[test]
fn test_rep_movsd() {
    // REP MOVSD: F3 66 A5
    let (mut cpu, mut mem) = setup(&[0xF3, 0x66, 0xA5]);
    for i in 0..8u8 {
        mem.ram[0x300 + i as usize] = i + 1;
    }
    cpu.regs.esi = 0x300;
    cpu.regs.edi = 0x400;
    cpu.regs.set_cx(2);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 10);
    assert_eq!(cpu.regs.cx(), 0);
    for i in 0..8u8 {
        assert_eq!(mem.ram[0x400 + i as usize], i + 1);
    }
}

// ========================================================================
// REP LODSW
// ========================================================================

#[test]
fn test_rep_lodsw() {
    // REP LODSW: F3 AD
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAD]);
    mem.ram[0x300] = 0x01;
    mem.ram[0x301] = 0x00;
    mem.ram[0x302] = 0x02;
    mem.ram[0x303] = 0x00;
    cpu.regs.esi = 0x300;
    cpu.regs.set_cx(2);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 10);
    assert_eq!(cpu.regs.cx(), 0);
    assert_eq!(cpu.regs.ax(), 0x0002); // last loaded word
}

// ========================================================================
// JCXZ/JECXZ (0xE3)
// ========================================================================

#[test]
fn test_jcxz_taken() {
    // JCXZ +4: E3 04 (16-bit addr mode, CX=0)
    let (mut cpu, mut mem) = setup(&[0xE3, 0x04, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.set_cx(0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0106); // 0x100 + 2 + 4
}

#[test]
fn test_jcxz_not_taken() {
    // JCXZ +4: E3 04 (CX != 0)
    let (mut cpu, mut mem) = setup(&[0xE3, 0x04, 0x90]);
    cpu.regs.set_cx(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ========================================================================
// LOOPE/LOOPNE (0xE0/0xE1)
// ========================================================================

#[test]
fn test_loopne_cx_addr16() {
    // LOOPNE -2 (0xFE): E0 FE — loop while CX!=0 and ZF=0
    let (mut cpu, mut mem) = setup(&[0xE0, 0xFE]);
    cpu.regs.set_cx(3);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn test_loope_stops_on_nz() {
    // LOOPE -2: E1 FE — loop while CX!=0 and ZF=1
    let (mut cpu, mut mem) = setup(&[0xE1, 0xFE]);
    cpu.regs.set_cx(10);
    cpu.regs.flags.remove(Flags::ZERO); // ZF=0 → should not loop
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cx(), 9); // decremented once, but didn't loop back
}

// ========================================================================
// Short conditional jumps (0x70-0x7F) — untested conditions
// ========================================================================

#[test]
fn test_jo_taken() {
    // JO +2: 70 02
    let (mut cpu, mut mem) = setup(&[0x70, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jo_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x70, 0x02, 0x90]);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0102);
}

#[test]
fn test_jno_taken() {
    let (mut cpu, mut mem) = setup(&[0x71, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jb_taken() {
    let (mut cpu, mut mem) = setup(&[0x72, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jnb_taken() {
    let (mut cpu, mut mem) = setup(&[0x73, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jz_taken() {
    let (mut cpu, mut mem) = setup(&[0x74, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jnz_taken() {
    let (mut cpu, mut mem) = setup(&[0x75, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jbe_taken_carry() {
    let (mut cpu, mut mem) = setup(&[0x76, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jbe_taken_zero() {
    let (mut cpu, mut mem) = setup(&[0x76, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_ja_taken() {
    let (mut cpu, mut mem) = setup(&[0x77, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_js_taken() {
    let (mut cpu, mut mem) = setup(&[0x78, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jns_taken() {
    let (mut cpu, mut mem) = setup(&[0x79, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jp_taken() {
    let (mut cpu, mut mem) = setup(&[0x7A, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::PARITY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jnp_taken() {
    let (mut cpu, mut mem) = setup(&[0x7B, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::PARITY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jl_taken() {
    // JL: SF != OF
    let (mut cpu, mut mem) = setup(&[0x7C, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jge_taken() {
    // JGE: SF == OF
    let (mut cpu, mut mem) = setup(&[0x7D, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.insert(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jle_taken() {
    // JLE: ZF=1 or SF!=OF
    let (mut cpu, mut mem) = setup(&[0x7E, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jg_taken() {
    // JG: ZF=0 and SF==OF
    let (mut cpu, mut mem) = setup(&[0x7F, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

// ========================================================================
// Short backward jumps
// ========================================================================

#[test]
fn test_jmp_short_backward() {
    // JMP -2 (infinite loop): EB FE
    let (mut cpu, mut mem) = setup(&[0xEB, 0xFE]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    assert_eq!(cpu.regs.eip, 0x0100); // loops back to self
}

// ========================================================================
// JMP near (0xE9)
// ========================================================================

#[test]
fn test_jmp_near_forward() {
    // JMP +4: E9 04 00
    let (mut cpu, mut mem) = setup(&[0xE9, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0107);
}

// ========================================================================
// CALL near (0xE8)
// ========================================================================

#[test]
fn test_call_near() {
    // CALL +3: E8 03 00 followed by NOPs, then HLT
    let (mut cpu, mut mem) = setup(&[0xE8, 0x03, 0x00, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0106);
    // Return address pushed
    let ret = u16::from_le_bytes([mem.ram[0x7FFE], mem.ram[0x7FFF]]);
    assert_eq!(ret, 0x0103);
}

// ========================================================================
// RET near (0xC3), RET imm16 (0xC2)
// ========================================================================

#[test]
fn test_ret_near() {
    // RET: C3
    let (mut cpu, mut mem) = setup(&[0xC3]);
    cpu.regs.esp = 0x7FFE;
    mem.ram[0x7FFE] = 0x00;
    mem.ram[0x7FFF] = 0x02; // return to 0x0200
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0200);
    assert_eq!(cpu.regs.sp(), 0x8000);
}

#[test]
fn test_ret_imm16() {
    // RET 4: C2 04 00
    let (mut cpu, mut mem) = setup(&[0xC2, 0x04, 0x00]);
    cpu.regs.esp = 0x7FFE;
    mem.ram[0x7FFE] = 0x00;
    mem.ram[0x7FFF] = 0x02;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0200);
    assert_eq!(cpu.regs.sp(), 0x8004); // SP += 2 (pop) + 4 (imm16)
}

// ========================================================================
// LEA 32-bit (0x66 0x8D)
// ========================================================================

#[test]
fn test_lea_r32_mem() {
    // LEA EAX, [0x200]: 66 8D 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x8D, 0x06, 0x00, 0x02]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x200);
}

// ========================================================================
// MOVZX/MOVSX 32→16 (0F B7, 0F BF)
// ========================================================================

#[test]
fn test_movzx_r32_rm16() {
    // MOVZX EAX, BX: 66 0F B7 C3
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xB7, 0xC3]);
    cpu.regs.eax = 0xFFFFFFFF;
    cpu.regs.set_bx(0x1234);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00001234);
}

#[test]
fn test_movsx_r32_rm16() {
    // MOVSX EAX, BX: 66 0F BF C3
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBF, 0xC3]);
    cpu.regs.eax = 0;
    cpu.regs.set_bx(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF8000);
}

// ========================================================================
// MOVZX/MOVSX memory operands
// ========================================================================

#[test]
fn test_movzx_r16_mem8() {
    // MOVZX AX, byte [0x200]: 0F B6 06 00 02
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB6, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0xFE;
    cpu.regs.set_ax(0xFFFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x00FE);
}

#[test]
fn test_movsx_r16_mem8() {
    // MOVSX AX, byte [0x200]: 0F BE 06 00 02
    let (mut cpu, mut mem) = setup(&[0x0F, 0xBE, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0x80;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFF80);
}

// ========================================================================
// BT/BTS/BTR/BTC 32-bit register, imm8 (0F BA)
// ========================================================================

#[test]
fn test_bt_eax_imm8_bit31() {
    // BT EAX, 31: 66 0F BA E0 1F
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBA, 0xE0, 0x1F]);
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_bts_eax_imm8_bit16() {
    // BTS EAX, 16: 66 0F BA E8 10
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBA, 0xE8, 0x10]);
    cpu.regs.eax = 0x00000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00010000);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was 0
}

#[test]
fn test_btr_eax_imm8_bit0() {
    // BTR EAX, 0: 66 0F BA F0 00
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBA, 0xF0, 0x00]);
    cpu.regs.eax = 0x00000001;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // was 1
}

#[test]
fn test_btc_eax_imm8_bit8() {
    // BTC EAX, 8: 66 0F BA F8 08
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBA, 0xF8, 0x08]);
    cpu.regs.eax = 0x00000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000100);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

// ========================================================================
// BT/BTS/BTR/BTC r32, r32 (0F A3, 0F AB, 0F B3, 0F BB)
// ========================================================================

#[test]
fn test_bt_r32_r32() {
    // BT EAX, ECX: 66 0F A3 C8
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xA3, 0xC8]);
    cpu.regs.eax = 0x00000010; // bit 4 set
    cpu.regs.ecx = 4;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_bts_r32_r32() {
    // BTS EAX, ECX: 66 0F AB C8
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xAB, 0xC8]);
    cpu.regs.eax = 0x00000000;
    cpu.regs.ecx = 20;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 1 << 20);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_btr_r32_r32() {
    // BTR EAX, ECX: 66 0F B3 C8
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xB3, 0xC8]);
    cpu.regs.eax = 0xFFFFFFFF;
    cpu.regs.ecx = 31;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x7FFFFFFF);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_btc_r32_r32() {
    // BTC EAX, ECX: 66 0F BB C8
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBB, 0xC8]);
    cpu.regs.eax = 0x00000001;
    cpu.regs.ecx = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// ========================================================================
// BSF/BSR 32-bit
// ========================================================================

#[test]
fn test_bsf_r32_r32() {
    // BSF EAX, EBX: 66 0F BC C3
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBC, 0xC3]);
    cpu.regs.eax = 0;
    cpu.regs.ebx = 0x00100000; // bit 20 is lowest set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_bsr_r32_r32() {
    // BSR EAX, EBX: 66 0F BD C3
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBD, 0xC3]);
    cpu.regs.eax = 0;
    cpu.regs.ebx = 0x00100000; // bit 20 is highest set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_bsf_r32_zero() {
    // BSF EAX, EBX with EBX=0: 66 0F BC C3
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBC, 0xC3]);
    cpu.regs.eax = 0x42;
    cpu.regs.ebx = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_bsr_r32_zero() {
    // BSR EAX, EBX with EBX=0: 66 0F BD C3
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xBD, 0xC3]);
    cpu.regs.eax = 0x42;
    cpu.regs.ebx = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// SHLD/SHRD 32-bit
// ========================================================================

#[test]
fn test_shld_r32_imm8() {
    // SHLD EAX, EBX, 8: 66 0F A4 D8 08
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xA4, 0xD8, 0x08]);
    cpu.regs.eax = 0x12340000;
    cpu.regs.ebx = 0xAB000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x340000AB);
}

#[test]
fn test_shrd_r32_imm8() {
    // SHRD EAX, EBX, 8: 66 0F AC D8 08
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xAC, 0xD8, 0x08]);
    cpu.regs.eax = 0x00001234;
    cpu.regs.ebx = 0x000000AB;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xAB000012);
}

#[test]
fn test_shld_r32_cl() {
    // SHLD EAX, EBX, CL: 66 0F A5 D8
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xA5, 0xD8]);
    cpu.regs.eax = 0xFF000000;
    cpu.regs.ebx = 0x12000000;
    cpu.regs.set_cl(4);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xF0000001);
}

#[test]
fn test_shrd_r32_cl() {
    // SHRD EAX, EBX, CL: 66 0F AD D8
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xAD, 0xD8]);
    cpu.regs.eax = 0x000000FF;
    cpu.regs.ebx = 0x00000012;
    cpu.regs.set_cl(4);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x2000000F);
}

// ========================================================================
// XADD (0F C0/C1) 32-bit
// ========================================================================

#[test]
fn test_xadd_r32_r32() {
    // XADD EAX, EBX: 66 0F C1 D8
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xC1, 0xD8]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 10;
    cpu.regs.ebx = 20;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 30); // dest = src + dest
    assert_eq!(cpu.regs.ebx, 10); // src = old dest
}

// ========================================================================
// CMPXCHG (0F B0/B1) memory operands
// ========================================================================

#[test]
fn test_cmpxchg_mem16_equal() {
    // CMPXCHG [0x200], BX: 0F B1 1E 00 02
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB1, 0x1E, 0x00, 0x02]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    mem.ram[0x200] = 0x34;
    mem.ram[0x201] = 0x12; // [0x200] = 0x1234
    cpu.regs.set_ax(0x1234); // AX matches
    cpu.regs.set_bx(0x5678); // replacement
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]);
    assert_eq!(val, 0x5678); // replaced
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpxchg_mem16_not_equal() {
    // CMPXCHG [0x200], BX: 0F B1 1E 00 02
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB1, 0x1E, 0x00, 0x02]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    mem.ram[0x200] = 0x78;
    mem.ram[0x201] = 0x56; // [0x200] = 0x5678
    cpu.regs.set_ax(0x1234); // AX doesn't match
    cpu.regs.set_bx(0x9999);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x5678); // AX loaded with [mem]
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// PUSHAD/POPAD (0x66 60/61)
// ========================================================================

#[test]
fn test_pushad_popad_roundtrip() {
    // PUSHAD (66 60), POPAD (66 61)
    let (mut cpu, mut mem) = setup(&[0x66, 0x60, 0x66, 0x61]);
    cpu.regs.eax = 0x11111111;
    cpu.regs.ecx = 0x22222222;
    cpu.regs.edx = 0x33333333;
    cpu.regs.ebx = 0x44444444;
    cpu.regs.esp = 0x8000;
    cpu.regs.ebp = 0x66666666;
    cpu.regs.esi = 0x77777777;
    cpu.regs.edi = 0x88888888;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0x11111111);
    assert_eq!(cpu.regs.ecx, 0x22222222);
    assert_eq!(cpu.regs.edx, 0x33333333);
    assert_eq!(cpu.regs.ebx, 0x44444444);
    assert_eq!(cpu.regs.esp, 0x8000);
    assert_eq!(cpu.regs.ebp, 0x66666666);
    assert_eq!(cpu.regs.esi, 0x77777777);
    assert_eq!(cpu.regs.edi, 0x88888888);
}

// ========================================================================
// PUSH/POP EAX (0x66 50/58)
// ========================================================================

#[test]
fn test_push_pop_eax() {
    // PUSH EAX (66 50), POP ECX (66 59)
    let (mut cpu, mut mem) = setup(&[0x66, 0x50, 0x66, 0x59]);
    cpu.regs.eax = 0xDEADBEEF;
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.ecx, 0xDEADBEEF);
    assert_eq!(cpu.regs.esp, 0x8000);
}

// ========================================================================
// PUSH imm32 (0x66 0x68)
// ========================================================================

#[test]
fn test_push_imm32() {
    // PUSH 0x12345678: 66 68 78 56 34 12
    let (mut cpu, mut mem) = setup(&[0x66, 0x68, 0x78, 0x56, 0x34, 0x12]);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // In 16-bit stack mode, PUSH with 32-bit operand pushes 4 bytes,
    // SP decrements by 4 (truncated to 16 bits)
    assert_eq!(cpu.regs.sp(), 0x7FFC);
    let val = u32::from_le_bytes([
        mem.ram[0x7FFC],
        mem.ram[0x7FFD],
        mem.ram[0x7FFE],
        mem.ram[0x7FFF],
    ]);
    assert_eq!(val, 0x12345678);
}

// ========================================================================
// Segment override: FS/GS
// ========================================================================

#[test]
fn test_fs_override_mov() {
    // FS: MOV AX, [0x100] = 64 A1 00 01
    let (mut cpu, mut mem) = setup(&[0x64, 0xA1, 0x00, 0x01]);
    cpu.regs.fs = 0x0020; // FS base = 0x200
    mem.ram[0x300] = 0xEF; // 0x200 + 0x100 = 0x300
    mem.ram[0x301] = 0xBE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xBEEF);
}

#[test]
fn test_gs_override_mov() {
    // GS: MOV AX, [0x100] = 65 A1 00 01
    let (mut cpu, mut mem) = setup(&[0x65, 0xA1, 0x00, 0x01]);
    cpu.regs.gs = 0x0030; // GS base = 0x300
    mem.ram[0x400] = 0xAD; // 0x300 + 0x100 = 0x400
    mem.ram[0x401] = 0xDE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xDEAD);
}

// ========================================================================
// CS override (0x2E)
// ========================================================================

#[test]
fn test_cs_override_mov() {
    // CS: MOV AX, [0x300] = 2E A1 00 03
    let (mut cpu, mut mem) = setup(&[0x2E, 0xA1, 0x00, 0x03]);
    // CS=0x0000, so CS:0x300 = 0x300
    mem.ram[0x300] = 0x42;
    mem.ram[0x301] = 0x00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0042);
}

// ========================================================================
// SS override (0x36)
// ========================================================================

#[test]
fn test_ss_override_mov() {
    // SS: MOV AX, [0x100] = 36 A1 00 01
    let (mut cpu, mut mem) = setup(&[0x36, 0xA1, 0x00, 0x01]);
    cpu.regs.ss = 0x0010; // SS base = 0x100
    mem.ram[0x200] = 0x34; // 0x100 + 0x100 = 0x200
    mem.ram[0x201] = 0x12;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1234);
}

// ========================================================================
// LOCK prefix
// ========================================================================

#[test]
fn test_lock_xchg() {
    // LOCK XCHG [0x200], AX: F0 87 06 00 02
    let (mut cpu, mut mem) = setup(&[0xF0, 0x87, 0x06, 0x00, 0x02]);
    cpu.regs.set_ax(0x1234);
    mem.ram[0x200] = 0x78;
    mem.ram[0x201] = 0x56;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x5678);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x1234);
}

// ========================================================================
// CMC (F5)
// ========================================================================

#[test]
fn test_cmc_set_to_clear() {
    // CMC: F5
    let (mut cpu, mut mem) = setup(&[0xF5]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_cmc_clear_to_set() {
    let (mut cpu, mut mem) = setup(&[0xF5]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// ========================================================================
// PUSHF/POPF 32-bit (66 9C/9D)
// ========================================================================

#[test]
fn test_pushfd_popfd_roundtrip() {
    // PUSHFD (66 9C), POPFD (66 9D)
    let (mut cpu, mut mem) = setup(&[0x66, 0x9C, 0x66, 0x9D]);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.flags.insert(Flags::ZERO);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// Lazy flags: verify conditions after ALU ops
// ========================================================================

#[test]
fn test_lazy_add_then_jz() {
    // ADD AL, 0 (04 00); JZ +2 (74 02)
    let (mut cpu, mut mem) = setup(&[0x04, 0x00, 0x74, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.set_al(0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eip, 0x0106); // should jump (ZF=1)
}

#[test]
fn test_lazy_sub_then_jb() {
    // SUB AL, 5 (2C 05); JB +2 (72 02)
    let (mut cpu, mut mem) = setup(&[0x2C, 0x05, 0x72, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.set_al(3);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eip, 0x0106); // should jump (CF=1, 3 < 5)
}

#[test]
fn test_lazy_and_then_js() {
    // AND AL, 0x80 (24 80); JS +2 (78 02)
    let (mut cpu, mut mem) = setup(&[0x24, 0x80, 0x78, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.set_al(0xFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eip, 0x0106); // should jump (SF=1)
}

#[test]
fn test_lazy_xor_clears_cf() {
    // STC (F9); XOR AL, AL (30 C0); JB +2 (72 02)
    let (mut cpu, mut mem) = setup(&[0xF9, 0x30, 0xC0, 0x72, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.set_al(0xFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // XOR clears CF, so JB should NOT be taken
    assert_eq!(cpu.regs.eip, 0x0105);
}

#[test]
fn test_lazy_inc_preserves_cf() {
    // STC (F9); INC AX (40); JB +2 (72 02)
    let (mut cpu, mut mem) = setup(&[0xF9, 0x40, 0x72, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.set_ax(0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // INC preserves CF; CF was set by STC, so JB should be taken
    assert_eq!(cpu.regs.eip, 0x0106);
}

#[test]
fn test_lazy_dec_preserves_cf() {
    // STC (F9); DEC AX (48); JB +2 (72 02)
    let (mut cpu, mut mem) = setup(&[0xF9, 0x48, 0x72, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.set_ax(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // DEC preserves CF; CF was set by STC, so JB should be taken
    assert_eq!(cpu.regs.eip, 0x0106);
}

// ========================================================================
// Lazy flags: 16/32-bit variants
// ========================================================================

#[test]
fn test_lazy_add16_then_jo() {
    // ADD AX, 0x7FFF (05 FF 7F); JO +2 (70 02)
    let (mut cpu, mut mem) = setup(&[0x05, 0xFF, 0x7F, 0x70, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.set_ax(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    // 1 + 0x7FFF = 0x8000, which is overflow for signed 16-bit
    assert_eq!(cpu.regs.eip, 0x0107);
}

#[test]
fn test_lazy_sub16_then_jp() {
    // SUB AL, 0 (2C 00); JP +2 (7A 02)
    let (mut cpu, mut mem) = setup(&[0x2C, 0x00, 0x7A, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.set_al(0); // 0 has even parity
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eip, 0x0106);
}

// ========================================================================
// DAA/DAS/AAA/AAS (0x27/0x2F/0x37/0x3F)
// ========================================================================

#[test]
fn test_daa() {
    // DAA: 27
    let (mut cpu, mut mem) = setup(&[0x27]);
    cpu.regs.set_al(0x0A); // After adding, needs decimal adjust
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x10);
}

#[test]
fn test_das() {
    // DAS: 2F
    let (mut cpu, mut mem) = setup(&[0x2F]);
    cpu.regs.set_al(0x10);
    cpu.regs.flags.insert(Flags::AUX_CARRY);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // DAS adjusts AL after subtraction
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_aaa() {
    // AAA: 37
    let (mut cpu, mut mem) = setup(&[0x37]);
    cpu.regs.set_al(0x0A);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0101);
}

#[test]
fn test_aas() {
    // AAS: 3F
    let (mut cpu, mut mem) = setup(&[0x3F]);
    cpu.regs.set_al(0x0A);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ========================================================================
// Multi-instruction sequences
// ========================================================================

#[test]
fn test_budget_exhausted_multi() {
    // NOP NOP NOP with budget=2 → only 2 execute
    let (mut cpu, mut mem) = setup(&[0x90, 0x90, 0x90]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 2 }));
    assert_eq!(cpu.regs.eip, 0x0102);
}

// ========================================================================
// MOV segment register (0x8E)
// ========================================================================

#[test]
fn test_mov_es_ax() {
    // MOV ES, AX: 8E C0
    let (mut cpu, mut mem) = setup(&[0x8E, 0xC0]);
    cpu.regs.set_ax(0x0020);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.es, 0x0020);
}

#[test]
fn test_mov_ds_bx() {
    // MOV DS, BX: 8E DB
    let (mut cpu, mut mem) = setup(&[0x8E, 0xDB]);
    cpu.regs.set_bx(0x0040);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ds, 0x0040);
}

// ========================================================================
// MOV from segment register (0x8C)
// ========================================================================

#[test]
fn test_mov_ax_es() {
    // MOV AX, ES: 8C C0
    let (mut cpu, mut mem) = setup(&[0x8C, 0xC0]);
    cpu.regs.es = 0x1234;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1234);
}

#[test]
fn test_mov_bx_cs() {
    // MOV BX, CS: 8C CB
    let (mut cpu, mut mem) = setup(&[0x8C, 0xCB]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.bx(), cpu.regs.cs);
}

#[test]
fn test_mov_cx_ss() {
    // MOV CX, SS: 8C D1
    let (mut cpu, mut mem) = setup(&[0x8C, 0xD1]);
    cpu.regs.ss = 0x5678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cx(), 0x5678);
}

// ========================================================================
// MOV r/m8, imm8 (0xC6)
// ========================================================================

#[test]
fn test_mov_rm8_imm8_reg() {
    // MOV AL, 0x42: C6 C0 42
    let (mut cpu, mut mem) = setup(&[0xC6, 0xC0, 0x42]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x42);
}

#[test]
fn test_mov_rm8_imm8_mem() {
    // MOV byte [0x200], 0xAB: C6 06 00 02 AB
    let (mut cpu, mut mem) = setup(&[0xC6, 0x06, 0x00, 0x02, 0xAB]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0xAB);
}

// ========================================================================
// MOV r/m16, imm16 (0xC7)
// ========================================================================

#[test]
fn test_mov_rm16_imm16_reg() {
    // MOV AX, 0x1234: C7 C0 34 12
    let (mut cpu, mut mem) = setup(&[0xC7, 0xC0, 0x34, 0x12]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x1234);
}

#[test]
fn test_mov_rm16_imm16_mem() {
    // MOV word [0x200], 0xBEEF: C7 06 00 02 EF BE
    let (mut cpu, mut mem) = setup(&[0xC7, 0x06, 0x00, 0x02, 0xEF, 0xBE]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0xBEEF);
}

// ========================================================================
// Group 2: ROL/ROR/RCL/RCR memory by imm8 (C0/C1)
// ========================================================================

#[test]
fn test_rol_mem8_imm8() {
    // ROL byte [0x200], 4: C0 06 00 02 04
    let (mut cpu, mut mem) = setup(&[0xC0, 0x06, 0x00, 0x02, 0x04]);
    mem.ram[0x200] = 0xF0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x0F);
}

#[test]
fn test_ror_mem8_imm8() {
    // ROR byte [0x200], 4: C0 0E 00 02 04
    let (mut cpu, mut mem) = setup(&[0xC0, 0x0E, 0x00, 0x02, 0x04]);
    mem.ram[0x200] = 0x0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0xF0);
}

// ========================================================================
// ALU r/m16 with memory destination (0x01/0x09/0x21/0x29/0x31)
// ========================================================================

#[test]
fn test_add_mem16_reg16() {
    // ADD [0x200], AX: 01 06 00 02
    let (mut cpu, mut mem) = setup(&[0x01, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01; // 0x0100
    cpu.regs.set_ax(0x0050);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0150);
}

#[test]
fn test_sub_mem16_reg16() {
    // SUB [0x200], AX: 29 06 00 02
    let (mut cpu, mut mem) = setup(&[0x29, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x02; // 0x0200
    cpu.regs.set_ax(0x0100);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0100);
}

#[test]
fn test_and_mem16_reg16() {
    // AND [0x200], AX: 21 06 00 02
    let (mut cpu, mut mem) = setup(&[0x21, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0xFF;
    mem.ram[0x201] = 0xFF;
    cpu.regs.set_ax(0x0F0F);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0F0F);
}

#[test]
fn test_or_mem16_reg16() {
    // OR [0x200], AX: 09 06 00 02
    let (mut cpu, mut mem) = setup(&[0x09, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0x0F;
    mem.ram[0x201] = 0x00;
    cpu.regs.set_ax(0xF000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0xF00F);
}

#[test]
fn test_xor_mem16_reg16() {
    // XOR [0x200], AX: 31 06 00 02
    let (mut cpu, mut mem) = setup(&[0x31, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0xFF;
    mem.ram[0x201] = 0xFF;
    cpu.regs.set_ax(0xFFFF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0000);
}

// ========================================================================
// ALU r/m8 with memory destination (0x00/0x08/0x20/0x28/0x30)
// ========================================================================

#[test]
fn test_add_mem8_reg8() {
    // ADD [0x200], AL: 00 06 00 02
    let (mut cpu, mut mem) = setup(&[0x00, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 10;
    cpu.regs.set_al(5);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 15);
}

#[test]
fn test_sub_mem8_reg8() {
    // SUB [0x200], AL: 28 06 00 02
    let (mut cpu, mut mem) = setup(&[0x28, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 10;
    cpu.regs.set_al(3);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 7);
}

// ========================================================================
// CMP r/m16, r16 (0x39) and CMP r16, r/m16 (0x3B)
// ========================================================================

#[test]
fn test_cmp_rm16_r16() {
    // CMP AX, BX: 39 D8
    let (mut cpu, mut mem) = setup(&[0x39, 0xD8]);
    cpu.regs.set_ax(0x100);
    cpu.regs.set_bx(0x100);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmp_r16_rm16() {
    // CMP BX, [0x200]: 3B 1E 00 02
    let (mut cpu, mut mem) = setup(&[0x3B, 0x1E, 0x00, 0x02]);
    cpu.regs.set_bx(0x200);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01; // 0x100
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // 0x200 > 0x100
}

// ========================================================================
// Group 3: TEST r/m8, imm8 (0xF6 /0) and TEST r/m16, imm16 (0xF7 /0)
// ========================================================================

#[test]
fn test_test_rm8_imm8_grp3() {
    // TEST AL, 0x01: F6 C0 01
    let (mut cpu, mut mem) = setup(&[0xF6, 0xC0, 0x01]);
    cpu.regs.set_al(0xFE);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_test_rm16_imm16_grp3() {
    // TEST AX, 0x8000: F7 C0 00 80
    let (mut cpu, mut mem) = setup(&[0xF7, 0xC0, 0x00, 0x80]);
    cpu.regs.set_ax(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// MUL/DIV memory operands
// ========================================================================

#[test]
fn test_mul_mem8() {
    // MUL byte [0x200]: F6 26 00 02
    let (mut cpu, mut mem) = setup(&[0xF6, 0x26, 0x00, 0x02]);
    mem.ram[0x200] = 10;
    cpu.regs.set_al(20);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 200);
}

#[test]
fn test_div_mem8() {
    // DIV byte [0x200]: F6 36 00 02
    let (mut cpu, mut mem) = setup(&[0xF6, 0x36, 0x00, 0x02]);
    mem.ram[0x200] = 3;
    cpu.regs.set_ax(10); // AX / [0x200]
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 3); // quotient
    assert_eq!(cpu.regs.ah(), 1); // remainder
}

// ========================================================================
// NOT/NEG memory operands
// ========================================================================

#[test]
fn test_not_mem16() {
    // NOT word [0x200]: F7 16 00 02
    let (mut cpu, mut mem) = setup(&[0xF7, 0x16, 0x00, 0x02]);
    mem.ram[0x200] = 0xFF;
    mem.ram[0x201] = 0x00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0xFF00);
}

#[test]
fn test_neg_mem16() {
    // NEG word [0x200]: F7 1E 00 02
    let (mut cpu, mut mem) = setup(&[0xF7, 0x1E, 0x00, 0x02]);
    mem.ram[0x200] = 0x05;
    mem.ram[0x201] = 0x00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]) as i16;
    assert_eq!(val, -5);
}

// ========================================================================
// IMUL r16, r/m16, imm (0x69/0x6B)
// ========================================================================

#[test]
fn test_imul_r16_rm16_imm16() {
    // IMUL AX, BX, 10: 69 C3 0A 00
    let (mut cpu, mut mem) = setup(&[0x69, 0xC3, 0x0A, 0x00]);
    cpu.regs.set_bx(5);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 50);
}

#[test]
fn test_imul_r16_rm16_imm8() {
    // IMUL AX, BX, 3: 6B C3 03
    let (mut cpu, mut mem) = setup(&[0x6B, 0xC3, 0x03]);
    cpu.regs.set_bx(100);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 300);
}

// ========================================================================
// IMUL r32, r/m32, imm (66 69/6B)
// ========================================================================

#[test]
fn test_imul_r32_rm32_imm32() {
    // IMUL EAX, EBX, 256: 66 69 C3 00 01 00 00
    let (mut cpu, mut mem) = setup(&[0x66, 0x69, 0xC3, 0x00, 0x01, 0x00, 0x00]);
    cpu.regs.ebx = 1000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 256000);
}

#[test]
fn test_imul_r32_rm32_imm8() {
    // IMUL EAX, EBX, 7: 66 6B C3 07
    let (mut cpu, mut mem) = setup(&[0x66, 0x6B, 0xC3, 0x07]);
    cpu.regs.ebx = 100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 700);
}

// ========================================================================
// IMUL r32, r/m32 two-operand (66 0F AF)
// ========================================================================

#[test]
fn test_imul_r32_rm32_two_operand() {
    // IMUL EAX, EBX: 66 0F AF C3
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xAF, 0xC3]);
    cpu.regs.eax = 100;
    cpu.regs.ebx = 200;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20000);
}

// ========================================================================
// RunResult::executed() accessor for all variants
// ========================================================================

#[test]
fn test_runresult_executed_budget() {
    let r = RunResult::BudgetExhausted { executed: 42 };
    assert_eq!(r.executed(), 42);
}

#[test]
fn test_runresult_executed_halt() {
    let r = RunResult::Halt { executed: 7 };
    assert_eq!(r.executed(), 7);
}

#[test]
fn test_runresult_executed_interrupt() {
    let r = RunResult::SoftwareInterrupt {
        vector: 0x21,
        executed: 10,
    };
    assert_eq!(r.executed(), 10);
}

#[test]
fn test_runresult_executed_exception() {
    let r = RunResult::Exception {
        vector: 13,
        error_code: 0,
        fault_addr: None,
        executed: 5,
    };
    assert_eq!(r.executed(), 5);
}

#[test]
fn test_runresult_executed_hle() {
    let r = RunResult::HleEntry { executed: 3 };
    assert_eq!(r.executed(), 3);
}

#[test]
fn test_runresult_executed_io() {
    let r = RunResult::IoCompleted { executed: 8 };
    assert_eq!(r.executed(), 8);
}

#[test]
fn test_runresult_executed_iret() {
    let r = RunResult::Iret { executed: 1 };
    assert_eq!(r.executed(), 1);
}

#[test]
fn test_runresult_executed_retf() {
    let r = RunResult::Retf { executed: 2 };
    assert_eq!(r.executed(), 2);
}

#[test]
fn test_runresult_executed_mode_change() {
    let r = RunResult::ModeChange { executed: 4 };
    assert_eq!(r.executed(), 4);
}

#[test]
fn test_runresult_executed_invlpg() {
    let r = RunResult::Invlpg {
        linear_addr: 0x1000,
        executed: 6,
    };
    assert_eq!(r.executed(), 6);
}

#[test]
fn test_runresult_executed_task_switch() {
    let r = RunResult::TaskSwitch { executed: 9 };
    assert_eq!(r.executed(), 9);
}

#[test]
fn test_runresult_executed_task_switch_jmp() {
    let r = RunResult::TaskSwitchJmp {
        selector: 0x10,
        executed: 11,
    };
    assert_eq!(r.executed(), 11);
}

#[test]
fn test_runresult_executed_task_switch_call() {
    let r = RunResult::TaskSwitchCall {
        selector: 0x18,
        executed: 12,
    };
    assert_eq!(r.executed(), 12);
}

#[test]
fn test_runresult_executed_call_gate() {
    let r = RunResult::CallGate {
        selector: 0x20,
        return_cs: 0x08,
        return_eip: 0x1000,
        executed: 13,
    };
    assert_eq!(r.executed(), 13);
}

#[test]
fn test_runresult_executed_script_sentinel_after_batch() {
    let r = RunResult::ScriptSentinel { executed: 14 };
    assert_eq!(r.executed(), 14);
}

#[test]
fn test_runresult_executed_script_sentinel() {
    let r = RunResult::ScriptSentinel { executed: 15 };
    assert_eq!(r.executed(), 15);
}

// ========================================================================
// REP SCASW (F3 AF)
// ========================================================================

#[test]
fn test_rep_scasw_find() {
    // REPE SCASW: F3 AF
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAF]);
    // Words at ES:DI
    mem.ram[0x400] = 0x34;
    mem.ram[0x401] = 0x12; // 0x1234
    mem.ram[0x402] = 0x34;
    mem.ram[0x403] = 0x12; // 0x1234
    mem.ram[0x404] = 0x78;
    mem.ram[0x405] = 0x56; // 0x5678
    cpu.regs.set_ax(0x1234);
    cpu.regs.edi = 0x400;
    cpu.regs.set_cx(3);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 10);
    // REPE stops when mismatch found or CX=0
    assert_eq!(cpu.regs.cx(), 0); // Ran through to mismatch at 3rd word then stopped
}

// ========================================================================
// Shift by count of 0 (should be no-op for flags)
// ========================================================================

#[test]
fn test_shl_al_cl_zero() {
    // SHL AL, CL with CL=0: D2 E0
    let (mut cpu, mut mem) = setup(&[0xD2, 0xE0]);
    cpu.regs.set_al(0x42);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.set_cl(0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x42); // unchanged
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // flags unchanged
}

// ========================================================================
// REP INS/OUTS (0x6C-0x6F with F3)
// ========================================================================

#[test]
fn test_insb() {
    // INSB: 6C
    let (mut cpu, mut mem) = setup(&[0x6C]);
    cpu.regs.edi = 0x400;
    cpu.regs.set_dx(0x80);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::IoCompleted { .. })
            || matches!(result, RunResult::BudgetExhausted { .. })
    );
}

#[test]
fn test_outsb() {
    // OUTSB: 6E
    let (mut cpu, mut mem) = setup(&[0x6E]);
    cpu.regs.esi = 0x300;
    cpu.regs.set_dx(0x80);
    mem.ram[0x300] = 0x42;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::IoCompleted { .. })
            || matches!(result, RunResult::BudgetExhausted { .. })
    );
}
