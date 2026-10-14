//! Source contract for `benches/fast_interp.rs`.
//!
//! MDD-BUG-CRUCIBLE-00533: this used to assert global substring counts over the
//! whole benchmark file — `matches("run_bounded").count() >= 5` and
//! `matches("preflight_").count() >= 5` — while claiming "every benchmark group"
//! was covered. The file's own `use` line contributes one `run_bounded` and all
//! five `preflight_*` names, so one runner call could vanish and the first
//! assertion still passed, and **every** preflight call could vanish and the
//! second still passed. The oracle never associated either requirement with a
//! group.
//!
//! It now parses the benchmark source into its per-group function bodies and
//! checks each body on its own, and `bench_contract_catches_*` proves the check
//! is not vacuous by mutating the real source one group at a time.

#[path = "../benches/fast_interp_support.rs"]
mod fast_interp_support;

use fast_interp_support::{
    preflight_add, preflight_push_pop, preflight_rep_stosw, preflight_shifts, preflight_sse2,
    preflight_sse2_mem, preflight_sse_addps, preflight_sse_addss, preflight_x87, run_bounded,
    setup_add_cpu, setup_warm_x87_cpu, ExpectedTerminal,
};
use gp64_memory::Memory;

const BENCH_SOURCE: &str = include_str!("../benches/fast_interp.rs");

/// A benchmark group as the contract sees it: the body of a top-level
/// `fn <name>(c: &mut Criterion) { .. }`.
struct BenchGroup {
    name: String,
    body: String,
}

