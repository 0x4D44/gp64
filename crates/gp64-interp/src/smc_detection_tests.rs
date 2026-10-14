use super::*;
use gp64_memory::Memory;
use gp64_types::SmcNotify;
use std::sync::{Arc, Mutex};

/// Test SMC notifier that records all write notifications.
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

/// Helper: set up CPU+Memory with code at 0000:0100, with SMC notifier bound.
fn setup_with_smc(code: &[u8]) -> (Cpu, Memory, Arc<TestSmcNotify>) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let notifier = Arc::new(TestSmcNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);
    // `Memory::smc_before_write` only notifies for a page the dyncore holds
    // code on (its `CodePageHandler` peer). These tests are about WHICH
    // interpreter write paths notify and HOW MANY times, not about the
    // page policy, so declare the whole first megabyte — every address they
    // address, ROM window included — as compiled-code pages. Keeping
    // 0xF0000..0x100000 watched matters: it makes
    // `test_smc_rom_write_no_notification` still prove the ROM DROP rather
    // than pass because the page gate happened to close.
    for page in 0..0x100u32 {
        mem.set_page_watched(page, true);
    }
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    // Set up a valid stack
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem, notifier)
}

fn setup_with_gdt_snoop(code: &[u8], gdt_base: u32, gdt_limit: u16) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    mem.set_gdt_snoop_range(gdt_base, gdt_limit);
    (cpu, mem)
}

// ========================================================================
// MOV [mem], reg — non-VGA writes trigger SMC notification
// ========================================================================

#[test]
fn test_smc_mov_byte_to_memory() {
    // MOV [0x5000], AL  — opcode A2, addr 0x5000
    let code: &[u8] = &[0xA2, 0x00, 0x50];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    cpu.regs.set_al(0x42);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.ram[0x5000], 0x42);

    let writes = notifier.snapshot();
    // Should have at least one notification for addr 0x5000, size 1
    assert!(
        writes.iter().any(|&(addr, sz)| addr == 0x5000 && sz == 1),
        "Expected SMC notification at 0x5000 size 1, got: {:?}",
        writes
    );
}

#[test]
fn test_smc_mov_word_to_memory() {
    // MOV [0x5000], AX — opcode A3, addr 0x5000
    let code: &[u8] = &[0xA3, 0x00, 0x50];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    cpu.regs.set_ax(0x1234);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.read_u16(0x5000), 0x1234);

    let writes = notifier.snapshot();
    assert!(
        writes.iter().any(|&(addr, sz)| addr == 0x5000 && sz == 2),
        "Expected SMC notification at 0x5000 size 2, got: {:?}",
        writes
    );
}

#[test]
fn test_smc_mov_dword_to_memory() {
    // 66 A3 00 50 — MOV [0x5000], EAX (32-bit operand prefix)
    let code: &[u8] = &[0x66, 0xA3, 0x00, 0x50];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.read_u32(0x5000), 0xDEADBEEF);

    let writes = notifier.snapshot();
    assert!(
        writes.iter().any(|&(addr, sz)| addr == 0x5000 && sz == 4),
        "Expected SMC notification at 0x5000 size 4, got: {:?}",
        writes
    );
}

// ========================================================================
// ROM writes do NOT trigger SMC notification (silently ignored)
// ========================================================================

#[test]
fn test_smc_rom_write_no_notification() {
    // MOV [0xF100], AL — write to ROM area, should be silently ignored
    // Use DS override: DS=0xF000, offset=0x0100
    // A2 00 01 — MOV [0x0100], AL  with DS=0xF000 → addr 0xF0100
    let code: &[u8] = &[0xA2, 0x00, 0x01];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    cpu.regs.ds = 0xF000;
    cpu.regs.set_al(0x55);
    let mut io = ();

    // Save original ROM value
    let orig = mem.ram[0xF0100];

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));

    // ROM write should have been silently ignored
    assert_eq!(mem.ram[0xF0100], orig, "ROM should not have been modified");

    let writes = notifier.snapshot();
    // Should NOT have any notification for ROM range
    assert!(
        !writes
            .iter()
            .any(|&(addr, _)| addr >= 0xF0000 && addr < 0x100000),
        "Should not have SMC notification for ROM writes, got: {:?}",
        writes
    );
}

// ========================================================================
// VGA writes go through Memory::write_u8 (which already has SMC internally)
// — no duplicate notification from our new code
// ========================================================================

#[test]
fn test_smc_vga_write_no_duplicate() {
    // Write to VGA range (0xA0000): MOV [0x0000], AL with DS=0xA000
    let code: &[u8] = &[0xA2, 0x00, 0x00];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    cpu.regs.ds = 0xA000;
    cpu.regs.set_al(0x77);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));

    let writes = notifier.snapshot();
    // VGA writes go through Memory::write_u8 which calls smc_before_write
    // once internally. Our vga_aware_write_u8 takes the VGA branch and does
    // NOT call smc_before_write itself, so there should be exactly 1
    // notification (from Memory::write_u8).
    let vga_writes: Vec<_> = writes
        .iter()
        .filter(|&&(addr, _)| addr >= 0xA0000 && addr < 0xC0000)
        .collect();
    assert_eq!(
        vga_writes.len(),
        1,
        "Expected exactly 1 SMC notification for VGA write (from Memory::write_u8), got: {:?}",
        vga_writes
    );
}

