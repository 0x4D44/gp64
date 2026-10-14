//! DPMI (DOS Protected Mode Interface) emulation
//!
//! This module provides a minimal DPMI 0.9 host implementation.

mod descriptors;
mod interrupts;
mod memory;
mod mode_switch;

use crate::dpmi_error::{
    DPMI_ERR_BACKING_STORE_UNAVAILABLE, DPMI_ERR_CALLBACK_UNAVAIL, DPMI_ERR_DESC_UNAVAILABLE,
    DPMI_ERR_HANDLE_UNAVAILABLE, DPMI_ERR_INVALID_CALLBACK, DPMI_ERR_INVALID_HANDLE,
    DPMI_ERR_INVALID_LINEAR, DPMI_ERR_INVALID_SELECTOR, DPMI_ERR_INVALID_VALUE,
    DPMI_ERR_LINEAR_UNAVAILABLE, DPMI_ERR_PHYS_UNAVAILABLE, DPMI_ERR_RESOURCE_UNAVAILABLE,
    DPMI_ERR_UNSUPPORTED,
};
use crate::memory::{DosMem, DosMemError};

/// Protected mode BIOS code selector (GDT[3] with RPL=3).
/// Used for IDT stubs that the emulator intercepts as HLE handlers.
/// This is index 3 * 8 = 0x18, with RPL=3 it becomes 0x1B.
pub const PM_BIOS_CODE_SEL: u16 = (3 * 8 + 3) as u16; // 0x1B
/// HLE prior-vector stubs returned by INT 21h/AH=35h for INT 1 and INT 3.
/// These are distinct from the live FE00+vector*2 dispatchers: a client saves
/// this target before installing a debugger, so a later far jump through it
/// must take the default IRET action instead of resolving the current handler.
/// The two used addresses are FD22 (INT 1) and FD26 (INT 3).
pub const HLE_PM_OLD_INT_VECTOR_STUB_BASE: u16 = 0xFD20;
/// Protected mode flat data selector (GDT[2] with RPL=3).
/// This is index 2 * 8 = 0x10, with RPL=3 it becomes 0x13.
pub const PM_FLAT_DATA_SEL: u16 = (2 * 8 + 3) as u16; // 0x13
/// Protected mode LDT selector (GDT[4], RPL=0).
pub const PM_LDT_SEL: u16 = (4 * 8) as u16; // 0x20
/// Ring-0 flat data selector (GDT[6], RPL=0), used as the TSS `SS0`.
///
/// **This cannot be [`PM_FLAT_DATA_SEL`].** On a privilege transition to CPL 0
/// the CPU loads `SS:ESP` from the TSS and requires the new stack selector's
/// DPL *and* RPL both to equal the new CPL (Intel SDM Vol. 3 §6.4.1;
/// enforced in `gp64_cpu::Cpu::load_ss`). `PM_FLAT_DATA_SEL` is a DPL=3
/// descriptor referenced at RPL=3, so it fails both checks and every gate that
/// actually changes privilege raises #GP instead of switching stacks.
pub const PM_RING0_STACK_SEL: u16 = (6 * 8) as u16; // 0x30
/// Locked exception stack selector (GDT[7], DPL=3, RPL=3).
///
/// DPMI 0.9 calls exception handlers "on a locked stack" of at least 4 KB,
/// depends on it: its handler simulates a far call by pushing onto the
/// faulting SS:SP named in the frame, which would overwrite a frame built on
/// that same stack (MDD-00017).
pub const PM_EXCEPTION_STACK_SEL: u16 = (7 * 8 + 3) as u16; // 0x3B
/// Size of the locked exception stack; the selector's initial SP/ESP is this.
pub const PM_EXCEPTION_STACK_SIZE: u32 = 0x1000;

/// HLE stub address for the Microsoft "MS-DOS API Extensions for DPMI Hosts"
/// vendor-specific API entry point (INT 2Fh AX=168Ah, vendor "MS-DOS").
///
/// Mirrors `HLE_MSDOS_EXT_ENTRY` in the root crate's `src/lib.rs`. Kept here
/// (rather than imported) because this crate has no dependency on the root,
/// and the wire-protocol nature of the address means the value cannot drift
/// without breaking guests anyway. A unit test
/// (`test_msdos_ext_entry_address_matches_root`) verifies the two stay in
/// sync via documentation rather than at compile time.
pub const HLE_MSDOS_EXT_ENTRY: u16 = 0xFD60;

/// Minimum RAM size for DPMI applications (8 MB).
/// protected mode executables. 8MB matches typical 386 systems.
const DPMI_MIN_RAM_SIZE: usize = 8 * 1024 * 1024;
const DPMI_PAGE_SIZE: usize = 0x1000;
const LINEAR_COMMIT_WORDS: usize = DOS_RAM_CEILING.div_ceil(DPMI_PAGE_SIZE * u64::BITS as usize);

// DOS error codes for INT 21h memory services (used by DPMI 0x0100-0x0102)
const DOS_ERR_MCB_DESTROYED: u16 = 0x0007;
const DOS_ERR_INSUFFICIENT_MEMORY: u16 = 0x0008;
const DOS_ERR_INVALID_MEMORY_BLOCK: u16 = 0x0009;

// DPMI 0.9 State Buffer size for 32-bit clients (54 bytes total).
// The state save/restore saves DPMI host internal state, not CPU registers.
// The buffer size is reported to the client via INT 31h AX=0305.
const STATE_BUFFER_SIZE_32: u16 = 0x36; // 54 bytes for 32-bit client

// Field offsets within the state buffer (used by tests only).
#[cfg(test)]
const STATE_OFF_CS: u32 = 0x2E;
use anyhow::Result;
use gp64_cpu::{Cpu, Registers, SegmentCache};
use gp64_memory::{Memory, DOS_RAM_CEILING};
use gp64_types::{FixedVec, PageAccess};
use std::sync::OnceLock;

/// Check if DPMI tracing is enabled via RUST_LOG (target: gp64::dos::dpmi)
#[allow(dead_code)] // Used in tests; also available for guarding expensive trace ops
#[cfg(test)]
fn dpmi_trace_enabled() -> bool {
    dpmi_trace_env_enabled() || tracing::enabled!(target: "gp64::dos::dpmi", tracing::Level::DEBUG)
}

// KILN-00086: the DPMI trace env enable was retired; the trace is now gated only
// by the RUST_LOG target `gp64::dos::dpmi` (see `dpmi_trace_enabled`).
fn dpmi_trace_env_enabled() -> bool {
    false
}

fn dpmi_trace_file() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    static FILE: OnceLock<Option<std::sync::Mutex<std::fs::File>>> = OnceLock::new();
    FILE.get_or_init(|| {
        // KILN-00086: the trace-file path override was env-configured (retired);
        // now the fixed default path.
        let path = std::path::PathBuf::from("tmp/dpmi_trace.log");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(std::sync::Mutex::new)
    })
    .as_ref()
}

fn dpmi_trace_emit(line: &str) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    const DPMI_TRACE_LIMIT: usize = 20000;
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if !dpmi_trace_env_enabled() {
        return;
    }
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if n >= DPMI_TRACE_LIMIT {
        return;
    }
    if let Some(file) = dpmi_trace_file() {
        if let Ok(mut handle) = file.lock() {
            use std::io::Write;
            let _ = writeln!(handle, "{line}");
            return;
        }
    }
    tracing::debug!(target: "gp64::dos::dpmi", "{}", line);
}

fn dpmi_trace_selector_update(
    cpu: &Cpu,
    label: &str,
    selector: u16,
    base: u32,
    limit: u32,
    access: u8,
    flags: u8,
) {
    if !dpmi_trace_env_enabled() {
        return;
    }
    let table = if (selector & 0x04) != 0 { "LDT" } else { "GDT" };
    dpmi_trace_emit(&format!(
        "[DPMI] {} sel={:04X} table={} base={:08X} limit={:08X} access={:02X} flags={:02X} cs={:04X} ss={:04X}",
        label, selector, table, base, limit, access, flags, cpu.regs.cs, cpu.regs.ss
    ));
}

// KILN-00086: the DPMI-return trace env enable was retired; permanently off.
fn dpmi_ret_trace_env_enabled() -> bool {
    false
}

fn dpmi_ret_trace_path() -> std::path::PathBuf {
    std::path::PathBuf::from("tmp/dpmi_ret_trace.log")
}

fn dpmi_ret_trace_emit(line: &str) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    const DPMI_RET_TRACE_LIMIT: usize = 50000;
    static COUNT: AtomicUsize = AtomicUsize::new(0);

    if !dpmi_ret_trace_env_enabled() {
        return;
    }
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if n >= DPMI_RET_TRACE_LIMIT {
        return;
    }

    let path = dpmi_ret_trace_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut handle) => {
            use std::io::Write;
            let _ = writeln!(handle, "{line}");
        }
        Err(_) => {
            tracing::debug!(target: "gp64::dos::dpmi", "{}", line);
        }
    }
}

fn next_dpmi_ret_trace_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

fn dpmi_error_name(code: u16) -> &'static str {
    match code {
        DPMI_ERR_UNSUPPORTED => "unsupported_function",
        DPMI_ERR_RESOURCE_UNAVAILABLE => "resource_unavailable",
        DPMI_ERR_DESC_UNAVAILABLE => "descriptor_unavailable",
        DPMI_ERR_LINEAR_UNAVAILABLE => "linear_memory_unavailable",
        DPMI_ERR_PHYS_UNAVAILABLE => "physical_memory_unavailable",
        DPMI_ERR_BACKING_STORE_UNAVAILABLE => "backing_store_unavailable",
        DPMI_ERR_CALLBACK_UNAVAIL => "callback_unavailable",
        DPMI_ERR_HANDLE_UNAVAILABLE => "handle_unavailable",
        DPMI_ERR_INVALID_VALUE => "invalid_value",
        DPMI_ERR_INVALID_SELECTOR => "invalid_selector",
        DPMI_ERR_INVALID_HANDLE => "invalid_handle",
        DPMI_ERR_INVALID_CALLBACK => "invalid_callback",
        DPMI_ERR_INVALID_LINEAR => "invalid_linear_address",
        _ => "unknown_error",
    }
}

