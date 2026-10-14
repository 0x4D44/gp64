use super::test_helpers::{make_descriptor, write_gdt_entry};
use super::*;
use gp64_memory::Memory;

const FLAT_DATA: u16 = 0x10;
const FLAT_STACK: u16 = 0x18;
const BOUNDED_DATA: u16 = 0x20;
const BOUNDED_STACK: u16 = 0x28;

fn setup_pm32(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    let gdt_base = 0x10000usize;

    write_gdt_entry(&mut mem, gdt_base, [0; 8]);
    write_gdt_entry(
        &mut mem,
        gdt_base + 0x08,
        make_descriptor(0, 0xFFFFF, 0x9A, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base + FLAT_DATA as usize,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base + FLAT_STACK as usize,
        make_descriptor(0, 0xFFFFF, 0x92, 0x0C),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base + BOUNDED_DATA as usize,
        make_descriptor(0, 0x0FFF, 0x92, 0x04),
    );
    write_gdt_entry(
        &mut mem,
        gdt_base + BOUNDED_STACK as usize,
        make_descriptor(0, 0x0FFF, 0x92, 0x04),
    );

    cpu.regs.gdtr.base = gdt_base as u32;
    cpu.regs.gdtr.limit = 0x2F;
    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 0;
    cpu.regs.cs = 0x08;
    cpu.regs.cs_cache = cpu.read_descriptor(&mem, 0x08).unwrap();
    cpu.cs_bits = 32;
    cpu.regs.ds = FLAT_DATA;
    cpu.regs.ds_cache = cpu.read_descriptor(&mem, FLAT_DATA).unwrap();
    cpu.regs.ss = FLAT_STACK;
    cpu.regs.ss_cache = cpu.read_descriptor(&mem, FLAT_STACK).unwrap();
    cpu.regs.esp = 0x8000;
    cpu.regs.eip = 0x0100;
    cpu.regs.set_ip(0x0100);

    mem.ram[0x0100..0x0100 + code.len()].copy_from_slice(code);
    (cpu, mem)
}

fn assert_segment_fault(result: RunResult, vector: u8, context: &str) {
    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: actual,
                error_code: 0,
                executed: 1,
                ..
            } if actual == vector
        ),
        "{context} must fault after the reload retires, got {result:?}"
    );
}

#[test]
fn mov_ds_flat_to_bounded_rechecks_following_read_in_same_batch() {
    // MOV DS, AX; MOV EBX, [0x1000]
    let code = [0x8E, 0xD8, 0x8B, 0x1D, 0x00, 0x10, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm32(&code);
    cpu.regs.eax = BOUNDED_DATA as u32;
    cpu.regs.ebx = 0xA5A5_A5A5;

    let result = run(&mut cpu, &mut mem, &mut (), 2);

    assert_segment_fault(result, 13, "MOV DS followed by an out-of-limit DS read");
    assert_eq!(cpu.regs.ebx, 0xA5A5_A5A5);
}

#[test]
fn pop_ds_flat_to_bounded_rechecks_following_write_in_same_batch() {
    // POP DS; MOV [0x1000], EBX
    let code = [0x1F, 0x89, 0x1D, 0x00, 0x10, 0x00, 0x00];
    let (mut cpu, mut mem) = setup_pm32(&code);
    cpu.regs.esp -= 4;
    mem.write_u32(cpu.regs.esp as usize, BOUNDED_DATA as u32);
    cpu.regs.ebx = 0x1122_3344;
    mem.write_u32(0x1000, 0x5566_7788);

    let result = run(&mut cpu, &mut mem, &mut (), 2);

    assert_segment_fault(result, 13, "POP DS followed by an out-of-limit DS write");
    assert_eq!(mem.read_u32(0x1000), 0x5566_7788);
}

#[test]
fn lds_flat_to_bounded_rechecks_following_read_in_same_batch() {
    // LDS EAX, [0x2000]; MOV EBX, [0x1000]
    let code = [
        0xC5, 0x05, 0x00, 0x20, 0x00, 0x00, 0x8B, 0x1D, 0x00, 0x10, 0x00, 0x00,
    ];
    let (mut cpu, mut mem) = setup_pm32(&code);
    mem.write_u32(0x2000, 0x1234_5678);
    mem.write_u16(0x2004, BOUNDED_DATA);
    cpu.regs.ebx = 0xA5A5_A5A5;

    let result = run(&mut cpu, &mut mem, &mut (), 2);

    assert_segment_fault(result, 13, "LDS followed by an out-of-limit DS read");
    assert_eq!(cpu.regs.eax, 0x1234_5678);
    assert_eq!(cpu.regs.ebx, 0xA5A5_A5A5);
}

#[test]
fn lss_flat_to_bounded_rechecks_following_stack_read() {
    // LSS ESP, [0x2000]; MOV EBX, [ESP]
    let code = [0x0F, 0xB2, 0x25, 0x00, 0x20, 0x00, 0x00, 0x8B, 0x1C, 0x24];
    let (mut cpu, mut mem) = setup_pm32(&code);
    mem.write_u32(0x2000, 0x1000);
    mem.write_u16(0x2004, BOUNDED_STACK);
    cpu.regs.ebx = 0xA5A5_A5A5;

    let first = run(&mut cpu, &mut mem, &mut (), 2);
    assert!(
        matches!(first, RunResult::InterruptPollBoundary { executed: 1 }),
        "LSS must expose its interrupt-shadow boundary, got {first:?}"
    );

    let second = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(
            second,
            RunResult::Exception {
                vector: 12,
                error_code: 0,
                executed: 0,
                ..
            }
        ),
        "the out-of-limit SS read after LSS must raise #SS(0), got {second:?}"
    );
    assert_eq!(cpu.regs.ebx, 0xA5A5_A5A5);
}

