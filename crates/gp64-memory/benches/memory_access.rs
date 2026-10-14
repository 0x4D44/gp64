//! Microbenchmarks for the memory subsystem.
//!
//! Measures read/write throughput across different address regions, JIT vs
//! standard write paths, and bulk sequential reads.

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use gp64_memory::Memory;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fresh_memory() -> Memory {
    Memory::new()
}

// ---------------------------------------------------------------------------
// read_u8 across address regions
// ---------------------------------------------------------------------------

fn bench_read_u8(c: &mut Criterion) {
    let mut g = c.benchmark_group("memory/read_u8");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    let regions: &[(&str, usize)] = &[
        ("conventional_low", 0x0500),    // Just above BDA
        ("conventional_high", 0x9_FF00), // Top of conventional
        ("vga_graphics", 0xA_0000),      // VGA graphics region
        ("text_video", 0xB_8000),        // Text-mode video memory
        ("bios_rom", 0xF_0000),          // BIOS ROM area
    ];

    for &(name, addr) in regions {
        g.bench_with_input(BenchmarkId::from_parameter(name), &addr, |b, &addr| {
            b.iter_batched_ref(
                || {
                    let mut mem = fresh_memory();
                    mem.write_u8(addr, 0x42);
                    mem
                },
                |mem| black_box(mem.read_u8(black_box(addr))),
                BatchSize::PerIteration,
            );
        });
    }

    g.finish();
}

// ---------------------------------------------------------------------------
// write_u8 across address regions
// ---------------------------------------------------------------------------

fn bench_write_u8(c: &mut Criterion) {
    let mut g = c.benchmark_group("memory/write_u8");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    let regions: &[(&str, usize)] = &[
        ("conventional", 0x0500),
        ("vga_graphics", 0xA_0000),
        ("text_video", 0xB_8000),
    ];

    for &(name, addr) in regions {
        g.bench_with_input(BenchmarkId::from_parameter(name), &addr, |b, &addr| {
            b.iter_batched_ref(
                fresh_memory,
                |mem| mem.write_u8(black_box(addr), black_box(0x42)),
                BatchSize::PerIteration,
            );
        });
    }

    g.finish();
}

// ---------------------------------------------------------------------------
// read_u32_physical — bypass path
// ---------------------------------------------------------------------------

fn bench_read_u32_physical(c: &mut Criterion) {
    let mut g = c.benchmark_group("memory/read_u32_physical");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    g.bench_function("conventional", |b| {
        b.iter_batched_ref(
            || {
                let mut mem = fresh_memory();
                mem.write_u32(0x1000, 0xDEAD_BEEF);
                mem
            },
            |mem| black_box(mem.read_u32_physical(black_box(0x1000))),
            BatchSize::PerIteration,
        );
    });

    g.finish();
}

// ---------------------------------------------------------------------------
// write_u8_jit — JIT-optimized path (no SMC tracking)
// ---------------------------------------------------------------------------

fn bench_write_u8_jit(c: &mut Criterion) {
    let mut g = c.benchmark_group("memory/write_u8_jit");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    g.bench_function("conventional", |b| {
        b.iter_batched_ref(
            fresh_memory,
            |mem| mem.write_u8_jit(black_box(0x0500), black_box(0x42)),
            BatchSize::PerIteration,
        );
    });

    g.finish();
}

// ---------------------------------------------------------------------------
// write path comparison: standard vs JIT
// ---------------------------------------------------------------------------

fn bench_write_path_comparison(c: &mut Criterion) {
    let mut g = c.benchmark_group("memory/write_path_comparison");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    g.bench_function("standard", |b| {
        b.iter_batched_ref(
            fresh_memory,
            |mem| mem.write_u8(black_box(0x0500), black_box(0x42)),
            BatchSize::PerIteration,
        );
    });

    g.bench_function("jit", |b| {
        b.iter_batched_ref(
            fresh_memory,
            |mem| mem.write_u8_jit(black_box(0x0500), black_box(0x42)),
            BatchSize::PerIteration,
        );
    });

    g.finish();
}

// ---------------------------------------------------------------------------
// Bulk sequential read (with throughput)
// ---------------------------------------------------------------------------

fn bench_bulk_read(c: &mut Criterion) {
    let mut g = c.benchmark_group("memory/bulk_read");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    for &size in &[64usize, 1024, 16384, 65536] {
        g.throughput(Throughput::Bytes(size as u64));
        g.bench_with_input(
            BenchmarkId::new("sequential_u8", size),
            &size,
            |b, &size| {
                b.iter_batched_ref(
                    || {
                        let mut mem = fresh_memory();
                        // Fill a region so reads aren't trivially optimized
                        for i in 0..size {
                            mem.write_u8(0x0500 + i, (i & 0xFF) as u8);
                        }
                        mem
                    },
                    |mem| {
                        let mut sum = 0u32;
                        for i in 0..size {
                            sum = sum.wrapping_add(mem.read_u8(0x0500 + i) as u32);
                        }
                        black_box(sum)
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    g.finish();
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_read_u8,
    bench_write_u8,
    bench_read_u32_physical,
    bench_write_u8_jit,
    bench_write_path_comparison,
    bench_bulk_read,
);
criterion_main!(benches);
