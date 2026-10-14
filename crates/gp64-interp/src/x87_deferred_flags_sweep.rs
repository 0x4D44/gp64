use super::test_helpers::setup;
use super::*;

const DATA_BASE: usize = 0x2000;
const STACK_VALS: [f32; 4] = [3.5, -1.25, 0.3, 7.0];
const F64_OFF: u16 = 16;
const INT_OFF: u16 = 24;
const CW_OFF: u16 = 28;
const SCRATCH_OFF: u16 = 0x30;

/// Flag-residual seeders prepended to the program. In a batched run
/// each leaves a lazy residual LIVE at the following escape.
const SEEDERS: [(&str, &[u8]); 3] = [
    ("none", &[]),
    // MOV AL,0xFF; ADD AL,1 -> CF/ZF set via a lazy ADD residual.
    ("add_cf", &[0xB0, 0xFF, 0x04, 0x01]),
    // STC; INC AX -> lazy INC residual whose materialization must READ
    // the seeded CF=1 from flags_in (get_cf consumes the input flags).
    ("stc_inc", &[0xF9, 0x40]),
];

fn drive(cpu: &mut Cpu, mem: &mut Memory, budget: u32) -> RunResult {
    let mut io = ();
    for _ in 0..256 {
        let r = run_ext(cpu, mem, &mut io, &mut NoExec, budget, false);
        match r {
            RunResult::Halt { .. }
            | RunResult::Exception { .. }
            | RunResult::SoftwareInterrupt { .. } => return r,
            _ => {}
        }
    }
    panic!("driver cap exceeded (no Halt/Exception in 256 slices)");
}

