use super::test_helpers::{make_descriptor, setup, write_gdt_entry};
use super::*;
use gp64_cpu::{Cpu, Flags};
use gp64_memory::Memory;
use std::cell::RefCell;
use std::collections::VecDeque;

// ========================================================================
// Helper: setup protected-mode 32-bit (flat model)
// ========================================================================
fn setup_pm32(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 8 * 8 - 1;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]);
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x20,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x28,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x30,
        make_descriptor(0x1000, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x38,
        make_descriptor(0x2000, 0xFFFFF, 0x92, 0x0C),
    );
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1;
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 32;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.es = 0x10;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.fs = 0x30;
    cpu.regs.fs_cache = cpu.read_descriptor(&mem, 0x30).unwrap();
    cpu.regs.gs = 0x38;
    cpu.regs.gs_cache = cpu.read_descriptor(&mem, 0x38).unwrap();
    cpu.regs.set_eip(0x0100);
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

fn setup_pm16(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 6 * 8 - 1;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]);
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x08),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x20,
        make_descriptor(0x1000, 0xFFFFF, 0x92, 0x08),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x28,
        make_descriptor(0x2000, 0xFFFFF, 0x92, 0x08),
    );
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1;
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 16;
    cpu.regs.cpl = 0;
    cpu.regs.ss = 0x10;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.es = 0x10;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.fs = 0x20;
    cpu.regs.fs_cache = cpu.read_descriptor(&mem, 0x20).unwrap();
    cpu.regs.gs = 0x28;
    cpu.regs.gs_cache = cpu.read_descriptor(&mem, 0x28).unwrap();
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

// Helper: setup PM with ring-3 CPL, custom IOPL, 16-bit code segment
fn setup_pm_ring3(code: &[u8], iopl: u8) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 |= 1;
    cpu.regs.cs = 0x001B; // RPL=3
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0xFB; // code, readable, DPL=3
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = false;
    cpu.regs.cs_cache.present = true;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.present = true;
    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0xFFFFF;
    cpu.regs.es_cache.granularity = true;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.present = true;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.big = false;
    cpu.regs.ss_cache.present = true;
    cpu.regs.cpl = 3;
    cpu.regs.set_iopl(iopl);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.cs_bits = 16;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

// Helper: setup paging (identity map first 1MB)
fn setup_paging(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 = 0x8000_0001;
    cpu.regs.cpl = 0;
    cpu.regs.cr3 = 0x10000;
    mem.write_u32_physical(0x10000, 0x11007);
    for page in 0..256u32 {
        let pte_addr = 0x11000 + (page as usize) * 4;
        mem.write_u32_physical(pte_addr, (page << 12) | 0x07);
    }
    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0x9B;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = true;
    cpu.regs.cs_cache.present = true;
    cpu.cs_bits = 32;
    cpu.regs.ds = 0x0010;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.access = 0x93;
    cpu.regs.ds_cache.is_code = false;
    cpu.regs.ds_cache.big = true;
    cpu.regs.ds_cache.present = true;
    cpu.regs.ss = 0x0018;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.access = 0x93;
    cpu.regs.ss_cache.is_code = false;
    cpu.regs.ss_cache.big = true;
    cpu.regs.ss_cache.present = true;
    cpu.regs.es = 0x0020;
    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0xFFFFF;
    cpu.regs.es_cache.granularity = true;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.access = 0x93;
    cpu.regs.es_cache.is_code = false;
    cpu.regs.es_cache.big = true;
    cpu.regs.es_cache.present = true;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

// Helper: setup V86 mode (PE=1, VM=1, real-mode style segments)
fn setup_v86(code: &[u8], iopl: u8) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 |= 1; // PE
    cpu.regs.set_v86_mode(true); // VM bit in EFLAGS
    cpu.regs.set_iopl(iopl);
    cpu.regs.cpl = 3; // V86 always runs at CPL 3
    cpu.regs.cs = 0x0000;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFF;
    cpu.regs.cs_cache.granularity = false;
    cpu.regs.cs_cache.access = 0x9B;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = false;
    cpu.regs.cs_cache.present = true;
    cpu.cs_bits = 16;
    cpu.regs.ds = 0x0000;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFF;
    cpu.regs.ds_cache.granularity = false;
    cpu.regs.ds_cache.access = 0x93;
    cpu.regs.ds_cache.is_code = false;
    cpu.regs.ds_cache.big = false;
    cpu.regs.ds_cache.present = true;
    cpu.regs.es = 0x0000;
    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0xFFFF;
    cpu.regs.es_cache.granularity = false;
    cpu.regs.es_cache.access = 0x93;
    cpu.regs.es_cache.is_code = false;
    cpu.regs.es_cache.big = false;
    cpu.regs.es_cache.present = true;
    cpu.regs.ss = 0x0000;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFF;
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.access = 0x93;
    cpu.regs.ss_cache.is_code = false;
    cpu.regs.ss_cache.big = false;
    cpu.regs.ss_cache.present = true;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

// MockIo for I/O instruction tests
struct MockIo {
    read_u8_queue: RefCell<VecDeque<u8>>,
    read_u16_queue: RefCell<VecDeque<u16>>,
    read_u32_queue: RefCell<VecDeque<u32>>,
    written_u8: RefCell<Vec<(u16, u8)>>,
    written_u16: RefCell<Vec<(u16, u16)>>,
    written_u32: RefCell<Vec<(u16, u32)>>,
}

impl MockIo {
    fn new() -> Self {
        Self {
            read_u8_queue: RefCell::new(VecDeque::new()),
            read_u16_queue: RefCell::new(VecDeque::new()),
            read_u32_queue: RefCell::new(VecDeque::new()),
            written_u8: RefCell::new(Vec::new()),
            written_u16: RefCell::new(Vec::new()),
            written_u32: RefCell::new(Vec::new()),
        }
    }
    fn push_read_u8(&self, val: u8) {
        self.read_u8_queue.borrow_mut().push_back(val);
    }
    fn push_read_u16(&self, val: u16) {
        self.read_u16_queue.borrow_mut().push_back(val);
    }
    #[allow(dead_code)]
    fn push_read_u32(&self, val: u32) {
        self.read_u32_queue.borrow_mut().push_back(val);
    }
}

impl IoPort for MockIo {
    fn read_u8(&mut self, _port: u16) -> u8 {
        self.read_u8_queue.borrow_mut().pop_front().unwrap_or(0xFF)
    }
    fn read_u16(&mut self, _port: u16) -> u16 {
        self.read_u16_queue
            .borrow_mut()
            .pop_front()
            .unwrap_or(0xFFFF)
    }
    fn read_u32(&mut self, _port: u16) -> u32 {
        self.read_u32_queue
            .borrow_mut()
            .pop_front()
            .unwrap_or(0xFFFF_FFFF)
    }
    fn write_u8(&mut self, port: u16, value: u8) {
        self.written_u8.borrow_mut().push((port, value));
    }
    fn write_u16(&mut self, port: u16, value: u16) {
        self.written_u16.borrow_mut().push((port, value));
    }
    fn write_u32(&mut self, port: u16, value: u32) {
        self.written_u32.borrow_mut().push((port, value));
    }
}

// Helper: set up TSS with I/O bitmap
fn setup_tss(mem: &mut Memory, cpu: &mut Cpu, tss_base: u32, tss_limit: u32, io_map_off: u16) {
    cpu.regs.tss_base = tss_base;
    cpu.regs.tss_limit = tss_limit;
    let addr = (tss_base + 0x66) as usize;
    mem.ram[addr] = io_map_off as u8;
    mem.ram[addr + 1] = (io_map_off >> 8) as u8;
    let bitmap_start = (tss_base as usize) + (io_map_off as usize);
    let bitmap_end = (tss_base as usize) + (tss_limit as usize) + 1;
    for i in bitmap_start..bitmap_end.min(mem.ram.len()) {
        mem.ram[i] = 0xFF; // all denied
    }
}

// Helper: allow a port in the TSS I/O bitmap
fn allow_port(mem: &mut Memory, tss_base: u32, io_map_off: u16, port: u16, size: u8) {
    for i in 0..size {
        let p = port + i as u16;
        let byte_off = (p / 8) as usize;
        let bit_off = p % 8;
        let addr = (tss_base as usize) + (io_map_off as usize) + byte_off;
        mem.ram[addr] &= !(1 << bit_off);
    }
}

// ========================================================================
// Area 1: V86 mode paths
// ========================================================================

// 1. V86 mode: CLI with IOPL < 3 should #GP
#[test]
fn test_v86_cli_iopl_lt3_gp() {
    // CLI = 0xFA
    let (mut cpu, mut mem) = setup_v86(&[0xFA], 0);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("V86 CLI with IOPL=0 should #GP(0), got {:?}", other),
    }
}

// 2. V86 mode: CLI with IOPL=3 should succeed
#[test]
fn test_v86_cli_iopl3_succeeds() {
    let (mut cpu, mut mem) = setup_v86(&[0xFA], 3);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "V86 CLI with IOPL=3 should succeed, got {:?}",
        result
    );
    assert!(!cpu.regs.flags.contains(Flags::INTERRUPT));
}

// 3. V86 mode: STI with IOPL < 3 should #GP
#[test]
fn test_v86_sti_iopl_lt3_gp() {
    // STI = 0xFB
    let (mut cpu, mut mem) = setup_v86(&[0xFB], 1);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("V86 STI with IOPL=1 should #GP(0), got {:?}", other),
    }
}

// 4. V86 mode: STI with IOPL=3 should succeed
#[test]
fn test_v86_sti_iopl3_succeeds() {
    let (mut cpu, mut mem) = setup_v86(&[0xFB], 3);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::InterruptPollBoundary { executed: 1 }),
        "V86 STI with IOPL=3 should succeed, got {:?}",
        result
    );
    assert!(cpu.regs.flags.contains(Flags::INTERRUPT));
}

// 5. V86 mode: POPF with IOPL < 3 should #GP
#[test]
fn test_v86_popf_iopl_lt3_gp() {
    // POPF = 0x9D
    let (mut cpu, mut mem) = setup_v86(&[0x9D], 2);
    // Push a flags word onto the stack so POPF has something to pop
    let sp = cpu.regs.sp();
    let new_sp = sp.wrapping_sub(2);
    let addr = (cpu.regs.ss_cache.base + new_sp as u32) as usize;
    mem.ram[addr] = 0x02;
    mem.ram[addr + 1] = 0x00;
    cpu.regs.set_sp(new_sp);
    cpu.regs.esp = new_sp as u32;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("V86 POPF with IOPL=2 should #GP(0), got {:?}", other),
    }
}

// 6. V86 mode: POPF with IOPL=3 should succeed
#[test]
fn test_v86_popf_iopl3_succeeds() {
    let (mut cpu, mut mem) = setup_v86(&[0x9D], 3);
    // Push flags with IF set
    let sp = cpu.regs.sp();
    let addr = (cpu.regs.ss_cache.base + sp.wrapping_sub(2) as u32) as usize;
    mem.ram[addr] = 0x02; // RESERVED1 only
    mem.ram[addr + 1] = 0x02; // IF set
                              // Pretend stack already has the value; we need to adjust SP to point below
                              // Actually, POPF reads from current SP. Set data at current SP.
    let sp_addr = (cpu.regs.ss_cache.base + sp as u32) as usize;
    mem.ram[sp_addr] = 0x02;
    mem.ram[sp_addr + 1] = 0x02; // IF set (bit 9 = byte offset 1 bit 1)
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::InterruptPollBoundary { executed: 1 }),
        "V86 POPF with IOPL=3 should succeed, got {:?}",
        result
    );
}

// 7. V86 mode: PUSHF should work with IOPL=3
#[test]
fn test_v86_pushf_iopl3() {
    // PUSHF = 0x9C
    let (mut cpu, mut mem) = setup_v86(&[0x9C], 3);
    cpu.regs.flags.insert(Flags::CARRY);
    let sp_before = cpu.regs.sp();
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "V86 PUSHF with IOPL=3 should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.sp(), sp_before.wrapping_sub(2));
}

// 8. V86 mode: MOV to/from segment registers should work
#[test]
fn test_v86_mov_ds_ax() {
    // MOV DS, AX = 8E D8
    let (mut cpu, mut mem) = setup_v86(&[0x8E, 0xD8], 3);
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "V86 MOV DS,AX should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ds, 0x1234);
}

// 9. V86 mode: basic arithmetic instructions should work
#[test]
fn test_v86_add_ax_imm() {
    // ADD AX, 0x1234 = 05 34 12
    let (mut cpu, mut mem) = setup_v86(&[0x05, 0x34, 0x12], 3);
    cpu.regs.set_ax(0x1000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x2234);
}

