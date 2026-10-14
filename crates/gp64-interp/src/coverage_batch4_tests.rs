use super::test_helpers::{make_descriptor, setup, write_gdt_entry};
use super::*;
use gp64_cpu::{Cpu, Flags};
use gp64_memory::Memory;

// ========================================================================
// Helper: setup protected-mode environment with 32-bit segments
// ========================================================================

fn setup_pm32(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 8 * 8 - 1;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]); // null
                                                            // selector 0x08: ring-0 code, 32-bit, base 0, limit 0xFFFFF (granularity 4K)
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    );
    // selector 0x10: ring-0 data, 32-bit, base 0, limit 0xFFFFF
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    // selector 0x18: ring-3 code, 32-bit
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0xFA, 0x0C),
    );
    // selector 0x20: ring-3 data, 32-bit
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x20,
        make_descriptor(0, 0xFFFFF, 0xF2, 0x0C),
    );
    // selector 0x28: second code segment, 16-bit, base 0
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x28,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    );
    // selector 0x30: FS data, 32-bit, base 0x1000
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x30,
        make_descriptor(0x1000, 0xFFFFF, 0x92, 0x0C),
    );
    // selector 0x38: GS data, 32-bit, base 0x2000
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x38,
        make_descriptor(0x2000, 0xFFFFF, 0x92, 0x0C),
    );
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = gdt_limit;
    cpu.regs.cr0 |= 1; // PE bit
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

#[test]
fn s3_mmio_dword_preserves_width_and_ends_the_instruction_batch() {
    // MOV dword ptr [000A8108h], 000051A0h; INC EAX
    let (mut cpu, mut mem) = setup_pm32(&[
        0xC7, 0x05, 0x08, 0x81, 0x0A, 0x00, 0xA0, 0x51, 0x00, 0x00, 0x40,
    ]);
    mem.configure_s3_mmio_decoder(true);
    cpu.regs.eax = 7;
    let mut io = ();

    let result = run(&mut cpu, &mut mem, &mut io, 2);

    let writes = mem.pending_s3_mmio_writes();
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "unexpected interpreter boundary: {result:?}; writes={writes:?}"
    );
    assert_eq!(cpu.regs.eip, 0x010A);
    assert_eq!(cpu.regs.eax, 7, "the following instruction must not retire");
    assert_eq!(
        writes,
        &[gp64_memory::S3MmioWrite {
            offset: 0x8108,
            width: gp64_memory::S3MmioWidth::Dword,
            value: 0x0000_51A0,
        }]
    );
}

fn setup_pm16(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let gdt_base: u32 = 0x10000;
    let gdt_limit: u16 = 6 * 8 - 1;
    write_gdt_entry(&mut mem, gdt_base as usize, [0u8; 8]);
    // selector 0x08: ring-0 code, 16-bit
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    );
    // selector 0x10: ring-0 data, 16-bit
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x10,
        make_descriptor(0, 0xFFFFF, 0x92, 0x08),
    );
    // selector 0x18: another code segment, 16-bit
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x18,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x08),
    );
    // selector 0x20: ring-0 data with base 0x1000
    write_gdt_entry(
        &mut mem,
        gdt_base as usize + 0x20,
        make_descriptor(0x1000, 0xFFFFF, 0x92, 0x08),
    );
    // selector 0x28: ring-0 data with base 0x2000
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

fn setup_near_transfer(cs_bits: u32, start_ip: u32, code: &[u8]) -> (Cpu, Memory) {
    let (mut cpu, mut mem) = if cs_bits == 16 {
        setup_pm16(&[])
    } else {
        setup_pm32(&[])
    };
    let fetch_ip = if cs_bits == 16 {
        start_ip as u16 as usize
    } else {
        start_ip as usize
    };
    cpu.regs.set_eip(start_ip);
    if cs_bits == 16 {
        cpu.regs.set_ip(start_ip as u16);
    }
    mem.ram[fetch_ip..fetch_ip + code.len()].copy_from_slice(code);
    (cpu, mem)
}

fn relative_transfer_code(opcode: u8, prefixed: bool, operand_size_32: bool) -> Vec<u8> {
    let mut code = Vec::with_capacity(7);
    if prefixed {
        code.push(0x66);
    }
    code.push(opcode);
    if operand_size_32 {
        code.extend_from_slice(&0x0001_0000u32.to_le_bytes());
    } else {
        code.extend_from_slice(&0u16.to_le_bytes());
    }
    code
}

#[test]
fn test_near_relative_transfers_use_operand_size_for_targets_and_returns() {
    for cs_bits in [16, 32] {
        for prefixed in [false, true] {
            let operand_size_32 = if prefixed {
                cs_bits == 16
            } else {
                cs_bits == 32
            };
            let start_ip: u32 = if cs_bits == 16 { 0x0100 } else { 0x0001_0100 };
            let displacement = if operand_size_32 { 0x0001_0000 } else { 0 };

            let call_code = relative_transfer_code(0xE8, prefixed, operand_size_32);
            let call_len = call_code.len() as u32;
            let expected_return = if operand_size_32 {
                start_ip.wrapping_add(call_len)
            } else {
                (start_ip as u16).wrapping_add(call_len as u16) as u32
            };
            let expected_target = if operand_size_32 {
                expected_return.wrapping_add(displacement as u32)
            } else {
                (expected_return as u16).wrapping_add(displacement as u16) as u32
            };
            let (mut cpu, mut mem) = setup_near_transfer(cs_bits, start_ip, &call_code);
            let mut io = ();
            let _ = run(&mut cpu, &mut mem, &mut io, 1);
            assert_eq!(
                cpu.regs.eip, expected_target,
                "CALL target width: CS{cs_bits}, prefix={prefixed}"
            );
            if operand_size_32 {
                let sp = cpu.regs.sp();
                assert_eq!(sp, 0x7FFC);
                assert_eq!(
                    u32::from_le_bytes(mem.ram[sp as usize..sp as usize + 4].try_into().unwrap()),
                    expected_return,
                    "CALL return width: CS{cs_bits}, prefix={prefixed}"
                );
            } else {
                let sp = cpu.regs.sp();
                assert_eq!(sp, 0x7FFE);
                assert_eq!(
                    u16::from_le_bytes(mem.ram[sp as usize..sp as usize + 2].try_into().unwrap())
                        as u32,
                    expected_return,
                    "CALL return width: CS{cs_bits}, prefix={prefixed}"
                );
            }

            let jmp_code = relative_transfer_code(0xE9, prefixed, operand_size_32);
            let (mut cpu, mut mem) = setup_near_transfer(cs_bits, start_ip, &jmp_code);
            let mut io = ();
            let _ = run(&mut cpu, &mut mem, &mut io, 1);
            assert_eq!(
                cpu.regs.eip, expected_target,
                "JMP target width: CS{cs_bits}, prefix={prefixed}"
            );

            let jcc_code = if prefixed {
                let mut code = vec![0x66, 0x0F, 0x84];
                if operand_size_32 {
                    code.extend_from_slice(&0x0001_0000u32.to_le_bytes());
                } else {
                    code.extend_from_slice(&0u16.to_le_bytes());
                }
                code
            } else {
                let mut code = vec![0x0F, 0x84];
                if operand_size_32 {
                    code.extend_from_slice(&0x0001_0000u32.to_le_bytes());
                } else {
                    code.extend_from_slice(&0u16.to_le_bytes());
                }
                code
            };
            let jcc_len = jcc_code.len() as u32;
            let jcc_next = if operand_size_32 {
                start_ip.wrapping_add(jcc_len)
            } else {
                (start_ip as u16).wrapping_add(jcc_len as u16) as u32
            };
            let jcc_target = if operand_size_32 {
                jcc_next.wrapping_add(displacement as u32)
            } else {
                (jcc_next as u16).wrapping_add(displacement as u16) as u32
            };
            let (mut cpu, mut mem) = setup_near_transfer(cs_bits, start_ip, &jcc_code);
            cpu.regs.flags.insert(Flags::ZERO);
            let mut io = ();
            let _ = run(&mut cpu, &mut mem, &mut io, 1);
            assert_eq!(
                cpu.regs.eip, jcc_target,
                "JZ target width: CS{cs_bits}, prefix={prefixed}"
            );
        }
    }
}

#[test]
fn test_near_indirect_transfers_use_operand_size_for_targets_and_returns() {
    for cs_bits in [16, 32] {
        for prefixed in [false, true] {
            let operand_size_32 = if prefixed {
                cs_bits == 16
            } else {
                cs_bits == 32
            };
            let start_ip: u32 = if cs_bits == 16 { 0x0100 } else { 0x0001_0100 };
            let expected_target = if operand_size_32 { 0x1234_5678 } else { 0x5678 };
            let instruction_len: u32 = (if prefixed { 1 } else { 0 }) + 2;
            let expected_return = if operand_size_32 {
                start_ip.wrapping_add(instruction_len)
            } else {
                (start_ip as u16).wrapping_add(instruction_len as u16) as u32
            };

            for (is_call, modrm) in [(true, 0xD0), (false, 0xE0)] {
                let mut code = Vec::with_capacity(3);
                if prefixed {
                    code.push(0x66);
                }
                code.extend_from_slice(&[0xFF, modrm]);
                let (mut cpu, mut mem) = setup_near_transfer(cs_bits, start_ip, &code);
                cpu.regs.eax = 0x1234_5678;
                let mut io = ();
                let _ = run(&mut cpu, &mut mem, &mut io, 1);
                assert_eq!(
                    cpu.regs.eip,
                    expected_target,
                    "{} target width: CS{cs_bits}, prefix={prefixed}",
                    if is_call { "CALL" } else { "JMP" }
                );
                if is_call {
                    if operand_size_32 {
                        let sp = cpu.regs.sp();
                        assert_eq!(sp, 0x7FFC);
                        assert_eq!(
                            u32::from_le_bytes(
                                mem.ram[sp as usize..sp as usize + 4].try_into().unwrap()
                            ),
                            expected_return,
                            "CALL return width: CS{cs_bits}, prefix={prefixed}"
                        );
                    } else {
                        let sp = cpu.regs.sp();
                        assert_eq!(sp, 0x7FFE);
                        assert_eq!(
                            u16::from_le_bytes(
                                mem.ram[sp as usize..sp as usize + 2].try_into().unwrap()
                            ) as u32,
                            expected_return,
                            "CALL return width: CS{cs_bits}, prefix={prefixed}"
                        );
                    }
                }
            }
        }
    }
}

// ========================================================================
// 1. RunResult::V86Switch executed() coverage (line 110)
// ========================================================================

#[test]
fn test_run_result_v86switch_executed() {
    let r = RunResult::V86Switch {
        executed: 5,
        frame: gp64_cpu::V86SwitchFrame {
            eip: 0,
            cs: 0,
            eflags: 0,
            esp: 0,
            ss: 0,
            es: 0,
            ds: 0,
            fs: 0,
            gs: 0,
        },
    };
    assert_eq!(r.executed(), 5);
}

#[test]
fn test_run_result_mode_change_executed() {
    let r = RunResult::ModeChange { executed: 10 };
    assert_eq!(r.executed(), 10);
}

#[test]
fn test_run_result_invlpg_executed() {
    let r = RunResult::Invlpg {
        executed: 2,
        linear_addr: 0x1000,
    };
    assert_eq!(r.executed(), 2);
}

#[test]
fn test_run_result_task_switch_executed() {
    let r = RunResult::TaskSwitch { executed: 3 };
    assert_eq!(r.executed(), 3);
}

#[test]
fn test_run_result_task_switch_jmp_executed() {
    let r = RunResult::TaskSwitchJmp {
        selector: 0x28,
        executed: 4,
    };
    assert_eq!(r.executed(), 4);
}

#[test]
fn test_run_result_task_switch_call_executed() {
    let r = RunResult::TaskSwitchCall {
        selector: 0x30,
        executed: 7,
    };
    assert_eq!(r.executed(), 7);
}

#[test]
fn test_run_result_call_gate_executed() {
    let r = RunResult::CallGate {
        selector: 0x40,
        return_cs: 0x08,
        return_eip: 0x1234,
        executed: 6,
    };
    assert_eq!(r.executed(), 6);
}

#[test]
fn test_run_result_iret_executed() {
    let r = RunResult::Iret { executed: 1 };
    assert_eq!(r.executed(), 1);
}

#[test]
fn test_run_result_retf_executed() {
    let r = RunResult::Retf { executed: 9 };
    assert_eq!(r.executed(), 9);
}

#[test]
fn test_run_result_script_sentinel_executed() {
    let r = RunResult::ScriptSentinel { executed: 11 };
    assert_eq!(r.executed(), 11);
}

// ========================================================================
// 2. seg_base CS/FS/GS in protected mode (lines 892, 895-896)
// ========================================================================

#[test]
fn test_seg_base_fs_protected_mode() {
    // MOV EAX, FS:[0x10]  => 64 A1 10 00 00 00
    // FS base is 0x1000 in our PM32 setup, so reads from 0x1010
    let (mut cpu, mut mem) = setup_pm32(&[0x64, 0xA1, 0x10, 0x00, 0x00, 0x00]);
    mem.ram[0x1010] = 0xAB;
    mem.ram[0x1011] = 0xCD;
    mem.ram[0x1012] = 0xEF;
    mem.ram[0x1013] = 0x12;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12EFCDAB);
}

#[test]
fn test_seg_base_gs_protected_mode() {
    // MOV EAX, GS:[0x20]  => 65 A1 20 00 00 00
    // GS base is 0x2000, so reads from 0x2020
    let (mut cpu, mut mem) = setup_pm32(&[0x65, 0xA1, 0x20, 0x00, 0x00, 0x00]);
    mem.ram[0x2020] = 0x11;
    mem.ram[0x2021] = 0x22;
    mem.ram[0x2022] = 0x33;
    mem.ram[0x2023] = 0x44;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x44332211);
}

// ========================================================================
// 3. get_reg32 / set_reg32 for ESP, ESI, EDI (lines 1033-1055)
// ========================================================================

#[test]
fn test_mov_esp_imm32() {
    // MOV ESP, 0x12345678  => 66 BC 78 56 34 12 (with o32 prefix in 16-bit mode)
    // In 32-bit PM: BC 78 56 34 12
    let (mut cpu, mut mem) = setup_pm32(&[0xBC, 0x78, 0x56, 0x34, 0x12]);
    cpu.regs.esp = 0x8000; // initial
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esp, 0x12345678);
}

#[test]
fn test_mov_esi_imm32() {
    // MOV ESI, 0xAABBCCDD  => BE DD CC BB AA
    let (mut cpu, mut mem) = setup_pm32(&[0xBE, 0xDD, 0xCC, 0xBB, 0xAA]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esi, 0xAABBCCDD);
}

#[test]
fn test_mov_edi_imm32() {
    // MOV EDI, 0x11223344  => BF 44 33 22 11
    let (mut cpu, mut mem) = setup_pm32(&[0xBF, 0x44, 0x33, 0x22, 0x11]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edi, 0x11223344);
}

#[test]
fn test_xchg_eax_esp_covers_get_set_reg32_esp() {
    // XCHG EAX, ESP => 94 (in 32-bit mode)
    let (mut cpu, mut mem) = setup_pm32(&[0x94]);
    cpu.regs.eax = 0xAAAA_BBBB;
    cpu.regs.esp = 0xCCCC_DDDD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xCCCC_DDDD);
    assert_eq!(cpu.regs.esp, 0xAAAA_BBBB);
}

#[test]
fn test_xchg_eax_esi() {
    // XCHG EAX, ESI => 96
    let (mut cpu, mut mem) = setup_pm32(&[0x96]);
    cpu.regs.eax = 0x1111_2222;
    cpu.regs.esi = 0x3333_4444;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x3333_4444);
    assert_eq!(cpu.regs.esi, 0x1111_2222);
}

