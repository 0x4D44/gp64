use gp64_cpu::{Cpu, CpuType, SegmentCache};
use gp64_interp::{run, RunResult};
use gp64_memory::Memory;

fn setup(code: &[u8], cpl: u8) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    cpu.cpu_type = CpuType::PentiumIIICoppermine;
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = cpl;
    cpu.cs_bits = 32;
    cpu.regs.cs = 8 | u16::from(cpl);
    cpu.regs.ss = 16 | u16::from(cpl);
    cpu.regs.cs_cache =
        SegmentCache::from_descriptor(&[0xff, 0xff, 0, 0, 0, 0x9b | (cpl << 5), 0xcf, 0]);
    cpu.regs.ss_cache =
        SegmentCache::from_descriptor(&[0xff, 0xff, 0, 0, 0, 0x93 | (cpl << 5), 0xcf, 0]);
    cpu.regs.ds = cpu.regs.ss;
    cpu.regs.ds_cache = cpu.regs.ss_cache;
    cpu.regs.set_eip(0x100);
    let mut mem = Memory::new();
    mem.ram[0x100..0x100 + code.len()].copy_from_slice(code);
    (cpu, mem)
}

#[test]
fn coppermine_cpuid_identity_and_cache_leaf() {
    let (mut cpu, mut mem) = setup(&[0x0f, 0xa2], 0);
    for (leaf, expected) in [
        (0, [2, 0x756e6547, 0x6c65746e, 0x49656e69]),
        (1, [0x68a, 0, 0, 0x0381b9bd]),
        (2, [0x03020101, 0, 0, 0x0c040841]),
    ] {
        cpu.regs.set_eip(0x100);
        cpu.regs.eax = leaf;
        run(&mut cpu, &mut mem, &mut (), 1);
        assert_eq!(
            [cpu.regs.eax, cpu.regs.ebx, cpu.regs.ecx, cpu.regs.edx],
            expected
        );
    }
}

#[test]
fn coppermine_sysenter_sysexit_synthetic_descriptors_and_flags() {
    let (mut cpu, mut mem) = setup(&[0x0f, 0x34], 3);
    cpu.sysenter_cs = 0x2f;
    cpu.sysenter_esp = 0x76543210;
    cpu.sysenter_eip = 0x200;
    cpu.regs.set_eflags(0x10247); // IF, RF and arithmetic flags
    let old_flags = cpu.regs.eflags();
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(result, RunResult::ModeChange { executed: 1 }),
        "{result:?}"
    );
    assert_eq!(
        (
            cpu.regs.cs,
            cpu.regs.ss,
            cpu.regs.cpl,
            cpu.regs.eip,
            cpu.regs.esp
        ),
        (0x2c, 0x34, 0, 0x200, 0x76543210)
    );
    assert_eq!(cpu.regs.eflags(), old_flags & !0x30200);
    assert_eq!(
        (
            cpu.regs.cs_cache.base,
            cpu.regs.cs_cache.cached_effective_limit,
            cpu.regs.cs_cache.access
        ),
        (0, u32::MAX, 0x9b)
    );
    assert_eq!(cpu.regs.ss_cache.access, 0x93);
    mem.ram[0x200..0x202].copy_from_slice(&[0x0f, 0x35]);
    cpu.regs.ecx = 0xabcdef00;
    cpu.regs.edx = 0x12345678;
    cpu.regs.eflags_high |= 1; // successful SYSEXIT retires RF
    let flags = cpu.regs.eflags();
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(result, RunResult::ModeChange { executed: 1 }),
        "{result:?}"
    );
    assert_eq!(
        (
            cpu.regs.cs,
            cpu.regs.ss,
            cpu.regs.cpl,
            cpu.regs.eip,
            cpu.regs.esp
        ),
        (0x3f, 0x47, 3, 0x12345678, 0xabcdef00)
    );
    assert_eq!(cpu.regs.eflags(), flags & !0x10000);
    assert_eq!(cpu.regs.cs_cache.access, 0xfb);
    assert_eq!(cpu.regs.ss_cache.access, 0xf3);
}

