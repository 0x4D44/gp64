use super::test_helpers::setup;
use super::*;

/// INT 0xCD must increment `executed` before returning SoftwareInterrupt.
#[test]
fn test_int_cd_counts_as_executed() {
    let (mut cpu, mut mem) = setup(&[0xCD, 0x21]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::SoftwareInterrupt { vector: 0x21, executed } if executed >= 1),
        "INT 0x21 should have executed >= 1, got {:?}",
        result
    );
}

/// IRET (16-bit) must increment `executed` before returning Iret.
#[test]
fn test_iret_counts_as_executed() {
    let (mut cpu, mut mem) = setup(&[0xCF]);
    let stack_base = 0x1000usize;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    cpu.regs.ss = 0x0000;
    cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(0);
    mem.ram[stack_base] = 0x00;
    mem.ram[stack_base + 1] = 0x02;
    mem.ram[stack_base + 2] = 0x00;
    mem.ram[stack_base + 3] = 0x00;
    mem.ram[stack_base + 4] = 0x02;
    mem.ram[stack_base + 5] = 0x02;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Iret { executed } if executed >= 1),
        "IRET should have executed >= 1, got {:?}",
        result
    );
}

/// RETF (0xCB) must increment `executed` before returning Retf.
#[test]
fn test_retf_counts_as_executed() {
    let (mut cpu, mut mem) = setup(&[0xCB]);
    let stack_base = 0x1000usize;
    cpu.regs.set_sp(stack_base as u16);
    cpu.regs.esp = stack_base as u32;
    cpu.regs.ss = 0x0000;
    cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(0);
    mem.ram[stack_base] = 0x00;
    mem.ram[stack_base + 1] = 0x02;
    mem.ram[stack_base + 2] = 0x00;
    mem.ram[stack_base + 3] = 0x00;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Retf { executed } if executed >= 1),
        "RETF should have executed >= 1, got {:?}",
        result
    );
}

#[test]
fn far_return_watch_controls_retf_batch_boundary() {
    for watch in [false, true] {
        let (mut cpu, mut mem) = setup(&[0x9A, 0x00, 0x03, 0x00, 0x00, 0x90]);
        cpu.far_return_watch = watch;
        cpu.regs.set_sp(0x1000);
        cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(0);
        mem.ram[0x0300] = 0xCB;
        let result = run(&mut cpu, &mut mem, &mut (), 3);
        if watch {
            assert!(
                matches!(result, RunResult::Retf { executed: 2 }),
                "{result:?}"
            );
            assert_eq!(cpu.regs.ip(), 0x0105);
        } else {
            assert!(
                matches!(result, RunResult::BudgetExhausted { executed: 3 }),
                "{result:?}"
            );
            assert_eq!(cpu.regs.ip(), 0x0106);
        }
    }
}

#[test]
fn far_return_watch_controls_iret_batch_boundary_and_if_transition() {
    for (watch, returned_flags, should_exit) in [
        (false, 0x0002u16, false),
        (true, 0x0002, true),
        (false, 0x0202, true),
    ] {
        let (mut cpu, mut mem) = setup(&[0xCF]);
        cpu.far_return_watch = watch;
        cpu.regs.flags = Flags::RESERVED1;
        cpu.regs.set_sp(0x1000);
        cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(0);
        mem.ram[0x1000..0x1006].copy_from_slice(&[
            0x00,
            0x02,
            0x00,
            0x00,
            returned_flags as u8,
            (returned_flags >> 8) as u8,
        ]);
        mem.ram[0x0200] = 0x90;
        let result = run(&mut cpu, &mut mem, &mut (), 2);
        if should_exit {
            assert!(
                matches!(result, RunResult::Iret { executed: 1 }),
                "{result:?}"
            );
            assert_eq!(cpu.regs.ip(), 0x0200);
        } else {
            assert!(
                matches!(result, RunResult::BudgetExhausted { executed: 2 }),
                "{result:?}"
            );
            assert_eq!(cpu.regs.ip(), 0x0201);
        }
    }
}

