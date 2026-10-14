use super::*;

// ========================================================================
// BSWAP (0F C8-CF) — byte-swap 32-bit register
// ========================================================================

#[test]
fn test_bswap_eax() {
    // 0F C8 = BSWAP EAX (need 66 prefix in 16-bit mode for 32-bit reg)
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xC8]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax, 0x78563412);
}

#[test]
fn test_bswap_ecx() {
    // 0F C9 = BSWAP ECX
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xC9]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.ecx = 0xAABBCCDD;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx, 0xDDCCBBAA);
}

#[test]
fn test_bswap_zero() {
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xC8]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x00000000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax, 0x00000000);
}

#[test]
fn test_bswap_16bit_zeros_register() {
    // 16-bit BSWAP AX (0F C8, no 66 prefix) — real hardware zeros the register
    let (mut cpu, mut mem) = setup(&[0x0F, 0xC8]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    // 16-bit BSWAP zeros the 16-bit register, preserves high 16 bits
    assert_eq!(cpu.regs.eax & 0xFFFF, 0);
    assert_eq!(cpu.regs.eax >> 16, 0x1234);
}

#[test]
fn test_bswap_16bit_zeros_ecx() {
    // 16-bit BSWAP CX (0F C9, no 66 prefix)
    let (mut cpu, mut mem) = setup(&[0x0F, 0xC9]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.ecx = 0xAABBCCDD;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx & 0xFFFF, 0);
    assert_eq!(cpu.regs.ecx >> 16, 0xAABB);
}

#[test]
fn test_bswap_32bit_regression() {
    // Ensure 32-bit BSWAP still works after the 16-bit fix
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xC8]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax, 0xEFBEADDE);
}

// ========================================================================
// MOV CRn/DRn (S3) — must not raise #UD
// ========================================================================

#[test]
fn test_mov_cr0_eax_does_not_ud() {
    // 0F 22 C0 = MOV CR0, EAX
    let (mut cpu, mut mem) = setup(&[0x0F, 0x22, 0xC0]);
    cpu.regs.eax = cpu.regs.cr0; // write back same value
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // Should execute successfully, not raise #UD (vector 6)
    match result {
        RunResult::Exception { vector: 6, .. } => {
            panic!("MOV CR0, EAX raised #UD");
        }
        RunResult::BudgetExhausted { executed: 1 } => {} // success
        other => {
            // Other results (e.g. #GP in real mode) are acceptable —
            // the point is it must NOT be #UD
            assert!(
                !matches!(other, RunResult::Exception { vector: 6, .. }),
                "unexpected #UD: {:?}",
                other
            );
        }
    }
}

#[test]
fn test_mov_eax_cr0_does_not_ud() {
    // 0F 20 C0 = MOV EAX, CR0
    let (mut cpu, mut mem) = setup(&[0x0F, 0x20, 0xC0]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    if let RunResult::Exception { vector: 6, .. } = result {
        panic!("MOV EAX, CR0 raised #UD");
    }
}

// ========================================================================
// PUSH/POP FS (0F A0/A1) and PUSH/POP GS (0F A8/A9) (S3)
// ========================================================================

#[test]
fn test_push_fs_pop_fs_roundtrip() {
    // 0F A0 = PUSH FS; 0F A1 = POP FS
    let (mut cpu, mut mem) = setup(&[0x0F, 0xA0, 0x0F, 0xA1]);
    cpu.regs.fs = 0x1234;
    cpu.regs.set_sp(0xFFFE);
    cpu.regs.esp = 0xFFFE;
    cpu.regs.ss = 0x0000;
    let mut io = ();
    // Execute PUSH FS
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.sp(),
        0xFFFC,
        "SP should decrement by 2 for PUSH FS"
    );
    // Verify stack contains FS value
    let stk_lo = mem.ram[0xFFFC] as u16;
    let stk_hi = mem.ram[0xFFFD] as u16;
    assert_eq!(stk_lo | (stk_hi << 8), 0x1234);

    // Now change FS and execute POP FS to restore
    cpu.regs.fs = 0x0000;
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.sp(), 0xFFFE, "SP should restore after POP FS");
    assert_eq!(cpu.regs.fs, 0x1234, "POP FS should restore FS value");
}

