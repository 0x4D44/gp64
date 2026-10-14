use super::test_helpers::setup;
use super::*;

// S1: REP STOSB caps at budget per call
#[test]
fn test_rep_stosb_caps_at_budget() {
    // F3 AA = REP STOSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x42);
    cpu.regs.set_cx(2048);
    cpu.regs.set_di(0x5000);
    // ES:DI destination area
    cpu.regs.es = 0x0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 128 }),
        "REP STOSB should yield after 128 iters, got {:?}",
        result
    );
    // CX should have 2048-128=1920 remaining
    assert_eq!(cpu.regs.cx(), 1920, "CX should be 1920 after first chunk");
    // IP should be rewound to the REP STOSB instruction start
    assert_eq!(
        cpu.regs.eip, 0x0100,
        "IP should be rewound to REP instruction"
    );
    // First 128 bytes should be filled
    for i in 0..128usize {
        assert_eq!(mem.ram[0x5000 + i], 0x42, "byte {} should be 0x42", i);
    }
    // Bytes beyond 128 should be untouched
    assert_eq!(mem.ram[0x5000 + 128], 0x00, "byte 128 should be untouched");

    // Run again: should process another 128 iterations
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 128 }),
        "second call should exhaust budget, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 1792, "CX should be 1792 after second chunk");
    // IP should still be rewound (not done yet)
    assert_eq!(cpu.regs.eip, 0x0100, "IP should be rewound");
    // First 256 bytes should be filled
    for i in 0..256usize {
        assert_eq!(
            mem.ram[0x5000 + i],
            0x42,
            "byte {} should be 0x42 after second chunk",
            i
        );
    }
}

// S1: REP MOVSB caps at budget per call
#[test]
fn test_rep_movsb_caps_at_budget() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA4]);
    // Fill source area with pattern
    for i in 0..2048usize {
        mem.ram[0x3000 + i] = (i & 0xFF) as u8;
    }
    cpu.regs.set_cx(2048);
    cpu.regs.set_si(0x3000);
    cpu.regs.set_di(0x6000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 128 }),
        "REP MOVSB should yield after 128 iters, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 1920, "CX should be 1920 after first chunk");
    assert_eq!(cpu.regs.eip, 0x0100, "IP should be rewound");
    // First 128 bytes should be copied
    for i in 0..128usize {
        assert_eq!(
            mem.ram[0x6000 + i],
            (i & 0xFF) as u8,
            "byte {} should match source",
            i
        );
    }
    // Beyond 128 untouched
    assert_eq!(mem.ram[0x6000 + 128], 0x00);
}

// S2: REPE CMPSB caps at budget when all data matches
#[test]
fn test_repe_cmpsb_caps_at_budget() {
    // F3 A6 = REPE CMPSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA6]);
    // Fill both source and dest with identical data
    for i in 0..2048usize {
        mem.ram[0x3000 + i] = 0xAA;
        mem.ram[0x6000 + i] = 0xAA;
    }
    cpu.regs.set_cx(2048);
    cpu.regs.set_si(0x3000);
    cpu.regs.set_di(0x6000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 128 }),
        "REPE CMPSB should yield after 128 iters, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 1920, "CX should be 1920 after first chunk");
    assert_eq!(cpu.regs.eip, 0x0100, "IP should be rewound");
}

// S2: REPE CMPSB with early mismatch does NOT rewind
#[test]
fn test_repe_cmpsb_mismatch_no_rewind() {
    // F3 A6 = REPE CMPSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xA6]);
    // All matching except position 10
    for i in 0..2048usize {
        mem.ram[0x3000 + i] = 0xAA;
        mem.ram[0x6000 + i] = 0xAA;
    }
    mem.ram[0x6000 + 10] = 0xBB; // mismatch at element 10
    cpu.regs.set_cx(2048);
    cpu.regs.set_si(0x3000);
    cpu.regs.set_di(0x6000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    // Budget=11 matches the 11 iterations before mismatch terminates the REP
    let result = run(&mut cpu, &mut mem, &mut io, 11);
    // Should complete in one call (mismatch before budget)
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 11 }),
        "got {:?}",
        result
    );
    // CX should reflect early termination (2048 - 11 = 2037)
    assert_eq!(cpu.regs.cx(), 2037, "CX should reflect early termination");
    // IP should advance past instruction (not rewound)
    assert_eq!(cpu.regs.eip, 0x0102, "IP should advance past instruction");
}

// S2: REPNE SCASB caps at budget when no match found
#[test]
fn test_repne_scasb_caps_at_budget() {
    // F2 AE = REPNE SCASB
    let (mut cpu, mut mem) = setup(&[0xF2, 0xAE]);
    // Fill dest with 0x00, search for 0xFF (no match)
    for i in 0..2048usize {
        mem.ram[0x6000 + i] = 0x00;
    }
    cpu.regs.set_al(0xFF);
    cpu.regs.set_cx(2048);
    cpu.regs.set_di(0x6000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 128 }),
        "REPNE SCASB should yield after 128 iters, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 1920, "CX should be 1920 after first chunk");
    assert_eq!(cpu.regs.eip, 0x0100, "IP should be rewound");
}

