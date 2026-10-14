//! Decomposed x87 cost benchmarks.
//!
//! The end-to-end cached interpreter envelope lives in gp64-interp's
//! `fast_interp/x87_memory` group. These rows isolate the CPU-owned primitives
//! needed to attribute that envelope without timing emulator-image allocation.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use gp64_cpu::{
    fpu::{self, X87Dispatch},
    x87_decode::{decode_x87, CodeBitness, DecodedX87},
    x87_ops::F80,
    Cpu,
};
use gp64_memory::Memory;

#[cfg(target_arch = "x86_64")]
mod host {
    use std::{arch::asm, time::Instant};

    use gp64_cpu::x87_ops;

    use super::*;

    /// Proof token for raw benchmark-only x87 probes. Acquisition fails closed
    /// if dyncore owns a resident guest context; the enclosing guard restores the
    /// caller's control word after the measured region.
    struct BenchmarkX87Ownership;

    impl BenchmarkX87Ownership {
        fn acquire() -> Self {
            gp64_cpu::host_x87_enter();
            Self
        }

        #[inline(always)]
        fn certify_inline_asm(&self) {}
    }

    fn read_control_word(ownership: &BenchmarkX87Ownership) -> u16 {
        let mut cw = 0u16;
        unsafe {
            ownership.certify_inline_asm();
            asm!(
                "fnstcw word ptr [{cw}]",
                cw = in(reg) &mut cw as *mut u16,
                options(nostack, preserves_flags),
            );
        }
        cw
    }

    fn load_control_word(ownership: &BenchmarkX87Ownership, cw: &u16) {
        unsafe {
            ownership.certify_inline_asm();
            asm!(
                "fldcw word ptr [{cw}]",
                cw = in(reg) cw,
                options(nostack, preserves_flags),
            );
        }
    }

    fn clear_x87_exceptions(ownership: &BenchmarkX87Ownership) {
        unsafe {
            ownership.certify_inline_asm();
            asm!("fnclex", options(nostack));
        }
    }

    struct ControlWordGuard {
        original: u16,
        ownership: BenchmarkX87Ownership,
    }

    impl ControlWordGuard {
        fn new() -> Self {
            let ownership = BenchmarkX87Ownership::acquire();
            let original = read_control_word(&ownership);
            Self {
                original,
                ownership,
            }
        }

        fn ownership(&self) -> &BenchmarkX87Ownership {
            &self.ownership
        }
    }

    impl Drop for ControlWordGuard {
        fn drop(&mut self) {
            load_control_word(&self.ownership, &self.original);
        }
    }

