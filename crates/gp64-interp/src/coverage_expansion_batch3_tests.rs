use super::test_helpers::{make_descriptor, write_gdt_entry};
use super::*;
use gp64_cpu::{Cpu, Flags};
use gp64_memory::Memory;

// ========================================================================
// Helper: setup protected-mode 32-bit (flat model, 8 GDT entries)
// GDT layout:
//   0x00: null
//   0x08: ring-0 code 32-bit (0x9A, G=1 D=1)
//   0x10: ring-0 data 32-bit (0x92, G=1 B=1)
//   0x18: ring-3 code 32-bit (0xFA, G=1 D=1)
//   0x20: ring-3 data 32-bit (0xF2, G=1 B=1)
//   0x28: ring-0 code 16-bit (0x9A, G=1 D=0)
//   0x30: data base=0x1000  (0x92, G=1 B=1)
//   0x38: data base=0x2000  (0x92, G=1 B=1)
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

// Helper: setup paging (identity map first 1MB, PE+PG, 32-bit PM flat)
fn setup_paging(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 = 0x8000_0001; // PE + PG
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

// Helper: setup paging with a GDT (needed for CALL FAR, segment loads, etc.)
fn setup_paging_with_gdt(code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mut mem) = setup_paging(code);
    // Place GDT at 0x20000 (page 0x20 is identity-mapped within 256-page range)
    let gdt_base: u32 = 0x20000;
    let gdt_limit: u16 = 10 * 8 - 1;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]); // 0x00: null
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C), // 0x08: ring-0 code 32-bit
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C), // 0x10: ring-0 data 32-bit
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C), // 0x18: ring-0 SS 32-bit big
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x20,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C), // 0x20: ring-0 ES 32-bit
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x28,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08), // 0x28: ring-0 code 16-bit
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x30,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C), // 0x30: FS data
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x38,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C), // 0x38: GS data
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x40,
        make_descriptor(0, 0xFFFFF, 0x92, 0x08), // 0x40: ring-0 SS 16-bit
    );
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x48,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C), // 0x48: extra data 32-bit
    );
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    // Update segment caches from GDT
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 32;
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ss = 0x18;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x18).unwrap();
    cpu.regs.es = 0x20;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, 0x20).unwrap();
    cpu.regs.fs = 0x30;
    cpu.regs.fs_cache = cpu.read_descriptor(&mem, 0x30).unwrap();
    cpu.regs.gs = 0x38;
    cpu.regs.gs_cache = cpu.read_descriptor(&mem, 0x38).unwrap();
    // IDT at 0x30000 for INT tests
    cpu.regs.idtr.base = 0x30000;
    cpu.regs.idtr.limit = 0x07FF;
    (cpu, mem)
}

const HIGH_IDT_BASE: u32 = 0x8003_6400;
const HIGH_IDT_PAGE: u32 = 0x8003_6000;
const HIGH_IDT_PHYS: usize = 0x70000;
const HIGH_IDT_PAGE_TABLE: usize = 0x12000;

fn map_high_idt_page(mem: &mut Memory, user_accessible: bool) {
    let flags = 0x02 | if user_accessible { 0x04 } else { 0 };
    let pde_index = (HIGH_IDT_PAGE >> 22) as usize;
    let pte_index = ((HIGH_IDT_PAGE >> 12) & 0x03FF) as usize;
    mem.write_u32_physical(
        0x10000 + pde_index * 4,
        HIGH_IDT_PAGE_TABLE as u32 | flags | 1,
    );
    mem.write_u32_physical(
        HIGH_IDT_PAGE_TABLE + pte_index * 4,
        HIGH_IDT_PHYS as u32 | flags | 1,
    );
}

fn high_idt_gate_phys(vector: u8) -> usize {
    let gate_linear = HIGH_IDT_BASE + (vector as u32) * 8;
    HIGH_IDT_PHYS + gate_linear.wrapping_sub(HIGH_IDT_PAGE) as usize
}

fn write_test_idt_gate(mem: &mut Memory, gate_phys: usize, type_attr: u8) {
    mem.ram[gate_phys] = 0x00;
    mem.ram[gate_phys + 1] = 0x60;
    mem.ram[gate_phys + 2] = 0x08;
    mem.ram[gate_phys + 3] = 0x00;
    mem.ram[gate_phys + 4] = 0x00;
    mem.ram[gate_phys + 5] = type_attr;
    mem.ram[gate_phys + 6] = 0x00;
    mem.ram[gate_phys + 7] = 0x00;
}

fn enter_ring3_for_int_test(cpu: &mut Cpu) {
    cpu.regs.cpl = 3;
    cpu.regs.cs = 0x1B;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0xFA;
    cpu.regs.cs_cache.dpl = 3;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = true;
    cpu.regs.cs_cache.present = true;
}

// ========================================================================
// 1. REP MOVSW with paging (word-size copy)
// ========================================================================
#[test]
fn test_paging_rep_movsw() {
    // F3 66 A5 = REP MOVSW (66h prefix switches MOVSD to 16-bit operand)
    // In 32-bit CS, MOVSD (A5) is the default; 66h prefix makes it MOVSW.
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0x66, 0xA5]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    cpu.regs.ecx = 8;
    for i in 0..16u8 {
        mem.ram[0x2000 + i as usize] = 0xA0 + i;
    }
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..16u8 {
        assert_eq!(mem.ram[0x3000 + i as usize], 0xA0 + i);
    }
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.esi, 0x2010);
    assert_eq!(cpu.regs.edi, 0x3010);
}

// ========================================================================
// 2. REP MOVSD with paging (dword-size copy)
// ========================================================================
#[test]
fn test_paging_rep_movsd() {
    // F3 A5 = REP MOVSD (in 32-bit mode)
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA5]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4000;
    cpu.regs.ecx = 4;
    for i in 0..16u8 {
        mem.ram[0x2000 + i as usize] = 0x10 + i;
    }
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..16u8 {
        assert_eq!(mem.ram[0x4000 + i as usize], 0x10 + i);
    }
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.esi, 0x2010);
    assert_eq!(cpu.regs.edi, 0x4010);
}

// ========================================================================
// 3. REP MOVSB with paging, DF=1 (backward)
// ========================================================================
#[test]
fn test_paging_rep_movsb_backward() {
    // STD; REP MOVSB = FD F3 A4
    let (mut cpu, mut mem) = setup_paging(&[0xFD, 0xF3, 0xA4]);
    for i in 0..16u8 {
        mem.ram[0x2000 + i as usize] = 0x50 + i;
    }
    cpu.regs.esi = 0x200F; // start from end
    cpu.regs.edi = 0x300F;
    cpu.regs.ecx = 16;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 256);
    for i in 0..16u8 {
        assert_eq!(mem.ram[0x3000 + i as usize], 0x50 + i);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 4. REP MOVSW with paging, DF=1 (backward words)
// ========================================================================
#[test]
fn test_paging_rep_movsw_backward() {
    // STD; REP MOVSW = FD F3 66 A5
    let (mut cpu, mut mem) = setup_paging(&[0xFD, 0xF3, 0x66, 0xA5]);
    for i in 0..8u16 {
        let addr = 0x2000 + (i as usize) * 2;
        mem.ram[addr] = (0xAA + i) as u8;
        mem.ram[addr + 1] = (0xBB + i) as u8;
    }
    cpu.regs.esi = 0x200E; // last word starts at 0x200E
    cpu.regs.edi = 0x300E;
    cpu.regs.ecx = 8;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 256);
    for i in 0..8u16 {
        let addr = 0x3000 + (i as usize) * 2;
        assert_eq!(mem.ram[addr], (0xAA + i) as u8);
        assert_eq!(mem.ram[addr + 1], (0xBB + i) as u8);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 5. REP MOVSD with paging, DF=1 (backward dwords)
// ========================================================================
#[test]
fn test_paging_rep_movsd_backward() {
    // STD; REP MOVSD = FD F3 A5
    let (mut cpu, mut mem) = setup_paging(&[0xFD, 0xF3, 0xA5]);
    for i in 0..4u32 {
        let addr = 0x2000 + (i as usize) * 4;
        mem.ram[addr] = (i + 1) as u8;
        mem.ram[addr + 1] = (i + 2) as u8;
        mem.ram[addr + 2] = (i + 3) as u8;
        mem.ram[addr + 3] = (i + 4) as u8;
    }
    cpu.regs.esi = 0x200C; // last dword at 0x200C
    cpu.regs.edi = 0x400C;
    cpu.regs.ecx = 4;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 256);
    for i in 0..16 {
        assert_eq!(mem.ram[0x4000 + i], mem.ram[0x2000 + i]);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 6. REP STOSW with paging
// ========================================================================
#[test]
fn test_paging_rep_stosw() {
    // F3 66 AB = REP STOSW
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0x66, 0xAB]);
    cpu.regs.eax = 0x0000_BEEF; // AX=0xBEEF
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..8 {
        let addr = 0x5000 + i * 2;
        let word = u16::from_le_bytes([mem.ram[addr], mem.ram[addr + 1]]);
        assert_eq!(word, 0xBEEF);
    }
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.edi, 0x5010);
}

