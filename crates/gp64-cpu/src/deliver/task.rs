//! Hardware task switching and TSS access, with today's root semantics.

use super::{DeliveryMemory, DeliveryObserver, TaskSwitchSource};
use crate::{Cpu, CpuError, Flags, PageAccess, PagedMemory, Result, SegmentCache};

impl Cpu {
    pub fn read_tss_u16<M: DeliveryMemory<O>, O>(
        &self,
        memory: &mut M,
        offset: u32,
    ) -> Result<u16> {
        let limit = self.regs.tss_limit;
        if offset + 1 > limit {
            return Err(CpuError::GeneralProtectionFault);
        }
        let mut bytes = [0u8; 2];
        for (i, byte) in bytes.iter_mut().enumerate() {
            let addr = self.regs.tss_base.wrapping_add(offset + i as u32);
            // SDM Vol 3 §4.11.3: TSS reads during interrupt delivery / task
            // switches are CPU-initiated implicit supervisor accesses.
            let phys = memory.translate_linear_supervisor_checked(self, addr, PageAccess::Read)?;
            *byte = memory.read_u8(phys);
        }
        Ok(u16::from_le_bytes(bytes))
    }

    pub fn read_tss_u32<M: DeliveryMemory<O>, O>(
        &self,
        memory: &mut M,
        offset: u32,
    ) -> Result<u32> {
        let limit = self.regs.tss_limit;
        if offset + 3 > limit {
            return Err(CpuError::GeneralProtectionFault);
        }
        let mut bytes = [0u8; 4];
        for (i, byte) in bytes.iter_mut().enumerate() {
            let addr = self.regs.tss_base.wrapping_add(offset + i as u32);
            // SDM Vol 3 §4.11.3: TSS reads during interrupt delivery / task
            // switches are CPU-initiated implicit supervisor accesses.
            let phys = memory.translate_linear_supervisor_checked(self, addr, PageAccess::Read)?;
            *byte = memory.read_u8(phys);
        }
        Ok(u32::from_le_bytes(bytes))
    }

    fn current_tss_uses_286_layout<M: DeliveryMemory<O>, O>(&self, memory: &M) -> bool {
        if self.regs.tr == 0 {
            return false;
        }
        self.read_raw_descriptor(memory, self.regs.tr)
            .map(|desc| (desc[5] & 0x10) == 0 && matches!(desc[5] & 0x0F, 0x01 | 0x03))
            .unwrap_or(false)
    }

    pub fn read_tss_stack<M: DeliveryMemory<O>, O>(
        &self,
        memory: &mut M,
        level: u8,
    ) -> Result<(u32, u16)> {
        // Hardware reads the privilege stack pointer from the current TSS
        // during each stack switch. Some V86 monitors update ESP0 dynamically,
        // so the cached snapshot from LTR/task-switch cannot be authoritative.
        let uses_286_layout = self.current_tss_uses_286_layout(memory);
        let (esp_off, ss_off) = match (uses_286_layout, level) {
            (true, 0) => (0x02, 0x04),
            (true, 1) => (0x06, 0x08),
            (true, 2) => (0x0A, 0x0C),
            (false, 0) => (0x04, 0x08),
            (false, 1) => (0x0C, 0x10),
            (false, 2) => (0x14, 0x18),
            (_, _) => return Err(CpuError::GeneralProtectionFault),
        };
        let esp = if uses_286_layout {
            self.read_tss_u16(memory, esp_off)? as u32
        } else {
            self.read_tss_u32(memory, esp_off)?
        };
        let ss = self.read_tss_u16(memory, ss_off)?;
        Ok((esp, ss))
    }

    // =========================================================================
    // TSS write helpers (for hardware task switching)
    // =========================================================================

