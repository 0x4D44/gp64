//! Host commit-scaling oracle for the shared arena.
//!
//! Two claims, each against the counter that can see it:
//!
//! 1. **Attaching adds no gap charge.** The gap between private RAM and the
//!    arena stays reserved, so attaching six processes' `Memory` to one store
//!    must not add anything like `gap_bytes` (about 2 GiB each) of commit.
//!    Measured as this PROCESS's private commit (`PrivateUsage`), which is
//!    where a committed gap would be charged, and which other processes on a
//!    busy box cannot move. This test binary holds nothing else, and its two
//!    tests share a process-local mutex, so nothing in the process allocates
//!    meanwhile.
//! 2. **Arena payload is charged once.** Committing a payload through one
//!    view, with six views attached, must charge it once. A pagefile-backed
//!    section's commit is charged to the SYSTEM (it is not private bytes of
//!    any process), so this half has to read the system `CommitTotal`. Other
//!    processes move that counter, so the payload is large (512 MiB), each
//!    trial brackets only the commit call, and the median of fifteen trials
//!    decides: one charge reads about 1x the payload, a per-view charge 6x.
//!    On a busy box a bracket can catch a few hundred MiB of other processes'
//!    commit in either direction (0.5x-1.6x seen), in bursts that spoil three
//!    trials in a row, so a median of five was too few.

#![cfg(all(windows, target_pointer_width = "64"))]

use gp64_memory::arena::{ArenaStore, ARENA_PHYS_START};
use gp64_memory::{Memory, PRIVATE_WINDOW_BYTES};
use std::sync::{Mutex, MutexGuard};

static ARENA_COMMIT_TEST_LOCK: Mutex<()> = Mutex::new(());

fn arena_commit_test_guard() -> MutexGuard<'static, ()> {
    ARENA_COMMIT_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCountersEx {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
}

#[repr(C)]
#[derive(Default)]
struct PerformanceInformation {
    cb: u32,
    commit_total: usize,
    commit_limit: usize,
    commit_peak: usize,
    physical_total: usize,
    physical_available: usize,
    system_cache: usize,
    kernel_total: usize,
    kernel_paged: usize,
    kernel_nonpaged: usize,
    page_size: usize,
    handle_count: u32,
    process_count: u32,
    thread_count: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut core::ffi::c_void;
    fn K32GetProcessMemoryInfo(
        process: *mut core::ffi::c_void,
        counters: *mut ProcessMemoryCountersEx,
        cb: u32,
    ) -> i32;
    fn K32GetPerformanceInfo(info: *mut PerformanceInformation, cb: u32) -> i32;
}

/// This process's private commit, in bytes.
fn process_private_commit() -> usize {
    let mut counters = ProcessMemoryCountersEx {
        cb: size_of::<ProcessMemoryCountersEx>() as u32,
        ..Default::default()
    };
    // SAFETY: a correctly sized PROCESS_MEMORY_COUNTERS_EX for this process.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    assert_ne!(
        ok,
        0,
        "GetProcessMemoryInfo: {}",
        std::io::Error::last_os_error()
    );
    counters.private_usage
}

/// The system commit charge, in bytes.
fn system_commit_total() -> usize {
    let mut info = PerformanceInformation {
        cb: size_of::<PerformanceInformation>() as u32,
        ..Default::default()
    };
    // SAFETY: a correctly sized PERFORMANCE_INFORMATION.
    let ok = unsafe { K32GetPerformanceInfo(&mut info, info.cb) };
    assert_ne!(
        ok,
        0,
        "GetPerformanceInfo: {}",
        std::io::Error::last_os_error()
    );
    info.commit_total * info.page_size
}

const PROCESSES: usize = 6;
const MIB: usize = 1024 * 1024;

fn expanded_memory() -> Memory {
    let mut mem = Memory::new();
    mem.expand_ram(128 * MIB);
    mem
}

#[test]
fn attaching_six_processes_adds_no_gap_commit() {
    let _serial = arena_commit_test_guard();
    // The gap one committed-gap design would charge per process (HLD D3).
    let gap_bytes = ARENA_PHYS_START - 128 * MIB;
    assert!(gap_bytes > 1024 * MIB);
    assert_eq!(ARENA_PHYS_START, PRIVATE_WINDOW_BYTES);

    let (store, token) = ArenaStore::new().expect("session section");
    let mut processes: Vec<Memory> = (0..PROCESSES).map(|_| expanded_memory()).collect();
    let before = process_private_commit();
    for mem in &mut processes {
        mem.attach_arena_store(store.clone()).expect("attach");
    }
    processes[0].give_arena_admission(token);
    let attached = process_private_commit();
    let per_process = attached.saturating_sub(before) / PROCESSES;
    eprintln!(
        "private commit: before {} MiB, after attaching {PROCESSES}: {} MiB ({} KiB per process)",
        before / MIB,
        attached / MIB,
        per_process / 1024
    );
    // The page-table part (1 MiB) is shared section commit, not private, and
    // the per-process bookkeeping (watch bitmaps, generation table, exec
    // pages) is a few MiB at most. A committed gap would be ~2 GiB.
    assert!(
        per_process < 32 * MIB,
        "attaching charged {per_process} bytes of private commit per process \
         (a committed gap is {gap_bytes})"
    );
}

#[test]
fn arena_payload_commit_is_charged_once_across_six_views() {
    let _serial = arena_commit_test_guard();
    const PAYLOAD: usize = 512 * MIB;
    const TRIALS: usize = 15;
    let mut ratios = Vec::new();
    let mut private_deltas = Vec::new();
    for _ in 0..TRIALS {
        let (store, token) = ArenaStore::new().expect("session section");
        let mut processes: Vec<Memory> = (0..PROCESSES).map(|_| expanded_memory()).collect();
        for mem in &mut processes {
            mem.attach_arena_store(store.clone()).expect("attach");
        }
        processes[0].give_arena_admission(token);
        let private_before = process_private_commit();
        let before = system_commit_total();
        processes[0]
            .commit_arena_host_range(ARENA_PHYS_START..ARENA_PHYS_START + PAYLOAD)
            .expect("commit payload");
        let after = system_commit_total();
        private_deltas.push(process_private_commit().saturating_sub(private_before));
        ratios.push((after as f64 - before as f64) / PAYLOAD as f64);

        // Every view sees the pages committed, and they are shared: a byte
        // stored through one process reads back through each of the others.
        let last = PAYLOAD - 1;
        processes[0].ram[ARENA_PHYS_START + last] = 0x5C;
        for i in 1..PROCESSES {
            let token = processes[i - 1].take_arena_admission().expect("token");
            processes[i].give_arena_admission(token);
            assert_eq!(processes[i].ram[ARENA_PHYS_START + last], 0x5C);
        }
        drop(processes);
    }
    ratios.sort_by(f64::total_cmp);
    let median = ratios[ratios.len() / 2];
    eprintln!(
        "system CommitTotal delta / payload over {TRIALS} trials: {ratios:?}; \
         process private commit deltas (bytes): {private_deltas:?}"
    );
    assert!(
        (0.75..1.5).contains(&median),
        "committing {PAYLOAD} bytes with {PROCESSES} views attached charged {median:.2}x \
         the payload (once is 1x, per view would be {PROCESSES}x)"
    );
}
