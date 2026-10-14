use super::*;

// ========================================================================
// Debug Registers (DR0-DR7)
// ========================================================================

#[test]
fn test_read_dr0_default_zero() {
    // MOV EAX, DR0: 0F 21 C0 (modrm: mod=11, reg=DR0(000), rm=EAX(000))
    let (mut cpu, mut mem) = setup(&[0x0F, 0x21, 0xC0]);
    cpu.regs.eax = 0xDEAD_BEEF; // pre-fill to confirm it gets overwritten
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax, 0, "DR0 should default to 0");
}

#[test]
fn test_write_then_read_dr0() {
    // MOV DR0, EAX: 0F 23 C0 (write), then MOV ECX, DR0: 0F 21 C1 (read into ECX)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x23, 0xC0, 0x0F, 0x21, 0xC1]);
    cpu.regs.eax = 0x0040_1000; // breakpoint address
    cpu.regs.ecx = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 2 }));
    assert_eq!(cpu.regs.dr0, 0x0040_1000);
    assert_eq!(cpu.regs.ecx, 0x0040_1000, "DR0 read-back should match");
}

#[test]
fn test_read_dr6_default() {
    // MOV EAX, DR6: 0F 21 F0 (modrm: mod=11, reg=DR6(110), rm=EAX(000))
    let (mut cpu, mut mem) = setup(&[0x0F, 0x21, 0xF0]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.eax, 0xFFFF_0FF0,
        "DR6 default should be 0xFFFF0FF0"
    );
}

#[test]
fn test_read_dr7_default() {
    // MOV EAX, DR7: 0F 21 F8 (modrm: mod=11, reg=DR7(111), rm=EAX(000))
    let (mut cpu, mut mem) = setup(&[0x0F, 0x21, 0xF8]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.eax, 0x0000_0400,
        "DR7 default should be 0x00000400"
    );
}

#[test]
fn test_dr4_aliases_dr6() {
    // DR4 aliases DR6 (when CR4.DE=0). Write DR6, read DR4.
    // MOV DR6, EAX: 0F 23 F0, then MOV ECX, DR4: 0F 21 E1
    let (mut cpu, mut mem) = setup(&[0x0F, 0x23, 0xF0, 0x0F, 0x21, 0xE1]);
    cpu.regs.eax = 0x1234_5678;
    cpu.regs.ecx = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 2 }));
    assert_eq!(cpu.regs.ecx, 0x1234_5678, "DR4 should alias DR6");
}

// ========================================================================
// XCHG
// ========================================================================

#[test]
fn test_xchg_ax_cx() {
    // XCHG AX, CX (0x91)
    let (mut cpu, mut mem) = setup(&[0x91]);
    cpu.regs.eax = 0x1111;
    cpu.regs.ecx = 0x2222;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x2222);
    assert_eq!(cpu.regs.ecx as u16, 0x1111);
}

// ========================================================================
// sync_on_exit
// ========================================================================

#[test]
fn test_sync_on_exit_enables_interrupts() {
    let mut cpu = Cpu::new();
    cpu.regs.flags.insert(Flags::INTERRUPT);
    cpu.interrupts_enabled = false;

    sync_on_exit(&mut cpu);

    assert!(cpu.interrupts_enabled);
}

#[test]
fn test_sync_on_exit_disables_interrupts() {
    let mut cpu = Cpu::new();
    cpu.regs.flags.remove(Flags::INTERRUPT);
    cpu.interrupts_enabled = true;

    sync_on_exit(&mut cpu);

    assert!(!cpu.interrupts_enabled);
}

// ========================================================================
// step_one
// ========================================================================

#[test]
fn test_step_one_calls_run() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0200);
    cpu.regs.eip = 0x0200;
    mem.ram[0x0200] = 0xCD;
    mem.ram[0x0201] = 0x20;

    let result = step_one(&mut cpu, &mut mem, &mut io);
    assert!(matches!(
        result,
        RunResult::SoftwareInterrupt {
            vector: 0x20,
            executed: 1
        }
    ));
}

// ========================================================================
// FPU delegation
// ========================================================================

#[test]
fn test_fpu_executes() {
    // FADD ST(0), ST(0) — 0xD8 0xC0
    let (mut cpu, mut mem) = setup(&[0xD8, 0xC0]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "Expected FPU to execute successfully, got: {:?}",
        result
    );
}