fn run_route(program: &[u8], budget: u32) -> String {
    let (mut cpu, mut mem) = setup(program);
    // BP-default (SS) rows address DATA via SS:BP (SS=0 == DS here);
    // SIB rows use [EBX*2 + disp32] with EBX=8.
    cpu.regs.ebp = DATA_BASE as u32;
    cpu.regs.ebx = 8;
    for (k, v) in STACK_VALS.iter().enumerate() {
        mem.ram[DATA_BASE + k * 4..DATA_BASE + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    mem.ram[DATA_BASE + F64_OFF as usize..DATA_BASE + F64_OFF as usize + 8]
        .copy_from_slice(&2.75f64.to_le_bytes());
    mem.ram[DATA_BASE + INT_OFF as usize..DATA_BASE + INT_OFF as usize + 4]
        .copy_from_slice(&(-123_456i32).to_le_bytes());
    mem.ram[DATA_BASE + CW_OFF as usize..DATA_BASE + CW_OFF as usize + 2]
        .copy_from_slice(&0x027Fu16.to_le_bytes());
    let r = drive(&mut cpu, &mut mem, budget);
    let outcome = match r {
        RunResult::Halt { .. } => "halt".to_string(),
        RunResult::Exception {
            vector, error_code, ..
        } => format!("exc v={vector} e={error_code}"),
        RunResult::SoftwareInterrupt { vector, .. } => format!("int v={vector}"),
        _ => unreachable!("drive() only returns Halt/Exception/SoftwareInterrupt"),
    };
    let mut regs = String::new();
    for i in 0..8usize {
        for byte in cpu.fpu.get_st(i).0 {
            regs.push_str(&format!("{byte:02x}"));
        }
        regs.push(' ');
    }
    let mut data = String::new();
    for b in &mem.ram[DATA_BASE..DATA_BASE + 0x40] {
        data.push_str(&format!("{b:02x}"));
    }
    format!(
        "{outcome} eip={:x} sw={:04x} tw={:04x} cw={:04x} top={} ax={:04x} fl={:04x} regs={regs} data={data}",
        cpu.regs.eip,
        cpu.fpu.status_word,
        cpu.fpu.tag_word,
        cpu.fpu.control_word,
        cpu.fpu.get_top(),
        cpu.regs.ax(),
        cpu.regs.flags.bits(),
    )
}

/// The nine admitted dispatches: (label, escape byte, /reg field, data
/// offset the operand points at).
const FORMS: [(&str, u8, u8, u16); 11] = [
    ("fld_m32", 0xD9, 0, 0),
    ("fld_m64", 0xDD, 0, F64_OFF),
    ("fstp_m32", 0xD9, 3, SCRATCH_OFF),
    ("fstp_m64", 0xDD, 3, SCRATCH_OFF),
    ("fild_m32", 0xDB, 0, INT_OFF),
    ("fistp_m32", 0xDB, 3, SCRATCH_OFF),
    ("fldcw", 0xD9, 5, CW_OFF),
    ("fadd_m32", 0xD8, 0, 0),
    ("fdiv_m64", 0xDC, 6, F64_OFF),
    ("fcom_m32", 0xD8, 2, 0),
    ("fcomp_m32", 0xD8, 3, 4),
];

/// Build every escape row: each form in each addressing shape, plus the
/// Generic decline-parity rows and the reachable fault rows.
fn rows() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for &(label, esc, reg, off) in &FORMS {
        let target = (DATA_BASE as u32) + u32::from(off);
        let t16 = target as u16;
        // disp16: mod00 rm110.
        out.push((
            format!("{label}/disp16"),
            vec![esc, (reg << 3) | 0x06, t16 as u8, (t16 >> 8) as u8],
        ));
        // BP+disp8 (SS default): mod01 rm110, BP = DATA_BASE.
        out.push((
            format!("{label}/bp_ss"),
            vec![esc, (reg << 3) | 0x46, off as u8],
        ));
        // 67h [disp32]: mod00 rm100, SIB base=disp32 index=none.
        let d = target.to_le_bytes();
        out.push((
            format!("{label}/sib_disp32"),
            vec![0x67, esc, (reg << 3) | 0x04, 0x25, d[0], d[1], d[2], d[3]],
        ));
        // 67h [EBX*2+disp32]: SIB ss=1 index=EBX base=disp32; EBX=8.
        let d2 = (target - 16).to_le_bytes();
        out.push((
            format!("{label}/sib_ebx2"),
            vec![
                0x67,
                esc,
                (reg << 3) | 0x04,
                0x5D,
                d2[0],
                d2[1],
                d2[2],
                d2[3],
            ],
        ));
    }
    // Generic decline-parity rows (never fat-dispatched; the batch
    // route's Generic fallback must still materialize before its
    // pre-write/re-read round-trip or these rows corrupt EFLAGS).
    out.push((
        "fiadd_m32_generic/disp16".into(),
        vec![0xDA, 0x06, 0x00, 0x20],
    ));
    out.push((
        "fild_m16_generic/disp16".into(),
        vec![0xDF, 0x06, INT_OFF as u8, 0x20],
    ));
    out.push(("fsqrt_generic_reg".into(), vec![0xD9, 0xFA]));
    // Fault rows reachable in real mode. 32-bit addressing past the
    // 0xFFFF cached limit -> #GP with a live residual pending.
    out.push((
        "gp_limit_67_disp32".into(),
        vec![0x67, 0xD9, 0x04, 0x25, 0x00, 0x00, 0x02, 0x00],
    ));
    // D9 /1 memory form has no instruction: decode-fail #UD arm.
    out.push(("ud_decode_d9_1".into(), vec![0xD9, 0x0E, 0x00, 0x20]));
    // FISTTP m32 (DB /1) decodes but needs SSE3: feature-check #UD.
    out.push(("ud_fisttp_db_1".into(), vec![0xDB, 0x0E, 0x00, 0x20]));
    out
}

#[test]
fn x87_deferred_flags_sweep_batch_matches_single_step() {
    for (label, escape) in rows() {
        for (sname, seeder) in SEEDERS {
            for preload in [true, false] {
                let mut program = Vec::new();
                if preload {
                    for k in 0..4usize {
                        let disp = (DATA_BASE + k * 4) as u16;
                        program.extend_from_slice(&[0xD9, 0x06, disp as u8, (disp >> 8) as u8]);
                    }
                }
                program.extend_from_slice(seeder);
                program.extend_from_slice(&escape);
                program.push(0xF4);
                let batch = run_route(&program, 256);
                let step = run_route(&program, 1);
                assert_eq!(
                    batch, step,
                    "batch vs single-step divergence at {label} seeder={sname} preload={preload}"
                );
            }
        }
    }
}

/// Back-to-back escapes in one batch: one residual carried across
/// several Epilogues (HLD v2 sweep row). FLD m32; FADD m32; FSTP m32.
#[test]
fn residual_survives_back_to_back_deferred_escapes() {
    for (sname, seeder) in SEEDERS {
        let mut program = Vec::new();
        program.extend_from_slice(seeder);
        program.extend_from_slice(&[0xD9, 0x06, 0x00, 0x20]); // FLD m32
        program.extend_from_slice(&[0xD8, 0x06, 0x04, 0x20]); // FADD m32
        program.extend_from_slice(&[0xD9, 0x1E, 0x30, 0x20]); // FSTP m32
        program.push(0xF4);
        let batch = run_route(&program, 256);
        let step = run_route(&program, 1);
        assert_eq!(batch, step, "back-to-back divergence seeder={sname}");
    }
}
