//! x86 CPU registers
use crate::flags::Flags;
use serde::{Deserialize, Serialize};
/// CR0.MP: FWAIT raises #NM when this and [`CR0_TS`] are both set.
pub const CR0_MP: u32 = 1 << 1;
/// CR0.EM: every x87 escape raises #NM instead of executing.
pub const CR0_EM: u32 = 1 << 2;
/// CR0.TS: every x87 escape raises #NM; FWAIT does so when [`CR0_MP`] is also set.
pub const CR0_TS: u32 = 1 << 3;
/// CR0 bits that affect x87 escape and FWAIT availability.
pub const CR0_X87_CONTROL_MASK: u32 = CR0_MP | CR0_EM | CR0_TS;
/// CPU state struct field offsets for JIT code generation.
///
/// Per HLD Section 15.4, these constants define the byte offsets of
/// Registers struct fields. JIT-generated code uses these offsets
/// to access guest registers via `[rbx + OFF_*]` addressing.
///
/// # Safety
/// These offsets MUST match the actual struct layout. Compile-time
/// assertions in the tests module verify correctness.
pub mod cpu_offsets {
    pub const OFF_EAX: i32 = std::mem::offset_of!(super::Registers, eax) as i32;
    pub const OFF_EBX: i32 = std::mem::offset_of!(super::Registers, ebx) as i32;
    pub const OFF_ECX: i32 = std::mem::offset_of!(super::Registers, ecx) as i32;
    pub const OFF_EDX: i32 = std::mem::offset_of!(super::Registers, edx) as i32;
    pub const OFF_ESI: i32 = std::mem::offset_of!(super::Registers, esi) as i32;
    pub const OFF_EDI: i32 = std::mem::offset_of!(super::Registers, edi) as i32;
    pub const OFF_EBP: i32 = std::mem::offset_of!(super::Registers, ebp) as i32;
    pub const OFF_ESP: i32 = std::mem::offset_of!(super::Registers, esp) as i32;
    pub const OFF_CS: i32 = std::mem::offset_of!(super::Registers, cs) as i32;
    pub const OFF_DS: i32 = std::mem::offset_of!(super::Registers, ds) as i32;
    pub const OFF_ES: i32 = std::mem::offset_of!(super::Registers, es) as i32;
    pub const OFF_FS: i32 = std::mem::offset_of!(super::Registers, fs) as i32;
    pub const OFF_GS: i32 = std::mem::offset_of!(super::Registers, gs) as i32;
    pub const OFF_SS: i32 = std::mem::offset_of!(super::Registers, ss) as i32;
    pub const OFF_EIP: i32 = std::mem::offset_of!(super::Registers, eip) as i32;
    pub const OFF_CPL: i32 = std::mem::offset_of!(super::Registers, cpl) as i32;
    pub const OFF_FLAGS: i32 = std::mem::offset_of!(super::Registers, flags) as i32;
    pub const OFF_EFLAGS_HIGH: i32 = std::mem::offset_of!(super::Registers, eflags_high) as i32;
    pub const OFF_CR0: i32 = std::mem::offset_of!(super::Registers, cr0) as i32;
    pub const OFF_CR4: i32 = std::mem::offset_of!(super::Registers, cr4) as i32;
    pub const OFF_AX: i32 = OFF_EAX;
    pub const OFF_BX: i32 = OFF_EBX;
    pub const OFF_CX: i32 = OFF_ECX;
    pub const OFF_DX: i32 = OFF_EDX;
    pub const OFF_CHAIN_COUNTER: i32 = std::mem::size_of::<super::Registers>() as i32;
    pub const OFF_JIT_BAILOUT: i32 = OFF_CHAIN_COUNTER + 4;
    pub const OFF_JIT_REGION_EXIT_REASON: i32 =
        std::mem::offset_of!(crate::Cpu, jit_region_exit_reason) as i32;
    pub const OFF_JIT_REGION_EXIT_SOURCE_LINEAR: i32 =
        std::mem::offset_of!(crate::Cpu, jit_region_exit_source_linear) as i32;
    pub const OFF_JIT_REGION_EXIT_TARGET_LINEAR: i32 =
        std::mem::offset_of!(crate::Cpu, jit_region_exit_target_linear) as i32;
    pub const OFF_JIT_REGION_INTERNAL_EDGES: i32 =
        std::mem::offset_of!(crate::Cpu, jit_region_internal_edges) as i32;
    pub const OFF_JIT_REP_CYCLES: i32 = std::mem::offset_of!(crate::Cpu, jit_rep_cycles) as i32;
    pub const OFF_FLAG_SLOT0: i32 =
        std::mem::offset_of!(super::Registers, flag_operand_slots) as i32;
    pub const OFF_FLAG_SLOT1: i32 = OFF_FLAG_SLOT0 + 4;
    pub const OFF_CYCLES: i32 = std::mem::offset_of!(crate::Cpu, cycles) as i32;
    pub const OFF_TSC_WRITE_OFFSET: i32 = std::mem::offset_of!(crate::Cpu, tsc_write_offset) as i32;
    const FPU_REL_REGS: i32 = 0;
    const FPU_REL_STATUS_WORD: i32 = 128;
    const FPU_REL_CONTROL_WORD: i32 = 130;
    const FPU_REL_TAG_WORD: i32 = 132;
    const FPU_REL_X87_SAVE_AREA: i32 = 150;
    const FPU_BASE: i32 = std::mem::offset_of!(crate::Cpu, fpu) as i32;
    pub const OFF_FPU_REGS: i32 = FPU_BASE + FPU_REL_REGS;
    pub const OFF_FPU_STATUS_WORD: i32 = FPU_BASE + FPU_REL_STATUS_WORD;
    pub const OFF_FPU_CONTROL_WORD: i32 = FPU_BASE + FPU_REL_CONTROL_WORD;
    pub const OFF_FPU_TAG_WORD: i32 = FPU_BASE + FPU_REL_TAG_WORD;
    /// FRSTOR/FNSAVE image used for DH ownership transfer.
    pub const OFF_FPU_X87_SAVE_AREA: i32 = FPU_BASE + FPU_REL_X87_SAVE_AREA;
    /// Emitted residency flag; set before FRSTOR and cleared after FNSAVE.
    pub const OFF_FPU_HOST_RESIDENT: i32 =
        std::mem::offset_of!(crate::Cpu, fpu_host_resident) as i32;
    const _: () = assert!(std::mem::offset_of!(crate::Fpu, regs) == FPU_REL_REGS as usize);
    const _: () =
        assert!(std::mem::offset_of!(crate::Fpu, status_word) == FPU_REL_STATUS_WORD as usize);
    const _: () =
        assert!(std::mem::offset_of!(crate::Fpu, control_word) == FPU_REL_CONTROL_WORD as usize);
    const _: () = assert!(std::mem::offset_of!(crate::Fpu, tag_word) == FPU_REL_TAG_WORD as usize);
    const _: () =
        assert!(std::mem::offset_of!(crate::Fpu, x87_save_area) == FPU_REL_X87_SAVE_AREA as usize);
    const _: () = assert!(std::mem::size_of::<crate::Fpu>() == 368);
    const _: () = assert!(std::mem::size_of::<std::cell::Cell<bool>>() == 1);
    const _: () = assert!(std::mem::align_of::<std::cell::Cell<bool>>() == 1);
    const SSE_BASE: i32 = std::mem::offset_of!(crate::Cpu, sse) as i32;
    /// Guest `XMM0`; `XMMi` is at `OFF_SSE_XMM0 + 16 * i`.
    pub const OFF_SSE_XMM0: i32 = SSE_BASE + std::mem::offset_of!(crate::SseState, xmm) as i32;
    /// Guest MXCSR (u32).
    pub const OFF_SSE_MXCSR: i32 = SSE_BASE + std::mem::offset_of!(crate::SseState, mxcsr) as i32;
    /// The identity's MXCSR_MASK (u32): bits a guest MXCSR may legally hold.
    pub const OFF_SSE_MXCSR_MASK: i32 =
        SSE_BASE + std::mem::offset_of!(crate::SseState, mxcsr_mask) as i32;
    const DBX_SIMD_BASE: i32 = std::mem::offset_of!(crate::Cpu, dbx_simd) as i32;
    /// 16-byte memory-operand staging temp.
    pub const OFF_DBX_SIMD_TEMP: i32 =
        DBX_SIMD_BASE + std::mem::offset_of!(crate::DbxSimdScratch, temp) as i32;
    /// Host MXCSR parked for the length of one float bracket.
    pub const OFF_DBX_SIMD_HOST_MXCSR: i32 =
        DBX_SIMD_BASE + std::mem::offset_of!(crate::DbxSimdScratch, host_mxcsr) as i32;
    /// Sanitised guest MXCSR the bracket loads.
    pub const OFF_DBX_SIMD_GUEST_MXCSR: i32 =
        DBX_SIMD_BASE + std::mem::offset_of!(crate::DbxSimdScratch, guest_mxcsr) as i32;
    const _: () = assert!(std::mem::align_of::<crate::Cpu>().is_multiple_of(16));
    const _: () = assert!(OFF_SSE_XMM0 % 16 == 0);
    const _: () = assert!(std::mem::size_of::<[u128; 8]>() == 128);
    const _: () = assert!(OFF_DBX_SIMD_TEMP % 16 == 0);
    const _: () = assert!(std::mem::size_of::<crate::DbxSimdScratch>() == 32);
    pub const OFF_JIT_TLB_PTR: i32 = std::mem::offset_of!(crate::Cpu, jit_tlb_ptr) as i32;
    pub const OFF_VGA_COMMIT_KIND: i32 = std::mem::offset_of!(crate::Cpu, vga_commit_kind) as i32;
    pub const OFF_VGA_PLANE_MASK: i32 = std::mem::offset_of!(crate::Cpu, vga_plane_mask) as i32;
    /// Offset of the emitted inline commit's VGA-progress counter
    /// (MDD-BUG-CRUCIBLE-00150).
    pub const OFF_VGA_NATIVE_WRITES: i32 =
        std::mem::offset_of!(crate::Cpu, vga_native_writes) as i32;
    pub const OFF_VGA_PLANES_FLAT_PTR: i32 =
        std::mem::offset_of!(crate::Cpu, vga_planes_flat_ptr) as i32;
    pub const OFF_VGA_WRITE_DELAY_CYCLES: i32 =
        std::mem::offset_of!(crate::Cpu, vga_write_delay_cycles) as i32;
    pub const OFF_JIT_RAM_PTR: i32 = std::mem::offset_of!(crate::Cpu, jit_ram_ptr) as i32;
    pub const OFF_JIT_TLB_VIEW: i32 = std::mem::offset_of!(crate::Cpu, jit_tlb_view) as i32;
    /// `Cpu.fpu` — base for the EXP-A018 native x87 fast path's shadow and
    /// architectural-word addressing (compose with `fpu::shadow_offsets`).
    pub const OFF_FPU: i32 = std::mem::offset_of!(crate::Cpu, fpu) as i32;
    /// `Cpu.x87_retired` — the engagement counter the native path bumps.
    pub const OFF_X87_RETIRED: i32 = std::mem::offset_of!(crate::Cpu, x87_retired) as i32;
    /// `Cpu.mmx_retired` — the engagement counter native MMX legs bump.
    pub const OFF_MMX_RETIRED: i32 = std::mem::offset_of!(crate::Cpu, mmx_retired) as i32;
    /// `Cpu.mmx_profile` — non-null while the optional MMX histogram is active.
    /// Native paths refuse to bypass the checked profiler while this is set.
    pub const OFF_MMX_PROFILE: i32 = std::mem::offset_of!(crate::Cpu, mmx_profile) as i32;
    /// `Cpu.cpu_type` — one `repr(u8)` discriminant byte; native MMX gates
    /// test it against a capability mask built from `CpuType::ALL`.
    pub const OFF_CPU_TYPE: i32 = std::mem::offset_of!(crate::Cpu, cpu_type) as i32;
    const _: () = assert!(std::mem::size_of::<crate::CpuType>() == 1);
    const _: () = assert!(
        std::mem::size_of::<Option<Box<crate::MmxProfile>>>() == std::mem::size_of::<usize>()
    );
    pub const OFF_DYNCORE_CTX_PTR: i32 = std::mem::offset_of!(crate::Cpu, dyncore_ctx_ptr) as i32;
    pub const OFF_DBX_DISPATCH_CTX: i32 = std::mem::offset_of!(crate::Cpu, dbx_dispatch_ctx) as i32;
    pub const OFF_DBX_X87_LOAD_INLINE_HITS: i32 =
        std::mem::offset_of!(crate::Cpu, dbx_x87_load_inline_hits) as i32;
    pub const OFF_DBX_X87_STORE_INLINE_HITS: i32 =
        std::mem::offset_of!(crate::Cpu, dbx_x87_store_inline_hits) as i32;
    pub const OFF_DBX_BLOCK_ARRIVALS: i32 =
        std::mem::offset_of!(crate::Cpu, dbx_block_arrivals) as i32;
    pub const OFF_DBX_ARRIVAL_WEIGHTED_BYTES: i32 =
        std::mem::offset_of!(crate::Cpu, dbx_arrival_weighted_bytes) as i32;
    pub const OFF_DBX_VGA_STUB_CALLS: i32 =
        std::mem::offset_of!(crate::Cpu, dbx_vga_stub_calls) as i32;
    pub const OFF_DBX_VGA_STUB_PLANAR_HITS: i32 =
        std::mem::offset_of!(crate::Cpu, dbx_vga_stub_planar_hits) as i32;
    pub const OFF_DBX_VGA_STUB_CHAIN4_HITS: i32 =
        std::mem::offset_of!(crate::Cpu, dbx_vga_stub_chain4_hits) as i32;
    pub const OFF_DBX_WRITE_MAP_TABLE: i32 =
        std::mem::offset_of!(crate::Cpu, dbx_write_map_table) as i32;
    pub const OFF_DBX_WATCHED_INLINE_STORES: i32 =
        std::mem::offset_of!(crate::Cpu, dbx_watched_inline_stores) as i32;
    pub const OFF_DYNCORE_BUDGET: i32 = std::mem::offset_of!(crate::Cpu, dyncore_budget) as i32;
    pub const OFF_DYNCORE_CHAIN_RETIRED: i32 =
        std::mem::offset_of!(crate::Cpu, dyncore_chain_retired) as i32;
    pub const OFF_DYNCORE_LINK_FROM_EIP: i32 =
        std::mem::offset_of!(crate::Cpu, dyncore_link_from_eip) as i32;
    /// γ2.2 conditional-`Jcc` LINKING: the slot index (`0` taken / `1` not-taken)
    /// of the edge that bounced unresolved, written by the `Jcc` tail beside
    /// `OFF_DYNCORE_LINK_FROM_EIP` so the dispatcher fills the correct slot.
    pub const OFF_DYNCORE_LINK_FROM_SLOT: i32 =
        std::mem::offset_of!(crate::Cpu, dyncore_link_from_slot) as i32;
    /// γ2.4 near-`RET` LINKING: the RET-miss flag a linking `RET` block's tail sets
    /// to `1` (via `mov dword [rdi + OFF], 1`) before returning, so the dispatcher
    /// routes to the RET-cache fill/lookup rather than the outbound-slot fill. A
    /// `u32` (not `u8`) so the tail can write it with the existing
    /// `mov dword [rdi+OFF], imm` emitter — no byte-store primitive needed.
    pub const OFF_DYNCORE_RET_MISS: i32 = std::mem::offset_of!(crate::Cpu, dyncore_ret_miss) as i32;
    /// γ2.5 inline RET-cache HIT: the baked address of the FROM (RET) block's inline
    /// ret-cache cell, written by the RET tail's miss path beside
    /// `OFF_DYNCORE_RET_MISS` so `resolve_ret` can fill that exact cell without a
    /// second page-walk for the FROM identity. A `usize`-wide carrier (the raw cell
    /// pointer); only read when `dyncore_ret_miss != 0`.
    pub const OFF_DYNCORE_RET_FROM_CELL: i32 =
        std::mem::offset_of!(crate::Cpu, dyncore_ret_from_cell) as i32;
    /// γ2.5 inline RET-cache HIT observability: the per-dispatch count of native
    /// inline RET HITs. The emitted HIT path bumps it (`add qword [rdi+OFF], 1`)
    /// before its `jmp cached_entry`; `DynCore::run` drains it after the chain
    /// returns (an inline HIT never re-enters `run`, so this is the only proof a HIT
    /// happened).
    pub const OFF_DYNCORE_RET_INLINE_HITS: i32 =
        std::mem::offset_of!(crate::Cpu, dyncore_ret_inline_hits) as i32;
    /// Offset of `dyncore_slow_linear` — the faulting guest linear address the
    /// native memory-op slow stub captures (`mov [rdi + OFF_DYNCORE_SLOW_LINEAR],
    /// eax`) so the host can warm the software TLB for that page under CR0.PG=0.
    pub const OFF_DYNCORE_SLOW_LINEAR: i32 =
        std::mem::offset_of!(crate::Cpu, dyncore_slow_linear) as i32;
    /// Sentinel for [`OFF_DYNCORE_LINK_FROM_EIP`] meaning "no link bounce this
    /// dispatch". A linking `JMP` block cannot legally start at `0xFFFF_FFFF`
    /// (its bytes would wrap past the 4 GiB boundary, which the single-physical-
    /// page block builder never emits), so this value is unambiguous.
    pub const DYNCORE_LINK_FROM_NONE: u32 = u32::MAX;
    pub const OFF_SMC_HAS_CODE_PAGES: i32 =
        std::mem::offset_of!(crate::Cpu, smc_has_code_pages) as i32;
    pub const OFF_HOST_FP_ENV_DEFAULT: i32 =
        std::mem::offset_of!(crate::Cpu, host_fp_env_default) as i32;
    pub const OFF_WAKES_PENDING: i32 = std::mem::offset_of!(crate::Cpu, wakes_pending) as i32
        + std::mem::offset_of!(crate::WakesPendingFlag, flag) as i32;
    const DBX_SCRATCH_BASE: i32 = std::mem::offset_of!(crate::Cpu, dbx_scratch) as i32;
    pub const OFF_DBX_NEWESP: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, newesp) as i32;
    pub const OFF_DBX_EA: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, ea) as i32;
    pub const OFF_DBX_STACK: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, stack) as i32;
    pub const OFF_DBX_CYCLES_REMAINING: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, cycles_remaining) as i32;
    pub const OFF_DBX_NON_RETIRED_CYCLES: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, non_retired_cycles) as i32;
    pub const OFF_DBX_TMPB: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, tmpb) as i32;
    pub const OFF_DBX_TMPW: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, tmpw) as i32;
    pub const OFF_DBX_TMPD: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, tmpd) as i32;
    pub const OFF_DBX_SHIFT: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, shift) as i32;
    pub const OFF_DBX_EXIT: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, exit) as i32;
    pub const OFF_DBX_PAGING_STATUS: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, paging_status) as i32;
    pub const OFF_DBX_HELPER_STATUS: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, helper_status) as i32;
    pub const OFF_DBX_RESTART_FLAGS: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, restart_flags) as i32;
    pub const OFF_DBX_RUNNING_BLOCK_ID: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, running_block_id) as i32;
    pub const OFF_DBX_PENDING_INDIRECT_CELL: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, pending_indirect_cell) as i32;
    pub const OFF_DBX_HOST_CALL_GATEWAY: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, host_call_gateway) as i32;
    /// D1 route split: the D5 VERIFIED tail-jump gateway entry — the default
    /// route for gateway-routed helpers outside the bracketed class.
    pub const OFF_DBX_HOST_CALL_GATEWAY_VERIFIED: i32 = DBX_SCRATCH_BASE
        + std::mem::offset_of!(crate::DbxScratch, host_call_gateway_verified) as i32;
    /// Unit 2b return-address stack. Base of the sixteen return-cell
    /// addresses; the emitted tails index it as `[rbp + top*8 + OFF_DBX_RAS]`.
    pub const OFF_DBX_RAS: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, ras) as i32;
    /// Unit 2b-ext: base of the sixteen predicted return EIPs, indexed
    /// `[rbp + top*4 + OFF_DBX_RAS_EIP]` by the emitted push.
    pub const OFF_DBX_RAS_EIP: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, ras_eip) as i32;
    /// Unit 2b return-address stack top (a slot index, not a byte offset).
    pub const OFF_DBX_RAS_TOP: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, ras_top) as i32;
    /// Memo generation the return-address stack's pointers were published
    /// under; read only by the dispatcher, never by emitted code.
    pub const OFF_DBX_RAS_GEN: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, ras_gen) as i32;
    /// Unit 2b-ext census: native `CALL r/m32` executions.
    pub const OFF_DBX_INDIRECT_CALLS: i32 =
        DBX_SCRATCH_BASE + std::mem::offset_of!(crate::DbxScratch, indirect_calls) as i32;
    pub const OFF_CS_CACHE_BASE: i32 = std::mem::offset_of!(super::Registers, cs_cache) as i32
        + std::mem::offset_of!(super::SegmentCache, base) as i32;
    pub const OFF_DS_CACHE_BASE: i32 = std::mem::offset_of!(super::Registers, ds_cache) as i32
        + std::mem::offset_of!(super::SegmentCache, base) as i32;
    pub const OFF_ES_CACHE_BASE: i32 = std::mem::offset_of!(super::Registers, es_cache) as i32
        + std::mem::offset_of!(super::SegmentCache, base) as i32;
    pub const OFF_SS_CACHE_BASE: i32 = std::mem::offset_of!(super::Registers, ss_cache) as i32
        + std::mem::offset_of!(super::SegmentCache, base) as i32;
    pub const OFF_FS_CACHE_BASE: i32 = std::mem::offset_of!(super::Registers, fs_cache) as i32
        + std::mem::offset_of!(super::SegmentCache, base) as i32;
    pub const OFF_GS_CACHE_BASE: i32 = std::mem::offset_of!(super::Registers, gs_cache) as i32
        + std::mem::offset_of!(super::SegmentCache, base) as i32;
    pub const OFF_CS_CACHE_EFFECTIVE_LIMIT: i32 = std::mem::offset_of!(super::Registers, cs_cache)
        as i32
        + std::mem::offset_of!(super::SegmentCache, cached_effective_limit) as i32;
    pub const OFF_DS_CACHE_EFFECTIVE_LIMIT: i32 = std::mem::offset_of!(super::Registers, ds_cache)
        as i32
        + std::mem::offset_of!(super::SegmentCache, cached_effective_limit) as i32;
    pub const OFF_ES_CACHE_EFFECTIVE_LIMIT: i32 = std::mem::offset_of!(super::Registers, es_cache)
        as i32
        + std::mem::offset_of!(super::SegmentCache, cached_effective_limit) as i32;
    pub const OFF_SS_CACHE_EFFECTIVE_LIMIT: i32 = std::mem::offset_of!(super::Registers, ss_cache)
        as i32
        + std::mem::offset_of!(super::SegmentCache, cached_effective_limit) as i32;
    pub const OFF_FS_CACHE_EFFECTIVE_LIMIT: i32 = std::mem::offset_of!(super::Registers, fs_cache)
        as i32
        + std::mem::offset_of!(super::SegmentCache, cached_effective_limit) as i32;
    pub const OFF_GS_CACHE_EFFECTIVE_LIMIT: i32 = std::mem::offset_of!(super::Registers, gs_cache)
        as i32
        + std::mem::offset_of!(super::SegmentCache, cached_effective_limit) as i32;
}
pub const CR4_VME: u32 = 1 << 0;
pub const CR4_PVI: u32 = 1 << 1;
/// CR4.TSD: restrict RDTSC/RDTSCP to CPL 0 when set.
pub const CR4_TSD: u32 = 1 << 2;
/// CR4.DE: enable debug-register exact breakpoint extensions.
pub const CR4_DE: u32 = 1 << 3;
/// CR4.PSE: enable 4 MiB paging.
pub const CR4_PSE: u32 = 1 << 4;
/// CR4.PAE: enable physical-address extensions.
pub const CR4_PAE: u32 = 1 << 5;
/// CR4.MCE: enable machine-check exceptions.
pub const CR4_MCE: u32 = 1 << 6;
/// CR4.PGE: enable global TLB entries.
pub const CR4_PGE: u32 = 1 << 7;
/// CR4.PCE: allow RDPMC at CPL > 0.
pub const CR4_PCE: u32 = 1 << 8;
/// CR4.OSFXSR: operating system support for FXSAVE/FXRSTOR and XMM state.
pub const CR4_OSFXSR: u32 = 1 << 9;
/// CR4.OSXMMEXCPT: operating system support for SIMD FP exceptions.
pub const CR4_OSXMMEXCPT: u32 = 1 << 10;
/// Descriptor table register (GDTR/IDTR)
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct DescriptorTableRegister {
    pub base: u32,
    pub limit: u16,
}
/// Hidden descriptor cache for segment registers
/// Caches the descriptor information to avoid re-reading from GDT/LDT on every access
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentCache {
    pub base: u32,
    pub limit: u32,
    /// Pre-computed effective limit (granularity-applied).
    /// Always equals `if granularity { (limit << 12) | 0xFFF } else { limit }`.
    /// Cached to avoid recomputing on every memory access and for JIT codegen
    /// to emit runtime comparisons via `OFF_*_CACHE_EFFECTIVE_LIMIT`.
    pub cached_effective_limit: u32,
    pub access: u8,
    pub flags: u8,
    pub present: bool,
    pub dpl: u8,
    pub is_code: bool,
    pub is_conforming: bool,
    pub is_readable: bool,
    pub is_writable: bool,
    pub granularity: bool,
    pub big: bool,
}
/// The fields a [`SegmentCache`] derives from its access-rights and flags
/// bytes. See [`SegmentCache::derive_bits`].
pub(crate) struct DerivedSegmentBits {
    pub present: bool,
    pub dpl: u8,
    pub is_code: bool,
    pub is_conforming: bool,
    pub is_readable: bool,
    pub is_writable: bool,
    pub granularity: bool,
    pub big: bool,
}
impl SegmentCache {
    /// Create a real-mode style segment cache (base = segment << 4, limit = 64KB)
    pub fn real_mode(segment: u16) -> Self {
        Self {
            base: (segment as u32) << 4,
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            access: 0x93,
            flags: 0,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: false,
            big: false,
        }
    }
    /// Descriptor cache for a data segment register that the CPU has loaded
    /// with the **null selector (0)** during a V86→ring-0 interrupt/exception
    /// transition (SDM §20.3.1.2 pushes then zeroes DS/ES/FS/GS).
    ///
    /// The visible selector value is set to 0 by the caller; this is the
    /// *hidden* descriptor cache that backs it. The automatic V86-entry
    /// null retains a usable flat segment (base 0, 4 GiB), allowing a handler
    /// to access flat data before explicitly loading a new data selector.
    ///
    /// This is scoped to the CPU's *automatic* V86-entry null; a software
    /// `mov ds, 0` still goes through the normal load path and leaves an
    /// unusable null segment that faults on use.
    pub fn null_flat_usable() -> Self {
        Self {
            base: 0,
            limit: 0x000F_FFFF,
            cached_effective_limit: 0xFFFF_FFFF,
            access: 0x93,
            flags: 0x0C,
            present: true,
            dpl: 0,
            is_code: false,
            is_conforming: false,
            is_readable: true,
            is_writable: true,
            granularity: true,
            big: true,
        }
    }
    /// Parse a descriptor from raw bytes
    pub fn from_descriptor(desc: &[u8; 8]) -> Self {
        let limit_low = u16::from_le_bytes([desc[0], desc[1]]);
        let base_low = u16::from_le_bytes([desc[2], desc[3]]);
        let base_mid = desc[4];
        let access = desc[5];
        let limit_flags = desc[6];
        let base_high = desc[7];
        let base = (base_low as u32) | ((base_mid as u32) << 16) | ((base_high as u32) << 24);
        let limit = (limit_low as u32) | (((limit_flags & 0x0F) as u32) << 16);
        let flags = limit_flags >> 4;
        let DerivedSegmentBits {
            present,
            dpl,
            is_code,
            is_conforming,
            is_readable,
            is_writable,
            granularity,
            big,
        } = Self::derive_bits(access, flags);
        let cached_effective_limit = Self::compute_effective_limit(limit, granularity);
        Self {
            base,
            limit,
            cached_effective_limit,
            access,
            flags,
            present,
            dpl,
            is_code,
            is_conforming,
            is_readable,
            is_writable,
            granularity,
            big,
        }
    }
    /// Decode the fields this cache DERIVES from its access-rights and flags
    /// bytes.
    ///
    /// MDD-BUG-CRUCIBLE-00581: extracted so `from_descriptor` and
    /// `CpuSnapshot::validate` share one decoding, rather than validation
    /// carrying a second copy that could drift away from the real rule. The
    /// point of the check is that a snapshot's derived fields must be exactly
    /// what its canonical bytes produce, so it has to reconstruct them with the
    /// same code that produced them.
    #[inline(always)]
    pub(crate) fn derive_bits(access: u8, flags: u8) -> DerivedSegmentBits {
        let is_code = (access & 0x08) != 0;
        DerivedSegmentBits {
            present: (access & 0x80) != 0,
            dpl: (access >> 5) & 0x03,
            is_code,
            is_conforming: is_code && (access & 0x04) != 0,
            is_readable: is_code && (access & 0x02) != 0,
            is_writable: !is_code && (access & 0x02) != 0,
            granularity: (flags & 0x08) != 0,
            big: (flags & 0x04) != 0,
        }
    }
    /// Whether every derived field agrees with the canonical bytes it comes
    /// from, and the cached effective limit matches its documented expression.
    ///
    /// MDD-BUG-CRUCIBLE-00581: a snapshot serialises both the canonical fields
    /// and their derivatives, so a malformed one can disagree with itself - for
    /// instance a small, non-writable segment in `limit`/`access` alongside a
    /// `cached_effective_limit` of `0xFFFF_FFFF`. Bounds checks and permission
    /// checks read the derived fields, so that image grants access the
    /// canonical descriptor never described.
    pub fn derived_fields_are_coherent(&self) -> bool {
        let want = Self::derive_bits(self.access, self.flags);
        (!self.is_code || self.is_readable == want.is_readable)
            && self.present == want.present
            && self.dpl == want.dpl
            && self.is_code == want.is_code
            && self.is_conforming == want.is_conforming
            && self.is_writable == want.is_writable
            && self.granularity == want.granularity
            && self.big == want.big
            && self.cached_effective_limit
                == Self::compute_effective_limit(self.limit, want.granularity)
    }
    /// Compute effective limit from limit and granularity fields.
    #[inline(always)]
    fn compute_effective_limit(limit: u32, granularity: bool) -> u32 {
        if granularity {
            (limit << 12) | 0xFFF
        } else {
            limit
        }
    }
    /// Get the effective limit (accounting for granularity).
    ///
    /// Always recomputes from the raw `limit` and `granularity` fields, so
    /// callers do not need to keep `cached_effective_limit` in sync when
    /// mutating those fields directly. The cached field is still maintained
    /// (via `set_limit` / `set_granularity` / `recompute_effective_limit`)
    /// for JIT-emitted code, which reads it through `OFF_*_CACHE_EFFECTIVE_LIMIT`
    /// as a single memory load. Recomputing here is cheap (one branch on
    /// `granularity`) and ensures interpreter and test code paths see the
    /// authoritative value even when fixtures pre-date the cache invariant.
    #[inline(always)]
    pub fn effective_limit(&self) -> u32 {
        Self::compute_effective_limit(self.limit, self.granularity)
    }
    /// Recompute and update the cached effective limit.
    /// Must be called after any mutation to `limit` or `granularity`.
    #[inline(always)]
    pub fn recompute_effective_limit(&mut self) {
        self.cached_effective_limit = Self::compute_effective_limit(self.limit, self.granularity);
    }
    /// Set the segment limit and automatically recompute the effective limit cache.
    #[inline(always)]
    pub fn set_limit(&mut self, limit: u32) {
        self.limit = limit;
        self.recompute_effective_limit();
    }
    /// Set the granularity flag and automatically recompute the effective limit cache.
    #[inline(always)]
    pub fn set_granularity(&mut self, granularity: bool) {
        self.granularity = granularity;
        self.recompute_effective_limit();
    }
}
/// Intel reset value for DR6 (Debug Status Register). Used as the serde default
/// so a snapshot omitting `dr6` restores the reset value rather than 0.
fn default_dr6() -> u32 {
    0xFFFF_0FF0
}
/// Intel reset value for DR7 (Debug Control Register). Used as the serde default
/// so a snapshot omitting `dr7` restores the reset value rather than 0.
fn default_dr7() -> u32 {
    0x0000_0400
}
/// CPU register file
///
/// Layout is `#[repr(C)]` for deterministic field ordering, enabling
/// JIT code to access registers via fixed offsets. The offsets are
/// defined in [`cpu_offsets`] and verified by compile-time assertions.
#[repr(C)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registers {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esi: u32,
    pub edi: u32,
    pub ebp: u32,
    pub esp: u32,
    pub cs: u16,
    pub ds: u16,
    pub es: u16,
    pub fs: u16,
    pub gs: u16,
    pub ss: u16,
    pub eip: u32,
    pub flags: Flags,
    /// Extended EFLAGS (bits 16-31 for VM, RF, etc.)
    /// Use `eflags()` and `set_eflags()` to access full 32-bit EFLAGS.
    /// Bits 0-15 are stored in `flags`, bits 16-31 here.
    pub eflags_high: u16,
    pub cr0: u32,
    pub cr2: u32,
    pub cr3: u32,
    #[serde(default)]
    pub cr4: u32,
    #[serde(default)]
    pub dr0: u32,
    #[serde(default)]
    pub dr1: u32,
    #[serde(default)]
    pub dr2: u32,
    #[serde(default)]
    pub dr3: u32,
    #[serde(default = "default_dr6")]
    pub dr6: u32,
    #[serde(default = "default_dr7")]
    pub dr7: u32,
    pub gdtr: DescriptorTableRegister,
    pub idtr: DescriptorTableRegister,
    /// Table-level generation for GDT descriptor proof invalidation.
    #[serde(default)]
    pub gdt_generation: u64,
    /// Table-level generation for LDT descriptor proof invalidation.
    #[serde(default)]
    pub ldt_generation: u64,
    pub tr: u16,
    pub tss_base: u32,
    pub tss_limit: u32,
    pub tss_ss0: u16,
    pub tss_esp0: u32,
    pub ldtr: u16,
    pub ldt_base: u32,
    pub ldt_limit: u32,
    pub cpl: u8,
    pub cs_cache: SegmentCache,
    pub ds_cache: SegmentCache,
    pub es_cache: SegmentCache,
    pub fs_cache: SegmentCache,
    pub gs_cache: SegmentCache,
    pub ss_cache: SegmentCache,
    pub flag_operand_slots: [u32; 2],
}
/// Generate 8-bit register getter/setter pairs from a 32-bit backing register.
///
/// - `low` variant accesses the low byte (bits 0-7): al, bl, cl, dl
/// - `high` variant accesses the high byte (bits 8-15): ah, bh, ch, dh
macro_rules! byte_reg_accessors {
    (low $getter:ident, $setter:ident, $reg:ident) => {
        #[inline]
        pub fn $getter(&self) -> u8 {
            self.$reg as u8
        }
        #[inline]
        pub fn $setter(&mut self, val: u8) {
            self.$reg = (self.$reg & 0xFFFF_FF00) | val as u32;
        }
    };
    (high $getter:ident, $setter:ident, $reg:ident) => {
        #[inline]
        pub fn $getter(&self) -> u8 {
            (self.$reg >> 8) as u8
        }
        #[inline]
        pub fn $setter(&mut self, val: u8) {
            self.$reg = (self.$reg & 0xFFFF_00FF) | ((val as u32) << 8);
        }
    };
}
impl Registers {
    pub fn new() -> Self {
        Self {
            eax: 0,
            ebx: 0,
            ecx: 0,
            edx: 0,
            esi: 0,
            edi: 0,
            ebp: 0,
            esp: 0,
            cs: 0,
            ds: 0,
            es: 0,
            fs: 0,
            gs: 0,
            ss: 0,
            eip: 0,
            flags: Flags::default(),
            eflags_high: 0,
            cr0: 0,
            cr2: 0,
            cr3: 0,
            cr4: 0,
            dr0: 0,
            dr1: 0,
            dr2: 0,
            dr3: 0,
            dr6: 0xFFFF_0FF0,
            dr7: 0x0000_0400,
            gdtr: DescriptorTableRegister::default(),
            idtr: DescriptorTableRegister::default(),
            gdt_generation: 0,
            ldt_generation: 0,
            tr: 0,
            tss_base: 0,
            tss_limit: 0,
            tss_ss0: 0,
            tss_esp0: 0,
            ldtr: 0,
            ldt_base: 0,
            ldt_limit: 0,
            cpl: 0,
            cs_cache: SegmentCache::real_mode(0),
            ds_cache: SegmentCache::real_mode(0),
            es_cache: SegmentCache::real_mode(0),
            fs_cache: SegmentCache::real_mode(0),
            gs_cache: SegmentCache::real_mode(0),
            ss_cache: SegmentCache::real_mode(0),
            flag_operand_slots: [0, 0],
        }
    }
    #[inline]
    pub fn ax(&self) -> u16 {
        self.eax as u16
    }
    #[inline]
    pub fn bx(&self) -> u16 {
        self.ebx as u16
    }
    #[inline]
    pub fn cx(&self) -> u16 {
        self.ecx as u16
    }
    #[inline]
    pub fn dx(&self) -> u16 {
        self.edx as u16
    }
    #[inline]
    pub fn si(&self) -> u16 {
        self.esi as u16
    }
    #[inline]
    pub fn di(&self) -> u16 {
        self.edi as u16
    }
    #[inline]
    pub fn bp(&self) -> u16 {
        self.ebp as u16
    }
    #[inline]
    pub fn set_ax(&mut self, val: u16) {
        self.eax = (self.eax & 0xFFFF_0000) | val as u32;
    }
    #[inline]
    pub fn set_bx(&mut self, val: u16) {
        self.ebx = (self.ebx & 0xFFFF_0000) | val as u32;
    }
    #[inline]
    pub fn set_cx(&mut self, val: u16) {
        self.ecx = (self.ecx & 0xFFFF_0000) | val as u32;
    }
    #[inline]
    pub fn set_dx(&mut self, val: u16) {
        self.edx = (self.edx & 0xFFFF_0000) | val as u32;
    }
    #[inline]
    pub fn set_si(&mut self, val: u16) {
        self.esi = (self.esi & 0xFFFF_0000) | val as u32;
    }
    #[inline]
    pub fn set_di(&mut self, val: u16) {
        self.edi = (self.edi & 0xFFFF_0000) | val as u32;
    }
    #[inline]
    pub fn set_bp(&mut self, val: u16) {
        self.ebp = (self.ebp & 0xFFFF_0000) | val as u32;
    }
    #[inline]
    pub fn ip(&self) -> u16 {
        self.eip as u16
    }
    #[inline]
    pub fn set_ip(&mut self, val: u16) {
        self.eip = val as u32;
    }
    /// Set the full 32-bit EIP.
    #[inline]
    pub fn set_eip(&mut self, val: u32) {
        self.eip = val;
    }
    #[inline]
    pub fn sp(&self) -> u16 {
        self.esp as u16
    }
    #[inline]
    pub fn set_sp(&mut self, val: u16) {
        self.esp = (self.esp & 0xFFFF_0000) | val as u32;
    }
    /// Set the full 32-bit ESP.
    #[inline]
    pub fn set_esp(&mut self, val: u32) {
        self.esp = val;
    }
    byte_reg_accessors!(low al, set_al, eax);
    byte_reg_accessors!(high ah, set_ah, eax);
    byte_reg_accessors!(low bl, set_bl, ebx);
    byte_reg_accessors!(high bh, set_bh, ebx);
    byte_reg_accessors!(low cl, set_cl, ecx);
    byte_reg_accessors!(high ch, set_ch, ecx);
    byte_reg_accessors!(low dl, set_dl, edx);
    byte_reg_accessors!(high dh, set_dh, edx);
    /// Get full 32-bit EFLAGS
    #[inline]
    pub fn eflags(&self) -> u32 {
        (self.flags.bits() as u32) | ((self.eflags_high as u32) << 16)
    }
    /// Set full 32-bit EFLAGS
    #[inline]
    pub fn set_eflags(&mut self, val: u32) {
        self.flags = Flags::from_bits_truncate(val as u16) | Flags::RESERVED1;
        self.eflags_high = (val >> 16) as u16;
    }
    /// Check if VM (Virtual 8086 mode) flag is set (bit 17)
    #[inline]
    pub fn is_v86_mode(&self) -> bool {
        (self.eflags_high & 0x0002) != 0
    }
    /// Set or clear VM flag.
    ///
    /// Per Intel SDM §20.1, V86 tasks always run at architectural CPL=3.
    /// When entering V86 mode we therefore force `cpl = 3` so the rest of
    /// the emulator never observes the impossible "VM=1, CPL<3" state.
    /// Leaving V86 does not alter CPL — the caller is responsible for
    /// restoring the protected-mode CPL from saved state (e.g. saved CS
    /// RPL or DPMI's V86SavedState).
    #[inline]
    pub fn set_v86_mode(&mut self, enabled: bool) {
        if enabled {
            self.eflags_high |= 0x0002;
            self.cpl = 3;
        } else {
            self.eflags_high &= !0x0002;
        }
    }
    /// Set CPL, enforcing the SDM §20.1 invariant that V86 tasks always
    /// run at architectural CPL=3.
    ///
    /// Production code must go through this setter. The `cpl` field
    /// remains `pub` for test-only invariant-violation scenarios; in
    /// release builds the V86 forcing is silent. In debug/test builds,
    /// any attempt to set CPL != 3 while VM=1 panics so latent bugs
    /// surface immediately.
    #[inline]
    pub fn set_cpl(&mut self, v: u8) {
        debug_assert!(v <= 3, "CPL must be 0..=3, got {v}");
        if self.is_v86_mode() {
            debug_assert!(
                v == 3,
                "CPL invariant violation: VM=1 requires CPL=3, got {v}"
            );
            self.cpl = 3;
        } else {
            self.cpl = v;
        }
    }
    /// Get IOPL (I/O Privilege Level) from EFLAGS (bits 12-13)
    #[inline]
    pub fn iopl(&self) -> u8 {
        ((self.flags.bits() >> 12) & 0x03) as u8
    }
    /// Set IOPL in EFLAGS
    #[inline]
    pub fn set_iopl(&mut self, iopl: u8) {
        let new_flags = (self.flags.bits() & !0x3000) | (((iopl & 0x03) as u16) << 12);
        self.flags = Flags::from_bits_truncate(new_flags);
    }
}
impl Default for Registers {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn test_8bit_access() {
        let mut regs = Registers::new();
        regs.eax = 0x12345678;
        assert_eq!(regs.al(), 0x78);
        assert_eq!(regs.ah(), 0x56);
        regs.set_al(0xAB);
        assert_eq!(regs.eax, 0x123456AB);
        regs.set_ah(0xCD);
        assert_eq!(regs.eax, 0x1234CDAB);
    }
    #[test]
    fn test_16bit_access() {
        let mut regs = Registers::new();
        regs.eax = 0x12345678;
        assert_eq!(regs.ax(), 0x5678);
        regs.set_ax(0xABCD);
        assert_eq!(regs.eax, 0x1234ABCD);
    }
    #[test]
    fn test_new() {
        let regs = Registers::new();
        assert_eq!(regs.eax, 0);
        assert_eq!(regs.ebx, 0);
        assert_eq!(regs.ecx, 0);
        assert_eq!(regs.edx, 0);
        assert_eq!(regs.esi, 0);
        assert_eq!(regs.edi, 0);
        assert_eq!(regs.ebp, 0);
        assert_eq!(regs.esp, 0);
        assert_eq!(regs.cs, 0);
        assert_eq!(regs.ds, 0);
        assert_eq!(regs.es, 0);
        assert_eq!(regs.fs, 0);
        assert_eq!(regs.gs, 0);
        assert_eq!(regs.ss, 0);
        assert_eq!(regs.ip(), 0);
        assert_eq!(regs.sp(), 0);
        assert_eq!(regs.cr0, 0);
        assert_eq!(regs.cr3, 0);
    }
    #[test]
    fn test_default() {
        let regs = Registers::default();
        assert_eq!(regs.eax, 0);
    }
    #[test]
    fn test_bx_access() {
        let mut regs = Registers::new();
        regs.ebx = 0xAABBCCDD;
        assert_eq!(regs.bx(), 0xCCDD);
        regs.set_bx(0x1234);
        assert_eq!(regs.ebx, 0xAABB1234);
    }
    #[test]
    fn test_cx_access() {
        let mut regs = Registers::new();
        regs.ecx = 0x11223344;
        assert_eq!(regs.cx(), 0x3344);
        regs.set_cx(0x5566);
        assert_eq!(regs.ecx, 0x11225566);
    }
    #[test]
    fn test_dx_access() {
        let mut regs = Registers::new();
        regs.edx = 0x99887766;
        assert_eq!(regs.dx(), 0x7766);
        regs.set_dx(0xAABB);
        assert_eq!(regs.edx, 0x9988AABB);
    }
    #[test]
    fn test_si_access() {
        let mut regs = Registers::new();
        regs.esi = 0xDEADBEEF;
        assert_eq!(regs.si(), 0xBEEF);
        regs.set_si(0xCAFE);
        assert_eq!(regs.esi, 0xDEADCAFE);
    }
    #[test]
    fn test_di_access() {
        let mut regs = Registers::new();
        regs.edi = 0xFACEFEED;
        assert_eq!(regs.di(), 0xFEED);
        regs.set_di(0xF00D);
        assert_eq!(regs.edi, 0xFACEF00D);
    }
    #[test]
    fn test_bp_access() {
        let mut regs = Registers::new();
        regs.ebp = 0xC0FFEE00;
        assert_eq!(regs.bp(), 0xEE00);
        regs.set_bp(0x1337);
        assert_eq!(regs.ebp, 0xC0FF1337);
    }
    #[test]
    fn test_sp_access() {
        let mut regs = Registers::new();
        regs.esp = 0xBABE0000;
        regs.set_sp(0xFACE);
        assert_eq!(regs.esp, 0xBABEFACE);
        assert_eq!(regs.sp(), 0xFACE);
    }
    #[test]
    fn test_bl_bh_access() {
        let mut regs = Registers::new();
        regs.ebx = 0x12345678;
        assert_eq!(regs.bl(), 0x78);
        assert_eq!(regs.bh(), 0x56);
        regs.set_bl(0xAA);
        assert_eq!(regs.ebx, 0x123456AA);
        regs.set_bh(0xBB);
        assert_eq!(regs.ebx, 0x1234BBAA);
    }
    #[test]
    fn test_cl_ch_access() {
        let mut regs = Registers::new();
        regs.ecx = 0xDEADBEEF;
        assert_eq!(regs.cl(), 0xEF);
        assert_eq!(regs.ch(), 0xBE);
        regs.set_cl(0x11);
        assert_eq!(regs.ecx, 0xDEADBE11);
        regs.set_ch(0x22);
        assert_eq!(regs.ecx, 0xDEAD2211);
    }
    #[test]
    fn test_dl_dh_access() {
        let mut regs = Registers::new();
        regs.edx = 0xCAFEBABE;
        assert_eq!(regs.dl(), 0xBE);
        assert_eq!(regs.dh(), 0xBA);
        regs.set_dl(0x33);
        assert_eq!(regs.edx, 0xCAFEBA33);
        regs.set_dh(0x44);
        assert_eq!(regs.edx, 0xCAFE4433);
    }
    #[test]
    fn test_descriptor_table_register_default() {
        let dtr = DescriptorTableRegister::default();
        assert_eq!(dtr.base, 0);
        assert_eq!(dtr.limit, 0);
    }
    #[test]
    fn test_registers_clone() {
        let mut regs = Registers::new();
        regs.eax = 0x12345678;
        regs.cs = 0x1000;
        let cloned = regs.clone();
        assert_eq!(cloned.eax, 0x12345678);
        assert_eq!(cloned.cs, 0x1000);
    }
    #[test]
    fn test_8bit_doesnt_affect_high_word() {
        let mut regs = Registers::new();
        regs.eax = 0xFFFF_0000;
        regs.set_al(0x12);
        regs.set_ah(0x34);
        assert_eq!(regs.eax, 0xFFFF3412);
    }
    #[test]
    fn test_16bit_doesnt_affect_high_word() {
        let mut regs = Registers::new();
        regs.eax = 0xABCD0000;
        regs.set_ax(0x1234);
        assert_eq!(regs.eax, 0xABCD1234);
    }
    #[test]
    fn test_set_ip_zero_extends_eip() {
        let mut regs = Registers::new();
        regs.eip = 0x0041_0000;
        regs.set_ip(0xFCDE);
        assert_eq!(regs.eip, 0x0000_FCDE);
        assert_eq!(regs.ip(), 0xFCDE);
    }
    #[test]
    fn test_gdtr_idtr_initialization() {
        let regs = Registers::new();
        assert_eq!(regs.gdtr.base, 0);
        assert_eq!(regs.gdtr.limit, 0);
        assert_eq!(regs.idtr.base, 0);
        assert_eq!(regs.idtr.limit, 0);
    }
    #[test]
    fn test_flags_initialization() {
        let regs = Registers::new();
        assert_eq!(regs.flags, Flags::default());
    }
    #[test]
    fn test_segment_cache_default() {
        let cache = SegmentCache::default();
        assert_eq!(cache.base, 0);
        assert_eq!(cache.limit, 0);
        assert!(!cache.present);
        assert!(!cache.is_code);
    }
    #[test]
    fn test_segment_cache_real_mode() {
        let cache = SegmentCache::real_mode(0x1000);
        assert_eq!(cache.base, 0x10000);
        assert_eq!(cache.limit, 0xFFFF);
        assert!(cache.present);
        assert!(!cache.is_code);
        assert!(cache.is_writable);
        assert!(!cache.granularity);
        assert!(!cache.big);
    }
    #[test]
    fn test_segment_cache_not_present() {
        let desc: [u8; 8] = [0; 8];
        let cache = SegmentCache::from_descriptor(&desc);
        assert!(!cache.present);
    }
    #[test]
    fn test_segment_cache_from_descriptor() {
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x01, 0x92, 0x00, 0x00];
        let cache = SegmentCache::from_descriptor(&desc);
        assert_eq!(cache.base, 0x00010000);
        assert_eq!(cache.limit, 0x0000FFFF);
        assert!(cache.present);
        assert_eq!(cache.dpl, 0);
        assert!(!cache.is_code);
        assert!(cache.is_writable);
        assert!(!cache.granularity);
        assert!(!cache.big);
    }
    #[test]
    fn test_segment_cache_from_descriptor_code() {
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x10, 0xFA, 0xCF, 0x00];
        let cache = SegmentCache::from_descriptor(&desc);
        assert_eq!(cache.base, 0x00100000);
        assert_eq!(cache.limit, 0x000FFFFF);
        assert!(cache.present);
        assert_eq!(cache.dpl, 3);
        assert!(cache.is_code);
        assert!(cache.is_readable);
        assert!(cache.granularity);
        assert!(cache.big);
    }
    #[test]
    fn test_segment_cache_effective_limit() {
        let cache = SegmentCache {
            limit: 0xFFFF,
            cached_effective_limit: 0xFFFF,
            granularity: false,
            ..Default::default()
        };
        assert_eq!(cache.effective_limit(), 0xFFFF);
        let cache = SegmentCache {
            limit: 0xFFFFF,
            cached_effective_limit: 0xFFFF_FFFF,
            granularity: true,
            ..Default::default()
        };
        assert_eq!(cache.effective_limit(), 0xFFFFFFFF);
    }
    #[test]
    fn test_new_registers_have_protected_mode_fields() {
        let regs = Registers::new();
        assert_eq!(regs.tr, 0);
        assert_eq!(regs.tss_base, 0);
        assert_eq!(regs.tss_limit, 0);
        assert_eq!(regs.ldtr, 0);
        assert_eq!(regs.ldt_base, 0);
        assert_eq!(regs.ldt_limit, 0);
        assert_eq!(regs.cpl, 0);
    }
    #[test]
    fn test_segment_cache_conforming() {
        let desc: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9C, 0x00, 0x00];
        let cache = SegmentCache::from_descriptor(&desc);
        assert!(cache.is_code);
        assert!(cache.is_conforming);
        let desc_non_conf: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x98, 0x00, 0x00];
        let cache = SegmentCache::from_descriptor(&desc_non_conf);
        assert!(cache.is_code);
        assert!(!cache.is_conforming);
        let desc_data_ed: [u8; 8] = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x94, 0x00, 0x00];
        let cache = SegmentCache::from_descriptor(&desc_data_ed);
        assert!(!cache.is_code);
        assert!(!cache.is_conforming);
    }
    #[test]
    fn test_segment_cache_access_parsing() {
        let desc_ro_data: [u8; 8] = [0; 8];
        let mut desc_data = desc_ro_data;
        desc_data[5] = 0x90;
        let cache = SegmentCache::from_descriptor(&desc_data);
        assert!(!cache.is_code);
        assert!(!cache.is_writable);
        desc_data[5] = 0x92;
        let cache = SegmentCache::from_descriptor(&desc_data);
        assert!(!cache.is_code);
        assert!(cache.is_writable);
        desc_data[5] = 0x98;
        let cache = SegmentCache::from_descriptor(&desc_data);
        assert!(cache.is_code);
        assert!(!cache.is_readable);
        desc_data[5] = 0x9A;
        let cache = SegmentCache::from_descriptor(&desc_data);
        assert!(cache.is_code);
        assert!(cache.is_readable);
    }
    #[test]
    fn test_eflags_logic() {
        let mut regs = Registers::new();
        assert!(!regs.is_v86_mode());
        regs.set_v86_mode(true);
        assert!(regs.is_v86_mode());
        assert_eq!(regs.eflags_high & 0x0002, 0x0002);
        regs.set_v86_mode(false);
        assert!(!regs.is_v86_mode());
        assert_eq!(regs.iopl(), 0);
        regs.set_iopl(3);
        assert_eq!(regs.iopl(), 3);
        assert!(regs.flags.contains(Flags::IOPL0 | Flags::IOPL1));
        regs.set_iopl(1);
        assert_eq!(regs.iopl(), 1);
        assert!(regs.flags.contains(Flags::IOPL0));
        assert!(!regs.flags.contains(Flags::IOPL1));
        regs.flags = Flags::CARRY | Flags::ZERO;
        regs.eflags_high = 0xABCD;
        let eflags = regs.eflags();
        assert_eq!(eflags & 0xFFFF, (Flags::CARRY | Flags::ZERO).bits() as u32);
        assert_eq!(eflags >> 16, 0xABCD);
        regs.set_eflags(0x12340201);
        assert_eq!(regs.eflags_high, 0x1234);
        assert_eq!(regs.flags.bits(), 0x0203);
    }
    #[test]
    fn test_segment_cache_base_limit_construction() {
        let desc: [u8; 8] = [0xFF, 0xFF, 0x78, 0x56, 0x34, 0x92, 0xCF, 0x12];
        let cache = SegmentCache::from_descriptor(&desc);
        assert_eq!(cache.base, 0x12345678);
        assert_eq!(cache.limit, 0x000FFFFF);
        assert!(cache.granularity);
        assert!(cache.big);
        assert_eq!(cache.effective_limit(), 0xFFFFFFFF);
    }
    #[test]
    fn test_set_eip_full_32bit() {
        let mut regs = Registers::new();
        regs.set_eip(0xDEADBEEF);
        assert_eq!(regs.eip, 0xDEADBEEF);
    }
    #[test]
    fn test_set_esp_full_32bit() {
        let mut regs = Registers::new();
        regs.set_esp(0xCAFEBABE);
        assert_eq!(regs.esp, 0xCAFEBABE);
    }
    #[test]
    fn test_iopl_all_levels() {
        let mut regs = Registers::new();
        for level in 0..=3 {
            regs.set_iopl(level);
            assert_eq!(regs.iopl(), level);
        }
    }
    #[test]
    fn test_iopl_clamps_to_2_bits() {
        let mut regs = Registers::new();
        regs.set_iopl(0xFF);
        assert_eq!(regs.iopl(), 3);
    }
    #[test]
    fn test_debug_register_defaults() {
        let regs = Registers::new();
        assert_eq!(regs.dr0, 0);
        assert_eq!(regs.dr1, 0);
        assert_eq!(regs.dr2, 0);
        assert_eq!(regs.dr3, 0);
        assert_eq!(regs.dr6, 0xFFFF_0FF0);
        assert_eq!(regs.dr7, 0x0000_0400);
    }
    #[test]
    fn test_cr4_default_zero() {
        let regs = Registers::new();
        assert_eq!(regs.cr4, 0);
    }
    #[test]
    fn test_descriptor_table_register_clone_debug() {
        let dtr = DescriptorTableRegister {
            base: 0x12345678,
            limit: 0xFFFF,
        };
        let cloned = dtr;
        assert_eq!(cloned.base, 0x12345678);
        assert_eq!(cloned.limit, 0xFFFF);
        let dbg = format!("{dtr:?}");
        assert!(dbg.contains("DescriptorTableRegister"));
    }
    #[test]
    fn test_segment_cache_debug_clone() {
        let cache = SegmentCache::real_mode(0x2000);
        let cloned = cache;
        assert_eq!(cloned.base, cache.base);
        let dbg = format!("{cache:?}");
        assert!(dbg.contains("SegmentCache"));
    }
    #[test]
    fn test_registers_debug() {
        let mut regs = Registers::new();
        regs.eax = 0xDEADBEEF;
        regs.cs = 0x1234;
        let dbg = format!("{regs:?}");
        assert!(dbg.contains("eax"));
        assert!(dbg.contains("cs"));
    }
    #[test]
    fn test_effective_limit_small() {
        let cache = SegmentCache {
            limit: 0x100,
            cached_effective_limit: 0x100,
            granularity: false,
            ..Default::default()
        };
        assert_eq!(cache.effective_limit(), 0x100);
    }
    #[test]
    fn test_flag_operand_slots_default() {
        let regs = Registers::new();
        assert_eq!(regs.flag_operand_slots, [0, 0]);
    }
    #[test]
    fn test_set_eip() {
        let mut regs = Registers::new();
        regs.set_eip(0xDEAD_BEEF);
        assert_eq!(regs.eip, 0xDEAD_BEEF);
        assert_eq!(regs.ip(), 0xBEEF);
    }
    #[test]
    fn dr6_dr7_restore_intel_defaults_when_absent_from_snapshot() {
        let regs = Registers::new();
        let mut v: serde_json::Value = serde_json::to_value(&regs).unwrap();
        let obj = v
            .as_object_mut()
            .expect("Registers serializes to a JSON object");
        assert!(
            obj.remove("dr6").is_some(),
            "dr6 should be present pre-trim"
        );
        assert!(
            obj.remove("dr7").is_some(),
            "dr7 should be present pre-trim"
        );
        let restored: Registers = serde_json::from_value(v).unwrap();
        assert_eq!(
            restored.dr6, 0xFFFF_0FF0,
            "dr6 must default to the Intel reset value"
        );
        assert_eq!(
            restored.dr7, 0x0000_0400,
            "dr7 must default to the Intel reset value"
        );
    }
}