#[test]
fn test_xchg_eax_edi() {
    // XCHG EAX, EDI => 97
    let (mut cpu, mut mem) = setup_pm32(&[0x97]);
    cpu.regs.eax = 0xAAAA_0000;
    cpu.regs.edi = 0x0000_BBBB;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0000_BBBB);
    assert_eq!(cpu.regs.edi, 0xAAAA_0000);
}

// ========================================================================
// 4. get_reg8 / set_reg8 for CH, DH, BH (lines 1068-1086)
// ========================================================================

#[test]
fn test_mov_ch_imm8() {
    // MOV CH, 0x42 => B5 42
    let (mut cpu, mut mem) = setup(&[0xB5, 0x42]);
    cpu.regs.ecx = 0x0000_0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!((cpu.regs.ecx >> 8) & 0xFF, 0x42); // CH
}

#[test]
fn test_mov_dh_imm8() {
    // MOV DH, 0x99 => B6 99
    let (mut cpu, mut mem) = setup(&[0xB6, 0x99]);
    cpu.regs.edx = 0x0000_0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!((cpu.regs.edx >> 8) & 0xFF, 0x99); // DH
}

#[test]
fn test_mov_bh_imm8_readback() {
    // MOV BH, 0xAA => B7 AA
    // Then MOV [0x200], BH => 88 3E 00 02
    let (mut cpu, mut mem) = setup(&[0xB7, 0xAA, 0x88, 0x3E, 0x00, 0x02]);
    cpu.regs.ebx = 0x0000_0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(mem.ram[0x200], 0xAA);
}

#[test]
fn test_xchg_ch_al() {
    // XCHG CH, AL => 86 E8  (modrm=0xE8: reg=CH(5), rm=AL(0))
    let (mut cpu, mut mem) = setup(&[0x86, 0xE8]);
    cpu.regs.set_al(0x11);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_00FF) | (0x22 << 8);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x22);
    assert_eq!((cpu.regs.ecx >> 8) & 0xFF, 0x11);
}

// ========================================================================
// 5. get_af with LazyOp::None (line 1284)
//    DAA/DAS read AF from flags_reg when lazy=None
// ========================================================================

#[test]
fn test_get_af_lazy_none_via_sahf_daa() {
    // SAHF loads AH into flags (sets AF if AH bit 4 is set)
    // Then DAA checks AF via get_af(LazyOp::None)
    // SAHF = 0x9E, DAA = 0x27
    let (mut cpu, mut mem) = setup(&[0x9E, 0x27]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF_00FF) | (0x10 << 8); // AH=0x10 => AF=1
    cpu.regs.set_al(0x0A); // AL=0x0A, with AF=1 DAA adjusts
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    // DAA with AF=1: AL=0x0A+0x06=0x10
    assert_eq!(cpu.regs.al(), 0x10);
}

#[test]
fn test_get_af_lazy_none_via_sahf_das() {
    // SAHF = 0x9E, DAS = 0x2F
    let (mut cpu, mut mem) = setup(&[0x9E, 0x2F]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF_00FF) | (0x10 << 8); // AH=0x10 => AF=1
    cpu.regs.set_al(0x10); // AL=0x10, with AF=1 DAS adjusts: 0x10-0x06=0x0A
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.al(), 0x0A);
}

// ========================================================================
// 6. 16-bit addressing mode arms (lines 2057-2069)
//    decode_modrm_addr16: BX+DI, BP+SI, DI, [BP] with md!=0
// ========================================================================

#[test]
fn test_addr16_bx_di() {
    // MOV AL, [BX+DI] => 8A 01 (modrm=0x01, rm=1 => BX+DI)
    let (mut cpu, mut mem) = setup(&[0x8A, 0x01]);
    cpu.regs.ebx = 0x200;
    cpu.regs.edi = 0x10;
    mem.ram[0x210] = 0x77;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x77);
}

#[test]
fn test_addr16_bp_si() {
    // MOV AL, [BP+SI+0x04] => 8A 42 04 (modrm=0x42, md=1 rm=2 => BP+SI+disp8)
    let (mut cpu, mut mem) = setup(&[0x8A, 0x42, 0x04]);
    cpu.regs.ebp = 0x300;
    cpu.regs.esi = 0x10;
    mem.ram[0x314] = 0xBB;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xBB);
}

#[test]
fn test_addr16_di_only() {
    // MOV AL, [DI] => 8A 05 (modrm=0x05, rm=5 => DI)
    let (mut cpu, mut mem) = setup(&[0x8A, 0x05]);
    cpu.regs.edi = 0x400;
    mem.ram[0x400] = 0xCC;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xCC);
}

#[test]
fn test_addr16_bp_md1() {
    // MOV AL, [BP+0x02] => 8A 46 02 (modrm=0x46, md=1 rm=6 => BP+disp8)
    let (mut cpu, mut mem) = setup(&[0x8A, 0x46, 0x02]);
    cpu.regs.ebp = 0x500;
    mem.ram[0x502] = 0xDD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xDD);
}

#[test]
fn test_addr16_bp_md2() {
    // MOV AL, [BP+0x1000] => 8A 86 00 10 (modrm=0x86, md=2 rm=6 => BP+disp16)
    let (mut cpu, mut mem) = setup(&[0x8A, 0x86, 0x00, 0x10]);
    cpu.regs.ebp = 0x200;
    mem.ram[0x1200] = 0xEE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xEE);
}

// ========================================================================
// 7. 32-bit addressing mode SIB paths (lines 2112-2182)
// ========================================================================

#[test]
fn test_addr32_sib_ecx_index() {
    // MOV EAX, [ECX*1 + 0x0200] => 8B 04 0D 00 02 00 00
    // In 32-bit PM: SIB with scale=0, index=ECX(1), base=5 (disp32)
    let (mut cpu, mut mem) = setup_pm32(&[0x8B, 0x04, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.ecx = 0x10;
    mem.ram[0x210] = 0xAA;
    mem.ram[0x211] = 0xBB;
    mem.ram[0x212] = 0xCC;
    mem.ram[0x213] = 0xDD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xDDCCBBAA);
}

#[test]
fn test_addr32_sib_esp_base() {
    // MOV EAX, [ESP+0x04] => 8B 44 24 04
    // SIB byte 0x24: base=ESP(4), index=none(4), scale=0
    let (mut cpu, mut mem) = setup_pm32(&[0x8B, 0x44, 0x24, 0x04]);
    cpu.regs.esp = 0x7000;
    mem.ram[0x7004] = 0x11;
    mem.ram[0x7005] = 0x22;
    mem.ram[0x7006] = 0x33;
    mem.ram[0x7007] = 0x44;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x44332211);
}

#[test]
fn test_addr32_sib_edi_base() {
    // MOV EAX, [EDI] => 8B 07 (no SIB, rm=7 => EDI)
    let (mut cpu, mut mem) = setup_pm32(&[0x8B, 0x07]);
    cpu.regs.edi = 0x3000;
    mem.ram[0x3000] = 0xFF;
    mem.ram[0x3001] = 0xEE;
    mem.ram[0x3002] = 0xDD;
    mem.ram[0x3003] = 0xCC;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xCCDDEEFF);
}

#[test]
fn test_addr32_sib_base5_md0_disp32() {
    // MOV EAX, [EBP*1 + 0x0300] via SIB: md=0, base=5, index!=4
    // SIB with index=EBP(5), base=5, md=0 => disp32 + EBP*scale
    // Encoding: 8B 04 2D 00 03 00 00  (modrm=0x04 rm=4, sib=0x2D: idx=5 base=5 scale=0)
    let (mut cpu, mut mem) = setup_pm32(&[0x8B, 0x04, 0x2D, 0x00, 0x03, 0x00, 0x00]);
    cpu.regs.ebp = 0x100;
    mem.ram[0x400] = 0xDE;
    mem.ram[0x401] = 0xAD;
    mem.ram[0x402] = 0xBE;
    mem.ram[0x403] = 0xEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xEFBEADDE);
}

#[test]
fn test_addr32_sib_base_disp32() {
    // MOV EAX, [ECX + EBX*2 + disp32]
    // modrm=0x84 (md=2, rm=4 SIB), SIB=0x59 (scale=1 idx=3(EBX) base=1(ECX))
    // disp32=0x00001000
    let (mut cpu, mut mem) = setup_pm32(&[0x8B, 0x84, 0x59, 0x00, 0x10, 0x00, 0x00]);
    cpu.regs.ecx = 0x100;
    cpu.regs.ebx = 0x80;
    // addr = ECX + EBX*2 + 0x1000 = 0x100 + 0x100 + 0x1000 = 0x1200
    mem.ram[0x1200] = 0x01;
    mem.ram[0x1201] = 0x02;
    mem.ram[0x1202] = 0x03;
    mem.ram[0x1203] = 0x04;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x04030201);
}

#[test]
fn test_addr32_rm_disp32() {
    // MOV EAX, [disp32] => 8B 05 00 30 00 00  (rm=5, md=0 => disp32)
    let (mut cpu, mut mem) = setup_pm32(&[0x8B, 0x05, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000] = 0x10;
    mem.ram[0x3001] = 0x20;
    mem.ram[0x3002] = 0x30;
    mem.ram[0x3003] = 0x40;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x40302010);
}

// ========================================================================
// 8. push16/pop16 with big stack (lines 2237-2310)
// ========================================================================

#[test]
fn test_push16_big_stack_32bit_pm() {
    // In 32-bit PM: PUSH AX => 66 50 (o16 prefix in 32-bit mode)
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0x50]);
    cpu.regs.set_ax(0xBEEF);
    let initial_esp = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esp, initial_esp - 2);
    let addr = cpu.regs.esp as usize;
    assert_eq!(
        u16::from_le_bytes([mem.ram[addr], mem.ram[addr + 1]]),
        0xBEEF
    );
}

#[test]
fn test_pop16_big_stack_32bit_pm() {
    // In 32-bit PM: PUSH AX then POP BX => 66 50 66 5B
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0x50, 0x66, 0x5B]);
    cpu.regs.set_ax(0xCAFE);
    cpu.regs.set_bx(0x0000);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.bx(), 0xCAFE);
}

// ========================================================================
// 9. push32/pop32 with wrapping 16-bit stack (lines 2278-2355)
// ========================================================================

#[test]
fn test_push32_16bit_stack_boundary_fault_386() {
    // Real mode push32 with SP near boundary
    // o32 PUSH EAX => 66 50
    let (mut cpu, mut mem) = setup(&[0x66, 0x50]);
    cpu.regs.eax = 0xDEADBEEF;
    cpu.regs.set_sp(0x0002); // Only 2 bytes before wrap
    cpu.regs.esp = 0x0002;
    cpu.regs.ss = 0x0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 12, .. }),
        "386 real-mode PUSHD crossing SS limit should #SS, got {:?}",
        result
    );
    assert_eq!(cpu.regs.sp(), 0x0002);
}

#[test]
fn test_pop32_16bit_stack_boundary_fault_386() {
    // o32 POP EAX => 66 58
    let (mut cpu, mut mem) = setup(&[0x66, 0x58]);
    cpu.regs.eax = 0;
    // Place value across boundary: SP=0xFFFE, 4 bytes at FFFE..0001
    cpu.regs.set_sp(0xFFFE);
    cpu.regs.esp = 0xFFFE;
    cpu.regs.ss = 0x0000;
    mem.ram[0xFFFE] = 0xEF;
    mem.ram[0xFFFF] = 0xBE;
    mem.ram[0x0000] = 0xAD;
    mem.ram[0x0001] = 0xDE;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 12, .. }),
        "386 real-mode POPD crossing SS limit should #SS, got {:?}",
        result
    );
    assert_eq!(cpu.regs.eax, 0);
    assert_eq!(cpu.regs.sp(), 0xFFFE);
}

#[test]
fn test_pop_sreg32_reads_word_and_advances_four_386() {
    // o32 POP ES => 66 07. The selector is only a word, so SP=FFFE is
    // valid; operand-size 32 still advances the 16-bit stack pointer by 4.
    let (mut cpu, mut mem) = setup(&[0x66, 0x07]);
    cpu.regs.set_sp(0xFFFE);
    cpu.regs.esp = 0xFFFE;
    cpu.regs.ss = 0;
    mem.ram[0xFFFE] = 0xEF;
    mem.ram[0xFFFF] = 0xBE;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
    assert_eq!(cpu.regs.es, 0xBEEF);
    assert_eq!(cpu.regs.sp(), 0x0002);
}

#[test]
fn test_pop_sreg32_selector_crossing_stack_limit_faults_386() {
    // At SP=FFFF, the 16-bit selector read itself crosses the SS limit.
    let (mut cpu, mut mem) = setup(&[0x66, 0x07]);
    cpu.regs.set_sp(0xFFFF);
    cpu.regs.esp = 0xFFFF;
    cpu.regs.ss = 0x0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 12, .. }),
        "386 real-mode POPD word crossing SS top should #SS, got {:?}",
        result
    );
    assert_eq!(cpu.regs.sp(), 0xFFFF);
}

#[test]
fn test_push16_16bit_stack_wrapping() {
    // PUSH AX => 50 (16-bit mode)
    let (mut cpu, mut mem) = setup(&[0x50]);
    cpu.regs.set_ax(0xCAFE);
    cpu.regs.set_sp(0x0000); // SP=0, will wrap to 0xFFFE
    cpu.regs.esp = 0x0000;
    cpu.regs.ss = 0x0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.sp(), 0xFFFE);
    assert_eq!(
        u16::from_le_bytes([mem.ram[0xFFFE], mem.ram[0xFFFF]]),
        0xCAFE
    );
}

#[test]
fn test_push16_16bit_stack_wraps_sp_0001_8086_80186() {
    for cpu_type in [gp64_cpu::CpuType::I8086, gp64_cpu::CpuType::I80186] {
        // PUSH AX => 50 (16-bit mode)
        let (mut cpu, mut mem) = setup(&[0x50]);
        cpu.cpu_type = cpu_type;
        cpu.regs.set_ax(0xCAFE);
        cpu.regs.set_sp(0x0001);
        cpu.regs.esp = 0x0001;
        cpu.regs.ss = 0x0000;
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::BudgetExhausted { executed: 1 }),
            "{:?} real-mode PUSH should wrap, got {:?}",
            cpu_type,
            result
        );
        assert_eq!(cpu.regs.sp(), 0xFFFF);
        assert_eq!(
            u16::from_le_bytes([mem.ram[0xFFFF], mem.ram[0x0000]]),
            0xCAFE
        );
    }
}

#[test]
fn test_push16_386_real_mode_sp_0001_stack_fault() {
    // PUSH AX => 50 (16-bit mode)
    let (mut cpu, mut mem) = setup(&[0x50]);
    cpu.regs.set_ax(0xCAFE);
    cpu.regs.set_sp(0x0001);
    cpu.regs.esp = 0x0001;
    cpu.regs.ss = 0x0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 12, .. }),
        "386 real-mode PUSH crossing SS limit should #SS, got {:?}",
        result
    );
    assert_eq!(cpu.regs.sp(), 0x0001);
}

#[test]
fn test_pop16_16bit_stack_wrapping_sp_ffff() {
    for cpu_type in [gp64_cpu::CpuType::I8086, gp64_cpu::CpuType::I80186] {
        // POP AX => 58
        // SP=0xFFFF crosses boundary: bytes at FFFF and 0000
        let (mut cpu, mut mem) = setup(&[0x58]);
        cpu.cpu_type = cpu_type;
        cpu.regs.eax = 0;
        cpu.regs.set_sp(0xFFFF);
        cpu.regs.esp = 0xFFFF;
        cpu.regs.ss = 0x0000;
        mem.ram[0xFFFF] = 0xAA;
        mem.ram[0x0000] = 0xBB;
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::BudgetExhausted { executed: 1 }),
            "{:?} real-mode POP should wrap, got {:?}",
            cpu_type,
            result
        );
        assert_eq!(cpu.regs.ax(), 0xBBAA);
        assert_eq!(cpu.regs.sp(), 0x0001);
    }
}