// 10. V86 mode: INT n with IOPL < 3 must raise #GP(0) per SDM §20.3.1.
// The V86 monitor  needs this trap to intercept and
// redirect guest INTs — without it, reflection loops can livelock on
// opcodes like CLI that share a monitor entry path.
#[test]
fn test_v86_int_iopl_lt3_gp() {
    // INT 0x21 = CD 21 with IOPL=0 in V86 mode → #GP(0) for VM86 monitor.
    let (mut cpu, mut mem) = setup_v86(&[0xCD, 0x21], 0);
    let saved_eip = cpu.regs.eip;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {
            // EIP must still point at the INT instruction so the monitor
            // can decode the vector from [CS:EIP+1].
            assert_eq!(
                cpu.regs.eip, saved_eip,
                "V86 INT n → #GP must leave EIP on the INT opcode"
            );
        }
        other => panic!(
            "V86 INT 0x21 with IOPL=0 must raise #GP(0), got {:?}",
            other
        ),
    }
}

// 10b. V86 mode: INT n with IOPL=3 reaches protected-IDT validation, but
// still cannot call a DPL0 gate from CPL3/V86. The result is the normal
// software-gate DPL violation #GP(vec*8+2), not the IOPL<3 #GP(0) trap.
#[test]
fn test_v86_int_iopl_3_dpl0_gate_raises_selector_gp() {
    let (mut cpu, mut mem) = setup_v86(&[0xCD, 0x21], 3);
    cpu.regs.idtr.base = 0x30000;
    cpu.regs.idtr.limit = 0x07FF;
    let idt_addr = 0x30000 + 0x21 * 8;
    mem.ram[idt_addr] = 0x00;
    mem.ram[idt_addr + 1] = 0x60;
    mem.ram[idt_addr + 2] = 0x08;
    mem.ram[idt_addr + 3] = 0x00;
    mem.ram[idt_addr + 4] = 0x00;
    mem.ram[idt_addr + 5] = 0x8E;
    mem.ram[idt_addr + 6] = 0x00;
    mem.ram[idt_addr + 7] = 0x00;
    let saved_eip = cpu.regs.eip;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code,
            ..
        } => {
            assert_eq!(
                error_code,
                0x21 * 8 + 2,
                "V86 IOPL=3 DPL0 gate violation must report the IDT selector error"
            );
            assert_eq!(
                cpu.regs.eip, saved_eip,
                "DPL violation must leave EIP on the INT opcode"
            );
        }
        other => panic!(
            "V86 INT 0x21 IOPL=3 through DPL0 gate must raise #GP(vec*8+2), got {:?}",
            other
        ),
    }
}

#[test]
fn test_v86_int_iopl_3_dpl3_gate_dispatches() {
    let (mut cpu, mut mem) = setup_v86(&[0xCD, 0x21], 3);
    cpu.regs.idtr.base = 0x30000;
    cpu.regs.idtr.limit = 0x07FF;
    let idt_addr = 0x30000 + 0x21 * 8;
    mem.ram[idt_addr] = 0x00;
    mem.ram[idt_addr + 1] = 0x60;
    mem.ram[idt_addr + 2] = 0x08;
    mem.ram[idt_addr + 3] = 0x00;
    mem.ram[idt_addr + 4] = 0x00;
    mem.ram[idt_addr + 5] = 0xEE;
    mem.ram[idt_addr + 6] = 0x00;
    mem.ram[idt_addr + 7] = 0x00;
    let saved_eip = cpu.regs.eip;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::SoftwareInterrupt { vector: 0x21, .. } => {
            assert_eq!(
                cpu.regs.eip,
                saved_eip + 2,
                "V86 IOPL=3 INT n should dispatch through a callable DPL3 gate"
            );
        }
        other => panic!(
            "V86 INT 0x21 IOPL=3 through DPL3 gate should dispatch, got {:?}",
            other
        ),
    }
}

// ========================================================================
// Area 2: I/O instructions with IOPL checks
// ========================================================================

// 11. IN AL, imm8 with CPL > IOPL should #GP (no TSS bitmap)
#[test]
fn test_in_al_imm8_cpl_gt_iopl_gp() {
    // E4 80 = IN AL, 0x80
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xE4, 0x80], 0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("IN AL with CPL>IOPL and no TSS should #GP, got {:?}", other),
    }
}

// 12. IN AX, imm8 (16-bit) with CPL > IOPL should #GP
#[test]
fn test_in_ax_imm8_cpl_gt_iopl_gp() {
    // E5 80 = IN AX, 0x80
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xE5, 0x80], 0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("IN AX with CPL>IOPL should #GP, got {:?}", other),
    }
}

// 13. OUT imm8, AL with CPL > IOPL should #GP
#[test]
fn test_out_al_imm8_cpl_gt_iopl_gp() {
    // E6 80 = OUT 0x80, AL
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xE6, 0x80], 0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("OUT AL with CPL>IOPL should #GP, got {:?}", other),
    }
}

// 14. OUT imm8, AX with CPL > IOPL should #GP
#[test]
fn test_out_ax_imm8_cpl_gt_iopl_gp() {
    // E7 80 = OUT 0x80, AX
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xE7, 0x80], 0);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("OUT AX with CPL>IOPL should #GP, got {:?}", other),
    }
}

// 15. IN AL, DX with CPL > IOPL should #GP
#[test]
fn test_in_al_dx_cpl_gt_iopl_gp() {
    // EC = IN AL, DX
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xEC], 0);
    cpu.regs.set_dx(0x80);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("IN AL,DX with CPL>IOPL should #GP, got {:?}", other),
    }
}

// 16. IN AX/EAX, DX with CPL > IOPL should #GP
#[test]
fn test_in_ax_dx_cpl_gt_iopl_gp() {
    // ED = IN AX, DX
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xED], 0);
    cpu.regs.set_dx(0x80);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("IN AX,DX with CPL>IOPL should #GP, got {:?}", other),
    }
}

// 17. OUT DX, AL with CPL > IOPL should #GP
#[test]
fn test_out_dx_al_cpl_gt_iopl_gp() {
    // EE = OUT DX, AL
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xEE], 0);
    cpu.regs.set_dx(0x80);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("OUT DX,AL with CPL>IOPL should #GP, got {:?}", other),
    }
}

// 18. OUT DX, AX/EAX with CPL > IOPL should #GP
#[test]
fn test_out_dx_ax_cpl_gt_iopl_gp() {
    // EF = OUT DX, AX
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xEF], 0);
    cpu.regs.set_dx(0x80);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("OUT DX,AX with CPL>IOPL should #GP, got {:?}", other),
    }
}

// 19. IN AL with CPL > IOPL but TSS bitmap allows port
#[test]
fn test_in_al_imm8_bitmap_allows() {
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xE4, 0x42], 0);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x42 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x42, 1);
    let mut io = MockIo::new();
    io.push_read_u8(0xBB);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "IN AL with bitmap allowed should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.al(), 0xBB);
}

// 20. OUT imm8, AL with bitmap allowing port
#[test]
fn test_out_al_imm8_bitmap_allows() {
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xE6, 0x42], 0);
    cpu.regs.set_al(0xDE);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x42 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x42, 1);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "OUT AL with bitmap allowed should succeed, got {:?}",
        result
    );
    assert_eq!(io.written_u8.borrow()[0], (0x42, 0xDE));
}

// ========================================================================
// Area 3: INS/OUTS string I/O
// ========================================================================

// 21. INSB with no REP in real mode
#[test]
fn test_insb_no_rep_real_mode() {
    // INSB = 0x6C
    let (mut cpu, mut mem) = setup(&[0x6C]);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_di(0x2000);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = MockIo::new();
    io.push_read_u8(0xAA);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "INSB should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x2000], 0xAA);
    assert_eq!(cpu.regs.di(), 0x2001);
}

// 22. INSW (word) with no REP in real mode
#[test]
fn test_insw_no_rep_real_mode() {
    // INSW = 0x6D (16-bit mode)
    let (mut cpu, mut mem) = setup(&[0x6D]);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_di(0x2000);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = MockIo::new();
    io.push_read_u16(0xBEEF);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "INSW should succeed, got {:?}",
        result
    );
    let val = u16::from_le_bytes([mem.ram[0x2000], mem.ram[0x2001]]);
    assert_eq!(val, 0xBEEF);
    assert_eq!(cpu.regs.di(), 0x2002);
}

// 23. OUTSB with no REP in real mode
#[test]
fn test_outsb_no_rep_real_mode() {
    // OUTSB = 0x6E
    let (mut cpu, mut mem) = setup(&[0x6E]);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_si(0x2000);
    mem.ram[0x2000] = 0xCC;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "OUTSB should succeed, got {:?}",
        result
    );
    assert_eq!(io.written_u8.borrow()[0], (0x60, 0xCC));
    assert_eq!(cpu.regs.si(), 0x2001);
}

// 24. OUTSW with no REP in real mode
#[test]
fn test_outsw_no_rep_real_mode() {
    // OUTSW = 0x6F
    let (mut cpu, mut mem) = setup(&[0x6F]);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_si(0x2000);
    mem.ram[0x2000] = 0xEF;
    mem.ram[0x2001] = 0xBE;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "OUTSW should succeed, got {:?}",
        result
    );
    assert_eq!(io.written_u16.borrow()[0], (0x60, 0xBEEF));
    assert_eq!(cpu.regs.si(), 0x2002);
}

// 25. REP INSB with CX=4
#[test]
fn test_rep_insb() {
    // REP INSB = F3 6C
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6C]);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_di(0x3000);
    cpu.regs.set_cx(4);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = MockIo::new();
    for i in 0..4 {
        io.push_read_u8(0xA0 + i);
    }
    let _ = run(&mut cpu, &mut mem, &mut io, 16);
    // All 4 bytes should be written
    for i in 0..4u8 {
        assert_eq!(mem.ram[0x3000 + i as usize], 0xA0 + i);
    }
    assert_eq!(cpu.regs.cx(), 0);
}

// 26. REP OUTSB with CX=4
#[test]
fn test_rep_outsb() {
    // REP OUTSB = F3 6E
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6E]);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_si(0x3000);
    cpu.regs.set_cx(4);
    for i in 0..4u8 {
        mem.ram[0x3000 + i as usize] = 0xB0 + i;
    }
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = MockIo::new();
    let _ = run(&mut cpu, &mut mem, &mut io, 16);
    let writes = io.written_u8.borrow();
    assert_eq!(writes.len(), 4);
    for i in 0..4u8 {
        assert_eq!(writes[i as usize], (0x60, 0xB0 + i));
    }
    assert_eq!(cpu.regs.cx(), 0);
}

// 27. INSB with IOPL check failure in PM
#[test]
fn test_insb_iopl_gp() {
    // INSB = 0x6C in PM with CPL > IOPL
    let (mut cpu, mut mem) = setup_pm_ring3(&[0x6C], 0);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_di(0x2000);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("INSB with CPL>IOPL should #GP, got {:?}", other),
    }
}

// 28. OUTSB with IOPL check failure in PM
#[test]
fn test_outsb_iopl_gp() {
    // OUTSB = 0x6E
    let (mut cpu, mut mem) = setup_pm_ring3(&[0x6E], 0);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_si(0x2000);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("OUTSB with CPL>IOPL should #GP, got {:?}", other),
    }
}

// 29. INSW with IOPL check failure in PM
#[test]
fn test_insw_iopl_gp() {
    // INSW = 0x6D
    let (mut cpu, mut mem) = setup_pm_ring3(&[0x6D], 0);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_di(0x2000);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("INSW with CPL>IOPL should #GP, got {:?}", other),
    }
}

// 30. OUTSW with IOPL check failure in PM
#[test]
fn test_outsw_iopl_gp() {
    // OUTSW = 0x6F
    let (mut cpu, mut mem) = setup_pm_ring3(&[0x6F], 0);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_si(0x2000);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("OUTSW with CPL>IOPL should #GP, got {:?}", other),
    }
}

// ========================================================================
// Area 4: Shift/rotate with paging (memory operands)
// ========================================================================

