use super::test_helpers::{make_descriptor, write_gdt_entry};
use super::*;
use gp64_memory::Memory;

/// Set up PM with a code segment at selector 0x08 with a specific limit.
/// access=0x9A: present, ring-0, code, readable. flags=0x00: byte granularity, 16-bit.
fn setup_pm_with_cs_limit(cpu: &mut Cpu, mem: &mut Memory, limit: u32) {
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 3 * 8 - 1;
    write_gdt_entry(mem, gdt_base as usize, [0u8; 8]); // null
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, limit, 0x9A, 0x00), // code, byte granularity, 16-bit
    );
    write_gdt_entry(
        mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFF, 0x92, 0x00), // data, byte granularity
    );
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1; // PE=1
    cpu.regs.cpl = 0;

    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(mem, 0x08).unwrap();
    cpu.cs_bits = if cpu.regs.cs_cache.big { 32 } else { 16 };

    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(mem, 0x10).unwrap();
    cpu.regs.ss_cache.is_writable = true;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;
}

fn assert_gp0(result: RunResult, context: &str) {
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "{} should raise #GP(0), got {:?}",
        context,
        result
    );
}

#[test]
fn test_cs_limit_exceeded_raises_gp() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    // CS limit = 0x1000 (4KB, byte granularity)
    setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);

    // Place NOP at EIP=0x1001 — past the limit
    cpu.regs.set_ip(0x1001);
    cpu.regs.eip = 0x1001;
    mem.ram[0x1001] = 0x90; // NOP

    let result = run(&mut cpu, &mut mem, &mut io, 32);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "EIP past CS limit should raise #GP(0), got {:?}",
        result
    );
}

#[test]
fn test_cs_limit_at_boundary_executes() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    // CS limit = 0x1000
    setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);

    // Place NOP at EIP=0x1000 — exactly at the limit (should execute)
    cpu.regs.set_ip(0x1000);
    cpu.regs.eip = 0x1000;
    mem.ram[0x1000] = 0x90; // NOP

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // Should NOT be a #GP for the first instruction — ip_val == limit is
    // valid (limit is the last valid byte). A larger budget would correctly
    // #GP on the following fetch at limit+1.
    assert!(
        !matches!(result, RunResult::Exception { vector: 13, .. }),
        "EIP at CS limit should execute, not #GP. Got {:?}",
        result
    );
}

#[test]
fn test_cs_limit_prefix_continuation_rechecks_instruction_bytes() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);

    // Operand-size prefix at the last valid byte; the opcode byte is beyond
    // the code segment limit and must fault before the instruction executes.
    cpu.regs.set_ip(0x1000);
    cpu.regs.eip = 0x1000;
    mem.ram[0x1000] = 0x66;
    mem.ram[0x1001] = 0x90; // NOP, must not execute.

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_gp0(
        result,
        "instruction byte fetch after a prefix at the CS limit",
    );
}

#[test]
fn test_cs_limit_multibyte_instruction_rechecks_immediate_byte() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);

    // MOV AL,imm8 starts at the last valid byte, but its immediate byte is
    // outside the code segment. It must fault before committing AL.
    cpu.regs.set_ip(0x1000);
    cpu.regs.eip = 0x1000;
    cpu.regs.eax = 0x1234_5678;
    mem.ram[0x1000] = 0xB0;
    mem.ram[0x1001] = 0x42;

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_gp0(result, "MOV AL,imm8 immediate byte beyond the CS limit");
    assert_eq!(cpu.regs.al(), 0x78, "MOV must not commit before #GP");
}

#[test]
fn test_cs_limit_two_byte_opcode_rechecks_second_opcode_byte() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);

    // 0F starts an escaped opcode. The second opcode byte is still an
    // instruction fetch and must fault if it is beyond the code segment.
    cpu.regs.set_ip(0x1000);
    cpu.regs.eip = 0x1000;
    mem.ram[0x1000] = 0x0F;
    mem.ram[0x1001] = 0xAF; // IMUL Gv,Ev, must not dispatch.

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_gp0(result, "0F escaped opcode byte beyond the CS limit");
}

