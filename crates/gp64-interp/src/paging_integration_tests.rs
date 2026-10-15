use super::*;
use gp64_memory::{
    Memory, SoftwareTlb, TlbEntry, EMS_FRAME_BASE, EMS_FRAME_SIZE, TLB_FLAG_DIRTY, TLB_FLAG_USER,
    TLB_FLAG_WRITABLE,
};

// ========================================================================
// Helper: Set up a 32-bit PM environment with paging enabled.
//
// Page directory at physical 0x10000 (one page).
// Page table   at physical 0x11000 (one page).
//
// By default, identity-maps linear pages 0..N to physical 0..N where
// N covers the first 4 MB (one PDE with a full page table).
// Code placed at physical 0x0100 (linear 0x0100).
// Stack at ESP = 0x8000 (identity mapped).
// All segments have base 0, limit 4GB, 32-bit.
// ========================================================================
fn setup_paging(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();

    // Enable protected mode + paging
    cpu.regs.cr0 = 0x8000_0001; // PE + PG
    cpu.regs.cpl = 0;

    // Page directory at physical 0x10000
    cpu.regs.cr3 = 0x10000;

    // PDE #0 at 0x10000: points to page table at 0x11000, present+rw+user
    mem.write_u32_physical(0x10000, 0x11007);

    // Identity-map the first 256 pages (1 MB) via page table at 0x11000.
    // Each PTE: phys_page << 12 | 0x07 (present + rw + user)
    for page in 0..256u32 {
        let pte_addr = 0x11000 + (page as usize) * 4;
        mem.write_u32_physical(pte_addr, (page << 12) | 0x07);
    }

    // CS: base 0, limit 4GB, 32-bit code
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

    // DS: base 0, limit 4GB, 32-bit data
    cpu.regs.ds = 0x0010;
    cpu.regs.ds_cache.base = 0;
    cpu.regs.ds_cache.limit = 0xFFFFF;
    cpu.regs.ds_cache.granularity = true;
    cpu.regs.ds_cache.recompute_effective_limit();
    cpu.regs.ds_cache.access = 0x93;
    cpu.regs.ds_cache.is_code = false;
    cpu.regs.ds_cache.big = true;
    cpu.regs.ds_cache.present = true;

    // SS: base 0, limit 4GB, 32-bit stack
    cpu.regs.ss = 0x0018;
    cpu.regs.ss_cache.base = 0;
    cpu.regs.ss_cache.limit = 0xFFFFF;
    cpu.regs.ss_cache.granularity = true;
    cpu.regs.ss_cache.recompute_effective_limit();
    cpu.regs.ss_cache.access = 0x93;
    cpu.regs.ss_cache.is_code = false;
    cpu.regs.ss_cache.big = true;
    cpu.regs.ss_cache.present = true;

    // ES: same as DS
    cpu.regs.es = 0x0020;
    cpu.regs.es_cache.base = 0;
    cpu.regs.es_cache.limit = 0xFFFFF;
    cpu.regs.es_cache.granularity = true;
    cpu.regs.es_cache.recompute_effective_limit();
    cpu.regs.es_cache.access = 0x93;
    cpu.regs.es_cache.is_code = false;
    cpu.regs.es_cache.big = true;
    cpu.regs.es_cache.present = true;

    // Stack and IP
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;

    // Write code at physical 0x0100
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }

    (cpu, mem)
}

// ========================================================================
// 16-bit protected mode WITH paging enabled.
//
// A 16-bit code segment still translates instruction and data addresses
// through the page tables when CR0.PG is set. A non-identity mapping ensures
// that skipping translation cannot accidentally pass these tests.
// ========================================================================

/// Map one 4 KB page, linear -> physical, in a fresh PD/PT pair at
/// 0x10000/0x11000. Only linear addresses below 4 MB are supported (one
/// page table), which is all these tests need.
fn map_page_non_identity(mem: &mut Memory, linear: u32, physical: u32) {
    assert!(linear < 0x0040_0000, "single page table covers 4 MB");
    let pte_index = (linear >> 12) & 0x3FF;
    mem.write_u32_physical(0x11000 + pte_index as usize * 4, physical | 0x07);
}

/// 16-bit PM, paging on, one code page and one data page, both mapped at a
/// non-zero offset. Segment bases are non-zero too, so a correct run has to
/// do `base + offset` and *then* translate.
fn setup_paging_16bit(code: &[u8]) -> (Cpu, Memory) {
    const CODE_LIN: u32 = 0x0011_0000;
    const CODE_PHYS: u32 = 0x0031_0000;
    const DATA_LIN: u32 = 0x0012_0000;
    const DATA_PHYS: u32 = 0x0032_0000;

    let mut cpu = Cpu::new();
    let mut mem = Memory::new();

    cpu.regs.cr0 = 0x8000_0001; // PE | PG
    cpu.regs.cr3 = 0x10000;
    cpu.regs.cpl = 3;
    mem.write_u32_physical(0x10000, 0x11007); // PDE 0 -> PT at 0x11000, user/rw
    map_page_non_identity(&mut mem, CODE_LIN, CODE_PHYS);
    map_page_non_identity(&mut mem, DATA_LIN, DATA_PHYS);

    // 16-bit ring-3 code segment, base CODE_LIN, 64 KB limit.
    cpu.regs.cs = 0x000F;
    cpu.regs.cs_cache.base = CODE_LIN;
    cpu.regs.cs_cache.limit = 0xFFFF;
    cpu.regs.cs_cache.granularity = false;
    cpu.regs.cs_cache.recompute_effective_limit();
    cpu.regs.cs_cache.access = 0xFA; // present, DPL 3, code, readable
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.big = false; // <- 16-bit
    cpu.regs.cs_cache.present = true;
    cpu.cs_bits = 16;

    // 16-bit ring-3 data segment, base DATA_LIN, shared by DS/ES/SS.
    let mut data_cache = cpu.regs.ds_cache;
    data_cache.base = DATA_LIN;
    data_cache.limit = 0xFFFF;
    data_cache.granularity = false;
    data_cache.recompute_effective_limit();
    data_cache.access = 0xF2; // present, DPL 3, data, writable
    data_cache.is_code = false;
    data_cache.big = false;
    data_cache.present = true;

    cpu.regs.ds = 0x0017;
    cpu.regs.es = 0x0017;
    cpu.regs.ss = 0x0017;
    cpu.regs.ds_cache = data_cache;
    cpu.regs.es_cache = data_cache;
    cpu.regs.ss_cache = data_cache;

    cpu.regs.set_sp(0xF000);
    cpu.regs.eip = 0;
    cpu.regs.set_ip(0);

    for (i, &b) in code.iter().enumerate() {
        mem.ram[CODE_PHYS as usize + i] = b;
    }

    (cpu, mem)
}

/// Instruction fetch for 16-bit code must resolve CS base + IP through the
/// page tables. If translation were skipped, the fetch would read physical
/// 0x0011_0000 — which is zeroed — and decode garbage.
#[test]
fn sixteen_bit_protected_mode_fetches_code_through_paging() {
    // B8 34 12   mov ax, 0x1234
    // 40         inc ax
    let (mut cpu, mut mem) = setup_paging_16bit(&[0xB8, 0x34, 0x12, 0x40]);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);

    assert!(
        !matches!(result, RunResult::Exception { .. }),
        "16-bit PM code fetch under paging faulted: {result:?}"
    );
    assert_eq!(
        cpu.regs.eax & 0xFFFF,
        0x1235,
        "expected mov+inc to run; got EAX={:#x}",
        cpu.regs.eax
    );
    assert_eq!(
        cpu.regs.ip(),
        4,
        "IP should have advanced past both opcodes"
    );
}

/// A 16-bit data access must translate DS base + offset the same way. The
/// data page is mapped to a different physical page than its linear
/// address, so reading the right value proves the walk happened.
#[test]
fn sixteen_bit_protected_mode_reads_data_through_paging() {
    // A1 10 00   mov ax, [0x0010]
    let (mut cpu, mut mem) = setup_paging_16bit(&[0xA1, 0x10, 0x00]);
    let mut io = ();

    // Value lives at DATA_PHYS + 0x10; the guest asks for DS:0x0010.
    mem.write_u16(0x0032_0010, 0xBEEF);
    // Poison the *linear* address, so an untranslated read is detectably wrong.
    mem.write_u16(0x0012_0010, 0xDEAD);

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        !matches!(result, RunResult::Exception { .. }),
        "16-bit PM data read under paging faulted: {result:?}"
    );
    assert_eq!(
        cpu.regs.eax & 0xFFFF,
        0xBEEF,
        "16-bit data read did not go through the page tables (0xDEAD means it \
         used the linear address as physical)"
    );
}

fn force_direct_host_tlb_entry(
    mem: &mut Memory,
    linear_addr: u32,
    physical_addr: usize,
    writable: bool,
) {
    let linear_page = linear_addr >> 12;
    let physical_page = (physical_addr >> 12) as u32;
    let page_base = physical_addr & !0xFFF;
    let ram_base = mem.ram.host_base();
    let host_write = if writable {
        unsafe { ram_base.add(page_base) }
    } else {
        std::ptr::null_mut()
    };
    let flags = TLB_FLAG_USER
        | if writable {
            TLB_FLAG_WRITABLE | TLB_FLAG_DIRTY
        } else {
            0
        };

    unsafe {
        *mem.tlb_entries_ptr()
            .cast_mut()
            .add(SoftwareTlb::index(linear_page)) = TlbEntry {
            host_read: ram_base.add(page_base).cast_const(),
            host_write,
            linear_page,
            physical_page,
            flags,
            pad: [0; 7],
        };
    }
}

#[test]
fn test_rep_stosb_protected_mode_paged_ems_alias_uses_backing() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAA]);
    let mut io = ();

    mem.init_ems_pool(1).unwrap();
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();
    mem.set_ems_frame_page(0, Some(0)).unwrap();

    let pte_addr = 0x11000 + 5 * 4;
    mem.write_u32_physical(pte_addr, (EMS_FRAME_BASE as u32) | 0x07);
    mem.invalidate_tlb();
    mem.ram[EMS_FRAME_BASE..EMS_FRAME_BASE + 4].copy_from_slice(&[0xC0, 0xC1, 0xC2, 0xC3]);

    cpu.regs.eax = 0x5A;
    cpu.regs.ecx = 4;
    cpu.regs.edi = 0x5000;

    let result = run(&mut cpu, &mut mem, &mut io, 4);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 4 }),
        "PM32 REP STOSB to paged EMS alias should complete, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.edi, 0x5004);

    for i in 0..4 {
        assert_eq!(mem.read_ems_backing_byte(0, i).unwrap(), 0x5A);
        assert_eq!(mem.read_u8(EMS_FRAME_BASE + i), 0x5A);
        assert_eq!(
            mem.ram[EMS_FRAME_BASE + i],
            0xC0 + i as u8,
            "paged REP STOSB must not write the reserved raw EMS frame RAM"
        );
    }
}

#[test]
fn test_rep_stosb_protected_mode_paged_ems_alias_rejects_warm_raw_tlb() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAA]);
    let mut io = ();

    mem.init_ems_pool(1).unwrap();
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();
    mem.set_ems_frame_page(0, Some(0)).unwrap();

    let pte_addr = 0x11000 + 5 * 4;
    mem.write_u32_physical(pte_addr, (EMS_FRAME_BASE as u32) | 0x07);
    mem.invalidate_tlb();
    force_direct_host_tlb_entry(&mut mem, 0x5000, EMS_FRAME_BASE, true);
    mem.ram[EMS_FRAME_BASE..EMS_FRAME_BASE + 8]
        .copy_from_slice(&[0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7]);

    cpu.regs.eax = 0x5A;
    cpu.regs.ecx = 8;
    cpu.regs.edi = 0x5000;

    let result = run(&mut cpu, &mut mem, &mut io, 8);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 8 }),
        "PM32 REP STOSB to warm-TLB EMS alias should complete, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.edi, 0x5008);

    for i in 0..8 {
        assert_eq!(mem.read_ems_backing_byte(0, i).unwrap(), 0x5A);
        assert_eq!(mem.read_u8(EMS_FRAME_BASE + i), 0x5A);
        assert_eq!(
            mem.ram[EMS_FRAME_BASE + i],
            0xC0 + i as u8,
            "warm-TLB REP STOSB must not write reserved raw EMS frame RAM"
        );
    }
}

#[test]
fn test_rep_movsb_protected_mode_paged_ems_alias_rejects_warm_raw_tlb() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);
    let mut io = ();

    mem.init_ems_pool(1).unwrap();
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();
    mem.set_ems_frame_page(0, Some(0)).unwrap();

    let pte_addr = 0x11000 + 5 * 4;
    mem.write_u32_physical(pte_addr, (EMS_FRAME_BASE as u32) | 0x07);
    mem.invalidate_tlb();
    force_direct_host_tlb_entry(&mut mem, 0x5000, EMS_FRAME_BASE, true);
    mem.tlb_insert(6, 6, true, true, true);
    mem.ram[EMS_FRAME_BASE..EMS_FRAME_BASE + 8]
        .copy_from_slice(&[0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7]);
    for i in 0..8 {
        mem.write_ems_backing_byte(0, i, 0xA0 + i as u8).unwrap();
    }

    cpu.regs.esi = 0x5000;
    cpu.regs.edi = 0x6000;
    cpu.regs.ecx = 8;

    let result = run(&mut cpu, &mut mem, &mut io, 8);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 8 }),
        "PM32 REP MOVSB from warm-TLB EMS alias should complete, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.esi, 0x5008);
    assert_eq!(cpu.regs.edi, 0x6008);

    for i in 0..8 {
        assert_eq!(mem.ram[0x6000 + i], 0xA0 + i as u8);
        assert_eq!(mem.ram[EMS_FRAME_BASE + i], 0xC0 + i as u8);
    }
}

#[test]
fn test_rep_movsb_protected_mode_paged_ems_destination_rejects_warm_raw_tlb() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);
    let mut io = ();

    mem.init_ems_pool(1).unwrap();
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();
    mem.set_ems_frame_page(0, Some(0)).unwrap();

    let pte_addr = 0x11000 + 5 * 4;
    mem.write_u32_physical(pte_addr, (EMS_FRAME_BASE as u32) | 0x07);
    mem.invalidate_tlb();
    mem.tlb_insert(6, 6, true, true, true);
    force_direct_host_tlb_entry(&mut mem, 0x5000, EMS_FRAME_BASE, true);
    mem.ram[EMS_FRAME_BASE..EMS_FRAME_BASE + 8]
        .copy_from_slice(&[0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7]);
    for i in 0..8 {
        mem.ram[0x6000 + i] = 0xA0 + i as u8;
    }

    cpu.regs.esi = 0x6000;
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 8;

    let result = run(&mut cpu, &mut mem, &mut io, 8);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 8 }),
        "PM32 REP MOVSB to warm-TLB EMS alias should complete, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 0);
    assert_eq!(cpu.regs.esi, 0x6008);
    assert_eq!(cpu.regs.edi, 0x5008);

    for i in 0..8 {
        assert_eq!(mem.read_ems_backing_byte(0, i).unwrap(), 0xA0 + i as u8);
        assert_eq!(
            mem.ram[EMS_FRAME_BASE + i],
            0xC0 + i as u8,
            "warm-TLB REP MOVSB must not write reserved raw EMS frame RAM"
        );
    }
}

#[test]
fn test_paging_with_a20_disabled_aliases_physical_bit20() {
    let code: &[u8] = &[
        0xA1, 0x00, 0x20, 0x00, 0x00, // MOV EAX, [0x2000]
        0xF4, // HLT
    ];
    let (mut cpu, mut mem) = setup_paging(code);

    // Linear page 2 translates to physical page 0x100. With A20 disabled,
    // the final physical address must alias back to physical page 0.
    mem.write_u32_physical(0x11000 + 2 * 4, (0x100 << 12) | 0x07);
    mem.write_u32_physical(0x00000, 0x1122_3344);
    mem.write_u32_physical(0x100000, 0xAABB_CCDD);
    mem.set_a20(false);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);

    assert!(
        matches!(result, RunResult::Halt { executed: 2 }),
        "paging with A20 disabled should execute without panic, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax, 0x1122_3344,
        "A20-disabled paged read must clear physical bit 20"
    );
}

#[test]
fn test_paged_warm_tlb_word_dword_writes_to_newly_marked_rom_drop() {
    let code: &[u8] = &[
        0x66, 0xA3, 0x00, 0x50, 0x00, 0x00, // MOV [0x5000], AX
        0xA3, 0x00, 0x60, 0x00, 0x00, // MOV [0x6000], EAX
        0xF4, // HLT
    ];
    let (mut cpu, mut mem) = setup_paging(code);
    let mut io = ();

    cpu.regs.eax = 0xDEAD_1234;
    mem.ram[0x5000..0x5002].copy_from_slice(&0xAAAAu16.to_le_bytes());
    mem.ram[0x6000..0x6004].copy_from_slice(&0xBBBB_CCCCu32.to_le_bytes());

    // Warm direct write TLB entries while the pages are ordinary RAM, then
    // mark them ROM. The ROM policy transition must invalidate those host
    // pointers before the paged store fast path can use them.
    mem.tlb_insert(0x5, 0x5, true, true, true);
    mem.tlb_insert(0x6, 0x6, true, true, true);
    mem.mark_rom_page(0x5);
    mem.mark_rom_page(0x6);

    let result = run(&mut cpu, &mut mem, &mut io, 3);
    assert!(
        matches!(result, RunResult::Halt { executed: 3 }),
        "paged ROM writes should execute and then halt, got {result:?}"
    );
    assert_eq!(
        u16::from_le_bytes([mem.ram[0x5000], mem.ram[0x5001]]),
        0xAAAA,
        "paged u16 write through a warm TLB must not mutate a ROM page"
    );
    assert_eq!(
        u32::from_le_bytes([
            mem.ram[0x6000],
            mem.ram[0x6001],
            mem.ram[0x6002],
            mem.ram[0x6003],
        ]),
        0xBBBB_CCCC,
        "paged u32 write through a warm TLB must not mutate a ROM page"
    );
}

// ========================================================================
// Item 33: Differential tests — tlb_translate + translate_slow vs
//          translate_paging_checked
// ========================================================================

#[test]
fn test_paging_translate_split_matches_direct_read() {
    // Verify TLB fast path + slow path fallback produces identical results
    // to calling translate_paging_checked directly.
    let (cpu, mut mem) = setup_paging(&[0x90]); // NOP, unused
    let access = gp64_types::PageAccess::Read;

    // Test a set of addresses across page boundaries
    let test_addrs: &[u32] = &[
        0x0000,  // first byte of page 0
        0x0100,  // code location
        0x0FFF,  // last byte of page 0
        0x1000,  // first byte of page 1
        0x7FFC,  // near stack
        0x8000,  // stack top
        0x10100, // page directory area
    ];

    for &addr in test_addrs {
        // Path A: translate_paging_checked directly
        let direct = cpu.translate_paging_checked(&mut mem, addr, access);
        // Clear TLB to force full page walk on slow path
        mem.invalidate_tlb();

        // Path B: try TLB (will miss after invalidation), then translate_slow
        let tlb = mem.tlb_entries_ptr();
        let tlb_result = unsafe { tlb_translate(tlb, addr, false, false, false) };
        let split = if let Some(phys) = tlb_result {
            Ok(phys)
        } else {
            translate_slow(&cpu, &mut mem, addr, access)
        };

        assert_eq!(
            direct.is_ok(),
            split.is_ok(),
            "Mismatch at address {:#010X}: direct={:?}, split={:?}",
            addr,
            direct,
            split
        );
        if let (Ok(d), Ok(s)) = (&direct, &split) {
            assert_eq!(d, s, "Physical address mismatch at linear {:#010X}", addr);
        }
    }
}

