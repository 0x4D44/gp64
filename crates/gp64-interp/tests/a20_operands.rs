use gp64_cpu::{Cpu, CpuType, SegmentCache};
use gp64_interp::{run, RunResult};
use gp64_memory::Memory;

const LOW: [u8; 4] = [0xff, 0x21, 0x43, 0x65];
const HIGH: [u8; 4] = [0xff, 0x32, 0x54, 0x76];
const VALUE: u32 = 0x9876_54ab;

fn fixture(a20: bool, boundary: usize) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    cpu.set_cpu_type(CpuType::I80386);
    cpu.regs.cr0 = 0;
    cpu.regs.cs = 0x100;
    cpu.regs.cs_cache = SegmentCache::real_mode(cpu.regs.cs);
    cpu.regs.eip = 0;
    for cache in [&mut cpu.regs.ds_cache, &mut cpu.regs.ss_cache] {
        // A synthetic unreal-mode base lets the same legal 000F operand test
        // every A20 transition, including preservation of bits above bit 20.
        *cache = SegmentCache::real_mode(0xffff);
        cache.base = (boundary - 0xf) as u32;
    }
    cpu.regs.ds = 0xffff;
    cpu.regs.ss = 0xffff;
    let mut mem = Memory::new();
    assert!(mem.usable_len() > 0x400003);
    for i in 0..4 {
        let linear = boundary + i;
        mem.ram[linear] = HIGH[i];
        mem.ram[linear & !(1 << 20)] = LOW[i];
    }
    // Only bit 20 distinguishes these sentinels, independent of fixture order.
    if boundary & (1 << 20) == 0 {
        mem.ram[boundary] = LOW[0];
    }
    mem.set_a20(a20);
    (cpu, mem)
}

fn instruction(cpu: &mut Cpu, mem: &mut Memory, bytes: &[u8], wide: bool) {
    let mut code = Vec::new();
    if wide {
        code.push(0x66);
    }
    code.extend_from_slice(bytes);
    mem.ram[0x1000..0x1000 + code.len()].copy_from_slice(&code);
    let result = run(cpu, mem, &mut (), 1);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 1 }),
        "{result:?}"
    );
    assert_eq!(cpu.regs.eip as usize, code.len());
}

fn expected(mem: &Memory, boundary: usize, width: usize, a20: bool) -> u32 {
    (0..width).fold(0, |value, i| {
        let addr = if a20 {
            boundary + i
        } else {
            (boundary + i) & !(1 << 20)
        };
        value | (u32::from(mem.ram[addr]) << (i * 8))
    })
}

fn assert_store(mem: &Memory, before: &[u8], boundary: usize, width: usize, a20: bool, value: u32) {
    for i in 0..width {
        let linear = boundary + i;
        let addr = if a20 { linear } else { linear & !(1 << 20) };
        let protected = (0xf0000..0x100000).contains(&addr);
        assert_eq!(
            mem.ram[addr],
            if protected {
                before[addr]
            } else {
                (value >> (8 * i)) as u8
            },
            "selected byte {i} at {addr:x}, boundary={boundary:x}, a20={a20}"
        );
        let alternate = if a20 {
            linear ^ (1 << 20)
        } else {
            linear | (1 << 20)
        };
        assert_eq!(
            mem.ram[alternate], before[alternate],
            "unselected A20 alias byte {i}"
        );
    }
}

fn loads(wide: bool) {
    let width = if wide { 4 } else { 2 };
    for boundary in [0xfffff, 0x1fffff, 0x2fffff, 0x3fffff] {
        for a20 in [true, false] {
            let (mut cpu, mut mem) = fixture(a20, boundary);
            let value = expected(&mem, boundary, width, a20);
            instruction(&mut cpu, &mut mem, &[0xa1, 0x0f, 0], wide);
            assert_eq!(
                cpu.regs.eax, value,
                "MOV load boundary={boundary:x}, a20={a20}"
            );
        }
    }
}

fn stores(wide: bool, stack: bool) {
    let width = if wide { 4 } else { 2 };
    for boundary in [0xfffff, 0x1fffff, 0x2fffff, 0x3fffff] {
        for a20 in [true, false] {
            let (mut cpu, mut mem) = fixture(a20, boundary);
            cpu.regs.eax = VALUE;
            cpu.regs.set_sp(0xf + width as u16);
            let before = mem.ram[..0x500004].to_vec();
            instruction(
                &mut cpu,
                &mut mem,
                if stack { &[0x50] } else { &[0xa3, 0xf, 0] },
                wide,
            );
            assert_store(&mem, &before, boundary, width, a20, VALUE);
            if stack {
                assert_eq!(cpu.regs.sp(), 0xf);
            }
        }
    }
}

fn pops(wide: bool) {
    let width = if wide { 4 } else { 2 };
    for boundary in [0xfffff, 0x1fffff, 0x2fffff, 0x3fffff] {
        for a20 in [true, false] {
            let (mut cpu, mut mem) = fixture(a20, boundary);
            cpu.regs.set_sp(0xf);
            let value = expected(&mem, boundary, width, a20);
            instruction(&mut cpu, &mut mem, &[0x58], wide);
            assert_eq!(cpu.regs.eax, value, "POP boundary={boundary:x}, a20={a20}");
            assert_eq!(cpu.regs.sp(), 0xf + width as u16);
        }
    }
}