// 31. SHL dword [addr], 1 with paging
#[test]
fn test_paging_shl_mem_dword_by1() {
    // D1 /4 with ModR/M for [0x5000]: D1 25 00 50 00 00
    // SHL dword [0x5000], 1
    let (mut cpu, mut mem) = setup_paging(&[0xD1, 0x25, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x01;
    mem.ram[0x5001] = 0x00;
    mem.ram[0x5002] = 0x00;
    mem.ram[0x5003] = 0x80; // 0x80000001
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    assert_eq!(val, 0x00000002); // shift left 1 from 0x80000001
}

// 32. SHR byte [addr], CL with paging
#[test]
fn test_paging_shr_mem_byte_cl() {
    // D2 /5 with ModR/M for [0x5000]: D2 2D 00 50 00 00
    // SHR byte [0x5000], CL
    let (mut cpu, mut mem) = setup_paging(&[0xD2, 0x2D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x80;
    cpu.regs.set_cl(3);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x5000], 0x10); // 0x80 >> 3 = 0x10
}

// 33. SAR word [addr], imm8 with paging
#[test]
fn test_paging_sar_mem_word_imm8() {
    // 66 C1 /7 with ModR/M for [0x5000]: 66 C1 3D 00 50 00 00 02
    // SAR word [0x5000], 2
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0xC1, 0x3D, 0x00, 0x50, 0x00, 0x00, 0x02]);
    mem.ram[0x5000] = 0x00;
    mem.ram[0x5001] = 0x80; // -32768
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u16::from_le_bytes([mem.ram[0x5000], mem.ram[0x5001]]);
    // SAR preserves sign: -32768 >> 2 = -8192 = 0xE000
    assert_eq!(val, 0xE000);
}

// 34. SAL (SHL) dword [addr], imm8 with paging
#[test]
fn test_paging_sal_mem_dword_imm8() {
    // C1 /4 with ModR/M for [0x5000]: C1 25 00 50 00 00 04
    // SHL dword [0x5000], 4
    let (mut cpu, mut mem) = setup_paging(&[0xC1, 0x25, 0x00, 0x50, 0x00, 0x00, 0x04]);
    mem.ram[0x5000] = 0xFF;
    mem.ram[0x5001] = 0x00;
    mem.ram[0x5002] = 0x00;
    mem.ram[0x5003] = 0x00; // 0xFF
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    assert_eq!(val, 0xFF0); // 0xFF << 4
}

// 35. ROL dword [addr], 1 with paging
#[test]
fn test_paging_rol_mem_dword_by1() {
    // D1 /0 with ModR/M for [0x5000]: D1 05 00 50 00 00
    // ROL dword [0x5000], 1
    let (mut cpu, mut mem) = setup_paging(&[0xD1, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x01;
    mem.ram[0x5001] = 0x00;
    mem.ram[0x5002] = 0x00;
    mem.ram[0x5003] = 0x80; // 0x80000001
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    // ROL 0x80000001 by 1 = 0x00000003
    assert_eq!(val, 0x00000003);
}

// 36. ROR byte [addr], CL with paging
#[test]
fn test_paging_ror_mem_byte_cl() {
    // D2 /1 with ModR/M for [0x5000]: D2 0D 00 50 00 00
    // ROR byte [0x5000], CL
    let (mut cpu, mut mem) = setup_paging(&[0xD2, 0x0D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x01;
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // ROR 0x01 by 1 = 0x80
    assert_eq!(mem.ram[0x5000], 0x80);
}

// 37. RCL word [addr], imm8 with paging
#[test]
fn test_paging_rcl_mem_word_imm8() {
    // 66 C1 /2 with ModR/M for [0x5000]: 66 C1 15 00 50 00 00 01
    // RCL word [0x5000], 1
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0xC1, 0x15, 0x00, 0x50, 0x00, 0x00, 0x01]);
    mem.ram[0x5000] = 0xFF;
    mem.ram[0x5001] = 0x7F; // 0x7FFF
    cpu.regs.flags.remove(Flags::CARRY); // CF=0
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u16::from_le_bytes([mem.ram[0x5000], mem.ram[0x5001]]);
    // RCL 0x7FFF, 1 with CF=0 -> 0xFFFE, CF=0
    assert_eq!(val, 0xFFFE);
}

// 38. RCR dword [addr], 1 with paging
#[test]
fn test_paging_rcr_mem_dword_by1() {
    // D1 /3 with ModR/M for [0x5000]: D1 1D 00 50 00 00
    // RCR dword [0x5000], 1
    let (mut cpu, mut mem) = setup_paging(&[0xD1, 0x1D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x01;
    mem.ram[0x5001] = 0x00;
    mem.ram[0x5002] = 0x00;
    mem.ram[0x5003] = 0x00; // 0x00000001
    cpu.regs.flags.insert(Flags::CARRY); // CF=1
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    // RCR 0x00000001, 1 with CF=1 -> 0x80000000, CF=1
    assert_eq!(val, 0x80000000);
}

// 39. SHR dword [addr], imm8 with paging
#[test]
fn test_paging_shr_mem_dword_imm8() {
    // C1 /5 with ModR/M for [0x5000]: C1 2D 00 50 00 00 08
    // SHR dword [0x5000], 8
    let (mut cpu, mut mem) = setup_paging(&[0xC1, 0x2D, 0x00, 0x50, 0x00, 0x00, 0x08]);
    mem.ram[0x5000] = 0x00;
    mem.ram[0x5001] = 0xFF;
    mem.ram[0x5002] = 0x00;
    mem.ram[0x5003] = 0x00; // 0x0000FF00
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    assert_eq!(val, 0xFF); // 0x0000FF00 >> 8
}

// 40. SHL word [addr], CL with paging
#[test]
fn test_paging_shl_mem_word_cl() {
    // 66 D3 /4 with ModR/M for [0x5000]: 66 D3 25 00 50 00 00
    // SHL word [0x5000], CL
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0xD3, 0x25, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x01;
    mem.ram[0x5001] = 0x00;
    cpu.regs.set_cl(5);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u16::from_le_bytes([mem.ram[0x5000], mem.ram[0x5001]]);
    assert_eq!(val, 0x0020); // 1 << 5
}

// ========================================================================
// Area 5: Protected mode exception handling paths
// ========================================================================

// 41. MOV to segment register with null selector should #GP in PM
#[test]
fn test_pm_mov_ds_null_selector_gp() {
    // MOV DS, AX = 8E D8 with AX = 0 (null selector)
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xD8]);
    cpu.regs.eax = 0; // null selector
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // Loading null into DS should succeed at CPL 0 (null is allowed for DS/ES/FS/GS)
    // Actually, x86 allows null selector for data segments at any CPL
    assert!(
        matches!(result, RunResult::BudgetExhausted { .. }),
        "MOV DS, 0 should succeed (null allowed for data segs), got {:?}",
        result
    );
}

// 42. MOV SS, invalid selector should #GP
#[test]
fn test_pm_mov_ss_invalid_selector_gp() {
    // MOV SS, AX = 8E D0 with AX pointing beyond GDT
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xD0]);
    cpu.regs.eax = 0xFF00; // selector way beyond GDT limit
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception { vector: 13, .. } | RunResult::Exception { vector: 12, .. } => {}
        other => panic!("MOV SS with invalid selector should fault, got {:?}", other),
    }
}

// 42b. MOV SS with a not-present stack segment must #SS(12), not #GP.
// fault-fidelity: the SS arm previously flattened every
// `load_ss` error to #GP; a valid-but-not-present writable-data selector is
// an #SS(selector), matching the sibling DS/POP-DS arms' error mapping.
#[test]
fn test_pm_mov_ss_not_present_is_ss_fault() {
    // MOV SS, AX = 8E D0.
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xD0]);
    // GDT slot 8 (selector 0x40): writable data, DPL=0, but P=0 (access 0x12).
    let gdt_base = cpu.regs.gdtr.base as usize;
    write_gdt_entry(
        &mut mem,
        gdt_base + 0x40,
        make_descriptor(0, 0xFFFFF, 0x12, 0x0C),
    );
    cpu.regs.gdtr.limit = 9 * 8 - 1;
    cpu.regs.eax = 0x40; // RPL=0, index 8 — privilege matches CPL=0
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 12, .. }),
        "MOV SS with a not-present stack segment must #SS(12), got {:?}",
        result
    );
}

// 43. POP with segment not present should #NP(selector)
#[test]
fn test_pm_load_not_present_segment_np() {
    // MOV ES, AX = 8E C0 with a selector pointing to a not-present segment
    let gdt_base: u32 = 0x10000;
    // Write a not-present data segment at slot 9 (index 0x48)
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xC0]);
    // Access byte 0x12 = data, not-present (bit 7 = 0)
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x48,
        make_descriptor(0, 0xFFFFF, 0x12, 0x0C),
    );
    cpu.regs.gdtr.limit = 10 * 8 - 1; // extend GDT to cover slot 9
    cpu.regs.eax = 0x48; // selector
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 11,
            error_code,
            ..
        } => {
            assert_eq!(
                error_code,
                (0x48 & 0xFFFC) as u32,
                "error code should be selector & 0xFFFC"
            );
        }
        other => panic!(
            "Loading not-present segment should #NP(selector), got {:?}",
            other
        ),
    }
}

// 44. CLI in PM with CPL > IOPL should #GP
#[test]
fn test_pm_cli_cpl_gt_iopl_gp() {
    // CLI = 0xFA with CPL=3, IOPL=0
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xFA], 0);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("CLI with CPL>IOPL should #GP(0), got {:?}", other),
    }
}

// 45. STI in PM with CPL > IOPL should #GP
#[test]
fn test_pm_sti_cpl_gt_iopl_gp() {
    // STI = 0xFB with CPL=3, IOPL=0
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xFB], 0);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!("STI with CPL>IOPL should #GP(0), got {:?}", other),
    }
}

// 46. CLI in PM with CPL <= IOPL should succeed
#[test]
fn test_pm_cli_cpl_le_iopl_succeeds() {
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xFA], 3);
    cpu.regs.flags.insert(Flags::INTERRUPT);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CLI with CPL<=IOPL should succeed, got {:?}",
        result
    );
    assert!(!cpu.regs.flags.contains(Flags::INTERRUPT));
}

// 47. STI in PM with CPL <= IOPL should succeed
#[test]
fn test_pm_sti_cpl_le_iopl_succeeds() {
    let (mut cpu, mut mem) = setup_pm_ring3(&[0xFB], 3);
    cpu.regs.flags.remove(Flags::INTERRUPT);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::InterruptPollBoundary { executed: 1 }),
        "STI with CPL<=IOPL should succeed, got {:?}",
        result
    );
    assert!(cpu.regs.flags.contains(Flags::INTERRUPT));
}

// ========================================================================
// Area 6: 16-bit protected mode paths
// ========================================================================

// 48. 66h prefix in 16-bit PM: MOV EAX, imm32
#[test]
fn test_pm16_66h_mov_eax_imm32() {
    // 66 B8 DD CC BB AA = MOV EAX, 0xAABBCCDD (66h prefix in 16-bit CS)
    let (mut cpu, mut mem) = setup_pm_ring3(&[0x66, 0xB8, 0xDD, 0xCC, 0xBB, 0xAA], 3);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xAABBCCDD);
}

// 49. 66h prefix in 16-bit PM: ADD EAX, imm32
#[test]
fn test_pm16_66h_add_eax_imm32() {
    // 66 05 00 00 01 00 = ADD EAX, 0x00010000
    let (mut cpu, mut mem) = setup_pm_ring3(&[0x66, 0x05, 0x00, 0x00, 0x01, 0x00], 3);
    cpu.regs.eax = 0x0000FFFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0001FFFF);
}

// 50. 66h prefix in 16-bit PM: PUSH EAX / POP EAX
#[test]
fn test_pm16_66h_push_pop_eax() {
    // 66 50 = PUSH EAX, 66 58 = POP EAX
    let (mut cpu, mut mem) = setup_pm_ring3(&[0x66, 0x50, 0x66, 0x58], 3);
    cpu.regs.eax = 0xDEADBEEF;
    let sp_before = cpu.regs.sp();
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0xDEADBEEF);
    assert_eq!(cpu.regs.sp(), sp_before);
}

// 51. 66h prefix in 16-bit PM: CMP EAX, imm32
#[test]
fn test_pm16_66h_cmp_eax_imm32() {
    // 66 3D 00 00 01 00 = CMP EAX, 0x00010000
    let (mut cpu, mut mem) = setup_pm_ring3(&[0x66, 0x3D, 0x00, 0x00, 0x01, 0x00], 3);
    cpu.regs.eax = 0x00010000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// 52. 16-bit PM stack: PUSH AX / POP BX
#[test]
fn test_pm16_push_ax_pop_bx() {
    // 50 = PUSH AX, 5B = POP BX
    let (mut cpu, mut mem) = setup_pm_ring3(&[0x50, 0x5B], 3);
    cpu.regs.set_ax(0x1234);
    cpu.regs.set_bx(0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.bx(), 0x1234);
}

// 53. 16-bit PM: PUSHA / POPA
#[test]
fn test_pm16_pusha_popa() {
    // 60 = PUSHA, 61 = POPA
    let (mut cpu, mut mem) = setup_pm_ring3(&[0x60, 0x61], 3);
    cpu.regs.set_ax(0x1111);
    cpu.regs.set_cx(0x2222);
    cpu.regs.set_dx(0x3333);
    cpu.regs.set_bx(0x4444);
    cpu.regs.set_si(0x5555);
    cpu.regs.set_di(0x6666);
    cpu.regs.set_bp(0x7777);
    let sp_before = cpu.regs.sp();
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.ax(), 0x1111);
    assert_eq!(cpu.regs.cx(), 0x2222);
    assert_eq!(cpu.regs.dx(), 0x3333);
    assert_eq!(cpu.regs.bx(), 0x4444);
    assert_eq!(cpu.regs.si(), 0x5555);
    assert_eq!(cpu.regs.di(), 0x6666);
    assert_eq!(cpu.regs.bp(), 0x7777);
    assert_eq!(cpu.regs.sp(), sp_before);
}