// ========================================================================
// 7. REP STOSD with paging
// ========================================================================
#[test]
fn test_paging_rep_stosd() {
    // F3 AB = REP STOSD
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAB]);
    cpu.regs.eax = 0xDEAD_BEEF;
    cpu.regs.edi = 0x6000;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..4 {
        let addr = 0x6000 + i * 4;
        let dword = u32::from_le_bytes([
            mem.ram[addr],
            mem.ram[addr + 1],
            mem.ram[addr + 2],
            mem.ram[addr + 3],
        ]);
        assert_eq!(dword, 0xDEAD_BEEF);
    }
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.edi, 0x6010);
}

// ========================================================================
// 8. REP STOSB with paging, DF=1 (backward), non-identity mapping
// ========================================================================
#[test]
fn test_paging_rep_stosb_backward_nonidentity() {
    // STD; REP STOSB = FD F3 AA
    let (mut cpu, mut mem) = setup_paging(&[0xFD, 0xF3, 0xAA]);
    // Remap page 5 -> physical page 0x40
    mem.write_u32_physical(0x11000 + 5 * 4, (0x40 << 12) | 0x07);
    cpu.regs.set_al(0xDD);
    cpu.regs.edi = 0x500F;
    cpu.regs.ecx = 16;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 256);
    for i in 0..16 {
        assert_eq!(
            mem.ram[0x40000 + i],
            0xDD,
            "Byte {} should be 0xDD in remapped page",
            i
        );
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 9. REP STOSW with paging, DF=1 (backward words)
// ========================================================================
#[test]
fn test_paging_rep_stosw_backward() {
    // STD; REP STOSW = FD F3 66 AB
    let (mut cpu, mut mem) = setup_paging(&[0xFD, 0xF3, 0x66, 0xAB]);
    cpu.regs.eax = 0x0000_CAFE;
    cpu.regs.edi = 0x500E;
    cpu.regs.ecx = 8;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 256);
    for i in 0..8 {
        let addr = 0x5000 + i * 2;
        let word = u16::from_le_bytes([mem.ram[addr], mem.ram[addr + 1]]);
        assert_eq!(word, 0xCAFE);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 10. REP STOSD with paging, DF=1 (backward dwords)
// ========================================================================
#[test]
fn test_paging_rep_stosd_backward() {
    // STD; REP STOSD = FD F3 AB
    let (mut cpu, mut mem) = setup_paging(&[0xFD, 0xF3, 0xAB]);
    cpu.regs.eax = 0xABCD_1234;
    cpu.regs.edi = 0x600C;
    cpu.regs.ecx = 4;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 256);
    for i in 0..4 {
        let addr = 0x6000 + i * 4;
        let dword = u32::from_le_bytes([
            mem.ram[addr],
            mem.ram[addr + 1],
            mem.ram[addr + 2],
            mem.ram[addr + 3],
        ]);
        assert_eq!(dword, 0xABCD_1234);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 11. REP LODSB with paging (byte load, multiple iterations)
// ========================================================================
#[test]
fn test_paging_rep_lodsb() {
    // F3 AC = REP LODSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAC]);
    for i in 0..8u8 {
        mem.ram[0x3000 + i as usize] = 0x10 + i;
    }
    cpu.regs.esi = 0x3000;
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    // After REP LODSB, AL contains the last loaded byte
    assert_eq!(cpu.regs.al(), 0x17);
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.esi, 0x3008);
}

// ========================================================================
// 12. REP LODSW with paging (word load)
// ========================================================================
#[test]
fn test_paging_rep_lodsw() {
    // F3 66 AD = REP LODSW
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0x66, 0xAD]);
    mem.ram[0x3000] = 0x11;
    mem.ram[0x3001] = 0x22;
    mem.ram[0x3002] = 0x33;
    mem.ram[0x3003] = 0x44;
    cpu.regs.esi = 0x3000;
    cpu.regs.ecx = 2;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    // Last loaded word: 0x4433
    assert_eq!(cpu.regs.eax & 0xFFFF, 0x4433);
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.esi, 0x3004);
}

// ========================================================================
// 13. REP LODSB with paging, DF=1 (backward)
// ========================================================================
#[test]
fn test_paging_rep_lodsb_backward() {
    // STD; REP LODSB = FD F3 AC
    let (mut cpu, mut mem) = setup_paging(&[0xFD, 0xF3, 0xAC]);
    for i in 0..4u8 {
        mem.ram[0x3000 + i as usize] = 0xA0 + i;
    }
    cpu.regs.esi = 0x3003; // start from last byte
    cpu.regs.ecx = 4;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    // Last loaded byte is mem[0x3000] = 0xA0
    assert_eq!(cpu.regs.al(), 0xA0);
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 14. REP CMPSB with paging (match — REPE terminates after all equal)
// ========================================================================
#[test]
fn test_paging_rep_cmpsb_all_equal() {
    // F3 A6 = REPE CMPSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA6]);
    for i in 0..8u8 {
        mem.ram[0x2000 + i as usize] = 0x30 + i;
        mem.ram[0x4000 + i as usize] = 0x30 + i;
    }
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4000;
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    assert_eq!(cpu.regs.ecx, 0);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.esi, 0x2008);
    assert_eq!(cpu.regs.edi, 0x4008);
}

// ========================================================================
// 15. REP CMPSB with paging (mismatch terminates early)
// ========================================================================
#[test]
fn test_paging_rep_cmpsb_mismatch() {
    // F3 A6 = REPE CMPSB, F4 = HLT
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA6, 0xF4]);
    for i in 0..8u8 {
        mem.ram[0x2000 + i as usize] = 0x30 + i;
        mem.ram[0x4000 + i as usize] = 0x30 + i;
    }
    // Introduce mismatch at position 3
    mem.ram[0x4003] = 0xFF;
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4000;
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(matches!(result, RunResult::Halt { .. }), "got {:?}", result);
    // Should stop after 4 comparisons (index 3 is mismatch)
    assert_eq!(cpu.regs.ecx, 4); // 8 - 4 = 4 remaining
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.esi, 0x2004);
    assert_eq!(cpu.regs.edi, 0x4004);
}

// ========================================================================
// 16. REPNE CMPSB with paging (scan for first match)
// ========================================================================
#[test]
fn test_paging_repne_cmpsb() {
    // F2 A6 = REPNE CMPSB
    let (mut cpu, mut mem) = setup_paging(&[0xF2, 0xA6]);
    for i in 0..8u8 {
        mem.ram[0x2000 + i as usize] = 0x10 + i;
        mem.ram[0x4000 + i as usize] = 0x20 + i; // different
    }
    // Make position 4 match
    mem.ram[0x4004] = 0x14;
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4000;
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    // Should stop after comparing index 4 (first match)
    assert_eq!(cpu.regs.ecx, 3); // 8 - 5 = 3 remaining
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 17. REP CMPSW with paging (word comparison)
// ========================================================================
#[test]
fn test_paging_rep_cmpsw() {
    // F3 66 A7 = REPE CMPSW
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0x66, 0xA7]);
    // Write 4 equal words
    for i in 0..4u16 {
        let src_addr = 0x2000 + (i as usize) * 2;
        let dst_addr = 0x4000 + (i as usize) * 2;
        mem.ram[src_addr] = (i + 1) as u8;
        mem.ram[src_addr + 1] = 0xAA;
        mem.ram[dst_addr] = (i + 1) as u8;
        mem.ram[dst_addr + 1] = 0xAA;
    }
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4000;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    assert_eq!(cpu.regs.ecx, 0);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 18. REP CMPSB with paging, DF=1 (backward compare)
// ========================================================================
#[test]
fn test_paging_rep_cmpsb_backward() {
    // STD; REPE CMPSB = FD F3 A6
    let (mut cpu, mut mem) = setup_paging(&[0xFD, 0xF3, 0xA6]);
    for i in 0..4u8 {
        mem.ram[0x2000 + i as usize] = 0x50 + i;
        mem.ram[0x4000 + i as usize] = 0x50 + i;
    }
    cpu.regs.esi = 0x2003; // start from last byte
    cpu.regs.edi = 0x4003;
    cpu.regs.ecx = 4;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    assert_eq!(cpu.regs.ecx, 0);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 19. REP SCASB with paging (REPE — scan until mismatch)
// ========================================================================
#[test]
fn test_paging_repe_scasb() {
    // F3 AE = REPE SCASB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAE]);
    // Fill with 0x42, then put 0xFF at position 5
    for i in 0..8 {
        mem.ram[0x5000 + i] = 0x42;
    }
    mem.ram[0x5005] = 0xFF;
    cpu.regs.set_al(0x42);
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    // Should stop after finding mismatch at position 5
    assert_eq!(cpu.regs.ecx, 2); // 8 - 6 = 2 remaining
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 20. REPNE SCASB with paging (scan for match)
// ========================================================================
#[test]
fn test_paging_repne_scasb() {
    // F2 AE = REPNE SCASB, F4 = HLT
    let (mut cpu, mut mem) = setup_paging(&[0xF2, 0xAE, 0xF4]);
    for i in 0..8 {
        mem.ram[0x5000 + i] = (0x10 + i) as u8;
    }
    // Searching for 0x13 (at position 3)
    cpu.regs.set_al(0x13);
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(matches!(result, RunResult::Halt { .. }), "got {:?}", result);
    // Should find match at position 3, then advance past it
    assert_eq!(cpu.regs.ecx, 4); // 8 - 4 = 4 remaining
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.edi, 0x5004);
}

// ========================================================================
// 21. REP SCASW with paging (word scan)
// ========================================================================
#[test]
fn test_paging_repne_scasw() {
    // F2 66 AF = REPNE SCASW, F4 = HLT
    let (mut cpu, mut mem) = setup_paging(&[0xF2, 0x66, 0xAF, 0xF4]);
    for i in 0..4u16 {
        let addr = 0x5000 + (i as usize) * 2;
        mem.ram[addr] = (i + 1) as u8;
        mem.ram[addr + 1] = 0x00;
    }
    // Search for word 0x0003
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF_0000) | 0x0003;
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(matches!(result, RunResult::Halt { .. }), "got {:?}", result);
    // Match at position 2 (word 0x0003)
    assert_eq!(cpu.regs.ecx, 1); // 4 - 3 = 1
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 22. REP SCASB with paging, DF=1 (backward scan)
// ========================================================================
#[test]
fn test_paging_repne_scasb_backward() {
    // STD; REPNE SCASB; HLT = FD F2 AE F4
    let (mut cpu, mut mem) = setup_paging(&[0xFD, 0xF2, 0xAE, 0xF4]);
    for i in 0..8 {
        mem.ram[0x5000 + i] = 0x00;
    }
    mem.ram[0x5002] = 0xAA; // target at position 2
    cpu.regs.set_al(0xAA);
    cpu.regs.edi = 0x5007; // start from end
    cpu.regs.ecx = 8;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(matches!(result, RunResult::Halt { .. }), "got {:?}", result);
    // Should find 0xAA at 0x5002 (scanned backward from 0x5007)
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.ecx, 2); // 8 - 6 = 2 remaining
}

