use super::*;
use gp64_cpu::Flags;
use gp64_memory::Memory;

fn setup_real(code_addr: u32, code: &[u8], cs_bits: u32) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0;
    cpu.regs.cs_cache = SegmentCache::real_mode(0);
    cpu.regs.ds = 0;
    cpu.regs.ds_cache = SegmentCache::real_mode(0);
    cpu.regs.es = 0;
    cpu.regs.es_cache = SegmentCache::real_mode(0);
    cpu.regs.ss = 0;
    cpu.regs.ss_cache = SegmentCache::real_mode(0);
    cpu.regs.set_eip(code_addr);
    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.cs_bits = cs_bits;
    for (i, &byte) in code.iter().enumerate() {
        mem.ram[code_addr as usize + i] = byte;
    }
    (cpu, mem)
}

fn setup_paging(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();

    cpu.regs.cr0 = 0x8000_0001;
    cpu.regs.cpl = 0;
    cpu.regs.cr3 = 0x10000;
    mem.write_u32_physical(0x10000, 0x11007);
    for page in 0..256u32 {
        mem.write_u32_physical(0x11000 + (page as usize) * 4, (page << 12) | 0x07);
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

    for (selector, cache) in [
        (0x0010, &mut cpu.regs.ds_cache),
        (0x0020, &mut cpu.regs.es_cache),
        (0x0018, &mut cpu.regs.ss_cache),
    ] {
        cache.base = 0;
        cache.limit = 0xFFFFF;
        cache.granularity = true;
        cache.recompute_effective_limit();
        cache.access = 0x93;
        cache.is_code = false;
        cache.big = true;
        cache.present = true;
        match selector {
            0x0010 => cpu.regs.ds = selector,
            0x0020 => cpu.regs.es = selector,
            0x0018 => cpu.regs.ss = selector,
            _ => unreachable!(),
        }
    }

    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.eip = 0x0100;
    for (i, &byte) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = byte;
    }

    (cpu, mem)
}

fn op_stat(opcode: u16) -> InterpHotspotStat {
    interp_hotspot_test_stat(opcode).expect("expected hotspot stat for opcode")
}

struct BreakBatchIo;

impl IoPort for BreakBatchIo {
    fn read_u8(&mut self, _port: u16) -> u8 {
        0x5A
    }

    fn read_u16(&mut self, _port: u16) -> u16 {
        0xA55A
    }

    fn read_u32(&mut self, _port: u16) -> u32 {
        0xA55A_5AA5
    }

    fn write_u8(&mut self, _port: u16, _value: u8) {}

    fn write_u16(&mut self, _port: u16, _value: u16) {}

    fn write_u32(&mut self, _port: u16, _value: u32) {}

    fn should_break_batch(&self) -> bool {
        true
    }
}

#[test]
fn test_hotspots_test_config_guard_clears_thread_local_config() {
    assert!(
        INTERP_HOTSPOTS_TEST_CONFIG.with(|slot| slot.borrow().is_none()),
        "hotspot test config should start clear on this worker thread"
    );
    {
        let _hotspots = interp_hotspot_test_reset();
        assert!(
            INTERP_HOTSPOTS_TEST_CONFIG.with(|slot| slot.borrow().is_some()),
            "hotspot test config should be installed while guard is live"
        );
    }
    assert!(
        INTERP_HOTSPOTS_TEST_CONFIG.with(|slot| slot.borrow().is_none()),
        "hotspot test config should be cleared when guard drops"
    );
}

#[test]
fn test_hotspots_common_epilogue_records_once_for_nop() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_real(0x0100, &[0x90], 16);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    let stat = op_stat(0x90);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 1);
}

#[test]
fn test_hotspots_site_records_full_32_bit_eip_and_linear() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_real(0x0001_2345, &[0x90], 32);
    cpu.regs.cs = 0x1234;
    cpu.regs.cs_cache.base = 0x0020_0000;
    cpu.regs.cs_cache.limit = u32::MAX;
    mem.ram[0x0021_2345] = 0x90;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    let sites = interp_hotspot_test_sites(0x90);
    assert_eq!(sites.len(), 1);
    let (site, stat) = sites[0];
    assert_eq!(site.cs, 0x1234);
    assert_eq!(site.ip, 0x0001_2345);
    assert_eq!(site.linear, 0x0021_2345);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 1);
}