// ========================================================================
// No SMC notifier bound — writes should not panic
// ========================================================================

#[test]
fn test_smc_no_notifier_no_panic() {
    // MOV [0x5000], AL with no SMC notifier
    let code: &[u8] = &[0xA2, 0x00, 0x50];
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    cpu.regs.set_al(0x42);
    let mut io = ();

    // Should not panic — smc_before_write returns early when notifier is None
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.ram[0x5000], 0x42);
}

// ========================================================================
// REP MOVSB bulk fast path triggers SMC for destination range
// ========================================================================

#[test]
fn test_smc_rep_movsb_bulk_path() {
    // REP MOVSB: F3 A4
    // Set up source data at DS:SI (0x2000), dest at ES:DI (0x4000), CX=16
    let code: &[u8] = &[0xF3, 0xA4];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    cpu.regs.ds = 0x0000;
    cpu.regs.set_si(0x2000);
    cpu.regs.esi = 0x2000;
    cpu.regs.es = 0x0000;
    cpu.regs.set_di(0x4000);
    cpu.regs.edi = 0x4000;
    cpu.regs.set_cx(16);
    cpu.regs.ecx = 16;
    // Clear direction flag (forward)
    cpu.regs.flags.remove(Flags::DIRECTION);

    // Write source data
    for i in 0..16u8 {
        mem.ram[0x2000 + i as usize] = 0xA0 + i;
    }
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 16);
    assert!(matches!(
        result,
        RunResult::BudgetExhausted { executed: 16 }
    ));

    // Verify copy happened
    for i in 0..16u8 {
        assert_eq!(mem.ram[0x4000 + i as usize], 0xA0 + i);
    }

    let writes = notifier.snapshot();
    // Bulk path should produce a single SMC notification covering the
    // destination range (addr=0x4000, size=16)
    assert!(
        writes.iter().any(|&(addr, sz)| addr == 0x4000 && sz == 16),
        "Expected SMC notification at 0x4000 size 16 for REP MOVSB bulk path, got: {:?}",
        writes
    );
}

// ========================================================================
// REP STOSB bulk fast path triggers SMC for destination range
// ========================================================================

#[test]
fn test_smc_rep_stosb_bulk_path() {
    // REP STOSB: F3 AA
    // Fill 32 bytes at ES:DI (0x6000) with AL=0x55
    let code: &[u8] = &[0xF3, 0xAA];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    cpu.regs.es = 0x0000;
    cpu.regs.set_di(0x6000);
    cpu.regs.edi = 0x6000;
    cpu.regs.set_cx(32);
    cpu.regs.ecx = 32;
    cpu.regs.set_al(0x55);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 32);
    assert!(matches!(
        result,
        RunResult::BudgetExhausted { executed: 32 }
    ));

    // Verify fill happened
    for i in 0..32 {
        assert_eq!(mem.ram[0x6000 + i], 0x55);
    }

    let writes = notifier.snapshot();
    assert!(
        writes.iter().any(|&(addr, sz)| addr == 0x6000 && sz == 32),
        "Expected SMC notification at 0x6000 size 32 for REP STOSB bulk path, got: {:?}",
        writes
    );
}

// ========================================================================
// REP MOVSW bulk fast path triggers SMC for destination range
// ========================================================================

#[test]
fn test_smc_rep_movsw_bulk_path() {
    // REP MOVSW: F3 A5
    // Copy 8 words (16 bytes) from DS:SI (0x2000) to ES:DI (0x4000)
    let code: &[u8] = &[0xF3, 0xA5];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    cpu.regs.ds = 0x0000;
    cpu.regs.set_si(0x2000);
    cpu.regs.esi = 0x2000;
    cpu.regs.es = 0x0000;
    cpu.regs.set_di(0x4000);
    cpu.regs.edi = 0x4000;
    cpu.regs.set_cx(8);
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);

    // Write source data (8 words = 16 bytes)
    for i in 0..16u8 {
        mem.ram[0x2000 + i as usize] = 0xB0 + i;
    }
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 8);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 8 }));

    // Verify copy happened
    for i in 0..16u8 {
        assert_eq!(mem.ram[0x4000 + i as usize], 0xB0 + i);
    }

    let writes = notifier.snapshot();
    // 8 words * 2 bytes = 16 bytes total
    assert!(
        writes.iter().any(|&(addr, sz)| addr == 0x4000 && sz == 16),
        "Expected SMC notification at 0x4000 size 16 for REP MOVSW bulk path, got: {:?}",
        writes
    );
}

