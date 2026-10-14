//! Microbenchmarks for the instruction decode cache.
//!
//! Measures cache hit/miss lookup, store operations, and simulated
//! full-array lookups matching the interpreter's hot path.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use gp64_cpu::{CachedInstruction, INSTR_CACHE_MASK, INSTR_CACHE_SIZE};
use iced_x86::{Decoder, DecoderOptions};

// ---------------------------------------------------------------------------
// Cache hit vs miss
// ---------------------------------------------------------------------------

fn bench_cache_hit_miss(c: &mut Criterion) {
    let mut g = c.benchmark_group("instr_cache/hit");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    g.bench_function("hit", |b| {
        b.iter_batched(
            || {
                let mut entry = CachedInstruction::new();
                let nop = &[0x90u8];
                let mut dec = Decoder::with_ip(16, nop, 0, DecoderOptions::NONE);
                entry.store(0x1000, dec.decode());
                entry
            },
            |entry| black_box(entry.is_valid_for(black_box(0x1000))),
            BatchSize::SmallInput,
        );
    });

    g.bench_function("miss", |b| {
        b.iter_batched(
            || {
                let mut entry = CachedInstruction::new();
                let nop = &[0x90u8];
                let mut dec = Decoder::with_ip(16, nop, 0, DecoderOptions::NONE);
                entry.store(0x1000, dec.decode());
                entry
            },
            |entry| black_box(entry.is_valid_for(black_box(0x2000))),
            BatchSize::SmallInput,
        );
    });

    g.finish();
}

// ---------------------------------------------------------------------------
// Cache store
// ---------------------------------------------------------------------------

fn bench_cache_store(c: &mut Criterion) {
    let mut g = c.benchmark_group("instr_cache/store");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    let nop = &[0x90u8];
    let mut dec = Decoder::with_ip(16, nop, 0, DecoderOptions::NONE);
    let instr = dec.decode();

    g.bench_function("store_entry", |b| {
        b.iter_batched(
            CachedInstruction::new,
            |mut entry| entry.store(black_box(0x1000), instr),
            BatchSize::SmallInput,
        );
    });

    g.finish();
}

// ---------------------------------------------------------------------------
// Full cache array lookup simulation
// ---------------------------------------------------------------------------

fn bench_cache_array_lookup(c: &mut Criterion) {
    let mut g = c.benchmark_group("instr_cache/array_lookup");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    g.bench_function("16k_indexed_lookup", |b| {
        b.iter_batched(
            || {
                // Populate a full 16K cache
                let mut cache = vec![CachedInstruction::new(); INSTR_CACHE_SIZE].into_boxed_slice();
                let nop = &[0x90u8];
                let mut dec = Decoder::with_ip(16, nop, 0, DecoderOptions::NONE);
                let instr = dec.decode();
                // Fill every 4th entry to simulate realistic occupancy
                for i in (0..INSTR_CACHE_SIZE).step_by(4) {
                    cache[i].store(i * 16, instr);
                }
                cache
            },
            |cache| {
                // Simulate 1000 lookups with varying addresses
                let mut hits = 0u32;
                for i in 0..1000u32 {
                    let addr = (i.wrapping_mul(7919)) as usize; // Prime stride
                    let idx = addr & INSTR_CACHE_MASK;
                    if cache[idx].is_valid_for(addr) {
                        hits += 1;
                    }
                }
                black_box(hits)
            },
            BatchSize::SmallInput,
        );
    });

    g.finish();
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_cache_hit_miss,
    bench_cache_store,
    bench_cache_array_lookup,
);
criterion_main!(benches);