const I186_OPCODE_CASES: [(&str, &[u8]); 10] = [
    ("PUSH imm16", &[0x68, 0x34, 0x12]),
    ("PUSH imm8", &[0x6A, 0x7F]),
    ("IMUL r16,r/m16,imm16", &[0x69, 0xC0, 0x02, 0x00]),
    ("IMUL r16,r/m16,imm8", &[0x6B, 0xC0, 0x02]),
    ("INSB", &[0x6C]),
    ("INSW", &[0x6D]),
    ("OUTSB", &[0x6E]),
    ("OUTSW", &[0x6F]),
    ("ROL r/m8,imm8", &[0xC0, 0xC0, 0x01]),
    ("SHL r/m16,imm8", &[0xC1, 0xE0, 0x01]),
];

#[test]
fn test_80186_opcode_matrix_is_ud_on_8086_class_without_state_change() {
    for cpu_type in [gp64_cpu::CpuType::I8086, gp64_cpu::CpuType::I8088] {
        for (name, code) in I186_OPCODE_CASES {
            let (mut cpu, mut mem) = setup(code);
            cpu.set_cpu_type(cpu_type);
            cpu.regs.eax = 0x1234;
            cpu.regs.edx = 0x0042;
            cpu.regs.esi = 0x0200;
            cpu.regs.edi = 0x0300;
            cpu.regs.esp = 0x8000;
            cpu.regs.flags = Flags::RESERVED1
                | Flags::CARRY
                | Flags::PARITY
                | Flags::AUX_CARRY
                | Flags::ZERO
                | Flags::SIGN
                | Flags::DIRECTION
                | Flags::OVERFLOW;
            mem.ram[0x0200..0x0202].copy_from_slice(&[0xA5, 0x5A]);
            let registers_before = (
                cpu.regs.eax,
                cpu.regs.edx,
                cpu.regs.esi,
                cpu.regs.edi,
                cpu.regs.esp,
                cpu.regs.flags,
            );
            let source_and_destination_before = mem.ram[0x0200..0x0302].to_vec();
            let stack_before = mem.ram[0x7FF0..0x8002].to_vec();

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
                "{cpu_type:?} must reject {name} with #UD, got {result:?}"
            );
            assert_eq!(cpu.regs.eip, 0x0100, "{cpu_type:?} {name} advanced EIP");
            assert_eq!(
                (
                    cpu.regs.eax,
                    cpu.regs.edx,
                    cpu.regs.esi,
                    cpu.regs.edi,
                    cpu.regs.esp,
                    cpu.regs.flags,
                ),
                registers_before,
                "{cpu_type:?} {name} changed registers or flags before #UD"
            );
            assert_eq!(
                &mem.ram[0x0200..0x0302],
                source_and_destination_before.as_slice(),
                "{cpu_type:?} {name} changed string source/destination memory"
            );
            assert_eq!(
                &mem.ram[0x7FF0..0x8002],
                stack_before.as_slice(),
                "{cpu_type:?} {name} changed stack memory"
            );
        }
    }
}

#[test]
fn test_80186_opcode_matrix_executes_on_every_newer_cpu() {
    for cpu_type in [
        gp64_cpu::CpuType::I80186,
        gp64_cpu::CpuType::I80286,
        gp64_cpu::CpuType::I80386,
        gp64_cpu::CpuType::I80486,
        gp64_cpu::CpuType::Pentium,
        gp64_cpu::CpuType::PentiumMmx,
    ] {
        for (name, code) in I186_OPCODE_CASES {
            let (mut cpu, mut mem) = setup(code);
            cpu.set_cpu_type(cpu_type);
            cpu.regs.eax = 0x1234;
            cpu.regs.edx = 0x0042;
            cpu.regs.esi = 0x0200;
            cpu.regs.edi = 0x0300;
            cpu.regs.esp = 0x8000;
            mem.ram[0x0200..0x0202].copy_from_slice(&[0xA5, 0x5A]);

            let result = run(&mut cpu, &mut mem, &mut (), 1);

            assert!(
                matches!(result, RunResult::BudgetExhausted { executed: 1 }),
                "{cpu_type:?} should execute {name}, got {result:?}"
            );
        }
    }
}

// ========================================================================
// CMOVcc (0F 40-4F) — Conditional Move
// ========================================================================