#[test]
fn test_paging_translate_split_matches_direct_write() {
    let (cpu, mut mem) = setup_paging(&[0x90]);
    let access = gp64_types::PageAccess::Write;

    let test_addrs: &[u32] = &[0x0000, 0x0100, 0x1000, 0x7FFC, 0x8000];

    for &addr in test_addrs {
        // Path A: direct
        let direct = cpu.translate_paging_checked(&mut mem, addr, access);
        mem.invalidate_tlb();

        // Path B: TLB miss + slow
        let tlb = mem.tlb_entries_ptr();
        let tlb_result = unsafe { tlb_translate(tlb, addr, true, false, false) };
        let split = if let Some(phys) = tlb_result {
            Ok(phys)
        } else {
            translate_slow(&cpu, &mut mem, addr, access)
        };

        assert_eq!(
            direct.is_ok(),
            split.is_ok(),
            "Write mismatch at {:#010X}: direct={:?}, split={:?}",
            addr,
            direct,
            split
        );
        if let (Ok(d), Ok(s)) = (&direct, &split) {
            assert_eq!(d, s, "Physical address mismatch at linear {:#010X}", addr);
        }
    }
}

#[test]
fn test_paging_translate_tlb_hit_matches_direct() {
    // After a successful translate_paging_checked, the TLB should be warm.
    // A subsequent tlb_translate should return the same physical address.
    let (cpu, mut mem) = setup_paging(&[0x90]);
    let addr = 0x5000u32;

    // Prime the TLB via translate_paging_checked
    let direct = cpu
        .translate_paging_checked(&mut mem, addr, gp64_types::PageAccess::Read)
        .unwrap();

    // TLB should now be warm
    let tlb = mem.tlb_entries_ptr();
    let hit = unsafe { tlb_translate(tlb, addr, false, false, false) };
    assert_eq!(hit, Some(direct), "TLB hit should match direct result");
}

#[test]
fn test_paging_translate_not_present_matches() {
    // Map page 0x50 as NOT present, verify both paths return the same error.
    let (cpu, mut mem) = setup_paging(&[0x90]);

    // Clear PTE for page 0x50 (make not present)
    let pte_addr = 0x11000 + 0x50 * 4;
    mem.write_u32_physical(pte_addr, 0x00); // not present

    let linear = 0x50_000u32;
    let access = gp64_types::PageAccess::Read;

    // Path A: direct
    let direct = cpu.translate_paging_checked(&mut mem, linear, access);
    mem.invalidate_tlb();
    mem.clear_page_fault();

    // Path B: TLB miss + slow
    let tlb = mem.tlb_entries_ptr();
    assert!(unsafe { tlb_translate(tlb, linear, false, false, false) }.is_none());
    let split = translate_slow(&cpu, &mut mem, linear, access);

    assert!(direct.is_err(), "Direct should fail for non-present page");
    assert!(split.is_err(), "Split should fail for non-present page");
    assert_eq!(
        direct.unwrap_err().error_code,
        split.unwrap_err().error_code,
        "Error codes should match"
    );
}

#[test]
fn test_paging_translate_write_protection_matches() {
    // Map page 0x60 as read-only, verify write fails through both paths.
    let mut cpu_state = Cpu::new();
    let mut mem = Memory::new();
    cpu_state.regs.cr0 = 0x8001_0001; // PE + PG + WP
    cpu_state.regs.cr3 = 0x10000;
    cpu_state.regs.cpl = 0;

    // PDE: present + rw + user
    mem.write_u32_physical(0x10000, 0x11007);
    // PTE for page 0x60: present + user, but NOT writable (bit 1 = 0)
    let pte_addr = 0x11000 + 0x60 * 4;
    mem.write_u32_physical(pte_addr, (0x60 << 12) | 0x05); // present + user, no rw

    let linear = 0x60_000u32;
    let access = gp64_types::PageAccess::Write;

    // Path A: direct
    let direct = cpu_state.translate_paging_checked(&mut mem, linear, access);
    mem.invalidate_tlb();
    mem.clear_page_fault();

    // Path B: TLB miss + slow
    let tlb = mem.tlb_entries_ptr();
    assert!(unsafe { tlb_translate(tlb, linear, true, false, true) }.is_none());
    let split = translate_slow(&cpu_state, &mut mem, linear, access);

    assert!(
        direct.is_err(),
        "Direct should fail for write to read-only page"
    );
    assert!(
        split.is_err(),
        "Split should fail for write to read-only page"
    );
    let d_err = direct.unwrap_err();
    let s_err = split.unwrap_err();
    assert_eq!(d_err.error_code, s_err.error_code);
    // Error code should have P=1 (present), W=1 (write)
    assert_ne!(d_err.error_code & 0x01, 0, "P bit should be set");
    assert_ne!(d_err.error_code & 0x02, 0, "W bit should be set");
}

#[test]
fn test_paging_translate_dirty_bit_set_on_write() {
    // After a write translation, the PTE dirty bit should be set.
    let (cpu, mut mem) = setup_paging(&[0x90]);
    let page = 0x30u32;
    let pte_addr = 0x11000 + (page as usize) * 4;

    // Verify dirty bit is initially clear
    let pte_before = mem.read_u32_physical(pte_addr);
    assert_eq!(pte_before & 0x40, 0, "Dirty bit should be clear initially");

    // Do a write translation
    let _ = cpu.translate_paging_checked(&mut mem, page << 12, gp64_types::PageAccess::Write);

    let pte_after = mem.read_u32_physical(pte_addr);
    assert_ne!(pte_after & 0x40, 0, "Dirty bit should be set after write");
}

#[test]
fn test_paging_translate_accessed_bit_set_on_read() {
    // After a read translation, the PTE accessed bit should be set.
    let (cpu, mut mem) = setup_paging(&[0x90]);
    let page = 0x40u32;
    let pte_addr = 0x11000 + (page as usize) * 4;

    // Clear the accessed bit
    let pte = mem.read_u32_physical(pte_addr);
    mem.write_u32_physical(pte_addr, pte & !0x20);
    let pte_before = mem.read_u32_physical(pte_addr);
    assert_eq!(pte_before & 0x20, 0, "Accessed bit should be clear");

    // Do a read translation
    let _ = cpu.translate_paging_checked(&mut mem, page << 12, gp64_types::PageAccess::Read);

    let pte_after = mem.read_u32_physical(pte_addr);
    assert_ne!(pte_after & 0x20, 0, "Accessed bit should be set after read");
}

// ========================================================================
// Item 34: Fault-atomicity tests — SP/ESP preserved on page fault
// ========================================================================

#[test]
fn test_push_fault_preserves_esp() {
    // Setup: stack area page is NOT writable.
    // PUSH EAX should fault with ESP unchanged.
    //
    // 50 = PUSH EAX
    let (mut cpu, mut mem) = setup_paging(&[0x50]);

    // Make the stack page (page containing ESP-4 = 0x7FFC, page 7)
    // read-only by clearing the RW bit in its PTE.
    let stack_page = 7u32; // 0x7000..0x7FFF
    let pte_addr = 0x11000 + (stack_page as usize) * 4;
    mem.write_u32_physical(pte_addr, (stack_page << 12) | 0x05); // present + user, NO rw

    // Set CPL=3 so write protection is enforced
    cpu.regs.cpl = 3;
    cpu.regs.eax = 0xDEADBEEF;
    let saved_esp = cpu.regs.esp;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    match result {
        RunResult::Exception {
            vector, fault_addr, ..
        } => {
            assert_eq!(vector, 14, "Should be page fault (vector 14)");
            assert!(fault_addr.is_some(), "Fault addr should be reported");
        }
        other => panic!("Expected page fault exception, got {:?}", other),
    }
    assert_eq!(
        cpu.regs.esp, saved_esp,
        "ESP should be unchanged after PUSH page fault"
    );
}

#[test]
fn test_pushad_fault_restores_esp() {
    // PUSHAD pushes 8 registers (32 bytes). Set up stack so that the
    // first few pushes succeed but a later one faults.
    //
    // 60 = PUSHAD
    let (mut cpu, mut mem) = setup_paging(&[0x60]);

    // ESP = 0x8000. PUSHAD decrements by 32 to 0x7FE0.
    // All pushes land in page 7 (0x7000-0x7FFF).
    // Make page 7 read-only to fault on the first push.
    let pte_addr = 0x11000 + 7 * 4;
    mem.write_u32_physical(pte_addr, (7 << 12) | 0x05); // present + user, NO rw

    cpu.regs.cpl = 3;
    let saved_esp = cpu.regs.esp;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    match result {
        RunResult::Exception { vector, .. } => {
            assert_eq!(vector, 14, "Should be page fault");
        }
        other => panic!("Expected page fault, got {:?}", other),
    }
    assert_eq!(
        cpu.regs.esp, saved_esp,
        "ESP should be restored after PUSHAD page fault"
    );
}

#[test]
fn test_push_succeeds_with_paging() {
    // Verify PUSH works correctly when paging is enabled and pages are
    // properly mapped (the happy path through paging push helpers).
    //
    // 50 = PUSH EAX
    // 5B = POP EBX
    let (mut cpu, mut mem) = setup_paging(&[0x50, 0x5B]);
    cpu.regs.eax = 0xCAFEBABE;
    cpu.regs.ebx = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "PUSH+POP should succeed with paging, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ebx, 0xCAFEBABE, "POP should retrieve pushed value");
    assert_eq!(
        cpu.regs.esp, 0x8000,
        "ESP should be restored after PUSH+POP"
    );
}

#[test]
fn test_rep_movsb_fault_restores_eip() {
    // REP MOVSB: copy bytes from DS:ESI to ES:EDI, decrement ECX.
    // Map source pages present, make destination page absent after N bytes.
    //
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);

    // Source: page 0x20 (physical 0x20000-0x20FFF), fully mapped
    // Destination: page 0x30 (physical 0x30000-0x30FFF), initially mapped
    //              page 0x31 NOT mapped
    // ESI = 0x20000, EDI = 0x30F00, ECX = 0x200
    // This means after 0x100 bytes (256), the write crosses into page 0x31
    // which is not present, triggering a page fault.

    // Ensure page 0x31 is not present
    let pte_addr_31 = 0x11000 + 0x31 * 4;
    mem.write_u32_physical(pte_addr_31, 0x00); // not present

    // Write source data
    for i in 0..0x200u32 {
        mem.ram[0x20000 + i as usize] = (i & 0xFF) as u8;
    }

    cpu.regs.esi = 0x20000;
    cpu.regs.edi = 0x30F00;
    cpu.regs.ecx = 0x200;
    cpu.regs.cpl = 0; // supervisor, but page not present = fault regardless

    let saved_eip = cpu.regs.eip;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1024);

    match result {
        RunResult::Exception {
            vector, executed, ..
        } => {
            assert_eq!(vector, 14, "Should be page fault");
            // Charge accounting: 256 elements completed before the fault
            // (committed to ECX/ESI/EDI below) must be reported in
            // `executed` — the root charges cycles 1:1 from it, and the
            // charge must not depend on how the budget was sliced.
            assert_eq!(executed, 256, "completed elements must be charged");
        }
        other => panic!("Expected page fault on REP MOVSB, got {:?}", other),
    }

    // EIP should point back to REP MOVSB so it can be restarted
    assert_eq!(
        cpu.regs.eip, saved_eip,
        "EIP should point back to REP MOVSB for restart"
    );

    // ECX is decremented AFTER successful memory access (Intel restart
    // semantics: on page fault, ECX/ESI/EDI reflect state BEFORE the
    // faulting iteration so the instruction can restart correctly).
    // 256 iterations completed, faulting iteration's CX not decremented.
    // ECX = 0x200 - 256 = 0x100.
    assert_eq!(
        cpu.regs.esi, 0x20100,
        "ESI should advance by completed iterations only"
    );
    assert_eq!(
        cpu.regs.edi, 0x31000,
        "EDI should advance to the faulting page boundary"
    );
    assert_eq!(
        cpu.regs.ecx, 0x100,
        "ECX should reflect remaining count (faulting iteration NOT decremented)"
    );
}

// ========================================================================
// Mid-REP fault charge accounting (2026-07-19).
//
// Contract: a mid-REP #PF reports `Exception{executed: K}` where K is the
// number of elements that COMPLETED (and were committed to ECX/ESI/EDI)
// before the faulting one — on BOTH the paged-bulk path (which banks
// `*executed += done` per chunk) and the per-element path. The root
// charges cycles 1:1 from `executed`, so the charge must be invariant to
// budget slicing (run(1)^n must charge the same total as one run(N)) and
// to TLB warmth (bulk vs per-element path selection). REP string
// iterations are charged as they retire.
//
// Layout used throughout: setup_paging identity-maps pages 0..=0xFF (the
// low 1 MiB); each test then clears the PTE of page 0x41 so linear
// 0x41000 is NOT PRESENT. A 4-element dword op starting 8 bytes below
// that boundary completes 2 elements and faults on the 3rd. K = 2.
// (Page 0x40 is plain conventional RAM — deliberately NOT the BIOS
// F-segment 0xF0000..0x100000, where host_write is never published and
// guest stores are silently dropped.)
// ========================================================================

/// Batch REP MOVSD, cold TLB (pure per-element path): the two completed
/// elements must be charged.
#[test]
fn test_rep_movsd_mid_rep_pf_charges_completed_elements() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA5]); // REP MOVSD
    mem.write_u32_physical(0x11000 + 0x41 * 4, 0); // page 0x41 not present
    for (i, v) in [0x1111_1111u32, 0x2222_2222, 0x3333_3333, 0x4444_4444]
        .iter()
        .enumerate()
    {
        mem.ram[0x2000 + i * 4..0x2000 + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4_0FF8; // elements at 0x40FF8, 0x40FFC, then #PF at 0x41000
    cpu.regs.ecx = 4;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 16);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 14,
                executed: 2,
                ..
            }
        ),
        "cold-TLB mid-REP #PF must charge the 2 completed elements, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 2, "2 elements committed");
    assert_eq!(cpu.regs.esi, 0x2008);
    assert_eq!(cpu.regs.edi, 0x4_1000);
    assert_eq!(cpu.regs.eip, 0x0100, "EIP restored to the REP for restart");
    assert_eq!(&mem.ram[0x4_0FF8..0x4_0FFC], &0x1111_1111u32.to_le_bytes());
    assert_eq!(&mem.ram[0x4_0FFC..0x4_1000], &0x2222_2222u32.to_le_bytes());
}

/// The same program driven with budget-1 slices must charge the same
/// total as one batch call, and land in the same architectural state.
#[test]
fn test_rep_movsd_mid_rep_pf_charge_is_slice_invariant() {
    let seed = || {
        let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA5]);
        mem.write_u32_physical(0x11000 + 0x41 * 4, 0); // page 0x41 not present
        for (i, v) in [0x1111_1111u32, 0x2222_2222, 0x3333_3333, 0x4444_4444]
            .iter()
            .enumerate()
        {
            mem.ram[0x2000 + i * 4..0x2000 + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        cpu.regs.esi = 0x2000;
        cpu.regs.edi = 0x4_0FF8;
        cpu.regs.ecx = 4;
        (cpu, mem)
    };
    let mut io = ();

    // Batch drive.
    let (mut cpu_a, mut mem_a) = seed();
    let batch_charge = match run(&mut cpu_a, &mut mem_a, &mut io, 16) {
        RunResult::Exception {
            vector: 14,
            executed,
            ..
        } => executed,
        other => panic!("expected #PF, got {other:?}"),
    };

    // Sliced drive: budget 1 per call until the fault surfaces.
    let (mut cpu_b, mut mem_b) = seed();
    let mut sliced_charge = 0u32;
    loop {
        match run(&mut cpu_b, &mut mem_b, &mut io, 1) {
            RunResult::BudgetExhausted { executed } => sliced_charge += executed,
            RunResult::Exception {
                vector: 14,
                executed,
                ..
            } => {
                sliced_charge += executed;
                break;
            }
            other => panic!("expected progress or #PF, got {other:?}"),
        }
    }

    assert_eq!(
        batch_charge, sliced_charge,
        "the cycle charge for the same guest work must not depend on budget slicing"
    );
    assert_eq!(cpu_a.regs.ecx, cpu_b.regs.ecx);
    assert_eq!(cpu_a.regs.esi, cpu_b.regs.esi);
    assert_eq!(cpu_a.regs.edi, cpu_b.regs.edi);
    assert_eq!(cpu_a.regs.eip, cpu_b.regs.eip);
}

/// Warm write-TLB (paged-bulk path banks the completed chunk) must agree
/// with the cold per-element path: same charge, same architectural state.
/// `tlb_insert(dirty=true)` is the deterministic warmth instrument — a
/// read-touch alone never warms the WRITE TLB (host_write needs the
/// dirty bit), which is why "touch the page first" recipes collapse to
/// the per-element path.
#[test]
fn test_rep_movsd_mid_rep_pf_bulk_path_agrees_with_per_element() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA5]);
    mem.write_u32_physical(0x11000 + 0x41 * 4, 0); // page 0x41 not present
    for (i, v) in [0x1111_1111u32, 0x2222_2222, 0x3333_3333, 0x4444_4444]
        .iter()
        .enumerate()
    {
        mem.ram[0x2000 + i * 4..0x2000 + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4_0FF8;
    cpu.regs.ecx = 4;
    // Warm the source read entry and the destination WRITE entry
    // (writable + dirty — both required for host_write to be usable).
    mem.tlb_insert(0x2, 0x2, false, true, false);
    mem.tlb_insert(0x40, 0x40, true, true, true);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 16);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 14,
                executed: 2,
                ..
            }
        ),
        "warm-TLB (bulk-path) mid-REP #PF must charge the 2 completed elements, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 2);
    assert_eq!(cpu.regs.esi, 0x2008);
    assert_eq!(cpu.regs.edi, 0x4_1000);
    assert_eq!(cpu.regs.eip, 0x0100);
}

/// REP STOSD: dword store variant, per-element path.
#[test]
fn test_rep_stosd_mid_rep_pf_charges_completed_elements() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAB]); // REP STOSD
    mem.write_u32_physical(0x11000 + 0x41 * 4, 0); // page 0x41 not present
    cpu.regs.eax = 0xDEAD_BEEF;
    cpu.regs.edi = 0x4_0FF8;
    cpu.regs.ecx = 4;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 16);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 14,
                executed: 2,
                ..
            }
        ),
        "REP STOSD mid-REP #PF must charge the 2 completed elements, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 2);
    assert_eq!(cpu.regs.edi, 0x4_1000);
    assert_eq!(cpu.regs.eip, 0x0100);
    assert_eq!(&mem.ram[0x4_0FF8..0x4_0FFC], &0xDEAD_BEEFu32.to_le_bytes());
}

/// REP STOSB: byte store variant, per-element path.
#[test]
fn test_rep_stosb_mid_rep_pf_charges_completed_elements() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAA]); // REP STOSB
    mem.write_u32_physical(0x11000 + 0x41 * 4, 0); // page 0x41 not present
    cpu.regs.eax = 0x5A;
    cpu.regs.edi = 0x4_0FFE; // 2 bytes complete, then #PF at 0x41000
    cpu.regs.ecx = 4;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 16);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 14,
                executed: 2,
                ..
            }
        ),
        "REP STOSB mid-REP #PF must charge the 2 completed elements, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 2);
    assert_eq!(cpu.regs.edi, 0x4_1000);
    assert_eq!(mem.ram[0x4_0FFE], 0x5A);
    assert_eq!(mem.ram[0x4_0FFF], 0x5A);
}

