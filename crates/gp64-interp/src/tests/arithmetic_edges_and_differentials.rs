use super::*;

// ========================================================================
// IDIV signed overflow — must raise #DE, not Rust panic (Bug A1)
// ========================================================================

#[test]
fn test_idiv8_min_neg1_raises_de() {
    // IDIV BL with AX=0x8000 (i16::MIN), BL=0xFF (-1 as i8)
    // i16::MIN / -1 overflows i16 → must raise #DE (vector 0)
    let (mut cpu, mut mem) = setup(&[0xF6, 0xFB]); // F6 /7 BL
    cpu.regs.eax = 0x8000; // AX = -32768 as i16
    cpu.regs.ebx = 0xFF; // BL = -1 as i8
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 0,
                error_code: 0,
                ..
            }
        ),
        "IDIV8 i16::MIN / -1 must raise #DE, got {:?}",
        result
    );
}

#[test]
fn test_idiv16_min_neg1_raises_de() {
    // IDIV CX with DX:AX=0x80000000 (i32::MIN), CX=0xFFFF (-1 as i16)
    // i32::MIN / -1 overflows i32 → must raise #DE (vector 0)
    let (mut cpu, mut mem) = setup(&[0xF7, 0xF9]); // F7 /7 CX
    cpu.regs.eax = 0x0000; // AX = low word of 0x80000000
    cpu.regs.edx = 0x8000; // DX = high word → DX:AX = 0x80000000
    cpu.regs.ecx = 0xFFFF; // CX = -1 as i16
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 0,
                error_code: 0,
                ..
            }
        ),
        "IDIV16 i32::MIN / -1 must raise #DE, got {:?}",
        result
    );
}

#[test]
fn test_idiv32_min_neg1_raises_de() {
    // IDIV ECX with EDX:EAX=0x8000000000000000 (i64::MIN), ECX=0xFFFFFFFF (-1 as i32)
    // i64::MIN / -1 overflows i64 → must raise #DE (vector 0)
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xF9]); // 66 F7 /7 ECX (32-bit operand)
    cpu.regs.eax = 0x00000000; // EAX = low dword of i64::MIN
    cpu.regs.edx = 0x80000000; // EDX = high dword → EDX:EAX = i64::MIN
    cpu.regs.ecx = 0xFFFFFFFF; // ECX = -1 as i32
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 0,
                error_code: 0,
                ..
            }
        ),
        "IDIV32 i64::MIN / -1 must raise #DE, got {:?}",
        result
    );
}

#[test]
fn test_idiv8_normal() {
    // IDIV BL with AX=7, BL=2 → AL=3 (quotient), AH=1 (remainder)
    let (mut cpu, mut mem) = setup(&[0xF6, 0xFB]); // F6 /7 BL
    cpu.regs.eax = 7;
    cpu.regs.ebx = 2;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Normal IDIV8 should complete, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax as u8, 3, "AL = quotient = 3");
    assert_eq!((cpu.regs.eax >> 8) as u8, 1, "AH = remainder = 1");
}

#[test]
fn test_idiv8_singlestep_boundary_matches_386_result() {
    let (mut cpu, mut mem) = setup(&[0xF6, 0xF9]);
    cpu.regs.eax = 0xA171_9C71;
    cpu.regs.ecx = 0x2BA2_0547;

    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 0,
                error_code: 0,
                ..
            }
        ),
        "IDIV8 quotient overflow must raise #DE, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax, 0xA171_9C71,
        "#DE must preserve the architectural dividend in EAX"
    );
}

#[test]
fn test_idiv16_normal() {
    // IDIV CX with DX:AX=7, CX=2 → AX=3 (quotient), DX=1 (remainder)
    let (mut cpu, mut mem) = setup(&[0xF7, 0xF9]); // F7 /7 CX
    cpu.regs.eax = 7;
    cpu.regs.edx = 0;
    cpu.regs.ecx = 2;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Normal IDIV16 should complete, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax as u16, 3, "AX = quotient = 3");
    assert_eq!(cpu.regs.edx as u16, 1, "DX = remainder = 1");
}

// ----------------------------------------------------------------
// S13: BT/BTS/BTR/BTC memory bit indexing for large offsets
// ----------------------------------------------------------------

/// BT [mem], reg16 with bit index > 15 — should access addr + (bit_index >> 3)
#[test]
fn test_bt_mem_reg16_large_offset() {
    // BT [0x2000], AX  — 0F A3 06 00 20
    // ModR/M 0x06 = [disp16], reg = AX (bits 3-5 = 000)
    let code: &[u8] = &[0x0F, 0xA3, 0x06, 0x00, 0x20];
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.eax = 19; // bit 19 → byte offset 2, bit 3
                       // Set bit 3 at address 0x2002 (byte offset = 19 >> 3 = 2)
    mem.ram[0x2002] = 0x08; // bit 3 set
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        cpu.regs.flags.contains(Flags::CARRY),
        "CF should be set: bit 19 (byte 2, bit 3) is set"
    );

    // Same but bit NOT set
    let (mut cpu2, mut mem2) = setup(code);
    cpu2.regs.eax = 20; // bit 20 → byte offset 2, bit 4
    mem2.ram[0x2002] = 0x08; // only bit 3 set, not bit 4
    let result = run(&mut cpu2, &mut mem2, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        !cpu2.regs.flags.contains(Flags::CARRY),
        "CF should be clear: bit 20 (byte 2, bit 4) is not set"
    );
}