#[test]
fn test_fpu_fld_fstp() {
    // FLD dword [0x200] = D9 06 00 02 (modrm: [disp16])
    // FSTP dword [0x210] = D9 1E 10 02
    let (mut cpu, mut mem) = setup(&[0xD9, 0x06, 0x00, 0x02, 0xD9, 0x1E, 0x10, 0x02]);
    // Write a float value at address 0x200
    let val: f32 = 3.14;
    let bytes = val.to_le_bytes();
    mem.ram[0x200..0x204].copy_from_slice(&bytes);
    let mut io = ();
    // Execute FLD
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "FLD failed: {:?}",
        result
    );
    // Execute FSTP
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "FSTP failed: {:?}",
        result
    );
    // Verify the value was stored at 0x210
    let stored = f32::from_le_bytes([
        mem.ram[0x210],
        mem.ram[0x211],
        mem.ram[0x212],
        mem.ram[0x213],
    ]);
    assert!((stored - 3.14).abs() < 0.001);
}

#[test]
fn fisttp_is_ud_before_state_mutation_on_every_cpu_type() {
    // FISTTP dword [0x0200] = DB /1, mod=00 r/m=110 disp16.
    const FISTTP_M32: [u8; 4] = [0xDB, 0x0E, 0x00, 0x02];
    const SENTINEL: u32 = 0xA5A5_5A5A;
    // Driven off `CpuType::ALL` rather than a hand-written list: as a
    // literal this had 7 entries and silently omitted PentiumMmx, and it
    // would have gone on omitting every identity added after it. FISTTP is
    // SSE3, so "every CPU that must #UD it" is exactly the non-SSE3 set.
    let cpu_types: Vec<_> = gp64_cpu::CpuType::ALL
        .into_iter()
        .filter(|t| !t.supports_sse3())
        .collect();
    assert!(
        cpu_types.len() >= 8,
        "expected every modeled identity to predate SSE3"
    );

    for cpu_type in cpu_types {
        let (mut cpu, mut mem) = setup(&FISTTP_M32);
        cpu.set_cpu_type(cpu_type);
        cpu.fpu.push(gp64_cpu::x87_ops::F80::from_f64(12.75));
        mem.ram[0x0200..0x0204].copy_from_slice(&SENTINEL.to_le_bytes());
        let fpu_before = cpu.fpu.clone();

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 6,
                    error_code: 0,
                    fault_addr: None,
                    executed: 0,
                }
            ),
            "{cpu_type:?} must reject SSE3 FISTTP with #UD, got {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x0100, "{cpu_type:?} advanced EIP");
        assert_eq!(
            &mem.ram[0x0200..0x0204],
            &SENTINEL.to_le_bytes(),
            "{cpu_type:?} wrote the FISTTP destination before #UD"
        );
        assert_eq!(cpu.fpu.regs, fpu_before.regs, "{cpu_type:?} changed ST");
        assert_eq!(
            cpu.fpu.status_word, fpu_before.status_word,
            "{cpu_type:?} changed the x87 status word"
        );
        assert_eq!(
            cpu.fpu.tag_word, fpu_before.tag_word,
            "{cpu_type:?} changed the x87 tag word"
        );
        assert_eq!(
            cpu.fpu.x87_save_area, fpu_before.x87_save_area,
            "{cpu_type:?} changed the native x87 save image"
        );
    }
}

