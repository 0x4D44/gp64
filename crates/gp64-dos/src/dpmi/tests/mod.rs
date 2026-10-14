mod bios_and_dos_extensions;
mod coverage_calls_and_descriptors;
mod dos_memory_and_rm_calls;
mod exception_handlers;
mod extender_regressions;
mod linear_allocation;
mod linear_memory;
mod memory_and_page_attributes;
mod pm_interrupts;
mod protected_mode_switches;
mod selector_aliases;
mod state_and_selectors;
mod vector_and_resize;
mod watchpoints_and_handlers;

use super::*;
use crate::memory::DEFAULT_FIRST_MCB_SEG;
use gp64_cpu::{Cpu, CpuError, SegmentCache};
use gp64_memory::Memory;
use gp64_types::test_util::env_lock;

fn setup_protected_mode(dpmi: &mut Dpmi, cpu: &mut Cpu, memory: &mut Memory, is_32bit: bool) {
    cpu.regs.ss = 0x1000;
    cpu.regs.set_sp(0xFFFE);
    let stack_base = 0x10000usize;
    memory.write_u16(stack_base + 0xFFFC, 0x0100); // Return IP
    memory.write_u16(stack_base + 0xFFFE, 0x2000); // Return CS
    cpu.regs.set_sp(0xFFFC);
    cpu.regs.set_ax(if is_32bit { 1 } else { 0 });
    cpu.regs.ds = 0x3000;
    cpu.regs.es = 0x4000;
    dpmi.enter_protected_mode(cpu, memory).unwrap();
}

fn setup_gdt_code_descriptor(
    dpmi: &Dpmi,
    cpu: &mut Cpu,
    memory: &mut Memory,
    selector: u16,
    limit: u32,
) {
    let gdt_base = 0x2000;
    let index = (selector >> 3) as usize;
    cpu.regs.gdtr.base = gdt_base;
    cpu.regs.gdtr.limit = ((index + 1) * 8 - 1) as u16;
    dpmi.write_descriptor_raw(memory, gdt_base as usize, index, 0, limit, 0xFA, 0x00);
}

fn init_dos_mem(memory: &mut Memory) -> DosMem {
    let mut dos_mem = DosMem::new(DEFAULT_FIRST_MCB_SEG);
    dos_mem.init(memory);
    dos_mem
}

fn init_tracing() -> tracing::subscriber::DefaultGuard {
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(std::io::sink)
        .finish();
    tracing::subscriber::set_default(sub)
}

// KILN-00086: test_dpmi_trace_env_enabled, test_dpmi_ret_trace_env_enabled, and
// test_dpmi_ret_trace_int31_emits_return_contract removed — they asserted the retired
// DPMI trace / return-trace env enables + file emission (both env enables are now
// permanently off; the DPMI trace is gated only by RUST_LOG).

// ============================================================================
// Stage 2 Tests: State/Mode Switch Functions
// ============================================================================

// ========== New comprehensive tests ==========

fn install_memory_only_physical_mapping(memory: &mut Memory) {
    memory.add_physical_mapping(0xFF00_0000, 0x03F0_0000, 0x1000);
    assert_eq!(memory.map_linear_physical(0xFF00_0000), Some(0x03F0_0000));
}

fn allocate_physical_mapping_test_block(
    dpmi: &mut Dpmi,
    cpu: &mut Cpu,
    memory: &mut Memory,
) -> (u32, u32) {
    dpmi.cpu_mode = 1;
    cpu.regs.set_ax(0x0504);
    cpu.regs.ecx = 64 * 0x1000;
    cpu.regs.ebx = 0;
    cpu.regs.edx = 0;
    dpmi.int31(cpu, memory).unwrap();
    assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
    (cpu.regs.esi, cpu.regs.ebx)
}

// === Descriptor Management Tests ===

// === DOS Memory Block Tests ===

// === Interrupt Vector Tests ===

// === Real Mode Callback Tests ===

// === Memory Management Tests ===

// === Virtual Interrupt State Tests ===

// === Edge Cases and Stress Tests ===

// =========================================================================
// Stage 4: Debug Watchpoint Tests
// =========================================================================

// ========== Phase 5: Exception Frame Verification Tests ==========

// Standard protected mode interrupt frame pushes:
// - EFLAGS/FLAGS
// - CS
// - EIP/IP
// - Error code (if exception has one)
//
// 1. vector < 32 (CPU exception)
// 2. target_cpl == 3 (ring 3 handler)
// 3. !is_default_bios_stub
// 4. Gate type is 0x06 (16-bit) or 0x0E (32-bit)

// === Page Locking Statistics Tests ===

// -------------------------------------------------------------------------
// Regression tests for fuzz-discovered descriptor flag bugs
// Crash: crash-282b03fa42e49d5b035d311ec96ddc53606e6a1c
// -------------------------------------------------------------------------