#[test]
fn coppermine_sysenter_sysexit_faults_leave_state_unchanged() {
    for opcode in [0x34, 0x35] {
        for (identity, pe, cs, cpl, expected) in [
            (CpuType::PentiumIII, true, 8, 0, 6),
            (CpuType::PentiumIIICoppermine, false, 8, 0, 13),
            (CpuType::PentiumIIICoppermine, true, 3, 0, 13),
            (
                CpuType::PentiumIIICoppermine,
                true,
                8,
                3,
                if opcode == 0x35 { 13 } else { 0 },
            ),
        ] {
            if expected == 0 {
                continue;
            }
            let (mut cpu, mut mem) = setup(&[0x0f, opcode], cpl);
            cpu.cpu_type = identity;
            if !pe {
                cpu.regs.cr0 &= !1;
            }
            cpu.sysenter_cs = cs;
            let regs = cpu.regs.clone();
            let result = run(&mut cpu, &mut mem, &mut (), 1);
            assert!(
                matches!(result, RunResult::Exception {vector, error_code:0, executed:0, ..} if vector == expected),
                "opcode {opcode:x}: {result:?}"
            );
            assert_eq!(
                (
                    cpu.regs.eip,
                    cpu.regs.esp,
                    cpu.regs.cs,
                    cpu.regs.ss,
                    cpu.regs.cpl
                ),
                (regs.eip, regs.esp, regs.cs, regs.ss, regs.cpl)
            );
        }
    }
}

fn msr_roundtrip(index: u32, value: u64, invalid: u64) {
    let (mut cpu, mut mem) = setup(&[0x0f, 0x30, 0x0f, 0x32], 0);
    cpu.regs.ecx = index;
    cpu.regs.eax = value as u32;
    cpu.regs.edx = (value >> 32) as u32;
    assert!(matches!(
        run(&mut cpu, &mut mem, &mut (), 2),
        RunResult::BudgetExhausted { executed: 2 }
    ));
    assert_eq!(
        (u64::from(cpu.regs.edx) << 32) | u64::from(cpu.regs.eax),
        value
    );
    cpu.regs.set_eip(0x100);
    cpu.regs.eax = invalid as u32;
    cpu.regs.edx = (invalid >> 32) as u32;
    assert!(matches!(
        run(&mut cpu, &mut mem, &mut (), 1),
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            executed: 0,
            ..
        }
    ));
    cpu.regs.set_eip(0x102);
    run(&mut cpu, &mut mem, &mut (), 1);
    assert_eq!(
        (u64::from(cpu.regs.edx) << 32) | u64::from(cpu.regs.eax),
        value,
        "fault must preserve MSR"
    );
    {
        let (mut cpu, mut mem) = setup(&[0x0f, 0x32], 3);
        cpu.regs.ecx = index;
        assert!(matches!(
            run(&mut cpu, &mut mem, &mut (), 1),
            RunResult::Exception { vector: 13, .. }
        ));
    }
}

#[test]
fn coppermine_mtrr_cap_and_default_type_dispatch() {
    let (mut cpu, mut mem) = setup(&[0x0f, 0x32, 0x0f, 0x30], 0);
    cpu.regs.ecx = 0xfe;
    run(&mut cpu, &mut mem, &mut (), 1);
    assert_eq!((cpu.regs.eax, cpu.regs.edx), (0x508, 0));
    assert!(matches!(
        run(&mut cpu, &mut mem, &mut (), 1),
        RunResult::Exception { vector: 13, .. }
    ));
    msr_roundtrip(0x2ff, 0xc06, 1 << 12);
}

#[test]
fn coppermine_mtrr_fixed_dispatch() {
    for index in [
        0x250, 0x258, 0x259, 0x268, 0x269, 0x26a, 0x26b, 0x26c, 0x26d, 0x26e, 0x26f,
    ] {
        msr_roundtrip(index, 0x0606050504040100, 0x0606050504040102);
    }
}