// ========================================================================
// 23. REP MOVSB with paging, non-identity page mapping
// ========================================================================
#[test]
fn test_paging_rep_movsb_nonidentity() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);
    // Remap page 3 -> phys 0x50, page 5 -> phys 0x70
    mem.write_u32_physical(0x11000 + 3 * 4, (0x50 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 5 * 4, (0x70 << 12) | 0x07);
    for i in 0..16u8 {
        mem.ram[0x50000 + i as usize] = 0xC0 + i; // phys page 0x50
    }
    cpu.regs.esi = 0x3000; // linear -> phys 0x50000
    cpu.regs.edi = 0x5000; // linear -> phys 0x70000
    cpu.regs.ecx = 16;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..16u8 {
        assert_eq!(mem.ram[0x70000 + i as usize], 0xC0 + i);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 24. REP MOVSB crossing page boundary with paging
// ========================================================================
#[test]
fn test_paging_rep_movsb_page_crossing() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);
    // Source: straddle page 2/3 boundary (0x2FFC-0x3003)
    for i in 0..8u8 {
        mem.ram[0x2FFC + i as usize] = 0xE0 + i;
    }
    cpu.regs.esi = 0x2FFC;
    cpu.regs.edi = 0x6000;
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..8u8 {
        assert_eq!(mem.ram[0x6000 + i as usize], 0xE0 + i);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 25. CALL FAR 32-bit with paging (pushes CS:EIP through page tables)
// ========================================================================
#[test]
fn test_paging_call_far_32bit() {
    // CALL FAR 0x0008:0x00005000 = 9A 00 50 00 00 08 00
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0x9A, 0x00, 0x50, 0x00, 0x00, 0x08, 0x00]);
    // Place a HLT at the target
    mem.ram[0x5000] = 0xF4;
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    // CALL FAR pushes CS:EIP (8 bytes for 32-bit), then jumps to 0x5000
    assert!(
        matches!(result, RunResult::Halt { .. }),
        "Expected HLT at 0x5000, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, 0x5001);
    // ESP should have decremented by 8 (two 32-bit pushes: CS, return EIP)
    assert_eq!(cpu.regs.esp, old_esp - 8);
}

// ========================================================================
// 26. CALL FAR 16-bit operand with paging
// ========================================================================
#[test]
fn test_paging_call_far_16bit() {
    // 66 prefix in 32-bit mode switches to 16-bit operand size
    // 66 9A offset16 selector16 = CALL FAR 0x0008:0x5000
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0x66, 0x9A, 0x00, 0x50, 0x08, 0x00]);
    mem.ram[0x5000] = 0xF4; // HLT at target
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::Halt { .. }),
        "Expected HLT at 0x5000, got {:?}",
        result
    );
    // 16-bit operand pushes two 16-bit values (4 bytes total)
    assert_eq!(cpu.regs.esp, old_esp - 4);
}

// ========================================================================
// 27. RET FAR (0xCB) with paging — 32-bit operand
// ========================================================================
#[test]
fn test_paging_retf_32bit() {
    // CB = RETF (32-bit)
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCB]);
    // Push a return frame: CS=0x08, EIP=0x5000 on the stack
    let esp = cpu.regs.esp;
    // EIP at [ESP]
    mem.ram[esp as usize - 8] = 0x00;
    mem.ram[esp as usize - 7] = 0x50;
    mem.ram[esp as usize - 6] = 0x00;
    mem.ram[esp as usize - 5] = 0x00;
    // CS at [ESP+4]
    mem.ram[esp as usize - 4] = 0x08;
    mem.ram[esp as usize - 3] = 0x00;
    mem.ram[esp as usize - 2] = 0x00;
    mem.ram[esp as usize - 1] = 0x00;
    cpu.regs.esp = esp - 8;
    cpu.regs.set_sp((esp - 8) as u16);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Retf { .. }),
        "Expected Retf, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, 0x5000);
    assert_eq!(cpu.regs.cs, 0x08);
    assert_eq!(cpu.regs.esp, esp);
}

// ========================================================================
// 28. RET FAR with imm16 (0xCA) with paging
// ========================================================================
#[test]
fn test_paging_retf_imm16_32bit() {
    // CA 08 00 = RETF 8 (32-bit, pop 8 extra bytes)
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCA, 0x08, 0x00]);
    let esp = cpu.regs.esp;
    // Push return frame
    mem.ram[esp as usize - 8] = 0x00;
    mem.ram[esp as usize - 7] = 0x40;
    mem.ram[esp as usize - 6] = 0x00;
    mem.ram[esp as usize - 5] = 0x00;
    mem.ram[esp as usize - 4] = 0x08;
    mem.ram[esp as usize - 3] = 0x00;
    mem.ram[esp as usize - 2] = 0x00;
    mem.ram[esp as usize - 1] = 0x00;
    cpu.regs.esp = esp - 8;
    cpu.regs.set_sp((esp - 8) as u16);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Retf { .. }),
        "Expected Retf, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, 0x4000);
    assert_eq!(cpu.regs.cs, 0x08);
    // ESP = original - 8 + 8 (pop) + 8 (imm) = original + 8
    assert_eq!(cpu.regs.esp, esp + 8);
}

// a far RETF to an OUTER ring whose new SS descriptor lives
// on a not-present page must deliver #PF(14) carrying CR2 = the descriptor linear
// address -- NOT the pre-fix flatten of #SS(12) with no fault address. This pins
// the PageFault arm of the four op_retf SS-load sites (the #GP arm is pinned by
// the null-SS RETF tests in the sibling module).
#[test]
fn test_paging_retf32_priv_transition_ss_descriptor_pagefault_is_pf() {
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCB]); // RETF32 at linear 0x0100

    // Ring-3 CODE descriptor (present, DPL=3) at GDT index 0xA -> selector 0x53,
    // on the present GDT page (linear 0x20050).
    write_gdt_entry(
        &mut mem,
        0x20000 + 0x50,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x0C),
    );
    let ring3_cs: u32 = 0x50 | 3; // 0x53

    // Ring-3 SS descriptor (writable data, DPL=3, present) placed on a SEPARATE
    // page: GDT index 0x200 -> linear 0x20000 + 0x200*8 = 0x21000 (page 0x21).
    write_gdt_entry(&mut mem, 0x21000, make_descriptor(0, 0xFFFFF, 0xF2, 0x0C));
    let ring3_ss: u32 = (0x200 << 3) | 3; // 0x1003
    cpu.regs.gdtr.limit = 0x1007; // cover index 0x200

    // Make the SS descriptor's page (0x21) NOT present, so load_ss's descriptor
    // read page-faults (identity PTE at 0x11000 + page*4; clear the present bit).
    mem.write_u32_physical(0x11000 + 0x21 * 4, (0x21 << 12) | 0x06);

    // Lay a privilege-changing RETF32 return frame on the ring-0 stack (ESP=0x8000):
    // [esp]=return EIP, [esp+4]=ring-3 CS, [esp+8]=outer ESP, [esp+12]=ring-3 SS.
    let esp: u32 = 0x7FF0;
    mem.ram[esp as usize..esp as usize + 4].copy_from_slice(&0x0000_5000u32.to_le_bytes());
    mem.ram[esp as usize + 4..esp as usize + 8].copy_from_slice(&ring3_cs.to_le_bytes());
    mem.ram[esp as usize + 8..esp as usize + 12].copy_from_slice(&0x0000_6000u32.to_le_bytes());
    mem.ram[esp as usize + 12..esp as usize + 16].copy_from_slice(&ring3_ss.to_le_bytes());
    cpu.regs.esp = esp;
    cpu.regs.set_sp(esp as u16);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 14,
            fault_addr: Some(cr2),
            error_code,
            ..
        } => {
            assert_eq!(
                cr2 & 0xFFFF_F000,
                0x21000,
                "CR2 must point at the not-present SS-descriptor page"
            );
            assert_eq!(error_code & 1, 0, "page-not-present #PF error code (P=0)");
        }
        other => panic!("RETF SS-descriptor #PF must be vector 14 with CR2, got {other:?}"),
    }
}