// =========================================================================
// DPMI 0.9 State Save/Restore Buffer Layout Tests (TDD - expect failures)
// =========================================================================
//
// DPMI 0.9 Correct State Buffer Layout for 32-bit clients:
// Offset  Size  Contents
// +0x00   4     EDI
// +0x04   4     ESI
// +0x08   4     EBP
// +0x0C   4     Reserved (must be 0)
// +0x10   4     EBX
// +0x14   4     EDX
// +0x18   4     ECX
// +0x1C   4     EAX
// +0x20   2     FLAGS (16-bit only!)
// +0x22   2     ES
// +0x24   2     DS
// +0x26   2     FS
// +0x28   2     GS
// +0x2A   4     EIP (32-bit for 32-bit clients)
// +0x2E   2     CS
// +0x30   4     ESP (32-bit for 32-bit clients)
// +0x34   2     SS
// Total: 0x36 = 54 bytes for 32-bit client
//
// These tests verify the CORRECT layout per DPMI 0.9 spec. They will FAIL
// until the implementation is fixed.

fn install_flat_es_selector(dpmi: &mut Dpmi, cpu: &mut Cpu, memory: &mut Memory) -> u16 {
    let es_selector = dpmi.allocate_descriptors(cpu, memory, 1);
    assert_ne!(
        es_selector, 0,
        "test setup: failed to allocate flat ES selector"
    );
    let entry = dpmi
        .selectors
        .iter_mut()
        .find(|s| s.selector == es_selector)
        .expect("allocated selector should be tracked");
    entry.base = 0;
    entry.limit = 0xFFFF_FFFF;
    entry.access = 0xF2;
    entry.flags = 0xC0;
    dpmi.write_selector_descriptor(cpu, memory, es_selector, 0, 0xFFFF_FFFF, 0xF2, 0xC0);
    cpu.load_data_segment(memory, gp64_cpu::DataSegReg::ES, es_selector);
    es_selector
}

// ========================================================================
// Coverage: Extended Exception Handler (0x13) Error Paths
// ========================================================================

// ========================================================================
// Coverage: Get Extended Exception Handler (0x12) with live IDTR
// ========================================================================

// ========================================================================
// Coverage: Set Exception Handler (0x03) Error Paths
// ========================================================================

// ========================================================================
// Coverage: Get Exception Handler (0x02) with IDT path
// ========================================================================

// test_get_exception_handler_0202_reads_idt_gate removed:
// Remote engineer changed GetExceptionHandler to always read from
// exception_handlers[] instead of IDT (see adc34222).

// ========================================================================
// Coverage: Get PM Interrupt Vector (0x0204) with IDT path
// ========================================================================

// ========================================================================
// Coverage: Various INT 31h Small Error Paths
// ========================================================================

// ========================================================================
// Coverage: INT 31h function 0x0604 - Get page size
// ========================================================================

// ========================================================================
// Coverage: INT 31h function 0x0A00 - Vendor API entry point
// ========================================================================

// ========================================================================
// Coverage: INT 31h function 0x0B - Debug watchpoint unsupported subfunc
// ========================================================================

// ========================================================================
// Coverage: INT 31h function 0x0E - Coprocessor status
// ========================================================================

// ========================================================================
// Coverage: INT 31h function 0x0D - Shared memory unsupported subfunc
// ========================================================================

// ========================================================================
// Coverage: Descriptor management error paths (0x0004-0x000C)
// ========================================================================

// ========================================================================
// Coverage: DOS Memory Multi-Selector Operations (lines 2079-2157)
// ========================================================================

// ========================================================================
// Coverage: Allocate specific LDT descriptor (0x0D) error paths
// ========================================================================

// ========================================================================
// Coverage: Unhandled descriptor subfunction
// ========================================================================

// ========================================================================
// Coverage: INT 31h trace path (lines 820-828)
// ========================================================================

// ========================================================================
// Coverage: Physical address mapping error paths (0x0800)
// ========================================================================

// ========================================================================
// Coverage: Demand paging page-level subfunctions (0x07)
// ========================================================================

// ========================================================================
// Coverage: 32-bit mode DOS memory (single selector for large blocks)
// ========================================================================

// ========================================================================
// Coverage: Debug watchpoint reset state (0x0B03)
// ========================================================================

// ========================================================================
// Coverage: Physical mapping size-unaligned (0x0800 with size not aligned)
// ========================================================================

// ========================================================================
// Coverage: Set exception handler 0x03 preserves IDT when available
// ========================================================================

// ========================================================================
// Coverage: DOS memory resize cannot extend selectors
// ========================================================================

// ========================================================================
// Coverage: save_rm_registers (lines 4386-4401)
// ========================================================================

// ========================================================================
// Coverage: pm_callback_depth / is_pm_callback_active / enter_pm_callback /
// leave_pm_callback (lines 4432-4439)
// ========================================================================

// ========================================================================
// Coverage: are_virtual_interrupts_enabled (lines 4444-4446)
// ========================================================================

// ========================================================================
// Coverage: 0x0504 allocation - find_free_linear_range returns None (line 4932-4934)
// Fill all memory so no free range is available
// ========================================================================

// ========================================================================
// Coverage: 0x0504 - handle exhaustion (lines 4956-4958)
// ========================================================================

// ========================================================================
// Coverage: 0x0504 with 16-bit client (now allowed for protected-mode clients compat)
// ========================================================================