/// RDPMC's full privilege matrix.
///
/// The SDM's condition is
/// `IF (CR4.PCE = 1 OR CPL = 0 OR CR0.PE = 0) AND ECX valid`, and each
/// disjunct is exercised separately - a test that only covered "CPL 3
/// without PCE faults" would pass against an implementation that always
/// faults.
///
/// V86 is called out because it is the case a naive `cpl != 0` check gets
/// wrong: `cpu.regs.cpl` is not reliably 3 there, which is why the RDTSC
/// gate this copies spells the V86 term out explicitly
///
#[test]
fn rdpmc_privilege_and_index_matrix() {
    const RDPMC: &[u8] = &[0x0F, 0x33];
    const PCE: u32 = 1 << 8;

    // Helper outcomes.
    let expect_ok = |cpu: &Cpu, result: &RunResult, what: &str| {
        assert!(
            matches!(result, RunResult::BudgetExhausted { executed: 1 }),
            "{what}: RDPMC should have executed, got {result:?}"
        );
        assert_eq!(cpu.regs.eax, 0, "{what}: EAX must read 0");
        assert_eq!(cpu.regs.edx, 0, "{what}: EDX must read 0");
    };
    let expect_gp = |result: &RunResult, what: &str| {
        assert!(
            matches!(result, RunResult::Exception { vector: 13, .. }),
            "{what}: expected #GP(0), got {result:?}"
        );
    };

    for cpu_type in p6_cpu_types() {
        // Real mode (CR0.PE = 0): permitted unconditionally, no PCE needed.
        let (mut cpu, mut mem) = setup(RDPMC);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.eax = 0xDEAD_BEEF;
        cpu.regs.edx = 0xDEAD_BEEF;
        let r = run(&mut cpu, &mut mem, &mut (), 1);
        expect_ok(&cpu, &r, "real mode");

        // Protected mode, CPL 0: permitted without PCE.
        let (mut cpu, mut mem) = setup(RDPMC);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.cr0 |= 1;
        cpu.regs.set_cpl(0);
        let r = run(&mut cpu, &mut mem, &mut (), 1);
        expect_ok(&cpu, &r, "PM CPL0");

        // Protected mode, CPL 3, PCE clear: #GP.
        let (mut cpu, mut mem) = setup(RDPMC);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.cr0 |= 1;
        cpu.regs.set_cpl(3);
        cpu.regs.cr4 &= !PCE;
        let r = run(&mut cpu, &mut mem, &mut (), 1);
        expect_gp(&r, "PM CPL3 without PCE");

        // Protected mode, CPL 3, PCE SET: permitted. Without this case the
        // test would pass against an implementation that always faults at
        // CPL 3.
        let (mut cpu, mut mem) = setup(RDPMC);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.cr0 |= 1;
        cpu.regs.set_cpl(3);
        cpu.regs.cr4 |= PCE;
        cpu.regs.eax = 0xDEAD_BEEF;
        let r = run(&mut cpu, &mut mem, &mut (), 1);
        expect_ok(&cpu, &r, "PM CPL3 with PCE");

        // ECX bound, both sides: counter 1 is valid, counter 2 is not.
        let (mut cpu, mut mem) = setup(RDPMC);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.ecx = 1;
        let r = run(&mut cpu, &mut mem, &mut (), 1);
        expect_ok(&cpu, &r, "ECX=1 (second counter)");

        let (mut cpu, mut mem) = setup(RDPMC);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.ecx = 2;
        let r = run(&mut cpu, &mut mem, &mut (), 1);
        expect_gp(&r, "ECX=2 (out of range)");
    }
}

/// RDPMC exists on the Pentium MMX too, and NOT below it.
///
/// This is the boundary the implementation deliberately does not draw at
/// P6: the SDM introduces RDPMC on "the Pentium Pro processor and the
/// Pentium processor with MMX technology". Gating on `supports_p6()` would
/// make the emulator wrong about P55C, a part it already models.
#[test]
fn rdpmc_is_available_from_pentium_mmx_upward() {
    const RDPMC: &[u8] = &[0x0F, 0x33];
    for cpu_type in gp64_cpu::CpuType::ALL {
        if matches!(
            cpu_type,
            gp64_cpu::CpuType::I8086 | gp64_cpu::CpuType::I8088
        ) {
            continue; // `0F` is POP CS there.
        }
        let (mut cpu, mut mem) = setup(RDPMC);
        cpu.set_cpu_type(cpu_type);
        let result = run(&mut cpu, &mut mem, &mut (), 1);
        if cpu_type.supports_rdpmc() {
            assert!(
                matches!(result, RunResult::BudgetExhausted { executed: 1 }),
                "{cpu_type:?}: RDPMC must execute, got {result:?}"
            );
        } else {
            assert!(
                matches!(result, RunResult::Exception { vector: 6, .. }),
                "{cpu_type:?}: RDPMC must #UD, got {result:?}"
            );
        }
    }
}