// ========================================================================
// REP STOSW bulk fast path triggers SMC for destination range
// ========================================================================

#[test]
fn test_smc_rep_stosw_bulk_path() {
    // REP STOSW: F3 AB
    // Fill 10 words (20 bytes) at ES:DI (0x6000) with AX=0x1234
    let code: &[u8] = &[0xF3, 0xAB];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    cpu.regs.es = 0x0000;
    cpu.regs.set_di(0x6000);
    cpu.regs.edi = 0x6000;
    cpu.regs.set_cx(10);
    cpu.regs.ecx = 10;
    cpu.regs.set_ax(0x1234);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(matches!(
        result,
        RunResult::BudgetExhausted { executed: 10 }
    ));

    // Verify fill happened
    for i in 0..10 {
        assert_eq!(mem.read_u16(0x6000 + i * 2), 0x1234);
    }

    let writes = notifier.snapshot();
    // 10 words * 2 bytes = 20 bytes total
    assert!(
        writes.iter().any(|&(addr, sz)| addr == 0x6000 && sz == 20),
        "Expected SMC notification at 0x6000 size 20 for REP STOSW bulk path, got: {:?}",
        writes
    );
}

#[test]
fn test_gdt_snoop_rep_movsb_bulk_path() {
    let code: &[u8] = &[0xF3, 0xA4];
    let (mut cpu, mut mem) = setup_with_gdt_snoop(code, 0x4000, 0x000F);
    cpu.regs.ds = 0x0000;
    cpu.regs.set_si(0x2000);
    cpu.regs.esi = 0x2000;
    cpu.regs.es = 0x0000;
    cpu.regs.set_di(0x4000);
    cpu.regs.edi = 0x4000;
    cpu.regs.set_cx(16);
    cpu.regs.ecx = 16;
    cpu.regs.flags.remove(Flags::DIRECTION);
    for i in 0..16u8 {
        mem.ram[0x2000 + i as usize] = 0x80 + i;
    }
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 16);
    assert!(matches!(
        result,
        RunResult::BudgetExhausted { executed: 16 }
    ));
    assert!(
        mem.take_gdt_dirty(),
        "REP MOVSB bulk path should dirty the GDT snoop range"
    );
}

#[test]
fn test_gdt_snoop_rep_movsw_bulk_path() {
    let code: &[u8] = &[0xF3, 0xA5];
    let (mut cpu, mut mem) = setup_with_gdt_snoop(code, 0x4000, 0x000F);
    cpu.regs.ds = 0x0000;
    cpu.regs.set_si(0x2000);
    cpu.regs.esi = 0x2000;
    cpu.regs.es = 0x0000;
    cpu.regs.set_di(0x4000);
    cpu.regs.edi = 0x4000;
    cpu.regs.set_cx(8);
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    for i in 0..16u8 {
        mem.ram[0x2000 + i as usize] = 0x90 + i;
    }
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 8);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 8 }));
    assert!(
        mem.take_gdt_dirty(),
        "REP MOVSW bulk path should dirty the GDT snoop range"
    );
}

#[test]
fn test_gdt_snoop_rep_stosb_bulk_path() {
    let code: &[u8] = &[0xF3, 0xAA];
    let (mut cpu, mut mem) = setup_with_gdt_snoop(code, 0x6000, 0x001F);
    cpu.regs.es = 0x0000;
    cpu.regs.set_di(0x6000);
    cpu.regs.edi = 0x6000;
    cpu.regs.set_cx(32);
    cpu.regs.ecx = 32;
    cpu.regs.set_al(0x5A);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 32);
    assert!(matches!(
        result,
        RunResult::BudgetExhausted { executed: 32 }
    ));
    assert!(
        mem.take_gdt_dirty(),
        "REP STOSB bulk path should dirty the GDT snoop range"
    );
}

#[test]
fn test_gdt_snoop_rep_stosw_bulk_path() {
    let code: &[u8] = &[0xF3, 0xAB];
    let (mut cpu, mut mem) = setup_with_gdt_snoop(code, 0x6000, 0x0013);
    cpu.regs.es = 0x0000;
    cpu.regs.set_di(0x6000);
    cpu.regs.edi = 0x6000;
    cpu.regs.set_cx(10);
    cpu.regs.ecx = 10;
    cpu.regs.set_ax(0x1234);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(matches!(
        result,
        RunResult::BudgetExhausted { executed: 10 }
    ));
    assert!(
        mem.take_gdt_dirty(),
        "REP STOSW bulk path should dirty the GDT snoop range"
    );
}