// ========================================================================
// Additional coverage: page fault on write-protected page
// ========================================================================

// 54. Page fault on write to write-protected page
#[test]
fn test_paging_write_protected_page_fault() {
    // MOV [0x50000], EAX = 89 05 00 00 05 00
    // Map page 0x50 as read-only (present but not writable)
    let (mut cpu, mut mem) = setup_paging(&[0x89, 0x05, 0x00, 0x00, 0x05, 0x00]);
    // Page 0x50 = PTE at 0x11000 + 0x50*4 = 0x11140
    let pte_addr = 0x11000 + 0x50 * 4;
    // Set present + supervisor but NOT writable (bit 1 = 0)
    mem.write_u32_physical(pte_addr, (0x50 << 12) | 0x05); // P=1, R/W=0, U/S=1
                                                           // Enable CR0.WP so supervisor writes to read-only pages fault
    cpu.regs.cr0 |= 0x0001_0000; // WP bit
    cpu.regs.eax = 0xDEADBEEF;
    mem.invalidate_tlb();
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 14,
            fault_addr: Some(addr),
            ..
        } => {
            assert_eq!(addr, 0x50000);
        }
        other => panic!(
            "Write to WP page should #PF with fault addr, got {:?}",
            other
        ),
    }
}

// 55. Page fault on read from non-present page
#[test]
fn test_paging_read_nonpresent_page_fault() {
    // MOV EAX, [0x400000] = 8B 05 00 00 40 00
    // Address 0x400000 is in PDE[1] which doesn't exist (only PDE[0] is set up)
    let (mut cpu, mut mem) = setup_paging(&[0x8B, 0x05, 0x00, 0x00, 0x40, 0x00]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 14,
            fault_addr: Some(addr),
            ..
        } => {
            assert_eq!(addr, 0x400000);
        }
        other => panic!("Read from non-present page should #PF, got {:?}", other),
    }
}

// ========================================================================
// Additional coverage: More V86 mode I/O bitmap paths
// ========================================================================

// 56. V86 mode: IN AL with I/O bitmap allowing port
#[test]
fn test_v86_in_al_bitmap_allows() {
    let (mut cpu, mut mem) = setup_v86(&[0xE4, 0x42], 0);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x42 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x42, 1);
    let mut io = MockIo::new();
    io.push_read_u8(0xCC);
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "V86 IN AL with bitmap allowed should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.al(), 0xCC);
}

// 57. V86 mode: IN AL with I/O bitmap denying port
#[test]
fn test_v86_in_al_bitmap_denies() {
    let (mut cpu, mut mem) = setup_v86(&[0xE4, 0x42], 3);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x42 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    // Port 0x42 is denied (all bits 1 by default)
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        } => {}
        other => panic!(
            "V86 IN AL with bitmap denied should #GP(0), got {:?}",
            other
        ),
    }
}

// 58. V86 mode: OUT AL with bitmap allowing port
#[test]
fn test_v86_out_al_bitmap_allows() {
    let (mut cpu, mut mem) = setup_v86(&[0xE6, 0x42], 0);
    cpu.regs.set_al(0xDD);
    let tss_base = 0x5000u32;
    let io_map_off = 0x68u16;
    let tss_limit = 0x68 + (0x42 / 8) as u32 + 1;
    setup_tss(&mut mem, &mut cpu, tss_base, tss_limit, io_map_off);
    allow_port(&mut mem, tss_base, io_map_off, 0x42, 1);
    let mut io = MockIo::new();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "V86 OUT AL with bitmap allowed should succeed, got {:?}",
        result
    );
    assert_eq!(io.written_u8.borrow()[0], (0x42, 0xDD));
}

// ========================================================================
// Additional coverage: INSB/OUTSB with paging and backward direction
// ========================================================================

// 59. INSB with DF=1 (backward)
#[test]
fn test_insb_backward() {
    // STD; INSB = FD 6C
    let (mut cpu, mut mem) = setup(&[0xFD, 0x6C]);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_di(0x2005);
    let mut io = MockIo::new();
    io.push_read_u8(0xEE);
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(mem.ram[0x2005], 0xEE);
    assert_eq!(cpu.regs.di(), 0x2004);
}

// 60. OUTSB with DF=1 (backward)
#[test]
fn test_outsb_backward() {
    // STD; OUTSB = FD 6E
    let (mut cpu, mut mem) = setup(&[0xFD, 0x6E]);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_si(0x2005);
    mem.ram[0x2005] = 0xAA;
    let mut io = MockIo::new();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(io.written_u8.borrow()[0], (0x60, 0xAA));
    assert_eq!(cpu.regs.si(), 0x2004);
}

// ========================================================================
// Additional coverage: REP INSW / REP OUTSW
// ========================================================================

// 61. REP INSW with CX=3
#[test]
fn test_rep_insw() {
    // REP INSW = F3 6D
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6D]);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_di(0x3000);
    cpu.regs.set_cx(3);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = MockIo::new();
    io.push_read_u16(0x1111);
    io.push_read_u16(0x2222);
    io.push_read_u16(0x3333);
    let _ = run(&mut cpu, &mut mem, &mut io, 16);
    let v0 = u16::from_le_bytes([mem.ram[0x3000], mem.ram[0x3001]]);
    let v1 = u16::from_le_bytes([mem.ram[0x3002], mem.ram[0x3003]]);
    let v2 = u16::from_le_bytes([mem.ram[0x3004], mem.ram[0x3005]]);
    assert_eq!(v0, 0x1111);
    assert_eq!(v1, 0x2222);
    assert_eq!(v2, 0x3333);
    assert_eq!(cpu.regs.cx(), 0);
}

// 62. REP OUTSW with CX=3
#[test]
fn test_rep_outsw() {
    // REP OUTSW = F3 6F
    let (mut cpu, mut mem) = setup(&[0xF3, 0x6F]);
    cpu.regs.set_dx(0x60);
    cpu.regs.set_si(0x3000);
    cpu.regs.set_cx(3);
    mem.ram[0x3000] = 0x11;
    mem.ram[0x3001] = 0x11;
    mem.ram[0x3002] = 0x22;
    mem.ram[0x3003] = 0x22;
    mem.ram[0x3004] = 0x33;
    mem.ram[0x3005] = 0x33;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = MockIo::new();
    let _ = run(&mut cpu, &mut mem, &mut io, 16);
    let writes = io.written_u16.borrow();
    assert_eq!(writes.len(), 3);
    assert_eq!(writes[0], (0x60, 0x1111));
    assert_eq!(writes[1], (0x60, 0x2222));
    assert_eq!(writes[2], (0x60, 0x3333));
    assert_eq!(cpu.regs.cx(), 0);
}

// ========================================================================
// Additional coverage: SHLD/SHRD with paging (memory operands)
// ========================================================================

// 63. SHLD [mem], reg, imm8 with paging (32-bit)
#[test]
fn test_paging_shld_mem_dword_imm8() {
    // SHLD [0x5000], EBX, 8: 0F A4 1D 00 50 00 00 08
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xA4, 0x1D, 0x00, 0x50, 0x00, 0x00, 0x08]);
    mem.ram[0x5000] = 0x00;
    mem.ram[0x5001] = 0x00;
    mem.ram[0x5002] = 0xFF;
    mem.ram[0x5003] = 0x00; // 0x00FF0000
    cpu.regs.ebx = 0xAABBCCDD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    // SHLD: shift 0x00FF0000 left by 8, fill with top 8 bits of EBX (0xAA)
    assert_eq!(val, 0xFF0000AA);
}

// 64. SHRD [mem], reg, imm8 with paging (32-bit)
#[test]
fn test_paging_shrd_mem_dword_imm8() {
    // SHRD [0x5000], EBX, 8: 0F AC 1D 00 50 00 00 08
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xAC, 0x1D, 0x00, 0x50, 0x00, 0x00, 0x08]);
    mem.ram[0x5000] = 0x00;
    mem.ram[0x5001] = 0x00;
    mem.ram[0x5002] = 0xFF;
    mem.ram[0x5003] = 0x00; // 0x00FF0000
    cpu.regs.ebx = 0xAABBCCDD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    // SHRD: shift 0x00FF0000 right by 8, fill with bottom 8 bits of EBX (0xDD)
    assert_eq!(val, 0xDD00FF00);
}

// 65. SHLD [mem], reg, CL with paging (32-bit)
#[test]
fn test_paging_shld_mem_dword_cl() {
    // 0F A5 1D 00 50 00 00 = SHLD [0x5000], EBX, CL
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xA5, 0x1D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x01;
    mem.ram[0x5001] = 0x00;
    mem.ram[0x5002] = 0x00;
    mem.ram[0x5003] = 0x00; // 0x00000001
    cpu.regs.ebx = 0x80000000;
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    // SHLD: shift 0x00000001 left by 1, fill with top 1 bit of EBX (1)
    assert_eq!(val, 0x00000003);
}

// 66. SHRD [mem], reg, CL with paging (32-bit)
#[test]
fn test_paging_shrd_mem_dword_cl() {
    // 0F AD 1D 00 50 00 00 = SHRD [0x5000], EBX, CL
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xAD, 0x1D, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x00;
    mem.ram[0x5001] = 0x00;
    mem.ram[0x5002] = 0x00;
    mem.ram[0x5003] = 0x80; // 0x80000000
    cpu.regs.ebx = 0x00000001;
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    // SHRD: shift 0x80000000 right by 1, fill with bottom 1 bit of EBX (1)
    assert_eq!(val, 0xC0000000);
}

// ========================================================================
// Additional: CALL FAR gate paths and protected mode JMP FAR
// ========================================================================

fn assert_far_target_limit_fault(op_size_32: bool, indirect: bool, call: bool) {
    let opcode = if call { 0x9A } else { 0xEA };
    let (mut cpu, mut mem, target_selector) = if op_size_32 {
        let code = if indirect {
            vec![0xFF, if call { 0x1D } else { 0x2D }, 0x00, 0x20, 0x00, 0x00]
        } else {
            vec![opcode, 0x00, 0x01, 0x00, 0x00, 0x28, 0x00]
        };
        let (cpu, mut mem) = setup_pm32(&code);
        if indirect {
            mem.write_u32(0x2000, 0x0000_0100);
            mem.write_u16(0x2004, 0x0028);
        }
        (cpu, mem, 0x0028)
    } else {
        let code = if indirect {
            vec![0xFF, if call { 0x1E } else { 0x2E }, 0x00, 0x20]
        } else {
            vec![opcode, 0x00, 0x01, 0x18, 0x00]
        };
        let (cpu, mut mem) = setup_pm16(&code);
        if indirect {
            mem.write_u16(0x2000, 0x0100);
            mem.write_u16(0x2002, 0x0018);
        }
        (cpu, mem, 0x0018)
    };

    let gdt_base = cpu.regs.gdtr.base as usize;
    write_gdt_entry(
        &mut mem,
        gdt_base + target_selector as usize,
        make_descriptor(0x4000, 0x00FF, 0x9A, if op_size_32 { 0x04 } else { 0x00 }),
    );

    let source_cs = cpu.regs.cs;
    let source_cpl = cpu.regs.cpl;
    let source_cs_bits = cpu.cs_bits;
    let source_cache = format!("{:?}", cpu.regs.cs_cache);
    let source_eip = cpu.regs.eip;
    let source_esp = cpu.regs.esp;
    let source_stack = mem.ram[0x7FF8..0x8000].to_vec();
    let source_ret_guard_depth = cpu.ret_guard_depth();

    let context = format!(
        "{} {}-bit {} far target beyond CS limit",
        if indirect { "indirect" } else { "immediate" },
        if op_size_32 { 32 } else { 16 },
        if call { "CALL" } else { "JMP" },
    );
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
        "{context}: expected #GP(0), got {result:?}"
    );
    assert_eq!(cpu.regs.cs, source_cs, "{context}: CS changed");
    assert_eq!(cpu.regs.cpl, source_cpl, "{context}: CPL changed");
    assert_eq!(cpu.cs_bits, source_cs_bits, "{context}: CS width changed");
    assert_eq!(
        format!("{:?}", cpu.regs.cs_cache),
        source_cache,
        "{context}: hidden CS cache changed"
    );
    assert_eq!(cpu.regs.eip, source_eip, "{context}: EIP changed");
    assert_eq!(cpu.regs.esp, source_esp, "{context}: ESP changed");
    assert_eq!(
        &mem.ram[0x7FF8..0x8000],
        source_stack.as_slice(),
        "{context}: return frame was written"
    );
    assert_eq!(
        cpu.ret_guard_depth(),
        source_ret_guard_depth,
        "{context}: return guard changed"
    );
}