/// REP LODSD: read-side fault (LODS has no bulk path — pure per-element).
#[test]
fn test_rep_lodsd_mid_rep_pf_charges_completed_elements() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAD]); // REP LODSD
    mem.write_u32_physical(0x11000 + 0x41 * 4, 0); // page 0x41 not present
    mem.ram[0x4_0FF8..0x4_0FFC].copy_from_slice(&0xAAAA_0001u32.to_le_bytes());
    mem.ram[0x4_0FFC..0x4_1000].copy_from_slice(&0xAAAA_0002u32.to_le_bytes());
    cpu.regs.esi = 0x4_0FF8;
    cpu.regs.ecx = 4;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 16);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 14,
                executed: 2,
                ..
            }
        ),
        "REP LODSD mid-REP #PF must charge the 2 completed elements, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 2);
    assert_eq!(cpu.regs.esi, 0x4_1000);
    assert_eq!(
        cpu.regs.eax, 0xAAAA_0002,
        "EAX holds the last COMPLETED element's value"
    );
}

/// REP LODSB: byte load variant.
#[test]
fn test_rep_lodsb_mid_rep_pf_charges_completed_elements() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAC]); // REP LODSB
    mem.write_u32_physical(0x11000 + 0x41 * 4, 0); // page 0x41 not present
    mem.ram[0x4_0FFE] = 0xC1;
    mem.ram[0x4_0FFF] = 0xC2;
    cpu.regs.esi = 0x4_0FFE;
    cpu.regs.ecx = 4;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 16);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 14,
                executed: 2,
                ..
            }
        ),
        "REP LODSB mid-REP #PF must charge the 2 completed elements, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 2);
    assert_eq!(cpu.regs.esi, 0x4_1000);
    assert_eq!(cpu.regs.eax & 0xFF, 0xC2);
}

/// REPE CMPSD: equal data keeps the compare running until the EDI-side
/// read crosses into the unmapped page. The two completed compares must
/// be charged, and their committed ZF must survive the fault.
#[test]
fn test_repe_cmpsd_mid_rep_pf_charges_completed_elements() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA7]); // REPE CMPSD
    mem.write_u32_physical(0x11000 + 0x41 * 4, 0); // page 0x41 not present
    for (i, v) in [0x0BAD_F00Du32, 0x0BAD_F00E].iter().enumerate() {
        mem.ram[0x2000 + i * 4..0x2000 + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        mem.ram[0x4_0FF8 + i * 4..0x4_0FF8 + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    mem.ram[0x2008..0x200C].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x4_0FF8;
    cpu.regs.ecx = 4;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 16);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 14,
                executed: 2,
                ..
            }
        ),
        "REPE CMPSD mid-REP #PF must charge the 2 completed elements, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 2);
    assert_eq!(cpu.regs.esi, 0x2008);
    assert_eq!(cpu.regs.edi, 0x4_1000);
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF from the last completed (equal) compare is committed"
    );
}

/// REPE SCASD: equal scans until the read crosses into the unmapped page.
#[test]
fn test_repe_scasd_mid_rep_pf_charges_completed_elements() {
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAF]); // REPE SCASD
    mem.write_u32_physical(0x11000 + 0x41 * 4, 0); // page 0x41 not present
    mem.ram[0x4_0FF8..0x4_0FFC].copy_from_slice(&0xCAFE_D00Du32.to_le_bytes());
    mem.ram[0x4_0FFC..0x4_1000].copy_from_slice(&0xCAFE_D00Du32.to_le_bytes());
    cpu.regs.eax = 0xCAFE_D00D;
    cpu.regs.edi = 0x4_0FF8;
    cpu.regs.ecx = 4;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 16);
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 14,
                executed: 2,
                ..
            }
        ),
        "REPE SCASD mid-REP #PF must charge the 2 completed elements, got {result:?}"
    );
    assert_eq!(cpu.regs.ecx, 2);
    assert_eq!(cpu.regs.edi, 0x4_1000);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// Item 35: Cross-page tests
// ========================================================================

#[test]
fn test_cross_page_u16_read() {
    // Setup: u16 at linear address 0x1FFF (straddles page 0x1 and 0x2).
    // Page 0x1 maps to physical 0x5000, page 0x2 maps to physical 0x8000.
    // Write bytes 0xAB at phys 0x5FFF and 0xCD at phys 0x8000.
    // MOV AX, [0x1FFF] should return 0xCDAB.
    //
    // 66 A1 FF 1F 00 00 = MOV AX, [moffs32=0x1FFF] (16-bit operand)
    // But we need 32-bit code with 16-bit operand size prefix.
    // Actually: in 32-bit mode, A1 = MOV EAX, [moffs32].
    //           66 A1 = MOV AX, [moffs32] (operand size override).
    let code: &[u8] = &[0x66, 0xA1, 0xFF, 0x1F, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 1 to physical page 5, page 2 to physical page 8
    mem.write_u32_physical(0x11000 + 4, (0x05 << 12) | 0x07); // page 1 -> phys 0x5000
    mem.write_u32_physical(0x11000 + 2 * 4, (0x08 << 12) | 0x07); // page 2 -> phys 0x8000

    // Write the u16 straddling the page boundary
    mem.ram[0x5FFF] = 0xAB; // low byte at phys 0x5FFF (linear 0x1FFF)
    mem.ram[0x8000] = 0xCD; // high byte at phys 0x8000 (linear 0x2000)

    cpu.regs.eax = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page u16 read should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax & 0xFFFF,
        0xCDAB,
        "Cross-page u16 read should return 0xCDAB"
    );
}

#[test]
fn test_cross_page_u32_read() {
    // u32 at linear 0x1FFE straddles pages 1 and 2.
    // A1 FE 1F 00 00 = MOV EAX, [moffs32=0x1FFE]
    let code: &[u8] = &[0xA1, 0xFE, 0x1F, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap pages
    mem.write_u32_physical(0x11000 + 4, (0x05 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 2 * 4, (0x08 << 12) | 0x07);

    // Write u32 straddling boundary: 0x1FFE-0x2001
    // phys: 0x5FFE, 0x5FFF, 0x8000, 0x8001
    mem.ram[0x5FFE] = 0x11; // byte 0 at linear 0x1FFE
    mem.ram[0x5FFF] = 0x22; // byte 1 at linear 0x1FFF
    mem.ram[0x8000] = 0x33; // byte 2 at linear 0x2000
    mem.ram[0x8001] = 0x44; // byte 3 at linear 0x2001

    cpu.regs.eax = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page u32 read should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax, 0x44332211,
        "Cross-page u32 read should return 0x44332211"
    );
}

#[test]
fn test_cross_page_u32_write() {
    // Write a u32 at linear address straddling a page boundary.
    // A3 FE 1F 00 00 = MOV [moffs32=0x1FFE], EAX
    let code: &[u8] = &[0xA3, 0xFE, 0x1F, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap pages
    mem.write_u32_physical(0x11000 + 4, (0x05 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 2 * 4, (0x08 << 12) | 0x07);

    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page u32 write should succeed, got {:?}",
        result
    );

    // Verify bytes at correct physical locations
    assert_eq!(mem.ram[0x5FFE], 0xEF, "Byte 0 at phys 0x5FFE");
    assert_eq!(mem.ram[0x5FFF], 0xBE, "Byte 1 at phys 0x5FFF");
    assert_eq!(mem.ram[0x8000], 0xAD, "Byte 2 at phys 0x8000");
    assert_eq!(mem.ram[0x8001], 0xDE, "Byte 3 at phys 0x8001");
}

#[test]
fn test_cross_page_instruction_fetch() {
    // Place a 2-byte instruction at linear 0x0FFF (crosses into page 0x1).
    // 90 = NOP (1 byte, at 0x0FFF)
    // Then another NOP at 0x1000.
    // We need to execute from 0x0FFF.
    let (mut cpu, mut mem) = setup_paging(&[]);

    // Put NOPs at the boundary
    mem.ram[0x0FFF] = 0x90; // NOP at linear 0x0FFF
    mem.ram[0x1000] = 0x90; // NOP at linear 0x1000

    cpu.regs.eip = 0x0FFF;
    cpu.regs.set_ip(0x0FFF);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "Cross-page instruction fetch should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, 0x1001, "EIP should advance past both NOPs");
}

#[test]
fn test_cross_page_instruction_fetch_multi_byte() {
    // Place a multi-byte instruction that crosses a page boundary.
    // B8 11 22 33 44 = MOV EAX, 0x44332211 (5 bytes)
    // Put it at linear 0x0FFD so it spans 0x0FFD-0x1001.
    let (mut cpu, mut mem) = setup_paging(&[]);

    mem.ram[0x0FFD] = 0xB8; // MOV EAX, imm32
    mem.ram[0x0FFE] = 0x11;
    mem.ram[0x0FFF] = 0x22;
    mem.ram[0x1000] = 0x33;
    mem.ram[0x1001] = 0x44;

    cpu.regs.eip = 0x0FFD;
    cpu.regs.set_ip(0x0FFD);
    cpu.regs.eax = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page multi-byte fetch should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax, 0x44332211,
        "MOV EAX should load correct value"
    );
    assert_eq!(
        cpu.regs.eip, 0x1002,
        "EIP should advance past the 5-byte instruction"
    );
}

#[test]
fn test_paged_mov_reg_crosses_64k_after_rep() {
    // MOV ECX,EDX at a synthetic 64 KiB boundary splits its opcode and ModR/M
    // across two virtual pages. Neither page is mapped to its linear address,
    // and their physical pages are deliberately noncontiguous.
    for preceded_by_rep in [false, true] {
        for slice_budget in [1, 8] {
            let (mut cpu, mut mem) = setup_paging(&[]);
            let code_start = 0x1234_FFFDu32;
            let code_page = code_start & !0xFFF;
            let next_page = code_page + 0x1000;
            let table = 0x12000usize;
            mem.write_u32_physical(0x10000 + ((code_page >> 22) as usize) * 4, table as u32 | 7);
            mem.write_u32_physical(table + (((code_page >> 12) & 0x3FF) as usize) * 4, 0x30007);
            mem.write_u32_physical(table + (((next_page >> 12) & 0x3FF) as usize) * 4, 0x50007);
            mem.ram[0x30FFD..0x31000].copy_from_slice(&[0xF3, 0xA5, 0x8B]);
            mem.ram[0x50000] = 0xCA; // ModR/M: MOV ECX,EDX.

            cpu.regs.cpl = 3;
            cpu.regs.cs = 0x0017;
            cpu.regs.cs_cache.access = 0xFB;
            for cache in [
                &mut cpu.regs.ds_cache,
                &mut cpu.regs.ss_cache,
                &mut cpu.regs.es_cache,
            ] {
                cache.limit = 0xFFFF;
                cache.granularity = false;
                cache.access = 0xF7; // Present ring-3 writable expand-down data.
                cache.recompute_effective_limit();
            }
            cpu.regs.ds = 0x001F;
            cpu.regs.ss = 0x001F;
            cpu.regs.es = 0x001F;
            cpu.regs.esp = 0x0012_3456;
            cpu.regs.edx = 30;
            cpu.regs.ecx = if preceded_by_rep { 7 } else { 0 };
            cpu.regs.esi = 0x20000;
            cpu.regs.edi = 0x60000;
            cpu.regs.flags.remove(Flags::DIRECTION);
            let source: &[u8; 30] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ123\0";
            assert_eq!(source.len(), 30);
            mem.ram[0x20000..0x20000 + source.len()].copy_from_slice(source);
            cpu.regs.eip = if preceded_by_rep {
                code_start
            } else {
                code_start + 2
            };

            let mut io = ();
            let expected_total = if preceded_by_rep { 8 } else { 1 };
            let mut executed = 0;
            while executed < expected_total {
                let budget = slice_budget.min(expected_total - executed);
                let result = run(&mut cpu, &mut mem, &mut io, budget);
                assert!(matches!(result, RunResult::BudgetExhausted { executed: n } if n == budget),
                    "paged reg-reg MOV after_rep={preceded_by_rep} slice={slice_budget}: {result:?}");
                executed += budget;
            }
            assert_eq!(cpu.regs.eip, 0x1235_0001);
            assert_eq!(cpu.regs.ecx, 30);
            if preceded_by_rep {
                assert_eq!(cpu.regs.esi, 0x2001C);
                assert_eq!(cpu.regs.edi, 0x6001C);
                assert_eq!(&mem.ram[0x60000..0x6001C], &source[..28]);
            }
        }
    }
}

#[test]
fn test_rep_fetch_latch_does_not_decode_absent_page_padding() {
    // REP fits the present page. The following MOV has its ModR/M on an
    // absent page; speculative padding must not become retained code bytes.
    for opcode in [0xA5, 0xAB] {
        for slice_budget in [1, 8] {
            let (mut cpu, mut mem) = setup_paging(&[]);
            let code_page = 0x1234_F000u32;
            let table = 0x12000usize;
            mem.write_u32_physical(0x10000 + ((code_page >> 22) as usize) * 4, table as u32 | 7);
            mem.write_u32_physical(table + (((code_page >> 12) & 0x3FF) as usize) * 4, 0x30007);
            mem.ram[0x30FFD..0x31000].copy_from_slice(&[0xF3, opcode, 0x8B]);

            cpu.regs.cpl = 3;
            cpu.regs.cs = 0x0017;
            cpu.regs.cs_cache.access = 0xFB;
            for cache in [
                &mut cpu.regs.ds_cache,
                &mut cpu.regs.ss_cache,
                &mut cpu.regs.es_cache,
            ] {
                cache.limit = 0xFFFF;
                cache.granularity = false;
                cache.access = 0xF7;
                cache.recompute_effective_limit();
            }
            cpu.regs.eip = 0x1234_FFFD;
            cpu.regs.edx = 30;
            cpu.regs.ecx = 7;
            cpu.regs.esi = 0x20000;
            cpu.regs.edi = 0x60000;
            cpu.regs.flags.remove(Flags::DIRECTION);
            let mut io = ();
            let mut executed = 0;
            while executed < 7 {
                let budget = slice_budget.min(7 - executed);
                let result = run(&mut cpu, &mut mem, &mut io, budget);
                assert!(
                    matches!(result, RunResult::BudgetExhausted { executed: n } if n == budget),
                    "REP opcode={opcode:02X} slice={slice_budget}: {result:?}"
                );
                executed += budget;
            }
            assert_eq!(cpu.regs.eip, 0x1234_FFFF);
            assert_eq!(cpu.regs.ecx, 0);
            assert_eq!(cpu.regs.edi, 0x6001C);
            let result = run(&mut cpu, &mut mem, &mut io, 1);
            assert!(
                matches!(
                    result,
                    RunResult::Exception {
                        vector: 14,
                        fault_addr: Some(0x1235_0000),
                        ..
                    }
                ),
                "MOV after REP opcode={opcode:02X} slice={slice_budget}: {result:?}"
            );
            assert_eq!(cpu.regs.eip, 0x1234_FFFF);
            assert_eq!(cpu.regs.ecx, 0);
            // Let the guest resolve the fault. Fetch the real ModR/M and
            // execute MOV without repeating any completed string element.
            mem.write_u32_physical(
                table + (((0x1235_0000u32 >> 12) & 0x3FF) as usize) * 4,
                0x50007,
            );
            mem.ram[0x50000] = 0xCA;
            let result = run(&mut cpu, &mut mem, &mut io, 1);
            assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
            assert_eq!(cpu.regs.eip, 0x1235_0001);
            assert_eq!(cpu.regs.ecx, 30);
            assert_eq!(cpu.regs.edi, 0x6001C);
        }
    }
}

#[test]
fn test_rep_fetch_latch_retains_complete_tail_on_missing_page() {
    for opcode in [0xA4, 0xAA] {
        let (mut cpu, mut mem) = setup_paging(&[]);
        let code_page = 0x1234_F000u32;
        let table = 0x12000usize;
        mem.write_u32_physical(0x10000 + ((code_page >> 22) as usize) * 4, table as u32 | 7);
        mem.write_u32_physical(table + (((code_page >> 12) & 0x3FF) as usize) * 4, 0x30007);
        mem.ram[0x30FFD..0x31000].copy_from_slice(&[0xF3, opcode, 0xF4]);
        cpu.regs.eip = 0x1234_FFFD;
        cpu.regs.ecx = 1;
        cpu.regs.esi = 0x20000;
        cpu.regs.edi = 0x1234_FFFF;
        cpu.regs.flags.remove(Flags::DIRECTION);
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
        assert_eq!(mem.ram[0x30FFF], 0, "REP overwrites the following HLT");
        assert_eq!(cpu.regs.ecx, 0);
        assert_eq!(cpu.regs.eip, 0x1234_FFFF);
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::Halt { .. }),
            "complete prefetched HLT after REP opcode={opcode:02X}: {result:?}"
        );
    }
}

#[test]
fn test_rep_fetch_latch_refetches_instruction_longer_than_window_tail() {
    // The latch holds the 15 bytes starting at the REP. After a two-byte REP
    // only 13 remain, so a 14-byte instruction after it is incomplete there.
    // Its last byte lies on the same, present page and must be fetched from
    // memory, never read as latch padding.
    for opcode in [0xA4, 0xAA] {
        let mut code = vec![0xF3, opcode];
        // DS DS DS DS MOV DWORD [0x4000], 0x11223344
        code.extend_from_slice(&[0x3E, 0x3E, 0x3E, 0x3E, 0xC7, 0x05]);
        code.extend_from_slice(&0x4000u32.to_le_bytes());
        code.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        assert_eq!(code.len(), 2 + 14);
        let (mut cpu, mut mem) = setup_paging(&code);
        cpu.regs.ecx = 1;
        cpu.regs.esi = 0x2000;
        cpu.regs.edi = 0x3000;
        cpu.regs.flags.remove(Flags::DIRECTION);
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
        assert_eq!(cpu.regs.eip, 0x102);
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::BudgetExhausted { executed: 1 }),
            "MOV after REP opcode={opcode:02X}: {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x110);
        assert_eq!(mem.read_u32_physical(0x4000), 0x1122_3344);
    }
}

#[test]
fn test_cross_page_instruction_fetch_second_page_absent() {
    // If the second page is not present and the instruction actually crosses
    // into it, a page fault should be raised.
    //
    // B8 11 22 33 44 = MOV EAX, imm32 (5 bytes)
    // Place at linear 0x0FFD, but page 1 is NOT present.
    let (mut cpu, mut mem) = setup_paging(&[]);

    // Make page 1 not present
    mem.write_u32_physical(0x11000 + 4, 0x00);

    mem.ram[0x0FFD] = 0xB8; // MOV EAX, imm32
    mem.ram[0x0FFE] = 0x11;
    mem.ram[0x0FFF] = 0x22;
    // bytes at 0x1000, 0x1001 would be on absent page 1

    cpu.regs.eip = 0x0FFD;
    cpu.regs.set_ip(0x0FFD);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception { vector, .. } => {
            assert_eq!(
                vector, 14,
                "Should be page fault for absent page during fetch"
            );
        }
        other => panic!("Expected page fault, got {:?}", other),
    }
}