// ========================================================================
// Coverage: 0x050A - 16-bit path (lines 5001-5005)
// ========================================================================

// ========================================================================
// Coverage: 0x050A - invalid handle in 16-bit mode
// ========================================================================

// ========================================================================
// Coverage: 0x0505 resize linear block - invalid handle (lines 5167-5170)
// ========================================================================

// ========================================================================
// Coverage: 0x0505 resize linear block - zero size (lines 5078-5082)
// ========================================================================

// ========================================================================
// Coverage: 0x0505 resize linear - no free range for move (lines 5160-5163)
// ========================================================================

// ========================================================================
// Coverage: 0x050B get memory info with allocations (lines 5030-5035)
// ========================================================================

// ========================================================================
// Coverage: 0x0501 alloc memory block - alloc_linear_block fails (lines 4813-4816)
// ========================================================================

// ========================================================================
// Coverage: 0x0506 get page attributes - 16-bit client with ECX=0 hits invalid value
// ========================================================================

// ========================================================================
// Coverage: 0x0506 get page attributes - zero page count (lines 5186-5190)
// ========================================================================

// ========================================================================
// Coverage: 0x0506 get page attributes - invalid handle (lines 5236-5238)
// ========================================================================

// ========================================================================
// Coverage: 0x0506 get page attributes - page range exceeds block (lines 5242-5245)
// ========================================================================

// Test removed: test_get_page_attributes_0506_buffer_overflow
// Setup was fundamentally wrong - used real-mode segment as ES in 32-bit protected mode,
// which doesn't work because client_pointer_with_access treats ES as an LDT selector.

// ========================================================================
// Coverage: 0x0507 set page attributes - 16-bit client with ECX=0 hits invalid value
// ========================================================================

// ========================================================================
// Coverage: 0x0507 set page attributes - zero count (lines 5284-5288)
// ========================================================================

// ========================================================================
// Coverage: 0x0507 set page attributes - unaligned offset (lines 5292-5296)
// ========================================================================

// ========================================================================
// Coverage: 0x0507 set page attributes - invalid handle (lines 5330-5332)
// ========================================================================

// ========================================================================
// Coverage: 0x0507 set page attributes - page range out of bounds (lines 5336-5341)
// ========================================================================

// ========================================================================
// Coverage: 0x0507 set page attributes - invalid attribute type > 1 (lines 5359-5362)
// ========================================================================

// Test removed: test_set_page_attributes_0507_buffer_overflow
// Setup was fundamentally wrong - used real-mode segment as ES in 32-bit protected mode,
// which doesn't work because client_pointer_with_access treats ES as an LDT selector.

// ========================================================================
// Coverage: 0x0508 map device - 16-bit client with ECX=0 hits invalid value
// ========================================================================

// ========================================================================
// Coverage: 0x0508 map device - zero page count (lines 5410-5413)
// ========================================================================

// ========================================================================
// Coverage: 0x0508 map device - unaligned offset (lines 5415-5418)
// ========================================================================

// ========================================================================
// Coverage: 0x0508 map device - unaligned physical (lines 5415-5418)
// ========================================================================

// ========================================================================
// Coverage: 0x0508 map device - invalid handle (lines 5434-5436)
// ========================================================================

// ========================================================================
// Coverage: 0x0508 map device - page range exceeds block (lines 5441-5444)
// ========================================================================

// ========================================================================
// Coverage: 0x0508 map device - physical beyond DOS_RAM_CEILING (lines 5456-5459)
// ========================================================================

// ========================================================================
// Coverage: 0x0508 map device - overlapping physical mapping (lines 5461-5464)
// ========================================================================

// ========================================================================
// Coverage: 0x0509 map conventional - 16-bit client with ECX=0 hits invalid value
// ========================================================================

// ========================================================================
// Coverage: 0x0509 map conventional - zero pages (lines 5496-5499)
// ========================================================================

// ========================================================================
// Coverage: 0x0509 map conventional - unaligned offset/physical (lines 5501-5504)
// ========================================================================

// ========================================================================
// Coverage: 0x0509 map conventional - invalid handle (lines 5520-5522)
// ========================================================================

// ========================================================================
// Coverage: 0x0509 map conventional - page range exceeds block (lines 5527-5530)
// ========================================================================

// ========================================================================
// Coverage: 0x0509 map conventional - phys beyond DOS_RAM_CEILING (lines 5542-5545)
// ========================================================================

// ========================================================================
// Coverage: 0x0509 map conventional - overlapping (lines 5547-5550)
// ========================================================================

// ========================================================================
// Coverage: memory_services unsupported subfunc (lines 5568-5571)
// ========================================================================

// ========================================================================
// Coverage: in_protected_mode / is_32bit_mode (lines 5578-5584)
// ========================================================================

// ========================================================================
// Group 2: Multiple Descriptor Error Paths (0x000E/0x000F)
// ========================================================================

// ========================================================================
// Group 3: Helper Functions
// ========================================================================

// ========================================================================
// Group 4: Real-Mode Call/Simulate Error Paths
// ========================================================================

// ========================================================================
// Group 5: Set Access Rights Error Paths
// ========================================================================

