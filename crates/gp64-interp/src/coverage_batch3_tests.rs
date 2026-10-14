use super::test_helpers::setup;
use super::*;
use gp64_cpu::Flags;

// ========================================================================
// ADC/SBB with memory operands (0x10-0x13, 0x18-0x1B)
// ========================================================================

#[test]
fn test_adc_mem8_r8() {
    // STC, ADC [0x200], AL: F9 10 06 00 02
    let (mut cpu, mut mem) = setup(&[0xF9, 0x10, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 10;
    cpu.regs.set_al(5);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(mem.ram[0x200], 16); // 10 + 5 + 1(CF)
}

#[test]
fn test_sbb_mem8_r8() {
    // STC, SBB [0x200], AL: F9 18 06 00 02
    let (mut cpu, mut mem) = setup(&[0xF9, 0x18, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 10;
    cpu.regs.set_al(3);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(mem.ram[0x200], 6); // 10 - 3 - 1(CF)
}

#[test]
fn test_adc_r8_mem8() {
    // STC, ADC AL, [0x200]: F9 12 06 00 02
    let (mut cpu, mut mem) = setup(&[0xF9, 0x12, 0x06, 0x00, 0x02]);
    cpu.regs.set_al(5);
    mem.ram[0x200] = 10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.al(), 16); // 5 + 10 + 1(CF)
}

#[test]
fn test_sbb_r8_mem8() {
    // STC, SBB AL, [0x200]: F9 1A 06 00 02
    let (mut cpu, mut mem) = setup(&[0xF9, 0x1A, 0x06, 0x00, 0x02]);
    cpu.regs.set_al(20);
    mem.ram[0x200] = 5;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.al(), 14); // 20 - 5 - 1(CF)
}

#[test]
fn test_adc_mem16_r16() {
    // STC, ADC [0x200], AX: F9 11 06 00 02
    let (mut cpu, mut mem) = setup(&[0xF9, 0x11, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01; // 0x0100
    cpu.regs.set_ax(0x0050);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0151);
    // 0x0100 + 0x0050 + 1
}

#[test]
fn test_sbb_mem16_r16() {
    // STC, SBB [0x200], AX: F9 19 06 00 02
    let (mut cpu, mut mem) = setup(&[0xF9, 0x19, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x02; // 0x0200
    cpu.regs.set_ax(0x0100);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x00FF);
    // 0x0200 - 0x0100 - 1
}

#[test]
fn test_adc_r16_mem16() {
    // STC, ADC AX, [0x200]: F9 13 06 00 02
    let (mut cpu, mut mem) = setup(&[0xF9, 0x13, 0x06, 0x00, 0x02]);
    cpu.regs.set_ax(0x0050);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.ax(), 0x0151);
}

#[test]
fn test_sbb_r16_mem16() {
    // STC, SBB AX, [0x200]: F9 1B 06 00 02
    let (mut cpu, mut mem) = setup(&[0xF9, 0x1B, 0x06, 0x00, 0x02]);
    cpu.regs.set_ax(0x0200);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.ax(), 0x00FF); // 0x200 - 0x100 - 1
}

// ========================================================================
// ADC/SBB Group 1 with memory (0x80-0x83 /2, /3)
// ========================================================================

#[test]
fn test_adc_rm8_imm8_mem() {
    // STC, ADC byte [0x200], 5: F9 80 16 00 02 05
    let (mut cpu, mut mem) = setup(&[0xF9, 0x80, 0x16, 0x00, 0x02, 0x05]);
    mem.ram[0x200] = 10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(mem.ram[0x200], 16); // 10 + 5 + 1
}

#[test]
fn test_sbb_rm8_imm8_mem() {
    // STC, SBB byte [0x200], 3: F9 80 1E 00 02 03
    let (mut cpu, mut mem) = setup(&[0xF9, 0x80, 0x1E, 0x00, 0x02, 0x03]);
    mem.ram[0x200] = 10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(mem.ram[0x200], 6); // 10 - 3 - 1
}

#[test]
fn test_adc_rm16_imm16_mem() {
    // STC, ADC word [0x200], 100: F9 81 16 00 02 64 00
    let (mut cpu, mut mem) = setup(&[0xF9, 0x81, 0x16, 0x00, 0x02, 0x64, 0x00]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01; // 0x100
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0165);
    // 0x100 + 100 + 1
}

#[test]
fn test_sbb_rm16_imm8_mem() {
    // STC, SBB word [0x200], 5: F9 83 1E 00 02 05
    let (mut cpu, mut mem) = setup(&[0xF9, 0x83, 0x1E, 0x00, 0x02, 0x05]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01; // 0x100
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x00FA);
    // 0x100 - 5 - 1
}

// ========================================================================
// MOV r/m32, imm32 (0x66 0xC7) — register and memory forms
// ========================================================================

#[test]
fn test_mov_rm32_imm32_reg() {
    // MOV EAX, 0xDEADBEEF: 66 C7 C0 EF BE AD DE
    let (mut cpu, mut mem) = setup(&[0x66, 0xC7, 0xC0, 0xEF, 0xBE, 0xAD, 0xDE]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xDEADBEEF);
}

#[test]
fn test_mov_rm32_imm32_mem() {
    // MOV dword [0x200], 0x12345678: 66 C7 06 00 02 78 56 34 12
    let (mut cpu, mut mem) = setup(&[0x66, 0xC7, 0x06, 0x00, 0x02, 0x78, 0x56, 0x34, 0x12]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x12345678);
}

// ========================================================================
// 32-bit ALU with memory operands
// ========================================================================

#[test]
fn test_add_mem32_r32() {
    // ADD [0x200], EAX: 66 01 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x01, 0x06, 0x00, 0x02]);
    let bytes = 0x00000100u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.eax = 0x00000050;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x00000150);
}

#[test]
fn test_sub_mem32_r32() {
    // SUB [0x200], EAX: 66 29 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x29, 0x06, 0x00, 0x02]);
    let bytes = 0x00000200u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.eax = 0x00000100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x00000100);
}