#[test]
fn test_push_gs_pop_gs_roundtrip() {
    // 0F A8 = PUSH GS; 0F A9 = POP GS
    let (mut cpu, mut mem) = setup(&[0x0F, 0xA8, 0x0F, 0xA9]);
    cpu.regs.gs = 0x5678;
    cpu.regs.set_sp(0xFFFE);
    cpu.regs.esp = 0xFFFE;
    cpu.regs.ss = 0x0000;
    let mut io = ();
    // Execute PUSH GS
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.sp(),
        0xFFFC,
        "SP should decrement by 2 for PUSH GS"
    );
    let stk_lo = mem.ram[0xFFFC] as u16;
    let stk_hi = mem.ram[0xFFFD] as u16;
    assert_eq!(stk_lo | (stk_hi << 8), 0x5678);

    // Now change GS and execute POP GS to restore
    cpu.regs.gs = 0x0000;
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.sp(), 0xFFFE, "SP should restore after POP GS");
    assert_eq!(cpu.regs.gs, 0x5678, "POP GS should restore GS value");
}

#[test]
fn test_push_fs_32bit() {
    // 66 0F A0 = PUSH FS (32-bit operand size)
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xA0]);
    cpu.regs.fs = 0xABCD;
    cpu.regs.set_sp(0xFFFE);
    cpu.regs.esp = 0xFFFE;
    cpu.regs.ss = 0x0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.sp(),
        0xFFFA,
        "SP should decrement by 4 for 32-bit PUSH FS"
    );
    // Value pushed is zero-extended FS (0x0000ABCD)
    let val = u32::from_le_bytes([
        mem.ram[0xFFFA],
        mem.ram[0xFFFB],
        mem.ram[0xFFFC],
        mem.ram[0xFFFD],
    ]);
    assert_eq!(val, 0x0000ABCD);
}

// ========================================================================
// FPU error downcast — CpuError variant mapping (M7)
// ========================================================================

#[test]
fn test_fpu_gpf_maps_to_vector_13() {
    // FLD dword [0x0100] in 16-bit PM with DS limit=0x00FF
    // Address 0x0100 + 4 bytes exceeds segment limit → #GP(0)
    // D9 06 00 01 = FLD dword ptr [0x0100]
    let (mut cpu, mut mem) = setup(&[0xD9, 0x06, 0x00, 0x01]);
    cpu.regs.cr0 |= 1; // Protected mode
                       // Use distinct selectors so get_segment_cache returns the right cache
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFF;
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0x00FF; // DS limit too small for [0x0100]
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ss = 0x18;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.cpl = 0;
    cpu.cs_bits = 16;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("expected #GP (vector 13), got {:?}", other),
    }
}

// ========================================================================
// CMPXCHG (0F B0/B1)
// ========================================================================

#[test]
fn test_cmpxchg_byte_equal() {
    // CMPXCHG CL, DL: AL == CL -> CL = DL, ZF=1
    // 0F B0 D1 = CMPXCHG CL, DL (modrm: reg=DL(2), rm=CL(1))
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB0, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x42; // AL = 0x42
    cpu.regs.ecx = 0x42; // CL = 0x42 (matches AL)
    cpu.regs.edx = 0x99; // DL = 0x99 (source)
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u8, 0x99); // CL = DL
    assert_eq!(cpu.regs.eax as u8, 0x42); // AL unchanged
    assert!(cpu.regs.flags.contains(Flags::ZERO)); // ZF=1 (equal)
}

#[test]
fn test_cmpxchg_byte_not_equal() {
    // CMPXCHG CL, DL: AL != CL -> AL = CL, ZF=0
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB0, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x42; // AL = 0x42
    cpu.regs.ecx = 0x55; // CL = 0x55 (does NOT match AL)
    cpu.regs.edx = 0x99; // DL = 0x99
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u8, 0x55); // CL unchanged
    assert_eq!(cpu.regs.eax as u8, 0x55); // AL = old CL
    assert!(!cpu.regs.flags.contains(Flags::ZERO)); // ZF=0 (not equal)
}

