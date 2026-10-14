use super::test_helpers::setup;
use super::*;
use gp64_cpu::msr::{IA32_APIC_BASE_DEFAULT, MSR_IA32_TIME_STAMP_COUNTER};

/// Common starting point: real-mode, Pentium-class CPU, with the
/// requested two-byte opcode at CS:IP.
fn setup_pentium(code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mem) = setup(code);
    cpu.cpu_type = gp64_cpu::CpuType::Pentium;
    cpu.regs.ecx = 0;
    (cpu, mem)
}

#[test]
fn rdmsr_pre_pentium_raises_ud() {
    // RDMSR on a default 386 CPU must take the #UD path before any
    // MSR table lookup or CPL gate is consulted.
    let (mut cpu, mut mem) = setup(&[0x0F, 0x32]);
    cpu.regs.ecx = MSR_IA32_TIME_STAMP_COUNTER;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "RDMSR on 386 should raise #UD, got {:?}",
        result
    );
}

#[test]
fn wrmsr_pre_pentium_raises_ud() {
    let (mut cpu, mut mem) = setup(&[0x0F, 0x30]);
    cpu.regs.ecx = MSR_IA32_TIME_STAMP_COUNTER;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 6, .. }),
        "WRMSR on 386 should raise #UD, got {:?}",
        result
    );
}

#[test]
fn rdmsr_tsc_returns_cycle_counter() {
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x32]);
    cpu.cycles = 0x0000_0001_2345_6789;
    cpu.regs.ecx = MSR_IA32_TIME_STAMP_COUNTER;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "RDMSR(0x10) should not fault, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax, 0x2345_6789, "EAX = low 32 bits of TSC");
    assert_eq!(cpu.regs.edx, 0x0000_0001, "EDX = high 32 bits of TSC");
}

#[test]
fn rdmsr_apic_base_returns_default() {
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x32]);
    cpu.regs.ecx = gp64_cpu::msr::MSR_IA32_APIC_BASE;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "RDMSR(0x1B) should not fault, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax, IA32_APIC_BASE_DEFAULT as u32);
    assert_eq!(cpu.regs.edx, (IA32_APIC_BASE_DEFAULT >> 32) as u32);
}

#[test]
fn rdmsr_bios_sign_id_returns_zero() {
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x32]);
    cpu.regs.ecx = gp64_cpu::msr::MSR_IA32_BIOS_SIGN_ID;
    // Pre-load garbage to ensure the read clears it.
    cpu.regs.eax = 0xDEAD_BEEF;
    cpu.regs.edx = 0xCAFE_BABE;
    let mut io = ();
    run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0);
    assert_eq!(cpu.regs.edx, 0);
}

#[test]
fn wrmsr_tsc_rebase_observed_by_subsequent_rdtsc() {
    // WRMSR(0x10, V); RDTSC in one interpreter batch. WRMSR rebases at
    // the architectural cycle at its instruction point; the following
    // RDTSC observes the one charged cycle between the two instructions.
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x30, 0x0F, 0x31]);
    cpu.regs.ecx = MSR_IA32_TIME_STAMP_COUNTER;
    cpu.regs.eax = 0x1000_0000;
    cpu.regs.edx = 0;
    cpu.cycles = 5; // arbitrary "real" cycle count
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "WRMSR + RDTSC pair should run to completion, got {:?}",
        result
    );
    let observed = (cpu.regs.eax as u64) | ((cpu.regs.edx as u64) << 32);
    assert_eq!(observed, 0x1000_0001);
}

#[test]
fn rdmsr_unknown_strict_raises_gp() {
    // Strict mode (the production default) must raise #GP(0) for any
    // unknown MSR. We rely on the env-var-backed cache being unset in
    // this test process. The gp64-cpu::msr unit tests pin strict
    // mode via the test-only ScopedLenientOverride and exercise the
    // policy directly; this dispatch test asserts the interpreter
    // wires the GP fault back through to RunResult::Exception.
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x32]);
    cpu.regs.ecx = 0xDEAD_BEEF;
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
        "RDMSR of an unknown index must raise #GP(0), got {:?}",
        result
    );
}

#[test]
fn rdmsr_pm_cpl3_raises_gp() {
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x32]);
    cpu.regs.cr0 |= 1; // enter protected mode
    cpu.regs.set_cpl(3);
    cpu.regs.ecx = MSR_IA32_TIME_STAMP_COUNTER;
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
        "RDMSR at CPL=3 in PM should raise #GP(0), got {:?}",
        result
    );
}

#[test]
fn wrmsr_pm_cpl3_raises_gp() {
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x30]);
    cpu.regs.cr0 |= 1;
    cpu.regs.set_cpl(3);
    cpu.regs.ecx = MSR_IA32_TIME_STAMP_COUNTER;
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
        "WRMSR at CPL=3 in PM should raise #GP(0), got {:?}",
        result
    );
}

#[test]
fn rdmsr_v86_raises_gp_regardless_of_iopl() {
    // V86 (PE=1, EFLAGS.VM=1) must trap RDMSR even if IOPL=3, per SDM.
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x32]);
    cpu.regs.cr0 |= 1; // PE=1
    cpu.regs.set_cpl(3); // V86 always implies CPL=3
    cpu.regs.eflags_high |= 0x0002; // VM=1 (bit 17 of EFLAGS)
                                    // IOPL bits in low EFLAGS = bits 12..14 (= 0x3000 for IOPL=3).
    cpu.regs.flags |= gp64_cpu::Flags::from_bits_truncate(0x3000);
    cpu.regs.ecx = MSR_IA32_TIME_STAMP_COUNTER;
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
        "RDMSR in V86 mode should raise #GP(0), got {:?}",
        result
    );
}

#[test]
fn rdmsr_real_mode_cpl3_proceeds() {
    // Real mode: cr0.PE=0, CPL field in cs_cache is undefined but
    // gating only applies when pm=true. The instruction must execute.
    let (mut cpu, mut mem) = setup_pentium(&[0x0F, 0x32]);
    // cr0.PE remains 0 (real mode). Even with cpl bookkeeping = 3,
    // the gate is skipped because pm is false.
    cpu.regs.set_cpl(3);
    cpu.regs.ecx = MSR_IA32_TIME_STAMP_COUNTER;
    cpu.cycles = 0xCAFE_BABE;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "RDMSR in real mode must always proceed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax, 0xCAFE_BABE);
}