/// MDD-BUG-CRUCIBLE-00532: a PRESENT second fetch page whose physical
/// address lands above installed RAM is open bus, not a page fault.
///
/// Translation succeeds here — the guest's own page tables map the page —
/// so there is nothing for the CPU to fault on. The arm used to fabricate a
/// `PageFaultInfo` and hand the guest vector 14 for a page it had mapped,
/// while the first-half and single-page arms returned 0xFF for the very
/// same condition.
///
/// The 0xFF tail is asserted through EAX rather than by "no exception"
/// alone: `B8 11 22` + two open-bus bytes must decode as
/// `MOV EAX, 0xFFFF2211`, which pins the open-bus policy itself.
#[test]
fn test_cross_page_fetch_present_pte_over_ram_is_open_bus_not_a_fault() {
    let (mut cpu, mut mem) = setup_paging(&[]);

    // Page 1 stays PRESENT but maps far above installed RAM.
    let oob_frame = 0x7F00_0000u32;
    mem.write_u32_physical(0x11000 + 4, oob_frame | 0x07);
    assert!(
        oob_frame as usize >= mem.ram.len(),
        "test frame must be above installed RAM"
    );

    // MOV EAX, imm32 spanning the page boundary: 3 bytes on page 0, the
    // remaining 2 immediate bytes on the open-bus page.
    mem.ram[0x0FFD] = 0xB8;
    mem.ram[0x0FFE] = 0x11;
    mem.ram[0x0FFF] = 0x22;

    cpu.regs.eip = 0x0FFD;
    cpu.regs.set_ip(0x0FFD);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    if let RunResult::Exception { vector, .. } = result {
        panic!("a mapped page above RAM is open bus, not a fault (got vector {vector})");
    }
    assert_eq!(
        cpu.regs.eax, 0xFFFF_2211,
        "the tail must read as open-bus 0xFF, not zeros"
    );
    assert_eq!(cpu.regs.eip, 0x1002, "the 5-byte instruction must retire");
}

/// Control for the test above: a genuinely NON-PRESENT second fetch page
/// must still raise vector 14. The open-bus fix must not swallow real
/// faults (MDD-BUG-CRUCIBLE-00532).
#[test]
fn test_cross_page_fetch_absent_pte_still_faults_after_open_bus_fix() {
    let (mut cpu, mut mem) = setup_paging(&[]);

    mem.write_u32_physical(0x11000 + 4, 0x00); // page 1 not present

    mem.ram[0x0FFD] = 0xB8;
    mem.ram[0x0FFE] = 0x11;
    mem.ram[0x0FFF] = 0x22;

    cpu.regs.eip = 0x0FFD;
    cpu.regs.set_ip(0x0FFD);
    let mut io = ();

    match run(&mut cpu, &mut mem, &mut io, 1) {
        RunResult::Exception {
            vector, fault_addr, ..
        } => {
            assert_eq!(vector, 14, "an absent page must still fault");
            assert_eq!(fault_addr, Some(0x1000));
        }
        other => panic!("Expected page fault, got {other:?}"),
    }
}

#[test]
fn test_cross_page_fetch_fault_preempts_zero_filled_data_access() {
    // Instruction-fetch faults take priority over operand-access faults:
    // a ModR/M instruction beginning at the end of a present code page
    // must fault on the absent second *instruction* page before any
    // zero-filled placeholder bytes can become a bogus data address.
    //
    // Only the opcode, ModR/M and low displacement word are present.
    // Missing instruction bytes must fault before a zero-filled tail could
    // select the absent operand page.
    let (mut cpu, mut mem) = setup_paging(&[]);

    mem.write_u32_physical(0x11000 + 1 * 4, 0x00); // second fetch page absent
    mem.write_u32_physical(0x11000 + 2 * 4, 0x00); // bogus zero-tail data page absent

    mem.ram[0x0FFC] = 0x83;
    mem.ram[0x0FFD] = 0x3D;
    mem.ram[0x0FFE] = 0x00;
    mem.ram[0x0FFF] = 0x20;

    cpu.regs.eip = 0x0FFC;
    cpu.regs.set_ip(0x0FFC);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::Exception {
            vector, fault_addr, ..
        } => {
            assert_eq!(vector, 14);
            assert_eq!(
                fault_addr,
                Some(0x1000),
                "fetch #PF must target the missing instruction page"
            );
        }
        other => panic!("Expected instruction fetch page fault, got {:?}", other),
    }
    assert_eq!(cpu.regs.eip, 0x0FFC);
}

#[test]
fn test_cross_page_fetch_probe_clears_non_crossing_speculative_fault() {
    // A short instruction at the end of a present page may require a
    // speculative probe of the following page to safely populate fetch_buf.
    // If the instruction length proves it does not actually cross, that
    // probe's not-present result is not architectural and must not leave a
    // pending #PF for later execution paths.
    let (mut cpu, mut mem) = setup_paging(&[]);

    mem.write_u32_physical(0x11000 + 1 * 4, 0x00); // second fetch page absent
    mem.ram[0x0FFF] = 0x90; // NOP fits entirely in the first page

    cpu.regs.eip = 0x0FFF;
    cpu.regs.set_ip(0x0FFF);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    match result {
        RunResult::BudgetExhausted { executed } => assert_eq!(executed, 1),
        other => panic!("Expected one NOP to execute, got {:?}", other),
    }
    assert_eq!(cpu.regs.eip, 0x1000);
    assert!(
        !mem.page_fault_pending,
        "non-crossing speculative second-page probe must not leak #PF state"
    );
}

#[test]
fn test_cross_page_stack_push() {
    // ESP at page boundary, push crosses pages.
    // Set ESP = 0x7001, so PUSH EAX (subtract 4 -> 0x6FFD) writes
    // across pages 6 and 7.
    //
    // 50 = PUSH EAX
    // 5B = POP EBX
    let (mut cpu, mut mem) = setup_paging(&[0x50, 0x5B]);

    cpu.regs.esp = 0x7001;
    cpu.regs.set_sp(0x7001);
    cpu.regs.eax = 0xBAADF00D;
    cpu.regs.ebx = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "Cross-page stack push should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ebx, 0xBAADF00D,
        "POP should retrieve cross-page pushed value"
    );
    assert_eq!(
        cpu.regs.esp, 0x7001,
        "ESP should be restored after PUSH+POP"
    );
}

#[test]
fn test_cross_page_stack_push_non_identity_mapping() {
    // Stack push crosses a page boundary with non-identity page mapping.
    // This verifies the push writes to the correct physical addresses.
    //
    // 50 = PUSH EAX
    let (mut cpu, mut mem) = setup_paging(&[0x50]);

    // Remap page 6 to physical page 0x20, page 7 to physical page 0x30
    mem.write_u32_physical(0x11000 + 6 * 4, (0x20 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 7 * 4, (0x30 << 12) | 0x07);

    // ESP = 0x7002, so PUSH EAX writes to 0x6FFE-0x7001 (crosses page 6/7)
    cpu.regs.esp = 0x7002;
    cpu.regs.set_sp(0x7002);
    cpu.regs.eax = 0xAABBCCDD;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page push with remapping should succeed, got {:?}",
        result
    );

    // The PUSH writes 4 bytes at linear 0x6FFE-0x7001:
    //   0x6FFE -> phys 0x20FFE (page 6 mapped to phys page 0x20)
    //   0x6FFF -> phys 0x20FFF
    //   0x7000 -> phys 0x30000 (page 7 mapped to phys page 0x30)
    //   0x7001 -> phys 0x30001
    assert_eq!(mem.ram[0x20FFE], 0xDD, "Low byte at phys 0x20FFE");
    assert_eq!(mem.ram[0x20FFF], 0xCC, "Byte 1 at phys 0x20FFF");
    assert_eq!(mem.ram[0x30000], 0xBB, "Byte 2 at phys 0x30000");
    assert_eq!(mem.ram[0x30001], 0xAA, "High byte at phys 0x30001");
}

#[test]
fn test_cross_page_big_stack_push_uses_full_esp() {
    // 32-bit stack offsets must not be truncated to SP when the access
    // crosses a page boundary.
    //
    // 50 = PUSH EAX
    let (mut cpu, mut mem) = setup_paging(&[0x50]);

    // ESP = 0x00021002, so PUSH EAX writes to 0x00020FFE..0x00021001.
    // Remap those pages away from their linear page numbers; a 16-bit
    // truncation bug would instead write to 0x00000FFE..0x00001001.
    cpu.regs.esp = 0x0002_1002;
    cpu.regs.set_sp(0x1002);
    cpu.regs.eax = 0xA1B2_C3D4;

    mem.write_u32_physical(0x11000 + 0x20 * 4, (0x60 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 0x21 * 4, (0x61 << 12) | 0x07);
    mem.ram[0x0FFE] = 0x11;
    mem.ram[0x0FFF] = 0x22;
    mem.ram[0x1000] = 0x33;
    mem.ram[0x1001] = 0x44;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page 32-bit PUSH with high ESP should succeed, got {result:?}"
    );

    assert_eq!(mem.ram[0x60FFE], 0xD4, "low byte at high stack page");
    assert_eq!(mem.ram[0x60FFF], 0xC3, "byte 1 at high stack page");
    assert_eq!(mem.ram[0x61000], 0xB2, "byte 2 at next high stack page");
    assert_eq!(mem.ram[0x61001], 0xA1, "high byte at next high stack page");
    assert_eq!(mem.ram[0x0FFE], 0x11, "low truncated stack page untouched");
    assert_eq!(mem.ram[0x0FFF], 0x22, "low truncated stack page untouched");
    assert_eq!(mem.ram[0x1000], 0x33, "low truncated stack page untouched");
    assert_eq!(mem.ram[0x1001], 0x44, "low truncated stack page untouched");
    assert_eq!(cpu.regs.esp, 0x0002_0FFE);
}

#[test]
fn test_cross_page_big_stack_pop_uses_full_esp() {
    // 58 = POP EAX
    let (mut cpu, mut mem) = setup_paging(&[0x58]);

    // ESP = 0x00020FFE, so POP EAX reads 0x00020FFE..0x00021001.
    // The low 16-bit alias contains a different sentinel value.
    cpu.regs.esp = 0x0002_0FFE;
    cpu.regs.set_sp(0x0FFE);

    mem.write_u32_physical(0x11000 + 0x20 * 4, (0x60 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 0x21 * 4, (0x61 << 12) | 0x07);
    mem.ram[0x60FFE] = 0x78;
    mem.ram[0x60FFF] = 0x56;
    mem.ram[0x61000] = 0x34;
    mem.ram[0x61001] = 0x12;
    mem.ram[0x0FFE] = 0xEF;
    mem.ram[0x0FFF] = 0xBE;
    mem.ram[0x1000] = 0xAD;
    mem.ram[0x1001] = 0xDE;

    cpu.regs.eax = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page 32-bit POP with high ESP should succeed, got {result:?}"
    );

    assert_eq!(
        cpu.regs.eax, 0x1234_5678,
        "POP must read through the full 32-bit ESP linear address"
    );
    assert_eq!(cpu.regs.esp, 0x0002_1002);
}

// ========================================================================
// Item 35 bonus: Paging with non-identity mapped instruction execution
// ========================================================================

#[test]
fn test_paging_non_identity_code_execution() {
    // Execute code from a non-identity-mapped page.
    // Linear page 0 maps to physical page 0x20.
    // Place NOP at physical 0x20100 (linear 0x0100).
    let (mut cpu, mut mem) = setup_paging(&[]);

    // Remap page 0 to physical page 0x20
    mem.write_u32_physical(0x11000, (0x20 << 12) | 0x07);

    // Place NOP at the physical address that linear 0x0100 maps to
    mem.ram[0x20100] = 0x90; // NOP

    cpu.regs.eip = 0x0100;
    cpu.regs.set_ip(0x0100);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Non-identity-mapped code fetch should succeed, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eip, 0x0101, "EIP should advance past NOP");
}

#[test]
fn test_paging_data_read_non_identity() {
    // Read data from a non-identity-mapped page.
    // DS base = 0, linear 0x2000 maps to physical page 0x50.
    // A1 00 20 00 00 = MOV EAX, [0x2000]
    let code: &[u8] = &[0xA1, 0x00, 0x20, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 2 to physical page 0x50
    mem.write_u32_physical(0x11000 + 2 * 4, (0x50 << 12) | 0x07);

    // Write data at physical 0x50000 (linear 0x2000)
    let phys_addr = 0x50000;
    mem.ram[phys_addr] = 0x78;
    mem.ram[phys_addr + 1] = 0x56;
    mem.ram[phys_addr + 2] = 0x34;
    mem.ram[phys_addr + 3] = 0x12;

    cpu.regs.eax = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Non-identity data read should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax, 0x12345678,
        "Should read from remapped physical page"
    );
}

// ========================================================================
// B3: Cross-page paging tests
//
// Verify that memory accesses spanning 4 KB page boundaries work
// correctly when paging is enabled, including non-identity mappings.
// ========================================================================

#[test]
fn test_paging_cross_page_push16() {
    // 16-bit PUSH where the 2-byte write crosses a page boundary.
    // In 32-bit code mode, 66 50 = PUSH AX (operand-size override).
    // We also need a matching POP to verify the value round-trips.
    // 66 50 = PUSH AX
    // 66 5B = POP BX
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0x50, 0x66, 0x5B]);

    // Set ESP so that ESP-2 crosses from page 4 into page 3.
    // ESP = 0x4001, so the 16-bit push writes to 0x3FFF..0x4000:
    //   byte 0 (low) at linear 0x3FFF (page 3)
    //   byte 1 (high) at linear 0x4000 (page 4)
    cpu.regs.esp = 0x4001;
    cpu.regs.set_sp(0x4001);

    // Remap pages to non-identity to prove translation works
    // Page 3 -> physical page 0x20, page 4 -> physical page 0x30
    mem.write_u32_physical(0x11000 + 3 * 4, (0x20 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 4 * 4, (0x30 << 12) | 0x07);

    cpu.regs.eax = 0x0000_BEEF; // AX = 0xBEEF
    cpu.regs.ebx = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "Cross-page 16-bit PUSH+POP should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ebx & 0xFFFF,
        0xBEEF,
        "POP BX should retrieve the cross-page pushed 16-bit value"
    );
    assert_eq!(
        cpu.regs.esp, 0x4001,
        "ESP should be restored after PUSH AX + POP BX"
    );

    // Verify the push wrote to the correct physical addresses
    // Low byte 0xEF at phys 0x20FFF (linear 0x3FFF, page 3 -> phys page 0x20)
    // High byte 0xBE at phys 0x30000 (linear 0x4000, page 4 -> phys page 0x30)
    // Note: the POP already consumed the data, but the bytes are still there.
    assert_eq!(mem.ram[0x20FFF], 0xEF, "Low byte at phys 0x20FFF");
    assert_eq!(mem.ram[0x30000], 0xBE, "High byte at phys 0x30000");
}

#[test]
fn test_paging_cross_page_pop16() {
    // 16-bit POP where the 2-byte read crosses a page boundary.
    // In 32-bit code mode, 66 58 = POP AX.
    // Set ESP at page_end-1 so the 2-byte read spans two pages.
    let (mut cpu, mut mem) = setup_paging(&[0x66, 0x58]);

    // ESP = 0x4FFF, so POP reads from 0x4FFF..0x5000:
    //   byte 0 (low) at linear 0x4FFF (page 4)
    //   byte 1 (high) at linear 0x5000 (page 5)
    cpu.regs.esp = 0x4FFF;
    cpu.regs.set_sp(0x4FFF);

    // Remap pages to non-identity
    // Page 4 -> physical page 0x40, page 5 -> physical page 0x50
    mem.write_u32_physical(0x11000 + 4 * 4, (0x40 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 5 * 4, (0x50 << 12) | 0x07);

    // Write the value at the correct physical locations
    mem.ram[0x40FFF] = 0xCD; // low byte at phys 0x40FFF (linear 0x4FFF)
    mem.ram[0x50000] = 0xAB; // high byte at phys 0x50000 (linear 0x5000)

    cpu.regs.eax = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page 16-bit POP should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax & 0xFFFF,
        0xABCD,
        "POP AX should read 0xABCD from cross-page stack"
    );
    assert_eq!(cpu.regs.esp, 0x5001, "ESP should advance by 2 after POP AX");
}

#[test]
fn test_paging_cross_page_mov_r16_mem_modrm() {
    // MOV r16, [mem] using ModR/M addressing, where the 2-byte read
    // crosses a page boundary.
    // In 32-bit mode: 66 8B 05 FF 1F 00 00 = MOV AX, [0x1FFF]
    //   66 = operand size override (16-bit)
    //   8B = MOV r, r/m
    //   05 = ModR/M: mod=00, reg=AX, r/m=101 (disp32)
    //   FF 1F 00 00 = disp32 = 0x00001FFF
    let code: &[u8] = &[0x66, 0x8B, 0x05, 0xFF, 0x1F, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 1 -> physical page 0x10, page 2 -> physical page 0x20
    mem.write_u32_physical(0x11000 + 4, (0x10 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 2 * 4, (0x20 << 12) | 0x07);

    // Write u16 straddling the boundary:
    //   low byte at phys 0x10FFF (linear 0x1FFF, page 1 -> phys page 0x10)
    //   high byte at phys 0x20000 (linear 0x2000, page 2 -> phys page 0x20)
    mem.ram[0x10FFF] = 0x34;
    mem.ram[0x20000] = 0x12;

    cpu.regs.eax = 0xFFFF_FFFF;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page MOV r16,[mem] should succeed, got {:?}",
        result
    );
    // The 66-prefixed MOV should only load into AX (low 16 bits).
    // Upper 16 bits of EAX should be zeroed on x86 for 16-bit MOV.
    // Actually in real x86, 16-bit MOV only modifies the low 16 bits.
    assert_eq!(
        cpu.regs.eax & 0xFFFF,
        0x1234,
        "Cross-page MOV AX,[0x1FFF] should load 0x1234"
    );
}

#[test]
fn test_paging_cross_page_mov_mem_r16_modrm() {
    // MOV [mem], r16 using ModR/M addressing, where the 2-byte write
    // crosses a page boundary.
    // In 32-bit mode: 66 89 05 FF 1F 00 00 = MOV [0x1FFF], AX
    //   66 = operand size override (16-bit)
    //   89 = MOV r/m, r
    //   05 = ModR/M: mod=00, reg=AX, r/m=101 (disp32)
    //   FF 1F 00 00 = disp32 = 0x00001FFF
    let code: &[u8] = &[0x66, 0x89, 0x05, 0xFF, 0x1F, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 1 -> physical page 0x10, page 2 -> physical page 0x20
    mem.write_u32_physical(0x11000 + 4, (0x10 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 2 * 4, (0x20 << 12) | 0x07);

    cpu.regs.eax = 0x0000_5678;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page MOV [mem],r16 should succeed, got {:?}",
        result
    );

    // Verify bytes at correct physical locations
    assert_eq!(
        mem.ram[0x10FFF], 0x78,
        "Low byte at phys 0x10FFF (linear 0x1FFF)"
    );
    assert_eq!(
        mem.ram[0x20000], 0x56,
        "High byte at phys 0x20000 (linear 0x2000)"
    );
}

#[test]
fn test_paging_cross_page_rep_movsb() {
    // REP MOVSB that successfully copies across a page boundary.
    // Source starts near end of one page, destination on another.
    // No fault — both source and destination pages are present.
    //
    // F3 A4 = REP MOVSB
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);

    // Source: starts at linear 0x2FFE (page 2), crosses into page 3
    // Destination: starts at linear 0x5000 (page 5), all within one page
    // Count: 4 bytes (2 bytes from page 2, 2 bytes from page 3)

    // Remap source pages: page 2 -> phys 0x40, page 3 -> phys 0x50
    mem.write_u32_physical(0x11000 + 2 * 4, (0x40 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 3 * 4, (0x50 << 12) | 0x07);
    // Page 5 stays identity-mapped

    // Write source data at physical locations
    mem.ram[0x40FFE] = 0xAA; // linear 0x2FFE -> phys 0x40FFE
    mem.ram[0x40FFF] = 0xBB; // linear 0x2FFF -> phys 0x40FFF
    mem.ram[0x50000] = 0xCC; // linear 0x3000 -> phys 0x50000
    mem.ram[0x50001] = 0xDD; // linear 0x3001 -> phys 0x50001

    cpu.regs.esi = 0x2FFE;
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 4;
    // DF=0 (forward)
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 4);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 4 }),
        "Cross-page REP MOVSB should succeed, got {:?}",
        result
    );

    // Verify destination contains copied data (page 5 is identity-mapped)
    assert_eq!(mem.ram[0x5000], 0xAA, "Byte 0 copied correctly");
    assert_eq!(mem.ram[0x5001], 0xBB, "Byte 1 copied correctly");
    assert_eq!(
        mem.ram[0x5002], 0xCC,
        "Byte 2 copied correctly (from page 3)"
    );
    assert_eq!(
        mem.ram[0x5003], 0xDD,
        "Byte 3 copied correctly (from page 3)"
    );

    // Verify registers advanced correctly
    assert_eq!(cpu.regs.esi, 0x3002, "ESI advanced by 4");
    assert_eq!(cpu.regs.edi, 0x5004, "EDI advanced by 4");
    assert_eq!(
        cpu.regs.ecx, 0,
        "ECX should be 0 after completing all iterations"
    );
}