// ========================================================================
// Group 1: Trace/Debug Functions (lines 79-145)
// ========================================================================

// KILN-00086: test_dpmi_trace_emit_with_env_enabled removed — it asserted the retired
// DPMI trace env enable (now permanently off).

// ========================================================================
// Group 2: INT 31h Entry with IDT (lines 794-826)
// ========================================================================

// ========================================================================
// Group 3: Multiple Descriptor Error Paths (lines 1814-1946, 2007-2010)
// ========================================================================

// ========================================================================
// Group 4: Exception Dispatch (lines 2365-2660)
// ========================================================================

// ========================================================================
// Group 4b: Extended Exception Handler (0x0212) lines 2720-2778
// ========================================================================

// ========================================================================
// Group 4c: PM Interrupt Vector (lines 2789-2910)
// ========================================================================

// ========================================================================
// Group 5: Real-Mode Call Error Paths (lines 3062-3504)
// ========================================================================

// ========================================================================
// Group 6: DPMI 1.0 Memory Edge Cases
// ========================================================================

// ========================================================================
// Group 7: Protected Mode Entry/Exit
// ========================================================================

// ========================================================================
// Group 8: Remaining Small Ranges
// ========================================================================

// ========================================================================
// Coverage batch 3: Targeted uncovered line ranges
// ========================================================================

// --- B: Set Access Rights (0x09) selector not found ---

// --- C: Create Alias source not found (0x0A) ---

// --- D: Exception handler diagnostic code via DPMI_TRACE ---

// ========================================================================
// Lenient NULL-selector tests for DPMI 0203 / 0205 / 0213
// (HLD: 2026.05.10 - HLD - generic DPMI 8001 unhandled.md)
//
// These exercise the positive coverage of the clear-handler path:
// a previously-installed handler is correctly removed when the guest
// re-issues the same call with a NULL selector. They also pin the
// precedence of BL (exception number) validation over CX (selector)
// validation.
// ========================================================================

// --- E: Real-mode call stack copy error ---

// --- F: Real-Mode Call DPMI Trace Paths (0x0302 trace) ---

// --- G: Callback allocation trace paths ---

// --- H: Protected mode entry with PSP and environment ---

// --- I: DPMI 1.0 Linear Memory ---

// --- J: Helper Functions ---

// --- K: DOS Memory Paths ---

// --- Additional edge cases ---

// ========================================================================
// Coverage tests: Set PM Interrupt Vector (0x0205) error paths
// ========================================================================

// ========================================================================
// Coverage tests: Set Access Rights (0x0009) DPL path
// ========================================================================

// ========================================================================
// Coverage tests: Create Alias (0x000A) source not found
// ========================================================================

// ========================================================================
// Coverage tests: DOS Memory Resize Error Paths (0x0102)
// ========================================================================

// ========================================================================
// Coverage tests: Exception Handler Paths (0x0203)
// ========================================================================

// ========================================================================
// Coverage tests: prepare_exception_pointer (lines 2949-2978)
// ========================================================================

// ========================================================================
// Coverage tests: Real-Mode Callback Error Paths (0x0303/0x0304)
// ========================================================================

// ========================================================================
// Coverage tests: DPMI 1.0 Linear Memory Edge Cases (0x05xx)
// ========================================================================

// ========================================================================
// Coverage tests: Set Page Attributes (0x0507) error paths
// ========================================================================

// ========================================================================
// Coverage tests: Protected Mode Entry paths
// ========================================================================

// ========================================================================
// Coverage tests: Descriptor Management Unsupported Subfunction
// ========================================================================

// ========================================================================
// Coverage tests: Multiple Descriptors (0x0E/0x0F) - GDT selector in entry
// ========================================================================

// ========================================================================
// Coverage tests: Real-Mode Call Trace Paths (0x0301/0x0302 with trace)
// ========================================================================

// ========================================================================
// Coverage tests: DPMI 1.0 0x0504 with 16-bit client (now allowed)
// ========================================================================

// ========================================================================
// Coverage tests: Interrupt services unsupported subfunction
// ========================================================================

// ========================================================================
// Coverage tests: DOS Memory services unsupported subfunction
// ========================================================================

// ========================================================================
// Coverage tests: Translation services unsupported subfunction
// ========================================================================

// ========================================================================
// Coverage tests: DPMI 1.0 0x0506 - get page attrs with 16-bit (now allowed)
// ========================================================================

// ========================================================================
// Coverage tests: Set page attributes success path
// ========================================================================

// ========================================================================
// Coverage tests: Get page attributes success path
// ========================================================================

// ========================================================================
// Coverage tests: DPMI 1.0 0x0506 zero page count
// ========================================================================

// ========================================================================
// Coverage tests: DPMI 1.0 0x0507 zero page count
// ========================================================================

// ========================================================================
// Coverage tests: DPMI 1.0 0x050A invalid handle
// ========================================================================

// ========================================================================
// Coverage tests: exception_handler_gate_type and entry
// ========================================================================

// ========================================================================
// Coverage tests: DPMI 1.0 allocate with no free range
// ========================================================================

// ========================================================================
// Coverage tests: DPMI 1.0 0x0504 non-aligned address
// ========================================================================

