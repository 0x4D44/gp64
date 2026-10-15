//! PC `Memory` as an [`X86Machine`] (HLD 2026.10.02 R1/R3, LLD §3; step M1).
//!
//! Each body is today's PC code, moved or delegated verbatim, behind
//! `#[inline(always)]`. The engines do not consume the trait until M2 (the
//! interpreter) and M3 (DRC), so in M1 nothing calls these items on a hot path;
//! the M2 assembly diff is what proves the generic call sites match today's.
//!
//! The ordered access preludes (`write_prelude`, `read_prelude`,
//! `write_epilogue`) carry the machine half of the interpreter's
//! `vga_aware_*` funnels (`crates/gp64-interp/src/lib.rs`), moved verbatim
//! in M2.1 (LLD 2026.10.03 "M2 machine-generic interpreter map" §1.1). The
//! engine keeps its half: the attribution counter, the core duties and the
//! raw store on `Raw`, the checked calls on `Checked`, the byte split on
//! `SplitBytes`, and its write/read observers on every route but `Dropped`.

use gp64_memory_core::machine::__audited::AuditedMachine;
use gp64_memory_core::{
    AccessIntent, AccessPolicyCore, ArchPhys, BackingOffset, BackingPage, DirectSpan, GuestPhys,
    MachineId, PhysPage, PortSync, ReadRoute, WriteRoute, X86Machine,
};
use gp64_types::IoPort;

use crate::{
    range_overlaps, Memory, MemoryCore, VgaCommitKind, BIOS_BASE, EMS_FRAME_BASE, EMS_FRAME_SIZE,
    HIGH_MEM_ALIAS_BASE, HIGH_MEM_ALIAS_END, HIGH_MEM_ALIAS_FOLD_BASE, LOW_MEMORY_LIMIT,
    VIDEO_BASE, VIDEO_WINDOW_64K,
};

/// VGA memory end address (exclusive): the interpreter's `VGA_END`.
const VGA_END: usize = VIDEO_BASE + 0x20000; // 0xC0000

/// The interpreter's `ems_frame_overlaps`, verbatim.
#[inline(always)]
fn ems_frame_overlaps(mem: &Memory, addr: usize, len: usize) -> bool {
    len != 0
        && mem.hle_ems_frame_owned()
        && addr < EMS_FRAME_BASE + EMS_FRAME_SIZE
        && addr.saturating_add(len) > EMS_FRAME_BASE
}

/// The PC per-run access policy: today's interpreter run-entry hoists that the
/// machine owns, field for field (LLD 3.4; `run_ext_inner_core`).
///
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
//
// Field order is deliberate: the five flags the interpreter funnels test sit
// at the byte offsets the pre-M2 `WriteGuards` used (0, 1, 2, 3, 5), so the
// release codegen of every funnel and handler is unchanged.
pub struct PcAccessPolicy {
    pub smc_notifier: bool,
    /// Monotone; engines OR it with the live snoop state (R-AP3).
    pub descriptor_snoop: bool,
    pub traced_writes: bool,

    /// The A20 gate is open: the linear mask is `0xFFFF_FFFF`, else
    /// `!(1 << 20)`. A bool, not the mask, so the policy stays 7 bytes and an
    /// engine's `WriteGuards` fits one register (M2 LLD §8 erratum 7).
    pub a20_enabled: bool,
    pub allow_rom_writes: bool,
    pub write_tracking: bool,
}

impl AccessPolicyCore for PcAccessPolicy {
    #[inline(always)]
    fn smc_notifier(self) -> bool {
        self.smc_notifier
    }

    #[inline(always)]
    fn descriptor_snoop(self) -> bool {
        self.descriptor_snoop
    }

    #[inline(always)]
    fn linear_mask(self) -> u32 {
        if self.a20_enabled {
            0xFFFF_FFFF
        } else {
            !(1u32 << 20)
        }
    }

    #[inline(always)]
    fn diag_write_tracking(self) -> bool {
        self.write_tracking
    }

    #[inline(always)]
    fn diag_traced_writes(self) -> bool {
        self.traced_writes
    }
}

// The register-passed size budget (M2 LLD §8 erratum 7): a policy above
// pointer size is passed by reference under the Rust ABI, and every engine
// call site then builds a stack copy.
const _: () = assert!(core::mem::size_of::<PcAccessPolicy>() <= 8);

// SAFETY: audited against LLD 2026.10.02 §3 for step M1; on the allow-list of
// `tests/config_surface.rs::x86_machine_implementations_are_audited`.
unsafe impl AuditedMachine for Memory {}