/// Byte offsets of `src` that sit inside a `"…"` literal or after a `//`
/// comment marker, so brace counting cannot be thrown by either.
///
/// Deliberately simple — the benchmark source has no raw strings, no block
/// comments and no braces inside literals. If that ever changes, the
/// `every registered group must be parseable` check below fails loudly rather
/// than silently under-reporting.
fn code_mask(src: &str) -> Vec<bool> {
    let bytes = src.as_bytes();
    let mut is_code = vec![true; bytes.len()];
    let mut in_string = false;
    let mut in_line_comment = false;
    let mut escaped = false;

    for i in 0..bytes.len() {
        let c = bytes[i];
        if in_line_comment {
            is_code[i] = false;
            if c == b'\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_string {
            is_code[i] = false;
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        if c == b'"' {
            in_string = true;
            is_code[i] = false;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            in_line_comment = true;
            is_code[i] = false;
            continue;
        }
    }

    is_code
}

/// Names listed in the `criterion_group!(benches, ..)` registration.
///
/// This is the totality driver: a group that is not registered never runs, and
/// a registered group that cannot be parsed is a contract failure rather than a
/// silent skip.
/// Each registered benchmark group and the preflight that verifies ITS loop.
///
/// The per-group check used to accept any `preflight_` substring, so a group
/// could call another group's preflight and stay green while timing a loop
/// nothing had verified (MDD-BUG-KIL-01057). The names do not follow one
/// pattern - `loop` is verified by `preflight_add`, `x87_memory` by
/// `preflight_x87`, `sse2_memory` by `preflight_sse2_mem` - so the binding is
/// written out rather than derived, and checked exhaustively in both
/// directions.
const EXPECTED_PREFLIGHTS: &[(&str, &str)] = &[
    ("bench_fast_interp_loop", "preflight_add"),
    ("bench_fast_interp_rep_stosw", "preflight_rep_stosw"),
    ("bench_fast_interp_shifts", "preflight_shifts"),
    ("bench_fast_interp_push_pop", "preflight_push_pop"),
    ("bench_fast_interp_x87_memory", "preflight_x87"),
    ("bench_fast_interp_sse2", "preflight_sse2"),
    ("bench_fast_interp_sse2_memory", "preflight_sse2_mem"),
    ("bench_fast_interp_sse_addps", "preflight_sse_addps"),
    ("bench_fast_interp_sse_addss", "preflight_sse_addss"),
];

fn registered_group_names(src: &str) -> Vec<String> {
    let start = match src.find("criterion_group!(") {
        Some(idx) => idx + "criterion_group!(".len(),
        None => return Vec::new(),
    };
    let end = match src[start..].find(')') {
        Some(idx) => start + idx,
        None => return Vec::new(),
    };
    src[start..end]
        .split(',')
        .map(|item| item.trim())
        .filter(|item| !item.is_empty() && *item != "benches")
        .map(|item| item.to_string())
        .collect()
}

fn bench_groups(src: &str) -> Vec<BenchGroup> {
    let is_code = code_mask(src);
    let bytes = src.as_bytes();
    let signature_tail = "(c: &mut Criterion) {";
    let mut groups = Vec::new();
    let mut search_from = 0usize;

    while let Some(rel) = src[search_from..].find(signature_tail) {
        let sig_at = search_from + rel;
        search_from = sig_at + signature_tail.len();
        if !is_code[sig_at] {
            continue;
        }

        // Walk back over the fn name to the `fn ` keyword.
        let name_end = sig_at;
        let mut name_start = name_end;
        while name_start > 0 && {
            let c = bytes[name_start - 1];
            c.is_ascii_alphanumeric() || c == b'_'
        } {
            name_start -= 1;
        }
        if !src[..name_start].ends_with("fn ") {
            continue;
        }

        let open = sig_at + signature_tail.len() - 1;
        let mut depth = 0i32;
        let mut close = None;
        for i in open..bytes.len() {
            if !is_code[i] {
                continue;
            }
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { continue };

        groups.push(BenchGroup {
            name: src[name_start..name_end].to_string(),
            body: src[open + 1..close].to_string(),
        });
        search_from = close;
    }

    groups
}

/// Every violation of the per-group contract, as human-readable lines.
///
/// Returns ALL violations rather than aborting at the first, so one run names
/// every group that drifted.
fn contract_violations(src: &str) -> Vec<String> {
    let mut violations = Vec::new();

    if src.contains("BatchSize::SmallInput") {
        violations.push(
            "128 MiB emulator images must use PerIteration batching, not BatchSize::SmallInput"
                .to_string(),
        );
    }
    if src.contains(".executed()") {
        violations.push(
            "benchmarks must inspect RunResult variants instead of accepting terminal exits \
             via .executed()"
                .to_string(),
        );
    }

    let groups = bench_groups(src);
    let registered = registered_group_names(src);
    if registered.is_empty() {
        violations.push("no criterion_group! registration found".to_string());
    }

    for name in &registered {
        let Some(group) = groups.iter().find(|g| &g.name == name) else {
            violations.push(format!(
                "{name} is registered in criterion_group! but its \
                 `fn {name}(c: &mut Criterion)` body could not be parsed"
            ));
            continue;
        };
        if !group.body.contains("run_bounded(") {
            violations.push(format!(
                "{name} does not call run_bounded(): the group would accept any \
                 interpreter exit instead of failing closed"
            ));
        }
        match EXPECTED_PREFLIGHTS.iter().find(|(group, _)| *group == name) {
            Some((_, preflight)) => {
                // Bind the group to ITS OWN helper. An arbitrary `preflight_`
                // substring let a group keep a different group's preflight -
                // green, and verifying the wrong loop (MDD-BUG-KIL-01057).
                // `sse2` is also a prefix of `sse2_memory`'s helper, so the
                // call is matched with its opening paren.
                let call = format!("{preflight}(");
                if !group.body.contains(&call) {
                    violations.push(format!(
                        "{name} does not call {preflight}(): a group must run the \
                         preflight that verifies ITS OWN loop, not merely some \
                         preflight_*"
                    ));
                }
            }
            None => violations.push(format!(
                "{name} is registered but has no entry in EXPECTED_PREFLIGHTS: \
                 add the group and the preflight that verifies it, or the \
                 binding silently stops covering new benchmarks"
            )),
        }
    }

    for (group, _) in EXPECTED_PREFLIGHTS {
        if !registered.iter().any(|name| name == group) {
            violations.push(format!(
                "EXPECTED_PREFLIGHTS names {group}, which is not registered in \
                 criterion_group!: drop the stale entry"
            ));
        }
    }

    violations
}

#[test]
fn fast_interp_benchmarks_are_bounded_and_keep_large_inputs_outside_timing() {
    let violations = contract_violations(BENCH_SOURCE);
    assert!(
        violations.is_empty(),
        "benchmark contract violations ({}):\n  - {}",
        violations.len(),
        violations.join("\n  - ")
    );
}

/// The contract must see every registered group, and see them as distinct.
///
/// If the parser silently returned nothing, `contract_violations` above would
/// pass vacuously — the same failure mode this bug is about.
#[test]
fn bench_contract_sees_every_registered_group() {
    let registered = registered_group_names(BENCH_SOURCE);
    let groups = bench_groups(BENCH_SOURCE);
    assert!(
        registered.len() >= 5,
        "expected at least the five original benchmark groups, found {registered:?}"
    );
    for name in &registered {
        assert!(
            groups.iter().any(|g| &g.name == name),
            "registered group {name} was not parsed out of the benchmark source"
        );
    }
}

/// Rename one identifier INSIDE one group's body, leaving the imports and every
/// other group untouched — the exact edit the old global-count oracle could not
/// see.
fn mutate_inside_group(src: &str, group: &str, needle: &str, replacement: &str) -> String {
    let target = bench_groups(src)
        .into_iter()
        .find(|g| g.name == group)
        .unwrap_or_else(|| panic!("group {group} not found"));
    let body_at = src
        .find(&target.body)
        .unwrap_or_else(|| panic!("body of {group} not found in source"));
    let hit = target
        .body
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not found inside {group}"));
    let at = body_at + hit;

    let mut mutated = String::with_capacity(src.len());
    mutated.push_str(&src[..at]);
    mutated.push_str(replacement);
    mutated.push_str(&src[at + needle.len()..]);
    mutated
}

#[test]
fn bench_contract_catches_a_group_that_drops_its_runner() {
    let mutated = mutate_inside_group(
        BENCH_SOURCE,
        "bench_fast_interp_shifts",
        "run_bounded(",
        "run_unchecked_placeholder(",
    );
    assert_ne!(mutated, BENCH_SOURCE, "mutation must change the source");
    // The import line still names `run_bounded`, and four other groups still
    // call it — which is exactly why the old `count() >= 5` stayed green.
    assert!(
        mutated.matches("run_bounded").count() >= 5,
        "the mutation must survive the OLD global-count oracle, or it does not \
         demonstrate the bug"
    );

    let violations = contract_violations(&mutated);
    assert!(
        violations
            .iter()
            .any(|v| v.contains("bench_fast_interp_shifts") && v.contains("run_bounded")),
        "expected a run_bounded violation naming bench_fast_interp_shifts, got {violations:?}"
    );
}

#[test]
fn bench_contract_catches_a_group_that_drops_its_preflight() {
    let mutated = mutate_inside_group(
        BENCH_SOURCE,
        "bench_fast_interp_x87_memory",
        "preflight_x87(",
        "black_box_only_x87(",
    );
    assert_ne!(mutated, BENCH_SOURCE, "mutation must change the source");
    assert!(
        mutated.matches("preflight_").count() >= 5,
        "the mutation must survive the OLD global-count oracle, or it does not \
         demonstrate the bug"
    );

    let violations = contract_violations(&mutated);
    assert!(
        violations
            .iter()
            .any(|v| v.contains("bench_fast_interp_x87_memory") && v.contains("preflight_")),
        "expected a preflight violation naming bench_fast_interp_x87_memory, got {violations:?}"
    );
}

/// MDD-BUG-KIL-00812: the x87 row claims a cached-dispatch envelope, so the
/// CPU it times must already hold every decode the loop needs. The cache is
/// per-`Cpu`, and the row used to time a fresh, cold one on every batch.
#[test]
fn x87_memory_row_times_a_warm_decode_cache() {
    let mut mem = Memory::new();
    let warm = preflight_x87(&mut mem, 10);
    let mut cpu = setup_warm_x87_cpu(10, &warm);
    run_bounded(&mut cpu, &mut mem, 10 * 4 + 1, ExpectedTerminal::Halt);
    assert_eq!(
        cpu.x87_decode_cache.misses, 0,
        "the timed CPU decoded an x87 instruction: its cache was cold"
    );
    assert_eq!(cpu.x87_decode_cache.hits, 2 * 10);

    let body = bench_groups(BENCH_SOURCE)
        .into_iter()
        .find(|group| group.name == "bench_fast_interp_x87_memory")
        .expect("x87 group must parse")
        .body;
    assert!(
        body.contains("setup_warm_x87_cpu("),
        "the x87 row must time a CPU built from the preflight's warm cache"
    );
}

#[test]
fn every_fast_interp_preflight_checks_real_architectural_state() {
    let mut mem = Memory::new();
    preflight_add(&mut mem, 10);
    preflight_rep_stosw(&mut mem, 10);
    preflight_shifts(&mut mem, 10);
    preflight_push_pop(&mut mem, 10);
    preflight_x87(&mut mem, 10);
    preflight_sse2(&mut mem, 10);
    preflight_sse2_mem(&mut mem, 10);
    preflight_sse_addps(&mut mem, 10);
    preflight_sse_addss(&mut mem, 10);
}

#[test]
#[should_panic(expected = "unexpected interpreter exit")]
fn bounded_runner_rejects_terminal_exceptions() {
    let mut cpu = setup_add_cpu();
    let mut mem = Memory::new();
    mem.write_u8(0x1_0000, 0x0f);
    mem.write_u8(0x1_0001, 0x0b); // UD2
    run_bounded(&mut cpu, &mut mem, 1, ExpectedTerminal::Budget);
}