fn dpmi_function_name(ax: u16) -> &'static str {
    match ax {
        0x0000 => "allocate_ldt_descriptors",
        0x0001 => "free_ldt_descriptor",
        0x0002 => "segment_to_descriptor",
        0x0003 => "get_selector_increment",
        0x0004 => "lock_selector",
        0x0005 => "unlock_selector",
        0x0006 => "get_segment_base",
        0x0007 => "set_segment_base",
        0x0008 => "set_segment_limit",
        0x0009 => "set_descriptor_access_rights",
        0x000A => "create_code_segment_alias",
        0x000B => "get_descriptor",
        0x000C => "set_descriptor",
        0x000D => "allocate_specific_ldt_descriptor",
        0x000E => "get_multiple_descriptors",
        0x000F => "set_multiple_descriptors",
        0x0100 => "allocate_dos_memory",
        0x0101 => "free_dos_memory",
        0x0102 => "resize_dos_memory",
        0x0200 => "get_real_mode_interrupt_vector",
        0x0201 => "set_real_mode_interrupt_vector",
        0x0202 => "get_exception_handler",
        0x0203 => "set_exception_handler",
        0x0204 => "get_pm_interrupt_vector",
        0x0205 => "set_pm_interrupt_vector",
        0x0212 => "get_extended_exception_handler",
        0x0213 => "set_extended_exception_handler",
        0x0300 => "simulate_real_mode_interrupt",
        0x0301 => "call_real_mode_proc_retf",
        0x0302 => "call_real_mode_proc_iret",
        0x0303 => "allocate_real_mode_callback",
        0x0304 => "free_real_mode_callback",
        0x0305 => "get_state_save_restore_addresses",
        0x0306 => "get_raw_mode_switch_addresses",
        0x0400 => "get_version",
        0x0401 => "get_capabilities",
        0x0500 => "get_free_memory_information",
        0x0501 => "allocate_memory_block",
        0x0502 => "free_memory_block",
        0x0503 => "resize_memory_block",
        0x0504 => "allocate_linear_memory_block",
        0x050A => "get_memory_block_size_and_base",
        0x050B => "get_memory_information",
        0x050C => "resize_linear_memory_block",
        0x0600 => "lock_linear_region",
        0x0601 => "unlock_linear_region",
        0x0602 => "mark_real_mode_region_pageable",
        0x0603 => "relock_real_mode_region",
        0x0604 => "get_page_size",
        0x0700 => "mark_pages_demand_paging_candidate",
        0x0701 => "discard_pages",
        0x0702 => "mark_page_demand_paging_candidate",
        0x0703 => "discard_page_contents",
        0x0800 => "map_physical_address",
        0x0801 => "free_physical_mapping",
        0x0900 => "get_and_disable_virtual_interrupt_state",
        0x0901 => "get_and_enable_virtual_interrupt_state",
        0x0902 => "get_virtual_interrupt_state",
        0x0A00 => "get_vendor_api_entry_point",
        0x0B00 => "set_debug_watchpoint",
        0x0B01 => "clear_debug_watchpoint",
        0x0B02 => "get_debug_watchpoint_state",
        0x0B03 => "reset_debug_watchpoint_state",
        0x0D00 => "allocate_shared_memory",
        0x0D01 => "free_shared_memory",
        0x0D02 => "serialize_shared_memory",
        0x0E00 => "get_coprocessor_status",
        0x0E01 => "set_coprocessor_emulation",
        _ => "unknown",
    }
}

#[derive(Clone, Copy)]
struct DpmiTraceRegs {
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
    esi: u32,
    edi: u32,
    ebp: u32,
    esp: u32,
    eip: u32,
    cs: u16,
    ds: u16,
    es: u16,
    fs: u16,
    gs: u16,
    ss: u16,
    flags: u32,
    cr0: u32,
}

impl DpmiTraceRegs {
    fn capture(cpu: &Cpu) -> Self {
        Self {
            eax: cpu.regs.eax,
            ebx: cpu.regs.ebx,
            ecx: cpu.regs.ecx,
            edx: cpu.regs.edx,
            esi: cpu.regs.esi,
            edi: cpu.regs.edi,
            ebp: cpu.regs.ebp,
            esp: cpu.regs.esp,
            eip: cpu.regs.eip,
            cs: cpu.regs.cs,
            ds: cpu.regs.ds,
            es: cpu.regs.es,
            fs: cpu.regs.fs,
            gs: cpu.regs.gs,
            ss: cpu.regs.ss,
            flags: cpu.regs.eflags(),
            cr0: cpu.regs.cr0,
        }
    }

    fn ax(self) -> u16 {
        self.eax as u16
    }

    fn bx(self) -> u16 {
        self.ebx as u16
    }

    fn cx(self) -> u16 {
        self.ecx as u16
    }

    fn dx(self) -> u16 {
        self.edx as u16
    }

    fn si(self) -> u16 {
        self.esi as u16
    }

    fn di(self) -> u16 {
        self.edi as u16
    }

    fn offset_di(self, is_32bit: bool) -> u32 {
        if is_32bit {
            self.edi
        } else {
            self.di() as u32
        }
    }

    fn offset_dx(self, is_32bit: bool) -> u32 {
        if is_32bit {
            self.edx
        } else {
            self.dx() as u32
        }
    }

    fn format_fields(self, prefix: &str) -> String {
        format!(
            "{prefix}_eax={:08X} {prefix}_ebx={:08X} {prefix}_ecx={:08X} {prefix}_edx={:08X} \
             {prefix}_esi={:08X} {prefix}_edi={:08X} {prefix}_ebp={:08X} {prefix}_esp={:08X} \
             {prefix}_cs={:04X} {prefix}_eip={:08X} {prefix}_ds={:04X} {prefix}_es={:04X} \
             {prefix}_fs={:04X} {prefix}_gs={:04X} {prefix}_ss={:04X} {prefix}_flags={:08X} \
             {prefix}_cr0={:08X}",
            self.eax,
            self.ebx,
            self.ecx,
            self.edx,
            self.esi,
            self.edi,
            self.ebp,
            self.esp,
            self.cs,
            self.eip,
            self.ds,
            self.es,
            self.fs,
            self.gs,
            self.ss,
            self.flags,
            self.cr0
        )
    }
}

#[derive(Clone, Copy)]
struct DpmiTraceFrame {
    is_32bit: bool,
    ss: u16,
    esp: u32,
    ret_cs: u16,
    ret_eip: u32,
    ret_flags: u32,
}

impl DpmiTraceFrame {
    fn capture(dpmi: &Dpmi, cpu: &Cpu, memory: &Memory) -> Self {
        let is_32bit = cpu.is_protected_mode()
            && !cpu.regs.is_v86_mode()
            && (cpu.regs.ss_cache.big || dpmi.cpu_mode == 1);
        let ss_base = cpu.regs.ss_cache.base as usize;
        let esp = if is_32bit {
            cpu.regs.esp
        } else {
            cpu.regs.sp() as u32
        };
        let base = ss_base.wrapping_add(esp as usize);

        if is_32bit {
            Self {
                is_32bit,
                ss: cpu.regs.ss,
                esp,
                ret_eip: memory.read_u32(base),
                ret_cs: memory.read_u32(base.wrapping_add(4)) as u16,
                ret_flags: memory.read_u32(base.wrapping_add(8)),
            }
        } else {
            Self {
                is_32bit,
                ss: cpu.regs.ss,
                esp,
                ret_eip: memory.read_u16(base) as u32,
                ret_cs: memory.read_u16(base.wrapping_add(2)),
                ret_flags: memory.read_u16(base.wrapping_add(4)) as u32,
            }
        }
    }

    fn caller_eip(self) -> u32 {
        self.ret_eip.wrapping_sub(2)
    }
}

fn format_trace_bytes(memory: &Memory, addr: usize, len: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for idx in 0..len {
        if idx > 0 {
            out.push('_');
        }
        let _ = write!(out, "{:02X}", memory.read_u8(addr + idx));
    }
    out
}

fn format_trace_dwords(memory: &Memory, addr: usize, bytes: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("[");
    for idx in 0..(bytes / 4) {
        if idx > 0 {
            out.push(',');
        }
        let _ = write!(out, "{:08X}", memory.read_u32(addr + idx * 4));
    }
    out.push(']');
    out
}

fn format_rmcs_state(prefix: &str, rmcs: RealModeCallStruct) -> String {
    format!(
        "{prefix}_edi={:08X} {prefix}_esi={:08X} {prefix}_ebp={:08X} \
         {prefix}_ebx={:08X} {prefix}_edx={:08X} {prefix}_ecx={:08X} {prefix}_eax={:08X} \
         {prefix}_flags={:04X} {prefix}_es={:04X} {prefix}_ds={:04X} {prefix}_fs={:04X} \
         {prefix}_gs={:04X} {prefix}_cs={:04X} {prefix}_ip={:04X} {prefix}_ss={:04X} {prefix}_sp={:04X}",
        rmcs.edi,
        rmcs.esi,
        rmcs.ebp,
        rmcs.ebx,
        rmcs.edx,
        rmcs.ecx,
        rmcs.eax,
        rmcs.flags,
        rmcs.es,
        rmcs.ds,
        rmcs.fs,
        rmcs.gs,
        rmcs.cs,
        rmcs.ip,
        rmcs.ss,
        rmcs.sp
    )
}

/// Emit the post-HLE RMCS result for INT 31h/0300-0302 requests that were
/// handled directly by the emulator instead of by entering V86 mode.
pub fn trace_ret_hle_rmcs(
    int_num: u8,
    struct_addr: usize,
    before: RealModeCallStruct,
    after: RealModeCallStruct,
    input_wait: bool,
) {
    if !dpmi_ret_trace_env_enabled() {
        return;
    }
    dpmi_ret_trace_emit(&format!(
        "[DPMI_RET_RMCS] source=hle int={:02X} rmcs_addr={:08X} input_wait={} {} {}",
        int_num,
        struct_addr,
        input_wait,
        format_rmcs_state("before", before),
        format_rmcs_state("after", after)
    ));
}

/// Emit the post-V86 RMCS result for INT 31h/0300-0302 requests that ran
/// real-mode code and returned through the DPMI translation sentinel.
pub fn trace_ret_v86_rmcs(
    struct_addr: usize,
    before: RealModeCallStruct,
    after: RealModeCallStruct,
) {
    if !dpmi_ret_trace_env_enabled() {
        return;
    }
    dpmi_ret_trace_emit(&format!(
        "[DPMI_RET_RMCS] source=v86 rmcs_addr={:08X} {} {}",
        struct_addr,
        format_rmcs_state("before", before),
        format_rmcs_state("after", after)
    ));
}

/// Page locking statistics for debugging (DPMI 0600h-0603h)
#[derive(Clone, Copy, Default, Debug)]
pub struct PageLockStats {
    /// Number of lock linear region calls (0600h)
    pub lock_calls: u32,
    /// Number of unlock linear region calls (0601h)
    pub unlock_calls: u32,
    /// Number of mark pageable calls (0602h)
    pub pageable_calls: u32,
    /// Number of relock calls (0603h)
    pub relock_calls: u32,
    /// Total bytes requested for locking
    pub total_bytes_locked: u64,
    /// Total bytes requested for unlocking
    pub total_bytes_unlocked: u64,
}

/// IDT entry for protected mode interrupts/exceptions
#[derive(Clone, Copy, Default)]
pub struct IdtEntry {
    /// Code segment selector
    pub selector: u16,
    /// Handler offset
    pub offset: u32,
    /// Gate type: 0x0E = 32-bit interrupt gate, 0x0F = 32-bit trap gate
    pub gate_type: u8,
    /// Descriptor privilege level
    pub dpl: u8,
    /// Entry present
    pub present: bool,
}