#[test]
fn test_pop16_386_real_mode_sp_ffff_stack_fault() {
    // POP AX => 58
    let (mut cpu, mut mem) = setup(&[0x58]);
    cpu.regs.eax = 0;
    cpu.regs.set_sp(0xFFFF);
    cpu.regs.esp = 0xFFFF;
    cpu.regs.ss = 0x0000;
    mem.ram[0xFFFF] = 0xAA;
    mem.ram[0x0000] = 0xBB;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 12, .. }),
        "386 real-mode POP crossing SS limit should #SS, got {:?}",
        result
    );
    assert_eq!(cpu.regs.ax(), 0);
    assert_eq!(cpu.regs.sp(), 0xFFFF);
}

// ========================================================================
// 10. CWDE (0x98 with o32 prefix) and CDQ (0x99 with o32 prefix)
// ========================================================================

#[test]
fn test_cwde_positive() {
    // CWDE in 32-bit mode: 0x98
    let (mut cpu, mut mem) = setup_pm32(&[0x98]);
    cpu.regs.eax = 0x0000_007F; // AX=0x007F (positive)
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0000_007F);
}

#[test]
fn test_cwde_negative() {
    // CWDE: sign-extend AX to EAX
    let (mut cpu, mut mem) = setup_pm32(&[0x98]);
    cpu.regs.eax = 0x0000_FF80; // AX=0xFF80 (negative -128)
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF_FF80);
}

#[test]
fn test_cdq_positive() {
    // CDQ in 32-bit mode: 0x99
    let (mut cpu, mut mem) = setup_pm32(&[0x99]);
    cpu.regs.eax = 0x7FFF_FFFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edx, 0x0000_0000);
}

#[test]
fn test_cdq_negative() {
    // CDQ: sign-extend EAX to EDX:EAX
    let (mut cpu, mut mem) = setup_pm32(&[0x99]);
    cpu.regs.eax = 0x8000_0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edx, 0xFFFF_FFFF);
}

// ========================================================================
// 11. MOV to CS (0x8E seg_idx=1) => #UD (lines 7481-7489)
// ========================================================================

#[test]
fn test_mov_cs_rm16_is_ud() {
    // MOV CS, AX => 8E C8 (modrm=0xC8: seg_idx=1, rm=AX)
    let (mut cpu, mut mem) = setup(&[0x8E, 0xC8]);
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            ..
        }
    ));
}

#[test]
fn mov_to_reserved_sreg_field_raises_ud() {
    // MDD-BUG-ANVIL-00007: MOV Sreg,r/m16 (0x8E) with reserved reg field /6 or
    // /7 must #UD (only ES/CS/SS/DS/FS/GS = 0..=5 exist), before reading the
    // source operand. Register form (modrm >= 0xC0), rm = AX.
    for modrm in [0xF0u8, 0xF8u8] {
        // reg=6 (F0), reg=7 (F8)
        let (mut cpu, mut mem) = setup(&[0x8E, modrm]);
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::Exception { vector: 6, .. }),
            "MOV to reserved sreg field (modrm {modrm:#04X}) must #UD, got {result:?}"
        );
        assert_eq!(cpu.regs.eip, 0x0100, "#UD reported at the instruction IP");
    }
}

#[test]
fn mov_from_reserved_sreg_field_raises_ud() {
    // MDD-BUG-ANVIL-00007: MOV r/m16,Sreg (0x8C) with reserved reg field /6 or
    // /7 must #UD and not write the destination (MOV from CS /1 stays valid).
    for modrm in [0xF0u8, 0xF8u8] {
        // reg=6/7, rm = AX (register destination)
        let (mut cpu, mut mem) = setup(&[0x8C, modrm]);
        cpu.regs.eax = 0x1234_5678;
        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);
        assert!(
            matches!(result, RunResult::Exception { vector: 6, .. }),
            "MOV from reserved sreg field (modrm {modrm:#04X}) must #UD, got {result:?}"
        );
        assert_eq!(
            cpu.regs.eax, 0x1234_5678,
            "destination must be unchanged on #UD"
        );
        assert_eq!(cpu.regs.eip, 0x0100, "#UD reported at the instruction IP");
    }
}

// ========================================================================
// 12. MOV seg, r/m16 with memory operand (line 7494)
//     MOV ES, [0x200]
// ========================================================================

#[test]
fn test_mov_seg_from_memory() {
    // MOV ES, [0x200] => 8E 06 00 02 (modrm=0x06, md=0 rm=6 => disp16, seg=ES)
    let (mut cpu, mut mem) = setup(&[0x8E, 0x06, 0x00, 0x02]);
    mem.ram[0x200] = 0x34;
    mem.ram[0x201] = 0x12;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.es, 0x1234);
}

// ========================================================================
// 13. BOUND 32-bit (lines 3681-3692) — #BR exception
// ========================================================================

#[test]
fn test_bound_32bit_in_range() {
    // o32 prefix + BOUND EAX, [0x200] => 66 62 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x62, 0x06, 0x00, 0x02]);
    cpu.regs.eax = 50;
    // Lower bound at 0x200: 10
    mem.ram[0x200..0x204].copy_from_slice(&10u32.to_le_bytes());
    // Upper bound at 0x204: 100
    mem.ram[0x204..0x208].copy_from_slice(&100u32.to_le_bytes());
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // Should succeed (no exception)
    assert!(matches!(result, RunResult::BudgetExhausted { executed: 1 }));
}

#[test]
fn test_bound_32bit_out_of_range() {
    // o32 prefix + BOUND EAX, [0x200] => 66 62 06 00 02
    let (mut cpu, mut mem) = setup(&[0x66, 0x62, 0x06, 0x00, 0x02]);
    cpu.regs.eax = 200; // Out of range
                        // Lower bound at 0x200: 10
    mem.ram[0x200..0x204].copy_from_slice(&10u32.to_le_bytes());
    // Upper bound at 0x204: 100
    mem.ram[0x204..0x208].copy_from_slice(&100u32.to_le_bytes());
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // Should get #BR (vector 5)
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 5,
            error_code: 0,
            ..
        }
    ));
}

#[test]
fn test_bound_32bit_below_lower() {
    let (mut cpu, mut mem) = setup(&[0x66, 0x62, 0x06, 0x00, 0x02]);
    cpu.regs.eax = 5; // Below lower bound
    mem.ram[0x200..0x204].copy_from_slice(&10u32.to_le_bytes());
    mem.ram[0x204..0x208].copy_from_slice(&100u32.to_le_bytes());
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 5,
            error_code: 0,
            ..
        }
    ));
}

// ========================================================================
// 14. ENTER instruction with level > 0 (lines 3922-3956)
// ========================================================================

#[test]
fn test_enter_16bit_level0() {
    // ENTER 0x0010, 0x00 => C8 10 00 00
    let (mut cpu, mut mem) = setup(&[0xC8, 0x10, 0x00, 0x00]);
    cpu.regs.set_sp(0x1000);
    cpu.regs.esp = 0x1000;
    cpu.regs.set_bp(0x2000);
    cpu.regs.ebp = 0x2000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // ENTER pushes BP, sets BP=SP, then SP -= 0x10
    // After push BP: SP = 0x0FFE
    // BP = 0x0FFE
    // SP = 0x0FFE - 0x10 = 0x0FEE
    assert_eq!(cpu.regs.bp(), 0x0FFE);
    assert_eq!(cpu.regs.sp(), 0x0FEE);
}

#[test]
fn test_enter_32bit_level0() {
    // o32 ENTER 0x0020, 0x00 => 66 C8 20 00 00 (32-bit operand)
    let (mut cpu, mut mem) = setup(&[0x66, 0xC8, 0x20, 0x00, 0x00]);
    cpu.regs.set_sp(0x2000);
    cpu.regs.esp = 0x2000;
    cpu.regs.ebp = 0x3000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // ENTER pushes EBP (4 bytes), sets EBP=ESP, then ESP -= 0x20
    // After push EBP: ESP = 0x2000 - 4 = 0x1FFC (in 16-bit stack mode)
    let new_bp = cpu.regs.ebp & 0xFFFF;
    assert!(new_bp < 0x2000);
}

#[test]
fn test_enter_16bit_level1() {
    // ENTER 0x0008, 0x01 => C8 08 00 01
    // Level 1: push BP, push [BP-2] (frame pointer), then push frame, BP=frame, SP-=size
    let (mut cpu, mut mem) = setup(&[0xC8, 0x08, 0x00, 0x01]);
    cpu.regs.set_sp(0x1000);
    cpu.regs.esp = 0x1000;
    cpu.regs.set_bp(0x0F00);
    cpu.regs.ebp = 0x0F00;
    // Place a frame pointer at [old_BP - 2]
    mem.ram[0x0EFE] = 0x50;
    mem.ram[0x0EFF] = 0x0E;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // Level 1 ENTER does more stack work but should complete
    // SP should be below 0x1000 by at least 8 + push overhead
    assert!(cpu.regs.sp() < 0x1000);
}

// ========================================================================
// 15. HLT with TF set => INT 1 (lines 9951-9957)
// ========================================================================

#[test]
fn test_hlt_with_tf_set_generates_int1() {
    // HLT => 0xF4
    let (mut cpu, mut mem) = setup(&[0xF4]);
    cpu.regs.flags.insert(Flags::TRAP);
    cpu.interrupt_inhibit = 0;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    // HLT with TF should generate debug exception (vector 1)
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 1,
            error_code: 0,
            ..
        }
    ));
}

// ========================================================================
// 16. HLT in protected mode with CPL != 0 => #GP (lines 9928-9937)
// ========================================================================

#[test]
fn test_hlt_ring3_generates_gp() {
    let (mut cpu, mut mem) = setup_pm32(&[0xF4]); // HLT
    cpu.regs.cpl = 3; // Ring 3
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        }
    ));
}

// ========================================================================
// 17. Reserved opcode 0xFF /7 => #UD (lines 9909-9917)
// ========================================================================

#[test]
fn test_ff_sub7_undefined_opcode() {
    // 0xFF with sub_op 7: modrm=0xF8..0xFF, reg field=7
    // 0xFF 0xF8 => r/m=AX, sub_op=7 (reserved)
    let (mut cpu, mut mem) = setup(&[0xFF, 0xF8]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 6,
            error_code: 0,
            ..
        }
    ));
}

// ========================================================================
// 18. Shift operations with various counts (lines 2925-2939)
//     SHL/SHR with count >= 8 for 8-bit ops
// ========================================================================

#[test]
fn test_shl_r8_count_8() {
    // SHL AL, 8 via SHL AL, CL => D2 E0 (with CL=8)
    let (mut cpu, mut mem) = setup(&[0xD2, 0xE0]);
    cpu.regs.set_al(0xFF);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 8;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00); // shifted out entirely
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // last bit shifted out was bit 0 = 1
}

#[test]
fn test_shl_r8_count_greater_than_8() {
    // SHL AL, CL with CL=10
    let (mut cpu, mut mem) = setup(&[0xD2, 0xE0]);
    cpu.regs.set_al(0xFF);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 0x0A;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // count>8, CF=false
}

#[test]
fn test_shr_r8_count_8() {
    // SHR AL, CL => D2 E8 with CL=8
    let (mut cpu, mut mem) = setup(&[0xD2, 0xE8]);
    cpu.regs.set_al(0x80);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 8;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // bit 7 was 1
}

#[test]
fn test_shr_r8_count_greater_than_8() {
    let (mut cpu, mut mem) = setup(&[0xD2, 0xE8]);
    cpu.regs.set_al(0xFF);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 0x0C;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x00);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // count>8
}

#[test]
fn test_shl_r8_count_0_no_change() {
    // SHL AL, CL with CL=0 => no operation
    let (mut cpu, mut mem) = setup(&[0xD2, 0xE0]);
    cpu.regs.set_al(0x42);
    cpu.regs.ecx &= 0xFFFF_FF00; // CL=0
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x42);
}

#[test]
fn test_shr_r8_count_1_of() {
    // SHR AL, 1 => D0 E8, sets OF = MSB of original value
    let (mut cpu, mut mem) = setup(&[0xD0, 0xE8]);
    cpu.regs.set_al(0x80); // MSB=1
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x40);
    assert!(cpu.regs.flags.contains(Flags::OVERFLOW)); // MSB was 1
}

// ========================================================================
// 19. POPF (0x9D) basic coverage
// ========================================================================

#[test]
fn test_popf_restores_flags() {
    // PUSHF + POPF round-trip: 9C 9D
    let (mut cpu, mut mem) = setup(&[0x9C, 0x9D]);
    cpu.regs.set_sp(0x1000);
    cpu.regs.esp = 0x1000;
    cpu.regs
        .flags
        .insert(Flags::CARRY | Flags::ZERO | Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// 20. Write to ROM area is silently ignored (line 1834)
// ========================================================================

#[test]
fn test_write_to_rom_area_ignored() {
    // MOV [0xF000], AL => A2 00 F0
    // Writing to ROM area 0xF0000..0x100000 should be silently ignored
    // This is in real-mode: DS:offset where DS=0, offset=0xF000
    // But segment << 4 + offset for real ROM would need segment setup
    // Use a direct approach with segment base
    let (mut cpu, mut mem) = setup(&[0xA2, 0x00, 0x00]);
    cpu.regs.ds = 0xF000; // DS=0xF000, so linear = 0xF0000 + 0x0000
    cpu.regs.set_al(0xFF);
    let original = mem.ram[0xF0000];
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0xF0000], original); // Write should be ignored
}

// ========================================================================
// 21. VGA memory write routing (lines 1829-1830, 1863-1864, 1894-1895)
// ========================================================================

#[test]
fn test_write_u8_vga_range() {
    // MOV [0xA000:0x0000], AL => write to VGA memory
    let (mut cpu, mut mem) = setup(&[0xA2, 0x00, 0x00]);
    cpu.regs.ds = 0xA000;
    cpu.regs.set_al(0x42);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // VGA write goes through Memory pipeline, verify it arrived
    let val = mem.read_u8(0xA0000);
    assert_eq!(val, 0x42);
}

#[test]
fn test_write_u16_vga_range() {
    // MOV [0xA000:0x0000], AX => A3 00 00 (with DS=0xA000)
    let (mut cpu, mut mem) = setup(&[0xA3, 0x00, 0x00]);
    cpu.regs.ds = 0xA000;
    cpu.regs.set_ax(0x1234);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.read_u16(0xA0000), 0x1234);
}

#[test]
fn test_write_u32_vga_range() {
    // o32 MOV [0xA000:0x0000], EAX => 66 A3 00 00 (with DS=0xA000)
    let (mut cpu, mut mem) = setup(&[0x66, 0xA3, 0x00, 0x00]);
    cpu.regs.ds = 0xA000;
    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.read_u32(0xA0000), 0xDEADBEEF);
}

// ========================================================================
// 22. PUSHAD / POPAD (0x60 / 0x61) in 32-bit mode
// ========================================================================

#[test]
fn test_pushad_popad_32bit() {
    // PUSHAD = 0x60, POPAD = 0x61 (in 32-bit PM mode)
    let (mut cpu, mut mem) = setup_pm32(&[0x60, 0x61]);
    cpu.regs.eax = 0x11111111;
    cpu.regs.ecx = 0x22222222;
    cpu.regs.edx = 0x33333333;
    cpu.regs.ebx = 0x44444444;
    cpu.regs.ebp = 0x66666666;
    cpu.regs.esi = 0x77777777;
    cpu.regs.edi = 0x88888888;
    let saved_esp = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0x11111111);
    assert_eq!(cpu.regs.ecx, 0x22222222);
    assert_eq!(cpu.regs.edx, 0x33333333);
    assert_eq!(cpu.regs.ebx, 0x44444444);
    assert_eq!(cpu.regs.ebp, 0x66666666);
    assert_eq!(cpu.regs.esi, 0x77777777);
    assert_eq!(cpu.regs.edi, 0x88888888);
    assert_eq!(cpu.regs.esp, saved_esp); // ESP restored
}