#[test]
fn test_and_mem32_r32() {
    // AND [0x200], EAX: 66 21 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x21, 0x06, 0x00, 0x02]);
    let bytes = 0xFFFF0000u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.eax = 0xFF00FF00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0xFF000000);
}

#[test]
fn test_or_mem32_r32() {
    // OR [0x200], EAX: 66 09 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x09, 0x06, 0x00, 0x02]);
    let bytes = 0x0000FF00u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.eax = 0x00FF0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x00FFFF00);
}

#[test]
fn test_xor_mem32_r32() {
    // XOR [0x200], EAX: 66 31 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x31, 0x06, 0x00, 0x02]);
    let bytes = 0xFFFFFFFFu32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.eax = 0xFFFF0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x0000FFFF);
}

#[test]
fn test_cmp_mem32_r32() {
    // CMP [0x200], EAX: 66 39 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x39, 0x06, 0x00, 0x02]);
    let bytes = 0x00000100u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.eax = 0x00000100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 32-bit ALU: reg <- mem
// ========================================================================

#[test]
fn test_add_r32_mem32() {
    // ADD EAX, [0x200]: 66 03 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x03, 0x06, 0x00, 0x02]);
    let bytes = 0x00000100u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.eax = 0x00000050;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000150);
}

#[test]
fn test_sub_r32_mem32() {
    // SUB EAX, [0x200]: 66 2B 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x2B, 0x06, 0x00, 0x02]);
    let bytes = 0x00000100u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.eax = 0x00000200;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000100);
}

// ========================================================================
// Group 1: 32-bit ALU with memory (0x66 0x81)
// ========================================================================

#[test]
fn test_add_rm32_imm32_mem() {
    // ADD dword [0x200], 0x100: 66 81 06 00 02 00 01 00 00
    let (mut cpu, mut mem) = setup(&[0x66, 0x81, 0x06, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00]);
    let bytes = 0x00000050u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x00000150);
}

#[test]
fn test_sub_rm32_imm8_mem() {
    // SUB dword [0x200], 5: 66 83 2E 00 02 05
    let (mut cpu, mut mem) = setup(&[0x66, 0x83, 0x2E, 0x00, 0x02, 0x05]);
    let bytes = 0x00000100u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x000000FB);
}

// ========================================================================
// Group 1: ALU memory with 8-bit (0x80 with memory dest)
// ========================================================================

#[test]
fn test_add_rm8_imm8_mem_dest() {
    // ADD byte [0x200], 0x10: 80 06 00 02 10
    let (mut cpu, mut mem) = setup(&[0x80, 0x06, 0x00, 0x02, 0x10]);
    mem.ram[0x200] = 0x20;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x30);
}

#[test]
fn test_or_rm8_imm8_mem_dest() {
    // OR byte [0x200], 0xF0: 80 0E 00 02 F0
    let (mut cpu, mut mem) = setup(&[0x80, 0x0E, 0x00, 0x02, 0xF0]);
    mem.ram[0x200] = 0x0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0xFF);
}