// SAFETY: every proof below comes from `direct_host_ram_range` (V1-V4 over
// the whole span, D2 because PC backing offset == guest physical address)
// plus the same intent withholds as `tlb_insert`; `core` returns the one
// embedded core; the policy is the run-entry capture the interpreter already
// relies on (R-AP1/R-AP2).
unsafe impl X86Machine for Memory {
    const MACHINE_ID: MachineId = MachineId::PC;

    type AccessPolicy = PcAccessPolicy;
    type Posted = ();

    const DEVICE_DISPATCH: bool = false;
    const NATIVE_DEVICE_COMMIT: bool = true;
    const WRITE_TRACKING: bool = true;
    const DPMI_MAPPINGS: bool = true;
    /// The PIC and the interrupt poll live outside memory (`timing.rs`).
    const QUIET_POLL_BOUNDARY: bool = false;
    /// Real chipsets return open bus above installed RAM (the fetch arms
    /// predating the trait, kept verbatim).
    const OPEN_BUS_FETCH_ABOVE_RAW_LIMIT: bool = true;

    #[inline(always)]
    fn core(&self) -> &MemoryCore {
        &self.core
    }

    const CORE_OFFSET: usize = core::mem::offset_of!(Memory, core);

    #[inline(always)]
    fn access_policy(&self) -> PcAccessPolicy {
        PcAccessPolicy {
            a20_enabled: self.a20_enabled(),
            smc_notifier: self.has_smc_notifier(),
            descriptor_snoop: self.has_descriptor_snoop(),
            write_tracking: self.is_write_tracking_enabled(),
            traced_writes: self.has_traced_write_config(),

            allow_rom_writes: self.allow_rom_writes,
        }
    }

    /// Clears exactly the address lines the A20 mask clears. Unlike the
    /// interpreter's `u32` AND, this never truncates a flat-memory-layout physical
    /// address above 4 GiB: it equals `Memory::mask_address` for every input.
    #[inline(always)]
    fn phys_mask(p: PcAccessPolicy, a: ArchPhys) -> GuestPhys {
        GuestPhys(a.0 & !((!p.linear_mask()) as usize))
    }

    /// The A20 mask alone: the policy capture reads the diagnostic trace
    /// configuration, which the DRC fetch path must not pay for.
    #[inline(always)]
    fn mask_phys(&self, a: ArchPhys) -> GuestPhys {
        GuestPhys(self.dynrec_mask_physical_address(a.0))
    }

    #[inline(always)]
    fn raw_limit(&self) -> usize {
        self.usable_len()
    }

    /// Today's bulk REP gate (`op_stos` and friends): A20 fully open, and the
    /// span ends at or below the VGA window.
    #[inline(always)]
    fn bulk_raw_limit(p: PcAccessPolicy) -> Option<usize> {
        if p.a20_enabled {
            Some(VIDEO_BASE)
        } else {
            None
        }
    }