#[test]
fn test_real_mode_instruction_crossing_cs_limit_raises_gp() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0;
    cpu.regs.cs_cache = SegmentCache::real_mode(0);
    cpu.regs.set_eip(0xFFF8);
    let code = [0x26, 0x2E, 0x36, 0x3E, 0x64, 0x65, 0x66, 0x67, 0x90];
    mem.ram[0xFFF8..0x1_0001].copy_from_slice(&code);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert_gp0(result, "real-mode instruction crossing CS:FFFF");
    assert_eq!(cpu.regs.eip, 0xFFF8);
}

#[test]
fn test_real_mode_retirement_preserves_eip_carry_past_cs_limit() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0;
    cpu.regs.cs_cache = SegmentCache::real_mode(0);
    cpu.regs.set_eip(0xFFFF);
    mem.ram[0xFFFF] = 0x90;

    let first = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(first, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eip, 0x1_0000);

    let second = run(&mut cpu, &mut mem, &mut (), 1);
    assert_gp0(second, "next fetch after real-mode CS limit");
    assert_eq!(cpu.regs.eip, 0x1_0000);
}

#[test]
fn test_cs_limit_loadip_fast_path_rechecks_next_eip() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);

    // The NOP at the limit is valid, but the LOADIP fast path must not
    // decode the following HLT at EIP=limit+1.
    cpu.regs.set_ip(0x1000);
    cpu.regs.eip = 0x1000;
    mem.ram[0x1000] = 0x90; // NOP
    mem.ram[0x1001] = 0xF4; // HLT, must not execute.

    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert_gp0(result, "LOADIP fast path target beyond the CS limit");
}

/// EXP-IQ013 mutation-certified pin: for a FLAT 32-bit CS, the prologue
/// byte check at the `cs_fetch_byte_ok` call site is the SOLE guard
/// against an instruction fetch wrapping past EIP 0xFFFF_FFFF — the
/// crosses-limit precheck is skipped for flat CS, and the 15-byte guard
/// needs consumed > 14. `checked_add(0xFFFF_FFFF, 1)` returns `None` →
/// #GP(0) when a prefix byte sits at EIP 0xFFFF_FFFF. Certified RED with
/// the call-site check deleted (2026.08.05, EXP-IQ013 review). Retained
/// after the EXP-IQ013 revert: it pins the check's one sole-guarded case
/// against any future prologue restructuring.
#[test]
fn test_flat_cs_fetch_wrap_past_4g_raises_gp() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    // Flat 4 GiB 32-bit code segment with a NONZERO base so that
    // base + EIP wraps into low RAM: linear = 0x1002 + 0xFFFF_FFFF
    // (mod 2^32) = 0x1001.
    let gdt_base: u32 = 0x10000;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]); // null
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0x1002, 0xFFFFF, 0x9A, 0x0C), // code, G=1 D=1 (nibble) → flat 32-bit
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C), // flat data
    );
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = 3 * 8 - 1;
    cpu.regs.cr0 |= 1; // PE=1
    cpu.regs.cpl = 0;
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 32;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ss_cache.is_writable = true;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;

    cpu.regs.eip = 0xFFFF_FFFF; // NOTE: set_ip() would clobber the full EIP
    mem.ram[0x1001] = 0x66; // operand-size prefix at the wrap point
    mem.ram[0x1002] = 0x90; // NOP — must never execute

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_gp0(result, "instruction fetch wrapping past EIP 0xFFFF_FFFF");
}