// ========================================================================
// 23. LEA with various addressing modes in 32-bit mode
// ========================================================================

#[test]
fn test_lea_r32_sib_scaled_index() {
    // LEA EAX, [ECX*4 + 0x100] => 8D 04 8D 00 01 00 00
    // SIB: scale=2(x4), index=ECX(1), base=5(disp32)
    let (mut cpu, mut mem) = setup_pm32(&[0x8D, 0x04, 0x8D, 0x00, 0x01, 0x00, 0x00]);
    cpu.regs.ecx = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x140); // 0x10*4 + 0x100
}

#[test]
fn test_lea_r32_base_index_disp8() {
    // LEA EAX, [ECX + EDX*2 + 0x10] => 8D 44 51 10
    // modrm=0x44 (md=1, rm=4=SIB, reg=EAX)
    // SIB=0x51 (scale=1(x2), index=2(EDX), base=1(ECX))
    let (mut cpu, mut mem) = setup_pm32(&[0x8D, 0x44, 0x51, 0x10]);
    cpu.regs.ecx = 0x100;
    cpu.regs.edx = 0x20;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x150); // 0x100 + 0x20*2 + 0x10
}

#[test]
fn test_lea_sib_no_index_ignores_scale_bits() {
    // SIB A5 encodes scale=4, index=none, base=EBP. Intel defines the
    // scale bits as ignored when no index is present, so this is EBP-0x1E.
    let (mut cpu, mut mem) = setup_pm32(&[0x8D, 0x44, 0xA5, 0xE2]);
    cpu.regs.ebp = 0x1234_5678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x1234_565A);
}

#[test]
fn test_lea_r16_sib_no_index_preserves_upper_word() {
    // Operand-size override selects AX while address-size remains 32-bit.
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0x8D, 0x44, 0xA5, 0xE2]);
    cpu.regs.eax = 0xAAAA_0000;
    cpu.regs.ebp = 0x1234_5678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xAAAA_565A);
}

// ========================================================================
// 24. MOVZX/MOVSX 32-bit operands in PM
// ========================================================================

#[test]
fn test_movzx_r32_rm8_pm() {
    // MOVZX EAX, CL => 0F B6 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB6, 0xC1]);
    cpu.regs.ecx = 0xFFFF_FF80;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x80);
}

#[test]
fn test_movsx_r32_rm8_pm() {
    // MOVSX EAX, CL => 0F BE C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBE, 0xC1]);
    cpu.regs.ecx = 0x0000_0080;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF_FF80);
}

#[test]
fn test_movzx_r32_rm16_pm() {
    // MOVZX EAX, CX => 0F B7 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB7, 0xC1]);
    cpu.regs.ecx = 0xFFFF_8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x8000);
}

#[test]
fn test_movsx_r32_rm16_pm() {
    // MOVSX EAX, CX => 0F BF C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBF, 0xC1]);
    cpu.regs.ecx = 0x0000_8000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF_8000);
}

// ========================================================================
// 25. 32-bit ALU operations in PM (ADC/SBB imm32)
// ========================================================================

#[test]
fn test_adc_eax_imm32() {
    // STC + ADC EAX, 0x10000 => F9 15 00 00 01 00
    let (mut cpu, mut mem) = setup_pm32(&[0xF9, 0x15, 0x00, 0x00, 0x01, 0x00]);
    cpu.regs.eax = 0x20000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0x30001); // 0x20000 + 0x10000 + 1(CF)
}

#[test]
fn test_sbb_eax_imm32() {
    // STC + SBB EAX, 0x10000 => F9 1D 00 00 01 00
    let (mut cpu, mut mem) = setup_pm32(&[0xF9, 0x1D, 0x00, 0x00, 0x01, 0x00]);
    cpu.regs.eax = 0x30000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 0x1FFFF); // 0x30000 - 0x10000 - 1(CF)
}

// ========================================================================
// 26. ALU reg-reg 32-bit ops in PM
// ========================================================================

#[test]
fn test_adc_r32_r32_mem() {
    // STC + ADC [0x200], EAX => F9 11 05 00 02 00 00 (in 32-bit PM)
    // Actually: STC (F9), ADC [disp32], EAX
    // 0x11 => ADC r/m32, r32, modrm=0x05 => rm=5(disp32), reg=EAX
    let (mut cpu, mut mem) = setup_pm32(&[0xF9, 0x11, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 100;
    mem.ram[0x200..0x204].copy_from_slice(&200u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap());
    assert_eq!(result, 301); // 200 + 100 + 1(CF)
}

#[test]
fn test_sbb_r32_r32_mem() {
    // STC + SBB [0x200], EAX => F9 19 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xF9, 0x19, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 100;
    mem.ram[0x200..0x204].copy_from_slice(&500u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    let result = u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap());
    assert_eq!(result, 399); // 500 - 100 - 1(CF)
}

// ========================================================================
// 27. Protected mode RETF same-privilege (already partially covered)
//     Exercise 32-bit RETF path
// ========================================================================

#[test]
fn test_retf_32bit_pm() {
    // RETF = 0xCB in 32-bit PM
    let (mut cpu, mut mem) = setup_pm32(&[0xCB]);
    // Push CS:EIP on stack for return
    let target_eip: u32 = 0x5000;
    let target_cs: u32 = 0x08; // same ring-0 CS
    let esp = cpu.regs.esp;
    // Stack grows down: push CS then EIP
    mem.ram[(esp - 8) as usize..(esp - 4) as usize].copy_from_slice(&target_eip.to_le_bytes());
    mem.ram[(esp - 4) as usize..esp as usize].copy_from_slice(&target_cs.to_le_bytes());
    cpu.regs.esp = esp - 8;
    cpu.regs.set_sp((esp - 8) as u16);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Retf { .. }));
}

// ========================================================================
// 28. XCHG with memory operands (32-bit)
// ========================================================================

#[test]
fn test_xchg_eax_mem32() {
    // XCHG EAX, [0x200] => 87 05 00 02 00 00 (in 32-bit PM)
    let (mut cpu, mut mem) = setup_pm32(&[0x87, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 0xAAAA_BBBB;
    mem.ram[0x200..0x204].copy_from_slice(&0xCCCC_DDDDu32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xCCCC_DDDD);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        0xAAAA_BBBB
    );
}

// ========================================================================
// 29. IMUL two-operand and three-operand 32-bit in PM
// ========================================================================

#[test]
fn test_imul_r32_rm32_pm() {
    // IMUL EAX, ECX => 0F AF C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xAF, 0xC1]);
    cpu.regs.eax = 100;
    cpu.regs.ecx = 200;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20000);
}

#[test]
fn test_imul_r32_rm32_imm8_pm() {
    // IMUL EAX, ECX, 10 => 6B C1 0A
    let (mut cpu, mut mem) = setup_pm32(&[0x6B, 0xC1, 0x0A]);
    cpu.regs.ecx = 50;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 500);
}

#[test]
fn test_imul_r32_rm32_imm32_pm() {
    // IMUL EAX, ECX, 1000 => 69 C1 E8 03 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x69, 0xC1, 0xE8, 0x03, 0x00, 0x00]);
    cpu.regs.ecx = 7;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 7000);
}

// ========================================================================
// 30. CMPXCHG 32-bit (0F B1) in PM (requires 486+)
// ========================================================================

#[test]
fn test_cmpxchg_r32_equal() {
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB1, 0xD9]);
    cpu.set_cpu_type(gp64_cpu::CpuType::I80486);
    cpu.regs.eax = 0x1234;
    cpu.regs.ecx = 0x1234; // matches EAX
    cpu.regs.ebx = 0x5678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0x5678); // replaced with EBX
}

#[test]
fn test_cmpxchg_r32_not_equal() {
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB1, 0xD9]);
    cpu.set_cpu_type(gp64_cpu::CpuType::I80486);
    cpu.regs.eax = 0x1234;
    cpu.regs.ecx = 0x9999; // doesn't match EAX
    cpu.regs.ebx = 0x5678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x9999); // EAX loaded with ECX
    assert_eq!(cpu.regs.ecx, 0x9999); // unchanged
}

// ========================================================================
// 31. BSF/BSR 32-bit in PM
// ========================================================================

#[test]
fn test_bsf_r32_pm() {
    // BSF EAX, ECX => 0F BC C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBC, 0xC1]);
    cpu.regs.ecx = 0x00100000; // bit 20
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_bsr_r32_pm() {
    // BSR EAX, ECX => 0F BD C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBD, 0xC1]);
    cpu.regs.ecx = 0x00100000; // bit 20
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 32. BT/BTS/BTR/BTC with memory operand in 32-bit PM
// ========================================================================

#[test]
fn test_bt_mem32_imm8() {
    // BT [0x200], 7 => 0F BA 25 00 02 00 00 07
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBA, 0x25, 0x00, 0x02, 0x00, 0x00, 0x07]);
    mem.ram[0x200] = 0x80; // bit 7 set
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_bts_mem32_imm8() {
    // BTS [0x200], 3 => 0F BA 2D 00 02 00 00 03
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBA, 0x2D, 0x00, 0x02, 0x00, 0x00, 0x03]);
    mem.ram[0x200] = 0x00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was 0
    assert_eq!(mem.ram[0x200] & 0x08, 0x08); // bit 3 now set
}

// ========================================================================
// 33. SHLD/SHRD 32-bit in PM
// ========================================================================

#[test]
fn test_shld_r32_r32_imm8() {
    // SHLD EAX, ECX, 4 => 0F A4 C8 04
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xA4, 0xC8, 0x04]);
    cpu.regs.eax = 0x12345678;
    cpu.regs.ecx = 0xABCDEF00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // EAX shifted left 4, filled with top 4 bits of ECX
    assert_eq!(cpu.regs.eax, 0x2345678A);
}

#[test]
fn test_shrd_r32_r32_imm8() {
    // SHRD EAX, ECX, 4 => 0F AC C8 04
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xAC, 0xC8, 0x04]);
    cpu.regs.eax = 0x12345678;
    cpu.regs.ecx = 0xABCDEF00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // EAX shifted right 4, filled with bottom 4 bits of ECX
    assert_eq!(cpu.regs.eax, 0x01234567);
}

// ========================================================================
// 34. SETCC 32-bit PM paths
// ========================================================================

#[test]
fn test_sete_rm8() {
    // SETE CL => 0F 94 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x94, 0xC1]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setne_rm8() {
    // SETNE CL => 0F 95 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x95, 0xC1]);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setb_rm8() {
    // SETB CL => 0F 92 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x92, 0xC1]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setge_rm8() {
    // SETGE CL => 0F 9D C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x9D, 0xC1]);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setl_rm8() {
    // SETL CL => 0F 9C C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x9C, 0xC1]);
    cpu.regs.flags.insert(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setle_rm8() {
    // SETLE CL => 0F 9E C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x9E, 0xC1]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setg_rm8() {
    // SETG CL => 0F 9F C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x9F, 0xC1]);
    cpu.regs.flags.remove(Flags::ZERO);
    cpu.regs.flags.remove(Flags::SIGN);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_seta_rm8() {
    // SETA CL => 0F 97 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x97, 0xC1]);
    cpu.regs.flags.remove(Flags::CARRY);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setbe_rm8() {
    // SETBE CL => 0F 96 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x96, 0xC1]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_sets_rm8() {
    // SETS CL => 0F 98 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x98, 0xC1]);
    cpu.regs.flags.insert(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setns_rm8() {
    // SETNS CL => 0F 99 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x99, 0xC1]);
    cpu.regs.flags.remove(Flags::SIGN);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_seto_rm8() {
    // SETO CL => 0F 90 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x90, 0xC1]);
    cpu.regs.flags.insert(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setno_rm8() {
    // SETNO CL => 0F 91 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x91, 0xC1]);
    cpu.regs.flags.remove(Flags::OVERFLOW);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setp_rm8() {
    // SETP CL => 0F 9A C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x9A, 0xC1]);
    cpu.regs.flags.insert(Flags::PARITY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setnp_rm8() {
    // SETNP CL => 0F 9B C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x9B, 0xC1]);
    cpu.regs.flags.remove(Flags::PARITY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

#[test]
fn test_setnb_rm8() {
    // SETNB (SETAE) CL => 0F 93 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x93, 0xC1]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 1);
}

// ========================================================================
// 35. More arithmetic and ALU operations for coverage
// ========================================================================

#[test]
fn test_add_r32_r32_sets_flags() {
    // ADD EAX, ECX => 01 C8 (causes overflow)
    let (mut cpu, mut mem) = setup_pm32(&[0x01, 0xC8]);
    cpu.regs.eax = 0x7FFF_FFFF;
    cpu.regs.ecx = 1;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x8000_0000);
}

#[test]
fn test_sub_r32_r32_zero_result() {
    // SUB EAX, ECX => 29 C8
    let (mut cpu, mut mem) = setup_pm32(&[0x29, 0xC8]);
    cpu.regs.eax = 42;
    cpu.regs.ecx = 42;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0);
}

#[test]
fn test_xor_eax_eax_clears() {
    // XOR EAX, EAX => 31 C0
    let (mut cpu, mut mem) = setup_pm32(&[0x31, 0xC0]);
    cpu.regs.eax = 0xDEADBEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0);
}

#[test]
fn test_add_mem32_reg32_pm() {
    // ADD [0x200], ECX => 01 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x01, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&100u32.to_le_bytes());
    cpu.regs.ecx = 50;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        150
    );
}

#[test]
fn test_sub_mem32_reg32_pm() {
    // SUB [0x200], ECX => 29 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x29, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&100u32.to_le_bytes());
    cpu.regs.ecx = 30;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        70
    );
}

#[test]
fn test_and_mem32_reg32_pm() {
    // AND [0x200], ECX => 21 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x21, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&0xFF00FF00u32.to_le_bytes());
    cpu.regs.ecx = 0x0F0F0F0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        0x0F000F00
    );
}

#[test]
fn test_or_mem32_reg32_pm() {
    // OR [0x200], ECX => 09 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x09, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&0xF0F0F0F0u32.to_le_bytes());
    cpu.regs.ecx = 0x0F0F0F0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        0xFFFFFFFF
    );
}

#[test]
fn test_xor_mem32_reg32_pm() {
    // XOR [0x200], ECX => 31 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x31, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&0xAAAAAAAAu32.to_le_bytes());
    cpu.regs.ecx = 0x55555555;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        0xFFFFFFFF
    );
}

#[test]
fn test_cmp_mem32_reg32_pm() {
    // CMP [0x200], ECX => 39 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x39, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&42u32.to_le_bytes());
    cpu.regs.ecx = 42;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // Memory unchanged after CMP
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        42
    );
}

// ========================================================================
// 36. MOV r32, r32 in PM
// ========================================================================

#[test]
fn test_mov_r32_r32_all_regs() {
    // MOV EAX, ECX => 89 C8
    let (mut cpu, mut mem) = setup_pm32(&[0x89, 0xC8]);
    cpu.regs.ecx = 0xDEAD_BEEF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xDEAD_BEEF);
}

#[test]
fn test_mov_esi_edx() {
    // MOV ESI, EDX => 89 D6
    let (mut cpu, mut mem) = setup_pm32(&[0x89, 0xD6]);
    cpu.regs.edx = 0xCAFE_BABE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esi, 0xCAFE_BABE);
}

#[test]
fn test_mov_edi_ebx() {
    // MOV EDI, EBX => 89 DF
    let (mut cpu, mut mem) = setup_pm32(&[0x89, 0xDF]);
    cpu.regs.ebx = 0x1234_5678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edi, 0x1234_5678);
}

// ========================================================================
// 37. DIV/IDIV/MUL/IMUL single-operand 32-bit in PM
// ========================================================================

#[test]
fn test_div_r32_pm() {
    // DIV ECX => F7 F1 (32-bit in PM)
    let (mut cpu, mut mem) = setup_pm32(&[0xF7, 0xF1]);
    cpu.regs.edx = 0x0000_0000;
    cpu.regs.eax = 100;
    cpu.regs.ecx = 7;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 14); // 100/7 = 14
    assert_eq!(cpu.regs.edx, 2); // 100%7 = 2
}

#[test]
fn test_idiv_r32_pm() {
    // IDIV ECX => F7 F9
    let (mut cpu, mut mem) = setup_pm32(&[0xF7, 0xF9]);
    cpu.regs.edx = 0xFFFF_FFFF; // -1 in high part
    cpu.regs.eax = 0xFFFF_FF9C; // EDX:EAX = -100
    cpu.regs.ecx = 7;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax as i32, -14); // -100/7 = -14
    assert_eq!(cpu.regs.edx as i32, -2); // -100%7 = -2
}

