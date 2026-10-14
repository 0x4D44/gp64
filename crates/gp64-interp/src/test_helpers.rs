use gp64_cpu::Cpu;
use gp64_memory::Memory;

/// Set up a real-mode CPU+Memory with code placed at 0000:0100.
pub(crate) fn setup(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

/// Build an 8-byte x86 segment descriptor from base, limit, access, and flags.
pub(crate) fn make_descriptor(base: u32, limit: u32, access: u8, flags: u8) -> [u8; 8] {
    let limit_low = (limit & 0xFFFF) as u16;
    let limit_high = ((limit >> 16) & 0x0F) as u8;
    let base_low = (base & 0xFFFF) as u16;
    let base_mid = ((base >> 16) & 0xFF) as u8;
    let base_high = ((base >> 24) & 0xFF) as u8;
    let [ll, lh] = limit_low.to_le_bytes();
    let [bl, bh] = base_low.to_le_bytes();
    [
        ll,
        lh,
        bl,
        bh,
        base_mid,
        access,
        limit_high | (flags << 4),
        base_high,
    ]
}

/// Write an 8-byte GDT entry into memory at the given address.
pub(crate) fn write_gdt_entry(mem: &mut Memory, addr: usize, desc: [u8; 8]) {
    for (i, &b) in desc.iter().enumerate() {
        mem.ram[addr + i] = b;
    }
}

/// Set up a PM32 flat CPU+Memory: CS/DS/SS/ES all base 0, 4 GB limit,
/// 32-bit big segments, paging off, `cs_bits = 32`, code placed at
/// linear/physical 0x1000. Segment caches are set directly (the
/// `paging_integration_tests::setup_paging` pattern, lib.rs — search
/// for `mod paging_integration_tests`) rather than via a GDT + selector
/// load, since `Cpu::new()`'s default `SegmentCache::real_mode(0)`
/// already supplies `present`/`is_writable`/`is_readable`.
///
/// EXP-IQ012: the x87 memory lane declines everything under
/// `cs_bits != 32` (design §4), so the real-mode `setup` harness above
/// would compare full-vs-full vacuously for the memory-lane tests.
pub(crate) fn setup_pm32(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();

    cpu.regs.cr0 = 0x0000_0001; // PE, no paging
    cpu.regs.cpl = 0;

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
    cpu.regs.set_ip(0x1000);
    cpu.regs.eip = 0x1000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x1000 + i] = b;
    }

    (cpu, mem)
}
