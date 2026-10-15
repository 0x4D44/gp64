//! Memory subsystem for PC emulation
//!
//! Provides up to 32MB of RAM with:
//! - 1MB conventional memory (0x00000 - 0xFFFFF)
//! - High Memory Area (0x100000 - 0x10FFEF) via A20 gate
//! - Extended memory for protected-mode applications
//! - Software TLB for paging translation
//! - VGA planar/chain-4 memory and LFB support
//! - MMIO regions, write tracking, self-modifying code detection
//!
//! Memory map:
//! - 0x00000 - 0x003FF: Interrupt Vector Table (IVT)
//! - 0x00400 - 0x004FF: BIOS Data Area (BDA)
//! - 0x00500 - 0x9FFFF: Conventional memory (free for DOS/programs)

//! - 0xA0000 - 0xBFFFF: Video memory
//! - 0xC0000 - 0xEFFFF: ROM area (BIOS extensions)
//! - 0xF0000 - 0xFFFFF: System BIOS

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
// VGA plane loops use indices for bitwise operations
#![allow(clippy::needless_range_loop)]

pub mod arena;
pub mod arena_smc;
pub mod direct_proof;
mod ram_backing;
mod x86_machine;
pub use x86_machine::PcAccessPolicy;

// The machine-neutral core moved to `gp64-memory-core` in M1. These
// re-exports keep every engine path (`gp64_memory::TlbEntry`,
// `gp64_memory::flat_tlb::...`) unchanged.
pub use gp64_memory_core::flat_tlb;
pub use gp64_memory_core::{
    tlb_index, MemoryCore, SoftwareTlb, TlbEntry, TlbStats, TLB_FLAG_DIRTY, TLB_FLAG_GLOBAL,
    TLB_FLAG_USER, TLB_FLAG_WRITABLE, TLB_HASH_SHIFT, TLB_SIZE,
};
use gp64_memory_core::{
    BackingPage, DirectPage, LinearPage, PagePerms, PhysPage, RamBasePtr, TlbFill,
    INSTR_EXEC_PAGE_SHIFT, INSTR_EXEC_PAGE_SIZE, TLB_INVALID_TAG,
};
pub use ram_backing::{host_page_size, RamBacking, RamIndex, PRIVATE_WINDOW_BYTES};
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod flat_tlb_tests;
pub mod xms;

pub use direct_proof::{
    DirectPageProof, DirectProofAccess, DirectProofClass, DirectProofPaging,
    ProofUnavailableReason, PublishedDirectPageProof, AMBIGUOUS_PHYSICAL_PAGE_ID,
    DIRECT_PROOF_PAGE_SIZE, PROOF_PERM_READ, PROOF_PERM_USER, PROOF_PERM_WRITE,
    PROOF_WATCH_SOURCE_NOTIFIER,
};
pub use gp64_memory_core::{DirectGenerations, DirectGenerationsSnapshot};

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fmt;
use std::ops::{Deref, DerefMut, Range};
use std::ptr::NonNull;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ram_backing::FLAT_MEMORY_PHYS_BIAS;

use gp64_types::span_write_census;
use gp64_types::{FixedVec, IoCpuMode, MemoryAccess, PagedMemory, SmcNotify, VgaPipelineState};

use std::sync::{Arc, Mutex};

const INTEL_MP_FLOATING_POINTER_SIGNATURE: [u8; 4] = *b"_MP_";
const INTEL_MP_CONFIG_TABLE_SIGNATURE: [u8; 4] = *b"PCMP";
const INTEL_MP_SIGNATURE_LEN: usize = 4;
const INTEL_MP_FLOATING_POINTER_LEN: usize = 16;
const INTEL_MP_CONFIG_TABLE_HEADER_LEN: usize = 44;
const LOW_MEMORY_LIMIT: usize = 0x1_00000;
/// One past the last addressable 32-bit linear byte.
///
/// An exclusive end may equal this (a mapping finishing exactly at 4 GiB is
/// legal); anything larger has run off the address space.
const LINEAR_ADDRESS_SPACE_END: u64 = 0x1_0000_0000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FirmwareMpTableScrubResult {
    pub floating_pointer_signatures: usize,
    pub configuration_table_signatures: usize,
}

impl FirmwareMpTableScrubResult {
    #[inline]
    pub fn any(self) -> bool {
        self.floating_pointer_signatures != 0 || self.configuration_table_signatures != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VideoWriteChangeCounts {
    pub changed: u64,
    pub unchanged: u64,
}

impl VideoWriteChangeCounts {
    #[inline]
    pub fn delta_since(self, previous: Self) -> Self {
        Self {
            changed: self.changed.saturating_sub(previous.changed),
            unchanged: self.unchanged.saturating_sub(previous.unchanged),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoWriteChangeSample {
    pub source: Option<WriteContext>,
    pub offset: u32,
    pub plane: u8,
    pub old_value: u8,
    pub new_value: u8,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VideoWriteChangeSamples {
    pub dropped: u64,
    pub samples: Vec<VideoWriteChangeSample>,
}

#[derive(Debug, Default)]
struct VideoWriteChangeSampleBuffer {
    dropped: u64,
    samples: Vec<VideoWriteChangeSample>,
}

const VIDEO_WRITE_CHANGE_SAMPLE_LIMIT: usize = 1024;

#[cfg(test)]
#[inline]
fn vga_write_change_stats_enabled() -> bool {
    true
}

// env-gated VGA write-change stats (retired); off in production.
#[cfg(not(test))]
#[inline]
fn vga_write_change_stats_enabled() -> bool {
    false
}

#[cfg(test)]
#[inline]
fn vga_write_change_sample_enabled() -> bool {
    true
}

// env-gated VGA write-change sampling (retired); off in production.
#[cfg(not(test))]
#[inline]
fn vga_write_change_sample_enabled() -> bool {
    false
}

/// A physical-address decoder that claims addresses ahead of plain RAM
/// ([`Memory::physical_decoders`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalDecoder {
    /// A20 masked: every address with bit 20 set aliases the one without.
    A20Wrap,
    /// The top 64 KiB of the 4 GiB space folded onto the BIOS image.
    HighMemAlias,
    /// The Intel MP-table signature filter over the first megabyte.
    FirmwareMpFilter,
    /// The HLE EMS page frame.
    EmsFrame,
    /// A PCI expansion ROM.
    OptionRom,
    /// A write-protected ROM page.
    RomPage,
    /// The S3 memory-mapped register window.
    S3Mmio,
    /// The VGA memory window.
    VgaWindow,
    /// A linear framebuffer aperture (Dispi or S3).
    LinearFramebuffer,
    /// A registered device MMIO region.
    MmioRegion,
}

/// Interpreter watchpoint: packed CS:IP of current instruction (set by gp64-interp).
/// Format: (CS << 16) | (IP & 0xFFFF)
pub static INTERP_WATCHPOINT_CSIP: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: packed SI:DI of current instruction.
/// Format: (SI << 16) | (DI & 0xFFFF)
pub static INTERP_WATCHPOINT_SIDI: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: packed DS:ES of current instruction.
/// Format: (DS << 16) | (ES & 0xFFFF)
pub static INTERP_WATCHPOINT_DSES: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: full 32-bit EIP of current instruction.
pub static INTERP_WATCHPOINT_EIP: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: packed AX:BX of current instruction.
/// Format: (AX << 16) | (BX & 0xFFFF)
pub static INTERP_WATCHPOINT_AXBX: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: packed CX:DX of current instruction.
/// Format: (CX << 16) | (DX & 0xFFFF)
pub static INTERP_WATCHPOINT_CXDX: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: packed BP:SP of current instruction.
/// Format: (BP << 16) | (SP & 0xFFFF)
pub static INTERP_WATCHPOINT_BPSP: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: packed SS:FS of current instruction.
/// Format: (SS << 16) | (FS & 0xFFFF)
pub static INTERP_WATCHPOINT_SSFS: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: hidden CS base of current instruction.
pub static INTERP_WATCHPOINT_CS_BASE: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: hidden DS base of current instruction.
pub static INTERP_WATCHPOINT_DS_BASE: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: hidden ES base of current instruction.
pub static INTERP_WATCHPOINT_ES_BASE: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: hidden SS base of current instruction.
pub static INTERP_WATCHPOINT_SS_BASE: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: first four fetched bytes of current instruction.
pub static INTERP_WATCHPOINT_FETCH0: AtomicU32 = AtomicU32::new(0);
/// Interpreter watchpoint: next four fetched bytes of current instruction.
pub static INTERP_WATCHPOINT_FETCH1: AtomicU32 = AtomicU32::new(0);

pub static CURRENT_HLE_TAG: AtomicU32 = AtomicU32::new(0);

pub static CURRENT_HLE_CALLBACK_TAG: AtomicU32 = AtomicU32::new(0);

pub const HLE_DLL_KERNEL: u16 = 1;
pub const HLE_DLL_USER: u16 = 2;
pub const HLE_DLL_GDI: u16 = 3;
pub const HLE_DLL_WIN87EM: u16 = 4;
pub const HLE_DLL_KEYBOARD: u16 = 5;
pub const HLE_DLL_MOUSE: u16 = 6;
pub const HLE_DLL_MMSYSTEM: u16 = 7;
pub const HLE_DLL_SOUND: u16 = 8;
pub const HLE_DLL_SHELL: u16 = 9;
pub const HLE_DLL_TOOLHELP: u16 = 10;
pub const HLE_DLL_VER: u16 = 11;
pub const HLE_DLL_OLECLI: u16 = 12;
pub const HLE_DLL_OLESVR: u16 = 13;
pub const HLE_DLL_DDEML: u16 = 14;
pub const HLE_DLL_COMMCTRL: u16 = 15;
pub const HLE_DLL_MSVIDEO: u16 = 16;
pub const HLE_DLL_WING: u16 = 17;
pub const HLE_DLL_AVIFILE: u16 = 18;
pub const HLE_DLL_SCONFIG: u16 = 19;
pub const HLE_DLL_WINSOCK: u16 = 20;
pub const HLE_DLL_NDDEAPI: u16 = 22;
pub const HLE_DLL_COMMDLG: u16 = 23;

/// Build a packed `(dll, ordinal)` tag.
#[inline]
pub fn hle_tag(dll: u16, ordinal: u16) -> u32 {
    ((dll as u32) << 16) | (ordinal as u32)
}

/// Decode a packed HLE tag into `(dll, ordinal)`.
#[inline]
pub fn unpack_hle_tag(tag: u32) -> (u16, u16) {
    (((tag >> 16) & 0xFFFF) as u16, (tag & 0xFFFF) as u16)
}

/// Map a `16-bitSubsystem::dispatch_api_with_dosmem` module string to a DLL id.
/// Unknown modules return 0 (the writer will log the raw module string anyway).
#[inline]
pub fn hle_dll_id_for_module(module: &str) -> u16 {
    match module {
        "KERNEL" => HLE_DLL_KERNEL,
        "USER" => HLE_DLL_USER,
        "GDI" => HLE_DLL_GDI,
        "WIN87EM" => HLE_DLL_WIN87EM,
        "KEYBOARD" => HLE_DLL_KEYBOARD,
        "MOUSE" => HLE_DLL_MOUSE,
        "MMSYSTEM" => HLE_DLL_MMSYSTEM,
        "SOUND" => HLE_DLL_SOUND,
        "SHELL" => HLE_DLL_SHELL,
        "TOOLHELP" => HLE_DLL_TOOLHELP,
        "VER" => HLE_DLL_VER,
        "OLECLI" => HLE_DLL_OLECLI,
        "OLESVR" => HLE_DLL_OLESVR,
        "DDEML" => HLE_DLL_DDEML,
        "COMMCTRL" => HLE_DLL_COMMCTRL,
        "MSVIDEO" => HLE_DLL_MSVIDEO,
        "WING" => HLE_DLL_WING,
        "AVIFILE" => HLE_DLL_AVIFILE,
        "SCONFIG" => HLE_DLL_SCONFIG,
        "WINSOCK" => HLE_DLL_WINSOCK,
        "NDDEAPI" => HLE_DLL_NDDEAPI,
        "COMMDLG" => HLE_DLL_COMMDLG,
        _ => 0,
    }
}

/// Human-readable name for the DLL id, for log formatting.
#[inline]
pub fn hle_dll_name(id: u16) -> &'static str {
    match id {
        HLE_DLL_KERNEL => "KERNEL",
        HLE_DLL_USER => "USER",
        HLE_DLL_GDI => "GDI",
        HLE_DLL_WIN87EM => "WIN87EM",
        HLE_DLL_KEYBOARD => "KEYBOARD",
        HLE_DLL_MOUSE => "MOUSE",
        HLE_DLL_MMSYSTEM => "MMSYSTEM",
        HLE_DLL_SOUND => "SOUND",
        HLE_DLL_SHELL => "SHELL",
        HLE_DLL_TOOLHELP => "TOOLHELP",
        HLE_DLL_VER => "VER",
        HLE_DLL_OLECLI => "OLECLI",
        HLE_DLL_OLESVR => "OLESVR",
        HLE_DLL_DDEML => "DDEML",
        HLE_DLL_COMMCTRL => "COMMCTRL",
        HLE_DLL_MSVIDEO => "MSVIDEO",
        HLE_DLL_WING => "WING",
        HLE_DLL_AVIFILE => "AVIFILE",
        HLE_DLL_SCONFIG => "SCONFIG",
        HLE_DLL_WINSOCK => "WINSOCK",
        HLE_DLL_NDDEAPI => "NDDEAPI",
        HLE_DLL_COMMDLG => "COMMDLG",
        0 => "GUEST",
        _ => "?",
    }
}

/// RAII helper: set `CURRENT_HLE_TAG` to `(dll_id, ordinal)` on construction
/// and restore the previous value on drop. Safe to nest (16-bit HLE can re-enter
/// itself via callbacks; the previous tag is preserved in the local field and
/// restored when this guard drops).
pub struct HleTagGuard {
    prev: u32,
}

impl HleTagGuard {
    #[inline]
    pub fn new(dll_id: u16, ordinal: u16) -> Self {
        let new_tag = hle_tag(dll_id, ordinal);
        let prev = CURRENT_HLE_TAG.swap(new_tag, std::sync::atomic::Ordering::Relaxed);
        Self { prev }
    }
}

impl Drop for HleTagGuard {
    #[inline]
    fn drop(&mut self) {
        CURRENT_HLE_TAG.store(self.prev, std::sync::atomic::Ordering::Relaxed);
    }
}

// the write-trace range/exact/backtrace were env-configured (retired
// env vars); permanently absent/off.
#[inline]
fn traced_write_range() -> Option<(usize, usize)> {
    None
}

#[inline]
fn traced_write_exact() -> Option<usize> {
    None
}

#[inline]
fn traced_write_backtrace_enabled() -> bool {
    false
}

/// Parse an optional physical write range from a retired env var
/// (format: "start-end" hex). Unlike the retired linear write-range env var, this is
/// checked at the memory layer and catches HLE/DMA/raw writes as well.
// env-gated physical write-range trace (retired); permanently absent.
#[inline]
fn traced_phys_write_range() -> Option<(usize, usize)> {
    None
}

// VGA write trace context: published by the interpreter per-instruction
// so each VGA write trace record carries the CS:IP and cycle of the
// originating CPU instruction. The interp calls `set_vga_trace_ctx`
// each iteration; Memory's vga_trace_record reads it on each VGA write.
thread_local! {
    static VGA_TRACE_CTX: std::cell::Cell<(u64, u16, u32)> = const { std::cell::Cell::new((0, 0, 0)) };
}

#[cfg(test)]
thread_local! {
    static VGA_TRACE_TEST_RECORDS: RefCell<Option<Vec<[u8; 20]>>> = const { RefCell::new(None) };
}

#[cfg(test)]
#[must_use]
struct VgaTraceTestCaptureGuard;

#[cfg(test)]
impl Drop for VgaTraceTestCaptureGuard {
    fn drop(&mut self) {
        VGA_TRACE_TEST_RECORDS.with(|records| {
            *records.borrow_mut() = None;
        });
        VGA_TRACE_CTX.with(|c| c.set((0, 0, 0)));
    }
}

#[cfg(test)]
fn vga_trace_test_begin_capture() -> VgaTraceTestCaptureGuard {
    VGA_TRACE_TEST_RECORDS.with(|records| {
        *records.borrow_mut() = Some(Vec::new());
    });
    VgaTraceTestCaptureGuard
}

#[cfg(test)]
fn vga_trace_test_take_capture() -> Vec<[u8; 20]> {
    VGA_TRACE_TEST_RECORDS.with(|records| records.borrow_mut().take().unwrap_or_default())
}

#[cfg(test)]
fn vga_trace_test_capture_enabled() -> bool {
    VGA_TRACE_TEST_RECORDS.with(|records| records.borrow().is_some())
}

#[cfg(test)]
fn vga_trace_test_push_record(bytes: [u8; 20]) {
    VGA_TRACE_TEST_RECORDS.with(|records| {
        if let Some(records) = records.borrow_mut().as_mut() {
            records.push(bytes);
        }
    });
}

#[cfg(test)]
fn vga_trace_test_set_ctx(cycle: u64, cs: u16, eip: u32) {
    VGA_TRACE_CTX.with(|c| c.set((cycle, cs, eip)));
}

/// Fast-path gate: cached on first observation of the trace writer state.
/// Lets the interp loop short-circuit `set_vga_trace_ctx` with a single
/// relaxed atomic load when the (retired) VGA write-trace env var is unset (the
/// overwhelmingly common case in production).
static VGA_TRACE_CTX_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static VGA_TRACE_CTX_INIT: std::sync::Once = std::sync::Once::new();

#[inline]
fn vga_trace_ctx_enabled() -> bool {
    // One-shot initialisation: probe the writer state and cache the result.
    // After init, this is a single relaxed load — cheap enough for the
    // interpreter hot loop.
    VGA_TRACE_CTX_INIT.call_once(|| {
        let on = vga_write_trace_writer().is_some();
        VGA_TRACE_CTX_ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
    });
    VGA_TRACE_CTX_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Publish the current (cycle, cs, eip) so the next VGA write inherits it.
/// Call once per executed CPU instruction from the interpreter loop.
/// Fast-path: returns immediately when the (retired) VGA write-trace env var is unset
/// (single relaxed atomic load — no TLS access).
#[inline]
pub fn set_vga_trace_ctx(cycle: u64, cs: u16, eip: u32) {
    if !vga_trace_ctx_enabled() {
        return;
    }
    VGA_TRACE_CTX.with(|c| c.set((cycle, cs, eip)));
}

fn vga_write_trace_writer() -> Option<&'static std::sync::Mutex<std::io::BufWriter<std::fs::File>>>
{
    static WRITER: std::sync::OnceLock<
        Option<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>,
    > = std::sync::OnceLock::new();
    // the VGA write-trace output file was env-configured (retired trace
    // path); the trace is permanently off, so no writer is ever opened.
    WRITER.get_or_init(|| None).as_ref()
}

#[inline]
fn vga_trace_record_bytes(addr: usize, value: u8) -> [u8; 20] {
    let (cycle, cs, eip) = VGA_TRACE_CTX.with(|c| c.get());
    let mut bytes = [0u8; 20];
    bytes[0..8].copy_from_slice(&cycle.to_le_bytes());
    bytes[8..12].copy_from_slice(&eip.to_le_bytes());
    bytes[12..16].copy_from_slice(&(addr as u32).to_le_bytes());
    bytes[16..18].copy_from_slice(&cs.to_le_bytes());
    bytes[18] = value;
    // bytes[19] = 0 (pad)
    bytes
}

#[inline]
fn vga_trace_record(addr: usize, value: u8) {
    #[cfg(test)]
    let capture_enabled = vga_trace_test_capture_enabled();
    #[cfg(not(test))]
    let capture_enabled = false;

    #[cfg(test)]
    let writer = if capture_enabled {
        None
    } else {
        vga_write_trace_writer()
    };
    #[cfg(not(test))]
    let writer = vga_write_trace_writer();

    if writer.is_none() && !capture_enabled {
        return;
    }

    let bytes = vga_trace_record_bytes(addr, value);

    if let Some(mtx) = writer {
        if let Ok(mut w) = mtx.lock() {
            use std::io::Write as _;
            let _ = w.write_all(&bytes);
        }
    }

    #[cfg(test)]
    if capture_enabled {
        vga_trace_test_push_record(bytes);
    }
}

/// Flush the VGA write trace buffer to disk. Call at test capture points so
/// the trace file is durable even if the test process is killed afterwards.
/// No-op if the VGA write trace is not active.
pub fn vga_write_trace_flush() {
    if let Some(mtx) = vga_write_trace_writer() {
        if let Ok(mut w) = mtx.lock() {
            use std::io::Write as _;
            let _ = w.flush();
        }
    }
}

/// Memory size constants
pub const CONVENTIONAL_SIZE: usize = 640 * 1024; // 640KB conventional
pub const TOTAL_SIZE: usize = 1024 * 1024 + 64 * 1024; // 1MB + 64KB HMA
pub const DEFAULT_RAM_SIZE: usize = 128 * 1024 * 1024;

/// Architectural upper bound on linear guest RAM (`ram.len()` never exceeds
/// this plus [`FETCH_PADDING`]; enforced by [`Memory::resize_ram`]).
///
/// The upper allocation limit sizes the SMC watch bitmap and snapshot bound.
/// Backing storage is reserved up to [`DOS_RAM_CEILING`] initially and grows
/// lazily through [`Memory::expand_ram`].
pub const MAX_RAM_SIZE: usize = 768 * 1024 * 1024;

/// Guest-visible DOS RAM ceiling for XMS allocation, explicit mapping,
/// automatic RAM growth and physical write dispatch. This also bounds the
/// backing storage reserved by [`Memory::new`].
pub const DOS_RAM_CEILING: usize = 256 * 1024 * 1024;

/// Granularity for RAM growth caused by physical write first touches. A
/// bounded batch avoids repeating whole-memory invalidation for every byte of
/// an ascending write while preserving the DOS RAM ceiling.
const RAM_GROWTH_GRANULARITY: usize = 64 * 1024;

/// Padding bytes appended past the usable RAM size.  The fast interpreter may
/// read up to 15 bytes ahead during instruction decode (FPU via iced-x86,
/// opcode fetch).  16 bytes of guaranteed padding prevents segfaults when
/// CS:IP is near the end of RAM.
pub const FETCH_PADDING: usize = 16;
pub const VIDEO_BASE: usize = 0xA0000;
pub const VIDEO_SIZE: usize = 128 * 1024; // 128KB video memory
/// End of the A0000h-FFFFFh legacy hole the chipset never backs with DRAM.
const LEGACY_HOLE_END: usize = 0x10_0000;
/// Original S3 backward-compatible MMIO aperture.
pub const S3_MMIO_BASE: usize = VIDEO_BASE;
pub const S3_MMIO_SIZE: usize = 64 * 1024;
const S3_MMIO_READBACK_SIZE: usize = S3_MMIO_SIZE;
pub const EMS_PAGE_SIZE: usize = 16 * 1024;
pub const EMS_PHYSICAL_PAGES: usize = 4;
pub const EMS_FRAME_BASE: usize = 0xE0000;
pub const EMS_FRAME_SIZE: usize = EMS_PAGE_SIZE * EMS_PHYSICAL_PAGES;
pub const EMS_FRAME_END: usize = EMS_FRAME_BASE + EMS_FRAME_SIZE;
pub const VIDEO_WINDOW_64K: usize = 64 * 1024;
const VIDEO_WINDOW_32K: usize = 32 * 1024;
const TEXT_VIDEO_BASE_MONO: usize = 0xB0000;
const TEXT_VIDEO_BASE_COLOR: usize = 0xB8000;
const TEXT_VIDEO_SIZE: usize = 0x8000;
pub const BIOS_BASE: usize = 0xF0000;

/// Real-boot high-memory alias window. Physical addresses here fold back
/// onto the top 64 KiB of the first MiB (0xF0000..=0xFFFFF), which is the
/// top of the system BIOS image regardless of its size (the BIOS sits at
/// the top of the first MiB, with its base varying with image size).
pub const HIGH_MEM_ALIAS_BASE: usize = 0xFFFF_0000;
pub const HIGH_MEM_ALIAS_END: usize = 0xFFFF_FFFF;
pub const HIGH_MEM_ALIAS_FOLD_BASE: usize = 0x000F_0000;

/// Returns `true` if `addr` falls inside the real-boot high-memory alias
/// window (`0xFFFF_0000..=0xFFFF_FFFF`). Callers should gate on
/// `Memory::high_mem_alias_enabled()` first — the alias is off by default.
/// Kept local, not imported from `gp64-memory-core`'s twin: inlined across
/// the crate boundary it reorders `write_u32`'s EMS-frame test (M1 asm diff:
/// 542 -> 541 instructions, a different branch shape). Identical codegen to
/// the parent is the M1 acceptance bar.
#[inline(always)]
fn range_overlaps(a_start: usize, a_len: usize, b_start: usize, b_len: usize) -> bool {
    if a_len == 0 || b_len == 0 {
        return false;
    }
    let a_end = a_start.saturating_add(a_len);
    let b_end = b_start.saturating_add(b_len);
    a_start < b_end && b_start < a_end
}

#[inline(always)]
pub fn in_high_mem_alias(addr: usize) -> bool {
    (HIGH_MEM_ALIAS_BASE..=HIGH_MEM_ALIAS_END).contains(&addr)
}

/// A labelled region of guest physical memory.
///
/// Produced by `Memory::declared_regions()` and consumed by the real-boot
/// POST tracer to decide whether an out-of-region guest access is
/// anomalous and worth logging.
#[derive(Debug, Clone)]
pub struct DeclaredRegion {
    pub range: std::ops::RangeInclusive<usize>,
    pub label: &'static str,
}

/// A20 gate controls address line 20 (bit 20 = `0x0010_0000`)
#[cfg(test)]
const A20_BIT: usize = 0x0010_0000;

pub const LFB_BASE: usize = 0x4000_0000;
const _: () = assert!(
    LFB_BASE >= MAX_RAM_SIZE + FETCH_PADDING,
    "the LFB aperture must not alias any byte of the RAM allocation"
);
const _: () = assert!(
    (LFB_BASE & 0x007F_FFFF) == 0,
    "the LFB aperture must be 8 MiB-aligned (Trio64 BAR0 decode granularity)"
);
/// Compatibility alias for linear-framebuffer clients that use the
/// conventional Bochs/QEMU physical LFB aperture.
pub const LFB_BOCHS_COMPAT_BASE: usize = 0xE000_0000;
// `direct_host_ram_range`'s LFB test is endpoint-only (is_lfb_address(start) ||
// is_lfb_address(end-1)), and the interpreter's LOADIP whole-page fetch-proof
// cache probes a full 4 KiB page. Both are sound only if the LFB apertures are
// 4 KiB-page aligned: then any page overlapping the LFB has its (page-aligned)
// base inside the LFB, so the start-endpoint test catches every overlap and no
// page can be part-RAM/part-LFB without being rejected.
const _: () = assert!(
    (LFB_BASE & 0xFFF) == 0 && (LFB_BOCHS_COMPAT_BASE & 0xFFF) == 0,
    "LFB apertures must be 4 KiB-page aligned (fetch-proof cache soundness)"
);
pub const LFB_MAX_SIZE: usize = 4 * 1024 * 1024; // 4MB max LFB
pub const LFB_DIRTY_PAGE_SHIFT: usize = 12;
pub const LFB_DIRTY_PAGE_SIZE: usize = 1 << LFB_DIRTY_PAGE_SHIFT;
pub const LFB_DIRTY_PAGE_COUNT: usize = LFB_MAX_SIZE / LFB_DIRTY_PAGE_SIZE;
pub const LFB_DIRTY_WORDS: usize = LFB_DIRTY_PAGE_COUNT / 64;

/// Guest-visible linear-framebuffer decoder personality.
///
/// DISPI exposes the historical gp64 and Bochs-compatible aliases. S3
/// exposes exactly one relocatable aperture and never leaves either DISPI
/// alias active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfbDecoderConfig {
    Dispi {
        enabled: bool,
    },
    S3 {
        base: usize,
        size: usize,
        vram_offset: usize,
        enabled: bool,
    },
}

/// Original S3 legacy VGA-aperture routing that is not expressible through the
/// relocatable linear-framebuffer decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct S3LegacyApertureConfig {
    /// An S3 PCI function owns the legacy display-memory ranges.
    pub active: bool,
    /// PCI Command.Memory and Misc Output Enable RAM both permit a response.
    pub memory_response: bool,
    /// A0000h-AFFFFh remains decoded after linear addressing is enabled.
    pub a000_response: bool,
    /// CR31 enhanced mapping routes the fixed A0000h window directly to VRAM.
    pub enhanced_mapping: bool,
    /// CR3A simultaneous text/enhanced mode routes A0000h to top-of-VRAM.
    pub top_memory_access: bool,
    /// Selected 64 KiB CPU bank in the canonical VRAM backing.
    pub bank_offset: usize,
    /// CR53 swaps the two nibbles of every relocatable linear-window byte.
    pub linear_nibble_swap: bool,
}

/// Width of one original-S3 MMIO transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3MmioWidth {
    Byte,
    Word,
    Dword,
}

impl S3MmioWidth {
    pub const fn bytes(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
        }
    }
}

/// One ordered write through the original S3 A0000h-AFFFFh MMIO aperture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct S3MmioWrite {
    pub offset: u16,
    pub width: S3MmioWidth,
    pub value: u32,
}

/// One ordered read through the original S3 A0000h-AFFFFh MMIO aperture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct S3MmioRead {
    pub offset: u16,
    pub width: S3MmioWidth,
}

impl Default for LfbDecoderConfig {
    fn default() -> Self {
        Self::Dispi { enabled: false }
    }
}

/// Current guest-visible expansion-ROM decoder state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionRomDecoderConfig {
    pub base: usize,
    pub size: usize,
    pub enabled: bool,
}

/// ROM bitmap page size (4 KiB) and shift for the RomRegions write-protect
/// mechanism. Backs `Memory::rom_pages`: the bitmap covers the 1 MiB + HMA
/// range (`TOTAL_SIZE`) at 4-KiB granularity, so `TOTAL_SIZE / ROM_PAGE_SIZE`
/// = 272 entries.
const ROM_PAGE_SHIFT: usize = 12;
const ROM_PAGE_SIZE: usize = 1 << ROM_PAGE_SHIFT;

/// Pre-computed VGA write handler for the current display mode.
/// Set at mode-change time, consulted per-byte for most VGA-range writes.
/// Transient text-mode font-plane access is detected separately because it
/// changes CPU memory routing without being a display-mode transition.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum VgaWriteHandler {
    /// Text mode or linear — write directly to RAM, no plane logic
    Linear,
    /// Mode 13h chain-4 mode
    Chain4,
    /// Planar EGA/VGA modes (0x0D, 0x0E, 0x10, 0x12) and Mode X
    Planar,
}

/// How an admitted VGA aperture store commits natively, cached for the dyncore
/// `smc_store` fast path (EXP-033 Stage 1). `Some(kind)` means the current VGA state
/// admits a native aperture commit for EVERY Map-Mask and EVERY aperture address —
/// i.e. it is plane_mask- and address-INDEPENDENT — so the fast path can skip the
/// per-store predicate re-derivation. Recomputed in `update_vga_write_handler`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VgaCommitKind {
    /// Plain chain-4 Mode 13h: `commit_vga_chain4_plain_byte`.
    Chain4,
    /// Plain unchained planar (Mode-X / Mode-Y): `commit_vga_planar_plain_byte`.
    Planar,
}

#[derive(Debug)]
pub enum MemoryError {
    OutOfBounds(usize),
    Unaligned(usize),
    RawLoadOverlapsHleEmsFrame { addr: usize, len: usize },
}
impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds(address) => {
                write!(f, "Memory access out of bounds: {address:#X}")
            }
            Self::Unaligned(address) => write!(f, "Unaligned memory access at {address:#X}"),
            Self::RawLoadOverlapsHleEmsFrame { addr, len } => write!(
                f,
                "raw load at {addr:#X} for {len} bytes overlaps the HLE EMS page frame"
            ),
        }
    }
}
impl std::error::Error for MemoryError {}

pub type Result<T> = std::result::Result<T, MemoryError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmsPoolError {
    SizeOverflow(u16),
    InvalidPoolPage(u16),
    InvalidOffset(usize),
    RangeOutOfBounds,
    SnapshotSizeMismatch { expected: usize, actual: usize },
    InvalidFrameSlot(usize),
    FrameNotOwned,
}
impl fmt::Display for EmsPoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SizeOverflow(pages) => write!(f, "EMS pool size overflow for {pages} pages"),
            Self::InvalidPoolPage(page) => write!(f, "EMS pool page {page} is not allocated"),
            Self::InvalidOffset(offset) => {
                write!(
                    f,
                    "EMS pool offset {offset:#X} is outside a 16 KiB EMS page"
                )
            }
            Self::RangeOutOfBounds => f.write_str("EMS backing range exceeds allocated pool"),
            Self::SnapshotSizeMismatch { expected, actual } => write!(
                f,
                "EMS snapshot size mismatch: expected {expected} bytes, got {actual} bytes"
            ),
            Self::InvalidFrameSlot(slot) => {
                write!(
                    f,
                    "EMS frame slot {slot} is outside the four-slot page frame"
                )
            }
            Self::FrameNotOwned => f.write_str("HLE EMS frame is not owned by Memory"),
        }
    }
}
impl std::error::Error for EmsPoolError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryOwnerError {
    InvalidHleEmsFrame { base: usize, size: usize },
    OutOfRam,
    OverlapsVideo,
    OverlapsSystemBios,
    OverlapsRom,
}
impl fmt::Display for MemoryOwnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHleEmsFrame { base, size } => write!(
                f,
                "HLE EMS frame must be exactly 0xE0000..0xEFFFF, got base={base:#X} size={size:#X}"
            ),
            Self::OutOfRam => f.write_str("HLE EMS frame extends beyond RAM"),
            Self::OverlapsVideo => f.write_str("HLE EMS frame overlaps VGA memory"),
            Self::OverlapsSystemBios => f.write_str("HLE EMS frame overlaps system BIOS"),
            Self::OverlapsRom => f.write_str("HLE EMS frame overlaps a ROM-marked page"),
        }
    }
}
impl std::error::Error for MemoryOwnerError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmsAliasRange {
    pub frame_addr: usize,
    pub len: usize,
}

/// The page-frame windows one backing-pool range is visible through.
///
/// A range aliases each frame slot at most once, so the set never exceeds
/// [`EMS_PHYSICAL_PAGES`] entries and lives on the stack. MDD-BUG-CRU-01035:
/// every mapped EMS store builds one, and a heap `Vec` per byte put the
/// allocator on an ordinary guest memory path.
#[derive(Clone, Copy, Debug)]
pub struct EmsAliasRanges {
    len: usize,
    ranges: [EmsAliasRange; EMS_PHYSICAL_PAGES],
}

impl EmsAliasRanges {
    const EMPTY: Self = Self {
        len: 0,
        ranges: [EmsAliasRange {
            frame_addr: 0,
            len: 0,
        }; EMS_PHYSICAL_PAGES],
    };

    fn push(&mut self, range: EmsAliasRange) {
        self.ranges[self.len] = range;
        self.len += 1;
    }

    pub fn as_slice(&self) -> &[EmsAliasRange] {
        &self.ranges[..self.len]
    }
}

impl std::ops::Deref for EmsAliasRanges {
    type Target = [EmsAliasRange];

    fn deref(&self) -> &[EmsAliasRange] {
        self.as_slice()
    }
}

impl PartialEq for EmsAliasRanges {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for EmsAliasRanges {}

impl IntoIterator for EmsAliasRanges {
    type Item = EmsAliasRange;
    type IntoIter = std::iter::Take<std::array::IntoIter<EmsAliasRange, EMS_PHYSICAL_PAGES>>;

    fn into_iter(self) -> Self::IntoIter {
        self.ranges.into_iter().take(self.len)
    }
}

impl<'a> IntoIterator for &'a EmsAliasRanges {
    type Item = &'a EmsAliasRange;
    type IntoIter = std::slice::Iter<'a, EmsAliasRange>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmsFrameResolution {
    NotFrame,
    Unmapped {
        frame_addr: usize,
    },
    Mapped {
        slot: usize,
        pool_page: u16,
        pool_offset: usize,
        frame_addr: usize,
    },
}

/// Plane size constant (64KB per plane)
const PLANE_SIZE: usize = 65536;

/// Total EGA/VGA planar backing size: four fixed 64 KiB planes.
pub const VGA_PLANES_SIZE: usize = PLANE_SIZE * 4;

/// Granularity of [`Memory::take_vga_plane_rewrites`]: one bit per 4 KiB of
/// S3 VRAM in the Trio64 VGA-mode layout (VRAM byte `4 * cell + plane`), so a
/// bit covers 1 KiB of cells in every plane and 64 bits cover the planes.
pub const VGA_PLANE_VRAM_PAGE_SIZE: usize = 4096;

/// Every VRAM page of [`Memory::take_vga_plane_rewrites`].
pub const VGA_PLANE_VRAM_ALL_PAGES: u64 = u64::MAX;

/// A variable-length source cannot replace the fixed VGA plane backing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VgaPlanesSizeError {
    pub actual: usize,
}
impl fmt::Display for VgaPlanesSizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "VGA plane backing must contain exactly {VGA_PLANES_SIZE} bytes, got {}",
            self.actual
        )
    }
}
impl std::error::Error for VgaPlanesSizeError {}

// ============================================================================
// Write Tracking Infrastructure (for Time Machine debugging)
// ============================================================================

/// Context for the currently executing instruction (used for write tracking).
/// Set before instruction execution so Memory can associate writes with their source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteRegisterContext {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esi: u32,
    pub edi: u32,
    pub ebp: u32,
    pub esp: u32,
    pub ds: u16,
    pub es: u16,
    pub ss: u16,
    pub cs_base: u32,
    pub cs_limit: u32,
    pub cs_access: u8,
    pub cs_big: bool,
    pub ds_base: u32,
    pub ds_limit: u32,
    pub ds_access: u8,
    pub ds_big: bool,
    pub es_base: u32,
    pub es_limit: u32,
    pub es_access: u8,
    pub es_big: bool,
    pub ss_base: u32,
    pub ss_limit: u32,
    pub ss_access: u8,
    pub ss_big: bool,
    pub eflags: u32,
    pub cr0: u32,
    pub cr3: Option<u32>,
    pub cpl: Option<u8>,
    pub mode: IoCpuMode,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WriteContext {
    /// CS segment of the instruction performing the write
    pub cs: u16,
    /// IP/EIP of the instruction performing the write
    pub ip: u32,
    /// Optional diagnostic register snapshot for video-write provenance.
    pub regs: Option<WriteRegisterContext>,
}

/// A pending memory write captured during instruction execution.
/// These are accumulated in Memory and later flushed to `TimeMachine`.
#[derive(Clone, Copy, Debug)]
pub struct PendingWrite {
    /// CS segment of the instruction that performed the write
    pub source_cs: u16,
    /// IP/EIP of the instruction that performed the write
    pub source_ip: u32,
    /// Linear address that was written
    pub linear_address: u32,
    /// Value before the write
    pub old_value: u64,
    /// Value after the write
    pub new_value: u64,
    /// Size of the representative old/new values in bytes (1, 2, 4, or 8).
    pub size: u8,
    /// Number of consecutive bytes affected by the operation.
    ///
    /// This normally equals `size`. Bulk RAM operations use one representative
    /// value plus the full affected length so tracking stays bounded.
    pub affected_len: u32,
    /// True if this write is from an HLE (High-Level Emulation) handler
    pub is_hle_handler: bool,
    /// True if this write is a VGA planar write
    pub is_vga_write: bool,
    /// VGA plane number (0-3) if this is a VGA planar write
    pub vga_plane: Option<u8>,
}

impl PendingWrite {
    /// Create a new pending write record.
    pub fn new(
        source_cs: u16,
        source_ip: u32,
        linear_address: u32,
        old_value: u64,
        new_value: u64,
        size: u8,
    ) -> Self {
        Self {
            source_cs,
            source_ip,
            linear_address,
            old_value,
            new_value,
            size,
            affected_len: u32::from(size),
            is_hle_handler: false,
            is_vga_write: false,
            vga_plane: None,
        }
    }

    /// Create a new pending write record with explicit HLE handler flag.
    pub fn new_with_hle(
        source_cs: u16,
        source_ip: u32,
        linear_address: u32,
        old_value: u64,
        new_value: u64,
        size: u8,
        is_hle_handler: bool,
    ) -> Self {
        Self {
            source_cs,
            source_ip,
            linear_address,
            old_value,
            new_value,
            size,
            affected_len: u32::from(size),
            is_hle_handler,
            is_vga_write: false,
            vga_plane: None,
        }
    }

    /// Create a new pending write record for a VGA planar write.
    pub fn new_with_vga(
        source_cs: u16,
        source_ip: u32,
        linear_address: u32,
        old_value: u64,
        new_value: u64,
        size: u8,
        plane: u8,
    ) -> Self {
        Self {
            source_cs,
            source_ip,
            linear_address,
            old_value,
            new_value,
            size,
            affected_len: u32::from(size),
            is_hle_handler: false,
            is_vga_write: true,
            vga_plane: Some(plane),
        }
    }

    /// Describe a consecutive range affected by one bulk operation.
    pub fn with_affected_len(mut self, affected_len: u32) -> Self {
        self.affected_len = affected_len.max(u32::from(self.size));
        self
    }
}

fn pack_write_value(bytes: &[u8]) -> u64 {
    debug_assert!(matches!(bytes.len(), 1 | 2 | 4 | 8));
    bytes.iter().enumerate().fold(0u64, |value, (shift, byte)| {
        value | ((*byte as u64) << (shift * 8))
    })
}

/// A memory-mapped I/O region backed by a dedicated buffer.
/// Used for device memory that sits at physical addresses above normal RAM
/// (e.g., 3dfx Voodoo2 LFB at 0x60000000).
#[derive(Debug)]
pub struct MmioRegion {
    /// Physical start address of this MMIO region.
    pub phys_start: usize,
    /// Backing data buffer (owned by Memory).
    pub data: Vec<u8>,
    /// XOR applied to the low bits of a CPU access's byte offset before it
    /// touches `data`. Devices whose bus interface swaps bytes on PCI
    /// accesses (the Vérité's MEMENDIAN register) set this; device-side
    /// access through `mmio_slice*` is never swizzled. Always `< 4`, so a
    /// swizzled offset stays inside the same DWORD and inside `data`
    /// whenever the region length is a multiple of four.
    pub byte_swizzle: u8,
}

/// One process's shared-arena coherence state (HLD §5.3-5.4), boxed in
/// [`Memory`] because none of it is on a per-access path.
#[derive(Default)]
struct ArenaProcessState {
    /// Last shared arena PTE generation observed by this process.
    pte_generation_seen: u64,
    /// Cross-process SMC for the shared flat-memory arena, kept at the process
    /// seam, outside either native engine's SMC design. The session state and
    /// this process's PID, while this `Memory` is mounted.
    smc: Option<(u32, Arc<Mutex<arena_smc::ArenaSmc>>)>,
    /// Arena pages this process's DBX/DRC cache watches.
    local_watches: std::collections::BTreeSet<u32>,
    /// Arena pages the (legacy) interpreter decoded from.
    instr_watches: std::collections::BTreeSet<u32>,
    /// Arena pages some other process watches, as of the last attach.
    remote_watches: std::collections::BTreeSet<u32>,
    /// This process's position in the session arena write log.
    smc_cursor: u64,
    /// Set by the first `attach_arena_smc`: a process that never attached
    /// holds no translations, so it starts at the log's head.
    smc_attached: bool,
}

/// Main memory subsystem
#[allow(clippy::struct_excessive_bools)]
pub struct Memory {
    /// Raw host backing buffer, with the DOS-era capacity reserved
    /// (`DOS_RAM_CEILING + FETCH_PADDING`) by `Memory::new()`; flat-memory commits
    /// may grow it up to `MAX_RAM_SIZE + FETCH_PADDING` via over-capacity
    /// growth (WI-G1). All size changes go through `resize_ram`, which
    /// retires direct page-proof host pointers before the allocation can move
    /// or shrink (see `direct_proof` module). On 64-bit hosts the backing sits
    /// in a fixed host reservation and never moves (`ram_backing.rs`); 32-bit
    /// hosts keep a `Vec`, which over-capacity growth may move.
    ///
    /// This remains publicly byte-addressable for legacy cross-crate tests and
    /// low-level host integrations, but [`RamBacking`] prevents external code
    /// from resizing or replacing its allocation. Size changes must use
    /// [`Memory::resize_ram`] or a higher-level memory-owned API.
    pub ram: RamBacking,
    /// EMS backing storage owned by the memory core. The HLE INT 67h layer
    /// owns logical handles and maps them to these physical pool pages.
    ems_pool: Vec<u8>,
    /// Derived HLE EMS page-frame map: one optional EMS pool page per 16 KiB
    /// physical slot at 0xE0000..0xEFFFF.
    ems_frame_map: [Option<u16>; EMS_PHYSICAL_PAGES],
    /// Page fault pending flag - set when paging translation fails
    /// Callers can check this after `translate_address_full` to detect page faults
    pub page_fault_pending: bool,
    /// True once the HLE DOS path has reserved the fixed EMS page frame.
    hle_ems_frame_owned: bool,
    /// A20 gate state - when disabled, address bit 20 is forced to 0 (1MB wraparound)
    a20_enabled: bool,
    /// EGA/VGA planar memory - four fixed 64 KiB planes on the heap.
    /// Layout: [plane0: 64KB][plane1: 64KB][plane2: 64KB][plane3: 64KB]
    ///
    /// This is private and fixed-size because native VGA commits publish its
    /// raw pointer and write fixed offsets spanning the complete allocation.
    planes_flat: Box<[u8; VGA_PLANES_SIZE]>,
    /// Pre-computed VGA write dispatch handler, derived from video mode and chain4.
    /// Updated at mode-change time by `update_vga_write_handler()`.
    vga_write_handler: VgaWriteHandler,
    /// Cached plane_mask- and address-independent VGA native-commit admission for the
    /// dyncore `smc_store` fast path (EXP-033 Stage 1). Recomputed alongside
    /// `vga_write_handler` in `update_vga_write_handler()`; `None` = not admitted.
    vga_native_commit: Option<VgaCommitKind>,

    // EGA/VGA Graphics Controller state for proper planar operations
    /// Latch registers - loaded on ANY read from video memory
    /// Uses Cell for interior mutability (reads have side effects on EGA/VGA hardware)
    latches: Cell<[u8; 4]>,
    /// Allow writing to ROM area (for CPU logic tests)
    pub allow_rom_writes: bool,
    /// Per-4-KiB-page ROM bitmap covering the 1 MiB + HMA range (`TOTAL_SIZE`).
    /// A `true` entry means guest writes to that page are silently dropped.
    /// Used by the real-boot path (system firmware/VGA firmware images loaded at
    /// 0xC0000..=0xFFFFF) to catch stray stores that would corrupt ROM.
    /// See `mark_rom_page`, `mark_rom_range`, `is_rom_addr`.
    rom_pages: Vec<bool>,
    /// Fast guard: true when any ROM page has been marked. When false the
    /// `is_rom_addr` check collapses to a single bool-compare, keeping the
    /// non-real-boot path zero-cost.
    has_rom_regions: bool,
    /// Deferred option-ROM shadow lock applied at an instruction boundary.
    /// Chipsets allow firmware to initialise writable option-ROM shadow RAM
    /// before locking it read-only. `arm_option_rom_lock` records the range;
    /// `apply_option_rom_lock` locks it when the guest programs the 440FX PAM
    /// write-protection registers. Applying the lock earlier would discard
    /// stores made during option-ROM initialisation.
    pending_option_rom_lock: Option<std::ops::RangeInclusive<usize>>,
    /// PCI expansion-ROM bytes and their live physical decoder. This is
    /// separate from the legacy C-segment shadow-RAM loader so PCI ROM BAR
    /// gating and relocation cannot leave stale bytes decoded at an old base.
    option_rom_backing: Vec<u8>,
    option_rom_base: usize,
    option_rom_enabled: bool,
    /// Real-boot high-memory alias: when enabled, reads from
    /// `0xFFFF_0000..=0xFFFF_FFFF` fold back to `0xF0000..=0xFFFFF` (the top
    /// 64 KiB of the first MiB, which is the top of the system BIOS image
    /// regardless of its size — the BIOS sits at the top of the first MiB,
    /// with its base varying with image size). This mirrors the chipset
    /// alias real hardware uses so the reset vector at physical
    /// `0xFFFF_FFF0` hits the last 16 bytes of the BIOS ROM. Writes to the
    /// alias window are dropped silently. Disabled by default; the
    /// real-boot loader enables it via `set_high_mem_alias_enabled`.
    high_mem_alias_enabled: bool,
    /// Real-boot PIC-only firmware table filter. Intel MP Specification
    /// tables advertise APIC topology; hide those tables until LAPIC/IOAPIC
    /// interrupt delivery is supported. Disabled outside real-boot.
    firmware_mp_table_suppression_enabled: bool,
    /// Linear address that caused the page fault (for diagnostics)
    pub page_fault_address: u32,
    /// Error code for the last page fault (bitfield per x86)
    pub page_fault_error_code: u32,
    /// This process's shared-arena coherence state (HLD §5.3-5.4). Cold, so
    /// it is boxed: it must not move `Memory`'s hot fields (`memory_hot_layout`).
    arena: Box<ArenaProcessState>,
    /// DPMI physical mappings (linear -> physical)
    physical_mappings: FixedVec<PhysicalMapping, 64>,
    /// Video memory write counter for watchdog tracking
    video_write_count: std::sync::atomic::AtomicU64,
    /// VGA writes committed by NATIVE routes: the `commit_vga_*_plain_byte`
    /// helpers, plus counts folded in from the emitted inline corridor.
    ///
    /// Kept separate from `video_write_count` and deliberately NOT atomic:
    /// every native writer already holds `&mut Memory`, so a plain increment
    /// is correct and costs nothing in the hot path — which is what the bug
    /// asked for (MDD-BUG-CRUCIBLE-00150).
    native_video_writes: u64,
    /// Byte transactions committed through the legacy VGA aperture. Unlike
    /// the watchdog counter this excludes linear framebuffers and is plain
    /// because every writer already holds `&mut Memory`.
    vga_bus_write_count: u64,
    /// Video writes that changed at least one backing byte.
    video_write_changed_count: std::sync::atomic::AtomicU64,
    /// Video writes that left all observed backing bytes unchanged.
    video_write_unchanged_count: std::sync::atomic::AtomicU64,
    /// Bounded diagnostic sample of changed planar VGA byte writes.
    video_write_change_samples: Mutex<VideoWriteChangeSampleBuffer>,
    /// VBE 2.0 Linear Framebuffer storage (up to 4MB)
    lfb: Vec<u8>,
    /// Dirty tracking for canonical LFB writes (1024 x 4KB pages = 4MB).
    lfb_dirty_pages: [u64; LFB_DIRTY_WORDS],
    /// Last observed CR3 value (page directory base), cached for VGA LFB sync
    last_cr3: u32,
    /// Guest-visible LFB decoder. DISPI owns its two fixed aliases; S3 owns
    /// exactly one relocatable aperture.
    lfb_decoder: LfbDecoderConfig,
    /// Whether this instance has reported an IVT entry pointing into video
    /// memory. Per instance, so one Memory (or one test) cannot silence the
    /// diagnostic for every other (MDD-BUG-CRU-01484).
    ivt_corruption_logged: bool,
    /// Legacy A0000h-BFFFFh response and direct enhanced-mode routing.
    s3_legacy_aperture: S3LegacyApertureConfig,
    /// EXP-IQ002 engagement counters, ALWAYS-ON (the perf artifact carries no
    /// attrib feature, so the A/B certificate must come from the shipped
    /// binary — the `native_video_writes` precedent). Printed as the
    /// `[INTERP]` line in the end-of-run benchmark census.
    interp_vga_chain4_fast_commits: u64,
    /// EXP-IQ002 L1: stores served by reusing a TLB-held device-page
    /// translation instead of the cold `translate_slow` call.
    interp_tlb_device_reuse_hits: u64,
    /// A052 outcome census, ALWAYS-ON (same reasoning as the two counters
    /// above — the perf artifact carries no attrib feature, so the A/B
    /// certificate has to come out of the shipped binary). Spans
    /// `write_bytes_guest` served from its bulk arm, by width bucket.
    span_write_bulk: [u64; span_write_census::WIDTH_BUCKETS],
    /// A052 outcome census: spans `write_bytes_guest` sent down the per-byte
    /// `write_u8` fallback. Counted separately because a descriptor snoop or
    /// an armed trace sends EVERYTHING here, and "the funnel ran N million
    /// times" must never be readable as "N million bulk copies".
    span_write_per_byte: [u64; span_write_census::WIDTH_BUCKETS],
    /// Original S3 A0000h-AFFFFh side-effecting MMIO decoder.
    s3_mmio_enabled: bool,
    /// Pre-transaction read image for the standard MMIO register aliases.
    /// Packed and image-transfer aliases are deliberately left at open-bus FFh.
    s3_mmio_readback: Vec<u8>,
    /// Reads awaiting the post-instruction VGA device boundary. Reads use an
    /// interior-mutable mailbox because the CPU memory-read interface is shared.
    pending_s3_mmio_reads: Mutex<Vec<S3MmioRead>>,
    /// Lock-free hot-path indication that the read mailbox is non-empty.
    pending_s3_mmio_read: AtomicBool,
    /// Writes awaiting the post-instruction VGA device boundary.
    pending_s3_mmio_writes: Vec<S3MmioWrite>,
    /// Fused per-retire poll flag: true iff `pending_s3_mmio_read` is set OR
    /// `pending_s3_mmio_writes` is non-empty. The interpreter polls once per
    /// guest instruction, so this collapses its two loads (atomic + Vec len)
    /// into one. Every mutation site of the two underlying states maintains
    /// the invariant.
    s3_mmio_attention: AtomicBool,
    /// Video dirty page tracking (16 x 4KB pages = 64KB at A0000-AFFFF)
    video_dirty_pages: std::sync::atomic::AtomicU16,
    /// Plane cells rewritten by a path that does not mark `video_dirty_pages`
    /// (bulk resyncs, HLE plane access, snapshot restore), one bit per
    /// [`VGA_PLANE_VRAM_PAGE_SIZE`] of interleaved VRAM. The S3 VRAM model
    /// derives from the planes in VGA modes and must learn of these too.
    vga_plane_rewrites: std::sync::atomic::AtomicU64,

    // Write tracking for Time Machine debugging
    /// Context for the currently executing instruction (CS:IP)
    write_context: Option<WriteContext>,
    /// Pending writes to be flushed to `TimeMachine`
    pending_writes: Vec<PendingWrite>,
    /// Whether write tracking is enabled
    write_tracking_enabled: bool,
    /// Whether currently inside an HLE (High-Level Emulation) handler
    hle_handler_active: bool,

    /// Debug-only flag to detect missing VGA pipeline syncs.
    ///
    /// Set to `true` by callers (BIOS, JIT, emulator init) after VGA register
    /// changes that require `apply_vga_pipeline()`.  Cleared inside
    /// `apply_vga_pipeline()`.  A `debug_assert!` in `write_planar_ega_masked`
    /// fires if a planar VRAM write occurs while this flag is still set,
    /// indicating a missing sync.  Zero cost in release builds.
    #[cfg(debug_assertions)]
    pub vga_pipeline_debug_dirty: bool,

    /// One-way latch recording that a guest write landed in the VGA planar
    /// A0000–AFFFF window while the pipeline was configured for a plane-2
    /// font upload (non-chain4, plane_mask=0x04, memory_map_select ∈ {0,1}).
    ///
    /// Memory evaluates the condition at write time using its cached
    /// `VgaPipelineState` snapshot — the same font-upload predicate the `Vga`
    /// renderer uses.  The `Vga` renderer polls this flag via
    /// `planar_font_latch_pending()` and mirrors it into `Vga::custom_font_loaded`.
    /// We cannot call into `Vga` directly from here (dependency direction is
    /// `gp64-hw → gp64-memory`).
    ///
    /// Never cleared after set.  Fresh `Memory::new()` starts at `false`.
    planar_font_latch_pending: bool,
    /// MMIO regions for device memory (e.g., 3dfx Voodoo2 LFB at 0x60000000).
    mmio_regions: Vec<MmioRegion>,
    /// True when a TLB entry or direct page proof for a shared arena page may
    /// have been published since the last `take_arena_admission` (HLD §5.2,
    /// I10). Taking admission retires them; with no arena publication it has
    /// nothing to retire, so a mount keeps both processes' TLBs warm.
    arena_published: Cell<bool>,
    /// Some other process watches an arena page this process can store to
    /// (HLD §5.4). Maintained by the arena SMC seam (`attach_arena_smc`).
    remote_arena_watch: bool,
    /// `smc_before_write`'s one-boolean gate: local code pages OR a remote
    /// arena watch (HLD §5.4 piece 3). The core's own code-page gate is
    /// unchanged; this PC gate sits above it.
    smc_interest: bool,
    // ── Real-boot POST tracer ─────────────────────────────────────────
    // When enabled, guest memory accesses that fall outside any declared
    // region (see `declared_regions()`) are logged once per 4-KiB page via
    // `tracing::warn!(target: "gp64::realboot::post_trace", …)`.
    // Always-off by default; the real-boot wiring calls
    // `set_post_trace_enabled(true)` during `setup_real_boot_rom`.
    post_trace_enabled: bool,
    /// VGA pipeline state — video mode, chain4, plane mask, read plane, write mode,
    /// bit mask, set/reset, data rotate, read mode, color compare/don't care, memory map select.
    /// Public for test assertions; production writes go through `apply_vga_pipeline()`.
    ///
    /// Declared after the one-byte flags on purpose (layout, not logic): the
    /// compiler places it after them, so the 15-byte state does not push
    /// `allow_rom_writes` off the cache line that holds `page_fault_pending`,
    /// `hle_ems_frame_owned` and `a20_enabled` (`memory_hot_layout`).
    pub vga: VgaPipelineState,
    /// Deduplication set keyed by `(page_base, direction)` where
    /// `direction` is `0` for reads and `1` for writes. Page granularity
    /// (4 KiB) keeps log volume bounded when the guest walks a region.
    post_trace_seen: RefCell<HashSet<(usize, u8)>>,

    /// Shared private x86 translation and invalidation state.
    core: MemoryCore,
}

pub struct MemorySliceMut<'a> {
    mem: &'a mut Memory,
    addr: usize,
    len: usize,
    buf: Option<Vec<u8>>,
}

impl<'a> MemorySliceMut<'a> {
    fn borrowed(mem: &'a mut Memory, addr: usize, len: usize) -> Self {
        Self {
            mem,
            addr,
            len,
            buf: None,
        }
    }

    fn owned(mem: &'a mut Memory, addr: usize, buf: Vec<u8>) -> Self {
        let len = buf.len();
        Self {
            mem,
            addr,
            len,
            buf: Some(buf),
        }
    }
}

impl Deref for MemorySliceMut<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        match &self.buf {
            Some(buf) => buf.as_slice(),
            None => &self.mem.ram[self.addr..self.addr + self.len],
        }
    }
}

impl DerefMut for MemorySliceMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match &mut self.buf {
            Some(buf) => buf.as_mut_slice(),
            None => &mut self.mem.ram[self.addr..self.addr + self.len],
        }
    }
}

impl Drop for MemorySliceMut<'_> {
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take() {
            if !buf.is_empty() {
                if self.mem.hle_ems_frame_owned
                    && range_overlaps(self.addr, buf.len(), EMS_FRAME_BASE, EMS_FRAME_SIZE)
                {
                    for (i, byte) in buf.iter().copied().enumerate() {
                        self.mem.write_u8(self.addr.wrapping_add(i), byte);
                    }
                } else {
                    let _ = self.mem.load(self.addr, &buf);
                }
            }
        }
    }
}

// Note: PageAccess enum has been moved to gp64-types alongside the
// translate methods that use it (translate_address_full_checked, etc.)

/// DPMI physical address mapping entry
#[derive(Clone, Copy, Debug)]
struct PhysicalMapping {
    linear: u32,
    physical: u32,
    size: u32,
}

impl Memory {
    pub fn maybe_trace_watched_write_range(&self, start: usize, len: usize, site: &'static str) {
        let csip = INTERP_WATCHPOINT_CSIP.load(Ordering::Relaxed);
        let cs_val = ((csip >> 16) & 0xFFFF) as u16;
        let ip_val = (csip & 0xFFFF) as u16;
        let eip_val = INTERP_WATCHPOINT_EIP.load(Ordering::Relaxed);
        let dses = INTERP_WATCHPOINT_DSES.load(Ordering::Relaxed);
        let ds_val = ((dses >> 16) & 0xFFFF) as u16;
        let es_val = (dses & 0xFFFF) as u16;
        let sidi = INTERP_WATCHPOINT_SIDI.load(Ordering::Relaxed);
        let si_val = ((sidi >> 16) & 0xFFFF) as u16;
        let di_val = (sidi & 0xFFFF) as u16;
        let axbx = INTERP_WATCHPOINT_AXBX.load(Ordering::Relaxed);
        let ax_val = ((axbx >> 16) & 0xFFFF) as u16;
        let bx_val = (axbx & 0xFFFF) as u16;
        let cxdx = INTERP_WATCHPOINT_CXDX.load(Ordering::Relaxed);
        let cx_val = ((cxdx >> 16) & 0xFFFF) as u16;
        let dx_val = (cxdx & 0xFFFF) as u16;
        let bpsp = INTERP_WATCHPOINT_BPSP.load(Ordering::Relaxed);
        let bp_val = ((bpsp >> 16) & 0xFFFF) as u16;
        let sp_val = (bpsp & 0xFFFF) as u16;
        let ssfs = INTERP_WATCHPOINT_SSFS.load(Ordering::Relaxed);
        let ss_val = ((ssfs >> 16) & 0xFFFF) as u16;
        let fs_val = (ssfs & 0xFFFF) as u16;
        let cs_base = INTERP_WATCHPOINT_CS_BASE.load(Ordering::Relaxed);
        let ds_base = INTERP_WATCHPOINT_DS_BASE.load(Ordering::Relaxed);
        let es_base = INTERP_WATCHPOINT_ES_BASE.load(Ordering::Relaxed);
        let ss_base = INTERP_WATCHPOINT_SS_BASE.load(Ordering::Relaxed);
        let fetch0 = INTERP_WATCHPOINT_FETCH0.load(Ordering::Relaxed);
        let fetch1 = INTERP_WATCHPOINT_FETCH1.load(Ordering::Relaxed);
        if let Some((range_start, range_end)) = traced_write_range() {
            let end = start.saturating_add(len);
            if end > range_start && start < range_end {
                let overlap_start = start.max(range_start);
                let overlap_end = end.min(range_end).min(self.ram.len());
                let host_bt = if traced_write_backtrace_enabled() {
                    let bt = std::backtrace::Backtrace::force_capture();
                    let bt = format!("{bt}");
                    bt.lines().take(24).collect::<Vec<_>>().join(" | ")
                } else {
                    String::new()
                };
                for addr in overlap_start..overlap_end {
                    tracing::error!(
                        target: "gp64::memory::write_watch",
                        cs = format_args!("{:04X}", cs_val),
                        ip = format_args!("{:04X}", ip_val),
                        eip = format_args!("{:08X}", eip_val),
                        ds = format_args!("{:04X}", ds_val),
                        es = format_args!("{:04X}", es_val),
                        si = format_args!("{:04X}", si_val),
                        di = format_args!("{:04X}", di_val),
                        ax = format_args!("{:04X}", ax_val),
                        bx = format_args!("{:04X}", bx_val),
                        cx = format_args!("{:04X}", cx_val),
                        dx = format_args!("{:04X}", dx_val),
                        bp = format_args!("{:04X}", bp_val),
                        sp = format_args!("{:04X}", sp_val),
                        ss = format_args!("{:04X}", ss_val),
                        fs = format_args!("{:04X}", fs_val),
                        cs_base = format_args!("{:08X}", cs_base),
                        ds_base = format_args!("{:08X}", ds_base),
                        es_base = format_args!("{:08X}", es_base),
                        ss_base = format_args!("{:08X}", ss_base),
                        fetch0 = format_args!("{:08X}", fetch0),
                        fetch1 = format_args!("{:08X}", fetch1),
                        site,
                        host_bt = %host_bt,
                        "WRITE_WATCH linear {:#010x} = {:#04x}",
                        addr,
                        self.ram[addr]
                    );
                }
            }
        }
        if let Some(exact) = traced_write_exact() {
            if exact >= start && exact < start.saturating_add(len) && exact < self.ram.len() {
                let host_bt = if traced_write_backtrace_enabled() {
                    let bt = std::backtrace::Backtrace::force_capture();
                    let bt = format!("{bt}");
                    bt.lines().take(24).collect::<Vec<_>>().join(" | ")
                } else {
                    String::new()
                };
                tracing::error!(
                    target: "gp64::memory::write_watch",
                    cs = format_args!("{:04X}", cs_val),
                    ip = format_args!("{:04X}", ip_val),
                    eip = format_args!("{:08X}", eip_val),
                    ds = format_args!("{:04X}", ds_val),
                    es = format_args!("{:04X}", es_val),
                    si = format_args!("{:04X}", si_val),
                    di = format_args!("{:04X}", di_val),
                    ax = format_args!("{:04X}", ax_val),
                    bx = format_args!("{:04X}", bx_val),
                    cx = format_args!("{:04X}", cx_val),
                    dx = format_args!("{:04X}", dx_val),
                    bp = format_args!("{:04X}", bp_val),
                    sp = format_args!("{:04X}", sp_val),
                    ss = format_args!("{:04X}", ss_val),
                    fs = format_args!("{:04X}", fs_val),
                    cs_base = format_args!("{:08X}", cs_base),
                    ds_base = format_args!("{:08X}", ds_base),
                    es_base = format_args!("{:08X}", es_base),
                    ss_base = format_args!("{:08X}", ss_base),
                    fetch0 = format_args!("{:08X}", fetch0),
                    fetch1 = format_args!("{:08X}", fetch1),
                    site,
                    host_bt = %host_bt,
                    "WRITE_EXACT linear {:#010x} = {:#04x}",
                    exact,
                    self.ram[exact]
                );
            }
        }
    }

    #[inline]
    fn maybe_trace_phys_write_range(&self, start: usize, len: usize, value: u32) {
        let Some((range_start, range_end)) = traced_phys_write_range() else {
            return;
        };
        let end = start.saturating_add(len);
        if end <= range_start || start >= range_end {
            return;
        }

        let overlap_start = start.max(range_start);
        let overlap_end = end.min(range_end);
        let mut ivt_vector = None;
        let mut ivt_entry = None;
        if overlap_start < 0x400 {
            let entry_base = overlap_start & !3;
            if entry_base + 3 < self.ram.len() {
                let off = u16::from_le_bytes([self.ram[entry_base], self.ram[entry_base + 1]]);
                let seg = u16::from_le_bytes([self.ram[entry_base + 2], self.ram[entry_base + 3]]);
                ivt_vector = Some(entry_base / 4);
                ivt_entry = Some((seg, off));
            }
        }

        tracing::error!(
            target: "gp64::memory::phys_write",
            start = format_args!("{:05X}", start),
            len,
            value = format_args!("{:08X}", value),
            overlap_start = format_args!("{:05X}", overlap_start),
            overlap_end = format_args!("{:05X}", overlap_end),
            ivt_vector = ivt_vector
                .map(|vector| format!("{vector:02X}"))
                .unwrap_or_else(|| "-".to_string()),
            ivt_entry = ivt_entry
                .map(|(seg, off)| format!("{seg:04X}:{off:04X}"))
                .unwrap_or_else(|| "-".to_string()),
            backtrace = %std::backtrace::Backtrace::force_capture(),
            "physical write trace"
        );
    }

    pub fn maybe_trace_linear_write_range(&self, start: usize, len: usize, site: &'static str) {
        self.maybe_trace_watched_write_range(start, len, site);
    }

    /// Returns true if any linear-write trace range or exact watch is configured
    /// (via retired write-range/write-exact env vars).
    /// OnceLock-cached — no overhead after first call.
    #[inline]
    pub fn has_traced_write_config(&self) -> bool {
        traced_write_range().is_some() || traced_write_exact().is_some()
    }

    /// Get raw pointer to the RAM backing allocation for direct-host internals.
    ///
    /// # Safety
    /// The caller must ensure bounds checking when accessing through this
    /// pointer. This pointer is not proof that a guest physical range is safe to
    /// access directly: EMS page-frame mappings, VGA/LFB/MMIO dispatch,
    /// the wrong source of truth. Guest-visible direct access must first prove
    /// the specific range with [`Self::direct_host_ram_range`] or use the TLB's
    /// direct-host proof.
    #[inline]
    pub fn ram_ptr(&self) -> *const u8 {
        self.ram.host_base().cast_const()
    }

    /// Get mutable raw pointer to the RAM backing allocation for direct-host
    /// internals.
    ///
    /// See [`Self::ram_ptr`] for the proof requirement. Guest-visible writes
    /// through this pointer are only valid for ranges already accepted by
    /// [`Self::direct_host_ram_range`] or an equivalent TLB direct-host proof.
    #[inline]
    pub fn ram_ptr_mut(&mut self) -> *mut u8 {
        self.ram.host_base()
    }

    /// Logical size of usable RAM, excluding the fetch-safety padding.
    /// The fast interpreter should use this for bounds checking (`ram_len`)
    /// instead of `ram.len()`.
    ///
    /// Backing proof (HLD §5.2, U2): `[0, usable_len() + FETCH_PADDING)` is the
    /// private extent of the sparse [`RamBacking`], always host-backed. So a
    /// `phys + n <= usable_len()` bound proves `[phys, phys + n)` backed, and a
    /// fetch that reads up to 15 bytes past a proven start stays inside the
    /// padding. The bound covers private RAM only: anything above it (the gap,
    /// and U3's arena) needs [`RamBacking::is_backed`].
    #[inline]
    pub fn usable_len(&self) -> usize {
        self.ram.len() - FETCH_PADDING
    }

    /// Bytes of raw RAM generated code may read directly at a physical
    /// address (`MemoryCore::raw_ram_limit`); 0 means never inline.
    #[must_use]
    pub fn raw_ram_limit(&self) -> usize {
        self.core.raw_ram_limit()
    }

    /// Lower the direct-read RAM limit to at most `limit` until the next
    /// resize republishes it. Lowering only routes more reads to the checked
    /// helpers, so this is always safe; tests use it to force refusals.
    pub fn restrict_raw_ram_limit(&mut self, limit: usize) {
        self.core.lower_raw_ram_limit(limit);
    }
    /// Current broad memory-policy epoch.
    ///
    /// This includes translation-policy events such as TLB flushes. Generated
    /// direct-memory assumptions should use [`Self::direct_memory_policy_epoch`]
    /// instead so CR3/INVLPG churn does not force whole-cache rebuilds.
    #[inline]
    pub fn memory_policy_epoch(&self) -> u64 {
        self.core.memory_policy_epoch()
    }

    /// Current direct-memory epoch.
    ///
    /// The dynrec engine uses this to reject compiled direct-memory assumptions
    /// after memory mapping, source policy, ROM/MMIO/VGA routing, A20, or
    /// backing RAM size/pointer changes. TLB-only invalidations deliberately do
    /// not advance this epoch; paged code-fetch reuse must validate the current
    /// translated code page before running cached native blocks.
    #[inline]
    pub fn direct_memory_policy_epoch(&self) -> u64 {
        self.core.direct_memory_policy_epoch()
    }

    /// Current dynrec translation epoch.
    ///
    /// This advances on TLB-wide or single-page invalidation, CR3/paging policy
    /// churn routed through memory, A20 changes, and DPMI direct-mapping churn.
    /// It is intentionally narrower than [`Self::memory_policy_epoch`] so
    /// non-translation memory policy changes do not poison source-code identity.
    #[inline]
    pub fn dynrec_translation_epoch(&self) -> u64 {
        self.core.dynrec_translation_epoch()
    }

    /// History of CR4 full flushes that invalidate retained global mappings.
    #[inline]
    pub fn dynrec_global_tlb_flush_epoch(&self) -> u64 {
        self.core.dynrec_global_tlb_flush_epoch()
    }

    /// Current dynrec A20 generation.
    #[inline]
    pub fn dynrec_a20_generation(&self) -> u64 {
        self.core.alias_generation()
    }

    /// Apply the memory core's exact A20 physical-address masking rule.
    #[inline]
    pub fn dynrec_mask_physical_address(&self, addr: usize) -> usize {
        self.mask_address(addr)
    }

    // Every epoch bump below writes through to the stable `DirectGenerations`
    // block (dynrec-direct WI-C1) in the same helper: the block words mirror
    // the low 32 bits of the corresponding `u64` epoch, and every bump runs
    // the WI-G1 guarded-page sweep (`stale_all_guarded_pages`), which
    // advances word 0 (Rust-side diagnostic; no emitted readers) AND every
    // guarded per-page generation word — the words emitted guards read. A
    // missed write-through is a correctness bug — stale emitted proofs
    // would survive the event.

    #[inline]
    fn bump_memory_policy_epoch(&mut self) {
        self.core.bump_memory_policy_epoch()
    }

    #[inline]
    fn bump_direct_translation_policy_epoch(&mut self) {
        self.core.bump_direct_translation_policy_epoch()
    }

    #[inline]
    fn bump_translation_policy_epoch(&mut self) {
        self.core.bump_translation_policy_epoch()
    }

    #[inline]
    fn bump_a20_generation(&mut self) {
        self.core.bump_alias_generation()
    }

    /// EXP-N005: current PT-write tripwire epoch. See the field doc.
    #[inline]
    pub fn dynrec_pt_write_epoch(&self) -> u64 {
        self.core.dynrec_pt_write_epoch()
    }

    /// EXP-N006 amendment N2: this instance's process-unique nonce. See the
    /// field doc.
    #[inline]
    pub fn dynrec_instance_nonce(&self) -> u64 {
        self.core.dynrec_instance_nonce()
    }

    /// EXP-N005: current translation-structure epoch. See the field doc.
    #[inline]
    pub fn dynrec_translation_structure_epoch(&self) -> u64 {
        self.core.dynrec_translation_structure_epoch()
    }

    #[inline]
    fn flush_tlb_full(&mut self) {
        self.core.flush_tlb_full()
    }

    /// Set BIOS ROM write policy.
    ///
    /// Some legacy tests still write the public `allow_rom_writes` field
    /// directly, but production callers must use this setter so JIT profile
    /// invalidation sees the policy change.
    #[inline]
    pub fn set_allow_rom_writes(&mut self, allow: bool) {
        if self.allow_rom_writes != allow {
            self.allow_rom_writes = allow;
            self.bump_memory_policy_epoch();
            // TLB fill bakes the ROM-write policy into host_write publication
            // for the BIOS F-segment (see tlb_insert); flush so a policy flip
            // cannot leave stale write pointers (or stale null ones) behind.
            self.flush_tlb_full();
        }
    }

    /// Current BIOS ROM write policy.
    #[inline]
    pub fn allow_rom_writes(&self) -> bool {
        self.allow_rom_writes
    }

    pub fn bind_smc(&mut self, notifier: Option<Arc<dyn SmcNotify>>) {
        self.ram.set_hle_notifier(notifier.clone());
        self.core.bind_smc(notifier)
    }

    /// Returns true if a SMC notifier is installed (JIT is active).
    #[inline]
    /// Whether the bound SMC notifier is holding an unconsumed write record.
    ///
    /// A native engine that steps a nested interpreter instruction asks this
    /// after the step: a device reached by that instruction can write guest RAM
    /// holding compiled code, and such a write is only SNOOP-RECORDED — the
    /// invalidation lands later, at the engine root's drain. Without this the
    /// native chain could continue into a block whose source was already
    /// overwritten (MDD-BUG-FLUX-00146).
    ///
    /// Cheap and non-destructive: it does not drain, because draining is the
    /// root's job and taking the record here would lose it.
    pub fn smc_snoop_pending(&self) -> bool {
        self.core.smc_snoop_pending()
    }

    pub fn has_smc_notifier(&self) -> bool {
        self.core.has_smc_notifier()
    }

    /// Remove and return the SMC notifier, if any.
    /// Used by JIT verification to suppress SMC notifications during interpreter reference runs.
    pub fn take_smc_notifier(&mut self) -> Option<Arc<dyn SmcNotify>> {
        self.ram.set_hle_notifier(None);
        self.core.take_smc_notifier()
    }

    /// Update the fast SMC guard. Call with `true` when an execution engine
    /// may hold compiled code, `false` when no compiled blocks can exist.
    /// This avoids a vtable dispatch through `smc_notifier` on every memory
    /// write when no compiled blocks exist.
    #[inline]
    pub fn set_smc_has_code_pages(&mut self, has_pages: bool) {
        self.core.set_smc_has_code_pages(has_pages);
        self.smc_interest = has_pages || self.remote_arena_watch;
    }

    /// Some other process watches an arena page this process can store to
    /// (HLD §5.4). Keeps `smc_before_write`'s one-boolean gate in step.
    #[inline]
    fn set_remote_arena_watch(&mut self, remote: bool) {
        self.remote_arena_watch = remote;
        self.smc_interest = self.core.smc_has_code_pages() || remote;
    }

    /// Read the fast SMC guard. Interpreter-only runs must leave this false
    /// so `smc_before_write` stays a single bool test on the write hot path.
    #[inline]
    pub fn smc_has_code_pages(&self) -> bool {
        self.core.smc_has_code_pages()
    }

    pub fn new() -> Self {
        // Reserve the DOS-era ceiling, then expose the default usable RAM plus
        // FETCH_PADDING bytes of zeroed padding. The fast interpreter may read up
        // to 15 bytes ahead during decode; the padding guarantees those reads stay
        // within the allocation. Deliberately NOT `MAX_RAM_SIZE`: the 256..768 MiB
        // band exists only for flat-memory dynamic-high commits and is paid for lazily
        // by `expand_ram`'s over-capacity reallocation (WI-G1) when a guest
        // actually commits there — every DOS/16-bit workload keeps today's
        // footprint.
        //
        // `vec![0u8; N]` then `truncate` — NOT `with_capacity` + `resize`. The
        // latter reserves UNINITIALISED memory, so `resize` must zero-*write*
        // 128 MiB by hand; at the gate's `opt-level=0` that store loop dominated
        // `Emulator::new()` (~0.13s of a ~0.20s construction) and, multiplied by
        // the ~1,950 lib tests that build an emulator, was the single largest cost
        // in `cargo test --lib`. `vec![0u8; N]` specialises to `alloc_zeroed`, so
        // the allocator hands back OS zero-pages and nothing is touched until used.
        // `truncate` sets the length without shrinking capacity, so the pinned
        // ceiling (and the pointer-stability `ensure_ram_size` relies on) survives.
        //
        // `RamBacking::zeroed` keeps that shape: on 64-bit Unix it commits the
        // capacity inside a fixed host reservation, which the
        // kernel zero-fills on first touch just the same.
        let mut ram = RamBacking::zeroed(
            DOS_RAM_CEILING + FETCH_PADDING,
            DEFAULT_RAM_SIZE + FETCH_PADDING,
        );
        let instr_exec_pages = {
            let pages = DOS_RAM_CEILING.div_ceil(INSTR_EXEC_PAGE_SIZE);
            let words = pages.div_ceil(64);
            vec![0u64; words]
        };

        // Initialize BIOS data area with some defaults
        // BDA at 0x400-0x4FF
        ram[0x410] = 0x21; // Equipment list: 80x25 color, 1 floppy
        ram[0x411] = 0x00;
        ram[0x413] = 0x80; // Memory size in KB (low byte) = 640
        ram[0x414] = 0x02; // Memory size in KB (high byte)
        ram[0x449] = 0x03; // Current video mode (80x25 color text)
        ram[0x44A] = 80; // Columns
        ram[0x484] = 24; // Rows - 1

        // Initialize BIOS keyboard buffer pointers (0040:001A/001C) to the start of the
        // ring buffer at 0040:001E.
        ram[0x41A] = 0x1E;
        ram[0x41B] = 0x00;
        ram[0x41C] = 0x1E;
        ram[0x41D] = 0x00;
        // Keyboard buffer start/end pointers (0040:0080/0082). Many programs read these to
        // determine the ring bounds (BIOS uses end as an exclusive wrap marker).
        ram[0x480] = 0x1E;
        ram[0x481] = 0x00;
        ram[0x482] = 0x3E;
        ram[0x483] = 0x00;

        // Initialize BIOS ROM area with IRET stub at F000:FF00
        // This is done directly to bypass write protection
        let iret_addr = Self::seg_offset_to_linear(0xF000, 0xFF00);
        ram[iret_addr] = 0xCF; // IRET opcode

        let pinned_ram_capacity = ram.capacity();
        let planes_flat = vec![0u8; VGA_PLANES_SIZE]
            .into_boxed_slice()
            .try_into()
            .unwrap_or_else(|_| unreachable!("fixed VGA allocation has the requested size"));
        // DD-UNSAFE-1: capture the RAM base with WRITE provenance (from the
        // mapping itself, never a reference) so published direct-proof write
        // pointers are sound.
        let ram_base = RamBasePtr(ram.host_base());
        let mut memory = Self {
            ram,
            ems_pool: Vec::new(),
            ems_frame_map: [None; EMS_PHYSICAL_PAGES],
            hle_ems_frame_owned: false,
            // A20 gate starts enabled; set_a20(false) disables it (clears bit 20).
            a20_enabled: true,
            planes_flat,
            vga: VgaPipelineState {
                video_mode: 0x03, // Default text mode
                chain4: true,
                seq_odd_even_disabled: false,
                gc_odd_even_read: true,
                is_mode_x: false,
                plane_mask: 0x0F, // All planes enabled
                read_plane: 0,
                memory_map_select: 0, // Default before BIOS programs GC register 6
                gc_graphics_mode: false,
                write_mode: 0,  // Write mode 0 is default
                bit_mask: 0xFF, // All bits writable by default
                set_reset: 0,
                enable_set_reset: 0, // Disabled by default
                data_rotate: 0,      // No rotation, no logical op
                read_mode: 0,        // Read mode 0 is default
                color_compare: 0,
                color_dont_care: 0x0F, // All planes matter by default
            },
            // Default text mode 0x03 → Linear handler
            vga_write_handler: VgaWriteHandler::Linear,
            vga_native_commit: None,
            // EGA/VGA Graphics Controller state
            latches: Cell::new([0; 4]),
            allow_rom_writes: false,
            // ROM page bitmap covers the 1 MiB + HMA range at 4-KiB granularity.
            pending_option_rom_lock: None,
            option_rom_backing: Vec::new(),
            option_rom_base: 0,
            option_rom_enabled: false,
            // Starts empty; `mark_rom_*` sets entries. `has_rom_regions` stays
            // false until the first mark, giving zero-cost gating otherwise.
            rom_pages: vec![false; TOTAL_SIZE / ROM_PAGE_SIZE],
            has_rom_regions: false,
            high_mem_alias_enabled: false,
            firmware_mp_table_suppression_enabled: false,
            page_fault_pending: false,
            page_fault_address: 0,
            page_fault_error_code: 0,
            arena: Box::default(),
            physical_mappings: FixedVec::new(),
            video_write_count: std::sync::atomic::AtomicU64::new(0),
            native_video_writes: 0,
            vga_bus_write_count: 0,
            video_write_changed_count: std::sync::atomic::AtomicU64::new(0),
            video_write_unchanged_count: std::sync::atomic::AtomicU64::new(0),
            video_write_change_samples: Mutex::new(VideoWriteChangeSampleBuffer::default()),
            lfb: Vec::new(),
            lfb_dirty_pages: [0; LFB_DIRTY_WORDS],
            last_cr3: 0,
            lfb_decoder: LfbDecoderConfig::default(),
            ivt_corruption_logged: false,
            s3_legacy_aperture: S3LegacyApertureConfig::default(),
            interp_vga_chain4_fast_commits: 0,
            interp_tlb_device_reuse_hits: 0,
            span_write_bulk: [0; span_write_census::WIDTH_BUCKETS],
            span_write_per_byte: [0; span_write_census::WIDTH_BUCKETS],
            s3_mmio_enabled: false,
            s3_mmio_readback: vec![0xFF; S3_MMIO_READBACK_SIZE],
            pending_s3_mmio_reads: Mutex::new(Vec::new()),
            pending_s3_mmio_read: AtomicBool::new(false),
            pending_s3_mmio_writes: Vec::new(),
            s3_mmio_attention: AtomicBool::new(false),
            video_dirty_pages: std::sync::atomic::AtomicU16::new(0),
            vga_plane_rewrites: std::sync::atomic::AtomicU64::new(0),
            // Write tracking (disabled by default)
            write_context: None,
            pending_writes: Vec::new(),
            write_tracking_enabled: false,
            hle_handler_active: false,
            #[cfg(debug_assertions)]
            vga_pipeline_debug_dirty: false,
            planar_font_latch_pending: false,
            mmio_regions: Vec::new(),
            arena_published: Cell::new(false),
            remote_arena_watch: false,
            smc_interest: false,
            post_trace_enabled: false,
            post_trace_seen: RefCell::new(HashSet::new()),

            core: MemoryCore::new(
                ram_base,
                pinned_ram_capacity,
                instr_exec_pages,
                MAX_RAM_SIZE >> 12,
            ),
        };
        let ram_len = memory.ram.len();
        // SAFETY: `[0, ram.len())` is the committed private backing that
        // `ram_base` addresses; `resize_ram` lowers the limit before it can
        // shrink or move.
        unsafe { memory.core.set_raw_ram_limit(ram_len) };
        memory
    }

    /// Exchange the machine-owned physical state between two process memories.
    ///
    /// flat-memory guest processes keep private RAM, page tables, TLBs, direct-memory
    /// proofs, and instruction-cache state in separate [`Memory`] values while
    /// sharing one emulated machine. The process runner calls this only after
    /// both memories are quiescent and the native engine has been unbound. The
    /// CPU-side VGA plane pointer and any published translation/direct-memory
    /// views MUST be republished after this call; this method deliberately
    /// invalidates those derived assumptions.
    ///
    /// The VGA, EMS-frame, and ROM windows in `ram` are exchanged with the
    /// physical state as well. Ordinary RAM outside those windows remains
    /// private to each process. VGA's two backing views are reconciled before
    /// the exchange so the current write handler's source of truth survives a
    /// process switch. Between swaps, only the mounted memory may be used for
    /// guest execution or device servicing.
    pub fn swap_physical_state(&mut self, other: &mut Self) {
        assert_ne!(
            self.core.dynrec_instance_nonce(),
            other.core.dynrec_instance_nonce(),
            "a Memory cannot swap physical state with itself"
        );
        assert_eq!(
            self.rom_pages.len(),
            other.rom_pages.len(),
            "ROM page maps must have the fixed same size"
        );
        assert!(
            self.usable_len() >= VIDEO_BASE + VIDEO_SIZE
                && other.usable_len() >= VIDEO_BASE + VIDEO_SIZE
                && self.usable_len() >= EMS_FRAME_END
                && other.usable_len() >= EMS_FRAME_END,
            "physical state windows exceed one process memory"
        );

        self.sync_vga_mirror_for_physical_swap();
        other.sync_vga_mirror_for_physical_swap();

        let swap_ems_frame = self.hle_ems_frame_owned || other.hle_ems_frame_owned;
        if swap_ems_frame {
            self.swap_ram_range(other, EMS_FRAME_BASE, EMS_FRAME_SIZE);
        }

        // ROM bytes live in each process's RAM backing, while the protection
        // bitmap is part of the physical state. Exchange the bytes for the
        // union of both maps before exchanging the maps themselves so a state
        // newly marked ROM receives the canonical ROM image too.
        for page in 0..self.rom_pages.len() {
            if self.rom_pages[page] || other.rom_pages[page] {
                let start = page * ROM_PAGE_SIZE;
                if !swap_ems_frame
                    || start + ROM_PAGE_SIZE <= EMS_FRAME_BASE
                    || start >= EMS_FRAME_END
                {
                    self.swap_ram_range(other, start, ROM_PAGE_SIZE);
                }
            }
        }

        // The CPU-visible VGA window is a physical device aperture, even
        // though the current implementation mirrors it into each process's
        // RAM. Exchange that mirror alongside the plane backing.
        self.swap_ram_range(other, VIDEO_BASE, VIDEO_SIZE);

        macro_rules! swap_physical_fields {
            ($($field:ident),+ $(,)?) => {
                $(std::mem::swap(&mut self.$field, &mut other.$field);)+
            };
        }

        swap_physical_fields!(
            ems_pool,
            ems_frame_map,
            hle_ems_frame_owned,
            a20_enabled,
            planes_flat,
            vga,
            vga_write_handler,
            vga_native_commit,
            latches,
            allow_rom_writes,
            rom_pages,
            has_rom_regions,
            pending_option_rom_lock,
            option_rom_backing,
            option_rom_base,
            option_rom_enabled,
            high_mem_alias_enabled,
            firmware_mp_table_suppression_enabled,
            video_write_count,
            native_video_writes,
            vga_bus_write_count,
            video_write_changed_count,
            video_write_unchanged_count,
            video_write_change_samples,
            lfb,
            lfb_dirty_pages,
            lfb_decoder,
            s3_legacy_aperture,
            s3_mmio_enabled,
            s3_mmio_readback,
            pending_s3_mmio_reads,
            pending_s3_mmio_read,
            pending_s3_mmio_writes,
            s3_mmio_attention,
            video_dirty_pages,
            planar_font_latch_pending,
            mmio_regions,
        );

        #[cfg(debug_assertions)]
        std::mem::swap(
            &mut self.vga_pipeline_debug_dirty,
            &mut other.vga_pipeline_debug_dirty,
        );

        // `last_cr3` is a cache of the currently mounted CPU's address-space
        // root, not device state. A mount caller must republish the new CPU's
        // CR3 before asking the VGA device to synchronize an LFB.
        self.last_cr3 = 0;
        other.last_cr3 = 0;
        self.vga_native_commit = self.compute_vga_native_commit();
        other.vga_native_commit = other.compute_vga_native_commit();

        self.invalidate_after_physical_state_swap();
        other.invalidate_after_physical_state_swap();
        // Each side now holds the other's planes.
        self.mark_vga_plane_rewrites(VGA_PLANE_VRAM_ALL_PAGES);
        other.mark_vga_plane_rewrites(VGA_PLANE_VRAM_ALL_PAGES);
    }

    #[inline]
    fn sync_vga_mirror_for_physical_swap(&mut self) {
        match self.vga_write_handler {
            VgaWriteHandler::Linear | VgaWriteHandler::Chain4 => self.sync_chain4_to_planes(),
            VgaWriteHandler::Planar => self.sync_planes_to_chain4(),
        }
    }

    #[inline]
    fn swap_ram_range(&mut self, other: &mut Self, start: usize, len: usize) {
        let end = start
            .checked_add(len)
            .expect("physical state RAM range overflowed");
        assert!(
            end <= self.usable_len() && end <= other.usable_len(),
            "physical state RAM range exceeds one process memory"
        );
        let self_bytes = &mut self.ram[start..end];
        let other_bytes = &mut other.ram[start..end];
        for (self_byte, other_byte) in self_bytes.iter_mut().zip(other_bytes) {
            std::mem::swap(self_byte, other_byte);
        }
    }

    #[inline]
    fn invalidate_after_physical_state_swap(&mut self) {
        // Physical routing and backing pointers changed. Retire every TLB,
        // direct-proof and flat-view assumption before execution can resume.
        // The public invalidation hook also advances the A20 and map
        // generations used to invalidate cached translations.
        self.invalidate_all_direct_proofs();
        self.flush_tlb_full();
        self.clear_instr_exec_pages();
        self.core.set_instr_cache_dirty();
        self.core.bump_pt_write_epoch();
    }

    // ── RomRegions API ────────────────────────────────────────────────
    //
    // Used by the real-boot path to mark the system firmware/VGA firmware image
    // pages (0xC0000..=0xFFFFF) as write-protected so that stray guest
    // stores do not silently mutate the loaded ROM image. When no pages
    // are marked, `has_rom_regions` stays false and `is_rom_addr`
    // collapses to a single bool-compare — the non-real-boot fast paths
    // pay no cost.
    //
    // Cross-boundary writes (u16/u32 straddling a ROM/non-ROM boundary)
    // PARTIALLY COMMIT: the non-ROM bytes land and only the ROM bytes are
    // ignored. The multi-byte writers decompose to `write_u8` when the span
    // overlaps a ROM aperture, so each byte gets that aperture's point rule.
    //
    // This reverses an earlier "drop the whole transaction" policy, which
    // was justified as matching a memory controller that ignores the whole
    // access. It does not. `ROM_PAGE_SHIFT` is 12, so a straddle is by
    // construction an unaligned access, which a 386 itself splits into two
    // bus cycles against two adjacent aligned dwords with complementary
    // byte enables (BE0#-BE3#). The two cycles are decoded independently and
    // no bus mechanism lets the ROM decoder cancel the RAM decoder's cycle.

    /// Mark a single 4-KiB page (indexed from 0) in the 1 MiB + HMA
    /// range as ROM. Out-of-range indices are silently ignored.
    pub fn mark_rom_page(&mut self, page_index: usize) {
        if page_index < self.rom_pages.len() {
            let was_rom = self.rom_pages[page_index];
            if !was_rom {
                self.rom_pages[page_index] = true;
                self.has_rom_regions = true;
                self.bump_memory_policy_epoch();
            }
        }
    }

    /// Mark every 4-KiB page overlapping the inclusive byte range as ROM.
    /// Out-of-range bytes are silently clamped to the bitmap bounds.
    pub fn mark_rom_range(&mut self, range: std::ops::RangeInclusive<usize>) {
        let start_page = range.start() >> ROM_PAGE_SHIFT;
        let end_page = range.end() >> ROM_PAGE_SHIFT;
        let last = self.rom_pages.len().saturating_sub(1);
        let end_page = end_page.min(last);
        if start_page > end_page {
            return;
        }
        let mut changed = false;
        for page in start_page..=end_page {
            if !self.rom_pages[page] {
                self.rom_pages[page] = true;
                changed = true;
            }
        }
        if changed {
            self.has_rom_regions = true;
            self.bump_memory_policy_epoch();
        }
    }

    /// Arm the deferred option-ROM shadow lock: `range` will be marked ROM
    /// when [`Memory::apply_option_rom_lock`] runs (the emulator calls it at
    /// the guest BIOS's 440FX PAM commit — see the
    /// `pending_option_rom_lock` field doc for the PAM-timing rationale).
    pub fn arm_option_rom_lock(&mut self, range: std::ops::RangeInclusive<usize>) {
        self.pending_option_rom_lock = Some(range);
    }

    /// True while an armed option-ROM lock has not been applied yet.
    #[inline(always)]
    pub fn option_rom_lock_pending(&self) -> bool {
        self.pending_option_rom_lock.is_some()
    }

    /// Apply (and clear) the armed option-ROM lock. No-op when none is armed.
    pub fn apply_option_rom_lock(&mut self) {
        if let Some(range) = self.pending_option_rom_lock.take() {
            self.mark_rom_range(range);
        }
    }

    /// Replace the PCI expansion-ROM backing bytes without changing its BAR
    /// decode state. Guest writes never modify this backing.
    pub fn set_option_rom_backing(&mut self, data: &[u8]) {
        if self.option_rom_backing.as_slice() != data {
            self.option_rom_backing.clear();
            self.option_rom_backing.extend_from_slice(data);
            self.bump_memory_policy_epoch();
        }
    }

    /// Configure the live PCI expansion-ROM decoder atomically. Relocation
    /// removes the old decode immediately; disabling exposes the underlying
    /// RAM/open bus again.
    pub fn configure_option_rom_decoder(&mut self, base: usize, enabled: bool) {
        assert!(
            base.checked_add(self.option_rom_backing.len()).is_some(),
            "option-ROM decoder range overflows guest physical address space"
        );
        if self.option_rom_base != base || self.option_rom_enabled != enabled {
            self.option_rom_base = base;
            self.option_rom_enabled = enabled;
            self.bump_memory_policy_epoch();
        }
    }

    /// Current PCI expansion-ROM decoder state, including backing-derived size.
    pub fn option_rom_decoder(&self) -> OptionRomDecoderConfig {
        OptionRomDecoderConfig {
            base: self.option_rom_base,
            size: self.option_rom_backing.len(),
            enabled: self.option_rom_enabled,
        }
    }

    /// Immutable host view of the PCI expansion-ROM bytes.
    pub fn option_rom_backing(&self) -> &[u8] {
        &self.option_rom_backing
    }

    #[inline]
    fn option_rom_offset(&self, addr: usize) -> Option<usize> {
        if !self.option_rom_enabled {
            return None;
        }
        let offset = addr.checked_sub(self.option_rom_base)?;
        (offset < self.option_rom_backing.len()).then_some(offset)
    }

    /// True when `[addr, addr + len)` touches the VGA map window while
    /// `read_u8` routes that window to the planes rather than to `ram`
    /// (MDD-BUG-CRU-01475).
    ///
    /// The three routing conditions are exactly the ones `read_u8` tests
    /// before it reaches the planes: chain4 mode 13h, a planar mode, and the
    /// CPU sequential-planar read window. Outside them the window is plain RAM
    /// and a bulk copy is already byte-exact. The window itself moves with GC
    /// index 6, so it is asked for rather than assumed.
    ///
    /// Address first, mode second: this sits on the instruction-fetch path,
    /// and the widest window the GC can select is `VIDEO_SIZE` at
    /// `VIDEO_BASE`, so one range test rejects every read that is nowhere
    /// near it before any VGA state is consulted.
    fn vga_plane_window_overlaps(&self, addr: usize, len: usize) -> bool {
        if !range_overlaps(addr, len, VIDEO_BASE, VIDEO_SIZE) {
            return false;
        }
        let planes_are_live = (self.vga.video_mode == 0x13 && self.vga.chain4)
            || self.is_planar_mode()
            || self.is_cpu_sequential_planar_read();
        if !planes_are_live {
            return false;
        }
        let (base, size) = self.vga_memory_window();
        range_overlaps(addr, len, base, size)
    }

    /// True when any byte in the range is supplied by the enabled PCI
    /// expansion-ROM decoder rather than ordinary host-backed RAM.
    ///
    /// CPU fast paths use this as their admission check before dereferencing the
    /// raw RAM pointer. Keep the decoder state and backing-size calculation in
    /// Memory so every execution engine observes the same PCI mapping semantics.
    // This predicate sits on every interpreter memory fast path across a crate
    // boundary. Keep the disabled decoder to one folded boolean check in
    // non-S3 machines instead of paying a function call per guest access.
    #[inline(always)]
    pub fn option_rom_region_overlaps(&self, addr: usize, len: usize) -> bool {
        self.option_rom_enabled
            && range_overlaps(
                addr,
                len,
                self.option_rom_base,
                self.option_rom_backing.len(),
            )
    }

    /// Returns true if `addr` lies on a page that has been marked as ROM.
    /// Addresses outside the 1 MiB + HMA bitmap never match.
    #[inline(always)]
    pub fn is_rom_addr(&self, addr: usize) -> bool {
        if !self.has_rom_regions {
            return false;
        }
        let page = addr >> ROM_PAGE_SHIFT;
        // SAFETY-ish: bounds check preserves the fast path's lack of panics
        // for addresses outside the 1 MiB + HMA range (e.g., XMS, LFB).
        page < self.rom_pages.len() && self.rom_pages[page]
    }

    /// Returns true if any byte of the half-open range `[addr, addr+len)`
    /// falls on a ROM page. Used by multi-byte writes to decide they must
    /// decompose into per-byte `write_u8` calls, so that a span straddling a
    /// ROM boundary commits its non-ROM bytes (see the RomRegions API notes).
    #[inline]
    fn rom_region_overlaps(&self, addr: usize, len: usize) -> bool {
        if !self.has_rom_regions || len == 0 {
            return false;
        }
        let end = addr.saturating_add(len - 1);
        let start_page = addr >> ROM_PAGE_SHIFT;
        let end_page = end >> ROM_PAGE_SHIFT;
        let last = self.rom_pages.len();
        for page in start_page..=end_page {
            if page < last && self.rom_pages[page] {
                return true;
            }
        }
        false
    }

    // ── Real-boot high-memory alias ───────────────────────────────────
    //
    // On a real 386 the reset vector starts at CS=0xF000 with hidden
    // base=0xFFFF_0000 and EIP=0xFFF0, so the first fetch goes to
    // physical `0xFFFF_FFF0`. The chipset aliases the top 64 KiB of the
    // 4 GiB physical space onto the last 64 KiB of the BIOS ROM. We
    // reproduce that alias on `Memory` reads: folded address =
    // `0xF0000 + (addr - 0xFFFF_0000)`, which lands in the top 64 KiB of
    // the 128 KiB BIOS image loaded at `0xE0000..=0xFFFFF`. Writes in
    // the alias window are dropped — the window is ROM.

    /// Enable or disable the real-boot high-memory alias. Default is
    /// disabled so non-real-boot paths pay only a single bool compare.
    pub fn set_high_mem_alias_enabled(&mut self, enabled: bool) {
        if self.high_mem_alias_enabled != enabled {
            self.high_mem_alias_enabled = enabled;
            self.bump_memory_policy_epoch();
        }
    }

    /// Returns true if the high-memory alias is currently enabled.
    pub fn high_mem_alias_enabled(&self) -> bool {
        self.high_mem_alias_enabled
    }

    /// Enable or disable real-boot Intel MP table signature suppression.
    /// See the field comment for why this is tied to PIC-only real-boot.
    ///
    /// Arming or disarming is a READ-POLICY flip for every low-memory page:
    /// the filter is content-keyed, so no per-page admission can express it,
    /// and any already-published `host_read` (hashed entry or flat-projection
    /// view) would keep serving raw signature bytes that `read_u8` rewrites.
    /// Flush the TLB on BOTH transitions — the bounded live-tag walk nulls
    /// the projections — and let `tlb_insert` withhold low-memory `host_read`
    /// while armed (post-arm fills republish, so the flush alone is not
    /// enough). The epoch bump follows the `set_high_mem_alias_enabled` /
    /// `set_allow_rom_writes` policy-flip precedent.
    pub fn set_firmware_mp_table_suppression_enabled(&mut self, enabled: bool) {
        if self.firmware_mp_table_suppression_enabled != enabled {
            self.firmware_mp_table_suppression_enabled = enabled;
            self.bump_memory_policy_epoch();
        }
    }

    /// Returns true when guest-visible Intel MP table signatures are hidden.
    pub fn firmware_mp_table_suppression_enabled(&self) -> bool {
        self.firmware_mp_table_suppression_enabled
    }

    // ── HLE EMS mapped page frame ────────────────────────────────────

    pub fn init_ems_pool(&mut self, pages: u16) -> std::result::Result<(), EmsPoolError> {
        let size = (pages as usize)
            .checked_mul(EMS_PAGE_SIZE)
            .ok_or(EmsPoolError::SizeOverflow(pages))?;
        self.ems_pool.clear();
        self.ems_pool.resize(size, 0);
        self.clear_ems_frame_map();
        Ok(())
    }

    pub fn ems_pool_pages(&self) -> u16 {
        (self.ems_pool.len() / EMS_PAGE_SIZE).min(u16::MAX as usize) as u16
    }

    pub fn ems_pool_snapshot(&self) -> &[u8] {
        &self.ems_pool
    }

    pub fn restore_ems_pool_for_snapshot(
        &mut self,
        data: &[u8],
    ) -> std::result::Result<(), EmsPoolError> {
        if data.len() != self.ems_pool.len() {
            return Err(EmsPoolError::SnapshotSizeMismatch {
                expected: self.ems_pool.len(),
                actual: data.len(),
            });
        }
        self.ems_pool.copy_from_slice(data);
        self.invalidate_ems_aliases_for_pool_range(0, self.ems_pool.len());
        Ok(())
    }

    pub fn hle_ems_frame_owned(&self) -> bool {
        self.hle_ems_frame_owned
    }

    pub fn claim_hle_ems_frame(
        &mut self,
        base: usize,
        size: usize,
    ) -> std::result::Result<(), MemoryOwnerError> {
        if base != EMS_FRAME_BASE || size != EMS_FRAME_SIZE {
            return Err(MemoryOwnerError::InvalidHleEmsFrame { base, size });
        }
        if EMS_FRAME_END > self.usable_len() {
            return Err(MemoryOwnerError::OutOfRam);
        }
        if range_overlaps(base, size, VIDEO_BASE, VIDEO_SIZE) {
            return Err(MemoryOwnerError::OverlapsVideo);
        }
        if range_overlaps(base, size, BIOS_BASE, 0x10000) {
            return Err(MemoryOwnerError::OverlapsSystemBios);
        }
        if self.rom_region_overlaps(base, size) {
            return Err(MemoryOwnerError::OverlapsRom);
        }

        self.hle_ems_frame_owned = true;
        self.clear_ems_frame_map();
        // Claiming the frame flips `hle_ems_frame_owned`, which changes what
        // `direct_host_ram_range` returns for 0xE0000..0xF0000 (raw RAM ->
        // EMS-backed). Bump the direct-memory-policy epoch -- which internally
        // flushes the TLB (so pre-claim entries carrying direct pointers into
        // the frame are dropped) AND stales dynrec guarded pages -- so EVERY
        // consumer keyed on that epoch is invalidated, not just TLB-based fast
        // paths. A bare `flush_tlb_full()` would leave epoch-keyed proofs
        // stale: (a) the interpreter's LOADIP whole-page fetch-proof cache, and
        // (b) a dynrec direct-page proof published for a frame page BEFORE the
        // claim (`publish_direct_page_proof` only refuses frame pages while
        // *owned*). The claim fires once per boot, so the extra work is free.
        self.bump_memory_policy_epoch();
        Ok(())
    }

    pub fn clear_ems_frame_map(&mut self) {
        self.ems_frame_map = [None; EMS_PHYSICAL_PAGES];
        if self.hle_ems_frame_owned {
            self.invalidate_ems_frame_range(EMS_FRAME_BASE, EMS_FRAME_SIZE);
        }
    }

    pub fn ems_frame_map_snapshot(&self) -> [Option<u16>; EMS_PHYSICAL_PAGES] {
        self.ems_frame_map
    }

    /// Low-level frame-map hook for the HLE EMS manager/planning path.
    ///
    /// Production callers must keep logical EMS handle ownership outside
    /// `Memory` and use this only to commit the already-decided physical
    /// 16 KiB frame slot mapping. Unit tests call it directly to exercise
    /// the memory-core mapping behavior without constructing INT 67h state.
    pub fn set_ems_frame_page(
        &mut self,
        slot: usize,
        pool_page: Option<u16>,
    ) -> std::result::Result<(), EmsPoolError> {
        if !self.hle_ems_frame_owned {
            return Err(EmsPoolError::FrameNotOwned);
        }
        if slot >= EMS_PHYSICAL_PAGES {
            return Err(EmsPoolError::InvalidFrameSlot(slot));
        }
        if let Some(page) = pool_page {
            self.validate_ems_pool_page(page)?;
        }
        self.set_ems_frame_page_after_validation(slot, pool_page);
        Ok(())
    }

    fn set_ems_frame_page_after_validation(&mut self, slot: usize, pool_page: Option<u16>) {
        debug_assert!(slot < EMS_PHYSICAL_PAGES);
        debug_assert!(pool_page.is_none_or(|page| self.validate_ems_pool_page(page).is_ok()));
        self.ems_frame_map[slot] = pool_page;
        self.invalidate_ems_frame_range(EMS_FRAME_BASE + slot * EMS_PAGE_SIZE, EMS_PAGE_SIZE);
    }

    pub fn resolve_ems_frame_addr(&self, physical_addr: usize) -> EmsFrameResolution {
        if !self.hle_ems_frame_owned || !(EMS_FRAME_BASE..EMS_FRAME_END).contains(&physical_addr) {
            return EmsFrameResolution::NotFrame;
        }

        let frame_offset = physical_addr - EMS_FRAME_BASE;
        let slot = frame_offset / EMS_PAGE_SIZE;
        let slot_offset = frame_offset % EMS_PAGE_SIZE;
        let frame_addr = EMS_FRAME_BASE + frame_offset;
        match self.ems_frame_map[slot] {
            Some(pool_page) => EmsFrameResolution::Mapped {
                slot,
                pool_page,
                pool_offset: slot_offset,
                frame_addr,
            },
            None => EmsFrameResolution::Unmapped { frame_addr },
        }
    }

    #[inline]
    fn read_ems_frame_u8(&self, resolution: EmsFrameResolution) -> Option<u8> {
        match resolution {
            EmsFrameResolution::NotFrame => None,
            EmsFrameResolution::Unmapped { frame_addr } => {
                Some(self.ram.get(frame_addr).copied().unwrap_or(0xFF))
            }
            EmsFrameResolution::Mapped {
                pool_page,
                pool_offset,
                ..
            } => self.read_ems_backing_byte(pool_page, pool_offset).ok(),
        }
    }

    #[inline]
    fn write_ems_frame_u8(&mut self, physical_addr: usize, value: u8) -> bool {
        match self.resolve_ems_frame_addr(physical_addr) {
            EmsFrameResolution::NotFrame => false,
            EmsFrameResolution::Unmapped { frame_addr } => {
                let old_value = if self.write_tracking_enabled {
                    self.ram.get(frame_addr).copied().unwrap_or(0)
                } else {
                    0
                };
                self.mark_instr_cache_dirty_range(frame_addr, 1);
                self.smc_before_write(frame_addr, 1);
                self.snoop_descriptor_bytes(frame_addr, &[value]);
                if frame_addr < self.ram.len() {
                    self.ram[frame_addr] = value;
                }
                self.record_write(frame_addr, old_value as u64, value as u64, 1);
                true
            }
            EmsFrameResolution::Mapped {
                pool_page,
                pool_offset,
                ..
            } => {
                let Ok(old_value) = self.read_ems_backing_byte(pool_page, pool_offset) else {
                    return true;
                };
                let Ok(aliases) =
                    self.write_ems_backing_range_inner(pool_page, pool_offset, &[value], false)
                else {
                    return true;
                };
                for alias in aliases {
                    self.record_write(alias.frame_addr, old_value as u64, value as u64, 1);
                }
                true
            }
        }
    }

    #[inline]
    fn write_ems_aware_physical_u8_after_mask(&mut self, physical_addr: usize, value: u8) {
        if !self.write_ems_frame_u8(physical_addr, value) && physical_addr < self.usable_len() {
            self.ram[physical_addr] = value;
        }
    }

    pub fn read_ems_backing_byte(
        &self,
        pool_page: u16,
        offset: usize,
    ) -> std::result::Result<u8, EmsPoolError> {
        let index = self.ems_pool_index(pool_page, offset, 1)?;
        Ok(self.ems_pool[index])
    }

    pub fn write_ems_backing_byte(
        &mut self,
        pool_page: u16,
        offset: usize,
        value: u8,
    ) -> std::result::Result<(), EmsPoolError> {
        self.write_ems_backing_range(pool_page, offset, &[value])?;
        Ok(())
    }

    pub fn write_ems_backing_range(
        &mut self,
        pool_page: u16,
        offset: usize,
        data: &[u8],
    ) -> std::result::Result<EmsAliasRanges, EmsPoolError> {
        self.write_ems_backing_range_inner(pool_page, offset, data, true)
    }

    fn write_ems_backing_range_inner(
        &mut self,
        pool_page: u16,
        offset: usize,
        data: &[u8],
        track_alias_writes: bool,
    ) -> std::result::Result<EmsAliasRanges, EmsPoolError> {
        if data.is_empty() {
            self.validate_ems_pool_page(pool_page)?;
            if offset > EMS_PAGE_SIZE {
                return Err(EmsPoolError::InvalidOffset(offset));
            }
            return Ok(EmsAliasRanges::EMPTY);
        }

        let start = self.ems_pool_index(pool_page, offset, data.len())?;
        let end = start + data.len();
        let old_data = if track_alias_writes && self.write_tracking_enabled {
            Some(self.ems_pool[start..end].to_vec())
        } else {
            None
        };
        self.ems_pool[start..end].copy_from_slice(data);
        let aliases = self.ems_alias_ranges_for_pool_byte_range(start, data.len());
        for alias in aliases {
            self.mark_instr_cache_dirty_range(alias.frame_addr, alias.len);
            self.smc_before_write(alias.frame_addr, alias.len);
            // A descriptor table may sit in the page frame; the guest sees
            // these bytes at the frame alias, so snoop them there.
            let alias_page = (alias.frame_addr - EMS_FRAME_BASE) / EMS_PAGE_SIZE;
            if let Some(Some(page)) = self.ems_frame_map.get(alias_page).copied() {
                let alias_pool_start = page as usize * EMS_PAGE_SIZE
                    + (alias.frame_addr - EMS_FRAME_BASE) % EMS_PAGE_SIZE;
                let data_start = alias_pool_start - start;
                self.snoop_descriptor_bytes(
                    alias.frame_addr,
                    &data[data_start..data_start + alias.len],
                );
            }
        }
        if let Some(old_data) = old_data.as_deref() {
            self.record_ems_alias_writes_for_pool_write(start, data, old_data, &aliases);
        }
        Ok(aliases)
    }

    pub fn ems_alias_ranges_for_backing_range(
        &self,
        pool_page: u16,
        offset: usize,
        len: usize,
    ) -> std::result::Result<EmsAliasRanges, EmsPoolError> {
        if len == 0 {
            self.validate_ems_pool_page(pool_page)?;
            if offset > EMS_PAGE_SIZE {
                return Err(EmsPoolError::InvalidOffset(offset));
            }
            return Ok(EmsAliasRanges::EMPTY);
        }
        let start = self.ems_pool_index(pool_page, offset, len)?;
        Ok(self.ems_alias_ranges_for_pool_byte_range(start, len))
    }

    /// Return a raw mutable host pointer into RAM for direct-host fast paths.
    ///
    /// This is an escape hatch: callers must not use the pointer for
    /// guest-visible paths that require write tracking, SMC invalidation,
    /// other memory-side side effect. The checks below are a point-in-time
    /// filter only; cached pointers must be discarded when the memory map or
    /// RAM allocation changes.
    ///
    /// `#[inline]` is load-bearing for perf, not cosmetic: this is called once
    /// per retired instruction from the interpreter's fetch-window revalidation
    /// (`crates/gp64-interp/src/lib.rs`, the LOADIP fast-path epilogue) across
    /// a crate boundary. Without the hint, cross-crate inlining depends on LTO;
    /// empty-MMIO scan) into the caller and drop the call + argument marshalling.
    #[inline]
    pub fn direct_host_ram_range(&self, physical_addr: usize, len: usize) -> Option<NonNull<u8>> {
        let end = physical_addr.checked_add(len)?;
        if end > self.usable_len()
            && (physical_addr < arena::ARENA_PHYS_START
                || end > arena::ARENA_PHYS_END
                || !self.ram.is_backed(physical_addr..end))
        {
            return None;
        }
        if len > 0 {
            if self.hle_ems_frame_owned
                && range_overlaps(physical_addr, len, EMS_FRAME_BASE, EMS_FRAME_SIZE)
            {
                return None;
            }
            if range_overlaps(physical_addr, len, VIDEO_BASE, VIDEO_SIZE) {
                return None;
            }
            if self.lfb_range_overlaps(physical_addr, len) {
                return None;
            }
            if self.option_rom_region_overlaps(physical_addr, len) {
                return None;
            }
            if self.rom_region_overlaps(physical_addr, len) {
                return None;
            }

            if self.mmio_regions.iter().any(|region| {
                range_overlaps(physical_addr, len, region.phys_start, region.data.len())
            }) {
                return None;
            }
        }

        // SAFETY: the bounds above confine the pointer to usable private RAM
        // or a committed, admitted arena extent.
        debug_assert!(self.ram.is_backed(physical_addr..end));
        //
        // Derived from `ram_base`, the mutable-provenance base captured from
        // the mapping (`RamBacking::host_base`), never from a reference. This
        // function returns a pointer the escape-hatch contract lets callers
        // store through, and DD-UNSAFE-1 (`direct_proof::RamBasePtr`) records
        // that a store through a pointer derived from the shared `&self`
        // `Vec::as_ptr` provenance is undefined behaviour under the Rust
        // aliasing model (MDD-BUG-CRUCIBLE-00510).
        NonNull::new(unsafe { self.core.ram_base_ptr().add(physical_addr) })
    }

    /// True iff a store to `[physical_addr, physical_addr+len)` targets the plain
    /// chain-4 Mode 13h VGA framebuffer window and can be committed directly (via
    /// [`Memory::commit_vga_chain4_plain_byte`]) with a byte-for-byte result
    /// identical to the interpreter's chain-4 write handler.
    ///
    /// This is the dyncore native-commit admission gate for VGA stores (VGA
    /// chain4 native-store fast-path HLD). It is DELIBERATELY separate from
    /// [`Memory::direct_host_ram_range`] — which must keep rejecting the whole
    /// VGA window so no dynrec block is ever source-cached from it — and is
    /// tried only after the RAM check fails. It admits ONLY the "plain" pipeline
    /// state in which the dual-write reproduces the interpreter exactly:
    /// - the whole range lies inside the 64 KB `0xA0000` aperture;
    /// - the live CPU window maps that aperture (`memory_map_select & 3 ∈ {0,1}`;
    ///   select 2/3 route `0xA0000` elsewhere, so a dual-write would be wrong);
    /// - the cached write handler is [`VgaWriteHandler::Chain4`] — the single
    ///   source of truth `write_u8` dispatches on, so this equals the
    ///   interpreter's dispatch by construction (no `video_mode`/`is_mode_x`
    ///   re-derivation, no cross-crate `chain4 ⇔ !is_mode_x` dependency);
    /// - the GC pipeline is default pass-through: write-mode 0, no data-rotate /
    ///   logic-op, no set/reset, full bit-mask.
    ///
    /// Map-Mask (SR2) is intentionally NOT checked: chain-4 forces the plane via
    /// A1:A0 and ignores it, exactly as the interpreter's chain-4 arm does.
    pub fn vga_chain4_plain_store_range(&self, physical_addr: usize, len: usize) -> bool {
        // 64 KB 0xA0000 aperture only, overflow-safe (mirrors
        // `direct_host_ram_range`'s `checked_add`).
        if physical_addr < VIDEO_BASE {
            return false;
        }
        match physical_addr.checked_add(len) {
            Some(end)
                if end <= VIDEO_BASE + VIDEO_WINDOW_64K
                    && (len == 0 || ((end - VIDEO_BASE - 1) & !3) < PLANE_SIZE) => {}
            _ => return false,
        }
        if !matches!(self.vga.memory_map_select & 0x03, 0 | 1) {
            return false;
        }
        if self.vga_write_handler != VgaWriteHandler::Chain4 {
            return false;
        }
        self.vga.write_mode == 0
            && self.vga.data_rotate == 0
            && self.vga.enable_set_reset == 0
            && self.vga.bit_mask == 0xFF
    }

    pub fn vga_planar_plain_store_range(&self, physical_addr: usize, len: usize) -> bool {
        // 64 KB 0xA0000 aperture only, overflow-safe (mirrors the chain-4 predicate).
        if physical_addr < VIDEO_BASE {
            return false;
        }
        match physical_addr.checked_add(len) {
            Some(end) if end <= VIDEO_BASE + VIDEO_WINDOW_64K => {}
            _ => return false,
        }
        if !matches!(self.vga.memory_map_select & 0x03, 0 | 1) {
            return false;
        }
        if self.vga_write_handler != VgaWriteHandler::Planar {
            return false;
        }
        if !self.planar_font_latch_pending
            && !self.vga.chain4
            && !self.vga.gc_graphics_mode
            && self.vga.plane_mask == 0x04
        {
            return false;
        }
        self.vga.write_mode == 0
            && self.vga.data_rotate == 0
            && self.vga.enable_set_reset == 0
            && self.vga.bit_mask == 0xFF
    }

    /// True when a dynrec engine may cache generated code sourced from this physical
    /// range and rely on ordinary guest writes to notify the SMC tracker.
    ///
    /// This is narrower than "readable": VGA/video windows, enabled LFB,
    /// bytes through `read_u8`, but their write paths may bypass
    /// `smc_before_write`. Dynrec source caching is only sound for ordinary
    /// RAM bytes whose guest-visible writes flow through the notifier.
    pub fn dynrec_source_range_is_notifier_backed_ram(
        &self,
        physical_addr: usize,
        len: usize,
    ) -> bool {
        let Some(end) = physical_addr.checked_add(len) else {
            return false;
        };
        if end > self.usable_len() {
            return false;
        }
        if len == 0 {
            return true;
        }
        if self.hle_ems_frame_owned
            && range_overlaps(physical_addr, len, EMS_FRAME_BASE, EMS_FRAME_SIZE)
        {
            return false;
        }
        if range_overlaps(physical_addr, len, VIDEO_BASE, VIDEO_SIZE) {
            return false;
        }
        if self.lfb_range_overlaps(physical_addr, len) {
            return false;
        }
        if self.option_rom_region_overlaps(physical_addr, len) {
            return false;
        }
        if self.rom_region_overlaps(physical_addr, len) {
            return false;
        }
        if !self.allow_rom_writes && range_overlaps(physical_addr, len, BIOS_BASE, 0x10000) {
            return false;
        }

        if self
            .mmio_regions
            .iter()
            .any(|region| range_overlaps(physical_addr, len, region.phys_start, region.data.len()))
        {
            return false;
        }
        true
    }

    fn ems_pool_index(
        &self,
        pool_page: u16,
        offset: usize,
        len: usize,
    ) -> std::result::Result<usize, EmsPoolError> {
        self.validate_ems_pool_page(pool_page)?;
        if offset >= EMS_PAGE_SIZE && len != 0 {
            return Err(EmsPoolError::InvalidOffset(offset));
        }
        if offset > EMS_PAGE_SIZE {
            return Err(EmsPoolError::InvalidOffset(offset));
        }
        let start = pool_page as usize * EMS_PAGE_SIZE + offset;
        let end = start
            .checked_add(len)
            .ok_or(EmsPoolError::RangeOutOfBounds)?;
        if end > self.ems_pool.len() {
            return Err(EmsPoolError::RangeOutOfBounds);
        }
        Ok(start)
    }

    fn validate_ems_pool_page(&self, pool_page: u16) -> std::result::Result<(), EmsPoolError> {
        let start = pool_page as usize * EMS_PAGE_SIZE;
        if start < self.ems_pool.len() {
            Ok(())
        } else {
            Err(EmsPoolError::InvalidPoolPage(pool_page))
        }
    }

    fn ems_alias_ranges_for_pool_byte_range(
        &self,
        pool_start: usize,
        len: usize,
    ) -> EmsAliasRanges {
        let mut aliases = EmsAliasRanges::EMPTY;
        if len == 0 {
            return aliases;
        }
        let pool_end = pool_start + len;
        for (slot, mapped_page) in self.ems_frame_map.iter().copied().enumerate() {
            let Some(pool_page) = mapped_page else {
                continue;
            };
            let page_start = pool_page as usize * EMS_PAGE_SIZE;
            let page_end = page_start + EMS_PAGE_SIZE;
            let alias_start = pool_start.max(page_start);
            let alias_end = pool_end.min(page_end);
            if alias_start >= alias_end {
                continue;
            }
            aliases.push(EmsAliasRange {
                frame_addr: EMS_FRAME_BASE + slot * EMS_PAGE_SIZE + (alias_start - page_start),
                len: alias_end - alias_start,
            });
        }
        aliases
    }

    fn record_ems_alias_writes_for_pool_write(
        &mut self,
        pool_start: usize,
        new_data: &[u8],
        old_data: &[u8],
        aliases: &[EmsAliasRange],
    ) {
        for alias in aliases {
            let EmsFrameResolution::Mapped {
                pool_page,
                pool_offset,
                ..
            } = self.resolve_ems_frame_addr(alias.frame_addr)
            else {
                continue;
            };
            let alias_pool_start = pool_page as usize * EMS_PAGE_SIZE + pool_offset;
            let data_start = alias_pool_start.saturating_sub(pool_start);
            let mut byte_index = 0;
            while byte_index < alias.len {
                let data_index = data_start + byte_index;
                let available = old_data
                    .len()
                    .saturating_sub(data_index)
                    .min(new_data.len().saturating_sub(data_index))
                    .min(alias.len - byte_index);
                if available == 0 {
                    break;
                }
                let chunk_len = if available >= 4 {
                    4
                } else if available >= 2 {
                    2
                } else {
                    1
                };
                let old_value = pack_write_value(&old_data[data_index..data_index + chunk_len]);
                let new_value = pack_write_value(&new_data[data_index..data_index + chunk_len]);
                self.record_write(
                    alias.frame_addr + byte_index,
                    old_value,
                    new_value,
                    chunk_len as u8,
                );
                byte_index += chunk_len;
            }
        }
    }

    fn invalidate_ems_aliases_for_pool_range(&mut self, pool_start: usize, len: usize) {
        let aliases = self.ems_alias_ranges_for_pool_byte_range(pool_start, len);
        for alias in aliases {
            self.invalidate_ems_frame_range(alias.frame_addr, alias.len);
        }
    }

    fn invalidate_ems_frame_range(&mut self, frame_addr: usize, len: usize) {
        if len == 0 {
            return;
        }
        self.mark_instr_cache_dirty_range(frame_addr, len);
        self.smc_before_write(frame_addr, len);
        // A remap swaps the bytes a descriptor table in the frame reads.
        if self.has_descriptor_snoop() && self.descriptor_snoop_overlaps(frame_addr, len) {
            let (snoop_min, snoop_max) = self.core.snoop_hull();
            let start = frame_addr.max(snoop_min);
            let end = frame_addr.saturating_add(len).min(snoop_max);
            let visible: Vec<u8> = (start..end)
                .map(|addr| {
                    self.read_ems_frame_u8(self.resolve_ems_frame_addr(addr))
                        .unwrap_or(0xFF)
                })
                .collect();
            self.snoop_descriptor_bytes(start, &visible);
        }
        // An EMS remap changes the physical bytes behind an unchanged linear
        // address, so the flush must carry the translation-policy epoch with
        // it — same pairing as `invalidate_tlb`. Without it the flush is
        // invisible to every epoch consumer (the dyncore fetch-translation
        // memo, the dbx link keep), which would keep a translation whose
        // backing has just been swapped underneath it.
        self.flush_tlb_full();
        self.bump_translation_policy_epoch();
    }

    // ── Declared memory regions / POST tracer ─────────────────────────
    //
    // `declared_regions()` returns the labelled, non-overlapping regions
    // of guest physical memory that the emulator considers "normal".
    // The real-boot POST tracer uses this to decide whether an out-of-
    // region access is anomalous and should be logged.
    //
    // Region layout (always emitted):
    //   0x00000..=0x9FFFF   "conventional"
    //   0xA0000..=0xBFFFF   "video"
    //   0xC0000..=0xFFFFF   "bios-rom" if any ROM page has been marked,
    //                       else split into "umb" (0xC0000..=0xEFFFF)
    //                       and "system-bios-hle" (0xF0000..=0xFFFFF)
    //   0x100000..=0x10FFFF "hma"
    //   0xFFFF_0000..=0xFFFF_FFFF "bios-alias" (only when the
    //                              high-memory alias is enabled)

    /// Returns the labelled, non-overlapping regions of guest physical
    /// memory that the emulator considers "declared" (normal RAM, video
    /// memory, ROM, HMA, and the real-boot BIOS alias when enabled).
    pub fn declared_regions(&self) -> Vec<DeclaredRegion> {
        let mut regions: Vec<DeclaredRegion> = Vec::with_capacity(6);
        regions.push(DeclaredRegion {
            range: 0x00000..=0x9FFFF,
            label: "conventional",
        });
        regions.push(DeclaredRegion {
            range: 0xA0000..=0xBFFFF,
            label: "video",
        });
        if self.has_rom_regions {
            regions.push(DeclaredRegion {
                range: 0xC0000..=0xFFFFF,
                label: "bios-rom",
            });
        } else {
            regions.push(DeclaredRegion {
                range: 0xC0000..=0xEFFFF,
                label: "umb",
            });
            regions.push(DeclaredRegion {
                range: 0xF0000..=0xFFFFF,
                label: "system-bios-hle",
            });
        }
        regions.push(DeclaredRegion {
            range: 0x100000..=0x10FFFF,
            label: "hma",
        });
        if self.high_mem_alias_enabled {
            regions.push(DeclaredRegion {
                range: HIGH_MEM_ALIAS_BASE..=HIGH_MEM_ALIAS_END,
                label: "bios-alias",
            });
        }
        regions
    }

    /// Enable or disable the real-boot POST tracer. Clearing the flag
    /// also drops the dedup set so a subsequent re-enable starts fresh.
    pub fn set_post_trace_enabled(&mut self, enabled: bool) {
        self.post_trace_enabled = enabled;
        if !enabled {
            self.post_trace_seen.borrow_mut().clear();
        }
    }

    /// Returns true when the real-boot POST tracer is enabled.
    #[inline]
    pub fn post_trace_enabled(&self) -> bool {
        self.post_trace_enabled
    }

    /// Returns true if `addr` falls inside any declared region (fast
    /// path used by the tracer). Real-boot BIOS alias addresses match
    /// only when the alias is enabled — this is the same predicate the
    /// read path uses to fold those accesses.
    #[inline]
    fn addr_in_declared_region(&self, addr: usize) -> bool {
        if addr <= 0xFFFFF {
            return true;
        }
        if (0x100000..=0x10FFFF).contains(&addr) {
            return true;
        }
        if self.high_mem_alias_enabled && in_high_mem_alias(addr) {
            return true;
        }
        false
    }

    #[inline]
    fn should_hide_firmware_mp_signature_byte(&self, addr: usize) -> bool {
        if !self.firmware_mp_table_suppression_enabled {
            return false;
        }
        if addr >= LOW_MEMORY_LIMIT || (addr & 0x0F) != 0 {
            return false;
        }
        self.valid_mp_floating_pointer_at(addr) || self.valid_mp_config_table_at(addr)
    }

    #[inline]
    fn low_memory_signature_at(
        &self,
        addr: usize,
        signature: &[u8; INTEL_MP_SIGNATURE_LEN],
    ) -> bool {
        let Some(end) = addr.checked_add(INTEL_MP_SIGNATURE_LEN) else {
            return false;
        };
        end <= self.usable_len() && self.ram[addr..end] == signature[..]
    }

    fn hide_firmware_mp_signatures_in_read_buffer(&self, base: usize, dest: &mut [u8]) {
        if !self.firmware_mp_table_suppression_enabled || base >= LOW_MEMORY_LIMIT {
            return;
        }
        let end = base.saturating_add(dest.len()).min(LOW_MEMORY_LIMIT);
        let mut addr = (base + 0x0F) & !0x0F;
        while addr < end {
            if self.should_hide_firmware_mp_signature_byte(addr) {
                dest[addr - base] = 0x00;
            }
            addr = addr.saturating_add(0x10);
        }
    }

    fn mp_structure_checksum_valid(&self, addr: usize, len: usize) -> bool {
        let Some(end) = addr.checked_add(len) else {
            return false;
        };
        end <= self.usable_len()
            && end <= LOW_MEMORY_LIMIT
            && self.ram[addr..end]
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
                == 0
    }

    fn valid_mp_floating_pointer_at(&self, addr: usize) -> bool {
        if !self.low_memory_signature_at(addr, &INTEL_MP_FLOATING_POINTER_SIGNATURE) {
            return false;
        }
        let Some(len_byte_addr) = addr.checked_add(8) else {
            return false;
        };
        len_byte_addr < self.usable_len()
            && self.ram[len_byte_addr] == 1
            && self.mp_structure_checksum_valid(addr, INTEL_MP_FLOATING_POINTER_LEN)
    }

    fn valid_mp_config_table_at(&self, addr: usize) -> bool {
        if !self.low_memory_signature_at(addr, &INTEL_MP_CONFIG_TABLE_SIGNATURE) {
            return false;
        }
        let Some(length_end) = addr.checked_add(6) else {
            return false;
        };
        if length_end > self.usable_len() {
            return false;
        }
        let len = u16::from_le_bytes([self.ram[addr + 4], self.ram[addr + 5]]) as usize;
        len >= INTEL_MP_CONFIG_TABLE_HEADER_LEN && self.mp_structure_checksum_valid(addr, len)
    }

    pub fn scrub_firmware_mp_table_signatures(&mut self) -> FirmwareMpTableScrubResult {
        let mut result = FirmwareMpTableScrubResult::default();
        if !self.firmware_mp_table_suppression_enabled {
            return result;
        }

        let scan_end = self.usable_len().min(LOW_MEMORY_LIMIT);
        let mut addr = 0usize;
        while addr.saturating_add(INTEL_MP_SIGNATURE_LEN) <= scan_end {
            let scrub_floating_pointer = self.valid_mp_floating_pointer_at(addr);
            let scrub_config_table = self.valid_mp_config_table_at(addr);
            if scrub_floating_pointer || scrub_config_table {
                self.mark_instr_cache_dirty_range(addr, 1);
                self.smc_before_write(addr, 1);
                self.ram[addr] = 0x00;
                if scrub_floating_pointer {
                    result.floating_pointer_signatures += 1;
                }
                if scrub_config_table {
                    result.configuration_table_signatures += 1;
                }
            }
            addr = addr.saturating_add(0x10);
        }
        result
    }

    /// Log a POST-tracer event for an out-of-region guest memory access.
    /// Dedupes on `(page_base, direction)` so each 4-KiB page is reported
    /// at most once per direction.
    #[cold]
    fn post_trace_memory_event(&self, addr: usize, dir: u8, value: Option<u8>) {
        const PAGE_MASK: usize = !0xFFF;
        let page = addr & PAGE_MASK;
        let inserted = self.post_trace_seen.borrow_mut().insert((page, dir));
        if !inserted {
            return;
        }
        let dir_str = if dir == 0 { "read" } else { "write" };
        match value {
            Some(v) => {
                tracing::warn!(
                    target: "gp64::realboot::post_trace",
                    addr = format_args!("{:08X}", addr),
                    dir = dir_str,
                    value = format_args!("{:#04X}", v),
                    "unhandled guest memory access",
                );
            }
            None => {
                tracing::warn!(
                    target: "gp64::realboot::post_trace",
                    addr = format_args!("{:08X}", addr),
                    dir = dir_str,
                    "unhandled guest memory access",
                );
            }
        }
    }

    /// Test hook: current dedup-set size. Exposed for tests so they can
    /// verify that a second access to the same page does not re-log, and
    /// for real-boot POST smoke tests in other crates that need to count
    /// how many unique pages the tracer has seen.
    pub fn post_trace_seen_len(&self) -> usize {
        self.post_trace_seen.borrow().len()
    }

    /// Test hook: snapshot up to `max` tracer entries as `(page_base, dir)`
    /// tuples (`dir` is 0 for reads, 1 for writes). Order is not specified
    /// (HashSet iteration order); sorted by the caller if stability is
    /// needed. Used by real-boot POST smoke tests to dump the first few
    /// out-of-region pages the guest touched.
    pub fn post_trace_seen_sample(&self, max: usize) -> Vec<(usize, u8)> {
        let mut out: Vec<(usize, u8)> = self
            .post_trace_seen
            .borrow()
            .iter()
            .copied()
            .take(max)
            .collect();
        out.sort();
        out
    }

    // ── GDT write snooping API ────────────────────────────────────────

    /// Register the physical address range of the current GDT so that
    /// writes to it can be detected.  Called after LGDT or whenever the
    /// GDTR is updated.
    /// Get the GDT snoop base (physical address). 0 = not set.
    pub fn gdt_snoop_base(&self) -> usize {
        self.core.gdt_snoop_base()
    }
    /// Get the GDT snoop end (exclusive physical address). 0 = not set.
    pub fn gdt_snoop_end(&self) -> usize {
        self.core.gdt_snoop_end()
    }

    /// Get the LDT snoop base (physical address). 0 = not set.
    pub fn ldt_snoop_base(&self) -> usize {
        self.core.ldt_snoop_base()
    }

    /// Get the LDT snoop end (exclusive physical address). 0 = not set.
    pub fn ldt_snoop_end(&self) -> usize {
        self.core.ldt_snoop_end()
    }

    #[inline]
    pub fn has_descriptor_snoop(&self) -> bool {
        self.core.has_descriptor_snoop()
    }

    /// True when the physical span `[phys, phys + len)` intersects the
    /// combined GDT+LDT snoop hull — the same two-compare reject
    /// [`Self::snoop_gdt_write_ext`] itself applies first, so a span this
    /// returns `false` for would be a snoop no-op even if every byte went
    /// through the snooping write path. Lets bulk writers (the drc REP span
    /// batch) skip the snoop exactly when the snoop would have done nothing,
    /// instead of refusing whenever any table is registered — which in a
    /// PM32 guest is always.
    #[inline]
    pub fn descriptor_snoop_overlaps(&self, phys: usize, len: usize) -> bool {
        self.core.descriptor_snoop_overlaps(phys, len)
    }

    pub fn set_gdt_snoop_range(&mut self, base: u32, limit: u16) {
        self.core.set_gdt_snoop_range(base, limit)
    }

    pub fn set_ldt_snoop_range(&mut self, base: u32, limit: u32) {
        self.core.set_ldt_snoop_range(base, limit)
    }

    pub fn clear_ldt_snoop_range(&mut self) {
        self.core.clear_ldt_snoop_range()
    }

    /// Check and clear the GDT-dirty flag.  Returns `true` if at least
    /// one write landed in the GDT region since the last call.
    #[inline]
    pub fn take_gdt_dirty(&self) -> bool {
        self.core.take_gdt_dirty()
    }

    /// Check and clear the LDT-dirty flag.  Returns `true` if at least
    /// one write landed in the LDT region since the last call.
    #[inline]
    pub fn take_ldt_dirty(&self) -> bool {
        self.core.take_ldt_dirty()
    }

    /// Runs the descriptor snoop over `data` written at physical `addr`, in
    /// 8-byte pieces so arbitrarily long writes are covered.
    fn snoop_descriptor_bytes(&self, addr: usize, data: &[u8]) {
        if !self.has_descriptor_snoop()
            || addr >= self.core.snoop_hull().1
            || addr.saturating_add(data.len()) <= self.core.snoop_hull().0
        {
            return;
        }
        for (index, piece) in data.chunks(8).enumerate() {
            let value = piece
                .iter()
                .rev()
                .fold(0u64, |value, &byte| (value << 8) | u64::from(byte));
            self.snoop_gdt_write_ext(addr + index * 8, value, piece.len() as u8);
        }
    }

    /// Descriptor-table write snoop: checks whether any byte in
    /// `addr..addr+write_size` overlaps the snooped GDT or LDT region and
    /// marks the relevant table dirty. GDT writes also keep the existing
    /// diagnostic logging for upper descriptor bytes (access/flags/base_hi).
    /// `write_size` is the number of bytes being written (normally 1, 2, or 4).
    #[inline]
    pub fn snoop_gdt_write_ext(&self, addr: usize, val: u64, write_size: u8) {
        let write_len = usize::from(write_size).max(1);
        // Combined-hull fast-out: almost every guest write lands nowhere near
        // either descriptor table; two compares reject it before the
        // per-table range tests below. With no table snooped snoop_max is 0,
        // so every write exits here (same result as both per-table checks
        // failing). Writes in the gap between two distant tables pass the
        // hull and are rejected by the exact tests — correctness unchanged.
        if addr >= self.core.snoop_hull().1
            || addr.saturating_add(write_len) <= self.core.snoop_hull().0
        {
            return;
        }
        let write_end = addr.saturating_add(write_len);
        let value_byte = |byte_index: usize| -> u8 {
            if byte_index < 8 {
                ((val >> (byte_index * 8)) & 0xFF) as u8
            } else {
                0
            }
        };
        if self.core.snoop_descriptor_write(addr, write_len) {
            let gdt_write_start = addr.max(self.core.gdt_snoop_base());
            let offset_in_gdt = gdt_write_start - self.core.gdt_snoop_base();
            let skipped_prefix = gdt_write_start.saturating_sub(addr);
            let entry_index = offset_in_gdt / 8;
            let byte_in_entry = offset_in_gdt % 8;
            let gdt_write_end = write_end.min(self.core.gdt_snoop_end());
            let bytes_in_gdt = gdt_write_end.saturating_sub(gdt_write_start);
            let write_end = byte_in_entry + bytes_in_gdt;
            // Log writes to the upper bytes (access/flags/base_hi) of any GDT entry
            if write_end > 4 || byte_in_entry >= 4 {
                let csip = INTERP_WATCHPOINT_CSIP.load(Ordering::Relaxed);
                tracing::debug!(
                    target: "gp64::cpu::gdt_snoop",
                    "GDT_WRITE: entry {} (sel {:#06X}) byte {}..{} size={} val={:#X} at {:04X}:{:04X}",
                    entry_index,
                    entry_index * 8,
                    byte_in_entry,
                    write_end,
                    write_size,
                    val,
                    csip >> 16,
                    csip & 0xFFFF,
                );
            }
            // Detailed trace for byte 6 writes (flags/limit_hi byte containing D/B bit)
            if byte_in_entry <= 6 && write_end > 6 {
                let byte6_val = if byte_in_entry == 6 {
                    // Byte 6 is the low byte of the write value
                    value_byte(skipped_prefix)
                } else if write_size == 2 && byte_in_entry == 5 {
                    value_byte(skipped_prefix + 1)
                } else {
                    // wider write that covers byte 6
                    let byte_index_in_write = skipped_prefix + (6 - byte_in_entry);
                    value_byte(byte_index_in_write)
                };
                let has_db = (byte6_val & 0x40) != 0;
                // Only log entries with entry_index >= 8 (selector >= 0x40) to skip
                // initial GDT setup noise. Also dump the full GDT entry after the write.
                if entry_index >= 8 {
                    // Read the full 8-byte descriptor from memory after this write
                    let desc_addr = self.core.gdt_snoop_base() + entry_index * 8;
                    let mut desc = [0u8; 8];
                    for i in 0..8 {
                        if desc_addr + i < self.ram.len() {
                            desc[i] = self.ram[desc_addr + i];
                        }
                    }
                    // Patch in the value being written (since it may not be committed yet)
                    for i in 0..bytes_in_gdt {
                        let idx = byte_in_entry + i;
                        if idx < 8 {
                            desc[idx] = value_byte(skipped_prefix + i);
                        }
                    }
                    // Read interpreter's current CS:IP from the watchpoint atomics
                    let csip =
                        crate::INTERP_WATCHPOINT_CSIP.load(std::sync::atomic::Ordering::Relaxed);
                    let cs_val = (csip >> 16) & 0xFFFF;
                    let ip_val = csip & 0xFFFF;
                    let sidi =
                        crate::INTERP_WATCHPOINT_SIDI.load(std::sync::atomic::Ordering::Relaxed);
                    let si_val = (sidi >> 16) & 0xFFFF;
                    let di_val = sidi & 0xFFFF;
                    tracing::trace!(
                        target: "gp64::cpu::gdt_snoop",
                        "[GDT_BYTE6] entry {} (sel {:#06X}) byte6={:#04X} D/B={} ws={} desc=[{:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}] from {:04X}:{:04X} SI:{:04X} DI:{:04X}",
                        entry_index, entry_index * 8, byte6_val, if has_db { 1 } else { 0 }, write_size,
                        desc[0], desc[1], desc[2], desc[3], desc[4], desc[5], desc[6], desc[7],
                        cs_val, ip_val, si_val, di_val,
                    );
                }
            }
        }
    }

    /// Get the video write count for watchdog tracking.
    ///
    /// Sums the interpreter's atomic counter and the native routes' plain one.
    /// Both must be included or the watchdog reads "no VGA progress" for a
    /// guest whose rendering runs entirely on admitted native stores, and
    /// terminates it as `NoVgaWrites` (MDD-BUG-CRUCIBLE-00150).
    pub fn video_write_count(&self) -> u64 {
        self.video_write_count
            .load(std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(self.native_video_writes)
    }

    /// Cumulative CPU byte transactions through A0000h-BFFFFh.
    #[inline]
    pub fn vga_bus_write_count(&self) -> u64 {
        self.vga_bus_write_count
    }

    /// Engagement counter for the native VGA commit routes alone (unit:
    /// BYTES committed), excluding the interpreter's atomic counter. The
    /// dyncore fast-leg tests read this to prove a store took the native
    /// commit (delta == width) or the general route (delta == 0);
    /// [`Memory::video_write_count`] is the sum production consumers read.
    #[inline]
    pub fn native_video_writes(&self) -> u64 {
        self.native_video_writes
    }

    /// Fold in VGA writes committed by the emitted inline corridor, which
    /// writes `planes_flat`/`ram` from generated code and so cannot call the
    /// commit helpers. Called from the dyncore's per-dispatch mirror publish.
    pub fn add_native_video_writes(&mut self, count: u64) {
        self.native_video_writes = self.native_video_writes.wrapping_add(count);
        self.vga_bus_write_count = self.vga_bus_write_count.wrapping_add(count);
    }

    /// Count a VGA write performed by a native commit helper.
    #[inline]
    fn increment_native_video_writes(&mut self) {
        self.native_video_writes = self.native_video_writes.wrapping_add(1);
        self.vga_bus_write_count = self.vga_bus_write_count.wrapping_add(1);
    }

    /// EXP-IQ002 L2 engagement: one store committed by the interpreter's
    /// chain-4 fast arm (always-on; see the field doc).
    #[inline(always)]
    pub fn count_interp_vga_chain4_fast_commit(&mut self) {
        self.interp_vga_chain4_fast_commits = self.interp_vga_chain4_fast_commits.wrapping_add(1);
    }

    /// EXP-IQ002 L1 engagement: one store served by reusing the TLB-held
    /// device-page translation (always-on; see the field doc).
    #[inline(always)]
    pub fn count_interp_tlb_device_reuse(&mut self) {
        self.interp_tlb_device_reuse_hits = self.interp_tlb_device_reuse_hits.wrapping_add(1);
    }

    /// Cumulative EXP-IQ002 L2 chain-4 fast-arm commits.
    pub fn interp_vga_chain4_fast_commits(&self) -> u64 {
        self.interp_vga_chain4_fast_commits
    }

    /// Cumulative EXP-IQ002 L1 TLB device-translation reuses.
    pub fn interp_tlb_device_reuse_hits(&self) -> u64 {
        self.interp_tlb_device_reuse_hits
    }

    /// Returns whether changed-vs-unchanged video write stats are enabled.
    #[inline]
    pub fn video_write_change_stats_enabled(&self) -> bool {
        vga_write_change_stats_enabled()
    }

    /// Returns whether changed planar write samples are enabled.
    #[inline]
    pub fn video_write_change_sample_enabled(&self) -> bool {
        vga_write_change_sample_enabled()
    }

    /// Get changed/unchanged video write counters for diagnostics.
    #[inline]
    pub fn video_write_change_counts(&self) -> VideoWriteChangeCounts {
        VideoWriteChangeCounts {
            changed: self
                .video_write_changed_count
                .load(std::sync::atomic::Ordering::Relaxed),
            unchanged: self
                .video_write_unchanged_count
                .load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Snapshot changed planar write samples collected for diagnostics.
    #[inline]
    pub fn video_write_change_samples(&self) -> VideoWriteChangeSamples {
        let Ok(samples) = self.video_write_change_samples.lock() else {
            return VideoWriteChangeSamples::default();
        };
        VideoWriteChangeSamples {
            dropped: samples.dropped,
            samples: samples.samples.clone(),
        }
    }

    /// Clear changed planar write samples collected for the next diagnostic interval.
    #[inline]
    pub fn clear_video_write_change_samples(&self) {
        if let Ok(mut samples) = self.video_write_change_samples.lock() {
            samples.dropped = 0;
            samples.samples.clear();
        }
    }

    /// Peek at the dirty flag without clearing it (plain atomic load).
    /// Used by the fast path to avoid an expensive swap on every instruction.
    /// An HLE write into this process's own arena code sets a second bit on
    /// the backing (the write path keeps its one inline flag).
    #[inline]
    pub fn is_instr_cache_dirty(&self) -> bool {
        self.core.is_instr_cache_dirty() || self.ram.arena_code_dirty()
    }

    /// Check if code memory was modified since the last fetch.
    pub fn take_instr_cache_dirty(&self) -> bool {
        let written = self.core.take_instr_cache_dirty();
        self.ram.take_arena_code_dirty() | written
    }

    #[inline]
    pub fn mark_instr_exec_page(&mut self, addr: usize) {
        if (0x8000_0000..0xC000_0000).contains(&addr) {
            self.mark_arena_instr_exec_page(addr);
        }
        // The bitmap covers private RAM, and the arena once one is attached
        // (`attach_arena_store`). The legacy interpreter passes linear IPs, so
        // an address above that coverage marks nothing rather than growing it.
        if (addr >> INSTR_EXEC_PAGE_SHIFT) / 64 >= self.core.instr_exec_pages().len() {
            return;
        }
        self.core.mark_instr_exec_page(addr);
    }

    /// The (legacy) interpreter passes a linear IP, while writes test the
    /// bitmap with the physical address they store to. An arena page, the
    /// same physical page in every process, therefore also marks its physical
    /// bit, and counts as this process's arena watch (HLD §5.4:
    /// interpreter-only interest is a watch).
    #[cold]
    #[inline(never)]
    fn mark_arena_instr_exec_page(&mut self, addr: usize) {
        if !self.ram.has_arena_mapping() {
            return;
        }
        let physical_page = ((addr + FLAT_MEMORY_PHYS_BIAS) >> 12) as u32;
        if !self.arena.instr_watches.insert(physical_page) {
            return;
        }
        // The bitmap covers the arena once one is attached (`attach_arena_store`).
        if (physical_page as usize) / 64 < self.core.instr_exec_pages().len() {
            self.core
                .mark_instr_exec_page((physical_page as usize) << 12);
        }
        self.publish_arena_watch(physical_page);
    }

    #[inline]
    fn mark_instr_cache_dirty_range(&mut self, addr: usize, size: usize) {
        let ram_len = self.ram.len();
        // Private RAM and the arena are the only extents a store can mark
        // executed; a fixed array, so the hot store path never allocates.
        self.core.mark_instr_cache_dirty_range(
            || ram_len,
            || [0..ram_len, arena::ARENA_PHYS_RANGE].into_iter(),
            addr,
            size,
        );
    }

    /// See [`MemoryCore::store_would_dirty_instr_cache`].
    pub fn store_would_dirty_instr_cache(&self, addr: usize, size: usize) -> bool {
        self.core.store_would_dirty_instr_cache(addr, size)
    }

    fn clear_instr_exec_pages(&mut self) {
        self.core.clear_instr_exec_pages();
        for page in std::mem::take(&mut self.arena.instr_watches) {
            self.publish_arena_watch(page);
        }
    }

    // ========================================================================
    // Write Tracking Methods (for Time Machine debugging)
    // ========================================================================

    /// Enable write tracking. When enabled, memory writes are recorded
    /// in the `pending_writes` buffer for later flushing to `TimeMachine`.
    pub fn enable_write_tracking(&mut self) {
        if !self.write_tracking_enabled {
            self.write_tracking_enabled = true;
            // Tracking changes write-access policy: all stores must return to
            // the recording slow path. Retire both warm TLB host pointers and
            // emitted direct-write proofs published before tracking began.
            self.bump_memory_policy_epoch();
        }
    }

    /// Disable write tracking and clear any pending writes.
    pub fn disable_write_tracking(&mut self) {
        self.write_tracking_enabled = false;
        self.pending_writes.clear();
        self.write_context = None;
    }

    /// Check if write tracking is enabled.
    pub fn is_write_tracking_enabled(&self) -> bool {
        self.write_tracking_enabled
    }

    /// Set whether writes are coming from an HLE (High-Level Emulation) handler.
    ///
    /// When active, recorded writes will have `is_hle_handler` set to true,
    /// which allows Time Machine to distinguish between CPU instruction writes
    /// and HLE handler writes (DOS/BIOS emulation).
    pub fn set_hle_handler_active(&mut self, active: bool) {
        self.hle_handler_active = active;
    }

    /// Returns true if currently inside an HLE handler.
    pub fn is_hle_handler_active(&self) -> bool {
        self.hle_handler_active
    }

    /// Set the write context (CS:IP) for the next instruction's writes.
    /// Call this before executing each instruction.
    pub fn set_write_context(&mut self, cs: u16, ip: u32) {
        self.write_context = Some(WriteContext { cs, ip, regs: None });
    }

    /// Set the write context with an extended diagnostic register snapshot.
    ///
    /// Time Machine still records only CS:IP; the extra registers are emitted
    /// by opt-in memory diagnostics.
    pub fn set_write_context_with_regs(&mut self, cs: u16, ip: u32, regs: WriteRegisterContext) {
        self.write_context = Some(WriteContext {
            cs,
            ip,
            regs: Some(regs),
        });
    }

    /// Clear the write context after instruction execution.
    pub fn clear_write_context(&mut self) {
        self.write_context = None;
    }

    /// Get the current write context.
    pub fn write_context(&self) -> Option<WriteContext> {
        self.write_context
    }

    /// Drain and return all pending writes. Clears the internal buffer.
    pub fn drain_pending_writes(&mut self) -> Vec<PendingWrite> {
        std::mem::take(&mut self.pending_writes)
    }

    /// Get the number of pending writes.
    pub fn pending_write_count(&self) -> usize {
        self.pending_writes.len()
    }

    /// Record a memory write (internal helper called by `write_u8`/16/32).
    /// Only records if write tracking is enabled and context is set.
    /// Force non-inline to prevent stack overflow in release mode JIT callbacks.
    #[inline(never)]
    fn record_write(&mut self, addr: usize, old_value: u64, new_value: u64, size: u8) {
        if !self.write_tracking_enabled {
            return;
        }
        if let Some(ctx) = self.write_context {
            self.pending_writes.push(PendingWrite::new_with_hle(
                ctx.cs,
                ctx.ip,
                addr as u32,
                old_value,
                new_value,
                size,
                self.hle_handler_active,
            ));
        }
    }

    /// Record one bounded write-tracking event for a bulk RAM operation.
    fn record_bulk_write(
        &mut self,
        addr: usize,
        old_value: u64,
        new_value: u64,
        sample_size: u8,
        affected_len: usize,
    ) {
        if !self.write_tracking_enabled {
            return;
        }
        if let Some(ctx) = self.write_context {
            self.pending_writes.push(
                PendingWrite::new_with_hle(
                    ctx.cs,
                    ctx.ip,
                    addr as u32,
                    old_value,
                    new_value,
                    sample_size,
                    self.hle_handler_active,
                )
                .with_affected_len(affected_len.min(u32::MAX as usize) as u32),
            );
        }
    }

    /// Record a VGA planar write (internal helper called by planar write functions).
    /// Only records if write tracking is enabled and context is set.
    #[inline]
    fn record_vga_plane_write(&mut self, offset: usize, plane: u8, old_value: u8, new_value: u8) {
        if !self.write_tracking_enabled {
            return;
        }
        if let Some(ctx) = self.write_context {
            // Linear address is VIDEO_BASE + offset (the CPU-visible address)
            let linear_address = (VIDEO_BASE + offset) as u32;
            self.pending_writes.push(PendingWrite::new_with_vga(
                ctx.cs,
                ctx.ip,
                linear_address,
                old_value as u64,
                new_value as u64,
                1, // VGA planar writes are always byte-sized per plane
                plane,
            ));
        }
    }

    /// Increment the video write count (called on writes to video memory).
    #[inline]
    fn increment_video_write_count(&self) {
        self.video_write_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record whether a video write changed backing display state.
    #[inline]
    fn record_video_write_change(&self, changed: bool) {
        if !vga_write_change_stats_enabled() {
            return;
        }
        let counter = if changed {
            &self.video_write_changed_count
        } else {
            &self.video_write_unchanged_count
        };
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record one changed planar VGA byte write for diagnostics.
    #[inline]
    fn record_video_write_change_sample(
        &self,
        offset: usize,
        plane: u8,
        old_value: u8,
        new_value: u8,
    ) {
        if !vga_write_change_sample_enabled() || old_value == new_value {
            return;
        }
        let Ok(mut samples) = self.video_write_change_samples.lock() else {
            return;
        };
        if samples.samples.len() < VIDEO_WRITE_CHANGE_SAMPLE_LIMIT {
            samples.samples.push(VideoWriteChangeSample {
                source: self.write_context,
                offset: offset as u32,
                plane,
                old_value,
                new_value,
            });
        } else {
            samples.dropped = samples.dropped.saturating_add(1);
        }
    }

    /// Mark a video page as dirty (for SVGA sync optimization).
    /// Page is calculated relative to A0000h (16 x 4KB pages = 64KB).
    #[inline]
    pub fn mark_video_page_dirty(&self, addr: usize) {
        if (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&addr) {
            let page = (addr - VIDEO_BASE) >> 12; // 4KB pages
            if page < 16 {
                self.video_dirty_pages
                    .fetch_or(1 << page, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    #[inline]
    pub fn mark_all_video_pages_dirty(&self) {
        self.video_dirty_pages
            .fetch_or(0xFFFF, std::sync::atomic::Ordering::Relaxed);
    }

    /// Get and clear dirty video pages. Returns a 16-bit mask where each bit
    /// represents a 4KB page in the A0000-AFFFF range.
    #[inline]
    pub fn take_video_dirty_pages(&self) -> u16 {
        self.video_dirty_pages
            .swap(0, std::sync::atomic::Ordering::Relaxed)
    }

    /// Read the dirty video page mask without clearing it (diagnostics).
    #[inline]
    pub fn peek_video_dirty_pages(&self) -> u16 {
        self.video_dirty_pages
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record that plane cells changed without a `video_dirty_pages` mark.
    /// `vram_pages` has one bit per [`VGA_PLANE_VRAM_PAGE_SIZE`] of VRAM in
    /// the interleaved layout (cells `1024 * bit .. 1024 * (bit + 1)`).
    #[inline]
    pub fn mark_vga_plane_rewrites(&self, vram_pages: u64) {
        self.vga_plane_rewrites
            .fetch_or(vram_pages, std::sync::atomic::Ordering::Relaxed);
    }

    /// Mark the VRAM page that holds plane cell `offset`.
    #[inline]
    fn mark_vga_plane_cell_rewritten(&self, offset: usize) {
        let page = (offset * 4) / VGA_PLANE_VRAM_PAGE_SIZE;
        if page < 64 {
            self.mark_vga_plane_rewrites(1 << page);
        }
    }

    /// Store bytes that the S3 side (linear aperture, drawing engine) wrote
    /// to VRAM `[vram_offset, vram_offset + bytes.len())` while the VGA is in
    /// a VGA mode. A Trio64 has one VRAM: byte `v` below 256 KiB is plane
    /// `v & 3`, cell `v >> 2`. Under chain-4 the A000 mirror that mode 13h
    /// scanout reads takes only bytes at chain-addressable cells (`cell & 3 == 0`),
    /// at mirror offset `cell | plane` (the inverse chain-4 resync mapping).
    ///
    /// Records no dirty mark: the S3 already holds these bytes.
    pub fn store_s3_vram_to_planes(&mut self, vram_offset: usize, bytes: &[u8]) {
        let end = vram_offset.saturating_add(bytes.len()).min(VGA_PLANES_SIZE);
        if vram_offset >= end {
            return;
        }
        let chain4_mirror = self.vga_write_handler == VgaWriteHandler::Chain4;
        for (vram, &byte) in (vram_offset..end).zip(bytes) {
            let plane = vram & 3;
            let cell = vram >> 2;
            self.planes_flat[plane * PLANE_SIZE + cell] = byte;
            // Invert the chain-4 mapping only for cells chain-4 can address.
            // The other three cells in each group have no A000 mirror byte.
            if chain4_mirror && cell & 3 == 0 {
                self.ram[VIDEO_BASE + (cell | plane)] = byte;
            }
        }
        if chain4_mirror {
            // One conservative range over every mirror offset the run can
            // reach, instead of a per-byte mark.
            let first = (vram_offset >> 2) & !3;
            let last = ((end - 1) >> 2) | 3;
            self.mark_instr_cache_dirty_range(VIDEO_BASE + first, last + 1 - first);
        }
    }

    /// Get and clear the plane rewrites recorded by
    /// [`Self::mark_vga_plane_rewrites`].
    #[inline]
    pub fn take_vga_plane_rewrites(&self) -> u64 {
        self.vga_plane_rewrites
            .swap(0, std::sync::atomic::Ordering::Relaxed)
    }

    /// Read the plane rewrites without clearing them (diagnostics).
    #[inline]
    pub fn peek_vga_plane_rewrites(&self) -> u64 {
        self.vga_plane_rewrites
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Check if any video pages are dirty (quick check before sync).
    #[inline]
    pub fn has_dirty_video_pages(&self) -> bool {
        self.video_dirty_pages
            .load(std::sync::atomic::Ordering::Relaxed)
            != 0
    }

    /// Reset RAM to all zeros (for test isolation)
    pub fn reset_ram(&mut self) {
        // A process reset must not erase the session's shared section or its
        // page tables. Only this process's private extent is reset.
        let private_len = self.ram.len();
        self.ram[0..private_len].fill(0);
        // Re-initialize BIOS data area
        self.ram[0x410] = 0x21;
        self.ram[0x411] = 0x00;
        self.ram[0x413] = 0x80;
        self.ram[0x414] = 0x02;
        self.ram[0x449] = 0x03;
        self.ram[0x44A] = 80;
        self.ram[0x484] = 24;
        self.ram[0x41A] = 0x1E;
        self.ram[0x41B] = 0x00;
        self.ram[0x41C] = 0x1E;
        self.ram[0x41D] = 0x00;
        self.ram[0x480] = 0x1E;
        self.ram[0x481] = 0x00;
        self.ram[0x482] = 0x3E;
        self.ram[0x483] = 0x00;
        // IRET stub at F000:FF00
        let iret_addr = Self::seg_offset_to_linear(0xF000, 0xFF00);
        self.ram[iret_addr] = 0xCF;
        self.clear_instr_exec_pages();
        self.core.set_instr_cache_dirty();
        self.lfb_dirty_pages = [0; LFB_DIRTY_WORDS];
    }

    /// Get a reference to a specific plane
    #[inline]
    pub fn plane(&self, plane: usize) -> &[u8] {
        let start = plane * PLANE_SIZE;
        &self.planes_flat[start..start + PLANE_SIZE]
    }

    /// Get a mutable reference to a specific plane. The caller may rewrite
    /// any cell, so every VRAM page is recorded as rewritten.
    #[inline]
    pub fn plane_mut(&mut self, plane: usize) -> &mut [u8] {
        self.mark_vga_plane_rewrites(VGA_PLANE_VRAM_ALL_PAGES);
        let start = plane * PLANE_SIZE;
        &mut self.planes_flat[start..start + PLANE_SIZE]
    }

    /// Return the complete fixed-size VGA plane backing.
    #[inline]
    pub fn vga_planes(&self) -> &[u8; VGA_PLANES_SIZE] {
        self.planes_flat.as_ref()
    }

    /// Return a mutable fixed-size view without permitting resize or replacement.
    ///
    /// Records no plane rewrite: its production users fetch the base pointer
    /// for generated code, whose stores mark `video_dirty_pages` when folded.
    /// A caller that writes through the view itself must call
    /// [`Self::mark_vga_plane_rewrites`].
    #[inline]
    pub fn vga_planes_mut(&mut self) -> &mut [u8; VGA_PLANES_SIZE] {
        self.planes_flat.as_mut()
    }

    /// Replace all VGA planes only when the source proves the exact backing size.
    pub fn replace_vga_planes(
        &mut self,
        source: &[u8],
    ) -> std::result::Result<(), VgaPlanesSizeError> {
        if source.len() != VGA_PLANES_SIZE {
            return Err(VgaPlanesSizeError {
                actual: source.len(),
            });
        }
        self.planes_flat.copy_from_slice(source);
        self.mark_vga_plane_rewrites(VGA_PLANE_VRAM_ALL_PAGES);
        Ok(())
    }

    pub fn get_latches(&self) -> [u8; 4] {
        self.latches.get()
    }

    pub fn set_latches(&mut self, values: [u8; 4]) {
        self.latches.set(values);
    }

    /// Set the plane mask (Map Mask register, Sequencer index 2)
    pub fn set_plane_mask(&mut self, mask: u8) {
        self.vga.plane_mask = mask & 0x0F;
    }

    /// Set the write mode (Graphics Controller register 5, bits 0-1)
    pub fn set_write_mode(&mut self, mode: u8) {
        self.vga.write_mode = mode & 0x03;
    }

    /// Set the bit mask (Graphics Controller register 8)
    pub fn set_bit_mask(&mut self, mask: u8) {
        self.vga.bit_mask = mask;
    }

    /// Set the set/reset register (Graphics Controller register 0)
    pub fn set_set_reset(&mut self, value: u8) {
        self.vga.set_reset = value & 0x0F;
    }

    /// Set the enable set/reset register (Graphics Controller register 1)
    pub fn set_enable_set_reset(&mut self, value: u8) {
        self.vga.enable_set_reset = value & 0x0F;
    }

    /// Set the data rotate/function select register (Graphics Controller register 3)
    pub fn set_data_rotate(&mut self, value: u8) {
        self.vga.data_rotate = value;
    }

    /// Set the video mode (for testing/initialization)
    pub fn set_video_mode(&mut self, mode: u8) {
        let old_handler = self.vga_write_handler;
        let mode_changed = self.vga.video_mode != mode;
        self.vga.video_mode = mode;
        self.update_vga_write_handler();
        if mode_changed && self.vga_write_handler == old_handler {
            self.bump_memory_policy_epoch();
        }
    }

    /// Set VGA Graphics Controller memory map select (GC index 6 bits 2-3).
    pub fn set_vga_memory_map_select(&mut self, select: u8) {
        let select = select & 0x03;
        if self.vga.memory_map_select != select {
            self.vga.memory_map_select = select;
            self.bump_memory_policy_epoch();
        }
    }

    /// Current VGA Graphics Controller memory map select cached from the
    /// hardware pipeline (GC index 6 bits 2-3).
    #[inline]
    pub fn vga_memory_map_select(&self) -> u8 {
        self.vga.memory_map_select & 0x03
    }

    /// Apply a VGA pipeline state snapshot from the VGA hardware.
    /// Handles chain4-to-planar VRAM rearrangement on mode 0x13 transitions.
    ///
    /// **Invariant**: Must NOT reallocate any buffer (`planes_flat`, `ram`).
    /// The fast interpreter holds raw pointers to these across the call.
    pub fn apply_vga_pipeline(&mut self, state: &VgaPipelineState) {
        #[cfg(debug_assertions)]
        {
            self.vga_pipeline_debug_dirty = false;
        }
        let old = self.vga;
        let old_handler = self.vga_write_handler;

        if old.chain4 != state.chain4
            || old.video_mode != state.video_mode
            || old.is_mode_x != state.is_mode_x
        {
            tracing::info!(
                target: "gp64::diag::vga_dispatch",
                old_mode = format_args!("0x{:02X}", old.video_mode),
                new_mode = format_args!("0x{:02X}", state.video_mode),
                old_chain4 = old.chain4,
                new_chain4 = state.chain4,
                old_mode_x = old.is_mode_x,
                new_mode_x = state.is_mode_x,
                old_plane_mask = format_args!("0x{:X}", old.plane_mask),
                new_plane_mask = format_args!("0x{:X}", state.plane_mask),
                "apply_vga_pipeline: pipeline state transition"
            );
        }

        let old_memory_map_select = old.memory_map_select;
        self.vga = *state;
        self.update_vga_write_handler();
        if old_memory_map_select != state.memory_map_select
            || old.seq_odd_even_disabled != state.seq_odd_even_disabled
            || old.gc_odd_even_read != state.gc_odd_even_read
            || (old.video_mode != state.video_mode && old_handler == self.vga_write_handler)
        {
            self.bump_memory_policy_epoch();
        }
        let new_handler = self.vga_write_handler;

        // Keep the dual-buffer (`ram` vs `planes_flat`) coherent across
        // dispatch-handler transitions:
        //   - Linear writes → ram only; Linear reads → ram only.
        //   - Chain4 writes → both ram and planes_flat (in sync per byte);
        //     Chain4 reads → planes_flat.
        //   - Planar writes → planes_flat only; Planar reads → planes_flat.
        //
        // So when the *write* path was Linear (only ram is current) and the
        // *new* path reads or writes via planes_flat (Chain4 or Planar),
        // we must seed `planes_flat` from `ram` first. Symmetrically, when
        // the *write* path was Planar (only planes_flat is current) and the
        // *new* path reads via ram (Linear, or Chain4 render which uses
        // `video_memory()` -> ram), we must seed `ram` from `planes_flat`.
        //
        // Without this, the VGA firmware Mode-13h-init transient
        // (Linear during the brief `(0x13, !chain4)` window) leaves
        // `planes_flat` stale; subsequent Chain4 reads return zeros and
        // the rendered VRAM ends up corrupt.
        match (old_handler, new_handler) {
            (VgaWriteHandler::Linear, VgaWriteHandler::Chain4)
            | (VgaWriteHandler::Linear, VgaWriteHandler::Planar) => {
                self.sync_chain4_to_planes();
            }
            (VgaWriteHandler::Planar, VgaWriteHandler::Linear)
            | (VgaWriteHandler::Planar, VgaWriteHandler::Chain4) => {
                self.sync_planes_to_chain4();
            }
            _ => {}
        }
    }

    /// Mark that VGA pipeline state has changed and needs syncing.
    ///
    /// Call this after VGA register changes (e.g., `set_mode_bios`) and
    /// **before** `apply_vga_pipeline()`.  The assert in
    /// `write_planar_ega_masked` will fire if a planar VRAM write happens
    /// while the flag is still set.  No-op in release builds.
    #[cfg(debug_assertions)]
    #[inline]
    pub fn mark_vga_pipeline_dirty(&mut self) {
        self.vga_pipeline_debug_dirty = true;
    }

    /// Release-build stub — optimises away completely.
    #[cfg(not(debug_assertions))]
    #[inline(always)]
    pub fn mark_vga_pipeline_dirty(&mut self) {}

    /// Returns `true` once a guest planar VRAM write has matched the
    /// plane-2 font-upload pattern since this `Memory` was constructed.
    /// One-way: the flag never returns to `false` after set.
    ///
    /// Polled by `Vga::render` to propagate the latch into
    /// `Vga::custom_font_loaded`, since the memory subsystem cannot reach
    /// into the `Vga` struct directly.
    #[inline]
    pub fn planar_font_latch_pending(&self) -> bool {
        self.planar_font_latch_pending
    }

    #[inline]
    fn vga_memory_window(&self) -> (usize, usize) {
        match self.vga.memory_map_select & 0x03 {
            0 => (VIDEO_BASE, VIDEO_SIZE),                  // A0000-BFFFF (128K)
            1 => (VIDEO_BASE, VIDEO_WINDOW_64K),            // A0000-AFFFF (64K)
            2 => (TEXT_VIDEO_BASE_MONO, VIDEO_WINDOW_32K),  // B0000-B7FFF (32K)
            _ => (TEXT_VIDEO_BASE_COLOR, VIDEO_WINDOW_32K), // B8000-BFFFF (32K)
        }
    }

    #[inline]
    fn vga_window_offset(&self, addr: usize) -> Option<usize> {
        let (base, size) = self.vga_memory_window();
        let end = base.checked_add(size)?;
        if addr >= base && addr < end {
            Some(addr - base)
        } else {
            None
        }
    }

    /// Check if current mode is a planar (EGA/VGA) graphics mode.
    /// Includes both 16-color and monochrome planar modes.
    ///
    /// For 256-colour video, `is_mode_x` is the strict predicate
    /// (256-colour + !chain4 + CR9-or-CR17 reprogrammed). The bare
    /// `(video_mode == 0x13 && !chain4)` test would mistreat the transient
    /// state VGA firmware passes through while programming Mode 13h
    /// (chain-4 not yet enabled, but no Mode-X CRTC reprogramming either)
    /// as Mode X, producing speckled Planar-dispatch corruption when
    /// chain-4 is finally enabled and `sync_planes_to_chain4()` runs.
    ///
    /// `is_mode_x` is gated on `video_mode == 0x13` so a stale cache
    /// (e.g. after a non-pipeline `set_video_mode(0x03)`) doesn't
    /// mis-classify text modes as planar.
    #[inline]
    pub fn is_planar_mode(&self) -> bool {
        matches!(self.vga.video_mode, 0x0D..=0x12)
            || (self.vga.video_mode == 0x13 && self.vga.is_mode_x)
    }

    // The selected B800 CGA modes retain their packed/interleaved backing store.
    // Elsewhere, sequential CPU addressing follows the VGA register selectors.
    #[inline]
    fn uses_cga_packed_routing_compat(&self) -> bool {
        matches!(self.vga.video_mode, 0x04..=0x06) && (self.vga.memory_map_select & 0x03) == 0x03
    }

    #[inline]
    fn is_cpu_sequential_planar_write(&self) -> bool {
        !self.uses_cga_packed_routing_compat() && !self.vga.chain4 && self.vga.seq_odd_even_disabled
    }

    #[inline]
    fn is_cpu_sequential_planar_read(&self) -> bool {
        !self.uses_cga_packed_routing_compat() && !self.vga.chain4 && !self.vga.gc_odd_even_read
    }

    /// Recompute `vga_write_handler` from the current VGA pipeline state.
    ///
    /// Must be called after any change to `video_mode`, `chain4`, or
    /// `is_mode_x`. The handler is consulted per-byte in `write_u8_inner()`
    /// to avoid re-deriving the mode on every VGA write.
    #[inline]
    pub fn update_vga_write_handler(&mut self) {
        let old = self.vga_write_handler;
        self.vga_write_handler = if self.is_planar_mode() {
            VgaWriteHandler::Planar
        } else if self.vga.video_mode == 0x13 && self.vga.chain4 {
            VgaWriteHandler::Chain4
        } else {
            VgaWriteHandler::Linear
        };
        if self.vga_write_handler != old {
            self.bump_memory_policy_epoch();
        }
        // Recompute the dyncore fast-path arming alongside the handler — this is the
        // single funnel (`apply_vga_pipeline` → here) through which every admission
        // field changes in production.
        self.vga_native_commit = self.compute_vga_native_commit();
    }

    /// Return the current VGA write handler (for testing/diagnostics).
    #[inline]
    pub fn vga_write_handler(&self) -> VgaWriteHandler {
        self.vga_write_handler
    }

    /// Cached plane_mask- and address-independent VGA native-commit admission for the
    /// dyncore `smc_store` fast path (EXP-033 Stage 1). `Some(kind)` guarantees that
    /// EVERY store to the 0xA0000 aperture, under EVERY Map-Mask, commits byte-for-byte
    /// identically to the interpreter via `commit_vga_{planar,chain4}_plain_byte`.
    #[inline]
    pub fn vga_native_commit_kind(&self) -> Option<VgaCommitKind> {
        self.vga_native_commit
    }

    /// Recompute [`Memory::vga_native_commit_kind`]. This is the per-store predicate
    /// (`vga_{planar,chain4}_plain_store_range`) with the address dropped and made
    /// plane_mask-INDEPENDENT: it admits only when the per-store predicate holds for
    /// EVERY aperture address and EVERY Map-Mask. For chain-4 that is the plain-GC +
    /// mms state (chain-4 ignores the Map Mask). For planar it additionally excludes
    /// the whole font-upload-latch pattern (`!chain4 && !gc_graphics_mode` with the
    /// latch clear) — where the per-store predicate would reject `plane_mask==0x04` —
    /// so the fast path never commits a store the interpreter would treat as a font
    /// upload. Excluding `plane_mask` here is deliberate: it is read live at commit and
    /// changes constantly on the hot path, so it must never gate this cached flag.
    fn compute_vga_native_commit(&self) -> Option<VgaCommitKind> {
        // S3 front-decoder gate (EXP-IQ002 panel, lens 2): the S3 old-MMIO
        // decoder window IS the chain-4 aperture (S3_MMIO_BASE == VIDEO_BASE),
        // the S3 legacy aperture can redirect or block A-window bytes, and an
        // S3 linear window may be placed over the legacy hole. `write_u8`
        // consults all three BEFORE the VGA arm, so the kind arms only when
        // none of them can claim a byte of the 64 KiB A0000h window the
        // native commits cover. Every input is port-I/O reachable, and each
        // configure site recomputes this flag.
        if self.s3_mmio_enabled
            || !self.s3_legacy_aperture_passes_a000()
            || self.lfb_range_overlaps(VIDEO_BASE, VIDEO_WINDOW_64K)
        {
            return None;
        }
        let plain_gc = self.vga.write_mode == 0
            && self.vga.data_rotate == 0
            && self.vga.enable_set_reset == 0
            && self.vga.bit_mask == 0xFF;
        if !plain_gc || !matches!(self.vga.memory_map_select & 0x03, 0 | 1) {
            return None;
        }
        match self.vga_write_handler {
            VgaWriteHandler::Chain4 => Some(VgaCommitKind::Chain4),
            VgaWriteHandler::Planar => {
                if !self.planar_font_latch_pending && !self.vga.chain4 && !self.vga.gc_graphics_mode
                {
                    None
                } else {
                    Some(VgaCommitKind::Planar)
                }
            }
            VgaWriteHandler::Linear => None,
        }
    }

    /// Convert segment:offset to linear address (real mode only)
    #[inline]
    pub fn seg_offset_to_linear(segment: u16, offset: u16) -> usize {
        ((segment as usize) << 4) + (offset as usize)
    }

    // Note: translate_address, translate_address_full, translate_address_full_checked,
    // and translate_linear_checked methods have been moved to the root crate (MemoryExt trait) because
    // they depend on CPU types (Cpu, CpuError, PageFaultInfo). These are CPU operations
    // that happen to use memory, not memory operations.

    /// Register a physical mapping (linear -> physical).
    ///
    /// Returns `false` when a distinct mapping cannot fit, or when either the
    /// linear or the physical range would run past the end of the 32-bit
    /// address space (a physical range that did would fold back to physical 0
    /// partway through, MDD-BUG-CRU-01482). Replacing a mapping with the same
    /// linear base does not require another table slot.
    ///
    /// The end is computed in `u64` so a range finishing exactly at 4 GiB is
    /// representable: a page at `0xFFFF_F000` ends at `0x1_0000_0000`, which is
    /// legal, while `0xFFFF_FFFF` with size 2 is not (MDD-BUG-CRUCIBLE-00666).
    /// Rejection happens BEFORE the table is touched, so a bad request cannot
    /// evict the mapping that shares its linear base.
    pub fn add_physical_mapping(&mut self, linear: u32, physical: u32, size: u32) -> bool {
        if size == 0 {
            return true;
        }
        if u64::from(linear) + u64::from(size) > LINEAR_ADDRESS_SPACE_END
            || u64::from(physical) + u64::from(size) > LINEAR_ADDRESS_SPACE_END
        {
            return false;
        }
        // Capture the size of the mapping being replaced BEFORE dropping it —
        // the same thing `remove_physical_mapping` does below, and for the same
        // reason: once it is gone there is nothing left to say which pages it
        // had warmed.
        let replaced_size = self
            .physical_mappings
            .iter()
            .find(|mapping| mapping.linear == linear)
            .map(|mapping| mapping.size);
        if replaced_size.is_none() && self.physical_mappings.is_full() {
            return false;
        }
        self.physical_mappings.retain(|m| m.linear != linear);
        self.physical_mappings.push(PhysicalMapping {
            linear,
            physical,
            size,
        });
        // Flush the union of the OLD and NEW linear page ranges.
        //
        // Both share `linear`, so the union is whichever is larger. Flushing
        // only the new range left a SHRINKING replacement with live
        // translations for the old tail: a three-page mapping replaced by a
        // one-page one kept two warmed pages pointing at the old physical
        // base, and `bump_direct_translation_policy_epoch` does not clear the
        // hashed TLB. DPMI 1.0 map-physical and map-conventional pass
        // guest-selected page counts straight here, so a guest can drive the
        // shrink (MDD-BUG-KIL-01041).
        let flush_size = replaced_size.map_or(size, |replaced| replaced.max(size));
        self.invalidate_linear_span(linear, flush_size);
        self.bump_direct_translation_policy_epoch();
        true
    }

    /// Invalidates every cached translation for the pages of
    /// `[linear, linear + size)`, `size` nonzero.
    ///
    /// A span with more pages than the TLB has entries is flushed whole: a
    /// per-page walk cannot remove more translations than the TLB holds, and
    /// DPMI map-physical passes a guest-chosen size here, so the walk let one
    /// call drive up to 2^20 invalidations (MDD-BUG-CRU-01483).
    fn invalidate_linear_span(&mut self, linear: u32, size: u32) {
        let start_page = linear >> 12;
        let end_page = linear.saturating_add(size - 1) >> 12;
        if (end_page - start_page) as usize >= TLB_SIZE {
            self.flush_tlb_full();
            return;
        }
        for page in start_page..=end_page {
            self.core.tlb_invalidate_page_quiet(page);
        }
    }

    /// Remove a DPMI physical mapping by linear base
    pub fn remove_physical_mapping(&mut self, linear: u32) -> bool {
        // Find the mapping size before removing, so we can flush the right TLB pages
        let mapping_size = self
            .physical_mappings
            .iter()
            .find(|m| m.linear == linear)
            .map(|m| m.size);
        let before = self.physical_mappings.len();
        self.physical_mappings.retain(|m| m.linear != linear);
        let removed = before != self.physical_mappings.len();
        if removed {
            if let Some(size) = mapping_size {
                self.invalidate_linear_span(linear, size);
            }
            self.bump_direct_translation_policy_epoch();
        }
        removed
    }

    /// Look up a DPMI physical mapping (linear -> physical)
    /// Returns the physical address if a mapping exists, None otherwise
    #[inline]
    pub fn map_linear_physical(&self, linear: u32) -> Option<usize> {
        for mapping in &self.physical_mappings {
            // In `u64`: a `u32` saturating add pinned an end of 4 GiB back to
            // 0xFFFF_FFFF, so the exclusive `<` test rejected the mapping's own
            // final byte (MDD-BUG-CRUCIBLE-00666). `add_physical_mapping`
            // guarantees the end never exceeds 4 GiB, so this cannot overflow.
            let end = u64::from(mapping.linear) + u64::from(mapping.size);
            if linear >= mapping.linear && u64::from(linear) < end {
                let offset = linear - mapping.linear;
                // `add_physical_mapping` also keeps the physical end within
                // 4 GiB, so this cannot overflow (MDD-BUG-CRU-01482).
                return Some((mapping.physical + offset) as usize);
            }
        }
        None
    }

    /// Whether the guest has registered ANY DPMI physical mapping.
    ///
    /// A cheap precondition for the byte-granular translation walks that
    /// sub-page mappings force. Within a single linear page one PTE governs
    /// every byte, so paging alone cannot make logical-to-physical translation
    /// non-contiguous — a DPMI remap is the only source. Callers already
    /// clamped to a page can therefore skip the walk entirely when this is
    /// false, which is the common case.
    #[inline]
    pub fn has_physical_mappings(&self) -> bool {
        !self.physical_mappings.is_empty()
    }

    /// Return whether any byte in a wrapping linear range has a registered DPMI
    /// physical mapping.
    ///
    /// Checked multi-byte helpers use this to decide whether page-fragment
    /// translation is valid. DPMI mappings may start or end inside a page, so a
    /// helper that overlaps one must retain byte-granular translation.
    ///
    /// O(number of mappings), not O(len): each mapping is tested for overlap
    /// with the range in `u64`, and a range that wraps past 4 GiB is split at
    /// 2^32. Testing mappings avoids scanning every byte when a caller checks
    /// an entire code page (MDD-BUG-FLU-03025). A
    /// `len` of 4 GiB or more covers every linear address, so it is clamped
    /// there; `add_physical_mapping` stores no empty mapping and no mapping
    /// that runs past 4 GiB. `range_overlaps` is not reused: it saturates at
    /// the end of the address space instead of wrapping.
    ///
    /// Only the empty-table test is inline, as before: callers with no
    /// mapping (every PC route but DPMI remaps) keep the same shape.
    #[inline]
    pub fn linear_range_has_physical_mapping(&self, linear: u32, len: usize) -> bool {
        !self.physical_mappings.is_empty() && self.physical_mapping_overlaps(linear, len)
    }

    fn physical_mapping_overlaps(&self, linear: u32, len: usize) -> bool {
        if len == 0 {
            return false;
        }
        let start = u64::from(linear);
        let end = start + (len as u64).min(LINEAR_ADDRESS_SPACE_END);
        let low_end = end.min(LINEAR_ADDRESS_SPACE_END);
        let wrapped_end = end.saturating_sub(LINEAR_ADDRESS_SPACE_END);
        self.physical_mappings.iter().any(|mapping| {
            let mapping_start = u64::from(mapping.linear);
            let mapping_end = mapping_start + u64::from(mapping.size);
            (start < mapping_end && mapping_start < low_end) || mapping_start < wrapped_end
        })
    }

    // Note: translate_paging and translate_paging_checked methods have been moved
    // to the root crate (MemoryExt trait) because they depend on CPU types (Cpu, PageFaultInfo).
    // These are CPU operations that happen to use memory, not memory operations.

    /// Read a byte from physical memory (bypasses paging and LFB checks).
    ///
    /// low-level callers that read paging structures (page directory /
    /// table walks) — those structures live in RAM by construction and
    /// address falls inside the configured region.
    #[inline]
    pub fn read_u8_physical(&self, addr: usize) -> u8 {
        let addr = self.mask_address(addr);
        if let Some(value) = self.read_ems_frame_u8(self.resolve_ems_frame_addr(addr)) {
            return value;
        }
        if addr < self.usable_len()
            || (addr >= arena::ARENA_PHYS_START && self.ram.is_backed(addr..addr.saturating_add(1)))
        {
            self.ram[addr]
        } else {
            0
        }
    }

    /// Read a byte from physical RAM, returning `None` when the masked address
    /// is outside the usable backing allocation.
    #[inline]
    pub fn read_u8_physical_checked(&self, addr: usize) -> Option<u8> {
        let masked = self.mask_address(addr);
        if masked < self.usable_len()
            || (masked >= arena::ARENA_PHYS_START
                && self.ram.is_backed(masked..masked.saturating_add(1)))
        {
            Some(self.read_u8_physical(addr))
        } else {
            None
        }
    }

    /// Read a 32-bit value from physical memory (bypasses paging).
    ///
    /// [`Self::read_u8_physical`]: only paging-structure walks are
    /// expected to use this. Debug builds panic if any of the four
    /// bytes falls inside the configured region.
    #[inline]
    pub fn read_u32_physical(&self, addr: usize) -> u32 {
        let addr = self.mask_address(addr);
        if !(self.hle_ems_frame_owned && range_overlaps(addr, 4, EMS_FRAME_BASE, EMS_FRAME_SIZE))
            && addr + 3 >= self.usable_len()
            && !(addr >= arena::ARENA_PHYS_START
                && self.ram.is_backed(addr..addr.saturating_add(4)))
        {
            return 0;
        }
        u32::from_le_bytes([
            self.read_u8_physical(addr),
            self.read_u8_physical(addr.wrapping_add(1)),
            self.read_u8_physical(addr.wrapping_add(2)),
            self.read_u8_physical(addr.wrapping_add(3)),
        ])
    }

    /// Write a 32-bit value to physical memory (bypasses paging).
    ///
    /// callers of this `_physical` variant must still guarantee the
    /// address is not in the region: a paging-structure write that lands
    /// inside the BIOS image is always a bug. Debug builds panic if any
    /// of the four bytes falls inside the configured region.
    #[inline]
    pub fn write_u32_physical(&mut self, addr: usize, value: u32) {
        // Intentionally NOT gated by RomRegions. Callers are DMA and paging
        // (PTE/PDE writes). During real-boot POST paging is off, so the paging
        // callers do not fire; DMA for floppy/IDE targets RAM buffers, never
        // the ROM window. Leaving this ungated means any stray DMA descriptor
        // pointing into ROM surfaces loudly rather than being silently dropped.
        // The real-boot path protects ROM via the CPU-side write_u8/u16/u32
        // helpers only.
        let addr = self.mask_address(addr);
        let bytes = value.to_le_bytes();
        let full_write_in_bounds = addr.checked_add(4).is_some_and(|end| {
            (end <= self.usable_len()
                || (addr >= arena::ARENA_PHYS_START && end <= arena::ARENA_PHYS_END))
                && self.ram.is_backed(addr..end)
        });
        if full_write_in_bounds && self.has_descriptor_snoop() {
            self.snoop_gdt_write_ext(addr, value as u64, 4);
        }
        if self.hle_ems_frame_owned && range_overlaps(addr, 4, EMS_FRAME_BASE, EMS_FRAME_SIZE) {
            self.smc_before_write(addr, 4);
            // MDD-BUG-CRUCIBLE-00566: dyncore was told, the interpreter was
            // not. Page walks set PDE/PTE Accessed and Dirty bits through this
            // writer, and a legal (if pathological) guest can make an
            // executable page double as a live page-table page. A hardware A/D
            // update then changes bytes the interpreter still serves from its
            // instruction cache, because `instr_cache_dirty` was never set.
            // Every other RAM-mutating path in this file pairs the two calls;
            // this one only ever made the dyncore half.
            self.mark_instr_cache_dirty_range(addr, 4);
            for (i, byte) in bytes.iter().copied().enumerate() {
                self.write_ems_aware_physical_u8_after_mask(addr.wrapping_add(i), byte);
            }
            return;
        }
        if full_write_in_bounds {
            self.smc_before_write(addr, 4);
            // MDD-BUG-CRUCIBLE-00566: dyncore was told, the interpreter was
            // not. Page walks set PDE/PTE Accessed and Dirty bits through this
            // writer, and a legal (if pathological) guest can make an
            // executable page double as a live page-table page. A hardware A/D
            // update then changes bytes the interpreter still serves from its
            // instruction cache, because `instr_cache_dirty` was never set.
            // Every other RAM-mutating path in this file pairs the two calls;
            // this one only ever made the dyncore half.
            self.mark_instr_cache_dirty_range(addr, 4);
            self.ram[addr] = bytes[0];
            self.ram[addr + 1] = bytes[1];
            self.ram[addr + 2] = bytes[2];
            self.ram[addr + 3] = bytes[3];
            self.maybe_trace_phys_write_range(addr, 4, value);
        }
    }

    /// Invalidate the entire TLB (CR3 with PGE clear, CR0.PG/CR4 changes).
    pub fn invalidate_tlb(&mut self) {
        self.core.invalidate_tlb()
    }

    /// Flush all translations and record the global-mapping lifetime boundary.
    pub fn invalidate_tlb_global_flush(&mut self) {
        self.core.invalidate_tlb_global_flush()
    }

    /// Invalidate nonglobal translations on CR3 writes with CR4.PGE set.
    /// The epoch still advances: native premise and translation caches must
    /// revalidate against the new address space, including retained globals.
    pub fn invalidate_tlb_non_global(&mut self) {
        self.core.invalidate_tlb_non_global()
    }

    /// Retain the final paging-structure address without changing pointers or
    /// the derived flat views. This uses existing padding, preserving JIT ABI.
    pub fn mark_tlb_global(&mut self, linear_page: u32, backing_addr: u32, large: bool) {
        let leaf_permissions = (self.read_u32_physical(backing_addr as usize) & 6) as u8;
        self.core
            .mark_tlb_global(linear_page, backing_addr, large, leaf_permissions);
    }

    /// Original PTE/PDE for a cached global page, even after CR3 changes.
    pub fn tlb_global_source(&self, linear_page: u32) -> Option<(u32, bool)> {
        self.core.tlb_global_source(linear_page)
    }

    fn tlb_global_backing_matches(&self, linear_page: u32, backing: u32) -> bool {
        self.core.tlb_global_backing_matches(linear_page, backing)
    }

    /// Invalidate a specific TLB entry (called for INVLPG instruction). Inside a
    /// region a 4 MiB PSE page filled, that is every slice of the large page
    /// (MDD-BUG-CRU-01472).
    pub fn invalidate_tlb_page(&mut self, linear_addr: u32) {
        self.core.invalidate_tlb_page(linear_addr)
    }

    pub fn arena_pte_generation_seen(&self) -> u64 {
        self.arena.pte_generation_seen
    }

    pub fn set_arena_pte_generation_seen(&mut self, generation: u64) {
        self.arena.pte_generation_seen = generation;
    }

    /// Another process rewrote shared arena PTEs while this one was parked
    /// (HLD §5.3). Called at mount, after the admission token arrived.
    ///
    /// Every arena TLB entry, flat view and direct proof this `Memory`
    /// published was already retired when it gave the token up
    /// (`take_arena_admission`). The edit went through the other process's
    /// `Memory`, so this one's PT tripwire never saw it. Bump the PT-write
    /// epoch and drop any arena entry the TLB still holds, which moves the
    /// translation epoch.
    pub fn retire_foreign_arena_translations(&mut self) {
        self.invalidate_cached_arena_translations();
        self.core.bump_pt_write_epoch();
    }

    fn invalidate_cached_arena_translations(&mut self) {
        for index in 0..TLB_SIZE {
            let tag = self.core.tlb().entries[index].linear_page;
            if (0x80000..0xc0000).contains(&tag) {
                self.core.tlb_invalidate_page_quiet(tag);
            }
        }
        self.bump_translation_policy_epoch();
    }

    /// Inserts one 4 KiB slice of a 4 MiB PSE mapping, recording its region so
    /// INVLPG anywhere in the large page drops every slice.
    pub fn tlb_insert_large_page_slice(
        &mut self,
        linear_page: u32,
        physical_page: u32,
        writable: bool,
        user: bool,
        dirty: bool,
    ) {
        self.core.tlb_mark_large(linear_page);
        self.tlb_insert(linear_page, physical_page, writable, user, dirty);
    }

    /// Base address of the flat TLB projection block, for publication into
    /// `Cpu::jit_tlb_view`.
    ///
    /// Stable for this `Memory`'s whole lifetime. Emitted probes reach the
    /// CPL-3 views by adding `FLAT_VIEW_CPL3_DELTA` at publication time and the
    /// write views by a `disp32` of `FLAT_VIEW_WRITE_DELTA`.
    #[inline]
    pub fn flat_views_ptr(&self) -> *const u64 {
        self.core.flat_views_ptr()
    }

    /// **Oracle O3 — the forced-miss sweep.** Arm (or disarm) suppression of the
    /// flat TLB projection: the authoritative hashed table keeps filling normally,
    /// but nothing is published into the projection, so every emitted probe reads a
    /// null view and every native memory operation takes its slow exit.
    ///
    /// Arming also clears what is already published, so the effect is immediate
    /// rather than only applying to future fills.
    ///
    /// The point is to run a guest program by a completely different route —
    /// every memory access exits native code through the MEM-RESTART stub, is
    /// re-run by the interpreter, and re-enters — and require the final `Cpu` state
    /// to be bit-identical. That is the only oracle for MEM-RESTART clause 1
    /// (commit-last atomicity); the frame-poisoning oracle O2 is structurally blind
    /// to it, because a stub spilling POST-op state overwrites the poison with
    /// plausible-but-wrong values.
    ///
    /// Instrumented and test builds only; absent from production release.
    #[cfg(any(test, debug_assertions, feature = "test-instrumentation"))]
    pub fn set_flat_projection_suppressed(&mut self, on: bool) {
        self.core.set_flat_projection_suppressed(on);
    }

    /// Whether Oracle O3 flat-projection suppression is armed. Always `false`
    /// in builds where it cannot be armed. Read-only: for census and disarm
    /// predicates that must match what an emitted arm would be told.
    #[inline]
    pub fn flat_projection_suppressed(&self) -> bool {
        self.core.flat_projection_suppressed()
    }

    /// The hashed software-TLB slot `linear_page` indexes, tag unchecked: the
    /// caller compares `linear_page` itself, exactly as an emitted probe does.
    /// Unlike [`Self::tlb_lookup`] this records no TLB stats, so a diagnostic
    /// census may read it without moving any counter.
    #[inline]
    pub fn tlb_hashed_slot(&self, linear_page: u32) -> &TlbEntry {
        self.core.tlb_hashed_slot(linear_page)
    }

    /// Mark or unmark a physical page as holding dyncore-compiled code.
    ///
    /// See `SoftwareTlb::set_page_watched`. The dyncore drives this from its
    /// cache-page bucket transitions; the effect is that a watched page's write
    /// views read as a miss, so stores route to the checked-store trampoline
    /// where `smc_store` remains the decision authority.
    #[inline]
    pub fn set_page_watched(&mut self, physical_page: u32, on: bool) {
        if (arena::ARENA_PHYS_START >> 12..arena::ARENA_PHYS_END >> 12)
            .contains(&(physical_page as usize))
        {
            self.set_arena_page_watched(physical_page, on);
            return;
        }
        self.set_flat_page_watched(physical_page, on);
    }

    /// A dyncore watch on an arena page: recorded locally, published to the
    /// session watch count, and OR-ed with remote interest so local eviction
    /// never clears a remote watch (HLD §5.4 pieces 1-2).
    #[cold]
    #[inline(never)]
    fn set_arena_page_watched(&mut self, physical_page: u32, on: bool) {
        if on {
            self.arena.local_watches.insert(physical_page);
        } else {
            self.arena.local_watches.remove(&physical_page);
        }
        self.publish_arena_watch(physical_page);
    }

    /// Re-derive one arena page's watch after this process's own interest in
    /// it changed: the session count (while mounted), the flat-view write
    /// revocation (own OR remote), and whether HLE writes need the hook.
    fn publish_arena_watch(&mut self, physical_page: u32) {
        let own = self.arena.local_watches.contains(&physical_page)
            || self.arena.instr_watches.contains(&physical_page);
        if let Some((pid, state)) = &self.arena.smc {
            state
                .lock()
                .expect("arena SMC lock poisoned")
                .watch(*pid, physical_page, own);
        }
        self.set_flat_page_watched(
            physical_page,
            own || self.arena.remote_watches.contains(&physical_page),
        );
        self.refresh_arena_hle_hook();
    }

    /// Arm the `RamBacking` HLE-write hook only while this process is
    /// mounted and some process, this one included, watches an arena page;
    /// otherwise an HLE arena write pays one `Option` test.
    fn refresh_arena_hle_hook(&mut self) {
        let interested = !self.arena.local_watches.is_empty()
            || !self.arena.instr_watches.is_empty()
            || !self.arena.remote_watches.is_empty();
        let hook = self
            .arena
            .smc
            .as_ref()
            .filter(|_| interested)
            .map(|(pid, state)| (*pid, state.clone()));
        self.ram.set_arena_hle_smc(hook);
    }

    #[inline]
    fn set_flat_page_watched(&mut self, physical_page: u32, on: bool) {
        self.core.set_page_watched(physical_page, on);
    }

    /// Mount half of the process seam (HLD §5.4): publish this process's own
    /// arena watches to the session, take the union with every other
    /// process's (revoking flat-view write access on those pages), and
    /// arm the remote-interest gate. The caller then binds the native caches
    /// and replays the arena write log since this process's cursor.
    pub fn attach_arena_smc(&mut self, pid: u32, state: Arc<Mutex<arena_smc::ArenaSmc>>) {
        let remote: std::collections::BTreeSet<u32> = {
            let mut shared = state.lock().expect("arena SMC lock poisoned");
            for &page in self.arena.local_watches.union(&self.arena.instr_watches) {
                shared.watch(pid, page, true);
            }
            if !self.arena.smc_attached {
                // Nothing translated yet: earlier log entries are moot.
                self.arena.smc_attached = true;
                self.arena.smc_cursor = shared.cursor();
            }
            shared.remote_pages(pid).collect()
        };
        self.arena.smc = Some((pid, state));
        let old = std::mem::replace(&mut self.arena.remote_watches, remote);
        for page in old
            .union(&self.arena.remote_watches)
            .copied()
            .collect::<Vec<_>>()
        {
            self.set_flat_page_watched(
                page,
                self.arena.local_watches.contains(&page)
                    || self.arena.instr_watches.contains(&page)
                    || self.arena.remote_watches.contains(&page),
            );
        }
        let remote = !self.arena.remote_watches.is_empty();
        self.set_remote_arena_watch(remote);
        self.refresh_arena_hle_hook();
    }

    /// Unmount half: stop reporting watch changes (the session keeps this
    /// PID's watches as remote interest for the others) and move the cursor
    /// past this turn's own log entries. Only the mounted process appends,
    /// and its own stores, HLE writes and range events already reached its
    /// own caches through the local snoop and decode-cache flag.
    pub fn park_arena_smc(&mut self) {
        if let Some((_, state)) = self.arena.smc.take() {
            self.arena.smc_cursor = state.lock().expect("arena SMC lock poisoned").cursor();
        }
        self.ram.set_arena_hle_smc(None);
    }

    /// Arena pages this process watches (dyncore or interpreter): the flush
    /// set when the log wrapped past its cursor.
    pub fn arena_watched_pages(&self) -> Vec<u32> {
        self.arena
            .local_watches
            .union(&self.arena.instr_watches)
            .copied()
            .collect()
    }

    pub fn arena_smc_pid(&self) -> Option<u32> {
        self.arena.smc.as_ref().map(|(pid, _)| *pid)
    }

    pub fn mark_arena_code_dirty(&mut self, start: usize, size: usize) {
        self.mark_instr_cache_dirty_range(start, size);
    }

    pub fn arena_smc_cursor(&self) -> u64 {
        self.arena.smc_cursor
    }

    pub fn set_arena_smc_cursor(&mut self, cursor: u64) {
        self.arena.smc_cursor = cursor;
    }

    /// Is this physical page currently watched? (O-P1(c) cross-check.)
    #[inline]
    pub fn is_page_watched(&self, physical_page: u32) -> bool {
        self.core.is_page_watched(physical_page)
    }

    /// EXP-N005: record that the page walker read a PDE/PTE from this
    /// physical frame. See `SoftwareTlb::record_pt_frame` for the write-view
    /// divert this arms.
    ///
    /// Unprobeable frames — an HLE EMS overlay alias (walker-visible "PTE"
    /// bytes can change on an overlay remap with no tracked physical write)
    /// or a frame beyond the bitmap — bump `pt_write_epoch` on EVERY record
    /// instead of being tracked. That encodes amendment A5's sever-always
    /// without sticky poison state: every translation-epoch edge flushes the
    /// TLB (unchanged behavior), so a live translation that still traverses
    /// the unprobeable frame re-walks, re-records, and re-bumps before any
    /// link refills — every subsequent edge severs. Once the translation
    /// stops traversing it, bumps cease and keeps resume on their own.
    pub fn record_pt_frame(&mut self, physical_page: u32) {
        let phys_byte = self.mask_address((physical_page as usize) << 12);
        let page = (phys_byte >> 12) as u32;
        let unprobeable = (self.hle_ems_frame_owned
            && range_overlaps(phys_byte, ROM_PAGE_SIZE, EMS_FRAME_BASE, EMS_FRAME_SIZE))
            || !self.core.tlb().flat().pt_frame_representable(page);
        if unprobeable {
            self.core.note_unprobeable_pt_frame();
            return;
        }
        self.core.record_pt_frame(page);
    }

    /// EXP-N005: is this physical page a recorded PT frame? (Test/oracle
    /// cross-check.)
    #[inline]
    pub fn is_pt_frame_recorded(&self, physical_page: u32) -> bool {
        self.core.is_pt_frame_recorded(physical_page)
    }

    /// EXP-N005: has the page walker recorded any probeable paging-structure
    /// frame? DBX uses this as a conservative guard for emitted stores whose
    /// inline corridor cannot call the PT-write tripwire.
    #[inline]
    pub fn has_recorded_pt_frames(&self) -> bool {
        self.core.has_recorded_pt_frames()
    }

    /// See [`MemoryCore::has_recorded_pt_frame_in`].
    #[inline]
    pub fn has_recorded_pt_frame_in(&self, first_page: u32, end_page: u32) -> bool {
        self.core.has_recorded_pt_frame_in(first_page, end_page)
    }

    /// EXP-N005 amendment A3: physical u32 write for the page walker's
    /// accessed/dirty-bit write-backs — identical to `write_u32_physical`
    /// minus the PT tripwire classification. A/D write-backs OR-in status
    /// bits no walk conclusion reads, so they cannot stale a translation
    /// premise; classifying them would erode every keep (each HLE
    /// first-touch A-bit would sever). SMC notification and every other
    /// write side effect are the ordinary physical write's.
    #[inline]
    pub fn write_u32_physical_ad(&mut self, addr: usize, value: u32) {
        self.core.set_pt_ad_writeback(true);
        self.write_u32_physical(addr, value);
        self.core.set_pt_ad_writeback(false);
    }
}

impl Memory {
    /// EXP-N005: classify a physical write against the recorded PT frames and
    /// bump `pt_write_epoch` on overlap.
    ///
    /// Called by `smc_before_write` (the audited slow-path funnel) and
    /// directly by the dbx checked-write helper, whose pure-RAM commit writes
    /// `Memory::ram` without re-entering the funnel. Walker A/D write-backs
    /// are excluded via the `pt_ad_writeback` window (amendment A3): they
    /// OR-in status bits no walk conclusion reads, so they cannot stale a
    /// translation premise.
    #[inline]
    pub fn pt_tripwire_note_physical_write(&mut self, addr: usize, size: usize) {
        self.core.pt_tripwire_note_physical_write(addr, size)
    }

    /// `(watch_transitions, watch_slots_scanned, bias_zero_collisions)` —
    /// the §6 sweep-cost instruments and the T7 tripwire counter.
    #[inline]
    pub fn flat_projection_counters(&self) -> (u64, u64, u64) {
        self.core.flat_projection_counters()
    }

    /// **O-P1(a) + O-P4** over this `Memory`'s live translation set. Cheap
    /// (O(4096)); safe to call after every guest instruction.
    ///
    /// With arena admission the bound is the whole physical span, and every
    /// live host pointer must also lie on a backed page (private RAM or a
    /// committed arena page), so a pointer into the gap still fails.
    pub fn oracle_check_flat_projection(&self) -> std::result::Result<(), String> {
        let base = self.ram.host_base().cast_const();
        if !self.ram.has_arena_admission() {
            return self
                .core
                .tlb()
                .oracle_check_live_tags(base, self.usable_len());
        }
        self.core
            .tlb()
            .oracle_check_live_tags(base, arena::ARENA_PHYS_END)?;
        for entry in self.core.tlb().entries.iter() {
            if entry.linear_page == TLB_INVALID_TAG {
                continue;
            }
            for ptr in [entry.host_read, entry.host_write.cast_const()] {
                if ptr.is_null() {
                    continue;
                }
                let offset = ptr as usize - base as usize;
                if !self.ram.is_backed(offset..offset + ROM_PAGE_SIZE) {
                    return Err(format!(
                        "O-P4: page {:#x} host pointer at physical {offset:#x} is not backed",
                        entry.linear_page
                    ));
                }
            }
        }
        Ok(())
    }

    /// Monotonic counter of real flat-projection slot changes.
    ///
    /// A per-instruction oracle can compare this against its previous
    /// observation and skip the walk when they match: an unchanged projection
    /// cannot have become incoherent on its own. This trades no oracle strength
    /// for a large constant factor in the differential harness.
    #[inline]
    pub fn flat_mutation_epoch(&self) -> u64 {
        self.core.flat_mutation_epoch()
    }

    /// See [`flat_tlb::FlatTlbViews::mutation_epoch_ptr`]: capture per engine
    /// entry only; never cache across entries.
    #[inline]
    pub fn flat_mutation_epoch_ptr(&self) -> *const u64 {
        self.core.flat_mutation_epoch_ptr()
    }

    /// Idempotence probe for TLB warmers: is any READ view already published
    /// for this linear page? A nonzero entry proves a prior equivalent
    /// publish is in place, so re-warming would only churn the mutation
    /// epoch. NOT a translation answer — see [`flat_tlb::FlatTlbViews::entry`].
    #[inline]
    pub fn flat_read_published(&self, linear_page: u32) -> bool {
        self.core.flat_read_published(linear_page)
    }

    /// **O-P1(b)** — full 4 Mi-slot stale-survivor scan. Expensive; call at
    /// scenario boundaries.
    pub fn oracle_check_flat_no_stale_survivors(&self) -> std::result::Result<(), String> {
        self.core.tlb().oracle_check_no_stale_survivors()
    }

    /// **O-SMC-ANYINSTANT** — `watched(phys) ⇒ W views null` for every live tag.
    /// Cheap (O(4096)); safe after any single mutation. See the `SoftwareTlb`
    /// method for the rationale.
    pub fn oracle_check_smc_fold_any_instant(&self) -> std::result::Result<(), String> {
        self.core.tlb().oracle_check_smc_fold_any_instant()
    }

    /// Look up a page in the TLB.
    ///
    /// Returns `Some((physical_page, writable, user, dirty))` if the page is cached,
    /// `None` if not in TLB (requires page table walk).
    ///
    /// Used by CPU-side paging translation in the main crate's `MemoryExt` trait.
    #[inline]
    pub fn tlb_lookup(&self, linear_page: u32) -> Option<(u32, bool, bool, bool)> {
        self.core.tlb_lookup(linear_page)
    }

    /// Insert a page translation into the TLB.
    ///
    /// Precomputes host pointers from the current RAM allocation so the
    /// fast path can dereference directly without composing addresses.
    /// VGA-range pages get null pointers, forcing the slow path.
    ///
    /// Used by CPU-side paging translation after page table walk.
    #[inline]
    pub fn tlb_insert(
        &mut self,
        linear_page: u32,
        physical_page: u32,
        writable: bool,
        user: bool,
        dirty: bool,
    ) {
        let phys_byte = self.mask_address((physical_page as usize) << 12);
        let physical_page = (phys_byte >> 12) as u32;
        let host_accessible = self
            .direct_host_ram_range(phys_byte, ROM_PAGE_SIZE)
            .is_some();
        if host_accessible && phys_byte >= arena::ARENA_PHYS_START {
            self.arena_published.set(true);
        }
        // BIOS F-segment ROM semantics (the "F7 silent drop", see
        // prepare_write_addr): when ROM writes are disallowed, never publish
        // a host_write pointer for 0xF0000..0x100000 so every TLB write fast
        // path (interpreter u8/u16/u32 hit arms) falls back to the guarded
        // slow path that drops the write. Found by the EXP-B005 adversarial
        // panel; previously a writable+dirty F-seg mapping let warm-TLB
        // stores land in the HLE BIOS image. set_allow_rom_writes() flushes
        // the TLB so a policy flip can't leave stale write pointers behind.
        let host_write_allowed =
            self.allow_rom_writes || !range_overlaps(phys_byte, ROM_PAGE_SIZE, BIOS_BASE, 0x10000);
        // Firmware MP-table suppression is content-keyed, so `tlb_direct_read_ptr`
        // refuses globally while it is armed — and every read fast path built on
        // published `host_read` pointers (interpreter hit arms, the dyncore's
        // emitted flat-view probes) must refuse the same set. Withhold the read
        // pointer for any page the filter could rewrite: physical pages below
        // LOW_MEMORY_LIMIT while armed. The WRITE view is withheld for the
        // same set even though writes are never filtered: a published W entry
        // with a null R entry breaks the `W-published ⊆ R-published` invariant
        // that lets an RMW fast leg read through its write-view hit (the
        // EXP-A031 panel's Q1 counterexample — a raw read would bypass the
        // content filter). The cost is that A008-class store probes refuse
        // low memory while suppression is armed, a rare boot-window regime.
        let host_read_allowed =
            !(self.firmware_mp_table_suppression_enabled && phys_byte < LOW_MEMORY_LIMIT);
        // Descriptor-table snoop (MDD-BUG-FLUX-00152): a page overlapping the
        // GDT/LDT snoop hull must never carry a host_write pointer. Every write
        // fast path built on published `host_write` — the interpreter's warm-TLB
        // store arms and the drc's `tlb_direct_write_ptr` commit leg — stores
        // straight to guest RAM without calling `snoop_gdt_write_ext`, so a
        // published write pointer here means a descriptor edit lands with the
        // table never marked dirty and stale segment admissions left live. REP
        // MOVSD into a descriptor table is the canonical DPMI/extender idiom, so
        // this is reachable guest behaviour, not a corner case.
        //
        // This is the same enforcement the dbx DirectGenerations publish already
        // applies (see `snoop_gdt_write_ext`'s invariant note and
        // `set_gdt_snoop_range`'s "snooped pages must never be natively
        // writable"); the SoftwareTlb half was simply never wired up. Withholding
        // the pointer sends those stores down the guarded slow path, which does
        // snoop. The cost is bounded: descriptor tables are a few pages at most,
        // and `recompute_snoop_window` flushes only when the hull actually moves.
        let host_write_allowed =
            host_write_allowed && !self.descriptor_snoop_overlaps(phys_byte, ROM_PAGE_SIZE);
        let host_write_allowed = host_write_allowed && host_read_allowed;
        // Publication proof (HLD R2, LLD 2.2). `direct_host_ram_range(phys,
        // 4 KiB)` refuses the VGA window, the LFB, ROM and option-ROM regions,
        // `[0, usable_len())` — the backed private extent — or a committed
        // arena page this process holds admission for (HLD §5.2; PC only). So
        // a proof here is a committed RAM page whose backing offset is its
        // physical address (D2), which is what the VGA-window and RAM-bound
        // tests that used to sit inside `SoftwareTlb::insert` encoded.
        debug_assert!(self.ram.is_backed(0..self.usable_len()));
        debug_assert_eq!(self.core.ram_base_ptr(), self.ram.host_base());
        let direct = host_accessible.then(|| {
            DirectPage::new(
                BackingPage(physical_page),
                host_read_allowed,
                host_write_allowed,
            )
        });
        // SAFETY: see the publication proof above. The read and write
        // withholds split LLD 1.4 V1's intents, and the core derives the host
        // pointer from the same `ram_base` `direct_host_ram_range` uses.
        unsafe {
            self.core.tlb_fill(TlbFill {
                linear: LinearPage(linear_page),
                phys: PhysPage(physical_page),
                perms: PagePerms {
                    writable,
                    user,
                    dirty,
                },
                direct,
            });
        }
    }

    /// See [`MemoryCore::flat_null_cause`].
    pub fn flat_null_cause(
        &self,
        linear_page: u32,
        write: bool,
        cpl3: bool,
    ) -> flat_tlb::FlatNullCause {
        self.core.flat_null_cause(linear_page, write, cpl3)
    }

    /// Host pointer for a read that a caller may serve with a plain host load
    /// instead of running a full checked-memory transaction.
    ///
    /// `Some(ptr)` is returned only when ALL of these hold:
    ///
    /// * `[linear, linear + width)` lies inside ONE 4 KiB page, so a single TLB
    ///   entry describes the whole access;
    /// * no content-dependent read filter is armed (see below);
    /// * the software TLB holds a live entry for that page whose `host_read` is
    ///   non-null. [`Memory::tlb_insert`] publishes that pointer only when
    ///   [`Memory::direct_host_ram_range`] admits the WHOLE page, i.e. the page
    ///   is ordinary in-bounds RAM overlapping no VGA window, LFB aperture,
    ///   registered MMIO region — and `SoftwareTlb::insert` additionally nulls
    ///   the pointer for the VGA physical range outright;
    /// * the entry is reachable at the caller's CPL.
    ///
    /// This is deliberately the SAME predicate `gp64-interp`'s
    /// `tlb_translate_read` and the dyncore's emitted flat-view read probe
    /// already use on every guest read; it is expressed once here so a Rust
    /// caller reuses it instead of reimplementing the TLB hash and the nullity
    /// contract.
    ///
    /// The firmware MP-table filter is the one thing a per-page admission cannot
    /// express: it rewrites individual paragraph-aligned low-memory bytes chosen
    /// by their *content*, on pages that are otherwise ordinary RAM. It is
    /// therefore refused globally here rather than left to a page test that
    /// cannot see it.
    ///
    /// Callers must not cache the result: it is valid only until the next
    /// mutation of `Memory`.
    #[inline(always)]
    pub fn tlb_direct_read_ptr(&self, linear: u32, width: usize, cpl3: bool) -> Option<*const u8> {
        if width == 0 || (linear as usize & 0xFFF) + width > 0x1000 {
            return None;
        }
        if self.firmware_mp_table_suppression_enabled {
            return None;
        }
        // SAFETY: the check above keeps `[linear, linear + width)` in one page.
        unsafe { self.core.tlb_direct_read_ptr_in_page(linear, cpl3) }
    }

    /// Host pointer for a write that a dyncore caller may commit directly.
    ///
    /// This is the write-side peer of [`Self::tlb_direct_read_ptr`]. `Some`
    /// requires one warm, writable, already-dirty ordinary-RAM page at the
    /// caller's CPL. A physical page watched by the dyncore is refused even
    /// though the authoritative hashed entry retains its host pointer; the
    /// flat projection folds the same watch into write-view nullity.
    ///
    /// Callers must commit immediately and must not cache the pointer. Any
    /// mutable memory operation can invalidate the translation or arm a watch.
    #[inline]
    pub fn tlb_direct_write_ptr(&self, linear: u32, width: usize, cpl3: bool) -> Option<*mut u8> {
        self.tlb_direct_write_span(linear, width, cpl3)
            .map(|(pointer, _physical)| pointer)
    }

    /// See [`MemoryCore::tlb_direct_write_span`].
    #[inline]
    pub fn tlb_direct_write_span(
        &self,
        linear: u32,
        width: usize,
        cpl3: bool,
    ) -> Option<(*mut u8, usize)> {
        self.core.tlb_direct_write_span(linear, width, cpl3)
    }

    /// Returns a raw pointer to the software-TLB entries array.
    ///
    /// The pointer is stable while the `Memory` allocation is alive because the
    /// TLB entries are stored inline, not in a relocatable `Vec`. Dynrec native
    /// code may keep this pointer in a reserved host register for one runner
    /// call, including across helper callouts that borrow `Memory`, as long as
    /// generated code live-loads entries and respects their tags/pointers.
    ///
    /// The pointer must not be cached in compiled block metadata or used after
    /// the owning `Memory` is dropped. TLB contents may change across any
    /// mutable memory operation; callers must treat this as a pointer to a
    /// mutable table, not as a stable translation result.
    pub fn tlb_entries_ptr(&self) -> *const TlbEntry {
        self.core.tlb_entries_ptr()
    }

    /// Get a reference to the TLB stats counters.
    pub fn tlb_stats(&self) -> &TlbStats {
        self.core.tlb_stats()
    }

    /// Get a mutable reference to the TLB stats (for enabling in tests).
    pub fn tlb_stats_mut(&mut self) -> &mut TlbStats {
        self.core.tlb_stats_mut()
    }

    /// Return a formatted TLB stats summary, or `None` if stats are disabled.
    pub fn tlb_stats_summary(&self) -> Option<String> {
        self.core.tlb_stats_summary()
    }

    /// Clear the page fault pending flag
    /// Call this after handling a page fault to reset the state
    pub fn clear_page_fault(&mut self) {
        self.page_fault_pending = false;
        self.page_fault_address = 0;
        self.page_fault_error_code = 0;
    }

    /// Mask address based on A20 gate state.
    /// When A20 is disabled, bit 20 is cleared to emulate 8086-style 1MB wrap.
    #[inline(always)]
    fn mask_address(&self, addr: usize) -> usize {
        if self.a20_enabled {
            addr
        } else {
            addr & !0x0010_0000 // Clear bit 20 for 1MB wrap
        }
    }

    /// Read a byte from memory
    ///
    /// For EGA/VGA planar modes: reads from the selected VGA map window have the
    /// side effect of loading ALL 4 latches with data from all planes at the
    /// read address. The returned value comes from the plane selected by the
    /// Read Map Select register.
    #[inline]
    /// Side-effect-free read of a byte in the legacy video window.
    /// Routes like a CPU Read Mode 0 access without loading VGA latches
    /// or applying Read Mode 1 color comparison.
    ///
    /// Host-side surface synchronization must not reload latches between
    /// guest instructions, which would corrupt an in-progress latch copy.
    pub fn peek_video_u8(&self, addr: usize) -> u8 {
        let addr = self.mask_address(addr);
        debug_assert!(
            (0xA0000..0xC0000).contains(&addr),
            "peek_video_u8 is only routed for the legacy video window, got {addr:#X}"
        );

        // Mirror `read_u8`'s S3 window routing (these branches are pure).
        if let Some(off) = self.lfb_offset(addr) {
            return self.transform_s3_linear_byte(self.lfb[off]);
        }
        if let Some(off) = self.s3_legacy_direct_lfb_offset(addr) {
            return self.lfb[off];
        }
        if self.s3_legacy_access_blocked(addr) {
            return 0xFF;
        }

        // Chain-4 (mode 13h): same byte a CPU read returns, latches untouched.
        if self.vga.video_mode == 0x13 && self.vga.chain4 {
            if let Some(offset) = self.vga_window_offset(addr) {
                let plane = offset & 3;
                let plane_offset = offset & !3;
                if plane_offset < PLANE_SIZE {
                    return self.planes_flat[plane * PLANE_SIZE + plane_offset];
                }
            }
        }

        // Planar modes: the Read Mode 0 byte from the selected read plane.
        if self.is_planar_mode() || self.is_cpu_sequential_planar_read() {
            if let Some(offset) = self.vga_window_offset(addr) {
                if offset < PLANE_SIZE {
                    let plane_idx = (self.vga.read_plane & 0x03) as usize;
                    return self.planes_flat[plane_idx * PLANE_SIZE + offset];
                }
            }
        }

        if addr < self.usable_len() {
            self.ram[addr]
        } else {
            0xFF
        }
    }

    /// Bulk [`Self::peek_video_u8`]: fill `out[j]` with
    /// `self.peek_video_u8(base + j)` for every `j`, deciding the routing
    /// once for the whole range instead of once per byte.
    ///
    /// Same side-effect-free contract as `peek_video_u8` (no latch reload,
    /// no Read Mode 1). A range on which some routing predicate would change
    /// part-way falls back to the per-byte peek, so the result is always
    /// byte-identical; every 4 KiB-aligned page of the legacy window takes
    /// the bulk route (see `peek_video_range_bulk`).
    pub fn peek_video_range(&self, base: usize, out: &mut [u8]) {
        if !self.peek_video_range_bulk(base, out) {
            for (index, byte) in out.iter_mut().enumerate() {
                *byte = self.peek_video_u8(base + index);
            }
        }
    }

    /// The uniform-routing fast path of [`Self::peek_video_range`]. Returns
    /// `false` (leaving `out` in an unspecified state) when the range is not
    /// uniformly routed and the caller must peek byte by byte.
    ///
    /// Each arm of `peek_video_u8` is taken in the same order; an arm is
    /// applied in bulk only when it provably claims either every byte of the
    /// range or none of it:
    /// - the range lies inside one 64 KiB half of `A0000h..C0000h`, where
    ///   A20 masking is the identity (bit 20 clear) and the S3 legacy
    ///   predicates are constant (they change only at `B0000h`);
    /// - LFB: no byte decodes (`lfb_range_overlaps` false), or the whole
    ///   range decodes to one contiguous VRAM span (`lfb_span_offset`);
    /// - S3 direct window: the first byte misses (then all miss: the gates
    ///   are global and the window is `A0000h..B0000h`), or both ends hit
    ///   inside one 8 KiB block (constant A13 window select) with contiguous
    ///   VRAM offsets (no modular wrap);
    /// - VGA map window: wholly inside or wholly outside, and the
    ///   `PLANE_SIZE` bound holds at both ends or at neither (offsets are
    ///   monotone);
    /// - RAM: wholly below or wholly at/above `usable_len`.
    fn peek_video_range_bulk(&self, base: usize, out: &mut [u8]) -> bool {
        let len = out.len();
        if len == 0 {
            return true;
        }
        let Some(last) = base.checked_add(len - 1) else {
            return false;
        };
        if base < VIDEO_BASE || last >= VIDEO_BASE + VIDEO_SIZE || (base ^ last) & !0xFFFF != 0 {
            return false;
        }

        // Relocatable S3 / DISPI linear window.
        if self.lfb_range_overlaps(base, len) {
            let Some(off) = self.lfb_span_offset(base, len) else {
                return false;
            };
            let src = &self.lfb[off..off + len];
            if self.s3_linear_nibble_swap() {
                for (dst, &value) in out.iter_mut().zip(src) {
                    *dst = value.rotate_left(4);
                }
            } else {
                out.copy_from_slice(src);
            }
            return true;
        }

        // S3 legacy A0000h window routed straight to VRAM.
        if let Some(off) = self.s3_legacy_direct_lfb_offset(base) {
            if (base ^ last) & !0x1FFF != 0
                || self.s3_legacy_direct_lfb_offset(last) != Some(off + len - 1)
            {
                return false;
            }
            out.copy_from_slice(&self.lfb[off..off + len]);
            return true;
        }

        if self.s3_legacy_access_blocked(base) {
            out.fill(0xFF);
            return true;
        }

        // Where the range sits relative to the selected VGA map window:
        // `Some(Some(first_offset))` wholly inside, `Some(None)` wholly
        // outside, `None` straddling an edge.
        let window = {
            let (window_base, window_size) = self.vga_memory_window();
            let window_end = window_base.saturating_add(window_size);
            if base >= window_base && last < window_end {
                Some(Some(base - window_base))
            } else if last < window_base || base >= window_end {
                Some(None)
            } else {
                None
            }
        };

        if self.vga.video_mode == 0x13 && self.vga.chain4 {
            match window {
                None => return false,
                Some(Some(first)) => {
                    let end = first + len - 1;
                    if (end & !3) < PLANE_SIZE {
                        for (index, dst) in out.iter_mut().enumerate() {
                            let offset = first + index;
                            *dst = self.planes_flat[(offset & 3) * PLANE_SIZE + (offset & !3)];
                        }
                        return true;
                    }
                    if (first & !3) < PLANE_SIZE {
                        return false;
                    }
                }
                Some(None) => {}
            }
        }

        if self.is_planar_mode() || self.is_cpu_sequential_planar_read() {
            match window {
                None => return false,
                Some(Some(first)) => {
                    let end = first + len - 1;
                    if end < PLANE_SIZE {
                        let plane = (self.vga.read_plane & 0x03) as usize * PLANE_SIZE;
                        out.copy_from_slice(&self.planes_flat[plane + first..=plane + end]);
                        return true;
                    }
                    if first < PLANE_SIZE {
                        return false;
                    }
                }
                Some(None) => {}
            }
        }

        let usable = self.usable_len();
        if last < usable {
            out.copy_from_slice(&self.ram[base..=last]);
        } else if base >= usable {
            out.fill(0xFF);
        } else {
            return false;
        }
        true
    }

    pub fn read_u8(&self, addr: usize) -> u8 {
        // Real-boot high-memory alias: fold the top 64 KiB of the 4 GiB
        // physical space back onto the top 64 KiB of the BIOS image in
        // the 1 MiB window. The folded address is always < 0x10_0000 and
        // therefore cannot re-enter this branch — recursion is bounded.
        // `read_u16`/`read_u32` cover the alias by decomposing to `read_u8`
        // on the slow path; keep this fold here as the single source of truth.
        if self.high_mem_alias_enabled && in_high_mem_alias(addr) {
            let folded = HIGH_MEM_ALIAS_FOLD_BASE + (addr - HIGH_MEM_ALIAS_BASE);
            return self.read_u8(folded);
        }

        let addr = self.mask_address(addr);
        if self.should_hide_firmware_mp_signature_byte(addr) {
            return 0x00;
        }

        if let Some(value) = self.read_ems_frame_u8(self.resolve_ems_frame_addr(addr)) {
            return value;
        }

        // common case) this collapses to a single `Option::is_none`
        // check that the optimiser predicts not-taken. When a region
        // is installed, addresses inside `[base, base+size)` route
        // fall through to the normal RAM/VGA/MMIO path. The HLD's
        // `u32` because the region can only ever live in the low 4 GiB
        // of guest physical address space.

        if let Some(offset) = self.option_rom_offset(addr) {
            return self.option_rom_backing[offset];
        }

        if let Some(value) = self.s3_mmio_read(addr, S3MmioWidth::Byte) {
            return value as u8;
        }

        if let Some(off) = self.lfb_offset(addr) {
            return self.transform_s3_linear_byte(self.lfb[off]);
        }
        if let Some(off) = self.s3_legacy_direct_lfb_offset(addr) {
            return self.lfb[off];
        }
        if self.s3_legacy_access_blocked(addr) {
            return 0xFF;
        }

        // Fast path: addresses below video memory never need VGA handling
        if addr < VIDEO_BASE {
            return if addr < self.usable_len() {
                self.ram[addr]
            } else {
                0xFF
            };
        }

        let is_chain4 = self.vga.video_mode == 0x13 && self.vga.chain4;
        if is_chain4 {
            if let Some(offset) = self.vga_window_offset(addr) {
                let plane = offset & 3;
                let plane_offset = offset & !3;
                if plane_offset < PLANE_SIZE {
                    // Load ALL 4 latches on ANY video memory read
                    let mut new_latches = [0u8; 4];
                    #[allow(clippy::needless_range_loop)]
                    for p in 0..4 {
                        new_latches[p] = self.planes_flat[p * PLANE_SIZE + plane_offset];
                    }
                    self.latches.set(new_latches);

                    // Read Mode 1: Color Compare
                    if self.vga.read_mode == 1 {
                        let mut result = 0xFFu8;
                        for p in 0..4 {
                            let plane_bit = 1 << p;
                            if (self.vga.color_dont_care & plane_bit) != 0 {
                                let compare_bit = (self.vga.color_compare >> p) & 1;
                                let plane_expected = if compare_bit == 1 { 0xFF } else { 0x00 };
                                let mismatch = new_latches[p] ^ plane_expected;
                                result &= !mismatch;
                            }
                        }
                        return result;
                    }

                    return new_latches[plane];
                }
            }
        }

        // Intercept reads from the selected VGA map window for planar modes.
        // Text-mode font save/restore code also reads plane 2 through A000
        // after temporarily reprogramming the sequencer/GC; display mode stays
        // text, but CPU memory routing is planar.
        if self.is_planar_mode() || self.is_cpu_sequential_planar_read() {
            if let Some(offset) = self.vga_window_offset(addr) {
                if offset < PLANE_SIZE {
                    // Load ALL 4 latches on ANY video memory read
                    // This is critical for read-modify-write patterns!
                    let mut new_latches = [0u8; 4];
                    #[allow(clippy::needless_range_loop)]
                    for plane in 0..4 {
                        new_latches[plane] = self.planes_flat[plane * PLANE_SIZE + offset];
                    }

                    // Debug: trace latch loads that result in all zeros
                    {
                        let all_zero = new_latches.iter().all(|&v| v == 0);
                        if all_zero && self.vga.write_mode == 1 {
                            tracing::debug!(
                                offset = format_args!("{:04X}", offset),
                                "EGA latch read loaded zeros in WM1 context"
                            );
                        }
                    }

                    self.latches.set(new_latches);

                    // Read Mode 1: Color Compare
                    if self.vga.read_mode == 1 {
                        // Compare each pixel's color against color_compare
                        // Only planes in color_dont_care are checked
                        // Returns byte where bit N is 1 if pixel N matches
                        let mut result = 0xFFu8;
                        for plane in 0..4 {
                            let plane_bit = 1 << plane;
                            if (self.vga.color_dont_care & plane_bit) != 0 {
                                // This plane matters for the comparison
                                let compare_bit = (self.vga.color_compare >> plane) & 1;
                                let plane_expected = if compare_bit == 1 { 0xFF } else { 0x00 };
                                // XOR to find non-matching bits, then NOT to find matches
                                let mismatch = new_latches[plane] ^ plane_expected;
                                // Clear result bits where there's a mismatch
                                result &= !mismatch;
                            }
                        }
                        return result;
                    }

                    // Read Mode 0: Return data from the selected read plane
                    let plane_idx = (self.vga.read_plane & 0x03) as usize;
                    return new_latches[plane_idx];
                }
            }
        }

        if addr < self.usable_len() {
            self.ram[addr]
        } else if (arena::ARENA_PHYS_START..arena::ARENA_PHYS_END).contains(&addr) {
            // Shared arena (HLD §5.2): no device decodes it (U1 audit), so it
            // is RAM or, unadmitted or uncommitted, open bus.
            self.ram.get(addr).copied().unwrap_or(0xFF)
        } else {
            match self.mmio_read_u8(addr) {
                Some(v) => v,
                None => {
                    if self.post_trace_enabled && !self.addr_in_declared_region(addr) {
                        self.post_trace_memory_event(addr, 0, None);
                    }
                    0xFF // Open bus
                }
            }
        }
    }

    /// Read a byte from memory, returning None if the address is out of bounds.
    pub fn read_byte_checked(&self, addr: usize) -> Option<u8> {
        let addr = self.mask_address(addr);
        if addr < self.usable_len()
            || (addr >= arena::ARENA_PHYS_START && self.ram.is_backed(addr..addr.saturating_add(1)))
            || self.is_lfb_address(addr)
            || self.option_rom_offset(addr).is_some()
            || self.s3_mmio_span_offset(addr, S3MmioWidth::Byte).is_some()
        {
            Some(self.read_u8(addr))
        } else {
            None
        }
    }

    /// Read multiple bytes into a slice (optimized for instruction fetch - one `copy_from_slice` instead of 15 `read_u8` calls)
    #[inline]
    pub fn read_bytes(&self, addr: usize, dest: &mut [u8]) {
        if dest.is_empty() {
            return;
        }
        // Real-boot high-memory alias: fold 0xFFFF_0000..=0xFFFF_FFFF onto
        // 0xF0000..=0xFFFFF (the top 64 KiB of the BIOS image loaded at
        // 0xC0000..=0xFFFFF). If `addr` is in the alias window, re-enter with
        // the folded base so the subsequent slice copy hits real RAM. A 15-byte
        // instruction fetch near the 4 GiB end still stays inside the 64 KiB
        // alias, so we do not handle a straddling read here; recursion is
        // bounded because the folded address is always ≤ 0xFFFFF.
        if self.high_mem_alias_enabled && in_high_mem_alias(addr) {
            let folded = HIGH_MEM_ALIAS_FOLD_BASE + (addr - HIGH_MEM_ALIAS_BASE);
            return self.read_bytes(folded, dest);
        }

        // With A20 off, RAM is contiguous only inside one 1 MiB block: bit 20
        // is cleared per address, so a span that crosses a block boundary has
        // a discontinuity that masking the START cannot express — 0xFFFFF is
        // followed by 0x00000, never by 0x100000. Route such a span byte by
        // byte, where `read_u8` masks each address itself (MDD-BUG-KIL-00816).
        if !self.a20_enabled && (addr >> 20) != (addr.saturating_add(dest.len() - 1) >> 20) {
            for (i, byte) in dest.iter_mut().enumerate() {
                *byte = self.read_u8(addr.wrapping_add(i));
            }
            return;
        }

        let addr = self.mask_address(addr);
        if self.hle_ems_frame_owned
            && range_overlaps(addr, dest.len(), EMS_FRAME_BASE, EMS_FRAME_SIZE)
        {
            for (i, byte) in dest.iter_mut().enumerate() {
                *byte = self.read_u8(addr.wrapping_add(i));
            }
            return;
        }
        // RAM behind it: INT 13h disk writes source guest buffers through here
        // (MDD-BUG-CRU-01036). The VGA map window is the same argument with a
        // different backing store - while the planes are live, `read_u8`
        // serves them, and `ram` under the window is stale (MDD-BUG-CRU-01475).
        if self.lfb_range_overlaps(addr, dest.len())
            || self.option_rom_region_overlaps(addr, dest.len())
            || self.s3_mmio_range_overlaps(addr, dest.len())
            || self.vga_plane_window_overlaps(addr, dest.len())
        {
            for (i, byte) in dest.iter_mut().enumerate() {
                *byte = self.read_u8(addr.wrapping_add(i));
            }
            return;
        }
        let usable = self.usable_len();
        let end = (addr + dest.len()).min(usable);
        let copy_len = end.saturating_sub(addr);
        if copy_len > 0 {
            dest[..copy_len].copy_from_slice(&self.ram[addr..end]);
            self.hide_firmware_mp_signatures_in_read_buffer(addr, &mut dest[..copy_len]);
        }
        if copy_len < dest.len() && addr >= arena::ARENA_PHYS_START {
            // Shared arena (HLD §5.2): a backed span copies in one go; a
            // span over an unbacked page reads it as `read_u8` does.
            let rest = addr + copy_len;
            if let Some(src) = self.ram.get(rest..addr + dest.len()) {
                dest[copy_len..].copy_from_slice(src);
            } else {
                for (i, byte) in dest[copy_len..].iter_mut().enumerate() {
                    *byte = self.read_u8(rest + i);
                }
            }
            return;
        }
        for byte in &mut dest[copy_len..] {
            *byte = 0xFF;
        }
    }

    #[inline(never)]
    pub fn read_guest_run(&self, addr: usize, dest: &mut [u8]) {
        if self.guest_run_is_plain_ram(addr, dest.len()) {
            dest.copy_from_slice(&self.ram[addr..addr + dest.len()]);
            self.hide_firmware_mp_signatures_in_read_buffer(addr, dest);
            return;
        }
        for (i, byte) in dest.iter_mut().enumerate() {
            *byte = self.read_u8(addr + i);
        }
    }

    /// True when [`Self::read_guest_run`] may copy `[addr, addr + len)`
    /// straight out of `self.ram`.
    #[inline(always)]
    fn guest_run_is_plain_ram(&self, addr: usize, len: usize) -> bool {
        let end = addr.saturating_add(len);
        let low_ram = end <= VIDEO_BASE;
        let high_ram = addr >= 0x10_0000
            && self.a20_enabled
            && (!self.high_mem_alias_enabled || end <= HIGH_MEM_ALIAS_BASE);
        (low_ram || high_ram)
            && end <= self.usable_len()
            // reads bytes that are not `self.ram`'s; a disjoint span keeps
            // the bulk copy.

            && !self.lfb_range_overlaps(addr, len)
            && !self.option_rom_region_overlaps(addr, len)
            && !self.s3_mmio_range_overlaps(addr, len)
    }

    /// True when every byte of `[addr, addr + len)` would take a `write_u8`
    /// path whose per-byte bookkeeping is span-invariant, so a span write can
    /// do it once. Anything that decodes or is observed per byte — an
    /// aperture, A20 folding, write tracking, a descriptor snoop that would
    /// actually fire, an armed diagnostic — disqualifies the span.
    ///
    /// **Two admitted windows, answering to DIFFERENT reference paths.** The
    /// older "this mirrors `write_u8` gate for gate" claim was true only of
    /// the low window and is false for the high one; keep them apart.
    ///
    /// * **Low RAM** (`end <= VIDEO_BASE`) mirrors `write_u8`'s fast-path
    ///   gates. Below the VGA window there is no planar pipeline, no
    ///   text-video aperture, no EMS page frame (0xE0000) and no option ROM
    ///   (0xC0000). A20 is settled for free: `mask_address` only clears bit
    ///   20, and every address below 0xA0000 already has it clear, so the mask
    ///   is the identity whatever the gate. (`write_u8`'s own fast path
    ///   insists on `a20_enabled`, which is why a plain real-mode DOS guest —
    ///   A20 off — used to take the slow path for every byte of a sector
    ///   read.)
    /// * **Extended RAM** (`addr >= 0x10_0000 && a20_enabled`) has NO
    ///   `write_u8` fast path to mirror: that arm is gated on
    ///   `addr < VIDEO_BASE`, so every high write takes the SLOW path, and
    ///   that is this window's reference — `prepare_write_addr` →
    ///   `smc_before_write` → `snoop_gdt_write_ext` → `write_u8_inner`
    ///   (whose own first acts are the S3-MMIO and ROM gates, then
    ///   `mark_instr_cache_dirty_range`) → `record_write`. On plain extended
    ///   RAM the fast and slow paths produce the same EFFECT SET in a
    ///   different ORDER: fast marks the instruction cache *before* the
    ///   SMC/snoop pair, slow marks it *after*. The reorder is unobservable —
    ///   `mark_instr_cache_dirty_range` only reads the exec-page bitmap and
    ///   sets an `AtomicBool`, and nothing in between reads or clears that
    ///   flag (`smc_before_write` touches the PT tripwire and the watch
    ///   bitmap; the SMC notifier is a separate object holding no `&Memory`;
    ///   `snoop_gdt_write_ext` takes `&self` and only sets the table-dirty
    ///   cells). Here A20 IS load-bearing: with the gate off `mask_address`
    ///   folds bit 20, the span stops being contiguous, and the arm must
    ///   refuse — hence the explicit `a20_enabled`.
    ///
    /// Neither window can reach the byte-decoding machinery, structurally:
    /// the S3 legacy aperture (`s3_legacy_direct_lfb_offset`,
    /// `s3_legacy_access_blocked`), every `vga_memory_window` variant, the
    /// text-video ranges and the EMS frame all live inside
    /// `0xA0000..0x100000`, which is exactly the band BETWEEN the two windows
    /// — the low arm ends at 0xA0000 and the high arm starts at 0x100000, and
    /// a span straddling the band satisfies neither. `prepare_write_addr`'s
    /// BIOS-ROM refusal (`0xF0000..=0xFFFFF`) sits in the same band. The
    /// relocatable LFB and S3 MMIO decoders CAN sit high, so they keep their
    /// explicit overlap refusals. The `masked >= MAX_RAM_SIZE` MMIO branch is
    /// unreachable because `end <= usable_len()` and `resize_ram` asserts
    /// `usable_len() <= MAX_RAM_SIZE`.
    ///
    /// `end <= usable_len()`, not `ram.len()`: opening the high window makes
    /// the fetch-padding tail addressable, and a span ending in it must refuse
    /// so the per-byte path's `prepare_write_addr` can grow RAM (or drop the
    /// write) exactly as it does today.
    ///
    /// Known, pre-existing, diagnostic-only divergence in the LOW window
    /// (unchanged here): `write_u8_inner`'s once-per-process IVT-corruption
    /// `tracing::error!` for `addr < 0x400` does not fire on the bulk arm.
    fn bulk_write_span_is_plain_ram(&self, addr: usize, len: usize) -> bool {
        let Some(end) = addr.checked_add(len) else {
            return false;
        };
        let low_ram = end <= VIDEO_BASE;
        let high_ram = addr >= 0x10_0000 && self.a20_enabled;
        len != 0
            && (low_ram || high_ram)
            && end <= self.usable_len()
            // bytes that are not `self.ram`'s, but only inside its interval
            // (MDD-BUG-KIL-01042).

            && !self.high_mem_alias_enabled
            && !self.write_tracking_enabled
            // NOT `has_descriptor_snoop()`: in a PM32 guest a table is always
            // registered, which refused every span. `snoop_gdt_write_ext`'s
            // own first statement is this same two-compare hull test, so a
            // span the hull rejects is a PROVEN per-byte no-op; a span that
            // overlaps refuses to the per-byte path, which snoops per byte
            // (the snoop takes a `u64` value and is inexpressible for a span
            // wider than 8 bytes).
            && !self.descriptor_snoop_overlaps(addr, len)
            && !self.rom_region_overlaps(addr, len)
            && !self.option_rom_region_overlaps(addr, len)
            && !self.lfb_range_overlaps(addr, len)
            && !self.s3_mmio_range_overlaps(addr, len)


            && traced_phys_write_range().is_none()
    }

    /// Write multiple guest-visible bytes.
    ///
    /// Unlike `load`, this observes mapped/device memory semantics, including
    /// EMS page-frame resolution and write-side tracking.
    ///
    /// Large transfers would repeat aperture decoding, SMC snooping and
    /// instruction-cache range marking for every byte. For spans entirely in
    /// plain low RAM, those checks are span-invariant: run them once and copy.
    /// Every other span keeps the exact per-byte path.
    pub fn write_bytes_guest(&mut self, addr: usize, data: &[u8]) {
        let bucket = span_write_census::width_bucket(data.len());
        if self.bulk_write_span_is_plain_ram(addr, data.len()) {
            self.span_write_bulk[bucket] = self.span_write_bulk[bucket].wrapping_add(1);
            self.mark_instr_cache_dirty_range(addr, data.len());
            self.smc_before_write(addr, data.len());
            self.ram[addr..addr + data.len()].copy_from_slice(data);
            return;
        }
        self.span_write_per_byte[bucket] = self.span_write_per_byte[bucket].wrapping_add(1);
        for (i, byte) in data.iter().copied().enumerate() {
            self.write_u8(addr.wrapping_add(i), byte);
        }
    }

    /// A052 outcome census: `(bulk, per_byte)` span counts by width bucket.
    ///
    /// The pair is the point. A funnel-side "millions of runs" figure proves
    /// only that the funnel ran; whether those spans were bulk-copied or fell
    /// back to the exact per-byte dispatch is decided here, by
    /// `bulk_write_span_is_plain_ram`.
    pub fn span_write_census(
        &self,
    ) -> (
        [u64; span_write_census::WIDTH_BUCKETS],
        [u64; span_write_census::WIDTH_BUCKETS],
    ) {
        (self.span_write_bulk, self.span_write_per_byte)
    }

    /// Read a word (little-endian) from memory
    #[inline]
    pub fn read_u16(&self, addr: usize) -> u16 {
        let masked_addr = self.mask_address(addr);
        if let Some(value) = self.s3_mmio_read(masked_addr, S3MmioWidth::Word) {
            return value as u16;
        }
        // Fast path: both bytes below video memory
        // The MP-signature filter is CONTENT-keyed, and a content-keyed filter
        // a guest defeats by changing its operand size is not a filter. Only
        // `read_u8` applies it, so a paragraph-aligned `_MP_` or `PCMP` in
        // conventional memory or the EBDA read one byte at a time answered
        // 0x00 while the same address read as a word or dword answered the
        // real signature - and an MP-aware guest scans with word loads
        // (MDD-BUG-CRU-01477).
        //
        // The fast paths' own `< VIDEO_BASE` guard sits entirely INSIDE the
        // filter's domain (`should_hide_firmware_mp_signature_byte` acts below
        // LOW_MEMORY_LIMIT, 1 MiB), so every address they serve is one the
        // filter is responsible for.
        //
        // Dropping to the `read_u8` decomposition is cheaper to reason about
        // than teaching both fast paths the content test, and costs one bool
        // compare on a flag that is false on every run that is not a real
        // boot.
        if addr.wrapping_add(1) < VIDEO_BASE
            && !self.firmware_mp_table_suppression_enabled
            && !self.lfb_range_overlaps(addr, 2)
            && !self.option_rom_region_overlaps(addr, 2)
        {
            let addr = self.mask_address(addr);
            if addr.wrapping_add(1) < self.usable_len() {
                return u16::from_le_bytes([self.ram[addr], self.ram[addr + 1]]);
            }
        }
        let lo = self.read_u8(addr) as u16;
        let hi = self.read_u8(addr.wrapping_add(1)) as u16;
        lo | (hi << 8)
    }

    /// Read a dword (little-endian) from memory
    #[inline]
    pub fn read_u32(&self, addr: usize) -> u32 {
        let masked_addr = self.mask_address(addr);
        if let Some(value) = self.s3_mmio_read(masked_addr, S3MmioWidth::Dword) {
            return value;
        }
        // Fast path: all bytes below video memory. Suppression sends this
        // to the byte decomposition for the reason `read_u16` records.
        if addr.wrapping_add(3) < VIDEO_BASE
            && !self.firmware_mp_table_suppression_enabled
            && !self.lfb_range_overlaps(addr, 4)
            && !self.option_rom_region_overlaps(addr, 4)
        {
            let addr = self.mask_address(addr);
            if addr.wrapping_add(3) < self.usable_len() {
                return u32::from_le_bytes([
                    self.ram[addr],
                    self.ram[addr + 1],
                    self.ram[addr + 2],
                    self.ram[addr + 3],
                ]);
            }
        }
        let lo = self.read_u16(addr) as u32;
        let hi = self.read_u16(addr.wrapping_add(2)) as u32;
        lo | (hi << 16)
    }

    /// Write a byte to memory
    #[inline]
    #[allow(clippy::needless_range_loop)]
    pub fn write_u8(&mut self, addr: usize, value: u8) {
        // Real-boot high-memory alias: the alias window is ROM — drop writes
        // silently. Single bool-compare when the alias is disabled.
        if self.high_mem_alias_enabled && in_high_mem_alias(addr) {
            return;
        }
        let masked_addr = self.mask_address(addr);
        if self.write_ems_frame_u8(masked_addr, value) {
            return;
        }
        if self.option_rom_region_overlaps(masked_addr, 1) {
            return;
        }
        // RomRegions gate: silently drop writes to pages registered as ROM.
        // Single-branch cost when `has_rom_regions == false` (non-real-boot mode).
        if self.is_rom_addr(addr) {
            return;
        }
        if self.stage_s3_mmio_write(masked_addr, S3MmioWidth::Byte, u32::from(value)) {
            return;
        }
        // Fast path: below video memory, A20 enabled, no TimeMachine tracking.
        // Skips LFB/VGA/ROM checks and planar write pipeline.
        if addr < VIDEO_BASE
            && self.a20_enabled
            && !self.write_tracking_enabled
            && !self.is_lfb_address(addr)
        {
            self.mark_instr_cache_dirty_range(addr, 1);
            self.smc_before_write(addr, 1);
            if self.has_descriptor_snoop() {
                self.snoop_gdt_write_ext(addr, value as u64, 1);
            }
            if addr < self.ram.len() {
                self.ram[addr] = value;

                self.maybe_trace_phys_write_range(addr, 1, value as u32);
            }
            return;
        }

        // Check for LFB access.
        // The LFB decodes the A20-masked address, as reads do (MDD-BUG-CRU-01492).
        if self.write_lfb_span(masked_addr, &[value], true) {
            return;
        }
        if let Some(off) = self.s3_legacy_direct_lfb_offset(addr) {
            let changed = vga_write_change_stats_enabled() && self.lfb[off] != value;
            self.lfb[off] = value;
            self.mark_lfb_dirty_range(off, 1);
            self.increment_video_write_count();
            self.record_video_write_change(changed);
            return;
        }
        if self.s3_legacy_access_blocked(addr) {
            return;
        }

        // Check MMIO regions (e.g., 3dfx Voodoo2 LFB) before prepare_write_addr,
        // which would try to expand RAM for high addresses.
        {
            let masked = self.mask_address(addr);
            // One compare on the private path; the shared arena (HLD §5.2) is
            // RAM for `prepare_write_addr`, never MMIO.
            if masked >= MAX_RAM_SIZE
                && !(arena::ARENA_PHYS_START..arena::ARENA_PHYS_END).contains(&masked)
            {
                if self.mmio_write_u8(masked, value) {
                    return;
                }
                // High address, no MMIO match → the write is silently
                // dropped by `prepare_write_addr` (its auto-grow window ends
                // at DOS_RAM_CEILING and RAM never reaches MAX_RAM_SIZE, so
                // an address at or above the bound is always out of range).
                // Log it as an anomalous out-of-region guest memory access
                // if the POST tracer is enabled. Cheap — slow path.
                if self.post_trace_enabled && !self.addr_in_declared_region(masked) {
                    self.post_trace_memory_event(masked, 1, Some(value));
                }
            }
        }

        let Some(addr) = self.prepare_write_addr(addr) else {
            return;
        };

        // Record write for Time Machine (read old value before write)
        // Skip video memory tracking for now (planar writes are complex)
        let is_video = (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&addr);
        let old_value = if self.write_tracking_enabled && !is_video {
            self.ram.get(addr).copied().unwrap_or(0)
        } else {
            0
        };

        self.smc_before_write(addr, 1);
        if self.has_descriptor_snoop() {
            self.snoop_gdt_write_ext(addr, value as u64, 1);
        }

        self.write_u8_inner(addr, value);

        // Record the write after it completes (for non-video memory)
        if !is_video {
            self.record_write(addr, old_value as u64, value as u64, 1);
        }
    }

    /// EGA/VGA planar write with full graphics pipeline
    ///
    /// Implements all 4 write modes with proper handling of:
    /// - Data rotation
    /// - Set/Reset expansion
    /// - Logical operations (AND/OR/XOR)
    /// - Bit mask selection between new data and latches
    ///
    /// Core EGA/VGA planar write logic shared by both the fast path and
    /// the Time Machine tracked path.  When `track` is true, each plane
    /// write is recorded via `record_vga_plane_write()` for time-travel
    /// debugging.
    #[inline]
    fn write_planar_ega_core(
        &mut self,
        offset: usize,
        cpu_data: u8,
        plane_mask: u8,
        track: bool,
    ) -> bool {
        let collect_change = vga_write_change_stats_enabled();
        let collect_sample = vga_write_change_sample_enabled();
        let inspect_change = collect_change || collect_sample;
        let mut changed = false;

        // Latch the custom-font marker on writes that match the plane-2
        // font-upload pattern (alphanumeric addressing, non-chain4,
        // plane_mask=0x04, memory_map window covers A0000–AFFFF).  Ordinary
        // graphics-mode planar writes can also target plane 2, but GC06 bit 0
        // is set for those and they must not make text rendering consume
        // graphics data as glyphs.  One-way: once set, stays set.
        if !self.planar_font_latch_pending
            && !self.vga.chain4
            && !self.vga.gc_graphics_mode
            && plane_mask == 0x04
            && (self.vga.memory_map_select == 0 || self.vga.memory_map_select == 1)
        {
            self.planar_font_latch_pending = true;
        }

        let latches = self.latches.get();

        match self.vga.write_mode & 0x03 {
            0 => {
                // Write Mode 0: Full pipeline
                // 1. Rotate CPU data
                let rotate_count = self.vga.data_rotate & 0x07;
                let rotated = cpu_data.rotate_right(rotate_count as u32);

                // 2. For each plane, determine source data
                for plane in 0..4usize {
                    if (plane_mask & (1 << plane)) == 0 {
                        continue; // Plane not selected for write
                    }

                    // Select between rotated CPU data or Set/Reset expansion
                    let plane_data = if (self.vga.enable_set_reset & (1 << plane)) != 0 {
                        // Use Set/Reset: expand bit to full byte
                        if (self.vga.set_reset & (1 << plane)) != 0 {
                            0xFF
                        } else {
                            0x00
                        }
                    } else {
                        rotated
                    };

                    // 3. Apply logical operation with latch data
                    let logical_op = (self.vga.data_rotate >> 3) & 0x03;
                    let after_logic = match logical_op {
                        1 => plane_data & latches[plane], // AND
                        2 => plane_data | latches[plane], // OR
                        3 => plane_data ^ latches[plane], // XOR
                        // 0 or any other value => None (pass through)
                        _ => plane_data,
                    };

                    // 4. Apply bit mask: select between processed data and latch
                    let final_data =
                        (after_logic & self.vga.bit_mask) | (latches[plane] & !self.vga.bit_mask);

                    let plane_offset = plane * PLANE_SIZE + offset;
                    if track || inspect_change {
                        let old_value = self.planes_flat.get(plane_offset).copied().unwrap_or(0);
                        if old_value != final_data {
                            changed = true;
                            if collect_sample {
                                self.record_video_write_change_sample(
                                    offset,
                                    plane as u8,
                                    old_value,
                                    final_data,
                                );
                            }
                        }
                        self.planes_flat[plane_offset] = final_data;
                        if track {
                            self.record_vga_plane_write(offset, plane as u8, old_value, final_data);
                        }
                    } else {
                        self.planes_flat[plane_offset] = final_data;
                    }
                }
            }

            1 => {
                // Write Mode 1: Latch copy (CPU data ignored)
                // Used for fast screen scrolling and memory-to-memory copies

                // Debug: trace Write Mode 1 operations
                tracing::trace!(
                    offset = format_args!("{:04X}", offset),
                    lat0 = format_args!("{:02X}", latches[0]),
                    lat1 = format_args!("{:02X}", latches[1]),
                    lat2 = format_args!("{:02X}", latches[2]),
                    lat3 = format_args!("{:02X}", latches[3]),
                    mask = format_args!("{:02X}", plane_mask),
                    "EGA Write Mode 1"
                );

                for plane in 0..4usize {
                    if (plane_mask & (1 << plane)) != 0 {
                        let final_data = latches[plane];
                        let plane_offset = plane * PLANE_SIZE + offset;
                        if track || inspect_change {
                            let old_value =
                                self.planes_flat.get(plane_offset).copied().unwrap_or(0);
                            if old_value != final_data {
                                changed = true;
                                if collect_sample {
                                    self.record_video_write_change_sample(
                                        offset,
                                        plane as u8,
                                        old_value,
                                        final_data,
                                    );
                                }
                            }
                            self.planes_flat[plane_offset] = final_data;
                            if track {
                                self.record_vga_plane_write(
                                    offset,
                                    plane as u8,
                                    old_value,
                                    final_data,
                                );
                            }
                        } else {
                            self.planes_flat[plane_offset] = final_data;
                        }
                    }
                }
            }

            2 => {
                // Write Mode 2: Color expansion
                // Bits 0-3 of CPU data are expanded to planes
                for plane in 0..4usize {
                    if (plane_mask & (1 << plane)) == 0 {
                        continue;
                    }

                    // Expand the plane's bit to a full byte (0x00 or 0xFF)
                    let expanded = if (cpu_data & (1 << plane)) != 0 {
                        0xFF
                    } else {
                        0x00
                    };

                    // Apply logical operation
                    let logical_op = (self.vga.data_rotate >> 3) & 0x03;
                    let after_logic = match logical_op {
                        1 => expanded & latches[plane],
                        2 => expanded | latches[plane],
                        3 => expanded ^ latches[plane],
                        // 0 or any other value => pass through
                        _ => expanded,
                    };

                    // Apply bit mask
                    let final_data =
                        (after_logic & self.vga.bit_mask) | (latches[plane] & !self.vga.bit_mask);

                    let plane_offset = plane * PLANE_SIZE + offset;
                    if track || inspect_change {
                        let old_value = self.planes_flat.get(plane_offset).copied().unwrap_or(0);
                        if old_value != final_data {
                            changed = true;
                            if collect_sample {
                                self.record_video_write_change_sample(
                                    offset,
                                    plane as u8,
                                    old_value,
                                    final_data,
                                );
                            }
                        }
                        self.planes_flat[plane_offset] = final_data;
                        if track {
                            self.record_vga_plane_write(offset, plane as u8, old_value, final_data);
                        }
                    } else {
                        self.planes_flat[plane_offset] = final_data;
                    }
                }
            }

            _ => {
                // Write Mode 3: VGA-only optimized pixel drawing
                // Set/Reset is always used, CPU data ANDed with bit mask creates effective mask
                let rotate_count = self.vga.data_rotate & 0x07;
                let rotated = cpu_data.rotate_right(rotate_count as u32);
                let effective_mask = rotated & self.vga.bit_mask;

                for plane in 0..4usize {
                    if (plane_mask & (1 << plane)) == 0 {
                        continue;
                    }

                    // Expand Set/Reset value for this plane
                    let set_reset_expanded = if (self.vga.set_reset & (1 << plane)) != 0 {
                        0xFF
                    } else {
                        0x00
                    };

                    // Use effective mask to select between set/reset and latch
                    let final_data =
                        (set_reset_expanded & effective_mask) | (latches[plane] & !effective_mask);

                    let plane_offset = plane * PLANE_SIZE + offset;
                    if track || inspect_change {
                        let old_value = self.planes_flat.get(plane_offset).copied().unwrap_or(0);
                        if old_value != final_data {
                            changed = true;
                            if collect_sample {
                                self.record_video_write_change_sample(
                                    offset,
                                    plane as u8,
                                    old_value,
                                    final_data,
                                );
                            }
                        }
                        self.planes_flat[plane_offset] = final_data;
                        if track {
                            self.record_vga_plane_write(offset, plane as u8, old_value, final_data);
                        }
                    } else {
                        self.planes_flat[plane_offset] = final_data;
                    }
                }
            }
        }

        changed
    }

    fn write_planar_ega_masked(&mut self, offset: usize, cpu_data: u8, plane_mask: u8) -> bool {
        #[cfg(debug_assertions)]
        assert!(
            !self.vga_pipeline_debug_dirty,
            "VRAM write at offset {offset:#06X} with stale VGA pipeline state \
             — missing apply_vga_pipeline() call after VGA register change"
        );

        let latches = self.latches.get();

        // Debug: trace all EGA writes
        tracing::trace!(
            offset = format_args!("{:04X}", offset),
            cpu = format_args!("{:02X}", cpu_data),
            wm = self.vga.write_mode,
            bm = format_args!("{:02X}", self.vga.bit_mask),
            sr = format_args!("{:02X}", self.vga.set_reset),
            esr = format_args!("{:02X}", self.vga.enable_set_reset),
            dr = format_args!("{:02X}", self.vga.data_rotate),
            lat0 = format_args!("{:02X}", latches[0]),
            lat1 = format_args!("{:02X}", latches[1]),
            lat2 = format_args!("{:02X}", latches[2]),
            lat3 = format_args!("{:02X}", latches[3]),
            "EGA write"
        );

        self.write_planar_ega_core(offset, cpu_data, plane_mask, false)
    }

    #[inline]
    fn write_planar_ega(&mut self, offset: usize, cpu_data: u8) -> bool {
        let plane_mask = self.vga.plane_mask;
        self.write_planar_ega_masked(offset, cpu_data, plane_mask)
    }

    /// EGA/VGA planar write with Time Machine tracking.
    /// This is the tracked version of `write_planar_ega_masked` that records
    /// each plane write for debugging purposes.
    pub fn write_planar_ega_tracked(&mut self, offset: usize, cpu_data: u8) {
        let plane_mask = self.vga.plane_mask;
        let _ = self.write_planar_ega_core(offset, cpu_data, plane_mask, true);
        self.mark_vga_plane_cell_rewritten(offset);
    }

    /// Write a word (little-endian) to memory
    #[inline]
    pub fn write_u16(&mut self, addr: usize, value: u16) {
        // Real-boot high-memory alias: the alias window is ROM — drop writes
        // silently. Single bool-compare when the alias is disabled.
        if self.high_mem_alias_enabled && in_high_mem_alias(addr) {
            return;
        }
        let masked_addr = self.mask_address(addr);
        if self.hle_ems_frame_owned
            && range_overlaps(masked_addr, 2, EMS_FRAME_BASE, EMS_FRAME_SIZE)
        {
            self.write_u8(addr, value as u8);
            self.write_u8(addr.wrapping_add(1), (value >> 8) as u8);
            return;
        }
        // ROM apertures are decoded per byte, so a word that only partly
        // overlaps one must still commit its non-ROM byte. Decompose to
        // `write_u8`, which applies each aperture's point rule (and whose
        // `prepare_write_addr` point-checks the fixed BIOS window). Single
        // branch when no ROM pages are registered. See `rom_region_overlaps`.
        if self.option_rom_region_overlaps(masked_addr, 2) || self.rom_region_overlaps(addr, 2) {
            self.write_u8(addr, value as u8);
            self.write_u8(addr.wrapping_add(1), (value >> 8) as u8);
            return;
        }
        if self.stage_s3_mmio_write(masked_addr, S3MmioWidth::Word, u32::from(value)) {
            return;
        }
        // Fast path: both bytes below video memory, A20 enabled, no tracking
        if addr.wrapping_add(1) < VIDEO_BASE
            && self.a20_enabled
            && !self.write_tracking_enabled
            && !self.lfb_range_overlaps(addr, 2)
        {
            self.mark_instr_cache_dirty_range(addr, 2);
            self.smc_before_write(addr, 2);
            if self.has_descriptor_snoop() {
                self.snoop_gdt_write_ext(addr, value as u64, 2);
            }
            if addr.wrapping_add(1) < self.ram.len() {
                self.ram[addr] = value as u8;
                self.ram[addr + 1] = (value >> 8) as u8;

                self.maybe_trace_phys_write_range(addr, 2, value as u32);
            }

            return;
        }

        // LFB access: write directly to the linear framebuffer buffer.
        if self.write_lfb_span(masked_addr, &value.to_le_bytes(), true) {
            return;
        }
        // An access straddling an aperture edge is decoded per byte. Keep the
        // in-aperture byte in VRAM and let the point decoder route the other.
        if self.lfb_range_overlaps(addr, 2) {
            self.write_u8(addr, value as u8);
            self.write_u8(addr.wrapping_add(1), (value >> 8) as u8);
            return;
        }
        // Original S3 legacy decoding is byte-addressed. Preserve its bank,
        // top-memory, and blocked-range rules for CPU word transactions too;
        // the scalar slow path below does not re-enter the legacy decoder.
        if self.s3_legacy_range_overlaps(masked_addr, 2) {
            self.write_u8(addr, value as u8);
            self.write_u8(addr.wrapping_add(1), (value >> 8) as u8);
            return;
        }

        // Registered MMIO: `write_u8` owns the only dispatch, so a word that
        // fell through to `prepare_write_addr` would have BOTH bytes silently
        // discarded above `MAX_RAM_SIZE`. Decompose instead. The constant
        // compare keeps the `mmio_regions` scan off every RAM and VGA write.
        if masked_addr.wrapping_add(1) >= MAX_RAM_SIZE && self.mmio_range_overlaps(masked_addr, 2) {
            self.write_u8(addr, value as u8);
            self.write_u8(addr.wrapping_add(1), (value >> 8) as u8);
            return;
        }

        let lo = value as u8;
        let hi = (value >> 8) as u8;
        let addr_lo = self.prepare_write_addr(addr);
        let addr_hi = self.prepare_write_addr(addr.wrapping_add(1));

        // Record write for Time Machine (read old value before write)
        // Track at the logical address level for contiguous non-video writes
        let (track, old_value) = if self.write_tracking_enabled {
            if let (Some(a0), Some(a1)) = (addr_lo, addr_hi) {
                let is_video = (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&a0)
                    || (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&a1);
                if !is_video && a1 == a0 + 1 {
                    // Contiguous non-video: track as single 16-bit write
                    let old = self
                        .ram
                        .get(a0..a0 + 2)
                        .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]));
                    (Some(a0), old as u64)
                } else {
                    (None, 0)
                }
            } else {
                (None, 0)
            }
        } else {
            (None, 0)
        };

        match (addr_lo, addr_hi) {
            (None, None) => {}
            (Some(a0), Some(a1)) => {
                if a1 == a0 + 1 {
                    self.smc_before_write(a0, 2);
                    if self.has_descriptor_snoop() {
                        self.snoop_gdt_write_ext(a0, value as u64, 2);
                    }
                } else {
                    self.smc_before_write(a0, 1);
                    self.smc_before_write(a1, 1);
                    if self.has_descriptor_snoop() {
                        self.snoop_gdt_write_ext(a0, lo as u64, 1);
                        self.snoop_gdt_write_ext(a1, hi as u64, 1);
                    }
                }
                self.write_u8_inner(a0, lo);
                self.write_u8_inner(a1, hi);
            }
            (Some(a0), None) => {
                self.smc_before_write(a0, 1);
                if self.has_descriptor_snoop() {
                    self.snoop_gdt_write_ext(a0, lo as u64, 1);
                }
                self.write_u8_inner(a0, lo);
            }
            (None, Some(a1)) => {
                self.smc_before_write(a1, 1);
                if self.has_descriptor_snoop() {
                    self.snoop_gdt_write_ext(a1, hi as u64, 1);
                }
                self.write_u8_inner(a1, hi);
            }
        }

        // Record the write after it completes
        if let Some(a0) = track {
            self.record_write(a0, old_value, value as u64, 2);
        }
    }

    /// Write 32-bit value to memory
    pub fn write_u32(&mut self, addr: usize, value: u32) {
        // Real-boot high-memory alias: the alias window is ROM — drop writes
        // silently. Single bool-compare when the alias is disabled.
        if self.high_mem_alias_enabled && in_high_mem_alias(addr) {
            return;
        }
        let masked_addr = self.mask_address(addr);
        if self.hle_ems_frame_owned
            && range_overlaps(masked_addr, 4, EMS_FRAME_BASE, EMS_FRAME_SIZE)
        {
            for (i, byte) in value.to_le_bytes().iter().copied().enumerate() {
                self.write_u8(addr.wrapping_add(i), byte);
            }
            return;
        }
        // ROM apertures are decoded per byte, so a dword that only partly
        // overlaps one must still commit its non-ROM bytes. Decompose to
        // `write_u8`, which applies each aperture's point rule (and whose
        // `prepare_write_addr` point-checks the fixed BIOS window). Single
        // branch when no ROM pages are registered. See `rom_region_overlaps`.
        if self.option_rom_region_overlaps(masked_addr, 4) || self.rom_region_overlaps(addr, 4) {
            for (i, byte) in value.to_le_bytes().iter().copied().enumerate() {
                self.write_u8(addr.wrapping_add(i), byte);
            }
            return;
        }
        if self.stage_s3_mmio_write(masked_addr, S3MmioWidth::Dword, value) {
            return;
        }
        // LFB access: write directly to the linear framebuffer buffer.
        if self.write_lfb_span(masked_addr, &value.to_le_bytes(), true) {
            return;
        }
        // An access straddling an aperture edge is decoded per byte. This
        // preserves partial-commit behavior on both sides of the window.
        if self.lfb_range_overlaps(addr, 4) {
            for (i, byte) in value.to_le_bytes().iter().copied().enumerate() {
                self.write_u8(addr.wrapping_add(i), byte);
            }
            return;
        }
        // As for words above, a dword in the S3 legacy window must retain
        // the exact per-byte aperture semantics rather than falling through
        // to the generic VGA/RAM scalar store.
        if self.s3_legacy_range_overlaps(masked_addr, 4) {
            for (i, byte) in value.to_le_bytes().iter().copied().enumerate() {
                self.write_u8(addr.wrapping_add(i), byte);
            }
            return;
        }

        // Registered MMIO: `write_u8` owns the only dispatch, so a dword that
        // fell through to `prepare_write_addr` would have ALL FOUR bytes
        // silently discarded above `MAX_RAM_SIZE`. Decompose instead. The
        // constant compare keeps the `mmio_regions` scan off every RAM and
        // VGA write.
        if masked_addr.wrapping_add(3) >= MAX_RAM_SIZE && self.mmio_range_overlaps(masked_addr, 4) {
            for (i, byte) in value.to_le_bytes().iter().copied().enumerate() {
                self.write_u8(addr.wrapping_add(i), byte);
            }
            return;
        }

        let bytes = value.to_le_bytes();
        let addr0 = self.prepare_write_addr(addr);
        let addr1 = self.prepare_write_addr(addr.wrapping_add(1));
        let addr2 = self.prepare_write_addr(addr.wrapping_add(2));
        let addr3 = self.prepare_write_addr(addr.wrapping_add(3));

        // Record write for Time Machine (read old value before write)
        // Track at the logical address level for contiguous non-video writes
        let (track, old_value) = if self.write_tracking_enabled {
            if let (Some(a0), Some(a1), Some(a2), Some(a3)) = (addr0, addr1, addr2, addr3) {
                let is_video = (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&a0)
                    || (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&a3);
                if !is_video && a1 == a0 + 1 && a2 == a0 + 2 && a3 == a0 + 3 {
                    // Contiguous non-video: track as single 32-bit write
                    let old = self
                        .ram
                        .get(a0..a0 + 4)
                        .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
                    (Some(a0), old as u64)
                } else {
                    (None, 0)
                }
            } else {
                (None, 0)
            }
        } else {
            (None, 0)
        };

        if let (Some(a0), Some(a1), Some(a2), Some(a3)) = (addr0, addr1, addr2, addr3) {
            if a1 == a0 + 1 && a2 == a0 + 2 && a3 == a0 + 3 {
                self.smc_before_write(a0, 4);
                if self.has_descriptor_snoop() {
                    self.snoop_gdt_write_ext(a0, value as u64, 4);
                }
            } else {
                self.smc_before_write(a0, 1);
                self.smc_before_write(a1, 1);
                self.smc_before_write(a2, 1);
                self.smc_before_write(a3, 1);
                if self.has_descriptor_snoop() {
                    self.snoop_gdt_write_ext(a0, bytes[0] as u64, 1);
                    self.snoop_gdt_write_ext(a1, bytes[1] as u64, 1);
                    self.snoop_gdt_write_ext(a2, bytes[2] as u64, 1);
                    self.snoop_gdt_write_ext(a3, bytes[3] as u64, 1);
                }
            }
            self.write_u8_inner(a0, bytes[0]);
            self.write_u8_inner(a1, bytes[1]);
            self.write_u8_inner(a2, bytes[2]);
            self.write_u8_inner(a3, bytes[3]);

            // Record the write after it completes
            if let Some(a0) = track {
                self.record_write(a0, old_value, value as u64, 4);
            }
            return;
        }

        // Non-contiguous writes: fall through to byte-by-byte (not tracked as single write)
        if let Some(a0) = addr0 {
            self.smc_before_write(a0, 1);
            if self.has_descriptor_snoop() {
                self.snoop_gdt_write_ext(a0, bytes[0] as u64, 1);
            }
            self.write_u8_inner(a0, bytes[0]);
        }
        if let Some(a1) = addr1 {
            self.smc_before_write(a1, 1);
            if self.has_descriptor_snoop() {
                self.snoop_gdt_write_ext(a1, bytes[1] as u64, 1);
            }
            self.write_u8_inner(a1, bytes[1]);
        }
        if let Some(a2) = addr2 {
            self.smc_before_write(a2, 1);
            if self.has_descriptor_snoop() {
                self.snoop_gdt_write_ext(a2, bytes[2] as u64, 1);
            }
            self.write_u8_inner(a2, bytes[2]);
        }
        if let Some(a3) = addr3 {
            self.smc_before_write(a3, 1);
            if self.has_descriptor_snoop() {
                self.snoop_gdt_write_ext(a3, bytes[3] as u64, 1);
            }
            self.write_u8_inner(a3, bytes[3]);
        }
    }

    /// Write a byte to memory for JIT callouts (skips SMC tracking).
    /// Supports Time Machine write tracking when enabled.
    /// Force non-inline to prevent stack overflow in release mode JIT callbacks.
    #[inline(never)]
    pub fn write_u8_jit(&mut self, addr: usize, value: u8) {
        debug_assert!(
            !self.has_rom_regions,
            "JIT writer invoked while ROM regions active — real-boot is interpreter-only"
        );
        debug_assert!(
            !self.high_mem_alias_enabled,
            "JIT writer invoked while high-mem alias active — real-boot is interpreter-only"
        );

        let masked_addr = self.mask_address(addr);
        if self.write_ems_frame_u8(masked_addr, value) {
            return;
        }
        if self.option_rom_region_overlaps(masked_addr, 1) {
            return;
        }
        if self.stage_s3_mmio_write(masked_addr, S3MmioWidth::Byte, u32::from(value)) {
            return;
        }
        if self.write_lfb_span(addr, &[value], false) {
            return;
        }
        let Some(addr) = self.prepare_write_addr(addr) else {
            return;
        };

        // Skip video memory tracking (handled separately by VGA planar functions)
        let is_video = (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&addr);
        let old_value = if self.write_tracking_enabled && !is_video {
            self.ram.get(addr).copied().unwrap_or(0)
        } else {
            0
        };

        self.write_u8_inner(addr, value);

        // Record write for Time Machine (non-video memory only)
        if !is_video {
            self.record_write(addr, old_value as u64, value as u64, 1);
        }
    }

    /// Pure guard for the ND2A VGA no-SMC JIT word writer.
    #[inline]
    pub fn can_write_u16_vga_no_smc_jit(&self, addr: usize) -> bool {
        let Some(end) = addr.checked_add(1) else {
            return false;
        };
        if !(VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&addr)
            || !(VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&end)
            || self.s3_mmio_range_overlaps(addr, 2)
        {
            return false;
        }
        if end >= self.ram.len() {
            return false;
        }
        !self.is_rom_addr(addr) && !self.is_rom_addr(end)
    }

    /// Write a VGA word for JIT fast paths that have already proven the span,
    /// A20, paging, and SMC bitmap preconditions.
    ///
    /// This deliberately skips SMC prewrite/notifier/post-write handling, but
    /// keeps the normal byte-level VGA machinery in `write_u8_inner()`.
    /// This is only for the ND2A no-SMC VGA JIT path; it is not a general
    /// memory write primitive.
    #[inline(never)]
    pub fn write_u16_vga_no_smc_jit(&mut self, addr: usize, value: u16) -> bool {
        if !self.can_write_u16_vga_no_smc_jit(addr) {
            return false;
        }
        let end = addr + 1;
        debug_assert!(
            (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&addr)
                && (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&end),
            "write_u16_vga_no_smc_jit called outside VGA aperture"
        );

        self.write_u8_inner(addr, value as u8);
        self.write_u8_inner(end, (value >> 8) as u8);
        true
    }

    /// Record post-write instruction-cache side effects for JIT inline stores.
    ///
    /// Inline JIT stores can bypass `write_u*_jit()` and write RAM bytes directly.
    /// This helper mirrors the instruction-cache dirty marking behavior for the
    /// addresses that would be writable via normal memory semantics.
    #[inline(never)]
    pub fn note_jit_write(&mut self, addr: usize, size: usize) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }

        let masked_addr = self.mask_address(addr);
        if self.hle_ems_frame_owned
            && range_overlaps(masked_addr, size, EMS_FRAME_BASE, EMS_FRAME_SIZE)
        {
            for i in 0..size {
                let physical = self.mask_address(addr.wrapping_add(i));
                match self.resolve_ems_frame_addr(physical) {
                    EmsFrameResolution::NotFrame => {
                        if let Some(resolved) = self.prepare_write_addr(addr.wrapping_add(i)) {
                            self.mark_instr_cache_dirty_range(resolved, 1);
                        }
                    }
                    EmsFrameResolution::Unmapped { frame_addr } => {
                        self.mark_instr_cache_dirty_range(frame_addr, 1);
                    }
                    EmsFrameResolution::Mapped {
                        pool_page,
                        pool_offset,
                        ..
                    } => {
                        if let Ok(aliases) =
                            self.ems_alias_ranges_for_backing_range(pool_page, pool_offset, 1)
                        {
                            for alias in aliases {
                                self.mark_instr_cache_dirty_range(alias.frame_addr, alias.len);
                            }
                        }
                    }
                }
            }
            return;
        }

        let mut resolved: [Option<usize>; 4] = [None, None, None, None];
        for i in 0..size {
            resolved[i] = self.prepare_write_addr(addr.wrapping_add(i));
        }

        if resolved[..size].iter().all(Option::is_none) {
            return;
        }

        let contiguous = resolved[..size]
            .windows(2)
            .all(|pair| matches!(pair, [Some(a), Some(b)] if *b == *a + 1));

        if contiguous {
            if let Some(start) = resolved[0] {
                self.mark_instr_cache_dirty_range(start, size);
            }
            return;
        }

        for maybe_addr in &resolved[..size] {
            if let Some(addr) = *maybe_addr {
                self.mark_instr_cache_dirty_range(addr, 1);
            }
        }
    }

    /// Read 64-bit value from memory
    pub fn read_u64(&self, addr: usize) -> u64 {
        let low = self.read_u32(addr) as u64;
        let high = self.read_u32(addr + 4) as u64;
        low | (high << 32)
    }

    /// Write 64-bit value to memory
    pub fn write_u64(&mut self, addr: usize, value: u64) {
        self.write_u32(addr, (value & 0xFFFF_FFFF) as u32);
        self.write_u32(addr + 4, (value >> 32) as u32);
    }

    /// Read a byte using segment:offset addressing (real mode)
    #[inline]
    pub fn read_u8_seg(&self, segment: u16, offset: u16) -> u8 {
        self.read_u8(Self::seg_offset_to_linear(segment, offset))
    }

    #[inline]
    pub fn read_u16_seg(&self, segment: u16, offset: u16) -> u16 {
        self.read_u16(Self::seg_offset_to_linear(segment, offset))
    }

    #[inline]
    pub fn write_u8_seg(&mut self, segment: u16, offset: u16, value: u8) {
        self.write_u8(Self::seg_offset_to_linear(segment, offset), value);
    }

    #[inline]
    pub fn write_u16_seg(&mut self, segment: u16, offset: u16, value: u16) {
        self.write_u16(Self::seg_offset_to_linear(segment, offset), value);
    }

    /// Enable/disable A20 gate
    pub fn set_a20(&mut self, enabled: bool) {
        if self.a20_enabled != enabled {
            self.a20_enabled = enabled;
            // A20 changes alter physical addressing, not just cached
            // translations, so direct-memory assumptions must be invalidated.
            self.bump_memory_policy_epoch();
            self.bump_a20_generation();
        }
    }

    /// A20 gate state
    #[inline(always)]
    pub fn a20_enabled(&self) -> bool {
        self.a20_enabled
    }

    /// Bulk-copy within RAM, bypassing VGA planar logic.
    /// Returns `false` if either range overlaps the VGA window [A0000..C0000).
    pub fn bulk_copy_ram(&mut self, src: usize, dst: usize, len: usize) -> bool {
        if len == 0 {
            return true;
        }
        let vga_start = VIDEO_BASE; // 0xA0000
        let vga_end = 0xC0000;
        if (src < vga_end && src + len > vga_start) || (dst < vga_end && dst + len > vga_start) {
            return false;
        }
        if self.hle_ems_frame_owned
            && (range_overlaps(src, len, EMS_FRAME_BASE, EMS_FRAME_SIZE)
                || range_overlaps(dst, len, EMS_FRAME_BASE, EMS_FRAME_SIZE))
        {
            return false;
        }
        if self.lfb_range_overlaps(src, len)
            || self.lfb_range_overlaps(dst, len)
            || self.option_rom_region_overlaps(src, len)
            || self.option_rom_region_overlaps(dst, len)
        {
            return false;
        }
        // A copy must honour the same write protection an ordinary store does
        // (`prepare_write_addr`): the BIOS F-segment while ROM writes are
        // disallowed, and any marked ROM range. The F-segment holds the HLE
        // interrupt stubs the IVT points at, and an XMS move with a handle-0
        // destination of F000:0000 overwrote them here while the per-byte path
        // dropped the same bytes (MDD-BUG-CRU-01470). Refusing sends the
        // caller to its per-byte fallback, which applies the policy byte by
        // byte.
        if self.rom_region_overlaps(dst, len)
            || (!self.allow_rom_writes && range_overlaps(dst, len, BIOS_BASE, 0x10000))
        {
            return false;
        }
        // A copy onto the GDT/LDT must run the descriptor snoop, the only path
        // that advances the descriptor generation native engines check their
        // cached segment proofs against. Refuse, as bulk_write_span_is_plain_ram
        // does, so the caller's per-byte fallback snoops (MDD-BUG-CRU-01489).
        if self.descriptor_snoop_overlaps(dst, len) {
            return false;
        }
        let ram_len = self.ram.len();
        let copy_len = if src.max(dst) >= arena::ARENA_PHYS_START {
            // Shared arena (HLD §5.2): copy only when both spans are wholly
            // backed; otherwise the caller's per-byte fallback applies the
            // open-bus and dropped-store rules byte by byte.
            let backed = |start: usize| {
                start
                    .checked_add(len)
                    .is_some_and(|end| self.ram.is_backed(start..end))
            };
            if !(backed(src) && backed(dst)) {
                return false;
            }
            len
        } else {
            if src >= ram_len || dst >= ram_len {
                return true;
            }
            len.min(ram_len - src).min(ram_len - dst)
        };
        if copy_len == 0 {
            return true;
        }
        let sample_size = if self.write_tracking_enabled && self.write_context.is_some() {
            match copy_len {
                8.. => 8,
                4.. => 4,
                2.. => 2,
                _ => 1,
            }
        } else {
            0
        };
        let old_value = if sample_size == 0 {
            0
        } else {
            pack_write_value(&self.ram[dst..dst + sample_size])
        };
        self.mark_instr_cache_dirty_range(dst, copy_len);
        self.smc_before_write(dst, copy_len);
        self.ram.copy_within(src..src + copy_len, dst);
        if sample_size != 0 {
            let new_value = pack_write_value(&self.ram[dst..dst + sample_size]);
            self.record_bulk_write(dst, old_value, new_value, sample_size as u8, copy_len);
        }

        true
    }

    /// Bulk-read from RAM into a caller buffer, bypassing VGA planar logic.
    /// Returns `false` if the source range overlaps the VGA window [A0000..C0000).
    pub fn bulk_read_ram(&self, src: usize, len: usize, dst: &mut [u8]) -> bool {
        if len == 0 || dst.is_empty() {
            return true;
        }
        if src < 0xC0000 && src + len > VIDEO_BASE {
            return false;
        }
        if self.hle_ems_frame_owned && range_overlaps(src, len, EMS_FRAME_BASE, EMS_FRAME_SIZE) {
            return false;
        }
        if self.lfb_range_overlaps(src, len) || self.option_rom_region_overlaps(src, len) {
            return false;
        }
        if src >= arena::ARENA_PHYS_START {
            // Shared arena (HLD §5.2): serve a wholly backed span, and send
            // anything else to the caller's per-byte fallback.
            let copy_len = len.min(dst.len());
            let Some(bytes) = src
                .checked_add(copy_len)
                .and_then(|end| self.ram.get(src..end))
            else {
                return false;
            };
            dst[..copy_len].copy_from_slice(bytes);
            return true;
        }
        if src >= self.ram.len() {
            return true;
        }
        let copy_len = len.min(self.ram.len() - src).min(dst.len());
        dst[..copy_len].copy_from_slice(&self.ram[src..src + copy_len]);
        true
    }

    /// Write directly to BIOS ROM area (bypasses ROM protection)
    /// Used for BIOS initialization (IVT setup, etc.)
    #[inline]
    pub fn write_rom(&mut self, addr: usize, value: u8) {
        let addr = self.mask_address(addr);
        if addr < self.ram.len() {
            // RC-1: this helper can land anywhere in RAM (it bypasses only
            // the ROM write-protect gate), so it must notify the SMC snoop
            // like every other write channel. Free when no notifier is
            // bound.
            self.smc_before_write(addr, 1);
            self.mark_instr_cache_dirty_range(addr, 1);
            self.ram[addr] = value;
        }
    }

    /// Get direct access to video memory region
    pub fn video_memory(&self) -> &[u8] {
        &self.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE]
    }

    pub fn video_memory_mut(&mut self) -> &mut [u8] {
        &mut self.ram[VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE]
    }

    /// Sync linear chain-4 memory (A0000) into planar VGA planes.
    /// This keeps Mode X and chain-4 views coherent over the 64 KiB A000 window
    /// when toggling chain-4, using plane cell `offset & !3`.
    pub fn sync_chain4_to_planes(&mut self) {
        let plane_len = PLANE_SIZE;
        for offset in 0..VIDEO_WINDOW_64K {
            let plane = offset & 3;
            let plane_offset = offset & !3;
            let val = self.ram[VIDEO_BASE + offset];
            self.planes_flat[plane * plane_len + plane_offset] = val;
        }
        // Chain-4 mapping reaches cells through 0xFFFC in every plane,
        // covering all 64 interleaved VRAM pages.
        self.mark_vga_plane_rewrites(VGA_PLANE_VRAM_ALL_PAGES);
    }

    /// Sync planar VGA planes into linear chain-4 memory (A0000).
    /// This keeps Mode X and chain-4 views coherent over the 64 KiB A000 window
    /// when toggling chain-4, using plane cell `offset & !3`.
    pub fn sync_planes_to_chain4(&mut self) {
        let plane_len = PLANE_SIZE;
        for offset in 0..VIDEO_WINDOW_64K {
            let plane = offset & 3;
            let plane_offset = offset & !3;
            let val = self.planes_flat[plane * plane_len + plane_offset];
            self.ram[VIDEO_BASE + offset] = val;
        }
    }

    /// Commit a single plain chain-4 VGA store byte directly (the dyncore native
    /// fast path), reproducing the interpreter chain-4 write handler's effect
    /// byte-for-byte in the plain pipeline state gated by
    /// [`Memory::vga_chain4_plain_store_range`].
    ///
    /// Dual-writes BOTH buffers the interpreter keeps coherent: the
    /// de-interleaved `planes_flat` (what chain-4 CPU reads and `hash_state`
    /// observe) AND the linear `ram` mirror (what Mode 13h scanout reads). In the
    /// plain state the written byte equals the raw value (write-mode 0, no
    /// rotate/logic-op, no set/reset, full bit-mask), so no GC-pipeline decode is
    /// needed — the interpreter's chain-4 arm computes exactly `ram[addr] =
    /// planes_flat[plane*PLANE_SIZE + (offset & !3)] = value` in that state.
    ///
    /// The caller MUST have already admitted the address via
    /// `vga_chain4_plain_store_range`; the `debug_assert` fails loud on any
    /// future misuse against a non-plain state or out-of-window address.
    /// EXP-IQ002 L2: the interpreter chain-4 fast-arm commit — one aperture
    /// byte, ALL the side effects the interpreter's full route would perform
    /// that [`Memory::commit_vga_chain4_plain_byte`] alone does not:
    /// decoded-instruction-cache dirtying (the interp CAN source code from
    /// VRAM — the dbx skip's justification does not transfer, panel lens 2),
    /// the SVGA-sync video-page dirty mark (dbx compensates with a coarse mark
    /// whenever it folds a non-zero native commit count), and the
    /// always-on engagement counter (unit: BYTES committed). The caller owns
    /// the admission gate: cached kind == Chain4, whole access inside
    /// `[VIDEO_BASE, VIDEO_BASE + VIDEO_WINDOW_64K)`, group-of-four not
    /// crossed for multi-byte stores.
    /// EXP-A029: DRC helper-level native VGA commit for one admitted span
    /// (chain-4 or plain planar). The caller (`checked_store_bits`) owns the
    /// admission gate: `vga_native_commit_kind()` armed, one-page span,
    /// width <= 4, the per-store range predicate for `kind`, and (unpaged)
    /// no DPMI physical mapping overlapping the span. Skip-set soundness,
    /// established by the A029 adversarial panel:
    /// - No watch veto: VGA pages are never source-cached by any dynrec
    ///   (`direct_host_ram_range` rejects the aperture), so the SMC page
    ///   watch can never cover them; the only data-watch veto in the store
    ///   path lives in `tlb_direct_write_ptr`, which always misses VGA.
    /// - SMC span/notifier checks are vacuous for the same reason.
    /// - Descriptor-snoop skip inherits the interp fast arm's and the dbx
    ///   arm's shared assumption that descriptor tables do not live in the
    ///   VGA aperture (unenforced; shared with both precedents).
    /// - The PT-write tripwire is NOT skipped (page tables in VRAM are
    ///   legal): both precedents miss this; this seam closes it.
    ///
    /// The commit fns mark video dirty pages and count engagement in BYTES
    /// (`native_video_writes`), matching the general route's granularity.
    #[inline]
    pub fn drc_commit_vga_native_span(
        &mut self,
        kind: VgaCommitKind,
        physical_addr: usize,
        value: u64,
        width: usize,
    ) {
        debug_assert!(width <= 4);
        self.mark_instr_cache_dirty_range(physical_addr, width);
        self.pt_tripwire_note_physical_write(physical_addr, width);
        let bytes = value.to_le_bytes();
        for (index, byte) in bytes[..width].iter().enumerate() {
            match kind {
                VgaCommitKind::Chain4 => {
                    self.commit_vga_chain4_plain_byte(physical_addr + index, *byte);
                }
                VgaCommitKind::Planar => {
                    self.commit_vga_planar_plain_byte(physical_addr + index, *byte);
                }
            }
        }
    }

    /// Commit one admitted run of native VGA store bytes, in guest order, as a
    /// REP MOVS/STOS page span does. The result, every side effect and every
    /// counter equal [`Self::drc_commit_vga_native_span`] applied to the bytes
    /// in order: each byte lands in the same plane cells and linear mirror, each
    /// touched page is marked dirty, and `native_video_writes` and the bus count
    /// advance by one per byte. The instruction-cache and PT-write tripwire notes
    /// cover the whole run at once; both are range tests whose outcome does not
    /// depend on how the range is split.
    ///
    /// The caller owns the admission, exactly as for the per-store commit:
    /// `kind` armed, the whole run inside one page, and the per-store range
    /// predicate for `kind` true of the whole run.
    pub fn drc_commit_vga_native_bytes(
        &mut self,
        kind: VgaCommitKind,
        physical_addr: usize,
        bytes: &[u8],
    ) {
        let len = bytes.len();
        if len == 0 {
            return;
        }
        debug_assert!(
            match kind {
                VgaCommitKind::Chain4 => self.vga_chain4_plain_store_range(physical_addr, len),
                VgaCommitKind::Planar => self.vga_planar_plain_store_range(physical_addr, len),
            },
            "drc_commit_vga_native_bytes outside its admission at {physical_addr:#x}+{len}"
        );
        self.mark_instr_cache_dirty_range(physical_addr, len);
        self.pt_tripwire_note_physical_write(physical_addr, len);
        let offset = physical_addr - VIDEO_BASE;
        match kind {
            VgaCommitKind::Chain4 => {
                // Chain-4 forces the plane from A1:A0: byte `offset + i` lands in
                // plane `(offset + i) & 3` at cell `(offset + i) & !3`, and the
                // linear mirror takes the raw run.
                for (index, &byte) in bytes.iter().enumerate() {
                    let at = offset + index;
                    self.planes_flat[(at & 3) * PLANE_SIZE + (at & !3)] = byte;
                }
                self.ram[physical_addr..physical_addr + len].copy_from_slice(bytes);
            }
            VgaCommitKind::Planar => {
                // Every plane the live Map Mask selects takes the run at the same
                // offset; the linear mirror is untouched (see
                // `commit_vga_planar_plain_byte`).
                let plane_mask = self.vga.plane_mask;
                for plane in 0..4usize {
                    if plane_mask & (1 << plane) != 0 {
                        let base = plane * PLANE_SIZE + offset;
                        self.planes_flat[base..base + len].copy_from_slice(bytes);
                    }
                }
            }
        }
        let first_page = offset >> 12;
        let last_page = (offset + len - 1) >> 12;
        for page in first_page..=last_page {
            self.mark_video_page_dirty(VIDEO_BASE + (page << 12));
        }
        self.add_native_video_writes(len as u64);
    }

    /// Note a drc-native raw write to direct host RAM for instruction-cache
    /// coherence.
    ///
    /// The dynrec-drc checked-store legs write `ram[addr]` directly once every
    /// byte carries a `direct_host_ram_range` proof, which is much cheaper than
    /// `write_u8` but skips its side effects. One of those matters for
    /// correctness: `write_u8`'s fast path calls
    /// [`Self::mark_instr_cache_dirty_range`], and without it a drc-native code
    /// patch never sets `instr_cache_dirty`, so the INTERPRETER can keep serving
    /// a stale decoded instruction for a page it had already executed from
    /// (MDD-BUG-FLUX-00145).
    ///
    /// That is reachable rather than theoretical: the direct-RAM proof excludes
    /// video, LFB, EMS and ROM, so these stores land in ordinary guest RAM —
    /// which is exactly where guest code lives — and the interpreter still runs
    /// under `--engine dynrec-drc` for every 16-bit, real-mode, V86 and gap
    /// fallback. The drc's own block cache is already handled separately by
    /// `touches_code` / `invalidate_changed_cache_physical_spans`; this is the
    /// interpreter's decoded cache, which nothing else covers on this leg.
    ///
    /// Kept as cheap as the sibling VGA leg
    /// ([`Self::drc_commit_vga_native_span`], whose equivalent call `drc_core`'s
    /// T6 pins as load-bearing): `mark_instr_cache_dirty_range` returns
    /// immediately when the flag is already set, and otherwise only tests the
    /// exec-page bits for the touched pages.
    #[inline]
    pub fn drc_note_direct_ram_write(&mut self, physical_addr: usize, len: usize) {
        self.mark_instr_cache_dirty_range(physical_addr, len);
    }

    /// The interpreter's chain-4 fast arm. Like
    /// [`Self::drc_commit_vga_native_span`] it does NOT skip the PT-write
    /// tripwire: page tables in VRAM are legal, and this arm serves 16-bit,
    /// real-mode and V86 code under every engine (MDD-BUG-CRU-01474).
    #[inline]
    pub fn interp_commit_vga_chain4_fast(&mut self, physical_addr: usize, byte: u8) {
        self.mark_instr_cache_dirty_range(physical_addr, 1);
        self.pt_tripwire_note_physical_write(physical_addr, 1);
        self.commit_vga_chain4_plain_byte(physical_addr, byte);
        self.mark_video_page_dirty(physical_addr);
        self.interp_vga_chain4_fast_commits = self.interp_vga_chain4_fast_commits.wrapping_add(1);
    }

    pub fn commit_vga_chain4_plain_byte(&mut self, physical_addr: usize, byte: u8) {
        debug_assert!(
            self.vga_chain4_plain_store_range(physical_addr, 1),
            "commit_vga_chain4_plain_byte against a non-plain-chain4 state or out-of-window address {physical_addr:#x}"
        );
        let offset = physical_addr - VIDEO_BASE;
        let plane = offset & 3;
        let plane_offset = offset & !3;
        self.planes_flat[plane * PLANE_SIZE + plane_offset] = byte;
        self.ram[physical_addr] = byte;
        // The renderer learns a page changed only from `video_dirty_pages`.
        // The interpreter's chain-4 path and the native PLANAR commit below
        // both mark it; this one did not, so the same guest store was visible
        // to the renderer under the interpreter and invisible under the native
        // chain-4 path.
        self.mark_video_page_dirty(physical_addr);
        self.increment_native_video_writes();
    }

    pub fn commit_vga_planar_plain_byte(&mut self, physical_addr: usize, byte: u8) {
        debug_assert!(
            self.vga_planar_plain_store_range(physical_addr, 1),
            "commit_vga_planar_plain_byte against a non-plain-planar state or out-of-window address {physical_addr:#x}"
        );
        let offset = physical_addr - VIDEO_BASE;
        let plane_mask = self.vga.plane_mask;
        for plane in 0..4usize {
            if plane_mask & (1 << plane) != 0 {
                self.planes_flat[plane * PLANE_SIZE + offset] = byte;
            }
        }
        self.mark_video_page_dirty(physical_addr);
        self.increment_native_video_writes();
    }

    /// Write to video memory in Mode X (planar) format
    /// `plane_mask`: 4-bit mask indicating which planes to write (from Map Mask register)
    /// addr: offset within video memory (0xA0000-based)
    /// value: byte to write
    pub fn write_video_planar(&mut self, offset: usize, value: u8, plane_mask: u8) {
        self.mark_vga_plane_cell_rewritten(offset);
        // In Mode X, each "address" maps to 4 planes.
        // plane_mask bits 0-3 enable writing to planes 0-3.
        for plane in 0..4 {
            if plane_mask & (1 << plane) != 0 {
                let idx = plane * PLANE_SIZE + offset;
                if idx < self.planes_flat.len() {
                    self.planes_flat[idx] = value;
                }
            }
        }
    }

    /// Write to video memory in Mode X (planar) format with Time Machine tracking.
    /// This is the tracked version that records each plane write for debugging.
    pub fn write_video_planar_tracked(&mut self, offset: usize, value: u8, plane_mask: u8) {
        self.mark_vga_plane_cell_rewritten(offset);
        for plane in 0..4u8 {
            if plane_mask & (1 << plane) != 0 {
                let idx = (plane as usize) * PLANE_SIZE + offset;
                if idx < self.planes_flat.len() {
                    let old_value = self.planes_flat[idx];
                    self.planes_flat[idx] = value;
                    self.record_vga_plane_write(offset, plane, old_value, value);
                }
            }
        }
    }

    /// Read from video memory in Mode X (planar) format
    /// `read_plane`: which plane to read from (from Read Map Select register)
    /// offset: offset within video memory (0xA0000-based)
    pub fn read_video_planar(&self, offset: usize, read_plane: u8) -> u8 {
        let plane = (read_plane & 0x03) as usize;
        let idx = plane * PLANE_SIZE + offset;
        if idx < self.planes_flat.len() {
            self.planes_flat[idx]
        } else {
            0
        }
    }

    // ========================================================================
    // VBE 2.0 Linear Framebuffer (LFB) Support
    // ========================================================================

    /// Initialize or resize the linear framebuffer for SVGA modes
    /// Called by VGA when setting an SVGA mode with LFB enabled
    pub fn init_lfb(&mut self, size: usize) {
        let size = size.min(LFB_MAX_SIZE);
        if self.lfb.len() != size {
            self.lfb.resize(size, 0);
            self.lfb_dirty_pages = [0; LFB_DIRTY_WORDS];
            self.bump_memory_policy_epoch();
        }
    }

    /// Cache the CPU's CR3 value for VGA LFB page table walking
    pub fn set_last_cr3(&mut self, cr3: u32) {
        self.last_cr3 = cr3;
    }

    /// Get the last cached CR3 value
    pub fn last_cr3(&self) -> u32 {
        self.last_cr3
    }

    /// Replace the guest-visible LFB decoder atomically.
    ///
    /// DISPI keeps both historical aliases. S3 exposes only its configured
    /// base and size. The backing remains the single canonical LFB in either
    /// personality.
    pub fn configure_lfb_decoder(&mut self, config: LfbDecoderConfig) {
        if let LfbDecoderConfig::S3 {
            base,
            size,
            vram_offset,
            enabled,
        } = config
        {
            assert!(
                size <= LFB_MAX_SIZE,
                "S3 LFB aperture exceeds 4 MiB backing"
            );
            assert!(
                !enabled || size != 0,
                "enabled S3 LFB aperture has zero size"
            );
            assert!(
                base.checked_add(size).is_some(),
                "S3 LFB aperture range overflows guest physical address space"
            );
            assert!(
                vram_offset < LFB_MAX_SIZE,
                "S3 LFB aperture starts beyond its 4 MiB VRAM backing"
            );
        }
        if self.lfb_decoder != config {
            self.lfb_decoder = config;
            self.bump_memory_policy_epoch();
            // An S3 linear window over the legacy hole claims A0000h bytes
            // ahead of the VGA arm (see `compute_vga_native_commit`).
            self.vga_native_commit = self.compute_vga_native_commit();
        }
    }

    /// Current guest-visible LFB decoder state.
    pub fn lfb_decoder(&self) -> LfbDecoderConfig {
        self.lfb_decoder
    }

    /// Atomically replace the original S3 legacy-aperture routing state.
    pub fn configure_s3_legacy_aperture(&mut self, config: S3LegacyApertureConfig) {
        if self.s3_legacy_aperture != config {
            self.s3_legacy_aperture = config;
            self.bump_memory_policy_epoch();
            // The native-commit admission consults `s3_legacy_aperture.active`
            // (see `compute_vga_native_commit`); this toggle is port-I/O-
            // reachable only, so recomputing here keeps the cached flag
            // coherent for the interpreter (live reads) and the dbx mirror
            // (republished after scalar I/O).
            self.vga_native_commit = self.compute_vga_native_commit();
        }
    }

    #[inline]
    fn s3_legacy_direct_lfb_offset(&self, addr: usize) -> Option<usize> {
        let config = self.s3_legacy_aperture;
        if !config.active
            || !config.memory_response
            || !config.a000_response
            || !(VIDEO_BASE..VIDEO_BASE + 0x10000).contains(&addr)
            || (!config.enhanced_mapping && !config.top_memory_access)
            || self.lfb.is_empty()
        {
            return None;
        }
        let aperture_offset = addr - VIDEO_BASE;
        if config.top_memory_access {
            // CR3A.3 redirects accesses with A13 clear/set to the top
            // 32/64 KiB of fitted VRAM respectively. The databook describes
            // the two windows but not a gate-level equation; preserving the
            // selected window's low address bits is the literal mapping.
            let span = if aperture_offset & 0x2000 == 0 {
                0x8000
            } else {
                0x10000
            };
            let base = self.lfb.len().saturating_sub(span);
            Some((base + (aperture_offset & (span - 1))) % self.lfb.len())
        } else {
            Some((config.bank_offset + aperture_offset) % self.lfb.len())
        }
    }

    #[inline]
    fn s3_legacy_access_blocked(&self, addr: usize) -> bool {
        let config = self.s3_legacy_aperture;
        config.active
            && (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&addr)
            && (!config.memory_response
                || (config.enhanced_mapping && addr >= VIDEO_BASE + 0x10000)
                || ((VIDEO_BASE..VIDEO_BASE + 0x10000).contains(&addr) && !config.a000_response))
    }

    /// True when the S3 legacy aperture leaves every byte of the 64 KiB
    /// A0000h window to the plain VGA path in `write_u8`: it neither blocks it
    /// ([`Self::s3_legacy_access_blocked`]) nor redirects it to VRAM
    /// ([`Self::s3_legacy_direct_lfb_offset`]). This is the state a DOS program
    /// in mode 13h or mode X runs in on the real-boot S3 card.
    #[inline]
    fn s3_legacy_aperture_passes_a000(&self) -> bool {
        let config = self.s3_legacy_aperture;
        !config.active
            || (config.memory_response
                && config.a000_response
                && !config.enhanced_mapping
                && !config.top_memory_access)
    }

    #[inline]
    fn s3_legacy_range_overlaps(&self, addr: usize, len: usize) -> bool {
        self.s3_legacy_aperture.active && range_overlaps(addr, len, VIDEO_BASE, VIDEO_SIZE)
    }

    #[inline]
    fn s3_linear_nibble_swap(&self) -> bool {
        matches!(self.lfb_decoder, LfbDecoderConfig::S3 { .. })
            && self.s3_legacy_aperture.linear_nibble_swap
    }

    #[inline]
    fn transform_s3_linear_byte(&self, value: u8) -> u8 {
        if self.s3_linear_nibble_swap() {
            value.rotate_left(4)
        } else {
            value
        }
    }

    #[inline]
    fn write_lfb_span(&mut self, addr: usize, bytes: &[u8], record_change: bool) -> bool {
        let Some(off) = self.lfb_span_offset(addr, bytes.len()) else {
            return false;
        };
        let inspect_change = record_change && vga_write_change_stats_enabled();
        let mut changed = false;
        for (index, byte) in bytes.iter().copied().enumerate() {
            let value = self.transform_s3_linear_byte(byte);
            changed |= inspect_change && self.lfb[off + index] != value;
            self.lfb[off + index] = value;
        }
        self.mark_lfb_dirty_range(off, bytes.len());
        self.increment_video_write_count();
        if record_change {
            self.record_video_write_change(changed);
        }
        true
    }

    /// Enable or disable the active linear-framebuffer personality without
    /// changing its base or size.
    pub fn set_lfb_enabled(&mut self, enabled: bool) {
        let config = match self.lfb_decoder {
            LfbDecoderConfig::Dispi { .. } => LfbDecoderConfig::Dispi { enabled },
            LfbDecoderConfig::S3 {
                base,
                size,
                vram_offset,
                ..
            } => LfbDecoderConfig::S3 {
                base,
                size,
                vram_offset,
                enabled,
            },
        };
        self.configure_lfb_decoder(config);
    }

    /// Check if LFB is enabled
    pub fn is_lfb_enabled(&self) -> bool {
        match self.lfb_decoder {
            LfbDecoderConfig::Dispi { enabled } | LfbDecoderConfig::S3 { enabled, .. } => enabled,
        }
    }

    /// Get direct access to the LFB buffer
    pub fn lfb(&self) -> &[u8] {
        &self.lfb
    }

    /// Get mutable access to the LFB buffer
    pub fn lfb_mut(&mut self) -> &mut [u8] {
        &mut self.lfb
    }

    /// Check if an address is in the LFB range
    #[inline]
    pub fn is_lfb_address(&self, addr: usize) -> bool {
        self.lfb_offset(addr).is_some()
    }

    /// Whether `addr` lies in the linear-framebuffer aperture this adapter
    /// advertises (VBE `PhysBasePtr`), whether or not its decoder is enabled
    /// yet.
    ///
    /// [`Memory::is_lfb_address`] answers "is this decoding right now"; a DPMI
    /// physical-mapping request asks "is this the video aperture", and a VBE
    /// client may legally map `PhysBasePtr` before it sets the mode that
    /// enables the decoder (MDD-BUG-CRU-01488).
    pub fn is_lfb_aperture_address(&self, addr: usize) -> bool {
        let within = |base: usize, len: usize| addr.checked_sub(base).is_some_and(|off| off < len);
        match self.lfb_decoder {
            LfbDecoderConfig::Dispi { .. } => {
                within(LFB_BASE, LFB_MAX_SIZE) || within(LFB_BOCHS_COMPAT_BASE, LFB_MAX_SIZE)
            }
            LfbDecoderConfig::S3 { base, size, .. } => within(base, size),
        }
    }

    /// Return an LFB offset through the active adapter personality.
    #[inline]
    fn lfb_offset(&self, addr: usize) -> Option<usize> {
        match self.lfb_decoder {
            LfbDecoderConfig::Dispi { enabled: false }
            | LfbDecoderConfig::S3 { enabled: false, .. } => None,
            LfbDecoderConfig::Dispi { enabled: true } => {
                Self::lfb_offset_from_base(addr, LFB_BASE, self.lfb.len()).or_else(|| {
                    Self::lfb_offset_from_base(addr, LFB_BOCHS_COMPAT_BASE, self.lfb.len())
                })
            }
            LfbDecoderConfig::S3 {
                base,
                size,
                vram_offset,
                enabled: true,
            } => Self::lfb_offset_from_base(addr, base, size)
                .filter(|_| !self.is_system_dram(addr))
                .and_then(|offset| {
                    (!self.lfb.is_empty()).then(|| (vram_offset + offset) % self.lfb.len())
                }),
        }
    }

    /// Whether `addr` is installed system DRAM, which the host bridge claims
    /// before any bus device can decode it.
    ///
    /// A relocatable device window can overlap installed RAM. DRAM cycles
    /// take priority, so the device window only decodes holes the chipset
    /// forwards to the bus: the legacy aperture and addresses above RAM.
    #[inline]
    fn is_system_dram(&self, addr: usize) -> bool {
        (addr < self.usable_len() && !(VIDEO_BASE..LEGACY_HOLE_END).contains(&addr))
            || (self.ram.has_arena_mapping()
                && (arena::ARENA_PHYS_START..arena::ARENA_PHYS_END).contains(&addr))
    }

    /// Return an LFB offset when the whole byte span fits in one aperture.
    #[inline]
    fn lfb_span_offset(&self, addr: usize, len: usize) -> Option<usize> {
        let off = self.lfb_offset(addr)?;
        if len == 0 {
            return Some(off);
        }
        let last_addr = addr.checked_add(len - 1)?;
        let last_off = off.checked_add(len - 1)?;
        (self.lfb_offset(last_addr) == Some(last_off)).then_some(off)
    }

    #[inline]
    fn lfb_range_overlaps(&self, addr: usize, len: usize) -> bool {
        match self.lfb_decoder {
            LfbDecoderConfig::Dispi { enabled: true } => {
                range_overlaps(addr, len, LFB_BASE, self.lfb.len())
                    || range_overlaps(addr, len, LFB_BOCHS_COMPAT_BASE, self.lfb.len())
            }
            LfbDecoderConfig::S3 {
                base,
                size,
                enabled: true,
                ..
            } => {
                // Only the part of the window outside system DRAM decodes
                // (`is_system_dram`): the legacy hole and above installed RAM.
                let start = addr.max(base);
                let end = addr.saturating_add(len).min(base.saturating_add(size));
                start < end
                    && (range_overlaps(
                        start,
                        end - start,
                        VIDEO_BASE,
                        LEGACY_HOLE_END - VIDEO_BASE,
                    ) || end > self.usable_len())
            }
            LfbDecoderConfig::Dispi { enabled: false }
            | LfbDecoderConfig::S3 { enabled: false, .. } => false,
        }
    }

    #[inline]
    fn lfb_offset_from_base(addr: usize, base: usize, len: usize) -> Option<usize> {
        let off = addr.checked_sub(base)?;
        (off < len).then_some(off)
    }

    /// Enable or disable the original S3 A0000h-AFFFFh MMIO decoder.
    ///
    /// The caller drains writes before changing this gate at the ordered device
    /// boundary. Clearing the decoder defensively discards any stale writes and
    /// restores open-bus readback for the next enable.
    pub fn configure_s3_mmio_decoder(&mut self, enabled: bool) {
        if self.s3_mmio_enabled == enabled {
            return;
        }
        self.s3_mmio_enabled = enabled;
        self.pending_s3_mmio_reads
            .get_mut()
            .expect("S3 MMIO read mailbox poisoned")
            .clear();
        self.pending_s3_mmio_read.store(false, Ordering::Relaxed);
        self.pending_s3_mmio_writes.clear();
        self.s3_mmio_attention.store(false, Ordering::Relaxed);
        self.s3_mmio_readback.fill(0xFF);
        self.bump_memory_policy_epoch();
        // See `configure_s3_legacy_aperture`: the native-commit admission
        // consults `s3_mmio_enabled`, so recompute at this toggle too.
        self.vga_native_commit = self.compute_vga_native_commit();
    }

    pub fn s3_mmio_decoder_enabled(&self) -> bool {
        self.s3_mmio_enabled
    }

    pub fn pending_s3_mmio_writes(&self) -> &[S3MmioWrite] {
        &self.pending_s3_mmio_writes
    }

    // The interpreter polls this once per guest instruction so an S3 MMIO
    // transaction cannot retire behind the device boundary. The fused
    // `s3_mmio_attention` flag collapses the poll to a single load; every
    // mutation of the read flag or write queue below maintains it.
    #[inline(always)]
    pub fn has_pending_s3_mmio_transactions(&self) -> bool {
        self.s3_mmio_attention.load(Ordering::Relaxed)
    }

    pub fn take_s3_mmio_reads(&self) -> Vec<S3MmioRead> {
        let mut reads = self
            .pending_s3_mmio_reads
            .lock()
            .expect("S3 MMIO read mailbox poisoned");
        let reads = std::mem::take(&mut *reads);
        self.pending_s3_mmio_read.store(false, Ordering::Relaxed);
        // Writes may still pend: attention stays up until both are drained.
        self.s3_mmio_attention
            .store(!self.pending_s3_mmio_writes.is_empty(), Ordering::Relaxed);
        reads
    }

    pub fn drain_s3_mmio_writes(&mut self) -> std::vec::Drain<'_, S3MmioWrite> {
        // The Drain empties the queue when consumed or dropped; reads may
        // still pend, so attention follows the read flag.
        self.s3_mmio_attention.store(
            self.pending_s3_mmio_read.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        self.pending_s3_mmio_writes.drain(..)
    }

    /// Publish one standard-MMIO 16-bit register value for the next CPU read.
    ///
    /// The packed 8100h-816Ah aliases are write-only and are never published.
    pub fn publish_s3_mmio_read_u16(&mut self, offset: u16, value: u16) {
        let index = usize::from(offset);
        let Some(bytes) = self.s3_mmio_readback.get_mut(index..index + 2) else {
            return;
        };
        bytes.copy_from_slice(&value.to_le_bytes());
    }

    #[inline]
    fn s3_mmio_span_offset(&self, addr: usize, width: S3MmioWidth) -> Option<u16> {
        if !self.s3_mmio_enabled {
            return None;
        }
        let offset = addr.checked_sub(S3_MMIO_BASE)?;
        let end = offset.checked_add(width.bytes())?;
        (end <= S3_MMIO_SIZE).then_some(offset as u16)
    }

    #[inline]
    fn s3_mmio_open_bus(width: S3MmioWidth) -> u32 {
        match width {
            S3MmioWidth::Byte => u32::from(u8::MAX),
            S3MmioWidth::Word => u32::from(u16::MAX),
            S3MmioWidth::Dword => u32::MAX,
        }
    }

    #[inline]
    fn s3_mmio_read(&self, addr: usize, width: S3MmioWidth) -> Option<u32> {
        let offset = usize::from(self.s3_mmio_span_offset(addr, width)?);
        let aligned = offset % width.bytes() == 0;
        let command_register_space = offset >= 0x8000;
        if !aligned
            || matches!(width, S3MmioWidth::Byte)
            || command_register_space && matches!(width, S3MmioWidth::Dword)
        {
            return Some(Self::s3_mmio_open_bus(width));
        }
        let mut bytes = [0xFF; 4];
        for (index, byte) in bytes[..width.bytes()].iter_mut().enumerate() {
            if let Some(readback) = offset
                .checked_add(index)
                .and_then(|offset| self.s3_mmio_readback.get(offset))
            {
                *byte = *readback;
            }
        }
        self.pending_s3_mmio_reads
            .lock()
            .expect("S3 MMIO read mailbox poisoned")
            .push(S3MmioRead {
                offset: offset as u16,
                width,
            });
        self.pending_s3_mmio_read.store(true, Ordering::Relaxed);
        self.s3_mmio_attention.store(true, Ordering::Relaxed);
        Some(u32::from_le_bytes(bytes))
    }

    #[inline]
    fn stage_s3_mmio_write(&mut self, addr: usize, width: S3MmioWidth, value: u32) -> bool {
        let Some(offset) = self.s3_mmio_span_offset(addr, width) else {
            return false;
        };
        if usize::from(offset) % width.bytes() != 0
            || usize::from(offset) >= 0x8000 && matches!(width, S3MmioWidth::Byte)
        {
            return true;
        }
        self.pending_s3_mmio_writes.push(S3MmioWrite {
            offset,
            width,
            value,
        });
        self.s3_mmio_attention.store(true, Ordering::Relaxed);
        true
    }

    #[inline]
    fn s3_mmio_range_overlaps(&self, addr: usize, len: usize) -> bool {
        self.s3_mmio_enabled && range_overlaps(addr, len, S3_MMIO_BASE, S3_MMIO_SIZE)
    }

    #[inline]
    fn mark_lfb_dirty_range(&mut self, offset: usize, len: usize) {
        if len == 0 || offset >= self.lfb.len() {
            return;
        }

        let end = offset.saturating_add(len).min(self.lfb.len());
        if end == 0 {
            return;
        }

        let first_page = offset >> LFB_DIRTY_PAGE_SHIFT;
        let last_page = (end - 1) >> LFB_DIRTY_PAGE_SHIFT;
        for page in first_page..=last_page {
            if page < LFB_DIRTY_PAGE_COUNT {
                self.lfb_dirty_pages[page / 64] |= 1u64 << (page % 64);
            }
        }
    }

    /// Get and clear canonical LFB dirty pages. Each bit represents a 4KB
    /// page in the VBE linear framebuffer backing store.
    #[inline]
    pub fn take_lfb_dirty_pages(&mut self) -> [u64; LFB_DIRTY_WORDS] {
        let pages = self.lfb_dirty_pages;
        self.lfb_dirty_pages = [0; LFB_DIRTY_WORDS];
        pages
    }

    /// Check if any canonical LFB pages are dirty.
    #[inline]
    pub fn has_dirty_lfb_pages(&self) -> bool {
        self.lfb_dirty_pages.iter().any(|&word| word != 0)
    }

    /// Read a byte from the LFB
    #[inline]
    pub fn lfb_read_u8(&self, offset: usize) -> u8 {
        if offset < self.lfb.len() {
            self.lfb[offset]
        } else {
            0
        }
    }

    /// Write a byte to the LFB
    #[inline]
    pub fn lfb_write_u8(&mut self, offset: usize, value: u8) {
        if offset < self.lfb.len() {
            self.lfb[offset] = value;
            self.mark_lfb_dirty_range(offset, 1);
            self.increment_video_write_count();
        }
    }

    /// Load data into raw RAM at the specified address.
    ///
    /// This is for host-side initialization/loading only. Runtime guest writes
    /// must use `write_bytes_guest` so mapped/device memory semantics are
    /// honored. Once the HLE EMS frame is owned, raw loads into that reserved
    /// frame are rejected because they would bypass the canonical EMS backing
    /// pool and corrupt only the shadow frame RAM.
    pub fn load(&mut self, addr: usize, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        let addr = self.mask_address(addr);
        let end = addr
            .checked_add(data.len())
            .ok_or(MemoryError::OutOfBounds(usize::MAX))?;
        if end > self.usable_len() {
            return Err(MemoryError::OutOfBounds(end));
        }
        if self.hle_ems_frame_owned
            && range_overlaps(addr, data.len(), EMS_FRAME_BASE, EMS_FRAME_SIZE)
        {
            return Err(MemoryError::RawLoadOverlapsHleEmsFrame {
                addr,
                len: data.len(),
            });
        }
        self.smc_before_write(addr, data.len());
        self.mark_instr_cache_dirty_range(addr, data.len());

        self.ram[addr..end].copy_from_slice(data);
        self.maybe_trace_phys_write_range(addr, data.len(), 0);

        Ok(())
    }

    /// Fill a half-open RAM range `[start, end)` with `byte`.
    ///
    /// Designed for load-time use (e.g. the DOS BSS-init pass that fills
    /// free conventional memory before a program runs). Bypasses the
    /// per-byte `write_u8` path because no selector diagnostic is armed
    /// for load-time fill and the high-memory alias is irrelevant in
    /// the conventional-RAM ranges this is called against.
    ///
    /// Behaviour:
    /// - Clamps `end` to the usable RAM length (silently truncates).
    /// - No-op when `start >= end` (after clamping) or when `start` is
    ///   already past the RAM end.
    /// - Silently skips 4-KiB pages that have been registered as ROM
    ///   via [`Memory::mark_rom_page`] / [`Memory::mark_rom_range`].
    ///   Bytes on non-ROM pages within the range are still filled.
    /// - Notifies the SMC write snoop once for the whole range (RC-1:
    ///   compiled code may exist by the time a second program loads, so
    ///   a fill overlapping compiled source must invalidate it like any
    ///   other write channel; one range-level call, free when no
    ///   notifier is bound).
    ///
    /// Cost: one contiguous `[u8]::fill` when no ROM regions are
    /// registered (the common DOS / HLE configuration); one per
    /// non-ROM page otherwise.
    pub fn fill_ram(&mut self, start: usize, end: usize, byte: u8) {
        // Bounded by guest RAM, as `load` is: the FETCH_PADDING tail past it
        // is host fetch slack, not memory a guest fill may reach
        // (MDD-BUG-CRU-01485).
        let end = end.min(self.usable_len());
        if start >= end {
            return;
        }
        self.smc_before_write(start, end - start);
        let needs_page_walk = self.has_rom_regions
            || (self.hle_ems_frame_owned
                && range_overlaps(start, end - start, EMS_FRAME_BASE, EMS_FRAME_SIZE));
        if needs_page_walk {
            // Skip any 4-KiB page registered as ROM. Aligning the first
            // and last pages keeps the fill bounded by the requested
            // range while letting full middle pages take the fast path.
            let mut cursor = start;
            while cursor < end {
                let page_end = (cursor | (ROM_PAGE_SIZE - 1)) + 1;
                let chunk_end = page_end.min(end);
                if !self.is_rom_addr(cursor) {
                    let chunk_len = chunk_end - cursor;
                    if self.hle_ems_frame_owned
                        && range_overlaps(cursor, chunk_len, EMS_FRAME_BASE, EMS_FRAME_SIZE)
                    {
                        for addr in cursor..chunk_end {
                            self.write_u8(addr, byte);
                        }
                    } else {
                        self.mark_instr_cache_dirty_range(cursor, chunk_len);
                        self.ram[cursor..chunk_end].fill(byte);
                    }
                }
                cursor = chunk_end;
            }
        } else {
            self.mark_instr_cache_dirty_range(start, end - start);
            self.ram[start..end].fill(byte);
        }
    }

    /// Get current usable RAM size (excludes fetch-safety padding).
    pub fn ram_size(&self) -> usize {
        self.usable_len()
    }

    /// Return the reserved RAM backing capacity, including fetch padding.
    ///
    /// Callers can inspect the allocation ceiling but cannot move the backing
    /// storage; all size changes must go through [`Self::resize_ram`].
    pub fn ram_capacity(&self) -> usize {
        self.ram.capacity()
    }

    /// Map the session section at its fixed physical offset. This grants no
    /// Rust access until the unique mounted admission token is given.
    pub fn attach_arena_store(
        &mut self,
        store: std::sync::Arc<arena::ArenaStore>,
    ) -> std::io::Result<()> {
        self.invalidate_all_direct_proofs();
        self.flush_tlb_full();
        self.ram.attach_arena(store)?;
        self.core
            .ensure_page_generation_coverage(arena::ARENA_PHYS_END);
        self.core.ensure_watch_pages(arena::ARENA_PHYS_END >> 12);
        let arena_words = (arena::ARENA_PHYS_END / INSTR_EXEC_PAGE_SIZE).div_ceil(64);
        if self.core.instr_exec_pages().len() < arena_words {
            self.core.resize_instr_exec_pages(arena_words);
        }
        Ok(())
    }

    /// Revoke the mounted process's right to form arena references. The
    /// mutable borrow makes prior `mem.ram[..]` references end first.
    ///
    /// Every host pointer this `Memory` published into the arena (software
    /// TLB entries, which also back the flat views, and direct page proofs)
    /// is retired first. When nothing arena-backed was published since the
    /// last take, the TLB and proofs stay as they are.
    pub fn take_arena_admission(&mut self) -> Option<arena::ArenaAdmission> {
        if !self.ram.has_arena_admission() {
            return None;
        }
        if self.arena_published.replace(false) {
            self.invalidate_all_direct_proofs();
            self.invalidate_cached_arena_translations();
        }
        self.ram.take_admission()
    }

    /// Admit this process after its section view is attached. While it was
    /// unadmitted nothing could publish an arena pointer (every publication
    /// checks `is_backed`), so no TLB entry needs retiring.
    pub fn give_arena_admission(&mut self, token: arena::ArenaAdmission) {
        self.ram.give_admission(token);
    }

    /// Commit a host arena range before a guest PTE points to it.
    pub fn commit_arena_host_range(
        &mut self,
        range: std::ops::Range<usize>,
    ) -> std::io::Result<()> {
        self.ram.commit_arena(range)
    }

    /// Remove the view after retiring all process-local raw publications.
    pub fn detach_arena_store(&mut self) -> Option<arena::ArenaAdmission> {
        self.invalidate_all_direct_proofs();
        self.flush_tlb_full();
        self.ram.detach_arena()
    }

    /// Resize usable RAM while preserving the fetch-safety padding.
    ///
    /// Direct page proofs and cached host pointers are retired before the
    /// backing length or allocation can change. This is the sole structural
    /// mutation path for RAM outside the memory crate.
    pub fn resize_ram(&mut self, usable_size: usize) {
        if usable_size == self.usable_len() {
            return;
        }

        // Only the private RAM extent grows here. Arena pages live in the
        // separately mapped section and keep their own commitment record.
        // Every private growth path funnels through this architectural cap.
        assert!(
            usable_size <= MAX_RAM_SIZE,
            "RAM growth to {usable_size:#x} exceeds MAX_RAM_SIZE ({MAX_RAM_SIZE:#x})"
        );

        let new_len = usable_size
            .checked_add(FETCH_PADDING)
            .expect("RAM size plus fetch padding overflowed usize");

        // LLD E4: the direct-read limit is delivered before the epoch bumps
        // below announce the resize. A shrink lowers it here, so nothing that
        // observes the new epochs can still read past the new end; growth
        // raises it only in `resize_ram_backing`, once the backing exists.
        self.core.lower_raw_ram_limit(new_len);
        // Retire every published pointer before Vec::resize may move or shorten
        // the allocation. Keep the TLB flush explicit: direct-proof invalidation
        // stales dynrec guards but does not clear cached host pointers.
        self.invalidate_all_direct_proofs();
        self.flush_tlb_full();
        self.resize_ram_backing(new_len);

        let covered = if self.ram.has_arena_mapping() {
            arena::ARENA_PHYS_END
        } else {
            usable_size
        };
        let pages = covered.div_ceil(INSTR_EXEC_PAGE_SIZE);
        let words = pages.div_ceil(64);
        let arena_words = if self.ram.has_arena_mapping() {
            arena::ARENA_PHYS_END
                .div_ceil(INSTR_EXEC_PAGE_SIZE)
                .div_ceil(64)
        } else {
            0
        };
        self.core.resize_instr_exec_pages(words.max(arena_words));
    }

    /// Ensure RAM is at least the specified size (for DOS memory allocation).
    /// Always preserves FETCH_PADDING bytes past the usable size.
    ///
    /// Capped at [`DOS_RAM_CEILING`], whose backing capacity is reserved by
    /// [`Memory::new`] — growth through this path never
    /// reallocates the backing buffer. Only [`Memory::expand_ram`]
    /// grows past the DOS ceiling (up to [`MAX_RAM_SIZE`]).
    pub fn ensure_ram_size(&mut self, min_size: usize) {
        if self.usable_len() < min_size {
            let target = min_size.min(DOS_RAM_CEILING);
            if self.usable_len() < target {
                self.resize_ram(target);
            }
        }
    }

    // -- Physical decode map --

    /// Every physical window a device or bus alias claims before plain RAM, as
    /// `read_u8`/`write_u8` decode them now. The A20 gate is not a window (it
    /// folds bit 20 of every address) and is reported by
    /// [`Memory::first_decoder_overlapping`] alone. The S3 linear window is
    /// listed where it is programmed, although installed DRAM wins over it.
    pub fn physical_decoders(&self) -> Vec<(PhysicalDecoder, Range<usize>)> {
        let mut windows = Vec::new();
        let mut push = |decoder, start: usize, len: usize| {
            if len != 0 {
                windows.push((decoder, start..start.saturating_add(len)));
            }
        };
        if self.high_mem_alias_enabled {
            push(
                PhysicalDecoder::HighMemAlias,
                HIGH_MEM_ALIAS_BASE,
                HIGH_MEM_ALIAS_END - HIGH_MEM_ALIAS_BASE + 1,
            );
        }
        if self.firmware_mp_table_suppression_enabled {
            push(PhysicalDecoder::FirmwareMpFilter, 0, LOW_MEMORY_LIMIT);
        }
        if self.hle_ems_frame_owned {
            push(PhysicalDecoder::EmsFrame, EMS_FRAME_BASE, EMS_FRAME_SIZE);
        }

        if self.option_rom_enabled {
            push(
                PhysicalDecoder::OptionRom,
                self.option_rom_base,
                self.option_rom_backing.len(),
            );
        }
        for (page, _) in self.rom_pages.iter().enumerate().filter(|(_, rom)| **rom) {
            push(
                PhysicalDecoder::RomPage,
                page * ROM_PAGE_SIZE,
                ROM_PAGE_SIZE,
            );
        }
        if self.s3_mmio_enabled {
            push(PhysicalDecoder::S3Mmio, S3_MMIO_BASE, S3_MMIO_SIZE);
        }
        push(PhysicalDecoder::VgaWindow, VIDEO_BASE, VIDEO_SIZE);
        match self.lfb_decoder {
            LfbDecoderConfig::Dispi { enabled: true } => {
                push(PhysicalDecoder::LinearFramebuffer, LFB_BASE, self.lfb.len());
                push(
                    PhysicalDecoder::LinearFramebuffer,
                    LFB_BOCHS_COMPAT_BASE,
                    self.lfb.len(),
                );
            }
            LfbDecoderConfig::S3 {
                base,
                size,
                enabled: true,
                ..
            } => push(PhysicalDecoder::LinearFramebuffer, base, size),
            _ => {}
        }
        for region in &self.mmio_regions {
            push(
                PhysicalDecoder::MmioRegion,
                region.phys_start,
                region.data.len(),
            );
        }
        windows
    }

    /// The first decoder that claims any byte of `range` instead of plain RAM,
    /// with the window it claims. `None` means every byte of `range` decodes
    /// as RAM (or as absent RAM above the backing).
    pub fn first_decoder_overlapping(
        &self,
        range: Range<usize>,
    ) -> Option<(PhysicalDecoder, Range<usize>)> {
        if range.is_empty() {
            return None;
        }
        if !self.a20_enabled {
            // The first address at or above `range.start` with bit 20 set.
            let bit20 = 0x0010_0000;
            let first = if range.start & bit20 != 0 {
                range.start
            } else {
                (range.start | (bit20 - 1)) + 1
            };
            if first < range.end {
                return Some((PhysicalDecoder::A20Wrap, first..first + 1));
            }
        }
        self.physical_decoders()
            .into_iter()
            .find(|(_, window)| window.start < range.end && range.start < window.end)
    }

    // -- MMIO region management --

    /// Registers an MMIO region at the given physical address.
    ///
    /// PLACEMENT CONTRACT: put the region at or above `MAX_RAM_SIZE`. Write
    /// dispatch is gated on that threshold (in `write_u8`, and in the wide
    /// writers' decomposition arms), so a region registered below it is
    /// shadowed by RAM on the write side and never sees a guest store.
    pub fn add_mmio_region(&mut self, phys_start: usize, size: usize) {
        self.mmio_regions.retain(|r| r.phys_start != phys_start);
        self.mmio_regions.push(MmioRegion {
            phys_start,
            data: vec![0u8; size],
            byte_swizzle: 0,
        });
        self.bump_memory_policy_epoch();
    }

    /// Set the CPU-access byte swizzle of the MMIO region at `phys_start`
    /// (see [`MmioRegion::byte_swizzle`]). Returns `false` if no region
    /// starts there.
    pub fn set_mmio_byte_swizzle(&mut self, phys_start: usize, swizzle: u8) -> bool {
        match self
            .mmio_regions
            .iter_mut()
            .find(|r| r.phys_start == phys_start)
        {
            Some(region) => {
                region.byte_swizzle = swizzle & 0x03;
                true
            }
            None => false,
        }
    }

    /// Check if an MMIO region exists at the given physical address.
    pub fn has_mmio_region(&self, phys_start: usize) -> bool {
        self.mmio_regions.iter().any(|r| r.phys_start == phys_start)
    }

    /// Removes an MMIO region by its physical start address.
    pub fn remove_mmio_region(&mut self, phys_start: usize) {
        let before = self.mmio_regions.len();
        self.mmio_regions.retain(|r| r.phys_start != phys_start);
        if self.mmio_regions.len() != before {
            self.bump_memory_policy_epoch();
        }
    }

    /// Returns true if the given physical address falls within a registered MMIO region.
    pub fn is_mmio_address(&self, phys_addr: usize) -> bool {
        self.mmio_regions
            .iter()
            .any(|r| phys_addr >= r.phys_start && phys_addr < r.phys_start + r.data.len())
    }

    /// Find the MMIO region containing `phys_addr`, returning `(phys_start, len)`.
    pub fn find_mmio_region_containing(&self, phys_addr: usize) -> Option<(usize, usize)> {
        self.mmio_regions.iter().find_map(|r| {
            if phys_addr >= r.phys_start && phys_addr < r.phys_start + r.data.len() {
                Some((r.phys_start, r.data.len()))
            } else {
                None
            }
        })
    }

    /// Returns the first registered MMIO region's `(phys_start, len)`, if any.
    pub fn first_mmio_region(&self) -> Option<(usize, usize)> {
        self.mmio_regions
            .first()
            .map(|r| (r.phys_start, r.data.len()))
    }

    /// Returns the total byte size of all registered MMIO regions.
    /// Used by the demand-page gate to size the linear window when more
    /// than one MMIO region has been registered.
    pub fn total_mmio_size(&self) -> usize {
        self.mmio_regions.iter().map(|r| r.data.len()).sum()
    }

    /// Returns a mutable slice to an MMIO region's data buffer.
    pub fn mmio_slice_mut(&mut self, phys_start: usize) -> Option<&mut [u8]> {
        self.mmio_regions
            .iter_mut()
            .find(|r| r.phys_start == phys_start)
            .map(|r| r.data.as_mut_slice())
    }

    /// Guest RAM alongside a mutable MMIO region buffer — disjoint borrows, for
    /// device DMA that reads RAM while writing its own aperture.
    pub fn ram_and_mmio_slice_mut(
        &mut self,
        phys_start: usize,
    ) -> (&RamBacking, Option<&mut [u8]>) {
        let region = self
            .mmio_regions
            .iter_mut()
            .find(|r| r.phys_start == phys_start)
            .map(|r| r.data.as_mut_slice());
        (&self.ram, region)
    }

    /// Returns an immutable slice to an MMIO region's data buffer.
    pub fn mmio_slice(&self, phys_start: usize) -> Option<&[u8]> {
        self.mmio_regions
            .iter()
            .find(|r| r.phys_start == phys_start)
            .map(|r| r.data.as_slice())
    }

    /// Returns true if any byte of the half-open range `[addr, addr+len)`
    /// falls in a registered MMIO region. `addr` must already be A20-masked.
    ///
    /// Multi-byte writers use this to decompose into `write_u8`, which owns
    /// the only MMIO dispatch. Callers gate on `>= MAX_RAM_SIZE` first so the
    /// `mmio_regions` heap allocation is never touched on a RAM or VGA write.
    #[inline]
    fn mmio_range_overlaps(&self, addr: usize, len: usize) -> bool {
        self.mmio_regions
            .iter()
            .any(|region| range_overlaps(addr, len, region.phys_start, region.data.len()))
    }

    /// Read a byte from an MMIO region. Returns None if no region matches.
    fn mmio_read_u8(&self, addr: usize) -> Option<u8> {
        let result = self.mmio_regions.iter().find_map(|region| {
            let offset = addr.wrapping_sub(region.phys_start);
            if offset < region.data.len() {
                let offset = (offset ^ usize::from(region.byte_swizzle)).min(region.data.len() - 1);
                Some(region.data[offset])
            } else {
                None
            }
        });
        result
    }

    /// Write a byte to an MMIO region. Returns true if a region matched.
    fn mmio_write_u8(&mut self, addr: usize, value: u8) -> bool {
        for region in &mut self.mmio_regions {
            let offset = addr.wrapping_sub(region.phys_start);
            if offset < region.data.len() {
                let offset = (offset ^ usize::from(region.byte_swizzle)).min(region.data.len() - 1);
                region.data[offset] = value;
                return true;
            }
        }
        false
    }

    /// Get a copy of memory (for debugging/DMA). Applies A20 wrapping.
    pub fn slice(&self, addr: usize, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        self.read_bytes(addr, &mut buf);
        buf
    }

    /// Get a writable view of memory (for debugging/DMA).
    pub fn slice_mut(&mut self, addr: usize, len: usize) -> MemorySliceMut<'_> {
        if len == 0 {
            return MemorySliceMut::owned(self, addr, Vec::new());
        }

        let addr = self.mask_address(addr);
        if self.hle_ems_frame_owned && range_overlaps(addr, len, EMS_FRAME_BASE, EMS_FRAME_SIZE) {
            let mut buf = vec![0u8; len];
            self.read_bytes(addr, &mut buf);
            return MemorySliceMut::owned(self, addr, buf);
        }
        // Clamp BOTH endpoints. Reducing only the length left the original
        // out-of-range start in place, and `deref` then indexed
        // `ram[addr..addr]` — Rust rejects even an empty range whose start is
        // past the slice, so this safe API panicked as soon as the caller
        // looked at the view (MDD-BUG-CRUCIBLE-00508). An above-RAM start owns
        // no bytes at all, so hand back an owned empty view, which keeps `addr`
        // truthful and drops without writing anything.
        let usable = self.usable_len();
        if addr >= usable {
            // A wholly backed shared-arena span (HLD §5.2) is a direct view;
            // anything else above RAM owns no bytes.
            if addr >= arena::ARENA_PHYS_START
                && addr
                    .checked_add(len)
                    .is_some_and(|end| self.ram.is_backed(addr..end))
            {
                return MemorySliceMut::borrowed(self, addr, len);
            }
            return MemorySliceMut::owned(self, addr, Vec::new());
        }
        // saturating_add: `addr` is guest-supplied and unmasked here, so a
        // length near usize::MAX must not overflow the range computation.
        let end = addr.saturating_add(len).min(usable);
        MemorySliceMut::borrowed(self, addr, end - addr)
    }

    /// Could the physical write `[addr, addr + size)` land on a page the
    /// dyncore holds compiled code on?
    ///
    /// `false` is a PROOF that the write is invisible to every SMC consumer:
    /// both dyncore engines short-circuit a drained dirty span whose page has
    /// no cached block (`DbxCore::invalidate_dirty_spans`' `code_pages.get()`
    /// miss; `DynCore::invalidate_overlapping_channel`'s `cache_pages.get()`
    /// miss), so recording it can only cost time — and, past
    /// `SNOOP_PAGE_CAP` distinct pages, provoke a whole-cache flush that
    /// nothing asked for.
    ///
    /// Sound because the watch bit leads the cache in the dangerous direction:
    /// a page is watched BEFORE any block on it can execute (DBX sets it inside
    /// `install`, before the resolved block is returned; the legacy engine
    /// RUN-PINs the page before entering a freshly built block, which is
    /// entered before `cache_insert`), and is un-watched only through the
    /// deferred reconcile queue, i.e. LATE. Over-watching costs a notification;
    /// under-watching is what would be unsound, and no path produces it.
    #[inline]
    #[must_use]
    pub fn smc_span_may_hold_code(&self, addr: usize, size: usize) -> bool {
        self.core.smc_span_may_hold_code(addr, size)
    }

    #[inline]
    pub fn smc_before_write(&mut self, addr: usize, size: usize) -> bool {
        // One fast guard covers local JIT pages and remote arena interest
        // (HLD §5.4 piece 3). A process with neither still pays one boolean
        // test per store.
        if !self.smc_interest {
            return false;
        }
        let recorded_remote = self.remote_arena_watch && self.log_remote_arena_write(addr, size);
        self.core.smc_before_write(addr, size) || recorded_remote
    }

    /// A store to an arena page another process translated reaches the
    /// session arena write log (HLD §5.4 piece 4, I11), whether or not this
    /// process has code pages of its own.
    #[cold]
    #[inline(never)]
    fn log_remote_arena_write(&mut self, addr: usize, size: usize) -> bool {
        let start = addr.max(arena::ARENA_PHYS_START);
        let end = addr.saturating_add(size).min(arena::ARENA_PHYS_END);
        if start >= end
            || self
                .arena
                .remote_watches
                .range((start >> 12) as u32..=((end - 1) >> 12) as u32)
                .next()
                .is_none()
        {
            return false;
        }
        let Some((_, state)) = &self.arena.smc else {
            return false;
        };
        state
            .lock()
            .expect("arena SMC lock poisoned")
            .append(start as u32..end as u32);
        true
    }

    /// Force non-inline to prevent stack overflow in release mode JIT callbacks.
    #[inline(never)]
    fn prepare_write_addr(&mut self, addr: usize) -> Option<usize> {
        let addr = self.mask_address(addr);

        // Expand RAM if needed (up to the DOS-era auto-grow ceiling, NOT
        // MAX_RAM_SIZE — the 256..768 MiB band is committed only explicitly by
        // expand_ram, so a stray physical write there stays dropped
        // exactly as it was before the bound was raised), preserving fetch
        // padding. After resize the base pointer changes, invalidating TLB
        // host pointers.
        if addr >= self.usable_len() {
            if addr < DOS_RAM_CEILING {
                let target = (addr + 1).div_ceil(RAM_GROWTH_GRANULARITY) * RAM_GROWTH_GRANULARITY;
                self.resize_ram(target.min(DOS_RAM_CEILING));
            } else if (arena::ARENA_PHYS_START..arena::ARENA_PHYS_END).contains(&addr) {
                // Shared arena (HLD §5.2): a store lands only on a committed
                // page this process is admitted to; otherwise it is dropped.
                return self.ram.is_backed(addr..addr + 1).then_some(addr);
            } else {
                // Address out of range, silently ignore
                return None;
            }
        }

        // Don't allow writes to ROM area (0xF0000-0xFFFFF)
        // Allow writes below ROM or in HMA (above 0xFFFFF)
        if !self.allow_rom_writes && (BIOS_BASE..=0xFFFFF).contains(&addr) {
            return None;
        }

        Some(addr)
    }

    #[inline]
    fn write_u8_inner(&mut self, addr: usize, value: u8) {
        if self.stage_s3_mmio_write(addr, S3MmioWidth::Byte, u32::from(value)) {
            return;
        }
        // RomRegions gate: silently drop writes to pages registered as ROM.
        // Defence-in-depth — callers that reach this helper via multi-byte
        // write paths may have bypassed the outer drop-whole-write guard
        // (e.g., if only a subset of bytes overlapped ROM in some future
        // refactor). Single-branch cost when no ROM pages are registered.
        if self.is_rom_addr(addr) {
            return;
        }
        self.mark_instr_cache_dirty_range(addr, 1);
        // Intercept writes to the selected VGA map window for planar/chain-4 modes.
        let vga_offset = self.vga_window_offset(addr);
        let is_video = vga_offset.is_some();
        let is_text_video = (TEXT_VIDEO_BASE_MONO..TEXT_VIDEO_BASE_MONO + TEXT_VIDEO_SIZE)
            .contains(&addr)
            || (TEXT_VIDEO_BASE_COLOR..TEXT_VIDEO_BASE_COLOR + TEXT_VIDEO_SIZE).contains(&addr);

        // Track video writes for watchdog: count ANY write to A0000-BFFFF or text video.
        // Use the full VGA address range (not the current window) because Memory's
        // vga_memory_map_select may lag behind VGA hardware during HLE BIOS mode sets.
        let is_any_video = (VIDEO_BASE..VIDEO_BASE + VIDEO_SIZE).contains(&addr);

        if is_any_video || is_text_video {
            self.increment_video_write_count();
            if is_any_video {
                self.vga_bus_write_count = self.vga_bus_write_count.wrapping_add(1);
            }
            vga_trace_record(addr, value);
        }
        let collect_video_change =
            (is_any_video || is_text_video) && vga_write_change_stats_enabled();

        // Mark video page dirty for SVGA sync optimization
        if is_video {
            self.mark_video_page_dirty(addr);
        }

        // Sequential CPU writes use planes even while display mode stays text.
        // Keep the cached display handler unchanged to avoid buffer syncs when
        // software temporarily maps font memory and then restores text addressing.
        if is_video {
            if self.vga_write_handler == VgaWriteHandler::Linear
                && self.is_cpu_sequential_planar_write()
            {
                if let Some(offset) = vga_offset {
                    if offset < PLANE_SIZE {
                        let changed = self.write_planar_ega(offset, value);
                        if collect_video_change {
                            self.record_video_write_change(changed);
                        }
                        return;
                    }
                }
            }

            // Dispatch VGA-range writes via pre-computed handler (avoids per-byte mode checks).
            match self.vga_write_handler {
                VgaWriteHandler::Chain4 => {
                    if let Some(offset) = vga_offset {
                        let plane = offset & 3;
                        let plane_offset = offset & !3;
                        if plane_offset < PLANE_SIZE {
                            // Chain-4 selects the plane via A1:A0 and ignores SR[2]
                            // (Map Mask). The address-selected plane overrides the mask.
                            let mask = 1u8 << plane;
                            let plane_changed =
                                self.write_planar_ega_masked(plane_offset, value, mask);
                            let idx = plane * PLANE_SIZE + plane_offset;
                            let result = self.planes_flat[idx];
                            let ram_changed =
                                collect_video_change && self.ram.get(addr).copied() != Some(result);
                            self.ram[addr] = result;
                            if collect_video_change {
                                self.record_video_write_change(plane_changed || ram_changed);
                            }
                            return;
                        }
                    }
                }
                VgaWriteHandler::Planar => {
                    if let Some(offset) = vga_offset {
                        if offset < PLANE_SIZE {
                            let changed = self.write_planar_ega(offset, value);
                            if collect_video_change {
                                self.record_video_write_change(changed);
                            }
                            return;
                        }
                    }
                }
                VgaWriteHandler::Linear => {}
            }
        }

        {
            let old_value = if collect_video_change {
                self.ram.get(addr).copied().unwrap_or(0)
            } else {
                0
            };
            self.ram[addr] = value;
            if collect_video_change {
                self.record_video_write_change(old_value != value);
            }

            self.maybe_trace_phys_write_range(addr, 1, value as u32);

            // IVT write watchpoint: detect corruption of interrupt vectors
            // to VGA-range addresses (likely bug indicator)
            if addr < 0x400 {
                let entry_base = addr & !3; // Align to 4-byte IVT entry boundary
                if entry_base + 3 < self.ram.len() {
                    let segment =
                        u16::from_le_bytes([self.ram[entry_base + 2], self.ram[entry_base + 3]]);
                    if segment >= 0xA000 && segment < 0xC000 {
                        if !std::mem::replace(&mut self.ivt_corruption_logged, true) {
                            let vec_num = entry_base / 4;
                            let offset = u16::from_le_bytes([
                                self.ram[entry_base],
                                self.ram[entry_base + 1],
                            ]);
                            let (cs, ip) = gp64_types::debug_trace::get_last_instruction();
                            tracing::error!(
                                target: "gp64::memory",
                                "IVT corruption: vector {:02X} -> {:04X}:{:04X}, writer CS:IP={:04X}:{:08X}, addr={:05X} value={:02X}",
                                vec_num, segment, offset, cs, ip, addr, value
                            );
                        }
                    }
                }
            }
        }
    }

    /// Expand RAM for flat-memory mode.
    ///
    /// Grows the backing `ram` buffer to `new_size` (plus fetch padding),
    /// rebuilds the instruction-exec page bitmap, and flushes the TLB
    /// since all cached host pointers are now stale.
    pub fn expand_ram(&mut self, new_size: usize) {
        // Idempotent: if usable RAM is already at least new_size, nothing to do.
        // Compare against usable_len() (excludes FETCH_PADDING), not ram.len().
        if new_size <= self.usable_len() {
            return;
        }

        self.resize_ram(new_size);
    }

    /// Calculate a hash of the entire machine-visible memory state.
    /// Includes main RAM and VGA planes.
    pub fn hash_state(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        for extent in self.ram.backed_extents() {
            hasher.update(&self.ram[extent]);
        }
        hasher.update(self.vga_planes());
        // Include the FULL VGA pipeline state that affects how VRAM is viewed or
        // written, plus the derived cached write handler. This makes an A/B
        // parity diff (interpreter vs dynrec) catch any predicate-field
        // divergence even when the ram/planes_flat bytes and the final frame CRC
        // happen to coincide (VGA chain4 native-store fast-path HLD §4.4), and
        // strengthens checkpoint/time-travel parity generally. Field order
        // follows `VgaPipelineState` for auditability.
        hasher.update(&[
            self.vga.video_mode,
            self.vga.chain4 as u8,
            self.vga.seq_odd_even_disabled as u8,
            self.vga.gc_odd_even_read as u8,
            self.vga.is_mode_x as u8,
            self.vga.plane_mask,
            self.vga.read_plane,
            self.vga.memory_map_select,
            self.vga.gc_graphics_mode as u8,
            self.vga.write_mode,
            self.vga.bit_mask,
            self.vga.set_reset,
            self.vga.enable_set_reset,
            self.vga.data_rotate,
            self.vga.read_mode,
            self.vga.color_compare,
            self.vga.color_dont_care,
            self.vga_write_handler as u8,
        ]);
        *hasher.finalize().as_bytes()
    }

    /// VGA-only parity hash for the dyncore A/B store-lever gate: VRAM + VGA-state
    /// ONLY, excluding general guest RAM. General RAM legitimately diverges between
    /// the interpreter and the dynrec at equal timer boundaries (PIT IRQ0 timing
    /// from dyncore block overshoot), which made the all-RAM `hash_state()` a
    /// false-FAIL machine for the A/B gate. This hash covers exactly what a VGA
    /// store-lever bug could corrupt: the de-interleaved planes, the CPU-visible
    /// video window, and every VGA-pipeline field the write handler dispatches on.
    pub fn hash_vga_parity(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.vga_planes());
        // The 128 KB CPU-visible VGA aperture: A000 (graphics 64K) + B000/B800
        // (text 64K). Guard the slice against ram length. This is the linear /
        // text scanout view; planes_flat above is the Mode-X/planar view.
        let vwin_start = VIDEO_BASE; // 0xA0000
        let vwin_end = (VIDEO_BASE + VIDEO_SIZE).min(self.ram.len()); // 0xC0000
        if vwin_start < self.ram.len() {
            hasher.update(&self.ram[vwin_start..vwin_end]);
        }
        // SAME VGA-state block hash_state appends (field order identical).
        hasher.update(&[
            self.vga.video_mode,
            self.vga.chain4 as u8,
            self.vga.seq_odd_even_disabled as u8,
            self.vga.gc_odd_even_read as u8,
            self.vga.is_mode_x as u8,
            self.vga.plane_mask,
            self.vga.read_plane,
            self.vga.memory_map_select,
            self.vga.gc_graphics_mode as u8,
            self.vga.write_mode,
            self.vga.bit_mask,
            self.vga.set_reset,
            self.vga.enable_set_reset,
            self.vga.data_rotate,
            self.vga.read_mode,
            self.vga.color_compare,
            self.vga.color_dont_care,
            self.vga_write_handler as u8,
        ]);
        *hasher.finalize().as_bytes()
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        if self.core.tlb_stats().is_enabled() {
            eprintln!("{}", self.core.tlb_stats().summary());
        }
    }
}

// =============================================================================
// MemoryAccess trait implementation
// =============================================================================

impl MemoryAccess for Memory {
    #[inline]
    fn read_u8(&self, addr: usize) -> u8 {
        Memory::read_u8(self, addr)
    }

    #[inline]
    fn read_u16(&self, addr: usize) -> u16 {
        Memory::read_u16(self, addr)
    }

    #[inline]
    fn read_u32(&self, addr: usize) -> u32 {
        Memory::read_u32(self, addr)
    }

    #[inline]
    fn write_u8(&mut self, addr: usize, value: u8) {
        Memory::write_u8(self, addr, value);
    }

    #[inline]
    fn write_u16(&mut self, addr: usize, value: u16) {
        Memory::write_u16(self, addr, value);
    }

    #[inline]
    fn write_u32(&mut self, addr: usize, value: u32) {
        Memory::write_u32(self, addr, value);
    }

    #[inline]
    fn read_u64(&self, addr: usize) -> u64 {
        Memory::read_u64(self, addr)
    }

    #[inline]
    fn write_u64(&mut self, addr: usize, value: u64) {
        Memory::write_u64(self, addr, value);
    }

    #[inline]
    fn read_run(&self, addr: usize, dest: &mut [u8]) {
        Memory::read_guest_run(self, addr, dest);
    }

    /// The write-side peer of `read_run`. `write_bytes_guest` already owns the
    /// span admission decision (`bulk_write_span_is_plain_ram`) and falls back
    /// to the exact per-byte `write_u8` dispatch for everything it refuses, so
    /// the override is pure delegation — pinned by
    /// `write_run_matches_per_byte_write_u8`.
    #[inline]
    fn write_run(&mut self, addr: usize, data: &[u8]) {
        Memory::write_bytes_guest(self, addr, data);
    }
}

// =============================================================================
// PagedMemory Trait Implementation
// =============================================================================

impl PagedMemory for Memory {
    #[inline]
    fn map_linear_physical(&self, linear: u32) -> Option<usize> {
        Memory::map_linear_physical(self, linear)
    }

    #[inline]
    fn tlb_lookup(&self, linear_page: u32) -> Option<(u32, bool, bool, bool)> {
        Memory::tlb_lookup(self, linear_page)
    }

    #[inline]
    fn tlb_insert(
        &mut self,
        linear_page: u32,
        physical_page: u32,
        writable: bool,
        user: bool,
        dirty: bool,
    ) {
        Memory::tlb_insert(self, linear_page, physical_page, writable, user, dirty);
    }

    #[inline]
    fn tlb_insert_large_page_slice(
        &mut self,
        linear_page: u32,
        physical_page: u32,
        writable: bool,
        user: bool,
        dirty: bool,
    ) {
        Memory::tlb_insert_large_page_slice(
            self,
            linear_page,
            physical_page,
            writable,
            user,
            dirty,
        );
    }

    #[inline]
    fn mark_tlb_global(&mut self, linear_page: u32, backing_addr: u32, large: bool) {
        Memory::mark_tlb_global(self, linear_page, backing_addr, large);
    }

    #[inline]
    fn tlb_global_source(&self, linear_page: u32) -> Option<(u32, bool)> {
        Memory::tlb_global_source(self, linear_page)
    }

    #[inline]
    fn tlb_global_backing_matches(&self, linear_page: u32, backing: u32) -> bool {
        Memory::tlb_global_backing_matches(self, linear_page, backing)
    }

    #[inline]
    fn invalidate_tlb_non_global(&mut self) {
        Memory::invalidate_tlb_non_global(self);
    }

    #[inline]
    fn invalidate_tlb_global_flush(&mut self) {
        Memory::invalidate_tlb_global_flush(self);
    }

    #[inline]
    fn read_u32_physical(&self, addr: usize) -> u32 {
        Memory::read_u32_physical(self, addr)
    }

    #[inline]
    fn read_u8_physical_checked(&self, addr: usize) -> Option<u8> {
        Memory::read_u8_physical_checked(self, addr)
    }

    #[inline]
    fn write_u32_physical(&mut self, addr: usize, value: u32) {
        Memory::write_u32_physical(self, addr, value);
    }

    #[inline]
    fn record_pt_frame(&mut self, physical_page: u32) {
        Memory::record_pt_frame(self, physical_page);
    }

    #[inline]
    fn write_u32_physical_ad(&mut self, addr: usize, value: u32) {
        Memory::write_u32_physical_ad(self, addr, value);
    }

    #[inline]
    fn set_page_fault(&mut self, linear_address: u32, error_code: u32) {
        self.page_fault_pending = true;
        self.page_fault_address = linear_address;
        self.page_fault_error_code = error_code;
    }

    #[inline]
    fn clear_page_fault(&mut self) {
        Memory::clear_page_fault(self);
    }

    #[inline]
    fn page_fault_pending(&self) -> bool {
        self.page_fault_pending
    }

    #[inline]
    fn page_fault_address(&self) -> u32 {
        self.page_fault_address
    }

    #[inline]
    fn page_fault_error_code(&self) -> u32 {
        self.page_fault_error_code
    }

    #[inline]
    fn invalidate_tlb(&mut self) {
        Memory::invalidate_tlb(self);
    }

    #[inline]
    fn invalidate_tlb_page(&mut self, linear_addr: u32) {
        Memory::invalidate_tlb_page(self, linear_addr);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod vga_native_commit_tests;