macro_rules! far_target_limit_test {
    ($name:ident, $op_size_32:expr, $indirect:expr, $call:expr) => {
        #[test]
        fn $name() {
            assert_far_target_limit_fault($op_size_32, $indirect, $call);
        }
    };
}

far_target_limit_test!(
    test_pm16_immediate_far_call_target_limit_is_prevalidated,
    false,
    false,
    true
);
far_target_limit_test!(
    test_pm32_immediate_far_call_target_limit_is_prevalidated,
    true,
    false,
    true
);
far_target_limit_test!(
    test_pm16_far_indirect_call_limit_is_prevalidated,
    false,
    true,
    true
);
far_target_limit_test!(
    test_pm32_far_indirect_call_limit_is_prevalidated,
    true,
    true,
    true
);
far_target_limit_test!(
    test_pm16_immediate_far_jmp_target_limit_is_prevalidated,
    false,
    false,
    false
);
far_target_limit_test!(
    test_pm32_immediate_far_jmp_target_limit_is_prevalidated,
    true,
    false,
    false
);
far_target_limit_test!(
    test_pm16_indirect_far_jmp_target_limit_is_prevalidated,
    false,
    true,
    false
);
far_target_limit_test!(
    test_pm32_indirect_far_jmp_target_limit_is_prevalidated,
    true,
    true,
    false
);

// 67. JMP FAR to TSS descriptor triggers TaskSwitchJmp
#[test]
fn test_pm_jmp_far_tss_descriptor() {
    // EA offset32 selector16 = JMP FAR selector:offset
    // EA 00 00 00 00 48 00 = JMP FAR 0x0048:0x00000000
    let (mut cpu, mut mem) = setup_pm32(&[0xEA, 0x00, 0x00, 0x00, 0x00, 0x48, 0x00]);
    // Write a TSS descriptor at GDT slot 9 (selector 0x48)
    // Access byte 0x89 = 32-bit TSS, available, DPL=0, present
    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x48,
        make_descriptor(0x6000, 0x67, 0x89, 0x00),
    );
    cpu.regs.gdtr.limit = 10 * 8 - 1;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::TaskSwitchJmp { selector: 0x48, .. } => {}
        other => panic!(
            "JMP FAR to TSS should trigger TaskSwitchJmp, got {:?}",
            other
        ),
    }
    assert_eq!(
        cpu.regs.eip, 0x0107,
        "immediate JMP FAR to TSS should save EIP at the next instruction"
    );
}

#[test]
fn test_pm_indirect_jmp_far_tss_descriptor_saves_instruction_end_eip() {
    // FF /5 m16:32 = JMP FAR through memory. The instruction is six bytes:
    // FF 2D disp32. The outgoing TSS must save EIP pointing
    // at the byte immediately after this instruction, not one byte later.
    let (mut cpu, mut mem) = setup_pm32(&[0xFF, 0x2D, 0x00, 0x20, 0x00, 0x00, 0xFB]);
    mem.write_u32(0x2000, 0x0000_0000);
    mem.write_u16(0x2004, 0x0048);
    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x48,
        make_descriptor(0x6000, 0x67, 0x89, 0x00),
    );
    cpu.regs.gdtr.limit = 10 * 8 - 1;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::TaskSwitchJmp { selector: 0x48, .. } => {}
        other => panic!(
            "indirect JMP FAR to TSS should trigger TaskSwitchJmp, got {:?}",
            other
        ),
    }
    assert_eq!(
        cpu.regs.eip, 0x0106,
        "indirect JMP FAR to TSS should save EIP at the following STI byte"
    );
}

// 68. CALL FAR to TSS descriptor triggers TaskSwitchCall
#[test]
fn test_pm_call_far_tss_descriptor() {
    // 9A offset32 selector16 = CALL FAR selector:offset
    // 9A 00 00 00 00 48 00 = CALL FAR 0x0048:0x00000000
    let (mut cpu, mut mem) = setup_pm32(&[0x9A, 0x00, 0x00, 0x00, 0x00, 0x48, 0x00]);
    // Write a TSS descriptor at GDT slot 9 (selector 0x48)
    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x48,
        make_descriptor(0x6000, 0x67, 0x89, 0x00),
    );
    cpu.regs.gdtr.limit = 10 * 8 - 1;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::TaskSwitchCall { selector: 0x48, .. } => {}
        other => panic!(
            "CALL FAR to TSS should trigger TaskSwitchCall, got {:?}",
            other
        ),
    }
    assert_eq!(
        cpu.regs.eip, 0x0107,
        "immediate CALL FAR to TSS should save EIP at the next instruction"
    );
}

#[test]
fn test_pm_indirect_call_far_tss_descriptor_saves_instruction_end_eip() {
    // FF /3 m16:32 = CALL FAR through memory. Task-switch CALL has
    // different nesting semantics from JMP, so keep its EIP accounting
    // pinned separately.
    let (mut cpu, mut mem) = setup_pm32(&[0xFF, 0x1D, 0x00, 0x20, 0x00, 0x00, 0xFB]);
    mem.write_u32(0x2000, 0x0000_0000);
    mem.write_u16(0x2004, 0x0048);
    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x48,
        make_descriptor(0x6000, 0x67, 0x89, 0x00),
    );
    cpu.regs.gdtr.limit = 10 * 8 - 1;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::TaskSwitchCall { selector: 0x48, .. } => {}
        other => panic!(
            "indirect CALL FAR to TSS should trigger TaskSwitchCall, got {:?}",
            other
        ),
    }
    assert_eq!(
        cpu.regs.eip, 0x0106,
        "indirect CALL FAR to TSS should save EIP at the following byte"
    );
}

fn call_gate_descriptor(target_selector: u16, target_offset: u32, access: u8) -> [u8; 8] {
    [
        target_offset as u8,
        (target_offset >> 8) as u8,
        target_selector as u8,
        (target_selector >> 8) as u8,
        0x00,
        access,
        (target_offset >> 16) as u8,
        (target_offset >> 24) as u8,
    ]
}

#[test]
fn test_mov_sreg_memory_limit_check_reads_only_selector_word() {
    // ES:[DI+0E] spans exactly the final two bytes of a 16-byte segment.
    // MOV Sreg,r/m16 reads only that selector word; checking 4/6 bytes
    // raises a false #GP during a task switch.
    let (mut cpu, mut mem) = setup_pm16(&[0x26, 0x8E, 0x45, 0x0E]);
    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        make_descriptor(0x3000, 0x000F, 0x92, 0x00),
    );
    cpu.regs.es = 0x18;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, 0x18).unwrap();
    cpu.regs.set_di(0);
    mem.write_u16(0x300E, 0x10);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "MOV ES, ES:[DI+0E] should not fault when the selector word fits, got {:?}",
        result
    );
    assert_eq!(cpu.regs.es, 0x10);
    assert_eq!(cpu.regs.ip(), 0x0104);
}

// 69. CALL FAR to call gate descriptor triggers CallGate
#[test]
fn test_pm_call_far_call_gate() {
    // 9A offset32 selector16 = CALL FAR selector:offset
    // 9A 00 00 00 00 48 00 = CALL FAR 0x0048:0x00000000
    let (mut cpu, mut mem) = setup_pm32(&[0x9A, 0x00, 0x00, 0x00, 0x00, 0x48, 0x00]);
    // Write a call gate descriptor at GDT slot 9 (selector 0x48)
    // Call gate format: offset[15:0] in bytes 0-1, selector in bytes 2-3,
    // param count + access in bytes 4-5, offset[31:16] in bytes 6-7
    // Access: 0x8C = 32-bit call gate, DPL=0, present
    let gdt_base = cpu.regs.gdtr.base;
    let gate_desc = call_gate_descriptor(0x08, 0, 0x8C);
    write_gdt_entry(&mut mem, gdt_base as usize + 0x48, gate_desc);
    cpu.regs.gdtr.limit = 10 * 8 - 1;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::CallGate {
            selector: 0x48,
            return_cs,
            return_eip,
            ..
        } => {
            assert_eq!(return_cs, 0x08);
            assert_eq!(return_eip, 0x0107);
        }
        other => panic!(
            "CALL FAR to call gate should trigger CallGate, got {:?}",
            other
        ),
    }
}

#[test]
fn test_pm16_immediate_far_call_wraps_between_frame_words() {
    // CALL FAR 0x0018:0x0200. With SP=2, the CS word lands at 0..1
    // and the return IP lands at FFFE..FFFF; the frame is not contiguous.
    let (mut cpu, mut mem) = setup_pm16(&[0x9A, 0x00, 0x02, 0x18, 0x00]);
    cpu.regs.set_sp(0x0002);
    cpu.regs.esp = 0x0002;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "wrapped 16-bit CALL FAR should succeed, got {result:?}"
    );
    assert_eq!(cpu.regs.cs, 0x18);
    assert_eq!(cpu.regs.ip(), 0x0200);
    assert_eq!(cpu.regs.sp(), 0xFFFE);
    assert_eq!(mem.read_u16(0x0000), 0x0008);
    assert_eq!(mem.read_u16(0xFFFE), 0x0105);
}

#[test]
fn test_pm16_indirect_far_call_wraps_between_frame_words() {
    // FF /3 m16:16 = CALL FAR [0600]. The pointer targets 0018:0200.
    let (mut cpu, mut mem) = setup_pm16(&[0xFF, 0x1E, 0x00, 0x06]);
    mem.write_u16(0x0600, 0x0200);
    mem.write_u16(0x0602, 0x0018);
    cpu.regs.set_sp(0x0002);
    cpu.regs.esp = 0x0002;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "wrapped 16-bit indirect CALL FAR should succeed, got {result:?}"
    );
    assert_eq!(cpu.regs.cs, 0x18);
    assert_eq!(cpu.regs.ip(), 0x0200);
    assert_eq!(cpu.regs.sp(), 0xFFFE);
    assert_eq!(mem.read_u16(0x0000), 0x0008);
    assert_eq!(mem.read_u16(0xFFFE), 0x0104);
}

#[test]
fn test_pm16_iret_wraps_between_frame_words() {
    // IRET pops IP at FFFC, CS at FFFE, and FLAGS at 0000.
    let (mut cpu, mut mem) = setup_pm16(&[0xCF]);
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.esp = 0xFFFC;
    mem.write_u16(0xFFFC, 0xFFFF);
    mem.write_u16(0xFFFE, 0x0018);
    mem.write_u16(0x0000, 0x0202);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(
        matches!(result, RunResult::Iret { executed: 1 }),
        "wrapped 16-bit IRET should succeed, got {result:?}"
    );
    assert_eq!(cpu.regs.cs, 0x18);
    assert_eq!(cpu.regs.ip(), 0xFFFF);
    assert_eq!(cpu.regs.sp(), 0x0002);
}

#[test]
fn test_pm32_immediate_far_call_wraps_between_frame_dwords() {
    // With ESP=4, the two dword return-frame items land at 0 and FFFFFFFC.
    let (mut cpu, mut mem) = setup_pm32(&[0x9A, 0x00, 0x02, 0x00, 0x00, 0x08, 0x00]);
    cpu.regs.esp = 0x0000_0004;
    cpu.regs.set_sp(0x0004);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "wrapped 32-bit CALL FAR should succeed, got {result:?}"
    );
    assert_eq!(cpu.regs.cs, 0x08);
    assert_eq!(cpu.regs.eip, 0x0200);
    assert_eq!(cpu.regs.esp, 0xFFFF_FFFC);
}

#[test]
fn test_pm32_indirect_far_call_wraps_between_frame_dwords() {
    // FF /3 m16:32 = CALL FAR [0600], targeting 0008:0200.
    let (mut cpu, mut mem) = setup_pm32(&[0xFF, 0x1D, 0x00, 0x06, 0x00, 0x00]);
    mem.write_u32(0x0600, 0x0000_0200);
    mem.write_u16(0x0604, 0x0008);
    cpu.regs.esp = 0x0000_0004;
    cpu.regs.set_sp(0x0004);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "wrapped 32-bit indirect CALL FAR should succeed, got {result:?}"
    );
    assert_eq!(cpu.regs.cs, 0x08);
    assert_eq!(cpu.regs.eip, 0x0200);
    assert_eq!(cpu.regs.esp, 0xFFFF_FFFC);
}