#[test]
fn test_ldt_only_snoop_mov_word_hot_path() {
    // MOV [0x6010], AX. Pre-fix the run() entry guard only checked the
    // GDT snoop range, so an LDT-only range skipped descriptor snooping.
    let code: &[u8] = &[0xA3, 0x10, 0x60];
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.ss = 0x0000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;
    cpu.regs.set_ax(0x1234);
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    mem.set_ldt_snoop_range(0x6000, 0x001F);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.read_u16(0x6010), 0x1234);
    assert!(mem.take_ldt_dirty());
    assert!(!mem.take_gdt_dirty());
}

#[test]
fn test_gdt_write_does_not_refresh_loaded_ds_cache_without_reload() {
    fn descriptor_bytes(base: u32, limit: u32, access: u8, flags: u8) -> [u8; 8] {
        [
            (limit & 0xFF) as u8,
            ((limit >> 8) & 0xFF) as u8,
            (base & 0xFF) as u8,
            ((base >> 8) & 0xFF) as u8,
            ((base >> 16) & 0xFF) as u8,
            access,
            (((limit >> 16) & 0x0F) as u8) | ((flags & 0x0F) << 4),
            ((base >> 24) & 0xFF) as u8,
        ]
    }

    fn write_descriptor(mem: &mut Memory, addr: usize, desc: [u8; 8], snoop: bool) {
        for (i, byte) in desc.into_iter().enumerate() {
            mem.write_u8(addr + i, byte);
            if snoop {
                mem.snoop_gdt_write_ext(addr + i, byte as u64, 1);
            }
        }
    }

    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let gdt_base = 0x2000usize;

    cpu.regs.cr0 |= 1;
    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = 0x001F;
    mem.set_gdt_snoop_range(gdt_base as u32, 0x001F);

    write_descriptor(
        &mut mem,
        gdt_base + 0x08,
        descriptor_bytes(0, 0xFFFF, 0x9B, 0x0),
        false,
    );
    write_descriptor(
        &mut mem,
        gdt_base + 0x10,
        descriptor_bytes(0x3000, 0xFFFF, 0x93, 0x0),
        false,
    );
    write_descriptor(
        &mut mem,
        gdt_base + 0x18,
        descriptor_bytes(0, 0xFFFF, 0x93, 0x0),
        false,
    );

    cpu.load_cs(&mut mem, 0x0008).unwrap();
    cpu.load_data_segment(&mem, DataSegReg::DS, 0x0010);
    cpu.load_ss(&mut mem, 0x0018).unwrap();
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.set_sp(0x8000);
    cpu.regs.esp = 0x8000;

    // MOV AL, moffs8 [0000]
    mem.ram[0x0100] = 0xA0;
    mem.ram[0x0101] = 0x00;
    mem.ram[0x0102] = 0x00;
    mem.ram[0x3000] = 0x11;
    mem.ram[0x4000] = 0x22;

    // Change the descriptor in memory only. Real x86 keeps using the old
    // hidden DS cache until the guest explicitly reloads DS.
    write_descriptor(
        &mut mem,
        gdt_base + 0x10,
        descriptor_bytes(0x4000, 0xFFFF, 0x93, 0x0),
        true,
    );

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax & 0xFF, 0x11);
    assert_eq!(cpu.regs.ds_cache.base, 0x3000);

    cpu.load_data_segment(&mem, DataSegReg::DS, 0x0010);
    assert_eq!(cpu.regs.ds_cache.base, 0x4000);
}

// ========================================================================
// MOV [mem], imm — immediate-to-memory writes also trigger SMC
// ========================================================================

#[test]
fn test_smc_mov_imm_byte_to_memory() {
    // C6 06 00 50 42 — MOV byte [0x5000], 0x42
    let code: &[u8] = &[0xC6, 0x06, 0x00, 0x50, 0x42];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.ram[0x5000], 0x42);

    let writes = notifier.snapshot();
    assert!(
        writes.iter().any(|&(addr, sz)| addr == 0x5000 && sz == 1),
        "Expected SMC notification at 0x5000 size 1, got: {:?}",
        writes
    );
}

#[test]
fn test_smc_mov_imm_word_to_memory() {
    // C7 06 00 50 34 12 — MOV word [0x5000], 0x1234
    let code: &[u8] = &[0xC7, 0x06, 0x00, 0x50, 0x34, 0x12];
    let (mut cpu, mut mem, notifier) = setup_with_smc(code);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(mem.read_u16(0x5000), 0x1234);

    let writes = notifier.snapshot();
    assert!(
        writes.iter().any(|&(addr, sz)| addr == 0x5000 && sz == 2),
        "Expected SMC notification at 0x5000 size 2, got: {:?}",
        writes
    );
}

// ========================================================================
// F5: Segment limit enforcement
// ========================================================================

