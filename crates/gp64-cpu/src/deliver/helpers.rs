//! Root-semantics copies of the segment and stack helpers that delivery uses.
//!
//! These are NOT the shared `load_cs`/`load_ss`/`try_push*`/`stack_range_ok`
//! in `cpu.rs`, which differ in their fault-reporting contract:
//! the Accessed bit is committed at descriptor read time, a not-present CS is
//! #GP, and so on. Slice S1b converges them on the SDM one divergence at a
//! time; the pushes are done (item (a): per-page frames; item (b): SP/ESP
//! moves only after a successful write).

use super::{extender_trace_enabled, selector_trace_enabled, stack_fault_trace_enabled};
use crate::{Cpu, CpuError, PageAccess, PagedMemory, Result, SegmentCache};

impl Cpu {
    /// The stack range check used by delivery.
    ///
    /// In V86 mode the check applies too, against the fixed V86 segment
    /// limit: SDM Vol. 3A §20.1.1, a V86 segment load sets limit 0xFFFF,
    /// expand-up, 16-bit, so a push past offset 0xFFFF is #SS as in real mode
    /// (S1b item (h); the root `stack_range_ok` still skips it).
    #[inline]
    pub(crate) fn deliver_stack_range_ok(&self, offset: u32, size: u32) -> bool {
        let v86 = self.is_v86_mode();
        let limits_active = self.is_protected_mode()
            || v86
            || (self.is_real_mode() && self.cpu_type.supports_286());
        if !limits_active {
            return true;
        }
        if size == 0 {
            return true;
        }
        let cache = &self.regs.ss_cache;
        let (limit, expand_down, max) = if v86 {
            (0xFFFF, false, 0xFFFF)
        } else {
            (
                cache.effective_limit(),
                !cache.is_code && (cache.access & 0x04) != 0,
                if cache.big { 0xFFFF_FFFF } else { 0xFFFF },
            )
        };
        let end = match offset.checked_add(size - 1) {
            Some(end) => end,
            None => return false,
        };
        let ok = if expand_down {
            offset > limit && end <= max
        } else {
            end <= limit
        };
        if !ok && stack_fault_trace_enabled() {
            tracing::debug!(
                target: "gp64::cpu::stack_fault",
                ss = format_args!("{:04X}", self.regs.ss),
                offset = format_args!("{:08X}", offset),
                size,
                end = format_args!("{:08X}", end),
                limit = format_args!("{:08X}", limit),
                max = format_args!("{:08X}", max),
                expand_down,
                access = format_args!("{:02X}", cache.access),
                big = cache.big,
                "Stack range check failed"
            );
        }
        ok
    }

    /// Root-semantics read-and-commit: the Accessed bit is written at read
    /// time, before the caller validates the descriptor. Kept for the call-gate
    /// CS load; delivery's handler CS and stack SS loads validate first and
    /// commit the Accessed bit afterwards (S1b item (e)).
    pub(crate) fn deliver_read_descriptor_checked_for_segment_load<M: PagedMemory>(
        &self,
        memory: &mut M,
        selector: u16,
    ) -> Result<SegmentCache> {
        let (mut desc, addr) = self.deliver_read_descriptor_checked(memory, selector)?;
        if Self::deliver_needs_accessed_commit(desc[5]) {
            self.deliver_write_accessed(memory, selector, addr, desc[5])?;
            desc[5] |= 0x01;
        }
        Ok(SegmentCache::from_descriptor(&desc))
    }

    /// Whether loading a descriptor with this access byte must set its
    /// Accessed bit: a present code or data (S=1) descriptor that has it clear.
    #[inline]
    pub(crate) fn deliver_needs_accessed_commit(access: u8) -> bool {
        (access & 0x10) != 0 && (access & 0x80) != 0 && (access & 0x01) == 0
    }

    /// Linear address of `selector`'s descriptor (its table limits already
    /// checked by the read that produced the descriptor).
    #[inline]
    pub(crate) fn deliver_descriptor_linear(&self, selector: u16) -> usize {
        let table_base = if (selector & 0x4) == 0 {
            self.regs.gdtr.base as usize
        } else {
            self.regs.ldt_base as usize
        };
        table_base + ((selector >> 3) as usize) * 8
    }