// the direct SS-load opcodes share the RETF fault
// contract, but each has its own interpreter arm. Keep the descriptor page
// absent so the fault must come from load_ss's page walk, not from a bad
// selector or an already-cached descriptor.
const PAGING_SS_DESCRIPTOR_PAGE: u32 = 0x21000;
const PAGING_SS_DESCRIPTOR_SELECTOR: u16 = 0x1000;
const NOT_PRESENT_SS_SELECTOR: u16 = 0x0040;

fn setup_paged_ss_descriptor_fault(code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mut mem) = setup_paging_with_gdt(code);
    write_gdt_entry(
        &mut mem,
        PAGING_SS_DESCRIPTOR_PAGE as usize,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    cpu.regs.gdtr.limit = 0x1007;
    // Clear PTE.present for linear page 0x21 while retaining the descriptor
    // bytes in RAM. Any descriptor read must therefore raise #PF(14).
    mem.write_u32_physical(0x11000 + 0x21 * 4, (0x21 << 12) | 0x06);
    (cpu, mem)
}

fn setup_not_present_ss(code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mut mem) = setup_paging_with_gdt(code);
    // Slot 8 is on the present GDT page. It is a valid writable-data SS
    // descriptor except for P=0, which must map to #SS(12).
    write_gdt_entry(
        &mut mem,
        0x20000 + 0x40,
        make_descriptor(0, 0xFFFFF, 0x12, 0x0C),
    );
    cpu.regs.gdtr.limit = 0x47;
    (cpu, mem)
}

fn assert_direct_ss_descriptor_page_fault(result: RunResult) {
    match result {
        RunResult::Exception {
            vector: 14,
            error_code,
            fault_addr: Some(fault_addr),
            ..
        } => {
            assert_eq!(
                fault_addr, PAGING_SS_DESCRIPTOR_PAGE,
                "direct SS-load #PF must preserve the descriptor CR2"
            );
            assert_eq!(error_code & 1, 0, "descriptor page must report P=0");
        }
        other => {
            panic!("direct SS-load descriptor fault must be #PF(14) with CR2, got {other:?}")
        }
    }
}

fn assert_not_present_ss_fault(result: RunResult) {
    match result {
        RunResult::Exception {
            vector: 12,
            error_code,
            fault_addr: None,
            ..
        } => assert_eq!(
            error_code, NOT_PRESENT_SS_SELECTOR as u32,
            "#SS must carry the selector error code"
        ),
        other => panic!("not-present direct SS load must be #SS(12), got {other:?}"),
    }
}

#[test]
fn test_paging_mov_ss_descriptor_pagefault_is_pf() {
    let (mut cpu, mut mem) = setup_paged_ss_descriptor_fault(&[0x8E, 0xD0]);
    cpu.regs.eax = PAGING_SS_DESCRIPTOR_SELECTOR as u32;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_direct_ss_descriptor_page_fault(result);
    assert_eq!(cpu.regs.ss, 0x18, "MOV SS must not publish a failed load");
}

#[test]
fn test_paging_pop_ss_descriptor_pagefault_is_pf() {
    let (mut cpu, mut mem) = setup_paged_ss_descriptor_fault(&[0x17]);
    let saved_esp = cpu.regs.esp;
    mem.ram[saved_esp as usize..saved_esp as usize + 4]
        .copy_from_slice(&(PAGING_SS_DESCRIPTOR_SELECTOR as u32).to_le_bytes());
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_direct_ss_descriptor_page_fault(result);
    assert_eq!(cpu.regs.esp, saved_esp, "POP SS must restore ESP after #PF");
    assert_eq!(cpu.regs.ss, 0x18, "POP SS must not publish a failed load");
}

#[test]
fn test_paging_lss_descriptor_pagefault_is_pf() {
    // LSS EAX, [0x6000] = 0F B2 05 00 60 00 00.
    let (mut cpu, mut mem) =
        setup_paged_ss_descriptor_fault(&[0x0F, 0xB2, 0x05, 0x00, 0x60, 0x00, 0x00]);
    mem.ram[0x6000..0x6004].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    mem.ram[0x6004..0x6006].copy_from_slice(&PAGING_SS_DESCRIPTOR_SELECTOR.to_le_bytes());
    cpu.regs.eax = 0xCAFE_BABE;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_direct_ss_descriptor_page_fault(result);
    assert_eq!(cpu.regs.eax, 0xCAFE_BABE, "LSS must restore EAX after #PF");
    assert_eq!(cpu.regs.ss, 0x18, "LSS must not publish a failed load");
}

#[test]
fn test_lss_not_present_ss_is_ss_fault() {
    // LSS EAX, [0x6000] = 0F B2 05 00 60 00 00.
    let (mut cpu, mut mem) = setup_not_present_ss(&[0x0F, 0xB2, 0x05, 0x00, 0x60, 0x00, 0x00]);
    mem.ram[0x6000..0x6004].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    mem.ram[0x6004..0x6006].copy_from_slice(&NOT_PRESENT_SS_SELECTOR.to_le_bytes());
    cpu.regs.eax = 0xCAFE_BABE;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_not_present_ss_fault(result);
    assert_eq!(cpu.regs.eax, 0xCAFE_BABE, "LSS must restore EAX after #SS");
    assert_eq!(cpu.regs.ss, 0x18, "LSS must not publish a failed load");
}

#[test]
fn test_pop_ss_not_present_is_ss_fault() {
    let (mut cpu, mut mem) = setup_not_present_ss(&[0x17]);
    let saved_esp = cpu.regs.esp;
    mem.ram[saved_esp as usize..saved_esp as usize + 4]
        .copy_from_slice(&(NOT_PRESENT_SS_SELECTOR as u32).to_le_bytes());
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_not_present_ss_fault(result);
    assert_eq!(cpu.regs.esp, saved_esp, "POP SS must restore ESP after #SS");
    assert_eq!(cpu.regs.ss, 0x18, "POP SS must not publish a failed load");
}

fn assert_retf_stack_page_fault(result: RunResult, expected_addr: u32) {
    match result {
        RunResult::Exception {
            vector: 14,
            error_code,
            fault_addr: Some(fault_addr),
            ..
        } => {
            assert_eq!(fault_addr, expected_addr, "RETF must preserve CR2");
            assert_eq!(error_code & 1, 0, "the missing stack page must report P=0");
        }
        other => panic!("RETF stack page fault must be #PF(14), got {other:?}"),
    }
}

fn run_retf_base_cs_pop_page_fault(code: &[u8], operand_size_32: bool) {
    let (mut cpu, mut mem) = setup_paging_with_gdt(code);
    let saved_cs = cpu.regs.cs;
    let saved_cpl = cpu.regs.cpl;
    let saved_eip = cpu.regs.eip;
    let saved_esp = if operand_size_32 {
        0x0000_0FFC
    } else {
        // A 16-bit stack wraps through SP while preserving ESP[31:16].
        cpu.regs.ss_cache.big = false;
        0xABCD_0FFE
    };
    cpu.regs.esp = saved_esp;

    let stack_offset = saved_esp as u16 as usize;
    if operand_size_32 {
        mem.ram[stack_offset..stack_offset + 4].copy_from_slice(&0x0000_5000u32.to_le_bytes());
    } else {
        mem.ram[stack_offset..stack_offset + 2].copy_from_slice(&0x5000u16.to_le_bytes());
    }

    // The return offset is wholly present, but the following CS slot starts
    // on a missing page. The complete logical RETF frame must be atomic.
    mem.write_u32_physical(0x11000 + 4, (1 << 12) | 0x06);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_retf_stack_page_fault(result, 0x1000);
    assert_eq!(cpu.regs.esp, saved_esp, "RETF must restore full ESP");
    assert_eq!(cpu.regs.cs, saved_cs, "RETF must preserve CS");
    assert_eq!(cpu.regs.cpl, saved_cpl, "RETF must preserve CPL");
    assert_eq!(
        cpu.regs.eip, saved_eip,
        "RETF fault must restart at the instruction"
    );
}

#[test]
fn test_paging_retf16_base_cs_pop_pagefault_is_atomic() {
    run_retf_base_cs_pop_page_fault(&[0x66, 0xCB], false);
}

#[test]
fn test_paging_retf32_base_cs_pop_pagefault_is_atomic() {
    run_retf_base_cs_pop_page_fault(&[0xCB], true);
}

#[test]
fn test_paging_retf_imm16_base_cs_pop_pagefault_is_atomic() {
    run_retf_base_cs_pop_page_fault(&[0x66, 0xCA, 0x04, 0x00], false);
}

#[test]
fn test_paging_retf_imm32_base_cs_pop_pagefault_is_atomic() {
    run_retf_base_cs_pop_page_fault(&[0xCA, 0x08, 0x00], true);
}