#[test]
fn coppermine_mtrr_variable_dispatch() {
    for index in (0x200..=0x20f).step_by(2) {
        msr_roundtrip(index, 0xf_1234_5006, 1 << 36);
        msr_roundtrip(index + 1, 0xf_fff0_0800, 1 << 10);
    }
}

#[test]
fn coppermine_pat_dispatch() {
    msr_roundtrip(0x277, 0x0706050401000006, 0x0706050401000003);
}

#[test]
fn coppermine_sysenter_msr_dispatch() {
    for (index, value) in [
        (0x174, 0xfedc_ba98_5678_0028u64),
        (0x175, 0xfedc_ba98_7654_3210),
        (0x176, 0xfedc_ba98_1234_5678),
    ] {
        let (mut cpu, mut mem) = setup(&[0x0f, 0x30, 0x0f, 0x32], 0);
        cpu.regs.ecx = index;
        cpu.regs.eax = value as u32;
        cpu.regs.edx = (value >> 32) as u32;
        assert!(matches!(
            run(&mut cpu, &mut mem, &mut (), 2),
            RunResult::BudgetExhausted { executed: 2 }
        ));
        assert_eq!((cpu.regs.eax, cpu.regs.edx), (value as u32, 0));
        // SYSENTER_CS[31:16] is unused but read/write; P6 discards MSR[63:32].
        // Privilege and identity, rather than the input value, cause #GP here.
        for opcode in [0x30, 0x32] {
            for (identity, cpl) in [(CpuType::PentiumIIICoppermine, 3), (CpuType::PentiumIII, 0)] {
                let (mut cpu, mut mem) = setup(&[0x0f, opcode], cpl);
                cpu.cpu_type = identity;
                cpu.regs.ecx = index;
                let result = run(&mut cpu, &mut mem, &mut (), 1);
                assert!(
                    matches!(
                        result,
                        RunResult::Exception {
                            vector: 13,
                            error_code: 0,
                            executed: 0,
                            ..
                        }
                    ),
                    "{result:?}"
                );
            }
        }
    }
}

#[test]
fn coppermine_sysenter_from_v86_clears_vm_rf_and_if() {
    let (mut cpu, mut mem) = setup(&[0x0f, 0x34], 3);
    cpu.regs.cs = 0;
    cpu.regs.cs_cache = SegmentCache::real_mode(cpu.regs.cs);
    cpu.regs.cs_cache.access = 0x9b;
    cpu.regs.cs_cache.is_code = true;
    cpu.regs.cs_cache.is_readable = true;
    cpu.regs.ds = 0;
    cpu.regs.ds_cache = SegmentCache::real_mode(cpu.regs.ds);
    cpu.regs.es = 0;
    cpu.regs.es_cache = SegmentCache::real_mode(cpu.regs.es);
    cpu.regs.ss = 0;
    cpu.regs.ss_cache = SegmentCache::real_mode(cpu.regs.ss);
    cpu.regs.fs = 0;
    cpu.regs.fs_cache = SegmentCache::real_mode(cpu.regs.fs);
    cpu.regs.gs = 0;
    cpu.regs.gs_cache = SegmentCache::real_mode(cpu.regs.gs);
    cpu.regs.cpl = 3;
    cpu.regs.cr0 |= 1;
    cpu.regs.set_eflags(0x30247); // VM, RF, IF and arithmetic flags
    cpu.cs_bits = 16;
    assert_ne!(cpu.regs.eflags_high & 2, 0, "fixture starts in V86 mode");
    assert_eq!(cpu.regs.cpl, 3, "V86 executes at CPL 3");
    for cache in [
        cpu.regs.cs_cache,
        cpu.regs.ss_cache,
        cpu.regs.ds_cache,
        cpu.regs.es_cache,
        cpu.regs.fs_cache,
        cpu.regs.gs_cache,
    ] {
        assert_eq!(
            (cache.base, cache.limit, cache.granularity, cache.big),
            (0, 0xffff, false, false),
            "V86 segment caches use real-mode bases and limits"
        );
    }
    assert!(cpu.regs.cs_cache.is_code, "V86 CS cache remains executable");
    cpu.sysenter_cs = 8;
    cpu.sysenter_eip = 0x200;
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(result, RunResult::ModeChange { executed: 1 }),
        "{result:?}"
    );
    assert_eq!(cpu.regs.eflags(), 0x47);
    assert_eq!((cpu.regs.cpl, cpu.cs_bits, cpu.regs.cs), (0, 32, 8));
}