#[test]
fn test_pm32_iretd_wraps_between_frame_dwords() {
    // IRETD pops EIP at FFFFFFFC, CS at 00000000, and EFLAGS at 00000004.
    // The high-address read is intentionally outside the backing RAM; the
    // full-limit preflight must still not report a false #SS first.
    let (mut cpu, mut mem) = setup_pm32(&[0xCF]);
    cpu.regs.esp = 0xFFFF_FFFC;
    cpu.regs.set_sp(0xFFFC);
    mem.write_u32(0x0000, 0x0000_0008);
    mem.write_u32(0x0004, 0x0000_0202);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(
        matches!(result, RunResult::Iret { executed: 1 }),
        "wrapped 32-bit IRETD should succeed, got {result:?}"
    );
    assert_eq!(cpu.regs.cs, 0x08);
    assert_eq!(cpu.regs.eip, 0xFFFF_FFFF);
    assert_eq!(cpu.regs.esp, 0x0000_0008);
}

#[test]
fn test_pm32_call_far_code_stack_boundary_fault_is_ss() {
    let (mut cpu, mut mem) = setup_pm32(&[0x9A, 0x00, 0x20, 0x00, 0x00, 0x08, 0x00]);
    cpu.regs.esp = 0x0020;
    cpu.regs.set_sp(0x0020);
    cpu.regs.ss_cache.limit = 0x001B;
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.recompute_effective_limit();

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 12, .. }),
        "normal PM32 far CALL with an invalid return-frame slot should #SS, got {:?}",
        result
    );
    assert_eq!(cpu.regs.cs, 0x08);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.esp, 0x0020);
}

#[test]
fn test_pm32_call_far_call_gate_not_masked_by_stack_boundary() {
    let (mut cpu, mut mem) = setup_pm32(&[0x9A, 0x00, 0x00, 0x00, 0x00, 0x48, 0x00]);
    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x48,
        call_gate_descriptor(0x08, 0, 0x8C),
    );
    cpu.regs.gdtr.limit = 10 * 8 - 1;
    cpu.regs.esp = 0x0020;
    cpu.regs.set_sp(0x0020);
    cpu.regs.ss_cache.limit = 0x001B;
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.recompute_effective_limit();

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::CallGate {
            selector: 0x48,
            return_cs: 0x08,
            return_eip: 0x0107,
            ..
        } => {}
        other => panic!(
            "CALL FAR to call gate should not be masked by return-frame #SS, got {:?}",
            other
        ),
    }
    assert_eq!(cpu.regs.esp, 0x0020);
}

#[test]
fn test_pm32_indirect_call_far_call_gate_not_masked_by_stack_boundary() {
    let (mut cpu, mut mem) = setup_pm32(&[0xFF, 0x1D, 0x00, 0x20, 0x00, 0x00]);
    mem.write_u32(0x2000, 0);
    mem.write_u16(0x2004, 0x48);

    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x48,
        call_gate_descriptor(0x08, 0, 0x8C),
    );
    cpu.regs.gdtr.limit = 10 * 8 - 1;
    cpu.regs.esp = 0x0020;
    cpu.regs.set_sp(0x0020);
    cpu.regs.ss_cache.limit = 0x001B;
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.recompute_effective_limit();

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::CallGate {
            selector: 0x48,
            return_cs: 0x08,
            return_eip: 0x0106,
            ..
        } => {}
        other => panic!(
            "indirect CALL FAR to call gate should not be masked by return-frame #SS, got {:?}",
            other
        ),
    }
    assert_eq!(cpu.regs.esp, 0x0020);
}

#[test]
fn test_pm16_indirect_call_far_call_gate_not_masked_by_stack_boundary() {
    let (mut cpu, mut mem) = setup_pm16(&[0xFF, 0x1E, 0x00, 0x20]);
    mem.write_u16(0x2000, 0);
    mem.write_u16(0x2002, 0x18);

    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        call_gate_descriptor(0x08, 0, 0x84),
    );
    cpu.regs.set_sp(0x0020);
    cpu.regs.esp = 0x0020;
    cpu.regs.ss_cache.limit = 0x001D;
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.recompute_effective_limit();

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::CallGate {
            selector: 0x18,
            return_cs: 0x08,
            return_eip: 0x0104,
            ..
        } => {}
        other => panic!(
            "16-bit indirect CALL FAR to call gate should not be masked by return-frame #SS, got {:?}",
            other
        ),
    }
    assert_eq!(cpu.regs.sp(), 0x0020);
    assert_eq!(cpu.regs.esp, 0x0020);
}

#[test]
fn test_pm32_iretw_outer_stack_fault_restores_esp() {
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0xCF]);
    cpu.regs.esp = 0x1000;
    cpu.regs.set_sp(0x1000);
    cpu.regs.ss_cache.limit = 0x1007;
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.recompute_effective_limit();
    mem.write_u16(0x1000, 0x2000);
    mem.write_u16(0x1002, 0x001B);
    mem.write_u16(0x1004, 0x0202);
    mem.write_u16(0x1006, 0x3000);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 12,
            error_code: 0,
            ..
        } => {}
        other => panic!(
            "IRETW outer-ring SS pop beyond the stack limit should #SS(0), got {:?}",
            other
        ),
    }
    assert_eq!(cpu.regs.esp, 0x1000);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.cs, 0x08);
}

#[test]
fn test_pm32_iretd_outer_stack_fault_restores_esp() {
    let (mut cpu, mut mem) = setup_pm32(&[0xCF]);
    cpu.regs.esp = 0x1000;
    cpu.regs.set_sp(0x1000);
    cpu.regs.ss_cache.limit = 0x100F;
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.recompute_effective_limit();
    mem.write_u32(0x1000, 0x2000);
    mem.write_u32(0x1004, 0x001B);
    mem.write_u32(0x1008, 0x0202);
    mem.write_u32(0x100C, 0x3000);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 12,
            error_code: 0,
            ..
        } => {}
        other => panic!(
            "IRETD outer-ring SS pop beyond the stack limit should #SS(0), got {:?}",
            other
        ),
    }
    assert_eq!(cpu.regs.esp, 0x1000);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.cs, 0x08);
}

#[test]
fn test_pm16_call_far_call_gate_return_eip_is_next_instruction() {
    // 9A offset16 selector16 = CALL FAR selector:offset (5 bytes).
    let (mut cpu, mut mem) = setup_pm16(&[0x9A, 0x00, 0x00, 0x18, 0x00]);
    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        call_gate_descriptor(0x08, 0, 0x84),
    );

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::CallGate {
            selector: 0x18,
            return_cs,
            return_eip,
            ..
        } => {
            assert_eq!(return_cs, 0x08);
            assert_eq!(return_eip, 0x0105);
        }
        other => panic!(
            "16-bit CALL FAR to call gate should save the next IP, got {:?}",
            other
        ),
    }
}

#[test]
fn test_pm32_indirect_call_far_call_gate_return_eip_is_next_instruction() {
    // FF /3 m16:32 = CALL FAR [disp32] (6 bytes).
    let (mut cpu, mut mem) = setup_pm32(&[0xFF, 0x1D, 0x00, 0x20, 0x00, 0x00]);
    mem.write_u32(0x2000, 0);
    mem.write_u16(0x2004, 0x48);

    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x48,
        call_gate_descriptor(0x08, 0, 0x8C),
    );
    cpu.regs.gdtr.limit = 10 * 8 - 1;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::CallGate {
            selector: 0x48,
            return_cs,
            return_eip,
            ..
        } => {
            assert_eq!(return_cs, 0x08);
            assert_eq!(return_eip, 0x0106);
        }
        other => panic!(
            "32-bit indirect CALL FAR to call gate should save the next EIP, got {:?}",
            other
        ),
    }
}

#[test]
fn test_pm16_indirect_call_far_call_gate_return_eip_is_next_instruction() {
    // FF /3 m16:16 = CALL FAR [disp16] (4 bytes).
    let (mut cpu, mut mem) = setup_pm16(&[0xFF, 0x1E, 0x00, 0x20]);
    mem.write_u16(0x2000, 0);
    mem.write_u16(0x2002, 0x18);

    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        call_gate_descriptor(0x08, 0, 0x84),
    );

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::CallGate {
            selector: 0x18,
            return_cs,
            return_eip,
            ..
        } => {
            assert_eq!(return_cs, 0x08);
            assert_eq!(return_eip, 0x0104);
        }
        other => panic!(
            "16-bit indirect CALL FAR to call gate should save the next IP, got {:?}",
            other
        ),
    }
}

// 70. JMP FAR to not-present segment should #NP
#[test]
fn test_pm_jmp_far_not_present_np() {
    // EA 00 00 00 00 48 00 = JMP FAR 0x0048:0x00000000
    let (mut cpu, mut mem) = setup_pm32(&[0xEA, 0x00, 0x00, 0x00, 0x00, 0x48, 0x00]);
    // Not-present code segment at slot 9
    let gdt_base = cpu.regs.gdtr.base;
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x48,
        make_descriptor(0, 0xFFFFF, 0x1A, 0x0C), // code, not-present (bit 7=0)
    );
    cpu.regs.gdtr.limit = 10 * 8 - 1;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception { vector: 11, .. } => {}
        other => panic!("JMP FAR to not-present should #NP, got {:?}", other),
    }
}

// ========================================================================
// Additional: V86 mode string and arithmetic ops
// ========================================================================

// 71. V86 mode: REP MOVSB
#[test]
fn test_v86_rep_movsb() {
    // CLD; REP MOVSB = FC F3 A4
    let (mut cpu, mut mem) = setup_v86(&[0xFC, 0xF3, 0xA4], 3);
    cpu.regs.set_si(0x2000);
    cpu.regs.set_di(0x3000);
    cpu.regs.set_cx(8);
    for i in 0..8u8 {
        mem.ram[0x2000 + i as usize] = 0x50 + i;
    }
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    for i in 0..8u8 {
        assert_eq!(mem.ram[0x3000 + i as usize], 0x50 + i);
    }
    assert_eq!(cpu.regs.cx(), 0);
}

// 72. V86 mode: REP STOSB
#[test]
fn test_v86_rep_stosb() {
    // CLD; REP STOSB = FC F3 AA
    let (mut cpu, mut mem) = setup_v86(&[0xFC, 0xF3, 0xAA], 3);
    cpu.regs.set_al(0xFF);
    cpu.regs.set_di(0x4000);
    cpu.regs.set_cx(8);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    for i in 0..8 {
        assert_eq!(mem.ram[0x4000 + i], 0xFF);
    }
    assert_eq!(cpu.regs.cx(), 0);
}

// 73. V86 mode: LODSB
#[test]
fn test_v86_lodsb() {
    // CLD; LODSB = FC AC
    let (mut cpu, mut mem) = setup_v86(&[0xFC, 0xAC], 3);
    cpu.regs.set_si(0x2000);
    mem.ram[0x2000] = 0x42;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.al(), 0x42);
    assert_eq!(cpu.regs.si(), 0x2001);
}

// ========================================================================
// Additional: POPFD in PM (not V86), PUSHFD in PM
// ========================================================================

// 74. PUSHFD / POPFD roundtrip in PM32
#[test]
fn test_pm32_pushfd_popfd_roundtrip() {
    // PUSHFD = 9C, POPFD = 9D (32-bit mode)
    let (mut cpu, mut mem) = setup_pm32(&[0x9C, 0x9D]);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.flags.insert(Flags::ZERO);
    let sp_before = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.esp, sp_before);
}

// 75. POPFD in PM32 (non-V86) should succeed and not modify VM
#[test]
fn test_pm32_popfd_clears_rf_preserves_vm() {
    // POPFD clears RF and leaves VM unchanged, even if the stack image
    // has VM set.
    let (mut cpu, mut mem) = setup_pm32(&[0x9D]); // POPFD
    cpu.regs.eflags_high &= !0x0002;
    let sp = cpu.regs.esp;
    let flags_val: u32 = 0x0003_0202; // VM=1, RF=1, IF=1, RESERVED1
    mem.ram[sp as usize] = (flags_val & 0xFF) as u8;
    mem.ram[(sp as usize) + 1] = ((flags_val >> 8) & 0xFF) as u8;
    mem.ram[(sp as usize) + 2] = ((flags_val >> 16) & 0xFF) as u8;
    mem.ram[(sp as usize) + 3] = ((flags_val >> 24) & 0xFF) as u8;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // RF should be cleared by POPFD per Intel SDM
    assert_eq!(
        cpu.regs.eflags_high & 0x0001,
        0,
        "RF should be cleared by POPFD"
    );
    assert_eq!(
        cpu.regs.eflags_high & 0x0002,
        0,
        "VM should be unchanged by POPFD"
    );
}