/// Real-mode callback entry
#[derive(Clone, Copy, Default)]
pub struct RealModeCallback {
    /// Callback address in real-mode addressable memory (segment)
    pub rm_segment: u16,
    /// Callback offset
    pub rm_offset: u16,
    /// Protected mode handler selector
    pub pm_selector: u16,
    /// Protected mode handler offset
    pub pm_offset: u32,
    /// Protected mode data selectors captured at allocation time
    pub pm_ds: u16,
    pub pm_es: u16,
    pub pm_fs: u16,
    pub pm_gs: u16,
    /// Register structure selector
    pub reg_struct_selector: u16,
    /// Register structure offset
    pub reg_struct_offset: u32,
    /// Is this slot in use?
    pub in_use: bool,
}

/// Real-mode call structure (used by INT 31h AX=0300-0302)
/// This is the register structure passed to simulate real-mode interrupts.
/// DPMI 0.9 spec says 50 bytes, but we use proper alignment internally.
/// The read_from_memory/write_to_memory methods use correct offsets per spec.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct RealModeCallStruct {
    pub edi: u32,
    pub esi: u32,
    pub ebp: u32,
    pub reserved: u32, // ESP (ignored on call)
    pub ebx: u32,
    pub edx: u32,
    pub ecx: u32,
    pub eax: u32,
    pub flags: u16,
    pub es: u16,
    pub ds: u16,
    pub fs: u16,
    pub gs: u16,
    pub ip: u16,
    pub cs: u16,
    pub sp: u16,
    pub ss: u16,
}

impl RealModeCallStruct {
    /// The size of the structure in memory (50 bytes as per DPMI spec)
    pub const MEMORY_SIZE: usize = 50;

    /// Read the structure from memory at the given linear address
    pub fn read_from_memory(memory: &Memory, addr: usize) -> Self {
        Self {
            edi: memory.read_u32(addr),
            esi: memory.read_u32(addr + 4),
            ebp: memory.read_u32(addr + 8),
            reserved: memory.read_u32(addr + 12),
            ebx: memory.read_u32(addr + 16),
            edx: memory.read_u32(addr + 20),
            ecx: memory.read_u32(addr + 24),
            eax: memory.read_u32(addr + 28),
            flags: memory.read_u16(addr + 32),
            es: memory.read_u16(addr + 34),
            ds: memory.read_u16(addr + 36),
            fs: memory.read_u16(addr + 38),
            gs: memory.read_u16(addr + 40),
            ip: memory.read_u16(addr + 42),
            cs: memory.read_u16(addr + 44),
            sp: memory.read_u16(addr + 46),
            ss: memory.read_u16(addr + 48),
        }
    }

    /// Write the structure to memory at the given linear address
    pub fn write_to_memory(&self, memory: &mut Memory, addr: usize) {
        memory.write_u32(addr, self.edi);
        memory.write_u32(addr + 4, self.esi);
        memory.write_u32(addr + 8, self.ebp);
        memory.write_u32(addr + 12, self.reserved);
        memory.write_u32(addr + 16, self.ebx);
        memory.write_u32(addr + 20, self.edx);
        memory.write_u32(addr + 24, self.ecx);
        memory.write_u32(addr + 28, self.eax);
        memory.write_u16(addr + 32, self.flags);
        memory.write_u16(addr + 34, self.es);
        memory.write_u16(addr + 36, self.ds);
        memory.write_u16(addr + 38, self.fs);
        memory.write_u16(addr + 40, self.gs);
        memory.write_u16(addr + 42, self.ip);
        memory.write_u16(addr + 44, self.cs);
        memory.write_u16(addr + 46, self.sp);
        memory.write_u16(addr + 48, self.ss);
    }
}

/// Snapshot of the CPU's TSS-related registers, captured by
/// [`Dpmi::install_v86_dispatch_tss`] and restored by
/// [`Dpmi::restore_v86_dispatch_tss`]. Kept tiny and on the stack —
/// V86 dispatch is synchronous so there's no need to thread it through
/// the persistent `V86SavedState` stack.
#[derive(Clone, Copy)]
pub struct V86DispatchTssSavedState {
    pub tr: u16,
    pub tss_base: u32,
    pub tss_limit: u32,
    pub tss_esp0: u32,
    pub tss_ss0: u16,
}

/// Saved state when entering V86 mode for translation
#[derive(Clone, Copy)]
struct V86SavedState {
    cs: u16,
    ip: u32,
    ss: u16,
    esp: u32,
    ds: u16,
    es: u16,
    fs: u16,
    gs: u16,
    eflags: u32,
    // CR0 must be saved/restored to prevent corruption if raw mode-switch
    // or other CR0-modifying operations occur during V86 execution
    cr0: u32,
    // Pre-V86 CPL — V86 forces CPL=3 architecturally (SDM §20.1), so we
    // must restore the saved PM CPL on exit_v86 to avoid leaking 3 into
    // PM and tripping any CPL>0 privilege checks.
    cpl: u8,
    // General purpose registers to restore (V86 overwrites these)
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
    esi: u32,
    edi: u32,
    ebp: u32,
    // Segment caches to restore
    cs_cache: SegmentCache,
    ds_cache: SegmentCache,
    es_cache: SegmentCache,
    ss_cache: SegmentCache,
    fs_cache: SegmentCache,
    gs_cache: SegmentCache,
    /// Linear address of the RealModeCallStruct to update on exit
    rm_struct_addr: usize,
    prev_exit_on_iret: bool,
    prev_exit_on_retf: bool,
}

/// Pending HLE simulation request from DPMI 0x0300/0x0301/0x0302
/// When the IVT handler points to an HLE stub (F000:xxxx), DPMI cannot
/// enter V86 mode because hle_iret() would pop the sentinel frame and
/// corrupt the CPU state. Instead, DPMI stores this request and the
/// emulator handles the HLE interrupt directly.
#[derive(Clone, Copy, Debug)]
pub struct PendingHleSimulation {
    /// Interrupt number to simulate (from BL for 0x0300, or 0 for 0x0301/0x0302)
    pub int_num: u8,
    /// Linear address of the RealModeCallStruct
    pub struct_addr: usize,
    /// Whether to reset IF before simulation (from BH bit 0)
    pub reset_if: bool,
}

/// DPMI state
#[derive(Clone)]
pub struct Dpmi {
    /// Protected mode active
    protected_mode: bool,
    /// Allocated selectors
    selectors: FixedVec<Selector, 2048>,
    /// Next selector value
    next_selector: u16,
    /// Freed selectors available for reuse
    free_selectors: FixedVec<u16, 2048>,
    /// First LDT **index** that DPMI must never hand out, because another owner
    /// holds it.
    ///
    /// `None` — the default and the state of every guest that does not host a
    /// 16-bit DLL inside a 32-bit process — means the whole LDT is DPMI's, so
    /// allocation behaviour is byte-identical to before this field existed.
    ///
    /// `Some(i)` is set by the root when a hosted 16-bit context is installed
    /// over the flat-thunk window's LDT, whose upper indices belong to the
    /// `MapLS` table and the hosted HLE code selectors. A *cursor cap* would
    /// not be enough: the free list, a multi-descriptor run straddling the
    /// boundary and `INT 31h/000Dh` all reach selectors the cursor never
    /// touched. Every allocation path therefore asks
    /// [`Dpmi::selector_run_admitted`] instead.
    ///
    reserved_ldt_index: Option<u16>,
    /// Set while the LDT is a 32-bit process's system LDT: the index of its
    /// flat code descriptor (flat data occupies the next index). Those two and the null index 0 belong to the system
    /// and are never handed out. `None` for every plain DPMI client, which
    /// keeps every index. Checked by [`Dpmi::selector_run_admitted`] too.
    system_flat_ldt_index: Option<u16>,
    /// Cached mapping from real-mode segment to selector (INT 31h/0002)
    /// Index = segment, value = selector (0 = unmapped sentinel)
    pub(crate) segment_selectors: Box<[u16; 65536]>,
    /// Allocated memory blocks (extended memory)
    memory_blocks: FixedVec<MemoryBlock, 512>,
    /// DPMI 1.0 linear memory blocks (separate from memory_blocks)
    linear_memory_blocks: FixedVec<LinearMemoryBlock, 32>,
    /// Next handle for memory blocks
    next_mem_handle: u32,
    /// Allocated DOS memory blocks (conventional memory)
    dos_memory_blocks: FixedVec<DosMemoryBlock, 256>,
    /// Physical address mappings (DPMI 0.9 0800h/0801h)
    physical_mappings: FixedVec<PhysicalMapping, 64>,
    /// CPU mode (0 = 16-bit, 1 = 32-bit)
    cpu_mode: u8,
    /// GDT base (physical address) used by the DPMI host (0 if uninitialized)
    gdt_base: u32,
    /// GDT size in bytes (0 if uninitialized)
    gdt_size: u32,
    /// LDT base (physical address) used for allocated selectors (0 if uninitialized)
    ldt_base: u32,
    /// LDT size in bytes (0 if uninitialized)
    ldt_size: u32,
    /// IDT base (physical address) used for a safe default IDT (0 if uninitialized)
    idt_base: u32,
    /// IDT size in bytes (0 if uninitialized)
    idt_size: u32,
    /// Base of the host's TSS + I/O-permission bitmap + ring-0 stack span
    /// (0 if uninitialized).
    ///
    /// Reserved from client linear allocation for the same reason the GDT,
    /// LDT and IDT are: the CPU reads the TSS out of guest memory on every
    /// privilege transition, so a client block placed over it corrupts the
    /// interrupt stack switch. Recording base and size here is what lets
    /// `push_descriptor_ranges` and `overlaps_descriptor_tables` see it.
    host_control_base: u32,
    /// Size in bytes of that span (0 if uninitialized).
    host_control_size: u32,
    /// Protected mode IDT entries (256 entries)
    pm_idt: [IdtEntry; 256],
    /// Exception handlers (0x00-0x1F)
    exception_handlers: [IdtEntry; 32],
    /// Real-mode callback slots (max 16)
    rm_callbacks: [RealModeCallback; 16],
    /// Callback segment (fixed at F000h for simplicity)
    callback_segment: u16,
    /// V86 mode is currently active (executing real-mode code for translation)
    v86_active: bool,
    /// Should IRET in V86 return to protected mode
    v86_exit_on_iret: bool,
    /// Should RETF in V86 return to protected mode
    v86_exit_on_retf: bool,
    /// Saved protected mode state during V86
    v86_saved_state: FixedVec<V86SavedState, 16>,
    /// Virtual interrupt state (DPMI 0900h-0902h)
    virtual_interrupts_enabled: bool,
    /// Pending hardware interrupts queued while virtual interrupts were disabled (DPMI-116)
    pending_interrupts: FixedVec<u8, 256>,
    /// Number of protected-mode callbacks currently executing.
    ///
    /// DPMI supplies one locked PM stack per task. Nested callbacks share the
    /// active stack frame, so this must retain the outer callback's state while
    /// an inner callback returns.
    pm_callback_depth: u16,
    /// Debug watchpoints (DPMI 0B00h-0B03h)
    debug_watchpoints: [DebugWatchpoint; 4],
    /// Page locking statistics (DPMI 0600h-0603h)
    page_lock_stats: PageLockStats,
    /// Environment selector allocated during protected mode entry (DPMI 0.9 spec)
    env_selector: u16,
    /// Pending HLE simulation request (when IVT handler is F000:xxxx)
    pending_hle_simulation: Option<PendingHleSimulation>,
    /// Client PM interrupt handlers set via INT 21h/25h DPMI translation.
    /// Separate from pm_idt[] to distinguish application handlers from
    /// DOS extender thunks installed via INT 31h/0205h.
    client_pm_int_handlers: [Option<(u16, u32)>; 256],
    /// Cached selector returned by MS-DOS API Extensions Function 0100h
    /// (Get Selector to Base of LDT). Allocated on first call and reused.
    /// 0 = not yet allocated.
    msdos_ext_ldt_selector: u16,
}