#[test]
fn test_cs_limit_ret_fast_path_rechecks_target_eip() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);

    // RET at the limit is valid. Its near target is on the same page but
    // outside the code segment, so the branch fast path must not decode it.
    cpu.regs.set_ip(0x1000);
    cpu.regs.eip = 0x1000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;
    mem.ram[0x1000] = 0xC3; // RET near
    mem.ram[0x1001] = 0xF4; // HLT, must not execute.
    mem.ram[0x8000] = 0x01;
    mem.ram[0x8001] = 0x10;

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_gp0(result, "same-page RET target beyond the CS limit");
    assert_eq!(cpu.regs.eip, 0x1000, "#GP must point at the RET");
    assert_eq!(cpu.regs.esp, 0x8000, "RET must not commit its pop");
}

#[test]
fn test_cs_limit_jump_faults_on_the_transfer_instruction() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);
    cpu.regs.eip = 0x0FFC;
    mem.ram[0x0FFC..0x0FFE].copy_from_slice(&[0xEB, 0x03]);

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert_gp0(result, "taken JMP target beyond the CS limit");
    assert_eq!(cpu.regs.eip, 0x0FFC, "#GP must point at the JMP");
}

#[test]
fn test_cs_limit_jcc_checks_only_the_taken_target() {
    let mut taken_cpu = Cpu::new();
    let mut taken_mem = Memory::new();
    let mut io = ();

    setup_pm_with_cs_limit(&mut taken_cpu, &mut taken_mem, 0x1000);
    taken_cpu.regs.eip = 0x0FFC;
    taken_cpu.regs.flags.insert(Flags::ZERO);
    taken_mem.ram[0x0FFC..0x0FFE].copy_from_slice(&[0x74, 0x03]);

    let taken = run(&mut taken_cpu, &mut taken_mem, &mut io, 1);
    assert_gp0(taken, "taken JZ target beyond the CS limit");
    assert_eq!(taken_cpu.regs.eip, 0x0FFC, "#GP must point at the JZ");

    let mut near_cpu = Cpu::new();
    let mut near_mem = Memory::new();
    setup_pm_with_cs_limit(&mut near_cpu, &mut near_mem, 0x1000);
    near_cpu.regs.eip = 0x0FFA;
    near_cpu.regs.flags.insert(Flags::ZERO);
    near_mem.ram[0x0FFA..0x0FFE].copy_from_slice(&[0x0F, 0x84, 0x03, 0x00]);

    let near = run(&mut near_cpu, &mut near_mem, &mut io, 1);
    assert_gp0(near, "taken near JZ target beyond the CS limit");
    assert_eq!(near_cpu.regs.eip, 0x0FFA, "#GP must point at the near JZ");

    let mut untaken_cpu = Cpu::new();
    let mut untaken_mem = Memory::new();
    setup_pm_with_cs_limit(&mut untaken_cpu, &mut untaken_mem, 0x1000);
    untaken_cpu.regs.eip = 0x0FFC;
    untaken_cpu.regs.flags.remove(Flags::ZERO);
    untaken_mem.ram[0x0FFC..0x0FFE].copy_from_slice(&[0x74, 0x03]);

    let untaken = run(&mut untaken_cpu, &mut untaken_mem, &mut io, 1);
    assert!(
        !matches!(untaken, RunResult::Exception { vector: 13, .. }),
        "untaken JZ must not validate its branch target: {untaken:?}"
    );
    assert_eq!(untaken_cpu.regs.eip, 0x0FFE, "untaken JZ falls through");
}

#[test]
fn test_cs_limit_call_faults_before_stack_mutation() {
    for (code, indirect) in [(&[0xE8, 0x03, 0x00][..], false), (&[0xFF, 0xD0][..], true)] {
        let mut cpu = Cpu::new();
        let mut mem = Memory::new();
        let mut io = ();

        setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);
        cpu.regs.eip = 0x0FFB;
        cpu.regs.eax = 0x1001;
        mem.ram[0x0FFB..0x0FFB + code.len()].copy_from_slice(code);
        mem.ram[0x7FFE..0x8000].copy_from_slice(&[0xAA, 0xBB]);

        let result = run(&mut cpu, &mut mem, &mut io, 1);

        assert_gp0(
            result,
            if indirect {
                "indirect CALL target beyond CS.limit"
            } else {
                "relative CALL target beyond CS.limit"
            },
        );
        assert_eq!(cpu.regs.eip, 0x0FFB, "#GP must point at the CALL");
        assert_eq!(cpu.regs.esp, 0x8000, "CALL must not decrement SP");
        assert_eq!(
            &mem.ram[0x7FFE..0x8000],
            &[0xAA, 0xBB],
            "CALL must not push"
        );
    }
}