/// BTS [mem], reg32 with bit index = 35 — should access addr+4, bit 3
#[test]
fn test_bts_mem_reg32_large_offset() {
    // 66 prefix for 32-bit operand size
    // BTS [0x2000], EAX — 66 0F AB 06 00 20
    // ModR/M 0x06 = [disp16], reg = EAX (bits 3-5 = 000)
    let code: &[u8] = &[0x66, 0x0F, 0xAB, 0x06, 0x00, 0x20];
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.eax = 35; // bit 35 → byte offset 4, bit 3
    mem.ram[0x2004] = 0x00; // bit 3 NOT set
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        !cpu.regs.flags.contains(Flags::CARRY),
        "CF should be clear: original bit was 0"
    );
    assert_eq!(mem.ram[0x2004], 0x08, "BTS should have set bit 3 at addr+4");
}

/// BT [mem], reg with negative bit index wraps backward
#[test]
fn test_bt_mem_reg_negative_offset() {
    // BT [0x2010], AX — 0F A3 06 10 20
    let code: &[u8] = &[0x0F, 0xA3, 0x06, 0x10, 0x20];
    let (mut cpu, mut mem) = setup(code);
    // AX = 0xFFF0 = -16 as i16 → byte offset = -2, bit 0
    cpu.regs.eax = 0xFFF0;
    // Address = 0x2010 + (-2) = 0x200E, bit 0
    mem.ram[0x200E] = 0x01; // bit 0 set
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        cpu.regs.flags.contains(Flags::CARRY),
        "CF should be set: bit at negative offset is set"
    );
}

#[test]
fn test_bt_mem_reg16_negative_offset_wraps_within_segment() {
    // BT [0x0001], AX with AX=-16 addresses DS:FFFF, not linear DS-1.
    let code: &[u8] = &[0x0F, 0xA3, 0x06, 0x01, 0x00];
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.ds = 0x1000;
    cpu.regs.ds_cache = gp64_cpu::SegmentCache::real_mode(0x1000);
    cpu.regs.eax = 0xFFF0;
    mem.ram[0x1_FFFF] = 0x01;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

/// BTC [mem], reg at large offset — verify CF and bit complement
#[test]
fn test_btc_mem_reg_sets_cf() {
    // BTC [0x2000], AX — 0F BB 06 00 20
    let code: &[u8] = &[0x0F, 0xBB, 0x06, 0x00, 0x20];
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.eax = 27; // bit 27 → byte offset 3, bit 3
    mem.ram[0x2003] = 0x08; // bit 3 set
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        cpu.regs.flags.contains(Flags::CARRY),
        "CF should reflect original bit (was set)"
    );
    assert_eq!(
        mem.ram[0x2003], 0x00,
        "BTC should have complemented bit 3 (was 1 → 0)"
    );

    // BTC again — bit was cleared, should set it
    let (mut cpu2, mut mem2) = setup(code);
    cpu2.regs.eax = 27;
    mem2.ram[0x2003] = 0x00; // bit 3 NOT set
    let result = run(&mut cpu2, &mut mem2, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(
        !cpu2.regs.flags.contains(Flags::CARRY),
        "CF should reflect original bit (was clear)"
    );
    assert_eq!(
        mem2.ram[0x2003], 0x08,
        "BTC should have complemented bit 3 (was 0 → 1)"
    );
}

// ----------------------------------------------------------------
// A8-A9: MUL/IMUL undefined flag matching
// ----------------------------------------------------------------

/// MUL AL, 0 should set ZF (result is 0)
#[test]
fn test_mul8_sets_zf_sf_pf() {
    // MUL CL (F6 /4, modrm=0xE1): AX = AL * CL
    let code: &[u8] = &[0xF6, 0xE1];
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.set_al(5);
    cpu.regs.ecx = 0; // multiply by 0
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0, "5 * 0 = 0");
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be set for zero result"
    );
    assert!(
        !cpu.regs.flags.contains(Flags::SIGN),
        "SF should be clear for zero result"
    );
    assert!(
        cpu.regs.flags.contains(Flags::PARITY),
        "PF should be set for zero (even parity)"
    );
}

/// MUL AX with non-trivial result — check SF and PF on low half
#[test]
fn test_mul16_undefined_flags() {
    // MUL CX (F7 /4, modrm=0xE1): DX:AX = AX * CX
    let code: &[u8] = &[0xF7, 0xE1];
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.eax = 0x0080; // 128
    cpu.regs.ecx = 0x0003; // * 3 = 384 = 0x0180
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0x0180);
    assert_eq!(cpu.regs.dx(), 0x0000);
    assert!(
        !cpu.regs.flags.contains(Flags::ZERO),
        "ZF clear: result != 0"
    );
    // SF = sign of low half (0x0180) → bit 15 = 0
    assert!(
        !cpu.regs.flags.contains(Flags::SIGN),
        "SF clear: bit 15 of AX is 0"
    );
    // PF = parity of low byte (0x80 = 1 bit set → odd parity)
    assert!(
        !cpu.regs.flags.contains(Flags::PARITY),
        "PF clear: 0x80 has odd parity"
    );
}

/// IMUL r/m8 — check ZF/SF/PF on result
#[test]
fn test_imul8_undefined_flags() {
    // IMUL CL (F6 /5, modrm=0xE9): AX = AL * CL (signed)
    let code: &[u8] = &[0xF6, 0xE9];
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.set_al(0xFE); // -2
    cpu.regs.ecx = 0x03; // * 3 = -6 = 0xFFFA
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0xFFFA);
    assert!(
        !cpu.regs.flags.contains(Flags::ZERO),
        "ZF clear: result != 0"
    );
    // SF = sign of low byte (0xFA = bit 7 set)
    assert!(
        cpu.regs.flags.contains(Flags::SIGN),
        "SF set: low byte 0xFA has bit 7 set"
    );
    // PF = parity of low byte 0xFA = 11111010 = 6 bits set → even parity
    assert!(
        cpu.regs.flags.contains(Flags::PARITY),
        "PF set: 0xFA has even parity"
    );
}