    #[allow(dead_code)] // Retained for completeness alongside write_tss_u32
    fn write_tss_u16<M: DeliveryMemory<O>, O>(
        &self,
        memory: &mut M,
        offset: u32,
        value: u16,
    ) -> Result<()> {
        let limit = self.regs.tss_limit;
        if offset + 1 > limit {
            return Err(CpuError::GeneralProtectionFault);
        }
        let bytes = value.to_le_bytes();
        for (i, &byte) in bytes.iter().enumerate() {
            let addr = self.regs.tss_base.wrapping_add(offset + i as u32);
            // SDM Vol 3 §4.11.3: TSS writes during task switching are
            // CPU-initiated implicit supervisor accesses.
            let phys = memory.translate_linear_supervisor_checked(self, addr, PageAccess::Write)?;
            memory.write_u8(phys, byte);
        }
        Ok(())
    }

    fn write_tss_u32<M: DeliveryMemory<O>, O>(
        &self,
        memory: &mut M,
        offset: u32,
        value: u32,
    ) -> Result<()> {
        let limit = self.regs.tss_limit;
        if offset + 3 > limit {
            return Err(CpuError::GeneralProtectionFault);
        }
        let bytes = value.to_le_bytes();
        for (i, &byte) in bytes.iter().enumerate() {
            let addr = self.regs.tss_base.wrapping_add(offset + i as u32);
            // SDM Vol 3 §4.11.3: TSS writes during task switching are
            // CPU-initiated implicit supervisor accesses.
            let phys = memory.translate_linear_supervisor_checked(self, addr, PageAccess::Write)?;
            memory.write_u8(phys, byte);
        }
        Ok(())
    }

    // =========================================================================
    // Raw descriptor helpers (for hardware task switching)
    // =========================================================================

    /// Read raw 8-byte descriptor from GDT/LDT.
    /// Returns None if the selector is null (GDT index 0) or outside the table limit.
    pub fn read_raw_descriptor<M: DeliveryMemory<O>, O>(
        &self,
        memory: &M,
        selector: u16,
    ) -> Option<[u8; 8]> {
        let ti = (selector >> 2) & 1;
        if ti == 0 && (selector & 0xFFF8) == 0 {
            return None; // Null GDT selector
        }
        let index = (selector >> 3) as usize;
        let (table_base, table_limit) = if ti == 0 {
            (self.regs.gdtr.base as usize, self.regs.gdtr.limit as usize)
        } else {
            (self.regs.ldt_base as usize, self.regs.ldt_limit as usize)
        };
        let desc_offset = index * 8;
        if desc_offset + 7 > table_limit {
            // For GDT: strict limit enforcement
            if ti == 0 {
                return None;
            }
            // limit without calling LLDT.  Allow reads to LDT memory if
            // the address is within the RAM buffer.
            if table_base == 0 || table_base + desc_offset + 7 >= memory.usable_len() {
                return None;
            }
        }
        let addr = table_base + desc_offset;
        let mut desc = [0u8; 8];
        #[allow(clippy::needless_range_loop)]
        for i in 0..8 {
            let linear = u32::try_from(addr + i).ok()?;
            let phys = self.translate_system_linear_read(memory, linear)?;
            desc[i] = memory.read_u8(phys);
        }
        Some(desc)
    }

    /// Set the Accessed bit of a code or data descriptor the task switch has
    /// just accepted into a segment register (S1b item (f); SDM Vol. 3A
    /// §3.4.5: the bit is set when the selector is loaded). Present S=1
    /// descriptors only; `cache` gets the bit too. The task switch commits in
    /// its load order: CS, SS, then DS, ES, FS, GS. When one fails, those
    /// loaded before it keep their bit and it and the rest get none. That
    /// order is this implementation's; SDM Table 7-1 says the check order is
    /// model specific. A #PF on the write comes back after the commit point.
    fn task_commit_accessed<M: PagedMemory>(
        &self,
        memory: &mut M,
        selector: u16,
        cache: &mut SegmentCache,
    ) -> Result<()> {
        if Self::deliver_needs_accessed_commit(cache.access) {
            let addr = self.deliver_descriptor_linear(selector);
            self.deliver_write_accessed(memory, selector, addr, cache.access)?;
            cache.access |= 0x01;
        }
        Ok(())
    }

