//! Benchmark for the fast interpreter (gp64-interp).
//!
//! Measures a tight ADD/CMP/JNE loop at varying iteration counts,
//! exercising the full `gp64_interp::run()` hot path.

mod fast_interp_support;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use fast_interp_support::{
    preflight_add, preflight_push_pop, preflight_rep_stosw, preflight_shifts, preflight_sse2,
    preflight_sse2_mem, preflight_sse_addps, preflight_sse_addss, preflight_x87, run_bounded,
    setup_add_cpu, setup_push_pop_cpu, setup_rep_stosw_cpu, setup_shifts_cpu, setup_sse2_cpu,
    setup_sse2_mem_cpu, setup_sse_addps_cpu, setup_sse_addss_cpu, setup_warm_x87_cpu,
    ExpectedTerminal,
};
use gp64_memory::Memory;

fn bench_fast_interp_loop(c: &mut Criterion) {
    let mut g = c.benchmark_group("fast_interp/add_cmp_jne");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    for &iterations in &[10u16, 100, 1000, 10000] {
        g.bench_with_input(
            BenchmarkId::from_parameter(iterations),
            &iterations,
            |b, &iterations| {
                let mut mem = Memory::new();
                preflight_add(&mut mem, iterations);
                b.iter_batched_ref(
                    setup_add_cpu,
                    |cpu| {
                        let retired = run_bounded(
                            cpu,
                            &mut mem,
                            u32::from(iterations) * 3,
                            ExpectedTerminal::Budget,
                        );
                        black_box((retired, cpu.regs.ax()))
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    g.finish();
}

/// Benchmark a REP STOSW loop to measure string operation performance.
fn bench_fast_interp_rep_stosw(c: &mut Criterion) {
    let mut g = c.benchmark_group("fast_interp/rep_stosw");
    g.sample_size(100);
    g.warm_up_time(std::time::Duration::from_secs(1));
    g.measurement_time(std::time::Duration::from_secs(5));

    for &count in &[100u16, 1000, 4096] {
        g.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            let mut mem = Memory::new();
            preflight_rep_stosw(&mut mem, count);
            b.iter_batched_ref(
                || setup_rep_stosw_cpu(count),
                |cpu| {
                    let retired =
                        run_bounded(cpu, &mut mem, u32::from(count) + 1, ExpectedTerminal::Halt);
                    black_box((retired, cpu.regs.di()))
                },
                BatchSize::PerIteration,
            );
        });
    }

    g.finish();
}

/// Benchmark a shift-heavy loop (SHL/SHR) to measure shift performance.
fn bench_fast_interp_shifts(c: &mut Criterion) {
    let mut g = c.benchmark_group("fast_interp/shifts");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));

    for &iterations in &[100u16, 1000] {
        g.bench_with_input(
            BenchmarkId::from_parameter(iterations),
            &iterations,
            |b, &iterations| {
                let mut mem = Memory::new();
                preflight_shifts(&mut mem, iterations);
                b.iter_batched_ref(
                    setup_shifts_cpu,
                    |cpu| {
                        let retired = run_bounded(
                            cpu,
                            &mut mem,
                            u32::from(iterations) * 5,
                            ExpectedTerminal::Budget,
                        );
                        black_box((retired, cpu.regs.ax()))
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    g.finish();
}

/// Benchmark push/pop heavy loop (measures stack mask optimization).
fn bench_fast_interp_push_pop(c: &mut Criterion) {
    let mut g = c.benchmark_group("fast_interp/push_pop");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));

    for &iterations in &[100u16, 1000] {
        g.bench_with_input(
            BenchmarkId::from_parameter(iterations),
            &iterations,
            |b, &iterations| {
                let mut mem = Memory::new();
                preflight_push_pop(&mut mem, iterations);
                b.iter_batched_ref(
                    setup_push_pop_cpu,
                    |cpu| {
                        let retired = run_bounded(
                            cpu,
                            &mut mem,
                            u32::from(iterations) * 7,
                            ExpectedTerminal::Budget,
                        );
                        black_box((retired, cpu.regs.ax(), cpu.regs.bx()))
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    g.finish();
}

/// Benchmark the complete cached x87 interpreter envelope with the two most
fn bench_fast_interp_x87_memory(c: &mut Criterion) {
    let mut g = c.benchmark_group("fast_interp/x87_memory");
    g.sample_size(100);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    for &iterations in &[100u16, 1000, 10000] {
        g.bench_with_input(
            BenchmarkId::from_parameter(iterations),
            &iterations,
            |b, &iterations| {
                let mut mem = Memory::new();
                // The preflight's warmed decode cache is copied into every timed
                // CPU, making the measured row a cached-dispatch envelope.
                let warm = preflight_x87(&mut mem, iterations);
                b.iter_batched_ref(
                    || setup_warm_x87_cpu(iterations, &warm),
                    |cpu| {
                        let retired = run_bounded(
                            cpu,
                            &mut mem,
                            u32::from(iterations) * 4 + 1,
                            ExpectedTerminal::Halt,
                        );
                        black_box((retired, cpu.fpu.get_st(0), cpu.fpu.status_word))
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    g.finish();
}

fn bench_fast_interp_sse2(c: &mut Criterion) {
    let mut group = c.benchmark_group("fast_interp/sse2_paddd");
    group.sample_size(100);
    group.warm_up_time(std::time::Duration::from_secs(2));
    group.measurement_time(std::time::Duration::from_secs(5));
    group.noise_threshold(0.03);

    for &iterations in &[100u16, 1000, 10_000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(iterations),
            &iterations,
            |b, &iterations| {
                let mut mem = Memory::new();
                preflight_sse2(&mut mem, iterations);
                b.iter_batched_ref(
                    || setup_sse2_cpu(iterations),
                    |cpu| {
                        let retired = run_bounded(
                            cpu,
                            &mut mem,
                            u32::from(iterations) * 3 + 1,
                            ExpectedTerminal::Halt,
                        );
                        black_box((retired, cpu.xmm_reg(0)))
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

/// PADDD with a memory source. Unlike `sse2_paddd` above this leaves the
/// interpreter's inline fast lane and measures the whole rewritten envelope:
/// SSE decode cache lookup, gating, effective-address resolution and
/// `sse_exec::execute`.
fn bench_fast_interp_sse2_memory(c: &mut Criterion) {
    let mut group = c.benchmark_group("fast_interp/sse2_paddd_mem");
    group.sample_size(100);
    group.warm_up_time(std::time::Duration::from_secs(2));
    group.measurement_time(std::time::Duration::from_secs(5));
    group.noise_threshold(0.03);

    for &iterations in &[100u16, 1000, 10_000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(iterations),
            &iterations,
            |b, &iterations| {
                let mut mem = Memory::new();
                preflight_sse2_mem(&mut mem, iterations);
                b.iter_batched_ref(
                    || setup_sse2_mem_cpu(iterations),
                    |cpu| {
                        let retired = run_bounded(
                            cpu,
                            &mut mem,
                            u32::from(iterations) * 3 + 1,
                            ExpectedTerminal::Halt,
                        );
                        black_box((retired, cpu.xmm_reg(0)))
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

/// Packed single-precision ADDPS: four float lanes per instruction through
/// the executor's float path.
fn bench_fast_interp_sse_addps(c: &mut Criterion) {
    let mut group = c.benchmark_group("fast_interp/sse_addps");
    group.sample_size(100);
    group.warm_up_time(std::time::Duration::from_secs(2));
    group.measurement_time(std::time::Duration::from_secs(5));
    group.noise_threshold(0.03);

    for &iterations in &[100u16, 1000, 10_000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(iterations),
            &iterations,
            |b, &iterations| {
                let mut mem = Memory::new();
                preflight_sse_addps(&mut mem, iterations);
                b.iter_batched_ref(
                    || setup_sse_addps_cpu(iterations),
                    |cpu| {
                        let retired = run_bounded(
                            cpu,
                            &mut mem,
                            u32::from(iterations) * 3 + 1,
                            ExpectedTerminal::Halt,
                        );
                        black_box((retired, cpu.xmm_reg(0)))
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

/// Scalar single-precision ADDSS: one float lane plus the merge rule, so the
/// per-instruction envelope dominates the arithmetic.
fn bench_fast_interp_sse_addss(c: &mut Criterion) {
    let mut group = c.benchmark_group("fast_interp/sse_addss");
    group.sample_size(100);
    group.warm_up_time(std::time::Duration::from_secs(2));
    group.measurement_time(std::time::Duration::from_secs(5));
    group.noise_threshold(0.03);

    for &iterations in &[100u16, 1000, 10_000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(iterations),
            &iterations,
            |b, &iterations| {
                let mut mem = Memory::new();
                preflight_sse_addss(&mut mem, iterations);
                b.iter_batched_ref(
                    || setup_sse_addss_cpu(iterations),
                    |cpu| {
                        let retired = run_bounded(
                            cpu,
                            &mut mem,
                            u32::from(iterations) * 3 + 1,
                            ExpectedTerminal::Halt,
                        );
                        black_box((retired, cpu.xmm_reg(0)))
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_fast_interp_loop,
    bench_fast_interp_rep_stosw,
    bench_fast_interp_shifts,
    bench_fast_interp_push_pop,
    bench_fast_interp_x87_memory,
    bench_fast_interp_sse2,
    bench_fast_interp_sse2_memory,
    bench_fast_interp_sse_addps,
    bench_fast_interp_sse_addss,
);
criterion_main!(benches);