#[test]
fn test_and_rm8_imm8_mem_dest() {
    // AND byte [0x200], 0x0F: 80 26 00 02 0F
    let (mut cpu, mut mem) = setup(&[0x80, 0x26, 0x00, 0x02, 0x0F]);
    mem.ram[0x200] = 0xFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x0F);
}

#[test]
fn test_sub_rm8_imm8_mem_dest() {
    // SUB byte [0x200], 3: 80 2E 00 02 03
    let (mut cpu, mut mem) = setup(&[0x80, 0x2E, 0x00, 0x02, 0x03]);
    mem.ram[0x200] = 10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 7);
}

#[test]
fn test_xor_rm8_imm8_mem_dest() {
    // XOR byte [0x200], 0xFF: 80 36 00 02 FF
    let (mut cpu, mut mem) = setup(&[0x80, 0x36, 0x00, 0x02, 0xFF]);
    mem.ram[0x200] = 0xAA;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x55);
}

#[test]
fn test_cmp_rm8_imm8_mem_dest() {
    // CMP byte [0x200], 42: 80 3E 00 02 2A
    let (mut cpu, mut mem) = setup(&[0x80, 0x3E, 0x00, 0x02, 0x2A]);
    mem.ram[0x200] = 42;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// Group 1: 0x81 with memory dest (16-bit)
// ========================================================================

#[test]
fn test_add_rm16_imm16_mem_dest() {
    // ADD word [0x200], 0x100: 81 06 00 02 00 01
    let (mut cpu, mut mem) = setup(&[0x81, 0x06, 0x00, 0x02, 0x00, 0x01]);
    mem.ram[0x200] = 0x50;
    mem.ram[0x201] = 0x00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0150);
}

#[test]
fn test_sub_rm16_imm16_mem_dest() {
    // SUB word [0x200], 0x100: 81 2E 00 02 00 01
    let (mut cpu, mut mem) = setup(&[0x81, 0x2E, 0x00, 0x02, 0x00, 0x01]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x02; // 0x0200
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0100);
}

// ========================================================================
// Group 1: 0x83 with memory dest (16-bit, sign-extended imm8)
// ========================================================================

#[test]
fn test_add_rm16_imm8_mem_dest() {
    // ADD word [0x200], 5: 83 06 00 02 05
    let (mut cpu, mut mem) = setup(&[0x83, 0x06, 0x00, 0x02, 0x05]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01; // 0x0100
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0105);
}

#[test]
fn test_and_rm16_imm8_mem_dest() {
    // AND word [0x200], 0x0F: 83 26 00 02 0F
    let (mut cpu, mut mem) = setup(&[0x83, 0x26, 0x00, 0x02, 0x0F]);
    mem.ram[0x200] = 0xFF;
    mem.ram[0x201] = 0xFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x000F);
}

#[test]
fn test_or_rm16_imm8_mem_dest() {
    // OR word [0x200], 0x0F: 83 0E 00 02 0F
    let (mut cpu, mut mem) = setup(&[0x83, 0x0E, 0x00, 0x02, 0x0F]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x010F);
}

#[test]
fn test_xor_rm16_imm8_mem_dest() {
    // XOR word [0x200], -1 (FF → 0xFFFF sign-extended): 83 36 00 02 FF
    let (mut cpu, mut mem) = setup(&[0x83, 0x36, 0x00, 0x02, 0xFF]);
    mem.ram[0x200] = 0xAA;
    mem.ram[0x201] = 0x55;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0xAA55);
    // 0x55AA ^ 0xFFFF = 0xAA55
}

#[test]
fn test_cmp_rm16_imm8_mem_dest() {
    // CMP word [0x200], 0: 83 3E 00 02 00
    let (mut cpu, mut mem) = setup(&[0x83, 0x3E, 0x00, 0x02, 0x00]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 32-bit INC/DEC memory (FF /0, /1 with 0x66)
// ========================================================================

#[test]
fn test_inc_rm32_mem() {
    // INC dword [0x200]: 66 FF 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0xFF, 0x06, 0x00, 0x02]);
    let bytes = 0x000000FFu32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x00000100);
}

#[test]
fn test_dec_rm32_mem() {
    // DEC dword [0x200]: 66 FF 0E 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0xFF, 0x0E, 0x00, 0x02]);
    let bytes = 0x00000100u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x000000FF);
}

