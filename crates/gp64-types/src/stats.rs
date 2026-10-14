//! Statistics collection for the debug monitor
//!
//! Provides low-overhead instrumentation for tracking emulator performance,
//! instruction execution patterns, memory access, and hardware activity.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Lightweight stats collector for hot-path instrumentation.
///
/// All methods use relaxed atomic ordering for maximum performance.
/// The collector is designed to have near-zero overhead when inactive.
pub struct StatsCollector {
    /// Whether detailed monitoring is active (opcode counts, memory tracking, etc.)
    pub active: AtomicBool,
    /// Total instruction count (always collected for basic MIPS calculation)
    pub instruction_count: AtomicU64,
    /// Per-opcode execution counts (256 primary opcodes)
    pub opcode_counts: [AtomicU64; 256],
    /// Per-opcode cycle estimates (sum of estimated cycles)
    pub opcode_cycle_counts: [AtomicU64; 256],
    /// Memory access sampling counter (for 1-in-N sampling)
    memory_sample_counter: AtomicU64,
    /// Memory read counts per 1KB region (1024 regions for 1MB)
    pub memory_read_counts: [AtomicU64; 1024],
    /// Memory write counts per 1KB region
    pub memory_write_counts: [AtomicU64; 1024],
    /// Interrupt vector call counts (256 vectors)
    pub interrupt_counts: [AtomicU64; 256],
    /// DOS function (AH) counts for INT 21h
    pub dos_ah_counts: [AtomicU64; 256],
    /// I/O port read counts (first 1024 ports)
    pub io_read_counts: [AtomicU64; 1024],
    /// I/O port write counts (first 1024 ports)
    pub io_write_counts: [AtomicU64; 1024],
}