// M6: MUL/IMUL ZF checks low half only

#[test]
fn test_mul8_zf_low_half_zero_high_nonzero() {
    // MUL r/m8: AL=0x10 * CL=0x10 = AX=0x0100 → AL=0x00 (low half zero), AH=0x01
    // ZF should be SET because low half (AL) is zero
    let code: &[u8] = &[0xF6, 0xE1]; // MUL CL
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.set_al(0x10);
    cpu.regs.ecx = 0x10;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0x0100);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be set: AL (low half) is zero"
    );
}

#[test]
fn test_mul8_zf_low_half_nonzero() {
    // MUL r/m8: AL=3 * CL=5 = AX=0x000F → AL=0x0F (nonzero)
    // ZF should be CLEAR
    let code: &[u8] = &[0xF6, 0xE1];
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.set_al(3);
    cpu.regs.ecx = 5;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 15);
    assert!(
        !cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be clear: AL is nonzero"
    );
}

#[test]
fn test_mul16_zf_low_half_zero_high_nonzero() {
    // MUL r/m16: AX=0x0100 * CX=0x0100 = DX:AX = 0x0001:0x0000
    // ZF should be SET because low half (AX) is zero
    let code: &[u8] = &[0xF7, 0xE1]; // MUL CX
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.eax = 0x0100;
    cpu.regs.ecx = 0x0100;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0x0000);
    assert_eq!(cpu.regs.dx(), 0x0001);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be set: AX (low half) is zero"
    );
}

#[test]
fn test_mul32_zf_low_half_zero_high_nonzero() {
    // MUL r/m32: EAX=0x00010000 * ECX=0x00010000 = EDX:EAX = 1:0x00000000
    // ZF should be SET because low half (EAX) is zero
    // Need 32-bit op: use 0x66 prefix in 16-bit mode → F7 E1
    let code: &[u8] = &[0x66, 0xF7, 0xE1]; // o32 MUL ECX
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.eax = 0x0001_0000;
    cpu.regs.ecx = 0x0001_0000;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax, 0);
    assert_eq!(cpu.regs.edx, 1);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be set: EAX (low half) is zero"
    );
}

#[test]
fn test_imul8_zf_low_half_zero() {
    // IMUL r/m8: AL=0x10 * CL=0x10 = AX=0x0100 → AL=0x00 (low half zero)
    // ZF should be SET
    let code: &[u8] = &[0xF6, 0xE9]; // IMUL CL
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.set_al(0x10);
    cpu.regs.ecx = 0x10;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0x0100);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be set: AL (low half) is zero"
    );
}

#[test]
fn test_imul16_zf_low_half_zero() {
    // IMUL r/m16: AX=0x0100 * CX=0x0100 = DX:AX = 0x0001:0x0000
    // ZF should be SET because low half (AX) is zero
    let code: &[u8] = &[0xF7, 0xE9]; // IMUL CX
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.eax = 0x0100;
    cpu.regs.ecx = 0x0100;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0x0000);
    assert_eq!(cpu.regs.dx(), 0x0001);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be set: AX (low half) is zero"
    );
}

#[test]
fn test_imul32_zf_low_half_zero() {
    // IMUL r/m32: EAX=0x00010000 * ECX=0x00010000 = EDX:EAX = 1:0x00000000
    // ZF should be SET because low half (EAX) is zero
    let code: &[u8] = &[0x66, 0xF7, 0xE9]; // o32 IMUL ECX
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.eax = 0x0001_0000;
    cpu.regs.ecx = 0x0001_0000;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax, 0);
    assert_eq!(cpu.regs.edx, 1);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be set: EAX (low half) is zero"
    );
}

/// M7: DIV/IDIV flags behavior.
///
/// Intel SDM leaves flags undefined after DIV/IDIV. This interpreter
/// preserves their previous values; this test records that policy.
#[test]
fn test_div_preserves_flags() {
    // Set flags to known state, then DIV — flags should remain unchanged
    // DIV CL (F6 /6, modrm=0xF1): AL = AX / CL, AH = AX % CL
    let code: &[u8] = &[0xF6, 0xF1];
    let (mut cpu, mut mem) = setup(code);
    cpu.regs.eax = 100;
    cpu.regs.ecx = 7;
    // Set some flags before DIV
    cpu.regs.flags.insert(Flags::ZERO);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.insert(Flags::PARITY);
    cpu.regs.flags.insert(Flags::CARRY);
    let flags_before = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    // DIV preserves flags (Intel says "undefined"; we leave unchanged)
    assert_eq!(cpu.regs.flags, flags_before, "DIV should not modify flags");
}

// ========================================================================
// B1: x86 instruction length limit (15 bytes max)
// ========================================================================

fn assert_overlength_gp(result: RunResult) {
    match result {
        RunResult::Exception {
            vector,
            error_code,
            executed,
            ..
        } => {
            assert_eq!(vector, 13, "overlength instruction must raise #GP");
            assert_eq!(error_code, 0);
            assert_eq!(executed, 0, "faulting instruction must not retire");
        }
        other => panic!("expected overlength #GP(0), got {other:?}"),
    }
}

#[test]
fn test_instruction_length_limit_normal() {
    // 4-byte instruction: 66 26 89 00 (MOV ES:[BX+SI], AX with 66 prefix)
    // Should execute normally (well under 15 bytes)
    let (mut cpu, mut mem) = setup(&[0x66, 0x26, 0x89, 0x00]);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    // Should NOT be a #GP — instruction is only 4 bytes
    assert!(!matches!(result, RunResult::Exception { vector: 13, .. }));
}

