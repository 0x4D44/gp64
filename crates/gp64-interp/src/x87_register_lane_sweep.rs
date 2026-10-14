use super::test_helpers::setup;
use super::*;

/// Pairwise-distinct, distinct-magnitude, non-special values with
/// inexact-result pairs (3.5/2.75, x*0.3) — direction and dst errors
/// must change visible state for every admitted op.
const STACK_VALS: [f32; 8] = [3.5, -1.25, 7.0, 0.125, -12.5, 2.75, 0.3, -9.875];
const DATA_BASE: usize = 0x2000;

fn run_case(esc: u8, modrm: u8, prefixed: bool, loaded: bool) -> String {
    let mut code = Vec::new();
    if loaded {
        // Preamble: 8x FLD m32 [disp16] — memory forms, never laned,
        // identical on both routes.
        for k in 0..8usize {
            let disp = (DATA_BASE + k * 4) as u16;
            code.extend_from_slice(&[0xD9, 0x06, disp as u8, (disp >> 8) as u8]);
        }
    }
    let instr_start = code.len();
    if prefixed {
        code.push(0x66);
    }
    code.extend_from_slice(&[esc, modrm, 0xF4]);
    let (mut cpu, mut mem) = setup(&code);
    for (k, v) in STACK_VALS.iter().enumerate() {
        let b = v.to_le_bytes();
        mem.ram[DATA_BASE + k * 4..DATA_BASE + k * 4 + 4].copy_from_slice(&b);
    }
    let mut io = ();
    let r = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 64, false);
    let (outcome, eip_norm) = match r {
        RunResult::Exception {
            vector, error_code, ..
        } => {
            // Fault EIP restores to the instruction start (prefix
            // included), which sits at the same offset on both routes.
            (format!("exc v={vector} e={error_code}"), cpu.regs.eip)
        }
        _ => {
            // Executed rows advance one extra byte on the prefixed arm.
            ("ran".to_string(), cpu.regs.eip - u32::from(prefixed))
        }
    };
    let _ = instr_start;
    let mut regs = String::new();
    for i in 0..8usize {
        for byte in cpu.fpu.get_st(i).0 {
            regs.push_str(&format!("{byte:02x}"));
        }
        regs.push(' ');
    }
    format!(
        "{outcome} eip={eip_norm:x} sw={:04x} tw={:04x} cw={:04x} top={} ax={:04x} fl={:04x} regs={regs}",
        cpu.fpu.status_word,
        cpu.fpu.tag_word,
        cpu.fpu.control_word,
        cpu.fpu.get_top(),
        cpu.regs.ax(),
        cpu.regs.flags.bits(),
    )
}

/// Non-vacuity pin: the sweep compares two routes, but if the lane
/// always DECLINED (e.g. a mis-gated ops-trace check) it would compare
/// full-vs-full and pass while proving nothing. Assert admission and
/// refusal directly.
#[test]
fn lane_admission_is_live_and_exact() {
    let (mut cpu, mut mem) = setup(&[0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 4, false);
    // Admitted: FADD ST0,ST(1) — returns true and mutates ST0 (both
    // empty -> masked-IE indefinite path still counts as execution).
    assert!(gp64_cpu::fpu::try_execute_register_lane(
        &mut cpu, 0xD8, 0xC1
    ));
    // Excluded: register FCOM (D8 D0..D7) and FCOMPP (DE D9) and a
    // prefix byte in b0 must all decline.
    assert!(!gp64_cpu::fpu::try_execute_register_lane(
        &mut cpu, 0xD8, 0xD0
    ));
    assert!(!gp64_cpu::fpu::try_execute_register_lane(
        &mut cpu, 0xDE, 0xD9
    ));
    assert!(!gp64_cpu::fpu::try_execute_register_lane(
        &mut cpu, 0x66, 0xD8
    ));
    assert!(!gp64_cpu::fpu::try_execute_register_lane(
        &mut cpu, 0xDD, 0xC8
    ));
}

/// EXP-IQ011 regression pin — FAILS before the inline register-lane
/// probe existed at the escape arm (the admitted escape then still
/// entered the outlined `op_fpu_esc`, incrementing `x87_outlined`).
/// Uses the always-present per-Cpu routing counters, so it runs (and
/// can fail) in every tier — the feature-gated census counters are
/// compiled out of default builds and cannot carry this pin (panel
/// finding, 2026.08.05 lever-trio HLD). Per-Cpu state also makes it
/// parallel-safe.
#[test]
fn admitted_register_escape_bypasses_the_outlined_route() {
    // Admitted register form: FADD ST(0),ST(1) — must take the inline
    // probe, never the outlined route.
    let (mut cpu, mut mem) = setup(&[0xD8, 0xC1, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 0,
        "admitted register form took the outlined route"
    );

    // Memory form: FLD m32 [disp16] — must take the outlined route.
    let (mut cpu, mut mem) = setup(&[0xD9, 0x06, 0x00, 0x20, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 1,
        "memory form must take the outlined route"
    );

    // Prefixed admitted-shape escape (66 D8 C1): the probe's pos gate
    // must decline it onto the outlined route (today's lane admission
    // set, reproduced exactly).
    let (mut cpu, mut mem) = setup(&[0x66, 0xD8, 0xC1, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 1,
        "prefixed escape must take the outlined route"
    );
}

#[test]
fn x87_register_lane_sweep_matches_the_full_route() {
    for esc in 0xD8u8..=0xDF {
        for modrm in 0xC0u8..=0xFF {
            for loaded in [true, false] {
                let lane = run_case(esc, modrm, false, loaded);
                let full = run_case(esc, modrm, true, loaded);
                assert_eq!(
                    lane, full,
                    "two-route divergence at {esc:02X} {modrm:02X} loaded={loaded}"
                );
            }
        }
    }
}