#[test]
fn fscale_real_mode_extended_exponent_truncation() {
    use gp64_cpu::x87_ops::F80;

    fn raw(negative: bool, exponent: u16, significand: u64) -> F80 {
        let mut bytes = [0; 10];
        bytes[..8].copy_from_slice(&significand.to_le_bytes());
        bytes[8..].copy_from_slice(&(exponent | (u16::from(negative) << 15)).to_le_bytes());
        F80(bytes)
    }

    for rc in 0..4 {
        let cw = 0x037f | (rc << 10);
        let mut cases = Vec::new();
        for negative in [false, true] {
            for (exp, sig, magnitude) in [
                (0x3ffe, u64::MAX, 0),
                (0x3fff, 0x8000_0000_0000_0000, 1),
                (0x3fff, 0x8000_0000_0000_0001, 1),
                (0x3fff, u64::MAX, 1),
                (0x4000, 0x8000_0000_0000_0000, 2),
                (0x4000, 0x8000_0000_0000_0001, 2),
            ] {
                let n = if negative { -magnitude } else { magnitude };
                cases.push((
                    F80::ONE,
                    raw(negative, exp, sig),
                    raw(false, (0x3fff + n) as u16, 1 << 63),
                ));
            }
        }
        for exp in [0x401e, 0x7ffe] {
            for negative in [false, true] {
                let a = raw(negative, 0x3fff, 1 << 63);
                let rounds_outward = (rc == 1 && negative) || (rc == 2 && !negative);
                let overflow = if rc == 0 || rounds_outward {
                    raw(negative, 0x7fff, 1 << 63)
                } else {
                    raw(negative, 0x7ffe, u64::MAX)
                };
                cases.push((a, raw(false, exp, 1 << 63), overflow));
                cases.push((
                    a,
                    raw(true, exp, 1 << 63),
                    raw(negative, 0, u64::from(rounds_outward)),
                ));
            }
        }
        let inf = raw(false, 0x7fff, 1 << 63);
        let neg_inf = raw(true, 0x7fff, 1 << 63);
        let snan = raw(false, 0x7fff, (1 << 63) | 1);
        let qnan = raw(false, 0x7fff, (3 << 62) | 1);
        let unsupported = raw(false, 0x3fff, 1);
        cases.extend([
            (F80::ONE, F80::ZERO, F80::ONE),
            (F80::ONE, F80::NEG_ZERO, F80::ONE),
            (F80::ONE, raw(false, 0, 1), F80::ONE),
            (F80::ONE, raw(true, 0, 1), F80::ONE),
            (F80::NEG_ZERO, F80::ONE, F80::NEG_ZERO),
            (inf, F80::ONE, inf),
            (F80::ONE, inf, inf),
            (raw(true, 0x3fff, 1 << 63), neg_inf, F80::NEG_ZERO),
            (F80::ZERO, inf, F80::INDEFINITE),
            (inf, neg_inf, F80::INDEFINITE),
            (F80::ONE, snan, qnan),
            (snan, F80::ONE, qnan),
            (F80::ONE, unsupported, F80::INDEFINITE),
            (unsupported, F80::ONE, F80::INDEFINITE),
        ]);

        for (a, b, expected) in cases {
            // Synthetic real-mode guest: FLD tbyte ST1; FLD tbyte ST0;
            // FSCALE; FSTP tbyte result. All operands live in test RAM.
            let (mut cpu, mut mem) = setup(&[
                0xdb, 0x2e, 0x00, 0x02, 0xdb, 0x2e, 0x10, 0x02, 0xd9, 0xfd, 0xdb, 0x3e, 0x20, 0x02,
            ]);
            cpu.set_cpu_type(gp64_cpu::CpuType::I80386);
            cpu.fpu.control_word = cw;
            mem.ram[0x200..0x20a].copy_from_slice(&b.0);
            mem.ram[0x210..0x21a].copy_from_slice(&a.0);
            let result = run(&mut cpu, &mut mem, &mut (), 4);
            assert!(
                matches!(result, RunResult::BudgetExhausted { executed: 4 }),
                "{result:?}"
            );
            assert_eq!(&mem.ram[0x220..0x22a], &expected.0,
                "real-mode FSCALE must truncate extended ST1 before narrowing: a={a:?}, b={b:?}, CW={cw:04x}");
            // FLD m80 loads raw bits; FSCALE leaves ST1 untouched, including
            // its signaling NaN or unsupported encoding.
            assert_eq!(cpu.fpu.get_st(0).0, b.0, "FSCALE changed ST1");
        }
    }
}

#[test]
fn test_fpu_all_escape_bytes() {
    // Verify every x87 escape byte has an executable pre-P6 form. DA/DB
    // C0 are P6-only FCMOVcc encodings, so use 387-era FUCOMPP/FNCLEX
    // forms for those escape bytes.
    for (escape, modrm) in [
        (0xD8, 0xC0),
        (0xD9, 0xC0),
        (0xDA, 0xE9),
        (0xDB, 0xE2),
        (0xDC, 0xC0),
        (0xDD, 0xC0),
        (0xDE, 0xC0),
        (0xDF, 0xC0),
    ] {
        let (mut cpu, mut mem) = setup(&[escape, modrm]);
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::BudgetExhausted { .. }),
            "Escape byte 0x{:02X} didn't execute successfully: {:?}",
            escape,
            result
        );
    }
}

// ========================================================================
// Unhandled opcodes raise #UD
// ========================================================================

#[test]
fn test_unhandled_opcode_raises_ud() {
    // ARPL (0x63) is not implemented — should raise #UD (vector 6)
    let (mut cpu, mut mem) = setup(&[0x63, 0xC0]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Exception { vector: 6, .. }));
}