/// POPFD of an all-ones image must not store EFLAGS bits 22-31: they are
/// reserved on every CPU we model and read as zero on real silicon, so a
/// later PUSHFD would otherwise hand the guest an impossible EFLAGS
/// The bits that DO exist must still land, and they
/// differ by generation, so check all three.
#[test]
fn test_pm32_popfd_never_stores_reserved_eflags_bits_kiln283() {
    // (CPU, expected AC|VIF|VIP|ID after popping all ones)
    const CASES: [(gp64_cpu::CpuType, u16); 3] = [
        (gp64_cpu::CpuType::I80386, 0x0000),
        (gp64_cpu::CpuType::I80486, 0x0004),
        (gp64_cpu::CpuType::Pentium, 0x003C),
    ];

    for (cpu_type, expected_optional) in CASES {
        let (mut cpu, mut mem) = setup_pm32(&[0x9D]); // POPFD
        cpu.set_cpu_type(cpu_type);
        cpu.regs.eflags_high = 0;
        let sp = cpu.regs.esp as usize;
        for byte in 0..4 {
            mem.ram[sp + byte] = 0xFF;
        }

        let mut io = ();
        let _ = run(&mut cpu, &mut mem, &mut io, 1);

        assert_eq!(
            cpu.regs.eflags_high & 0xFFC0,
            0,
            "{cpu_type:?}: EFLAGS 22-31 are reserved and must read back zero"
        );
        assert_eq!(
            cpu.regs.eflags_high & 0x003C,
            expected_optional,
            "{cpu_type:?}: AC/VIF/VIP/ID must follow the modeled generation"
        );
        assert_eq!(
            cpu.regs.eflags_high & 0x0001,
            0,
            "{cpu_type:?}: POPFD clears RF"
        );
        assert_eq!(
            cpu.regs.eflags_high & 0x0002,
            0,
            "{cpu_type:?}: POPFD leaves VM unchanged"
        );
    }
}

#[test]
fn test_pm32_paging_popfd_preserves_stack_width_for_following_pop() {
    // Canonical Cluster B regression: PUSH EAX; PUSH EDI; POPFD; POP EDI;
    // MOV EAX,0; HLT. POPFD must not set VM from the stack image, because
    // that would make the following POP use 16-bit SP instead of 32-bit ESP.
    let (mut cpu, mut mem) =
        setup_paging(&[0x50, 0x57, 0x9D, 0x5F, 0xB8, 0x00, 0x00, 0x00, 0x00, 0xF4]);
    cpu.regs.eax = 0x0003_0000;
    cpu.regs.edi = 0x0003_0200;
    cpu.regs.esp = 0x0002_0FF0;
    cpu.regs.set_sp(0x0FF0);
    cpu.regs.eflags_high = 0;

    let mut io = ();
    let boundary_result = run(&mut cpu, &mut mem, &mut io, 5);
    assert!(
        matches!(
            boundary_result,
            RunResult::InterruptPollBoundary { executed: 3 }
        ),
        "POPFD enabling IF must return at its interrupt recognition boundary, got {boundary_result:?}"
    );

    let result = run(&mut cpu, &mut mem, &mut io, 2);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "expected the remaining two canonical instructions to execute, got {result:?}"
    );
    assert_eq!(cpu.regs.edi, 0x0003_0000);
    assert_eq!(cpu.regs.esp, 0x0002_0FF0);
    assert_eq!(
        cpu.regs.eflags_high & 0x0002,
        0,
        "POPFD must not load VM from the stack image"
    );
}

// ========================================================================
// Additional: Rotate register paths in PM to cover more shift/rotate
// ========================================================================

// 76. ROL reg32 by CL in PM32
#[test]
fn test_pm32_rol_reg32_cl() {
    // D3 C0 = ROL EAX, CL
    let (mut cpu, mut mem) = setup_pm32(&[0xD3, 0xC0]);
    cpu.regs.eax = 0x80000001;
    cpu.regs.set_cl(1);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000003);
}

// 77. ROR reg32 by 1 in PM32
#[test]
fn test_pm32_ror_reg32_by1() {
    // D1 C8 = ROR EAX, 1
    let (mut cpu, mut mem) = setup_pm32(&[0xD1, 0xC8]);
    cpu.regs.eax = 0x00000001;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x80000000);
}

// 78. RCL reg32 by imm8 in PM32
#[test]
fn test_pm32_rcl_reg32_imm8() {
    // C1 D0 01 = RCL EAX, 1
    let (mut cpu, mut mem) = setup_pm32(&[0xC1, 0xD0, 0x01]);
    cpu.regs.eax = 0x80000000;
    cpu.regs.flags.remove(Flags::CARRY); // CF=0
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // RCL 0x80000000, 1 with CF=0: result=0x00000000, CF=1
    assert_eq!(cpu.regs.eax, 0x00000000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// 79. RCR reg32 by imm8 in PM32
#[test]
fn test_pm32_rcr_reg32_imm8() {
    // C1 D8 01 = RCR EAX, 1
    let (mut cpu, mut mem) = setup_pm32(&[0xC1, 0xD8, 0x01]);
    cpu.regs.eax = 0x00000001;
    cpu.regs.flags.insert(Flags::CARRY); // CF=1
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // RCR 0x00000001, 1 with CF=1: result=0x80000000, CF=1
    assert_eq!(cpu.regs.eax, 0x80000000);
}

// 80. SAR reg32 by CL in PM32
#[test]
fn test_pm32_sar_reg32_cl() {
    // D3 F8 = SAR EAX, CL
    let (mut cpu, mut mem) = setup_pm32(&[0xD3, 0xF8]);
    cpu.regs.eax = 0x80000000;
    cpu.regs.set_cl(4);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // SAR preserves sign: 0x80000000 >> 4 = 0xF8000000
    assert_eq!(cpu.regs.eax, 0xF8000000);
}

// ========================================================================
// Additional: LODS/STOS/SCAS with paging
// ========================================================================

// 81. LODSD with paging
#[test]
fn test_paging_lodsd() {
    // LODSD = AD (in 32-bit mode, reads dword from [DS:ESI])
    let (mut cpu, mut mem) = setup_paging(&[0xAD]);
    cpu.regs.esi = 0x5000;
    mem.ram[0x5000] = 0xDD;
    mem.ram[0x5001] = 0xCC;
    mem.ram[0x5002] = 0xBB;
    mem.ram[0x5003] = 0xAA;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xAABBCCDD);
    assert_eq!(cpu.regs.esi, 0x5004);
}

// 82. STOSD with paging
#[test]
fn test_paging_stosd() {
    // STOSD = AB (in 32-bit mode, writes dword to [ES:EDI])
    let (mut cpu, mut mem) = setup_paging(&[0xAB]);
    cpu.regs.edi = 0x5000;
    cpu.regs.eax = 0xDEADBEEF;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes([
        mem.ram[0x5000],
        mem.ram[0x5001],
        mem.ram[0x5002],
        mem.ram[0x5003],
    ]);
    assert_eq!(val, 0xDEADBEEF);
    assert_eq!(cpu.regs.edi, 0x5004);
}

// 83. SCASD with paging (match)
#[test]
fn test_paging_scasd_match() {
    // SCASD = AF (in 32-bit mode, compare EAX with [ES:EDI])
    let (mut cpu, mut mem) = setup_paging(&[0xAF]);
    cpu.regs.edi = 0x5000;
    cpu.regs.eax = 0x12345678;
    mem.ram[0x5000] = 0x78;
    mem.ram[0x5001] = 0x56;
    mem.ram[0x5002] = 0x34;
    mem.ram[0x5003] = 0x12;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.edi, 0x5004);
}

// 84. REP STOSD with paging
#[test]
fn test_paging_rep_stosd() {
    // REP STOSD = F3 AB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAB]);
    cpu.regs.edi = 0x5000;
    cpu.regs.eax = 0xAAAAAAAA;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 64);
    for i in 0..4 {
        let off = 0x5000 + i * 4;
        let val = u32::from_le_bytes([
            mem.ram[off],
            mem.ram[off + 1],
            mem.ram[off + 2],
            mem.ram[off + 3],
        ]);
        assert_eq!(val, 0xAAAAAAAA);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// Additional: ENTER/LEAVE in PM, LFS/LGS in PM
// ========================================================================

fn configure_test_stack_segment(cpu: &mut Cpu, limit: u32, big: bool, expand_down: bool) {
    cpu.regs.ss_cache.limit = limit;
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.access = if expand_down { 0x97 } else { 0x93 };
    cpu.regs.ss_cache.present = true;
    cpu.regs.ss_cache.is_code = false;
    cpu.regs.ss_cache.is_writable = true;
    cpu.regs.ss_cache.big = big;
}

fn configure_real_mode_stack(cpu: &mut Cpu, selector: u16) -> usize {
    cpu.regs.ss = selector;
    cpu.regs.ss_cache = SegmentCache::real_mode(selector);
    usize::from(selector) << 4
}

#[test]
fn real_mode_pushad_late_stack_fault_preserves_completed_writes() {
    // 386EX corpus 6660 #302: the fourth dword crosses FFFF after three
    // successful PUSHAD writes. The writes survive, while SP rolls back.
    let (mut cpu, mut mem) = setup(&[0x66, 0x60]);
    let stack_base = configure_real_mode_stack(&mut cpu, 0x2000);
    cpu.regs.esp = 0xA5A5_000E;
    cpu.regs.eax = 0x1111_2222;
    cpu.regs.ecx = 0x3333_4444;
    cpu.regs.edx = 0x5555_6666;
    cpu.regs.ebx = 0x7777_8888;
    cpu.regs.ebp = 0x9999_AAAA;
    cpu.regs.esi = 0xBBBB_CCCC;
    cpu.regs.edi = 0xDDDD_EEEE;

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 12,
            error_code: 0,
            ..
        }
    ));
    assert_eq!(cpu.regs.esp, 0xA5A5_000E);
    assert_eq!(mem.read_u32(stack_base + 0xFFEE), 0xDDDD_EEEE);
    assert_eq!(mem.read_u32(stack_base + 0xFFF2), 0xBBBB_CCCC);
    assert_eq!(mem.read_u32(stack_base + 0xFFF6), 0x9999_AAAA);
    assert_eq!(mem.read_u32(stack_base + 0xFFFA), 0xA5A5_000E);
    assert_eq!(mem.read_u16(stack_base + 0xFFFE), 0);
}

#[test]
fn real_mode_popa_late_stack_fault_preserves_completed_register_loads() {
    // 386EX corpus 61 #681 faults on the skipped SP word after DI, SI,
    // and BP have loaded. Those destination loads are architecturally
    // visible even though the entry SP is restored.
    let (mut cpu, mut mem) = setup(&[0x61]);
    let stack_base = configure_real_mode_stack(&mut cpu, 0x2100);
    cpu.regs.esp = 0x4444_FFF9;
    cpu.regs.edi = 0x1111_0000;
    cpu.regs.esi = 0x2222_0000;
    cpu.regs.ebp = 0x3333_0000;
    mem.write_u16(stack_base + 0xFFF9, 0xA111);
    mem.write_u16(stack_base + 0xFFFB, 0xB222);
    mem.write_u16(stack_base + 0xFFFD, 0xC333);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::Exception { vector: 12, .. }));
    assert_eq!(cpu.regs.esp, 0x4444_FFF9);
    assert_eq!(cpu.regs.edi, 0x1111_A111);
    assert_eq!(cpu.regs.esi, 0x2222_B222);
    assert_eq!(cpu.regs.ebp, 0x3333_C333);
}