#[test]
fn test_hotspots_report_schema_v2_includes_linear_column() {
    let config = InterpHotspotConfig {
        sample_every: 1,
        dump_every_samples: 1,
        start_observed: 0,
        top: 64,
        path: "tmp/interp_hotspots_report_schema_test.tsv".to_string(),
    };
    let mut state = InterpHotspotState {
        observed: 1,
        sampled: 1,
        active: true,
        active_observed_start: 1,
        ..InterpHotspotState::default()
    };
    state.op_stats.insert(
        0x90,
        InterpHotspotStat {
            samples: 1,
            retired: 1,
            ns: 10,
        },
    );
    state.site_stats.insert(
        InterpHotspotSite {
            cs: 0x000B,
            ip: 0x0001_2345,
            linear: 0x00AB_CDEF,
            opcode: 0x90,
        },
        InterpHotspotStat {
            samples: 1,
            retired: 1,
            ns: 10,
        },
    );

    interp_hotspot_write_report(&config, &state).expect("write hotspot report");
    let report = fs::read_to_string(&config.path).expect("read hotspot report");

    assert!(report.contains("# schema=interp_hotspots_v2"));
    assert!(report.contains(
        "kind\trank\topcode\tname\tcs\tip\tsamples\tretired_sampled\tns\tavg_ns\tlinear"
    ));
    assert!(report.contains("site\t1\t90\tNOP/XCHG\t000B\t00012345\t1\t1\t10\t10\t00ABCDEF"));
}

#[test]
fn test_hotspots_same_page_ret_fast_path_records_once() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_real(0x0100, &[0xC3, 0xF4], 16);
    mem.ram[0x8000] = 0x01;
    mem.ram[0x8001] = 0x01;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);

    assert!(matches!(result, RunResult::Halt { executed: 2 }));
    let stat = op_stat(0xC3);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 1);
}

#[test]
fn test_hotspots_rep_stosd_records_bulk_retired_count() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_real(0x0100, &[0xF3, 0xAB, 0xF4], 32);
    cpu.regs.edi = 0x0400;
    cpu.regs.ecx = 4;
    cpu.regs.eax = 0xAABB_CCDD;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::Halt { executed: 5 }));
    let stat = op_stat(0xAB);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 4);
}

#[test]
fn test_hotspots_rep_movsb_budget_limited_records_committed_iterations() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_real(0x0100, &[0xF3, 0xA4], 16);
    cpu.regs.esi = 0x0300;
    cpu.regs.edi = 0x0400;
    cpu.regs.ecx = 8;
    cpu.regs.flags.remove(Flags::DIRECTION);
    for i in 0..8 {
        mem.ram[0x0300 + i] = i as u8;
    }
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 3);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 3 }));
    let stat = op_stat(0xA4);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 3);
}

#[test]
fn test_hotspots_prefix_does_not_steal_sampling_tick() {
    let _hotspots = interp_hotspot_test_reset_with(InterpHotspotConfig {
        sample_every: 2,
        dump_every_samples: u64::MAX,
        start_observed: 0,
        top: 64,
        path: "tmp/interp_hotspots_prefix_test.tsv".to_string(),
    });
    let (mut cpu, mut mem) = setup_real(0x0100, &[0x90, 0xF3, 0xAA], 16);
    cpu.regs.ecx = 0;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);

    assert!(matches!(result, RunResult::BudgetExhausted { executed: 2 }));
    assert!(
        interp_hotspot_test_stat(0xF3).is_none(),
        "REP prefix must not get its own hotspot sample"
    );
    let stat = op_stat(0xAA);
    assert_eq!(stat.samples, 1);
    assert_eq!(
        stat.retired, 1,
        "CX=0 REP retires the instruction but no REP elements"
    );
}

