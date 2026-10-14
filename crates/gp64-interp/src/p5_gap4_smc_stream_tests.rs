use super::*;
use gp64_memory::Memory;
use gp64_types::SmcNotify;
use std::sync::{Arc, Mutex};

/// Test SMC notifier that records write notifications.
#[derive(Default)]
struct TestSmcNotify {
    writes: Mutex<Vec<(usize, usize)>>,
}

impl TestSmcNotify {
    fn snapshot(&self) -> Vec<(usize, usize)> {
        self.writes.lock().unwrap().clone()
    }
}

impl SmcNotify for TestSmcNotify {
    fn notify_write(&self, address: usize, size: usize) {
        self.writes.lock().unwrap().push((address, size));
    }
    fn notify_write_range(&self, _start: usize, _end: usize) {}
}

fn setup_with_smc(code: &[u8]) -> (Cpu, Memory, Arc<TestSmcNotify>) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);
    // See `smc_detection_tests::setup_with_smc`: the page gate in
    // `Memory::smc_before_write` filters by code page, so declare the whole
    // first megabyte as compiled-code pages and keep this suite testing the
    // write PLUMBING it was written to test.
    for page in 0..0x100u32 {
        mem.set_page_watched(page, true);
    }
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem, notifier)
}

// ---- Test 1: Overwrite next instruction, verify new opcode executes ----
//
// The fast interpreter re-fetches from RAM at the start of each instruction
// decode cycle, so writes to the NEXT instruction's bytes take effect
// immediately.

#[test]
fn test_gap4_overwrite_next_instruction() {
    // Layout at 0x0100:
    //   MOV BYTE [0x0106], 0x40  (overwrite the NOP at 0x0106 with INC AX=0x40)
    //   NOP                      (at 0x0106 — will be overwritten to INC AX)
    //
    // C6 06 06 01 40 = MOV BYTE PTR [0x0106], 0x40 (5 bytes: 0x0100-0x0104)
    // 90             = NOP at 0x0105 (padding)
    // 90             = NOP at 0x0106 (target — overwritten to 0x40 = INC AX)
    let code: &[u8] = &[
        0xC6, 0x06, 0x06, 0x01, 0x40, // MOV BYTE [0x0106], 0x40
        0x90, // NOP (padding)
        0x90, // NOP (will be overwritten to INC AX)
    ];
    let (mut cpu, mut mem, _notifier) = setup_with_smc(code);
    cpu.regs.set_ax(0);
    let mut io = ();

    // Run budget=3: MOV, NOP, then the overwritten byte
    let result = run(&mut cpu, &mut mem, &mut io, 3);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 3 }),
        "Expected 3 instructions, got {:?}",
        result
    );
    // The byte at 0x0106 was overwritten to 0x40 (INC AX), so AX should be 1
    assert_eq!(
        cpu.regs.ax(),
        1,
        "Next instruction should see the new opcode (INC AX)"
    );
}

// ---- Test 2: SMC notification fires on code-area write ----

#[test]
fn test_gap4_smc_notification_fires() {
    // MOV BYTE [0x5000], 0x42 — write to code-reachable area
    // C6 06 00 50 42 = MOV BYTE PTR [0x5000], 0x42
    let code: &[u8] = &[0xC6, 0x06, 0x00, 0x50, 0x42];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));

    let writes = notifier.snapshot();
    assert!(
        writes.iter().any(|&(addr, sz)| addr == 0x5000 && sz == 1),
        "Expected SMC notification at 0x5000, got: {:?}",
        writes
    );
}

// ---- Test 3: REP STOSB overwriting own stream ----
//
// Real 386+ prefetch queue semantics: once the REP instruction is fetched
// and decoding begins, the loop runs from internal microcode. Even if STOSB
// overwrites the REP STOSB bytes themselves, the loop completes.

#[test]
fn test_gap4_rep_stosb_overwrites_own_stream() {
    // Place REP STOSB at 0x0100, destination = 0x0100 (overwrites itself)
    // F3 AA = REP STOSB (2 bytes at 0x0100-0x0101)
    let code: &[u8] = &[0xF3, 0xAA];
    let (mut cpu, mut mem, _notifier) = setup_with_smc(code);
    cpu.regs.set_al(0xCC); // fill byte
    cpu.regs.set_cx(4); // count
    cpu.regs.set_di(0x0100); // destination = own instruction
    cpu.regs.edi = 0x0100;
    cpu.regs.ds = 0x0000;
    cpu.regs.es = 0x0000;
    cpu.regs.es_cache = SegmentCache::real_mode(0);
    // DF=0 (forward)
    cpu.regs.flags.remove(gp64_cpu::Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 4);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 4 }),
        "REP STOSB should complete all iterations, got {:?}",
        result
    );
    // CX should be 0 (all iterations completed)
    assert_eq!(cpu.regs.cx(), 0, "REP should complete all iterations");
    // Bytes at 0x0100..0x0104 should now be 0xCC
    for i in 0..4 {
        assert_eq!(
            mem.ram[0x0100 + i],
            0xCC,
            "Byte at 0x{:04X} should be overwritten",
            0x0100 + i
        );
    }
}

// ---- Test 4: Cross-instruction SMC (write ahead, later instruction sees new value) ----

#[test]
fn test_gap4_cross_instruction_smc_write_ahead() {
    // Write to an address several instructions ahead, then execute through to it.
    // The interpreter re-reads from RAM each decode, so the new value should be seen.
    //
    // Layout:
    //   0x0100: MOV BYTE [0x0107], 0xF4  (C6 06 07 01 F4) — overwrite byte at 0x0107 to HLT
    //   0x0105: NOP (90)
    //   0x0106: NOP (90)
    //   0x0107: NOP (90) — will be overwritten to HLT (0xF4)
    let code: &[u8] = &[
        0xC6, 0x06, 0x07, 0x01, 0xF4, // MOV BYTE [0x0107], 0xF4
        0x90, // NOP
        0x90, // NOP
        0x90, // NOP (target — overwritten to HLT)
    ];
    let (mut cpu, mut mem, _notifier) = setup_with_smc(code);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 32);
    // MOV(1) + NOP(2) + NOP(3) + HLT(4) = 4 instructions
    assert!(
        matches!(result, RunResult::Halt { executed: 4 }),
        "Expected Halt after 4 instructions (HLT from SMC), got {:?}",
        result
    );
}