#[test]
fn pop_memory_segment_fault_restores_16_and_32_bit_stack_pointers() {
    for (name, code, width) in [
        ("POP r/m32", &[0x8F, 0x05, 0x00, 0x20, 0x00, 0x00][..], 4),
        (
            "POP r/m16",
            &[0x66, 0x8F, 0x05, 0x00, 0x20, 0x00, 0x00][..],
            2,
        ),
    ] {
        let (mut cpu, mut mem) = setup_pm32(code);
        cpu.regs.ds_cache.limit = 0x0FFF;
        cpu.regs.ds_cache.granularity = false;
        cpu.regs.ds_cache.recompute_effective_limit();
        let entry_esp = cpu.regs.esp;
        mem.write_u32(entry_esp as usize, 0x1122_3344);

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
            "{name} must #GP on its out-of-limit destination, got {result:?}"
        );
        assert_eq!(
            cpu.regs.esp, entry_esp,
            "{name} must roll back its {width}-byte pop"
        );
    }
}

#[test]
fn leave_stack_segment_fault_restores_16_and_32_bit_stack_pointers() {
    for (name, code, frame_pointer) in [
        ("LEAVE32", &[0xC9][..], 0x0000_2000),
        ("LEAVE16", &[0x66, 0xC9][..], 0xABCD_2000),
    ] {
        let (mut cpu, mut mem) = setup_pm32(code);
        cpu.regs.ss_cache.limit = 0x0FFF;
        cpu.regs.ss_cache.granularity = false;
        cpu.regs.ss_cache.recompute_effective_limit();
        cpu.regs.ebp = frame_pointer;
        let entry_esp = cpu.regs.esp;

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(
                result,
                RunResult::Exception {
                    vector: 12,
                    error_code: 0,
                    executed: 0,
                    ..
                }
            ),
            "{name} must #SS on its out-of-limit stack read, got {result:?}"
        );
        assert_eq!(cpu.regs.esp, entry_esp, "{name} must restore entry ESP");
        assert_eq!(cpu.regs.ebp, frame_pointer, "{name} must preserve EBP");
    }
}

#[test]
fn pop_esp_memory_destination_uses_post_pop_address_for_16_and_32_bits() {
    for (name, code, width) in [
        ("POP [ESP]", &[0x8F, 0x04, 0x24][..], 4usize),
        ("POP word [ESP]", &[0x66, 0x8F, 0x04, 0x24][..], 2),
    ] {
        let (mut cpu, mut mem) = setup_pm32(code);
        let entry_esp = cpu.regs.esp as usize;
        mem.write_u32(entry_esp, 0x1122_3344);
        mem.write_u32(entry_esp + width, 0xA5A5_A5A5);

        let result = run(&mut cpu, &mut mem, &mut (), 1);

        assert!(
            matches!(result, RunResult::BudgetExhausted { executed: 1 }),
            "{name} must execute successfully, got {result:?}"
        );
        assert_eq!(cpu.regs.esp as usize, entry_esp + width);
        if width == 4 {
            assert_eq!(mem.read_u32(entry_esp + width), 0x1122_3344);
        } else {
            assert_eq!(mem.read_u16(entry_esp + width), 0x3344);
        }
    }
}