#[test]
fn test_hotspots_rep_movsb_mixed_bulk_and_slow_records_all_committed_iterations() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4, 0xF4]);

    mem.write_u32_physical(0x11000 + 2 * 4, (0x40 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 3 * 4, ((VIDEO_BASE as u32) & 0xFFFF_F000) | 0x07);

    mem.ram[0x40FFE] = 0xA0;
    mem.ram[0x40FFF] = 0xA1;
    mem.ram[VIDEO_BASE] = 0xA2;
    mem.ram[VIDEO_BASE + 1] = 0xA3;
    cpu.regs.esi = 0x2FFE;
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::Halt { executed: 5 }));
    assert_eq!(&mem.ram[0x5000..0x5004], &[0xA0, 0xA1, 0xA2, 0xA3]);
    let stat = op_stat(0xA4);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 4);
}

#[test]
fn test_hotspots_rep_movsb_residual_fault_records_prior_bulk_iterations() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);

    mem.write_u32_physical(0x11000 + 2 * 4, (0x40 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 3 * 4, 0);
    mem.write_u32_physical(0x11000 + 5 * 4, (0x60 << 12) | 0x07);
    mem.tlb_insert(2, 0x40, true, true, true);
    mem.tlb_insert(5, 0x60, true, true, true);

    mem.ram[0x40FFE] = 0xB0;
    mem.ram[0x40FFF] = 0xB1;
    cpu.regs.esi = 0x2FFE;
    cpu.regs.edi = 0x5000;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 14,
                executed: 2,
                ..
            }
        ),
        "expected #PF after two committed bulk iterations, got {:?}",
        result
    );
    assert_eq!(&mem.ram[0x60000..0x60002], &[0xB0, 0xB1]);
    assert_eq!(cpu.regs.ecx, 2);
    let stat = op_stat(0xA4);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 2);
}

#[test]
fn test_hotspots_rep_movsb_residual_fault_records_bulk_plus_slow_iterations() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA4]);

    mem.write_u32_physical(0x11000 + 2 * 4, (0x40 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 5 * 4, (0x60 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 6 * 4, 0);
    mem.tlb_insert(2, 0x40, true, true, true);
    mem.tlb_insert(5, 0x60, true, true, true);

    mem.ram[0x40FFF] = 0xC0;
    mem.ram[0x3000] = 0xC1;
    cpu.regs.esi = 0x2FFF;
    cpu.regs.edi = 0x5FFE;
    cpu.regs.ecx = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(
        matches!(result, RunResult::Exception { vector: 14, .. }),
        "expected #PF after bulk plus one slow committed iteration, got {:?}",
        result
    );
    assert_eq!(&mem.ram[0x60FFE..0x61000], &[0xC0, 0xC1]);
    assert_eq!(cpu.regs.ecx, 2);
    let stat = op_stat(0xA4);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 2);
}

#[test]
fn test_hotspots_rep_stosb_partial_fault_records_committed_iterations() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAA]);

    mem.write_u32_physical(0x11000 + 5 * 4, (0x60 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 6 * 4, 0);
    cpu.regs.edi = 0x5FFF;
    cpu.regs.ecx = 2;
    cpu.regs.set_al(0xD4);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(
        matches!(result, RunResult::Exception { vector: 14, .. }),
        "expected #PF after one committed STOSB iteration, got {:?}",
        result
    );
    assert_eq!(mem.ram[0x60FFF], 0xD4);
    assert_eq!(cpu.regs.ecx, 1);
    let stat = op_stat(0xAA);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 1);
}

#[test]
fn test_hotspots_rep_lodsb_partial_fault_records_committed_iterations() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xAC]);

    mem.write_u32_physical(0x11000 + 5 * 4, (0x60 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 6 * 4, 0);
    mem.ram[0x60FFF] = 0xE1;
    cpu.regs.esi = 0x5FFF;
    cpu.regs.ecx = 2;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(
        matches!(result, RunResult::Exception { vector: 14, .. }),
        "expected #PF after one committed LODSB iteration, got {:?}",
        result
    );
    assert_eq!(cpu.regs.al(), 0xE1);
    assert_eq!(cpu.regs.ecx, 1);
    let stat = op_stat(0xAC);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 1);
}