// ========================================================================
// INC/DEC 16-bit memory (0xFF /0, /1)
// ========================================================================

#[test]
fn test_inc_rm16_mem() {
    // INC word [0x200]: FF 06 00 02
    let (mut cpu, mut mem) = setup(&[0xFF, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0xFF;
    mem.ram[0x201] = 0x00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0100);
}

#[test]
fn test_dec_rm16_mem() {
    // DEC word [0x200]: FF 0E 00 02
    let (mut cpu, mut mem) = setup(&[0xFF, 0x0E, 0x00, 0x02]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01; // 0x100
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x00FF);
}

// ========================================================================
// TEST r/m with memory (0x84, 0x85 with memory operands)
// ========================================================================

#[test]
fn test_test_rm8_r8_mem() {
    // TEST byte [0x200], AL: 84 06 00 02
    let (mut cpu, mut mem) = setup(&[0x84, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0xF0;
    cpu.regs.set_al(0x0F);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_test_rm16_r16_mem() {
    // TEST word [0x200], AX: 85 06 00 02
    let (mut cpu, mut mem) = setup(&[0x85, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0xFF;
    cpu.regs.set_ax(0x00FF);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// XCHG r, mem (0x86/0x87 with memory forms)
// ========================================================================

#[test]
fn test_xchg_r8_mem8() {
    // XCHG AL, [0x200]: 86 06 00 02
    let (mut cpu, mut mem) = setup(&[0x86, 0x06, 0x00, 0x02]);
    cpu.regs.set_al(0x11);
    mem.ram[0x200] = 0x22;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x22);
    assert_eq!(mem.ram[0x200], 0x11);
}

#[test]
fn test_xchg_r16_mem16() {
    // XCHG AX, [0x200]: 87 06 00 02
    let (mut cpu, mut mem) = setup(&[0x87, 0x06, 0x00, 0x02]);
    cpu.regs.set_ax(0x1234);
    mem.ram[0x200] = 0x78;
    mem.ram[0x201] = 0x56;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x5678);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x1234);
}

// ========================================================================
// 32-bit XCHG with memory
// ========================================================================

#[test]
fn test_xchg_r32_mem32() {
    // XCHG EAX, [0x200]: 66 87 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x87, 0x06, 0x00, 0x02]);
    cpu.regs.eax = 0x11111111;
    let bytes = 0x22222222u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x22222222);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x11111111);
}

// ========================================================================
// 32-bit NOT/NEG memory (66 F7 /2, /3 with memory operands)
// ========================================================================

#[test]
fn test_not_mem32() {
    // NOT dword [0x200]: 66 F7 16 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0x16, 0x00, 0x02]);
    let bytes = 0xFF00FF00u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x00FF00FF);
}

#[test]
fn test_neg_mem32() {
    // NEG dword [0x200]: 66 F7 1E 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0x1E, 0x00, 0x02]);
    let bytes = 5u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]) as i32;
    assert_eq!(val, -5);
}

// ========================================================================
// 32-bit MUL/IMUL/DIV memory
// ========================================================================

#[test]
fn test_mul_mem16() {
    // MUL word [0x200]: F7 26 00 02
    let (mut cpu, mut mem) = setup(&[0xF7, 0x26, 0x00, 0x02]);
    mem.ram[0x200] = 10;
    mem.ram[0x201] = 0;
    cpu.regs.set_ax(20);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 200);
    assert_eq!(cpu.regs.dx(), 0);
}

#[test]
fn test_mul_mem32() {
    // MUL dword [0x200]: 66 F7 26 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0x26, 0x00, 0x02]);
    let bytes = 2u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert_eq!(cpu.regs.edx, 0x00000001);
}

// ========================================================================
// TEST r/m32, imm32 (F7 /0 with 66 prefix)
// ========================================================================

#[test]
fn test_test_rm32_imm32() {
    // TEST EAX, 0x80000000: 66 F7 C0 00 00 00 80
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xC0, 0x00, 0x00, 0x00, 0x80]);
    cpu.regs.eax = 0x80000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// 32-bit shift/rotate with memory (0x66 D1/D3 for memory)
// ========================================================================