// ========================================================================
// Coverage tests: DPMI 1.0 0x0504 zero size
// ========================================================================

// ========================================================================
// Coverage: DOS memory resize error paths for multi-selector case
// Lines 2365-2373: resize returns InsufficientMemory / InvalidBlock
// ========================================================================

// ========================================================================
// Coverage: Env selector allocation failure (lines 5978-5985)
// ========================================================================

// ========================================================================
// Coverage: is_selector_32bit GDT match arms (lines 6233-6237)
// ========================================================================

// ========================================================================
// Coverage: Multi-selector DOS memory push branch (lines 2079-2087)
// in update_dos_memory_descriptors when selector not yet in self.selectors
// ========================================================================

// ========================================================================
// Coverage: Exception dispatch - handler byte read failure fallback
// Lines 2568-2600: translate_address_full_checked fallback paths
// ========================================================================

// ========================================================================
// Coverage: dpmi_trace_emit fallback to tracing::debug when the trace file
// is unavailable (lines 116-118)
// ========================================================================

// ========================================================================
// Coverage: adjust_dos_memory_selectors grow path (lines 2139-2157)
// when selectors for extra 64KB chunks don't exist yet
// ========================================================================

// ========================================================================
// Coverage: is_selector_32bit_mem GDT path (line 6286)
// ========================================================================

// ========================================================================
// Regression: SetAccessRights (0009h) must not leak limit bits into s.flags
// ========================================================================

// ========================================================================
// Coverage: allocate_descriptors consecutive run with gaps (lines 6307-6334)
// ========================================================================

// ========================================================================
// Coverage: allocate_mem_handle returns None — all handles exhausted (lines 4163-4164)
// ========================================================================

// ========================================================================
// Coverage: canonicalize_vector_selector tracing paths (lines 3848-3854, 3867-3873)
// ========================================================================

// ========================================================================

// ========================================================================
// Coverage: Create alias (0x000A) all selectors exhausted (lines 1636-1638)
// ========================================================================

// ========================================================================

// ========================================================================
// Coverage: PM entry error with PageFault and default match arm (lines 6077-6078)
// ========================================================================

// ========================================================================
// Coverage: Env selector allocation failure during PM entry (lines 5986-5993)
// ========================================================================

// ========================================================================

// ========================================================================

// ========================================================================
// Coverage: find_free_linear_range with physical_mappings (lines 4154-4155)
// ========================================================================

// test_get_exception_handler_uses_idt_when_present removed:
// Remote engineer changed GetExceptionHandler to always read from
// exception_handlers[] instead of IDT (see adc34222).

// ========================================================================

// ========================================================================
// Coverage: int2f_dpmi_detect normal path with tracing
// ========================================================================

// ========================================================================

// ========================================================================
// Coverage: Raw mode switch error rollback (lines 3644-3649)
// ========================================================================

// ========================================================================
// Coverage: Raw mode switch to PM with invalid CS (lines 3596-3599)
// ========================================================================

// ========================================================================
// Coverage: Segment-to-descriptor when allocation fails (0x0002 lines 1346-1349)
// ========================================================================

// ========================================================================
// Coverage: Segment-to-selector stale mapping cleanup (lines 1342-1343)
// ========================================================================

// ========================================================================

// ========================================================================
// Coverage: Set extended exception handler — exception >= 32 (line 2720)
// ========================================================================

// ========================================================================

// ========================================================================
// Coverage: Set extended exception handler — not present handler (lines 2742-2745)
// ========================================================================

// ========================================================================
// Coverage: Set extended exception handler — offset beyond limit (lines 2747-2749)
// ========================================================================

// ========================================================================

// ========================================================================

// ========================================================================
// Coverage: Set PM interrupt vector (0x0205) not present handler (lines 2837-2840)
// ========================================================================

// ========================================================================

// ========================================================================
// Coverage: State restore with invalid CS (lines 3562-3563)
// ========================================================================

// ========================================================================
// Coverage: State save/restore with tracing for save path coverage
// ========================================================================

// === DOS Memory Block Index / Selector Helper Branch Coverage ===

// ========================================================================
// Coverage: handle_raw_switch_to_pm validation branches (lines 3627-3683)
// ========================================================================

