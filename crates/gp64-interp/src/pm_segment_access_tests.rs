use super::test_helpers::{make_descriptor, write_gdt_entry};
use super::*;
use gp64_memory::Memory;

fn setup_pm_access(code: &[u8], cs_access: u8, ds_access: u8, es_access: u8) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let gdt_base = 0x10000usize;

    write_gdt_entry(&mut mem, gdt_base, [0; 8]);
    write_gdt_entry(
        &mut mem,
        gdt_base + 0x08,
        make_descriptor(0, 0xFFFFF, cs_access, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base + 0x10,
        make_descriptor(0, 0xFFFFF, ds_access, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base + 0x18,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base + 0x20,
        make_descriptor(0, 0xFFFFF, es_access, 0x0C),
    );

    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = 0x27;
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 0;
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 32;
    cpu.regs.ds = 0x10;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, 0x10).unwrap();
    cpu.regs.ss = 0x18;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, 0x18).unwrap();
    cpu.regs.es = 0x20;
    cpu.regs.es_cache = cpu.read_descriptor(&mem, 0x20).unwrap();
    cpu.regs.esp = 0x8000;
    cpu.regs.eip = 0x0100;
    cpu.regs.set_ip(0x0100);

    for (i, &byte) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = byte;
    }
    (cpu, mem)
}

fn assert_gp(result: RunResult, context: &str) {
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                ..
            }
        ),
        "{context} must raise #GP(0), got {result:?}"
    );
}

#[test]
fn adc_memory_destination_requires_writable_data_segment() {
    // ADC dword ptr [0x2000], EAX
    let (mut cpu, mut mem) =
        setup_pm_access(&[0x11, 0x05, 0x00, 0x20, 0x00, 0x00], 0x9A, 0x90, 0x92);
    cpu.regs.eax = 1;
    mem.write_u32(0x2000, 5);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert_gp(result, "ADC memory destination through read-only DS");
    assert_eq!(mem.read_u32(0x2000), 5, "faulting ADC must not write");
}

#[test]
fn adc_memory_source_requires_readable_code_segment() {
    // ADC EAX, CS:[0x2000]
    let (mut cpu, mut mem) = setup_pm_access(
        &[0x2E, 0x13, 0x05, 0x00, 0x20, 0x00, 0x00],
        0x98,
        0x92,
        0x92,
    );
    cpu.regs.eax = 1;
    mem.write_u32(0x2000, 5);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert_gp(result, "ADC memory source through execute-only CS");
    assert_eq!(cpu.regs.eax, 1, "faulting ADC must not update EAX");
}

#[test]
fn mov_moffs_store_requires_writable_data_segment() {
    // MOV moffs8, AL
    let (mut cpu, mut mem) = setup_pm_access(&[0xA2, 0x00, 0x20, 0x00, 0x00], 0x9A, 0x90, 0x92);
    cpu.regs.set_al(0x42);
    mem.write_u8(0x2000, 0);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert_gp(result, "MOV moffs store through read-only DS");
    assert_eq!(mem.read_u8(0x2000), 0, "faulting MOV must not write");
}

#[test]
fn stosb_requires_writable_es() {
    let (mut cpu, mut mem) = setup_pm_access(&[0xAA], 0x9A, 0x92, 0x90);
    cpu.regs.set_al(0x42);
    cpu.regs.edi = 0x2000;
    mem.write_u8(0x2000, 0);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert_gp(result, "STOSB through read-only ES");
    assert_eq!(mem.read_u8(0x2000), 0, "faulting STOSB must not write");
}

#[test]
fn cmpsb_allows_read_only_es() {
    let (mut cpu, mut mem) = setup_pm_access(&[0xA6], 0x9A, 0x92, 0x90);
    cpu.regs.esi = 0x2000;
    cpu.regs.edi = 0x3000;
    mem.write_u8(0x2000, 0x44);
    mem.write_u8(0x3000, 0x44);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "CMPSB may read from read-only ES, got {result:?}"
    );
    assert!(cpu.regs.flags.contains(Flags::ZERO));
}

#[test]
fn bound_requires_readable_memory_segment() {
    // BOUND EAX, CS:[0x2000]
    let (mut cpu, mut mem) = setup_pm_access(
        &[0x2E, 0x62, 0x05, 0x00, 0x20, 0x00, 0x00],
        0x98,
        0x92,
        0x92,
    );
    cpu.regs.eax = 1;
    mem.write_u32(0x2000, 0);
    mem.write_u32(0x2004, 10);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert_gp(result, "BOUND memory operand through execute-only CS");
}

#[test]
fn arpl_requires_writable_memory_segment() {
    // ARPL word ptr [0x2000], AX
    let (mut cpu, mut mem) =
        setup_pm_access(&[0x63, 0x05, 0x00, 0x20, 0x00, 0x00], 0x9A, 0x90, 0x92);
    cpu.regs.eax = 3;
    mem.write_u16(0x2000, 0);

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 1);

    assert_gp(result, "ARPL memory operand through read-only DS");
    assert_eq!(mem.read_u16(0x2000), 0, "faulting ARPL must not write");
}