#[test]
fn test_shl_mem32_1() {
    // SHL dword [0x200], 1: 66 D1 26 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0xD1, 0x26, 0x00, 0x02]);
    let bytes = 0x80000000u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shr_mem32_1() {
    // SHR dword [0x200], 1: 66 D1 2E 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0xD1, 0x2E, 0x00, 0x02]);
    let bytes = 0x00000001u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shl_mem32_cl() {
    // SHL dword [0x200], CL: 66 D3 26 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0xD3, 0x26, 0x00, 0x02]);
    let bytes = 1u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.set_cl(16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0x00010000);
}

#[test]
fn test_shl_mem32_imm8() {
    // SHL dword [0x200], 8: 66 C1 26 00 02 08
    let (mut cpu, mut mem) = setup(&[0x66, 0xC1, 0x26, 0x00, 0x02, 0x08]);
    let bytes = 0x00FF00FFu32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x200],
        mem.ram[0x201],
        mem.ram[0x202],
        mem.ram[0x203],
    ]);
    assert_eq!(val, 0xFF00FF00);
}

// ========================================================================
// 16-bit shift memory by 1 (D1 with memory)
// ========================================================================

#[test]
fn test_shl_mem16_1() {
    // SHL word [0x200], 1: D1 26 00 02
    let (mut cpu, mut mem) = setup(&[0xD1, 0x26, 0x00, 0x02]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x80; // 0x8000
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shr_mem16_1() {
    // SHR word [0x200], 1: D1 2E 00 02
    let (mut cpu, mut mem) = setup(&[0xD1, 0x2E, 0x00, 0x02]);
    mem.ram[0x200] = 0x01;
    mem.ram[0x201] = 0x00; // 0x0001
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_sar_mem16_1() {
    // SAR word [0x200], 1: D1 3E 00 02
    let (mut cpu, mut mem) = setup(&[0xD1, 0x3E, 0x00, 0x02]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x80; // 0x8000
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0xC000);
}

// ========================================================================
// 8-bit shift memory by 1 (D0)
// ========================================================================

#[test]
fn test_shl_mem8_1() {
    // SHL byte [0x200], 1: D0 26 00 02
    let (mut cpu, mut mem) = setup(&[0xD0, 0x26, 0x00, 0x02]);
    mem.ram[0x200] = 0x80;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x00);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shr_mem8_1() {
    // SHR byte [0x200], 1: D0 2E 00 02
    let (mut cpu, mut mem) = setup(&[0xD0, 0x2E, 0x00, 0x02]);
    mem.ram[0x200] = 0x01;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x00);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_rol_mem8_1() {
    // ROL byte [0x200], 1: D0 06 00 02
    let (mut cpu, mut mem) = setup(&[0xD0, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0x80;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x01);
}

#[test]
fn test_ror_mem8_1() {
    // ROR byte [0x200], 1: D0 0E 00 02
    let (mut cpu, mut mem) = setup(&[0xD0, 0x0E, 0x00, 0x02]);
    mem.ram[0x200] = 0x01;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x80);
}

// ========================================================================
// 16-bit shift memory by imm8 (C1)
// ========================================================================

#[test]
fn test_shl_mem16_imm8() {
    // SHL word [0x200], 4: C1 26 00 02 04
    let (mut cpu, mut mem) = setup(&[0xC1, 0x26, 0x00, 0x02, 0x04]);
    mem.ram[0x200] = 0xFF;
    mem.ram[0x201] = 0x0F; // 0x0FFF
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0xFFF0);
}

#[test]
fn test_shr_mem16_imm8() {
    // SHR word [0x200], 4: C1 2E 00 02 04
    let (mut cpu, mut mem) = setup(&[0xC1, 0x2E, 0x00, 0x02, 0x04]);
    mem.ram[0x200] = 0xF0;
    mem.ram[0x201] = 0xFF; // 0xFFF0
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0FFF);
}

// ========================================================================
// 8-bit shift memory by imm8 (C0)
// ========================================================================

#[test]
fn test_shl_mem8_imm8() {
    // SHL byte [0x200], 4: C0 26 00 02 04
    let (mut cpu, mut mem) = setup(&[0xC0, 0x26, 0x00, 0x02, 0x04]);
    mem.ram[0x200] = 0x0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0xF0);
}

#[test]
fn test_shr_mem8_imm8() {
    // SHR byte [0x200], 4: C0 2E 00 02 04
    let (mut cpu, mut mem) = setup(&[0xC0, 0x2E, 0x00, 0x02, 0x04]);
    mem.ram[0x200] = 0xF0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x0F);
}

// ========================================================================
// 32-bit PUSH/POP memory (66 FF /6)
// ========================================================================