#[test]
fn near_jcc_rejects_pre_386_cpus_before_reading_displacement() {
    // 8086/8088 are deliberately absent: there `0F` is the one-byte
    // `POP CS` encoding, not a two-byte-map prefix, so `#UD` is the WRONG
    // answer for them (MDD-BUG-CRUCIBLE-00659). Their behaviour is pinned
    // by `pop_cs_executes_as_one_byte_on_8086_and_8088`.
    const PRE_386: [gp64_cpu::CpuType; 2] = [gp64_cpu::CpuType::I80186, gp64_cpu::CpuType::I80286];

    for cpu_type in PRE_386 {
        // JZ +0x7F7F. The taken branch must not be evaluated on a pre-386 CPU.
        let (mut cpu, mut mem) = setup(&[0x0F, 0x84, 0x7F, 0x7F]);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.flags.insert(Flags::ZERO);
        cpu.regs.ebx = 0x1234_5678;
        let flags_before = cpu.regs.flags;

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 6,
                    error_code: 0,
                    fault_addr: None,
                    executed: 0,
                }
            ),
            "{cpu_type:?} must reject near Jcc with #UD, got {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x0100, "{cpu_type:?} advanced EIP");
        assert_eq!(cpu.regs.ebx, 0x1234_5678, "{cpu_type:?} changed a register");
        assert_eq!(cpu.regs.flags, flags_before, "{cpu_type:?} changed flags");
    }
}

#[test]
fn setcc_rejects_pre_386_cpus_before_destination_write() {
    // 8086/8088 absent for the same reason as the near-Jcc test above:
    // `0F` is one-byte `POP CS` there (MDD-BUG-CRUCIBLE-00659).
    const PRE_386: [gp64_cpu::CpuType; 2] = [gp64_cpu::CpuType::I80186, gp64_cpu::CpuType::I80286];
    const SENTINEL: u8 = 0xA5;

    for cpu_type in PRE_386 {
        // SETZ byte ptr [0200h]. The destination must not be decoded or written.
        let (mut cpu, mut mem) = setup(&[0x0F, 0x94, 0x06, 0x00, 0x02]);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.flags.insert(Flags::ZERO);
        cpu.regs.ecx = 0xCAFE_BABE;
        mem.ram[0x0200] = SENTINEL;
        let flags_before = cpu.regs.flags;

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 6,
                    error_code: 0,
                    fault_addr: None,
                    executed: 0,
                }
            ),
            "{cpu_type:?} must reject SETcc with #UD, got {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x0100, "{cpu_type:?} advanced EIP");
        assert_eq!(cpu.regs.ecx, 0xCAFE_BABE, "{cpu_type:?} changed a register");
        assert_eq!(
            mem.ram[0x0200], SENTINEL,
            "{cpu_type:?} wrote the destination"
        );
        assert_eq!(cpu.regs.flags, flags_before, "{cpu_type:?} changed flags");
    }
}

// ------------------------------------------------------------------
// MDD-BUG-CRUCIBLE-00659 — `0F` is one-byte `POP CS` on the 8086/8088.
// ------------------------------------------------------------------

/// Prime a real-mode stack at SS=0 holding `selector` as the top word.
fn prime_pop_cs_stack(cpu: &mut Cpu, mem: &mut Memory, sp: u16, selector: u16) {
    cpu.regs.ss = 0x0000;
    cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(0);
    cpu.regs.set_sp(sp);
    let [lo, hi] = selector.to_le_bytes();
    mem.ram[sp as usize] = lo;
    mem.ram[sp as usize + 1] = hi;
}

#[test]
fn pop_cs_executes_as_one_byte_on_8086_and_8088() {
    const PRE_186: [gp64_cpu::CpuType; 2] = [gp64_cpu::CpuType::I8086, gp64_cpu::CpuType::I8088];
    // New CS 0x0020 => hidden base 0x200, so the byte after the `0F`
    // (IP 0x0101) is fetched from linear 0x0301 — NOT from linear 0x0101.
    // That address separation is what proves the hidden base moved.
    const NEW_CS: u16 = 0x0020;
    const NEXT_INSN_LINEAR: usize = 0x0301;

    for cpu_type in PRE_186 {
        let (mut cpu, mut mem) = setup(&[0x0F]);
        cpu.set_cpu_type(cpu_type);
        prime_pop_cs_stack(&mut cpu, &mut mem, 0x0FF0, NEW_CS);
        // MOV AL, 0x5A at the far side of the CS change.
        mem.ram[NEXT_INSN_LINEAR] = 0xB0;
        mem.ram[NEXT_INSN_LINEAR + 1] = 0x5A;
        // A decoy at the byte the two-byte map would have consumed, so a
        // regression that re-enters `op_0f` cannot accidentally look right.
        mem.ram[0x0101] = 0xFF;
        cpu.regs.eax = 0;
        let flags_before = cpu.regs.flags;

        let result = run(&mut cpu, &mut mem, &mut (), 2);

        assert!(
            matches!(result, RunResult::BudgetExhausted { executed: 2 }),
            "{cpu_type:?} did not retire POP CS + the following MOV: {result:?}"
        );
        assert_eq!(cpu.regs.cs, NEW_CS, "{cpu_type:?} did not load CS");
        assert_eq!(
            cpu.regs.cs_cache.base,
            u32::from(NEW_CS) << 4,
            "{cpu_type:?} did not update the hidden CS base"
        );
        assert_eq!(
            cpu.regs.sp(),
            0x0FF2,
            "{cpu_type:?} did not advance SP by one word"
        );
        assert_eq!(
            cpu.regs.eax & 0xFF,
            0x5A,
            "{cpu_type:?} did not execute the byte after the POP CS in the new segment"
        );
        assert_eq!(
            cpu.regs.eip, 0x0103,
            "{cpu_type:?} left EIP off the instruction after the MOV"
        );
        assert_eq!(cpu.regs.flags, flags_before, "{cpu_type:?} changed flags");
    }
}