/// Snapshot of CPU fields mutated during protected mode entry.
struct CpuPmEntrySnapshot {
    regs: Registers,
    cs_bits: u32,
    allow_pm_real_mode_ss: bool,
    dpmi_active: bool,
}

impl CpuPmEntrySnapshot {
    fn new(cpu: &Cpu) -> Self {
        Self {
            regs: cpu.regs.clone(),
            cs_bits: cpu.cs_bits,
            allow_pm_real_mode_ss: cpu.allow_pm_real_mode_ss,
            dpmi_active: cpu.dpmi_active,
        }
    }

    fn restore(&self, cpu: &mut Cpu) {
        cpu.regs = self.regs.clone();
        cpu.cs_bits = self.cs_bits;
        cpu.allow_pm_real_mode_ss = self.allow_pm_real_mode_ss;
        cpu.dpmi_active = self.dpmi_active;
    }
}

/// Descriptor for a selector
#[derive(Clone, Copy)]
struct Selector {
    selector: u16,
    base: u32,
    limit: u32,
    access: u8,
    flags: u8,
    lock_count: u32,
}

/// Allocated memory block
#[derive(Clone, Copy)]
struct MemoryBlock {
    handle: u32,
    address: u32,
    size: u32,
    /// Actual reserved linear range (>= size).  Over-allocation lets DPMI 0503
    /// resize succeed without moving the block, matching real DPMI hosts that
    /// reserve all available extended memory for the first allocation.
    capacity: u32,
}

impl MemoryBlock {
    /// Create a new memory block where capacity equals size (no over-allocation).
    #[cfg(test)]
    fn new(handle: u32, address: u32, size: u32) -> Self {
        Self {
            handle,
            address,
            size,
            capacity: size,
        }
    }
}

/// Extended memory block with page-level control (DPMI 1.0)
/// Separate from MemoryBlock to support per-page commit tracking
#[derive(Clone)]
struct LinearMemoryBlock {
    handle: u32,
    base: u32, // Linear base address
    size: u32, // Total size in bytes
    // Per-page commit status. Capacity follows the allocator's RAM ceiling so
    // every accepted block is representable (64 pages per u64). Boxed because
    // the worst-case bitmap is 8 KiB; embedding it inline in the 32-entry
    // block table put ~256 KiB inside Dpmi, whose by-value transit through
    // debug-build construction overflowed libtest-sized (2 MiB) stacks.
    committed: Box<FixedVec<u64, LINEAR_COMMIT_WORDS>>,
    page_count: usize, // Total number of pages
}

impl LinearMemoryBlock {
    /// Create a new linear memory block with all pages set to the given commit state
    fn new(handle: u32, base: u32, size: u32, all_committed: bool) -> Self {
        let page_count = (size as usize).div_ceil(DPMI_PAGE_SIZE);
        let words_needed = page_count.div_ceil(64);
        let initial = if all_committed { u64::MAX } else { 0 };
        let mut committed = Box::new(FixedVec::new());
        for _ in 0..words_needed {
            committed.push(initial);
        }
        Self {
            handle,
            base,
            size,
            committed,
            page_count,
        }
    }

    /// Check if a page is committed
    fn is_page_committed(&self, page_idx: usize) -> bool {
        if page_idx >= self.page_count {
            return false;
        }
        let word = page_idx / 64;
        let bit = page_idx % 64;
        (self.committed.get(word).copied().unwrap_or(0) & (1u64 << bit)) != 0
    }

    /// Set page commit status
    fn set_page_committed(&mut self, page_idx: usize, committed: bool) {
        if page_idx >= self.page_count {
            return;
        }
        let word = page_idx / 64;
        let bit = page_idx % 64;
        while word >= self.committed.len() {
            self.committed.push(0);
        }
        if committed {
            self.committed[word] |= 1u64 << bit;
        } else {
            self.committed[word] &= !(1u64 << bit);
        }
    }

    /// Resize the page tracking to a new page count
    fn resize_pages(&mut self, new_page_count: usize, new_pages_committed: bool) {
        let old_page_count = self.page_count;
        let new_words = new_page_count.div_ceil(64);

        if new_page_count > old_page_count {
            // Growing: extend vector and set new pages
            while self.committed.len() < new_words {
                self.committed.push(0);
            }
            if new_pages_committed {
                // Set new pages as committed
                for i in old_page_count..new_page_count {
                    let word = i / 64;
                    let bit = i % 64;
                    self.committed[word] |= 1u64 << bit;
                }
            }
        } else {
            // Shrinking: just truncate the vector
            self.committed.truncate(new_words);
            // Clear any bits beyond the new page count in the last word
            if new_words > 0 && !new_page_count.is_multiple_of(64) {
                let mask = (1u64 << (new_page_count % 64)) - 1;
                self.committed[new_words - 1] &= mask;
            }
        }
        self.page_count = new_page_count;
    }
}

/// Physical address mapping entry
#[derive(Clone, Copy)]
struct PhysicalMapping {
    #[allow(dead_code)]
    physical: u32,
    linear: u32,
    size: u32,
}

/// Allocated DOS memory block (conventional memory)
#[allow(dead_code)]
#[derive(Clone, Copy)]
struct DosMemoryBlock {
    segment: u16,
    selector: u16,
    paragraphs: u16,
    selector_count: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DosMemoryBlockError {
    DescriptorUnavailable,
    InsufficientMemory { largest_available: u16 },
    InvalidBlock,
}

/// Watchpoint size encoding per DPMI spec (bytes)
#[derive(Clone, Copy, Default, Debug, PartialEq)]
#[repr(u8)]
enum WatchpointSize {
    #[default]
    Byte = 1, // 1 byte
    Word = 2,  // 2 bytes
    DWord = 4, // 4 bytes
}

impl WatchpointSize {
    /// Convert from raw byte value, returning None for invalid values
    fn from_raw(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Byte),
            2 => Some(Self::Word),
            4 => Some(Self::DWord),
            _ => None,
        }
    }
}

/// Watchpoint type encoding per DPMI spec
#[derive(Clone, Copy, Default, Debug, PartialEq)]
#[repr(u8)]
enum WatchpointType {
    Execute = 0, // Break on execution
    #[default]
    Write = 1, // Break on write
    ReadWrite = 2, // Break on read or write
}

impl WatchpointType {
    /// Convert from raw byte value, returning None for invalid values (>2)
    fn from_raw(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Execute),
            1 => Some(Self::Write),
            2 => Some(Self::ReadWrite),
            _ => None,
        }
    }
}

/// Debug watchpoint entry (for INT 31h AX=0Bxxh functions)
/// Note: This is a stub implementation - watchpoints are tracked but don't
/// actually trigger on memory access. This is for compatibility only.
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
struct DebugWatchpoint {
    active: bool,
    linear_address: u32,
    size: WatchpointSize,
    watch_type: WatchpointType,
    triggered: bool,
}

impl Dpmi {
    /// Linear address of the scratch `RealModeCallStruct` used by
    /// HLE-initiated V86 callback dispatches (mouse callback today;
    /// future keyboard callback likely). 50 bytes are needed for the RMCS.
    ///
    /// Lives inside the EBDA region (`0x9FC00–0x9FFFF`) which the BIOS
    /// reserves and DOS programs cannot allocate from, so it cannot
    /// collide with guest memory.
    pub const V86_CALLBACK_SCRATCH_ADDR: usize = 0x9FE00;

    /// Real-mode stack segment for HLE-initiated V86 callbacks.
    /// Paired with [`Dpmi::V86_CALLBACK_STACK_TOP`] this gives an initial
    /// `SS:SP = 9FF0:0100` → 256-byte stack growing down to `0x9FF00`
    /// at the top of the EBDA region. Chosen to keep the stack fully
    /// inside EBDA so the first push cannot wrap into VGA memory the
    /// way the legacy `enter_v86_for_interrupt` 9FC0:0000 default does.
    pub const V86_CALLBACK_STACK_SEG: u16 = 0x9FF0;

    /// Initial SP for the HLE-initiated V86 callback stack. See
    /// [`Dpmi::V86_CALLBACK_STACK_SEG`] for the rationale.
    pub const V86_CALLBACK_STACK_TOP: u16 = 0x0100;

    /// Linear address of the minimal V86-dispatch TSS used to grant the
    /// V86 callback unrestricted port I/O via an all-zeros IOPM.
    ///
    /// HLE-initiated V86 dispatch (`dispatch_rm_mouse_callback_via_v86`)
    /// installs this TSS into `cpu.regs.tr/tss_base/tss_limit/tss_esp0/
    /// tss_ss0` when the running PM environment (e.g. a DOS extender) didn't
    /// configure a TSS with an IOPM. Without it the V86 CPU's first
    /// `IN`/`OUT` raises `#GP(0)` (V86 always consults the IOPM per
    /// SDM Vol 3A §20.3.3), and `#GP` delivery itself faults because
    /// `read_tss_stack(0)` needs a valid `tss_limit`, producing the
    /// cascade into a double fault and then a triple fault.
    ///
    /// Lives in HMA, well away from a DOS extender's PM-mapped extended memory
    /// and conventional DOS heap. Restored to whatever the PM owner had
    /// when `dispatch_rm_mouse_callback_via_v86` returns.
    pub const V86_DISPATCH_TSS_BASE: u32 = 0x10A000;
    pub const V86_DISPATCH_TSS_HEADER_SIZE: u32 = 0x68;
    pub const V86_DISPATCH_TSS_IOPM_SIZE: u32 = 0x2000; // 65536 port bits