/// The four P6 performance-monitoring MSRs are reachable on a P6 identity.
///
/// Without them RDPMC is implemented but unusable: a guest following the
/// documented sequence faults at the WRMSR that programs the counter, one
/// instruction BEFORE the RDPMC. There is no CPUID bit for this facility,
/// so advertising family 6 is what entitles the guest to it.
#[test]
fn p6_performance_msrs_are_readable_and_writable() {
    for msr in [0xC1u32, 0xC2, 0x186, 0x187] {
        for cpu_type in gp64_cpu::CpuType::ALL {
            let mut cpu = Cpu::new();
            cpu.set_cpu_type(cpu_type);
            let read = gp64_cpu::msr::read_msr(&cpu, msr);
            let write = gp64_cpu::msr::write_msr(&mut cpu, msr, 0x1234_5678);
            if cpu_type.supports_p6() {
                assert_eq!(
                    read.expect("P6 must expose the perf MSRs"),
                    0,
                    "MSR {msr:#x} on {cpu_type:?}: we model no events, so it reads 0"
                );
                assert!(
                    write.is_ok(),
                    "MSR {msr:#x} on {cpu_type:?}: the programming write must be accepted"
                );
            } else {
                assert!(
                    read.is_err(),
                    "MSR {msr:#x} on {cpu_type:?}: pre-P6 must #GP"
                );
                assert!(
                    write.is_err(),
                    "MSR {msr:#x} on {cpu_type:?}: pre-P6 must #GP"
                );
            }
        }
    }
}

/// Every P6-capable identity, for the positive half of the P6 coverage.
fn p6_cpu_types() -> Vec<gp64_cpu::CpuType> {
    gp64_cpu::CpuType::ALL
        .into_iter()
        .filter(|t| t.supports_p6())
        .collect()
}

/// CMOVcc executes on a P6 identity - all 16 conditions, both directions.
///
/// The counterpart to the #UD tests below. Until the Pentium Pro identity
/// existed there was NO CpuType on which `supports_p6()` was true, so this
/// arm of the interpreter was unreachable and the repo's only CMOVcc
/// semantic coverage lived in the legacy oracle.
#[test]
fn cmovcc_executes_on_p6_identities() {
    // (condition nibble, flags to set, should the move happen)
    // Each condition appears in both polarities so a stuck-true or
    // stuck-false `eval_cc` cannot pass.
    let cases: &[(u8, Flags, bool)] = &[
        (0x0, Flags::OVERFLOW, true),
        (0x0, Flags::empty(), false),
        (0x1, Flags::OVERFLOW, false),
        (0x1, Flags::empty(), true),
        (0x2, Flags::CARRY, true),
        (0x2, Flags::empty(), false),
        (0x3, Flags::CARRY, false),
        (0x3, Flags::empty(), true),
        (0x4, Flags::ZERO, true),
        (0x4, Flags::empty(), false),
        (0x5, Flags::ZERO, false),
        (0x5, Flags::empty(), true),
        (0x6, Flags::CARRY, true),
        (0x6, Flags::ZERO, true),
        (0x6, Flags::empty(), false),
        (0x7, Flags::empty(), true),
        (0x7, Flags::ZERO, false),
        (0x8, Flags::SIGN, true),
        (0x8, Flags::empty(), false),
        (0x9, Flags::SIGN, false),
        (0x9, Flags::empty(), true),
        (0xA, Flags::PARITY, true),
        (0xA, Flags::empty(), false),
        (0xB, Flags::PARITY, false),
        (0xB, Flags::empty(), true),
        (0xC, Flags::SIGN, true),
        (0xC, Flags::empty(), false),
        (0xD, Flags::SIGN, false),
        (0xD, Flags::empty(), true),
        (0xE, Flags::ZERO, true),
        (0xE, Flags::SIGN, true),
        (0xE, Flags::empty(), false),
        (0xF, Flags::empty(), true),
        (0xF, Flags::ZERO, false),
    ];

    for cpu_type in p6_cpu_types() {
        for &(cc, flags, expect_move) in cases {
            // 66 0F 4x C3 = CMOVcc EAX, EBX.
            //
            // The 0x66 is load-bearing: `setup` builds a 16-bit code
            // segment, so without it this is the 16-bit CMOVcc AX, BX and
            // the assertion below would be checking a half-width move.
            let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0x40 | cc, 0xC3]);
            cpu.set_cpu_type(cpu_type);
            cpu.regs.eax = 0x1111_1111;
            cpu.regs.ebx = 0x2222_2222;
            cpu.regs.flags = flags | Flags::RESERVED1;
            let result = run(&mut cpu, &mut mem, &mut (), 1);
            assert!(
                matches!(result, RunResult::BudgetExhausted { executed: 1 }),
                "{cpu_type:?} CMOV cc={cc:X} flags={flags:?}: did not retire: {result:?}"
            );
            let want = if expect_move {
                0x2222_2222
            } else {
                0x1111_1111
            };
            assert_eq!(
                cpu.regs.eax, want,
                "{cpu_type:?} CMOV cc={cc:X} flags={flags:?}: wrong move decision"
            );
        }
    }
}

