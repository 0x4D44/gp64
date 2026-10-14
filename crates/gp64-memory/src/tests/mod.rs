use super::*;
mod arena_u3;
use std::sync::{Arc, Mutex};

use gp64_types::{MemoryAccess, PagedMemory, SmcNotify};

#[derive(Default)]
struct TestSmcNotify {
    writes: Mutex<Vec<(usize, usize)>>,
}

impl TestSmcNotify {
    fn snapshot(&self) -> Vec<(usize, usize)> {
        self.writes
            .lock()
            .map_or_else(|_| Vec::new(), |writes| writes.clone())
    }
}

fn assert_epoch(mem: &Memory, expected: u64) {
    assert_eq!(mem.memory_policy_epoch(), expected);
}

impl SmcNotify for TestSmcNotify {
    fn notify_write(&self, address: usize, size: usize) {
        if let Ok(mut writes) = self.writes.lock() {
            writes.push((address, size));
        }
    }

    fn notify_write_range(&self, start: usize, end: usize) {
        if let Ok(mut writes) = self.writes.lock() {
            writes.push((start, end.saturating_sub(start)));
        }
    }
}

#[derive(Default)]
struct AlwaysOnSubscriber;

impl tracing::Subscriber for AlwaysOnSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {}

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}

    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(tracing::metadata::LevelFilter::TRACE)
    }
}

fn with_trace_subscriber<F: FnOnce()>(f: F) {
    tracing::subscriber::with_default(AlwaysOnSubscriber, f);
}

mod chain4remap;
mod core_access_and_fast_paths;
mod coverage_edges;
mod misc_edges_and_lfb;
mod peek_video_range;
mod physical_decode_map;
mod physical_state_swap;
mod rom_regions_aliases_and_post_tracer;
mod slow_paths_vga_pipeline_and_edges;
mod tlb_and_vga_planar;
mod tlb_mappings_stats_and_gdt_snoop;
mod write_tracking;
mod x86_machine;

#[cfg(all(target_arch = "aarch64", target_os = "macos", debug_assertions))]
#[test]
fn memory_hot_layout() {
    use std::mem::{offset_of, size_of};
    let observed = [
        size_of::<Memory>(),
        offset_of!(Memory, ram),
        offset_of!(Memory, core) + MemoryCore::TLB_OFFSET,
        offset_of!(Memory, page_fault_pending),
        offset_of!(Memory, core) + MemoryCore::SMC_HAS_CODE_PAGES_OFFSET,
        offset_of!(Memory, a20_enabled),
        offset_of!(Memory, allow_rom_writes),
        offset_of!(Memory, hle_ems_frame_owned),
    ];
    println!("Memory hot layout: {observed:?}");
    println!(
        "MemoryCore size={} offset={}",
        size_of::<MemoryCore>(),
        offset_of!(Memory, core)
    );
    assert!(
        size_of::<Memory>() <= 2576 + 8,
        "Memory is {} B; the budget is 2584 B and SMC-3 left no slack. Map the hot cache \
         lines again before growing Memory or MemoryCore",
        size_of::<Memory>()
    );
    // The public fault flag stays on Memory in M0c, alongside PC policy.
    let pc_flags = [observed[3], observed[5], observed[6], observed[7]];
    let pc_line = pc_flags[0] / 64;
    // Memory is not repr(C): rustc's field-order heuristic places these
    // flags. `vga` is declared after the one-byte flags so that
    // allow_rom_writes stays on page_fault_pending's line (SMC-3 grew the
    // core by 8). This test is the only guard on that placement.
    assert!(
        pc_flags.iter().all(|offset| offset / 64 == pc_line),
        "PC hot flags split across cache lines {pc_flags:?}: Memory's field order \
         (vga declared after the one-byte flags) no longer keeps them on one line"
    );
    let smc_line = observed[4] / 64;
    if smc_line != pc_line {
        // Lead decision: a distant embedded core is permitted when keeping
        // the whole group together would exceed the eight-byte size budget.
        // Record the exception explicitly and assert that it is the core,
        // rather than a split PC tail, which separates the flags.
        assert!(offset_of!(Memory, core) + size_of::<MemoryCore>() <= pc_flags[0]);
        println!("hot-flags layout exception: SMC line={smc_line}, PC/fault line={pc_line}");
    } else {
        assert!(observed[3..].iter().all(|offset| offset / 64 == pc_line));
    }
    // 624 + the appended `raw_ram_limit` word (SMC-3); the hot flags stay put.
    assert_eq!(size_of::<MemoryCore>(), 632);
    assert_eq!(MemoryCore::RAW_RAM_LIMIT_OFFSET, 624);
    assert_eq!(size_of::<TlbEntry>(), 32);
    assert_eq!(std::mem::align_of::<TlbEntry>(), 32);
    assert_eq!(
        [
            offset_of!(TlbEntry, host_read),
            offset_of!(TlbEntry, host_write),
            offset_of!(TlbEntry, linear_page),
            offset_of!(TlbEntry, physical_page),
            offset_of!(TlbEntry, flags)
        ],
        [0, 8, 16, 20, 24]
    );
    assert_eq!(TLB_SIZE, 4096);
    assert_eq!(
        [
            flat_tlb::FLAT_VIEW_R0_BYTE_OFFSET,
            flat_tlb::FLAT_VIEW_W0_BYTE_OFFSET,
            flat_tlb::FLAT_VIEW_R3_BYTE_OFFSET,
            flat_tlb::FLAT_VIEW_W3_BYTE_OFFSET
        ],
        [0, 0x0080_0000, 0x0100_0000, 0x0180_0000]
    );
    assert_eq!(flat_tlb::FLAT_VIEW_WRITE_DELTA, 0x0080_0000);
    assert_eq!(flat_tlb::FLAT_VIEW_CPL3_DELTA, 0x0100_0000);
}
