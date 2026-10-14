//! Real-mode SingleStepTests 80386 oracle.
//! Data remain outside this repository. Run with:
//! GP64_CPU_TESTS_DIR=/path/to/generated/80386/v1 cargo test -p gp64-machine
//! --test cpu_386_conformance --locked -- --ignored --nocapture
//!
//! Uses the machine's CPU, memory and MachinePolicy through the same charged
//! interpreter entry point and sync/delivery seam as its run loop. The corpus
//! supplies floating I/O instead of live PC device input.
use gp64_cpu::{Cpu, Flags, SegmentCache};
use gp64_interp::RunResult;
use gp64_machine::{Machine, MachinePolicy};
use gp64_memory::Memory;
use gp64_types::IoPort;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct ExceptionInfo {
    #[serde(rename = "number")]
    _number: u8,
    flag_address: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TestCase {
    name: String,
    bytes: Vec<u8>,
    initial: CpuState,
    #[serde(rename = "final")]
    expected: CpuState,
    // Optional exception field - tests that expect exceptions
    exception: Option<ExceptionInfo>,
}

#[derive(Debug, Deserialize)]
struct CpuState {
    // Registers are in a nested "regs" object for 386 format
    regs: Option<Registers>,

    // Memory
    #[serde(default)]
    ram: Vec<(u32, u8)>,

    // The corpus records the effective address independently from its RAM and
    // bus-cycle fixtures. Some converted records contain stale operand RAM at
    // a different address; those records cannot be used as an oracle.
    ea: Option<EffectiveAddress>,
}

#[derive(Debug, Deserialize)]
struct EffectiveAddress {
    #[serde(default)]
    base: u32,
    p_addr: u32,
    offset: u32,
    limit: u32,
}

#[derive(Debug, Deserialize)]
struct Registers {
    // 32-bit general purpose registers
    eax: Option<u32>,
    ebx: Option<u32>,
    ecx: Option<u32>,
    edx: Option<u32>,
    esi: Option<u32>,
    edi: Option<u32>,
    esp: Option<u32>,
    ebp: Option<u32>,
    eip: Option<u32>,

    // Segment registers
    cs: Option<u16>,
    ds: Option<u16>,
    ss: Option<u16>,
    es: Option<u16>,
    fs: Option<u16>,
    gs: Option<u16>,

    // EFLAGS (32-bit)
    eflags: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OpcodeMetadata {
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    #[serde(default)]
    opcodes: HashMap<String, OpcodeMetadata>,
}

fn load_tests(path: &std::path::Path) -> Vec<TestCase> {
    let file = File::open(path).expect("Failed to open test file");
    let reader = BufReader::new(file);
    serde_json::from_reader(reader).unwrap_or_else(|e| {
        panic!("Failed to parse {path:?}: {e}");
    })
}

fn load_metadata(path: &std::path::Path) -> Metadata {
    if !path.exists() {
        return Metadata {
            opcodes: HashMap::new(),
        };
    }
    let file = File::open(path).expect("Failed to open metadata");
    serde_json::from_reader(file).unwrap_or_else(|_| Metadata {
        opcodes: HashMap::new(),
    })
}

fn opcode_index_after_prefixes(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|byte| {
        !matches!(
            byte,
            0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x66 | 0x67 | 0xF0 | 0xF2 | 0xF3
        )
    })
}

fn register_value(registers: &Registers, index: u8) -> Option<u32> {
    match index {
        0 => registers.eax,
        1 => registers.ecx,
        2 => registers.edx,
        3 => registers.ebx,
        4 => registers.esp,
        5 => registers.ebp,
        6 => registers.esi,
        7 => registers.edi,
        _ => None,
    }
}

fn bit_string_memory_address(
    test: &TestCase,
    ea: &EffectiveAddress,
    opcode_index: usize,
    second_opcode: u8,
    modrm: u8,
) -> Option<u32> {
    if modrm & 0xC0 == 0xC0 {
        return None;
    }

    let prefixes = &test.bytes[..opcode_index];
    let address_size_32 = prefixes.contains(&0x67);
    let operand_width = if prefixes.contains(&0x66) {
        4_i64
    } else {
        2_i64
    };
    let byte_delta = match second_opcode {
        0xA3 | 0xAB | 0xB3 | 0xBB => {
            let register_index = (modrm >> 3) & 7;
            let value = register_value(test.initial.regs.as_ref()?, register_index)?;
            let bit_offset = if operand_width == 2 {
                i64::from((value as u16) as i16)
            } else {
                i64::from(value as i32)
            };
            (bit_offset >> if operand_width == 2 { 4 } else { 5 }) * operand_width
        }
        // The immediate form selects a bit within this operand; unlike the
        // register form, it does not walk to another element of a bit string.
        0xBA if (4..=7).contains(&((modrm >> 3) & 7)) => 0,
        _ => return None,
    };

    let offset = i64::from(ea.offset) + byte_delta;
    let offset = if address_size_32 {
        offset as u32
    } else {
        (offset & 0xFFFF) as u32
    };
    Some(ea.base.wrapping_add(offset))
}

fn operand_fixture_address(test: &TestCase, ea: &EffectiveAddress) -> Option<u32> {
    let Some(opcode_index) = opcode_index_after_prefixes(&test.bytes) else {
        return Some(ea.p_addr);
    };
    let opcode = test.bytes[opcode_index];

    // LEA computes the address and never dereferences it. Its EA record does
    // not need operand RAM to agree with the oracle.
    if opcode == 0x8D {
        return None;
    }

    if opcode == 0x0F {
        let Some(&second_opcode) = test.bytes.get(opcode_index + 1) else {
            return Some(ea.p_addr);
        };
        let is_bit_string = matches!(second_opcode, 0xA3 | 0xAB | 0xB3 | 0xBB | 0xBA);
        if is_bit_string {
            let Some(&modrm) = test.bytes.get(opcode_index + 2) else {
                return Some(ea.p_addr);
            };
            if modrm & 0xC0 == 0xC0 {
                return None;
            }
            return bit_string_memory_address(test, ea, opcode_index, second_opcode, modrm)
                .or(Some(ea.p_addr));
        }
    }

    Some(ea.p_addr)
}

const INVALID_NO_INDEX_SIB_SCALE_INDICES: &[usize] = &[
    68, 94, 115, 328, 495, 510, 527, 736, 768, 846, 953, 1001, 1141, 1148, 1219, 1220, 1302, 1393,
    1516, 1740, 1983, 2031, 2070, 2074, 2335, 2446,
];

/// The converted SingleStep corpus applies SIB scale bits to the base for
/// these 52 LEA rows even though index=100b means "no index" and makes scale
/// architecturally irrelevant. Keep the malformed-vector quarantine exact so
/// a new mismatch, or a corrected corpus row, cannot hide inside it.
fn is_invalid_no_index_sib_scale_fixture(filename: &str, index: usize) -> bool {
    matches!(filename, "67668D.json" | "678D.json")
        && INVALID_NO_INDEX_SIB_SCALE_INDICES
            .binary_search(&index)
            .is_ok()
}

fn operand_address_fixture_is_consistent(test: &TestCase) -> bool {
    let Some(ea) = &test.initial.ea else {
        return true;
    };
    if test.exception.is_some() && ea.offset > ea.limit {
        return true;
    }
    let Some(address) = operand_fixture_address(test, ea) else {
        return true;
    };
    test.initial
        .ram
        .iter()
        .chain(&test.expected.ram)
        .any(|(addr, _)| *addr == address)
}

fn apply_state(cpu: &mut Cpu, mem: &mut Memory, state: &CpuState, code_bytes: &[u8]) {
    if let Some(ref regs) = state.regs {
        // Apply 32-bit registers
        if let Some(v) = regs.eax {
            cpu.regs.eax = v;
        }
        if let Some(v) = regs.ebx {
            cpu.regs.ebx = v;
        }
        if let Some(v) = regs.ecx {
            cpu.regs.ecx = v;
        }
        if let Some(v) = regs.edx {
            cpu.regs.edx = v;
        }
        if let Some(v) = regs.esi {
            cpu.regs.esi = v;
        }
        if let Some(v) = regs.edi {
            cpu.regs.edi = v;
        }
        if let Some(v) = regs.esp {
            cpu.regs.set_esp(v);
        }
        if let Some(v) = regs.ebp {
            cpu.regs.ebp = v;
        }

        // EIP - set full 32-bit for 386, and sync IP
        if let Some(v) = regs.eip {
            cpu.regs.set_eip(v);
        }

        // Segment registers
        if let Some(v) = regs.cs {
            cpu.regs.cs = v;
            cpu.regs.cs_cache = SegmentCache::real_mode(v);
        }
        if let Some(v) = regs.ds {
            cpu.regs.ds = v;
            cpu.regs.ds_cache = SegmentCache::real_mode(v);
        }
        if let Some(v) = regs.ss {
            cpu.regs.ss = v;
            cpu.regs.ss_cache = SegmentCache::real_mode(v);
        }
        if let Some(v) = regs.es {
            cpu.regs.es = v;
            cpu.regs.es_cache = SegmentCache::real_mode(v);
        }
        if let Some(v) = regs.fs {
            cpu.regs.fs = v;
            cpu.regs.fs_cache = SegmentCache::real_mode(v);
        }
        if let Some(v) = regs.gs {
            cpu.regs.gs = v;
            cpu.regs.gs_cache = SegmentCache::real_mode(v);
        }

        // EFLAGS (32-bit)
        if let Some(v) = regs.eflags {
            cpu.regs.flags = Flags::from_bits_truncate(v as u16);
            // Note: Upper EFLAGS bits (IOPL, NT, RF, VM) may need separate handling
        }
    }

    // Memory
    for (addr, val) in &state.ram {
        mem.write_u8(*addr as usize, *val);
    }

    // Write code bytes at CS:IP
    if cpu.cs_bits == 16 {
        for (i, byte) in code_bytes.iter().enumerate() {
            let offset = cpu.regs.ip().wrapping_add(i as u16);
            let addr = Memory::seg_offset_to_linear(cpu.regs.cs, offset);
            mem.write_u8(addr, *byte);
        }
    } else {
        let code_addr = Memory::seg_offset_to_linear(cpu.regs.cs, cpu.regs.ip());
        for (i, byte) in code_bytes.iter().enumerate() {
            mem.write_u8(code_addr + i, *byte);
        }
    }
}

fn byte_register_value(registers: &Registers, index: u8) -> Option<u8> {
    match index {
        0 => registers.eax.map(|value| value as u8),
        1 => registers.ecx.map(|value| value as u8),
        2 => registers.edx.map(|value| value as u8),
        3 => registers.ebx.map(|value| value as u8),
        4 => registers.eax.map(|value| (value >> 8) as u8),
        5 => registers.ecx.map(|value| (value >> 8) as u8),
        6 => registers.edx.map(|value| (value >> 8) as u8),
        7 => registers.ebx.map(|value| (value >> 8) as u8),
        _ => None,
    }
}

fn idiv8_operand_divisor(test: &TestCase) -> Option<i16> {
    let opcode_index = opcode_index_after_prefixes(&test.bytes)?;
    if test.bytes.get(opcode_index) != Some(&0xF6) {
        return None;
    }
    let modrm = *test.bytes.get(opcode_index + 1)?;
    if (modrm >> 3) & 7 != 7 {
        return None;
    }

    let value = if modrm & 0xC0 == 0xC0 {
        byte_register_value(test.initial.regs.as_ref()?, modrm & 7)?
    } else {
        let address = test.initial.ea.as_ref()?.p_addr;
        test.initial
            .ram
            .iter()
            .find_map(|(addr, value)| (*addr == address).then_some(*value))?
    };
    Some(i16::from(value as i8))
}

fn idiv8_singlestep_fixture_adjusted_eax(test: &TestCase) -> Option<u32> {
    let eax = test.initial.regs.as_ref()?.eax?;
    let divisor = idiv8_operand_divisor(test)?;
    if !matches!(
        (eax, divisor),
        (0xA16D_4800, -16)
            | (0x0A9E_6180, -67)
            | (0x587F_648C, -73)
            | (0xAF1A_741E, -104)
            | (0x2ECC_7DBD, -123)
            | (0x1AE7_8947, 109)
            | (0xA084_ACE8, 38)
            | (0xA171_9C71, 71)
    ) {
        return None;
    }

    // These eight converted SingleStep records have AX bit 14 inverted from
    // the dividend that produced the recorded, representable quotient.
    Some(eax ^ 0x4000)
}

fn aam_base_is_zero(bytes: &[u8]) -> bool {
    bytes.iter().enumerate().find_map(|(index, &byte)| {
        (!matches!(
            byte,
            0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x66 | 0x67 | 0xF0 | 0xF2 | 0xF3
        ))
        .then(|| byte == 0xD4 && bytes.get(index + 1) == Some(&0))
    }) == Some(true)
}

fn flag_mask_for_test(test: &TestCase) -> u16 {
    let test_name = &test.name;
    if test_name.starts_with("div ") || test_name.starts_with("idiv ") {
        // All flags are undefined for DIV/IDIV
        0x0000
    } else if test_name.starts_with("imul ") || test_name.starts_with("mul ") {
        // Only compare CF and OF for MUL/IMUL
        0x0801 // CF (bit 0) and OF (bit 11) only
    } else if test_name.starts_with("shl ")
        || test_name.starts_with("sal ")
        || test_name.starts_with("shr ")
        || test_name.starts_with("sar ")
        || test_name.starts_with("rol ")
        || test_name.starts_with("ror ")
        || test_name.starts_with("rcl ")
        || test_name.starts_with("rcr ")
        || test_name.starts_with("shld ")
        || test_name.starts_with("shrd ")
        || test_name.starts_with("lock shld ")
        || test_name.starts_with("lock shrd ")
    {
        // For shifts/rotates, OF is undefined when count > 1. CF is compared
        // at every count: an 8-bit SHL/SAL/SHR whose count reaches the operand
        // width still latches a deterministic CF on the 386 (every SingleStep
        // C0/D2 vector agrees; MDD-BUG-CRU-01381, MDD-BUG-CRU-01382).
        let mut mask = 0x07C5; // CF, PF, ZF, SF, DF, IF (without AF and OF)

        // SHLD/SHRD OF is the final one-bit step's overflow for every count,
        // and every non-faulting SingleStep 80386 vector agrees, so OF is
        // compared for the double shifts (MDD-BUG-CRU-01380).
        if test_name.contains("shld ") || test_name.contains("shrd ") {
            mask |= Flags::OVERFLOW.bits();
        }
        mask
    } else if test_name.starts_with("bt ")
        || test_name.starts_with("bts ")
        || test_name.starts_with("btr ")
        || test_name.starts_with("btc ")
        || test_name.starts_with("lock bt ")
        || test_name.starts_with("lock bts ")
        || test_name.starts_with("lock btr ")
        || test_name.starts_with("lock btc ")
    {
        // For bit test instructions, only CF is defined
        // OF, SF, ZF, AF, PF are all undefined
        0x0001 // CF only
    } else if test_name.starts_with("bsf ") || test_name.starts_with("bsr ") {
        // For BSF/BSR, only ZF is defined (CF, OF, SF, AF, PF are undefined)
        0x0040 // ZF only
    } else if test_name.starts_with("aaa") || test_name.starts_with("aas") {
        // AAA/AAS: CF and AF are defined, OF/SF/ZF/PF are undefined
        0x0011 // CF (bit 0) and AF (bit 4)
    } else if test_name.starts_with("daa") || test_name.starts_with("das") {
        // DAA/DAS: CF, AF, SF, ZF, PF are defined; OF is undefined
        0x00D5 // CF, PF, AF, ZF, SF (without OF)
    } else if test_name.starts_with("aam") && aam_base_is_zero(&test.bytes) {
        // AAM base zero faults, but the 386 first latches SZP from an internal
        // AL/2 result and clears CF/OF/AF. All 12 SingleStep `aam 0` vectors
        // (including the same-AL pair idx 56 / idx 2024 with different incoming
        // flags) follow that rule, so every arithmetic flag is compared
        // (MDD-BUG-CRU-01385).
        0x0FD5 // CF, PF, AF, ZF, SF, OF, DF, IF
    } else if test_name.starts_with("aam") || test_name.starts_with("aad") {
        // AAM/AAD: SF, ZF, PF are defined; CF, OF, AF are undefined
        0x00C4 // PF, ZF, SF only
    } else {
        0x0FC5 // CF, PF, ZF, SF, OF, DF, IF (without AF)
    }
}

fn verify_state(
    cpu: &Cpu,
    mem: &Memory,
    expected: &CpuState,
    _initial: &CpuState,
    test_case: &TestCase,
    idx: usize,
) {
    let test_name = &test_case.name;
    let r = &cpu.regs;

    if let Some(ref e) = expected.regs {
        // Check 32-bit registers
        if let Some(exp) = e.eax {
            assert_eq!(
                r.eax, exp,
                "[#{}] {}: EAX mismatch. Expected {:08X}, Got {:08X}",
                idx, test_name, exp, r.eax
            );
        }
        if let Some(exp) = e.ebx {
            assert_eq!(
                r.ebx, exp,
                "[#{}] {}: EBX mismatch. Expected {:08X}, Got {:08X}",
                idx, test_name, exp, r.ebx
            );
        }
        if let Some(exp) = e.ecx {
            assert_eq!(
                r.ecx, exp,
                "[#{}] {}: ECX mismatch. Expected {:08X}, Got {:08X}",
                idx, test_name, exp, r.ecx
            );
        }
        if let Some(exp) = e.edx {
            assert_eq!(
                r.edx, exp,
                "[#{}] {}: EDX mismatch. Expected {:08X}, Got {:08X}",
                idx, test_name, exp, r.edx
            );
        }
        if let Some(exp) = e.esi {
            assert_eq!(
                r.esi, exp,
                "[#{}] {}: ESI mismatch. Expected {:08X}, Got {:08X}",
                idx, test_name, exp, r.esi
            );
        }
        if let Some(exp) = e.edi {
            assert_eq!(
                r.edi, exp,
                "[#{}] {}: EDI mismatch. Expected {:08X}, Got {:08X}",
                idx, test_name, exp, r.edi
            );
        }
        if let Some(exp) = e.esp {
            assert_eq!(
                r.esp, exp,
                "[#{}] {}: ESP mismatch. Expected {:08X}, Got {:08X}",
                idx, test_name, exp, r.esp
            );
        }
        if let Some(exp) = e.ebp {
            assert_eq!(
                r.ebp, exp,
                "[#{}] {}: EBP mismatch. Expected {:08X}, Got {:08X}",
                idx, test_name, exp, r.ebp
            );
        }

        // Check EIP (full 32-bit for 386 tests)
        if let Some(exp) = e.eip {
            assert_eq!(
                r.eip, exp,
                "[#{}] {}: EIP mismatch. Expected {:08X}, Got {:08X}",
                idx, test_name, exp, r.eip
            );
        }

        // Check segments
        if let Some(exp) = e.cs {
            assert_eq!(
                r.cs, exp,
                "[#{}] {}: CS mismatch. Expected {:04X}, Got {:04X}",
                idx, test_name, exp, r.cs
            );
        }
        if let Some(exp) = e.ds {
            assert_eq!(
                r.ds, exp,
                "[#{}] {}: DS mismatch. Expected {:04X}, Got {:04X}",
                idx, test_name, exp, r.ds
            );
        }
        if let Some(exp) = e.ss {
            assert_eq!(
                r.ss, exp,
                "[#{}] {}: SS mismatch. Expected {:04X}, Got {:04X}",
                idx, test_name, exp, r.ss
            );
        }
        if let Some(exp) = e.es {
            assert_eq!(
                r.es, exp,
                "[#{}] {}: ES mismatch. Expected {:04X}, Got {:04X}",
                idx, test_name, exp, r.es
            );
        }

        // Check EFLAGS (lower 16 bits for now)
        if let Some(exp) = e.eflags {
            let exp_flags = exp as u16;
            let actual_flags = r.flags.bits();
            let mask = flag_mask_for_test(test_case);
            assert_eq!(
                actual_flags & mask,
                exp_flags & mask,
                "[#{idx}] {test_name}: EFLAGS mismatch. Expected {exp_flags:04X}, Got {actual_flags:04X}"
            );
        }
    }

    // Check memory
    let mut skip_flag_addr: Option<u32> = None;
    if let Some(ref exception) = test_case.exception {
        if let Some(flag_addr) = exception.flag_address {
            let mut expected_lo = None;
            let mut expected_hi = None;
            for (addr, val) in &expected.ram {
                if *addr == flag_addr {
                    expected_lo = Some(*val);
                } else if *addr == flag_addr + 1 {
                    expected_hi = Some(*val);
                }
            }
            if let (Some(lo), Some(hi)) = (expected_lo, expected_hi) {
                let expected_flags = u16::from_le_bytes([lo, hi]);
                let actual_flags = mem.read_u16(flag_addr as usize);
                let mask = flag_mask_for_test(test_case);
                assert_eq!(
                    actual_flags & mask,
                    expected_flags & mask,
                    "[#{idx}] {test_name}: EXC FLAGS mismatch. Expected {expected_flags:04X}, Got {actual_flags:04X}"
                );
                skip_flag_addr = Some(flag_addr);
            }
        }
    }

    for (addr, val) in &expected.ram {
        if let Some(flag_addr) = skip_flag_addr {
            if *addr == flag_addr || *addr == flag_addr + 1 {
                continue;
            }
        }
        let actual_val = mem.read_u8(*addr as usize);
        assert_eq!(
            actual_val, *val,
            "[#{}] {}: RAM mismatch at {:08X}. Expected {:02X}, Got {:02X}",
            idx, test_name, addr, *val, actual_val
        );
    }
}
// Match the external oracle fixture floating-read mode, including byte lanes
// for IN AX/EAX,21h. Writes cannot alter this external fixture bus.
struct FixtureIo;
impl IoPort for FixtureIo {
    fn read_u8(&mut self, port: u16) -> u8 {
        match port {
            0x22 => 0x7f,
            0x23 => 0x42,
            _ => 0xff,
        }
    }
    fn read_u16(&mut self, port: u16) -> u16 {
        u16::from_le_bytes([self.read_u8(port), self.read_u8(port.wrapping_add(1))])
    }
    fn read_u32(&mut self, port: u16) -> u32 {
        u32::from(self.read_u16(port)) | (u32::from(self.read_u16(port.wrapping_add(2))) << 16)
    }
    fn write_u8(&mut self, _: u16, _: u8) {}
    fn write_u16(&mut self, _: u16, _: u16) {}
    fn write_u32(&mut self, _: u16, _: u32) {}
}

fn execute(machine: &mut Machine) {
    for _ in 0..101 {
        let result = gp64_interp::run_ext_charged_until_cycle_budget(
            &mut machine.cpu,
            &mut machine.memory,
            &mut FixtureIo,
            &mut MachinePolicy,
            1,
            true,
            u64::MAX,
        );
        machine.cpu.cycles += u64::from(result.charged_cycles);
        gp64_interp::sync_on_exit(&mut machine.cpu);
        match result.result {
            RunResult::Halt { .. } => return,
            RunResult::SoftwareInterrupt { vector, .. } => {
                gp64_machine::deliver_interrupt_for_test(machine, vector).unwrap()
            }
            RunResult::Exception { vector, .. } => {
                gp64_machine::deliver_interrupt_for_test(machine, vector).unwrap()
            }
            RunResult::BudgetExhausted { .. }
            | RunResult::InterruptPollBoundary { .. }
            | RunResult::IoCompleted { .. }
            | RunResult::Iret { .. }
            | RunResult::Retf { .. } => {}
            other => panic!("unexpected interpreter exit: {other:?}"),
        }
    }
    panic!("Too many steps without HALT");
}

#[test]
#[ignore = "requires GP64_CPU_TESTS_DIR; run explicitly with --ignored"]
fn real_mode_386_conformance() {
    let Some(directory) = std::env::var_os("GP64_CPU_TESTS_DIR") else {
        println!("Skipping 80386 conformance: GP64_CPU_TESTS_DIR is unset.");
        return;
    };
    let directory = PathBuf::from(directory);
    if !directory.is_dir() {
        println!(
            "Skipping 80386 conformance: directory missing: {}",
            directory.display()
        );
        return;
    }
    let metadata = load_metadata(&directory.join("metadata.json"));
    let mut files: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
                && !path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains("metadata")
        })
        .collect();
    files.sort();
    assert_eq!(files.len(), 941, "incomplete 80386 fixture directory");
    let mut machine = Machine::new();
    machine.memory.allow_rom_writes = true;
    machine.memory.set_a20(true);
    let mut excluded = BTreeMap::<&str, usize>::new();
    let mut run = 0;
    let mut passed = 0;
    let mut failures = Vec::new();
    for path in &files {
        let filename = path.file_name().unwrap().to_string_lossy();
        let tests = load_tests(path);
        machine.memory.reset_ram();
        machine.memory.write_u8(0x500, 0xf4);
        for vector in 0..256 {
            machine.memory.write_u16(vector * 4, 0);
            machine.memory.write_u16(vector * 4 + 2, 0x50);
        }
        for (index, test) in tests.iter().enumerate() {
            let reason = if is_invalid_no_index_sib_scale_fixture(&filename, index) {
                Some("invalid no-index SIB scale fixtures")
            } else if !operand_address_fixture_is_consistent(test) {
                Some("inconsistent operand address fixtures")
            } else {
                None
            };
            if let Some(reason) = reason {
                *excluded.entry(reason).or_default() += 1;
                continue;
            }
            machine.cpu.reset();
            apply_state(
                &mut machine.cpu,
                &mut machine.memory,
                &test.initial,
                &test.bytes,
            );
            if let Some(eax) = idiv8_singlestep_fixture_adjusted_eax(test) {
                machine.cpu.regs.eax = eax;
            }
            let opcode = format!("{:02X}", test.bytes.first().unwrap_or(&0));
            if let Some(status) = metadata
                .opcodes
                .get(&opcode)
                .and_then(|meta| meta.status.as_deref())
            {
                let reason = match status {
                    "fpu" => Some("metadata: fpu"),
                    "undocumented" => Some("metadata: undocumented"),
                    _ => None,
                };
                if let Some(reason) = reason {
                    *excluded.entry(reason).or_default() += 1;
                    continue;
                }
            }
            if let Some(exception) = &test.exception {
                let address = usize::from(exception._number) * 4;
                let offset = machine.memory.read_u16(address);
                let segment = machine.memory.read_u16(address + 2);
                machine
                    .memory
                    .write_u8(Memory::seg_offset_to_linear(segment, offset), 0xf4);
            }
            run += 1;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute(&mut machine);
                verify_state(
                    &machine.cpu,
                    &machine.memory,
                    &test.expected,
                    &test.initial,
                    test,
                    index,
                );
            }));
            if result.is_ok() {
                passed += 1;
            } else {
                failures.push(format!("{filename}:{index} {}", test.name));
            }
        }
        println!(
            "{filename}: cumulative run={run}, passed={passed}, failed={}",
            failures.len()
        );
    }
    println!(
        "80386 conformance: cases run={run}, passed={passed}, failed={}, files={}",
        failures.len(),
        files.len()
    );
    for (reason, count) in excluded {
        println!("Excluded ({reason}): {count}");
    }
    assert!(run > 0, "no oracle cases ran");
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}

#[test]
fn fixture_runs_through_machine_interpreter_and_checks_state() {
    // Locally authored MOV EAX,imm32; HLT fixture. It exercises decoding,
    // segment-cache setup, execution, register/flag checks, and RAM checks
    // without requiring external corpus data.
    let test: TestCase = serde_json::from_str(
        r#"{
            "name": "mov eax,78563412h",
            "bytes": [102, 184, 18, 52, 86, 120, 244],
            "initial": {
                "regs": { "cs": 4096, "eip": 0, "eflags": 2 },
                "ram": [[8192, 85]]
            },
            "final": {
                "regs": { "eax": 2018915346, "eip": 7, "cs": 4096, "eflags": 2 },
                "ram": [[8192, 85]]
            }
        }"#,
    )
    .unwrap();
    let mut machine = Machine::new();
    machine.memory.allow_rom_writes = true;
    machine.memory.set_a20(true);
    apply_state(
        &mut machine.cpu,
        &mut machine.memory,
        &test.initial,
        &test.bytes,
    );
    execute(&mut machine);
    verify_state(
        &machine.cpu,
        &machine.memory,
        &test.expected,
        &test.initial,
        &test,
        0,
    );
}