#[test]
fn pop_cs_wraps_sp_at_the_top_of_the_stack_segment() {
    const NEW_CS: u16 = 0x0020;

    let (mut cpu, mut mem) = setup(&[0x0F]);
    cpu.set_cpu_type(gp64_cpu::CpuType::I8086);
    prime_pop_cs_stack(&mut cpu, &mut mem, 0xFFFE, NEW_CS);
    mem.ram[0x0301] = 0x90; // NOP in the new segment

    let result = run(&mut cpu, &mut mem, &mut (), 2);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "POP CS at SP=0xFFFE did not retire: {result:?}"
    );
    assert_eq!(cpu.regs.cs, NEW_CS, "POP CS did not load CS on SP wrap");
    assert_eq!(cpu.regs.sp(), 0x0000, "SP did not wrap to 0x0000");
}

#[test]
fn zero_f_stays_the_two_byte_map_from_the_186_onwards() {
    // The one-byte `POP CS` encoding is 8086/8088 only. Every later part
    // must keep `0F` as the two-byte opcode-map prefix — so an undefined
    // second byte is `#UD` and the stack is NOT popped.
    const FROM_186: [gp64_cpu::CpuType; 3] = [
        gp64_cpu::CpuType::I80186,
        gp64_cpu::CpuType::I80286,
        gp64_cpu::CpuType::I80386,
    ];

    for cpu_type in FROM_186 {
        let (mut cpu, mut mem) = setup(&[0x0F, 0x0F]);
        cpu.set_cpu_type(cpu_type);
        prime_pop_cs_stack(&mut cpu, &mut mem, 0x0FF0, 0x0020);
        let cs_before = cpu.regs.cs;

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(result, RunResult::Exception { vector: 6, .. }),
            "{cpu_type:?} must reject `0F 0F` with #UD, got {result:?}"
        );
        assert_eq!(cpu.regs.cs, cs_before, "{cpu_type:?} loaded CS from `0F`");
        assert_eq!(cpu.regs.sp(), 0x0FF0, "{cpu_type:?} popped the stack");
    }
}

// ========================================================================
// Budget exhaustion
// ========================================================================

#[test]
fn test_budget_zero_returns_immediately() {
    let (mut cpu, mut mem) = setup(&[0x90]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 0);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 0 }));
}

// ========================================================================
// 32-bit operand size tests
// ========================================================================

#[test]
fn test_xor_r32_r32() {
    // 0x66 XOR ECX, ECX in 16-bit mode
    let (mut cpu, mut mem) = setup(&[0x66, 0x31, 0xC9]);
    cpu.regs.ecx = 0x1234_5678;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx, 0);
}

#[test]
fn test_add_r32_imm8() {
    // 0x66 ADD ECX, 1 in 16-bit mode
    let (mut cpu, mut mem) = setup(&[0x66, 0x83, 0xC1, 0x01]);
    cpu.regs.ecx = 0x0000_FFFF;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx, 0x0001_0000);
}

