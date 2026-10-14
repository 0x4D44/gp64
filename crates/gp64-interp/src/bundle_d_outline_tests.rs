// Regression guards for perf Bundle D: the 0x8E (MOV Sreg,r/m16), 0x8C
// the hot `match op` dispatch into cold `#[cold]` fns. Behaviour must be
// byte-identical, so these exercise each outlined path and pin the observable
// results (loaded seg reg + cache base, #UD vectors, #GP-vs-Halt).
use super::test_helpers::setup;
use super::*;

// --- 0x8E MOV Sreg, r/m16 (register form) ---

#[test]
fn bundle_d_mov_es_reg() {
    // MOV ES, AX = 8E C0 (seg_idx=ES(0), rm=AX(0)).
    let (mut cpu, mut mem) = setup(&[0x8E, 0xC0]);
    cpu.regs.eax = 0x1234;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.es, 0x1234);
    assert_eq!(cpu.regs.es_cache.base, 0x1234 << 4);
}

#[test]
fn bundle_d_mov_ds_reg() {
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD8]); // MOV DS, AX
    cpu.regs.eax = 0x6000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.ds, 0x6000);
    assert_eq!(cpu.regs.ds_cache.base, 0x6000 << 4);
}

#[test]
fn bundle_d_mov_ss_reg_sets_inhibit() {
    let (mut cpu, mut mem) = setup(&[0x8E, 0xD0]); // MOV SS, AX
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

#[test]
fn bundle_d_mov_fs_reg() {
    let (mut cpu, mut mem) = setup(&[0x8E, 0xE0]); // MOV FS, AX
    cpu.regs.eax = 0x7000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.fs, 0x7000);
    assert_eq!(cpu.regs.fs_cache.base, 0x7000 << 4);
}

#[test]
fn bundle_d_mov_gs_reg() {
    let (mut cpu, mut mem) = setup(&[0x8E, 0xE8]); // MOV GS, AX
    cpu.regs.eax = 0x8000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.gs, 0x8000);
    assert_eq!(cpu.regs.gs_cache.base, 0x8000 << 4);
}

// --- 0x8E MOV Sreg, [mem] (memory form) ---

#[test]
fn bundle_d_mov_es_from_memory() {
    // MOV ES, [0x3000] = 8E 06 00 30 (seg_idx=ES(0), rm=6 → disp16 direct).
    // DS base is 0 in real-mode setup, so the operand linear addr == 0x3000.
    let (mut cpu, mut mem) = setup(&[0x8E, 0x06, 0x00, 0x30]);
    mem.ram[0x3000] = 0x00;
    mem.ram[0x3001] = 0x20; // little-endian word 0x2000
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.es, 0x2000);
    assert_eq!(cpu.regs.es_cache.base, 0x2000 << 4);
}

// --- 0x8E #UD paths ---

#[test]
fn bundle_d_mov_to_cs_is_ud() {
    // MOV CS, AX = 8E C8 (seg_idx=CS(1)) → #UD(6), no state change.
    let (mut cpu, mut mem) = setup(&[0x8E, 0xC8]);
    cpu.regs.eax = 0x1234;
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
        "MOV CS must #UD, got {:?}",
        result
    );
}

#[test]
fn bundle_d_mov_to_sreg_reserved_is_ud() {
    // seg_idx=6 reserved: 8E F0 (reg=110). #UD(6) before any operand read.
    let (mut cpu, mut mem) = setup(&[0x8E, 0xF0]);
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
        "reserved MOV Sreg must #UD, got {:?}",
        result
    );
}

// --- 0x8C MOV r/m16, Sreg ---

#[test]
fn bundle_d_mov_from_es_reg() {
    // MOV AX, ES = 8C C0 (seg_idx=ES(0), rm=AX(0)).
    let (mut cpu, mut mem) = setup(&[0x8C, 0xC0]);
    cpu.regs.es = 0x1234;
    cpu.regs.eax = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax as u16, 0x1234);
}

#[test]
fn bundle_d_mov_from_es_to_memory() {
    // MOV [0x3000], ES = 8C 06 00 30. DS base 0 → linear 0x3000.
    let (mut cpu, mut mem) = setup(&[0x8C, 0x06, 0x00, 0x30]);
    cpu.regs.es = 0x1234;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.ram[0x3000], 0x34);
    assert_eq!(mem.ram[0x3001], 0x12);
}

#[test]
fn bundle_d_mov_from_sreg_reserved_is_ud() {
    // seg_idx=6 reserved dest: 8C F0. #UD(6) before any dest write.
    let (mut cpu, mut mem) = setup(&[0x8C, 0xF0]);
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
        "reserved MOV r/m,Sreg must #UD, got {:?}",
        result
    );
}

// --- 0xF4 HLT ---

#[test]
fn bundle_d_hlt_real_mode_halts() {
    let (mut cpu, mut mem) = setup(&[0xF4]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Halt { executed: 1 }));
    assert!(cpu.halted);
}

#[test]
fn bundle_d_hlt_pm_cpl0_halts() {
    let (mut cpu, mut mem) = setup(&[0xF4]);
    cpu.regs.cr0 |= 1; // protected mode
    cpu.regs.cpl = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Halt { executed: 1 }));
    assert!(cpu.halted);
}

#[test]
fn bundle_d_hlt_pm_cpl3_raises_gp() {
    let (mut cpu, mut mem) = setup(&[0xF4]);
    cpu.regs.cr0 |= 1; // protected mode
    cpu.regs.cpl = 3;
    cpu.regs.cs = 0x001B; // RPL=3
    cpu.regs.cs_cache.base = 0;
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
        "HLT at CPL=3 must #GP(0), got {:?}",
        result
    );
    assert!(!cpu.halted);
}