#[test]
fn test_push_mem32() {
    // PUSH dword [0x200]: 66 FF 36 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0xFF, 0x36, 0x00, 0x02]);
    let bytes = 0xDEADBEEFu32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x200 + i] = b;
    }
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x7FFC],
        mem.ram[0x7FFD],
        mem.ram[0x7FFE],
        mem.ram[0x7FFF],
    ]);
    assert_eq!(val, 0xDEADBEEF);
}

// ========================================================================
// MOV EAX/moffs32 (0x66 A1/A3)
// ========================================================================

#[test]
fn test_mov_eax_moffs32() {
    // MOV EAX, [0x300]: 66 A1 00 03
    let (mut cpu, mut mem) = setup(&[0x66, 0xA1, 0x00, 0x03]);
    let bytes = 0x12345678u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x300 + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12345678);
}

#[test]
fn test_mov_moffs32_eax() {
    // MOV [0x300], EAX: 66 A3 00 03
    let (mut cpu, mut mem) = setup(&[0x66, 0xA3, 0x00, 0x03]);
    cpu.regs.eax = 0xCAFEBABE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x300],
        mem.ram[0x301],
        mem.ram[0x302],
        mem.ram[0x303],
    ]);
    assert_eq!(val, 0xCAFEBABE);
}

// ========================================================================
// CALL indirect 32-bit (FF /2 with 66)
// ========================================================================

#[test]
fn test_call_indirect_r32() {
    // CALL EAX: 66 FF D0
    let (mut cpu, mut mem) = setup(&[0x66, 0xFF, 0xD0]);
    cpu.regs.eax = 0x0200;
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0200);
}

// ========================================================================
// JMP indirect 32-bit (FF /4 with 66)
// ========================================================================

#[test]
fn test_jmp_indirect_r32() {
    // JMP EAX: 66 FF E0
    let (mut cpu, mut mem) = setup(&[0x66, 0xFF, 0xE0]);
    cpu.regs.eax = 0x0300;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0300);
}

// ========================================================================
// LEA with various addressing modes
// ========================================================================

#[test]
fn test_lea_bx_plus_si() {
    // LEA AX, [BX+SI]: 8D 00
    let (mut cpu, mut mem) = setup(&[0x8D, 0x00]);
    cpu.regs.set_bx(0x100);
    cpu.regs.set_si(0x50);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x150);
}

#[test]
fn test_lea_bp_plus_di_plus_disp8() {
    // LEA AX, [BP+DI+0x10]: 8D 43 10
    let (mut cpu, mut mem) = setup(&[0x8D, 0x43, 0x10]);
    cpu.regs.ebp = 0x200;
    cpu.regs.set_di(0x50);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x260);
}

// ========================================================================
// 32-bit RET near (66 C3)
// ========================================================================

#[test]
fn test_ret_near_32bit() {
    // RET 32-bit: 66 C3
    let (mut cpu, mut mem) = setup(&[0x66, 0xC3]);
    cpu.regs.esp = 0x7FFC;
    let bytes = 0x00000200u32.to_le_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        mem.ram[0x7FFC + i] = b;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0200);
}

// ========================================================================
// 32-bit CALL near (66 E8)
// ========================================================================

#[test]
fn test_call_near_32bit() {
    // CALL near 32-bit +0x100: 66 E8 00 01 00 00
    let (mut cpu, mut mem) = setup(&[0x66, 0xE8, 0x00, 0x01, 0x00, 0x00]);
    cpu.regs.esp = 0x8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // target = (IP + 6) + 0x100 = 0x106 + 0x100 = 0x206
    assert_eq!(cpu.regs.eip, 0x0206);
}

// ========================================================================
// 32-bit JMP near (66 E9)
// ========================================================================

#[test]
fn test_jmp_near_32bit() {
    // JMP near 32-bit +0x100: 66 E9 00 01 00 00
    let (mut cpu, mut mem) = setup(&[0x66, 0xE9, 0x00, 0x01, 0x00, 0x00]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0206);
}

// ========================================================================
// 0x0F near conditional jumps (0F 80 - 0F 8F) — not-taken cases
// ========================================================================

#[test]
fn test_jo_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x80, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jno_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x81, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jb_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x82, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jnb_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x83, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jz_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x84, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jnz_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x85, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jbe_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x86, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_ja_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x87, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_js_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x88, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jns_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x89, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jp_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x8A, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::PARITY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jnp_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x8B, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::PARITY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jl_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x8C, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jge_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x8D, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jle_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x8E, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}

#[test]
fn test_jg_near_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x8F, 0x04, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0104);
}