    /// The machine half of `vga_aware_write_u8/u16/u32`, in today's order.
    #[inline(always)]
    unsafe fn write_prelude<const W: usize>(
        &mut self,
        p: PcAccessPolicy,
        a: GuestPhys,
        v: u64,
    ) -> WriteRoute {
        let addr = a.0;
        match W {
            1 => {
                if addr >= VIDEO_BASE && addr < VGA_END {
                    // EXP-IQ002 L2 chain-4 fast arm (see the interpreter's u8
                    // funnel): exact Chain4 match, 64K aperture only.
                    if self.vga_native_commit_kind() == Some(VgaCommitKind::Chain4)
                        && addr < VIDEO_BASE + VIDEO_WINDOW_64K
                    {
                        self.interp_commit_vga_chain4_fast(addr, v as u8);
                        return WriteRoute::Handled;
                    }
                    return WriteRoute::Checked;
                }
                // F7: the ROM area drops writes unless real boot opened it.
                if !p.allow_rom_writes && addr >= 0xF0000 && addr < 0x100000 {
                    return WriteRoute::Dropped;
                }
                // RomRegions gate (bug 71) and the option-ROM window.
                if self.is_rom_addr(addr) {
                    return WriteRoute::Dropped;
                }
                if self.option_rom_region_overlaps(addr, 1) {
                    return WriteRoute::Dropped;
                }
                if ems_frame_overlaps(self, addr, 1) {
                    self.write_u8(addr, v as u8);
                    return WriteRoute::HandledReload;
                }
                if addr >= self.usable_len() {
                    return WriteRoute::Checked;
                }
            }
            2 => {
                if (addr >= VIDEO_BASE && addr.wrapping_add(1) < VGA_END)
                    || (addr.wrapping_add(1) >= VIDEO_BASE && addr < VGA_END)
                {
                    // Whole access inside the 64K aperture and not crossing a
                    // chain-4 group of four; otherwise the full-width route.
                    if self.vga_native_commit_kind() == Some(VgaCommitKind::Chain4)
                        && addr >= VIDEO_BASE
                        && addr + 2 <= VIDEO_BASE + VIDEO_WINDOW_64K
                        && (addr & 3) <= 2
                    {
                        let bytes = (v as u16).to_le_bytes();
                        self.interp_commit_vga_chain4_fast(addr, bytes[0]);
                        self.interp_commit_vga_chain4_fast(addr + 1, bytes[1]);
                        return WriteRoute::Handled;
                    }
                    return WriteRoute::Checked;
                }
                // ROM apertures are decoded per byte.
                if (!p.allow_rom_writes && addr < 0x100000 && addr.wrapping_add(1) >= 0xF0000)
                    || self.is_rom_addr(addr)
                    || self.is_rom_addr(addr.wrapping_add(1))
                    || self.option_rom_region_overlaps(addr, 2)
                {
                    return WriteRoute::SplitBytes;
                }
                if ems_frame_overlaps(self, addr, 2) {
                    return WriteRoute::SplitBytes;
                }
                if addr.wrapping_add(1) >= self.usable_len() {
                    return WriteRoute::Checked;
                }
            }
            4 => {
                if (addr >= VIDEO_BASE && addr.wrapping_add(3) < VGA_END)
                    || (addr.wrapping_add(3) >= VIDEO_BASE && addr < VGA_END)
                {
                    // Dword-aligned accesses fill exactly one chain-4 group.
                    if self.vga_native_commit_kind() == Some(VgaCommitKind::Chain4)
                        && addr >= VIDEO_BASE
                        && addr + 4 <= VIDEO_BASE + VIDEO_WINDOW_64K
                        && (addr & 3) == 0
                    {
                        let bytes = (v as u32).to_le_bytes();
                        self.interp_commit_vga_chain4_fast(addr, bytes[0]);
                        self.interp_commit_vga_chain4_fast(addr + 1, bytes[1]);
                        self.interp_commit_vga_chain4_fast(addr + 2, bytes[2]);
                        self.interp_commit_vga_chain4_fast(addr + 3, bytes[3]);
                        return WriteRoute::Handled;
                    }
                    return WriteRoute::Checked;
                }
                if (!p.allow_rom_writes && addr < 0x100000 && addr.wrapping_add(3) >= 0xF0000)
                    || self.is_rom_addr(addr)
                    || self.is_rom_addr(addr.wrapping_add(3))
                    || self.option_rom_region_overlaps(addr, 4)
                {
                    return WriteRoute::SplitBytes;
                }
                if ems_frame_overlaps(self, addr, 4) {
                    return WriteRoute::SplitBytes;
                }
                if addr.wrapping_add(3) >= self.usable_len() {
                    return WriteRoute::Checked;
                }
            }
            _ => return WriteRoute::Checked,
        }
        // Time Machine write tracking routes through the checked path so the
        // write is recorded in the pending_writes buffer (debug builds only,
        // read live, as the funnels do).
        #[cfg(debug_assertions)]
        if self.is_write_tracking_enabled() {
            match W {
                1 => self.write_u8(addr, v as u8),
                2 => self.write_u16(addr, v as u16),
                _ => self.write_u32(addr, v as u32),
            }
            return WriteRoute::HandledReload;
        }
        WriteRoute::Raw
    }

