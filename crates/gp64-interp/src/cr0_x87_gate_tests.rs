use super::test_helpers::setup;
use super::*;
use gp64_cpu::x87_ops::F80;

#[derive(Debug, PartialEq, Eq)]
struct X87State {
    regs: [[u8; 10]; 8],
    status: u16,
    control: u16,
    tag: u16,
    ip_offset: u32,
    ip_selector: u16,
    operand_offset: u32,
    operand_selector: u16,
    save_area: [u8; 108],
}

impl X87State {
    fn capture(cpu: &Cpu) -> Self {
        Self {
            regs: std::array::from_fn(|index| cpu.fpu.regs[index].0),
            status: cpu.fpu.status_word,
            control: cpu.fpu.control_word,
            tag: cpu.fpu.tag_word,
            ip_offset: cpu.fpu.ip_offset,
            ip_selector: cpu.fpu.ip_selector,
            operand_offset: cpu.fpu.operand_offset,
            operand_selector: cpu.fpu.operand_selector,
            save_area: cpu.fpu.x87_save_area,
        }
    }
}

fn assert_nm(result: RunResult, cpu: &Cpu) {
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 7,
                error_code: 0,
                fault_addr: None,
                executed: 0,
            }
        ),
        "expected restartable #NM, got {result:?}"
    );
    assert_eq!(cpu.regs.eip, 0x0100, "#NM must preserve the faulting EIP");
}

#[test]
fn cr0_x87_gate_pm_em_faults_before_register_form_effects() {
    let (mut cpu, mut mem) = setup(&[0xD9, 0xE8]); // FLD1
    cpu.regs.cr0 = 1 | (1 << 2); // PE | EM
    cpu.fpu.push(F80::from_f64(12.5));
    let before = X87State::capture(&cpu);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert_nm(result, &cpu);
    assert_eq!(X87State::capture(&cpu), before);
}

#[test]
fn cr0_x87_gate_v86_ts_faults_before_register_form_effects() {
    let (mut cpu, mut mem) = setup(&[0xD9, 0xE8]); // FLD1
    cpu.regs.cr0 = 1 | (1 << 3); // PE | TS
    cpu.regs.set_v86_mode(true);
    cpu.regs.cpl = 3;
    cpu.fpu.push(F80::from_f64(-7.25));
    let before = X87State::capture(&cpu);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert_nm(result, &cpu);
    assert_eq!(X87State::capture(&cpu), before);
}

#[test]
fn cr0_x87_gate_pm_ts_faults_before_memory_store_or_pop() {
    // FSTP dword [0200h].
    let (mut cpu, mut mem) = setup(&[0xD9, 0x1E, 0x00, 0x02]);
    cpu.regs.cr0 = 1 | (1 << 3); // PE | TS
    cpu.fpu.push(F80::from_f64(3.75));
    let before = X87State::capture(&cpu);
    const SENTINEL: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
    mem.ram[0x0200..0x0204].copy_from_slice(&SENTINEL);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert_nm(result, &cpu);
    assert_eq!(&mem.ram[0x0200..0x0204], &SENTINEL);
    assert_eq!(X87State::capture(&cpu), before);
}

#[test]
fn cr0_x87_gate_fwait_mp_and_ts_faults_in_protected_mode() {
    let (mut cpu, mut mem) = setup(&[0x9B]); // FWAIT
    cpu.regs.cr0 = 1 | (1 << 1) | (1 << 3); // PE | MP | TS

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert_nm(result, &cpu);
}

#[test]
fn cr0_x87_gate_fwait_requires_both_mp_and_ts() {
    for cr0 in [1 | (1 << 1), 1 | (1 << 3)] {
        let (mut cpu, mut mem) = setup(&[0x9B]); // FWAIT
        cpu.regs.cr0 = cr0;

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(result, RunResult::BudgetExhausted { executed: 1 }),
            "FWAIT must not #NM unless both CR0.MP and CR0.TS are set: {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x0101);
    }
}