#[test]
fn test_mul_r32_pm() {
    // MUL ECX => F7 E1 (32-bit)
    let (mut cpu, mut mem) = setup_pm32(&[0xF7, 0xE1]);
    cpu.regs.eax = 0x10000;
    cpu.regs.ecx = 0x20000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // 0x10000 * 0x20000 = 0x200000000
    assert_eq!(cpu.regs.eax, 0x0000_0000);
    assert_eq!(cpu.regs.edx, 0x0000_0002);
}

// ========================================================================
// 38. NEG/NOT 32-bit in PM
// ========================================================================

#[test]
fn test_neg_r32_pm() {
    // NEG EAX => F7 D8
    let (mut cpu, mut mem) = setup_pm32(&[0xF7, 0xD8]);
    cpu.regs.eax = 42;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax as i32, -42);
}

#[test]
fn test_not_r32_pm() {
    // NOT EAX => F7 D0
    let (mut cpu, mut mem) = setup_pm32(&[0xF7, 0xD0]);
    cpu.regs.eax = 0x0000_00FF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF_FF00);
}

// ========================================================================
// 39. ROL/ROR/RCL/RCR 8-bit (lines 2920+ shift variants)
// ========================================================================

#[test]
fn test_rol_r8_by_1() {
    // ROL AL, 1 => D0 C0
    let (mut cpu, mut mem) = setup(&[0xD0, 0xC0]);
    cpu.regs.set_al(0x80);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x01);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_ror_r8_by_1() {
    // ROR AL, 1 => D0 C8
    let (mut cpu, mut mem) = setup(&[0xD0, 0xC8]);
    cpu.regs.set_al(0x01);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x80);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_rcl_r8_by_1() {
    // RCL AL, 1 => D0 D0
    let (mut cpu, mut mem) = setup(&[0xD0, 0xD0]);
    cpu.regs.set_al(0x80);
    cpu.regs.flags.insert(Flags::CARRY); // CF=1
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x01); // 0x80 << 1 | CF=1 => 0x01
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // old MSB was 1
}

#[test]
fn test_rcr_r8_by_1() {
    // RCR AL, 1 => D0 D8
    let (mut cpu, mut mem) = setup(&[0xD0, 0xD8]);
    cpu.regs.set_al(0x01);
    cpu.regs.flags.insert(Flags::CARRY); // CF=1
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x80); // CF=1 into MSB, bit 0 to CF
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // old LSB was 1
}

#[test]
fn test_sar_r8_by_cl() {
    // SAR AL, CL => D2 F8
    let (mut cpu, mut mem) = setup(&[0xD2, 0xF8]);
    cpu.regs.set_al(0xF0); // -16 signed
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 2;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xFC); // -16 >> 2 = -4
}

// ========================================================================
// 40. Shift 16-bit operations
// ========================================================================

#[test]
fn test_shl_r16_by_1() {
    // SHL AX, 1 => D1 E0
    let (mut cpu, mut mem) = setup(&[0xD1, 0xE0]);
    cpu.regs.set_ax(0x8001);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0002);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shr_r16_by_1() {
    // SHR AX, 1 => D1 E8
    let (mut cpu, mut mem) = setup(&[0xD1, 0xE8]);
    cpu.regs.set_ax(0x8002);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x4001);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_sar_r16_by_1() {
    // SAR AX, 1 => D1 F8
    let (mut cpu, mut mem) = setup(&[0xD1, 0xF8]);
    cpu.regs.set_ax(0x8002);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xC001);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_rol_r16_by_1() {
    // ROL AX, 1 => D1 C0
    let (mut cpu, mut mem) = setup(&[0xD1, 0xC0]);
    cpu.regs.set_ax(0x8001);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0003);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_ror_r16_by_1() {
    // ROR AX, 1 => D1 C8
    let (mut cpu, mut mem) = setup(&[0xD1, 0xC8]);
    cpu.regs.set_ax(0x0001);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x8000);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

// ========================================================================
// 41. Shift 32-bit operations in PM
// ========================================================================

#[test]
fn test_shl_r32_by_1() {
    // SHL EAX, 1 => D1 E0 (in 32-bit PM)
    let (mut cpu, mut mem) = setup_pm32(&[0xD1, 0xE0]);
    cpu.regs.eax = 0x8000_0001;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0000_0002);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_shr_r32_by_cl() {
    // SHR EAX, CL => D3 E8 (in 32-bit PM)
    let (mut cpu, mut mem) = setup_pm32(&[0xD3, 0xE8]);
    cpu.regs.eax = 0xFF00_0000;
    cpu.regs.ecx = 8;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00FF_0000);
}

#[test]
fn test_sar_r32_by_cl() {
    // SAR EAX, CL => D3 F8
    let (mut cpu, mut mem) = setup_pm32(&[0xD3, 0xF8]);
    cpu.regs.eax = 0x8000_0000;
    cpu.regs.ecx = 4;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xF800_0000);
}

// ========================================================================
// 42. TEST instruction variants in PM
// ========================================================================

#[test]
fn test_test_r32_r32() {
    // TEST EAX, ECX => 85 C8
    let (mut cpu, mut mem) = setup_pm32(&[0x85, 0xC8]);
    cpu.regs.eax = 0x0000_00FF;
    cpu.regs.ecx = 0xFF00_0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO)); // no overlap
}

#[test]
fn test_test_eax_imm32() {
    // TEST EAX, 0x80000000 => A9 00 00 00 80
    let (mut cpu, mut mem) = setup_pm32(&[0xA9, 0x00, 0x00, 0x00, 0x80]);
    cpu.regs.eax = 0x8000_0000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// 43. LAHF / SAHF
// ========================================================================

#[test]
fn test_lahf() {
    // LAHF => 0x9F
    let (mut cpu, mut mem) = setup(&[0x9F]);
    cpu.regs.flags = Flags::CARRY | Flags::ZERO | Flags::SIGN | Flags::PARITY | Flags::RESERVED1;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let ah = (cpu.regs.eax >> 8) & 0xFF;
    assert_ne!(ah, 0); // AH should have flags
    assert_eq!(ah & 1, 1); // CF
    assert_eq!((ah >> 6) & 1, 1); // ZF
}

#[test]
fn test_sahf() {
    // SAHF => 0x9E
    let (mut cpu, mut mem) = setup(&[0x9E]);
    cpu.regs.eax = (cpu.regs.eax & 0xFFFF_00FF) | (0xD7 << 8); // AH = 0xD7
                                                               // 0xD7 = 1101_0111 = SF ZF AF PF CF
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::PARITY));
    assert!(cpu.regs.flags.contains(Flags::AUX_CARRY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// 44. LEAVE (0xC9) in various modes
// ========================================================================

#[test]
fn test_leave_16bit() {
    // LEAVE => 0xC9
    let (mut cpu, mut mem) = setup(&[0xC9]);
    cpu.regs.set_sp(0x0F00);
    cpu.regs.esp = 0x0F00;
    cpu.regs.set_bp(0x1000);
    cpu.regs.ebp = 0x1000;
    // Stack at 0x1000 should have the old BP value
    mem.ram[0x1000] = 0x00;
    mem.ram[0x1001] = 0x20; // old BP = 0x2000
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.sp(), 0x1002);
    assert_eq!(cpu.regs.bp(), 0x2000);
}

#[test]
fn test_leave_32bit_pm() {
    // LEAVE => 0xC9 in 32-bit PM
    let (mut cpu, mut mem) = setup_pm32(&[0xC9]);
    cpu.regs.esp = 0x5000;
    cpu.regs.ebp = 0x6000;
    mem.ram[0x6000..0x6004].copy_from_slice(&0x7000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esp, 0x6004);
    assert_eq!(cpu.regs.ebp, 0x7000);
}

// ========================================================================
// 45. XLATB (0xD7)
// ========================================================================

#[test]
fn test_xlatb() {
    // XLATB => 0xD7: AL = DS:[BX + AL]
    let (mut cpu, mut mem) = setup(&[0xD7]);
    cpu.regs.ebx = 0x300;
    cpu.regs.set_al(0x10);
    mem.ram[0x310] = 0x42;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x42);
}

// ========================================================================
// 46. CLC/STC/CLI/STI/CLD/STD in 32-bit PM
// ========================================================================

#[test]
fn test_clc_stc_pm() {
    // CLC => F8, STC => F9
    let (mut cpu, mut mem) = setup_pm32(&[0xF9, 0xF8]); // STC then CLC
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_cld_std_pm() {
    // STD => FD, CLD => FC
    let (mut cpu, mut mem) = setup_pm32(&[0xFD, 0xFC]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::DIRECTION));
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::DIRECTION));
}

// ========================================================================
// 47. MOV r/m32, imm32 in PM
// ========================================================================

#[test]
fn test_mov_rm32_imm32_reg() {
    // MOV EAX, 0x12345678 => C7 C0 78 56 34 12 (modrm=0xC0)
    let (mut cpu, mut mem) = setup_pm32(&[0xC7, 0xC0, 0x78, 0x56, 0x34, 0x12]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12345678);
}

#[test]
fn test_mov_rm32_imm32_mem() {
    // MOV [0x200], 0xAABBCCDD => C7 05 00 02 00 00 DD CC BB AA
    let (mut cpu, mut mem) =
        setup_pm32(&[0xC7, 0x05, 0x00, 0x02, 0x00, 0x00, 0xDD, 0xCC, 0xBB, 0xAA]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        0xAABBCCDD
    );
}

// ========================================================================
// 48. PUSH/POP segment registers in PM
// ========================================================================

#[test]
fn test_push_pop_es_pm() {
    // PUSH ES => 0x06, POP ES => 0x07
    let (mut cpu, mut mem) = setup_pm16(&[0x06, 0x07]);
    cpu.regs.es = 0x10;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.es, 0x10);
}

#[test]
fn test_push_pop_ds_pm() {
    // PUSH DS => 0x1E, POP DS => 0x1F
    let (mut cpu, mut mem) = setup_pm16(&[0x1E, 0x1F]);
    cpu.regs.ds = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.ds, 0x10);
}

// ========================================================================
// 49. LOOP/LOOPE/LOOPNE variants
// ========================================================================

#[test]
fn test_loop_ecx_addr32() {
    // In 32-bit PM: LOOP rel8 uses ECX
    // LOOP -2 => E2 FE (loop back to self)
    // With ECX=3, should execute 3 times then fall through
    let (mut cpu, mut mem) = setup_pm32(&[0xE2, 0xFE, 0x90]); // LOOP $, NOP
    cpu.regs.ecx = 3;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    assert_eq!(cpu.regs.ecx, 0);
}

#[test]
fn test_loope_ecx_stops_on_nz() {
    // LOOPE: loop while ECX != 0 AND ZF=1
    // 0xE1 FE => LOOPE $
    let (mut cpu, mut mem) = setup_pm32(&[0xE1, 0xFE, 0x90]);
    cpu.regs.ecx = 10;
    cpu.regs.flags.remove(Flags::ZERO); // ZF=0, should not loop
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 9); // decremented but didn't loop
}

#[test]
fn test_loopne_ecx_stops_on_z() {
    // LOOPNE: loop while ECX != 0 AND ZF=0
    // 0xE0 FE => LOOPNE $
    let (mut cpu, mut mem) = setup_pm32(&[0xE0, 0xFE, 0x90]);
    cpu.regs.ecx = 10;
    cpu.regs.flags.insert(Flags::ZERO); // ZF=1, should not loop
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 9);
}

// ========================================================================
// 50. String operations in 32-bit PM
// ========================================================================

#[test]
fn test_movsb_32bit() {
    // MOVSB in 32-bit mode => 0xA4
    let (mut cpu, mut mem) = setup_pm32(&[0xA4]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    mem.ram[0x2000] = 0x42;
    cpu.regs.flags.remove(Flags::DIRECTION); // forward
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x3000], 0x42);
    assert_eq!(cpu.regs.esi, 0x2001);
    assert_eq!(cpu.regs.edi, 0x3001);
}

#[test]
fn test_movsd_32bit() {
    // MOVSD in 32-bit mode => 0xA5 (32-bit operand by default)
    let (mut cpu, mut mem) = setup_pm32(&[0xA5]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    mem.ram[0x2000..0x2004].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x3000..0x3004].try_into().unwrap()),
        0xDEADBEEF
    );
    assert_eq!(cpu.regs.esi, 0x2004);
    assert_eq!(cpu.regs.edi, 0x3004);
}

#[test]
fn test_stosb_32bit() {
    // STOSB in 32-bit mode => 0xAA
    let (mut cpu, mut mem) = setup_pm32(&[0xAA]);
    cpu.regs.edi = 0x3000;
    cpu.regs.set_al(0x55);
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x3000], 0x55);
    assert_eq!(cpu.regs.edi, 0x3001);
}

#[test]
fn test_stosd_32bit() {
    // STOSD in 32-bit mode => 0xAB
    let (mut cpu, mut mem) = setup_pm32(&[0xAB]);
    cpu.regs.edi = 0x3000;
    cpu.regs.eax = 0xCAFEBABE;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x3000..0x3004].try_into().unwrap()),
        0xCAFEBABE
    );
}

#[test]
fn test_lodsb_32bit() {
    // LODSB in 32-bit mode => 0xAC
    let (mut cpu, mut mem) = setup_pm32(&[0xAC]);
    cpu.regs.esi = 0x2000;
    mem.ram[0x2000] = 0x77;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x77);
    assert_eq!(cpu.regs.esi, 0x2001);
}

#[test]
fn test_lodsd_32bit() {
    // LODSD in 32-bit mode => 0xAD
    let (mut cpu, mut mem) = setup_pm32(&[0xAD]);
    cpu.regs.esi = 0x2000;
    mem.ram[0x2000..0x2004].copy_from_slice(&0x11223344u32.to_le_bytes());
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x11223344);
    assert_eq!(cpu.regs.esi, 0x2004);
}

#[test]
fn test_cmpsb_32bit() {
    // CMPSB in 32-bit mode => 0xA6
    let (mut cpu, mut mem) = setup_pm32(&[0xA6]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    mem.ram[0x2000] = 0x42;
    mem.ram[0x3000] = 0x42;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO)); // equal
}

#[test]
fn test_cmpsd_32bit() {
    // CMPSD in 32-bit mode => 0xA7
    let (mut cpu, mut mem) = setup_pm32(&[0xA7]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    mem.ram[0x2000..0x2004].copy_from_slice(&100u32.to_le_bytes());
    mem.ram[0x3000..0x3004].copy_from_slice(&200u32.to_le_bytes());
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO)); // not equal
}