impl StatsCollector {
    /// Create a new stats collector (inactive by default)
    pub fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            instruction_count: AtomicU64::new(0),
            opcode_counts: std::array::from_fn(|_| AtomicU64::new(0)),
            opcode_cycle_counts: std::array::from_fn(|_| AtomicU64::new(0)),
            memory_sample_counter: AtomicU64::new(0),
            memory_read_counts: std::array::from_fn(|_| AtomicU64::new(0)),
            memory_write_counts: std::array::from_fn(|_| AtomicU64::new(0)),
            interrupt_counts: std::array::from_fn(|_| AtomicU64::new(0)),
            dos_ah_counts: std::array::from_fn(|_| AtomicU64::new(0)),
            io_read_counts: std::array::from_fn(|_| AtomicU64::new(0)),
            io_write_counts: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Enable or disable detailed stats collection
    #[inline]
    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    /// Check if detailed stats collection is active
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Record an instruction execution.
    ///
    /// This is called for EVERY instruction, so it must be extremely fast.
    /// Basic instruction count is always collected; opcode counts only when active.
    #[allow(clippy::inline_always)]
    #[inline(always)]
    pub fn record_instruction(&self, opcode: u8) {
        // Fast path: always count total instructions
        self.instruction_count.fetch_add(1, Ordering::Relaxed);

        // Slow path: detailed opcode stats only if monitoring active
        if self.active.load(Ordering::Relaxed) {
            self.opcode_counts[opcode as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a memory access (sampled to reduce overhead).
    ///
    /// Only 1 in 256 accesses are actually recorded to minimize performance impact.
    #[allow(clippy::inline_always)]
    #[inline(always)]
    pub fn record_memory_access(&self, linear_addr: usize, is_write: bool) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }

        // Sample 1 in 256 accesses
        let count = self.memory_sample_counter.fetch_add(1, Ordering::Relaxed);
        if count.trailing_zeros() >= 8 && linear_addr < (1 << 20) {
            let region = linear_addr >> 10;
            if is_write {
                self.memory_write_counts[region].fetch_add(1, Ordering::Relaxed);
            } else {
                self.memory_read_counts[region].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Record an interrupt execution
    #[allow(clippy::inline_always)]
    #[inline(always)]
    pub fn record_interrupt(&self, vector: u8) {
        if self.active.load(Ordering::Relaxed) {
            self.interrupt_counts[vector as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a DOS function call (INT 21h with specific AH value)
    #[allow(clippy::inline_always)]
    #[inline(always)]
    pub fn record_dos_call(&self, ah: u8) {
        if self.active.load(Ordering::Relaxed) {
            self.dos_ah_counts[ah as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record an I/O port access
    #[allow(clippy::inline_always)]
    #[inline(always)]
    pub fn record_io_access(&self, port: u16, is_write: bool) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }

        // Only track first 1024 ports
        if port < 1024 {
            if is_write {
                self.io_write_counts[port as usize].fetch_add(1, Ordering::Relaxed);
            } else {
                self.io_read_counts[port as usize].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Add `count` retired guest instructions.
    ///
    /// A plain load and store, not a locked read-modify-write: the collector
    /// is owned by one emulator, whose thread is the only writer, and every
    /// reader of this counter runs on that thread or only needs a recent
    /// value. Avoiding a locked read-modify-write reduces counter overhead.
    #[inline]
    pub fn add_instructions(&self, count: u64) {
        let total = self.instruction_count.load(Ordering::Relaxed);
        self.instruction_count
            .store(total.wrapping_add(count), Ordering::Relaxed);
    }

    /// Get the current instruction count
    #[inline]
    pub fn get_instruction_count(&self) -> u64 {
        self.instruction_count.load(Ordering::Relaxed)
    }

    /// Reset all counters
    pub fn reset(&self) {
        self.instruction_count.store(0, Ordering::Relaxed);
        for i in 0..256 {
            self.opcode_counts[i].store(0, Ordering::Relaxed);
            self.opcode_cycle_counts[i].store(0, Ordering::Relaxed);
            self.interrupt_counts[i].store(0, Ordering::Relaxed);
            self.dos_ah_counts[i].store(0, Ordering::Relaxed);
        }
        for i in 0..1024 {
            self.memory_read_counts[i].store(0, Ordering::Relaxed);
            self.memory_write_counts[i].store(0, Ordering::Relaxed);
            self.io_read_counts[i].store(0, Ordering::Relaxed);
            self.io_write_counts[i].store(0, Ordering::Relaxed);
        }
        self.memory_sample_counter.store(0, Ordering::Relaxed);
    }

    /// Take a snapshot of opcode counts
    pub fn snapshot_opcode_counts(&self) -> [u64; 256] {
        let mut counts = [0u64; 256];
        #[allow(clippy::needless_range_loop)]
        for i in 0..256 {
            counts[i] = self.opcode_counts[i].load(Ordering::Relaxed);
        }
        counts
    }

    /// Take a snapshot of opcode cycle totals
    pub fn snapshot_opcode_cycle_counts(&self) -> [u64; 256] {
        let mut counts = [0u64; 256];
        #[allow(clippy::needless_range_loop)]
        for i in 0..256 {
            counts[i] = self.opcode_cycle_counts[i].load(Ordering::Relaxed);
        }
        counts
    }

    /// Take a snapshot of memory access counts
    pub fn snapshot_memory_counts(&self) -> ([u64; 1024], [u64; 1024]) {
        let mut reads = [0u64; 1024];
        let mut writes = [0u64; 1024];
        #[allow(clippy::needless_range_loop)]
        for i in 0..1024 {
            reads[i] = self.memory_read_counts[i].load(Ordering::Relaxed);
            writes[i] = self.memory_write_counts[i].load(Ordering::Relaxed);
        }
        (reads, writes)
    }

    /// Take a snapshot of interrupt counts
    pub fn snapshot_interrupt_counts(&self) -> [u64; 256] {
        let mut counts = [0u64; 256];
        #[allow(clippy::needless_range_loop)]
        for i in 0..256 {
            counts[i] = self.interrupt_counts[i].load(Ordering::Relaxed);
        }
        counts
    }

    /// Take a snapshot of DOS call counts
    pub fn snapshot_dos_counts(&self) -> [u64; 256] {
        let mut counts = [0u64; 256];
        #[allow(clippy::needless_range_loop)]
        for i in 0..256 {
            counts[i] = self.dos_ah_counts[i].load(Ordering::Relaxed);
        }
        counts
    }

    /// Take a snapshot of I/O port counts
    pub fn snapshot_io_counts(&self) -> ([u64; 1024], [u64; 1024]) {
        let mut reads = [0u64; 1024];
        let mut writes = [0u64; 1024];
        for i in 0..1024 {
            reads[i] = self.io_read_counts[i].load(Ordering::Relaxed);
            writes[i] = self.io_write_counts[i].load(Ordering::Relaxed);
        }
        (reads, writes)
    }
}

impl Default for StatsCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Reference CPU performance data for comparison
#[derive(Debug, Clone, Copy)]
pub struct ReferenceCpu {
    pub name: &'static str,
    pub clock_mhz: f64,
    pub mips: f64,
}

/// Historical reference CPUs for performance comparison
pub const REFERENCE_CPUS: &[ReferenceCpu] = &[
    ReferenceCpu {
        name: "8086 4.77MHz",
        clock_mhz: 4.77,
        mips: 0.33,
    },
    ReferenceCpu {
        name: "80286 12MHz",
        clock_mhz: 12.0,
        mips: 1.5,
    },
    ReferenceCpu {
        name: "80386DX-16",
        clock_mhz: 16.0,
        mips: 2.5,
    },
    ReferenceCpu {
        name: "80386DX-33",
        clock_mhz: 33.0,
        mips: 5.0,
    },
    ReferenceCpu {
        name: "80486DX-25",
        clock_mhz: 25.0,
        mips: 8.0,
    },
    ReferenceCpu {
        name: "80486DX-50",
        clock_mhz: 50.0,
        mips: 20.0,
    },
    ReferenceCpu {
        name: "Pentium-60",
        clock_mhz: 60.0,
        mips: 50.0,
    },
];

/// Aggregated/computed statistics for display
#[derive(Debug, Clone)]
pub struct EmulatorStats {
    /// Snapshot timestamp
    pub timestamp: Instant,
    /// Instructions per second
    pub ips: f64,
    /// Million instructions per second
    pub mips: f64,
    /// JIT execution percentage (0-100)
    pub jit_percentage: f64,
    /// Interpreter execution percentage (0-100)
    pub interp_percentage: f64,
    /// JIT blocks compiled
    pub jit_blocks: u64,
    /// JIT cache size in bytes
    pub jit_cache_bytes: usize,
    /// Top opcodes by frequency: (opcode, count, percentage)
    pub top_opcodes: Vec<(u8, u64, f64)>,
    /// Opcode counts snapshot
    pub opcode_counts: [u64; 256],
    /// Memory read counts per 1KB region
    pub memory_reads: [u64; 1024],
    /// Memory write counts per 1KB region
    pub memory_writes: [u64; 1024],
    /// Interrupt counts
    pub interrupt_counts: [u64; 256],
    /// DOS function counts
    pub dos_counts: [u64; 256],
    /// I/O read counts
    pub io_reads: [u32; 1024],
    /// I/O write counts
    pub io_writes: [u32; 1024],
    /// Frame time breakdown
    pub frame_time_cpu_ms: f64,
    pub frame_time_vga_ms: f64,
    pub frame_time_audio_ms: f64,
    pub frame_time_total_ms: f64,
}

impl Default for EmulatorStats {
    fn default() -> Self {
        Self {
            timestamp: Instant::now(),
            ips: 0.0,
            mips: 0.0,
            jit_percentage: 0.0,
            interp_percentage: 0.0,
            jit_blocks: 0,
            jit_cache_bytes: 0,
            top_opcodes: Vec::new(),
            opcode_counts: [0; 256],
            memory_reads: [0; 1024],
            memory_writes: [0; 1024],
            interrupt_counts: [0; 256],
            dos_counts: [0; 256],
            io_reads: [0; 1024],
            io_writes: [0; 1024],
            frame_time_cpu_ms: 0.0,
            frame_time_vga_ms: 0.0,
            frame_time_audio_ms: 0.0,
            frame_time_total_ms: 0.0,
        }
    }
}

impl EmulatorStats {
    /// Calculate percentage relative to a reference CPU
    pub fn percentage_of_reference(&self, ref_cpu: &ReferenceCpu) -> f64 {
        if ref_cpu.mips > 0.0 {
            (self.mips / ref_cpu.mips) * 100.0
        } else {
            0.0
        }
    }

    /// Compute top N opcodes by frequency
    pub fn compute_top_opcodes(&mut self, n: usize) {
        let total: u64 = self.opcode_counts.iter().sum();
        if total == 0 {
            self.top_opcodes.clear();
            return;
        }

        let mut indexed: Vec<(u8, u64)> = self
            .opcode_counts
            .iter()
            .enumerate()
            .map(|(i, &count)| (i as u8, count))
            .filter(|(_, count)| *count > 0)
            .collect();

        indexed.sort_by_key(|b| std::cmp::Reverse(b.1));
        indexed.truncate(n);

        self.top_opcodes = indexed
            .into_iter()
            .map(|(opcode, count)| {
                let percentage = (count as f64 / total as f64) * 100.0;
                (opcode, count, percentage)
            })
            .collect();
    }
}

/// Opcode name lookup table
#[allow(clippy::match_same_arms)]
pub fn opcode_name(opcode: u8) -> &'static str {
    match opcode {
        0x00..=0x05 => "ADD",
        0x06 => "PUSH ES",
        0x07 => "POP ES",
        0x08..=0x0D => "OR",
        0x0E => "PUSH CS",
        0x0F => "Two-byte",
        0x10..=0x15 => "ADC",
        0x16 => "PUSH SS",
        0x17 => "POP SS",
        0x18..=0x1D => "SBB",
        0x1E => "PUSH DS",
        0x1F => "POP DS",
        0x20..=0x25 => "AND",
        0x26 => "ES:",
        0x27 => "DAA",
        0x28..=0x2D => "SUB",
        0x2E => "CS:",
        0x2F => "DAS",
        0x30..=0x35 => "XOR",
        0x36 => "SS:",
        0x37 => "AAA",
        0x38..=0x3D => "CMP",
        0x3E => "DS:",
        0x3F => "AAS",
        0x40..=0x47 => "INC",
        0x48..=0x4F => "DEC",
        0x50..=0x57 => "PUSH",
        0x58..=0x5F => "POP",
        0x60 => "PUSHA",
        0x61 => "POPA",
        0x62 => "BOUND",
        0x63 => "ARPL",
        0x64 => "FS:",
        0x65 => "GS:",
        0x66 => "OPSIZE",
        0x67 => "ADSIZE",
        0x68 => "PUSH imm",
        0x69 => "IMUL",
        0x6A => "PUSH imm8",
        0x6B => "IMUL",
        0x6C..=0x6F => "INS/OUTS",
        0x70..=0x7F => "Jcc",
        0x80..=0x83 => "ALU r/m,imm",
        0x84..=0x85 => "TEST",
        0x86..=0x87 => "XCHG",
        0x88..=0x8B => "MOV",
        0x8C => "MOV seg",
        0x8D => "LEA",
        0x8E => "MOV seg",
        0x8F => "POP",
        0x90 => "NOP",
        0x91..=0x97 => "XCHG",
        0x98 => "CBW",
        0x99 => "CWD",
        0x9A => "CALL far",
        0x9B => "WAIT",
        0x9C => "PUSHF",
        0x9D => "POPF",
        0x9E => "SAHF",
        0x9F => "LAHF",
        0xA0..=0xA3 => "MOV moffs",
        0xA4..=0xA5 => "MOVS",
        0xA6..=0xA7 => "CMPS",
        0xA8..=0xA9 => "TEST",
        0xAA..=0xAB => "STOS",
        0xAC..=0xAD => "LODS",
        0xAE..=0xAF => "SCAS",
        0xB0..=0xBF => "MOV r,imm",
        0xC0..=0xC1 => "Shift",
        0xC2 => "RET imm",
        0xC3 => "RET",
        0xC4 => "LES",
        0xC5 => "LDS",
        0xC6..=0xC7 => "MOV",
        0xC8 => "ENTER",
        0xC9 => "LEAVE",
        0xCA => "RETF imm",
        0xCB => "RETF",
        0xCC => "INT 3",
        0xCD => "INT",
        0xCE => "INTO",
        0xCF => "IRET",
        0xD0..=0xD3 => "Shift",
        0xD4 => "AAM",
        0xD5 => "AAD",
        0xD6 => "SALC",
        0xD7 => "XLAT",
        0xD8..=0xDF => "FPU",
        0xE0 => "LOOPNZ",
        0xE1 => "LOOPZ",
        0xE2 => "LOOP",
        0xE3 => "JCXZ",
        0xE4..=0xE5 => "IN",
        0xE6..=0xE7 => "OUT",
        0xE8 => "CALL",
        0xE9 => "JMP",
        0xEA => "JMP far",
        0xEB => "JMP short",
        0xEC..=0xED => "IN",
        0xEE..=0xEF => "OUT",
        0xF0 => "LOCK",
        0xF1 => "INT1",
        0xF2 => "REPNZ",
        0xF3 => "REP",
        0xF4 => "HLT",
        0xF5 => "CMC",
        0xF6..=0xF7 => "Grp3",
        0xF8 => "CLC",
        0xF9 => "STC",
        0xFA => "CLI",
        0xFB => "STI",
        0xFC => "CLD",
        0xFD => "STD",
        0xFE..=0xFF => "Grp4/5",
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_stats_collector_new() {
        let collector = StatsCollector::new();
        assert!(!collector.is_active());
        assert_eq!(collector.get_instruction_count(), 0);
    }

    /// MDD-BUG-CRUCIBLE-00667: an I/O port counter must not wrap at 2^32.
    ///
    /// Seeded one below `u32::MAX` and advanced twice, the counter has to reach
    /// `u32::MAX + 1`. As an `AtomicU32` with a wrapping `fetch_add` the second
    /// increment reported 0 instead, so a long diagnostic run could show a hot
    /// port with a tiny count — or rank it below a colder one.
    #[test]
    fn test_io_counters_do_not_wrap_past_u32_max() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        const PORT: u16 = 0x3DA;
        let seed = u64::from(u32::MAX) - 1;
        collector.io_read_counts[PORT as usize].store(seed, Ordering::Relaxed);
        collector.io_write_counts[PORT as usize].store(seed, Ordering::Relaxed);

        collector.record_io_access(PORT, false);
        collector.record_io_access(PORT, false);
        collector.record_io_access(PORT, true);
        collector.record_io_access(PORT, true);

        let (reads, writes) = collector.snapshot_io_counts();
        let expected = u64::from(u32::MAX) + 1;
        assert_eq!(
            reads[PORT as usize], expected,
            "read counter wrapped at 32 bits"
        );
        assert_eq!(
            writes[PORT as usize], expected,
            "write counter wrapped at 32 bits"
        );
        assert!(
            reads[PORT as usize] > u64::from(u32::MAX),
            "the reported value must exceed what a u32 can hold"
        );
    }

    /// Widening must not disturb the ordinary path: counting from zero, leaving
    /// other ports alone, ignoring ports past the tracked window, and resetting.
    #[test]
    fn test_io_counters_still_count_normally_after_widening() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        collector.record_io_access(0x60, false);
        collector.record_io_access(0x60, false);
        collector.record_io_access(0x60, true);
        // Outside the 1024-port window: must be ignored, not panic.
        collector.record_io_access(0x8000, false);

        let (reads, writes) = collector.snapshot_io_counts();
        assert_eq!(reads[0x60], 2);
        assert_eq!(writes[0x60], 1);
        assert_eq!(reads[0x61], 0, "an unrelated port must stay at zero");

        collector.reset();
        let (reads, writes) = collector.snapshot_io_counts();
        assert_eq!(reads[0x60], 0, "reset must clear the widened counters");
        assert_eq!(writes[0x60], 0);
    }

    #[test]
    fn test_stats_collector_record_instruction() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        collector.record_instruction(0x90); // NOP
        collector.record_instruction(0x90);
        collector.record_instruction(0xEB); // JMP short

        assert_eq!(collector.get_instruction_count(), 3);
        assert_eq!(collector.opcode_counts[0x90].load(Ordering::Relaxed), 2);
        assert_eq!(collector.opcode_counts[0xEB].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_stats_collector_inactive() {
        let collector = StatsCollector::new();
        // Not active, so opcode counts should not be updated
        collector.record_instruction(0x90);
        collector.record_instruction(0x90);

        // Instruction count is always updated
        assert_eq!(collector.get_instruction_count(), 2);
        // But opcode counts should be zero when inactive
        assert_eq!(collector.opcode_counts[0x90].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_stats_collector_default_matches_new() {
        let collector = StatsCollector::default();
        assert_eq!(collector.get_instruction_count(), 0);
        assert!(!collector.active.load(Ordering::Relaxed));
    }

    #[test]
    fn test_stats_collector_io_access_inactive_no_counts() {
        let collector = StatsCollector::new();
        collector.record_io_access(0x20, false);
        collector.record_io_access(0x20, true);

        assert_eq!(collector.io_read_counts[0x20].load(Ordering::Relaxed), 0);
        assert_eq!(collector.io_write_counts[0x20].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_stats_collector_memory_sampling() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        // Record 512 accesses - should record approximately 2 samples
        for i in 0..512 {
            collector.record_memory_access(0x1000 + i, false);
        }

        // Region 4 (0x1000 >> 10 = 4) should have some samples
        let reads = collector.memory_read_counts[4].load(Ordering::Relaxed);
        assert!(reads > 0);
        assert!(reads <= 3);
    }

    #[test]
    fn test_memory_counters_do_not_wrap_past_u32_max() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        const REGION: usize = 4;
        const ADDRESS: usize = REGION << 10;
        const SEED: u64 = 4_294_967_294;

        collector.memory_read_counts[REGION].store(SEED, Ordering::Relaxed);
        collector.memory_write_counts[REGION].store(SEED, Ordering::Relaxed);

        for is_write in [false, false, true, true] {
            collector
                .memory_sample_counter
                .store(0x100, Ordering::Relaxed);
            collector.record_memory_access(ADDRESS, is_write);
        }

        let (reads, writes) = collector.snapshot_memory_counts();
        let expected = u64::from(u32::MAX) + 1;
        assert_eq!(reads[REGION], expected, "read counter wrapped at 32 bits");
        assert_eq!(writes[REGION], expected, "write counter wrapped at 32 bits");
        assert!(reads[REGION] > u64::from(u32::MAX));
        assert!(writes[REGION] > u64::from(u32::MAX));
    }

    #[test]
    fn test_stats_collector_interrupt() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        collector.record_interrupt(0x21); // DOS
        collector.record_interrupt(0x21);
        collector.record_interrupt(0x10); // Video

        assert_eq!(collector.interrupt_counts[0x21].load(Ordering::Relaxed), 2);
        assert_eq!(collector.interrupt_counts[0x10].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_stats_collector_interrupt_inactive_no_counts() {
        let collector = StatsCollector::new();

        collector.record_interrupt(0x21);
        assert_eq!(collector.interrupt_counts[0x21].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_stats_collector_dos_call() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        collector.record_dos_call(0x3F); // Read file
        collector.record_dos_call(0x40); // Write file
        collector.record_dos_call(0x3F);

        assert_eq!(collector.dos_ah_counts[0x3F].load(Ordering::Relaxed), 2);
        assert_eq!(collector.dos_ah_counts[0x40].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_stats_collector_dos_call_inactive_no_counts() {
        let collector = StatsCollector::new();

        collector.record_dos_call(0x3F);
        assert_eq!(collector.dos_ah_counts[0x3F].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_stats_collector_io_access() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        collector.record_io_access(0x3D4, true); // VGA CRTC
        collector.record_io_access(0x3D5, false); // VGA CRTC data

        assert_eq!(collector.io_write_counts[0x3D4].load(Ordering::Relaxed), 1);
        assert_eq!(collector.io_read_counts[0x3D5].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_stats_collector_io_access_max_port() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        collector.record_io_access(1023, false);
        collector.record_io_access(1023, true);

        assert_eq!(collector.io_read_counts[1023].load(Ordering::Relaxed), 1);
        assert_eq!(collector.io_write_counts[1023].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_stats_collector_reset() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        collector.record_instruction(0x90);
        collector.record_interrupt(0x21);

        collector.reset();

        assert_eq!(collector.get_instruction_count(), 0);
        assert_eq!(collector.opcode_counts[0x90].load(Ordering::Relaxed), 0);
        assert_eq!(collector.interrupt_counts[0x21].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_stats_collector_reset_clears_memory_sample_counter() {
        let collector = StatsCollector::new();
        collector
            .memory_sample_counter
            .store(0x1234, Ordering::Relaxed);

        collector.reset();
        assert_eq!(collector.memory_sample_counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_emulator_stats_reference_comparison() {
        let stats = EmulatorStats {
            mips: 5.0, // Same as 386DX-33
            ..Default::default()
        };

        let ref_386 = &REFERENCE_CPUS[3]; // 386DX-33 at 5.0 MIPS
        assert!((stats.percentage_of_reference(ref_386) - 100.0).abs() < 0.01);

        let ref_8086 = &REFERENCE_CPUS[0]; // 8086 at 0.33 MIPS
        let pct = stats.percentage_of_reference(ref_8086);
        assert!(pct > 1500.0); // Should be about 1515%
    }

    #[test]
    fn test_emulator_stats_top_opcodes() {
        let mut stats = EmulatorStats::default();
        stats.opcode_counts[0x90] = 100; // NOP
        stats.opcode_counts[0xEB] = 50; // JMP short
        stats.opcode_counts[0xE8] = 25; // CALL

        stats.compute_top_opcodes(3);

        assert_eq!(stats.top_opcodes.len(), 3);
        assert_eq!(stats.top_opcodes[0].0, 0x90);
        assert_eq!(stats.top_opcodes[1].0, 0xEB);
        assert_eq!(stats.top_opcodes[2].0, 0xE8);
    }

    #[test]
    fn test_emulator_stats_top_opcodes_zero_limit_clears() {
        let mut stats = EmulatorStats::default();
        stats.opcode_counts[0x90] = 10;
        stats.compute_top_opcodes(0);
        assert!(stats.top_opcodes.is_empty());
    }

    #[test]
    fn test_opcode_name() {
        assert_eq!(opcode_name(0x90), "NOP");
        assert_eq!(opcode_name(0xEB), "JMP short");
        assert_eq!(opcode_name(0xCD), "INT");
        assert_eq!(opcode_name(0x50), "PUSH");
    }

    #[test]
    fn test_opcode_name_all_opcodes_have_names() {
        for opcode in 0u8..=u8::MAX {
            assert!(!opcode_name(opcode).is_empty());
        }
    }

    #[test]
    fn test_stats_collector_memory_access_inactive_no_sample() {
        let collector = StatsCollector::new();
        collector.record_memory_access(0x1000, false);
        assert_eq!(collector.memory_read_counts[4].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_stats_collector_memory_access_samples_first() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        collector.record_memory_access(0x2000, true);
        assert_eq!(collector.memory_write_counts[8].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_stats_collector_memory_access_reads_increment() {
        let collector = StatsCollector::new();
        collector.set_active(true);
        collector
            .memory_sample_counter
            .store(0x100, Ordering::Relaxed);

        collector.record_memory_access(0x1800, false);
        assert_eq!(collector.memory_read_counts[6].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_stats_collector_memory_access_skips_when_not_sampled() {
        let collector = StatsCollector::new();
        collector.set_active(true);
        collector.memory_sample_counter.store(1, Ordering::Relaxed);

        collector.record_memory_access(0x1800, false);
        assert_eq!(collector.memory_read_counts[6].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_stats_collector_memory_access_ignores_above_1mb() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        collector.record_memory_access(0x100000, false);
        assert_eq!(collector.memory_read_counts[0].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_stats_collector_memory_access_max_region() {
        let collector = StatsCollector::new();
        collector.set_active(true);
        collector
            .memory_sample_counter
            .store(0x100, Ordering::Relaxed);

        collector.record_memory_access(0xFFFFF, true);
        assert_eq!(
            collector.memory_write_counts[1023].load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn test_stats_collector_io_access_ignores_high_ports() {
        let collector = StatsCollector::new();
        collector.set_active(true);

        collector.record_io_access(1024, true);
        assert_eq!(collector.io_write_counts[0].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_stats_collector_snapshots() {
        let collector = StatsCollector::new();
        collector.set_active(true);
        collector.record_instruction(0x90);
        collector.record_interrupt(0x21);
        collector.record_dos_call(0x3F);
        collector.record_io_access(0x20, false);

        let opcodes = collector.snapshot_opcode_counts();
        let interrupts = collector.snapshot_interrupt_counts();
        let dos = collector.snapshot_dos_counts();
        let (io_reads, _) = collector.snapshot_io_counts();

        assert_eq!(opcodes[0x90], 1);
        assert_eq!(interrupts[0x21], 1);
        assert_eq!(dos[0x3F], 1);
        assert_eq!(io_reads[0x20], 1);
    }

    #[test]
    fn test_emulator_stats_top_opcodes_empty() {
        let mut stats = EmulatorStats::default();
        stats.compute_top_opcodes(5);
        assert!(stats.top_opcodes.is_empty());
    }

    #[test]
    fn test_emulator_stats_reference_zero_mips() {
        let stats = EmulatorStats::default();
        let reference = ReferenceCpu {
            name: "Zero",
            clock_mhz: 0.0,
            mips: 0.0,
        };
        assert_eq!(stats.percentage_of_reference(&reference), 0.0);
    }

    #[test]
    fn test_snapshot_opcode_cycle_counts() {
        let collector = StatsCollector::new();
        collector.opcode_cycle_counts[0x90].store(12, Ordering::Relaxed);
        collector.opcode_cycle_counts[0xEB].store(7, Ordering::Relaxed);

        let counts = collector.snapshot_opcode_cycle_counts();
        assert_eq!(counts[0x90], 12);
        assert_eq!(counts[0xEB], 7);
    }

    #[test]
    fn test_snapshot_memory_counts() {
        let collector = StatsCollector::new();
        collector.memory_read_counts[4].store(3, Ordering::Relaxed);
        collector.memory_write_counts[9].store(2, Ordering::Relaxed);

        let (reads, writes) = collector.snapshot_memory_counts();
        assert_eq!(reads[4], 3);
        assert_eq!(writes[9], 2);
    }

    // ---- ReferenceCpu coverage ----

    #[test]
    fn test_reference_cpus_non_empty() {
        assert!(!REFERENCE_CPUS.is_empty());
        for cpu in REFERENCE_CPUS {
            assert!(!cpu.name.is_empty());
            assert!(cpu.clock_mhz > 0.0);
            assert!(cpu.mips > 0.0);
        }
    }

    #[test]
    fn test_reference_cpu_debug_clone() {
        let cpu = REFERENCE_CPUS[0];
        let dbg = format!("{cpu:?}");
        assert!(dbg.contains("8086"));
    }

    // ---- EmulatorStats default fields ----

    #[test]
    fn test_emulator_stats_default_fields() {
        let stats = EmulatorStats::default();
        assert_eq!(stats.ips, 0.0);
        assert_eq!(stats.mips, 0.0);
        assert_eq!(stats.jit_percentage, 0.0);
        assert_eq!(stats.interp_percentage, 0.0);
        assert_eq!(stats.jit_blocks, 0);
        assert_eq!(stats.jit_cache_bytes, 0);
        assert!(stats.top_opcodes.is_empty());
        assert_eq!(stats.frame_time_cpu_ms, 0.0);
        assert_eq!(stats.frame_time_vga_ms, 0.0);
        assert_eq!(stats.frame_time_audio_ms, 0.0);
        assert_eq!(stats.frame_time_total_ms, 0.0);
    }

    #[test]
    fn test_emulator_stats_debug() {
        let stats = EmulatorStats::default();
        let dbg = format!("{stats:?}");
        assert!(dbg.contains("EmulatorStats"));
    }

    // ---- Top opcodes with fewer entries than requested ----

    #[test]
    fn test_compute_top_opcodes_fewer_than_n() {
        let mut stats = EmulatorStats::default();
        stats.opcode_counts[0x90] = 100; // only 1 non-zero
        stats.compute_top_opcodes(10);
        assert_eq!(stats.top_opcodes.len(), 1);
        assert_eq!(stats.top_opcodes[0].0, 0x90);
        // Percentage should be 100%
        assert!((stats.top_opcodes[0].2 - 100.0).abs() < 0.01);
    }

    // ---- Opcode name boundary values ----

    #[test]
    fn test_opcode_name_boundaries() {
        // Test first and last of ranges
        assert_eq!(opcode_name(0x00), "ADD");
        assert_eq!(opcode_name(0x05), "ADD");
        assert_eq!(opcode_name(0x06), "PUSH ES");
        assert_eq!(opcode_name(0x0F), "Two-byte");
        assert_eq!(opcode_name(0x70), "Jcc");
        assert_eq!(opcode_name(0x7F), "Jcc");
        assert_eq!(opcode_name(0xB0), "MOV r,imm");
        assert_eq!(opcode_name(0xBF), "MOV r,imm");
        assert_eq!(opcode_name(0xD8), "FPU");
        assert_eq!(opcode_name(0xDF), "FPU");
        assert_eq!(opcode_name(0xFE), "Grp4/5");
        assert_eq!(opcode_name(0xFF), "Grp4/5");
    }

    // ---- Stats collector: set_active toggle ----

    #[test]
    fn test_stats_collector_toggle_active() {
        let collector = StatsCollector::new();
        assert!(!collector.is_active());
        collector.set_active(true);
        assert!(collector.is_active());
        collector.set_active(false);
        assert!(!collector.is_active());
    }
}