fn run_retf_outer_ss_pop_page_fault(operand_size_32: bool) {
    let code: &[u8] = if operand_size_32 {
        &[0xCB]
    } else {
        &[0x66, 0xCB]
    };
    let (mut cpu, mut mem) = setup_paging_with_gdt(code);

    write_gdt_entry(
        &mut mem,
        0x20000 + 0x50,
        make_descriptor(0, 0xFFFFF, 0xFA, if operand_size_32 { 0x0C } else { 0x08 }),
    );
    cpu.regs.gdtr.limit = 0x57;
    let ring3_cs = 0x53u32;

    let saved_cs = cpu.regs.cs;
    let saved_cpl = cpu.regs.cpl;
    let saved_cs_bits = cpu.cs_bits;
    let saved_eip = cpu.regs.eip;
    let saved_esp = if operand_size_32 {
        0x0000_0FF4
    } else {
        cpu.regs.ss_cache.big = false;
        0xABCD_0FFA
    };
    cpu.regs.esp = saved_esp;
    let stack_offset = saved_esp as u16 as usize;

    if operand_size_32 {
        mem.ram[stack_offset..stack_offset + 4].copy_from_slice(&0x0000_5000u32.to_le_bytes());
        mem.ram[stack_offset + 4..stack_offset + 8].copy_from_slice(&ring3_cs.to_le_bytes());
        mem.ram[stack_offset + 8..stack_offset + 12].copy_from_slice(&0x0000_6000u32.to_le_bytes());
    } else {
        mem.ram[stack_offset..stack_offset + 2].copy_from_slice(&0x5000u16.to_le_bytes());
        mem.ram[stack_offset + 2..stack_offset + 4]
            .copy_from_slice(&(ring3_cs as u16).to_le_bytes());
        mem.ram[stack_offset + 4..stack_offset + 6].copy_from_slice(&0x6000u16.to_le_bytes());
    }

    // The first three fields are present. The outer SS is the only pop
    // that touches the missing page.
    mem.write_u32_physical(0x11000 + 4, (1 << 12) | 0x06);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_retf_stack_page_fault(result, 0x1000);
    assert_eq!(cpu.regs.esp, saved_esp, "RETF must restore full ESP");
    assert_eq!(cpu.regs.cs, saved_cs, "RETF must restore inner CS");
    assert_eq!(cpu.regs.cpl, saved_cpl, "RETF must restore inner CPL");
    assert_eq!(
        cpu.cs_bits, saved_cs_bits,
        "RETF must restore inner CS width"
    );
    assert_eq!(
        cpu.regs.eip, saved_eip,
        "RETF fault must restart at the instruction"
    );
}

#[test]
fn test_paging_retf16_outer_ss_pop_pagefault_is_atomic() {
    run_retf_outer_ss_pop_page_fault(false);
}

#[test]
fn test_paging_retf32_outer_ss_pop_pagefault_is_atomic() {
    run_retf_outer_ss_pop_page_fault(true);
}

fn install_missing_far_target_descriptor(cpu: &mut Cpu, mem: &mut Memory) -> u16 {
    // Put a valid ring-0 code descriptor at GDT index 0x200, then make its
    // page non-present. Far-control validation must report the page walk,
    // not reinterpret it as a selector fault.
    write_gdt_entry(mem, 0x21000, make_descriptor(0, 0xFFFFF, 0x9A, 0x0C));
    cpu.regs.gdtr.limit = 0x1007;
    mem.write_u32_physical(0x11000 + 0x21 * 4, (0x21 << 12) | 0x06);
    0x1000
}

fn assert_far_descriptor_page_fault(result: RunResult, cpu: &Cpu, saved_esp: u32, operation: &str) {
    match result {
        RunResult::Exception {
            vector: 14,
            error_code,
            fault_addr: Some(fault_addr),
            ..
        } => {
            assert_eq!(fault_addr, 0x21000, "{operation} must preserve CR2");
            assert_eq!(error_code & 1, 0, "{operation} must report P=0");
        }
        other => panic!("{operation} descriptor page fault must be #PF(14), got {other:?}"),
    }
    assert_eq!(cpu.regs.cs, 0x08, "{operation} must preserve CS");
    assert_eq!(
        cpu.regs.eip, 0x0100,
        "{operation} must restart at the faulting instruction"
    );
    assert_eq!(
        cpu.regs.esp, saved_esp,
        "{operation} must preserve the entry stack pointer"
    );
}

#[test]
fn test_paging_retf_cs_descriptor_pagefault_is_pf() {
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCB]);
    let selector = install_missing_far_target_descriptor(&mut cpu, &mut mem);
    let saved_esp = 0x7FF8;
    cpu.regs.esp = saved_esp;
    mem.ram[saved_esp as usize..saved_esp as usize + 4]
        .copy_from_slice(&0x0000_5000u32.to_le_bytes());
    mem.ram[saved_esp as usize + 4..saved_esp as usize + 8]
        .copy_from_slice(&(selector as u32).to_le_bytes());

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_far_descriptor_page_fault(result, &cpu, saved_esp, "RETF");
}

#[test]
fn test_paging_far_call_immediate_descriptor_pagefault_is_pf() {
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0x9A, 0x00, 0x50, 0x00, 0x00, 0x00, 0x10]);
    install_missing_far_target_descriptor(&mut cpu, &mut mem);
    let saved_esp = cpu.regs.esp;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_far_descriptor_page_fault(result, &cpu, saved_esp, "immediate far CALL");
}

#[test]
fn test_paging_far_call_indirect_descriptor_pagefault_is_pf() {
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xFF, 0x1D, 0x00, 0x50, 0x00, 0x00]);
    let selector = install_missing_far_target_descriptor(&mut cpu, &mut mem);
    mem.ram[0x5000..0x5004].copy_from_slice(&0x0000_5000u32.to_le_bytes());
    mem.ram[0x5004..0x5006].copy_from_slice(&selector.to_le_bytes());
    let saved_esp = cpu.regs.esp;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_far_descriptor_page_fault(result, &cpu, saved_esp, "indirect far CALL");
}

#[test]
fn test_paging_far_jmp_immediate_descriptor_pagefault_is_pf() {
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xEA, 0x00, 0x50, 0x00, 0x00, 0x00, 0x10]);
    install_missing_far_target_descriptor(&mut cpu, &mut mem);
    let saved_esp = cpu.regs.esp;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_far_descriptor_page_fault(result, &cpu, saved_esp, "immediate far JMP");
}

#[test]
fn test_paging_far_jmp_indirect_descriptor_pagefault_is_pf() {
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xFF, 0x2D, 0x00, 0x50, 0x00, 0x00]);
    let selector = install_missing_far_target_descriptor(&mut cpu, &mut mem);
    mem.ram[0x5000..0x5004].copy_from_slice(&0x0000_5000u32.to_le_bytes());
    mem.ram[0x5004..0x5006].copy_from_slice(&selector.to_le_bytes());
    let saved_esp = cpu.regs.esp;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert_far_descriptor_page_fault(result, &cpu, saved_esp, "indirect far JMP");
}

// ========================================================================
// 29. INT n in paged PM (validates IDT, returns SoftwareInterrupt)
// ========================================================================
#[test]
fn test_paging_int_n_pm() {
    // CD 20 = INT 0x20
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCD, 0x20]);
    // Set up IDT gate at vector 0x20
    // 8-byte IDT entry: offset_low(2), selector(2), zero(1), type_attr(1), offset_high(2)
    let idt_addr = 0x30000 + 0x20 * 8;
    // Present, DPL=0, 32-bit interrupt gate (type=0x0E)
    let type_attr: u8 = 0x8E; // P=1, DPL=0, type=0E
    mem.ram[idt_addr] = 0x00; // offset low
    mem.ram[idt_addr + 1] = 0x60;
    mem.ram[idt_addr + 2] = 0x08; // selector = 0x08
    mem.ram[idt_addr + 3] = 0x00;
    mem.ram[idt_addr + 4] = 0x00; // zero
    mem.ram[idt_addr + 5] = type_attr;
    mem.ram[idt_addr + 6] = 0x00; // offset high
    mem.ram[idt_addr + 7] = 0x00;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::SoftwareInterrupt { vector: 0x20, .. }),
        "Expected SoftwareInterrupt(0x20), got {:?}",
        result
    );
}

#[test]
fn test_pm_int_n_idt_type_attr_uses_paged_idtr_base() {
    // CD 2D = INT 0x2D, with IDTR.base in NT-style high linear memory.
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCD, 0x2D]);
    cpu.regs.idtr.base = HIGH_IDT_BASE;
    cpu.regs.idtr.limit = 0x07FF;
    map_high_idt_page(&mut mem, false);
    write_test_idt_gate(&mut mem, high_idt_gate_phys(0x2D), 0x8E);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::SoftwareInterrupt { vector: 0x2D, .. }),
        "Expected SoftwareInterrupt(0x2D), got {:?}",
        result
    );
}

#[test]
fn test_pm_int_n_idt_type_attr_unmapped_returns_page_fault() {
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCD, 0x2D]);
    cpu.regs.idtr.base = HIGH_IDT_BASE;
    cpu.regs.idtr.limit = 0x07FF;
    let saved_eip = cpu.regs.eip;
    let fault_linear = HIGH_IDT_BASE + 0x2D * 8 + 5;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector,
            error_code,
            fault_addr,
            ..
        } => {
            assert_eq!(vector, 14);
            assert_eq!(error_code, 0, "expected supervisor read not-present #PF");
            assert_eq!(fault_addr, Some(fault_linear));
            assert_eq!(
                cpu.regs.eip, saved_eip,
                "IDT byte page faults must leave EIP on the INT opcode"
            );
        }
        other => panic!("Expected #PF for unmapped IDT type byte, got {other:?}"),
    }
}

// ========================================================================
// 30. INT n with missing IDT gate (GP fault)
// ========================================================================
#[test]
fn test_paging_int_n_not_present() {
    // CD 30 = INT 0x30
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCD, 0x30]);
    let saved_eip = cpu.regs.eip;
    // IDT gate at vector 0x30: NOT present (type_attr P=0)
    let idt_addr = 0x30000 + 0x30 * 8;
    mem.ram[idt_addr + 5] = 0x0E; // P=0, DPL=0, type=0E
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "Expected GP fault for not-present IDT gate, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eip, saved_eip,
        "PM INT n gate faults must leave EIP on the INT opcode"
    );
}