/// Helper: set up a minimal LDT for raw PM switch tests.
/// Returns (cs_sel, ds_sel, ss_sel, es_sel) as LDT selectors with RPL=3.
/// Writes valid code (0xFA) at LDT[1], valid data (0xF2) at LDT[2-4].
/// LDT[0] is left empty for custom use.
fn setup_raw_switch_ldt(dpmi: &Dpmi, cpu: &mut Cpu, memory: &mut Memory) -> (u16, u16, u16, u16) {
    let ldt_base = 0x20000usize;
    cpu.regs.ldt_base = ldt_base as u32;
    cpu.regs.ldt_limit = 5 * 8 - 1; // 5 entries (indices 0-4)

    // LDT[1]: valid code, DPL=3 (0xFA = P=1, DPL=3, S=1, code readable)
    dpmi.write_descriptor_raw(memory, ldt_base, 1, 0, 0xFFFF, 0xFA, 0x0);
    // LDT[2]: valid data, DPL=3 (0xF2 = P=1, DPL=3, S=1, data writable)
    dpmi.write_descriptor_raw(memory, ldt_base, 2, 0, 0xFFFF, 0xF2, 0x0);
    // LDT[3]: valid data, DPL=3
    dpmi.write_descriptor_raw(memory, ldt_base, 3, 0, 0xFFFF, 0xF2, 0x0);
    // LDT[4]: valid data, DPL=3
    dpmi.write_descriptor_raw(memory, ldt_base, 4, 0, 0xFFFF, 0xF2, 0x0);

    let cs_sel: u16 = (1 << 3) | 0x04 | 0x03; // LDT index 1, TI=1, RPL=3 = 0x0F
    let ds_sel: u16 = (3 << 3) | 0x04 | 0x03; // LDT index 3, TI=1, RPL=3 = 0x1F
    let ss_sel: u16 = (2 << 3) | 0x04 | 0x03; // LDT index 2, TI=1, RPL=3 = 0x17
    let es_sel: u16 = (4 << 3) | 0x04 | 0x03; // LDT index 4, TI=1, RPL=3 = 0x27

    (cs_sel, ds_sel, ss_sel, es_sel)
}

/// Helper: set registers for raw PM switch.
fn set_raw_switch_regs(cpu: &mut Cpu, cs_sel: u16, ds_sel: u16, ss_sel: u16, es_sel: u16) {
    cpu.regs.set_si(cs_sel);
    cpu.regs.set_ax(ds_sel);
    cpu.regs.set_dx(ss_sel);
    cpu.regs.set_cx(es_sel);
    cpu.regs.ebx = 0x1000; // ESP
    cpu.regs.edi = 0x0100; // EIP
}

// === Branch coverage: descriptor & segment functions ===

// === Branch coverage: selector_in_ldt_range with max_entries == 0 ===

// === Branch coverage: env_limit_from_mcb edge cases ===

// === HLD 2026.05.24: 64 KB floor for 16-bit DPMI env selector limit ===
//
// Per DPMI 0.9 spec "Descriptor Usage Rules": 16-bit DPMI implementations
// always set the limit of the first descriptor to 64 KB even when running
// block with REPNZ SCASB / CX=0xFEFF and traps if the selector limit is
// smaller than 64 KB. These tests lock in the floor.

// === Branch coverage: overlaps_descriptor_tables table_size==0 ===

// === Branch coverage: allocate_mem_handle wraparound ===

// === Branch coverage: pm_interrupt_handler present==false ===

// === Branch coverage: Set Descriptor (000Ch) G-bit expansion ===

// === Branch coverage: offset_within_segment expand-down with big (D/B) bit ===

// === Branch coverage: additional env_limit_from_mcb test with valid MCB ===

// === Branch coverage: overlaps_descriptor_tables with non-zero gdt but zero ldt_size ===

// === Branch coverage: allocate_mem_handle next starts at zero ===

// === Branch coverage: allocate_descriptors retain filter (line 6392) ===

// ===================================================================
// Branch coverage: WatchpointSize and WatchpointType from_raw
// ===================================================================

// ===================================================================
// Branch coverage: LinearMemoryBlock commit tracking edge cases
// ===================================================================

// ===================================================================
// Branch coverage: free_memory_block (0x0502) for DPMI 1.0 blocks
// ===================================================================

// ===================================================================
// Branch coverage: validate_linear_region overflow
// ===================================================================

// ===================================================================
// Branch coverage: handle_in_use with linear memory blocks
// ===================================================================

// ===================================================================
// Branch coverage: client_pm_int_handler / set_client_pm_int_handler
// ===================================================================

// ===================================================================
// Branch coverage: pm_interrupt_handler present/not-present
// ===================================================================

// ===================================================================
// Branch coverage: dos_memory_selector_count 16-bit vs 32-bit
// ===================================================================

// ===================================================================
// Branch coverage: dos_memory_block_index_for_selector edge cases
// ===================================================================

// ===================================================================
// Branch coverage: free physical mapping (0x0801) success path
// ===================================================================

// ===================================================================
// Branch coverage: page locking subfunctions 0x0602 and 0x0603
// ===================================================================

// ===================================================================
// Branch coverage: get page size (0x0604)
// ===================================================================

// ===================================================================
// Branch coverage: demand paging unsupported subfunc
// ===================================================================

// ===================================================================
// Branch coverage: virtual interrupt state 0x0902 reports state
// ===================================================================

// ===================================================================
// Branch coverage: vendor API (0x0A00) returns unsupported
// ===================================================================

// ===================================================================
// Branch coverage: coprocessor status (0x0E00/0x0E01)
// ===================================================================

// ===================================================================
// Branch coverage: shared memory (0x0D00-0x0D02) all return unsupported
// ===================================================================

// ===================================================================
// Branch coverage: set_access_rights (0x0009) 16-bit client
// ===================================================================

// ===================================================================
// Branch coverage: segment to descriptor (0x0002) cache hit & stale
// ===================================================================