#[test]
fn test_paging_cross_page_instruction_fetch_opcode_modrm_split() {
    // Instruction fetch where the opcode is on one page and the ModR/M
    // byte plus displacement are on the next page.
    // Place a MOV EAX, [EBX] at linear 0x1FFF:
    //   8B 03 = MOV EAX, [EBX] (2 bytes: opcode 8B at 0x1FFF, ModR/M 03 at 0x2000)
    //
    // Use non-identity mapping to prove both pages are translated.
    let (mut cpu, mut mem) = setup_paging(&[]);

    // Remap page 1 -> physical page 0x10, page 2 -> physical page 0x20
    mem.write_u32_physical(0x11000 + 4, (0x10 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 2 * 4, (0x20 << 12) | 0x07);

    // Place instruction bytes at physical addresses
    mem.ram[0x10FFF] = 0x8B; // opcode at phys 0x10FFF (linear 0x1FFF)
    mem.ram[0x20000] = 0x03; // ModR/M at phys 0x20000 (linear 0x2000)

    // EBX points to a data address (within page 8, identity-mapped)
    cpu.regs.ebx = 0x8000;
    mem.ram[0x8000] = 0x11;
    mem.ram[0x8001] = 0x22;
    mem.ram[0x8002] = 0x33;
    mem.ram[0x8003] = 0x44;

    cpu.regs.eip = 0x1FFF;
    cpu.regs.set_ip(0x1FFF);
    cpu.regs.eax = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Cross-page instruction fetch (opcode/ModR/M split) should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax, 0x44332211,
        "MOV EAX,[EBX] should load from address in EBX"
    );
    assert_eq!(
        cpu.regs.eip, 0x2001,
        "EIP should advance past the 2-byte instruction"
    );
}

// ========================================================================
// Test G: Physical mapping must NOT bypass page tables when paging is active
// ========================================================================

#[test]
fn test_paging_physical_mapping_bypassed_when_paging_active() {
    // When paging is active (CR0.PG=1), page tables are authoritative.
    // A DPMI physical mapping that maps the same linear address to a
    // DIFFERENT physical address must be ignored — the page table wins.
    let (mut cpu, mut mem) = setup_paging(&[]);

    // Identity-mapped paging: linear 0x5000 -> physical 0x5000 (from setup_paging)
    // Write a sentinel value at physical 0x5000 (the page-table destination)
    mem.ram[0x5000] = 0xAA;

    // Add a physical mapping that says linear 0x5000 -> physical 0x9000
    // Write a different sentinel at physical 0x9000
    mem.ram[0x9000] = 0xBB;
    mem.add_physical_mapping(0x5000, 0x9000, 0x1000);

    // With paging active, reading linear 0x5000 must return 0xAA (page table),
    // NOT 0xBB (physical mapping).
    let result = translate_slow(&cpu, &mut mem, 0x5000, gp64_types::PageAccess::Read);
    let phys = result.expect("translate_slow should succeed");
    assert_eq!(
        phys, 0x5000,
        "Page table mapping (identity) should win over physical mapping"
    );
    assert_eq!(
        mem.ram[phys], 0xAA,
        "Should read from page-table physical address, not physical mapping"
    );

    // Sanity: disable paging, physical mapping should now take effect
    cpu.regs.cr0 &= !0x8000_0000;
    let result2 = translate_slow(&cpu, &mut mem, 0x5000, gp64_types::PageAccess::Read);
    let phys2 = result2.expect("translate_slow should succeed without paging");
    assert_eq!(
        phys2, 0x9000,
        "Without paging, physical mapping should apply"
    );
    assert_eq!(
        mem.ram[phys2], 0xBB,
        "Should read from physical-mapping address when paging is off"
    );
}

// ========================================================================
// Test B: CR3 Write Flushes TLB and Remap Takes Effect
// ========================================================================

#[test]
fn test_paging_cr3_write_flushes_tlb() {
    // 1. Identity-map first 256 pages via setup_paging()
    // 2. Read from linear 0x5000 (identity: phys 0x5000) to populate TLB
    // 3. Change PTE for page 5 to point to phys 0x9000
    // 4. MOV CR3, EAX (flush TLB)
    // 5. Read from 0x5000 again — should now get data from phys 0x9000
    //
    // Code layout at 0x0100:
    //   A1 00 50 00 00          MOV EAX, [0x5000]    ; first read (populates TLB)
    //   B8 xx xx 01 00          MOV EAX, 0x10000     ; load CR3 value into EAX
    //   0F 22 D8                MOV CR3, EAX         ; flush TLB
    //   A1 00 50 00 00          MOV EAX, [0x5000]    ; second read (should use new PTE)
    let code: &[u8] = &[
        0xA1, 0x00, 0x50, 0x00, 0x00, // MOV EAX, [0x5000]
        0xB8, 0x00, 0x00, 0x01, 0x00, // MOV EAX, 0x10000 (CR3 value)
        0x0F, 0x22, 0xD8, // MOV CR3, EAX
        0xA1, 0x00, 0x50, 0x00, 0x00, // MOV EAX, [0x5000]
    ];
    let (mut cpu, mut mem) = setup_paging(code);

    // Write sentinel at identity-mapped physical 0x5000
    mem.ram[0x5000] = 0x11;
    mem.ram[0x5001] = 0x22;
    mem.ram[0x5002] = 0x33;
    mem.ram[0x5003] = 0x44;

    // Write different sentinel at physical 0x9000 (future remap target)
    mem.ram[0x9000] = 0xAA;
    mem.ram[0x9001] = 0xBB;
    mem.ram[0x9002] = 0xCC;
    mem.ram[0x9003] = 0xDD;

    let mut io = ();

    // Step 1: execute the first MOV EAX, [0x5000] — reads from phys 0x5000
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "First read should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax, 0x44332211,
        "First read should return identity-mapped data from phys 0x5000"
    );

    // Step 2: remap PTE for page 5 to point to physical page 0x9
    let pte_addr_5 = 0x11000 + 5 * 4;
    mem.write_u32_physical(pte_addr_5, (0x9 << 12) | 0x07);

    // Step 3: execute MOV EAX, 0x10000 (load CR3 value) + MOV CR3, EAX (flush TLB)
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "MOV EAX + MOV CR3 should succeed, got {:?}",
        result
    );

    // Step 4: execute second MOV EAX, [0x5000] — should now read from phys 0x9000
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Second read should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax, 0xDDCCBBAA,
        "After CR3 write (TLB flush), read from linear 0x5000 should return data from phys 0x9000"
    );
}

// ========================================================================
// Test C: INVLPG Selective Invalidation
// ========================================================================

#[test]
fn test_paging_invlpg_selective_invalidation() {
    // 1. Read from pages 0x5000 and 0x6000 to populate TLB for both
    // 2. Change PTE for page 5 to point to phys 0x9000
    // 3. INVLPG [0x5000] — invalidate only page 5
    // 4. Read from 0x5000 — should use new mapping (phys 0x9000)
    // 5. Read from 0x6000 — should still use old mapping (phys 0x6000, TLB cached)
    //
    // Code layout at 0x0100:
    //   A1 00 50 00 00                MOV EAX, [0x5000]    ; prime TLB for page 5
    //   A1 00 60 00 00                MOV EAX, [0x6000]    ; prime TLB for page 6
    //   0F 01 3D 00 50 00 00          INVLPG [0x5000]      ; invalidate page 5 only
    //   A1 00 50 00 00                MOV EAX, [0x5000]    ; re-read page 5 (new PTE)
    //   8B 1D 00 60 00 00             MOV EBX, [0x6000]    ; re-read page 6 (TLB cached)
    let code: &[u8] = &[
        0xA1, 0x00, 0x50, 0x00, 0x00, // MOV EAX, [0x5000]
        0xA1, 0x00, 0x60, 0x00, 0x00, // MOV EAX, [0x6000]
        0x0F, 0x01, 0x3D, 0x00, 0x50, 0x00, 0x00, // INVLPG [0x5000]
        0xA1, 0x00, 0x50, 0x00, 0x00, // MOV EAX, [0x5000]
        0x8B, 0x1D, 0x00, 0x60, 0x00, 0x00, // MOV EBX, [0x6000]
    ];
    let (mut cpu, mut mem) = setup_paging(code);
    // MDD-BUG-CRUCIBLE-00561: INVLPG is 486+, and `CpuType::default()` is
    // `I80386`. A real 386 pages but has no INVLPG (it flushes via MOV
    // CR3), so without this the test would be asserting that a 386
    // executes an instruction it does not have.
    cpu.cpu_type = gp64_cpu::CpuType::I80486;

    // Write sentinels at identity-mapped locations
    mem.ram[0x5000] = 0x11;
    mem.ram[0x5001] = 0x22;
    mem.ram[0x5002] = 0x33;
    mem.ram[0x5003] = 0x44;

    mem.ram[0x6000] = 0x55;
    mem.ram[0x6001] = 0x66;
    mem.ram[0x6002] = 0x77;
    mem.ram[0x6003] = 0x88;

    // Write sentinel at physical 0x9000 (future remap target for page 5)
    mem.ram[0x9000] = 0xAA;
    mem.ram[0x9001] = 0xBB;
    mem.ram[0x9002] = 0xCC;
    mem.ram[0x9003] = 0xDD;

    let mut io = ();

    // Step 1: prime TLB for both pages
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "TLB priming reads should succeed, got {:?}",
        result
    );

    // Step 2: remap PTE for page 5 to point to physical page 0x9
    let pte_addr_5 = 0x11000 + 5 * 4;
    mem.write_u32_physical(pte_addr_5, (0x9 << 12) | 0x07);

    // Step 3: execute INVLPG [0x5000] — selective invalidation
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(
            result,
            RunResult::Invlpg {
                linear_addr: 0x5000,
                executed: 1
            }
        ),
        "INVLPG should return Invlpg variant, got {:?}",
        result
    );

    // Step 4: read from 0x5000 (should use new PTE → phys 0x9000)
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Read from remapped page should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.eax, 0xDDCCBBAA,
        "After INVLPG, read from linear 0x5000 should return data from phys 0x9000"
    );

    // Step 5: read from 0x6000 (TLB still cached — should use original mapping)
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "Read from non-invalidated page should succeed, got {:?}",
        result
    );
    assert_eq!(
        cpu.regs.ebx, 0x88776655,
        "Page 6 TLB entry should NOT be invalidated — should still read from phys 0x6000"
    );
}

// ========================================================================
// Test D: REP STOSD Fault Mid-Operation + Restart
// ========================================================================

#[test]
fn test_paging_rep_stosd_fault_mid_operation() {
    // Identity-map page at 0x5000, make page at 0x6000 NOT present.
    // REP STOSD with EDI starting near the end of page 0x5, crossing
    // into the absent page 0x6 — should fault partway through.
    //
    // F3 AB = REP STOSD (32-bit mode)
    let code: &[u8] = &[0xF3, 0xAB];
    let (mut cpu, mut mem) = setup_paging(code);

    // Make page 6 not present
    let pte_addr_6 = 0x11000 + 6 * 4;
    mem.write_u32_physical(pte_addr_6, 0x00); // not present

    // ES:EDI = 0x5FF0 (16 bytes from end of page 5)
    // ECX = 8 (8 DWORDs = 32 bytes, crossing into page 6)
    // EAX = 0xDEADBEEF (fill value)
    cpu.regs.edi = 0x5FF0;
    cpu.regs.ecx = 8;
    cpu.regs.eax = 0xDEADBEEF;

    let saved_eip = cpu.regs.eip;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1024);

    // Should get a page fault (vector 14)
    match result {
        RunResult::Exception { vector, .. } => {
            assert_eq!(vector, 14, "Should be page fault");
        }
        other => panic!("Expected page fault on REP STOSD, got {:?}", other),
    }

    // EIP should point back at the REP STOSD for restart
    assert_eq!(
        cpu.regs.eip, saved_eip,
        "EIP should point back to REP STOSD for restart"
    );

    // 4 DWORDs fit in 16 bytes (0x5FF0..0x5FFF). The fault occurs
    // when trying to write the 5th DWORD at 0x6000 (not present).
    // After 4 successful iterations: ECX = 8 - 4 = 4, EDI = 0x5FF0 + 16 = 0x6000
    assert_eq!(
        cpu.regs.ecx, 4,
        "ECX should reflect 4 remaining DWORDs (4 completed before fault)"
    );
    assert_eq!(
        cpu.regs.edi, 0x6000,
        "EDI should advance to the faulting page boundary"
    );

    // Verify the 4 completed DWORDs were written to physical memory
    for i in 0..4u32 {
        let addr = 0x5FF0 + i as usize * 4;
        let val = u32::from_le_bytes([
            mem.ram[addr],
            mem.ram[addr + 1],
            mem.ram[addr + 2],
            mem.ram[addr + 3],
        ]);
        assert_eq!(
            val, 0xDEADBEEF,
            "DWORD {} at phys {:#06X} should be 0xDEADBEEF",
            i, addr
        );
    }
}

// ========================================================================
// Test E: CALL Under Paging (Stack Push Through Page Tables)
// ========================================================================

#[test]
fn test_paging_call_stack_push_through_page_tables() {
    // Remap the stack page so the return address pushed by CALL
    // goes through page translation to a non-identity physical page.
    //
    // Page 8 (linear 0x8000-0x8FFF) is remapped to physical 0xA000.
    // SS:ESP = 0x8100. NEAR CALL pushes return address at ESP-4 = 0x80FC.
    // The push should write to physical 0xA0FC, not 0x80FC.
    //
    // E8 00 00 00 00 = CALL +0 (calls the next instruction, pushes EIP+5)
    let code: &[u8] = &[0xE8, 0x00, 0x00, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 8 to physical page 0xA
    let pte_addr_8 = 0x11000 + 8 * 4;
    mem.write_u32_physical(pte_addr_8, (0xA << 12) | 0x07);

    // Set stack within remapped page
    cpu.regs.esp = 0x8100;
    cpu.regs.set_sp(0x8100);

    // Clear both physical locations to detect where the write lands
    mem.ram[0x80FC] = 0x00;
    mem.ram[0x80FD] = 0x00;
    mem.ram[0x80FE] = 0x00;
    mem.ram[0x80FF] = 0x00;
    mem.ram[0xA0FC] = 0x00;
    mem.ram[0xA0FD] = 0x00;
    mem.ram[0xA0FE] = 0x00;
    mem.ram[0xA0FF] = 0x00;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CALL should succeed, got {:?}",
        result
    );

    // ESP should have decremented by 4
    assert_eq!(cpu.regs.esp, 0x80FC, "ESP should be 0x8100 - 4 = 0x80FC");

    // The return address (EIP after CALL = 0x0100 + 5 = 0x0105) should be
    // at physical 0xA0FC (page 8 remapped to phys page 0xA)
    let ret_addr = u32::from_le_bytes([
        mem.ram[0xA0FC],
        mem.ram[0xA0FD],
        mem.ram[0xA0FE],
        mem.ram[0xA0FF],
    ]);
    assert_eq!(
        ret_addr, 0x0105,
        "Return address should be written to physical 0xA0FC (remapped stack page)"
    );

    // Physical 0x80FC should NOT have been written (that's the old identity mapping)
    let old_phys = u32::from_le_bytes([
        mem.ram[0x80FC],
        mem.ram[0x80FD],
        mem.ram[0x80FE],
        mem.ram[0x80FF],
    ]);
    assert_eq!(
        old_phys, 0x00000000,
        "Physical 0x80FC should be untouched (page table remaps to 0xA0FC)"
    );
}

// ========================================================================
// Test F: Cross-Page Data Write Where Second Page Faults
// ========================================================================

#[test]
fn test_paging_cross_page_write_second_page_faults() {
    // Map page 5 (linear 0x5000), make page 6 (linear 0x6000) NOT present.
    // Write a 16-bit value at linear 0x5FFF — straddles the page boundary:
    //   byte 0 (low) at linear 0x5FFF (page 5, present)
    //   byte 1 (high) at linear 0x6000 (page 6, NOT present)
    // Should fault because the second byte is on a not-present page.
    // The first byte should NOT be modified (translate-before-commit).
    //
    // 66 A3 FF 5F 00 00 = MOV [0x5FFF], AX (16-bit operand, 32-bit addr)
    let code: &[u8] = &[0x66, 0xA3, 0xFF, 0x5F, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Make page 6 not present
    let pte_addr_6 = 0x11000 + 6 * 4;
    mem.write_u32_physical(pte_addr_6, 0x00); // not present

    // Write a known sentinel at physical 0x5FFF (the byte on the present page)
    mem.ram[0x5FFF] = 0x42;

    cpu.regs.eax = 0x0000BEEF; // AX = 0xBEEF

    let saved_eip = cpu.regs.eip;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    // Should get a page fault (vector 14) for the second page
    match result {
        RunResult::Exception { vector, .. } => {
            assert_eq!(vector, 14, "Should be page fault for second page");
        }
        other => panic!("Expected page fault on cross-page write, got {:?}", other),
    }

    // EIP should point back at the MOV instruction for restart
    assert_eq!(
        cpu.regs.eip, saved_eip,
        "EIP should point back to the MOV for restart after page fault"
    );

    // The first byte at physical 0x5FFF must NOT have been modified.
    // This verifies translate-before-commit: the emulator must check
    // both pages are writable before committing any bytes.
    assert_eq!(
        mem.ram[0x5FFF], 0x42,
        "First byte at phys 0x5FFF must be unchanged (translate-before-commit)"
    );
}

// ========================================================================
// Paging code-path coverage tests
//
// These exercise the `if paging { ... }` branches for instruction families
// that were previously only tested in real-mode (paging=false).
// ========================================================================

// ------------------------------------------------------------------
// 1. SHL r/m32, imm8 with paging (memory operand, RMW through page table)
// ------------------------------------------------------------------
#[test]
fn test_paging_shl_mem32_imm8() {
    // C1 /4 ib = SHL r/m32, imm8
    // C1 25 00 20 00 00 03 = SHL DWORD PTR [0x2000], 3
    //   ModR/M 25 = mod=00 reg=4(SHL) r/m=5(disp32)
    let code: &[u8] = &[0xC1, 0x25, 0x00, 0x20, 0x00, 0x00, 0x03];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 2 to physical page 0x30
    mem.write_u32_physical(0x11000 + 2 * 4, (0x30 << 12) | 0x07);

    // Write value at physical 0x30000 (linear 0x2000)
    let val: u32 = 0x0000_0005;
    mem.ram[0x30000] = val as u8;
    mem.ram[0x30001] = (val >> 8) as u8;
    mem.ram[0x30002] = (val >> 16) as u8;
    mem.ram[0x30003] = (val >> 24) as u8;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SHL [mem], imm8 with paging should succeed, got {:?}",
        result
    );

    let res = u32::from_le_bytes([
        mem.ram[0x30000],
        mem.ram[0x30001],
        mem.ram[0x30002],
        mem.ram[0x30003],
    ]);
    assert_eq!(res, 0x28, "5 << 3 = 40 = 0x28");
}