#[test]
fn test_pm_int_gate_dpl_fault_preserves_faulting_ip() {
    // CD 41 = INT 0x41 from CPL=3 through a DPL=0 gate.
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCD, 0x41]);
    cpu.regs.cpl = 3;
    cpu.regs.cs = 0x1B;
    cpu.regs.cs_cache.base = 0;
    cpu.regs.cs_cache.limit = 0xFFFFF;
    cpu.regs.cs_cache.granularity = true;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0xFA;
    cpu.regs.cs_cache.dpl = 3;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = true;
    cpu.regs.cs_cache.present = true;
    let saved_eip = cpu.regs.eip;

    cpu.regs.idtr.base = 0x30000;
    cpu.regs.idtr.limit = 0x07FF;
    let idt_addr = 0x30000 + 0x41 * 8;
    mem.ram[idt_addr] = 0x34;
    mem.ram[idt_addr + 1] = 0x12;
    mem.ram[idt_addr + 2] = 0x08;
    mem.ram[idt_addr + 3] = 0x00;
    mem.ram[idt_addr + 4] = 0x00;
    mem.ram[idt_addr + 5] = 0x8E; // present DPL=0 32-bit interrupt gate
    mem.ram[idt_addr + 6] = 0x00;
    mem.ram[idt_addr + 7] = 0x00;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector: 13,
            error_code,
            ..
        } => {
            assert_eq!(error_code, (0x41u32 * 8) + 2);
            assert_eq!(
                cpu.regs.eip, saved_eip,
                "PM software INT gate-DPL #GP must leave EIP on the INT opcode"
            );
        }
        other => panic!("Expected #GP for software INT gate-DPL fault, got {other:?}"),
    }
}

#[test]
fn test_pm_int_n_idt_type_attr_supervisor_read_at_cpl3() {
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCD, 0x2D]);
    enter_ring3_for_int_test(&mut cpu);
    cpu.regs.idtr.base = HIGH_IDT_BASE;
    cpu.regs.idtr.limit = 0x07FF;
    map_high_idt_page(&mut mem, false);
    write_test_idt_gate(&mut mem, high_idt_gate_phys(0x2D), 0xEE);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::SoftwareInterrupt { vector: 0x2D, .. }),
        "CPL3 INT through supervisor-only IDT page should precheck successfully, got {:?}",
        result
    );
}

#[test]
fn test_pm_int_n_gate_dpl_fault_uses_translated_type_attr() {
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCD, 0x2D]);
    enter_ring3_for_int_test(&mut cpu);
    cpu.regs.idtr.base = HIGH_IDT_BASE;
    cpu.regs.idtr.limit = 0x07FF;
    map_high_idt_page(&mut mem, false);
    write_test_idt_gate(&mut mem, high_idt_gate_phys(0x2D), 0x8E);
    let saved_eip = cpu.regs.eip;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector, error_code, ..
        } => {
            assert_eq!(vector, 13);
            assert_eq!(error_code, (0x2Du32 * 8) + 2);
            assert_eq!(
                cpu.regs.eip, saved_eip,
                "PM software INT gate-DPL #GP must leave EIP on the INT opcode"
            );
        }
        other => {
            panic!("Expected #GP for translated software INT gate-DPL fault, got {other:?}")
        }
    }
}

// ========================================================================
// 31. IRET in paged PM (32-bit IRETD returns Iret)
// ========================================================================
#[test]
fn test_paging_iretd() {
    // CF = IRET (32-bit = IRETD)
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0xCF]);
    let esp = cpu.regs.esp;
    // Push an IRET frame: EIP, CS, EFLAGS (3 dwords = 12 bytes)
    // EFLAGS at [ESP+8]
    let eflags: u32 = 0x0000_0202; // IF=1, reserved bit 1
    let frame_esp = esp - 12;
    mem.ram[frame_esp as usize..frame_esp as usize + 4].copy_from_slice(&0x5000u32.to_le_bytes()); // EIP=0x5000
    mem.ram[frame_esp as usize + 4..frame_esp as usize + 8]
        .copy_from_slice(&0x0008u32.to_le_bytes()); // CS=0x08
    mem.ram[frame_esp as usize + 8..frame_esp as usize + 12].copy_from_slice(&eflags.to_le_bytes()); // EFLAGS
    cpu.regs.esp = frame_esp;
    cpu.regs.set_sp(frame_esp as u16);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Iret { .. }),
        "Expected Iret, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, 0x5000);
    assert_eq!(cpu.regs.cs, 0x08);
}

// ========================================================================
// 32. IRET 16-bit in paged PM
// ========================================================================
#[test]
fn test_paging_iret_16bit() {
    // 66 CF = IRET 16-bit (operand size override in 32-bit mode)
    let (mut cpu, mut mem) = setup_paging_with_gdt(&[0x66, 0xCF]);
    let esp = cpu.regs.esp;
    // Push a 16-bit IRET frame: IP(2), CS(2), FLAGS(2)
    let frame_esp = esp - 6;
    mem.ram[frame_esp as usize] = 0x00; // IP low
    mem.ram[frame_esp as usize + 1] = 0x40; // IP high
    mem.ram[frame_esp as usize + 2] = 0x08; // CS low
    mem.ram[frame_esp as usize + 3] = 0x00; // CS high
    mem.ram[frame_esp as usize + 4] = 0x02; // FLAGS low (reserved bit 1)
    mem.ram[frame_esp as usize + 5] = 0x02; // FLAGS high (IF=1)
    cpu.regs.esp = frame_esp;
    cpu.regs.set_sp(frame_esp as u16);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Iret { .. }),
        "Expected Iret, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, 0x4000);
    assert_eq!(cpu.regs.cs, 0x08);
}

// ========================================================================
// 33. LES in protected mode (load ES:reg from memory)
// ========================================================================
#[test]
fn test_les_pm32() {
    // LES EAX, [0x3000] = C4 05 00 30 00 00
    // ModR/M: 05 = disp32, reg=EAX
    let (mut cpu, mut mem) = setup_pm32(&[0xC4, 0x05, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000..0x3004].copy_from_slice(&0x1234u32.to_le_bytes());
    mem.ram[0x3004..0x3006].copy_from_slice(&0x10u16.to_le_bytes()); // selector 0x10
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x1234);
    assert_eq!(cpu.regs.es, 0x10);
}

// ========================================================================
// 34. LDS in protected mode (load DS:reg from memory)
// ========================================================================
#[test]
fn test_lds_pm32() {
    // LDS EBX, [0x3000] = C5 1D 00 30 00 00
    // ModR/M: 1D = disp32, reg=EBX
    let (mut cpu, mut mem) = setup_pm32(&[0xC5, 0x1D, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000..0x3004].copy_from_slice(&0x5678u32.to_le_bytes());
    mem.ram[0x3004..0x3006].copy_from_slice(&0x10u16.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ebx, 0x5678);
    assert_eq!(cpu.regs.ds, 0x10);
}

// ========================================================================
// 35. LFS in protected mode
// ========================================================================
#[test]
fn test_lfs_pm32() {
    // LFS ECX, [0x3000] = 0F B4 0D 00 30 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB4, 0x0D, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000..0x3004].copy_from_slice(&0xABCDu32.to_le_bytes());
    mem.ram[0x3004..0x3006].copy_from_slice(&0x10u16.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0xABCD);
    assert_eq!(cpu.regs.fs, 0x10);
}

// ========================================================================
// 36. LGS in protected mode
// ========================================================================
#[test]
fn test_lgs_pm32() {
    // LGS EDX, [0x3000] = 0F B5 15 00 30 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB5, 0x15, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000..0x3004].copy_from_slice(&0x9999u32.to_le_bytes());
    mem.ram[0x3004..0x3006].copy_from_slice(&0x10u16.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edx, 0x9999);
    assert_eq!(cpu.regs.gs, 0x10);
}

// ========================================================================
// 37. LSS in protected mode (load SS:reg)
// ========================================================================
#[test]
fn test_lss_pm32() {
    // LSS ESP, [0x3000] = 0F B2 25 00 30 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB2, 0x25, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000..0x3004].copy_from_slice(&0x9000u32.to_le_bytes());
    mem.ram[0x3004..0x3006].copy_from_slice(&0x10u16.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esp, 0x9000);
    assert_eq!(cpu.regs.ss, 0x10);
}

// ========================================================================
// 38. MOV to segment register (DS) in PM
// ========================================================================
#[test]
fn test_mov_ds_pm32() {
    // MOV DS, AX = 8E D8
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xD8]);
    cpu.regs.eax = 0x10; // valid data selector
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ds, 0x10);
}

// ========================================================================
// 39. MOV to segment register (ES) in PM
// ========================================================================
#[test]
fn test_mov_es_pm32() {
    // MOV ES, AX = 8E C0
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xC0]);
    cpu.regs.eax = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.es, 0x10);
}

// ========================================================================
// 40. MOV to segment register (FS) in PM
// ========================================================================
#[test]
fn test_mov_fs_pm32() {
    // MOV FS, AX = 8E E0
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xE0]);
    cpu.regs.eax = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.fs, 0x10);
}