#[test]
fn test_cmpxchg_word_equal() {
    // 0F B1 D1 = CMPXCHG CX, DX (16-bit)
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB1, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x1234; // AX = 0x1234
    cpu.regs.ecx = 0x1234; // CX matches AX
    cpu.regs.edx = 0xBEEF; // DX = source
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u16, 0xBEEF); // CX = DX
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpxchg_word_not_equal() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB1, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x1234;
    cpu.regs.ecx = 0x5678; // CX != AX
    cpu.regs.edx = 0xBEEF;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u16, 0x5678); // CX unchanged
    assert_eq!(cpu.regs.eax as u16, 0x5678); // AX = old CX
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpxchg_dword_equal() {
    // 66 0F B1 D1 = CMPXCHG ECX, EDX (32-bit)
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xB1, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x12345678;
    cpu.regs.ecx = 0x12345678;
    cpu.regs.edx = 0xDEADBEEF;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx, 0xDEADBEEF);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpxchg_dword_not_equal() {
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0xB1, 0xD1]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x12345678;
    cpu.regs.ecx = 0xAAAAAAAA;
    cpu.regs.edx = 0xDEADBEEF;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx, 0xAAAAAAAA); // unchanged
    assert_eq!(cpu.regs.eax, 0xAAAAAAAA); // EAX = old ECX
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_cmpxchg_byte_memory() {
    // CMPXCHG [BX], DL — memory operand
    // 0F B0 17 = CMPXCHG [BX], DL (modrm=0x17: reg=DL(2), rm=[BX](7))
    let (mut cpu, mut mem) = setup(&[0x0F, 0xB0, 0x17]);
    cpu.cpu_type = gp64_cpu::CpuType::I80486;
    cpu.regs.eax = 0x42;
    cpu.regs.ebx = 0x0200;
    cpu.regs.edx = 0x99;
    mem.ram[0x0200] = 0x42; // [BX] == AL
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.ram[0x0200], 0x99); // memory = DL
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// AAM (0xD4) — BCD adjust after multiply
// ========================================================================

#[test]
fn test_aam_basic() {
    // AAM 0x0A: AL=35 -> AH=3, AL=5
    let (mut cpu, mut mem) = setup(&[0xD4, 0x0A]);
    cpu.regs.eax = 35;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 5); // AL
    assert_eq!((cpu.regs.eax >> 8) as u8, 3); // AH
}

#[test]
fn test_aam_zero() {
    // AAM 0x0A with AL=0
    let (mut cpu, mut mem) = setup(&[0xD4, 0x0A]);
    cpu.regs.eax = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 0);
    assert_eq!((cpu.regs.eax >> 8) as u8, 0);
    assert!(cpu.regs.flags.contains(Flags::ZERO)); // ZF from AL=0
}

#[test]
fn test_aam_divide_by_zero() {
    // AAM 0x00 should trigger #DE (INT 0)
    let (mut cpu, mut mem) = setup(&[0xD4, 0x00]);
    cpu.regs.eax = 42;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 0,
            error_code: 0,
            ..
        }
    ));
}

#[test]
fn test_aam_non_standard_base() {
    // AAM with base 16: AL=0x2B (43) -> AH=2, AL=11
    let (mut cpu, mut mem) = setup(&[0xD4, 0x10]);
    cpu.regs.eax = 43;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 43 % 16); // AL = 11
    assert_eq!((cpu.regs.eax >> 8) as u8, 43 / 16); // AH = 2
}

// ========================================================================
// AAD (0xD5) — BCD adjust before divide
// ========================================================================

#[test]
fn test_aad_basic() {
    // AAD 0x0A: AH=3, AL=5 -> AL=35, AH=0
    let (mut cpu, mut mem) = setup(&[0xD5, 0x0A]);
    cpu.regs.eax = 0x0305; // AH=3, AL=5
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 35); // AL = 3*10+5
    assert_eq!((cpu.regs.eax >> 8) as u8, 0); // AH = 0
}

#[test]
fn test_aad_zero() {
    let (mut cpu, mut mem) = setup(&[0xD5, 0x0A]);
    cpu.regs.eax = 0x0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_aad_truncates_to_byte() {
    // AAD with overflow: AH=0xFF, AL=0xFF, base=0x0A -> 255*10+255 = 2805 -> truncate to u8
    let (mut cpu, mut mem) = setup(&[0xD5, 0x0A]);
    cpu.regs.eax = 0xFF_FF;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    let expected = ((0xFF_u16 * 10 + 0xFF_u16) & 0xFF) as u8;
    assert_eq!(cpu.regs.eax as u8, expected);
    assert_eq!((cpu.regs.eax >> 8) as u8, 0);
}

// ========================================================================
// SALC (0xD6) — set AL from CF
// ========================================================================

#[test]
fn test_salc_cf_set() {
    // STC; SALC -> AL = 0xFF
    let (mut cpu, mut mem) = setup(&[0xF9, 0xD6]); // STC; SALC
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 2 }));
    assert_eq!(cpu.regs.eax as u8, 0xFF);
}