    fn move_f80(ownership: &BenchmarkX87Ownership, source: &F80, destination: &mut F80) {
        unsafe {
            ownership.certify_inline_asm();
            asm!(
                "fld tbyte ptr [{source}]",
                "fstp tbyte ptr [{destination}]",
                source = in(reg) source.0.as_ptr(),
                destination = in(reg) destination.0.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
    }

    fn resident_add(ownership: &BenchmarkX87Ownership, a: &F80, b: &F80, result: &mut F80) {
        unsafe {
            ownership.certify_inline_asm();
            asm!(
                "fld tbyte ptr [{a}]",
                "fld tbyte ptr [{b}]",
                "faddp",
                "fstp tbyte ptr [{result}]",
                a = in(reg) a.0.as_ptr(),
                b = in(reg) b.0.as_ptr(),
                result = in(reg) result.0.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
    }

    fn exact_context_add(
        ownership: &BenchmarkX87Ownership,
        a: &F80,
        b: &F80,
        cw: u16,
        guest_status: &mut u16,
        environment: &mut [u8; 28],
        result: &mut F80,
    ) {
        // A memory-backed context must restore guest status before every host
        // operation: the artificial FSTP used to return F80 bytes perturbs C1.
        environment[0..2].copy_from_slice(&(cw | 0x003f).to_le_bytes());
        environment[4..6].copy_from_slice(&(*guest_status & !0x3800).to_le_bytes());
        environment[8..10].copy_from_slice(&0xffffu16.to_le_bytes());
        unsafe {
            ownership.certify_inline_asm();
            asm!(
                "fldenv [{environment}]",
                "fld tbyte ptr [{a}]",
                "fld tbyte ptr [{b}]",
                "faddp",
                "fnstsw word ptr [{status}]",
                "fstp tbyte ptr [{result}]",
                environment = in(reg) environment.as_ptr(),
                a = in(reg) a.0.as_ptr(),
                b = in(reg) b.0.as_ptr(),
                status = in(reg) guest_status as *mut u16,
                result = in(reg) result.0.as_mut_ptr(),
                out("st(0)") _, out("st(1)") _, out("st(2)") _, out("st(3)") _,
                out("st(4)") _, out("st(5)") _, out("st(6)") _, out("st(7)") _,
                options(nostack),
            );
        }
    }

    pub fn bench_control_word(c: &mut Criterion) {
        let mut group = c.benchmark_group("x87/control_word");
        let guest_cw = 0x007fu16;

        group.bench_function("fldcw_only", |b| {
            b.iter_custom(|iterations| {
                let _restore = ControlWordGuard::new();
                let start = Instant::now();
                for _ in 0..iterations {
                    load_control_word(_restore.ownership(), black_box(&guest_cw));
                }
                start.elapsed()
            });
        });

        group.bench_function("fnclex_plus_fldcw", |b| {
            b.iter_custom(|iterations| {
                let _restore = ControlWordGuard::new();
                let start = Instant::now();
                for _ in 0..iterations {
                    clear_x87_exceptions(_restore.ownership());
                    load_control_word(_restore.ownership(), black_box(&guest_cw));
                }
                start.elapsed()
            });
        });

        group.finish();
    }

    pub fn bench_movement(c: &mut Criterion) {
        let source = F80::from_f64(1.25);
        let mut preflight = F80::ZERO;
        let preflight_guard = ControlWordGuard::new();
        move_f80(preflight_guard.ownership(), &source, &mut preflight);
        assert_eq!(preflight, source);

        c.bench_function("x87/f80_movement/fld_fstp_tbyte", |b| {
            b.iter_custom(|iterations| {
                let _restore = ControlWordGuard::new();
                let guest_cw = 0x037fu16;
                load_control_word(_restore.ownership(), &guest_cw);
                let mut destination = F80::ZERO;
                let start = Instant::now();
                for _ in 0..iterations {
                    move_f80(
                        _restore.ownership(),
                        black_box(&source),
                        black_box(&mut destination),
                    );
                }
                let elapsed = start.elapsed();
                assert_eq!(destination, source);
                elapsed
            });
        });
    }

    pub fn bench_arithmetic(c: &mut Criterion) {
        let a = F80::from_f64(1.25);
        let b_value = F80::from_f64(2.5);
        let cw = 0x037fu16;
        let (expected, expected_sw) = x87_ops::f80_add_status(&a, &b_value, cw);
        assert_eq!(expected, F80::from_f64(3.75));
        assert_eq!(expected_sw & 0x023f, 0);

        let mut group = c.benchmark_group("x87/arithmetic");
        group.bench_function("production_add_with_status", |b| {
            b.iter_custom(|iterations| {
                let _restore = ControlWordGuard::new();
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(x87_ops::f80_add_status(
                        black_box(&a),
                        black_box(&b_value),
                        black_box(cw),
                    ));
                }
                start.elapsed()
            });
        });

        group.bench_function("exact_context_add_with_status", |b| {
            let _restore = ControlWordGuard::new();
            let mut environment = [0u8; 28];
            let mut status = 0u16;
            let mut result = F80::ZERO;
            exact_context_add(
                _restore.ownership(),
                &a,
                &b_value,
                cw,
                &mut status,
                &mut environment,
                &mut result,
            );
            assert_eq!(result, expected);
            assert_eq!(status & 0x023f, expected_sw & 0x023f);
            b.iter(|| {
                exact_context_add(
                    _restore.ownership(),
                    black_box(&a),
                    black_box(&b_value),
                    black_box(cw),
                    black_box(&mut status),
                    black_box(&mut environment),
                    black_box(&mut result),
                );
                black_box((result, status));
            });
        });

        group.bench_function("resident_cw_add_movement", |b| {
            b.iter_custom(|iterations| {
                let _restore = ControlWordGuard::new();
                load_control_word(_restore.ownership(), &cw);
                let mut result = F80::ZERO;
                let start = Instant::now();
                for _ in 0..iterations {
                    resident_add(
                        _restore.ownership(),
                        black_box(&a),
                        black_box(&b_value),
                        black_box(&mut result),
                    );
                }
                let elapsed = start.elapsed();
                assert_eq!(result, expected);
                elapsed
            });
        });
        group.finish();
    }
}

fn dispatch_cpu(a: F80, b: F80, cw: u16) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.fpu.control_word = cw;
    cpu.fpu.push(b);
    cpu.fpu.push(a);
    cpu
}

#[derive(Clone, Copy)]
struct BenchOp {
    descriptor: DecodedX87,
    dispatch: X87Dispatch,
}

fn bench_op(bytes: &[u8]) -> BenchOp {
    let descriptor = decode_x87(bytes, CodeBitness::Bits32).unwrap();
    BenchOp {
        dispatch: X87Dispatch::classify(&descriptor),
        descriptor,
    }
}

fn fnstsw_ax_op() -> BenchOp {
    bench_op(&[0xdf, 0xe0])
}

fn add_op() -> BenchOp {
    bench_op(&[0xd8, 0xc1])
}

#[inline]
fn execute_generic_op(cpu: &mut Cpu, op: BenchOp, mem: &mut Memory) {
    fpu::execute_decoded(cpu, &op.descriptor, mem).unwrap();
    cpu.x87_retired += 1;
}

#[inline]
fn execute_cached_op(cpu: &mut Cpu, op: BenchOp, mem: &mut Memory) {
    fpu::execute_decoded_with_dispatch(cpu, &op.descriptor, mem, op.dispatch).unwrap();
    cpu.x87_retired += 1;
}

fn execute_generic_sequence(cpu: &mut Cpu, ops: &[BenchOp], mem: &mut Memory) {
    for &op in ops {
        execute_generic_op(cpu, op, mem);
    }
}

fn execute_cached_sequence(cpu: &mut Cpu, ops: &[BenchOp], mem: &mut Memory) {
    for &op in ops {
        execute_cached_op(cpu, op, mem);
    }
}

fn assert_sequence_state(label: &str, generic: &Cpu, cached: &Cpu, expected_retired: u64) {
    for stack_index in 0..8 {
        assert_eq!(
            generic.fpu.get_st(stack_index),
            cached.fpu.get_st(stack_index),
            "{label}: ST({stack_index}) mismatch"
        );
    }
    for physical_index in 0..8 {
        assert_eq!(
            generic.fpu.get_tag(physical_index),
            cached.fpu.get_tag(physical_index),
            "{label}: tag {physical_index} mismatch"
        );
    }
    assert_eq!(
        generic.fpu.status_word, cached.fpu.status_word,
        "{label}: status word mismatch"
    );
    assert_eq!(
        generic.fpu.control_word, cached.fpu.control_word,
        "{label}: control word mismatch"
    );
    assert_eq!(generic.regs.eax, cached.regs.eax, "{label}: AX mismatch");
    assert_eq!(
        generic.x87_retired, cached.x87_retired,
        "{label}: retired-count mismatch"
    );
    assert_eq!(
        generic.x87_retired, expected_retired,
        "{label}: unexpected retired count"
    );
}

fn assert_sequence_matches_generic(label: &str, a: F80, b_value: F80, cw: u16, ops: &[BenchOp]) {
    let mut generic = dispatch_cpu(a, b_value, cw);
    let mut cached = dispatch_cpu(a, b_value, cw);
    let mut generic_mem = Memory::new();
    let mut cached_mem = Memory::new();
    execute_generic_sequence(&mut generic, ops, &mut generic_mem);
    execute_cached_sequence(&mut cached, ops, &mut cached_mem);
    assert_sequence_state(label, &generic, &cached, ops.len() as u64);
}

fn bench_portable_dispatch(c: &mut Criterion) {
    let add = add_op();
    let status = fnstsw_ax_op();
    let exact_cluster = [add; 16];
    let alternating_cluster: [BenchOp; 16] =
        std::array::from_fn(|index| if index % 2 == 0 { add } else { status });

    // The old `CachedFpuDispatch` benchmark classified once before the timed
    // loop. Keep that shape here: only the portable executor is timed, and the
    // compatibility adapter is not mistaken for the former production path.
    assert_sequence_matches_generic(
        "single register add",
        F80::from_f64(1.25),
        F80::from_f64(2.5),
        0x037f,
        std::slice::from_ref(&add),
    );
    assert_sequence_matches_generic(
        "exact binary32 cluster",
        F80::from_f32(1.0),
        F80::from_f32(1.0),
        0x007f,
        &exact_cluster,
    );
    assert_sequence_matches_generic(
        "exact binary32 alternating FNSTSW",
        F80::from_f32(1.0),
        F80::from_f32(1.0),
        0x007f,
        &alternating_cluster,
    );

    let a = F80::from_f64(1.25);
    let b_value = F80::from_f64(2.5);
    let cw = 0x037fu16;
    let mut group = c.benchmark_group("x87/dispatch");

    let mut generic_mem = Memory::new();
    group.bench_function("portable_generic_register_add", |b| {
        b.iter_batched_ref(
            || dispatch_cpu(a, b_value, cw),
            |cpu| {
                execute_generic_op(cpu, black_box(add), &mut generic_mem);
                black_box((cpu.fpu.get_st(0), cpu.fpu.status_word, cpu.x87_retired));
            },
            BatchSize::PerIteration,
        );
    });

    let mut cached_mem = Memory::new();
    group.bench_function("portable_cached_register_add", |b| {
        b.iter_batched_ref(
            || dispatch_cpu(a, b_value, cw),
            |cpu| {
                execute_cached_op(cpu, black_box(add), &mut cached_mem);
                black_box((cpu.fpu.get_st(0), cpu.fpu.status_word, cpu.x87_retired));
            },
            BatchSize::PerIteration,
        );
    });

    let exact_a = F80::from_f32(1.0);
    let exact_b = F80::from_f32(1.0);
    let exact_cw = 0x007fu16;
    let mut exact_mem = Memory::new();
    group.bench_function("portable_cached_exact_binary32_cluster_16", |b| {
        b.iter_batched_ref(
            || dispatch_cpu(exact_a, exact_b, exact_cw),
            |cpu| {
                execute_cached_sequence(cpu, black_box(&exact_cluster), &mut exact_mem);
                black_box((cpu.fpu.get_st(0), cpu.fpu.status_word, cpu.x87_retired));
            },
            BatchSize::PerIteration,
        );
    });

    let mut alternating_mem = Memory::new();
    group.bench_function("portable_cached_exact_binary32_alternating_fnstsw_8", |b| {
        b.iter_batched_ref(
            || dispatch_cpu(exact_a, exact_b, exact_cw),
            |cpu| {
                execute_cached_sequence(cpu, black_box(&alternating_cluster), &mut alternating_mem);
                black_box((cpu.fpu.get_st(0), cpu.fpu.status_word, cpu.x87_retired));
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

#[cfg(not(target_arch = "x86_64"))]
fn bench_software_backend(c: &mut Criterion) {
    let add = add_op();
    let cases = [
        (
            "pc_single_exact_f32",
            F80::from_f32(1.25),
            F80::from_f32(2.5),
            0x007f,
        ),
        (
            "fallback_wide_extended",
            F80::from_f64(1.0 + 2f64.powi(-30)),
            F80::from_f64(2.5),
            0x037f,
        ),
    ];
    let mut group = c.benchmark_group("x87/software_backend");
    for (name, a, b_value, cw) in cases {
        let mut preflight = dispatch_cpu(a, b_value, cw);
        let mut mem = Memory::new();
        execute_cached_op(&mut preflight, add, &mut mem);
        assert_eq!(preflight.fpu.get_tag(preflight.fpu.get_top()), 0);

        group.bench_function(name, |b| {
            b.iter_batched_ref(
                || dispatch_cpu(a, b_value, cw),
                |cpu| {
                    execute_cached_op(cpu, add, &mut mem);
                    black_box((cpu.fpu.get_st(0), cpu.fpu.status_word));
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

#[cfg(target_arch = "x86_64")]
fn bench_software_backend(_: &mut Criterion) {}

#[cfg(target_arch = "x86_64")]
fn bench_control_word(c: &mut Criterion) {
    host::bench_control_word(c);
}

#[cfg(not(target_arch = "x86_64"))]
fn bench_control_word(_: &mut Criterion) {}

#[cfg(target_arch = "x86_64")]
fn bench_movement(c: &mut Criterion) {
    host::bench_movement(c);
}

#[cfg(not(target_arch = "x86_64"))]
fn bench_movement(_: &mut Criterion) {}

#[cfg(target_arch = "x86_64")]
fn bench_arithmetic(c: &mut Criterion) {
    host::bench_arithmetic(c);
}

#[cfg(not(target_arch = "x86_64"))]
fn bench_arithmetic(_: &mut Criterion) {}

criterion_group!(
    benches,
    bench_control_word,
    bench_movement,
    bench_arithmetic,
    bench_portable_dispatch,
    bench_software_backend,
);
criterion_main!(benches);