#[test]
fn test_instruction_length_limit_exceeded() {
    // 16 segment override prefixes (0x26 = ES:) followed by NOP (0x90)
    // Total = 17 bytes, exceeds 15-byte limit → #GP(0)
    let mut code = vec![0x26u8; 16];
    code.push(0x90); // NOP
    let (mut cpu, mut mem) = setup(&code);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    match result {
        RunResult::Exception {
            vector, error_code, ..
        } => {
            assert_eq!(vector, 13, "should raise #GP");
            assert_eq!(error_code, 0);
        }
        other => panic!("expected #GP, got {:?}", other),
    }
}

#[test]
fn test_instruction_length_limit_exactly_15() {
    // 14 segment override prefixes + NOP = exactly 15 bytes (should succeed)
    let mut code = vec![0x26u8; 14];
    code.push(0x90); // NOP — total 15 bytes
    let (mut cpu, mut mem) = setup(&code);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    // Exactly 15 bytes is OK
    assert!(
        !matches!(result, RunResult::Exception { vector: 13, .. }),
        "15-byte instruction should not raise #GP"
    );
}

#[test]
fn test_instruction_length_limit_post_decode() {
    // 14 prefixes + MOV [BX+SI], AX (0x89 0x00) = 16 bytes total.
    // Pre-decode check passes (14 prefixes < 15), but post-decode check
    // catches the 16-byte total.  Real hardware: #GP(0).
    let mut code = vec![0x26u8; 14];
    code.push(0x89); // MOV r/m16, r16
    code.push(0x00); // ModR/M: [BX+SI], AX
    let (mut cpu, mut mem) = setup(&code);
    cpu.regs.eax = 0xBEEF;
    cpu.regs.ebx = 0x0200;
    mem.ram[0x0200] = 0xA5;
    mem.ram[0x0201] = 0x5A;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_overlength_gp(result);
    assert_eq!(cpu.regs.eip, 0x0100, "#GP must point at the instruction");
    assert_eq!(
        &mem.ram[0x0200..0x0202],
        &[0xA5, 0x5A],
        "overlength MOV must not write memory"
    );
}

#[test]
fn test_instruction_length_limit_x87_escape() {
    // Fourteen prefixes plus FLD1 is 16 bytes. The x87 decoder reports its
    // dedicated InstructionTooLong error, which is architecturally #GP(0),
    // not the #UD used for invalid x87 encodings.
    let mut code = vec![0x66u8; 14];
    code.extend_from_slice(&[0xD9, 0xE8]); // FLD1
    let (mut cpu, mut mem) = setup(&code);
    let flags_before = cpu.regs.flags;
    let status_before = cpu.fpu.status_word;
    let tag_before = cpu.fpu.tag_word;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert_overlength_gp(result);
    assert_eq!(cpu.regs.eip, 0x0100, "#GP must point at the instruction");
    assert_eq!(cpu.regs.flags, flags_before);
    assert_eq!(cpu.fpu.status_word, status_before);
    assert_eq!(cpu.fpu.tag_word, tag_before);
}

#[test]
fn test_instruction_length_limit_preserves_ud_for_exact_15_byte_ud2() {
    // 13 prefixes + fixed-length UD2 = exactly 15 bytes. The preflight
    // must leave the opcode's architectural #UD classification intact.
    let mut code = vec![0x26u8; 13];
    code.extend_from_slice(&[0x0F, 0x0B]);
    let (mut cpu, mut mem) = setup(&code);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 6,
                error_code: 0,
                executed: 0,
                ..
            }
        ),
        "exactly 15-byte UD2 must raise #UD, got {result:?}"
    );
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn test_instruction_length_limit_preserves_register_state() {
    // 13 prefixes + MOV AX, imm16 = 16 bytes.
    let mut code = vec![0x26u8; 13];
    code.extend_from_slice(&[0xB8, 0x34, 0x12]);
    let (mut cpu, mut mem) = setup(&code);
    cpu.regs.eax = 0xCAFE_BABE;
    let flags_before = cpu.regs.flags;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert_overlength_gp(result);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0xCAFE_BABE);
    assert_eq!(cpu.regs.flags, flags_before);
}

#[test]
fn test_instruction_length_limit_preempts_control_transfer() {
    // 14 prefixes + JMP short + displacement = 16 bytes. This used to set
    // ip_set and bypass the common post-decode length check entirely.
    let mut code = vec![0x26u8; 14];
    code.extend_from_slice(&[0xEB, 0x00]);
    let (mut cpu, mut mem) = setup(&code);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert_overlength_gp(result);
    assert_eq!(cpu.regs.eip, 0x0100, "branch target must not commit");
}

#[test]
fn test_instruction_length_limit_preempts_terminal_interrupt() {
    // 14 prefixes + INT 20h = 16 bytes. INT returns from its opcode arm,
    // so the old epilogue-only check never ran.
    let mut code = vec![0x26u8; 14];
    code.extend_from_slice(&[0xCD, 0x20]);
    let (mut cpu, mut mem) = setup(&code);
    cpu.regs.esp = 0x8000;
    let flags_before = cpu.regs.flags;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert_overlength_gp(result);
    assert_eq!(cpu.regs.eip, 0x0100, "interrupt handler must not run");
    assert_eq!(cpu.regs.esp, 0x8000, "INT must not push a frame");
    assert_eq!(cpu.regs.flags, flags_before);
}

// ========================================================================
// B4: ADC/SBB chain differential tests — verify carry propagation
// ========================================================================