    /// Write the Accessed bit of the descriptor at linear `addr` (access byte
    /// `access`, bit 0 clear). A #PF on the write comes back as an error.
    pub(crate) fn deliver_write_accessed<M: PagedMemory>(
        &self,
        memory: &mut M,
        selector: u16,
        addr: usize,
        access: u8,
    ) -> Result<()> {
        let Ok(linear) = u32::try_from(addr + 5) else {
            return Err(CpuError::GeneralProtectionFault);
        };
        let phys = match self.translate_system_linear_write_checked(memory, linear) {
            Ok(phys) => phys,
            Err(pf) => {
                tracing::debug!(
                    target: "gp64::cpu::segment",
                    selector = format_args!("{:04X}", selector),
                    desc_addr = format_args!("{:08X}", addr),
                    access_linear = format_args!("{:08X}", linear),
                    error_code = pf.error_code,
                    "read_descriptor_checked_for_segment_load: accessed-bit write-back faulted"
                );
                return Err(CpuError::PageFault(pf));
            }
        };
        memory.write_u8(phys, access | 0x01);
        Ok(())
    }

    /// The checked descriptor read without the Accessed-bit write: the raw
    /// descriptor bytes and their linear address.
    pub(crate) fn deliver_read_descriptor_checked<M: PagedMemory>(
        &self,
        memory: &mut M,
        selector: u16,
    ) -> Result<([u8; 8], usize)> {
        let ti = (selector >> 2) & 1;
        let index = (selector >> 3) as usize;

        let (table_base, table_limit) = if ti == 0 {
            (self.regs.gdtr.base as usize, self.regs.gdtr.limit as usize)
        } else {
            (self.regs.ldt_base as usize, self.regs.ldt_limit as usize)
        };

        let desc_offset = index * 8;
        if desc_offset + 7 > table_limit {
            return Err(CpuError::GeneralProtectionFault);
        }

        let Some(addr) = table_base.checked_add(desc_offset) else {
            return Err(CpuError::GeneralProtectionFault);
        };

        let mut desc = [0u8; 8];
        #[allow(clippy::needless_range_loop)]
        for i in 0..8 {
            let Ok(linear) = u32::try_from(addr + i) else {
                return Err(CpuError::GeneralProtectionFault);
            };
            let phys = match self.translate_system_linear_read_checked(memory, linear) {
                Ok(phys) => phys,
                Err(pf) => {
                    tracing::debug!(
                        target: "gp64::cpu::segment",
                        selector = format_args!("{:04X}", selector),
                        ti,
                        index,
                        table_base = format_args!("{:08X}", table_base),
                        table_limit = format_args!("{:04X}", table_limit),
                        desc_addr = format_args!("{:08X}", addr),
                        failed_linear = format_args!("{:08X}", linear),
                        error_code = pf.error_code,
                        "read_descriptor_checked_for_segment_load: descriptor translation failed"
                    );
                    return Err(CpuError::PageFault(pf));
                }
            };
            desc[i] = memory.read_u8(phys);
        }

        Ok((desc, addr))
    }

    /// Side-effect-free check of an interrupt or trap gate's handler CS,
    /// made before delivery changes any state (S1b item (c)).
    ///
    /// SDM Vol. 2A, INT n/INTO/INT3/INT1 Operation (PROTECTED-MODE,
    /// TRAP-OR-INTERRUPT-GATE), with Vol. 3A §6.12.1: the processor validates
    /// the gate's code-segment selector and descriptor before it switches
    /// stacks or pushes the frame, so a bad handler CS faults with the
    /// interrupted state intact. `desc` is the descriptor delivery has already read with
    /// `read_descriptor` (no Accessed-bit write; table limits as in
    /// `deliver_read_descriptor_checked_for_segment_load`), so this adds no
    /// descriptor read. A null selector, a non-code descriptor or a handler
    /// DPL above CPL (S1b item (j2)) is #GP; a
    /// not-present code descriptor is #NP(selector) (S1b item (e)), checked
    /// after the type as the INT n pseudocode orders them. `desc` is then
    /// the descriptor the handler CS load uses ([`Self::deliver_load_handler_cs`]).
    /// The #NP error code without EXT goes to `last_exception_error_code`,
    /// where the #NP consumers read it (S1b item (k)).
    #[inline(never)]
    pub(crate) fn deliver_check_handler_cs(
        &mut self,
        selector: u16,
        desc: &SegmentCache,
    ) -> Result<()> {
        if selector <= 0x0003 || !desc.is_code {
            return Err(CpuError::GeneralProtectionFault);
        }
        // A handler at a less privileged level than the current one, for a
        // conforming or non-conforming target: #GP(error_code(selector, 0,
        // EXT)), with the code-segment type check and before the present
        // check (S1b item (j2); SDM Vol. 2A INT n Operation: "IF descriptor
        // does not indicate a code segment or new code-segment DPL > CPL THEN
        // #GP"). The code without EXT goes to `last_exception_error_code`.
        if desc.dpl > self.regs.cpl {
            self.last_exception_error_code = u32::from(selector & 0xFFFC);
            return Err(CpuError::GeneralProtectionFault);
        }
        if !desc.present {
            // SDM Vol. 2A INT n Operation: #NP(error_code(selector, 0, EXT))
            // (S1b item (e)); the escalation adds EXT for a serial delivery.
            self.last_exception_error_code = u32::from(selector & 0xFFFC);
            return Err(CpuError::SegmentNotPresent(selector));
        }
        Ok(())
    }