    /// Write the access byte (byte 5) of a GDT descriptor.
    /// Used for busy-bit manipulation during task switches.
    fn write_gdt_access_byte<M: PagedMemory>(&self, memory: &mut M, selector: u16, access: u8) {
        let index = (selector >> 3) as usize;
        let linear = self.regs.gdtr.base.wrapping_add((index * 8 + 5) as u32);
        if let Some(phys) = self.translate_system_linear_write(memory, linear) {
            memory.write_u8(phys, access);
        }
    }

    /// Extract base address from a raw 8-byte descriptor.
    pub fn descriptor_base(desc: &[u8; 8]) -> u32 {
        (desc[2] as u32)
            | ((desc[3] as u32) << 8)
            | ((desc[4] as u32) << 16)
            | ((desc[7] as u32) << 24)
    }

    /// Extract limit from a raw 8-byte descriptor, accounting for granularity.
    pub fn descriptor_limit(desc: &[u8; 8]) -> u32 {
        let limit_low = (desc[0] as u32) | ((desc[1] as u32) << 8);
        let limit_high = (desc[6] as u32) & 0x0F;
        let raw_limit = limit_low | (limit_high << 16);
        if (desc[6] & 0x80) != 0 {
            // Granularity bit set: limit is in 4KB pages
            (raw_limit << 12) | 0xFFF
        } else {
            raw_limit
        }
    }

    // =========================================================================
    // Hardware task switching (Intel 386+ TSS-based)
    // =========================================================================

    /// Perform a hardware task switch per the Intel 386 specification.
    ///
    /// `source`: Why the switch is happening (JMP, CALL, INT, IRET)
    /// `new_tss_selector`: GDT selector pointing to the new TSS descriptor
    /// `error_code`: For INT-triggered switches, pushed onto new task's stack
    #[cold]
    ///
    /// Observer-free entry: constructs `O::default()` for this delivery. Kept out of line
    /// (`#[inline(never)]`) to restore the baseline call structure at its callers.
    #[inline(never)]
    pub fn task_switch<M: DeliveryMemory<O>, O: DeliveryObserver<M> + Default>(
        &mut self,
        memory: &mut M,
        source: TaskSwitchSource,
        new_tss_selector: u16,
        error_code: Option<u32>,
    ) -> Result<()> {
        self.task_switch_obs(
            memory,
            &mut O::default(),
            source,
            new_tss_selector,
            error_code,
        )
    }

