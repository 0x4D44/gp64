use super::test_helpers::setup;
use super::*;
use gp64_memory::Memory;

// ====================================================================
// F4: A20 gate handling
// ====================================================================

#[test]
fn test_a20_mask_applied_to_instruction_fetch() {
    // With A20 disabled, addresses with bit 20 set wrap to bit 20 cleared.
    // CS=0xFFF0, IP=0x1100 => linear = 0xFFF00 + 0x1100 = 0x101000 (bit 20 set)
    // A20 mask: 0x101000 & ~0x100000 = 0x001000
    // Place NOP at 0x001000.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    mem.ram[0x001000] = 0x90; // NOP
    cpu.regs.cs = 0xFFF0;
    cpu.regs.cs_cache = SegmentCache::real_mode(0xFFF0);
    cpu.regs.set_ip(0x1100);
    cpu.regs.eip = 0x1100;
    mem.set_a20(false);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "A20-disabled fetch should wrap to 0x001000 and execute NOP, got {:?}",
        result
    );
}

#[test]
fn test_a20_enabled_no_wrap() {
    // With A20 enabled, linear address 0x101000 should NOT wrap.
    // CS=0xFFF0, IP=0x1100 => linear = 0xFFF00 + 0x1100 = 0x101000
    // This address is valid in HMA (Memory allocates 1MB + 64KB).
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    mem.ram[0x101000] = 0x90; // NOP at HMA address
    cpu.regs.cs = 0xFFF0;
    cpu.regs.cs_cache = SegmentCache::real_mode(0xFFF0);
    cpu.regs.set_ip(0x1100);
    cpu.regs.eip = 0x1100;
    mem.set_a20(true);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "A20-enabled fetch at HMA should work, got {:?}",
        result
    );
}

#[test]
fn test_a20_enabled_data_access_no_wrap() {
    // Same setup but with A20 enabled — should read from 0x100010 (HMA)
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    mem.ram[0x000010] = 0x77;
    mem.ram[0x100010] = 0x99;

    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0200);
    cpu.regs.eip = 0x0200;
    mem.ram[0x0200] = 0x8A;
    mem.ram[0x0201] = 0x07;
    cpu.regs.ds = 0xFFFF;
    cpu.regs.ebx = 0x0020;
    mem.set_a20(true);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Should execute, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax & 0xFF,
        0x99,
        "With A20 enabled, should read from unwrapped address 0x100010"
    );
}

// ====================================================================
// M1: LOOP/LOOPZ/LOOPNZ/JCXZ with addr_size_32
// ====================================================================

#[test]
fn test_loop_ecx_addr32() {
    // addr32 prefix + LOOP: decrement full ECX.
    // ECX = 0x0001_0002 => after decrement ECX = 0x0001_0001 (not zero, branch taken)
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    mem.ram[0x0100] = 0x67; // addr-size prefix
    mem.ram[0x0101] = 0xE2; // LOOP
    mem.ram[0x0102] = 0x00; // rel8 = 0 (branch to 0x0103)
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.ecx = 0x0001_0002;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "LOOP with addr32 should execute, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ecx, 0x0001_0001,
        "ECX should be decremented as full 32-bit"
    );
}

#[test]
fn test_loop_cx_addr16() {
    // Without addr32 prefix, LOOP uses 16-bit CX only.
    // ECX = 0x0001_0001 => CX=1, decrement to 0, high word preserved.
    let (mut cpu, mut mem) = setup(&[0xE2, 0x00]); // LOOP +0
    cpu.regs.ecx = 0x0001_0001;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "LOOP with 16-bit addr should execute, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ecx, 0x0001_0000,
        "only CX should be decremented, high word preserved"
    );
}

#[test]
fn test_jcxz_ecx_addr32() {
    // JECXZ with addr32 prefix: branch if ECX==0.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    mem.ram[0x0100] = 0x67; // addr-size prefix
    mem.ram[0x0101] = 0xE3; // JCXZ/JECXZ
    mem.ram[0x0102] = 0x04; // rel8 = +4 (target = 0x0107)
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.ecx = 0x0000_0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ip(), 0x0107, "JECXZ should branch when ECX==0");
}

#[test]
fn test_jcxz_ecx_nonzero_addr32() {
    // JECXZ with addr32 prefix: ECX != 0 but CX == 0 => NOT taken.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    mem.ram[0x0100] = 0x67; // addr-size prefix
    mem.ram[0x0101] = 0xE3; // JCXZ/JECXZ
    mem.ram[0x0102] = 0x04; // rel8 = +4
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.ecx = 0x0001_0000; // CX=0 but ECX!=0
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.ip(),
        0x0103,
        "JECXZ should not branch when ECX!=0 (even if CX==0)"
    );
}