/// Helper: set up PM with CS at base 0, DS with given base/limit.
fn setup_pm_with_ds_limit(code: &[u8], ds_base: u32, ds_limit: u32) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 |= 1;
    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true; // 4KB pages → 4GB effective limit
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0x9B;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = true;
    cpu.cs_bits = 32;
    cpu.regs.ds = 0x0010;
    cpu.regs.ds_cache.base = ds_base;
    cpu.regs.ds_cache.limit = ds_limit;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.access = 0x93;
    cpu.regs.ds_cache.is_code = false;
    cpu.regs.ds_cache.big = true;
    cpu.regs.ss = 0x0018;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true; // 4KB pages → 4GB effective limit
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.access = 0x93;
    cpu.regs.ss_cache.big = true;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

#[test]
fn test_f5_mov_within_limit_succeeds() {
    // MOV EAX, [moffs32=0x50] — DS limit = 0xFF (byte gran)
    // Access at offset 0x50, size 4 → last byte 0x53 ≤ 0xFF → OK
    let code = [0xA1, 0x50, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0xFF);
    mem.ram[0x50] = 0x42;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax & 0xFF, 0x42);
}

#[test]
fn test_f5_mov_past_limit_raises_gp() {
    // MOV EAX, [moffs32=0xFE] — DS limit = 0xFF (byte gran)
    // offset 0xFE + size 4 → last byte 0x101 > 0xFF → #GP(0)
    let code = [0xA1, 0xFE, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0xFF);
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
        "Expected #GP(0), got {:?}",
        result
    );
}

#[test]
fn test_f5_4gb_flat_never_faults() {
    // DS limit = 0xFFFFF with 4KB granularity = 4GB flat → never faults
    let code = [0xA1, 0x50, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0xFFFFF);
    cpu.regs.ds_cache.granularity = true; // 4KB pages → effective_limit = 0xFFFF_FFFF
    cpu.regs.ds_cache.recompute_effective_limit();
    assert_eq!(cpu.regs.ds_cache.effective_limit(), 0xFFFF_FFFF);
    mem.ram[0x50] = 0xAA;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
}

#[test]
fn test_f5_ss_relative_raises_ss_fault() {
    // MOV EAX, [EBP+0] — SS-relative (BP base). SS limit = 0xFF
    // EBP=0x100 → offset 0x100 > limit 0xFF → #SS(0), vector 12
    let code = [0x8B, 0x45, 0x00]; // MOV EAX, [EBP+0]
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0xFFFFF);
    cpu.regs.ss_cache.limit = 0xFF;
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ebp = 0x100;
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
        "Expected #SS(0), got {:?}",
        result
    );
}

#[test]
fn test_f5_segment_override_checks_overridden_segment() {
    // ES: MOV AL, [moffs32=0x50] — ES limit = 0x40 → #GP(0)
    let code = [0x26, 0xA0, 0x50, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0xFFFFF);
    cpu.regs.es = 0x0020;
    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0x40;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.flags = 0;
    cpu.regs.es_cache.access = 0x93;
    cpu.regs.es_cache.is_code = false;
    cpu.regs.es_cache.big = true;
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
        "Expected #GP(0) on ES limit violation, got {:?}",
        result
    );
}

#[test]
fn test_f5_byte_at_exact_limit_ok() {
    // MOV AL, [moffs32=0xFF] — DS limit = 0xFF → last byte 0xFF ≤ 0xFF → OK
    let code = [0xA0, 0xFF, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0xFF);
    mem.ram[0xFF] = 0x77;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax & 0xFF, 0x77);
}

#[test]
fn test_f5_byte_one_past_limit_faults() {
    // MOV AL, [moffs32=0x100] — DS limit = 0xFF → 0x100 > 0xFF → #GP(0)
    let code = [0xA0, 0x00, 0x01, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0xFF);
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
        "Expected #GP(0), got {:?}",
        result
    );
}

#[test]
fn test_f5_modrm_operand_limit_fault() {
    // MOV EAX, [EBX] — DS limit = 0xFF, EBX = 0x100 → #GP(0)
    // 0x8B 0x03 = MOV EAX, [EBX]
    let code = [0x8B, 0x03];
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0xFF);
    cpu.regs.ebx = 0x100;
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
        "Expected #GP(0) on ModR/M DS limit violation, got {:?}",
        result
    );
}

#[test]
fn test_f5_expand_down_within_range_ok() {
    // Expand-down segment: valid range is (limit, max].
    // DS limit = 0x1000, big=true → valid range (0x1000, 0xFFFF_FFFF]
    // Access at offset 0x2000, size 4 → last byte 0x2003 ≤ 0xFFFF_FFFF → OK
    let code = [0xA1, 0x00, 0x20, 0x00, 0x00]; // MOV EAX, [moffs32=0x2000]
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0x1000);
    cpu.regs.ds_cache.access = 0x97; // expand-down data (bit 2 set: 0x93 | 0x04)
    mem.ram[0x2000] = 0x42;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.eax & 0xFF, 0x42);
}