#[test]
fn test_scasb_32bit() {
    // SCASB in 32-bit mode => 0xAE
    let (mut cpu, mut mem) = setup_pm32(&[0xAE]);
    cpu.regs.edi = 0x3000;
    cpu.regs.set_al(0x42);
    mem.ram[0x3000] = 0x42;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO)); // equal
}

#[test]
fn test_scasd_32bit() {
    // SCASD in 32-bit mode => 0xAF
    let (mut cpu, mut mem) = setup_pm32(&[0xAF]);
    cpu.regs.edi = 0x3000;
    cpu.regs.eax = 0x12345678;
    mem.ram[0x3000..0x3004].copy_from_slice(&0x12345678u32.to_le_bytes());
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 51. REP string ops in 32-bit mode
// ========================================================================

#[test]
fn test_rep_movsb_32bit() {
    // REP MOVSB => F3 A4
    let (mut cpu, mut mem) = setup_pm32(&[0xF3, 0xA4]);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    cpu.regs.ecx = 4;
    mem.ram[0x2000] = 1;
    mem.ram[0x2001] = 2;
    mem.ram[0x2002] = 3;
    mem.ram[0x2003] = 4;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 4);
    assert_eq!(mem.ram[0x3000], 1);
    assert_eq!(mem.ram[0x3001], 2);
    assert_eq!(mem.ram[0x3002], 3);
    assert_eq!(mem.ram[0x3003], 4);
}

#[test]
fn test_rep_stosd_32bit() {
    // REP STOSD => F3 AB
    let (mut cpu, mut mem) = setup_pm32(&[0xF3, 0xAB]);
    cpu.regs.edi = 0x3000;
    cpu.regs.eax = 0xAAAAAAAA;
    cpu.regs.ecx = 2;
    cpu.regs.flags.remove(Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x3000..0x3004].try_into().unwrap()),
        0xAAAAAAAA
    );
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x3004..0x3008].try_into().unwrap()),
        0xAAAAAAAA
    );
}

// ========================================================================
// 52. 32-bit conditional jumps (Jcc near) in PM
// ========================================================================

#[test]
fn test_jz_near_32bit_taken() {
    // JZ near (0F 84 + rel32) in 32-bit PM
    let (mut cpu, mut mem) = setup_pm32(&[
        0x0F, 0x84, 0x04, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4,
    ]);
    cpu.regs.flags.insert(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x010A); // 0x0100 + 6 (instr) + 4 (rel32)
}

#[test]
fn test_jnz_near_32bit_taken() {
    let (mut cpu, mut mem) = setup_pm32(&[
        0x0F, 0x85, 0x04, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4,
    ]);
    cpu.regs.flags.remove(Flags::ZERO);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x010A);
}

#[test]
fn test_jb_near_32bit_taken() {
    let (mut cpu, mut mem) = setup_pm32(&[
        0x0F, 0x82, 0x04, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4,
    ]);
    cpu.regs.flags.insert(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x010A);
}

#[test]
fn test_jnb_near_32bit_taken() {
    let (mut cpu, mut mem) = setup_pm32(&[
        0x0F, 0x83, 0x04, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90, 0x90, 0xF4,
    ]);
    cpu.regs.flags.remove(Flags::CARRY);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x010A);
}

// ========================================================================
// 53. CALL/RET in 32-bit PM
// ========================================================================

#[test]
fn test_call_near_32bit() {
    // CALL near rel32 => E8 04 00 00 00
    let (mut cpu, mut mem) =
        setup_pm32(&[0xE8, 0x04, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90, 0x90, 0xC3]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // Target = 0x0100 + 5 + 4 = 0x0109
    assert_eq!(cpu.regs.eip, 0x0109);
    // Return address on stack = 0x0105
    let ret_addr = u32::from_le_bytes(
        mem.ram[cpu.regs.esp as usize..(cpu.regs.esp + 4) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(ret_addr, 0x0105);
}

#[test]
fn test_ret_near_32bit() {
    // RET => C3 in 32-bit PM
    let (mut cpu, mut mem) = setup_pm32(&[0xC3]);
    // Push return address on stack
    let ret_addr: u32 = 0x5000;
    let esp = cpu.regs.esp;
    mem.ram[(esp - 4) as usize..esp as usize].copy_from_slice(&ret_addr.to_le_bytes());
    cpu.regs.esp = esp - 4;
    cpu.regs.set_sp((esp - 4) as u16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x5000);
}

// ========================================================================
// 54. PUSHFD/POPFD in 32-bit PM
// ========================================================================

#[test]
fn test_pushfd_popfd() {
    // PUSHFD => 0x9C, POPFD => 0x9D (in 32-bit PM)
    let (mut cpu, mut mem) = setup_pm32(&[0x9C, 0x9D]);
    cpu.regs
        .flags
        .insert(Flags::CARRY | Flags::ZERO | Flags::SIGN | Flags::DIRECTION);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert!(cpu.regs.flags.contains(Flags::SIGN));
}

// ========================================================================
// 55. XADD instruction (0F C0/C1)
// ========================================================================

#[test]
fn test_xadd_r8_r8() {
    // XADD AL, CL => 0F C0 C8 (486+)
    let (mut cpu, mut mem) = setup(&[0x0F, 0xC0, 0xC8]);
    cpu.set_cpu_type(gp64_cpu::CpuType::I80486);
    cpu.regs.set_al(10);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 0x14;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 30); // AL = AL + CL
    assert_eq!(cpu.regs.ecx & 0xFF, 10); // CL = old AL
}

#[test]
fn test_xadd_r32_r32() {
    // XADD EAX, ECX => 0F C1 C8 (486+)
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xC1, 0xC8]);
    cpu.set_cpu_type(gp64_cpu::CpuType::I80486);
    cpu.regs.eax = 100;
    cpu.regs.ecx = 200;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 300);
    assert_eq!(cpu.regs.ecx, 100);
}

// ========================================================================
// 56. INC/DEC r/m32 with memory in PM
// ========================================================================

#[test]
fn test_inc_mem32() {
    // INC [0x200] => FF 05 00 02 00 00 (modrm=0x05: sub=0, rm=5=disp32)
    let (mut cpu, mut mem) = setup_pm32(&[0xFF, 0x05, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&42u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        43
    );
}

#[test]
fn test_dec_mem32() {
    // DEC [0x200] => FF 0D 00 02 00 00 (modrm=0x0D: sub=1, rm=5=disp32)
    let (mut cpu, mut mem) = setup_pm32(&[0xFF, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&42u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        41
    );
}

// ========================================================================
// 57. PUSH/POP r32 in PM
// ========================================================================

#[test]
fn test_push_pop_all_r32_pm() {
    // PUSH EAX..EDI then POP EDI..EAX
    // 50 51 52 53 55 56 57 5F 5E 5D 5B 5A 59 58
    let (mut cpu, mut mem) = setup_pm32(&[
        0x50, 0x51, 0x52, 0x53, 0x55, 0x56, 0x57, // push eax..edi
        0x5F, 0x5E, 0x5D, 0x5B, 0x5A, 0x59, 0x58, // pop edi..eax
    ]);
    cpu.regs.eax = 0x11;
    cpu.regs.ecx = 0x22;
    cpu.regs.edx = 0x33;
    cpu.regs.ebx = 0x44;
    cpu.regs.ebp = 0x66;
    cpu.regs.esi = 0x77;
    cpu.regs.edi = 0x88;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 14);
    assert_eq!(cpu.regs.eax, 0x11);
    assert_eq!(cpu.regs.ecx, 0x22);
    assert_eq!(cpu.regs.edx, 0x33);
    assert_eq!(cpu.regs.ebx, 0x44);
    assert_eq!(cpu.regs.ebp, 0x66);
    assert_eq!(cpu.regs.esi, 0x77);
    assert_eq!(cpu.regs.edi, 0x88);
}

// ========================================================================
// 58. PUSH imm8/imm32 in PM
// ========================================================================

#[test]
fn test_push_imm8_pm() {
    // PUSH 0x42 => 6A 42 (sign-extended to 32)
    let (mut cpu, mut mem) = setup_pm32(&[0x6A, 0x42]);
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esp, old_esp - 4);
    let val = u32::from_le_bytes(
        mem.ram[cpu.regs.esp as usize..(cpu.regs.esp + 4) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(val, 0x42);
}

#[test]
fn test_push_imm8_negative_pm() {
    // PUSH 0xFF => 6A FF (sign-extended to 0xFFFFFFFF)
    let (mut cpu, mut mem) = setup_pm32(&[0x6A, 0xFF]);
    let _old_esp = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    let val = u32::from_le_bytes(
        mem.ram[cpu.regs.esp as usize..(cpu.regs.esp + 4) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(val, 0xFFFFFFFF);
}

#[test]
fn test_push_imm32_pm() {
    // PUSH 0x12345678 => 68 78 56 34 12
    let (mut cpu, mut mem) = setup_pm32(&[0x68, 0x78, 0x56, 0x34, 0x12]);
    let old_esp = cpu.regs.esp;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esp, old_esp - 4);
    let val = u32::from_le_bytes(
        mem.ram[cpu.regs.esp as usize..(cpu.regs.esp + 4) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(val, 0x12345678);
}

// ========================================================================
// 59. Arithmetic with r/m32, imm8 (sign-extended) in PM
// ========================================================================

#[test]
fn test_add_rm32_imm8_pm() {
    // ADD EAX, 0x10 => 83 C0 10 (modrm=0xC0: sub=0, rm=EAX)
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xC0, 0x10]);
    cpu.regs.eax = 0x100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x110);
}

#[test]
fn test_sub_rm32_imm8_pm() {
    // SUB EAX, 0x10 => 83 E8 10
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xE8, 0x10]);
    cpu.regs.eax = 0x100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xF0);
}

#[test]
fn test_and_rm32_imm8_pm() {
    // AND EAX, 0x0F => 83 E0 0F
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xE0, 0x0F]);
    cpu.regs.eax = 0xFF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0F);
}

#[test]
fn test_or_rm32_imm8_pm() {
    // OR EAX, 0x80 => 83 C8 80 (sign-extends to 0xFFFFFF80)
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xC8, 0x80]);
    cpu.regs.eax = 0x100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFFFF80 | 0x100);
}

#[test]
fn test_xor_rm32_imm8_pm() {
    // XOR EAX, 0xFF => 83 F0 FF (sign-extends to 0xFFFFFFFF)
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xF0, 0xFF]);
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x12345678 ^ 0xFFFFFFFF);
}

#[test]
fn test_cmp_rm32_imm8_pm() {
    // CMP EAX, 0x10 => 83 F8 10
    let (mut cpu, mut mem) = setup_pm32(&[0x83, 0xF8, 0x10]);
    cpu.regs.eax = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 60. Far JMP in real mode via 0xFF /5 (JMP m16:16)
// ========================================================================

#[test]
fn test_jmp_far_indirect_16bit() {
    // JMP FAR [0x200] => FF 2E 00 02 (modrm=0x2E: sub=5, rm=6=disp16)
    let (mut cpu, mut mem) = setup(&[0xFF, 0x2E, 0x00, 0x02]);
    // At 0x200: offset=0x0300, segment=0x0000
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x03;
    mem.ram[0x202] = 0x00;
    mem.ram[0x203] = 0x00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0300);
    assert_eq!(cpu.regs.cs, 0x0000);
}

// ========================================================================
// 61. JECXZ in 32-bit mode
// ========================================================================

#[test]
fn test_jecxz_taken() {
    // addr32 prefix + JECXZ rel8: 67 E3 02 90 90 F4
    // In 16-bit mode with addr32 prefix, uses ECX
    let (mut cpu, mut mem) = setup(&[0x67, 0xE3, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.ecx = 0; // ECX=0 => jump taken
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0105); // 0x0100 + 3 + 2
}

#[test]
fn test_jecxz_not_taken() {
    let (mut cpu, mut mem) = setup(&[0x67, 0xE3, 0x02, 0x90, 0x90, 0xF4]);
    cpu.regs.ecx = 1; // ECX != 0 => not taken
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0103); // falls through
}

// ========================================================================
// 62. More 32-bit ALU: OR/XOR/AND/CMP r32, r/m32
// ========================================================================

#[test]
fn test_or_r32_rm32_pm() {
    // OR EAX, ECX => 09 C8 (in 32-bit PM)
    let (mut cpu, mut mem) = setup_pm32(&[0x09, 0xC8]);
    cpu.regs.eax = 0x00FF0000;
    cpu.regs.ecx = 0x000000FF;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00FF00FF);
}

#[test]
fn test_xor_r32_rm32_pm() {
    // XOR EAX, ECX => 31 C8
    let (mut cpu, mut mem) = setup_pm32(&[0x31, 0xC8]);
    cpu.regs.eax = 0xFFFF0000;
    cpu.regs.ecx = 0xFF00FF00;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x00FFFF00);
}

#[test]
fn test_and_r32_rm32_pm() {
    // AND EAX, ECX => 21 C8
    let (mut cpu, mut mem) = setup_pm32(&[0x21, 0xC8]);
    cpu.regs.eax = 0xFF00FF00;
    cpu.regs.ecx = 0x0F0F0F0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0F000F00);
}

#[test]
fn test_cmp_r32_rm32_pm() {
    // CMP EAX, ECX => 39 C8 (actually CMP r/m32, r32)
    let (mut cpu, mut mem) = setup_pm32(&[0x39, 0xC8]);
    cpu.regs.eax = 100;
    cpu.regs.ecx = 100;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn test_add_r32_rm32_pm() {
    // ADD EAX, ECX => 01 C8
    let (mut cpu, mut mem) = setup_pm32(&[0x01, 0xC8]);
    cpu.regs.eax = 0x10000;
    cpu.regs.ecx = 0x20000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x30000);
}

#[test]
fn test_sub_r32_rm32_pm() {
    // SUB EAX, ECX => 29 C8
    let (mut cpu, mut mem) = setup_pm32(&[0x29, 0xC8]);
    cpu.regs.eax = 0x30000;
    cpu.regs.ecx = 0x10000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x20000);
}

// ========================================================================
// 63. CS override prefix in PM (touches line 892)
// ========================================================================

#[test]
fn test_cs_override_read_pm() {
    // CS: MOV EAX, [0x200] => 2E 8B 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x2E, 0x8B, 0x05, 0x00, 0x02, 0x00, 0x00]);
    // CS base is 0 in our setup, so reads from 0x200
    mem.ram[0x200..0x204].copy_from_slice(&0x99887766u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x99887766);
}

// ========================================================================
// 64. CALL near indirect (FF /2) in PM
// ========================================================================

#[test]
fn test_call_near_indirect_r32() {
    // CALL ECX => FF D1 (modrm=0xD1: sub=2, rm=ECX)
    let (mut cpu, mut mem) = setup_pm32(&[0xFF, 0xD1]);
    cpu.regs.ecx = 0x5000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x5000);
    // Return address on stack
    let ret_addr = u32::from_le_bytes(
        mem.ram[cpu.regs.esp as usize..(cpu.regs.esp + 4) as usize]
            .try_into()
            .unwrap(),
    );
    assert_eq!(ret_addr, 0x0102); // 0x0100 + 2 bytes
}

// ========================================================================
// 65. JMP near indirect (FF /4) in PM
// ========================================================================

#[test]
fn test_jmp_near_indirect_r32() {
    // JMP ECX => FF E1 (modrm=0xE1: sub=4, rm=ECX)
    let (mut cpu, mut mem) = setup_pm32(&[0xFF, 0xE1]);
    cpu.regs.ecx = 0x5000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x5000);
}

// ========================================================================
// 66. MOV EAX, [moffs32] in PM (32-bit offset)
// ========================================================================

#[test]
fn test_mov_eax_moffs32_pm() {
    // MOV EAX, [0x3000] => A1 00 30 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xA1, 0x00, 0x30, 0x00, 0x00]);
    mem.ram[0x3000..0x3004].copy_from_slice(&0xFEEDFACEu32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFEEDFACE);
}