#[test]
fn test_adc_sbb_chain_add_then_adc() {
    // ADD AL, 0xFF (AL=1 + 0xFF = 0x00, CF=1)
    // ADC AH, 0x00 (AH=0 + 0 + CF=1 → AH=1, CF=0)
    let (mut cpu, mut mem) = setup(&[
        0x04, 0xFF, // ADD AL, 0xFF
        0x80, 0xD4, 0x00, // ADC AH, 0x00
    ]);
    cpu.regs.eax = 0x0001; // AL=1, AH=0
    let _ = run(&mut cpu, &mut mem, &mut (), 2);
    assert_eq!(cpu.regs.al(), 0x00, "ADD AL,0xFF: 1+0xFF=0x00");
    assert_eq!(cpu.regs.ah(), 0x01, "ADC AH,0: 0+0+CF=1");
    assert!(!cpu.regs.flags.contains(Flags::CARRY), "ADC result CF=0");
}

#[test]
fn test_adc_sbb_chain_sub_then_sbb() {
    // SUB AL, 1 (AL=0 → 0xFF, CF=1)
    // SBB AH, 0 (AH=0 - 0 - CF=1 → 0xFF, CF=1)
    let (mut cpu, mut mem) = setup(&[
        0x2C, 0x01, // SUB AL, 1
        0x80, 0xDC, 0x00, // SBB AH, 0
    ]);
    cpu.regs.eax = 0x0000; // AL=0, AH=0
    let _ = run(&mut cpu, &mut mem, &mut (), 2);
    assert_eq!(cpu.regs.al(), 0xFF, "SUB AL,1: 0-1=0xFF");
    assert_eq!(cpu.regs.ah(), 0xFF, "SBB AH,0: 0-0-1=0xFF");
    assert!(cpu.regs.flags.contains(Flags::CARRY), "SBB result CF=1");
}

#[test]
fn test_adc_sbb_carry_clear_no_borrow() {
    // ADD AL, 1 (AL=0 → 1, CF=0)
    // SBB AH, 0 (AH=5 - 0 - CF=0 → 5, CF=0)
    let (mut cpu, mut mem) = setup(&[
        0x04, 0x01, // ADD AL, 1
        0x80, 0xDC, 0x00, // SBB AH, 0
    ]);
    cpu.regs.eax = 0x0500; // AL=0, AH=5
    let _ = run(&mut cpu, &mut mem, &mut (), 2);
    assert_eq!(cpu.regs.al(), 0x01);
    assert_eq!(
        cpu.regs.ah(),
        0x05,
        "SBB with CF=0 should not subtract borrow"
    );
}

#[test]
fn test_adc_multi_chain_three_ops() {
    // ADD AL, 0xF0 (AL=0x20 → 0x10, CF=1)
    // ADC AH, 0x00 (AH=0 → 1, CF=0)
    // ADC DL, 0x00 (DL=0 → 0, CF=0)
    let (mut cpu, mut mem) = setup(&[
        0x04, 0xF0, // ADD AL, 0xF0
        0x80, 0xD4, 0x00, // ADC AH, 0
        0x80, 0xD2, 0x00, // ADC DL, 0
    ]);
    cpu.regs.eax = 0x0020; // AL=0x20, AH=0
    cpu.regs.edx = 0x0000;
    let _ = run(&mut cpu, &mut mem, &mut (), 3);
    assert_eq!(cpu.regs.al(), 0x10, "ADD: 0x20+0xF0=0x10, CF=1");
    assert_eq!(cpu.regs.ah(), 0x01, "ADC: 0+0+1=1, CF=0");
    assert_eq!(cpu.regs.dl(), 0x00, "ADC: 0+0+0=0, CF=0");
    assert!(!cpu.regs.flags.contains(Flags::CARRY), "final CF=0");
}