// ------------------------------------------------------------------
// 2. SHR r/m32, 1 with paging (D1 /5 memory operand)
// ------------------------------------------------------------------
#[test]
fn test_paging_shr_mem32_1() {
    // D1 2D 00 20 00 00 = SHR DWORD PTR [0x2000], 1
    //   ModR/M 2D = mod=00 reg=5(SHR) r/m=5(disp32)
    let code: &[u8] = &[0xD1, 0x2D, 0x00, 0x20, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    mem.write_u32_physical(0x11000 + 2 * 4, (0x30 << 12) | 0x07);

    let val: u32 = 0x0000_0080;
    mem.ram[0x30000] = val as u8;
    mem.ram[0x30001] = (val >> 8) as u8;
    mem.ram[0x30002] = (val >> 16) as u8;
    mem.ram[0x30003] = (val >> 24) as u8;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SHR [mem], 1 with paging should succeed, got {:?}",
        result
    );

    let res = u32::from_le_bytes([
        mem.ram[0x30000],
        mem.ram[0x30001],
        mem.ram[0x30002],
        mem.ram[0x30003],
    ]);
    assert_eq!(res, 0x40, "0x80 >> 1 = 0x40");
}

// ------------------------------------------------------------------
// 3. SAR r/m8, CL with paging (D2 /7 memory operand)
// ------------------------------------------------------------------
#[test]
fn test_paging_sar_mem8_cl() {
    // D2 3D 00 20 00 00 = SAR BYTE PTR [0x2000], CL
    //   ModR/M 3D = mod=00 reg=7(SAR) r/m=5(disp32)
    let code: &[u8] = &[0xD2, 0x3D, 0x00, 0x20, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    mem.write_u32_physical(0x11000 + 2 * 4, (0x30 << 12) | 0x07);

    mem.ram[0x30000] = 0x80u8; // -128 as signed byte
    cpu.regs.set_cl(2);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SAR [mem], CL with paging should succeed, got {:?}",
        result
    );

    // SAR preserves sign: 0x80 >> 2 = 0xE0 (-128 / 4 = -32)
    assert_eq!(mem.ram[0x30000], 0xE0, "SAR 0x80, 2 = 0xE0");
}

// ------------------------------------------------------------------
// 4. SHL r/m16, imm8 with paging (66 C1 /4 memory operand, 16-bit)
// ------------------------------------------------------------------
#[test]
fn test_paging_shl_mem16_imm8() {
    // In 32-bit mode: 66 C1 25 00 20 00 00 02 = SHL WORD PTR [0x2000], 2
    let code: &[u8] = &[0x66, 0xC1, 0x25, 0x00, 0x20, 0x00, 0x00, 0x02];
    let (mut cpu, mut mem) = setup_paging(code);

    mem.write_u32_physical(0x11000 + 2 * 4, (0x30 << 12) | 0x07);

    mem.ram[0x30000] = 0x0A;
    mem.ram[0x30001] = 0x00; // value = 10

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SHL WORD [mem], imm8 with paging should succeed, got {:?}",
        result
    );

    let res = u16::from_le_bytes([mem.ram[0x30000], mem.ram[0x30001]]);
    assert_eq!(res, 40, "10 << 2 = 40");
}

// ------------------------------------------------------------------
// 5. CALL near rel32 with paging — push return addr through page table
// ------------------------------------------------------------------
#[test]
fn test_paging_call_near_rel32() {
    // E8 05 00 00 00 = CALL +5 (call to EIP+5+5=EIP+10)
    // followed by: 90 90 90 90 90 = 5 NOPs (call target)
    // followed by: C3 = RET
    let code: &[u8] = &[
        0xE8, 0x05, 0x00, 0x00, 0x00, // CALL +5
        0x90, 0x90, 0x90, 0x90, 0x90, // 5 NOPs (skipped)
        0xC3, // RET (call lands here)
    ];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap stack page 7 to physical page 0x40
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);

    let mut io = ();
    // CALL pushes return addr, RET pops it
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "CALL+RET with paging should succeed, got {:?}",
        result
    );

    // After CALL+RET, EIP should be at 0x0105 (the instruction after CALL)
    assert_eq!(
        cpu.regs.eip, 0x0105,
        "After CALL+RET, EIP should be at instruction after CALL"
    );
    assert_eq!(
        cpu.regs.esp, 0x8000,
        "ESP should be restored after CALL+RET"
    );

    // Verify the return address was pushed to the remapped physical page.
    // ESP was 0x8000, push decrements to 0x7FFC (in page 7).
    // Page 7 (linear 0x7000-0x7FFF) maps to physical page 0x40 (0x40000-0x40FFF).
    // Linear 0x7FFC -> physical 0x40FFC.
    let ret_addr = u32::from_le_bytes([
        mem.ram[0x40FFC],
        mem.ram[0x40FFD],
        mem.ram[0x40FFE],
        mem.ram[0x40FFF],
    ]);
    assert_eq!(
        ret_addr, 0x0105,
        "Return address on remapped stack should be 0x0105"
    );
}

// ------------------------------------------------------------------
// 6. RET near with paging — pop return address through page table
// ------------------------------------------------------------------
#[test]
fn test_paging_ret_near() {
    // C3 = RET near
    let code: &[u8] = &[0xC3];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap stack page 7 to physical page 0x40
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);

    // Pre-push a return address at ESP (0x8000 - no decrement, stack already there)
    // Actually we need to put return value at current ESP = 0x8000 (stack page 8).
    // Remap page 8 to physical page 0x50 instead.
    mem.write_u32_physical(0x11000 + 8 * 4, (0x50 << 12) | 0x07);

    // Write return address 0xBEEF at physical location that linear 0x8000 maps to
    let ret_target: u32 = 0x0200;
    mem.ram[0x50000] = ret_target as u8;
    mem.ram[0x50001] = (ret_target >> 8) as u8;
    mem.ram[0x50002] = (ret_target >> 16) as u8;
    mem.ram[0x50003] = (ret_target >> 24) as u8;

    // Put a NOP at the return target so the test doesn't explode
    mem.ram[0x0200] = 0x90;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "RET near with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.eip, 0x0200,
        "RET should pop 0x0200 from remapped stack"
    );
    assert_eq!(cpu.regs.esp, 0x8004, "ESP should advance by 4 after RET");
}

// ------------------------------------------------------------------
// 7. RET near imm16 with paging (C2)
// ------------------------------------------------------------------
#[test]
fn test_paging_ret_near_imm16() {
    // C2 08 00 = RET 8 (pop return addr + add 8 to ESP)
    let code: &[u8] = &[0xC2, 0x08, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap stack page 8 to physical page 0x50
    mem.write_u32_physical(0x11000 + 8 * 4, (0x50 << 12) | 0x07);

    // Write return address at linear 0x8000 = physical 0x50000
    let ret_target: u32 = 0x0300;
    mem.ram[0x50000] = ret_target as u8;
    mem.ram[0x50001] = (ret_target >> 8) as u8;
    mem.ram[0x50002] = (ret_target >> 16) as u8;
    mem.ram[0x50003] = (ret_target >> 24) as u8;

    mem.ram[0x0300] = 0x90; // NOP at return target

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "RET imm16 with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.eip, 0x0300,
        "RET should pop 0x0300 from remapped stack"
    );
    assert_eq!(
        cpu.regs.esp, 0x800C,
        "ESP should advance by 4 (ret addr) + 8 (imm16) = 12"
    );
}

// ------------------------------------------------------------------
// 8. PUSH EAX / POP EBX round-trip through non-identity page mapping
// ------------------------------------------------------------------
#[test]
fn test_paging_push_pop_non_identity() {
    // 50 = PUSH EAX
    // 5B = POP EBX
    let (mut cpu, mut mem) = setup_paging(&[0x50, 0x5B]);

    // Remap stack pages to non-identity
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);

    cpu.regs.eax = 0x12345678;
    cpu.regs.ebx = 0;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "PUSH+POP with non-identity paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.ebx, 0x12345678,
        "POP should retrieve value pushed through non-identity mapped page"
    );
    assert_eq!(cpu.regs.esp, 0x8000, "ESP restored after PUSH+POP");
}

// ------------------------------------------------------------------
// 9. PUSH imm32 (0x68) with paging
// ------------------------------------------------------------------
#[test]
fn test_paging_push_imm32() {
    // 68 78 56 34 12 = PUSH 0x12345678
    // 5B = POP EBX
    let code: &[u8] = &[0x68, 0x78, 0x56, 0x34, 0x12, 0x5B];
    let (mut cpu, mut mem) = setup_paging(code);

    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);

    cpu.regs.ebx = 0;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "PUSH imm32 with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.ebx, 0x12345678,
        "POP should retrieve PUSH imm32 value from non-identity mapped page"
    );
}

// ------------------------------------------------------------------
// 10. PUSHAD / POPAD with paging
// ------------------------------------------------------------------
#[test]
fn test_paging_pushad_popad() {
    // 60 = PUSHAD
    // 61 = POPAD
    let (mut cpu, mut mem) = setup_paging(&[0x60, 0x61]);

    // Remap stack pages
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);

    cpu.regs.eax = 0xAAAA_AAAA;
    cpu.regs.ecx = 0xBBBB_BBBB;
    cpu.regs.edx = 0xCCCC_CCCC;
    cpu.regs.ebx = 0xDDDD_DDDD;
    cpu.regs.ebp = 0xEEEE_EEEE;
    cpu.regs.esi = 0x1111_1111;
    cpu.regs.edi = 0x2222_2222;

    let saved_esp = cpu.regs.esp;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "PUSHAD+POPAD with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.eax, 0xAAAA_AAAA,
        "EAX preserved across PUSHAD/POPAD"
    );
    assert_eq!(
        cpu.regs.ecx, 0xBBBB_BBBB,
        "ECX preserved across PUSHAD/POPAD"
    );
    assert_eq!(
        cpu.regs.edx, 0xCCCC_CCCC,
        "EDX preserved across PUSHAD/POPAD"
    );
    assert_eq!(
        cpu.regs.ebx, 0xDDDD_DDDD,
        "EBX preserved across PUSHAD/POPAD"
    );
    assert_eq!(
        cpu.regs.ebp, 0xEEEE_EEEE,
        "EBP preserved across PUSHAD/POPAD"
    );
    assert_eq!(
        cpu.regs.esi, 0x1111_1111,
        "ESI preserved across PUSHAD/POPAD"
    );
    assert_eq!(
        cpu.regs.edi, 0x2222_2222,
        "EDI preserved across PUSHAD/POPAD"
    );
    assert_eq!(cpu.regs.esp, saved_esp, "ESP restored after PUSHAD/POPAD");
}

// ------------------------------------------------------------------
// 11. REP STOSB with paging — fill through page translation
// ------------------------------------------------------------------
#[test]
fn test_paging_rep_stosb() {
    // F3 AA = REP STOSB
    let code: &[u8] = &[0xF3, 0xAA];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 5 to physical page 0x30
    mem.write_u32_physical(0x11000 + 5 * 4, (0x30 << 12) | 0x07);

    cpu.regs.edi = 0x5000; // destination in remapped page
    cpu.regs.ecx = 8; // fill 8 bytes
    cpu.regs.set_al(0x42); // fill value
    cpu.regs.flags.remove(Flags::DIRECTION); // forward

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 8);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 8 }),
        "REP STOSB with paging should succeed, got {:?}",
        result
    );

    // Verify bytes written to the remapped physical page
    for i in 0..8 {
        assert_eq!(
            mem.ram[0x30000 + i],
            0x42,
            "Byte {} at phys 0x{:05X} should be 0x42",
            i,
            0x30000 + i
        );
    }
    assert_eq!(cpu.regs.ecx, 0, "ECX should be 0 after REP STOSB completes");
    assert_eq!(cpu.regs.edi, 0x5008, "EDI should advance by 8");
}

// ------------------------------------------------------------------
// 12. LODSB with paging — load byte from translated address
// ------------------------------------------------------------------
#[test]
fn test_paging_lodsb() {
    // AC = LODSB
    let code: &[u8] = &[0xAC];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 3 to physical page 0x50
    mem.write_u32_physical(0x11000 + 3 * 4, (0x50 << 12) | 0x07);

    // Write test byte at physical 0x50100 (linear 0x3100)
    mem.ram[0x50100] = 0x77;

    cpu.regs.esi = 0x3100;
    cpu.regs.set_al(0);
    cpu.regs.flags.remove(Flags::DIRECTION);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "LODSB with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.al(),
        0x77,
        "AL should contain byte from remapped page"
    );
    assert_eq!(cpu.regs.esi, 0x3101, "ESI should advance by 1");
}

// ------------------------------------------------------------------
// 13. LODSD with paging — load dword from translated address
// ------------------------------------------------------------------
#[test]
fn test_paging_lodsd() {
    // AD = LODSD (in 32-bit mode)
    let code: &[u8] = &[0xAD];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 4 to physical page 0x60
    mem.write_u32_physical(0x11000 + 4 * 4, (0x60 << 12) | 0x07);

    // Write dword at physical 0x60200 (linear 0x4200)
    mem.ram[0x60200] = 0x11;
    mem.ram[0x60201] = 0x22;
    mem.ram[0x60202] = 0x33;
    mem.ram[0x60203] = 0x44;

    cpu.regs.esi = 0x4200;
    cpu.regs.eax = 0;
    cpu.regs.flags.remove(Flags::DIRECTION);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "LODSD with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.eax, 0x44332211,
        "EAX should contain dword from remapped page"
    );
    assert_eq!(cpu.regs.esi, 0x4204, "ESI should advance by 4");
}

// ------------------------------------------------------------------
// 14. SCASB with paging — scan byte through page translation
// ------------------------------------------------------------------
#[test]
fn test_paging_scasb() {
    // AE = SCASB (compare AL with [ES:EDI])
    let code: &[u8] = &[0xAE];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 5 to physical page 0x70
    mem.write_u32_physical(0x11000 + 5 * 4, (0x70 << 12) | 0x07);

    // Write test byte at physical 0x70000 (linear 0x5000)
    mem.ram[0x70000] = 0x42;

    cpu.regs.edi = 0x5000;
    cpu.regs.set_al(0x42); // same value -> ZF=1
    cpu.regs.flags.remove(Flags::DIRECTION);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "SCASB with paging should succeed, got {:?}",
        result
    );

    assert_eq!(cpu.regs.edi, 0x5001, "EDI should advance by 1");
    // ZF should be set since AL == [ES:EDI]
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be set when AL matches memory byte"
    );
}

// ------------------------------------------------------------------
// 15. CMPSB with paging — compare bytes through page translation
// ------------------------------------------------------------------
#[test]
fn test_paging_cmpsb() {
    // A6 = CMPSB (compare [DS:ESI] with [ES:EDI])
    let code: &[u8] = &[0xA6];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 3 to physical page 0x50 (source)
    mem.write_u32_physical(0x11000 + 3 * 4, (0x50 << 12) | 0x07);
    // Remap page 4 to physical page 0x60 (destination)
    mem.write_u32_physical(0x11000 + 4 * 4, (0x60 << 12) | 0x07);

    // Same byte at both locations -> ZF=1
    mem.ram[0x50000] = 0xAB;
    mem.ram[0x60000] = 0xAB;

    cpu.regs.esi = 0x3000;
    cpu.regs.edi = 0x4000;
    cpu.regs.flags.remove(Flags::DIRECTION);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CMPSB with paging should succeed, got {:?}",
        result
    );

    assert_eq!(cpu.regs.esi, 0x3001, "ESI should advance by 1");
    assert_eq!(cpu.regs.edi, 0x4001, "EDI should advance by 1");
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be set when bytes match"
    );
}

// ------------------------------------------------------------------
// 16. CMPSB with unequal bytes through paging
// ------------------------------------------------------------------
#[test]
fn test_paging_cmpsb_not_equal() {
    let code: &[u8] = &[0xA6];
    let (mut cpu, mut mem) = setup_paging(code);

    mem.write_u32_physical(0x11000 + 3 * 4, (0x50 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 4 * 4, (0x60 << 12) | 0x07);

    mem.ram[0x50000] = 0x10;
    mem.ram[0x60000] = 0x20; // different

    cpu.regs.esi = 0x3000;
    cpu.regs.edi = 0x4000;
    cpu.regs.flags.remove(Flags::DIRECTION);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CMPSB (not equal) with paging should succeed, got {:?}",
        result
    );

    assert!(
        !cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be clear when bytes differ"
    );
}

// ------------------------------------------------------------------
// 17. MOV [mem], EAX with paging (memory write through page table)
// ------------------------------------------------------------------
#[test]
fn test_paging_mov_mem_eax() {
    // A3 00 30 00 00 = MOV [0x3000], EAX
    let code: &[u8] = &[0xA3, 0x00, 0x30, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 3 to physical page 0x60
    mem.write_u32_physical(0x11000 + 3 * 4, (0x60 << 12) | 0x07);

    cpu.regs.eax = 0xDEADBEEF;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "MOV [mem], EAX with paging should succeed, got {:?}",
        result
    );

    let written = u32::from_le_bytes([
        mem.ram[0x60000],
        mem.ram[0x60001],
        mem.ram[0x60002],
        mem.ram[0x60003],
    ]);
    assert_eq!(
        written, 0xDEADBEEF,
        "Value should be written to the remapped physical page"
    );
}

// ------------------------------------------------------------------
// 18. MOV EAX, [mem] with paging (memory read through page table)
// ------------------------------------------------------------------
#[test]
fn test_paging_mov_eax_mem() {
    // A1 00 40 00 00 = MOV EAX, [0x4000]
    let code: &[u8] = &[0xA1, 0x00, 0x40, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 4 to physical page 0x70
    mem.write_u32_physical(0x11000 + 4 * 4, (0x70 << 12) | 0x07);

    mem.ram[0x70000] = 0xEF;
    mem.ram[0x70001] = 0xBE;
    mem.ram[0x70002] = 0xAD;
    mem.ram[0x70003] = 0xDE;

    cpu.regs.eax = 0;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "MOV EAX, [mem] with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.eax, 0xDEADBEEF,
        "EAX should contain value from remapped physical page"
    );
}