/// 16-bit CMOVcc writes only the low half, and an untaken CMOVcc with a
/// memory source still decodes its operand.
///
/// The source read matters because it can fault: a CMOVcc whose memory
/// operand is unmapped raises #PF regardless of the condition, so reading
/// only on the taken path would be an easy and invisible "optimisation".
#[test]
fn cmovcc_16bit_and_untaken_memory_source() {
    for cpu_type in p6_cpu_types() {
        // 0F 45 C3 = CMOVNE AX, BX (16-bit: no prefix in a 16-bit
        // segment), ZF=0 so the move is taken.
        let (mut cpu, mut mem) = setup(&[0x0F, 0x45, 0xC3]);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.eax = 0x1111_1111;
        cpu.regs.ebx = 0x2222_2222;
        cpu.regs.flags = Flags::RESERVED1;
        let result = run(&mut cpu, &mut mem, &mut (), 1);
        assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
        assert_eq!(
            cpu.regs.eax, 0x1111_2222,
            "{cpu_type:?}: 16-bit CMOV must write only AX and keep the high half"
        );

        // 66 0F 44 06 00 02 = CMOVE EAX, [0x0200], ZF=0 so NOT taken.
        let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0x44, 0x06, 0x00, 0x02]);
        cpu.set_cpu_type(cpu_type);
        cpu.regs.eax = 0x1111_1111;
        mem.ram[0x0200..0x0204].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        cpu.regs.flags = Flags::RESERVED1;
        let result = run(&mut cpu, &mut mem, &mut (), 1);
        assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
        assert_eq!(
            cpu.regs.eax, 0x1111_1111,
            "{cpu_type:?}: untaken CMOV must not write the destination"
        );
    }
}