#[test]
fn test_adc_after_logic_cf_clear() {
    // XOR AL, AL (CF=0 via Logic lazy state)
    // ADC AH, 0x00 (AH=0xFF + 0 + CF=0 → 0xFF, CF=0)
    let (mut cpu, mut mem) = setup(&[
        0x30, 0xC0, // XOR AL, AL
        0x80, 0xD4, 0x00, // ADC AH, 0
    ]);
    cpu.regs.eax = 0xFF00; // AH=0xFF, AL=0
    let _ = run(&mut cpu, &mut mem, &mut (), 2);
    assert_eq!(cpu.regs.ah(), 0xFF, "ADC with CF=0 from XOR");
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_adc_after_inc_preserves_cf() {
    // ADD AL, 0xFF (AL=1 → 0, CF=1)
    // INC AH (preserves CF=1)
    // ADC DL, 0 (DL=0 + 0 + CF=1 → 1)
    let (mut cpu, mut mem) = setup(&[
        0x04, 0xFF, // ADD AL, 0xFF
        0xFE, 0xC4, // INC AH
        0x80, 0xD2, 0x00, // ADC DL, 0
    ]);
    cpu.regs.eax = 0x0001; // AL=1, AH=0
    cpu.regs.edx = 0x0000;
    let _ = run(&mut cpu, &mut mem, &mut (), 3);
    assert_eq!(cpu.regs.al(), 0x00);
    assert_eq!(cpu.regs.ah(), 0x01, "INC AH: 0→1");
    assert_eq!(cpu.regs.dl(), 0x01, "ADC DL,0: CF=1 preserved through INC");
}

// ---- LTR tss_limit tests (P4: u16→u32 with effective_limit) ----

/// Helper: set up protected mode with a GDT at 0x1000 and code at 0x0100.
/// Returns the GDT base address for writing descriptors.
fn setup_pm_ltr(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 |= 1; // Protected mode
    cpu.regs.cpl = 0; // LTR requires ring 0
    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFF;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.present = true;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFF;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFF;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.esp = 0x1000;
    cpu.cs_bits = 16;

    // GDT at 0x1000 with enough room for several entries
    cpu.regs.gdtr.base = 0x1000;
    cpu.regs.gdtr.limit = 0xFF; // 32 entries max

    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

/// Write an 8-byte descriptor at GDT[index].
fn write_gdt_descriptor(mem: &mut Memory, gdt_base: u32, index: usize, desc: &[u8; 8]) {
    let addr = gdt_base as usize + index * 8;
    for (i, &b) in desc.iter().enumerate() {
        mem.ram[addr + i] = b;
    }
}

fn map_test_page(
    mem: &mut Memory,
    page_directory: usize,
    page_table: usize,
    linear: u32,
    physical: usize,
    pte_flags: u32,
) {
    let pde_index = (linear >> 22) as usize;
    let pte_index = ((linear >> 12) & 0x03FF) as usize;
    mem.write_u32_physical(page_directory + pde_index * 4, (page_table as u32) | 0x07);
    mem.write_u32_physical(
        page_table + pte_index * 4,
        ((physical as u32) & 0xFFFF_F000) | pte_flags,
    );
}

fn setup_ltr_paging(
    cpu: &mut Cpu,
    mem: &mut Memory,
    gdt_linear: u32,
    gdt_physical: usize,
    gdt_writable: bool,
) -> (usize, usize) {
    let page_directory = 0x20000usize;
    let identity_page_table = 0x21000usize;
    let high_page_table = 0x22000usize;

    cpu.regs.cr0 |= 0x8000_0000;
    cpu.regs.cr3 = page_directory as u32;

    map_test_page(
        mem,
        page_directory,
        identity_page_table,
        0x0000,
        0x0000,
        0x07,
    );
    map_test_page(
        mem,
        page_directory,
        identity_page_table,
        0x3000,
        0x3000,
        0x07,
    );
    map_test_page(
        mem,
        page_directory,
        high_page_table,
        gdt_linear,
        gdt_physical,
        if gdt_writable { 0x07 } else { 0x05 },
    );

    let high_pde_addr = page_directory + ((gdt_linear >> 22) as usize) * 4;
    let high_pte_addr = high_page_table + (((gdt_linear >> 12) & 0x03FF) as usize) * 4;
    (high_pde_addr, high_pte_addr)
}

/// MDD-BUG-CRU-01545: LTR with an LDT selector is #GP. It must neither
/// load TR from the LDT's TSS nor set the busy bit of the unrelated GDT
/// entry at the same index.
#[test]
fn ltr_with_an_ldt_selector_faults_and_touches_neither_table() {
    let (mut cpu, mut mem) = setup_pm_ltr(&[0x0F, 0x00, 0xD8]); // LTR AX
    cpu.regs.set_ax(0x000C); // index 1, TI = 1
                             // GDT[1]: a present read-only data segment (access 0x90).
    write_gdt_descriptor(&mut mem, 0x1000, 1, &[0xFF, 0xFF, 0, 0, 0, 0x90, 0, 0]);
    // LDT at 0x2000 whose entry 1 is an available 386 TSS.
    cpu.regs.ldt_base = 0x2000;
    cpu.regs.ldt_limit = 0x0F;
    write_gdt_descriptor(&mut mem, 0x2000, 1, &[0x67, 0, 0, 0x30, 0, 0x89, 0, 0]);
    cpu.regs.tr = 0x00A8;

    let mut io = ();
    match run(&mut cpu, &mut mem, &mut io, 1) {
        RunResult::Exception { vector, .. } => assert_eq!(vector, 13),
        other => panic!("LTR of an LDT selector must #GP, got {other:?}"),
    }
    assert_eq!(cpu.regs.tr, 0x00A8, "TR is unchanged");
    assert_eq!(mem.ram[0x100D], 0x90, "GDT[1] gains no busy bit");
    assert_eq!(mem.ram[0x200D], 0x89, "LDT[1] is untouched");
}

#[test]
fn ltr_invalid_descriptor_does_not_commit_tr() {
    // MDD-BUG-ANVIL-00011: LTR must validate the descriptor is an available TSS
    // (and present) BEFORE writing the busy bit or committing TR. A data/code
    // descriptor or an already-busy TSS #GPs; a not-present available TSS #NPs;
    // all with TR unchanged and the descriptor's busy bit left unset.
    // (access byte, expected fault vector):
    //   0x92 data (S=1)                 -> #GP (13)
    //   0x8B 386 busy TSS (S=0,type=B)  -> #GP (13)
    //   0x09 available TSS, NOT present -> #NP (11)
    for (access, expected_vector) in [(0x92u8, 13u8), (0x8Bu8, 13u8), (0x09u8, 11u8)] {
        let (mut cpu, mut mem) = setup_pm_ltr(&[0x0F, 0x00, 0xD8]); // LTR AX
        cpu.regs.set_ax(0x0008); // selector -> GDT[1]
        write_gdt_descriptor(
            &mut mem,
            0x1000,
            1,
            &[0x67, 0x00, 0x00, 0x30, 0x00, access, 0x00, 0x00],
        );
        // Sentinels that must survive a faulting LTR.
        cpu.regs.tr = 0x00A8;
        cpu.regs.tss_base = 0xCAFE_0000;
        cpu.regs.tss_limit = 0x0000_1234;

        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        match result {
            RunResult::Exception { vector, .. } => assert_eq!(
                vector, expected_vector,
                "LTR of access {access:#04X} should fault with vector {expected_vector}"
            ),
            other => panic!("LTR of access {access:#04X} must fault, got {other:?}"),
        }
        assert_eq!(
            cpu.regs.tr, 0x00A8,
            "TR must be unchanged on a faulting LTR (access {access:#04X})"
        );
        assert_eq!(
            cpu.regs.tss_base, 0xCAFE_0000,
            "tss_base must be unchanged (access {access:#04X})"
        );
        // GDT[1] access byte at linear 0x1000 + 1*8 + 5 = 0x100D: the busy bit
        // (0x02) must NOT have been written on a faulting LTR.
        assert_eq!(
            mem.ram[0x100D], access,
            "LTR must not set the busy bit on a faulting descriptor (access {access:#04X})"
        );
    }
}

#[test]
fn test_ltr_normal_tss_stores_limit_0x67() {
    // LTR AX (0F 00 D8) with selector 0x0008 pointing to GDT[1]
    // TSS descriptor: base=0x3000, limit=0x0067, no granularity
    // Access=0x89: Present, DPL=0, System, Type=9 (386 TSS available)
    let (mut cpu, mut mem) = setup_pm_ltr(&[0x0F, 0x00, 0xD8]); // LTR AX
    cpu.regs.set_ax(0x0008);

    // Write TSS descriptor at GDT[1]:
    // limit_low=0x0067, base_low=0x3000, base_mid=0x00
    // access=0x89, limit_high|flags=0x00, base_high=0x00
    write_gdt_descriptor(
        &mut mem,
        0x1000,
        1,
        &[
            0x67, 0x00, // limit low = 0x0067
            0x00, 0x30, // base low = 0x3000
            0x00, // base mid = 0x00
            0x89, // access: P=1, DPL=0, S=0, Type=9 (386 TSS available)
            0x00, // limit_high=0, flags=0 (no granularity)
            0x00, // base high = 0x00
        ],
    );

    // Write minimal TSS data at base 0x3000 (at least SS0/ESP0)
    mem.write_u32(0x3004, 0x9000); // ESP0
    mem.write_u16(0x3008, 0x0010); // SS0

    let _ = run(&mut cpu, &mut mem, &mut (), 1);

    assert_eq!(cpu.regs.tr, 0x0008);
    assert_eq!(cpu.regs.tss_base, 0x3000);
    assert_eq!(
        mem.read_u8(0x1000 + 1 * 8 + 5),
        0x8B,
        "LTR should mark the TSS descriptor busy"
    );
    assert_eq!(
        cpu.regs.tss_limit, 0x0067,
        "Normal TSS limit should be 0x67"
    );
}

#[test]
fn test_ltr_busy_bit_uses_paged_gdtr_base() {
    const GDT_LINEAR: u32 = 0x8003_6000;
    const GDT_PHYSICAL: usize = 0x40000;
    const SELECTOR: u16 = 0x0028;
    const INDEX: usize = (SELECTOR as usize) >> 3;

    let (mut cpu, mut mem) = setup_pm_ltr(&[0x0F, 0x00, 0xD8]);
    cpu.regs.set_ax(SELECTOR);
    cpu.regs.gdtr.base = GDT_LINEAR;
    let (high_pde_addr, high_pte_addr) =
        setup_ltr_paging(&mut cpu, &mut mem, GDT_LINEAR, GDT_PHYSICAL, true);

    write_gdt_descriptor(
        &mut mem,
        GDT_PHYSICAL as u32,
        INDEX,
        &[
            0x67, 0x00, // limit low = 0x0067
            0x00, 0x30, // base low = 0x3000
            0x00, // base mid = 0x00
            0x89, // access: available 386 TSS
            0x00, // flags
            0x00, // base high
        ],
    );
    mem.write_u32(0x3004, 0x9000);
    mem.write_u16(0x3008, 0x0010);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.tr, SELECTOR);
    assert_eq!(cpu.regs.tss_base, 0x3000);
    assert_eq!(
        mem.read_u8(GDT_PHYSICAL + INDEX * 8 + 5),
        0x8B,
        "busy bit should be written through the page-table mapping"
    );
    assert_ne!(
        mem.read_u32_physical(high_pde_addr) & 0x20,
        0,
        "descriptor write should set the PDE accessed bit"
    );
    let high_pte = mem.read_u32_physical(high_pte_addr);
    assert_ne!(
        high_pte & 0x20,
        0,
        "descriptor write should set PTE accessed"
    );
    assert_ne!(high_pte & 0x40, 0, "descriptor write should set PTE dirty");
}

#[test]
fn test_ltr_busy_bit_write_fault_does_not_commit_tr() {
    const GDT_LINEAR: u32 = 0x8003_6000;
    const GDT_PHYSICAL: usize = 0x40000;
    const SELECTOR: u16 = 0x0028;
    const INDEX: usize = (SELECTOR as usize) >> 3;

    let (mut cpu, mut mem) = setup_pm_ltr(&[0x0F, 0x00, 0xD8]);
    cpu.regs.set_ax(SELECTOR);
    cpu.regs.gdtr.base = GDT_LINEAR;
    cpu.regs.cr0 |= 0x0001_0000; // CR0.WP: supervisor writes fault on RO pages.
    cpu.regs.tr = 0x0030;
    cpu.regs.tss_base = 0x1234_5000;
    cpu.regs.tss_limit = 0x0000_BEEF;
    setup_ltr_paging(&mut cpu, &mut mem, GDT_LINEAR, GDT_PHYSICAL, false);

    write_gdt_descriptor(
        &mut mem,
        GDT_PHYSICAL as u32,
        INDEX,
        &[
            0x67, 0x00, // limit low = 0x0067
            0x00, 0x30, // base low = 0x3000
            0x00, // base mid = 0x00
            0x89, // access: available 386 TSS
            0x00, // flags
            0x00, // base high
        ],
    );

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
            assert_eq!(fault_addr, Some(GDT_LINEAR + SELECTOR as u32 + 5));
            assert_ne!(error_code & 0x01, 0, "fault should be protection violation");
            assert_ne!(error_code & 0x02, 0, "fault should be a write");
            assert_eq!(error_code & 0x04, 0, "LTR is a supervisor access");
        }
        other => panic!(
            "expected #PF from read-only descriptor page, got {:?}",
            other
        ),
    }
    assert_eq!(cpu.regs.tr, 0x0030, "failed LTR must not commit TR");
    assert_eq!(cpu.regs.tss_base, 0x1234_5000);
    assert_eq!(cpu.regs.tss_limit, 0x0000_BEEF);
    assert_eq!(
        mem.read_u8(GDT_PHYSICAL + INDEX * 8 + 5),
        0x89,
        "descriptor must remain available after the write fault"
    );
}