#[test]
fn test_hotspots_repe_cmpsb_partial_fault_records_committed_iterations() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_paging(&[0xF3, 0xA6]);

    mem.write_u32_physical(0x11000 + 5 * 4, (0x60 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 6 * 4, 0);
    mem.write_u32_physical(0x11000 + 7 * 4, (0x70 << 12) | 0x07);
    mem.ram[0x60FFF] = 0x35;
    mem.ram[0x70FFF] = 0x35;
    cpu.regs.esi = 0x5FFF;
    cpu.regs.edi = 0x7FFF;
    cpu.regs.ecx = 2;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(
        matches!(result, RunResult::Exception { vector: 14, .. }),
        "expected #PF after one committed CMPSB iteration, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ecx, 1);
    let stat = op_stat(0xA6);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 1);
}

#[test]
fn test_hotspots_repne_scasb_partial_fault_records_committed_iterations() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_paging(&[0xF2, 0xAE]);

    mem.write_u32_physical(0x11000 + 5 * 4, (0x60 << 12) | 0x07);
    mem.write_u32_physical(0x11000 + 6 * 4, 0);
    mem.ram[0x60FFF] = 0x24;
    cpu.regs.edi = 0x5FFF;
    cpu.regs.ecx = 2;
    cpu.regs.set_al(0x42);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(
        matches!(result, RunResult::Exception { vector: 14, .. }),
        "expected #PF after one committed SCASB iteration, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ecx, 1);
    let stat = op_stat(0xAE);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 1);
}

#[test]
fn test_hotspots_repne_scasb_records_actual_early_termination_count() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_real(0x0100, &[0xF2, 0xAE, 0xF4], 16);
    cpu.regs.edi = 0x0500;
    cpu.regs.ecx = 10;
    cpu.regs.set_al(0x42);
    cpu.regs.flags.remove(Flags::DIRECTION);
    mem.ram[0x0500] = 0x01;
    mem.ram[0x0501] = 0x02;
    mem.ram[0x0502] = 0x03;
    mem.ram[0x0503] = 0x42;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 10);

    assert!(matches!(result, RunResult::Halt { executed: 5 }));
    let stat = op_stat(0xAE);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 4);
}

/// The REP CMPS dispatch arm must charge the observer
/// clock PER DRAINED ELEMENT, exactly like its SCAS twin — not a flat 1
/// per dispatch. The post-run `ChargedRunResult` total is compensated by
/// the `charged_retired_floor`, so the guest-visible oracle is the IN-RUN
/// clock: an RDTSC right after the REP reads `cpu.cycles + charged`, and
/// the two twins draining the same element count must read the same time.
/// (Pre-fix: the CMPS side reads ~N-1 cycles low.)
#[test]
fn test_rep_cmps_charges_observer_clock_per_element_like_scas() {
    const N: u32 = 24;

    // Twin A: REPE CMPSB over N equal bytes (no early stop) ; RDTSC ; HLT.
    let (mut cpu_a, mut mem_a) = setup_real(0x0100, &[0xF3, 0xA6, 0x0F, 0x31, 0xF4], 16);
    cpu_a.cpu_type = gp64_cpu::CpuType::Pentium; // RDTSC needs TSC
    cpu_a.regs.set_si(0x0500);
    cpu_a.regs.set_di(0x0600);
    cpu_a.regs.ecx = N;
    cpu_a.regs.flags.remove(Flags::DIRECTION);
    for i in 0..N as usize {
        mem_a.ram[0x0500 + i] = 0x5A;
        mem_a.ram[0x0600 + i] = 0x5A;
    }
    let mut io_a = ();
    let result_a = run(&mut cpu_a, &mut mem_a, &mut io_a, 64);
    assert!(
        matches!(result_a, RunResult::Halt { executed } if executed == N + 2),
        "CMPS twin must drain all {N} elements then RDTSC+HLT, got {result_a:?}"
    );

    // Twin B: REPNE SCASB over N never-matching bytes ; RDTSC ; HLT.
    let (mut cpu_b, mut mem_b) = setup_real(0x0100, &[0xF2, 0xAE, 0x0F, 0x31, 0xF4], 16);
    cpu_b.cpu_type = gp64_cpu::CpuType::Pentium; // RDTSC needs TSC
    cpu_b.regs.set_di(0x0600);
    cpu_b.regs.ecx = N;
    cpu_b.regs.set_al(0xFF); // never matches 0x5A → scans all N
    cpu_b.regs.flags.remove(Flags::DIRECTION);
    for i in 0..N as usize {
        mem_b.ram[0x0600 + i] = 0x5A;
    }
    let mut io_b = ();
    let result_b = run(&mut cpu_b, &mut mem_b, &mut io_b, 64);
    assert!(
        matches!(result_b, RunResult::Halt { executed } if executed == N + 2),
        "SCAS twin must scan all {N} elements then RDTSC+HLT, got {result_b:?}"
    );

    // The SCAS side anchors the model: at least one charge per element.
    assert!(
        cpu_b.regs.eax >= N,
        "SCAS twin's RDTSC must see >= {N} charged cycles, got {}",
        cpu_b.regs.eax
    );
    // Twin parity — the guest-visible clock must not depend on which
    // compare instruction the compiler picked.
    assert_eq!(
        cpu_a.regs.eax, cpu_b.regs.eax,
        "REP CMPS must charge the observer clock per element like SCAS \
         (CMPS rdtsc={:#x} vs SCAS rdtsc={:#x})",
        cpu_a.regs.eax, cpu_b.regs.eax
    );
}