#[test]
fn test_salc_cf_clear() {
    // CLC; SALC -> AL = 0x00
    let (mut cpu, mut mem) = setup(&[0xF8, 0xD6]); // CLC; SALC
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 2 }));
    assert_eq!(cpu.regs.eax as u8, 0x00);
}

#[test]
fn test_salc_preserves_ah() {
    let (mut cpu, mut mem) = setup(&[0xF9, 0xD6]); // STC; SALC
    cpu.regs.eax = 0x1234_AB00;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 2 }));
    assert_eq!(cpu.regs.eax, 0x1234_ABFF); // only AL changed
}

// ========================================================================
// FWAIT (0x9B) — NOP
// ========================================================================

#[test]
fn test_fwait_nop() {
    let (mut cpu, mut mem) = setup(&[0x9B]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x0101);
}

// ========================================================================
// BOUND (0x62) — check array index against bounds
// ========================================================================

#[test]
fn test_bound_in_range() {
    // BOUND AX, [0x0200] — AX within bounds -> no exception
    // 62 06 00 02 = BOUND AX, [0x0200] (modrm=0x06: reg=AX(0), rm=[disp16])
    let (mut cpu, mut mem) = setup(&[0x62, 0x06, 0x00, 0x02]);
    cpu.regs.eax = 5;
    // lower bound = 0, upper bound = 10 at DS:0200
    let ds_base = (cpu.regs.ds as u32) << 4;
    let addr = (ds_base + 0x0200) as usize;
    mem.ram[addr] = 0;
    mem.ram[addr + 1] = 0; // lower = 0
    mem.ram[addr + 2] = 10;
    mem.ram[addr + 3] = 0; // upper = 10
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
}

#[test]
fn test_bound_below_lower() {
    let (mut cpu, mut mem) = setup(&[0x62, 0x06, 0x00, 0x02]);
    cpu.regs.eax = 0xFFFF; // -1 as i16
    let ds_base = (cpu.regs.ds as u32) << 4;
    let addr = (ds_base + 0x0200) as usize;
    mem.ram[addr] = 0;
    mem.ram[addr + 1] = 0;
    mem.ram[addr + 2] = 10;
    mem.ram[addr + 3] = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 5,
            error_code: 0,
            ..
        }
    ));
}

#[test]
fn test_bound_above_upper() {
    let (mut cpu, mut mem) = setup(&[0x62, 0x06, 0x00, 0x02]);
    cpu.regs.eax = 11; // above upper bound of 10
    let ds_base = (cpu.regs.ds as u32) << 4;
    let addr = (ds_base + 0x0200) as usize;
    mem.ram[addr] = 0;
    mem.ram[addr + 1] = 0;
    mem.ram[addr + 2] = 10;
    mem.ram[addr + 3] = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 5,
            error_code: 0,
            ..
        }
    ));
}

#[test]
fn test_bound_at_upper_edge() {
    // Index == upper bound is OK (inclusive)
    let (mut cpu, mut mem) = setup(&[0x62, 0x06, 0x00, 0x02]);
    cpu.regs.eax = 10;
    let ds_base = (cpu.regs.ds as u32) << 4;
    let addr = (ds_base + 0x0200) as usize;
    mem.ram[addr] = 0;
    mem.ram[addr + 1] = 0;
    mem.ram[addr + 2] = 10;
    mem.ram[addr + 3] = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
}

#[test]
fn test_bound_register_operand_is_ud() {
    // BOUND with register operand (modrm >= 0xC0) is #UD
    let (mut cpu, mut mem) = setup(&[0x62, 0xC0]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            ..
        }
    ));
}

// ========================================================================
// POP r/m16/32 (0x8F)
// ========================================================================

#[test]
fn test_pop_rm16_register() {
    // POP CX via ModR/M: 8F C1 (modrm=0xC1: rm=CX(1))
    let (mut cpu, mut mem) = setup(&[0x8F, 0xC1]);
    cpu.regs.esp = 0xFFF0;
    cpu.regs.set_sp(0xFFF0);
    // Push 0xBEEF onto stack
    let ss_base = (cpu.regs.ss as u32) << 4;
    let addr = (ss_base + 0xFFF0) as usize;
    mem.ram[addr] = 0xEF;
    mem.ram[addr + 1] = 0xBE;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx as u16, 0xBEEF);
    assert_eq!(cpu.regs.sp(), 0xFFF2);
}