#[test]
fn test_mov_moffs32_eax_pm() {
    // MOV [0x3000], EAX => A3 00 30 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xA3, 0x00, 0x30, 0x00, 0x00]);
    cpu.regs.eax = 0xBAADF00D;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x3000..0x3004].try_into().unwrap()),
        0xBAADF00D
    );
}

// ========================================================================
// 67. SUB/ADD/XOR AL/AX, imm
// ========================================================================

#[test]
fn test_sub_al_imm8() {
    // SUB AL, 0x10 => 2C 10
    let (mut cpu, mut mem) = setup(&[0x2C, 0x10]);
    cpu.regs.set_al(0x20);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0x10);
}

#[test]
fn test_xor_al_imm8() {
    // XOR AL, 0xFF => 34 FF
    let (mut cpu, mut mem) = setup(&[0x34, 0xFF]);
    cpu.regs.set_al(0x55);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xAA);
}

#[test]
fn test_or_al_imm8() {
    // OR AL, 0x0F => 0C 0F
    let (mut cpu, mut mem) = setup(&[0x0C, 0x0F]);
    cpu.regs.set_al(0xF0);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xFF);
}

// ========================================================================
// 68. SUB/ADD/OR/XOR/AND/CMP AX, imm16
// ========================================================================

#[test]
fn test_sub_ax_imm16() {
    // SUB AX, 0x0100 => 2D 00 01
    let (mut cpu, mut mem) = setup(&[0x2D, 0x00, 0x01]);
    cpu.regs.set_ax(0x0200);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x0100);
}

#[test]
fn test_xor_ax_imm16() {
    // XOR AX, 0xFFFF => 35 FF FF
    let (mut cpu, mut mem) = setup(&[0x35, 0xFF, 0xFF]);
    cpu.regs.set_ax(0x5555);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xAAAA);
}

#[test]
fn test_or_ax_imm16() {
    // OR AX, 0x00FF => 0D FF 00
    let (mut cpu, mut mem) = setup(&[0x0D, 0xFF, 0x00]);
    cpu.regs.set_ax(0xFF00);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0xFFFF);
}

// ========================================================================
// 69. ADD/SUB/OR/XOR/AND/CMP EAX, imm32 in PM
// ========================================================================

#[test]
fn test_sub_eax_imm32_pm() {
    // SUB EAX, 0x10000 => 2D 00 00 01 00
    let (mut cpu, mut mem) = setup_pm32(&[0x2D, 0x00, 0x00, 0x01, 0x00]);
    cpu.regs.eax = 0x20000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x10000);
}

#[test]
fn test_xor_eax_imm32_pm() {
    // XOR EAX, 0xFFFFFFFF => 35 FF FF FF FF
    let (mut cpu, mut mem) = setup_pm32(&[0x35, 0xFF, 0xFF, 0xFF, 0xFF]);
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xEDCBA987);
}

#[test]
fn test_or_eax_imm32_pm() {
    // OR EAX, 0x000000FF => 0D FF 00 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0D, 0xFF, 0x00, 0x00, 0x00]);
    cpu.regs.eax = 0xFF000000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFF0000FF);
}

#[test]
fn test_and_eax_imm32_pm() {
    // AND EAX, 0x0000FFFF => 25 FF FF 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x25, 0xFF, 0xFF, 0x00, 0x00]);
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x5678);
}

// ========================================================================
// 70. Far CALL (0x9A) in 32-bit mode (op_size_32 path)
// ========================================================================

#[test]
fn test_far_call_32bit_real_mode() {
    // In real mode with o32: 66 9A offset32 seg16
    // CALL FAR 0x0000:0x00000500 => 66 9A 00 05 00 00 00 00
    let (mut cpu, mut mem) = setup(&[0x66, 0x9A, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00]);
    cpu.regs.set_sp(0x2000);
    cpu.regs.esp = 0x2000;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0500);
}

#[test]
fn test_far_call_16bit_386_real_mode_stack_boundary_fault() {
    // CALL FAR 0000:0500h needs a 4-byte return frame. SP=0003 crosses
    // the default 64K SS limit on 286+ real mode.
    let (mut cpu, mut mem) = setup(&[0x9A, 0x00, 0x05, 0x00, 0x00]);
    cpu.cpu_type = gp64_cpu::CpuType::I80386;
    cpu.regs.set_sp(0x0003);
    cpu.regs.esp = 0x0003;
    cpu.regs.ss = 0x0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 12, .. }),
        "386 real-mode far CALL crossing SS limit should #SS, got {:?}",
        result
    );
    assert_eq!(cpu.regs.sp(), 0x0003);
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn test_iret_16bit_386_real_mode_stack_boundary_fault_is_ss() {
    let (mut cpu, mut mem) = setup(&[0xCF]);
    cpu.cpu_type = gp64_cpu::CpuType::I80386;
    cpu.regs.set_sp(0xFFFF);
    cpu.regs.esp = 0xFFFF;
    cpu.regs.ss = 0x0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 12, .. }),
        "386 real-mode IRET stack-boundary pop should #SS, got {:?}",
        result
    );
    assert_eq!(cpu.regs.sp(), 0xFFFF);
    assert_eq!(cpu.regs.eip, 0x0100);
}

#[test]
fn test_iretd_386_real_mode_stack_boundary_fault_is_ss() {
    let (mut cpu, mut mem) = setup(&[0x66, 0xCF]);
    cpu.cpu_type = gp64_cpu::CpuType::I80386;
    cpu.regs.set_sp(0xFFFE);
    cpu.regs.esp = 0xFFFE;
    cpu.regs.ss = 0x0000;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(
        matches!(result, RunResult::Exception { vector: 12, .. }),
        "386 real-mode IRETD stack-boundary pop should #SS, got {:?}",
        result
    );
    assert_eq!(cpu.regs.sp(), 0xFFFE);
    assert_eq!(cpu.regs.eip, 0x0100);
}

// ========================================================================
// 71. Far JMP (0xEA) in 32-bit mode
// ========================================================================

#[test]
fn test_far_jmp_32bit_real_mode() {
    // In real mode with o32: 66 EA offset32 seg16
    // JMP FAR 0x0000:0x00000500 => 66 EA 00 05 00 00 00 00
    let (mut cpu, mut mem) = setup(&[0x66, 0xEA, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x0500);
}

// ========================================================================
// 72. BSWAP (0F C8+rd)
// ========================================================================

#[test]
fn test_bswap_eax() {
    // BSWAP EAX => 0F C8 (486+)
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xC8]);
    cpu.set_cpu_type(gp64_cpu::CpuType::I80486);
    cpu.regs.eax = 0x12345678;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x78563412);
}

#[test]
fn test_bswap_ecx() {
    // BSWAP ECX => 0F C9 (486+)
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xC9]);
    cpu.set_cpu_type(gp64_cpu::CpuType::I80486);
    cpu.regs.ecx = 0xAABBCCDD;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0xDDCCBBAA);
}

// ========================================================================
// 73. MOV r32, Sreg (0x8C) in PM
// ========================================================================

#[test]
fn test_mov_r32_sreg_pm() {
    // MOV EAX, ES => 8C C0 (modrm=0xC0: seg=ES, rm=EAX)
    let (mut cpu, mut mem) = setup_pm32(&[0x8C, 0xC0]);
    cpu.regs.es = 0x10;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax & 0xFFFF, 0x10);
}

// ========================================================================
// 74. LDS/LES/LFS/LGS/LSS (C4/C5/0FB2/0FB4/0FB5)
// ========================================================================

#[test]
fn test_lds_16bit() {
    // LDS SI, [0x200] => C5 36 00 02 (modrm=0x36: reg=SI, rm=6=disp16)
    // Wait, C5 is LDS. Actually C5 is LDS and C4 is LES.
    // LDS reg, m16:16 => C5 modrm
    let (mut cpu, mut mem) = setup(&[0xC5, 0x36, 0x00, 0x02]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x03; // offset = 0x0300
    mem.ram[0x202] = 0x00;
    mem.ram[0x203] = 0x10; // segment = 0x1000
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.esi & 0xFFFF, 0x0300);
    assert_eq!(cpu.regs.ds, 0x1000);
}

#[test]
fn test_les_16bit() {
    // LES DI, [0x200] => C4 3E 00 02
    let (mut cpu, mut mem) = setup(&[0xC4, 0x3E, 0x00, 0x02]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x04; // offset = 0x0400
    mem.ram[0x202] = 0x00;
    mem.ram[0x203] = 0x20; // segment = 0x2000
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.edi & 0xFFFF, 0x0400);
    assert_eq!(cpu.regs.es, 0x2000);
}

// ========================================================================
// 75. INT n (0xCD) - software interrupt
// ========================================================================

#[test]
fn test_int_21h() {
    // INT 0x21 => CD 21
    let (mut cpu, mut mem) = setup(&[0xCD, 0x21]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::SoftwareInterrupt {
            vector: 0x21,
            executed: 1,
        }
    ));
}

// ========================================================================
// 76. IRET (0xCF)
// ========================================================================

#[test]
fn test_iret() {
    // IRET => CF
    let (mut cpu, mut mem) = setup(&[0xCF]);
    // Push flags, CS, IP on stack
    cpu.regs.set_sp(0x1000);
    cpu.regs.esp = 0x1000;
    // IP = 0x2000
    mem.ram[0x0FFA] = 0x00;
    mem.ram[0x0FFB] = 0x20;
    // CS = 0x0000
    mem.ram[0x0FFC] = 0x00;
    mem.ram[0x0FFD] = 0x00;
    // FLAGS
    mem.ram[0x0FFE] = 0x02; // RESERVED1
    mem.ram[0x0FFF] = 0x00;
    cpu.regs.set_sp(0x0FFA);
    cpu.regs.esp = 0x0FFA;
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(result, RunResult::Iret { .. }));
}

// ========================================================================
// 77. Instruction length limit (> 15 bytes of prefixes)
// ========================================================================

#[test]
fn test_instruction_length_limit() {
    // 16 x 0x66 prefix bytes should trigger #GP
    let (mut cpu, mut mem) = setup(&[
        0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
        0x90,
    ]);
    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(matches!(
        result,
        RunResult::Exception {
            vector: 13,
            error_code: 0,
            ..
        }
    ));
}

// ========================================================================
// 78. ADC/SBB reg, reg (dir=false, 32-bit) — covers line 6283
// ========================================================================

#[test]
fn test_adc_r32_r32_reg_is_dst() {
    // STC + ADC EAX, ECX => F9 13 C1 (0x13 = ADC r32, r/m32, modrm=0xC1)
    let (mut cpu, mut mem) = setup_pm32(&[0xF9, 0x13, 0xC1]);
    cpu.regs.eax = 100;
    cpu.regs.ecx = 200;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 301); // 100 + 200 + 1(CF)
}

#[test]
fn test_sbb_r32_r32_reg_is_dst() {
    // STC + SBB EAX, ECX => F9 1B C1 (0x1B = SBB r32, r/m32)
    let (mut cpu, mut mem) = setup_pm32(&[0xF9, 0x1B, 0xC1]);
    cpu.regs.eax = 500;
    cpu.regs.ecx = 200;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 299); // 500 - 200 - 1(CF)
}

// ========================================================================
// 79. ALU r8, [mem] (dir=false) — covers lines 6152-6157
// ========================================================================

#[test]
fn test_add_r8_mem8_pm() {
    // ADD CL, [0x200] => 02 0D 00 02 00 00 (0x02 = ADD r8, r/m8)
    let (mut cpu, mut mem) = setup_pm32(&[0x02, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 0x0A;
    mem.ram[0x200] = 20;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 30);
}

#[test]
fn test_or_r8_mem8_pm() {
    // OR CL, [0x200] => 0A 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0A, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 0xF0;
    mem.ram[0x200] = 0x0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 0xFF);
}

#[test]
fn test_and_r8_mem8_pm() {
    // AND CL, [0x200] => 22 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x22, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 0xFF;
    mem.ram[0x200] = 0x0F;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 0x0F);
}

#[test]
fn test_sub_r8_mem8_pm() {
    // SUB CL, [0x200] => 2A 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x2A, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 0x32;
    mem.ram[0x200] = 20;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 30);
}

#[test]
fn test_xor_r8_mem8_pm() {
    // XOR CL, [0x200] => 32 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x32, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 0xAA;
    mem.ram[0x200] = 0x55;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx & 0xFF, 0xFF);
}

#[test]
fn test_cmp_r8_mem8_pm() {
    // CMP CL, [0x200] => 3A 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x3A, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.ecx = (cpu.regs.ecx & 0xFFFF_FF00) | 0x2A;
    mem.ram[0x200] = 0x2A;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // CMP doesn't modify CL
    assert_eq!(cpu.regs.ecx & 0xFF, 0x2A);
}

// ========================================================================
// 80. CMP [mem16], reg16 — covers lines 6203-6206
// ========================================================================

#[test]
fn test_cmp_mem16_r16_pm() {
    // CMP [0x200], AX => 39 05 00 02 00 00 (0x39 = CMP r/m16, r16)
    // Wait, need 16-bit operand in PM: 66 39 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0x39, 0x05, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200] = 0x00;
    mem.ram[0x201] = 0x01; // 0x0100
    cpu.regs.set_ax(0x0100);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    // Memory unchanged by CMP
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x0100);
}

// ========================================================================
// 81. ADC/SBB r32, [mem] — covers lines 6370-6377
// ========================================================================

#[test]
fn test_adc_r32_mem32_pm() {
    // STC + ADC EAX, [0x200] => F9 13 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xF9, 0x13, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 100;
    mem.ram[0x200..0x204].copy_from_slice(&200u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 301); // 100 + 200 + 1
}

#[test]
fn test_sbb_r32_mem32_pm() {
    // STC + SBB EAX, [0x200] => F9 1B 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xF9, 0x1B, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 500;
    mem.ram[0x200..0x204].copy_from_slice(&200u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 2);
    assert_eq!(cpu.regs.eax, 299);
}

// ========================================================================
// 82. ALU r16, [mem] and r32, [mem] in PM
// ========================================================================

#[test]
fn test_add_r16_mem16_pm() {
    // ADD AX, [0x200] => 66 03 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0x03, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.set_ax(100);
    mem.ram[0x200..0x202].copy_from_slice(&200u16.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 300);
}

#[test]
fn test_add_r32_mem32_pm() {
    // ADD EAX, [0x200] => 03 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x03, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 1000;
    mem.ram[0x200..0x204].copy_from_slice(&2000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 3000);
}

#[test]
fn test_sub_r32_mem32_pm() {
    // SUB EAX, [0x200] => 2B 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x2B, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 5000;
    mem.ram[0x200..0x204].copy_from_slice(&2000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 3000);
}

#[test]
fn test_and_r32_mem32_pm() {
    // AND EAX, [0x200] => 23 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x23, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 0xFF00FF00;
    mem.ram[0x200..0x204].copy_from_slice(&0x0F0F0F0Fu32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x0F000F00);
}

#[test]
fn test_or_r32_mem32_pm() {
    // OR EAX, [0x200] => 0B 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0B, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 0xF0F0F0F0;
    mem.ram[0x200..0x204].copy_from_slice(&0x0F0F0F0Fu32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFFFFFF);
}

#[test]
fn test_xor_r32_mem32_pm() {
    // XOR EAX, [0x200] => 33 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x33, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 0xAAAAAAAA;
    mem.ram[0x200..0x204].copy_from_slice(&0x55555555u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFFFFFF);
}

#[test]
fn test_cmp_r32_mem32_pm() {
    // CMP EAX, [0x200] => 3B 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x3B, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 42;
    mem.ram[0x200..0x204].copy_from_slice(&42u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 42); // unchanged
}

// ========================================================================
// 83. Group 1 (0x81) r/m32, imm32 with memory operand in PM
// ========================================================================

