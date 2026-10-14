//! MDD-BUG-CRUCIBLE-00365 — the memory microbenchmarks must not time the
//! destruction of their own 128 MiB arena.
//!
//! `Memory::new` allocates the whole `MAX_RAM_SIZE` arena. Criterion's
//! `iter_batched` times the entire by-value routine call, so a routine that
//! takes `Memory` by value drops it — deallocating 128 MiB — *before the timer
//! stops*. Every sample in `memory_access.rs` was therefore contaminated by
//! teardown rather than measuring the byte read or write it is named after.
//!
//! `iter_batched_ref` hands the routine a `&mut` and drops the setup value
//! after timing, which is the fix. This guard exists because the mistake is
//! invisible: the benchmark still runs, still reports a number, and the number
//! is simply wrong — nothing fails, so only a source check can catch a
//! regression back to the by-value form.

/// The benchmark source, embedded at compile time so the guard cannot silently
/// pass by failing to find the file.
const BENCH_SRC: &str = include_str!("../benches/memory_access.rs");

/// A routine taking `Memory` by value drops the arena inside the timed
/// section. Only the `_ref` form is admissible here.
#[test]
fn memory_benchmarks_never_take_the_arena_by_value() {
    // `iter_batched_ref(` also contains `iter_batched`, so match the exact
    // by-value call form rather than the bare name.
    let by_value = BENCH_SRC.matches("iter_batched(").count();
    assert_eq!(
        by_value, 0,
        "memory_access.rs uses Criterion's by-value `iter_batched(` in {by_value} place(s). \
         That times the drop of a 128 MiB arena along with the operation under test — \
         use `iter_batched_ref` so the setup value is dropped after the timer stops."
    );
    assert!(
        BENCH_SRC.contains("iter_batched_ref("),
        "the guard found no timed routines at all — memory_access.rs has been \
         restructured, so this contract needs rewriting rather than deleting"
    );
}

/// `SmallInput` sizes batches on the assumption the setup value is small, and
/// retains many of them at once. At 128 MiB each that is the wrong policy.
#[test]
fn memory_benchmarks_do_not_batch_a_128_mib_input_as_small() {
    assert!(
        !BENCH_SRC.contains("BatchSize::SmallInput"),
        "memory_access.rs asks Criterion for `BatchSize::SmallInput`, but its setup \
         allocates the full 128 MiB RAM arena — that policy can retain several of them \
         at once. Use `BatchSize::PerIteration` for an input this large."
    );
}