    /// Observer-carrying form of [`Self::task_switch`].
    pub fn task_switch_obs<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        _obs: &mut O,
        source: TaskSwitchSource,
        new_tss_selector: u16,
        error_code: Option<u32>,
    ) -> Result<()> {
        self.task_switch_calls = self.task_switch_calls.saturating_add(1);
        tracing::debug!(
            target: "gp64::cpu::task_switch",
            source = ?source,
            new_tss_selector = format_args!("{:04X}", new_tss_selector),
            old_tr = format_args!("{:04X}", self.regs.tr),
            "Hardware task switch"
        );

        // =====================================================================
        // Step 0: Self-switch short-circuit
        // =====================================================================
        //
        // When the target TSS selector matches the current TR, the task is
        // already running, so no state transition is needed.
        //
        // a handler TSS.  If the handler itself faults, the exception goes
        // through the same interrupt gate → stub → JMP FAR TSS pattern,
        // targeting the SAME TSS.  Without this short-circuit, the busy
        // check rejects the switch and cascades into #GP.
        if new_tss_selector == self.regs.tr {
            tracing::debug!(
                target: "gp64::cpu::task_switch",
                tr = format_args!("{:04X}", self.regs.tr),
                eip = format_args!("{:08X}", self.regs.eip),
                "Task switch to self — short-circuit"
            );
            if let Some(code) = error_code {
                self.deliver_try_push32(memory, code)?;
            }
            return Ok(());
        }

        // =====================================================================
        // Step 1: Validate new TSS descriptor
        // =====================================================================

        // TSS descriptors must be in the GDT (TI=0)
        if (new_tss_selector & 0x04) != 0 {
            tracing::error!(
                target: "gp64::cpu",
                "Task switch: TSS selector {:04X} has TI=1 (must be in GDT)",
                new_tss_selector
            );
            return Err(CpuError::GeneralProtectionFault);
        }

        let new_tss_desc = match self.read_raw_descriptor(memory, new_tss_selector) {
            Some(desc) => desc,
            None => {
                tracing::error!(
                    target: "gp64::cpu",
                    "Task switch: cannot read TSS descriptor for selector {:04X}",
                    new_tss_selector
                );
                return Err(CpuError::GeneralProtectionFault);
            }
        };

        let new_access = new_tss_desc[5];
        let new_type = new_access & 0x0F;
        let is_system = (new_access & 0x10) == 0; // S bit = 0 for system descriptors
        let is_present = (new_access & 0x80) != 0;

        // Must be a system descriptor with type 0x9 (available 32-bit TSS) or 0xB (busy 32-bit TSS)
        if !is_system || (new_type != 0x09 && new_type != 0x0B) {
            tracing::error!(
                target: "gp64::cpu",
                "Task switch: selector {:04X} is not a 32-bit TSS (access={:02X}, type={:X})",
                new_tss_selector,
                new_access,
                new_type
            );
            return Err(CpuError::GeneralProtectionFault);
        }

        if !is_present {
            tracing::error!(
                target: "gp64::cpu",
                "Task switch: TSS descriptor {:04X} is not present",
                new_tss_selector
            );
            // #NP for not-present TSS; the error code without EXT goes to
            // `last_exception_error_code`, where delivery's #NP consumers
            // read it (S1b item (k)).
            self.last_exception_error_code = u32::from(new_tss_selector & 0xFFFC);
            return Err(CpuError::SegmentNotPresent(new_tss_selector));
        }

        // Busy bit enforcement
        match source {
            TaskSwitchSource::Jmp | TaskSwitchSource::Call | TaskSwitchSource::Int => {
                // New TSS must be available (type 0x9)
                if new_type == 0x0B {
                    tracing::error!(
                        target: "gp64::cpu",
                        "Task switch: TSS {:04X} busy, source={:?} from_TR={:04X} CS:EIP={:04X}:{:08X} DS={:04X}",
                        new_tss_selector,
                        source,
                        self.regs.tr,
                        self.regs.cs,
                        self.regs.eip,
                        self.regs.ds
                    );
                    return Err(CpuError::GeneralProtectionFault);
                }
            }
            TaskSwitchSource::Iret => {
                // New TSS must be busy (type 0xB)
                if new_type == 0x09 {
                    tracing::error!(
                        target: "gp64::cpu",
                        "Task switch: IRET to available TSS {:04X} (type 0x9)",
                        new_tss_selector
                    );
                    return Err(CpuError::GeneralProtectionFault);
                }
            }
        }

        let new_tss_base = Self::descriptor_base(&new_tss_desc);
        let new_tss_limit = Self::descriptor_limit(&new_tss_desc);

        // Minimum TSS size check: must have at least 0x68 bytes (limit >= 0x67)
        if new_tss_limit < 0x67 {
            tracing::error!(
                target: "gp64::cpu",
                "Task switch: TSS {:04X} limit {:X} < 0x67 (too small)",
                new_tss_selector,
                new_tss_limit
            );
            // #TS (Invalid TSS) for too-small TSS
            return Err(CpuError::Interrupt(10));
        }

        // =====================================================================
        // Step 2: Save current state to current TSS
        // =====================================================================

        let old_tr = self.regs.tr;

        // Save EIP
        self.write_tss_u32(memory, 0x20, self.regs.eip)?;
        // Save EFLAGS — IRET must clear NT (bit 14) per Intel spec
        let mut saved_eflags = self.regs.eflags();
        if matches!(source, TaskSwitchSource::Iret) {
            saved_eflags &= !0x4000; // Clear NT (bit 14)
        }
        self.write_tss_u32(memory, 0x24, saved_eflags)?;
        // Save general registers
        self.write_tss_u32(memory, 0x28, self.regs.eax)?;
        self.write_tss_u32(memory, 0x2C, self.regs.ecx)?;
        self.write_tss_u32(memory, 0x30, self.regs.edx)?;
        self.write_tss_u32(memory, 0x34, self.regs.ebx)?;
        self.write_tss_u32(memory, 0x38, self.regs.esp)?;
        self.write_tss_u32(memory, 0x3C, self.regs.ebp)?;
        self.write_tss_u32(memory, 0x40, self.regs.esi)?;
        self.write_tss_u32(memory, 0x44, self.regs.edi)?;
        // Save segment selectors (32-bit writes per Intel spec — upper 16 bits zeroed)
        self.write_tss_u32(memory, 0x48, self.regs.es as u32)?;
        self.write_tss_u32(memory, 0x4C, self.regs.cs as u32)?;
        self.write_tss_u32(memory, 0x50, self.regs.ss as u32)?;
        self.write_tss_u32(memory, 0x54, self.regs.ds as u32)?;
        self.write_tss_u32(memory, 0x58, self.regs.fs as u32)?;
        self.write_tss_u32(memory, 0x5C, self.regs.gs as u32)?;

        // =====================================================================
        // Step 3: Update busy bits in GDT
        // =====================================================================

        // Read old TSS descriptor to manipulate busy bit.
        // Save state for rollback: if any step after this fails, we must
        // restore the GDT busy bits to their pre-switch state.  Real x86
        // validates the new TSS before committing GDT changes, but our
        // implementation does several steps between setting busy bits and
        // validating segments.  Rollback prevents a failed task switch from
        // permanently marking the target TSS as busy, which would cascade
        let mut busy_bit_set = false;
        let mut old_busy_cleared = false;
        let old_access_byte;
        if let Some(old_desc) = self.read_raw_descriptor(memory, old_tr) {
            old_access_byte = old_desc[5];
            match source {
                TaskSwitchSource::Jmp => {
                    // Clear busy bit in old TSS, set in new
                    self.write_gdt_access_byte(memory, old_tr, old_access_byte & !0x02);
                    self.write_gdt_access_byte(memory, new_tss_selector, new_access | 0x02);
                    busy_bit_set = true;
                    old_busy_cleared = true;
                }
                TaskSwitchSource::Call | TaskSwitchSource::Int => {
                    // Keep old TSS busy (nested), set new busy
                    self.write_gdt_access_byte(memory, new_tss_selector, new_access | 0x02);
                    busy_bit_set = true;
                }
                TaskSwitchSource::Iret => {
                    // Clear old busy bit, keep new busy (it's already busy)
                    self.write_gdt_access_byte(memory, old_tr, old_access_byte & !0x02);
                    old_busy_cleared = true;
                }
            }
        } else {
            old_access_byte = 0;
        }
        if busy_bit_set || old_busy_cleared {
            self.mark_gdt_changed();
        }

        // Macro to rollback busy bit changes on error.
        macro_rules! rollback_busy_bits {
            ($self:expr, $memory:expr) => {
                if busy_bit_set {
                    $self.write_gdt_access_byte($memory, new_tss_selector, new_access & !0x02);
                }
                if old_busy_cleared {
                    $self.write_gdt_access_byte($memory, old_tr, old_access_byte);
                }
            };
        }

        // =====================================================================
        // Step 4: Load TR with new selector
        // =====================================================================

        self.regs.tr = new_tss_selector;
        self.regs.tss_base = new_tss_base;
        self.regs.tss_limit = new_tss_limit;

        // =====================================================================
        // Step 5: Load CR3 from new TSS (offset 0x1C)
        // =====================================================================

        let new_cr3 = self.read_tss_u32(memory, 0x1C)?;
        if new_cr3 == self.regs.cr3 {
            self.task_switch_same_cr3 = self.task_switch_same_cr3.saturating_add(1);
        } else {
            self.task_switch_changed_cr3 = self.task_switch_changed_cr3.saturating_add(1);
        }
        self.regs.cr3 = new_cr3;
        // Intel SDM Vol. 3A, Section 7.3: "Loading the PDBR into control
        // register CR3 [during a task switch] causes the contents of the TLB
        // to be invalidated." Even a same-value load invalidates nonglobals;
        // CR4.PGE preserves global translations on identities implementing it.
        if self.cpu_type.supports_pge() && self.regs.cr4 & crate::CR4_PGE != 0 {
            memory.invalidate_tlb_non_global();
        } else {
            memory.invalidate_tlb();
        }

        // =====================================================================
        // Step 6: Load register state from new TSS
        // =====================================================================

        let new_eip = self.read_tss_u32(memory, 0x20)?;
        let new_eflags = self.read_tss_u32(memory, 0x24)?;
        let new_eax = self.read_tss_u32(memory, 0x28)?;
        let new_ecx = self.read_tss_u32(memory, 0x2C)?;
        let new_edx = self.read_tss_u32(memory, 0x30)?;
        let new_ebx = self.read_tss_u32(memory, 0x34)?;
        let new_esp = self.read_tss_u32(memory, 0x38)?;
        let new_ebp = self.read_tss_u32(memory, 0x3C)?;
        let new_esi = self.read_tss_u32(memory, 0x40)?;
        let new_edi = self.read_tss_u32(memory, 0x44)?;
        let new_es = self.read_tss_u16(memory, 0x48)?;
        let new_cs = self.read_tss_u16(memory, 0x4C)?;
        let new_ss = self.read_tss_u16(memory, 0x50)?;
        let new_ds = self.read_tss_u16(memory, 0x54)?;
        let new_fs = self.read_tss_u16(memory, 0x58)?;
        let new_gs = self.read_tss_u16(memory, 0x5C)?;

        // Set EFLAGS (populates both flags and eflags_high)
        self.regs.set_eflags(new_eflags);

        // Set general registers
        self.regs.eip = new_eip;
        self.regs.eax = new_eax;
        self.regs.ecx = new_ecx;
        self.regs.edx = new_edx;
        self.regs.ebx = new_ebx;
        self.regs.esp = new_esp;
        self.regs.ebp = new_ebp;
        self.regs.esi = new_esi;
        self.regs.edi = new_edi;

        // =====================================================================
        // Step 7: Handle nesting (back-link and NT flag)
        // =====================================================================

        match source {
            TaskSwitchSource::Call | TaskSwitchSource::Int => {
                // Write old TR as back-link in new TSS (32-bit per Intel spec)
                self.write_tss_u32(memory, 0x00, old_tr as u32)?;
                // Set NT in new task's EFLAGS
                self.regs.flags.insert(Flags::NESTED);
            }
            TaskSwitchSource::Jmp => {
                // Clear NT — no nesting for JMP
                self.regs.flags.remove(Flags::NESTED);
            }
            TaskSwitchSource::Iret => {
                // Do not modify NT in the restored EFLAGS.
                // The restored task's saved EFLAGS determines NT
                // (may be set if this task is itself nested in a chain).
            }
        }

        // =====================================================================
        // Step 8: Load LDT from new TSS (offset 0x60)
        // =====================================================================

        let new_ldt_selector = self.read_tss_u16(memory, 0x60)?;
        if new_ldt_selector != 0 {
            // Look up LDT descriptor in GDT
            if let Some(ldt_desc) = self.read_raw_descriptor(memory, new_ldt_selector) {
                let ldt_access = ldt_desc[5];
                let ldt_type = ldt_access & 0x0F;
                let ldt_is_system = (ldt_access & 0x10) == 0;
                // LDT descriptor type is 0x02
                if ldt_is_system && ldt_type == 0x02 {
                    let gdt_base = Self::descriptor_base(&ldt_desc);
                    let gdt_limit = Self::descriptor_limit(&ldt_desc);
                    // update the GDT descriptor's limit field.  The CPU's
                    // cached LDTR (from the last LLDT) has the authoritative
                    // limit.  When the selector and base match, keep the
                    // larger of the GDT and cached limits to avoid clobbering
                    // the expanded limit during task switches.
                    let keep_cached = new_ldt_selector == self.regs.ldtr
                        && gdt_base == self.regs.ldt_base
                        && self.regs.ldt_limit > gdt_limit;
                    self.regs.ldtr = new_ldt_selector;
                    self.regs.ldt_base = gdt_base;
                    if !keep_cached {
                        self.regs.ldt_limit = gdt_limit;
                    }
                    memory.set_ldt_snoop_range(self.regs.ldt_base, self.regs.ldt_limit);
                    self.mark_ldt_changed();
                } else {
                    tracing::error!(
                        target: "gp64::cpu",
                        "Task switch: LDT selector {:04X} is not an LDT descriptor (type={:X})",
                        new_ldt_selector,
                        ldt_type
                    );
                    return Err(CpuError::Interrupt(10)); // #TS
                }
            } else {
                tracing::error!(
                    target: "gp64::cpu",
                    "Task switch: cannot read LDT descriptor for selector {:04X}",
                    new_ldt_selector
                );
                return Err(CpuError::Interrupt(10)); // #TS
            }
        } else {
            // Null LDT selector in the new TSS — per Intel 386 manual §7.5,
            // "no change is made to LDTR" when the TSS specifies a null
            // exception handler TSSs typically have LDT=0 (handler runs
            // ring-0 with GDT segments).  If we cleared LDTR here, the
            // return task switch's keep_cached heuristic would fail
            // (selector mismatch 0x0078 != 0), clobbering the expanded
            // LDT limit with the smaller GDT-declared limit.
        }

        // =====================================================================
        // Step 9: Load segment descriptor caches
        // =====================================================================

        let is_v86 = (new_eflags & 0x0002_0000) != 0; // VM bit (bit 17)

        // Set segment selectors first (raw values)
        self.regs.es = new_es;
        self.regs.cs = new_cs;
        self.regs.ss = new_ss;
        self.regs.ds = new_ds;
        // Every selector comes from the new TSS, a null one included (S1b
        // item (g); SDM Vol. 3A §7.3: the segment selectors are loaded from
        // the TSS). The live FS is not kept.
        self.regs.fs = new_fs;
        self.regs.gs = new_gs;

        if is_v86 {
            // V86 mode: segment registers are paragraph addresses, not selectors.
            // Use real-mode-style caches (base = selector * 16, limit = 0xFFFF).
            self.regs.cs_cache = SegmentCache::real_mode(new_cs);
            self.regs.ss_cache = SegmentCache::real_mode(new_ss);
            self.regs.ds_cache = SegmentCache::real_mode(new_ds);
            self.regs.es_cache = SegmentCache::real_mode(new_es);
            self.regs.fs_cache = SegmentCache::real_mode(new_fs);
            self.regs.gs_cache = SegmentCache::real_mode(new_gs);
            self.cs_bits = 16;
            // SDM §20.1: V86 always runs at CPL=3. set_eflags above set
            // VM=1, so the setter takes the V86 path and forces CPL=3.
            self.regs.set_cpl(3);
        } else {
            // Protected mode: load CS descriptor cache — CS must not be null
            if new_cs == 0 {
                tracing::error!(target: "gp64::cpu", "Task switch: new CS is null");
                rollback_busy_bits!(self, memory);
                return Err(CpuError::Interrupt(10)); // #TS
            }
            match self.read_descriptor(memory, new_cs) {
                Some(mut cache) => {
                    self.task_commit_accessed(memory, new_cs, &mut cache)?;
                    self.regs.cs_cache = cache;
                    self.cs_bits = if cache.big { 32 } else { 16 };
                    // Setter enforces SDM §20.1 V86 invariant.
                    self.regs.set_cpl((new_cs & 3) as u8);
                }
                None => {
                    tracing::error!(target: "gp64::cpu", "Task switch: cannot read CS descriptor for {:04X}", new_cs);
                    rollback_busy_bits!(self, memory);
                    return Err(CpuError::Interrupt(10)); // #TS
                }
            }

            // Load SS descriptor cache — SS must not be null
            if new_ss == 0 {
                tracing::error!(target: "gp64::cpu", "Task switch: new SS is null");
                rollback_busy_bits!(self, memory);
                return Err(CpuError::Interrupt(10)); // #TS
            }
            match self.read_descriptor(memory, new_ss) {
                Some(mut cache) if cache.present => {
                    self.task_commit_accessed(memory, new_ss, &mut cache)?;
                    self.regs.ss_cache = cache;
                }
                _ => {
                    // the application TSS before creating PM descriptors.
                    // The initial task switch to the application happens
                    // before DPMI sets up proper selectors.  Fall back to
                    // real-mode base, matching the allow_pm_real_mode_ss
                    // behavior in load_ss().
                    // creating PM descriptors.  Create a flat segment cache
                    // (base=0, limit=4GB) so 32-bit addressing works.
                    self.regs.ss_cache = SegmentCache {
                        base: 0,
                        limit: 0xFFFF_FFFF,
                        cached_effective_limit: 0xFFFF_FFFF,
                        access: 0xF3, // present, DPL=3, writable, expand-up
                        flags: 0xC0,  // G=1, D/B=1
                        is_code: false,
                        is_conforming: false,
                        is_readable: true,
                        is_writable: true,
                        present: true,
                        big: true,
                        granularity: true,
                        dpl: 3,
                    };
                }
            }

            // Load DS/ES/FS/GS — null selectors are acceptable
            let data_segs: [(u16, &str); 4] = [
                (new_ds, "DS"),
                (new_es, "ES"),
                (new_fs, "FS"),
                (new_gs, "GS"),
            ];
            for &(sel, name) in &data_segs {
                // A null selector (index 0, TI 0, any RPL) in DS/ES/FS/GS is
                // legal and loads a null, unusable segment (SDM Vol. 3A
                // §3.4.2, §7.3; S1b item (g)); a later use faults. It neither
                // keeps the live segment nor inherits DS.
                let cache = if sel <= 0x0003 {
                    SegmentCache::default()
                } else {
                    match self.read_descriptor(memory, sel) {
                        Some(mut c) => {
                            self.task_commit_accessed(memory, sel, &mut c)?;
                            c
                        }
                        None => {
                            // before creating PM descriptors.  Create a flat
                            // segment cache (base=0, limit=4GB) for 32-bit
                            SegmentCache {
                                base: 0,
                                limit: 0xFFFF_FFFF,
                                cached_effective_limit: 0xFFFF_FFFF,
                                access: 0xF3,
                                flags: 0xC0, // G=1, D/B=1
                                is_code: false,
                                is_conforming: false,
                                is_readable: true,
                                is_writable: true,
                                present: true,
                                big: true,
                                granularity: true,
                                dpl: 3,
                            }
                        }
                    }
                };
                match name {
                    "DS" => self.regs.ds_cache = cache,
                    "ES" => self.regs.es_cache = cache,
                    "FS" => self.regs.fs_cache = cache,
                    "GS" => self.regs.gs_cache = cache,
                    _ => unreachable!(),
                }
            }
        }

        // =====================================================================
        // Step 10: Update cached SS0:ESP0
        // =====================================================================

        let tss_esp0 = self.read_tss_u32(memory, 0x04)?;
        let tss_ss0 = self.read_tss_u16(memory, 0x08)?;
        self.regs.tss_esp0 = tss_esp0;
        self.regs.tss_ss0 = tss_ss0;

        // =====================================================================
        // Step 11: Sync IP/SP shadows
        // =====================================================================

        // Sync IP/SP shadows: truncate to 16 bits only for 16-bit segments.
        // 32-bit code (CS.D=1) keeps full EIP from the TSS.
        if self.cs_bits == 16 {
            self.regs.eip &= 0xFFFF;
        }
        // SP: set_sp preserves ESP[31:16], matching hardware behavior.
        let new_sp = self.regs.esp as u16;
        self.regs.set_sp(new_sp);

        // Update interrupts_enabled from new EFLAGS IF flag
        self.interrupts_enabled = self.regs.flags.contains(Flags::INTERRUPT);

        // =====================================================================
        // Step 12: Push error code if provided
        // =====================================================================

        if let Some(code) = error_code {
            self.deliver_try_push32(memory, code)?;
        }

        tracing::debug!(
            target: "gp64::cpu::task_switch",
            new_cs = format_args!("{:04X}", self.regs.cs),
            new_eip = format_args!("{:08X}", self.regs.eip),
            new_ss = format_args!("{:04X}", self.regs.ss),
            new_esp = format_args!("{:08X}", self.regs.esp),
            new_tr = format_args!("{:04X}", self.regs.tr),
            cpl = self.regs.cpl,
            "Task switch complete"
        );

        Ok(())
    }
}