#[test]
fn test_jcxz_cx_zero_addr16() {
    // JCXZ without prefix: CX==0 => branch taken, even if ECX high word != 0.
    let (mut cpu, mut mem) = setup(&[0xE3, 0x04]); // JCXZ +4
    cpu.regs.ecx = 0x0001_0000; // CX=0, ECX high word set
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.ip(),
        0x0106,
        "JCXZ should branch when CX==0 regardless of ECX high word"
    );
}

#[test]
fn test_loopnz_ecx_addr32() {
    // LOOPNZ with addr32 prefix: decrement full ECX.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    mem.ram[0x0100] = 0x67; // addr-size prefix
    mem.ram[0x0101] = 0xE0; // LOOPNZ
    mem.ram[0x0102] = 0x00; // rel8 = 0
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.ecx = 0x0001_0001;
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(
        cpu.regs.ecx, 0x0001_0000,
        "LOOPNZ should decrement full ECX with addr32"
    );
}

#[test]
fn test_loopz_ecx_addr32() {
    // LOOPZ with addr32 prefix: decrement full ECX.
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    mem.ram[0x0100] = 0x67; // addr-size prefix
    mem.ram[0x0101] = 0xE1; // LOOPZ
    mem.ram[0x0102] = 0x00; // rel8 = 0
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.ecx = 0x0000_0002;
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ecx, 0x0000_0001, "LOOPZ should decrement ECX to 1");
}

// ====================================================================
// S8: Multi-byte NOP (0F 19-1F)
// ====================================================================

/// MDD-BUG-CRUCIBLE-00665 — `0F 1F /0` is P6+, so on every identity
/// gp64 currently models it must #UD.
///
/// These four cases were previously positive ("should not raise #UD")
/// and ran on `Cpu::new()`, whose default identity is `I80386` — so they
/// were asserting that a 386 executes an instruction Intel introduced
/// with the Pentium Pro. All four encoding forms are kept, because the
/// point is that the fault happens BEFORE any of their ModR/M, SIB, or
/// displacement bytes are interpreted.
#[test]
fn test_nop_0f_1f_slash0_is_ud_on_every_modeled_cpu() {
    // (bytes, description) — register form, memory form, disp8, disp16.
    let forms: [(&[u8], &str); 4] = [
        (&[0x0F, 0x1F, 0xC0], "0F 1F C0 (register form)"),
        (&[0x0F, 0x1F, 0x00], "0F 1F 00 (memory form)"),
        (&[0x0F, 0x1F, 0x44, 0x00], "0F 1F 44 00 (disp8)"),
        (&[0x0F, 0x1F, 0x84, 0x00, 0x00], "0F 1F 84 00 00 (disp16)"),
    ];
    // 8086/8088 are deliberately absent: there `0F` is not a two-byte
    // opcode escape at all but a one-byte POP CS, so those parts never
    // reach this arm and asserting #UD for them would be wrong.
    //
    // Derived from `CpuType::ALL`, not hand-written. As a literal this
    // list could never see a newly added identity, so the
    // `!supports_p6()` assertion below — whose message promises to fire
    // when the premise breaks — would simply never have been handed a P6
    // identity to reject. The forcing function did not force anything.
    let cpus: Vec<_> = gp64_cpu::CpuType::ALL
        .into_iter()
        .filter(|t| {
            !matches!(t, gp64_cpu::CpuType::I8086 | gp64_cpu::CpuType::I8088) && !t.supports_p6()
        })
        .collect();
    assert!(!cpus.is_empty());

    for cpu_type in cpus {
        assert!(
            !cpu_type.supports_p6(),
            "{cpu_type:?} is P6-capable — this test's premise (that every                  modeled identity is pre-P6) no longer holds, and the positive                  case must be restored for it"
        );
        for (bytes, what) in forms {
            let (mut cpu, mut mem) = setup(bytes);
            cpu.cpu_type = cpu_type;
            let eax_before = cpu.regs.eax;
            let mut io = ();

            let result = run(&mut cpu, &mut mem, &mut io, 1);

            assert!(
                matches!(
                    result,
                    RunResult::Exception {
                        vector: 6,
                        error_code: 0,
                        ..
                    }
                ),
                "{cpu_type:?}: {what} must #UD on a pre-P6 CPU, got {:?}",
                result
            );
            assert_eq!(
                cpu.regs.ip(),
                0x0100,
                "{cpu_type:?}: {what} — the faulting EIP must be the                      instruction start, not past its operand bytes"
            );
            assert_eq!(
                cpu.regs.eax, eax_before,
                "{cpu_type:?}: {what} must not change registers"
            );
        }
    }
}