fn paging(code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mut mem) = setup(code, 0);
    cpu.regs.cr0 |= 0x80000000;
    cpu.regs.cr3 = 0x1000;
    cpu.regs.cr4 = gp64_cpu::CR4_PGE;
    for (directory, table) in [(0x1000, 0x3000), (0x2000, 0x5000)] {
        mem.write_u32_physical(directory, table as u32 | 7);
        mem.write_u32_physical(table, 7); // code stays identity-mapped
    }
    for (table, global, ordinary) in [(0x3000, 0x7000, 0x9000), (0x5000, 0x8000, 0xa000)] {
        mem.write_u32_physical(table + 16, global | 0x107);
        mem.write_u32_physical(table + 20, ordinary | 7);
    }
    (cpu, mem)
}

#[test]
fn coppermine_global_cr3_invlpg_and_flat_views() {
    let (mut cpu, mut mem) = paging(&[0x0f, 0x22, 0xd8, 0x0f, 0x01, 0x3d, 0x00, 0x40, 0x00, 0x00]);
    for linear in [0x4000, 0x5000] {
        cpu.translate_paging_checked(&mut mem, linear, gp64_cpu::PageAccess::Read)
            .unwrap();
        assert!(mem.flat_read_published(linear >> 12));
    }
    let epoch = mem.dynrec_translation_epoch();
    cpu.regs.eax = 0x2000;
    run(&mut cpu, &mut mem, &mut (), 1); // MOV CR3,EAX
    assert!(mem.dynrec_translation_epoch() > epoch);
    assert!(mem.tlb_lookup(4).is_some(), "CR3 must preserve globals");
    assert!(
        mem.tlb_lookup(5).is_none(),
        "CR3 must invalidate nonglobals"
    );
    assert!(mem.flat_read_published(4));
    assert!(!mem.flat_read_published(5));
    assert_eq!(
        cpu.translate_paging_checked(&mut mem, 0x4000, gp64_cpu::PageAccess::Read)
            .unwrap(),
        0x7000
    );
    let epoch = mem.dynrec_translation_epoch();
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(
            result,
            RunResult::Invlpg {
                linear_addr: 0x4000,
                ..
            }
        ),
        "{result:?}"
    );
    assert!(mem.dynrec_translation_epoch() > epoch);
    assert!(mem.tlb_lookup(4).is_none());
    assert!(!mem.flat_read_published(4));
    assert_eq!(
        cpu.translate_paging_checked(&mut mem, 0x4000, gp64_cpu::PageAccess::Read)
            .unwrap(),
        0x8000
    );
    mem.oracle_check_flat_projection().unwrap();
    mem.oracle_check_flat_no_stale_survivors().unwrap();
}

#[test]
fn coppermine_g_bit_without_pge_does_not_survive_cr3() {
    let (mut cpu, mut mem) = paging(&[0x0f, 0x22, 0xd8]);
    cpu.regs.cr4 &= !gp64_cpu::CR4_PGE;
    assert_eq!(
        cpu.translate_paging_checked(&mut mem, 0x4000, gp64_cpu::PageAccess::Read)
            .unwrap(),
        0x7000
    );
    assert!(mem.tlb_global_source(4).is_none());
    cpu.regs.eax = 0x2000;
    run(&mut cpu, &mut mem, &mut (), 1);
    assert!(mem.tlb_lookup(4).is_none());
    assert!(!mem.flat_read_published(4));
    assert_eq!(
        cpu.translate_paging_checked(&mut mem, 0x4000, gp64_cpu::PageAccess::Read)
            .unwrap(),
        0x8000
    );
}