/// `0F 1F /0` is a NOP on P6 and consumes its full ModR/M encoding.
///
/// The consumption is the interesting half: getting the length wrong
/// desynchronises the instruction stream, so every form is followed by a
/// distinctive MOV that must then decode and execute.
#[test]
fn multi_byte_nop_executes_and_consumes_its_modrm_on_p6() {
    // 66 B8 imm32 = MOV EAX, 0xDEADBEEF. The 0x66 is needed because
    // `setup` builds a 16-bit code segment.
    let tail: &[u8] = &[0x66, 0xB8, 0xEF, 0xBE, 0xAD, 0xDE];
    // `setup` builds a 16-bit segment, so ModR/M is decoded with 16-bit
    // ADDRESSING unless a 0x67 prefix says otherwise. That changes the
    // encoding lengths: mod=10 means disp16 here, and there is no SIB byte
    // at all. The last form carries 0x67 precisely to reach the 32-bit
    // addressing path, where mod=10 rm=100 DOES pull in a SIB plus disp32 -
    // the longest encoding, and the one most likely to be mis-measured.
    let forms: [(&[u8], &str); 5] = [
        (&[0x0F, 0x1F, 0xC0], "register form"),
        (&[0x0F, 0x1F, 0x00], "memory form, no displacement"),
        (&[0x0F, 0x1F, 0x44, 0x00], "disp8"),
        (&[0x0F, 0x1F, 0x84, 0x00, 0x00], "disp16"),
        (
            &[0x67, 0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
            "32-bit addressing: SIB + disp32",
        ),
    ];
    for cpu_type in p6_cpu_types() {
        for (nop, what) in forms {
            let mut code = nop.to_vec();
            code.extend_from_slice(tail);
            let (mut cpu, mut mem) = setup(&code);
            cpu.set_cpu_type(cpu_type);
            cpu.regs.eax = 0;
            let result = run(&mut cpu, &mut mem, &mut (), 2);
            assert!(
                matches!(result, RunResult::BudgetExhausted { executed: 2 }),
                "{cpu_type:?} {what}: expected NOP + MOV to retire, got {result:?}"
            );
            assert_eq!(
                cpu.regs.eax, 0xDEAD_BEEF,
                "{cpu_type:?} {what}: NOP consumed the wrong byte count, so the following MOV did not decode"
            );
        }
    }
}

/// UD2 (`0F 0B`) raises #UD on EVERY identity, P6 included.
///
/// Unlike the rest of the P6 delta this needed no implementation - raising
/// #UD *is* UD2's architectural definition, so the two-byte catch-all is
/// already correct. The test exists because "already correct" is a claim
/// worth pinning, and so a future explicit arm cannot change it silently.
#[test]
fn ud2_raises_ud_on_every_identity_including_p6() {
    for cpu_type in gp64_cpu::CpuType::ALL {
        if matches!(
            cpu_type,
            gp64_cpu::CpuType::I8086 | gp64_cpu::CpuType::I8088
        ) {
            continue; // `0F` is POP CS there, not a two-byte escape.
        }
        let (mut cpu, mut mem) = setup(&[0x0F, 0x0B]);
        cpu.set_cpu_type(cpu_type);
        let result = run(&mut cpu, &mut mem, &mut (), 1);
        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 6,
                    executed: 0,
                    ..
                }
            ),
            "{cpu_type:?}: UD2 must raise #UD, got {result:?}"
        );
    }
}

/// Every modeled CPU that must **reject** `0F 4x`.
///
/// Two exclusions, for different reasons. The 8086/8088 are excluded
/// because `0F` is the one-byte `POP CS` encoding there, so `0F 4x` never
/// reaches the two-byte map at all (MDD-BUG-CRUCIBLE-00659). P6-capable
/// identities are excluded because on them `0F 4x` IS CMOVcc and executes
/// — see `cmovcc_executes_on_p6_identities` for that side.
///
/// Derived from `CpuType::ALL` rather than hand-written. As a literal this
/// had 5 entries and silently omitted PentiumMmx, contradicting its own
/// doc comment: P55C was never actually covered by the CMOV-#UD tests.
fn two_byte_map_cpu_types() -> Vec<gp64_cpu::CpuType> {
    gp64_cpu::CpuType::ALL
        .into_iter()
        .filter(|t| {
            !matches!(t, gp64_cpu::CpuType::I8086 | gp64_cpu::CpuType::I8088) && !t.supports_p6()
        })
        .collect()
}

fn assert_cmov_ud(result: RunResult, cpu_type: gp64_cpu::CpuType, opcode: u8) {
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
        "{cpu_type:?} must #UD on CMOV opcode 0F {opcode:02X}, got {result:?}"
    );
}

#[test]
fn test_cmov_is_ud_for_every_modeled_cpu_with_register_source() {
    for cpu_type in two_byte_map_cpu_types() {
        for opcode in 0x40..=0x4F {
            let (mut cpu, mut mem) = setup(&[0x0F, opcode, 0xC3]); // CMOVcc AX, BX
            cpu.cpu_type = cpu_type;
            cpu.regs.eax = 0xCAFE_1111;
            cpu.regs.ebx = 0xBEEF_2222;
            cpu.regs
                .flags
                .insert(Flags::CARRY | Flags::PARITY | Flags::ZERO | Flags::SIGN | Flags::OVERFLOW);
            let saved_flags = cpu.regs.flags;

            let result = run(&mut cpu, &mut mem, &mut (), 1);

            assert_cmov_ud(result, cpu_type, opcode);
            assert_eq!(cpu.regs.eip, 0x0100, "#UD must restart at CMOV");
            assert_eq!(cpu.regs.eax, 0xCAFE_1111, "destination changed");
            assert_eq!(cpu.regs.ebx, 0xBEEF_2222, "source changed");
            assert_eq!(cpu.regs.flags, saved_flags, "flags changed");
        }
    }
}