#[test]
fn test_add_mem32_imm32_pm() {
    // ADD [0x200], 0x100 => 81 05 00 02 00 00 00 01 00 00
    let (mut cpu, mut mem) =
        setup_pm32(&[0x81, 0x05, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&500u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        756
    );
}

#[test]
fn test_sub_mem32_imm32_pm() {
    // SUB [0x200], 0x100 => 81 2D 00 02 00 00 00 01 00 00
    let (mut cpu, mut mem) =
        setup_pm32(&[0x81, 0x2D, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&500u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        244
    );
}

// ========================================================================
// 84. MOV r/m32, r32 and MOV r32, r/m32 with memory in PM
// ========================================================================

#[test]
fn test_mov_mem32_r32_pm() {
    // MOV [0x200], EAX => 89 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x89, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 0xCAFEBABE;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        0xCAFEBABE
    );
}

#[test]
fn test_mov_r32_mem32_pm() {
    // MOV ECX, [0x200] => 8B 0D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x8B, 0x0D, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ecx, 0xDEADBEEF);
}

// ========================================================================
// 85. TEST r/m32, imm32 with memory in PM (F7 /0)
// ========================================================================

#[test]
fn test_test_mem32_imm32_pm() {
    // TEST [0x200], 0x80000000 => F7 05 00 02 00 00 00 00 00 80
    let (mut cpu, mut mem) =
        setup_pm32(&[0xF7, 0x05, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80]);
    mem.ram[0x200..0x204].copy_from_slice(&0x80000000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::ZERO));
}

// ========================================================================
// 86. Shift/rotate with memory operands in PM
// ========================================================================

#[test]
fn test_shl_mem32_by_1_pm() {
    // SHL [0x200], 1 => D1 25 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xD1, 0x25, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&0x40000000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        0x80000000
    );
}

#[test]
fn test_shr_mem32_by_1_pm() {
    // SHR [0x200], 1 => D1 2D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xD1, 0x2D, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&0x80000000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        0x40000000
    );
}

// ========================================================================
// 87. NEG/NOT with memory operands in PM
// ========================================================================

#[test]
fn test_neg_mem32_pm() {
    // NEG [0x200] => F7 1D 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xF7, 0x1D, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&42u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()) as i32,
        -42
    );
}

#[test]
fn test_grp3_mem32_pm_nonwritable_ds_faults_before_write() {
    for (name, code) in [
        ("neg", [0xF7, 0x1D, 0x00, 0x02, 0x00, 0x00]),
        ("not", [0xF7, 0x15, 0x00, 0x02, 0x00, 0x00]),
    ] {
        let (mut cpu, mut mem) = setup_pm32(&code);
        let flags = Flags::from_bits_truncate(0x08D7);
        cpu.regs.ds_cache.is_writable = false;
        cpu.regs.ds_cache.access = 0x91;
        cpu.regs.flags = flags;
        mem.ram[0x200..0x204].copy_from_slice(&5u32.to_le_bytes());

        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);

        assert!(
            matches!(result, RunResult::Exception { vector: 13, .. }),
            "{name} should #GP on a non-writable DS"
        );
        assert_eq!(cpu.regs.eip, 0x0100, "{name} leaves EIP at fault");
        assert_eq!(
            cpu.regs.flags.bits(),
            flags.bits(),
            "{name} preserves FLAGS"
        );
        assert_eq!(
            u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
            5,
            "{name} must not write memory before the fault"
        );
    }
}

#[test]
fn test_not_mem32_pm() {
    // NOT [0x200] => F7 15 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xF7, 0x15, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&0x0000FFFFu32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(
        u32::from_le_bytes(mem.ram[0x200..0x204].try_into().unwrap()),
        0xFFFF0000
    );
}

// ========================================================================
// 88. MUL/DIV with memory operands in PM
// ========================================================================

#[test]
fn test_mul_mem32_pm() {
    // MUL [0x200] => F7 25 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xF7, 0x25, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 10;
    mem.ram[0x200..0x204].copy_from_slice(&20u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 200);
}

#[test]
fn test_div_mem32_pm() {
    // DIV [0x200] => F7 35 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0xF7, 0x35, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 100;
    cpu.regs.edx = 0;
    mem.ram[0x200..0x204].copy_from_slice(&7u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 14);
    assert_eq!(cpu.regs.edx, 2);
}

// ========================================================================
// 89. MOV r/m8, imm8 with memory operand in PM
// ========================================================================

#[test]
fn test_mov_mem8_imm8_pm() {
    // MOV [0x200], 0x42 => C6 05 00 02 00 00 42
    let (mut cpu, mut mem) = setup_pm32(&[0xC6, 0x05, 0x00, 0x02, 0x00, 0x00, 0x42]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 0x42);
}

// ========================================================================
// 90. MOV r/m16, imm16 with memory in PM (66 prefix)
// ========================================================================

#[test]
fn test_mov_mem16_imm16_pm() {
    // MOV [0x200], 0x1234 => 66 C7 05 00 02 00 00 34 12
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0xC7, 0x05, 0x00, 0x02, 0x00, 0x00, 0x34, 0x12]);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x1234);
}

#[test]
fn test_mov_mem_imm_pm_nonwritable_ds_faults_before_write() {
    for (name, code, len) in [
        (
            "mov imm8",
            &[0xC6, 0x05, 0x00, 0x02, 0x00, 0x00, 0x42][..],
            1usize,
        ),
        (
            "mov imm16",
            &[0x66, 0xC7, 0x05, 0x00, 0x02, 0x00, 0x00, 0x34, 0x12][..],
            2,
        ),
        (
            "mov imm32",
            &[0xC7, 0x05, 0x00, 0x02, 0x00, 0x00, 0xDD, 0xCC, 0xBB, 0xAA][..],
            4,
        ),
    ] {
        let (mut cpu, mut mem) = setup_pm32(code);
        let flags = Flags::from_bits_truncate(0x08D7);
        cpu.regs.ds_cache.is_writable = false;
        cpu.regs.ds_cache.access = 0x91;
        cpu.regs.flags = flags;
        mem.ram[0x200..0x204].copy_from_slice(&0xBEEF_CAFEu32.to_le_bytes());

        let mut io = ();
        let result = run(&mut cpu, &mut mem, &mut io, 1);

        assert!(
            matches!(result, RunResult::Exception { vector: 13, .. }),
            "{name} should #GP on a non-writable DS"
        );
        assert_eq!(cpu.regs.eip, 0x0100, "{name} leaves EIP at fault");
        assert_eq!(
            cpu.regs.flags.bits(),
            flags.bits(),
            "{name} preserves FLAGS"
        );
        assert_eq!(
            &mem.ram[0x200..0x200 + len],
            &0xBEEF_CAFEu32.to_le_bytes()[..len],
            "{name} must not write memory before the fault"
        );
    }
}

#[test]
fn test_setcc_mem_pm_nonwritable_ds_faults_before_write() {
    // SETG byte [0x200]
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x9F, 0x05, 0x00, 0x02, 0x00, 0x00]);
    let flags = Flags::from_bits_truncate(0x08D7);
    cpu.regs.ds_cache.is_writable = false;
    cpu.regs.ds_cache.access = 0x91;
    cpu.regs.flags = flags;
    mem.ram[0x200] = 0xAA;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "SETcc should #GP on a non-writable DS"
    );
    assert_eq!(cpu.regs.eip, 0x0100, "SETcc leaves EIP at fault");
    assert_eq!(cpu.regs.flags.bits(), flags.bits(), "SETcc preserves FLAGS");
    assert_eq!(mem.ram[0x200], 0xAA, "SETcc must fault before writing");
}

#[test]
fn test_alu_mem_pm_nonwritable_ds_faults_before_write() {
    // ADD word [0x200], AX
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0x01, 0x05, 0x00, 0x02, 0x00, 0x00]);
    let flags = Flags::from_bits_truncate(0x08D7);
    cpu.regs.ds_cache.is_writable = false;
    cpu.regs.ds_cache.access = 0x91;
    cpu.regs.flags = flags;
    cpu.regs.set_ax(1);
    mem.ram[0x200..0x202].copy_from_slice(&0x1234u16.to_le_bytes());

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "ADD r/m16,r16 should #GP on a non-writable DS"
    );
    assert_eq!(cpu.regs.eip, 0x0100, "ADD leaves EIP at fault");
    assert_eq!(cpu.regs.flags.bits(), flags.bits(), "ADD preserves FLAGS");
    assert_eq!(
        u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]),
        0x1234,
        "ADD must fault before writing"
    );
}

#[test]
fn test_cmp_mem_pm_nonwritable_ds_remains_read_only() {
    // CMP word [0x200], AX
    let (mut cpu, mut mem) = setup_pm32(&[0x66, 0x39, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.ds_cache.is_writable = false;
    cpu.regs.ds_cache.access = 0x91;
    cpu.regs.set_ax(0x1234);
    mem.ram[0x200..0x202].copy_from_slice(&0x1234u16.to_le_bytes());

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        !matches!(result, RunResult::Exception { .. }),
        "CMP r/m16,r16 should read a non-writable DS"
    );
    assert_eq!(cpu.regs.eip, 0x0107);
    assert!(cpu.regs.flags.contains(Flags::ZERO));
    assert_eq!(u16::from_le_bytes([mem.ram[0x200], mem.ram[0x201]]), 0x1234);
}

#[test]
fn test_alu_reg_mem_pm_null_ds_faults_before_destination_write() {
    // ADD AL, byte [0x200]
    let (mut cpu, mut mem) = setup_pm32(&[0x02, 0x05, 0x00, 0x02, 0x00, 0x00]);
    let flags = Flags::from_bits_truncate(0x08D7);
    cpu.regs.ds = 0;
    cpu.regs.ds_cache = SegmentCache::default();
    cpu.regs.flags = flags;
    cpu.regs.set_al(0x34);
    mem.ram[0x200] = 1;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        matches!(result, RunResult::Exception { vector: 13, .. }),
        "ADD AL,[mem] should #GP on a null DS"
    );
    assert_eq!(cpu.regs.eip, 0x0100, "ADD leaves EIP at fault");
    assert_eq!(cpu.regs.al(), 0x34, "ADD preserves destination");
    assert_eq!(cpu.regs.flags.bits(), flags.bits(), "ADD preserves FLAGS");
    assert_eq!(mem.ram[0x200], 1, "ADD must not write memory");
}

// ========================================================================
// 91. XCHG r/m8, r8 with memory in PM
// ========================================================================

#[test]
fn test_xchg_mem8_r8_pm() {
    // XCHG [0x200], AL => 86 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x86, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.set_al(0xAA);
    mem.ram[0x200] = 0xBB;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.al(), 0xBB);
    assert_eq!(mem.ram[0x200], 0xAA);
}

// ========================================================================
// 92. MOVSX/MOVZX with memory in PM
// ========================================================================

#[test]
fn test_movzx_r32_mem8_pm() {
    // MOVZX EAX, [0x200] => 0F B6 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB6, 0x05, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200] = 0x80;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x80);
}

#[test]
fn test_movsx_r32_mem8_pm() {
    // MOVSX EAX, [0x200] => 0F BE 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBE, 0x05, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200] = 0x80;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFFFF80);
}

#[test]
fn test_movzx_r32_mem16_pm() {
    // MOVZX EAX, [0x200] => 0F B7 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB7, 0x05, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x202].copy_from_slice(&0x8000u16.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0x8000);
}

#[test]
fn test_movsx_r32_mem16_pm() {
    // MOVSX EAX, [0x200] => 0F BF 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBF, 0x05, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x202].copy_from_slice(&0x8000u16.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 0xFFFF8000);
}

// ========================================================================
// 93. IMUL r32, [mem] in PM
// ========================================================================

#[test]
fn test_imul_r32_mem32_pm() {
    // IMUL EAX, [0x200] => 0F AF 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xAF, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.eax = 100;
    mem.ram[0x200..0x204].copy_from_slice(&200u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20000);
}

// ========================================================================
// 94. BSF/BSR with memory in PM
// ========================================================================

#[test]
fn test_bsf_mem32_pm() {
    // BSF EAX, [0x200] => 0F BC 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBC, 0x05, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&0x00100000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20);
}

#[test]
fn test_bsr_mem32_pm() {
    // BSR EAX, [0x200] => 0F BD 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBD, 0x05, 0x00, 0x02, 0x00, 0x00]);
    mem.ram[0x200..0x204].copy_from_slice(&0x00100000u32.to_le_bytes());
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eax, 20);
}

// ========================================================================
// 95. BT/BTS/BTR/BTC r32, r32 in PM
// ========================================================================

#[test]
fn test_bt_r32_r32_pm() {
    // BT ECX, EAX => 0F A3 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xA3, 0xC1]);
    cpu.regs.ecx = 0x80000000;
    cpu.regs.eax = 31;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY));
}

#[test]
fn test_bts_r32_r32_pm() {
    // BTS ECX, EAX => 0F AB C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xAB, 0xC1]);
    cpu.regs.ecx = 0;
    cpu.regs.eax = 5;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was 0
    assert_eq!(cpu.regs.ecx & 0x20, 0x20); // bit 5 set
}

#[test]
fn test_btr_r32_r32_pm() {
    // BTR ECX, EAX => 0F B3 C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xB3, 0xC1]);
    cpu.regs.ecx = 0xFF;
    cpu.regs.eax = 3;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(cpu.regs.flags.contains(Flags::CARRY)); // was 1
    assert_eq!(cpu.regs.ecx & 0x08, 0); // bit 3 cleared
}

#[test]
fn test_btc_r32_r32_pm() {
    // BTC ECX, EAX => 0F BB C1
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0xBB, 0xC1]);
    cpu.regs.ecx = 0;
    cpu.regs.eax = 7;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert!(!cpu.regs.flags.contains(Flags::CARRY)); // was 0
    assert_eq!(cpu.regs.ecx & 0x80, 0x80); // bit 7 toggled to 1
}

// ========================================================================
// 96. SETCC with memory operand in PM
// ========================================================================

#[test]
fn test_sete_mem8_pm() {
    // SETE [0x200] => 0F 94 05 00 02 00 00
    let (mut cpu, mut mem) = setup_pm32(&[0x0F, 0x94, 0x05, 0x00, 0x02, 0x00, 0x00]);
    cpu.regs.flags.insert(Flags::ZERO);
    mem.ram[0x200] = 0;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(mem.ram[0x200], 1);
}

// ========================================================================
// 97. LEA with addr16 mode in 16-bit PM
// ========================================================================

#[test]
fn test_lea_r16_bp_si_disp8() {
    // LEA AX, [BP+SI+0x10] => 8D 42 10 (modrm=0x42, md=1 rm=2 => BP+SI+disp8)
    let (mut cpu, mut mem) = setup(&[0x8D, 0x42, 0x10]);
    cpu.regs.ebp = 0x100;
    cpu.regs.esi = 0x20;
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.ax(), 0x130); // 0x100 + 0x20 + 0x10
}

// ========================================================================
// 98. RET imm16 in PM
// ========================================================================

#[test]
fn test_ret_imm16_pm() {
    // RET 0x0004 => C2 04 00 (in 32-bit PM)
    let (mut cpu, mut mem) = setup_pm32(&[0xC2, 0x04, 0x00]);
    let ret_addr: u32 = 0x5000;
    let esp = cpu.regs.esp;
    mem.ram[(esp - 4) as usize..esp as usize].copy_from_slice(&ret_addr.to_le_bytes());
    cpu.regs.esp = esp - 4;
    cpu.regs.set_sp((esp - 4) as u16);
    let mut io = ();
    let _ = run(&mut cpu, &mut mem, &mut io, 1);
    assert_eq!(cpu.regs.eip, 0x5000);
    // ESP should be old_esp + 4 (ret) + 4 (imm16 bytes to pop)
    assert_eq!(cpu.regs.esp, esp + 4);
}