#[test]
fn test_nop_0f_19_through_0f_1e() {
    // All opcodes 0F 19 through 0F 1E with register form should be NOP
    for op2 in 0x19..=0x1E {
        let (mut cpu, mut mem) = setup(&[0x0F, op2, 0xC0]);
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::BudgetExhausted { executed: 1 }),
            "0F {:02X} C0 should be NOP, got {:?}",
            op2,
            result
        );
        assert_eq!(
            cpu.regs.ip(),
            0x0103,
            "0F {:02X} C0: IP should advance 3 bytes",
            op2
        );
    }
}

// ====================================================================
// S9: RDTSC (0F 31)
// ====================================================================

#[test]
fn test_rdtsc_executes() {
    // 0F 31 = RDTSC
    let (mut cpu, mut mem) = setup(&[0x0F, 0x31]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.cycles = 0x0000_0001_2345_6789;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::BudgetExhausted { executed: 1 } => {}
        RunResult::Exception { vector: 6, .. } => {
            panic!("RDTSC raised #UD");
        }
        other => {
            assert!(
                !matches!(other, RunResult::Exception { vector: 6, .. }),
                "RDTSC should not raise #UD, got {:?}",
                other
            );
        }
    }
    assert_eq!(
        cpu.regs.eax, 0x2345_6789,
        "RDTSC: EAX should have low 32 bits of cycles"
    );
    assert_eq!(
        cpu.regs.edx, 0x0000_0001,
        "RDTSC: EDX should have high 32 bits of cycles"
    );
}

#[test]
fn test_rdtsc_observes_same_batch_charged_cycles() {
    // NOP; RDTSC in one interpreter batch: RDTSC observes the cycle charged
    // for the preceding NOP even though run_ext has not returned yet.
    let (mut cpu, mut mem) = setup(&[0x90, 0x0F, 0x31]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.cycles = 0x1000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "NOP; RDTSC should run in one batch, got {:?}",
        result
    );
    let observed = (cpu.regs.eax as u64) | ((cpu.regs.edx as u64) << 32);
    assert_eq!(observed, 0x1001);
}

#[test]
fn test_run_ext_initial_charge_biases_rdtsc_without_mutating_cycles() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x31]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.cycles = 0x1000;
    let mut io = ();
    let result = run_ext_with_initial_charge(&mut cpu, &mut mem, &mut io, &mut NoExec, 1, false, 7);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "RDTSC should run with initial charge bias, got {:?}",
        result
    );
    let observed = (cpu.regs.eax as u64) | ((cpu.regs.edx as u64) << 32);
    assert_eq!(observed, 0x1007);
    assert_eq!(cpu.cycles, 0x1000);
}

#[test]
fn test_rdtsc_rejected_pre_pentium() {
    // RDTSC on 386 (default cpu_type) should raise #UD.
    let (mut cpu, mut mem) = setup(&[0x0F, 0x31]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "RDTSC on 386 should raise #UD, got {:?}",
        result
    );
}

// ----------------------------------------------------------------
// CR4.TSD gates RDTSC to ring 0.
// Matrix: {CPL0, CPL3} x {TSD clear, TSD set} in PM, plus V86 with
// TSD set, real mode with TSD set, and the pre-Pentium #UD
// precedence.
// ----------------------------------------------------------------

const CR4_TSD: u32 = 1 << 2;

/// Pentium-class CPU with RDTSC at CS:IP and a known TSC value.
fn setup_rdtsc_pentium() -> (Cpu, Memory) {
    let (mut cpu, mem) = setup(&[0x0F, 0x31]);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.cycles = 0x0000_0001_2345_6789;
    (cpu, mem)
}

fn assert_rdtsc_read_tsc(cpu: &Cpu, ctx: &str) {
    assert_eq!(
        cpu.regs.eax, 0x2345_6789,
        "{ctx}: RDTSC should have written the low TSC half to EAX"
    );
    assert_eq!(
        cpu.regs.edx, 0x0000_0001,
        "{ctx}: RDTSC should have written the high TSC half to EDX"
    );
}