// ========================================================================
// 41. MOV to segment register (GS) in PM
// ========================================================================
#[test]
fn test_mov_gs_pm32() {
    // MOV GS, AX = 8E E8
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xE8]);
    cpu.regs.eax = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.gs, 0x10);
}

// ========================================================================
// 42. MOV to SS in PM (with interrupt inhibit)
// ========================================================================
#[test]
fn test_mov_ss_pm32() {
    // MOV SS, AX = 8E D0
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xD0]);
    cpu.regs.eax = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ss, 0x10);
}

// ========================================================================
// 43. POP DS in protected mode
// ========================================================================
#[test]
fn test_pop_ds_pm32() {
    // POP DS = 1F
    let (mut cpu, mut mem) = setup_pm32(&[0x1F]);
    let esp = cpu.regs.esp;
    // Push selector 0x10 on the stack
    mem.ram[esp as usize - 4..esp as usize].copy_from_slice(&0x10u32.to_le_bytes());
    cpu.regs.esp = esp - 4;
    cpu.regs.set_sp((esp - 4) as u16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ds, 0x10);
    assert_eq!(cpu.regs.esp, esp);
}

// ========================================================================
// 44. POP ES in protected mode
// ========================================================================
#[test]
fn test_pop_es_pm32() {
    // POP ES = 07
    let (mut cpu, mut mem) = setup_pm32(&[0x07]);
    let esp = cpu.regs.esp;
    mem.ram[esp as usize - 4..esp as usize].copy_from_slice(&0x10u32.to_le_bytes());
    cpu.regs.esp = esp - 4;
    cpu.regs.set_sp((esp - 4) as u16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.es, 0x10);
    assert_eq!(cpu.regs.esp, esp);
}

// ========================================================================
// 45. POP SS in protected mode
// ========================================================================
#[test]
fn test_pop_ss_pm32() {
    // POP SS = 17
    let (mut cpu, mut mem) = setup_pm32(&[0x17]);
    let esp = cpu.regs.esp;
    mem.ram[esp as usize - 4..esp as usize].copy_from_slice(&0x10u32.to_le_bytes());
    cpu.regs.esp = esp - 4;
    cpu.regs.set_sp((esp - 4) as u16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ss, 0x10);
    assert_eq!(cpu.regs.esp, esp);
}

// ========================================================================
// 46. POP FS in protected mode
// ========================================================================
#[test]
fn test_pop_fs_pm32() {
    // POP FS = 0F A1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xA1]);
    let esp = cpu.regs.esp;
    mem.ram[esp as usize - 4..esp as usize].copy_from_slice(&0x10u32.to_le_bytes());
    cpu.regs.esp = esp - 4;
    cpu.regs.set_sp((esp - 4) as u16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.fs, 0x10);
}

// ========================================================================
// 47. POP GS in protected mode
// ========================================================================
#[test]
fn test_pop_gs_pm32() {
    // POP GS = 0F A9
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xA9]);
    let esp = cpu.regs.esp;
    mem.ram[esp as usize - 4..esp as usize].copy_from_slice(&0x10u32.to_le_bytes());
    cpu.regs.esp = esp - 4;
    cpu.regs.set_sp((esp - 4) as u16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.gs, 0x10);
}

// ========================================================================
// 48. PUSHAD with paging (32-bit stack through page translation)
// ========================================================================
#[test]
fn test_paging_pushad_nonidentity() {
    // PUSHAD = 60
    let (mut cpu, mut mem) = setup_paging(&[0x60]);
    // Remap stack page 7 -> phys page 0x40
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);
    cpu.regs.esp = 0x8000; // page 7
    cpu.regs.set_sp(0x8000);
    cpu.regs.eax = 0x11111111;
    cpu.regs.ecx = 0x22222222;
    cpu.regs.edx = 0x33333333;
    cpu.regs.ebx = 0x44444444;
    cpu.regs.ebp = 0x55555555;
    cpu.regs.esi = 0x66666666;
    cpu.regs.edi = 0x77777777;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "PUSHAD with paging should succeed, got {:?}",
        result
    );
    // ESP decremented by 32
    assert_eq!(cpu.regs.esp, 0x7FE0);
    // Verify values written to remapped physical page (phys page 0x40)
    // PUSHAD order: EAX, ECX, EDX, EBX, original ESP, EBP, ESI, EDI
    let base_phys = 0x40000 + 0xFE0; // phys 0x40FE0
    let read_dw = |off: usize| -> u32 {
        u32::from_le_bytes([
            mem.ram[base_phys + off],
            mem.ram[base_phys + off + 1],
            mem.ram[base_phys + off + 2],
            mem.ram[base_phys + off + 3],
        ])
    };
    assert_eq!(read_dw(28), 0x11111111); // EAX at highest address
    assert_eq!(read_dw(0), 0x77777777); // EDI at lowest address
}

// ========================================================================
// 49. POPAD with paging
// ========================================================================
#[test]
fn test_paging_popad_nonidentity() {
    // POPAD = 61
    let (mut cpu, mut mem) = setup_paging(&[0x61]);
    // Remap stack page 7 -> phys page 0x40
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);
    cpu.regs.esp = 0x7FE0; // points to bottom of push frame
    cpu.regs.set_sp(0x7FE0);
    // Write POPAD frame to remapped physical memory
    let base_phys = 0x40000 + 0xFE0;
    let mut write_dw = |off: usize, val: u32| {
        let bytes = val.to_le_bytes();
        mem.ram[base_phys + off] = bytes[0];
        mem.ram[base_phys + off + 1] = bytes[1];
        mem.ram[base_phys + off + 2] = bytes[2];
        mem.ram[base_phys + off + 3] = bytes[3];
    };
    // POPAD pops in reverse order: EDI, ESI, EBP, (skip ESP), EBX, EDX, ECX, EAX
    write_dw(0, 0x77777777); // EDI
    write_dw(4, 0x66666666); // ESI
    write_dw(8, 0x55555555); // EBP
    write_dw(12, 0xAAAA_AAAA); // ESP (ignored by POPAD)
    write_dw(16, 0x44444444); // EBX
    write_dw(20, 0x33333333); // EDX
    write_dw(24, 0x22222222); // ECX
    write_dw(28, 0x11111111); // EAX
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "POPAD with paging should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax, 0x11111111);
    assert_eq!(cpu.regs.ecx, 0x22222222);
    assert_eq!(cpu.regs.edx, 0x33333333);
    assert_eq!(cpu.regs.ebx, 0x44444444);
    assert_eq!(cpu.regs.ebp, 0x55555555);
    assert_eq!(cpu.regs.esi, 0x66666666);
    assert_eq!(cpu.regs.edi, 0x77777777);
}

// ========================================================================
// 50. PUSH/POP with SS.big=1 (32-bit stack) in PM
// ========================================================================
#[test]
fn test_push_pop_big_stack_pm() {
    // PUSH EAX = 50, POP EBX = 5B
    let (mut cpu, mut mem) = setup_pm32(&[0x50, 0x5B]);
    cpu.regs.eax = 0xDEADBEEF;
    cpu.regs.ebx = 0;
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.ebx, 0xDEADBEEF);
    assert_eq!(cpu.regs.esp, old_esp);
}

// ========================================================================
// 51. PUSHFD/POPFD with paging
// ========================================================================
#[test]
fn test_paging_pushfd_popfd_nonidentity() {
    // PUSHFD = 9C, POPFD = 9D
    let (mut cpu, mut mem) = setup_paging(&[0x9C, 0x9D]);
    // Remap stack page
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.flags.insert(Flags::CARRY);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.esp, 0x8000);
}

// ========================================================================
// 52. PUSH imm32 with paging, non-identity stack
// ========================================================================
#[test]
fn test_paging_push_imm32_nonidentity() {
    // PUSH 0x12345678 = 68 78 56 34 12
    let (mut cpu, mut mem) = setup_paging(&[0x68, 0x78, 0x56, 0x34, 0x12]);
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esp, 0x7FFC);
    // Read from physical page 0x40
    let val = u32::from_le_bytes([
        mem.ram[0x40FFC],
        mem.ram[0x40FFD],
        mem.ram[0x40FFE],
        mem.ram[0x40FFF],
    ]);
    assert_eq!(val, 0x12345678);
}