#[test]
fn test_cs_limit_indirect_jump_faults_before_eip_commit() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);
    cpu.regs.eip = 0x0FFC;
    cpu.regs.eax = 0x1001;
    mem.ram[0x0FFC..0x0FFE].copy_from_slice(&[0xFF, 0xE0]);

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert_gp0(result, "indirect JMP target beyond CS.limit");
    assert_eq!(cpu.regs.eip, 0x0FFC, "#GP must point at the JMP");
}

#[test]
fn test_cs_limit_loop_fault_restores_counter() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    setup_pm_with_cs_limit(&mut cpu, &mut mem, 0x1000);
    cpu.regs.eip = 0x0FFC;
    cpu.regs.ecx = 2;
    mem.ram[0x0FFC..0x0FFE].copy_from_slice(&[0xE2, 0x03]);

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert_gp0(result, "taken LOOP target beyond CS.limit");
    assert_eq!(cpu.regs.eip, 0x0FFC, "#GP must point at LOOP");
    assert_eq!(cpu.regs.ecx, 2, "faulting LOOP must not commit CX");
}

#[test]
fn test_cs_flat_4gb_no_limit_check() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    // Flat 4GB code segment: limit=0xFFFFF, granularity=1 -> effective 0xFFFFFFFF
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 3 * 8 - 1;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]);
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C), // code, page gran, 32-bit
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C), // data, page gran, 32-bit
    );
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 0;

    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 32;

    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ss_cache.is_writable = true;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;

    // Place NOP at a high EIP within RAM
    cpu.regs.eip = 0x100000;
    mem.ram[0x100000] = 0x90; // NOP

    let result = run(&mut cpu, &mut mem, &mut io, 32);
    // Should NOT be a #GP — flat 4GB bypasses the limit check entirely
    assert!(
        !matches!(result, RunResult::Exception { vector: 13, .. }),
        "Flat 4GB CS should not trigger limit check. Got {:?}",
        result
    );
}

#[test]
fn test_interp_trace_at_state_triggers_once_and_counts_down() {
    let state = InterpStepTraceState::new();
    let cfg = Some(InterpTraceAtCfg {
        cs: 0x1234,
        ip: 0x5678,
        n: 2,
    });

    assert_eq!(
        state.trace_at_should_log(cfg, 0x1234, 0x5677),
        (false, false)
    );
    assert_eq!(state.trace_at_should_log(cfg, 0x1234, 0x5678), (true, true));
    assert_eq!(
        state.trace_at_should_log(cfg, 0x1234, 0x5679),
        (false, true)
    );
    assert_eq!(
        state.trace_at_should_log(cfg, 0x1234, 0x567A),
        (false, false)
    );
    assert_eq!(
        state.trace_at_should_log(cfg, 0x1234, 0x5678),
        (false, false)
    );
}

#[test]
fn test_interp_step_trace_state_counts_down_from_first_instruction() {
    let state = InterpStepTraceState::new();

    assert!(state.step_should_log(3));
    assert!(state.step_should_log(3));
    assert!(state.step_should_log(3));
    assert!(!state.step_should_log(3));
    assert!(!state.step_should_log(0));
}