#[test]
fn test_f5_expand_down_below_limit_faults() {
    // Expand-down: valid range is (limit, max].
    // DS limit = 0x1000 → offset 0x0500 ≤ limit → #GP(0)
    let code = [0xA1, 0x00, 0x05, 0x00, 0x00]; // MOV EAX, [moffs32=0x0500]
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0x1000);
    cpu.regs.ds_cache.access = 0x97; // expand-down
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
        "Expected #GP(0) on expand-down violation, got {:?}",
        result
    );
}

#[test]
fn test_f5_expand_down_full_limit_has_empty_valid_range() {
    // A big expand-down segment is valid only in (limit, 0xFFFF_FFFF]. With a
    // full effective limit that interval is empty, so even offset zero faults.
    let code = [0xA1, 0x00, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0x000F_FFFF);
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.access = 0x97;
    cpu.regs.ds_cache.recompute_effective_limit();
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "empty expand-down DS must #GP(0), got {result:?}"
    );
}

#[test]
fn test_f5_expand_down_full_limit_push_raises_ss_before_commit() {
    let code = [0x50]; // PUSH EAX
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0x000F_FFFF);
    cpu.regs.ss_cache.limit = 0x000F_FFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.access = 0x97;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.eax = 0xDEAD_BEEF;
    let saved_esp = cpu.regs.esp;
    let before = mem.ram[0x7FFC..0x8000].to_vec();

    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 12,
                error_code: 0,
                ..
            }
        ),
        "empty expand-down SS must #SS(0), got {result:?}"
    );
    assert_eq!(cpu.regs.esp, saved_esp);
    assert_eq!(&mem.ram[0x7FFC..0x8000], before.as_slice());
}

#[test]
fn test_f5_pushad_preflights_each_expand_down_stack_slot() {
    // Valid expand-down range is (0x7FFF, MAX]. From ESP=0x8010 the first
    // four dword slots are valid (0x800C..0x8000), but the fifth is not.
    // PUSHAD must discover that later #SS before any earlier slot is written.
    let code = [0x60]; // PUSHAD
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0x000F_FFFF);
    cpu.regs.ss_cache.limit = 0x7FFF;
    cpu.regs.ss_cache.granularity = false;
    cpu.regs.ss_cache.access = 0x97;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.esp = 0x8010;
    cpu.regs.eax = 0x1111_1111;
    cpu.regs.ecx = 0x2222_2222;
    cpu.regs.edx = 0x3333_3333;
    cpu.regs.ebx = 0x4444_4444;
    let before = mem.ram[0x7FF0..0x8010].to_vec();

    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 12,
                error_code: 0,
                ..
            }
        ),
        "later invalid PUSHAD slot must #SS(0), got {result:?}"
    );
    assert_eq!(cpu.regs.esp, 0x8010);
    assert_eq!(&mem.ram[0x7FF0..0x8010], before.as_slice());
}

#[test]
fn test_f5_flat_segment_multi_byte_offset_overflow_faults() {
    // A full-span expand-up segment still cannot serve a dword whose offset
    // wraps past 0xFFFF_FFFF. This must not disappear through the flat fast path.
    let code = [0xA1, 0xFF, 0xFF, 0xFF, 0xFF];
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0x000F_FFFF);
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "flat DS offset overflow must #GP(0), got {result:?}"
    );
}

#[test]
fn test_f5_checked_add_overflow_faults() {
    // offset = 0xFFFF_FFFF, size = 4 → overflow → #GP(0)
    // Use MOV EAX, [moffs32=0xFFFFFFFF]
    let code = [0xA1, 0xFF, 0xFF, 0xFF, 0xFF];
    let (mut cpu, mut mem) = setup_pm_with_ds_limit(&code, 0, 0xFF);
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
        "Expected #GP(0) on overflow, got {:?}",
        result
    );
}

// M2: String op bulk path segment limit endpoint checks

/// Helper: set up PM with CS/SS flat, ES with given base/limit, DS flat.
fn setup_pm_with_es_limit(code: &[u8], es_base: u32, es_limit: u32) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 |= 1;
    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0x9B;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = true;
    cpu.cs_bits = 32;
    // DS: flat, base=0, 4GB limit
    cpu.regs.ds = 0x0010;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.access = 0x93;
    cpu.regs.ds_cache.big = true;
    // ES: restricted limit
    cpu.regs.es = 0x0020;
    cpu.regs.es_cache.base = es_base;
    cpu.regs.es_cache.limit = es_limit;
    cpu.regs.es_cache.granularity = false; // byte granularity
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.access = 0x93;
    cpu.regs.es_cache.big = true;
    // SS: flat
    cpu.regs.ss = 0x0018;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.access = 0x93;
    cpu.regs.ss_cache.big = true;
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

#[test]
fn test_m2_rep_stosb_within_es_limit_succeeds() {
    // REP STOSB: F3 AA, CX=256, ES limit=511 → all within limit
    let code: &[u8] = &[0xF3, 0xAA];
    let (mut cpu, mut mem) = setup_pm_with_es_limit(code, 0, 511);
    cpu.regs.eax = 0x42;
    cpu.regs.ecx = 256;
    cpu.regs.edi = 0; // start at ES:0
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 256);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 256 }),
        "REP STOSB within ES limit should succeed, got {:?}",
        result,
    );
    assert_eq!(cpu.regs.ecx, 0, "CX should be 0 after completion");
}