/// The REP LODS dispatch arm must charge the observer
/// clock PER DRAINED ELEMENT, exactly like its CMPS/SCAS twins — not a
/// flat 1 per dispatch. Same oracle as the CMPS twin above: the post-run
/// total is repaired by the `charged_retired_floor`, so the guest-visible
/// evidence is the IN-RUN clock an RDTSC reads immediately after the REP.
/// (Pre-fix: the LODS side reads N-1 cycles low.)
#[test]
fn test_rep_lods_charges_observer_clock_per_element_like_scas() {
    const N: u32 = 24;

    // Twin A: REP LODSB over N bytes ; RDTSC ; HLT.
    let (mut cpu_a, mut mem_a) = setup_real(0x0100, &[0xF3, 0xAC, 0x0F, 0x31, 0xF4], 16);
    cpu_a.cpu_type = gp64_cpu::CpuType::Pentium; // RDTSC needs TSC
    cpu_a.regs.set_si(0x0500);
    cpu_a.regs.ecx = N;
    cpu_a.regs.flags.remove(Flags::DIRECTION);
    for i in 0..N as usize {
        mem_a.ram[0x0500 + i] = 0x5A;
    }
    let mut io_a = ();
    let result_a = run(&mut cpu_a, &mut mem_a, &mut io_a, 64);
    assert!(
        matches!(result_a, RunResult::Halt { executed } if executed == N + 2),
        "LODS twin must drain all {N} elements then RDTSC+HLT, got {result_a:?}"
    );

    // Twin B: REPNE SCASB over N never-matching bytes ; RDTSC ; HLT.
    let (mut cpu_b, mut mem_b) = setup_real(0x0100, &[0xF2, 0xAE, 0x0F, 0x31, 0xF4], 16);
    cpu_b.cpu_type = gp64_cpu::CpuType::Pentium; // RDTSC needs TSC
    cpu_b.regs.set_di(0x0600);
    cpu_b.regs.ecx = N;
    cpu_b.regs.set_al(0xFF); // never matches 0x5A → scans all N
    cpu_b.regs.flags.remove(Flags::DIRECTION);
    for i in 0..N as usize {
        mem_b.ram[0x0600 + i] = 0x5A;
    }
    let mut io_b = ();
    let result_b = run(&mut cpu_b, &mut mem_b, &mut io_b, 64);
    assert!(
        matches!(result_b, RunResult::Halt { executed } if executed == N + 2),
        "SCAS twin must scan all {N} elements then RDTSC+HLT, got {result_b:?}"
    );

    // The SCAS side anchors the model: at least one charge per element.
    assert!(
        cpu_b.regs.eax >= N,
        "SCAS twin's RDTSC must see >= {N} charged cycles, got {}",
        cpu_b.regs.eax
    );
    assert_eq!(
        cpu_a.regs.eax, cpu_b.regs.eax,
        "REP LODS must charge the observer clock per element like SCAS \
         (LODS rdtsc={:#x} vs SCAS rdtsc={:#x})",
        cpu_a.regs.eax, cpu_b.regs.eax
    );
}