#[test]
fn test_ltr_286_tss_caches_16bit_ring0_stack() {
    // LTR AX (0F 00 D8) with selector 0x0008 pointing to a 286 TSS.
    // 286 TSS layout stores SP0 at +0x02 and SS0 at +0x04.
    let (mut cpu, mut mem) = setup_pm_ltr(&[0x0F, 0x00, 0xD8]);
    cpu.regs.set_ax(0x0008);

    write_gdt_descriptor(
        &mut mem,
        0x1000,
        1,
        &[
            0x2B, 0x00, // limit low = 0x002B
            0x00, 0x30, // base low = 0x3000
            0x00, // base mid = 0x00
            0x81, // access: P=1, DPL=0, S=0, Type=1 (286 TSS available)
            0x00, // limit_high=0, flags=0
            0x00, // base high = 0x00
        ],
    );

    mem.write_u16(0x3002, 0x9000); // SP0
    mem.write_u16(0x3004, 0x0010); // SS0

    let _ = run(&mut cpu, &mut mem, &mut (), 1);

    assert_eq!(cpu.regs.tr, 0x0008);
    assert_eq!(cpu.regs.tss_base, 0x3000);
    assert_eq!(cpu.regs.tss_limit, 0x002B);
    assert_eq!(cpu.regs.tss_esp0, 0x9000);
    assert_eq!(cpu.regs.tss_ss0, 0x0010);
}