    /// The machine half of `vga_aware_read_u8/u16/u32`, in today's order.
    #[inline(always)]
    unsafe fn read_prelude<const W: usize>(&self, a: GuestPhys) -> ReadRoute {
        let addr = a.0;
        let checked = match W {
            1 => {
                ems_frame_overlaps(self, addr, 1)
                    || addr >= self.usable_len()
                    || (addr >= VIDEO_BASE && addr < VGA_END)
                    || self.option_rom_region_overlaps(addr, 1)
            }
            2 => {
                ems_frame_overlaps(self, addr, 2)
                    || addr.wrapping_add(1) >= self.usable_len()
                    || (addr >= VIDEO_BASE && addr.wrapping_add(1) < VGA_END)
                    || (addr.wrapping_add(1) >= VIDEO_BASE && addr < VGA_END)
                    || self.option_rom_region_overlaps(addr, 2)
            }
            4 => {
                ems_frame_overlaps(self, addr, 4)
                    || self.option_rom_region_overlaps(addr, 4)
                    || (addr.wrapping_add(3) >= VIDEO_BASE && addr < VGA_END)
                    || std::hint::black_box(addr.wrapping_add(3))
                        >= std::hint::black_box(self.usable_len())
            }
            _ => true,
        };
        if checked {
            ReadRoute::Checked
        } else {
            ReadRoute::Raw
        }
    }

    /// The funnels' post-store diagnostics: the selector diagnostic, traced
    /// writes and the descriptor scratch trace. Never guest-visible.
    #[inline(always)]
    fn write_epilogue<const W: usize>(&self, p: PcAccessPolicy, a: GuestPhys, _v: u64) {
        let addr = a.0;
        match W {
            1 if p.traced_writes => {
                self.maybe_trace_linear_write_range(addr, 1, "vga_aware_write_u8");
            }
            2 if p.traced_writes => {
                self.maybe_trace_linear_write_range(addr, 2, "vga_aware_write_u16");
            }
            4 if p.traced_writes => {
                self.maybe_trace_linear_write_range(addr, 4, "vga_aware_write_u32");
            }
            _ => {}
        }
    }

    #[inline(always)]
    fn fetch_alias(&self, a: GuestPhys) -> GuestPhys {
        // The top-of-4-GiB reset-ROM alias, never A20/HMA (Astra §4).
        if self.high_mem_alias_enabled()
            && (HIGH_MEM_ALIAS_BASE..=HIGH_MEM_ALIAS_END).contains(&a.0)
        {
            // Only the reset-vector fetches of a real-boot POST take the
            // alias. The hint keeps the per-fetch fold a well-predicted branch
            // rather than a data-dependent select.
            std::hint::cold_path();
            GuestPhys(HIGH_MEM_ALIAS_FOLD_BASE + (a.0 - HIGH_MEM_ALIAS_BASE))
        } else {
            a
        }
    }

    #[inline(always)]
    fn direct_span(&self, a: GuestPhys, len: usize, intent: AccessIntent) -> Option<DirectSpan> {
        // Fetch callers mask the starting address, but A20 clears bit 20
        // separately for each byte. A direct window cannot cross a 1 MiB
        // bank boundary while the gate is closed; fetch_checked handles it.
        const A20_BANK_SIZE: usize = 1 << 20;
        if intent == AccessIntent::Fetch
            && !self.a20_enabled()
            && len > A20_BANK_SIZE - (a.0 & (A20_BANK_SIZE - 1))
        {
            return None;
        }
        let ptr = self.direct_host_ram_range(a.0, len)?;
        // The same withholds as `tlb_insert`: the firmware MP-table filter
        // rewrites low-memory reads, and W ⊆ R keeps every write proof
        // readable (EXP-A031 Q1).
        let read_withheld = self.firmware_mp_table_suppression_enabled && a.0 < LOW_MEMORY_LIMIT;
        if intent.reads() && read_withheld {
            return None;
        }
        if intent.writes()
            && (read_withheld
                || (!self.allow_rom_writes && range_overlaps(a.0, len, BIOS_BASE, 0x10000))
                || self.descriptor_snoop_overlaps(a.0, len))
        {
            return None;
        }
        // SAFETY: `direct_host_ram_range` proved V1-V4 for `[a, a + len)` and
        // derives `ptr` from the core's RAM base; PC backing == physical (D2).
        Some(unsafe { DirectSpan::from_raw_parts(ptr, BackingOffset(a.0), len, intent) })
    }

    #[inline(always)]
    fn fetch_checked(&self, a: GuestPhys, dst: &mut [u8]) {
        self.read_guest_run(a.0, dst);
    }

    #[inline(always)]
    fn ram_base(&self) -> *const u8 {
        self.ram_ptr()
    }

    /// The core's predicate behind the firmware MP-table filter's global
    /// refusal.
    #[inline(always)]
    fn tlb_direct_read_ptr(&self, linear: u32, width: usize, cpl3: bool) -> Option<*const u8> {
        Memory::tlb_direct_read_ptr(self, linear, width, cpl3)
    }