    /// Load an interrupt or trap gate's handler CS from `desc`, the
    /// descriptor `deliver_check_handler_cs` already read and validated, so
    /// delivery reads the handler descriptor once (S1b item (e)). The Accessed
    /// bit is committed here, after validation (a #PF on that write comes
    /// back with nothing loaded). The new CPL is `target_cpl` (the target
    /// DPL, or the current CPL for a conforming target) and CS.RPL = CPL; the
    /// gate selector's RPL is ignored (S1b item (j); SDM Vol. 2A INT n
    /// Operation: "CS(RPL) := CPL").
    pub(crate) fn deliver_load_handler_cs<M: PagedMemory>(
        &mut self,
        memory: &mut M,
        selector: u16,
        mut cache: SegmentCache,
        target_cpl: u8,
    ) -> Result<()> {
        if Self::deliver_needs_accessed_commit(cache.access) {
            let addr = self.deliver_descriptor_linear(selector);
            self.deliver_write_accessed(memory, selector, addr, cache.access)?;
            cache.access |= 0x01;
        }
        self.regs.cs = (selector & !0x3) | u16::from(target_cpl);
        self.regs.cs_cache = cache;
        // Setter enforces SDM §20.1 V86 invariant.
        self.regs.set_cpl(target_cpl);
        self.cs_bits = if cache.big { 32 } else { 16 };
        tracing::trace!(
            target: "gp64::cpu::segment",
            selector = format_args!("{:04X}", selector),
            base = format_args!("{:08X}", cache.base),
            limit = format_args!("{:08X}", cache.effective_limit()),
            d_b = if cache.big { 32 } else { 16 },
            "PM load CS"
        );
        Ok(())
    }

    /// The #SS error code for a handler stack SS (from the TSS) that failed to
    /// load because it is not present, or `None` for any other failure class
    /// (S1b item (d3)).
    ///
    /// SDM Vol. 2A, INT n Operation (INTER-PRIVILEGE-LEVEL-INTERRUPT and the
    /// V86 path), with Vol. 3A §6.13: the type, RPL and DPL checks come first
    /// and raise #TS(NewSS); only a descriptor that passes them and is not
    /// present raises #SS(error_code(NewSS, 0, EXT)), i.e.
    /// `(selector & 0xFFFC) | EXT`. EXT is 1 for a hardware interrupt or an
    /// exception delivery and 0 for INT n/INT3/INTO. The #TS classes are left
    /// to the #TS follow-up. Called only on the failure path; reads the
    /// descriptor with `read_descriptor` (no Accessed-bit write).
    pub(crate) fn deliver_tss_ss_not_present_code<M: PagedMemory>(
        &self,
        memory: &M,
        selector: u16,
        target_cpl: u8,
        ext: bool,
    ) -> Option<u32> {
        let desc = self.read_descriptor(memory, selector)?;
        let passes_ts_checks = !desc.is_code
            && desc.is_writable
            && desc.dpl == target_cpl
            && (selector & 3) as u8 == target_cpl;
        if desc.present || !passes_ts_checks {
            return None;
        }
        Some(u32::from(selector & 0xFFFC) | u32::from(ext))
    }