/// The cycle-budget gate must bound REP LODS element processing and
/// let the in-run cycle-budget gate overshoot; measured, it does not —
/// `op_lods` caps itself against the `remaining` out-param independently
/// of `charged_cycles`, so this assertion passes on both sides of the
/// fix. It is kept because nothing else pins LODS/SCAS budget-stop
/// parity, which the charging change could plausibly have disturbed.
#[test]
fn test_rep_lods_respects_the_cycle_budget_like_scas() {
    const ECX: u32 = 4096;
    const BUDGET: u32 = 32;

    let (mut cpu_a, mut mem_a) = setup_real(0x0100, &[0xF3, 0xAC, 0xF4], 16);
    cpu_a.regs.set_si(0x0500);
    cpu_a.regs.ecx = ECX;
    cpu_a.regs.flags.remove(Flags::DIRECTION);
    let mut io_a = ();
    let result_a = run(&mut cpu_a, &mut mem_a, &mut io_a, BUDGET);

    let (mut cpu_b, mut mem_b) = setup_real(0x0100, &[0xF2, 0xAE, 0xF4], 16);
    cpu_b.regs.set_di(0x0600);
    cpu_b.regs.ecx = ECX;
    cpu_b.regs.set_al(0xFF); // never matches the zeroed buffer → scans on
    cpu_b.regs.flags.remove(Flags::DIRECTION);
    let mut io_b = ();
    let result_b = run(&mut cpu_b, &mut mem_b, &mut io_b, BUDGET);

    let drained_a = ECX - cpu_a.regs.ecx;
    let drained_b = ECX - cpu_b.regs.ecx;
    assert!(
        drained_a < ECX,
        "REP LODS must stop on the cycle budget, not drain all {ECX} \
         elements (got {drained_a}, {result_a:?})"
    );
    assert_eq!(
        drained_a, drained_b,
        "REP LODS must reach the budget after the same element count as \
         its SCAS twin (LODS drained {drained_a}, SCAS drained {drained_b}; \
         {result_a:?} vs {result_b:?})"
    );
}

#[test]
fn test_hotspots_hlt_direct_return_records_once() {
    let _hotspots = interp_hotspot_test_reset();
    let (mut cpu, mut mem) = setup_real(0x0100, &[0xF4], 16);
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(matches!(result, RunResult::Halt { executed: 1 }));
    let stat = op_stat(0xF4);
    assert_eq!(stat.samples, 1);
    assert_eq!(stat.retired, 1);
}

#[test]
fn test_hotspots_io_completed_paths_record_retired_instruction() {
    let _hotspots = interp_hotspot_test_reset();

    let (mut cpu, mut mem) = setup_real(0x0100, &[0xEC], 16);
    cpu.regs.set_dx(0x0600);
    let mut io = BreakBatchIo;
    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));
    assert_eq!(cpu.regs.al(), 0x5A);

    let (mut cpu, mut mem) = setup_real(0x0200, &[0xEE], 16);
    cpu.regs.set_dx(0x0600);
    cpu.regs.set_al(0xC3);
    let mut io = BreakBatchIo;
    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));

    let (mut cpu, mut mem) = setup_real(0x0300, &[0xEF], 16);
    cpu.regs.set_dx(0x0600);
    cpu.regs.set_ax(0xCAFE);
    let mut io = BreakBatchIo;
    let result = run(&mut cpu, &mut mem, &mut io, 10);
    assert!(matches!(result, RunResult::IoCompleted { executed: 1 }));

    for opcode in [0xEC, 0xEE, 0xEF] {
        let stat = op_stat(opcode);
        assert_eq!(stat.samples, 1, "opcode {opcode:02X}");
        assert_eq!(stat.retired, 1, "opcode {opcode:02X}");
    }
}