#[test]
fn test_pop_rm16_memory() {
    // POP [BX]: 8F 07 (modrm=0x07: rm=[BX])
    let (mut cpu, mut mem) = setup(&[0x8F, 0x07]);
    cpu.regs.esp = 0xFFF0;
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ebx = 0x0300;
    // Push 0x1234 onto stack
    let ss_base = (cpu.regs.ss as u32) << 4;
    let addr = (ss_base + 0xFFF0) as usize;
    mem.ram[addr] = 0x34;
    mem.ram[addr + 1] = 0x12;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    let ds_base = (cpu.regs.ds as u32) << 4;
    let dest = (ds_base + 0x0300) as usize;
    assert_eq!(mem.ram[dest], 0x34);
    assert_eq!(mem.ram[dest + 1], 0x12);
}

#[test]
fn test_pop_rm32_register() {
    // 66 8F C1 = POP ECX (32-bit in 16-bit mode)
    let (mut cpu, mut mem) = setup(&[0x66, 0x8F, 0xC1]);
    cpu.regs.esp = 0xFFF0;
    cpu.regs.set_sp(0xFFF0);
    let ss_base = (cpu.regs.ss as u32) << 4;
    let addr = (ss_base + 0xFFF0) as usize;
    mem.ram[addr] = 0x78;
    mem.ram[addr + 1] = 0x56;
    mem.ram[addr + 2] = 0x34;
    mem.ram[addr + 3] = 0x12;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx, 0x12345678);
    assert_eq!(cpu.regs.sp(), 0xFFF4);
}

#[test]
fn test_pop_rm16_invalid_extension_register_is_ud_without_state_change() {
    // 8F /1 is reserved; C9 would target CX if it were incorrectly treated as /0.
    let (mut cpu, mut mem) = setup(&[0x8F, 0xC9]);
    cpu.regs.esp = 0xFFF0;
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ecx = 0xCAFE_BABE;
    let ss_base = (cpu.regs.ss as u32) << 4;
    let stack = (ss_base + 0xFFF0) as usize;
    mem.ram[stack..stack + 2].copy_from_slice(&0x1234u16.to_le_bytes());

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "reserved 8F /1 register form must raise #UD, got {result:?}"
    );
    assert_eq!(cpu.regs.eip, 0x0100, "#UD must restart at opcode 8F");
    assert_eq!(cpu.regs.sp(), 0xFFF0, "#UD must not pop the stack");
    assert_eq!(cpu.regs.ecx, 0xCAFE_BABE, "#UD must not write CX");
}

#[test]
fn test_pop_rm16_invalid_extension_memory_is_ud_without_state_change() {
    // 8F /1 with mod=00, r/m=111 would target [BX] if decoded as POP.
    let (mut cpu, mut mem) = setup(&[0x8F, 0x0F]);
    cpu.regs.esp = 0xFFF0;
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ebx = 0x0300;
    let ss_base = (cpu.regs.ss as u32) << 4;
    let stack = (ss_base + 0xFFF0) as usize;
    mem.ram[stack..stack + 2].copy_from_slice(&0x1234u16.to_le_bytes());
    let ds_base = (cpu.regs.ds as u32) << 4;
    let dest = (ds_base + 0x0300) as usize;
    mem.ram[dest..dest + 2].copy_from_slice(&0xBEEFu16.to_le_bytes());

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "reserved 8F /1 memory form must raise #UD, got {result:?}"
    );
    assert_eq!(cpu.regs.eip, 0x0100, "#UD must restart at opcode 8F");
    assert_eq!(cpu.regs.sp(), 0xFFF0, "#UD must not pop the stack");
    assert_eq!(
        u16::from_le_bytes(mem.ram[dest..dest + 2].try_into().unwrap()),
        0xBEEF,
        "#UD must not write the memory destination"
    );
}

// ========================================================================
// F6/F7 sub_op 1 — alias for TEST
// ========================================================================

#[test]
fn test_f6_subop1_test_rm8_imm8() {
    // F6 /1 = TEST r/m8, imm8 (alias)
    // F6 C9 42 = TEST CL, 0x42 (modrm=0xC9: sub_op=1, rm=CL)
    let (mut cpu, mut mem) = setup(&[0xF6, 0xC9, 0x42]);
    cpu.regs.ecx = 0x42;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(!cpu.regs.flags.contains(Flags::ZERO)); // 0x42 & 0x42 = 0x42 != 0
}