#[test]
fn test_cmov_is_ud_for_every_modeled_cpu_with_memory_source() {
    for cpu_type in two_byte_map_cpu_types() {
        for opcode in 0x40..=0x4F {
            // CMOVcc AX, [0x0200] (16-bit address and operand sizes).
            let (mut cpu, mut mem) = setup(&[0x0F, opcode, 0x06, 0x00, 0x02]);
            cpu.cpu_type = cpu_type;
            cpu.regs.eax = 0xCAFE_1111;
            cpu.regs
                .flags
                .insert(Flags::CARRY | Flags::PARITY | Flags::ZERO | Flags::SIGN | Flags::OVERFLOW);
            mem.ram[0x0200..0x0202].copy_from_slice(&0x2222u16.to_le_bytes());
            let saved_flags = cpu.regs.flags;
            let saved_memory = mem.ram[0x0200..0x0202].to_vec();

            let result = run(&mut cpu, &mut mem, &mut (), 1);

            assert_cmov_ud(result, cpu_type, opcode);
            assert_eq!(cpu.regs.eip, 0x0100, "#UD must restart at CMOV");
            assert_eq!(cpu.regs.eax, 0xCAFE_1111, "destination changed");
            assert_eq!(cpu.regs.flags, saved_flags, "flags changed");
            assert_eq!(
                &mem.ram[0x0200..0x0202],
                saved_memory.as_slice(),
                "source memory changed"
            );
        }
    }
}

#[test]
fn test_cmovz_16bit_is_ud_on_pentium() {
    // CMOVZ AX, BX = 0F 44 C3 (modrm: mod=11, reg=AX(000), rm=BX(011))
    let (mut cpu, mut mem) = setup(&[0x0F, 0x44, 0xC3]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.eax = 0x1111;
    cpu.regs.ebx = 0x2222;
    cpu.regs.flags.insert(Flags::ZERO); // ZF=1 -> condition true
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_cmov_ud(result, cpu.cpu_type, 0x44);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0x1111);
    assert_eq!(cpu.regs.ebx, 0x2222);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmovz_16bit_false_condition_is_still_ud_on_pentium() {
    // CMOVZ AX, BX = 0F 44 C3
    let (mut cpu, mut mem) = setup(&[0x0F, 0x44, 0xC3]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.eax = 0x1111;
    cpu.regs.ebx = 0x2222;
    cpu.regs.flags.remove(Flags::ZERO); // ZF=0 -> condition false
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_cmov_ud(result, cpu.cpu_type, 0x44);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0x1111);
    assert_eq!(cpu.regs.ebx, 0x2222);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmovnz_32bit_is_ud_on_pentium() {
    // 66 prefix + CMOVNZ EAX, ECX = 66 0F 45 C1
    let (mut cpu, mut mem) = setup(&[0x66, 0x0F, 0x45, 0xC1]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.eax = 0xAAAA_BBBB;
    cpu.regs.ecx = 0xCCCC_DDDD;
    cpu.regs.flags.remove(Flags::ZERO); // ZF=0 -> CMOVNZ condition true
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_cmov_ud(result, cpu.cpu_type, 0x45);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0xAAAA_BBBB);
    assert_eq!(cpu.regs.ecx, 0xCCCC_DDDD);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmov_memory_operand_is_ud_on_pentium() {
    // CMOVZ AX, [0x0200] = 0F 44 06 00 02
    // modrm: mod=00, reg=AX(000), rm=110 (disp16)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x44, 0x06, 0x00, 0x02]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.eax = 0x1111;
    mem.ram[0x0200] = 0xBB;
    mem.ram[0x0201] = 0xAA;
    cpu.regs.flags.insert(Flags::ZERO); // ZF=1 -> condition true
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_cmov_ud(result, cpu.cpu_type, 0x44);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0x1111);
    assert_eq!(&mem.ram[0x0200..0x0202], &[0xBB, 0xAA]);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmovb_16bit_is_ud_on_pentium() {
    // CMOVB AX, CX = 0F 42 C1 (cc=2=B/C)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x42, 0xC1]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.eax = 0x1111;
    cpu.regs.ecx = 0x3333;
    cpu.regs.flags.insert(Flags::CARRY); // CF=1 -> CMOVB true
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_cmov_ud(result, cpu.cpu_type, 0x42);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0x1111);
    assert_eq!(cpu.regs.ecx, 0x3333);
    assert_eq!(cpu.regs.flags, saved_flags);
}

#[test]
fn test_cmovg_16bit_is_ud_on_pentium() {
    // CMOVG AX, DX = 0F 4F C2 (cc=F=G)
    let (mut cpu, mut mem) = setup(&[0x0F, 0x4F, 0xC2]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.eax = 0x1111;
    cpu.regs.edx = 0x5555;
    // CMOVG: ZF=0 AND SF==OF
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let saved_flags = cpu.regs.flags;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_cmov_ud(result, cpu.cpu_type, 0x4F);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.eax, 0x1111);
    assert_eq!(cpu.regs.edx, 0x5555);
    assert_eq!(cpu.regs.flags, saved_flags);
}

// ========================================================================
// DAA (0x27) — Decimal Adjust AL After Addition
// ========================================================================

#[test]
fn test_daa_basic() {
    let (mut cpu, mut mem) = setup(&[0x27]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFFFF00) | 0x0A;
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 0x10, "DAA: 0x0A should become 0x10");
    assert!(cpu.regs.flags.contains(Flags::AUX_CARRY));
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_daa_high_nibble() {
    let (mut cpu, mut mem) = setup(&[0x27]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFFFF00) | 0xA5;
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 0x05, "DAA: 0xA5 should become 0x05");
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_daa_both_nibbles() {
    let (mut cpu, mut mem) = setup(&[0x27]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFFFF00) | 0xFB;
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 0x61, "DAA: 0xFB should become 0x61");
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::AUX_CARRY));
}