// ------------------------------------------------------------------
// 19. LEAVE (0xC9) with paging — restores EBP from paged stack
// ------------------------------------------------------------------
#[test]
fn test_paging_leave() {
    // C9 = LEAVE (ESP = EBP; POP EBP)
    let code: &[u8] = &[0xC9];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap stack page 6 to physical page 0x50
    mem.write_u32_physical(0x11000 + 6 * 4, (0x50 << 12) | 0x07);

    // Set EBP to point into remapped page; LEAVE will set ESP = EBP, then pop
    cpu.regs.ebp = 0x6100;
    cpu.regs.esp = 0x6000; // arbitrary, LEAVE overwrites it

    // Write old EBP value at physical 0x50100 (linear 0x6100)
    let old_ebp: u32 = 0xCAFEBABE;
    mem.ram[0x50100] = old_ebp as u8;
    mem.ram[0x50101] = (old_ebp >> 8) as u8;
    mem.ram[0x50102] = (old_ebp >> 16) as u8;
    mem.ram[0x50103] = (old_ebp >> 24) as u8;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "LEAVE with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.ebp, 0xCAFEBABE,
        "EBP should be popped from remapped stack page"
    );
    assert_eq!(cpu.regs.esp, 0x6104, "ESP should be EBP + 4 after LEAVE");
}

#[test]
fn paging_pop_memory_fault_restores_16_and_32_bit_stack_pointers() {
    for (name, code) in [
        ("POP r/m32", &[0x8F, 0x05, 0x00, 0x40, 0x00, 0x00][..]),
        ("POP r/m16", &[0x66, 0x8F, 0x05, 0x00, 0x40, 0x00, 0x00][..]),
    ] {
        let (mut cpu, mut mem) = setup_paging(code);
        mem.write_u32_physical(0x11000 + 4 * 4, 0);
        mem.invalidate_tlb();
        let entry_esp = cpu.regs.esp;
        mem.write_u32(entry_esp as usize, 0x1122_3344);

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 14,
                    executed: 0,
                    ..
                }
            ),
            "{name} must #PF on its unmapped destination, got {result:?}"
        );
        assert_eq!(cpu.regs.esp, entry_esp, "{name} must restore entry ESP");
    }
}

#[test]
fn paging_popa_late_fault_preserves_all_registers() {
    for (name, code, entry_esp, frame_bytes) in [
        ("POPAD", &[0x61][..], 0x0000_3FF0, 32usize),
        ("POPA", &[0x66, 0x61][..], 0x0000_3FF8, 16usize),
    ] {
        let (mut cpu, mut mem) = setup_paging(code);
        cpu.regs.eax = 0xA0A0_0001;
        cpu.regs.ecx = 0xC0C0_0002;
        cpu.regs.edx = 0xD0D0_0003;
        cpu.regs.ebx = 0xB0B0_0004;
        cpu.regs.ebp = 0xB0B0_0005;
        cpu.regs.esi = 0x5151_0006;
        cpu.regs.edi = 0xD1D1_0007;
        cpu.regs.esp = entry_esp;
        let entry_eip = cpu.regs.eip;
        let entry_gprs = [
            cpu.regs.eax,
            cpu.regs.ecx,
            cpu.regs.edx,
            cpu.regs.ebx,
            cpu.regs.ebp,
            cpu.regs.esi,
            cpu.regs.edi,
        ];

        for offset in 0..frame_bytes {
            mem.ram[entry_esp as usize + offset] = 0x10u8.wrapping_add(offset as u8);
        }
        // The first four frame items are readable at the end of page 3.
        // The fifth item (BX/EBX) starts on non-present page 4.
        mem.write_u32_physical(0x11000 + 4 * 4, 0);
        mem.invalidate_tlb();

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 14,
                    fault_addr: Some(0x4000),
                    executed: 0,
                    ..
                }
            ),
            "{name} must #PF on its late frame read, got {result:?}"
        );
        assert_eq!(cpu.regs.eip, entry_eip, "{name} must restore entry EIP");
        assert_eq!(cpu.regs.esp, entry_esp, "{name} must restore entry ESP");
        assert_eq!(
            [
                cpu.regs.eax,
                cpu.regs.ecx,
                cpu.regs.edx,
                cpu.regs.ebx,
                cpu.regs.ebp,
                cpu.regs.esi,
                cpu.regs.edi,
            ],
            entry_gprs,
            "{name} must not publish a partial register frame"
        );
    }
}

#[test]
fn paging_leave_fault_restores_16_and_32_bit_stack_pointers() {
    for (name, code, frame_pointer) in [
        ("LEAVE32", &[0xC9][..], 0x0000_4000),
        ("LEAVE16", &[0x66, 0xC9][..], 0xABCD_4000),
    ] {
        let (mut cpu, mut mem) = setup_paging(code);
        mem.write_u32_physical(0x11000 + 4 * 4, 0);
        mem.invalidate_tlb();
        cpu.regs.ebp = frame_pointer;
        let entry_esp = cpu.regs.esp;

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 14,
                    executed: 0,
                    ..
                }
            ),
            "{name} must #PF on its unmapped stack read, got {result:?}"
        );
        assert_eq!(cpu.regs.esp, entry_esp, "{name} must restore entry ESP");
        assert_eq!(cpu.regs.ebp, frame_pointer, "{name} must preserve EBP");
    }
}

// ------------------------------------------------------------------
// 20. REP MOVSD with paging — copy dwords through page translation
// ------------------------------------------------------------------
#[test]
fn test_paging_rep_movsd() {
    // F3 A5 = REP MOVSD
    let code: &[u8] = &[0xF3, 0xA5];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap source page 3 to physical page 0x50
    mem.write_u32_physical(0x11000 + 3 * 4, (0x50 << 12) | 0x07);
    // Remap dest page 5 to physical page 0x60
    mem.write_u32_physical(0x11000 + 5 * 4, (0x60 << 12) | 0x07);

    // Write 3 dwords at source
    for i in 0u32..3 {
        let val = 0x11111111 * (i + 1);
        let off = (i * 4) as usize;
        mem.ram[0x50000 + off] = val as u8;
        mem.ram[0x50001 + off] = (val >> 8) as u8;
        mem.ram[0x50002 + off] = (val >> 16) as u8;
        mem.ram[0x50003 + off] = (val >> 24) as u8;
    }

    cpu.regs.esi = 0x3000;
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 3;
    cpu.regs.flags.remove(Flags::DIRECTION);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 3);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 3 }),
        "REP MOVSD with paging should succeed, got {:?}",
        result
    );

    // Verify destination (physical page 0x60)
    for i in 0u32..3 {
        let off = (i * 4) as usize;
        let val = u32::from_le_bytes([
            mem.ram[0x60000 + off],
            mem.ram[0x60001 + off],
            mem.ram[0x60002 + off],
            mem.ram[0x60003 + off],
        ]);
        let expected = 0x11111111 * (i + 1);
        assert_eq!(val, expected, "DWORD {} should be {:08X}", i, expected);
    }

    assert_eq!(cpu.regs.ecx, 0, "ECX should be 0 after REP MOVSD completes");
    assert_eq!(cpu.regs.esi, 0x300C, "ESI should advance by 12");
    assert_eq!(cpu.regs.edi, 0x500C, "EDI should advance by 12");
}

// ------------------------------------------------------------------
// 21. ENTER with paging — create stack frame through page tables
// ------------------------------------------------------------------
#[test]
fn test_paging_enter() {
    // C8 10 00 00 = ENTER 0x10, 0 (allocate 16 bytes, nesting level 0)
    let code: &[u8] = &[0xC8, 0x10, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap stack page 7 to physical page 0x40
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);

    let saved_ebp = cpu.regs.ebp;
    let saved_esp = cpu.regs.esp;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "ENTER with paging should succeed, got {:?}",
        result
    );

    // ENTER 0x10, 0: push EBP, EBP = ESP, ESP -= 0x10
    // After push: ESP = saved_esp - 4
    // EBP = ESP after push = saved_esp - 4
    // Then ESP -= 0x10
    assert_eq!(
        cpu.regs.ebp,
        saved_esp - 4,
        "EBP should point to pushed frame pointer"
    );
    assert_eq!(
        cpu.regs.esp,
        saved_esp - 4 - 0x10,
        "ESP should be decremented by 4 (push) + 0x10 (locals)"
    );

    // Verify old EBP was pushed to the remapped page
    // The push at linear 0x7FFC -> physical 0x40000 - 4 = 0x3FFFC
    let pushed_ebp = u32::from_le_bytes([
        mem.ram[0x3FFFC],
        mem.ram[0x3FFFD],
        mem.ram[0x3FFFE],
        mem.ram[0x3FFFF],
    ]);
    assert_eq!(
        pushed_ebp, saved_ebp,
        "Old EBP should be pushed to remapped stack"
    );
}

// ------------------------------------------------------------------
// 22. XLAT (0xD7) with paging — table lookup through page table
// ------------------------------------------------------------------
#[test]
fn test_paging_xlat() {
    // D7 = XLAT (AL = [DS:EBX + AL])
    let code: &[u8] = &[0xD7];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 4 to physical page 0x80
    mem.write_u32_physical(0x11000 + 4 * 4, (0x80 << 12) | 0x07);

    // Table base at linear 0x4000, index by AL
    cpu.regs.ebx = 0x4000;
    cpu.regs.set_al(0x05);

    // Write lookup value at physical 0x80005 (linear 0x4005)
    mem.ram[0x80005] = 0xCC;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "XLAT with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.al(),
        0xCC,
        "AL should contain table value from remapped page"
    );
}

// ------------------------------------------------------------------
// 23. STOSW (16-bit) with paging — store word through page table
// ------------------------------------------------------------------
#[test]
fn test_paging_stosw() {
    // 66 AB = STOSW (16-bit operand override in 32-bit mode)
    let code: &[u8] = &[0x66, 0xAB];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 5 to physical page 0x70
    mem.write_u32_physical(0x11000 + 5 * 4, (0x70 << 12) | 0x07);

    cpu.regs.edi = 0x5000;
    cpu.regs.eax = 0x0000_BEEF; // AX = 0xBEEF
    cpu.regs.flags.remove(Flags::DIRECTION);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "STOSW with paging should succeed, got {:?}",
        result
    );

    assert_eq!(mem.ram[0x70000], 0xEF, "Low byte of STOSW at phys 0x70000");
    assert_eq!(mem.ram[0x70001], 0xBE, "High byte of STOSW at phys 0x70001");
    assert_eq!(cpu.regs.edi, 0x5002, "EDI should advance by 2");
}

// ------------------------------------------------------------------
// 24. MOV r8, [mem] with paging (8A /r)
// ------------------------------------------------------------------
#[test]
fn test_paging_mov_r8_mem() {
    // 8A 05 00 30 00 00 = MOV AL, [0x3000]
    //   ModR/M 05 = mod=00 reg=AL r/m=5(disp32)
    let code: &[u8] = &[0x8A, 0x05, 0x00, 0x30, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 3 to physical page 0x50
    mem.write_u32_physical(0x11000 + 3 * 4, (0x50 << 12) | 0x07);

    mem.ram[0x50000] = 0x99;
    cpu.regs.set_al(0);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "MOV AL, [mem] with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.al(),
        0x99,
        "AL should contain byte read from remapped page"
    );
}

// ------------------------------------------------------------------
// 25. MOV [mem], r8 with paging (88 /r)
// ------------------------------------------------------------------
#[test]
fn test_paging_mov_mem_r8() {
    // 88 05 00 30 00 00 = MOV [0x3000], AL
    //   ModR/M 05 = mod=00 reg=AL r/m=5(disp32)
    let code: &[u8] = &[0x88, 0x05, 0x00, 0x30, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 3 to physical page 0x50
    mem.write_u32_physical(0x11000 + 3 * 4, (0x50 << 12) | 0x07);

    cpu.regs.set_al(0xDD);
    mem.ram[0x50000] = 0x00;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "MOV [mem], AL with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        mem.ram[0x50000], 0xDD,
        "Byte should be written to remapped physical page"
    );
}

// ------------------------------------------------------------------
// 25a. MOV r8, [mem] warm-TLB: second byte load from the same page
// exercises the TLB read-hit fast path (EXP-B005).
// ------------------------------------------------------------------
#[test]
fn test_paging_mov_r8_mem_warm_tlb_hit() {
    // 8A 05 00 30 00 00 = MOV AL, [0x3000]   (fills the TLB entry)
    // 8A 1D 01 30 00 00 = MOV BL, [0x3001]   (TLB read hit, same page)
    let code: &[u8] = &[
        0x8A, 0x05, 0x00, 0x30, 0x00, 0x00, //
        0x8A, 0x1D, 0x01, 0x30, 0x00, 0x00,
    ];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap linear page 3 to physical page 0x50
    mem.write_u32_physical(0x11000 + 3 * 4, (0x50 << 12) | 0x07);
    mem.ram[0x50000] = 0x99;
    mem.ram[0x50001] = 0x77;
    cpu.regs.set_al(0);
    cpu.regs.set_bl(0);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "two byte loads with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.al(),
        0x99,
        "cold load (TLB fill) reads remapped page"
    );
    assert_eq!(
        cpu.regs.bl(),
        0x77,
        "warm load (TLB read hit) reads remapped page"
    );
}

// ------------------------------------------------------------------
// 25b. MOV [mem], r8 warm-TLB: second byte store to the same page
// exercises the TLB write-hit fast path (EXP-B005). The first store
// misses (page walk sets the PTE dirty bit and fills host_write).
// ------------------------------------------------------------------
#[test]
fn test_paging_mov_mem_r8_warm_tlb_hit() {
    // 88 05 00 30 00 00 = MOV [0x3000], AL   (miss -> walk -> fill)
    // 88 1D 01 30 00 00 = MOV [0x3001], BL   (TLB write hit, same page)
    let code: &[u8] = &[
        0x88, 0x05, 0x00, 0x30, 0x00, 0x00, //
        0x88, 0x1D, 0x01, 0x30, 0x00, 0x00,
    ];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap linear page 3 to physical page 0x50
    mem.write_u32_physical(0x11000 + 3 * 4, (0x50 << 12) | 0x07);
    cpu.regs.set_al(0xDD);
    cpu.regs.set_bl(0xEE);
    mem.ram[0x50000] = 0;
    mem.ram[0x50001] = 0;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "two byte stores with paging should succeed, got {:?}",
        result
    );

    assert_eq!(mem.ram[0x50000], 0xDD, "cold store lands on remapped page");
    assert_eq!(
        mem.ram[0x50001], 0xEE,
        "warm store (TLB write hit) lands on remapped page"
    );
}

// ------------------------------------------------------------------
// 25b-2. Byte stores to the BIOS F-segment (0xF0000..0x100000) must be
// silently dropped — real hardware ROM semantics — on BOTH the cold
// (miss) and warm (TLB write hit) paths. Found by the EXP-B005
// adversarial panel: without the fill-time exclusion, the page walk
// published a valid host_write for F-seg pages and the warm store
// landed in the HLE BIOS image.
// ------------------------------------------------------------------
#[test]
fn test_paging_mov_mem_r8_bios_fseg_dropped_warm_tlb() {
    // 88 05 00 00 0F 00 = MOV [0xF0000], AL   (cold: miss -> drop)
    // 88 1D 01 00 0F 00 = MOV [0xF0001], BL   (warm: must ALSO drop)
    let code: &[u8] = &[
        0x88, 0x05, 0x00, 0x00, 0x0F, 0x00, //
        0x88, 0x1D, 0x01, 0x00, 0x0F, 0x00,
    ];
    let (mut cpu, mut mem) = setup_paging(code);
    assert!(
        !mem.allow_rom_writes(),
        "default boot must disallow ROM writes"
    );

    mem.ram[0xF0000] = 0x11;
    mem.ram[0xF0001] = 0x22;
    cpu.regs.set_al(0xDD);
    cpu.regs.set_bl(0xEE);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "F-seg byte stores should execute (as drops), got {:?}",
        result
    );

    assert_eq!(
        mem.ram[0xF0000], 0x11,
        "cold F-seg byte store must be dropped"
    );
    assert_eq!(
        mem.ram[0xF0001], 0x22,
        "warm (TLB write hit) F-seg byte store must be dropped"
    );
}

// ------------------------------------------------------------------
// 25b-3. Same pin for the u32 store path (translate_write_u32!'s
// TLB-hit arm) — the pre-existing sibling of the u8 hole, closed by
// the same fill-time exclusion.
// ------------------------------------------------------------------
#[test]
fn test_paging_mov_mem_r32_bios_fseg_dropped_warm_tlb() {
    // 89 05 00 00 0F 00 = MOV [0xF0000], EAX  (cold: miss -> drop)
    // 89 1D 04 00 0F 00 = MOV [0xF0004], EBX  (warm: must ALSO drop)
    let code: &[u8] = &[
        0x89, 0x05, 0x00, 0x00, 0x0F, 0x00, //
        0x89, 0x1D, 0x04, 0x00, 0x0F, 0x00,
    ];
    let (mut cpu, mut mem) = setup_paging(code);
    assert!(
        !mem.allow_rom_writes(),
        "default boot must disallow ROM writes"
    );

    for i in 0..8 {
        mem.ram[0xF0000 + i] = 0x10 + i as u8;
    }
    cpu.regs.eax = 0xDEAD_BEEF;
    cpu.regs.ebx = 0xCAFE_F00D;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "F-seg dword stores should execute (as drops), got {:?}",
        result
    );

    for i in 0..8 {
        assert_eq!(
            mem.ram[0xF0000 + i],
            0x10 + i as u8,
            "F-seg dword store byte {i} must be dropped (cold and warm)"
        );
    }
}

// ------------------------------------------------------------------
// 25c. MOV [mem], r8 to the VGA aperture in a PLANAR mode, warm TLB:
// VGA pages get null host pointers at TLB fill, so byte stores must
// route through Memory's planar write pipeline every time — a u8
// TLB fast path must never capture them (EXP-B005 soundness pin).
// ------------------------------------------------------------------
#[test]
fn test_paging_mov_mem_r8_vga_planar_routes_to_planes_warm() {
    use gp64_memory::VgaWriteHandler;
    // 88 05 00 00 0A 00 = MOV [0xA0000], AL
    // 88 1D 01 00 0A 00 = MOV [0xA0001], BL  (same VGA page, "warm")
    let code: &[u8] = &[
        0x88, 0x05, 0x00, 0x00, 0x0A, 0x00, //
        0x88, 0x1D, 0x01, 0x00, 0x0A, 0x00,
    ];
    let (mut cpu, mut mem) = setup_paging(code);

    // Mode 0x12 planar write configuration: write mode 0, planes 0+2
    // enabled, bit mask 0xFF so latches don't contribute.
    mem.vga.video_mode = 0x12;
    mem.vga.chain4 = false;
    mem.vga.plane_mask = 0x05;
    mem.vga.write_mode = 0;
    mem.vga.bit_mask = 0xFF;
    mem.vga.enable_set_reset = 0;
    mem.vga.set_reset = 0;
    mem.vga.data_rotate = 0;
    mem.vga.memory_map_select = 1; // A0000-AFFFF
    mem.update_vga_write_handler();
    assert_eq!(mem.vga_write_handler(), VgaWriteHandler::Planar);

    cpu.regs.set_al(0xAA);
    cpu.regs.set_bl(0x55);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "byte stores to VGA aperture should succeed, got {:?}",
        result
    );

    assert_eq!(mem.ram[0xA0000], 0, "planar store must not hit raw RAM");
    assert_eq!(
        mem.ram[0xA0001], 0,
        "warm planar store must not hit raw RAM"
    );
    assert_eq!(mem.plane(0)[0], 0xAA, "plane 0 gets first byte");
    assert_eq!(mem.plane(2)[0], 0xAA, "plane 2 gets first byte");
    assert_eq!(mem.plane(1)[0], 0, "plane 1 masked off");
    assert_eq!(mem.plane(3)[0], 0, "plane 3 masked off");
    assert_eq!(mem.plane(0)[1], 0x55, "plane 0 gets warm-store byte");
    assert_eq!(mem.plane(2)[1], 0x55, "plane 2 gets warm-store byte");
}