#[test]
fn test_cmp_r32_imm32() {
    // 0x66 CMP ECX, 0x12345678 in 16-bit mode
    let (mut cpu, mut mem) = setup(&[0x66, 0x81, 0xF9, 0x78, 0x56, 0x34, 0x12]);
    cpu.regs.ecx = 0x12345678;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// Tight loop benchmark
// ========================================================================

#[cfg_attr(coverage, ignore)]
#[test]
fn benchmark_tight_loop() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    // Program: XOR CX,CX; .loop: ADD CX,1; CMP CX,0x2710; JNE .loop; INT 20h
    // At 0000:0100
    let code: &[u8] = &[
        0x31, 0xC9, // XOR CX, CX
        0x83, 0xC1, 0x01, // ADD CX, 1
        0x81, 0xF9, 0x10, 0x27, // CMP CX, 10000
        0x75, 0xF7, // JNE -9 (back to ADD)
        0xCD, 0x20, // INT 20h (exit)
    ];
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }

    let start = std::time::Instant::now();
    let mut total_executed = 0u64;
    loop {
        let result = run(&mut cpu, &mut mem, &mut io, 100_000);
        total_executed += result.executed() as u64;
        match result {
            RunResult::SoftwareInterrupt { vector: 0x20, .. } => break,
            RunResult::BudgetExhausted { .. } => continue,
            other => panic!("Unexpected: {:?}", other),
        }
    }
    let elapsed = start.elapsed();
    let mips = total_executed as f64 / elapsed.as_secs_f64() / 1_000_000.0;
    eprintln!(
        "Tight loop: {} instructions in {:?} = {:.1} MIPS",
        total_executed, elapsed, mips
    );
    // Preserve a release-only regression guard; debug timing is too variable for
    // a stable floor and should be validated in a dedicated perf run.
    if !cfg!(debug_assertions) {
        assert!(
            mips >= 30.0,
            "MIPS too low: {:.1} (need >= 30 in release)",
            mips
        );
    }
}

// ========================================================================
// Correctness: integrated loop test
// ========================================================================

#[test]
fn test_loop_counts_to_10() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    // XOR CX,CX; .loop: ADD CX,1; CMP CX,10; JNE .loop; INT 20h
    let code: &[u8] = &[
        0x31, 0xC9, // XOR CX, CX           @ 0x100 (2 bytes)
        0x83, 0xC1, 0x01, // ADD CX, 1       @ 0x102 (3 bytes)
        0x83, 0xF9, 0x0A, // CMP CX, 10      @ 0x105 (3 bytes)
        0x75, 0xF8, // JNE -8 (back to ADD)  @ 0x108 (2 bytes) → 0x10A + (-8) = 0x102
        0xCD, 0x20, // INT 20h               @ 0x10A
    ];
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }

    loop {
        let result = run(&mut cpu, &mut mem, &mut io, 1000);
        match result {
            RunResult::SoftwareInterrupt { vector: 0x20, .. } => break,
            RunResult::BudgetExhausted { .. } => continue,
            other => panic!("Unexpected: {:?}", other),
        }
    }

    // CX should be 10 after the loop
    assert_eq!(cpu.regs.ecx as u16, 10);
}

// ========================================================================
// Parity table correctness
// ========================================================================

#[test]
fn test_parity_table() {
    assert!(PARITY_TABLE[0x00]); // 0 bits set → even
    assert!(!PARITY_TABLE[0x01]); // 1 bit set → odd
    assert!(PARITY_TABLE[0x03]); // 2 bits set → even
    assert!(PARITY_TABLE[0xFF]); // 8 bits set → even
    assert!(!PARITY_TABLE[0x80]); // 1 bit set → odd
}

// ========================================================================
// Condition code evaluator
// ========================================================================

#[test]
fn test_eval_cc_all_conditions() {
    let mut cpu = Cpu::new();

    // Test JO (0) / JNO (1)
    cpu.regs.flags.insert(Flags::OVERFLOW);
    assert!(eval_cc(cpu.regs.flags, &LazyOp::None, 0x0));
    assert!(!eval_cc(cpu.regs.flags, &LazyOp::None, 0x1));
    cpu.regs.flags.remove(Flags::OVERFLOW);
    assert!(!eval_cc(cpu.regs.flags, &LazyOp::None, 0x0));
    assert!(eval_cc(cpu.regs.flags, &LazyOp::None, 0x1));

    // Test JB (2) / JNB (3)
    cpu.regs.flags.insert(Flags::CARRY);
    assert!(eval_cc(cpu.regs.flags, &LazyOp::None, 0x2));
    assert!(!eval_cc(cpu.regs.flags, &LazyOp::None, 0x3));
    cpu.regs.flags.remove(Flags::CARRY);

    // Test JZ (4) / JNZ (5)
    cpu.regs.flags.insert(Flags::ZERO);
    assert!(eval_cc(cpu.regs.flags, &LazyOp::None, 0x4));
    assert!(!eval_cc(cpu.regs.flags, &LazyOp::None, 0x5));
    cpu.regs.flags.remove(Flags::ZERO);

    // Test JBE (6) / JNBE (7) — CF=0, ZF=0
    assert!(!eval_cc(cpu.regs.flags, &LazyOp::None, 0x6));
    assert!(eval_cc(cpu.regs.flags, &LazyOp::None, 0x7));

    // Test JS (8) / JNS (9)
    cpu.regs.flags.insert(Flags::SIGN);
    assert!(eval_cc(cpu.regs.flags, &LazyOp::None, 0x8));
    assert!(!eval_cc(cpu.regs.flags, &LazyOp::None, 0x9));
    cpu.regs.flags.remove(Flags::SIGN);

    // Test JP (A) / JNP (B)
    cpu.regs.flags.insert(Flags::PARITY);
    assert!(eval_cc(cpu.regs.flags, &LazyOp::None, 0xA));
    assert!(!eval_cc(cpu.regs.flags, &LazyOp::None, 0xB));
    cpu.regs.flags.remove(Flags::PARITY);

    // Test JL (C) / JNL (D) — SF!=OF means L
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    assert!(eval_cc(cpu.regs.flags, &LazyOp::None, 0xC)); // SF(1)!=OF(0) → L
    assert!(!eval_cc(cpu.regs.flags, &LazyOp::None, 0xD));
    cpu.regs.flags.remove(Flags::SIGN);

    // Test JLE (E) / JNLE (F)
    cpu.regs.flags.insert(Flags::ZERO);
    assert!(eval_cc(cpu.regs.flags, &LazyOp::None, 0xE)); // ZF=1 → LE
    assert!(!eval_cc(cpu.regs.flags, &LazyOp::None, 0xF));
    cpu.regs.flags.remove(Flags::ZERO);
}