    /// Whether a `bytes`-long frame fits below the current SS:ESP (or SS:SP for
    /// a 16-bit stack) under the SS limit, checked once before any push
    /// (S1b (c2)). A 16-bit stack whose SP would wrap is left to the per-push
    /// checks, which handle the wrap.
    #[inline]
    pub(crate) fn deliver_frame_fits(&self, bytes: u32) -> bool {
        let offset = if self.regs.ss_cache.big {
            self.regs.esp.wrapping_sub(bytes)
        } else {
            let sp = u32::from(self.regs.sp());
            if sp < bytes {
                return true;
            }
            sp - bytes
        };
        self.deliver_stack_range_ok(offset, bytes)
    }

    /// Put SS, its cache, ESP and CPL back after a protected-mode delivery
    /// failed past its stack switch (S1b (c2)). Cold path.
    #[cold]
    #[inline(never)]
    pub(crate) fn deliver_restore_failed_pm_entry(
        &mut self,
        ss: u16,
        ss_cache: SegmentCache,
        esp: u32,
        cpl: u8,
    ) {
        self.regs.ss = ss;
        self.regs.ss_cache = ss_cache;
        self.regs.esp = esp;
        self.regs.set_cpl(cpl);
    }

    /// Put the V86 task's state back after a V86-to-ring-0 delivery failed
    /// past its switch (S1b (c2)): EFLAGS (with VM, so CPL 3), IF, cs_bits,
    /// SS:ESP and its cache, and DS/ES/FS/GS with their caches. Cold path.
    #[cold]
    #[inline(never)]
    pub(crate) fn deliver_restore_failed_v86_entry(
        &mut self,
        eflags: u32,
        interrupts_enabled: bool,
        cs_bits: u32,
        (ss, ss_cache, esp): (u16, SegmentCache, u32),
        [ds, es, fs, gs]: [u16; 4],
        [ds_cache, es_cache, fs_cache, gs_cache]: [SegmentCache; 4],
    ) {
        self.regs.set_eflags(eflags);
        self.regs.set_v86_mode(true);
        self.interrupts_enabled = interrupts_enabled;
        self.cs_bits = cs_bits;
        self.regs.ss = ss;
        self.regs.ss_cache = ss_cache;
        self.regs.esp = esp;
        self.regs.ds = ds;
        self.regs.ds_cache = ds_cache;
        self.regs.es = es;
        self.regs.es_cache = es_cache;
        self.regs.fs = fs;
        self.regs.fs_cache = fs_cache;
        self.regs.gs = gs;
        self.regs.gs_cache = gs_cache;
    }