// ===================================================================
// Branch coverage: get selector increment value (0x0003)
// ===================================================================

// ===================================================================
// Branch coverage: DPMI 1.0 resize linear memory - shrink in place
// ===================================================================

// ===================================================================
// Branch coverage: DPMI 1.0 resize move with uncommitted pages
// ===================================================================

// ===================================================================
// Branch coverage: DPMI version 0x0401 with valid buffer
// ===================================================================

// ===================================================================
// Branch coverage: unhandled INT 31h function
// ===================================================================

// ===================================================================
// Branch coverage: page locking unsupported subfunc (0x0605+)
// ===================================================================

// ===================================================================
// Branch coverage: set descriptor (0x000C) with G bit set in buffer
// ===================================================================

// ===================================================================
// Branch coverage: get/set real mode interrupt vector
// ===================================================================

// ===================================================================
// Branch coverage: get exception handler 0x0202 valid exception
// ===================================================================

// ===================================================================
// Branch coverage: get extended exception handler 0x0212 without IDTR
// ===================================================================

// ===================================================================
// Branch coverage: DPMI 1.0 get memory block size 0x050A 32-bit path
// ===================================================================

// ===================================================================
// Branch coverage: read_handle / write_handle 16-bit mode
// ===================================================================

// ===================================================================
// Branch coverage: env_limit_from_mcb with 'Z' tag
// ===================================================================

// ===================================================================
// Branch coverage: ldt_entry_count with zero limit
// ===================================================================

// ===================================================================
// Branch coverage: pending_hle_simulation take
// ===================================================================

// ===================================================================
// Branch coverage: set_descriptor (0x000C) system descriptor rejected
// ===================================================================

// ===================================================================
// Branch coverage: set_access_rights (0x0009) granularity transitions
// ===================================================================

// ===================================================================
// Branch coverage: DPMI 1.0 resize linear block - grow in place
// ===================================================================

// ===================================================================
// Branch coverage: allocate_descriptors with single free selector reuse
// ===================================================================

// ===================================================================
// Branch coverage: DPMI 1.0 memory info (0x050B) with allocations
// ===================================================================

// ===================================================================
// Branch coverage: set_page_committed extends committed vector
// ===================================================================

// ===================================================================
// Branch coverage: collect_allocated_ranges includes all block types
// ===================================================================

// ===================================================================
// Branch coverage: free descriptor removes from segment_selectors cache
// ===================================================================

// ===================================================================
// Branch coverage: DPMI 0.9 allocate memory block handle skips existing
// ===================================================================

// ===================================================================
// Branch coverage: can_extend_dos_memory_selectors failure paths
// ===================================================================

// ===================================================================
// Branch coverage: DPMI 0.9 physical mapping - overflow check
// ===================================================================

// ===================================================================
// Branch coverage: get descriptor (0x000B) success path
// ===================================================================

// ===== Coverage: V86 exit conditions (lines 4423, 4428) =====

// ===== Coverage: free_descriptors duplicate check (line 2115) =====

// ===== Coverage: segment-to-selector not found (line 1357) =====

// ===== Coverage: push_descriptor_ranges (lines 4100, 4103, 4106) =====

// ===== Coverage: env_limit_from_mcb (line 4081) =====

// ===== Coverage: find_next_mem_handle wrap (lines 4186, 4198) =====

// ===== Coverage: RM callback with IP < IVT base (line 3176) =====

// ===== Coverage: push_descriptor_ranges (lines 4100, 4103, 4106) =====

// ===== Coverage: should_exit_v86_on_iret_state / retf_state (lines 4423, 4428) =====

// ===== Coverage: set_descriptor_base with ldt_ready=false (lines 6467, 6473) =====

// ===== Coverage: set_descriptor_access with ldt_ready=false (lines 6554, 6561) =====

// ===== Coverage: free_dos_memory_selectors already-free check (line 2115) =====

// ===== Coverage: adjust_dos_memory_selectors shrink path (lines 2163, 2164, 2174) =====

// ===== Coverage: env_limit_from_mcb edge (line 4081) =====

// ===== Coverage: allocate_mem_handle wrap (lines 4186, 4198) =====

// ===== Coverage: simulate RM int HLE with IP below base (line 3176) =====

// ===== Coverage: get memory info DPMI 1.0 function 0x050B (line 5039) =====

// ===== Coverage: enter PM with env selector (lines 5982, 5985, 5933) =====

// ===== Coverage: allocate specific selector already allocated (line 1776-1778) =====

// ===== Coverage: allocate specific selector success (lines 1780-1782) =====

// ===== Coverage: create alias descriptor (line 1635) =====

// ===== Coverage: IDT write verification mismatch (line 2896) =====

// ===== Coverage: get multiple descriptors success (lines 1873, 1880) =====

// ===== Coverage: set multiple descriptors success (lines 2007, 2005) =====

// ===== Coverage: allocate_descriptors reusing free selectors (line 6375) =====

// ===== Coverage: enter PM with 32-bit mode → d-bit flag (line 5933) =====

// ===== Coverage: segment_to_descriptor cached hit (line 1343-1347) =====

// ===== Coverage: segment_to_descriptor stale cache (line 1349) =====