    #[inline(always)]
    fn tlb_direct_write_span(
        &self,
        linear: u32,
        width: usize,
        cpl3: bool,
    ) -> Option<(*mut u8, usize)> {
        Memory::tlb_direct_write_span(self, linear, width, cpl3)
    }

    #[inline(always)]
    fn backing_page(&self, a: PhysPage) -> Option<BackingPage> {
        (((a.0 as usize) << 12) < self.usable_len()).then_some(BackingPage(a.0))
    }

    #[inline(always)]
    fn set_page_watched(&mut self, page: BackingPage, on: bool) {
        Memory::set_page_watched(self, page.0, on);
    }

    #[inline(always)]
    fn smc_before_write(&mut self, a: GuestPhys, len: usize) -> bool {
        Memory::smc_before_write(self, a.0, len)
    }

    #[inline(always)]
    fn note_direct_write(&mut self, a: GuestPhys, len: usize) {
        self.drc_note_direct_ram_write(a.0, len);
    }

    #[inline(always)]
    fn smc_bind_notifier(&mut self, notifier: Option<std::sync::Arc<dyn gp64_types::SmcNotify>>) {
        Memory::bind_smc(self, notifier);
    }

    #[inline(always)]
    fn smc_set_has_code_pages(&mut self, has_pages: bool) {
        Memory::set_smc_has_code_pages(self, has_pages);
    }

    #[inline(always)]
    fn pt_tripwire_note_physical_write(&mut self, a: GuestPhys, len: usize) {
        Memory::pt_tripwire_note_physical_write(self, a.0, len);
    }

    const DEVICE_WRITE_DELAY: bool = true;

    /// The VGA bus write count the interpreter charges the write delay on.
    #[inline(always)]
    fn device_write_count(&self) -> u64 {
        self.vga_bus_write_count()
    }

    #[inline(always)]
    fn device_transaction_pending(&self) -> bool {
        self.has_pending_s3_mmio_transactions()
    }

    #[inline(always)]
    fn physical_span_is_passive(&self, _a: GuestPhys, _len: usize) -> bool {
        true
    }

    #[inline(always)]
    fn after_port_write<IO: IoPort>(&mut self, io: &mut IO, port: u16) -> PortSync {
        if (0x3B0..=0x3DF).contains(&port) && io.vga_pipeline_dirty() {
            self.apply_vga_pipeline(&io.take_vga_pipeline_state());
            PortSync::Republished
        } else {
            PortSync::None
        }
    }

    #[inline(always)]
    fn has_linear_physical_mappings(&self) -> bool {
        self.has_physical_mappings()
    }

    // ── descriptor-table snoop (M2.4a; the PC bodies stay the inherent
    // methods, which the trait items call) ──────────────────────────────
    #[inline(always)]
    fn descriptor_snoop_set_gdt(&mut self, base: u32, limit: u16) {
        self.set_gdt_snoop_range(base, limit);
    }

    #[inline(always)]
    fn descriptor_snoop_set_ldt(&mut self, base: u32, limit: u32) {
        self.set_ldt_snoop_range(base, limit);
    }

    #[inline(always)]
    fn descriptor_snoop_clear_ldt(&mut self) {
        self.clear_ldt_snoop_range();
    }

    /// The core duty plus the PC GDT-write trace (`snoop_gdt_write_ext`),
    /// exactly as the interpreter's funnels call it today.
    #[inline(always)]
    fn snoop_descriptor_write(&self, addr: usize, val: u64, write_size: u8) {
        self.snoop_gdt_write_ext(addr, val, write_size);
    }

    /// The paged REP bulk paths' plain-RAM proof, verbatim
    /// (`direct_host_ram_range`).
    #[inline(always)]
    fn raw_ram_span(&self, a: GuestPhys, len: usize) -> bool {
        self.direct_host_ram_range(a.0, len).is_some()
    }

    // The gate moved here from the engine (M2.4b). It is the same
    // `has_traced_write_config` value `access_policy` captures, which is a
    // compile-time `false` while the trace config stays retired.
    #[inline(always)]
    fn diag_linear_write_range(&self, start: usize, len: usize, site: &'static str) {
        if self.has_traced_write_config() {
            self.maybe_trace_linear_write_range(start, len, site);
        }
    }

    #[inline(always)]
    fn diag_count_tlb_device_reuse(&mut self) {
        self.count_interp_tlb_device_reuse();
    }

    #[inline(always)]
    fn diag_peek_phys_u8(&self, addr: usize) -> u8 {
        self.read_u8_physical(addr)
    }
}