    /// Load CS and update bitness (16/32-bit) based on descriptor
    pub(crate) fn deliver_load_cs<M: PagedMemory>(
        &mut self,
        memory: &mut M,
        selector: u16,
    ) -> Result<()> {
        if self.is_protected_mode() {
            if selector <= 0x0003 {
                if selector_trace_enabled() {
                    tracing::debug!(
                        target: "gp64::cpu::selector",
                        selector = format_args!("{:04X}", selector),
                        from_cs = format_args!("{:04X}", self.regs.cs),
                        from_ip = format_args!("{:04X}", self.regs.ip()),
                        "load_cs: null selector"
                    );
                }
                return Err(CpuError::GeneralProtectionFault);
            }
            match self.deliver_read_descriptor_checked_for_segment_load(memory, selector) {
                Ok(cache) => {
                    if !cache.present || !cache.is_code {
                        return Err(CpuError::GeneralProtectionFault);
                    }
                    let old_cpl = self.regs.cpl;
                    let new_cpl = if cache.is_conforming {
                        old_cpl
                    } else {
                        (selector & 3) as u8
                    };
                    let new_selector = if cache.is_conforming {
                        (selector & !0x3) | (new_cpl as u16)
                    } else {
                        selector
                    };
                    self.regs.cs = new_selector;
                    self.regs.cs_cache = cache;
                    // Setter enforces SDM §20.1 V86 invariant.
                    self.regs.set_cpl(new_cpl);
                    self.cs_bits = if cache.big { 32 } else { 16 };
                    // Per-CS-load event: trace-level only. At info this fires on every
                    // protected-mode far transfer (call/ret/int/iret/syscall) and floods
                    // info-level file logs. Keep it on a dedicated target for opt-in
                    // segment tracing.
                    tracing::trace!(
                        target: "gp64::cpu::segment",
                        selector = format_args!("{:04X}", selector),
                        base = format_args!("{:08X}", cache.base),
                        limit = format_args!("{:08X}", cache.effective_limit()),
                        d_b = if cache.big { 32 } else { 16 },
                        "PM load CS"
                    );
                }
                Err(CpuError::PageFault(pf)) => return Err(CpuError::PageFault(pf)),
                Err(_) => {
                    if selector_trace_enabled() {
                        tracing::debug!(
                            target: "gp64::cpu::selector",
                            selector = format_args!("{:04X}", selector),
                            from_cs = format_args!("{:04X}", self.regs.cs),
                            from_ip = format_args!("{:04X}", self.regs.ip()),
                            "load_cs: selector has no descriptor"
                        );
                    }
                    if extender_trace_enabled() {
                        let ti = (selector >> 2) & 1;
                        let index = (selector >> 3) as usize;
                        let (table_base, table_limit) = if ti == 0 {
                            (self.regs.gdtr.base as usize, self.regs.gdtr.limit as usize)
                        } else {
                            (self.regs.ldt_base as usize, self.regs.ldt_limit as usize)
                        };
                        let desc_offset = index * 8;
                        tracing::debug!(
                            target: "gp64::dos::dpmi",
                            selector = format_args!("{:04X}", selector),
                            ti,
                            index,
                            table_base = format_args!("{:08X}", table_base),
                            table_limit = format_args!("{:04X}", table_limit),
                            desc_offset = format_args!("{:08X}", desc_offset),
                            "load_cs: selector missing"
                        );
                        if desc_offset + 7 <= table_limit {
                            let addr = table_base + desc_offset;
                            let mut desc = [0u8; 8];
                            for (i, byte) in desc.iter_mut().enumerate() {
                                *byte = memory.read_u8(addr + i);
                            }
                            let parsed = SegmentCache::from_descriptor(&desc);
                            tracing::debug!(
                                target: "gp64::dos::dpmi",
                                desc_bytes = format_args!("{:02X?}", desc),
                                present = parsed.present,
                                code = parsed.is_code,
                                dpl = parsed.dpl,
                                base = format_args!("{:08X}", parsed.base),
                                limit = format_args!("{:08X}", parsed.limit),
                                flags = format_args!("{:02X}", parsed.flags),
                                "load_cs: parsed descriptor"
                            );
                        }
                    }
                    return Err(CpuError::GeneralProtectionFault);
                }
            }
        } else {
            // Real-mode-style segment translation (true real mode or V86):
            // set up a real-mode-shaped cache.
            self.regs.cs = selector;
            self.regs.cs_cache = SegmentCache::real_mode(selector);
            // True real mode (CR0.PE=0) has no privilege levels — pin CPL=0.
            // V86 mode (CR0.PE=1, VM=1) also takes this branch, but per
            // SDM §20.1 V86 must always run at CPL=3. Skip the CPL update
            // there so we don't violate the invariant enforced by
            // `Registers::set_cpl`.
            if !self.regs.is_v86_mode() {
                self.regs.set_cpl(0);
            }
            self.cs_bits = 16;
        }
        Ok(())
    }

    // read_descriptor - delegated via Deref