#[test]
fn coppermine_cr0_pg_toggle_drops_globals_and_advances_lifetime() {
    let (mut cpu, mut mem) = paging(&[0x0f, 0x22, 0xc0, 0x0f, 0x22, 0xc0]);
    cpu.translate_paging_checked(&mut mem, 0x4000, gp64_cpu::PageAccess::Read)
        .unwrap();
    let epoch = mem.dynrec_global_tlb_flush_epoch();
    cpu.regs.eax = cpu.regs.cr0 & !0x8000_0000;
    run(&mut cpu, &mut mem, &mut (), 1);
    assert!(mem.tlb_lookup(4).is_none());
    assert!(!mem.flat_read_published(4));
    assert_eq!(mem.dynrec_global_tlb_flush_epoch(), epoch + 1);
    cpu.regs.eax = cpu.regs.cr0 | 0x8000_0000;
    run(&mut cpu, &mut mem, &mut (), 1);
    assert!(mem.tlb_lookup(4).is_none());
    assert_eq!(
        mem.dynrec_global_tlb_flush_epoch(),
        epoch + 1,
        "empty flush must not sever PC regions"
    );
    mem.oracle_check_flat_projection().unwrap();
}

#[test]
fn coppermine_global_first_write_after_cr3_marks_original_pte() {
    let (mut cpu, mut mem) = paging(&[0x0f, 0x22, 0xd8]);
    cpu.translate_paging_checked(&mut mem, 0x4000, gp64_cpu::PageAccess::Read)
        .unwrap();
    cpu.regs.eax = 0x2000;
    run(&mut cpu, &mut mem, &mut (), 1);
    assert_eq!(
        cpu.translate_paging_checked(&mut mem, 0x4000, gp64_cpu::PageAccess::Write)
            .unwrap(),
        0x7000
    );
    assert_ne!(mem.read_u32_physical(0x3010) & 0x40, 0);
    assert_eq!(mem.read_u32_physical(0x5010) & 0x40, 0);
    assert!(mem.tlb_lookup(4).unwrap().3);
    mem.oracle_check_flat_projection().unwrap();
}

#[test]
fn coppermine_global_pge_toggle_flushes_every_view_and_legacy_rejects_pge() {
    let (mut cpu, mut mem) = paging(&[0x0f, 0x22, 0xe0]);
    cpu.translate_paging_checked(&mut mem, 0x4000, gp64_cpu::PageAccess::Read)
        .unwrap();
    let epoch = mem.dynrec_translation_epoch();
    let global_epoch = mem.dynrec_global_tlb_flush_epoch();
    cpu.regs.eax = 0;
    run(&mut cpu, &mut mem, &mut (), 1);
    assert!(mem.dynrec_translation_epoch() > epoch);
    assert_eq!(mem.dynrec_global_tlb_flush_epoch(), global_epoch + 1);
    assert!(mem.tlb_lookup(4).is_none());
    assert!(!mem.flat_read_published(4));
    cpu.translate_paging_checked(&mut mem, 0x4000, gp64_cpu::PageAccess::Read)
        .unwrap();
    assert!(mem.flat_read_published(4));
    cpu.regs.set_eip(0x100);
    cpu.regs.eax = gp64_cpu::CR4_PGE;
    run(&mut cpu, &mut mem, &mut (), 1);
    assert_eq!(cpu.regs.cr4, gp64_cpu::CR4_PGE);
    assert_eq!(mem.dynrec_global_tlb_flush_epoch(), global_epoch + 2);
    assert!(mem.tlb_lookup(4).is_none());
    assert!(!mem.flat_read_published(4));
    // An unchanged CR4 write also flushes retained globals in this emulator.
    cpu.translate_paging_checked(&mut mem, 0x4000, gp64_cpu::PageAccess::Read)
        .unwrap();
    cpu.regs.set_eip(0x100);
    run(&mut cpu, &mut mem, &mut (), 1);
    assert_eq!(mem.dynrec_global_tlb_flush_epoch(), global_epoch + 3);
    assert!(mem.tlb_lookup(4).is_none());
    assert!(!mem.flat_read_published(4));
    for identity in [
        CpuType::Pentium,
        CpuType::PentiumII,
        CpuType::PentiumIII,
        CpuType::Pentium4,
    ] {
        let (mut cpu, mut mem) = setup(&[0x0f, 0x22, 0xe0], 0);
        cpu.cpu_type = identity;
        cpu.regs.eax = gp64_cpu::CR4_PGE;
        assert!(matches!(
            run(&mut cpu, &mut mem, &mut (), 1),
            RunResult::Exception { vector: 13, .. }
        ));
        assert_eq!(cpu.regs.cr4, 0);
        assert_eq!(mem.dynrec_global_tlb_flush_epoch(), 0);
    }
}