// ========================================================================
// 53. PUSH imm8 with paging
// ========================================================================
#[test]
fn test_paging_push_imm8() {
    // PUSH 0x42 = 6A 42
    let (mut cpu, mut mem) = setup_paging(&[0x6A, 0x42]);
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esp, old_esp - 4);
    let val = u32::from_le_bytes(
        mem.ram[cpu.regs.esp as usize..cpu.regs.esp as usize + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(val, 0x42); // sign-extended to 32-bit
}

// ========================================================================
// 54. PUSH r32 / POP r32 with paging roundtrip
// ========================================================================
#[test]
fn test_paging_push_pop_r32_roundtrip() {
    // PUSH ECX = 51, PUSH EDX = 52, POP EAX = 58, POP EBX = 5B
    let (mut cpu, mut mem) = setup_paging(&[0x51, 0x52, 0x58, 0x5B]);
    cpu.regs.ecx = 0xAAAA_BBBB;
    cpu.regs.edx = 0xCCCC_DDDD;
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    // POP reverses: EAX gets EDX value, EBX gets ECX value
    assert_eq!(cpu.regs.eax, 0xCCCC_DDDD);
    assert_eq!(cpu.regs.ebx, 0xAAAA_BBBB);
    assert_eq!(cpu.regs.esp, old_esp);
}

// ========================================================================
// 55. CALL NEAR + RET with paging, non-identity stack
// ========================================================================
#[test]
fn test_paging_call_ret_nonidentity_stack() {
    // CALL +5 = E8 05 00 00 00 (5 bytes, target at 0x010A)
    // NOP NOP NOP NOP NOP (at 0x0105-0x0109)
    // RET = C3 (at 0x010A)
    let code = [
        0xE8, 0x05, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90, 0x90, 0x90, 0xC3,
    ];
    let (mut cpu, mut mem) = setup_paging(&code);
    // Remap stack page
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    // After CALL+RET, EIP should be at return address 0x0105
    assert_eq!(cpu.regs.eip, 0x0105);
    assert_eq!(cpu.regs.esp, 0x8000);
}

// ========================================================================
// 56. REP STOSB with paging, page crossing at destination
// ========================================================================
#[test]
fn test_paging_rep_stosb_page_crossing() {
    // F3 AA = REP STOSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAA]);
    cpu.regs.set_al(0xFF);
    cpu.regs.edi = 0x3FFC; // 4 bytes before page boundary
    cpu.regs.ecx = 8; // crosses into next page
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..8 {
        assert_eq!(mem.ram[0x3FFC + i], 0xFF);
    }
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.edi, 0x4004);
}

// ========================================================================
// 57. REP MOVSB with paging, destination page crossing
// ========================================================================
#[test]
fn test_paging_rep_movsb_dest_page_crossing() {
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);
    for i in 0..8u8 {
        mem.ram[0x2000 + i as usize] = 0xB0 + i;
    }
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4FFC; // crosses page 4->5 boundary
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 128);
    for i in 0..8u8 {
        assert_eq!(mem.ram[0x4FFC + i as usize], 0xB0 + i);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 58. PUSHAD/POPAD roundtrip with paging
// ========================================================================
#[test]
fn test_paging_pushad_popad_roundtrip() {
    // PUSHAD = 60, POPAD = 61
    let (mut cpu, mut mem) = setup_paging(&[0x60, 0x61]);
    cpu.regs.eax = 0xAA;
    cpu.regs.ecx = 0xBB;
    cpu.regs.edx = 0xCC;
    cpu.regs.ebx = 0xDD;
    cpu.regs.ebp = 0xEE;
    cpu.regs.esi = 0xFF;
    cpu.regs.edi = 0x11;
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0xAA);
    assert_eq!(cpu.regs.ecx, 0xBB);
    assert_eq!(cpu.regs.edx, 0xCC);
    assert_eq!(cpu.regs.ebx, 0xDD);
    assert_eq!(cpu.regs.ebp, 0xEE);
    assert_eq!(cpu.regs.esi, 0xFF);
    assert_eq!(cpu.regs.edi, 0x11);
    assert_eq!(cpu.regs.esp, old_esp);
}

// ========================================================================
// 59. REP STOSB large count with paging (exercises bulk fast path)
// ========================================================================
#[test]
fn test_paging_rep_stosb_large() {
    // F3 AA = REP STOSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAA]);
    cpu.regs.set_al(0x55);
    cpu.regs.edi = 0x6000;
    cpu.regs.ecx = 512;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1024);
    for i in 0..512 {
        assert_eq!(mem.ram[0x6000 + i], 0x55, "Byte {} should be 0x55", i);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 60. REP MOVSD large count with paging (exercises bulk fast path)
// ========================================================================
#[test]
fn test_paging_rep_movsd_large() {
    // F3 A5 = REP MOVSD
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA5]);
    for i in 0..256u32 {
        let addr = 0x2000 + (i as usize) * 4;
        let bytes = (0xDEAD_0000 + i).to_le_bytes();
        mem.ram[addr] = bytes[0];
        mem.ram[addr + 1] = bytes[1];
        mem.ram[addr + 2] = bytes[2];
        mem.ram[addr + 3] = bytes[3];
    }
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x6000;
    cpu.regs.ecx = 256;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1024);
    for i in 0..256u32 {
        let addr = 0x6000 + (i as usize) * 4;
        let dword = u32::from_le_bytes([
            mem.ram[addr],
            mem.ram[addr + 1],
            mem.ram[addr + 2],
            mem.ram[addr + 3],
        ]);
        assert_eq!(dword, 0xDEAD_0000 + i);
    }
    assert_eq!(cpu.regs.ecx, 0);
}

// ========================================================================
// 61. REP SCASD with paging (dword scan)
// ========================================================================
#[test]
fn test_paging_repne_scasd() {
    // F2 AF = REPNE SCASD, F4 = HLT
    let (mut cpu, mut mem) = setup_paging(&[0xF2, 0xAF, 0xF4]);
    for i in 0..4u32 {
        let addr = 0x5000 + (i as usize) * 4;
        let bytes = (i + 1).to_le_bytes();
        mem.ram[addr] = bytes[0];
        mem.ram[addr + 1] = bytes[1];
        mem.ram[addr + 2] = bytes[2];
        mem.ram[addr + 3] = bytes[3];
    }
    cpu.regs.eax = 3; // search for dword 3
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 128);
    assert!(matches!(result, RunResult::Halt { .. }), "got {:?}", result);
    // Match at index 2 (value 3), should advance past it
    assert_eq!(cpu.regs.ecx, 1); // 4 - 3 = 1
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 62. LODSW with paging (single word load)
// ========================================================================
#[test]
fn test_paging_lodsw() {
    // 66 AD = LODSW
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0xAD]);
    mem.ram[0x3000] = 0xEF;
    mem.ram[0x3001] = 0xBE;
    cpu.regs.esi = 0x3000;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax & 0xFFFF, 0xBEEF);
    assert_eq!(cpu.regs.esi, 0x3002);
}

// ========================================================================
// 63. STOSW single with paging (word store)
// ========================================================================
#[test]
fn test_paging_stosw_single() {
    // 66 AB = STOSW
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0xAB]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF_0000) | 0xCAFE;
    cpu.regs.edi = 0x5000;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x5000], 0xFE);
    assert_eq!(mem.ram[0x5001], 0xCA);
    assert_eq!(cpu.regs.edi, 0x5002);
}

// ========================================================================
// 64. CMPSW single with paging (word compare, equal)
// ========================================================================
#[test]
fn test_paging_cmpsw_equal() {
    // 66 A7 = CMPSW
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0xA7]);
    mem.ram[0x2000] = 0x34;
    mem.ram[0x2001] = 0x12;
    mem.ram[0x4000] = 0x34;
    mem.ram[0x4001] = 0x12;
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4000;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.esi, 0x2002);
    assert_eq!(cpu.regs.edi, 0x4002);
}

// ========================================================================
// 65. CMPSW single with paging (word compare, not equal)
// ========================================================================
#[test]
fn test_paging_cmpsw_not_equal() {
    // 66 A7 = CMPSW
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0xA7]);
    mem.ram[0x2000] = 0x01;
    mem.ram[0x2001] = 0x00;
    mem.ram[0x4000] = 0x02;
    mem.ram[0x4001] = 0x00;
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4000;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 66. CMPSD single with paging (dword compare)
// ========================================================================
#[test]
fn test_paging_cmpsd_equal() {
    // A7 = CMPSD (in 32-bit mode, no prefix)
    let (mut cpu, mut mem) = setup_paging(&[0xA7]);
    mem.ram[0x2000..0x2004].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    mem.ram[0x4000..0x4004].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4000;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.esi, 0x2004);
    assert_eq!(cpu.regs.edi, 0x4004);
}

// ========================================================================
// 67. SCASD single with paging
// ========================================================================
#[test]
fn test_paging_scasd_match() {
    // AF = SCASD (in 32-bit mode)
    let (mut cpu, mut mem) = setup_paging(&[0xAF]);
    mem.ram[0x5000..0x5004].copy_from_slice(&0x12345678u32.to_le_bytes());
    cpu.regs.eax = 0x12345678;
    cpu.regs.edi = 0x5000;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.edi, 0x5004);
}

// ========================================================================
// 68. SCASW single with paging
// ========================================================================
#[test]
fn test_paging_scasw_match() {
    // 66 AF = SCASW
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0xAF]);
    mem.ram[0x5000] = 0xEF;
    mem.ram[0x5001] = 0xBE;
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF_0000) | 0xBEEF;
    cpu.regs.edi = 0x5000;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(cpu.regs.edi, 0x5002);
}

// ========================================================================
// 69. MOV to null segment (loads zero selector) in PM
// ========================================================================
#[test]
fn test_mov_null_segment_pm() {
    // MOV DS, AX = 8E D8
    let (mut cpu, mut mem) = setup_pm32(&[0x8E, 0xD8]);
    cpu.regs.eax = 0x0000; // null selector
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ds, 0x0000);
}

// ========================================================================
// 70. PUSH/POP 16-bit segment in PM32 mode
// ========================================================================
#[test]
fn test_push_pop_ds_roundtrip_pm32() {
    // PUSH DS = 1E, MOV DS,AX = 8E D8, POP DS = 1F
    let (mut cpu, mut mem) = setup_pm32(&[0x1E, 0x8E, 0xD8, 0x1F]);
    let original_ds = cpu.regs.ds;
    cpu.regs.eax = 0x10; // set DS to different selector
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 3);
    // PUSH DS, then MOV DS,AX changes DS, then POP DS restores original
    assert_eq!(cpu.regs.ds, original_ds);
}