#[test]
fn test_ltr_granularity_bit_stores_effective_limit() {
    // TSS descriptor with granularity bit set:
    // limit=0x00010 (raw 20-bit), G=1 → effective_limit = (0x10 << 12) | 0xFFF = 0x10FFF
    let (mut cpu, mut mem) = setup_pm_ltr(&[0x0F, 0x00, 0xD8]); // LTR AX
    cpu.regs.set_ax(0x0008);

    // GDT[1]: base=0x3000, limit=0x00010, G=1
    // limit_low = 0x0010, limit_high = 0x0 (raw limit = 0x00010)
    // flags = 0x8 (G=1) → byte 6 = (limit_high << 0) | (flags << 4) = 0x80
    write_gdt_descriptor(
        &mut mem,
        0x1000,
        1,
        &[
            0x10, 0x00, // limit low = 0x0010
            0x00, 0x30, // base low = 0x3000
            0x00, // base mid = 0x00
            0x89, // access: P=1, DPL=0, S=0, Type=9
            0x80, // limit_high=0x0, flags=0x8 (G=1)
            0x00, // base high = 0x00
        ],
    );

    // Write TSS data (ESP0/SS0)
    mem.write_u32(0x3004, 0x9000);
    mem.write_u16(0x3008, 0x0010);

    let _ = run(&mut cpu, &mut mem, &mut (), 1);

    assert_eq!(cpu.regs.tr, 0x0008);
    assert_eq!(cpu.regs.tss_base, 0x3000);
    // effective_limit = (0x10 << 12) | 0xFFF = 0x10FFF
    assert_eq!(
        cpu.regs.tss_limit, 0x10FFF,
        "Granularity-bit TSS should store effective limit 0x10FFF"
    );
}

#[test]
fn test_ltr_large_tss_with_io_bitmap_granularity() {
    // TSS with I/O bitmap that exceeds 0xFFFF when granularity is applied.
    // Raw limit = 0x00020, G=1 → effective_limit = (0x20 << 12) | 0xFFF = 0x20FFF
    // This would have been truncated to 0x0FFF with the old u16 type.
    let (mut cpu, mut mem) = setup_pm_ltr(&[0x0F, 0x00, 0xD8]); // LTR AX
    cpu.regs.set_ax(0x0008);

    write_gdt_descriptor(
        &mut mem,
        0x1000,
        1,
        &[
            0x20, 0x00, // limit low = 0x0020
            0x00, 0x30, // base low = 0x3000
            0x00, // base mid = 0x00
            0x89, // access: P=1, DPL=0, S=0, Type=9
            0x80, // limit_high=0x0, flags=0x8 (G=1)
            0x00, // base high = 0x00
        ],
    );

    mem.write_u32(0x3004, 0x9000);
    mem.write_u16(0x3008, 0x0010);

    let _ = run(&mut cpu, &mut mem, &mut (), 1);

    assert_eq!(
        cpu.regs.tss_limit, 0x20FFF,
        "Large TSS with granularity should store full u32 effective limit"
    );
    // With the old u16 type, this would have been truncated to 0x0FFF
    assert!(
        cpu.regs.tss_limit > 0xFFFF,
        "Effective limit exceeds u16 range — proves u32 is required"
    );
}