#[test]
fn test_parse_interp_eip_range_spec() {
    assert_eq!(
        parse_interp_eip_range_spec("12345678:123456FF"),
        Some((0x1234_5678, 0x1234_56FF))
    );
    assert_eq!(parse_interp_eip_range_spec("0x10:0X1f"), Some((0x10, 0x1F)));
    assert_eq!(parse_interp_eip_range_spec("20:10"), None);
    assert_eq!(parse_interp_eip_range_spec("12345678"), None);
}

#[test]
fn test_parse_interp_eip_range_cs_spec() {
    assert_eq!(parse_interp_eip_range_cs_spec("1234"), Some(0x1234));
    assert_eq!(parse_interp_eip_range_cs_spec("0x5678"), Some(0x5678));
    assert_eq!(parse_interp_eip_range_cs_spec(""), None);
    assert_eq!(parse_interp_eip_range_cs_spec("10000"), None);
}

/// Helper: build a synthetic `InterpTraceRecord` with the given `ip` and
/// everything else zero — enough for backlog/drain tests that don't care
/// about the other fields.
fn make_trace_record(ip: u16) -> InterpTraceRecord {
    InterpTraceRecord {
        cs: 0x1234,
        ip,
        prev_cs: 0,
        prev_ip: 0,
        bytes: [0; TRACE_BYTES_LEN],
        ax: 0,
        bx: 0,
        cx: 0,
        dx: 0,
        si: 0,
        di: 0,
        bp: 0,
        ds: 0,
        es: 0,
        ss: 0,
        sp: 0,
    }
}

#[test]
fn test_interp_trace_backlog_caps_and_drains_in_order() {
    const CAP: usize = 32;
    let state = InterpStepTraceState::new();
    state
        .backlog_cap
        .set(CAP)
        .expect("backlog_cap should be settable before first use");
    for ip in 0..(CAP as u16 + 3) {
        state.push_backlog(make_trace_record(ip));
    }

    let drained = state.drain_backlog();
    assert_eq!(drained.len(), CAP);
    assert_eq!(drained.first().map(|r| r.ip), Some(3));
    assert_eq!(drained.last().map(|r| r.ip), Some((CAP as u16) + 2));
    assert!(state.drain_backlog().is_empty());
}

#[test]
fn test_interp_notify_interrupt_dispatch_triggers_backlog_drain() {
    let state = InterpStepTraceState::new();
    state
        .backlog_cap
        .set(128)
        .expect("backlog_cap should be settable before first use");
    state
        .int_cfg
        .set(Some(6))
        .expect("int_cfg should be settable before first use");

    for ip in 0..10u16 {
        state.push_backlog(make_trace_record(ip));
    }

    assert_eq!(state.backlog().lock().unwrap().len(), 10);
    assert!(!state.triggered.load(Ordering::Relaxed));

    state.notify_interrupt(6);

    assert_eq!(state.backlog().lock().unwrap().len(), 0);
    assert!(state.triggered.load(Ordering::Relaxed));
}

#[test]
fn test_interp_notify_interrupt_dispatch_wrong_vector_no_op() {
    let state = InterpStepTraceState::new();
    state
        .backlog_cap
        .set(128)
        .expect("backlog_cap should be settable before first use");
    state
        .int_cfg
        .set(Some(6))
        .expect("int_cfg should be settable before first use");

    for ip in 0..10u16 {
        state.push_backlog(make_trace_record(ip));
    }

    state.notify_interrupt(13);

    assert_eq!(state.backlog().lock().unwrap().len(), 10);
    assert!(!state.triggered.load(Ordering::Relaxed));
}

#[test]
fn test_interp_trace_backlog_capacity_configurable() {
    let state = InterpStepTraceState::new();
    state
        .backlog_cap
        .set(128)
        .expect("backlog_cap should be settable before first use");

    for ip in 0..200u16 {
        state.push_backlog(make_trace_record(ip));
    }

    let drained = state.drain_backlog();
    assert!(
        drained.len() <= 128,
        "backlog length {} exceeds configured cap 128",
        drained.len()
    );
    assert_eq!(drained.len(), 128);
    // Oldest entry retained is (200 - 128) = 72.
    assert_eq!(drained.first().map(|r| r.ip), Some(72));
    assert_eq!(drained.last().map(|r| r.ip), Some(199));
}