// ========================================================================
// DAS (0x2F) — Decimal Adjust AL After Subtraction
// ========================================================================

#[test]
fn test_das_basic() {
    let (mut cpu, mut mem) = setup(&[0x2F]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFFFF00) | 0x1A;
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 0x14, "DAS: 0x1A should become 0x14");
    assert!(cpu.regs.flags.contains(Flags::AUX_CARRY));
}

#[test]
fn test_das_with_af_borrow() {
    let (mut cpu, mut mem) = setup(&[0x2F]);
    cpu.regs.eax &= 0xFFFFFF00;
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.insert(Flags::AUX_CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.eax as u8, 0xFA,
        "DAS: 0x00 with AF=1 should become 0xFA"
    );
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_das_old_al_high() {
    let (mut cpu, mut mem) = setup(&[0x2F]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFFFF00) | 0x9B;
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 0x35, "DAS: 0x9B should become 0x35");
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// ========================================================================
// AAA (0x37) — ASCII Adjust After Addition
// ========================================================================

#[test]
fn test_aaa_basic() {
    let (mut cpu, mut mem) = setup(&[0x37]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF0000) | 0x000A;
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.eax as u16, 0x0100,
        "AAA: AX=0x000A should become 0x0100"
    );
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::AUX_CARRY));
}

#[test]
fn test_aaa_no_adjust() {
    let (mut cpu, mut mem) = setup(&[0x37]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF0000) | 0x0305;
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 0x05, "AAA: AL should be 0x05");
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
    assert!(!cpu.regs.flags.contains(Flags::AUX_CARRY));
}

// ========================================================================
// AAS (0x3F) — ASCII Adjust After Subtraction
// ========================================================================

#[test]
fn test_aas_basic() {
    let (mut cpu, mut mem) = setup(&[0x3F]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF0000) | 0x01FF;
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.eax as u16, 0x0009,
        "AAS: AX=0x01FF should become 0x0009"
    );
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::AUX_CARRY));
}

#[test]
fn test_aas_no_adjust() {
    let (mut cpu, mut mem) = setup(&[0x3F]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF0000) | 0x0205;
    cpu.regs.flags.remove(Flags::AUX_CARRY);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u8, 0x05, "AAS: AL should be 0x05");
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
    assert!(!cpu.regs.flags.contains(Flags::AUX_CARRY));
}