// ===== Coverage: allocate_specific_selector overflow (line 1782) =====

// ===== Coverage: set_multiple_descriptors system segment error (line 1999) =====

// ===== Coverage: free_dos_memory_selectors dup check (line 2115) =====

// ===== Coverage: adjust_dos_memory_selectors grow next_selector (line 2163-2164) =====

// ===== Coverage: allocate_descriptors free list consecutive reuse (line 6375) =====

// ===== Coverage: free_descriptor invalid canonical selector (line 6432) =====

// ===== Coverage: free_descriptor success path (line 6438-6443) =====

// ===== Coverage: set_descriptor_limit with ldt_ready (line 6500-6509) =====

// ===== Coverage: set_descriptor_access with ldt_ready (line 6554/6561) =====

// ===== Coverage: set_descriptor_base with ldt_ready (line 6467/6473) =====

// ===== Coverage: env_limit_from_mcb with mcb_addr+4 > ram_size (line 4081) =====

// ===== Coverage: call_real_mode_procedure (0x0301) with HLE stub (line 3173) =====

// ===== Coverage: call_rm_procedure HLE stub with IP < 0x0100 (line 3176-3180) =====

// ===== Coverage: call_rm_procedure invalid BH (line 3133) =====

// ===== Coverage: call_rm_procedure excessive stack words (line 3149) =====

// ===== Coverage: set_multiple_descriptors with unallocated selector (line 1974) =====

// ===== Coverage: allocate_mem_handle wrap-around (line 4198) =====

// ===== Coverage: get_descriptor_limit/access for unknown selector =====

// ===== Coverage: DPMI unhandled descriptor function (line 2026-2029) =====

// ===== Coverage batch 20: DPMI branch coverage push =====

// KILN-00086: test_cov_dpmi_trace_functions_with_env removed — it asserted the retired
// DPMI trace env enable + file emission (now permanently off / RUST_LOG-gated).

// ========== Batch 22: canonicalize_vector_selector ==========

// ========== Batch 22: adjust_dos_memory_selectors ==========

// ========== Batch 22: allocate_descriptors from free list (count > 1) ==========

// ========== Batch 22: set_exception_handler with trace env ==========

// ========== Batch 22: trace function custom file path ==========

// ========== Batch 22: allocate_specific_ldt overflow ==========

// ========== Batch 22: set_multiple_descriptors set mode - missing selector ==========

// ========== Batch 22: env_size MCB address out of range ==========

// ========== Batch 22: handle exhaustion ==========

// ========== Batch 23: more canonicalize branches ==========

// ========== Batch 23: exception handler trace deeper branches ==========

// ========== Batch 23: DPMI int31 exception handler subfunctions ==========

// ========== Batch 23: descriptor_services more branches ==========

// ========== Batch 23: create_alias when new_sel lookup fails ==========

// ========== Batch 24: adjust_dos_memory_selectors edge cases ==========

// ========== Batch 24: exception handler with no descriptor (line 2528, 2645) ==========

// ========== Batch 24: enter PM with invalid DS descriptor ==========

// ========== Batch 24: IDT write verification ==========

// ========== Batch 24: dpmi_trace_emit not-enabled ==========

// ========== Batch 25: targeted coverage push ==========

// KILN-00086: test_b25_dpmi_trace_emit_with_env removed — asserted the retired
// DPMI trace env enable (now permanently off).

// ========== Batch 26: exception handler trace sub-paths ==========

// ========== Coverage Batch 27 ==========

// ========================================================================
// Coverage batch: allocate_mem_handle paths
// ========================================================================

// ========================================================================
// Coverage batch: env_limit_from_mcb paths (additional)
// ========================================================================

// ========================================================================
// Coverage batch: canonicalize_vector_selector LDT fallback
// ========================================================================

// ========================================================================
// Coverage batch: allocate_descriptors contiguous from free_selectors
// ========================================================================

// ========================================================================
// Coverage batch: duplicate selector_in_ldt_range check
// ========================================================================

// ========================================================================
// Coverage batch: set_multiple_descriptors selector not found
// ========================================================================

// ========================================================================
// Coverage batch: create_alias descriptor None path
// ========================================================================

// ========================================================================
// Coverage batch: segment_to_selector (AX=0x0002)
// ========================================================================

// ========================================================================
// Coverage batch: env selector allocation in enter_protected_mode
// ========================================================================

// ========================================================================
// Coverage batch: dpmi_trace_emit paths
// ========================================================================

// ========================================================================
// PM BIOS break selector tests
// ========================================================================

// ============================================================
// GDT descriptor get/set (INT 31h AX=000B/000C) tests
// ============================================================

// ============================================================
// DPMI DOS memory block tests
// ============================================================

// ============================================================
// DPMI memory allocation (INT 31h AX=0501/0502/0503) tests
// ============================================================

// ── Coverage: dos_alloc_selector / dos_free_selector / dos_resize_selector ──

// ===================================================================
// MS-DOS API Extensions for DPMI Hosts (INT 2Fh AX=168A "MS-DOS")
// ===================================================================
//
// Supplementary host services extend the DPMI 0.9 standard.