#[test]
fn test_interp_notify_interrupt_dispatch_one_shot() {
    let state = InterpStepTraceState::new();
    state
        .backlog_cap
        .set(128)
        .expect("backlog_cap should be settable before first use");
    state
        .int_cfg
        .set(Some(6))
        .expect("int_cfg should be settable before first use");

    for ip in 0..5u16 {
        state.push_backlog(make_trace_record(ip));
    }

    state.notify_interrupt(6);
    assert!(state.triggered.load(Ordering::Relaxed));
    assert_eq!(state.backlog().lock().unwrap().len(), 0);

    // Push new records after the first trigger fired — a second matching
    // call must be a no-op and leave these records in place.
    for ip in 100..105u16 {
        state.push_backlog(make_trace_record(ip));
    }

    state.notify_interrupt(6);

    assert_eq!(state.backlog().lock().unwrap().len(), 5);
}

/// Phys-OOB fetch with paging OFF must NOT raise a synthetic #GP(0).
/// Real x86 CPUs do not raise a synchronous #GP(0) for physical reads
/// beyond installed RAM; the chipset responds however it is configured.
/// We model the most-common ISA-era behaviour by returning open-bus
/// (0xFF) bytes — see wrk_docs/2026.05.01 - HLD - phys-OOB open-bus
/// fetch.md. The fast interpreter previously synthesised vector=13
/// error_code=0 here, which caused cascading exceptions in DOS
/// extender teardown paths.
///
/// Positive contract: with 0xFF-padded fetch, the decoder sees
/// `0xFF FF` (group-FF /7 in 32-bit mode) which is a reserved
/// encoding → #UD (vector 6). BudgetExhausted is also acceptable
/// (the open-bus stream may decode to a budget-consuming op without
/// faulting). Anything else — and especially the old synthetic
/// vector=13 error_code=0 — is a regression.
#[test]
fn test_fetch_phys_oob_no_paging_returns_open_bus() {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let mut io = ();

    // Flat 4GB code segment so the limit check does not interfere.
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 3 * 8 - 1;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]);
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C), // code, page gran, 32-bit
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C), // data, page gran, 32-bit
    );
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1; // PE=1, paging OFF (CR0.PG=0)
    cpu.regs.cpl = 0;

    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 32;

    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ss_cache.is_writable = true;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;

    // EIP at 128 MB == usable_len(); fetch_linear is past usable RAM.
    // With cs_base=0 and paging OFF, the no-paging arm hits
    // `masked >= ram_len` and previously raised synthetic #GP(0).
    // NOTE: set EIP last — set_ip zero-extends val into EIP and would
    // clobber a high-32-bit EIP if called afterwards.
    let oob_eip = mem.usable_len() as u32;
    cpu.regs.eip = oob_eip;

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    // Open-bus (0xFF) means: NO synthetic #GP from the fetch path.
    // After the fix, the decoder runs against 0xFF-padded bytes;
    // `0xFF FF` (ModR/M=0xFF: mod=11, reg=7) is group-FF /7 — a
    // reserved encoding — so the legitimate outcome is #UD (vector
    // 6). BudgetExhausted is also acceptable. Anything else is a
    // regression.
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => panic!(
            "phys-OOB fetch (no paging) must not synthesise #GP(0); got {:?}",
            result
        ),
        RunResult::Exception { vector: 6, .. } => {
            // Expected: open-bus 0xFF FF decodes as group-FF /7 → #UD.
        }
        RunResult::BudgetExhausted { .. } => {
            // Acceptable: the open-bus stream consumed the cycle
            // without faulting.
        }
        other => panic!(
            "expected #UD (vector 6) or BudgetExhausted from open-bus fetch; got {:?}",
            other
        ),
    }
}