// Small REP (count < budget) should NOT rewind
#[test]
fn test_rep_stosb_small_count_no_rewind() {
    // F3 AA = REP STOSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x55);
    cpu.regs.set_cx(100);
    cpu.regs.set_di(0x5000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 100);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 100 }),
        "got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0");
    assert_eq!(cpu.regs.eip, 0x0102, "IP should advance past instruction");
    for i in 0..100usize {
        assert_eq!(mem.ram[0x5000 + i], 0x55);
    }
}

// REP LODSB caps at budget
#[test]
fn test_rep_lodsb_caps_at_budget() {
    // F3 AC = REP LODSB
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAC]);
    for i in 0..2048usize {
        mem.ram[0x3000 + i] = (i & 0xFF) as u8;
    }
    cpu.regs.set_cx(2048);
    cpu.regs.set_si(0x3000);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 128 }),
        "REP LODSB should yield after 128 iters, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 1920, "CX should be 1920 after first chunk");
    assert_eq!(cpu.regs.eip, 0x0100, "IP should be rewound");
    // AL should contain the last loaded byte (byte at offset 127)
    assert_eq!(cpu.regs.al(), (127 & 0xFF) as u8);
}

// REP STOSW caps at budget
#[test]
fn test_rep_stosw_caps_at_budget() {
    // F3 AB = REP STOSW
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAB]);
    cpu.regs.set_ax(0x1234);
    cpu.regs.set_cx(2048);
    cpu.regs.set_di(0x5000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 128 }),
        "REP STOSW should yield after 128 iters, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 1920, "CX should be 1920 after first chunk");
    assert_eq!(cpu.regs.eip, 0x0100, "IP should be rewound");
}

// V3: REP STOSB completes within budget
#[test]
fn test_rep_stosb_completes_within_budget() {
    // F3 AA = REP STOSB (2 bytes)
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x77);
    cpu.regs.set_cx(50);
    cpu.regs.set_di(0x5000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    // Budget=50: exactly enough to complete the REP
    let result = run(&mut cpu, &mut mem, &mut io, 50);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 50 }),
        "REP STOSB should complete within budget, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0 after completion");
    // IP should advance past the 2-byte REP STOSB instruction
    assert_eq!(cpu.regs.eip, 0x0102, "IP should advance past instruction");
    // Verify all 50 bytes were filled
    for i in 0..50usize {
        assert_eq!(mem.ram[0x5000 + i], 0x77, "byte {} should be 0x77", i);
    }
}

// V3: REP STOSB with CX=0 costs one cycle (common epilogue)
#[test]
fn test_rep_stosb_cx_zero_costs_one_cycle() {
    // F3 AA = REP STOSB (2 bytes)
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x99);
    cpu.regs.set_cx(0);
    cpu.regs.set_di(0x5000);
    cpu.regs.es = 0x0000;

    let mut io = ();
    // Budget=1: CX=0 means 0 REP iterations; the instruction falls through
    // to the common epilogue which costs 1 cycle
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.cx(), 0, "CX should remain 0");
    // IP should advance past the instruction
    assert_eq!(cpu.regs.eip, 0x0102, "IP should advance past instruction");
    // executed should be 1 from the common epilogue
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CX=0 REP should cost 1 cycle, got {:?}",
        result
    );
}

// V3: REP STOSB multi-batch completion
#[test]
fn test_rep_stosb_multi_batch_completion() {
    // F3 AA = REP STOSB (2 bytes)
    let (mut cpu, mut mem) = setup(&[0xF3, 0xAA]);
    cpu.regs.set_al(0xDD);
    cpu.regs.set_cx(200);
    cpu.regs.set_di(0x5000);
    cpu.regs.es = 0x0000;

    let mut io = ();

    // First batch: budget=128, should process 128 iterations
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 128 }),
        "first batch should exhaust budget, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.cx(),
        72,
        "CX should be 200-128=72 after first batch"
    );
    assert_eq!(
        cpu.regs.eip, 0x0100,
        "IP should be rewound after partial REP"
    );

    // Second batch: budget=128, should process remaining 72 iterations
    // Budget=72 to stop exactly after REP completes
    let result = run(&mut cpu, &mut mem, &mut io, 72);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 72 }),
        "second batch should complete REP, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cx(), 0, "CX should be 0 after completion");
    assert_eq!(cpu.regs.eip, 0x0102, "IP should advance past instruction");

    // Verify all 200 bytes were filled
    for i in 0..200usize {
        assert_eq!(mem.ram[0x5000 + i], 0xDD, "byte {} should be 0xDD", i);
    }
}