#[test]
fn test_f6_subop1_zero_result() {
    // TEST CL, 0x00: 0x42 & 0x00 = 0 -> ZF=1
    let (mut cpu, mut mem) = setup(&[0xF6, 0xC9, 0x00]);
    cpu.regs.ecx = 0x42;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_f7_subop1_test_rm16_imm16() {
    // F7 /1 = TEST r/m16, imm16 (alias)
    // F7 C9 34 12 = TEST CX, 0x1234 (modrm=0xC9: sub_op=1, rm=CX)
    let (mut cpu, mut mem) = setup(&[0xF7, 0xC9, 0x34, 0x12]);
    cpu.regs.ecx = 0x1234;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_f7_subop1_test_rm32_imm32() {
    // 66 F7 C9 78 56 34 12 = TEST ECX, 0x12345678
    let (mut cpu, mut mem) = setup(&[0x66, 0xF7, 0xC9, 0x78, 0x56, 0x34, 0x12]);
    cpu.regs.ecx = 0x12345678;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// ENTER level>0 (0xC8) — nested stack frames
// ========================================================================

#[test]
fn test_enter_level1_16bit() {
    // ENTER 4, 1: push BP, frame=SP, push frame, set BP=frame, SP -= 4
    // C8 04 00 01
    let (mut cpu, mut mem) = setup(&[0xC8, 0x04, 0x00, 0x01]);
    cpu.regs.esp = 0x1000;
    cpu.regs.set_sp(0x1000);
    cpu.regs.ebp = 0x0F00;
    cpu.regs.ss = 0x0000;
    cpu.regs.ss_cache = SegmentCache::real_mode(0);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    // After push BP (0x0F00): SP=0x0FFE
    // frame = 0x0FFE
    // level=1: no frame pointer copies (level-1=0 iterations), push frame (0x0FFE): SP=0x0FFC
    // BP = frame = 0x0FFE
    // SP -= 4 = 0x0FF8
    assert_eq!(cpu.regs.bp(), 0x0FFE);
    assert_eq!(cpu.regs.sp(), 0x0FF8);
}

#[test]
fn test_enter_level2_16bit() {
    // ENTER 0, 2: push BP, frame=SP, copy 1 frame ptr from [old_bp-2], push frame, BP=frame
    let (mut cpu, mut mem) = setup(&[0xC8, 0x00, 0x00, 0x02]);
    cpu.regs.esp = 0x1000;
    cpu.regs.set_sp(0x1000);
    cpu.regs.ebp = 0x0F00;
    cpu.regs.ss = 0x0000;
    cpu.regs.ss_cache = SegmentCache::real_mode(0);
    // Place a frame pointer at [BP-2] = [0x0EFE]
    mem.ram[0x0EFE] = 0xAA;
    mem.ram[0x0EFF] = 0xBB;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    // After push BP (0x0F00): SP=0x0FFE
    // frame = 0x0FFE
    // level=2: copy 1 frame ptr from [old_bp-2] = [0x0EFE] = 0xBBAA -> push 0xBBAA: SP=0x0FFC
    // push frame (0x0FFE): SP=0x0FFA
    // BP = frame = 0x0FFE
    // SP -= 0 = 0x0FFA
    assert_eq!(cpu.regs.bp(), 0x0FFE);
    assert_eq!(cpu.regs.sp(), 0x0FFA);
    // Check the pushed frame pointer on the stack
    assert_eq!(mem.ram[0x0FFC], 0xAA);
    assert_eq!(mem.ram[0x0FFD], 0xBB);
}

// ========================================================================
// POP SS / MOV SS interrupt_inhibit (bug fixes)
// ========================================================================

#[test]
fn test_pop_ss_sets_interrupt_inhibit() {
    // POP SS = 0x17
    let (mut cpu, mut mem) = setup(&[0x17]);
    cpu.regs.esp = 0xFFF0;
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ss = 0x0000;
    cpu.regs.ss_cache = SegmentCache::real_mode(0);
    // Place value 0x2000 on the stack at SS:SP
    mem.ram[0xFFF0] = 0x00;
    mem.ram[0xFFF1] = 0x20;
    cpu.interrupt_inhibit = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::InterruptPollBoundary { executed: 1 }
    ));
    assert_eq!(cpu.regs.ss, 0x2000);
    assert_eq!(cpu.interrupt_inhibit, 1);
}

#[test]
fn test_mov_ss_sets_interrupt_inhibit() {
    // MOV SS, AX = 8E D0 (modrm=0xD0: seg_idx=SS(2), rm=AX(0))
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD0]);
    cpu.regs.eax = 0x3000;
    cpu.interrupt_inhibit = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::InterruptPollBoundary { executed: 1 }
    ));
    assert_eq!(cpu.regs.ss, 0x3000);
    assert_eq!(cpu.interrupt_inhibit, 1);
}