// ========================================================================
// last_iret / last_retf clearing
// ========================================================================

#[test]
fn test_run_clears_last_iret_retf() {
    let (mut cpu, mut mem) = setup(&[0x90]); // NOP
    cpu.last_iret = true;
    cpu.last_retf = true;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.last_iret);
    assert!(!cpu.last_retf);
}

// write-context diagnostic is a `PcExec` hook.

// ========================================================================
// Lazy flags tests
// ========================================================================

#[test]
fn test_snapshot_write_register_context_materializes_lazy_flags() {
    // M2.4b split: the interpreter materialises the flags it hands the
    // policy; the register snapshot is `PcExec`'s
    // (the machine execution layer `write_register_context_records_flags_and_segments`).
    let cpu = Cpu::new();
    let lazy = LazyOp::Add16 {
        a: 0xFFFF,
        b: 1,
        r: 0,
    };

    let flags = write_context_flags(cpu.regs.flags, lazy);

    assert!(flags.contains(Flags::CARRY));
    assert!(flags.contains(Flags::ZERO));
    assert!(flags.contains(Flags::PARITY));
}

#[test]
fn test_lazy_flags_add_cf() {
    // Verify lazy CF from ADD via per-flag getter
    let cpu = Cpu::new();
    let lazy = LazyOp::Add16 {
        a: 0xFFFF,
        b: 1,
        r: 0,
    };
    assert!(get_cf(cpu.regs.flags, &lazy)); // 0xFFFF + 1 overflows → CF=1
    assert!(get_zf(cpu.regs.flags, &lazy)); // result is 0
}

#[test]
fn test_lazy_flags_sub_cf() {
    let cpu = Cpu::new();
    let lazy = LazyOp::Sub16 {
        a: 5,
        b: 10,
        r: 0xFFFB,
    };
    assert!(get_cf(cpu.regs.flags, &lazy)); // 5 < 10 → CF=1 (borrow)
    assert!(!get_zf(cpu.regs.flags, &lazy));
    assert!(get_sf(cpu.regs.flags, &lazy)); // 0xFFFB has sign bit set
}

#[test]
fn test_lazy_flags_logic_clears_cf_of() {
    let cpu = Cpu::new();
    let lazy = LazyOp::Logic16(0);
    assert!(!get_cf(cpu.regs.flags, &lazy)); // Logic always CF=0
    assert!(!get_of(cpu.regs.flags, &lazy)); // Logic always OF=0
    assert!(get_zf(cpu.regs.flags, &lazy)); // result is 0
}

#[test]
fn test_lazy_flags_inc_preserves_cf() {
    // INC should NOT affect CF — it reads from cpu.regs.flags
    let mut cpu = Cpu::new();
    cpu.regs.flags.insert(Flags::CARRY);
    let lazy = LazyOp::Inc16 { old: 0, r: 1 };
    assert!(get_cf(cpu.regs.flags, &lazy)); // Preserved: CF was set

    cpu.regs.flags.remove(Flags::CARRY);
    assert!(!get_cf(cpu.regs.flags, &lazy)); // Preserved: CF was clear
}

