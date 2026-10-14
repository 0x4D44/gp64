use super::test_helpers::{make_descriptor, write_gdt_entry};
use super::*;

#[derive(Clone, Copy, Debug)]
enum FarPointerOp {
    Lfs,
    Lgs,
    Les,
    Lds,
    Call,
    Jmp,
}

fn data_cache(base: u32, limit: u32, granularity: bool) -> SegmentCache {
    let mut cache = SegmentCache::real_mode(0);
    cache.base = base;
    cache.limit = limit;
    cache.granularity = granularity;
    cache.big = true;
    cache.flags = if granularity { 0xC0 } else { 0x40 };
    cache.recompute_effective_limit();
    cache
}

fn code_cache() -> SegmentCache {
    let mut cache = data_cache(0, 0xFFFFF, true);
    cache.access = 0x9B;
    cache.is_code = true;
    cache.is_readable = true;
    cache.is_writable = false;
    cache
}

fn encode(op: FarPointerOp, operand_size_32: bool, offset: u16) -> Vec<u8> {
    let mut code = Vec::with_capacity(8);
    if !operand_size_32 {
        code.push(0x66);
    }
    code.push(0x67); // 16-bit addressing in a 32-bit code segment
    match op {
        FarPointerOp::Lfs => code.extend_from_slice(&[0x0F, 0xB4, 0x06]),
        FarPointerOp::Lgs => code.extend_from_slice(&[0x0F, 0xB5, 0x06]),
        FarPointerOp::Les => code.extend_from_slice(&[0xC4, 0x06]),
        FarPointerOp::Lds => code.extend_from_slice(&[0xC5, 0x06]),
        FarPointerOp::Call => code.extend_from_slice(&[0xFF, 0x1E]),
        FarPointerOp::Jmp => code.extend_from_slice(&[0xFF, 0x2E]),
    }
    code.extend_from_slice(&offset.to_le_bytes());
    code
}

fn setup(op: FarPointerOp, operand_size_32: bool, offset: u16) -> (Cpu, Memory) {
    let code = encode(op, operand_size_32, offset);
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();

    cpu.regs.cr0 |= 1;
    cpu.regs.cpl = 0;
    cpu.regs.gdtr.base = 0x1_0000;
    cpu.regs.gdtr.limit = 0x27;
    write_gdt_entry(&mut mem, 0x1_0000, [0; 8]);
    write_gdt_entry(&mut mem, 0x1_0008, make_descriptor(0, 0xFFFFF, 0x9A, 0x0C));
    write_gdt_entry(
        &mut mem,
        0x1_0010,
        make_descriptor(0x2_0000, 0xFFFF, 0x92, 0x04),
    );
    write_gdt_entry(&mut mem, 0x1_0018, make_descriptor(0, 0xFFFFF, 0x92, 0x0C));
    write_gdt_entry(
        &mut mem,
        0x1_0020,
        make_descriptor(0x4_0000, 0xFFFFF, 0x92, 0x0C),
    );

    cpu.regs.cs = 0x0008;
    cpu.regs.cs_cache = code_cache();
    cpu.cs_bits = 32;
    cpu.regs.ds = 0x0010;
    cpu.regs.ds_cache = data_cache(0x2_0000, 0xFFFF, false);
    cpu.regs.es = 0x0010;
    cpu.regs.es_cache = cpu.regs.ds_cache;
    cpu.regs.fs = 0x0010;
    cpu.regs.fs_cache = cpu.regs.ds_cache;
    cpu.regs.gs = 0x0010;
    cpu.regs.gs_cache = cpu.regs.ds_cache;
    cpu.regs.ss = 0x0018;
    cpu.regs.ss_cache = data_cache(0, 0xFFFFF, true);
    cpu.regs.esp = 0x8000;
    cpu.regs.eax = 0xCAFE_BABE;
    cpu.regs.eip = 0x0100;
    for (index, byte) in code.into_iter().enumerate() {
        mem.ram[0x0100 + index] = byte;
    }

    let value_size = if operand_size_32 { 4 } else { 2 };
    let pointer_offset = offset as usize;
    let pointer_linear = 0x2_0000 + pointer_offset;
    let target_offset = 0x0000_0200u32;
    let target_selector = match op {
        FarPointerOp::Call | FarPointerOp::Jmp => 0x0008u16,
        _ => 0x0020,
    };
    let offset_bytes = target_offset.to_le_bytes();
    mem.ram[pointer_linear..pointer_linear + value_size]
        .copy_from_slice(&offset_bytes[..value_size]);

    // Seed both the contiguous selector bytes and the independently wrapped
    // location used by the buggy implementation. A boundary fault must win
    // before either valid selector can cause architectural state to commit.
    mem.ram[pointer_linear + value_size..pointer_linear + value_size + 2]
        .copy_from_slice(&target_selector.to_le_bytes());
    let wrapped_selector = 0x2_0000 + ((pointer_offset + value_size) & 0xFFFF);
    mem.ram[wrapped_selector..wrapped_selector + 2].copy_from_slice(&target_selector.to_le_bytes());
    (cpu, mem)
}

fn cache_state(cache: &SegmentCache) -> [u64; 13] {
    [
        cache.base as u64,
        cache.limit as u64,
        cache.cached_effective_limit as u64,
        cache.access as u64,
        cache.flags as u64,
        cache.present as u64,
        cache.dpl as u64,
        cache.is_code as u64,
        cache.is_conforming as u64,
        cache.is_readable as u64,
        cache.is_writable as u64,
        cache.granularity as u64,
        cache.big as u64,
    ]
}