#[test]
fn real_mode_popad_uses_skipped_dword_high_half_for_esp() {
    // A late fault after the skipped slot preserves completed loads and
    // the skipped dword's ESP high half, but restores the entry SP low.
    let (mut cpu, mut mem) = setup(&[0x66, 0x61]);
    let stack_base = configure_real_mode_stack(&mut cpu, 0x2200);
    cpu.regs.esp = 0x1111_FFEE;
    cpu.regs.ebx = 0xDEAD_BEEF;
    mem.write_u32(stack_base + 0xFFEE, 0x1111_0001);
    mem.write_u32(stack_base + 0xFFF2, 0x2222_0002);
    mem.write_u32(stack_base + 0xFFF6, 0x3333_0003);
    mem.write_u32(stack_base + 0xFFFA, 0xCAFE_0004);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::Exception { vector: 12, .. }));
    assert_eq!(cpu.regs.esp, 0xCAFE_FFEE);
    assert_eq!(cpu.regs.edi, 0x1111_0001);
    assert_eq!(cpu.regs.esi, 0x2222_0002);
    assert_eq!(cpu.regs.ebp, 0x3333_0003);
    assert_eq!(cpu.regs.ebx, 0xDEAD_BEEF);

    // On success the same high half remains, while the low 16-bit stack
    // pointer advances by the complete 32-byte POPAD frame.
    let (mut cpu, mut mem) = setup(&[0x66, 0x61]);
    let stack_base = configure_real_mode_stack(&mut cpu, 0x2200);
    cpu.regs.esp = 0x1111_0100;
    for (index, value) in [
        0x0101_0101,
        0x0202_0202,
        0x0303_0303,
        0xBEEF_0104,
        0x0505_0505,
        0x0606_0606,
        0x0707_0707,
        0x0808_0808,
    ]
    .into_iter()
    .enumerate()
    {
        mem.write_u32(stack_base + 0x0100 + index * 4, value);
    }

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.esp, 0xBEEF_0120);
}

#[test]
fn real_mode_enter_late_stack_fault_preserves_initial_push() {
    // 386EX 66C8 late-fault cases push the old EBP before a later copied
    // frame dword crosses FFFF. The write survives; SP and EBP roll back.
    let (mut cpu, mut mem) = setup(&[0x66, 0xC8, 0x00, 0x00, 0x02]);
    let stack_base = configure_real_mode_stack(&mut cpu, 0x2300);
    cpu.regs.esp = 0xAAAA_0006;
    cpu.regs.ebp = 0x1234_1004;
    mem.write_u32(stack_base + 0x1000, 0xCAFE_BABE);

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(matches!(result, RunResult::Exception { vector: 12, .. }));
    assert_eq!(cpu.regs.esp, 0xAAAA_0006);
    assert_eq!(cpu.regs.ebp, 0x1234_1004);
    assert_eq!(mem.read_u32(stack_base + 0x0002), 0x1234_1004);
    assert_eq!(mem.read_u16(stack_base + 0xFFFE), 0);
}

fn assert_enter_stack_fault_is_atomic(
    mut cpu: Cpu,
    mut mem: Memory,
    guarded_range: std::ops::Range<usize>,
    context: &str,
) {
    let source_eip = cpu.regs.eip;
    let source_esp = cpu.regs.esp;
    let source_ebp = cpu.regs.ebp;
    let guarded_bytes = mem.ram[guarded_range.clone()].to_vec();
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 12,
                error_code: 0,
                ..
            }
        ),
        "{context}: expected #SS(0), got {result:?}"
    );
    assert_eq!(cpu.regs.eip, source_eip, "{context}: EIP changed");
    assert_eq!(cpu.regs.esp, source_esp, "{context}: ESP changed");
    assert_eq!(cpu.regs.ebp, source_ebp, "{context}: EBP changed");
    assert_eq!(
        &mem.ram[guarded_range],
        guarded_bytes.as_slice(),
        "{context}: stack memory changed before #SS"
    );
}

#[test]
fn test_pm16_enter_first_push_checks_16bit_operand_and_stack() {
    let (mut cpu, mut mem) = setup_pm16(&[0xC8, 0x00, 0x00, 0x00]);
    cpu.regs.esp = 0x0100;
    cpu.regs.set_sp(0x0100);
    cpu.regs.ebp = 0x1234;
    configure_test_stack_segment(&mut cpu, 0x00FE, false, false);
    mem.ram[0x00FC..0x0100].fill(0xA5);

    assert_enter_stack_fault_is_atomic(cpu, mem, 0x00FC..0x0100, "16-bit ENTER on a 16-bit stack");
}

#[test]
fn test_real_mode_enter_uses_cached_stack_limit() {
    let (mut cpu, mut mem) = setup(&[0xC8, 0x00, 0x00, 0x00]);
    cpu.regs.esp = 0x0200;
    cpu.regs.ebp = 0x1234;
    configure_test_stack_segment(&mut cpu, 0x01FE, false, false);
    mem.ram[0x01FC..0x0200].fill(0xA5);

    assert_enter_stack_fault_is_atomic(
        cpu,
        mem,
        0x01FC..0x0200,
        "real-mode ENTER with a cached SS limit",
    );
}

#[test]
fn test_pm16_enter_o32_first_push_checks_32bit_operand_on_16bit_stack() {
    let (mut cpu, mut mem) = setup_pm16(&[0x66, 0xC8, 0x00, 0x00, 0x00]);
    cpu.regs.esp = 0x0100;
    cpu.regs.set_sp(0x0100);
    cpu.regs.ebp = 0x1234_5678;
    configure_test_stack_segment(&mut cpu, 0x00FE, false, false);
    mem.ram[0x00F8..0x0100].fill(0xA5);

    assert_enter_stack_fault_is_atomic(cpu, mem, 0x00F8..0x0100, "32-bit ENTER on a 16-bit stack");
}

#[test]
fn test_pm32_enter_first_push_checks_32bit_operand_and_stack() {
    let (mut cpu, mut mem) = setup_pm32(&[0xC8, 0x00, 0x00, 0x00]);
    cpu.regs.esp = 0x0100;
    cpu.regs.ebp = 0x1234_5678;
    configure_test_stack_segment(&mut cpu, 0x00FE, true, false);
    mem.ram[0x00F8..0x0100].fill(0xA5);

    assert_enter_stack_fault_is_atomic(cpu, mem, 0x00F8..0x0100, "32-bit ENTER on a 32-bit stack");
}

#[test]
fn test_pm32_enter_o16_first_push_checks_16bit_operand_on_32bit_stack() {
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0xC8, 0x00, 0x00, 0x00]);
    cpu.regs.esp = 0x0001_0100;
    cpu.regs.ebp = 0x1234_5678;
    configure_test_stack_segment(&mut cpu, 0x0001_00FE, true, false);
    mem.ram[0x0001_00FC..0x0001_0100].fill(0xA5);

    assert_enter_stack_fault_is_atomic(
        cpu,
        mem,
        0x0001_00FC..0x0001_0100,
        "16-bit ENTER on a 32-bit stack",
    );
}

#[test]
fn test_pm32_enter_o16_allocation_uses_32bit_stack_pointer() {
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0xC8, 0x02, 0x00, 0x00]);
    cpu.regs.esp = 0x0001_0003;
    cpu.regs.ebp = 0x1234_5678;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(matches!(result, RunResult::BudgetExhausted { .. }));
    assert_eq!(cpu.regs.esp, 0x0000_FFFF);
    assert_eq!(cpu.regs.bp(), 0x0001);
}

#[test]
fn test_pm32_enter_expand_up_nesting_read_faults_before_initial_push() {
    let (mut cpu, mut mem) = setup_pm32(&[0xC8, 0x00, 0x00, 0x02]);
    cpu.regs.esp = 0x0800;
    cpu.regs.ebp = 0x0804;
    configure_test_stack_segment(&mut cpu, 0x0800, true, false);
    mem.ram[0x07F8..0x0808].fill(0xA5);

    assert_enter_stack_fault_is_atomic(cpu, mem, 0x07F8..0x0808, "expand-up ENTER nesting read");
}

#[test]
fn test_pm32_enter_level1_frame_push_checks_stack_limit() {
    let (mut cpu, mut mem) = setup_pm32(&[0xC8, 0x00, 0x00, 0x01]);
    cpu.regs.esp = 0x0208;
    cpu.regs.ebp = 0x1234_5678;
    configure_test_stack_segment(&mut cpu, 0x0200, true, true);
    mem.ram[0x01FC..0x0208].fill(0xA5);

    assert_enter_stack_fault_is_atomic(cpu, mem, 0x01FC..0x0208, "level-1 ENTER frame push");
}

#[test]
fn test_pm32_enter_expand_down_copied_push_faults_atomically() {
    let (mut cpu, mut mem) = setup_pm32(&[0xC8, 0x00, 0x00, 0x02]);
    cpu.regs.esp = 0x0208;
    cpu.regs.ebp = 0x0304;
    configure_test_stack_segment(&mut cpu, 0x0200, true, true);
    mem.ram[0x01FC..0x0208].fill(0xA5);
    mem.write_u32(0x0300, 0xDEAD_BEEF);

    assert_enter_stack_fault_is_atomic(
        cpu,
        mem,
        0x01FC..0x0208,
        "expand-down ENTER copied-frame push",
    );
}

#[test]
fn test_pm32_enter_final_allocation_checks_resulting_stack_pointer() {
    let (mut cpu, mut mem) = setup_pm32(&[0xC8, 0x04, 0x00, 0x00]);
    cpu.regs.esp = 0x0208;
    cpu.regs.ebp = 0x1234_5678;
    configure_test_stack_segment(&mut cpu, 0x0200, true, true);
    mem.ram[0x0200..0x0208].fill(0xA5);

    assert_enter_stack_fault_is_atomic(cpu, mem, 0x0200..0x0208, "ENTER final local allocation");
}

#[test]
fn test_pm32_enter_stack_fault_precedes_page_fault() {
    let (mut cpu, mut mem) = setup_paging(&[0xC8, 0x00, 0x00, 0x00]);
    cpu.regs.esp = 0x0020_0000;
    cpu.regs.ebp = 0x1234_5678;
    configure_test_stack_segment(&mut cpu, 0x1000, true, false);
    mem.ram[0x001F_FFFC..0x0020_0000].fill(0xA5);

    assert_enter_stack_fault_is_atomic(
        cpu,
        mem,
        0x001F_FFFC..0x0020_0000,
        "ENTER segmentation-before-paging ordering",
    );
}

// 85. ENTER imm16, 0 in PM32
#[test]
fn test_pm32_enter_leave() {
    // ENTER 0x10, 0 = C8 10 00 00; LEAVE = C9
    let (mut cpu, mut mem) = setup_pm32(&[0xC8, 0x10, 0x00, 0x00, 0xC9]);
    let sp_before = cpu.regs.esp;
    let bp_before = cpu.regs.ebp;
    let mut io = ();
    // Run ENTER
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // After ENTER: EBP = old ESP-4, ESP = EBP - 0x10
    let new_bp = sp_before - 4;
    assert_eq!(cpu.regs.ebp, new_bp);
    assert_eq!(cpu.regs.esp, new_bp - 0x10);
    // Run LEAVE
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ebp, bp_before);
    assert_eq!(cpu.regs.esp, sp_before);
}

// 86. XCHG reg, reg in PM32
#[test]
fn test_pm32_xchg_eax_ecx() {
    // 91 = XCHG EAX, ECX
    let (mut cpu, mut mem) = setup_pm32(&[0x91]);
    cpu.regs.eax = 0x11111111;
    cpu.regs.ecx = 0x22222222;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x22222222);
    assert_eq!(cpu.regs.ecx, 0x11111111);
}

// 87. MOVSX r32, r/m8 in PM32 with paging
#[test]
fn test_paging_movsx_r32_rm8() {
    // 0F BE 05 00 50 00 00 = MOVSX EAX, byte [0x5000]
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBE, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x80; // -128
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFFFF80); // sign-extended
}

// 88. MOVZX r32, r/m8 in PM32 with paging
#[test]
fn test_paging_movzx_r32_rm8() {
    // 0F B6 05 00 50 00 00 = MOVZX EAX, byte [0x5000]
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xB6, 0x05, 0x00, 0x50, 0x00, 0x00]);
    mem.ram[0x5000] = 0x80;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00000080); // zero-extended
}

// 89. BT [mem], imm8 with paging
#[test]
fn test_paging_bt_mem_imm8() {
    // 0F BA /4 with ModR/M for [0x5000]: 0F BA 25 00 50 00 00 07
    // BT dword [0x5000], 7
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBA, 0x25, 0x00, 0x50, 0x00, 0x00, 0x07]);
    mem.ram[0x5000] = 0x80; // bit 7 is set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // bit 7 was set
}

// 90. BTS [mem], imm8 with paging
#[test]
fn test_paging_bts_mem_imm8() {
    // 0F BA /5 with ModR/M for [0x5000]: 0F BA 2D 00 50 00 00 03
    // BTS dword [0x5000], 3
    let (mut cpu, mut mem) = setup_paging(&[0x0F, 0xBA, 0x2D, 0x00, 0x50, 0x00, 0x00, 0x03]);
    mem.ram[0x5000] = 0x00; // bit 3 not set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was 0 before
    assert_eq!(mem.ram[0x5000] & 0x08, 0x08); // bit 3 now set
}