#[test]
fn test_lazy_flags_dec_preserves_cf() {
    let mut cpu = Cpu::new();
    cpu.regs.flags.insert(Flags::CARRY);
    let lazy = LazyOp::Dec16 { old: 1, r: 0 };
    assert!(get_cf(cpu.regs.flags, &lazy));
    assert!(get_zf(cpu.regs.flags, &lazy)); // DEC 1 → 0, ZF=1
}

#[test]
fn test_lazy_flags_inc_of() {
    let cpu = Cpu::new();
    // INC 0x7FFF → 0x8000 should set OF (signed overflow)
    let lazy = LazyOp::Inc16 {
        old: 0x7FFF,
        r: 0x8000,
    };
    assert!(get_of(cpu.regs.flags, &lazy));
}

#[test]
fn test_lazy_flags_dec_of() {
    let cpu = Cpu::new();
    // DEC 0x8000 → 0x7FFF should set OF (signed underflow)
    let lazy = LazyOp::Dec16 {
        old: 0x8000,
        r: 0x7FFF,
    };
    assert!(get_of(cpu.regs.flags, &lazy));
}

#[test]
fn test_lazy_flags_dec_af() {
    let cpu = Cpu::new();
    // DEC where low nibble wraps: 0x10 → 0x0F
    let lazy = LazyOp::Dec16 { old: 0x10, r: 0x0F };
    assert!(get_af(cpu.regs.flags, &lazy)); // 0x0F & 0x0F == 0x0F → AF=1
}

#[test]
fn test_lazy_flags_parity() {
    let cpu = Cpu::new();
    // 0 has even parity (0 set bits)
    let lazy = LazyOp::Logic16(0);
    assert!(get_pf(cpu.regs.flags, &lazy));

    // 1 has odd parity (1 set bit)
    let lazy = LazyOp::Logic16(1);
    assert!(!get_pf(cpu.regs.flags, &lazy));

    // 3 = 0b11 has even parity
    let lazy = LazyOp::Logic16(3);
    assert!(get_pf(cpu.regs.flags, &lazy));
}

#[test]
fn test_materialize_flags_writes_all() {
    let mut cpu = Cpu::new();
    let mut flags_reg = cpu.regs.flags;
    let mut lazy = LazyOp::Add16 {
        a: 0xFFFF,
        b: 1,
        r: 0,
    };
    materialize_flags(&mut flags_reg, &mut lazy);
    cpu.regs.flags = flags_reg;
    assert!(matches!(lazy, LazyOp::None));
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert!(!cpu.regs.flags.contains(Flags::SIGN));
    assert!(!cpu.regs.flags.contains(Flags::OVERFLOW));
    assert!(cpu.regs.flags.contains(Flags::PARITY)); // 0 = even parity
}

#[test]
fn test_materialize_none_is_noop() {
    let mut cpu = Cpu::new();
    cpu.regs.flags.insert(Flags::CARRY);
    let mut flags_reg = cpu.regs.flags;
    let mut lazy = LazyOp::None;
    materialize_flags(&mut flags_reg, &mut lazy);
    cpu.regs.flags = flags_reg;
    // Flags unchanged
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_inc_cf_preservation_through_run() {
    // STC; INC AX — CF should be preserved after INC
    let (mut cpu, mut mem) = setup(&[0xF9, 0x40]); // STC, INC AX
    cpu.regs.eax = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // Preserved by INC
    assert_eq!(cpu.regs.eax & 0xFFFF, 1);
}

#[test]
fn test_eval_cc_with_lazy_sub() {
    // Test Jcc evaluation directly against lazy Sub state
    let cpu = Cpu::new();
    // CMP AX, BX where AX=5, BX=10 → r=0xFFFB, a < b
    let lazy = LazyOp::Sub16 {
        a: 5,
        b: 10,
        r: 0xFFFB,
    };
    assert!(eval_cc(cpu.regs.flags, &lazy, 0x2)); // JB: a < b → true
    assert!(!eval_cc(cpu.regs.flags, &lazy, 0x3)); // JNB: false
    assert!(!eval_cc(cpu.regs.flags, &lazy, 0x4)); // JE: not equal
    assert!(eval_cc(cpu.regs.flags, &lazy, 0x5)); // JNE: true
}

#[test]
fn test_clc_materializes_before_clearing() {
    // ADD (sets CF=1) then CLC should clear it properly
    // ADD AX, BX (0x01 0xC8 = ADD AX, CX with CX=0xFFFF, AX=1)
    // Then CLC (0xF8)
    let (mut cpu, mut mem) = setup(&[0x01, 0xC8, 0xF8]); // ADD AX,CX; CLC
    cpu.regs.eax = 1;
    cpu.regs.ecx = 0xFFFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // CLC cleared it
}