#[test]
fn test_m2_rep_stosb_exceeds_es_limit_no_bulk() {
    // REP STOSB: F3 AA, ECX=512, ES limit=255 (byte granularity).
    // DI(0) + 512 - 1 = 511 > 255, so the run must stop AT the limit.
    //
    // MDD-BUG-CRUCIBLE-00563: this test is named for exceeding the limit, but
    // it used to set ECX to 128 and assert a successful in-range fill - making
    // it a duplicate of the in-limit test above, and leaving it green even if
    // the endpoint guard were deleted. It now runs the 512 its own comment
    // always described.
    let code: &[u8] = &[0xF3, 0xAA];
    let (mut cpu, mut mem) = setup_pm_with_es_limit(code, 0, 255);
    cpu.regs.eax = 0x42;
    cpu.regs.ecx = 512;
    cpu.regs.edi = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 512);

    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                executed: 256,
                ..
            }
        ),
        "crossing the ES limit must #GP after the last in-range store, got {result:?}"
    );
    // Restartable state: the faulting store is NOT retired, so ECX and EDI
    // both sit at the crossing point rather than at completion.
    assert_eq!(cpu.regs.ecx, 256, "ECX must hold the un-stored remainder");
    assert_eq!(
        cpu.regs.edi, 256,
        "EDI must stop at the first out-of-limit offset"
    );

    // The discriminating check: it stopped at exactly the right place. A guard
    // that faulted early or late would still produce a #GP, so asserting the
    // fault alone proves much less than it looks like it does.
    for i in 0..256 {
        assert_eq!(
            mem.ram[i], 0x42,
            "offset {i} is inside the limit and must be written"
        );
    }
    assert_ne!(
        mem.ram[256], 0x42,
        "offset 256 is past the 255-byte limit and must NOT have been written"
    );
}

#[test]
fn test_m2_rep_movsb_within_es_limit_succeeds() {
    // REP MOVSB: F3 A4, CX=64, DS flat, ES limit=255
    // Source at DS:SI=0x2000, dest at ES:DI=0
    let code: &[u8] = &[0xF3, 0xA4];
    let (mut cpu, mut mem) = setup_pm_with_es_limit(code, 0, 255);
    cpu.regs.ecx = 64;
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0;
    // Fill source
    for i in 0..64 {
        mem.ram[0x2000 + i] = i as u8;
    }
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 64);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 64 }),
        "REP MOVSB within limit should succeed, got {:?}",
        result,
    );
    assert_eq!(cpu.regs.ecx, 0);
    // Verify data was copied
    assert_eq!(mem.ram[0], 0);
    assert_eq!(mem.ram[63], 63);
}

/// Set up a 32-bit protected-mode code segment for MOVS with 16-bit
/// addressing. Exactly one data segment has a 64 KiB byte-granular limit;
/// the other is flat so each test isolates the source or destination check.
fn setup_pm_movs_16(code: &[u8], restrict_source: bool) -> (Cpu, Memory) {
    let ds_limit = if restrict_source { 0xFFFF } else { 0xFFFFF };
    let (mut cpu, mem) = setup_pm_with_ds_limit(code, 0x2_0000, ds_limit);
    cpu.regs.ds_cache.granularity = !restrict_source;
    cpu.regs.ds_cache.recompute_effective_limit();

    cpu.regs.es = 0x0020;
    cpu.regs.es_cache.base = 0x4_0000;
    cpu.regs.es_cache.limit = if restrict_source { 0xFFFFF } else { 0xFFFF };
    cpu.regs.es_cache.granularity = restrict_source;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.access = 0x93;
    cpu.regs.es_cache.is_code = false;
    cpu.regs.es_cache.big = true;
    (cpu, mem)
}

fn assert_movs_gp(result: RunResult, executed: u32) {
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                fault_addr: None,
                executed: actual,
            } if actual == executed
        ),
        "expected #GP(0) after {executed} completed element(s), got {result:?}"
    );
}

#[test]
fn test_movs_16bit_segment_boundary_movsw_source_checks_full_word() {
    let code = [0x67, 0x66, 0xA5]; // addr16 MOVSW

    let (mut cpu, mut mem) = setup_pm_movs_16(&code, true);
    cpu.regs.set_si(0xFFFE);
    cpu.regs.set_di(0x0100);
    mem.ram[0x2_FFFE..0x3_0000].copy_from_slice(&[0x34, 0x12]);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(&mem.ram[0x4_0100..0x4_0102], &[0x34, 0x12]);

    let (mut cpu, mut mem) = setup_pm_movs_16(&code, true);
    cpu.regs.set_si(0xFFFF);
    cpu.regs.set_di(0x0100);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_movs_gp(result, 0);
    assert_eq!(cpu.regs.si(), 0xFFFF);
    assert_eq!(cpu.regs.di(), 0x0100);
}