// ========================================================================
// POP ES/DS/SS segment cache update (bug fix)
// ========================================================================

#[test]
fn test_pop_es_updates_cache() {
    // POP ES = 0x07
    let (mut cpu, mut mem) = setup(&[0x07]);
    cpu.regs.esp = 0xFFF0;
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ss = 0x0000;
    cpu.regs.ss_cache = SegmentCache::real_mode(0);
    mem.ram[0xFFF0] = 0x00;
    mem.ram[0xFFF1] = 0x40; // ES = 0x4000
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.es, 0x4000);
    assert_eq!(cpu.regs.es_cache.base, 0x4000 << 4);
}

#[test]
fn test_pop_ds_updates_cache() {
    // POP DS = 0x1F
    let (mut cpu, mut mem) = setup(&[0x1F]);
    cpu.regs.esp = 0xFFF0;
    cpu.regs.set_sp(0xFFF0);
    cpu.regs.ss = 0x0000;
    cpu.regs.ss_cache = SegmentCache::real_mode(0);
    mem.ram[0xFFF0] = 0x00;
    mem.ram[0xFFF1] = 0x50; // DS = 0x5000
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ds, 0x5000);
    assert_eq!(cpu.regs.ds_cache.base, 0x5000 << 4);
}

#[test]
fn test_mov_seg_updates_cache() {
    // MOV DS, AX = 8E D8 (seg_idx=DS(3), rm=AX(0))
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD8]);
    cpu.regs.eax = 0x6000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ds, 0x6000);
    assert_eq!(cpu.regs.ds_cache.base, 0x6000 << 4);
}

#[test]
fn test_mov_fs_updates_cache() {
    // MOV FS, AX = 8E E0 (seg_idx=FS(4), rm=AX(0))
    let (mut cpu, mut mem) = setup(&[0x8E, 0xE0]);
    cpu.regs.eax = 0x7000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.fs, 0x7000);
    assert_eq!(cpu.regs.fs_cache.base, 0x7000 << 4);
}

#[test]
fn test_mov_gs_updates_cache() {
    // MOV GS, AX = 8E E8 (seg_idx=GS(5), rm=AX(0))
    let (mut cpu, mut mem) = setup(&[0x8E, 0xE8]);
    cpu.regs.eax = 0x8000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.gs, 0x8000);
    assert_eq!(cpu.regs.gs_cache.base, 0x8000 << 4);
}

#[test]
fn test_les_updates_segment_cache() {
    // LES AX, [BX] — 0xC4 0x07 (modrm: mod=00, reg=0(AX), rm=7(BX))
    let (mut cpu, mut mem) = setup(&[0xC4, 0x07]);
    cpu.regs.ebx = 0x200;
    // Write far pointer: offset=0x1234, segment=0x5678 at address 0x200
    mem.ram[0x200] = 0x34;
    mem.ram[0x201] = 0x12;
    mem.ram[0x202] = 0x78;
    mem.ram[0x203] = 0x56;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0x1234);
    assert_eq!(cpu.regs.es, 0x5678);
    // Verify segment cache was updated
    assert_eq!(cpu.regs.es_cache.base, (0x5678u32) << 4);
}

#[test]
fn test_lds_updates_segment_cache() {
    // LDS AX, [BX] — 0xC5 0x07 (modrm: mod=00, reg=0(AX), rm=7(BX))
    let (mut cpu, mut mem) = setup(&[0xC5, 0x07]);
    cpu.regs.ebx = 0x200;
    mem.ram[0x200] = 0x34;
    mem.ram[0x201] = 0x12;
    mem.ram[0x202] = 0x78;
    mem.ram[0x203] = 0x56;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ax(), 0x1234);
    assert_eq!(cpu.regs.ds, 0x5678);
    assert_eq!(cpu.regs.ds_cache.base, (0x5678u32) << 4);
}