fn segment_state(cpu: &Cpu) -> [(u16, [u64; 13]); 5] {
    [
        (cpu.regs.cs, cache_state(&cpu.regs.cs_cache)),
        (cpu.regs.ds, cache_state(&cpu.regs.ds_cache)),
        (cpu.regs.es, cache_state(&cpu.regs.es_cache)),
        (cpu.regs.fs, cache_state(&cpu.regs.fs_cache)),
        (cpu.regs.gs, cache_state(&cpu.regs.gs_cache)),
    ]
}

fn assert_fault_without_commit(op: FarPointerOp, operand_size_32: bool, offset: u16) {
    let (mut cpu, mut mem) = setup(op, operand_size_32, offset);
    let saved_eax = cpu.regs.eax;
    let saved_eip = cpu.regs.eip;
    let saved_esp = cpu.regs.esp;
    let saved_segments = segment_state(&cpu);
    let saved_stack = mem.ram[0x7FF0..0x8000].to_vec();

    let result = run(&mut cpu, &mut mem, &mut (), 1);

    assert!(
        matches!(
            result,
            RunResult::Exception {
                vector: 13,
                error_code: 0,
                fault_addr: None,
                executed: 0,
            }
        ),
        "{op:?} {}-bit far pointer at {offset:#06X} must #GP(0), got {result:?}",
        if operand_size_32 { 32 } else { 16 },
    );
    assert_eq!(cpu.regs.eax, saved_eax, "destination register committed");
    assert_eq!(cpu.regs.eip, saved_eip, "CS:EIP committed");
    assert_eq!(cpu.regs.esp, saved_esp, "stack pointer committed");
    assert_eq!(
        segment_state(&cpu),
        saved_segments,
        "segment state committed"
    );
    assert_eq!(
        &mem.ram[0x7FF0..0x8000],
        saved_stack.as_slice(),
        "far CALL stack frame committed"
    );
}

fn assert_last_valid_span_succeeds(op: FarPointerOp, operand_size_32: bool, offset: u16) {
    let (mut cpu, mut mem) = setup(op, operand_size_32, offset);
    let saved_esp = cpu.regs.esp;
    let expected_eax = if operand_size_32 {
        0x0000_0200
    } else {
        0xCAFE_0200
    };
    let result = run(&mut cpu, &mut mem, &mut (), 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "{op:?} {}-bit last valid far pointer must succeed, got {result:?}",
        if operand_size_32 { 32 } else { 16 },
    );
    match op {
        FarPointerOp::Call => {
            assert_eq!(cpu.regs.eip, 0x0200);
            assert_eq!(
                cpu.regs.esp,
                saved_esp - if operand_size_32 { 8 } else { 4 }
            );
        }
        FarPointerOp::Jmp => {
            assert_eq!(cpu.regs.eip, 0x0200);
            assert_eq!(cpu.regs.esp, saved_esp);
        }
        FarPointerOp::Lfs => {
            assert_eq!(cpu.regs.fs, 0x0020);
            assert_eq!(cpu.regs.eax, expected_eax);
        }
        FarPointerOp::Lgs => {
            assert_eq!(cpu.regs.gs, 0x0020);
            assert_eq!(cpu.regs.eax, expected_eax);
        }
        FarPointerOp::Les => {
            assert_eq!(cpu.regs.es, 0x0020);
            assert_eq!(cpu.regs.eax, expected_eax);
        }
        FarPointerOp::Lds => {
            assert_eq!(cpu.regs.ds, 0x0020);
            assert_eq!(cpu.regs.eax, expected_eax);
        }
    }
}

fn assert_far_pointer_boundaries(op: FarPointerOp, operand_size_32: bool) {
    let (last_valid, first_invalid, wrapped_invalid) = if operand_size_32 {
        (0xFFFA, 0xFFFB, 0xFFFC)
    } else {
        (0xFFFC, 0xFFFD, 0xFFFE)
    };
    assert_last_valid_span_succeeds(op, operand_size_32, last_valid);
    assert_fault_without_commit(op, operand_size_32, first_invalid);
    assert_fault_without_commit(op, operand_size_32, wrapped_invalid);
}

macro_rules! boundary_test {
    ($name:ident, $op:ident, $operand_size_32:expr) => {
        #[test]
        fn $name() {
            assert_far_pointer_boundaries(FarPointerOp::$op, $operand_size_32);
        }
    };
}

boundary_test!(lfs_m16_16_is_one_segment_span, Lfs, false);
boundary_test!(lfs_m16_32_is_one_segment_span, Lfs, true);
boundary_test!(lgs_m16_16_is_one_segment_span, Lgs, false);
boundary_test!(lgs_m16_32_is_one_segment_span, Lgs, true);
boundary_test!(les_m16_16_is_one_segment_span, Les, false);
boundary_test!(les_m16_32_is_one_segment_span, Les, true);
boundary_test!(lds_m16_16_is_one_segment_span, Lds, false);
boundary_test!(lds_m16_32_is_one_segment_span, Lds, true);
boundary_test!(call_m16_16_is_one_segment_span, Call, false);
boundary_test!(call_m16_32_is_one_segment_span, Call, true);
boundary_test!(jmp_m16_16_is_one_segment_span, Jmp, false);
boundary_test!(jmp_m16_32_is_one_segment_span, Jmp, true);