#[test]
fn test_movs_16bit_segment_boundary_movsw_destination_checks_full_word() {
    let code = [0x67, 0x66, 0xA5]; // addr16 MOVSW

    let (mut cpu, mut mem) = setup_pm_movs_16(&code, false);
    cpu.regs.set_si(0x0100);
    cpu.regs.set_di(0xFFFE);
    mem.ram[0x2_0100..0x2_0102].copy_from_slice(&[0x78, 0x56]);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(&mem.ram[0x4_FFFE..0x5_0000], &[0x78, 0x56]);

    let (mut cpu, mut mem) = setup_pm_movs_16(&code, false);
    cpu.regs.set_si(0x0100);
    cpu.regs.set_di(0xFFFF);
    mem.ram[0x2_0100..0x2_0102].copy_from_slice(&[0x78, 0x56]);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_movs_gp(result, 0);
    assert_eq!(cpu.regs.si(), 0x0100);
    assert_eq!(cpu.regs.di(), 0xFFFF);
}

#[test]
fn test_movs_16bit_segment_boundary_movsd_source_checks_full_dword() {
    let code = [0x67, 0xA5]; // addr16 MOVSD

    let (mut cpu, mut mem) = setup_pm_movs_16(&code, true);
    cpu.regs.set_si(0xFFFC);
    cpu.regs.set_di(0x0100);
    mem.ram[0x2_FFFC..0x3_0000].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(&mem.ram[0x4_0100..0x4_0104], &0x1234_5678u32.to_le_bytes());

    let (mut cpu, mut mem) = setup_pm_movs_16(&code, true);
    cpu.regs.set_si(0xFFFD);
    cpu.regs.set_di(0x0100);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_movs_gp(result, 0);
    assert_eq!(cpu.regs.si(), 0xFFFD);
    assert_eq!(cpu.regs.di(), 0x0100);
}

#[test]
fn test_movs_16bit_segment_boundary_movsd_destination_checks_full_dword() {
    let code = [0x67, 0xA5]; // addr16 MOVSD

    let (mut cpu, mut mem) = setup_pm_movs_16(&code, false);
    cpu.regs.set_si(0x0100);
    cpu.regs.set_di(0xFFFC);
    mem.ram[0x2_0100..0x2_0104].copy_from_slice(&0x89AB_CDEFu32.to_le_bytes());
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(&mem.ram[0x4_FFFC..0x5_0000], &0x89AB_CDEFu32.to_le_bytes());

    let (mut cpu, mut mem) = setup_pm_movs_16(&code, false);
    cpu.regs.set_si(0x0100);
    cpu.regs.set_di(0xFFFD);
    mem.ram[0x2_0100..0x2_0104].copy_from_slice(&0x89AB_CDEFu32.to_le_bytes());
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert_movs_gp(result, 0);
    assert_eq!(cpu.regs.si(), 0x0100);
    assert_eq!(cpu.regs.di(), 0xFFFD);
}

#[test]
fn test_movs_16bit_segment_boundary_rep_movsw_source_fault_is_restartable() {
    let code = [0xF3, 0x67, 0x66, 0xA5]; // REP addr16 MOVSW
    let (mut cpu, mut mem) = setup_pm_movs_16(&code, true);
    cpu.regs.set_si(0xFFFD);
    cpu.regs.set_di(0x0100);
    cpu.regs.set_cx(2);
    mem.ram[0x2_FFFD..0x3_0001].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);

    let result = run(&mut cpu, &mut mem, &mut (), 8);
    assert_movs_gp(result, 1);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(cpu.regs.si(), 0xFFFF);
    assert_eq!(cpu.regs.di(), 0x0102);
    assert_eq!(&mem.ram[0x4_0100..0x4_0102], &[0x11, 0x22]);
}

#[test]
fn test_movs_16bit_segment_boundary_rep_movsd_destination_fault_is_restartable() {
    let code = [0xF3, 0x67, 0xA5]; // REP addr16 MOVSD
    let (mut cpu, mut mem) = setup_pm_movs_16(&code, false);
    cpu.regs.set_si(0x0100);
    cpu.regs.set_di(0xFFF9);
    cpu.regs.set_cx(2);
    mem.ram[0x2_0100..0x2_0108].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);

    let result = run(&mut cpu, &mut mem, &mut (), 8);
    assert_movs_gp(result, 1);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(cpu.regs.cx(), 1);
    assert_eq!(cpu.regs.si(), 0x0104);
    assert_eq!(cpu.regs.di(), 0xFFFD);
    assert_eq!(&mem.ram[0x4_FFF9..0x4_FFFD], &[0x11, 0x22, 0x33, 0x44]);
}
