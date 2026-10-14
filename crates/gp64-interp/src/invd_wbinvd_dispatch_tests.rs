use super::test_helpers::setup;
use super::*;

const CACHE_OPS: [(&str, u8); 2] = [("INVD", 0x08), ("WBINVD", 0x09)];

#[test]
fn invd_wbinvd_pre_486_raise_ud_before_privilege_checks() {
    for (name, opcode) in CACHE_OPS {
        let (mut cpu, mut mem) = setup(&[0x0F, opcode]);
        cpu.set_cpu_type(gp64_cpu::CpuType::I80386);
        cpu.regs.cr0 |= 1;
        cpu.regs.set_cpl(3);
        cpu.regs.flags = Flags::RESERVED1
            | Flags::CARRY
            | Flags::PARITY
            | Flags::ZERO
            | Flags::SIGN
            | Flags::OVERFLOW;
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
            "386 {name} must #UD before privilege checks, got {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x0100, "386 {name} advanced EIP");
        assert_eq!(cpu.regs.flags, flags_before, "386 {name} changed FLAGS");
    }
}

#[test]
fn invd_wbinvd_execute_on_486_plus_in_real_mode_and_pm_cpl0() {
    for cpu_type in [gp64_cpu::CpuType::I80486, gp64_cpu::CpuType::PentiumMmx] {
        for protected_mode in [false, true] {
            for (name, opcode) in CACHE_OPS {
                let (mut cpu, mut mem) = setup(&[0x0F, opcode]);
                cpu.set_cpu_type(cpu_type);
                if protected_mode {
                    cpu.regs.cr0 |= 1;
                }
                cpu.regs.set_cpl(0);

                let result = run(&mut cpu, &mut mem, &mut (), 1);

                assert!(
                    matches!(result, RunResult::BudgetExhausted { executed: 1 }),
                    "{cpu_type:?} {name} at protected_mode={protected_mode} CPL0 should execute, got {result:?}"
                );
                assert_eq!(cpu.regs.eip, 0x0102);
            }
        }
    }
}

#[test]
fn invd_wbinvd_raise_gp_at_protected_mode_cpl3() {
    for (name, opcode) in CACHE_OPS {
        let (mut cpu, mut mem) = setup(&[0x0F, opcode]);
        cpu.set_cpu_type(gp64_cpu::CpuType::I80486);
        cpu.regs.cr0 |= 1;
        cpu.regs.set_cpl(3);
        let flags_before = cpu.regs.flags;

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 13,
                    error_code: 0,
                    fault_addr: None,
                    executed: 0,
                }
            ),
            "486 {name} at CPL3 must #GP(0), got {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x0100, "486 {name} advanced EIP");
        assert_eq!(cpu.regs.flags, flags_before, "486 {name} changed FLAGS");
    }
}

#[test]
fn invd_wbinvd_raise_gp_in_v86_regardless_of_iopl() {
    for (name, opcode) in CACHE_OPS {
        let (mut cpu, mut mem) = setup(&[0x0F, opcode]);
        cpu.set_cpu_type(gp64_cpu::CpuType::PentiumMmx);
        cpu.regs.cr0 |= 1;
        cpu.regs.set_v86_mode(true);
        cpu.regs.flags.insert(Flags::IOPL0 | Flags::IOPL1);

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 13,
                    error_code: 0,
                    fault_addr: None,
                    executed: 0,
                }
            ),
            "V86 {name} must #GP(0) even at IOPL3, got {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x0100, "V86 {name} advanced EIP");
    }
}