#[test]
fn rdtsc_pm_cpl0_tsd_set_proceeds() {
    // CR4.TSD only restricts CPL > 0. Ring 0 always reads the counter.
    let (mut cpu, mut mem) = setup_rdtsc_pentium();
    cpu.regs.cr0 |= 1; // PE=1
    cpu.regs.set_cpl(0);
    cpu.regs.cr4 |= CR4_TSD;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "RDTSC at CPL=0 with TSD set must proceed, got {:?}",
        result
    );
    assert_rdtsc_read_tsc(&cpu, "PM CPL0 TSD=1");
}

#[test]
fn rdtsc_pm_cpl3_tsd_clear_proceeds() {
    // With TSD clear, RDTSC is unprivileged — the default guest policy.
    let (mut cpu, mut mem) = setup_rdtsc_pentium();
    cpu.regs.cr0 |= 1; // PE=1
    cpu.regs.set_cpl(3);
    assert_eq!(cpu.regs.cr4 & CR4_TSD, 0, "TSD must start clear");
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "RDTSC at CPL=3 with TSD clear must proceed, got {:?}",
        result
    );
    assert_rdtsc_read_tsc(&cpu, "PM CPL3 TSD=0");
}

#[test]
fn rdtsc_pm_cpl3_tsd_set_raises_gp() {
    // The bug: this used to hand the guest the counter anyway.
    let (mut cpu, mut mem) = setup_rdtsc_pentium();
    cpu.regs.cr0 |= 1; // PE=1
    cpu.regs.set_cpl(3);
    cpu.regs.cr4 |= CR4_TSD;
    cpu.regs.eax = 0xDEAD_BEEF;
    cpu.regs.edx = 0xFEED_FACE;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "RDTSC at CPL=3 with TSD set must raise #GP(0), got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eip, 0x0100,
        "the #GP trap frame must point at the faulting RDTSC, not past it"
    );
    assert_eq!(
        (cpu.regs.eax, cpu.regs.edx),
        (0xDEAD_BEEF, 0xFEED_FACE),
        "a faulting RDTSC must not leak the counter into EDX:EAX"
    );
}

#[test]
fn rdtsc_v86_tsd_set_raises_gp_regardless_of_iopl() {
    // V86 is architecturally CPL 3, so TSD traps it even at IOPL=3.
    let (mut cpu, mut mem) = setup_rdtsc_pentium();
    cpu.regs.cr0 |= 1; // PE=1
    cpu.regs.set_cpl(3);
    cpu.regs.eflags_high |= 0x0002; // VM=1
    cpu.regs.flags |= gp64_cpu::Flags::from_bits_truncate(0x3000); // IOPL=3
    cpu.regs.cr4 |= CR4_TSD;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "RDTSC in V86 with TSD set must raise #GP(0), got {:?}",
        result
    );
}

#[test]
fn rdtsc_v86_tsd_clear_proceeds() {
    // TSD clear leaves V86 guests (the DOS-extender common case) reading
    // the counter as before.
    let (mut cpu, mut mem) = setup_rdtsc_pentium();
    cpu.regs.cr0 |= 1; // PE=1
    cpu.regs.set_cpl(3);
    cpu.regs.eflags_high |= 0x0002; // VM=1
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "RDTSC in V86 with TSD clear must proceed, got {:?}",
        result
    );
    assert_rdtsc_read_tsc(&cpu, "V86 TSD=0");
}

#[test]
fn rdtsc_real_mode_tsd_set_proceeds() {
    // Real mode runs at CPL 0 by definition; the stale CPL bookkeeping
    // must not synthesise a fault when CR0.PE is clear.
    let (mut cpu, mut mem) = setup_rdtsc_pentium();
    cpu.regs.set_cpl(3); // bookkeeping only — PE stays 0
    cpu.regs.cr4 |= CR4_TSD;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "RDTSC in real mode with TSD set must proceed, got {:?}",
        result
    );
    assert_rdtsc_read_tsc(&cpu, "real mode TSD=1");
}

#[test]
fn rdtsc_pre_pentium_ud_precedes_tsd_gp() {
    // #UD for a CPU with no TSC wins over the CR4.TSD #GP.
    let (mut cpu, mut mem) = setup(&[0x0F, 0x31]); // default 386
    cpu.regs.cr0 |= 1; // PE=1
    cpu.regs.set_cpl(3);
    cpu.regs.cr4 |= CR4_TSD;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "pre-Pentium RDTSC must still raise #UD, not #GP, got {:?}",
        result
    );
}