    /// Load SS with proper descriptor caching (SS has stricter rules than data segments)
    pub(crate) fn deliver_load_ss<M: PagedMemory>(
        &mut self,
        memory: &mut M,
        selector: u16,
    ) -> Result<()> {
        if self.is_protected_mode() && selector <= 0x0003 {
            return Err(CpuError::GeneralProtectionFault);
        }

        if self.is_protected_mode() {
            // Validate first, commit the Accessed bit after (S1b item (e)).
            match self
                .deliver_read_descriptor_checked(memory, selector)
                .map(|(desc, addr)| (SegmentCache::from_descriptor(&desc), addr))
            {
                Ok((mut cache, addr)) => {
                    if !cache.present || cache.is_code || !cache.is_writable {
                        if stack_fault_trace_enabled() {
                            tracing::debug!(
                                target: "gp64::cpu::stack_fault",
                                selector = format_args!("{:04X}", selector),
                                present = cache.present,
                                is_code = cache.is_code,
                                writable = cache.is_writable,
                                access = format_args!("{:02X}", cache.access),
                                "SS load fault"
                            );
                        }
                        return Err(CpuError::StackFault);
                    }
                    let rpl = (selector & 3) as u8;
                    if cache.dpl != self.regs.cpl || rpl != self.regs.cpl {
                        if stack_fault_trace_enabled() {
                            tracing::debug!(
                                target: "gp64::cpu::stack_fault",
                                selector = format_args!("{:04X}", selector),
                                dpl = cache.dpl,
                                cpl = self.regs.cpl,
                                rpl,
                                "SS load privilege fault"
                            );
                        }
                        return Err(CpuError::StackFault);
                    }
                    if Self::deliver_needs_accessed_commit(cache.access) {
                        self.deliver_write_accessed(memory, selector, addr, cache.access)?;
                        cache.access |= 0x01;
                    }
                    self.regs.ss = selector;
                    self.regs.ss_cache = cache;
                    if stack_fault_trace_enabled() {
                        tracing::debug!(
                            target: "gp64::cpu::stack_fault",
                            selector = format_args!("{:04X}", selector),
                            sp = format_args!("{:04X}", self.regs.sp()),
                            esp = format_args!("{:08X}", self.regs.esp),
                            base = format_args!("{:08X}", cache.base),
                            limit = format_args!("{:08X}", cache.effective_limit()),
                            access = format_args!("{:02X}", cache.access),
                            flags = format_args!("{:X}", cache.flags),
                            big = cache.big,
                            cpl = self.regs.cpl,
                            "SS load success"
                        );
                    }
                    tracing::trace!(
                        target: "gp64::cpu::segment",
                        selector = format_args!("{:04X}", selector),
                        base = format_args!("{:08X}", cache.base),
                        limit = format_args!("{:08X}", cache.effective_limit()),
                        "PM load SS"
                    );
                }
                Err(CpuError::PageFault(pf)) => return Err(CpuError::PageFault(pf)),
                Err(_) if self.allow_pm_real_mode_ss => {
                    self.regs.ss = selector;
                    self.regs.ss_cache = SegmentCache::real_mode(selector);
                    // TRACE like its `gp64-cpu` twin: a DPMI client can
                    // take this on every SS load (MDD-BUG-FLU-00625).
                    tracing::trace!(
                        target: "gp64::cpu::segment",
                        selector = format_args!("{:04X}", selector),
                        base = format_args!("{:08X}", self.regs.ss_cache.base),
                        "PM load SS fallback to real-mode base"
                    );
                }
                Err(_) => {
                    return Err(CpuError::GeneralProtectionFault);
                }
            }
        } else {
            self.regs.ss = selector;
            self.regs.ss_cache = SegmentCache::real_mode(selector);
        }
        Ok(())
    }

    /// Write one `N`-byte stack value at SS:`offset`, where the caller has
    /// already checked the span against the SS limit and it does not wrap the
    /// 16-bit offset space. Each byte goes through its own page's translation
    /// (SDM Vol. 3A §4.3-§4.5: a linear address maps through the page that
    /// contains it), so a value that straddles a 4 KiB page lands in both
    /// pages' frames, which need not be adjacent. Every byte is translated
    /// before the first write, so a #PF on the second page writes nothing.
    /// With paging off, or when the span sits in one page, `contiguous` writes
    /// the value at the first byte's address, as before.
    #[inline]
    fn deliver_write_stack_span<const N: usize, M: PagedMemory>(
        &self,
        memory: &mut M,
        offset: u32,
        bytes: [u8; N],
        contiguous: impl FnOnce(&mut M, usize),
    ) -> Result<()> {
        let first = self.translate_stack_addr_full_checked(memory, offset, PageAccess::Write)?;
        if (self.regs.cr0 & 0x8000_0000) == 0 || (first & 0xFFF) <= 0x1000 - N {
            contiguous(memory, first);
            return Ok(());
        }
        let mut addresses = [first; N];
        for (i, address) in addresses.iter_mut().enumerate().skip(1) {
            *address = self.translate_stack_addr_full_checked(
                memory,
                offset.wrapping_add(i as u32),
                PageAccess::Write,
            )?;
        }
        for (address, byte) in addresses.into_iter().zip(bytes) {
            memory.write_u8(address, byte);
        }
        Ok(())
    }

