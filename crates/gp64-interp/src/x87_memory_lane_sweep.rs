use super::test_helpers::setup_pm32;
use super::*;

/// EXP-IQ012 RED-first engagement regression pin (design §6.3),
/// patterned on `admitted_register_escape_bypasses_the_outlined_route`
/// above. FAILS at HEAD pre-lane: every admitted row currently takes
/// the outlined route (`x87_outlined == 1`).
#[test]
fn admitted_memory_escape_bypasses_the_outlined_route() {
    // FLD m32 [EAX] — admitted (mod=00, reg=0, rm=0).
    let (mut cpu, mut mem) = setup_pm32(&[0xD9, 0x00, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 0,
        "admitted FLD m32 took the outlined route"
    );

    // FSTP m32 [ECX] — admitted (mod=00, reg=3, rm=1).
    let (mut cpu, mut mem) = setup_pm32(&[0xD9, 0x19, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 0,
        "admitted FSTP m32 took the outlined route"
    );

    // FILD m32 [EDX] — admitted (mod=00, reg=0, rm=2).
    let (mut cpu, mut mem) = setup_pm32(&[0xDB, 0x02, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 0,
        "admitted FILD m32 took the outlined route"
    );

    // FISTP m32 [EBX] — admitted (mod=00, reg=3, rm=3).
    let (mut cpu, mut mem) = setup_pm32(&[0xDB, 0x1B, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 0,
        "admitted FISTP m32 took the outlined route"
    );

    // Decline: SIB shape (rm=4) — must take the outlined route.
    let (mut cpu, mut mem) = setup_pm32(&[0xD9, 0x04, 0x00, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 1,
        "SIB shape must take the outlined route"
    );

    // Decline: D9 /2 (FST) — unadmitted reg field.
    let (mut cpu, mut mem) = setup_pm32(&[0xD9, 0x10, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 1,
        "FST (D9/2) must take the outlined route"
    );

    // Decline: 66-prefixed FLD m32.
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0xD9, 0x00, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 1,
        "66-prefixed escape must take the outlined route"
    );

    // Decline: segment-override-prefixed FLD m32.
    let (mut cpu, mut mem) = setup_pm32(&[0x3E, 0xD9, 0x00, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 1,
        "segment-override-prefixed escape must take the outlined route"
    );
}

/// Pairwise-distinct, non-special values (mirrors the register sweep's
/// `STACK_VALS`) loaded via an all-memory-form preamble, unprefixed and
/// therefore identical between the lane and full-route runs of each
/// case (whether the preamble itself lanes cancels out — see `run_case`).
const STACK_VALS: [f32; 8] = [3.5, -1.25, 7.0, 0.125, -12.5, 2.75, 0.3, -9.875];
const CODE_DATA_BASE: u32 = 0x0000_2000;
const OPERAND_BASE: u32 = 0x0004_0000;

/// One addressing shape under test: the mod/rm bits, any trailing
/// displacement bytes, a register setup closure, an optional SS.base
/// override, and the resolved absolute operand address (flat, base-0
/// DS by default) used for the memory-window comparison.
///
/// Fold 3 (design §11): a flat harness cannot distinguish an SS default
/// from a DS default when SS.base == DS.base, so the two `[ebp+disp]`
/// rows set `ss_override` to a distinct base and leave EBP at 0 — a
/// lane bug that used DS instead of SS would read/write near linear
/// `disp` instead of `SS.base + disp`, a different location entirely,
/// which the two-route comparison would catch.
struct Shape {
    name: &'static str,
    mode: u8,
    rm: u8,
    disp_bytes: Vec<u8>,
    setup: fn(&mut Cpu),
    ss_override: Option<u32>,
    operand_addr: u32,
}

/// (rm, register-setup fn) pairs for the plain `[reg]` shape rows.
type RegShape = (u8, fn(&mut Cpu));

fn shapes() -> Vec<Shape> {
    let mut v = Vec::new();
    // mod=00, rm in {0,1,2,3,6,7}: [reg] — rm=4 is SIB, rm=5 is the
    // no-base [disp32] special case, neither is a plain register base.
    let regs: [RegShape; 6] = [
        (0, |c: &mut Cpu| c.regs.eax = OPERAND_BASE),
        (1, |c: &mut Cpu| c.regs.ecx = OPERAND_BASE),
        (2, |c: &mut Cpu| c.regs.edx = OPERAND_BASE),
        (3, |c: &mut Cpu| c.regs.ebx = OPERAND_BASE),
        (6, |c: &mut Cpu| c.regs.esi = OPERAND_BASE),
        (7, |c: &mut Cpu| c.regs.edi = OPERAND_BASE),
    ];
    for (rm, setup) in regs {
        v.push(Shape {
            name: "reg",
            mode: 0,
            rm,
            disp_bytes: vec![],
            setup,
            ss_override: None,
            operand_addr: OPERAND_BASE,
        });
    }
    // mod=00, rm=5: [disp32], no base.
    v.push(Shape {
        name: "disp32_nobase",
        mode: 0,
        rm: 5,
        disp_bytes: OPERAND_BASE.to_le_bytes().to_vec(),
        setup: |_c: &mut Cpu| {},
        ss_override: None,
        operand_addr: OPERAND_BASE,
    });
    // mod=01, rm=0 (EAX): [eax+disp8], positive and negative.
    v.push(Shape {
        name: "reg+disp8(+)",
        mode: 1,
        rm: 0,
        disp_bytes: vec![0x10],
        setup: |c: &mut Cpu| c.regs.eax = OPERAND_BASE,
        ss_override: None,
        operand_addr: OPERAND_BASE + 0x10,
    });
    v.push(Shape {
        name: "reg+disp8(-)",
        mode: 1,
        rm: 0,
        disp_bytes: vec![0xF0], // -16
        setup: |c: &mut Cpu| c.regs.eax = OPERAND_BASE,
        ss_override: None,
        operand_addr: OPERAND_BASE.wrapping_sub(0x10),
    });
    // mod=10, rm=1 (ECX): [ecx+disp32], positive and negative.
    v.push(Shape {
        name: "reg+disp32(+)",
        mode: 2,
        rm: 1,
        disp_bytes: 0x0000_1000u32.to_le_bytes().to_vec(),
        setup: |c: &mut Cpu| c.regs.ecx = OPERAND_BASE,
        ss_override: None,
        operand_addr: OPERAND_BASE + 0x1000,
    });
    v.push(Shape {
        name: "reg+disp32(-)",
        mode: 2,
        rm: 1,
        disp_bytes: 0xFFFF_F000u32.to_le_bytes().to_vec(), // -0x1000
        setup: |c: &mut Cpu| c.regs.ecx = OPERAND_BASE,
        ss_override: None,
        operand_addr: OPERAND_BASE.wrapping_sub(0x1000),
    });
    // mod=01/10, rm=5 (EBP): SS-default rows (fold 3).
    v.push(Shape {
        name: "ebp+disp8(ss)",
        mode: 1,
        rm: 5,
        disp_bytes: vec![0x08],
        setup: |c: &mut Cpu| c.regs.ebp = 0,
        ss_override: Some(OPERAND_BASE),
        operand_addr: OPERAND_BASE + 0x08,
    });
    v.push(Shape {
        name: "ebp+disp32(ss)",
        mode: 2,
        rm: 5,
        disp_bytes: 0x0000_2000u32.to_le_bytes().to_vec(),
        setup: |c: &mut Cpu| c.regs.ebp = 0,
        ss_override: Some(OPERAND_BASE),
        operand_addr: OPERAND_BASE + 0x0000_2000,
    });
    v
}

fn run_case(b0: u8, reg: u8, shape: &Shape, prefixed: bool, loaded: bool) -> String {
    let modrm = (shape.mode << 6) | (reg << 3) | shape.rm;
    let mut code = Vec::new();
    if loaded {
        // Preamble: 8x FLD m32 [disp32] (mod=00,reg=0,rm=5) — a memory
        // form itself, but always unprefixed and identical between the
        // lane and full-route runs of a case, so whichever route IT
        // takes cancels out of the comparison.
        for k in 0..8usize {
            let disp = CODE_DATA_BASE + (k as u32) * 4;
            code.push(0xD9);
            code.push(0x05);
            code.extend_from_slice(&disp.to_le_bytes());
        }
    }
    if prefixed {
        code.push(0x66);
    }
    code.push(b0);
    code.push(modrm);
    code.extend_from_slice(&shape.disp_bytes);
    code.push(0xF4); // HLT sentinel — never reached within budget

    let (mut cpu, mut mem) = setup_pm32(&code);
    (shape.setup)(&mut cpu);
    if let Some(ss_base) = shape.ss_override {
        cpu.regs.ss_cache.base = ss_base;
    }
    if loaded {
        for (k, v) in STACK_VALS.iter().enumerate() {
            let addr = (CODE_DATA_BASE + (k as u32) * 4) as usize;
            mem.ram[addr..addr + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    // Seed the operand cell and its neighbors with a recognizable
    // pattern so stray/partial/mis-addressed writes are visible.
    let base = shape.operand_addr as usize;
    for byte in mem.ram[base - 4..base + 12].iter_mut() {
        *byte = 0xAA;
    }
    mem.ram[base..base + 4].copy_from_slice(&0x3F80_0006u32.to_le_bytes());

    let mut io = ();
    let r = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 64, false);
    let (outcome, eip_norm) = match r {
        RunResult::Exception {
            vector, error_code, ..
        } => (format!("exc v={vector} e={error_code}"), cpu.regs.eip),
        _ => ("ran".to_string(), cpu.regs.eip - u32::from(prefixed)),
    };
    let mut regs = String::new();
    for i in 0..8usize {
        for byte in cpu.fpu.get_st(i).0 {
            regs.push_str(&format!("{byte:02x}"));
        }
        regs.push(' ');
    }
    let mem_window = mem.ram[base - 4..base + 12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!(
        "{outcome} eip={eip_norm:x} sw={:04x} tw={:04x} cw={:04x} top={} ax={:04x} fl={:04x} regs={regs} mem={mem_window}",
        cpu.fpu.status_word,
        cpu.fpu.tag_word,
        cpu.fpu.control_word,
        cpu.fpu.get_top(),
        cpu.regs.ax(),
        cpu.regs.flags.bits(),
    )
}

#[test]
fn x87_memory_lane_sweep_matches_the_full_route() {
    let forms: [(u8, u8); 4] = [(0xD9, 0), (0xD9, 3), (0xDB, 0), (0xDB, 3)];
    for (b0, reg) in forms {
        for shape in shapes() {
            for loaded in [true, false] {
                let lane = run_case(b0, reg, &shape, false, loaded);
                let full = run_case(b0, reg, &shape, true, loaded);
                assert_eq!(
                    lane, full,
                    "two-route divergence: form={b0:02X}/{reg} shape={} loaded={loaded}",
                    shape.name
                );
            }
        }
    }
}

fn run_raw_case(main_bytes: &[u8], prefixed: bool) -> String {
    let mut code = Vec::new();
    if prefixed {
        code.push(0x66);
    }
    code.extend_from_slice(main_bytes);
    code.push(0xF4);
    let (mut cpu, mut mem) = setup_pm32(&code);
    cpu.regs.esp = OPERAND_BASE; // valid base for the [ESP]-via-SIB rows
    mem.ram[OPERAND_BASE as usize..OPERAND_BASE as usize + 4]
        .copy_from_slice(&0x3F80_0006u32.to_le_bytes());
    let mut io = ();
    let r = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 64, false);
    let (outcome, eip_norm) = match r {
        RunResult::Exception {
            vector, error_code, ..
        } => (format!("exc v={vector} e={error_code}"), cpu.regs.eip),
        _ => ("ran".to_string(), cpu.regs.eip - u32::from(prefixed)),
    };
    let mut regs = String::new();
    for i in 0..8usize {
        for byte in cpu.fpu.get_st(i).0 {
            regs.push_str(&format!("{byte:02x}"));
        }
        regs.push(' ');
    }
    format!(
        "{outcome} eip={eip_norm:x} sw={:04x} tw={:04x} cw={:04x} top={} regs={regs}",
        cpu.fpu.status_word,
        cpu.fpu.tag_word,
        cpu.fpu.control_word,
        cpu.fpu.get_top(),
    )
}

/// Design §6.1 decline-equivalence rows: forms the lane must never
/// admit. Both the unprefixed (declined at the lane's own admission
/// check) and 66-prefixed (declined at the pos gate) routes go
/// outlined, so they must agree.
#[test]
fn x87_memory_lane_decline_equivalence() {
    let cases: [(&str, &[u8]); 5] = [
        ("sib_fld", &[0xD9, 0x04, 0x24]),  // FLD m32 [ESP] via SIB
        ("sib_fstp", &[0xD9, 0x1C, 0x24]), // FSTP m32 [ESP] via SIB
        ("d9_2_fst", &[0xD9, 0x10]),       // FST m32 [EAX] — D9/2
        ("db_1_fisttp", &[0xDB, 0x08]),    // FISTTP m32 [EAX] — DB/1
        ("db_2_fist", &[0xDB, 0x10]),      // FIST m32 [EAX] — DB/2
    ];
    for (name, bytes) in cases {
        let lane = run_raw_case(bytes, false);
        let full = run_raw_case(bytes, true);
        assert_eq!(lane, full, "decline-equivalence divergence: {name}");
    }

    // 67-prefixed and segment-override-prefixed forms decline via the
    // SAME pos-gate the register lane relies on — no unprefixed sibling
    // exists to two-route-compare (the prefix IS what is under test),
    // so pin the routing counter directly instead.
    let (mut cpu, mut mem) = setup_pm32(&[0x67, 0xD9, 0x00, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(
        cpu.x87_outlined, 1,
        "67-prefixed escape must take the outlined route"
    );

    let (mut cpu, mut mem) = setup_pm32(&[0x3E, 0xD9, 0x00, 0xF4]);
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(
        cpu.x87_outlined, 1,
        "segment-override-prefixed escape must take the outlined route"
    );
}

fn run_fault_case(main_bytes: &[u8], prefixed: bool, rig: fn(&mut Cpu)) -> String {
    let mut code = Vec::new();
    if prefixed {
        code.push(0x66);
    }
    code.extend_from_slice(main_bytes);
    code.push(0xF4);
    let (mut cpu, mut mem) = setup_pm32(&code);
    rig(&mut cpu);
    let mut io = ();
    let r = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    match r {
        RunResult::Exception {
            vector, error_code, ..
        } => format!("exc v={vector} e={error_code} eip={:x}", cpu.regs.eip),
        _ => format!("ran eip={:x}", cpu.regs.eip - u32::from(prefixed)),
    }
}

/// Fault axis: DS-limit #GP.
#[test]
fn x87_memory_lane_ds_limit_gp_matches_full_route() {
    let rig: fn(&mut Cpu) = |c| {
        c.regs.eax = 0x0002_0000;
        c.regs.ds_cache.limit = 0xFF;
        c.regs.ds_cache.granularity = false;
        c.regs.ds_cache.recompute_effective_limit();
    };
    let lane = run_fault_case(&[0xD9, 0x00], false, rig); // FLD m32 [EAX]
    let full = run_fault_case(&[0xD9, 0x00], true, rig);
    assert_eq!(lane, full, "DS-limit #GP two-route divergence");
    assert!(
        lane.starts_with("exc v=13 e=0"),
        "expected #GP(0), got {lane}"
    );
}

/// Fault axis: SS-limit #SS via `[ebp+disp]`.
#[test]
fn x87_memory_lane_ss_limit_ss_via_ebp_disp_matches_full_route() {
    let rig: fn(&mut Cpu) = |c| {
        c.regs.ebp = 0x0002_0000;
        c.regs.ss_cache.limit = 0xFF;
        c.regs.ss_cache.granularity = false;
        c.regs.ss_cache.recompute_effective_limit();
    };
    // FSTP m32 [EBP+8] (mod=01, reg=3, rm=5).
    let lane = run_fault_case(&[0xD9, 0x5D, 0x08], false, rig);
    let full = run_fault_case(&[0xD9, 0x5D, 0x08], true, rig);
    assert_eq!(lane, full, "SS-limit #SS two-route divergence");
    assert!(
        lane.starts_with("exc v=12 e=0"),
        "expected #SS(0), got {lane}"
    );
}

/// PM32 + paging harness: identity-maps the first 256 pages (1 MB) via
/// a page directory at physical 0x10000 and a page table at 0x11000,
/// mirroring `paging_integration_tests::setup_paging` (this file) minus
/// the code/stack placement, which the x87 memory-lane fault/straddle
/// rows need to control themselves.
fn setup_paging_pm32(code: &[u8], code_addr: usize) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cr0 = 0x8000_0001; // PE + PG
    cpu.regs.cpl = 0;
    cpu.regs.cr3 = 0x10000;
    mem.write_u32_physical(0x10000, 0x11007); // PDE#0 -> PT@0x11000, present+rw+user
    for page in 0..256u32 {
        let pte_addr = 0x11000 + (page as usize) * 4;
        mem.write_u32_physical(pte_addr, (page << 12) | 0x07); // present+rw+user
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
    cpu.regs.ss_cache = cpu.regs.ds_cache;

    cpu.regs.esp = 0x8000;
    cpu.regs.set_sp(0x8000);
    cpu.regs.set_ip(code_addr as u16);
    cpu.regs.eip = code_addr as u32;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[code_addr + i] = b;
    }
    (cpu, mem)
}

/// Fault axis: #PF not-present. Page 257 sits inside PDE#0's 4 MB span
/// (one page table covers 1024 entries) but its PTE was never written
/// by `setup_paging_pm32` (only pages 0..256 are mapped), so it
/// defaults to not-present with zero extra setup.
#[test]
fn x87_memory_lane_pf_not_present_matches_full_route() {
    fn run(prefixed: bool) -> String {
        let mut code = Vec::new();
        if prefixed {
            code.push(0x66);
        }
        code.extend_from_slice(&[0xD9, 0x00, 0xF4]); // FLD m32 [EAX]
        let (mut cpu, mut mem) = setup_paging_pm32(&code, 0x1000);
        cpu.regs.eax = 0x0010_1000; // page 257 — not present
        let mut io = ();
        let r = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
        match r {
            RunResult::Exception {
                vector, error_code, ..
            } => format!("exc v={vector} e={error_code} eip={:x}", cpu.regs.eip),
            _ => format!("ran eip={:x}", cpu.regs.eip - u32::from(prefixed)),
        }
    }
    let lane = run(false);
    let full = run(true);
    assert_eq!(lane, full, "#PF not-present two-route divergence");
    assert!(lane.starts_with("exc v=14"), "expected #PF, got {lane}");
}

/// Fault axis: write-#PF on a read-only page (store forms only).
/// CR0.WP must be set — supervisor-mode writes otherwise ignore the
/// page RW bit entirely (standard x86 behavior).
#[test]
fn x87_memory_lane_write_pf_on_readonly_page_matches_full_route() {
    fn run(prefixed: bool) -> String {
        let mut code = Vec::new();
        if prefixed {
            code.push(0x66);
        }
        code.extend_from_slice(&[0xD9, 0x19, 0xF4]); // FSTP m32 [ECX]
        let (mut cpu, mut mem) = setup_paging_pm32(&code, 0x1000);
        cpu.regs.cr0 |= 0x0001_0000; // WP
                                     // Re-mark page 100 present+read-only (present=1, rw=0, user=1).
        mem.write_u32_physical(0x11000 + 100 * 4, (100u32 << 12) | 0x05);
        cpu.regs.ecx = 100 * 0x1000;
        let mut io = ();
        let r = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
        match r {
            RunResult::Exception {
                vector, error_code, ..
            } => format!("exc v={vector} e={error_code} eip={:x}", cpu.regs.eip),
            _ => format!("ran eip={:x}", cpu.regs.eip - u32::from(prefixed)),
        }
    }
    let lane = run(false);
    let full = run(true);
    assert_eq!(lane, full, "write-#PF (RO page) two-route divergence");
    assert!(lane.starts_with("exc v=14"), "expected #PF, got {lane}");
}

/// Fault axis: a store operand straddling a page boundary whose second
/// page is absent must #PF with NO partial write — the in-page bytes
/// must be untouched (design §2, the two-pass write walk).
#[test]
fn x87_memory_lane_operand_straddle_pf_no_partial_write_matches_full_route() {
    // 255*0x1000 + 0xFFE straddles into page 256, which is absent
    // (only pages 0..256 are mapped by setup_paging_pm32).
    const STRADDLE_ADDR: u32 = 255 * 0x1000 + 0xFFE;
    fn run(prefixed: bool) -> (String, [u8; 2]) {
        let mut code = Vec::new();
        if prefixed {
            code.push(0x66);
        }
        code.extend_from_slice(&[0xD9, 0x19, 0xF4]); // FSTP m32 [ECX]
        let (mut cpu, mut mem) = setup_paging_pm32(&code, 0x1000);
        cpu.regs.ecx = STRADDLE_ADDR;
        mem.ram[STRADDLE_ADDR as usize] = 0xAA;
        mem.ram[STRADDLE_ADDR as usize + 1] = 0xBB;
        let mut io = ();
        let r = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
        let s = match r {
            RunResult::Exception {
                vector, error_code, ..
            } => format!("exc v={vector} e={error_code} eip={:x}", cpu.regs.eip),
            _ => format!("ran eip={:x}", cpu.regs.eip - u32::from(prefixed)),
        };
        (
            s,
            [
                mem.ram[STRADDLE_ADDR as usize],
                mem.ram[STRADDLE_ADDR as usize + 1],
            ],
        )
    }
    let (lane, lane_bytes) = run(false);
    let (full, full_bytes) = run(true);
    assert_eq!(lane, full, "straddle #PF two-route divergence");
    assert!(lane.starts_with("exc v=14"), "expected #PF, got {lane}");
    assert_eq!(
        lane_bytes,
        [0xAA, 0xBB],
        "lane must not partially write before the fault"
    );
    assert_eq!(
        full_bytes,
        [0xAA, 0xBB],
        "full route must not partially write before the fault"
    );
}

/// Design §11 fold 4: an ADMITTED instruction whose FETCH bytes
/// straddle a page boundary, both pages present. Exercises the
/// fetch_buf assembly path (lib.rs, the paging fetch-decode block)
/// feeding the lane's own 15-byte window read end-to-end — no fault,
/// the lane must still admit and execute correctly.
#[test]
fn x87_memory_lane_admits_across_a_fetch_page_straddle() {
    // [ECX+disp32] is 6 bytes (mod=10, reg=0(FLD), rm=1(ECX)); placing
    // the instruction at 0xFFD makes it span bytes 0xFFD..0x1003 —
    // across the page-0/page-1 boundary. Both pages are present
    // (identity-mapped by setup_paging_pm32).
    const CODE_ADDR: usize = 0x0FFD;
    const OPERAND_ADDR: usize = 0x0003_2000; // page 50 — present
    let mut code = vec![0xD9, 0x81];
    code.extend_from_slice(&(OPERAND_ADDR as u32).to_le_bytes());
    code.push(0xF4);
    let (mut cpu, mut mem) = setup_paging_pm32(&code, CODE_ADDR);
    cpu.regs.ecx = 0;
    mem.ram[OPERAND_ADDR..OPERAND_ADDR + 4].copy_from_slice(&3.5f32.to_le_bytes());
    let mut io = ();
    let _ = run_ext(&mut cpu, &mut mem, &mut io, &mut NoExec, 16, false);
    assert_eq!(cpu.x87_retired, 1, "escape must retire");
    assert_eq!(
        cpu.x87_outlined, 0,
        "admitted memory form must engage the lane even when its fetch straddles a page"
    );
    assert_eq!(
        cpu.fpu.get_st(0).to_f64(),
        3.5,
        "FLD must load the correct value despite the fetch-page straddle"
    );
}