    pub fn new() -> Self {
        // Initialize default IDT entries pointing to BIOS IRET stub
        let default_idt_entry = IdtEntry {
            selector: 0x0008, // Default code selector
            offset: 0xFF00,   // BIOS IRET stub
            gate_type: 0x0E,  // 32-bit interrupt gate
            dpl: 3,
            present: true,
        };

        Self {
            protected_mode: false,
            selectors: FixedVec::new(),
            // Start above reserved selectors, return LDT selectors with TI=1 and RPL=3.
            next_selector: 0x0107,
            free_selectors: FixedVec::new(),
            reserved_ldt_index: None,
            system_flat_ldt_index: None,
            segment_selectors: vec![0u16; 65536].into_boxed_slice().try_into().unwrap(),
            memory_blocks: FixedVec::new(),
            linear_memory_blocks: FixedVec::new(),
            next_mem_handle: 1,
            dos_memory_blocks: FixedVec::new(),
            physical_mappings: FixedVec::new(),
            cpu_mode: 0,
            gdt_base: 0,
            gdt_size: 0,
            ldt_base: 0,
            ldt_size: 0,
            idt_base: 0,
            idt_size: 0,
            host_control_base: 0,
            host_control_size: 0,
            pm_idt: [default_idt_entry; 256],
            exception_handlers: [IdtEntry::default(); 32],
            rm_callbacks: [RealModeCallback::default(); 16],
            callback_segment: 0xF200,
            v86_active: false,
            v86_exit_on_iret: false,
            v86_exit_on_retf: false,
            v86_saved_state: FixedVec::new(),
            virtual_interrupts_enabled: true,
            pending_interrupts: FixedVec::new(),
            pm_callback_depth: 0,
            debug_watchpoints: [DebugWatchpoint::default(); 4],
            page_lock_stats: PageLockStats::default(),
            env_selector: 0,
            pending_hle_simulation: None,
            client_pm_int_handlers: [None; 256],
            msdos_ext_ldt_selector: 0,
        }
    }

    /// Set DPMI client mode (0 = 16-bit, 1 = 32-bit).
    /// Used by fuzz harnesses to test 32-bit client code paths.
    pub fn set_cpu_mode(&mut self, mode: u8) {
        debug_assert!(mode <= 1, "cpu_mode must be 0 (16-bit) or 1 (32-bit)");
        self.cpu_mode = mode;
    }

    /// Reset DPMI state for new client (B12)
    /// Called when a program terminates to clear persistent state
    pub fn reset(&mut self) {
        // Default IDT entry for pm_idt (same as in new())
        let default_idt_entry = IdtEntry {
            selector: 0x0008, // Default code selector
            offset: 0xFF00,   // BIOS IRET stub
            gate_type: 0x0E,  // 32-bit interrupt gate
            dpl: 3,
            present: true,
        };

        // Clear all client-specific state
        self.selectors.clear();
        self.free_selectors.clear();
        self.segment_selectors.fill(0);
        self.memory_blocks.clear();
        self.linear_memory_blocks.clear();
        self.next_mem_handle = 1;
        self.dos_memory_blocks.clear();
        self.physical_mappings.clear();
        self.cpu_mode = 0;
        self.next_selector = 0x0107; // First client selector (LDT with TI=1, RPL=3)
                                     // A new client cannot inherit another owner's LDT reservation: reset
                                     // means the old address space is gone, and the hosted 16-bit context
                                     // that set the reservation is torn down with it. Leaving it set would
                                     // cap an ordinary DPMI guest's selectors at the reserved index for no
                                     // reason.
        self.reserved_ldt_index = None;
        self.system_flat_ldt_index = None;
        self.protected_mode = false;

        // Reset real-mode callbacks
        for cb in &mut self.rm_callbacks {
            *cb = RealModeCallback::default();
        }

        // Reset PM IDT to default (BIOS IRET stubs)
        self.pm_idt.fill(default_idt_entry);

        // Reset exception handlers
        for entry in &mut self.exception_handlers {
            *entry = IdtEntry::default();
        }

        // Clear V86 state
        self.v86_active = false;
        self.v86_exit_on_iret = false;
        self.v86_exit_on_retf = false;
        self.v86_saved_state.clear();

        // Clear interrupt state
        self.pending_interrupts.clear();
        self.virtual_interrupts_enabled = true;
        self.pm_callback_depth = 0;

        // Reset debug watchpoints
        for wp in &mut self.debug_watchpoints {
            *wp = DebugWatchpoint::default();
        }

        // Reset page lock stats
        self.page_lock_stats = PageLockStats::default();

        self.env_selector = 0;
        self.pending_hle_simulation = None;
        self.client_pm_int_handlers = [None; 256];
        self.msdos_ext_ldt_selector = 0;

        // Clear the terminated client's descriptor-table geometry so
        // was_initialized() (gdt_base != 0) no longer reports it as a live DPMI
        // client. Otherwise a later real-mode child's termination is misclassified
        // as a DPMI program exit and can stop the emulator early.
        // MDD-BUG-FLUXHOMEARPA-00013.
        self.gdt_base = 0;
        self.gdt_size = 0;
        self.ldt_base = 0;
        self.ldt_size = 0;
        self.idt_base = 0;
        self.idt_size = 0;
        self.host_control_base = 0;
        self.host_control_size = 0;
    }

    /// Reset DPMI state and remove any physical mappings from memory.
    /// Use this when a program terminates to avoid leaking mappings.
    pub fn reset_with_memory(&mut self, memory: &mut Memory) {
        for mapping in &self.physical_mappings {
            memory.remove_physical_mapping(mapping.linear);
        }
        self.reset();
    }

    /// Get page locking statistics for debugging
    pub fn page_lock_stats(&self) -> &PageLockStats {
        &self.page_lock_stats
    }