    /// Push a word onto the stack with segment limit checking
    /// Returns StackFault if the push would exceed SS segment limit.
    /// SP/ESP moves only after every byte has translated, so a #PF or #SS
    /// leaves it unchanged (SDM Vol. 3A §6.5: a fault reports the state from
    /// before the faulting operation, so it can be restarted).
    pub(crate) fn deliver_try_push16<M: PagedMemory>(
        &mut self,
        memory: &mut M,
        value: u16,
    ) -> Result<()> {
        if self.is_protected_mode() && self.regs.ss_cache.big {
            // 32-bit stack pointer mode
            let new_esp = self.regs.esp.wrapping_sub(2);
            if !self.deliver_stack_range_ok(new_esp, 2) {
                return Err(CpuError::StackFault);
            }

            self.deliver_write_stack_span(memory, new_esp, value.to_le_bytes(), |m, a| {
                m.write_u16(a, value)
            })?;
            self.regs.esp = new_esp;
        } else {
            // 16-bit stack pointer mode (real mode or protected mode with 16-bit stack)
            let new_sp = self.regs.sp().wrapping_sub(2);

            if !self.deliver_stack_range_ok(new_sp as u32, 2) {
                return Err(CpuError::StackFault);
            }

            // Handle segment wrap: when SP is 0xFFFF, the low byte goes to SS:FFFF
            // and the high byte wraps to SS:0000 (8086 behavior)
            if new_sp == 0xFFFF {
                let bytes = value.to_le_bytes();
                let lo_addr =
                    self.translate_stack_addr_full_checked(memory, 0xFFFF, PageAccess::Write)?;
                let hi_addr =
                    self.translate_stack_addr_full_checked(memory, 0x0000, PageAccess::Write)?;
                memory.write_u8(lo_addr, bytes[0]);
                memory.write_u8(hi_addr, bytes[1]);
            } else {
                self.deliver_write_stack_span(
                    memory,
                    new_sp as u32,
                    value.to_le_bytes(),
                    |m, a| m.write_u16(a, value),
                )?;
            }
            self.regs.set_sp(new_sp);
        }
        Ok(())
    }

    /// Push a dword onto the stack with segment limit checking
    /// Returns StackFault if the push would exceed SS segment limit.
    /// SP/ESP moves only after the write, as for [`Self::deliver_try_push16`].
    pub(crate) fn deliver_try_push32<M: PagedMemory>(
        &mut self,
        memory: &mut M,
        value: u32,
    ) -> Result<()> {
        tracing::debug!(
            target: "gp64::cpu::push32",
            sp_before = format_args!("{:04X}", self.regs.sp()),
            esp_before = format_args!("{:08X}", self.regs.esp),
            ss_big = self.regs.ss_cache.big,
            pm = self.is_protected_mode(),
            value = format_args!("{:08X}", value),
            "try_push32 entry"
        );
        if self.is_protected_mode() && self.regs.ss_cache.big {
            // 32-bit stack pointer mode
            let new_esp = self.regs.esp.wrapping_sub(4);
            if !self.deliver_stack_range_ok(new_esp, 4) {
                return Err(CpuError::StackFault);
            }

            self.deliver_write_stack_span(memory, new_esp, value.to_le_bytes(), |m, a| {
                m.write_u32(a, value)
            })?;
            self.regs.esp = new_esp;
        } else {
            // 16-bit stack pointer mode (real mode or protected mode with 16-bit stack)
            let new_sp = self.regs.sp().wrapping_sub(4);

            if !self.deliver_stack_range_ok(new_sp as u32, 4) {
                return Err(CpuError::StackFault);
            }

            // Handle segment wrap: when SP >= 0xFFFD, some bytes wrap to SS:0000+ (8086 behavior)
            if new_sp >= 0xFFFD {
                let bytes = value.to_le_bytes();
                let mut addresses = [0usize; 4];
                for (i, address) in addresses.iter_mut().enumerate() {
                    let offset = new_sp.wrapping_add(i as u16);
                    *address = self.translate_stack_addr_full_checked(
                        memory,
                        offset as u32,
                        PageAccess::Write,
                    )?;
                }
                for (address, byte) in addresses.into_iter().zip(bytes) {
                    memory.write_u8(address, byte);
                }
            } else {
                self.deliver_write_stack_span(
                    memory,
                    new_sp as u32,
                    value.to_le_bytes(),
                    |m, a| m.write_u32(a, value),
                )?;
            }
            self.regs.set_sp(new_sp);
        }
        tracing::debug!(
            target: "gp64::cpu::push32",
            sp_after = format_args!("{:04X}", self.regs.sp()),
            esp_after = format_args!("{:08X}", self.regs.esp),
            "try_push32 exit"
        );
        Ok(())
    }
}
