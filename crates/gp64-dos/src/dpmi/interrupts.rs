use super::*;

impl Dpmi {
    pub(super) fn interrupt_services(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        let subfunc = cpu.regs.al();

        match subfunc {
            // Get real mode interrupt vector
            0x00 => {
                let vector = cpu.regs.bl();
                let ivt_addr = (vector as usize) * 4;
                let offset = memory.read_u16(ivt_addr);
                let segment = memory.read_u16(ivt_addr + 2);
                cpu.regs.set_cx(segment);
                cpu.regs.set_dx(offset);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Set real mode interrupt vector
            0x01 => {
                let vector = cpu.regs.bl();
                let segment = cpu.regs.cx();
                let offset = cpu.regs.dx();
                let ivt_addr = (vector as usize) * 4;
                memory.write_u16(ivt_addr, offset);
                memory.write_u16(ivt_addr + 2, segment);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get exception handler vector
            // Always read from exception_handlers[] (set by 0203/0213), NOT from the
            // IDT.  0203 intentionally does NOT write to the IDT because exception
            // vectors 0-31 overlap with hardware interrupt vectors 0-31 in the IDT.
            // Reading from the IDT would return stale HLE stubs or hardware IRQ
            // handlers instead of the exception handler that was set via 0203.
            0x02 => {
                let exception = cpu.regs.bl() as usize;
                if exception < 32 {
                    let handler = &self.exception_handlers[exception];
                    cpu.regs.set_cx(handler.selector);
                    self.set_client_offset_reg(cpu, handler.offset);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE); // Invalid exception number
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Set exception handler vector. AX=0203 handlers use the DPMI
            // 0.9 RETF exception protocol, so the synthetic frame's return
            // slots must match the installed handler code segment width. The
            // must not drive this DPMI 0.9 gate width.
            0x03 => {
                let exception = cpu.regs.bl() as usize;
                if exception < 32 {
                    let selector = self.canonicalize_vector_selector(cpu, memory, cpu.regs.cx());
                    let offset = self.client_offset_reg(cpu);

                    // DPMI 0.9 / 1.0 §5.2.2: a NULL selector for Set Processor
                    // Exception Handler is a request to remove (clear) the user
                    // handler for this exception. The DPMI spec only mandates
                    // exception-number validation here; selector validation is
                    // not required for NULL. the DPMI model
                    // both accept NULL silently with CF=0.
                    //
                    // Note: we do not synchronously rewrite the IDT entry here.
                    // The 0x03 install path also deliberately skips the IDT
                    // (see comment further down at the install branch — exception
                    // dispatch reads `exception_handlers[]` directly to avoid
                    // overwriting hardware-IRQ vectors that share numbers with
                    // exceptions, e.g. INT 08h = #DF and IRQ0). The clear path
                    // is symmetric with that install path.
                    if (selector & 0xFFFC) == 0 {
                        self.exception_handlers[exception] = IdtEntry::default();
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                    if (selector & 0x04) != 0 && !self.selector_in_ldt_range(cpu, selector, 1) {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }

                    let handler_desc = self.vector_target_cache(cpu, memory, selector);
                    let Some(handler_cache) = handler_desc.as_ref() else {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    };
                    if !handler_cache.present {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                    // A DPMI exception handler is entered as CS, so the target
                    // selector must be a CODE segment. Reject data selectors with
                    // CF=1 + AX=0x8022, matching AX=0205h, and leave the handler
                    // table unchanged. MDD-BUG-FLUXHOMEARPA-00011.
                    if !handler_cache.is_code {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                    if !Self::offset_within_segment(handler_cache, offset) {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }

                    // DPMI 0.9 §exception handling: the exception frame format
                    // follows the CLIENT's bitness, not the handler code
                    // register a USE16 handler and still expects the 32-bit
                    // frame (dword slots, 32-bit far return via 66 CB).
                    // 16-bit frame, so its handler read EIP's high word as CS
                    // and re-faulted until the reentry threshold tripped
                    // (MDD-BUG-KILN-00635, C&C instance). Same rule as
                    // AX=0205h below ("Gate size matches DPMI client type").
                    let is_32bit_gate = self.cpu_mode == 1;
                    let gate_type = if is_32bit_gate { 0x0E } else { 0x06 }; // Interrupt gates

                    // Do NOT write exception handlers to the IDT.
                    // Exception dispatch goes through dispatch_dpmi_exception() which reads
                    // the exception_handlers[] table directly. Writing to the IDT would
                    // overwrite hardware interrupt handlers for the same vector numbers
                    // (e.g., exception 8 = #DF would overwrite INT 08h = IRQ0 timer tick).
                    // Only AX=0205 (Set PM Interrupt Vector) should write to the IDT.

                    let diag_handler = cpu.dpmi_exception_diag_should_log_handler(
                        exception as u8,
                        selector,
                        offset,
                    );
                    if dpmi_trace_env_enabled() || diag_handler {
                        let mut bytes = [0u8; 6];
                        let mut read_ok = false;

                        if let Some(cache) = handler_desc.as_ref() {
                            if let Some(linear) = cache.base.checked_add(offset) {
                                let end = linear as usize + bytes.len();
                                if end <= memory.ram_size() {
                                    for (i, byte) in bytes.iter_mut().enumerate() {
                                        *byte = memory.read_u8(linear as usize + i);
                                    }
                                    read_ok = true;
                                } else {
                                    let msg = format!(
                                        "[DPMI] exception handler bytes sel={selector:04X} off={offset:08X} read failed: linear 0x{linear:08X} out of range"
                                    );
                                    if dpmi_trace_env_enabled() {
                                        dpmi_trace_emit(&msg);
                                    }
                                    if diag_handler {
                                        tracing::warn!(
                                            target: "gp64::cpu::dpmi_diag",
                                            exception = format_args!("{:02X}", exception),
                                            selector = format_args!("{:04X}", selector),
                                            offset = format_args!("{:08X}", offset),
                                            msg,
                                            "DPMI exception handler byte read failed"
                                        );
                                    }
                                }
                            } else {
                                let msg = format!(
                                    "[DPMI] exception handler bytes sel={selector:04X} off={offset:08X} read failed: linear overflow"
                                );
                                if dpmi_trace_env_enabled() {
                                    dpmi_trace_emit(&msg);
                                }
                                if diag_handler {
                                    tracing::warn!(
                                        target: "gp64::cpu::dpmi_diag",
                                        exception = format_args!("{:02X}", exception),
                                        selector = format_args!("{:04X}", selector),
                                        offset = format_args!("{:08X}", offset),
                                        msg,
                                        "DPMI exception handler byte read failed"
                                    );
                                }
                            }
                        }

                        if !read_ok {
                            match cpu.translate_address_full_checked(
                                memory,
                                selector,
                                offset,
                                PageAccess::Read,
                            ) {
                                Ok(addr) => {
                                    for (i, byte) in bytes.iter_mut().enumerate() {
                                        *byte = memory.read_u8(addr + i);
                                    }
                                    read_ok = true;
                                }
                                Err(err) => {
                                    let msg = format!(
                                        "[DPMI] exception handler bytes sel={selector:04X} off={offset:08X} read failed: {err:?}"
                                    );
                                    if dpmi_trace_env_enabled() {
                                        dpmi_trace_emit(&msg);
                                    }
                                    if diag_handler {
                                        tracing::warn!(
                                            target: "gp64::cpu::dpmi_diag",
                                            exception = format_args!("{:02X}", exception),
                                            selector = format_args!("{:04X}", selector),
                                            offset = format_args!("{:08X}", offset),
                                            err = ?err,
                                            "DPMI exception handler byte read failed"
                                        );
                                    }
                                }
                            }
                        }

                        if read_ok {
                            let iret32 =
                                bytes.first() == Some(&0x66) && bytes.get(1) == Some(&0xCF);
                            if dpmi_trace_env_enabled() {
                                dpmi_trace_emit(&format!(
                                    "[DPMI] exception handler bytes sel={selector:04X} off={offset:08X} bytes={bytes:02X?} iret32={iret32}"
                                ));
                            }
                            if diag_handler {
                                let handler_bits = handler_desc
                                    .as_ref()
                                    .map(|cache| if cache.big { 32 } else { 16 })
                                    .unwrap_or(0);
                                let (frame_size, err_off, sp_off, ss_off) = if handler_bits == 32 {
                                    (0x80, 0x34u8, 0x68u8, 0x70u8)
                                } else {
                                    (0x60, 0x1Au8, 0x34u8, 0x38u8)
                                };
                                tracing::info!(
                                    target: "gp64::cpu::dpmi_diag",
                                    exception = format_args!("{:02X}", exception),
                                    selector = format_args!("{:04X}", selector),
                                    offset = format_args!("{:08X}", offset),
                                    gate_type = format_args!("{:02X}", gate_type),
                                    handler_bits,
                                    frame_size,
                                    err_off = format_args!("{:02X}", err_off),
                                    sp_off = format_args!("{:02X}", sp_off),
                                    ss_off = format_args!("{:02X}", ss_off),
                                    iret32,
                                    bytes = format_args!("{:02X?}", bytes),
                                    "DPMI exception handler bytes"
                                );
                            }
                        }
                    }

                    if let Some(cache) = handler_desc {
                        tracing::debug!(
                            selector = format_args!("{:04X}", selector),
                            base = format_args!("{:08X}", cache.base),
                            limit = format_args!("{:08X}", cache.limit),
                            access = format_args!("{:02X}", cache.access),
                            flags = format_args!("{:02X}", cache.flags),
                            big = cache.big,
                            is_code = cache.is_code,
                            present = cache.present,
                            "DPMI exception handler descriptor"
                        );
                    } else {
                        tracing::debug!(
                            selector = format_args!("{:04X}", selector),
                            "DPMI exception handler selector has no descriptor"
                        );
                    }
                    tracing::debug!(
                        exception = format_args!("{:02X}", exception),
                        selector = format_args!("{:04X}", selector),
                        offset = format_args!("{:08X}", offset),
                        is_32bit_gate,
                        cpu_mode = self.cpu_mode,
                        gate_type = format_args!("{:02X}", gate_type),
                        "DPMI set exception handler"
                    );

                    self.exception_handlers[exception] = IdtEntry {
                        selector,
                        offset,
                        gate_type,
                        dpl: 3,
                        present: true,
                    };
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE); // Invalid exception number
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // DPMI-111: Get extended exception handler vector (always 32-bit offset)
            0x12 => {
                let exception = cpu.regs.bl() as usize;
                if exception < 32 {
                    let handler = &self.exception_handlers[exception];
                    cpu.regs.set_cx(handler.selector);
                    cpu.regs.edx = handler.offset;
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // DPMI-111: Set extended exception handler vector. The extended
            // form always takes the handler offset from EDX; the DPMI
            // exception frame width follows the client's bitness (see 0x03
            // above / MDD-BUG-KILN-00635).
            0x13 => {
                let exception = cpu.regs.bl() as usize;
                if exception < 32 {
                    let selector = self.canonicalize_vector_selector(cpu, memory, cpu.regs.cx());
                    let offset = cpu.regs.edx;

                    // DPMI 1.0 §5.2.2 (extended variant of 0203h): NULL
                    // selector clears the user handler. Same rationale as
                    // same batched __SetExceptionHandlers helper. Lenient
                    // accept matches the DOS model.
                    //
                    // See: wrk_docs/2026.05.10 - HLD - generic DPMI 8001
                    // unhandled.md.
                    //
                    // Note: the 0x13 install path below also leaves the IDT slot
                    // as-is. Exception dispatch reads `exception_handlers[]`
                    // directly, while raw IDT writes are reserved for 0x05.
                    if (selector & 0xFFFC) == 0 {
                        self.exception_handlers[exception] = IdtEntry::default();
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                    if (selector & 0x04) != 0 && !self.selector_in_ldt_range(cpu, selector, 1) {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }

                    let handler_desc = self.vector_target_cache(cpu, memory, selector);
                    let Some(handler_cache) = handler_desc.as_ref() else {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    };
                    if !handler_cache.present {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                    // A DPMI exception handler is entered as CS, so the target
                    // selector must be a CODE segment. Reject data selectors with
                    // CF=1 + AX=0x8022, matching AX=0205h, and leave the handler
                    // table unchanged. MDD-BUG-FLUXHOMEARPA-00011.
                    if !handler_cache.is_code {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                    if !Self::offset_within_segment(handler_cache, offset) {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }

                    // Frame width follows client bitness, not the handler CS
                    // D bit — see the 0x03 arm above (MDD-BUG-KILN-00635).
                    let is_32bit_gate = self.cpu_mode == 1;
                    let gate_type = if is_32bit_gate { 0x0E } else { 0x06 };

                    self.exception_handlers[exception] = IdtEntry {
                        selector,
                        offset,
                        gate_type,
                        dpl: 3,
                        present: true,
                    };
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Get protected mode interrupt vector
            0x04 => {
                let vector = cpu.regs.bl() as usize;
                if cpu.regs.idtr.base != 0 {
                    let (selector, offset) =
                        self.read_idt_gate(memory, cpu.regs.idtr.base as usize, vector as u8);
                    tracing::trace!(
                        target: "gp64::dos::dpmi",
                        vector = format_args!("{:02X}", vector),
                        idtr_base = format_args!("{:08X}", cpu.regs.idtr.base),
                        selector = format_args!("{:04X}", selector),
                        offset = format_args!("{:08X}", offset),
                        "DPMI 0204 GET PM Vector"
                    );
                    cpu.regs.set_cx(selector);
                    self.set_client_offset_reg(cpu, offset);
                } else {
                    let handler = &self.pm_idt[vector];
                    cpu.regs.set_cx(handler.selector);
                    self.set_client_offset_reg(cpu, handler.offset);
                }
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Set protected mode interrupt vector
            // Use TRAP gates (0xEF/0xE7) instead of interrupt gates (0xEE/0xE6)
            // so that user-installed handlers run with IF preserved.
            //
            // Gate size must match cpu_mode because:
            // - 32-bit DPMI clients call INT from 32-bit code segments (full EIP)
            // - Handler code uses IRETD (0x66 0xCF) even in 16-bit code segments
            // - If we use 16-bit gates, only IP is saved, IRETD reads garbage
            0x05 => {
                let vector = cpu.regs.bl() as usize;
                let selector = self.canonicalize_vector_selector(cpu, memory, cpu.regs.cx());
                let offset = self.client_offset_reg(cpu);

                // DPMI 0.9 / 1.0 §5.2.2: a NULL selector for Set PM Interrupt
                // Vector is a request to remove the user handler. The DPMI
                // spec does not list 8022h (invalid selector) as a return
                // for 0205h. the DOS model accepts NULL silently
                // with CF=0 and leaves the IDT slot as-is — the slot retains
                // whatever was last written, and exception dispatch falls
                // back via the host's HLE BIOS stub if needed.
                //
                // We do not restore the default HLE stub here (matching
                // the DOS model). Only `pm_idt[v]` is cleared.
                //
                // See: wrk_docs/2026.05.10 - HLD - generic DPMI 8001
                // unhandled.md.
                if (selector & 0xFFFC) == 0 {
                    self.pm_idt[vector] = IdtEntry::default();
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                if (selector & 0x04) != 0 && !self.selector_in_ldt_range(cpu, selector, 1) {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let handler_desc = self.vector_target_cache(cpu, memory, selector);
                let Some(handler_cache) = handler_desc.as_ref() else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                };
                if !handler_cache.present {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                if !handler_cache.is_code {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_SELECTOR);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                if !Self::offset_within_segment(handler_cache, offset) {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // Gate size matches DPMI client type (cpu_mode)
                let is_32bit = self.cpu_mode == 1;
                let is_hw_irq = matches!(vector as u8, 0x08..=0x0F | 0x70..=0x77);
                let (type_attr, gate_type) = if is_hw_irq {
                    // Hardware IRQs should use interrupt gates (IF cleared on entry).
                    if is_32bit {
                        (0xEE, 0x0E)
                    } else {
                        (0xE6, 0x06)
                    }
                } else if is_32bit {
                    (0xEF, 0x0F)
                } else {
                    (0xE7, 0x07)
                };

                tracing::debug!(
                    vector = format_args!("{:02X}", vector),
                    selector = format_args!("{:04X}", selector),
                    offset = format_args!("{:08X}", offset),
                    is_32bit,
                    gate_type = format_args!("{:02X}", gate_type),
                    "DPMI set INT vector"
                );

                if cpu.regs.idtr.base != 0 {
                    self.write_idt_gate_full(
                        memory,
                        cpu.regs.idtr.base as usize,
                        vector as u8,
                        offset,
                        selector,
                        type_attr,
                    );
                    // Verify the write by reading it back
                    let (read_sel, read_off) =
                        self.read_idt_gate(memory, cpu.regs.idtr.base as usize, vector as u8);
                    if read_sel != selector || read_off != offset {
                        tracing::error!(
                            target: "gp64::dos::dpmi",
                            vector = format_args!("{:02X}", vector),
                            written_sel = format_args!("{:04X}", selector),
                            written_off = format_args!("{:08X}", offset),
                            read_sel = format_args!("{:04X}", read_sel),
                            read_off = format_args!("{:08X}", read_off),
                            "IDT write verification FAILED!"
                        );
                    }
                }

                self.pm_idt[vector] = IdtEntry {
                    selector,
                    offset,
                    gate_type,
                    dpl: 3,
                    present: true,
                };
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            _ => {
                cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }
        }

        Ok(())
    }

    pub fn exception_handler_gate_type(&self, vector: u8) -> Option<u8> {
        let entry = self.exception_handlers.get(vector as usize)?;
        if entry.present {
            Some(entry.gate_type)
        } else {
            None
        }
    }

    pub fn exception_handler_entry(&self, vector: u8) -> Option<IdtEntry> {
        let entry = *self.exception_handlers.get(vector as usize)?;
        if entry.present {
            Some(entry)
        } else {
            None
        }
    }

    /// Look up a registered protected-mode interrupt handler for a given vector.
    /// Returns the IDT entry if present.  The caller should compare the entry
    /// against the default HLE stub (PM_BIOS_CODE_SEL, 0xFE00 + vector*2) to
    /// decide whether the client installed a custom handler.
    pub fn pm_interrupt_handler(&self, vector: u8) -> Option<IdtEntry> {
        let entry = self.pm_idt[vector as usize];
        if entry.present {
            Some(entry)
        } else {
            None
        }
    }

    /// Get a client PM interrupt handler set via INT 21h/25h.
    pub fn client_pm_int_handler(&self, vector: u8) -> Option<(u16, u32)> {
        self.client_pm_int_handlers[vector as usize]
    }

    /// Set a client PM interrupt handler (from INT 21h/25h translation).
    pub fn set_client_pm_int_handler(&mut self, vector: u8, selector: u16, offset: u32) {
        self.client_pm_int_handlers[vector as usize] = Some((selector, offset));
    }

    /// Replace a vector's INT 21h/25h client handler, `None` restoring the
    /// host default, and answer the handler it replaces. A hooker that
    /// later unhooks writes back what it saved, as TOOLHELP does.
    pub fn replace_client_pm_int_handler(
        &mut self,
        vector: u8,
        handler: Option<(u16, u32)>,
    ) -> Option<(u16, u32)> {
        std::mem::replace(&mut self.client_pm_int_handlers[vector as usize], handler)
    }

    /// Prepare a minimal exception info block below the current SP for 16-bit
    /// exception handlers (gate_type 0x06). The info block is placed at
    /// SS:[SP-0x40] and contains the exception vector number.
    ///
    /// NOTE: Currently unused. The exception dispatch path in src/lib.rs
    /// (dispatch_dpmi_exception) builds the DPMI 0.9 frame directly and does
    #[allow(dead_code)]
    pub fn prepare_exception_pointer(
        &self,
        cpu: &Cpu,
        memory: &mut Memory,
        vector: u8,
    ) -> Result<()> {
        let Some(gate_type) = self.exception_handler_gate_type(vector) else {
            return Ok(());
        };
        if gate_type != 0x06 {
            return Ok(());
        }

        let sp0 = cpu.regs.sp() as u32;
        let ss = cpu.regs.ss;
        let frame_offset = sp0.wrapping_sub(0x40) & 0xFFFF;

        // Bug 2c (paging): Each write must be independently translated through
        // paging when CR0.PG is set, since consecutive linear addresses may map
        // to non-contiguous physical pages.

        // Minimal exception info block: store vector at offset 0, zero-fill a small slice.
        let phys =
            cpu.translate_address_full_checked(memory, ss, frame_offset, PageAccess::Write)?;
        memory.write_u8(phys, vector);
        for i in 1..0x40_u32 {
            let phys = cpu.translate_address_full_checked(
                memory,
                ss,
                frame_offset.wrapping_add(i),
                PageAccess::Write,
            )?;
            memory.write_u8(phys, 0);
        }

        // Store the SS:offset pointer to the info block at the start of
        // the info block itself (offsets +02 and +04) so the exception
        // handler can locate it. Previous code wrote these at SP+4/SP+8
        // which overlapped with the DPMI 0.9 frame's Error Code and CS
        // fields — that was a bug (DPMI-3).
        let phys = cpu.translate_address_full_checked(
            memory,
            ss,
            frame_offset.wrapping_add(2),
            PageAccess::Write,
        )?;
        memory.write_u16(phys, frame_offset as u16);
        let phys = cpu.translate_address_full_checked(
            memory,
            ss,
            frame_offset.wrapping_add(4),
            PageAccess::Write,
        )?;
        memory.write_u16(phys, ss);

        tracing::debug!(
            vector = format_args!("{:02X}", vector),
            ss = format_args!("{:04X}", ss),
            sp = format_args!("{:04X}", cpu.regs.sp()),
            frame_offset = format_args!("{:04X}", frame_offset as u16),
            "DPMI prepared exception pointer"
        );

        Ok(())
    }
}