// ------------------------------------------------------------------
// 25d. SMC notification fires for BOTH byte stores (cold fill and
// warm TLB hit) through the u8 fast path.
// ------------------------------------------------------------------
#[test]
fn test_paging_smc_notification_mov_mem_r8_warm_tlb() {
    use gp64_types::SmcNotify;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingNotify {
        writes: Mutex<Vec<(usize, usize)>>,
    }
    impl SmcNotify for RecordingNotify {
        fn notify_write(&self, address: usize, size: usize) {
            self.writes.lock().unwrap().push((address, size));
        }
        fn notify_write_range(&self, _start: usize, _end: usize) {}
    }

    // 88 05 00 30 00 00 = MOV [0x3000], AL   (miss -> fill)
    // 88 1D 01 30 00 00 = MOV [0x3001], BL   (TLB write hit)
    let code: &[u8] = &[
        0x88, 0x05, 0x00, 0x30, 0x00, 0x00, //
        0x88, 0x1D, 0x01, 0x30, 0x00, 0x00,
    ];
    let (mut cpu, mut mem) = setup_paging(code);
    let notifier = Arc::new(RecordingNotify::default());
    mem.bind_smc(Some(notifier.clone()));
    mem.set_smc_has_code_pages(true);
    // `Memory::smc_before_write` gates on the PHYSICAL page's watch bit, so
    // declare the store's target page (physical == linear here) as a
    // compiled-code page; this test is about the TLB-warm store path firing
    // the notifier, not about the page policy.
    mem.set_page_watched(0x3000 >> 12, true);

    cpu.regs.set_al(0x11);
    cpu.regs.set_bl(0x22);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "byte stores with SMC notifier should succeed, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x3000], 0x11);
    assert_eq!(mem.ram[0x3001], 0x22);

    let writes = notifier.writes.lock().unwrap().clone();
    assert!(
        writes.iter().any(|&(a, s)| a == 0x3000 && s == 1),
        "cold byte store must fire SMC notification, got {:?}",
        writes
    );
    assert!(
        writes.iter().any(|&(a, s)| a == 0x3001 && s == 1),
        "warm (TLB-hit) byte store must fire SMC notification, got {:?}",
        writes
    );
}

// ------------------------------------------------------------------
// 25e. Byte stores to a paged mapping of the HLE EMS frame must go to
// the EMS backing store, never raw RAM — EMS-overlapping pages get
// null host pointers at TLB fill (direct_host_ram_range exclusion),
// so a u8 fast path can never capture them.
// ------------------------------------------------------------------
#[test]
fn test_paging_mov_mem_r8_ems_frame_routes_to_backing() {
    // 88 05 00 50 00 00 = MOV [0x5000], AL
    // 88 1D 01 50 00 00 = MOV [0x5001], BL  (same EMS-aliased page)
    let code: &[u8] = &[
        0x88, 0x05, 0x00, 0x50, 0x00, 0x00, //
        0x88, 0x1D, 0x01, 0x50, 0x00, 0x00,
    ];
    let (mut cpu, mut mem) = setup_paging(code);

    mem.init_ems_pool(1).unwrap();
    mem.claim_hle_ems_frame(EMS_FRAME_BASE, EMS_FRAME_SIZE)
        .unwrap();
    mem.set_ems_frame_page(0, Some(0)).unwrap();

    // Map linear page 5 onto the EMS frame's physical base.
    let pte_addr = 0x11000 + 5 * 4;
    mem.write_u32_physical(pte_addr, (EMS_FRAME_BASE as u32) | 0x07);
    mem.invalidate_tlb();
    // Sentinel bytes in the raw RAM behind the frame must survive.
    mem.ram[EMS_FRAME_BASE] = 0xC0;
    mem.ram[EMS_FRAME_BASE + 1] = 0xC1;

    cpu.regs.set_al(0x5A);
    cpu.regs.set_bl(0xA5);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "byte stores to EMS-aliased page should succeed, got {:?}",
        result
    );

    assert_eq!(mem.read_ems_backing_byte(0, 0).unwrap(), 0x5A);
    assert_eq!(mem.read_ems_backing_byte(0, 1).unwrap(), 0xA5);
    assert_eq!(
        mem.ram[EMS_FRAME_BASE], 0xC0,
        "raw EMS frame RAM must not be written"
    );
    assert_eq!(
        mem.ram[EMS_FRAME_BASE + 1],
        0xC1,
        "raw EMS frame RAM must not be written"
    );
}

// ------------------------------------------------------------------
// 26. ADD [mem], EAX with paging (01 /r — RMW through page table)
// ------------------------------------------------------------------
#[test]
fn test_paging_add_mem_reg() {
    // 01 05 00 20 00 00 = ADD [0x2000], EAX
    //   ModR/M 05 = mod=00 reg=EAX r/m=5(disp32)
    let code: &[u8] = &[0x01, 0x05, 0x00, 0x20, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    mem.write_u32_physical(0x11000 + 2 * 4, (0x30 << 12) | 0x07);

    // Write initial value at physical 0x30000
    let init_val: u32 = 100;
    mem.ram[0x30000] = init_val as u8;
    mem.ram[0x30001] = (init_val >> 8) as u8;
    mem.ram[0x30002] = (init_val >> 16) as u8;
    mem.ram[0x30003] = (init_val >> 24) as u8;

    cpu.regs.eax = 50;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "ADD [mem], EAX with paging should succeed, got {:?}",
        result
    );

    let res = u32::from_le_bytes([
        mem.ram[0x30000],
        mem.ram[0x30001],
        mem.ram[0x30002],
        mem.ram[0x30003],
    ]);
    assert_eq!(res, 150, "100 + 50 = 150");
}

// ------------------------------------------------------------------
// 27. XCHG EAX, [mem] with paging (87 /r — RMW through page table)
// ------------------------------------------------------------------
#[test]
fn test_paging_xchg_reg_mem() {
    // 87 05 00 20 00 00 = XCHG EAX, [0x2000]
    //   ModR/M 05 = mod=00 reg=EAX r/m=5(disp32)
    let code: &[u8] = &[0x87, 0x05, 0x00, 0x20, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    mem.write_u32_physical(0x11000 + 2 * 4, (0x30 << 12) | 0x07);

    // Memory holds 0xAAAAAAAA, EAX holds 0xBBBBBBBB
    let mem_val: u32 = 0xAAAAAAAA;
    mem.ram[0x30000] = mem_val as u8;
    mem.ram[0x30001] = (mem_val >> 8) as u8;
    mem.ram[0x30002] = (mem_val >> 16) as u8;
    mem.ram[0x30003] = (mem_val >> 24) as u8;

    cpu.regs.eax = 0xBBBBBBBB;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "XCHG EAX, [mem] with paging should succeed, got {:?}",
        result
    );

    assert_eq!(cpu.regs.eax, 0xAAAAAAAA, "EAX should have old memory value");
    let new_mem = u32::from_le_bytes([
        mem.ram[0x30000],
        mem.ram[0x30001],
        mem.ram[0x30002],
        mem.ram[0x30003],
    ]);
    assert_eq!(new_mem, 0xBBBBBBBB, "Memory should have old EAX value");
}

// ------------------------------------------------------------------
// 28. PUSH FS / POP FS with paging
// ------------------------------------------------------------------
#[test]
fn test_paging_push_pop_fs() {
    // 0F A0 = PUSH FS
    // 0F A1 = POP FS
    let code: &[u8] = &[0x0F, 0xA0, 0x0F, 0xA1];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap stack page 7 to physical page 0x40
    mem.write_u32_physical(0x11000 + 7 * 4, (0x40 << 12) | 0x07);

    // Set FS to a known value
    cpu.regs.fs = 0x0010; // data selector
    cpu.regs.fs_cache.base = 0;
    cpu.regs.fs_cache.limit = 0xFFFFF;
    cpu.regs.fs_cache.granularity = true;
    cpu.regs.fs_cache.recompute_effective_limit();
    cpu.regs.fs_cache.access = 0x93;
    cpu.regs.fs_cache.is_code = false;
    cpu.regs.fs_cache.big = true;
    cpu.regs.fs_cache.present = true;
    cpu.regs.gdtr.base = 0x1000;
    cpu.regs.gdtr.limit = 0x0017;
    let fs_desc = [0xff, 0xff, 0, 0, 0, 0x93, 0xcf, 0];
    for (offset, byte) in fs_desc.iter().copied().enumerate() {
        mem.write_u8(0x1000 + 0x10 + offset, byte);
    }

    let saved_esp = cpu.regs.esp;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "PUSH FS / POP FS with paging should succeed, got {:?}",
        result
    );

    assert_eq!(
        cpu.regs.fs, 0x0010,
        "FS should be preserved across PUSH/POP"
    );
    assert_eq!(
        cpu.regs.esp, saved_esp,
        "ESP restored after PUSH FS / POP FS"
    );
}

// ------------------------------------------------------------------
// 29. ROL r/m32, 1 with paging (D1 /0 memory operand, rotate through page table)
// ------------------------------------------------------------------
#[test]
fn test_paging_rol_mem32_1() {
    // D1 05 00 20 00 00 = ROL DWORD PTR [0x2000], 1
    //   ModR/M 05 = mod=00 reg=0(ROL) r/m=5(disp32)
    let code: &[u8] = &[0xD1, 0x05, 0x00, 0x20, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_paging(code);

    mem.write_u32_physical(0x11000 + 2 * 4, (0x30 << 12) | 0x07);

    let val: u32 = 0x8000_0001;
    mem.ram[0x30000] = val as u8;
    mem.ram[0x30001] = (val >> 8) as u8;
    mem.ram[0x30002] = (val >> 16) as u8;
    mem.ram[0x30003] = (val >> 24) as u8;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "ROL [mem], 1 with paging should succeed, got {:?}",
        result
    );

    let res = u32::from_le_bytes([
        mem.ram[0x30000],
        mem.ram[0x30001],
        mem.ram[0x30002],
        mem.ram[0x30003],
    ]);
    // ROL 0x80000001, 1 = 0x00000003
    assert_eq!(res, 0x0000_0003, "ROL 0x80000001 by 1 = 0x00000003");
}

// ------------------------------------------------------------------
// 30. REP SCASB with paging — scan for byte, early termination
// ------------------------------------------------------------------
#[test]
fn test_paging_rep_scasb() {
    // F2 AE = REPNE SCASB (scan until [ES:EDI] == AL or ECX == 0)
    // F4 = HLT (stop execution after SCASB terminates)
    let code: &[u8] = &[0xF2, 0xAE, 0xF4];
    let (mut cpu, mut mem) = setup_paging(code);

    // Remap page 5 to physical page 0x70
    mem.write_u32_physical(0x11000 + 5 * 4, (0x70 << 12) | 0x07);

    // Fill destination with known pattern: 0x01, 0x02, 0x03, 0x42, 0x05
    mem.ram[0x70000] = 0x01;
    mem.ram[0x70001] = 0x02;
    mem.ram[0x70002] = 0x03;
    mem.ram[0x70003] = 0x42; // target byte
    mem.ram[0x70004] = 0x05;

    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 10;
    cpu.regs.set_al(0x42); // looking for 0x42
    cpu.regs.flags.remove(Flags::DIRECTION);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 10);
    // REPNE SCASB runs 4 iterations then terminates; HLT stops execution
    assert!(
        matches!(result, RunResult::Halt { .. }),
        "REPNE SCASB + HLT with paging should halt, got {:?}",
        result
    );

    // After finding match at byte 3 (0-indexed), ECX = 10 - 4 = 6
    assert_eq!(
        cpu.regs.ecx, 6,
        "ECX should reflect remaining count after match"
    );
    assert_eq!(
        cpu.regs.edi, 0x5004,
        "EDI should point past the matched byte"
    );
    assert!(
        cpu.regs.flags.contains(Flags::ZERO),
        "ZF should be set when SCASB finds a match"
    );
}

// ========================================================================
// REP INSW/INSD destination fault before the port read (MDD-BUG-FLU-02065).
//
// INS must raise a destination #PF BEFORE it reads the port: the port read
// consumes device data (an ATAPI/IDE data register advances), so a word
// read and then dropped by the fault is lost for good, and the restarted
// instruction reads the NEXT word. Probe the destination first
// ("trigger any segment or page faults before reading from IO port"), as
// our INSB already does. Page 0x41 is not present in each test.
// ========================================================================

/// Port that hands out 1, 2, 3, ... and counts its reads.
struct CountingPort {
    reads: u32,
}

impl IoPort for CountingPort {
    fn read_u8(&mut self, _port: u16) -> u8 {
        self.reads += 1;
        self.reads as u8
    }
    fn read_u16(&mut self, _port: u16) -> u16 {
        self.reads += 1;
        self.reads as u16
    }
    fn read_u32(&mut self, _port: u16) -> u32 {
        self.reads += 1;
        self.reads
    }
    fn write_u8(&mut self, _port: u16, _value: u8) {}
    fn write_u16(&mut self, _port: u16, _value: u16) {}
    fn write_u32(&mut self, _port: u16, _value: u32) {}
}

/// Run REP INS with page 0x41 absent, then map it and let the instruction
/// restart. Returns (port reads at the fault, EDI at the fault, ECX at the
/// fault, the memory).
fn rep_ins_fault_then_restart(code: &[u8], edi: u32, count: u32) -> (u32, u32, u32, Memory) {
    let (mut cpu, mut mem) = setup_paging(code);
    mem.write_u32_physical(0x11000 + 0x41 * 4, 0); // page 0x41 not present
    cpu.regs.edx = 0x170;
    cpu.regs.edi = edi;
    cpu.regs.ecx = count;
    let saved_eip = cpu.regs.eip;
    let mut port = CountingPort { reads: 0 };

    match run(&mut cpu, &mut mem, &mut port, 1024) {
        RunResult::Exception { vector, .. } => assert_eq!(vector, 14, "destination #PF"),
        other => panic!("expected #PF on REP INS, got {other:?}"),
    }
    assert_eq!(cpu.regs.eip, saved_eip, "REP INS must restart after #PF");
    let at_fault = (port.reads, cpu.regs.edi, cpu.regs.ecx);

    // The monitor maps the page and restarts the instruction.
    mem.write_u32_physical(0x11000 + 0x41 * 4, 0x41007);
    mem.invalidate_tlb();
    let _ = run(&mut cpu, &mut mem, &mut port, 1024);
    assert_eq!(cpu.regs.ecx, 0, "REP INS completes after the restart");
    assert_eq!(
        port.reads, count,
        "every element reads the port exactly once"
    );
    (at_fault.0, at_fault.1, at_fault.2, mem)
}

#[test]
fn rep_insw_destination_fault_does_not_consume_a_port_word() {
    // F3 66 6D = REP INSW (32-bit CS: 0x66 selects the word form).
    let (reads, edi, ecx, mem) = rep_ins_fault_then_restart(&[0xF3, 0x66, 0x6D], 0x40FFC, 4);
    assert_eq!(reads, 2, "the faulting element must not read the port");
    assert_eq!((edi, ecx), (0x41000, 2));
    let words: Vec<u16> = (0..4).map(|i| mem.read_u16(0x40FFC + i * 2)).collect();
    assert_eq!(
        words,
        [1, 2, 3, 4],
        "the stream arrives in order with no gap"
    );
}

#[test]
fn rep_insd_page_straddling_destination_fault_does_not_consume_a_port_dword() {
    // F3 6D = REP INSD. The first dword straddles 0x40FFE..0x41001, so its
    // second page faults before anything is written.
    let (reads, edi, ecx, mem) = rep_ins_fault_then_restart(&[0xF3, 0x6D], 0x40FFE, 2);
    assert_eq!(reads, 0, "the straddling element must not read the port");
    assert_eq!((edi, ecx), (0x40FFE, 2));
    assert_eq!(mem.read_u32(0x40FFE), 1);
    assert_eq!(mem.read_u32(0x41002), 2);
}

// ========================================================================
// M2.4b: the paged bulk STOS/MOVS chunk loops lost their explicit
// VGA-window bail; `raw_ram_span` (which never proves a device page) now
// sends a VGA destination to the per-element path. These twins pin that a
// paged REP STOSB / REP MOVSB into the VGA window still behaves exactly like
// the same number of single-element instructions: one VGA bus write per
// element and the same planar result.
//
// A cold TLB never hands the bulk loop a host pointer for a device page
// (`host_write` stays null), so the loop already breaks at
// `tlb_translate_write`. The `warm` variants plant a direct-host entry for
// 0xA0000. That entry is SYNTHETIC: `tlb_insert` never creates one for the
// VGA window. It follows the warm-raw-TLB precedent of the EMS tests above
// and leaves `raw_ram_span` as the only guard, which is the case the deleted
// bail used to cover.
// ========================================================================

const VGA_TWIN_ELEMENTS: u32 = 64;

fn run_vga_twin(code: &[u8], ecx: u32, budget: u32, warm: bool) -> Memory {
    let (mut cpu, mut mem) = setup_paging(code);
    if warm {
        force_direct_host_tlb_entry(&mut mem, 0xA_0000, 0xA_0000, true);
    }
    for i in 0..VGA_TWIN_ELEMENTS as usize {
        mem.ram[0x2000 + i] = 0x30 + i as u8;
    }
    cpu.regs.eax = 0xA5;
    cpu.regs.ecx = ecx;
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0xA_0000;
    let before = mem.vga_bus_write_count();
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, budget);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed } if executed == budget),
        "VGA twin did not retire its program: {result:?}"
    );
    assert_eq!(cpu.regs.edi, 0xA_0000 + VGA_TWIN_ELEMENTS);
    assert_eq!(
        mem.vga_bus_write_count() - before,
        u64::from(VGA_TWIN_ELEMENTS),
        "one VGA bus write per element"
    );
    mem
}

fn assert_vga_twins_match(bulk: &Memory, single: &Memory) {
    for i in 0..VGA_TWIN_ELEMENTS as usize {
        assert_eq!(
            bulk.read_u8(0xA_0000 + i),
            single.read_u8(0xA_0000 + i),
            "VGA byte {i}"
        );
    }
}

fn vga_twins(rep_op: [u8; 2], warm: bool) {
    let bulk = run_vga_twin(&rep_op, VGA_TWIN_ELEMENTS, VGA_TWIN_ELEMENTS, warm);
    let single = run_vga_twin(
        &[rep_op[1]; VGA_TWIN_ELEMENTS as usize],
        0,
        VGA_TWIN_ELEMENTS,
        false,
    );
    assert_vga_twins_match(&bulk, &single);
}

#[test]
fn paged_rep_stosb_into_the_vga_window_goes_per_element() {
    vga_twins([0xF3, 0xAA], false);
}

#[test]
fn paged_rep_movsb_into_the_vga_window_goes_per_element() {
    vga_twins([0xF3, 0xA4], false);
}

/// Synthetic direct-host TLB entry on 0xA0000 (`tlb_insert` never creates
/// one): pins `raw_ram_span` as the only guard of the paged STOSB bulk loop.
#[test]
fn paged_rep_stosb_into_a_warm_vga_tlb_entry_goes_per_element() {
    vga_twins([0xF3, 0xAA], true);
}

/// Synthetic direct-host TLB entry on 0xA0000 (`tlb_insert` never creates
/// one): pins `raw_ram_span` as the only guard of the paged MOVSB bulk loop.
#[test]
fn paged_rep_movsb_into_a_warm_vga_tlb_entry_goes_per_element() {
    vga_twins([0xF3, 0xA4], true);
}