fn rmw(wide: bool, exchange: bool) {
    let width = if wide { 4 } else { 2 };
    for boundary in [0xfffff, 0x1fffff, 0x2fffff, 0x3fffff] {
        for a20 in [true, false] {
            let (mut cpu, mut mem) = fixture(a20, boundary);
            cpu.regs.eax = VALUE;
            let old = expected(&mem, boundary, width, a20);
            let before = mem.ram[..0x500004].to_vec();
            instruction(
                &mut cpu,
                &mut mem,
                if exchange {
                    &[0x87, 0x06, 0xf, 0]
                } else {
                    &[0xff, 0x06, 0xf, 0]
                },
                wide,
            );
            assert_store(
                &mem,
                &before,
                boundary,
                width,
                a20,
                if exchange { VALUE } else { old.wrapping_add(1) },
            );
            if exchange {
                assert_eq!(cpu.regs.eax & if wide { u32::MAX } else { 0xffff }, old);
            }
        }
    }
}

fn movs(wide: bool, storing: bool) {
    let width = if wide { 4 } else { 2 };
    for boundary in [0xfffff, 0x1fffff, 0x2fffff, 0x3fffff] {
        for a20 in [true, false] {
            let (mut cpu, mut mem) = fixture(a20, boundary);
            cpu.regs.es = if storing { 0xffff } else { 0 };
            cpu.regs.es_cache = if storing {
                cpu.regs.ds_cache
            } else {
                SegmentCache::real_mode(0)
            };
            cpu.regs.set_si(if storing { 0x2000 } else { 0xf });
            cpu.regs.set_di(if storing { 0xf } else { 0x2000 });
            let value = if storing {
                VALUE
            } else {
                expected(&mem, boundary, width, a20)
            };
            if storing {
                cpu.regs.ds = 0;
                cpu.regs.ds_cache = SegmentCache::real_mode(0);
                mem.ram[0x2000..0x2004].copy_from_slice(&VALUE.to_le_bytes());
            }
            let before = mem.ram[..0x500004].to_vec();
            instruction(&mut cpu, &mut mem, &[0xa5], wide);
            if storing {
                assert_store(&mem, &before, boundary, width, a20, value);
            } else {
                assert_eq!(
                    expected(&mem, 0x2000, width, true),
                    value,
                    "MOVS boundary={boundary:x}, a20={a20}"
                );
            }
        }
    }
}

#[test]
fn a20_operands_word_load() {
    loads(false);
}
#[test]
fn a20_operands_dword_load() {
    loads(true);
}
#[test]
fn a20_operands_word_store() {
    stores(false, false);
}
#[test]
fn a20_operands_dword_store() {
    stores(true, false);
}
#[test]
fn a20_operands_word_push() {
    stores(false, true);
}
#[test]
fn a20_operands_dword_push() {
    stores(true, true);
}
#[test]
fn a20_operands_word_pop() {
    pops(false);
}
#[test]
fn a20_operands_dword_pop() {
    pops(true);
}
#[test]
fn a20_operands_word_inc() {
    rmw(false, false);
}
#[test]
fn a20_operands_dword_inc() {
    rmw(true, false);
}
#[test]
fn a20_operands_word_xchg() {
    rmw(false, true);
}
#[test]
fn a20_operands_dword_xchg() {
    rmw(true, true);
}
#[test]
fn a20_operands_word_movs_load() {
    movs(false, false);
}
#[test]
fn a20_operands_dword_movs_load() {
    movs(true, false);
}
#[test]
fn a20_operands_word_movs_store() {
    movs(false, true);
}
#[test]
fn a20_operands_dword_movs_store() {
    movs(true, true);
}

fn enter(wide: bool) {
    let width = if wide { 4 } else { 2 };
    for boundary in [0xfffff, 0x1fffff, 0x2fffff, 0x3fffff] {
        for a20 in [true, false] {
            let (mut cpu, mut mem) = fixture(a20, boundary);
            cpu.regs.set_sp(0x2000);
            cpu.regs.ebp = 0xf + width as u32;
            let value = expected(&mem, boundary, width, a20);
            let slot_linear = cpu.regs.ss_cache.base as usize + 0x2000 - 2 * width;
            let slot = if a20 {
                slot_linear
            } else {
                slot_linear & !(1 << 20)
            };
            // ENTER 0,2 copies one nested frame operand from BP - width.
            instruction(&mut cpu, &mut mem, &[0xc8, 0, 0, 2], wide);
            assert_eq!(
                expected(&mem, slot, width, true),
                value,
                "ENTER copied frame boundary={boundary:x}, a20={a20}"
            );
            assert_eq!(cpu.regs.sp(), 0x2000 - 3 * width as u16);
        }
    }
}

#[test]
fn a20_operands_word_enter_frame() {
    enter(false);
}
#[test]
fn a20_operands_dword_enter_frame() {
    enter(true);
}