#[test]
fn coppermine_global_large_page_cr3_and_invlpg_all_slices() {
    let (mut cpu, mut mem) = paging(&[0x0f, 0x22, 0xd8, 0x0f, 0x01, 0x3d, 0x00, 0x10, 0x40, 0x00]);
    cpu.regs.cr4 |= gp64_cpu::CR4_PSE;
    mem.write_u32_physical(0x1004, 0x400187); // global 4MiB region
    mem.write_u32_physical(0x2004, 0x800187);
    for linear in [0x401000, 0x402000] {
        cpu.translate_paging_checked(&mut mem, linear, gp64_cpu::PageAccess::Read)
            .unwrap();
    }
    cpu.regs.eax = 0x2000;
    run(&mut cpu, &mut mem, &mut (), 1);
    assert_eq!(
        cpu.translate_paging_checked(&mut mem, 0x402000, gp64_cpu::PageAccess::Write)
            .unwrap(),
        0x402000
    );
    assert_ne!(mem.read_u32_physical(0x1004) & 0x40, 0);
    assert_eq!(mem.read_u32_physical(0x2004) & 0x40, 0);
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(matches!(result, RunResult::Invlpg { .. }), "{result:?}");
    for page in [0x401, 0x402] {
        assert!(mem.tlb_lookup(page).is_none());
        assert!(!mem.flat_read_published(page));
    }
    assert_eq!(
        cpu.translate_paging_checked(&mut mem, 0x402000, gp64_cpu::PageAccess::Read)
            .unwrap(),
        0x802000
    );
}

#[test]
fn coppermine_sysenter_state_snapshot_and_hardware_reset() {
    let (mut cpu, mut mem) = setup(&[0x0f, 0x30], 0);
    for (index, value) in [(0x174, 0x28), (0x175, 0x76543210), (0x176, 0x12345678)] {
        cpu.regs.set_eip(0x100);
        cpu.regs.ecx = index;
        cpu.regs.eax = value;
        cpu.regs.edx = 0;
        run(&mut cpu, &mut mem, &mut (), 1);
    }
    assert_eq!(
        (cpu.sysenter_cs, cpu.sysenter_esp, cpu.sysenter_eip),
        (0x28, 0x76543210, 0x12345678)
    );
    let snapshot = cpu.snapshot_state();
    let mut restored = Cpu::new();
    restored.restore_state(&snapshot).unwrap();
    assert_eq!(
        (
            restored.sysenter_cs,
            restored.sysenter_esp,
            restored.sysenter_eip
        ),
        (0x28, 0x76543210, 0x12345678)
    );
    restored.set_reset_vector_state();
    assert_eq!(
        (
            restored.sysenter_cs,
            restored.sysenter_esp,
            restored.sysenter_eip
        ),
        (0x28, 0x76543210, 0x12345678),
        "INIT preserves SYSENTER MSRs"
    );
    cpu.reset();
    assert_eq!(
        (cpu.sysenter_cs, cpu.sysenter_esp, cpu.sysenter_eip),
        (0, 0, 0)
    );
}

#[test]
fn coppermine_sysenter_sysexit_lock_prefix_faults() {
    for opcode in [0x34, 0x35] {
        let (mut cpu, mut mem) = setup(&[0xf0, 0x0f, opcode], 0);
        cpu.sysenter_cs = 8;
        let result = run(&mut cpu, &mut mem, &mut (), 1);
        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 6,
                    executed: 0,
                    ..
                }
            ),
            "{result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x100);
    }
}