    /// Iterate over memory blocks that have capacity > size (can be grown).
    /// Returns (address, size, capacity) tuples.
    pub fn memory_blocks_with_capacity(&self) -> impl Iterator<Item = (u32, u32, u32)> + '_ {
        self.memory_blocks
            .iter()
            .filter(|b| b.capacity > b.size)
            .map(|b| (b.address, b.size, b.capacity))
    }

    /// Grow a memory block at the given address to its full capacity.
    pub fn grow_memory_block_to_capacity(&mut self, address: u32) {
        if let Some(block) = self.memory_blocks.iter_mut().find(|b| b.address == address) {
            block.size = block.capacity;
        }
    }

    /// Update all DPMI selectors that have the given base address to a new limit.
    /// Also writes the updated descriptors to the LDT.
    pub fn update_selectors_with_base(
        &mut self,
        cpu: &mut gp64_cpu::Cpu,
        memory: &mut gp64_memory::Memory,
        base: u32,
        new_limit: u32,
    ) {
        // Collect selectors that need updating
        let to_update: Vec<u16> = self
            .selectors
            .iter()
            .filter(|s| s.base == base)
            .map(|s| s.selector)
            .collect();

        for sel in to_update {
            if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == sel) {
                s.limit = new_limit;
                let (b, l, a, f) = (s.base, s.limit, s.access, s.flags);
                self.write_selector_descriptor(cpu, memory, sel, b, l, a, f);
                cpu.invalidate_segment_cache(memory, sel);
            }
        }
    }

    /// Take pending HLE simulation request (returns and clears)
    pub fn take_pending_hle_simulation(&mut self) -> Option<PendingHleSimulation> {
        self.pending_hle_simulation.take()
    }

    /// Check if there's a pending HLE simulation
    pub fn has_pending_hle_simulation(&self) -> bool {
        self.pending_hle_simulation.is_some()
    }

    /// Check if DPMI is available (INT 2Fh, AX=1687h)
    ///
    /// Returns DPMI entry point information. The caller is responsible for
    /// suppressing this call when a DOS extender is detected (so the extender
    /// falls through to XMS/raw mode).
    pub fn int2f_dpmi_detect(&self, cpu: &mut Cpu) {
        let ax = cpu.regs.ax();

        if ax == 0x1687 {
            // DPMI detection - return available
            cpu.regs.set_ax(0); // DPMI available
            cpu.regs.set_bx(0x0003); // 16-bit + 32-bit supported
            cpu.regs.set_cl(0x03); // Processor type (386+)
            cpu.regs.set_dx(0x005A); // DPMI version 0.90
            cpu.regs.set_si(0); // Paragraphs needed for private data
                                // ES:DI = entry point for protected mode switch
            cpu.return_data_seg_for_hle(None::<&Memory>, gp64_cpu::DataSegReg::ES, 0xF000, false);
            cpu.regs.set_di(0xFF10); // Points to our DPMI entry stub
        }
    }

    /// Handle INT 2Fh AX=168Ah — Get Vendor-Specific API Entry Point.
    ///
    /// DOS API extensions expose supplementary protected-mode host services.
    ///
    /// On entry: `DS:SI -> ASCIIZ vendor name`. Per spec the call is
    /// **protected-mode only** — caller checks `cpu.is_protected_mode()`
    /// before invoking us. Comparison is case-sensitive per spec.
    ///
    /// Currently we recognise vendor name `"MS-DOS"` only. On match we return
    /// `AL = 0` and `ES:DI = (PM_BIOS_CODE_SEL, HLE_MSDOS_EXT_ENTRY)` — a
    /// far-call entry point dispatched in `src/emulator/hle_dispatch.rs`.
    /// On any other vendor name we leave AX unchanged (spec failure signal:
    /// AL stays 0x8A).
    ///
    /// Returns `true` if the call was recognised, `false` otherwise (so the
    /// caller can chain to other handlers if needed).
    pub fn int2f_msdos_ext_lookup(&self, cpu: &mut Cpu, memory: &Memory) -> bool {
        // Resolve DS:SI to a linear address via the segment descriptor.
        // In PM the segment cache should hold the descriptor base; fall back
        // to read_descriptor for safety.
        let ds = cpu.regs.ds;
        let si = cpu.regs.si() as u32;
        let desc = if let Some(desc) = cpu.read_descriptor(memory, ds) {
            desc
        } else {
            // Defensive: a malformed selector — treat as failure (AX unchanged).
            tracing::debug!(
                target: "gp64::dos::dpmi",
                "INT 2Fh AX=168A: invalid DS selector {:04X} — treating as failure",
                ds
            );
            return false;
        };
        // The name is read through DS like any guest access: the segment
        // must be present and readable, and every byte up to the terminator
        // must lie inside its limit. A name that runs off the segment is not
        // a name (MDD-BUG-KIL-00772).
        if !desc.present || (desc.is_code && !desc.is_readable) {
            tracing::debug!(
                target: "gp64::dos::dpmi",
                "INT 2Fh AX=168A: DS selector {:04X} is not readable — treating as failure",
                ds
            );
            return false;
        }
        let limit = desc.effective_limit();
        let (low, high) = if !desc.is_code && desc.access & 0x04 != 0 {
            // Expand-down data: valid offsets lie above the limit.
            (
                limit.wrapping_add(1),
                if desc.big { u32::MAX } else { 0xFFFF },
            )
        } else {
            (0, limit)
        };
        let readable = if si < low || si > high {
            0
        } else {
            (high - si) as usize + 1
        };

        // Read up to 32 bytes; stop at NUL or any non-printable byte.
        let mut name = [0u8; 32];
        let mut len = 0usize;
        let mut terminated = false;
        for slot in name.iter_mut() {
            if len >= readable {
                break;
            }
            let b = memory.read_u8(desc.base.wrapping_add(si + len as u32) as usize);
            // Limit to ASCII printable to avoid mis-matching on garbage memory.
            if !(0x20..=0x7E).contains(&b) {
                terminated = true;
                break;
            }
            *slot = b;
            len += 1;
        }
        let vendor = if terminated { &name[..len] } else { &[][..] };

        match vendor {
            b"MS-DOS" => {
                // Success: AL=0, ES:DI = vendor API entry point.
                cpu.regs.set_al(0x00);
                cpu.return_data_seg_for_hle(
                    Some(memory),
                    gp64_cpu::DataSegReg::ES,
                    PM_BIOS_CODE_SEL,
                    true,
                );
                cpu.regs.set_di(HLE_MSDOS_EXT_ENTRY);
                tracing::debug!(
                    target: "gp64::dos::dpmi",
                    "INT 2Fh AX=168A: vendor \"MS-DOS\" recognised, returning entry {:04X}:{:04X}",
                    PM_BIOS_CODE_SEL,
                    HLE_MSDOS_EXT_ENTRY
                );
                true
            }
            _ => {
                // Unknown vendor — leave AX unchanged (AL stays 0x8A failure signal).
                tracing::debug!(
                    target: "gp64::dos::dpmi",
                    "INT 2Fh AX=168A: vendor {:?} not supported — failure (AX unchanged)",
                    String::from_utf8_lossy(vendor),
                );
                false
            }
        }
    }

    /// Handle a far-call to the MS-DOS API Extensions entry point.
    /// Called from `src/emulator/hle_dispatch.rs` when the guest does
    /// CALL FAR to (PM_BIOS_CODE_SEL, HLE_MSDOS_EXT_ENTRY).
    ///
    /// On entry the function code is in AX. We dispatch and update CPU
    /// registers in place. The caller is responsible for the RETF (or RETFD
    /// for 32-bit clients).
    ///
    /// Implemented functions:
    /// - `AX=0x0000` Get Version → AX=0x0100 (1.0), CF=0
    /// - `AX=0x0100` Get Selector to Base of LDT → AX=selector, CF=0
    ///
    /// Unimplemented function codes set CF=1 and log at WARN with the AX
    /// value, so the next missing function surfaces immediately when running
    /// continuation plan" — branch A/B/C decision.)
    pub fn handle_msdos_ext_call(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let func = cpu.regs.ax();
        match func {
            // Function 0000h: Get *MS-DOS API Extensions* version — NOT the DPMI
            // host version. The DPMI host version is 0.90 (reported at INT 31h
            // AX=0400h and INT 2Fh AX=1687h, matching every real host). The MS-DOS
            // API Extensions are a separate Microsoft vendor API (Lipe spec),
            // reached via INT 2Fh AX=168Ah, with their own version namespace; that
            // version is 1.0. Returns AX = (major<<8)|minor.
            // (0.90 != 1.0 here is correct — see
            // wrk_docs/2026.05.29 - HLD - DPMI - version reporting consistency.md.)
            0x0000 => {
                cpu.regs.set_ax(0x0100);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                tracing::debug!(
                    target: "gp64::dos::dpmi",
                    "MS-DOS ext Func 0000 (Get Version) -> AX=0100"
                );
            }

            // Function 0100h: Get Selector to Base of LDT.
            // Returns AX = a selector whose base = LDT linear address and
            // limit = LDT byte length. The client uses this to read/write
            // LDT entries directly. We allocate one LDT entry on first call
            // and cache it.
            0x0100 => {
                let ldt_base = cpu.regs.ldt_base;
                let ldt_limit = cpu.regs.ldt_limit;
                if ldt_base == 0 {
                    // Should never happen — caller is in PM, LDT must be set up.
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    tracing::warn!(
                        target: "gp64::dos::dpmi",
                        "MS-DOS ext Func 0100 (Get LDT Selector): ldt_base=0 — failing"
                    );
                    return;
                }

                let sel = if self.msdos_ext_ldt_selector != 0 {
                    self.msdos_ext_ldt_selector
                } else {
                    let new_sel = self.allocate_descriptors(cpu, memory, 1);
                    if new_sel == 0 {
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        tracing::warn!(
                            target: "gp64::dos::dpmi",
                            "MS-DOS ext Func 0100 (Get LDT Selector): allocate_descriptors failed"
                        );
                        return;
                    }
                    // Configure as a read-write data selector pointing at the LDT.
                    self.set_descriptor_base(cpu, memory, new_sel, ldt_base);
                    self.set_descriptor_limit(cpu, memory, new_sel, ldt_limit);
                    self.msdos_ext_ldt_selector = new_sel;
                    new_sel
                };

                cpu.regs.set_ax(sel);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                tracing::debug!(
                    target: "gp64::dos::dpmi",
                    "MS-DOS ext Func 0100 (Get LDT Selector) -> AX={:04X} (base={:08X} limit={:08X})",
                    sel, ldt_base, ldt_limit
                );
            }

            // Unimplemented function — set CF and log at WARN so the next
            _ => {
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                tracing::warn!(
                    target: "gp64::dos::dpmi",
                    "MS-DOS ext: unimplemented function AX={:04X} CS:IP={:04X}:{:04X} \
                     BX={:04X} CX={:04X} DX={:04X} — returning CF=1. \
                     If this blocks a game, see HLD Phase-2 continuation plan.",
                    func,
                    cpu.regs.cs, cpu.regs.ip(),
                    cpu.regs.bx(), cpu.regs.cx(), cpu.regs.dx()
                );
            }
        }
    }

    /// Handle INT 31h (DPMI services)
    pub fn int31(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        self.int31_with_dosmem(cpu, memory, None, 0)
    }

    pub fn int31_with_dosmem(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        dos_mem: Option<&mut DosMem>,
        owner_psp: u16,
    ) -> Result<()> {
        let ret_trace_enabled = dpmi_ret_trace_env_enabled();
        let ax = cpu.regs.ax();
        let trace_context = ret_trace_enabled.then(|| {
            let seq = next_dpmi_ret_trace_seq();
            let entry_regs = DpmiTraceRegs::capture(cpu);
            let frame = DpmiTraceFrame::capture(self, cpu, memory);
            let pre_detail = self.dpmi_ret_trace_pre_detail(ax, entry_regs, cpu, memory);
            (seq, entry_regs, frame, pre_detail)
        });

        let result = self.int31_with_dosmem_inner(cpu, memory, dos_mem, owner_psp);

        if let Some((seq, entry_regs, frame, pre_detail)) = trace_context {
            let exit_regs = DpmiTraceRegs::capture(cpu);
            let post_detail =
                self.dpmi_ret_trace_post_detail(ax, entry_regs, exit_regs, cpu, memory);
            let host_error = result.as_ref().err().map(|err| err.to_string());
            self.emit_int31_ret_trace(
                seq,
                ax,
                owner_psp,
                entry_regs,
                exit_regs,
                frame,
                &pre_detail,
                &post_detail,
                host_error.as_deref(),
            );
        }

        result
    }

    fn int31_with_dosmem_inner(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        dos_mem: Option<&mut DosMem>,
        owner_psp: u16,
    ) -> Result<()> {
        // Debug counter for DPMI call tracing
        let ax = cpu.regs.ax();
        let func = (ax >> 8) as u8;

        if dpmi_trace_env_enabled() {
            dpmi_trace_emit(&format!(
                "[DPMI] INT31 AX={:04X} BX={:04X} CX={:04X} DX={:04X} SI={:04X} DI={:04X} DS={:04X} ES={:04X} SS={:04X} CS:EIP={:04X}:{:08X}",
                ax,
                cpu.regs.bx(),
                cpu.regs.cx(),
                cpu.regs.dx(),
                cpu.regs.si(),
                cpu.regs.di(),
                cpu.regs.ds,
                cpu.regs.es,
                cpu.regs.ss,
                cpu.regs.cs,
                cpu.regs.eip
            ));
        }

        tracing::debug!(
            ax = format_args!("{:04X}", ax),
            bx = format_args!("{:04X}", cpu.regs.bx()),
            cx = format_args!("{:04X}", cpu.regs.cx()),
            dx = format_args!("{:04X}", cpu.regs.dx()),
            si = format_args!("{:04X}", cpu.regs.si()),
            di = format_args!("{:04X}", cpu.regs.di()),
            "DPMI INT 31h"
        );
        if func == 0x03 {
            tracing::debug!(
                target: "gp64::dos::dpmi",
                ax = format_args!("{:04X}", ax),
                bx = format_args!("{:04X}", cpu.regs.bx()),
                cx = format_args!("{:04X}", cpu.regs.cx()),
                dx = format_args!("{:04X}", cpu.regs.dx()),
                es = format_args!("{:04X}", cpu.regs.es),
                edi = format_args!("{:08X}", cpu.regs.edi),
                cs = format_args!("{:04X}", cpu.regs.cs),
                ip = format_args!("{:04X}", cpu.regs.ip()),
                cpu_mode = self.cpu_mode,
                "DPMI translation services entry"
            );
        }
        // Trace IDT entry 31 on each call
        if cpu.regs.idtr.base != 0 {
            let (sel31, off31) = self.read_idt_gate(memory, cpu.regs.idtr.base as usize, 0x31);
            if off31 != 0xFE62 {
                tracing::debug!(
                    sel = format_args!("{:04X}", sel31),
                    off = format_args!("{:08X}", off31),
                    "DPMI IDT[31] changed"
                );
            }
        }

        match func {
            // Descriptor management
            0x00 => self.descriptor_services(cpu, memory)?,

            // DOS memory services
            0x01 => self.dos_memory_services(cpu, memory, dos_mem, owner_psp)?,

            // Interrupt services
            0x02 => self.interrupt_services(cpu, memory)?,

            // Translation services
            0x03 => self.translation_services(cpu, memory)?,

            // Get DPMI version and capabilities
            0x04 => {
                let subfunc = cpu.regs.al();
                match subfunc {
                    // Get version
                    0x00 => {
                        cpu.regs.set_ax(0x005A); // Version 0.90
                        cpu.regs.set_bx(0x0003); // Flags (16-bit + 32-bit supported)
                        cpu.regs.set_cl(0x03); // Processor type (386)
                                               // DH = master PIC base vector (0x08), DL = slave PIC base vector (0x70)
                        cpu.regs.set_dx(0x0870);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // Get DPMI capabilities (DPMI 1.0)
                    // ES:DI = pointer to 128-byte buffer for host info
                    0x01 => {
                        // Validate buffer write access before responding
                        let Some(_) = self.client_pointer_with_access(
                            cpu,
                            memory,
                            cpu.regs.es,
                            self.client_offset(cpu),
                            PageAccess::Write,
                        ) else {
                            return Ok(());
                        };
                        // DPMI 1.0 only. As a 0.9 host, report unsupported.
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Memory management
            0x05 => self.memory_services(cpu, memory)?,

            // Page locking and paging functions
            0x06 => {
                let subfunc = cpu.regs.al();
                match subfunc {
                    // Lock linear region
                    0x00 => {
                        // BX:CX = linear address, SI:DI = size
                        // No-op in emulator (no real paging), but track statistics
                        let linear = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
                        let size = ((cpu.regs.si() as u64) << 16) | (cpu.regs.di() as u64);
                        if let Err(code) = self.validate_linear_region(memory, linear, size as u32)
                        {
                            cpu.regs.set_ax(code);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                        self.page_lock_stats.lock_calls += 1;
                        self.page_lock_stats.total_bytes_locked += size;
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // Unlock linear region
                    0x01 => {
                        // No-op in emulator, but track statistics
                        let linear = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
                        let size = ((cpu.regs.si() as u64) << 16) | (cpu.regs.di() as u64);
                        if let Err(code) = self.validate_linear_region(memory, linear, size as u32)
                        {
                            cpu.regs.set_ax(code);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                        self.page_lock_stats.unlock_calls += 1;
                        self.page_lock_stats.total_bytes_unlocked += size;
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // Mark real mode region as pageable
                    0x02 => {
                        // No-op in emulator, but track statistics
                        let linear = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
                        let size = ((cpu.regs.si() as u64) << 16) | (cpu.regs.di() as u64);
                        if let Err(code) = self.validate_linear_region(memory, linear, size as u32)
                        {
                            cpu.regs.set_ax(code);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                        self.page_lock_stats.pageable_calls += 1;
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // Relock real mode region
                    0x03 => {
                        // No-op in emulator, but track statistics
                        let linear = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
                        let size = ((cpu.regs.si() as u64) << 16) | (cpu.regs.di() as u64);
                        if let Err(code) = self.validate_linear_region(memory, linear, size as u32)
                        {
                            cpu.regs.set_ax(code);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                        self.page_lock_stats.relock_calls += 1;
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // Get page size
                    0x04 => {
                        // Returns BX:CX = page size in bytes
                        cpu.regs.set_bx(0); // High word = 0
                        cpu.regs.set_cx(0x1000); // Low word = 4096
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Demand paging functions
            0x07 => {
                let subfunc = cpu.regs.al();
                match subfunc {
                    // DPMI-137: Mark pages as demand paging candidates (0x0700, region form)
                    0x00 => {
                        // No-op in emulator (no real paging)
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // DPMI-137: Discard pages (0x0701, region form)
                    0x01 => {
                        // No-op in emulator (no real paging)
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // Mark page as demand paging candidate
                    0x02 => {
                        // No-op in emulator
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // Discard page contents
                    0x03 => {
                        // No-op in emulator
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Physical address mapping
            0x08 => {
                let subfunc = ax & 0xFF;
                match subfunc {
                    0x00 => {
                        // Map physical address
                        // BX:CX = physical address, SI:DI = size
                        // Return BX:CX = linear address
                        let physical = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
                        let size = ((cpu.regs.si() as u32) << 16) | (cpu.regs.di() as u32);
                        if size == 0 {
                            cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                        let page_size = 0x1000u32;
                        if (physical & (page_size - 1)) != 0 || (size & (page_size - 1)) != 0 {
                            cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                        let phys_end = physical.saturating_add(size);
                        let is_mmio = memory.is_mmio_address(physical as usize);
                        // The aperture, not "decoding right now": a VBE client
                        // may map PhysBasePtr before 4F02h enables the
                        // decoder (MDD-BUG-CRU-01488).
                        let is_lfb = memory.is_lfb_aperture_address(physical as usize);
                        if phys_end > DOS_RAM_CEILING as u32 && !is_mmio && !is_lfb {
                            cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                        // B19: Check for overlapping physical address mappings
                        if self.physical_mappings_overlap(physical, size) {
                            cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                        let Some(linear) = self.find_free_linear_range(size, page_size) else {
                            cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        };
                        let end = linear.saturating_add(size);
                        // The physical-mapping table is fixed-capacity (FixedVec<_, 64>);
                        // refuse a new mapping when full instead of panicking in
                        // FixedVec::push. MDD-BUG-CRUCIBLE-00057.
                        if self.physical_mappings.is_full() {
                            cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                        // Memory also contains system/LFB mappings. Secure its
                        // shared slot before committing any DPMI-side state.
                        if !memory.add_physical_mapping(linear, physical, size) {
                            cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                        // Only expand RAM for regions that map to actual RAM.
                        // MMIO buffers (e.g. 3dfx LFB at 0x60000000) and
                        // VBE LFB (at 0xE0000000) own their data directly.
                        if !is_mmio && !is_lfb {
                            memory.ensure_ram_size(end as usize);
                        }
                        self.physical_mappings.push(PhysicalMapping {
                            physical,
                            linear,
                            size,
                        });
                        cpu.regs.set_bx((linear >> 16) as u16);
                        cpu.regs.set_cx(linear as u16);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    0x01 => {
                        // Free physical mapping
                        let linear = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
                        if let Some(pos) = self
                            .physical_mappings
                            .iter()
                            .position(|m| m.linear == linear)
                        {
                            self.physical_mappings.remove(pos);
                            memory.remove_physical_mapping(linear);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                    _ => {
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Virtual interrupt state
            0x09 => {
                let subfunc = ax & 0xFF;
                match subfunc {
                    0x00 => {
                        // Get and disable virtual interrupt state
                        cpu.regs.set_al(if self.virtual_interrupts_enabled {
                            1
                        } else {
                            0
                        });
                        self.virtual_interrupts_enabled = false;
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    0x01 => {
                        // Get and enable virtual interrupt state
                        cpu.regs.set_al(if self.virtual_interrupts_enabled {
                            1
                        } else {
                            0
                        });
                        self.virtual_interrupts_enabled = true;
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    0x02 => {
                        // Get virtual interrupt state
                        cpu.regs.set_al(if self.virtual_interrupts_enabled {
                            1
                        } else {
                            0
                        });
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Vendor-specific API entry point
            0x0A => {
                // DS:ESI = null-terminated vendor string
                // We don't support any vendor APIs
                cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }

            // Debug watchpoints
            0x0B => {
                let subfunc = cpu.regs.al();
                match subfunc {
                    // Set debug watchpoint
                    0x00 => {
                        let linear_addr = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
                        let size_raw = cpu.regs.dl();
                        let watch_type_raw = cpu.regs.dh();

                        // Parse and validate size (DPMI spec: 1=byte, 2=word, 4=dword)
                        let size = match WatchpointSize::from_raw(size_raw) {
                            Some(s) => s,
                            None => {
                                cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                return Ok(());
                            }
                        };

                        // Parse and validate watch_type (0=exec, 1=write, 2=read/write)
                        let watch_type = match WatchpointType::from_raw(watch_type_raw) {
                            Some(t) => t,
                            None => {
                                cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                return Ok(());
                            }
                        };

                        // Find a free watchpoint slot
                        if let Some(slot) = self.debug_watchpoints.iter().position(|w| !w.active) {
                            self.debug_watchpoints[slot] = DebugWatchpoint {
                                active: true,
                                linear_address: linear_addr,
                                size,
                                watch_type,
                                triggered: false,
                            };
                            cpu.regs.set_bx(slot as u16); // Return handle
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            // No free slots
                            cpu.regs.set_ax(DPMI_ERR_HANDLE_UNAVAILABLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                    // Clear debug watchpoint
                    0x01 => {
                        let handle = cpu.regs.bx() as usize;
                        if handle < 4 && self.debug_watchpoints[handle].active {
                            self.debug_watchpoints[handle] = DebugWatchpoint::default();
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                    // Get debug watchpoint state
                    0x02 => {
                        let handle = cpu.regs.bx() as usize;
                        if handle < 4 && self.debug_watchpoints[handle].active {
                            // AX bit 0 = watchpoint triggered
                            let triggered = if self.debug_watchpoints[handle].triggered {
                                1
                            } else {
                                0
                            };
                            cpu.regs.set_ax(triggered);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                    // Reset debug watchpoint state
                    0x03 => {
                        let handle = cpu.regs.bx() as usize;
                        if handle < 4 && self.debug_watchpoints[handle].active {
                            self.debug_watchpoints[handle].triggered = false;
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                    _ => {
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Coprocessor status
            0x0E => {
                let subfunc = cpu.regs.al();
                match subfunc {
                    // Get coprocessor status
                    0x00 => {
                        // AX = coprocessor status:
                        //   Bit 0: Coprocessor enabled for client
                        //   Bit 1: Emulation enabled (0 = not emulated)
                        //   Bit 2: Coprocessor present
                        //   Bit 3: 80487 SX
                        //   Bits 4-5: Coprocessor type (0=none, 2=287, 3=387, 4=487)
                        // We always report FPU present and enabled, 387 type, no emulation
                        // 0x35 = 0b00110101:
                        //   bit 0 = 1 (enabled)
                        //   bit 1 = 0 (no emulation)
                        //   bit 2 = 1 (present)
                        //   bit 3 = 0 (not 487SX)
                        //   bits 4-5 = 11 = 3 (387 type)
                        cpu.regs.set_ax(0x0035);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // Set coprocessor emulation
                    0x01 => {
                        // BX bit 0: enable emulation
                        // BX bit 1: set coprocessor present
                        // We ignore this - FPU state is always emulated
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Shared memory services (DPMI 1.0)
            0x0D => {
                let subfunc = cpu.regs.al();
                match subfunc {
                    // Allocate Shared Memory
                    0x00 => {
                        // SI:DI = shared memory handle/name
                        // ES:BX = requested size
                        // For now, return unsupported (DPMI 1.0 feature rarely used)
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                    // Free Shared Memory
                    0x01 => {
                        // SI:DI = shared memory handle
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                    // Serialize on Shared Memory
                    0x02 => {
                        // SI:DI = shared memory handle
                        // AX bit 0 = 0 for exclusive, 1 for shared access
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            _ => {
                tracing::warn!("Unhandled DPMI INT 31h AX={ax:04X}");
                cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED); // Unsupported function
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_int31_ret_trace(
        &self,
        seq: u64,
        ax: u16,
        owner_psp: u16,
        entry_regs: DpmiTraceRegs,
        exit_regs: DpmiTraceRegs,
        frame: DpmiTraceFrame,
        pre_detail: &str,
        post_detail: &str,
        host_error: Option<&str>,
    ) {
        use std::fmt::Write as _;

        let cf = (exit_regs.flags & gp64_cpu::Flags::CARRY.bits() as u32) != 0;
        let entry_cf = (entry_regs.flags & gp64_cpu::Flags::CARRY.bits() as u32) != 0;
        let error = if cf {
            dpmi_error_name(exit_regs.ax())
        } else {
            "none"
        };
        let mut line = String::new();
        let _ = write!(
            line,
            "[DPMI_RET] seq={} psp={:04X} pm_entry={} pm_return={} mode={} func={:04X} name={} \
             caller={:04X}:{:08X} return={:04X}:{:08X} return_flags={:08X} frame32={} \
             frame_stack={:04X}:{:08X} hle_entry={:04X}:{:08X} entry_cf={} cf={} err={} {} {}",
            seq,
            owner_psp,
            (entry_regs.cr0 & 1) != 0,
            self.protected_mode,
            if self.cpu_mode == 1 { 32 } else { 16 },
            ax,
            dpmi_function_name(ax),
            frame.ret_cs,
            frame.caller_eip(),
            frame.ret_cs,
            frame.ret_eip,
            frame.ret_flags,
            frame.is_32bit,
            frame.ss,
            frame.esp,
            entry_regs.cs,
            entry_regs.eip,
            entry_cf,
            cf,
            error,
            entry_regs.format_fields("pre"),
            exit_regs.format_fields("post")
        );
        if !pre_detail.is_empty() {
            line.push(' ');
            line.push_str(pre_detail);
        }
        if !post_detail.is_empty() {
            line.push(' ');
            line.push_str(post_detail);
        }
        if let Some(host_error) = host_error {
            let sanitized = host_error.replace(['\r', '\n'], " ");
            let _ = write!(line, " host_error=\"{}\"", sanitized.replace('"', "'"));
        }

        dpmi_ret_trace_emit(&line);
    }

    fn dpmi_ret_trace_pre_detail(
        &self,
        ax: u16,
        entry_regs: DpmiTraceRegs,
        cpu: &Cpu,
        memory: &mut Memory,
    ) -> String {
        match ax {
            0x0300..=0x0302 => {
                let offset = entry_regs.offset_di(self.client_offset_is_32bit(cpu));
                match self.trace_client_pointer(cpu, memory, entry_regs.es, offset) {
                    Some(addr) => {
                        let rmcs = RealModeCallStruct::read_from_memory(memory, addr);
                        format!(
                            "rmcs_addr={:08X} {}",
                            addr,
                            format_rmcs_state("rmcs_before", rmcs)
                        )
                    }
                    None => format!("rmcs_addr=unresolved({:04X}:{:08X})", entry_regs.es, offset),
                }
            }
            _ => String::new(),
        }
    }

    fn dpmi_ret_trace_post_detail(
        &self,
        ax: u16,
        entry_regs: DpmiTraceRegs,
        exit_regs: DpmiTraceRegs,
        cpu: &Cpu,
        memory: &mut Memory,
    ) -> String {
        let cf = (exit_regs.flags & gp64_cpu::Flags::CARRY.bits() as u32) != 0;
        match ax {
            0x0202 | 0x0212 if !cf => format!(
                "handler={:04X}:{:08X}",
                exit_regs.cx(),
                exit_regs.offset_dx(self.client_offset_is_32bit(cpu))
            ),
            0x0204 if !cf => format!(
                "pm_vector={:04X}:{:08X}",
                exit_regs.cx(),
                exit_regs.offset_dx(self.client_offset_is_32bit(cpu))
            ),
            0x000B if !cf => self.trace_output_buffer(
                "descriptor",
                cpu,
                memory,
                entry_regs.es,
                entry_regs.offset_di(self.client_offset_is_32bit(cpu)),
                8,
            ),
            0x0500 if !cf => self.trace_output_buffer(
                "meminfo48",
                cpu,
                memory,
                entry_regs.es,
                entry_regs.offset_di(self.client_offset_is_32bit(cpu)),
                48,
            ),
            0x0501 if !cf => format!(
                "linear_base={:08X} handle={:08X}",
                ((exit_regs.bx() as u32) << 16) | exit_regs.cx() as u32,
                self.read_handle_from_trace(exit_regs)
            ),
            0x0503 if !cf => format!(
                "linear_base={:08X} handle={:08X}",
                ((exit_regs.bx() as u32) << 16) | exit_regs.cx() as u32,
                self.read_handle_from_trace(exit_regs)
            ),
            0x050B if !cf => self.trace_output_buffer(
                "meminfo128",
                cpu,
                memory,
                entry_regs.es,
                entry_regs.offset_di(self.client_offset_is_32bit(cpu)),
                128,
            ),
            0x0300..=0x0302 => {
                let offset = entry_regs.offset_di(self.client_offset_is_32bit(cpu));
                let status = if self.v86_active {
                    "v86_active"
                } else if self.pending_hle_simulation.is_some() {
                    "hle_pending"
                } else {
                    "complete"
                };
                match self.trace_client_pointer(cpu, memory, entry_regs.es, offset) {
                    Some(addr) => {
                        let rmcs = RealModeCallStruct::read_from_memory(memory, addr);
                        format!(
                            "rmcs_status={} rmcs_addr={:08X} {}",
                            status,
                            addr,
                            format_rmcs_state("rmcs_after", rmcs)
                        )
                    }
                    None => format!(
                        "rmcs_status={} rmcs_addr=unresolved({:04X}:{:08X})",
                        status, entry_regs.es, offset
                    ),
                }
            }
            0x0303 if !cf => {
                let seg = exit_regs.cx();
                let off = exit_regs.dx();
                let slot = self
                    .rm_callbacks
                    .iter()
                    .position(|cb| cb.in_use && cb.rm_segment == seg && cb.rm_offset == off)
                    .map(|idx| idx.to_string())
                    .unwrap_or_else(|| "not_found".to_string());
                format!(
                    "callback_alloc={:04X}:{:04X} callback_slot={}",
                    seg, off, slot
                )
            }
            0x0304 => {
                let seg = entry_regs.cx();
                let off = entry_regs.dx();
                let still_active = self
                    .rm_callbacks
                    .iter()
                    .any(|cb| cb.in_use && cb.rm_segment == seg && cb.rm_offset == off);
                format!(
                    "callback_free={:04X}:{:04X} callback_still_active={}",
                    seg, off, still_active
                )
            }
            _ => String::new(),
        }
    }

    fn trace_client_pointer(
        &self,
        cpu: &Cpu,
        memory: &mut Memory,
        segment: u16,
        offset: u32,
    ) -> Option<usize> {
        self.get_linear_address_with_access(cpu, memory, segment, offset, PageAccess::Read)
            .ok()
    }

    fn trace_output_buffer(
        &self,
        label: &str,
        cpu: &Cpu,
        memory: &mut Memory,
        segment: u16,
        offset: u32,
        len: usize,
    ) -> String {
        match self.trace_client_pointer(cpu, memory, segment, offset) {
            Some(addr) => format!(
                "{}_addr={:08X} {}_bytes={} {}_dwords={}",
                label,
                addr,
                label,
                format_trace_bytes(memory, addr, len),
                label,
                format_trace_dwords(memory, addr, len)
            ),
            None => format!("{label}_addr=unresolved({segment:04X}:{offset:08X})"),
        }
    }

    fn read_handle_from_trace(&self, regs: DpmiTraceRegs) -> u32 {
        if self.cpu_mode == 1 {
            regs.esi
        } else {
            ((regs.si() as u32) << 16) | (regs.di() as u32)
        }
    }

    pub fn linear_address_for_selector(
        &self,
        cpu: &Cpu,
        memory: &mut Memory,
        selector: u16,
        offset: u32,
    ) -> std::result::Result<usize, u16> {
        self.get_linear_address(cpu, memory, selector, offset)
    }

    /// Get linear address from segment:offset (handles protected mode selectors)
    fn get_linear_address(
        &self,
        cpu: &Cpu,
        memory: &mut Memory,
        segment: u16,
        offset: u32,
    ) -> std::result::Result<usize, u16> {
        self.get_linear_address_with_access(cpu, memory, segment, offset, PageAccess::Read)
    }

    fn get_linear_address_with_access(
        &self,
        cpu: &Cpu,
        memory: &mut Memory,
        segment: u16,
        offset: u32,
        access: PageAccess,
    ) -> std::result::Result<usize, u16> {
        if self.protected_mode && (segment & 0x04) != 0 {
            // LDT selector - look up in our selector table
            let canonical = (segment & !0x7) | 0x7;
            let sel = self
                .selectors
                .iter()
                .find(|s| s.selector == canonical)
                .ok_or(DPMI_ERR_INVALID_SELECTOR)?;
            if (sel.access & 0x80) == 0 {
                return Err(DPMI_ERR_INVALID_SELECTOR);
            }
            if offset > sel.limit {
                return Err(DPMI_ERR_INVALID_SELECTOR);
            }
            let linear = sel.base.wrapping_add(offset);
            return match cpu.translate_linear_checked(memory, linear, access) {
                Ok(addr) => Ok(addr),
                Err(_) => Err(DPMI_ERR_INVALID_SELECTOR),
            };
        }
        if self.protected_mode {
            return match cpu.translate_address_full_checked(memory, segment, offset, access) {
                Ok(addr) => Ok(addr),
                Err(_) => Err(DPMI_ERR_INVALID_SELECTOR),
            };
        }

        if (cpu.regs.cr0 & 0x8000_0000) != 0 {
            return Err(DPMI_ERR_INVALID_SELECTOR);
        }

        // Real mode - use segment*16
        Ok(cpu.translate_address_full(memory, segment, offset))
    }

    /// DPMI offset width follows the current code segment bitness.
    fn client_offset(&self, cpu: &Cpu) -> u32 {
        if self.client_offset_is_32bit(cpu) {
            cpu.regs.edi
        } else {
            cpu.regs.di() as u32
        }
    }

    /// Offset register width depends on current code segment bitness (DX vs EDX).
    fn client_offset_reg(&self, cpu: &Cpu) -> u32 {
        if self.client_offset_is_32bit(cpu) {
            cpu.regs.edx
        } else {
            cpu.regs.dx() as u32
        }
    }

    /// Set offset register based on current code segment bitness (DX vs EDX).
    fn set_client_offset_reg(&self, cpu: &mut Cpu, offset: u32) {
        if self.client_offset_is_32bit(cpu) {
            cpu.regs.edx = offset;
        } else {
            cpu.regs.set_dx(offset as u16);
        }
    }

    fn client_offset_is_32bit(&self, _cpu: &Cpu) -> bool {
        self.cpu_mode == 1
    }

    /// Resolve a client pointer (segment:offset) to a linear address.
    fn client_pointer(
        &self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        segment: u16,
        offset: u32,
    ) -> Option<usize> {
        self.client_pointer_with_access(cpu, memory, segment, offset, PageAccess::Read)
    }

    fn client_pointer_with_access(
        &self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        segment: u16,
        offset: u32,
        access: PageAccess,
    ) -> Option<usize> {
        match self.get_linear_address_with_access(cpu, memory, segment, offset, access) {
            Ok(addr) => Some(addr),
            Err(code) => {
                cpu.regs.set_ax(code);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                None
            }
        }
    }
}

impl Default for Dpmi {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;

// ===========================================================================
// LDT reservation: descriptor allocation must respect other owners.
//
// A 32-bit process hosting a 16-bit DLL runs DPMI's descriptor methods against
// the flat-thunk window's LDT, whose upper indices belong to the `MapLS` table
// and the hosted HLE code selectors. DPMI's cursor starts at index 32 and bumps
// with no ceiling of its own, so the two ranges meet. These pin that they
// cannot, through *every* path — not just the bump cursor, which is what an
// earlier revision of the design capped.
// ===========================================================================
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod ldt_reservation_tests;
