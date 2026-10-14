use super::*;

/// What the descriptor-borrow repair changed, and how to change it back.
///
/// MDD-BUG-CRUCIBLE-00443: the repair writes an access byte into the guest's
/// own LDT. A failed raw mode switch must be atomic, and neither the DPMI
/// snapshot (host memory) nor `CpuPmEntrySnapshot` (CPU registers) can undo a
/// write to guest RAM — so the write records its own undo.
#[derive(Clone, Copy, Debug)]
struct CsBorrowRepair {
    /// The CS selector that was repaired.
    selector: u16,
    /// The access byte as the guest had it, before the data→code conversion.
    original_access: u8,
    /// Guest address of the descriptor's access byte. `None` for a GDT
    /// selector, where no descriptor memory is patched.
    descriptor_access_addr: Option<usize>,
}

// KILN-00086: the DPMI control-target CS was env-configured (retired); now None.
fn dpmi_control_target_cs_cfg() -> Option<u16> {
    None
}

#[allow(clippy::too_many_arguments)]
fn maybe_log_dpmi_control_target(
    cpu: &Cpu,
    rm_struct: &RealModeCallStruct,
    rm_struct_addr: usize,
    entry_cs: u16,
    entry_ip: u16,
    rm_ss: u16,
    rm_sp: u16,
    exit_on_iret: bool,
) {
    if dpmi_control_target_cs_cfg() != Some(entry_cs) {
        return;
    }

    tracing::info!(
        target: "gp64::dos::dpmi::control_target",
        from_cs = format_args!("{:04X}", cpu.regs.cs),
        from_eip = format_args!("{:08X}", cpu.regs.eip),
        from_ss = format_args!("{:04X}", cpu.regs.ss),
        from_esp = format_args!("{:08X}", cpu.regs.esp),
        rm_struct_addr = format_args!("{rm_struct_addr:08X}"),
        entry_cs = format_args!("{entry_cs:04X}"),
        entry_ip = format_args!("{entry_ip:04X}"),
        rm_cs = format_args!("{:04X}", rm_struct.cs),
        rm_ip = format_args!("{:04X}", rm_struct.ip),
        rm_ss = format_args!("{rm_ss:04X}"),
        rm_sp = format_args!("{rm_sp:04X}"),
        struct_ss = format_args!("{:04X}", rm_struct.ss),
        struct_sp = format_args!("{:04X}", rm_struct.sp),
        eax = format_args!("{:08X}", rm_struct.eax),
        ebx = format_args!("{:08X}", rm_struct.ebx),
        ecx = format_args!("{:08X}", rm_struct.ecx),
        edx = format_args!("{:08X}", rm_struct.edx),
        ds = format_args!("{:04X}", rm_struct.ds),
        es = format_args!("{:04X}", rm_struct.es),
        flags = format_args!("{:04X}", rm_struct.flags),
        exit_on_iret,
        "DPMI enter_v86 control target"
    );
}

/// Whose stack a real-mode call will run on.
///
/// DPMI 0.9 lets the client pass `SS:SP = 0:0` to mean "host, supply one".
/// The two cases have different rules for how deep a `CX`-word copy may go, so
/// the copy has to be told which it is (MDD-BUG-CRU-01389).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RealModeStack {
    /// The client named `SS:SP` itself. Real-mode `SP` wraps within the
    /// segment, so there is no budget to police — hardware just wraps, and so
    /// does the copy below.
    ClientSupplied,
    /// The host substituted its own stack, so the host knows how big it is and
    /// a copy deeper than that genuinely cannot be honoured.
    HostSupplied,
}

impl Dpmi {
    fn report_v86_entry_error(cpu: &mut Cpu, error_code: u16) {
        cpu.regs.set_ax(error_code);
        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
    }

    pub(super) fn translation_services(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
    ) -> Result<()> {
        let subfunc = cpu.regs.al();

        match subfunc {
            // Simulate real mode interrupt
            0x00 => {
                // BL = interrupt number
                // BH = flags (bit 0 = reset IF)
                // CX = words to copy from PM stack to RM stack
                // ES:EDI = pointer to RealModeCallStruct
                let bh = cpu.regs.bh();
                if (bh & !0x01) != 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let reset_if = (bh & 0x01) != 0;
                let int_num = cpu.regs.bl();
                // DPMI spec: CX = number of words to copy (always 16-bit, even
                // for 32-bit clients). The upper 16 bits of ECX are undefined —
                // real programs (e.g. DJGPP go32 stub) use them as scratch.
                let stack_words = cpu.regs.cx();
                if stack_words > 0x7FFF {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let struct_offset = self.client_offset(cpu);
                let Some(struct_addr) = self.client_pointer_with_access(
                    cpu,
                    memory,
                    cpu.regs.es,
                    struct_offset,
                    PageAccess::Write,
                ) else {
                    tracing::debug!(
                        target: "gp64::dos::dpmi",
                        cpu_mode = self.cpu_mode,
                        es = format_args!("{:04X}", cpu.regs.es),
                        edi = format_args!("{:08X}", cpu.regs.edi),
                        struct_offset = format_args!("{:08X}", struct_offset),
                        access = "write",
                        "DPMI 0300 RMCS pointer resolve failed"
                    );
                    return Ok(());
                };
                let mut rm_struct = RealModeCallStruct::read_from_memory(memory, struct_addr);
                if reset_if {
                    self.apply_translation_reset_if(cpu, &mut rm_struct);
                }

                // Read the IVT handler address first to check if it's HLE
                let ivt_addr = (int_num as usize) * 4;
                let handler_ip = memory.read_u16(ivt_addr);
                let handler_cs = memory.read_u16(ivt_addr + 2);

                if dpmi_trace_env_enabled() {
                    dpmi_trace_emit(&format!(
                        "[DPMI] 0300 RMCS addr={:08X} pm={:04X}:{:08X} int={:02X} reset_if={} stack_words={} handler={:04X}:{:04X} cs:ip={:04X}:{:04X} ss:sp={:04X}:{:04X} flags={:04X} eax={:08X} ebx={:08X} ecx={:08X} edx={:08X} ds={:04X} es={:04X}",
                        struct_addr,
                        cpu.regs.cs,
                        cpu.regs.eip,
                        int_num,
                        reset_if,
                        stack_words,
                        handler_cs,
                        handler_ip,
                        rm_struct.cs,
                        rm_struct.ip,
                        rm_struct.ss,
                        rm_struct.sp,
                        rm_struct.flags,
                        rm_struct.eax,
                        rm_struct.ebx,
                        rm_struct.ecx,
                        rm_struct.edx,
                        rm_struct.ds,
                        rm_struct.es
                    ));
                }
                tracing::debug!(
                    target: "gp64::dos::dpmi",
                    struct_addr = format_args!("{:08X}", struct_addr),
                    pm_cs = format_args!("{:04X}", cpu.regs.cs),
                    pm_eip = format_args!("{:08X}", cpu.regs.eip),
                    int = format_args!("{:02X}", int_num),
                    reset_if = reset_if,
                    stack_words = stack_words,
                    handler_cs = format_args!("{:04X}", handler_cs),
                    handler_ip = format_args!("{:04X}", handler_ip),
                    rm_cs = format_args!("{:04X}", rm_struct.cs),
                    rm_ip = format_args!("{:04X}", rm_struct.ip),
                    rm_ss = format_args!("{:04X}", rm_struct.ss),
                    rm_sp = format_args!("{:04X}", rm_struct.sp),
                    flags = format_args!("{:04X}", rm_struct.flags),
                    eax = format_args!("{:08X}", rm_struct.eax),
                    ebx = format_args!("{:08X}", rm_struct.ebx),
                    ecx = format_args!("{:08X}", rm_struct.ecx),
                    edx = format_args!("{:08X}", rm_struct.edx),
                    ds = format_args!("{:04X}", rm_struct.ds),
                    es = format_args!("{:04X}", rm_struct.es),
                    "DPMI 0300 RMCS"
                );

                // Check if the IVT handler points to an HLE stub (F000:xxxx)
                // If so, we cannot enter V86 mode because hle_iret() would pop
                // the sentinel frame and corrupt the CPU state. Instead, we
                // store a pending request for the emulator to handle directly.
                if handler_cs == 0xF000 {
                    if dpmi_trace_env_enabled() {
                        dpmi_trace_emit(&format!(
                            "[DPMI] 0300 HLE defer int={:02X} handler={:04X}:{:04X} rmcs={:08X}",
                            int_num, handler_cs, handler_ip, struct_addr
                        ));
                    }
                    // Store pending HLE simulation for the emulator to handle
                    self.pending_hle_simulation = Some(PendingHleSimulation {
                        int_num,
                        struct_addr,
                        reset_if,
                    });
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                if let Err(code) = self.preflight_v86_entry() {
                    Self::report_v86_entry_error(cpu, code);
                    return Ok(());
                }

                // Non-HLE handler: set up V86 stack and enter V86 mode
                // SS:SP = 0:0 asks the HOST to supply the stack. It goes at the
                // top of the EBDA rather than at 9FC0:0000, whose very first
                // push wrapped to 0xAFBFE inside the VGA aperture
                // (MDD-BUG-CRU-01386).
                let (rm_ss, mut rm_sp, rm_stack) = if rm_struct.ss == 0 && rm_struct.sp == 0 {
                    (
                        Self::V86_CALLBACK_STACK_SEG,
                        Self::V86_CALLBACK_STACK_TOP,
                        RealModeStack::HostSupplied,
                    )
                } else {
                    (rm_struct.ss, rm_struct.sp, RealModeStack::ClientSupplied)
                };

                rm_sp = match self.copy_pm_stack_to_rm(
                    cpu,
                    memory,
                    rm_ss,
                    rm_sp,
                    stack_words,
                    rm_stack,
                ) {
                    Ok(sp) => sp,
                    Err(code) => {
                        cpu.regs.set_ax(code);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };

                // Push INT return frame: FLAGS, CS, IP (IRET pops IP, CS, FLAGS)
                rm_sp = self.push_rm_word(memory, rm_ss, rm_sp, rm_struct.flags);
                // CS:IP is ignored for 0300h; use sentinel 0000:0000 for return address.
                rm_sp = self.push_rm_word(memory, rm_ss, rm_sp, 0x0000);
                rm_sp = self.push_rm_word(memory, rm_ss, rm_sp, 0x0000);

                if let Err(code) = self.enter_v86(
                    cpu,
                    &rm_struct,
                    struct_addr,
                    handler_cs,
                    handler_ip,
                    rm_ss,
                    rm_sp,
                    true,
                ) {
                    Self::report_v86_entry_error(cpu, code);
                    return Ok(());
                }

                // Report success on the PM return path; do NOT clear the live V86
                // CF the real-mode handler must see (MDD-BUG-CRUCIBLE-00054).
                self.report_v86_translation_success();
            }

            // Call real mode procedure with far return frame
            0x01 => {
                // BH = flags (bit 0 = reset IF)
                // CX = words to copy from PM stack to RM stack
                // ES:EDI = pointer to RealModeCallStruct
                // CS:IP in structure = procedure to call
                let bh = cpu.regs.bh();
                if (bh & !0x01) != 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let reset_if = (bh & 0x01) != 0;
                // DPMI spec: CX = number of words to copy (always 16-bit)
                let stack_words = cpu.regs.cx();
                if stack_words > 0x7FFF {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let Some(struct_addr) = self.client_pointer_with_access(
                    cpu,
                    memory,
                    cpu.regs.es,
                    self.client_offset(cpu),
                    PageAccess::Write,
                ) else {
                    return Ok(());
                };
                let mut rm_struct = RealModeCallStruct::read_from_memory(memory, struct_addr);
                if reset_if {
                    self.apply_translation_reset_if(cpu, &mut rm_struct);
                }

                // Check if the target procedure is an HLE stub (F000:xxxx)
                // If so, we cannot enter V86 mode because hle_retf() would pop
                // the sentinel frame and corrupt the CPU state. Instead, we
                // store a pending request for the emulator to handle directly.
                // The IP offset encodes the interrupt number: int_num = (IP - 0x0100) / 4
                if rm_struct.cs == 0xF000 {
                    // Derive interrupt number from IP offset
                    // IVT_ENTRY_BASE = 0x0100, each entry is 4 bytes apart
                    let int_num = if rm_struct.ip >= 0x0100 {
                        ((rm_struct.ip - 0x0100) / 4) as u8
                    } else {
                        // IP below IVT_ENTRY_BASE - treat as unknown, use INT 0
                        0
                    };
                    // Store pending HLE simulation for the emulator to handle
                    self.pending_hle_simulation = Some(PendingHleSimulation {
                        int_num,
                        struct_addr,
                        reset_if,
                    });
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                if let Err(code) = self.preflight_v86_entry() {
                    Self::report_v86_entry_error(cpu, code);
                    return Ok(());
                }

                // SS:SP = 0:0 asks the HOST to supply the stack. It goes at the
                // top of the EBDA rather than at 9FC0:0000, whose very first
                // push wrapped to 0xAFBFE inside the VGA aperture
                // (MDD-BUG-CRU-01386).
                let (rm_ss, mut rm_sp, rm_stack) = if rm_struct.ss == 0 && rm_struct.sp == 0 {
                    (
                        Self::V86_CALLBACK_STACK_SEG,
                        Self::V86_CALLBACK_STACK_TOP,
                        RealModeStack::HostSupplied,
                    )
                } else {
                    (rm_struct.ss, rm_struct.sp, RealModeStack::ClientSupplied)
                };

                rm_sp = match self.copy_pm_stack_to_rm(
                    cpu,
                    memory,
                    rm_ss,
                    rm_sp,
                    stack_words,
                    rm_stack,
                ) {
                    Ok(sp) => sp,
                    Err(code) => {
                        cpu.regs.set_ax(code);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };

                // Push FAR return frame: CS then IP (RETF pops IP, CS)
                rm_sp = self.push_rm_word(memory, rm_ss, rm_sp, 0x0000);
                rm_sp = self.push_rm_word(memory, rm_ss, rm_sp, 0x0000);

                if let Err(code) = self.enter_v86(
                    cpu,
                    &rm_struct,
                    struct_addr,
                    rm_struct.cs,
                    rm_struct.ip,
                    rm_ss,
                    rm_sp,
                    false,
                ) {
                    Self::report_v86_entry_error(cpu, code);
                    return Ok(());
                }

                // Report success on the PM return path; do NOT clear the live V86
                // CF the real-mode handler must see (MDD-BUG-CRUCIBLE-00054).
                self.report_v86_translation_success();
            }

            // Call real mode procedure with IRET frame
            0x02 => {
                // Same as 0x01 but expects IRET return
                // BH = flags (bit 0 = reset IF)
                let bh = cpu.regs.bh();
                if (bh & !0x01) != 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                // DPMI spec: CX = number of words to copy (always 16-bit)
                let stack_words = cpu.regs.cx();
                if stack_words > 0x7FFF {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let reset_if = (bh & 0x01) != 0;
                let offset = self.client_offset(cpu);
                tracing::debug!(
                    es = format_args!("{:04X}", cpu.regs.es),
                    edi = format_args!("{:08X}", cpu.regs.edi),
                    client_offset = format_args!("{:08X}", offset),
                    cpu_mode = self.cpu_mode,
                    "DPMI 0302 call real mode"
                );
                let Some(struct_addr) = self.client_pointer_with_access(
                    cpu,
                    memory,
                    cpu.regs.es,
                    offset,
                    PageAccess::Write,
                ) else {
                    tracing::debug!(
                        es = format_args!("{:04X}", cpu.regs.es),
                        offset = format_args!("{:08X}", offset),
                        "DPMI 0302 client_pointer failed"
                    );
                    return Ok(());
                };
                let mut rm_struct = RealModeCallStruct::read_from_memory(memory, struct_addr);
                if reset_if {
                    self.apply_translation_reset_if(cpu, &mut rm_struct);
                }
                tracing::debug!(
                    struct_addr = format_args!("{:08X}", struct_addr),
                    cs = format_args!("{:04X}", rm_struct.cs),
                    ip = format_args!("{:04X}", rm_struct.ip),
                    ss = format_args!("{:04X}", rm_struct.ss),
                    sp = format_args!("{:04X}", rm_struct.sp),
                    flags = format_args!("{:04X}", rm_struct.flags),
                    "DPMI 0302 RealModeCallStruct"
                );
                if dpmi_trace_env_enabled() {
                    dpmi_trace_emit(&format!(
                        "[DPMI] 0302 RMCS addr={:08X} cs:ip={:04X}:{:04X} ss:sp={:04X}:{:04X} flags={:04X} eax={:08X} ebx={:08X} ecx={:08X} edx={:08X} ds={:04X} es={:04X} stack_words={}",
                        struct_addr,
                        rm_struct.cs,
                        rm_struct.ip,
                        rm_struct.ss,
                        rm_struct.sp,
                        rm_struct.flags,
                        rm_struct.eax,
                        rm_struct.ebx,
                        rm_struct.ecx,
                        rm_struct.edx,
                        rm_struct.ds,
                        rm_struct.es,
                        stack_words
                    ));
                }

                // Check if the target procedure is an HLE stub (F000:xxxx)
                // If so, we cannot enter V86 mode because hle_iret() would pop
                // the sentinel frame and corrupt the CPU state. Instead, we
                // store a pending request for the emulator to handle directly.
                // The IP offset encodes the interrupt number: int_num = (IP - 0x0100) / 4
                if rm_struct.cs == 0xF000 {
                    // Derive interrupt number from IP offset
                    // IVT_ENTRY_BASE = 0x0100, each entry is 4 bytes apart
                    let int_num = if rm_struct.ip >= 0x0100 {
                        ((rm_struct.ip - 0x0100) / 4) as u8
                    } else {
                        // IP below IVT_ENTRY_BASE - treat as unknown, use INT 0
                        0
                    };
                    // Store pending HLE simulation for the emulator to handle
                    self.pending_hle_simulation = Some(PendingHleSimulation {
                        int_num,
                        struct_addr,
                        reset_if,
                    });
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                if let Err(code) = self.preflight_v86_entry() {
                    Self::report_v86_entry_error(cpu, code);
                    return Ok(());
                }

                // SS:SP = 0:0 asks the HOST to supply the stack. It goes at the
                // top of the EBDA rather than at 9FC0:0000, whose very first
                // push wrapped to 0xAFBFE inside the VGA aperture
                // (MDD-BUG-CRU-01386).
                let (rm_ss, mut rm_sp, rm_stack) = if rm_struct.ss == 0 && rm_struct.sp == 0 {
                    (
                        Self::V86_CALLBACK_STACK_SEG,
                        Self::V86_CALLBACK_STACK_TOP,
                        RealModeStack::HostSupplied,
                    )
                } else {
                    (rm_struct.ss, rm_struct.sp, RealModeStack::ClientSupplied)
                };

                rm_sp = match self.copy_pm_stack_to_rm(
                    cpu,
                    memory,
                    rm_ss,
                    rm_sp,
                    stack_words,
                    rm_stack,
                ) {
                    Ok(sp) => sp,
                    Err(code) => {
                        cpu.regs.set_ax(code);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };

                // Push IRET return frame: FLAGS, CS, IP
                // Use sentinel address 0000:0000 with flags=0 so IRET returns here
                // and we can detect it to exit V86 mode
                rm_sp = self.push_rm_word(memory, rm_ss, rm_sp, 0x0000); // FLAGS
                rm_sp = self.push_rm_word(memory, rm_ss, rm_sp, 0x0000); // CS
                rm_sp = self.push_rm_word(memory, rm_ss, rm_sp, 0x0000); // IP

                if let Err(code) = self.enter_v86(
                    cpu,
                    &rm_struct,
                    struct_addr,
                    rm_struct.cs,
                    rm_struct.ip,
                    rm_ss,
                    rm_sp,
                    true,
                ) {
                    Self::report_v86_entry_error(cpu, code);
                    return Ok(());
                }

                // Report success on the PM return path; do NOT clear the live V86
                // CF the real-mode handler must see (MDD-BUG-CRUCIBLE-00054).
                self.report_v86_translation_success();
            }

            // Allocate real mode callback
            0x03 => {
                // DS:ESI = PM procedure to call
                // ES:EDI = real-mode call structure to use
                tracing::debug!(
                    ds = format_args!("{:04X}", cpu.regs.ds),
                    esi = format_args!("{:08X}", cpu.regs.esi),
                    es = format_args!("{:04X}", cpu.regs.es),
                    edi = format_args!("{:08X}", cpu.regs.edi),
                    "DPMI 0303 allocate callback"
                );
                let pm_selector = cpu.regs.ds;
                let pm_offset = if self.cpu_mode == 1 {
                    cpu.regs.esi
                } else {
                    cpu.regs.si() as u32
                };
                let reg_struct_sel = cpu.regs.es;
                let reg_struct_off = if self.cpu_mode == 1 {
                    cpu.regs.edi
                } else {
                    cpu.regs.di() as u32
                };

                // Find a free callback slot and compute offset from slot index
                // This allows offset recycling when callbacks are freed
                if let Some((slot_idx, slot)) = self
                    .rm_callbacks
                    .iter_mut()
                    .enumerate()
                    .find(|(_, cb)| !cb.in_use)
                {
                    slot.pm_selector = pm_selector;
                    slot.pm_offset = pm_offset;
                    slot.pm_ds = cpu.regs.ds;
                    slot.pm_es = cpu.regs.es;
                    slot.pm_fs = cpu.regs.fs;
                    slot.pm_gs = cpu.regs.gs;
                    slot.reg_struct_selector = reg_struct_sel;
                    slot.reg_struct_offset = reg_struct_off;
                    slot.rm_segment = self.callback_segment;
                    // Compute offset from slot index for proper recycling
                    slot.rm_offset = (slot_idx as u16) * 4;
                    slot.in_use = true;

                    cpu.regs.set_cx(slot.rm_segment);
                    cpu.regs.set_dx(slot.rm_offset);

                    tracing::debug!(
                        cx = format_args!("{:04X}", slot.rm_segment),
                        dx = format_args!("{:04X}", slot.rm_offset),
                        "DPMI 0303 callback allocated"
                    );
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.set_ax(DPMI_ERR_CALLBACK_UNAVAIL); // Callback unavailable
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Free real mode callback
            0x04 => {
                // CX:DX = callback address to free
                let seg = cpu.regs.cx();
                let off = cpu.regs.dx();

                if let Some(slot) = self
                    .rm_callbacks
                    .iter_mut()
                    .find(|cb| cb.in_use && cb.rm_segment == seg && cb.rm_offset == off)
                {
                    slot.in_use = false;
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_CALLBACK); // Invalid callback address
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Get state save/restore addresses
            0x05 => {
                // Returns:
                //   AX = size of state buffer in bytes
                //   BX:CX = real mode save/restore address
                //   SI:EDI = protected mode save/restore address

                // State buffer size per DPMI 0.9 spec (54 bytes for 32-bit client)
                cpu.regs.set_ax(STATE_BUFFER_SIZE_32);

                // Real mode address: F000:FD40
                // NOTE: offset must match HLE_DPMI_STATE_SAVE in src/lib.rs
                cpu.regs.set_bx(0xF000);
                cpu.regs.set_cx(0xFD40);

                // Protected mode address:
                // SI = BIOS code selector (base=0xF0000) to match real-mode stub address
                // EDI/DI = offset to PM stub
                cpu.regs.set_si(PM_BIOS_CODE_SEL);
                if self.cpu_mode == 1 {
                    cpu.regs.edi = 0x000FD40;
                } else {
                    cpu.regs.set_di(0xFD40);
                }

                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get raw mode switch addresses
            0x06 => {
                // Returns:
                //   BX:CX = real-to-protected mode switch address
                //   SI:EDI = protected-to-real mode switch address

                // Real-to-PM: BIOS stub at F000:FD50
                // NOTE: offsets must match HLE_DPMI_RAW_SWITCH_TO_PM/RM in src/lib.rs
                cpu.regs.set_bx(0xF000);
                cpu.regs.set_cx(0xFD50);

                // PM-to-Real: accessible via BIOS code selector (base=0xF0000)
                cpu.regs.set_si(PM_BIOS_CODE_SEL); // GDT[3] with RPL=3 = 0x1B
                if self.cpu_mode == 1 {
                    cpu.regs.edi = 0x000FD58;
                } else {
                    cpu.regs.set_di(0xFD58);
                }

                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            _ => {
                cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }
        }

        Ok(())
    }

    pub(super) fn apply_translation_reset_if(
        &self,
        _cpu: &Cpu,
        rm_struct: &mut RealModeCallStruct,
    ) {
        let if_mask = gp64_cpu::Flags::INTERRUPT.bits();
        // DPMI 0.9 spec: all register fields including flags should be used as-is.
        // flags=0 is a valid value (no flags set), not a sentinel for "use CPU flags".
        rm_struct.flags &= !if_mask;
    }

    /// Handle state save/restore (called from F000:FF40)
    /// AL=0: save DPMI host state to ES:EDI buffer
    /// AL=1: restore DPMI host state from ES:EDI buffer
    ///
    /// Per DPMI 0.9 spec, this saves/restores the DPMI HOST's internal state
    /// needed for safe raw mode switches from interrupt handlers.  It does NOT
    /// save/restore client CPU registers — the client is responsible for its
    /// own register context.
    ///
    /// Our HLE DPMI host tracks mode switch state internally and doesn't need
    /// external bookkeeping, so the save/restore is effectively a NOP.  We
    /// still validate the buffer pointer and write a small signature so callers
    /// that inspect the buffer don't see uninitialized memory.
    pub fn handle_state_save_restore(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        let operation = cpu.regs.al();
        let access = match operation {
            0 => PageAccess::Write,
            1 => PageAccess::Read,
            _ => {
                // Invalid operation - ignore
                return Ok(());
            }
        };
        let Some(buffer) = self.client_pointer_with_access(
            cpu,
            memory,
            cpu.regs.es,
            self.client_offset(cpu),
            access,
        ) else {
            return Ok(());
        };
        let buffer_linear = match u32::try_from(buffer) {
            Ok(addr) => addr,
            Err(_) => {
                cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                return Ok(());
            }
        };
        if let Err(code) =
            self.validate_linear_region(memory, buffer_linear, STATE_BUFFER_SIZE_32 as u32)
        {
            cpu.regs.set_ax(code);
            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            return Ok(());
        }

        match operation {
            // Save DPMI host state to buffer.
            // Write a small signature so the buffer isn't all zeros; the actual
            // host state is tracked internally and doesn't need external storage.
            0 => {
                memory.write_u32(buffer, 0x4D44_5353); // "MDSS" signature
                memory.write_u8(buffer + 4, if self.protected_mode { 1 } else { 0 });
            }
            // Restore DPMI host state from buffer.
            // Our HLE host state is already correct after raw mode switches,
            // so this is intentionally a NOP.  CPU registers are NOT touched.
            1 => {
                // Intentionally empty — host state is maintained internally.
            }
            _ => {}
        }
        Ok(())
    }

    /// The raw switch's new (E)SP and (E)IP: a 16-bit client passes BX and
    /// leaves FFFFh in them).
    fn raw_switch_stack_and_offset(&self, cpu: &Cpu) -> (u32, u32) {
        if self.is_32bit_mode() {
            (cpu.regs.ebx, cpu.regs.edi)
        } else {
            (u32::from(cpu.regs.bx()), u32::from(cpu.regs.di()))
        }
    }

    /// Handle raw mode switch: Real-to-Protected (called from F000:FF50)
    /// Input: AX=new DS, CX=new ES, DX=new SS, (E)BX=new (E)SP, SI=new CS, (E)DI=new (E)IP
    pub fn handle_raw_switch_to_pm(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        let new_ds = cpu.regs.ax();
        let new_es = cpu.regs.cx();
        let new_ss = cpu.regs.dx();
        let (new_esp, new_eip) = self.raw_switch_stack_and_offset(cpu);
        let new_cs = cpu.regs.si();

        tracing::debug!(
            target: "gp64::dos::dpmi",
            "RAW SWITCH RM→PM: DS={:04X} ES={:04X} SS={:04X} ESP={:08X} CS={:04X} EIP={:08X}",
            new_ds, new_es, new_ss, new_esp, new_cs, new_eip
        );

        // DPMI-108: validate selectors before switching state.
        let mut cs_cache = cpu
            .read_descriptor(memory, new_cs)
            .ok_or(gp64_cpu::CpuError::GeneralProtectionFault)?;
        if !cs_cache.present {
            return Err(gp64_cpu::CpuError::GeneralProtectionFault.into());
        }
        // data type during raw mode switches (the program temporarily changed
        // CS to data via INT 31h/000Ch for code copying and didn't restore it
        // before switching to real mode). A real DPMI host runs at ring 0 and
        // can fixup the descriptor table before loading CS. Emulate this by
        // converting data→code: set Execute bit, clear Expand-Down bit.
        //
        // MDD-BUG-CRUCIBLE-00443: this writes into the guest's own LDT, so
        // every path that fails AFTERWARDS has to put the byte back. The DPMI
        // and CPU snapshots taken further down live in host memory and cannot
        // undo a guest write, and the SS/DS/ES checks below return before
        // those snapshots even exist — so a switch rejected for a bad stack
        // used to leave CS permanently converted to executable.
        let mut cs_repair: Option<CsBorrowRepair> = None;
        if !cs_cache.is_code {
            let repair = self.apply_cs_borrow_repair(cpu, memory, new_cs, cs_cache.access);
            // Re-read the now-fixed descriptor
            let Some(fixed) = cpu.read_descriptor(memory, new_cs) else {
                self.undo_cs_borrow_repair(memory, &repair);
                return Err(gp64_cpu::CpuError::GeneralProtectionFault.into());
            };
            cs_cache = fixed;
            cs_repair = Some(repair);
        }

        // Validate the remaining selectors. Every rejection here has to unwind
        // the CS repair above before it propagates.
        let validated = (|| -> Result<u8> {
            let new_cpl = if cs_cache.is_conforming {
                cpu.regs.cpl
            } else {
                (new_cs & 3) as u8
            };
            let ss_cache = cpu
                .read_descriptor(memory, new_ss)
                .ok_or(gp64_cpu::CpuError::StackFault)?;
            if !ss_cache.present || ss_cache.is_code || !ss_cache.is_writable {
                return Err(gp64_cpu::CpuError::StackFault.into());
            }
            if ss_cache.dpl != new_cpl || ((new_ss & 3) as u8) != new_cpl {
                return Err(gp64_cpu::CpuError::StackFault.into());
            }
            for selector in [new_ds, new_es] {
                if selector <= 0x0003 {
                    continue;
                }
                let Some(cache) = cpu.read_descriptor(memory, selector) else {
                    return Err(gp64_cpu::CpuError::GeneralProtectionFault.into());
                };
                if !cache.present || (cache.is_code && !cache.is_readable) {
                    return Err(gp64_cpu::CpuError::GeneralProtectionFault.into());
                }
            }
            Ok(new_cpl)
        })();

        let new_cpl = match validated {
            Ok(new_cpl) => new_cpl,
            Err(err) => {
                if let Some(repair) = &cs_repair {
                    self.undo_cs_borrow_repair(memory, repair);
                }
                return Err(err);
            }
        };

        // Rollback record, not a snapshot of the whole world. The body below
        // mutates exactly ONE field of `self` — `protected_mode` — and does
        // everything else through `cpu`, which `CpuPmEntrySnapshot` already
        // covers. Cloning `Dpmi` to roll that back copied a boxed 65,536-entry
        // selector map plus every other live collection, so each successful
        // switch paid at least a 128 KiB allocation and copy for a rollback it
        // never performed (MDD-BUG-CRUCIBLE-00648).
        let dpmi_protected_mode = self.protected_mode;
        let cpu_snapshot = CpuPmEntrySnapshot::new(cpu);

        let result = (|| -> Result<()> {
            // Switch to protected mode
            self.protected_mode = true;
            cpu.dpmi_active = true;
            cpu.regs.cr0 |= 0x0000_0001; // Set PE bit

            // Load new selectors with proper descriptor caching
            cpu.load_data_segment(memory, gp64_cpu::DataSegReg::DS, new_ds);
            cpu.load_data_segment(memory, gp64_cpu::DataSegReg::ES, new_es);
            cpu.regs.cpl = new_cpl;
            cpu.load_cs(memory, new_cs)?;
            cpu.load_ss(memory, new_ss)?;
            cpu.allow_pm_real_mode_ss = true;
            cpu.regs.eip = new_eip;
            cpu.regs.esp = new_esp;

            // Preserve FS/GS across raw mode switches.  protected-mode clients uses FS
            // to point to its internal data area and expects it to survive
            // PM→RM→PM transitions.  The DPMI spec does not require clearing
            // FS/GS during raw mode switches (they are not parameters of the
            // switch).  Previously we cleared them here, which caused #GP
            // when protected-mode clients's code accessed data through FS after a task
            // switch restored the nulled value.

            // Set IOPL=3 so ring-3 code can execute I/O and sensitive instructions.
            // Without this, IOPL stays at 0 from real mode and all I/O is blocked
            // (TSS bitmap is intentionally beyond TSS limit).
            cpu.regs.set_iopl(3);

            Ok(())
        })();

        if let Err(err) = result {
            self.protected_mode = dpmi_protected_mode;
            cpu_snapshot.restore(cpu);
            // The CS borrow repair ran BEFORE this rollback point, so it was
            // never part of what a snapshot could undo; only a guest write can
            // restore the guest's descriptor byte.
            if let Some(repair) = &cs_repair {
                self.undo_cs_borrow_repair(memory, repair);
            }
            return Err(err);
        }

        Ok(())
    }

    /// Convert a borrowed data-typed CS descriptor to code, and return what is
    /// needed to put it back.
    ///
    /// See the call site for why the undo record exists (MDD-BUG-CRUCIBLE-00443).
    fn apply_cs_borrow_repair(
        &mut self,
        cpu: &Cpu,
        memory: &mut Memory,
        selector: u16,
        original_access: u8,
    ) -> CsBorrowRepair {
        let code_access = (original_access | 0x08) & !0x04;
        // Patch the LDT entry in memory
        let descriptor_access_addr = if (selector & 0x04) != 0 {
            let ldt_index = (selector >> 3) as usize;
            let addr = cpu.regs.ldt_base as usize + ldt_index * 8 + 5;
            memory.write_u8(addr, code_access);
            Some(addr)
        } else {
            None
        };
        // Update the DPMI selector table entry
        if let Some(entry) = self.selectors.iter_mut().find(|s| s.selector == selector) {
            entry.access = code_access;
        }
        tracing::debug!(
            target: "gp64::dos::dpmi",
            selector = format_args!("{:04X}", selector),
            old_access = format_args!("{:02X}", original_access),
            new_access = format_args!("{:02X}", code_access),
            "RM→PM raw switch: fixed descriptor-borrow (data→code)"
        );
        CsBorrowRepair {
            selector,
            original_access,
            descriptor_access_addr,
        }
    }

    /// Restore a descriptor-borrow repair, in guest memory and in the selector
    /// table, so a rejected raw mode switch leaves neither changed.
    fn undo_cs_borrow_repair(&mut self, memory: &mut Memory, repair: &CsBorrowRepair) {
        if let Some(addr) = repair.descriptor_access_addr {
            memory.write_u8(addr, repair.original_access);
        }
        if let Some(entry) = self
            .selectors
            .iter_mut()
            .find(|s| s.selector == repair.selector)
        {
            entry.access = repair.original_access;
        }
        tracing::debug!(
            target: "gp64::dos::dpmi",
            selector = format_args!("{:04X}", repair.selector),
            restored_access = format_args!("{:02X}", repair.original_access),
            "RM→PM raw switch rejected: rolled back descriptor-borrow repair"
        );
    }

    /// Handle raw mode switch: Protected-to-Real (called from F000:FF58)
    /// Input: AX=new DS, CX=new ES, DX=new SS, (E)BX=new (E)SP, SI=new CS, (E)DI=new (E)IP
    pub fn handle_raw_switch_to_rm(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        if !cpu.is_protected_mode() {
            return Err(gp64_cpu::CpuError::GeneralProtectionFault.into());
        }

        let new_ds = cpu.regs.ax();
        let new_es = cpu.regs.cx();
        let new_ss = cpu.regs.dx();
        let (new_esp, new_eip) = self.raw_switch_stack_and_offset(cpu);
        let new_cs = cpu.regs.si();

        tracing::debug!(
            target: "gp64::dos::dpmi",
            "RAW SWITCH PM→RM: DS={:04X} ES={:04X} SS={:04X} ESP={:08X} CS={:04X} EIP={:08X}",
            new_ds, new_es, new_ss, new_esp, new_cs, new_eip
        );

        // Diagnostic: detect raw switch to VGA-range code segment
        if new_cs >= 0xA000 && new_cs < 0xC000 {
            tracing::warn!(
                "=== RAW SWITCH TO RM: VGA-range CS={:04X} IP={:04X} (SI={:04X} EDI={:08X}) ===",
                new_cs,
                new_eip as u16,
                cpu.regs.si(),
                cpu.regs.edi
            );
        }

        // Roll back on failure. As in the RM->PM path above, the body mutates
        // only `self.protected_mode`, so that one field IS the rollback record
        // (MDD-BUG-CRUCIBLE-00648).
        let dpmi_protected_mode = self.protected_mode;
        let cpu_snapshot = CpuPmEntrySnapshot::new(cpu);

        let result = (|| -> Result<()> {
            // Switch to real mode
            self.protected_mode = false;
            cpu.dpmi_active = false;
            cpu.regs.cr0 &= !0x0000_0001; // Clear PE bit

            // Load new segments (real mode - these are segment values, not selectors)
            cpu.load_data_segment(memory, gp64_cpu::DataSegReg::DS, new_ds);
            cpu.load_data_segment(memory, gp64_cpu::DataSegReg::ES, new_es);
            cpu.load_ss(memory, new_ss)?;
            cpu.load_cs(memory, new_cs)?;
            cpu.allow_pm_real_mode_ss = false;
            cpu.regs.eip = new_eip;
            cpu.regs.esp = new_esp;

            // Preserve FS/GS — see RM→PM comment above.

            Ok(())
        })();

        if let Err(err) = result {
            self.protected_mode = dpmi_protected_mode;
            cpu_snapshot.restore(cpu);
            return Err(err);
        }

        Ok(())
    }

    pub fn find_rm_callback(&self, segment: u16, offset: u16) -> Option<RealModeCallback> {
        self.rm_callbacks
            .iter()
            .find(|cb| cb.in_use && cb.rm_segment == segment && cb.rm_offset == offset)
            .copied()
    }

    pub(super) fn copy_pm_stack_to_rm(
        &self,
        cpu: &Cpu,
        memory: &mut Memory,
        rm_ss: u16,
        rm_sp: u16,
        words: u16,
        stack: RealModeStack,
    ) -> std::result::Result<u16, u16> {
        if words == 0 {
            return Ok(rm_sp);
        }
        let bytes = (words as u32) * 2;
        // The old guard read `rm_sp` as a byte budget counted up from offset 0
        // and refused anything deeper. That premise holds for neither case.
        //
        // On a CLIENT stack real-mode `SP` wraps within the segment, so a deep
        // copy is serviced, not refused — `push_rm_word` below already wraps,
        // and so does this function's own arithmetic. On a HOST stack the
        // caller has already substituted the host's own `SS:SP`, so the guard
        // measured the wrong thing entirely: it saw the substituted offset and
        // rejected every `CX > 0` with a spurious 8021h (MDD-BUG-CRU-01389).
        //
        // The budget that does exist belongs to the HOST stack alone, and
        // `rm_sp` is exactly it there: the caller passes the host stack's top
        // offset, and the stack grows down to 0 in its segment. Reading it as
        // a budget on a CLIENT stack is what was wrong, not the arithmetic.
        if stack == RealModeStack::HostSupplied && bytes > u32::from(rm_sp) {
            return Err(DPMI_ERR_INVALID_VALUE);
        }
        let pm_sp = if self.cpu_mode == 1 {
            cpu.regs.esp
        } else {
            cpu.regs.sp() as u32
        };
        let new_rm_sp = rm_sp.wrapping_sub(bytes as u16);
        for i in 0..words {
            let pm_off = pm_sp.wrapping_add((i as u32) * 2);
            let pm_addr = self.get_linear_address(cpu, memory, cpu.regs.ss, pm_off)?;
            let word = memory.read_u16(pm_addr);
            let rm_off = new_rm_sp.wrapping_add(i.wrapping_mul(2));
            let rm_addr = Memory::seg_offset_to_linear(rm_ss, rm_off);
            memory.write_u16(rm_addr, word);
        }
        Ok(new_rm_sp)
    }

    pub(super) fn push_rm_word(
        &self,
        memory: &mut Memory,
        rm_ss: u16,
        rm_sp: u16,
        value: u16,
    ) -> u16 {
        let new_sp = rm_sp.wrapping_sub(2);
        let addr = Memory::seg_offset_to_linear(rm_ss, new_sp);
        memory.write_u16(addr, value);
        new_sp
    }

    pub(super) fn canonicalize_vector_selector(
        &self,
        cpu: &Cpu,
        memory: &Memory,
        selector: u16,
    ) -> u16 {
        if selector == 0 {
            return 0;
        }
        // If already an LDT selector, just ensure RPL=3
        if (selector & 0x04) != 0 {
            return (selector & !0x7) | 0x7;
        }

        // For GDT selectors, we try to find an equivalent LDT selector
        // But only if the LDT descriptor is the same type (code segment for interrupt handlers)
        let index = (selector >> 3) as usize;
        let desc_offset = index * 8;
        let gdt_limit = cpu.regs.gdtr.limit as usize;
        let ldt_limit = cpu.regs.ldt_limit as usize;

        // Check if the GDT entry is out of range but LDT entry is in range
        if cpu.regs.ldt_base != 0 && desc_offset + 7 <= ldt_limit && desc_offset + 7 > gdt_limit {
            let ldt_selector = (selector & !0x7) | 0x7;
            // Verify the LDT entry is a code segment (required for interrupt handlers)
            if let Some(desc) = cpu.read_descriptor(memory, ldt_selector) {
                if desc.present && desc.is_code {
                    tracing::debug!(
                        target: "gp64::dos::dpmi",
                        original = format_args!("{:04X}", selector),
                        canonical = format_args!("{:04X}", ldt_selector),
                        "Canonicalized vector selector (GDT out of range)"
                    );
                    return ldt_selector;
                }
            }
        }

        // An in-range GDT selector is the client's handler as given. It used
        // to be swapped for the same-index LDT selector whenever both entries
        // were code segments, but an index collision between an unrelated
        // GDT and LDT code segment is ordinary (the host's own 001Bh BIOS
        // selector collides with LDT index 3), and the swap sent the next
        // interrupt into the wrong segment (MDD-BUG-CRU-01387).
        selector
    }

    pub(super) fn vector_target_cache(
        &self,
        cpu: &Cpu,
        memory: &Memory,
        selector: u16,
    ) -> Option<SegmentCache> {
        if (selector & 0x04) != 0 {
            if let Some(cache) = self.selector_cache_from_ldt(selector) {
                return Some(cache);
            }
        }
        cpu.read_descriptor(memory, selector)
    }

    pub(super) fn offset_within_segment(cache: &SegmentCache, offset: u32) -> bool {
        let effective_limit = cache.effective_limit();
        let expand_down = !cache.is_code && (cache.access & 0x04) != 0;
        let max = if cache.big { 0xFFFF_FFFF } else { 0xFFFF };
        if expand_down {
            offset > effective_limit && offset <= max
        } else {
            offset <= effective_limit
        }
    }

    pub(super) fn env_limit_from_mcb(memory: &Memory, env_segment: u16) -> Option<u32> {
        if env_segment == 0 {
            return None;
        }
        let env_base = (env_segment as usize) * 16;
        if env_base >= memory.ram_size() {
            return None;
        }
        let mcb_seg = env_segment.wrapping_sub(1);
        let mcb_addr = (mcb_seg as usize) * 16;
        if mcb_addr + 4 > memory.ram_size() {
            return None;
        }
        let tag = memory.read_u8(mcb_addr);
        if tag != 0x4D && tag != 0x5A {
            return None;
        }
        let size_paras = memory.read_u16(mcb_addr + 3);
        if size_paras == 0 {
            return None;
        }
        let block_bytes = (size_paras as u32).saturating_mul(16);
        // Per DPMI 0.9 spec ("Descriptor Usage Rules"): 16-bit DPMI implementations
        // always set the limit of the first descriptor to 64 KB even when running
        // on an 80386. Clients may use this descriptor as a string-scan
        // safety bound via REPNZ SCASB and trap if the selector limit is smaller.
        // The conventional-memory paragraph range [env_base .. env_base + 0x10000]
        // is mapped (well below the EBDA at 0x9FC00), so a 64K limit only changes
        // what the LDT descriptor *advertises*, not what physical memory exists.
        let mcb_floor = block_bytes.saturating_sub(1);
        Some(mcb_floor.max(0xFFFF))
    }

    /// Load CPU registers from RealModeCallStruct
    pub fn load_rm_registers(
        &self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        rm: &RealModeCallStruct,
    ) -> Result<()> {
        cpu.regs.eax = rm.eax;
        cpu.regs.ebx = rm.ebx;
        cpu.regs.ecx = rm.ecx;
        cpu.regs.edx = rm.edx;
        cpu.regs.esi = rm.esi;
        cpu.regs.edi = rm.edi;
        cpu.regs.ebp = rm.ebp;
        cpu.load_data_segment(memory, gp64_cpu::DataSegReg::ES, rm.es);
        cpu.load_data_segment(memory, gp64_cpu::DataSegReg::DS, rm.ds);
        cpu.load_data_segment(memory, gp64_cpu::DataSegReg::FS, rm.fs);
        cpu.load_data_segment(memory, gp64_cpu::DataSegReg::GS, rm.gs);
        // Only set SS:SP if non-zero (0 means use internal stack)
        if rm.ss != 0 || rm.sp != 0 {
            cpu.load_ss(memory, rm.ss)?;
            cpu.regs.set_sp(rm.sp);
        }
        // FLAGS=0 is a valid value; always load and preserve bit 1 as set.
        cpu.regs.flags = gp64_cpu::Flags::from_bits_truncate(rm.flags) | gp64_cpu::Flags::RESERVED1;
        Ok(())
    }

    /// Save CPU registers to RealModeCallStruct
    pub fn save_rm_registers(&self, cpu: &Cpu, rm: &mut RealModeCallStruct) {
        rm.eax = cpu.regs.eax;
        rm.ebx = cpu.regs.ebx;
        rm.ecx = cpu.regs.ecx;
        rm.edx = cpu.regs.edx;
        rm.esi = cpu.regs.esi;
        rm.edi = cpu.regs.edi;
        rm.ebp = cpu.regs.ebp;
        rm.es = cpu.regs.es;
        rm.ds = cpu.regs.ds;
        rm.fs = cpu.regs.fs;
        rm.gs = cpu.regs.gs;
        rm.ss = cpu.regs.ss;
        rm.sp = cpu.regs.sp();
        rm.flags = cpu.regs.flags.bits();
    }

    /// Check if V86 mode is currently active for translation services
    #[inline]
    pub fn is_v86_active(&self) -> bool {
        self.v86_active
    }

    /// Check if IRET in V86 mode should return to protected mode
    #[inline]
    pub fn should_exit_v86_on_iret(&self) -> bool {
        self.v86_active && self.v86_exit_on_iret
    }

    /// Check if RETF in V86 mode should return to protected mode
    #[inline]
    pub fn should_exit_v86_on_retf(&self) -> bool {
        self.v86_active && self.v86_exit_on_retf
    }

    #[inline]
    pub fn should_exit_v86_on_iret_state(&self, cpu: &Cpu) -> bool {
        self.should_exit_v86_on_iret() && cpu.regs.cs == 0 && cpu.regs.ip() == 0
    }

    #[inline]
    pub fn should_exit_v86_on_retf_state(&self, cpu: &Cpu) -> bool {
        self.should_exit_v86_on_retf() && cpu.regs.cs == 0 && cpu.regs.ip() == 0
    }

    /// Return the current protected-mode callback nesting depth.
    #[inline]
    pub fn pm_callback_depth(&self) -> u16 {
        self.pm_callback_depth
    }

    /// Whether at least one protected-mode callback is executing.
    #[inline]
    pub fn is_pm_callback_active(&self) -> bool {
        self.pm_callback_depth != 0
    }

    /// Enter one protected-mode callback scope.
    #[inline]
    pub fn enter_pm_callback(&mut self) {
        self.pm_callback_depth = self.pm_callback_depth.saturating_add(1);
    }

    /// Leave one protected-mode callback scope.
    #[inline]
    pub fn leave_pm_callback(&mut self) {
        self.pm_callback_depth = self.pm_callback_depth.saturating_sub(1);
    }

    /// Mark a hardware CPU reset as having left protected/V86 mode without
    /// discarding the client's DPMI allocations or descriptor bookkeeping.
    pub fn leave_protected_mode_for_cpu_reset(&mut self) {
        self.protected_mode = false;
        self.v86_active = false;
        self.v86_exit_on_iret = false;
        self.v86_exit_on_retf = false;
        self.v86_saved_state.clear();
        self.pending_interrupts.clear();
        self.virtual_interrupts_enabled = true;
        self.pm_callback_depth = 0;
        self.pending_hle_simulation = None;
    }

    /// Check if virtual interrupts are enabled (DPMI 0900h-0902h)
    /// Hardware interrupts should not be delivered to PM programs when this returns false.
    #[inline]
    pub fn are_virtual_interrupts_enabled(&self) -> bool {
        self.virtual_interrupts_enabled
    }

    /// Queue a hardware interrupt for later delivery when virtual interrupts are re-enabled (DPMI-116).
    /// Duplicates are not queued to avoid interrupt storms.
    pub fn queue_interrupt(&mut self, vector: u8) {
        if !self.pending_interrupts.contains(&vector) {
            self.pending_interrupts.push(vector);
        }
    }

    /// Pop a pending interrupt from the queue for delivery.
    /// Returns None if the queue is empty.
    pub fn pop_pending_interrupt(&mut self) -> Option<u8> {
        // Pop from front (FIFO order) to maintain interrupt ordering
        if self.pending_interrupts.is_empty() {
            None
        } else {
            Some(self.pending_interrupts.remove(0))
        }
    }

    /// Check if there are pending interrupts queued.
    #[inline]
    pub fn has_pending_interrupts(&self) -> bool {
        !self.pending_interrupts.is_empty()
    }

    /// Install a minimal V86-dispatch TSS at `V86_DISPATCH_TSS_BASE` (HMA).
    ///
    /// Returns the prior CPU TSS-related register state so the caller can
    /// pair this with [`restore_v86_dispatch_tss`] after `exit_v86`.
    ///
    /// **Why this exists.** HLE-initiated V86 dispatch (mouse callback
    /// today) can fire while the PM owner is a DOS extender or any other DPMI
    /// extender that set up its own PM environment *without* going through
    /// our DPMI host's mode_switch. In that case `cpu.regs.tr` is zero
    /// and `cpu.regs.tss_base/tss_limit` are unset, so the first V86
    /// `IN`/`OUT` raises `#GP(0)` via the IOPM check (V86 always consults
    /// the IOPM regardless of IOPL per SDM Vol 3A §20.3.3), and `#GP`
    /// delivery itself can't read SS0:ESP0 — cascade triple-faults.
    ///
    /// Installing this lightweight TSS with an all-zeros IOPM grants the
    /// V86 callback full port I/O (matching what real DPMI hosts do) and
    /// gives the CPU a valid SS0:ESP0 to fall back on if anything else
    /// inside the callback raises an exception.
    ///
    /// `tss_esp0`/`tss_ss0` are cached in CPU registers (see
    /// `read_tss_stack`) so the V86 exception delivery path reads them
    /// directly without consulting GDT, sidestepping the need to add a
    /// TSS descriptor to whatever GDT the PM owner installed.
    pub fn install_v86_dispatch_tss(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
    ) -> V86DispatchTssSavedState {
        let tss_base = Self::V86_DISPATCH_TSS_BASE;
        let header = Self::V86_DISPATCH_TSS_HEADER_SIZE;
        let iopm = Self::V86_DISPATCH_TSS_IOPM_SIZE;
        let tss_limit = header + iopm; // covers header + IOPM + terminator byte
        let ring0_stack_top = tss_base + header + iopm + 1 + 0x1000; // 4 KiB ring-0 stack
        let tss_ss0: u16 = 0x10; // flat data RPL=0 (used only by fallback exception path)

        let saved = V86DispatchTssSavedState {
            tr: cpu.regs.tr,
            tss_base: cpu.regs.tss_base,
            tss_limit: cpu.regs.tss_limit,
            tss_esp0: cpu.regs.tss_esp0,
            tss_ss0: cpu.regs.tss_ss0,
        };

        // TSS header: SS0 / ESP0 / IOPM base.
        memory.write_u32((tss_base + 0x04) as usize, ring0_stack_top);
        memory.write_u16((tss_base + 0x08) as usize, tss_ss0);
        memory.write_u16((tss_base + 0x66) as usize, header as u16);

        // IOPM: all zeros = every I/O port allowed for V86 callbacks.
        for i in 0..iopm {
            memory.write_u8((tss_base + header + i) as usize, 0x00);
        }
        // Per SDM Vol 3 §17.5.2, terminator byte at IOPM end is 0xFF.
        memory.write_u8((tss_base + header + iopm) as usize, 0xFF);

        cpu.regs.tss_base = tss_base;
        cpu.regs.tss_limit = tss_limit;
        cpu.regs.tss_esp0 = ring0_stack_top;
        cpu.regs.tss_ss0 = tss_ss0;
        // Leave cpu.regs.tr alone — the fast paths consult tss_base/limit/
        // esp0/ss0 directly, and rewriting `tr` would also require a TSS
        // descriptor in the active GDT (which the PM owner controls).

        tracing::debug!(
            target: "gp64::dos::dpmi",
            tss_base = format_args!("{:08X}", tss_base),
            tss_limit = format_args!("{:08X}", tss_limit),
            ring0_stack_top = format_args!("{:08X}", ring0_stack_top),
            "Installed V86-dispatch TSS with all-zeros IOPM"
        );

        saved
    }

    /// Pair of [`install_v86_dispatch_tss`]. Restores the CPU TSS-related
    /// register state captured by the install call. The TSS bytes in HMA
    /// stay where they are — they're idempotent across installs.
    pub fn restore_v86_dispatch_tss(&mut self, cpu: &mut Cpu, saved: V86DispatchTssSavedState) {
        cpu.regs.tr = saved.tr;
        cpu.regs.tss_base = saved.tss_base;
        cpu.regs.tss_limit = saved.tss_limit;
        cpu.regs.tss_esp0 = saved.tss_esp0;
        cpu.regs.tss_ss0 = saved.tss_ss0;
    }

    /// Check whether one more V86 translation frame can be saved.
    ///
    /// Callers that prepare guest memory before entering V86 use this to reject
    /// the request before writing a stack frame. enter_v86 repeats the check so
    /// direct and reflection callers are safe as well.
    pub fn preflight_v86_entry(&self) -> std::result::Result<(), u16> {
        if self.v86_saved_state.is_full() {
            Err(DPMI_ERR_RESOURCE_UNAVAILABLE)
        } else {
            Ok(())
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn enter_v86(
        &mut self,
        cpu: &mut Cpu,
        rm_struct: &RealModeCallStruct,
        rm_struct_addr: usize,
        entry_cs: u16,
        entry_ip: u16,
        rm_ss: u16,
        rm_sp: u16,
        exit_on_iret: bool,
    ) -> std::result::Result<(), u16> {
        self.preflight_v86_entry()?;

        let ss_base = cpu.regs.ss_cache.base;
        let stack_linear = ss_base.wrapping_add(cpu.regs.esp);
        tracing::debug!(
            cs = format_args!("{:04X}", cpu.regs.cs),
            eip = format_args!("{:08X}", cpu.regs.eip),
            ss = format_args!("{:04X}", cpu.regs.ss),
            ss_base = format_args!("{:08X}", ss_base),
            esp = format_args!("{:08X}", cpu.regs.esp),
            stack_linear = format_args!("{:08X}", stack_linear),
            "DPMI enter_v86 saving PM state"
        );
        tracing::debug!(
            edi = format_args!("{:08X}", rm_struct.edi),
            esi = format_args!("{:08X}", rm_struct.esi),
            ebp = format_args!("{:08X}", rm_struct.ebp),
            ebx = format_args!("{:08X}", rm_struct.ebx),
            pm_ebp = format_args!("{:08X}", cpu.regs.ebp),
            "DPMI enter_v86 input struct"
        );
        maybe_log_dpmi_control_target(
            cpu,
            rm_struct,
            rm_struct_addr,
            entry_cs,
            entry_ip,
            rm_ss,
            rm_sp,
            exit_on_iret,
        );
        // Save protected mode state including GPRs (V86 will overwrite them)
        let saved = V86SavedState {
            cs: cpu.regs.cs,
            ip: cpu.regs.eip,
            ss: cpu.regs.ss,
            esp: cpu.regs.esp,
            ds: cpu.regs.ds,
            es: cpu.regs.es,
            fs: cpu.regs.fs,
            gs: cpu.regs.gs,
            eflags: cpu.regs.eflags(),
            cr0: cpu.regs.cr0, // Save CR0 to prevent corruption
            // Save pre-V86 CPL. set_v86_mode(true) below forces CPL=3
            // per SDM §20.1; exit_v86 will use this to restore PM CPL.
            cpl: cpu.regs.cpl,
            eax: cpu.regs.eax,
            ebx: cpu.regs.ebx,
            ecx: cpu.regs.ecx,
            edx: cpu.regs.edx,
            esi: cpu.regs.esi,
            edi: cpu.regs.edi,
            ebp: cpu.regs.ebp,
            cs_cache: cpu.regs.cs_cache,
            ds_cache: cpu.regs.ds_cache,
            es_cache: cpu.regs.es_cache,
            ss_cache: cpu.regs.ss_cache,
            fs_cache: cpu.regs.fs_cache,
            gs_cache: cpu.regs.gs_cache,
            rm_struct_addr,
            prev_exit_on_iret: self.v86_exit_on_iret,
            prev_exit_on_retf: self.v86_exit_on_retf,
        };
        self.v86_saved_state.push(saved);

        // Set VM flag in EFLAGS (bit 17); this lives in eflags_high and also forces
        // CPL=3, so it survives the RMCS flag load below. IOPL is set AFTER that load
        // (it lives in FLAGS bits 12-13, which the load overwrites).
        cpu.regs.set_v86_mode(true);

        // Load registers from RealModeCallStruct
        cpu.regs.eax = rm_struct.eax;
        cpu.regs.ebx = rm_struct.ebx;
        cpu.regs.ecx = rm_struct.ecx;
        cpu.regs.edx = rm_struct.edx;
        cpu.regs.esi = rm_struct.esi;
        cpu.regs.edi = rm_struct.edi;
        cpu.regs.ebp = rm_struct.ebp;
        // FLAGS=0 is a valid RMCS value, not a sentinel; always load the low flags
        // (matching load_rm_registers) so a cleared RMCS clears IF/DF/TF/CF instead of
        // inheriting the protected-mode caller's flags. MDD-BUG-FLUXHOMEARPA-00012.
        cpu.regs.flags =
            gp64_cpu::Flags::from_bits_truncate(rm_struct.flags) | gp64_cpu::Flags::RESERVED1;
        // The flag load overwrites IOPL (FLAGS bits 12-13); V86 needs IOPL=3 to allow
        // I/O and sensitive instructions, so restore it after the load.
        cpu.regs.set_iopl(3);
        // Sync the interrupt state from the freshly loaded flags.
        cpu.interrupts_enabled = cpu.regs.flags.contains(gp64_cpu::Flags::INTERRUPT);

        // Load segment registers (real-mode values)
        cpu.regs.ds = rm_struct.ds;
        cpu.regs.es = rm_struct.es;
        cpu.regs.fs = rm_struct.fs;
        cpu.regs.gs = rm_struct.gs;

        // Set up V86 stack
        cpu.regs.ss = rm_ss;
        cpu.regs.esp = rm_sp as u32;

        cpu.regs.cs = entry_cs;
        cpu.regs.eip = entry_ip as u32;
        cpu.cs_bits = 16;

        // Set segment caches for V86 (real-mode style)
        cpu.regs.cs_cache = SegmentCache::real_mode(entry_cs);
        cpu.regs.ds_cache = SegmentCache::real_mode(rm_struct.ds);
        cpu.regs.es_cache = SegmentCache::real_mode(rm_struct.es);
        cpu.regs.ss_cache = SegmentCache::real_mode(cpu.regs.ss);
        cpu.regs.fs_cache = SegmentCache::real_mode(rm_struct.fs);
        cpu.regs.gs_cache = SegmentCache::real_mode(rm_struct.gs);

        self.v86_active = true;
        self.v86_exit_on_iret = exit_on_iret;
        self.v86_exit_on_retf = !exit_on_iret;
        Ok(())
    }

    /// Enter V86 mode to execute a real-mode interrupt
    /// This saves the protected-mode state and switches to V86 mode
    pub fn enter_v86_for_interrupt(
        &mut self,
        cpu: &mut Cpu,
        memory: &Memory,
        rm_struct: &RealModeCallStruct,
        rm_struct_addr: usize,
        int_num: u8,
    ) -> std::result::Result<(), u16> {
        // Get interrupt vector from IVT
        let ivt_addr = (int_num as usize) * 4;
        let handler_offset = memory.read_u16(ivt_addr);
        let handler_segment = memory.read_u16(ivt_addr + 2);

        // SS:SP = 0:0 asks the HOST to supply the stack; see the copy sites
        // above for why it sits at the top of the EBDA (MDD-BUG-CRU-01386).
        let (rm_ss, rm_sp) = if rm_struct.ss == 0 && rm_struct.sp == 0 {
            (Self::V86_CALLBACK_STACK_SEG, Self::V86_CALLBACK_STACK_TOP)
        } else {
            (rm_struct.ss, rm_struct.sp)
        };

        self.enter_v86(
            cpu,
            rm_struct,
            rm_struct_addr,
            handler_segment,
            handler_offset,
            rm_ss,
            rm_sp,
            true,
        )?;

        tracing::trace!(
            "[DPMI] Entered V86 mode for INT {int_num:02X}h -> {handler_segment:04X}:{handler_offset:04X}"
        );
        Ok(())
    }

    /// Report success (CF=0) to the protected-mode caller of a DPMI real-mode
    /// translation service (INT 31h 0300h/0301h/0302h) after execution has been
    /// transferred into V86.
    ///
    /// The success flag must be reported on the PROTECTED-MODE return path, not
    /// by mutating the live flags now: `enter_v86` has already loaded the
    /// caller-provided RMCS flags (including CF) into the live CPU flags, and the
    /// real-mode handler must run with exactly those. So clear CF only in the
    /// SAVED protected-mode flags image that `exit_v86` restores when the handler
    /// returns — leaving the live V86 carry flag untouched. MDD-BUG-CRUCIBLE-00054.
    fn report_v86_translation_success(&mut self) {
        if let Some(saved) = self.v86_saved_state.last_mut() {
            saved.eflags &= !u32::from(gp64_cpu::Flags::CARRY.bits());
        }
    }

    /// Enter V86 mode directly, bypassing IRETD.
    ///
    /// Exit V86 mode and return to protected mode
    /// Saves results to the RealModeCallStruct and restores PM state
    pub fn exit_v86(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        if let Some(saved) = self.v86_saved_state.last() {
            let stack_linear = saved.ss_cache.base.wrapping_add(saved.esp);
            tracing::debug!(
                cs = format_args!("{:04X}", saved.cs),
                ip = format_args!("{:08X}", saved.ip),
                ss = format_args!("{:04X}", saved.ss),
                ss_base = format_args!("{:08X}", saved.ss_cache.base),
                esp = format_args!("{:08X}", saved.esp),
                stack_linear = format_args!("{:08X}", stack_linear),
                "DPMI exit_v86 restoring PM state"
            );
        }
        if let Some(saved) = self.v86_saved_state.pop() {
            // Save current V86 results to RealModeCallStruct
            // Note: We update general registers and flags, but NOT CS:IP.
            // CS:IP would be our sentinel address (0000:0000) which is meaningless.
            // Most DPMI implementations preserve the original CS:IP or save a
            // DPMI-internal address. Preserving original is safest for compatibility.
            let mut rm_struct = RealModeCallStruct::read_from_memory(memory, saved.rm_struct_addr);
            let rm_struct_before = rm_struct;
            let orig_cs = rm_struct.cs;
            let orig_ip = rm_struct.ip;
            rm_struct.eax = cpu.regs.eax;
            rm_struct.ebx = cpu.regs.ebx;
            rm_struct.ecx = cpu.regs.ecx;
            rm_struct.edx = cpu.regs.edx;
            rm_struct.esi = cpu.regs.esi;
            rm_struct.edi = cpu.regs.edi;
            rm_struct.ebp = cpu.regs.ebp;
            rm_struct.ds = cpu.regs.ds;
            rm_struct.es = cpu.regs.es;
            rm_struct.fs = cpu.regs.fs;
            rm_struct.gs = cpu.regs.gs;
            rm_struct.cs = orig_cs; // Preserve original - don't save sentinel
            rm_struct.ip = orig_ip;
            rm_struct.ss = cpu.regs.ss;
            rm_struct.sp = cpu.regs.sp();
            rm_struct.flags = cpu.regs.flags.bits();
            rm_struct.write_to_memory(memory, saved.rm_struct_addr);
            trace_ret_v86_rmcs(saved.rm_struct_addr, rm_struct_before, rm_struct);

            if dpmi_trace_env_enabled() {
                dpmi_trace_emit(&format!(
                    "[DPMI] RMCS return addr={:08X} cs:ip={:04X}:{:04X} ss:sp={:04X}:{:04X} flags={:04X} eax={:08X} ebx={:08X} ecx={:08X} edx={:08X} ds={:04X} es={:04X}",
                    saved.rm_struct_addr,
                    rm_struct.cs,
                    rm_struct.ip,
                    rm_struct.ss,
                    rm_struct.sp,
                    rm_struct.flags,
                    rm_struct.eax,
                    rm_struct.ebx,
                    rm_struct.ecx,
                    rm_struct.edx,
                    rm_struct.ds,
                    rm_struct.es
                ));
            }

            tracing::debug!(
                struct_addr = format_args!("{:08X}", saved.rm_struct_addr),
                edi = format_args!("{:08X}", rm_struct.edi),
                esi = format_args!("{:08X}", rm_struct.esi),
                ebp = format_args!("{:08X}", rm_struct.ebp),
                ebx = format_args!("{:08X}", rm_struct.ebx),
                eax = format_args!("{:08X}", rm_struct.eax),
                flags = format_args!("{:04X}", rm_struct.flags),
                "DPMI exit_v86 saved to struct"
            );
            tracing::debug!(
                target: "gp64::dos::dpmi",
                struct_addr = format_args!("{:08X}", saved.rm_struct_addr),
                rm_cs = format_args!("{:04X}", rm_struct.cs),
                rm_ip = format_args!("{:04X}", rm_struct.ip),
                rm_ss = format_args!("{:04X}", rm_struct.ss),
                rm_sp = format_args!("{:04X}", rm_struct.sp),
                flags = format_args!("{:04X}", rm_struct.flags),
                eax = format_args!("{:08X}", rm_struct.eax),
                ebx = format_args!("{:08X}", rm_struct.ebx),
                ecx = format_args!("{:08X}", rm_struct.ecx),
                edx = format_args!("{:08X}", rm_struct.edx),
                ds = format_args!("{:04X}", rm_struct.ds),
                es = format_args!("{:04X}", rm_struct.es),
                "DPMI RMCS return"
            );
            tracing::debug!(
                current_esp = format_args!("{:08X}", cpu.regs.esp),
                saved_esp = format_args!("{:08X}", saved.esp),
                "DPMI exit_v86 restoring ESP"
            );

            // Restore protected mode state
            cpu.regs.cr0 = saved.cr0; // Restore CR0 first (before segment loads)
            cpu.regs.cs = saved.cs;
            cpu.regs.eip = saved.ip; // Restore full 32-bit EIP
            cpu.regs.ss = saved.ss;
            cpu.regs.esp = saved.esp;
            cpu.regs.ds = saved.ds;
            cpu.regs.es = saved.es;
            cpu.regs.fs = saved.fs;
            cpu.regs.gs = saved.gs;
            cpu.regs.set_eflags(saved.eflags);
            // Restore pre-V86 CPL. set_eflags above cleared VM (the
            // saved EFLAGS image is the PM image with VM=0), so the
            // setter takes the non-V86 path and writes the PM CPL
            // verbatim. SDM §20.1 invariant: CPL=3 only when VM=1.
            cpu.regs.set_cpl(saved.cpl);

            // Restore general purpose registers
            cpu.regs.eax = saved.eax;
            cpu.regs.ebx = saved.ebx;
            cpu.regs.ecx = saved.ecx;
            cpu.regs.edx = saved.edx;
            cpu.regs.esi = saved.esi;
            cpu.regs.edi = saved.edi;
            cpu.regs.ebp = saved.ebp;

            // Restore segment caches
            cpu.regs.cs_cache = saved.cs_cache;
            cpu.regs.ds_cache = saved.ds_cache;
            cpu.regs.es_cache = saved.es_cache;
            cpu.regs.ss_cache = saved.ss_cache;
            cpu.regs.fs_cache = saved.fs_cache;
            cpu.regs.gs_cache = saved.gs_cache;
            cpu.cs_bits = if cpu.regs.cs_cache.big { 32 } else { 16 };

            tracing::trace!(
                "[DPMI] Exited V86 mode, returning to PM {:04X}:{:04X}",
                saved.cs,
                saved.ip
            );

            self.v86_exit_on_iret = saved.prev_exit_on_iret;
            self.v86_exit_on_retf = saved.prev_exit_on_retf;
        }

        self.v86_active = !self.v86_saved_state.is_empty();
        if !self.v86_active {
            self.v86_exit_on_iret = false;
            self.v86_exit_on_retf = false;
        }
    }

    /// Check if we're in protected mode
    pub fn in_protected_mode(&self) -> bool {
        self.protected_mode
    }

    /// Park or resume the client's protected-mode context around a child the
    /// host runs in real mode (a PM INT 21h EXEC), keeping the session.
    pub fn set_client_in_protected_mode(&mut self, in_pm: bool) {
        self.protected_mode = in_pm;
    }

    /// The selector the host wrote into the client PSP's environment word.
    pub fn env_selector(&self) -> u16 {
        self.env_selector
    }

    /// Check if a DPMI client was initialized (GDT was set up).
    /// Returns true even if the CPU is temporarily in real mode for a
    /// simulated real-mode interrupt.  Used to detect that a terminating
    /// process is a DPMI child (vs. a plain real-mode program).
    pub fn was_initialized(&self) -> bool {
        self.gdt_base != 0
    }

    /// The locked exception stack as `(selector, initial SP/ESP)`, or `None`
    /// when the host's GDT is not the live one (a client that installed its
    /// own GDT has no GDT[7] of ours to switch to).
    pub fn locked_exception_stack(&self, cpu: &Cpu) -> Option<(u16, u32)> {
        (self.gdt_base != 0 && cpu.regs.gdtr.base == self.gdt_base)
            .then_some((PM_EXCEPTION_STACK_SEL, PM_EXCEPTION_STACK_SIZE))
    }

    /// Return the protected-mode stack saved by the innermost V86 translation.
    ///
    /// A real-mode procedure called from a PM callback suspends the callback's
    /// PM context. If another callback arrives while that procedure runs, the
    /// saved `SS:ESP` is the suspended callback stack position to resume on.
    pub fn v86_saved_pm_stack(&self) -> Option<(u16, u32)> {
        self.v86_saved_state
            .last()
            .map(|saved| (saved.ss, saved.esp))
    }

    /// Select the protected-mode stack for a real-mode callback entry.
    ///
    /// The first callback in a task switches to the one locked host stack.
    /// A nested callback resumes the protected-mode stack suspended by V86
    /// translation, even when that saved selector is client-owned. A saved
    /// frame on that same host selector also keeps its saved `ESP`, so a new
    /// callback cannot overwrite the active host frame. A nested callback
    /// that is already in protected mode keeps the current stack.
    /// Return `None` when the host stack is unavailable or a V86 callback has
    /// no suspended protected-mode state to resume.
    pub fn pm_callback_stack(&self, cpu: &Cpu) -> Option<(u16, u32)> {
        let host_stack = self.locked_exception_stack(cpu);
        if self.is_pm_callback_active() {
            self.v86_saved_pm_stack()
                .or_else(|| (!cpu.regs.is_v86_mode()).then_some((cpu.regs.ss, cpu.regs.esp)))
        } else {
            match (self.v86_saved_pm_stack(), host_stack) {
                (Some(saved), Some(host)) if saved.0 == host.0 => Some(saved),
                _ => host_stack,
            }
        }
    }

    /// Check if in 32-bit protected mode (cpu_mode == 1)
    pub fn is_32bit_mode(&self) -> bool {
        self.cpu_mode == 1
    }

    /// Enter protected mode (called from entry point stub at F000:FF10)
    ///
    /// Per DPMI 0.9 specification:
    /// - ES is set to a selector pointing to the PSP with a 100h byte limit
    /// - The environment pointer at PSP[0x2C] is automatically converted to a selector
    pub fn enter_protected_mode(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        self.enter_protected_mode_with_psp(cpu, memory, 0, 0)
    }

    /// Enter protected mode with explicit PSP and environment segments.
    ///
    /// Per DPMI 0.9 specification:
    /// - ES is set to a selector pointing to the PSP with a 100h byte limit
    /// - The environment pointer at PSP[0x2C] is automatically converted to a selector
    ///
    /// If psp_segment is 0, ES will point to the caller's ES (for backward compatibility).
    pub fn enter_protected_mode_with_psp(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        psp_segment: u16,
        env_segment: u16,
    ) -> Result<()> {
        tracing::debug!(target: "gp64::dos::dpmi", "Switching to Protected Mode");

        // DPMI-107: preserve state on failure.
        let dpmi_snapshot = self.clone();
        let cpu_snapshot = CpuPmEntrySnapshot::new(cpu);
        let a20_before = memory.a20_enabled();

        let result = (|| -> Result<()> {
            // Protected-mode extenders assume the A20 line is enabled.
            memory.set_a20(true);

            // Ensure sufficient RAM for DPMI applications.
            memory.ensure_ram_size(DPMI_MIN_RAM_SIZE);

            // Get return address from stack (Real Mode CALL FAR pushed CS, IP)
            let ret_ip = cpu.try_pop16(memory)?;
            let ret_cs = cpu.try_pop16(memory)?;

            tracing::debug!(
                target: "gp64::dos::dpmi",
                ret_cs = format_args!("{:04X}", ret_cs),
                ret_ip = format_args!("{:04X}", ret_ip),
                psp_segment = format_args!("{:04X}", psp_segment),
                env_segment = format_args!("{:04X}", env_segment),
                "Return target"
            );

            // AX contains the requested mode (0 = 16-bit, 1 = 32-bit)
            let mode_flags = cpu.regs.ax();
            let is_32bit = (mode_flags & 1) != 0;
            self.cpu_mode = if is_32bit { 1 } else { 0 };
            tracing::debug!(
                target: "gp64::dos::dpmi",
                mode = if is_32bit { "32-bit" } else { "16-bit" },
                "Requested mode"
            );

            // Calculate linear base addresses for current segments
            let cs_base = (ret_cs as u32) << 4;
            let ds_base = (cpu.regs.ds as u32) << 4;
            let ss_base = (cpu.regs.ss as u32) << 4;

            // Per DPMI 0.9 spec: ES points to PSP with 100h limit (if PSP is provided)
            // Otherwise fall back to caller's ES for backward compatibility
            let (es_base, es_limit) = if psp_segment != 0 {
                ((psp_segment as u32) << 4, 0xFF_u32) // PSP is 256 bytes (0x100), limit is last valid offset
            } else {
                ((cpu.regs.es as u32) << 4, 0xFFFF_u32)
            };

            // Place descriptor tables in the HMA so we don't collide with conventional-memory DOS allocations.
            // The emulator's memory model includes 64KB of HMA at 0x100000..0x10FFFF.
            let gdt_base = 0x100000usize;
            let gdt_size = 0x1000usize;
            let ldt_base = gdt_base + gdt_size;
            let ldt_size = 0x8000usize;
            let idt_base = ldt_base + ldt_size;
            let idt_size = 0x1000usize;

            // TSS and ring-0 stack: placed after IDT in HMA.
            // The TSS is needed so hardware interrupt dispatch (timer, keyboard)
            // can switch to a clean stack, matching real hardware ring-3→0 behavior.
            let tss_base = idt_base + idt_size;
            // TSS layout: 104-byte 386 TSS header + 8192-byte I/O permission bitmap
            // covering all 65536 ports + 1 terminator byte. Real DPMI hosts populate
            // the IOPM with zeros (= all ports allowed) so RM ISRs reflected into V86
            // can freely do port I/O — per Intel SDM Vol 3A §20.3.3 V86 always
            // consults the IOPM regardless of IOPL, so the IOPM is the only place a
            // host can grant V86 I/O.
            const TSS_HEADER_SIZE: usize = 0x68;
            const TSS_IOPM_OFFSET: usize = TSS_HEADER_SIZE;
            const TSS_IOPM_SIZE: usize = 0x2000; // 8192 bytes = 65536 port bits
            let tss_size = TSS_HEADER_SIZE + TSS_IOPM_SIZE + 1; // +1 terminator byte
            let ring0_stack_base = tss_base + tss_size;
            let ring0_stack_size = 0x1000usize; // 4KB ring-0 stack
            let ring0_stack_top = ring0_stack_base + ring0_stack_size;
            // Locked exception stack (GDT[7]) directly after the ring-0 stack.
            let exception_stack_base = ring0_stack_top;
            let host_control_end = exception_stack_base + PM_EXCEPTION_STACK_SIZE as usize;

            // GDT layout (only system-level descriptors that can't be modified by clients):
            // 0: null
            // 1: 32-bit flat code (base=0, limit=4GB) - for future use
            // 2: 32-bit flat data (base=0, limit=4GB) - for future use
            // 3: BIOS code (base=0xF0000, limit=64KB) for default IDT stubs
            // 4: LDT descriptor
            // 5: TSS descriptor
            // Expose the full 4KB GDT region via GDTR limit. The GDT
            // memory is allocated as gdt_size bytes anyway, so advertising
            // the full capacity is safe. DOS extenders that use our DPMI
            // host's GDT (rather than replacing it with LGDT) can allocate
            // descriptors freely without hitting an artificially low limit.
            let gdt_limit = (gdt_size - 1) as u16;
            let bios_code_sel = PM_BIOS_CODE_SEL; // GDT[3] with RPL=3 = 0x1B

            // Determine flags based on 16/32-bit mode (DPMI 0.9 spec section 5.1)
            // The D bit in code segments controls default operand/address size:
            //   D=0 (0x0): 16-bit default (use for 16-bit DPMI clients, AX=0)
            //   D=1 (0x4): 32-bit default (use for 32-bit DPMI clients, AX=1)
            // so instructions execute with 32-bit operand/address sizes by default.
            let code_flags = if is_32bit { 0x4_u8 } else { 0x0_u8 };
            let data_flags = if is_32bit { 0x4_u8 } else { 0x0_u8 };

            // LDT: 4096 entries (32KB).
            cpu.regs.ldt_base = ldt_base as u32;
            cpu.regs.ldt_limit = (ldt_size - 1) as u32;

            // IDT: 256 entries (2KB).
            cpu.regs.idtr.base = idt_base as u32;
            cpu.regs.idtr.limit = (256 * 8 - 1) as u16;

            // GDT Descriptor 0: Null
            self.write_descriptor_raw(memory, gdt_base, 0, 0, 0, 0, 0);

            // DPMI applications run at ring 3 (CPL=3), so user-accessible segments need DPL=3.
            // Access byte format: P(1) DPL(2) S(1) Type(4)
            // DPL=3 code: 0xFA = 1111_1010 = P=1, DPL=3, S=1, type=1010 (execute/read)
            // DPL=3 data: 0xF2 = 1111_0010 = P=1, DPL=3, S=1, type=0010 (read/write)

            // GDT Descriptor 1 (0x08): 32-bit flat code (for future use)
            self.write_descriptor_raw(memory, gdt_base, 1, 0, 0xFFFFF, 0xFA, 0xC);

            // GDT Descriptor 2 (0x10): 32-bit flat data (for future use)
            self.write_descriptor_raw(memory, gdt_base, 2, 0, 0xFFFFF, 0xF2, 0xC);

            // GDT Descriptor 3 (0x18): BIOS code segment (for default IDT stubs)
            // Must match IDT gate size: D=1 (32-bit) if is_32bit, D=0 (16-bit) otherwise
            // Use DPL=3 (0xFA instead of 0x9A) so no privilege change occurs when
            // ring 3 code invokes software interrupts (no ring transition = no TSS lookup).
            let bios_code_flags = if is_32bit { 0x4_u8 } else { 0x0_u8 };
            self.write_descriptor_raw(
                memory,
                gdt_base,
                3,
                0x000F_0000,
                0xFFFF,
                0xFA, // DPL=3 code segment (execute/read)
                bios_code_flags,
            );

            // GDT Descriptor 4 (0x20): LDT descriptor (system segment, type=0x2).
            let ldt_limit = (ldt_size - 1) as u32;
            self.write_descriptor_raw(
                memory,
                gdt_base,
                4,
                ldt_base as u32,
                ldt_limit,
                0x82, // Present, system, LDT
                0x00,
            );
            cpu.regs.ldtr = PM_LDT_SEL;

            // GDT Descriptor 5 (0x28): TSS descriptor (system segment, type=0x9 = available 386 TSS).
            // DPL=0 since only the CPU references TSS via TR, not via software selectors.
            self.write_descriptor_raw(
                memory,
                gdt_base,
                5,
                tss_base as u32,
                (tss_size - 1) as u32,
                0x89, // Present, DPL=0, system, type=9 (available 386 TSS)
                0x00,
            );

            // GDT Descriptor 6 (0x30): flat RING-0 data, the TSS's SS0.
            //
            // A privilege transition to CPL 0 loads SS:ESP from the TSS, and
            // the CPU requires the new stack selector's DPL **and** RPL both to
            // equal the new CPL (Intel SDM Vol. 3 §6.4.1; `load_ss` enforces
            // `cache.dpl == cpl && rpl == cpl`). GDT[2] is the DPL=3 user data
            // segment and cannot serve: referenced as `0x13` it fails on RPL
            // and on DPL, so every gate that really changes privilege raised
            // #GP rather than switching to the ring-0 stack. Only DPL=3 gates
            // masked it, because they perform no transition at all.
            //
            // 0x92 = P=1, DPL=0, S=1, type=0010 (read/write data).
            self.write_descriptor_raw(memory, gdt_base, 6, 0, 0xFFFFF, 0x92, 0xC);

            // GDT Descriptor 7 (0x3B): the locked exception stack. DPL=3 so a
            // ring-3 handler can run on it; B follows the client's bitness.
            self.write_descriptor_raw(
                memory,
                gdt_base,
                7,
                exception_stack_base as u32,
                PM_EXCEPTION_STACK_SIZE - 1,
                0xF2,
                data_flags,
            );

            // Initialize TSS fields: SS0:ESP0 for ring-3→0 stack switch.
            // SS0 = GDT[6] flat ring-0 data selector, RPL=0 (base=0, limit=4GB).
            // ESP0 = top of the allocated ring-0 stack.
            let tss_ss0: u16 = PM_RING0_STACK_SEL;
            let tss_esp0: u32 = ring0_stack_top as u32;
            // TSS offset 0x04: ESP0
            memory.write_u32(tss_base + 0x04, tss_esp0);
            // TSS offset 0x08: SS0 (stored as u16, padded to u32)
            memory.write_u16(tss_base + 0x08, tss_ss0);
            // TSS offset 0x66: I/O Map Base Address — points at the all-zeros IOPM
            // embedded in the TSS. Real DPMI hosts grant full V86 I/O by setting
            // every IOPM bit to 0; PM CPL>IOPL also falls back here and likewise
            // gets full access. PM with CPL<=IOPL takes the fast path in
            // check_io_permission and never reads the IOPM.
            memory.write_u16(tss_base + 0x66, TSS_IOPM_OFFSET as u16);

            // Zero the IOPM (all 65536 ports allowed).
            for i in 0..TSS_IOPM_SIZE {
                memory.write_u8(tss_base + TSS_IOPM_OFFSET + i, 0x00);
            }
            // IOPM terminator byte at IOPM_END per Intel SDM Vol 3 §17.5.2.
            memory.write_u8(tss_base + TSS_IOPM_OFFSET + TSS_IOPM_SIZE, 0xFF);

            // Load task register so read_tss_stack() can find SS0:ESP0.
            let tss_sel: u16 = 5 * 8; // 0x28, RPL=0
            cpu.regs.tr = tss_sel;
            cpu.regs.tss_base = tss_base as u32;
            cpu.regs.tss_limit = (tss_size - 1) as u32;
            // Cache SS0:ESP0 in CPU registers — immune to RAM corruption from
            // flat-segment writes by DPMI clients.  The TSS in RAM exists for
            // LTR compatibility, but read_tss_stack() returns these cached values.
            cpu.regs.tss_ss0 = tss_ss0;
            cpu.regs.tss_esp0 = tss_esp0;

            // Initial CS/DS/SS/ES go in the LDT so DOS extenders can modify them via INT 31h.
            // Per DPMI spec, clients can modify descriptors they own. GDT selectors cannot be
            // modified by client code - only LDT selectors can.
            //
            // LDT selector format: (index << 3) | TI=1 | RPL=3
            // Keep LDT index 0 reserved so selector values 0004..0007 do not
            // accidentally alias a client descriptor. Real 16-bit code uses
            // tiny data values in far-pointer shaped records, and those must
            // fault as invalid selectors rather than resolving to initial CS.

            // ==========================================================================
            // SEGMENT CONFIGURATION: 16-bit vs 32-bit DPMI clients
            // ==========================================================================
            //
            // Both 16-bit (AX=0) and 32-bit (AX=1) clients initially receive
            // segmented descriptors with real-mode bases:
            //   - Base = real-mode segment origin (e.g., 0x20000 for CS 0x2000)
            //   - Limit = 0xFFFF (64KB)
            //   - EIP/ESP = offset within segment
            //
            // 16-bit clients (AX=0):
            //   - Flags = 0x0 (D=0, G=0) for 16-bit operand/address size
            //   - These descriptors are the final working segments.
            //
            // 32-bit clients (AX=1):
            //   - Flags = 0x0 (D=0, G=0) — same as 16-bit, NOT flat model yet.
            //   - D=0 preserves the 16-bit real-mode entry code at the return
            //     address of the DPMI entry call.
            //   - The stub code is then responsible for creating flat-model
            //     descriptors (base=0, limit=4GB, D=1, G=1) via INT 31h or
            //     direct LDT writes, and far-jumping to the actual 32-bit
            //     application entry point.
            //
            // This matches the DOS model behavior: the DPMI host provides minimal
            // segmented descriptors at mode switch time; the DOS extender's stub
            // code bootstraps itself into flat model before entering 32-bit code.
            // ==========================================================================

            let cs_selector = 0x0Fu16; // LDT index 1, TI=1, RPL=3
            let ds_selector = 0x17u16; // LDT index 2, TI=1, RPL=3
            let ss_selector = 0x1Fu16; // LDT index 3, TI=1, RPL=3
            let es_selector = 0x27u16; // LDT index 4, TI=1, RPL=3

            self.write_descriptor_raw(memory, ldt_base, 0, 0, 0, 0, 0);

            if is_32bit {
                // ===== 32-BIT CLIENT MODE: 16-BIT STUB COMPATIBILITY =====
                // The DPMI entry contract returns to the caller's 16-bit code,
                // so the initial code descriptor retains D=0.
                //
                // The stub will then modify descriptors (via direct LDT writes
                // or INT 31h) to create 32-bit flat segments and far-jump to
                // the actual 32-bit application code.
                let stub_flags = 0x0_u8; // D=0, G=0 for 16-bit stub code

                // Write 16-bit segmented descriptors to LDT. Index 0 stays
                // zeroed/invalid; the client-visible selectors begin at index 1.
                self.write_descriptor_raw(memory, ldt_base, 1, cs_base, 0xFFFF, 0xFA, stub_flags); // CS
                self.write_descriptor_raw(memory, ldt_base, 2, ds_base, 0xFFFF, 0xF2, stub_flags); // DS
                self.write_descriptor_raw(memory, ldt_base, 3, ss_base, 0xFFFF, 0xF2, stub_flags); // SS

                // ES: Per DPMI 0.9 spec, ES should point to PSP with 0x100 limit when psp_segment is provided.
                let (actual_es_base, actual_es_limit) = if psp_segment != 0 {
                    (es_base, es_limit)
                } else {
                    (ds_base, 0xFFFF)
                };
                self.write_descriptor_raw(
                    memory,
                    ldt_base,
                    4,
                    actual_es_base,
                    actual_es_limit,
                    0xF2,
                    stub_flags,
                ); // ES

                // Register 16-bit selectors
                self.selectors.push(Selector {
                    selector: cs_selector,
                    base: cs_base,
                    limit: 0xFFFF,
                    access: 0xFA,
                    flags: stub_flags,
                    lock_count: 0,
                });
                self.selectors.push(Selector {
                    selector: ds_selector,
                    base: ds_base,
                    limit: 0xFFFF,
                    access: 0xF2,
                    flags: stub_flags,
                    lock_count: 0,
                });
                self.selectors.push(Selector {
                    selector: ss_selector,
                    base: ss_base,
                    limit: 0xFFFF,
                    access: 0xF2,
                    flags: stub_flags,
                    lock_count: 0,
                });
                self.selectors.push(Selector {
                    selector: es_selector,
                    base: actual_es_base,
                    limit: actual_es_limit,
                    access: 0xF2,
                    flags: stub_flags,
                    lock_count: 0,
                });
            } else {
                // ===== 16-BIT: SEGMENTED MODEL =====
                // Segments have real-mode bases, 64KB limits
                let es_flags = if psp_segment != 0 { 0x0_u8 } else { data_flags };

                self.write_descriptor_raw(memory, ldt_base, 1, cs_base, 0xFFFF, 0xFA, code_flags);
                self.write_descriptor_raw(memory, ldt_base, 2, ds_base, 0xFFFF, 0xF2, data_flags);
                self.write_descriptor_raw(memory, ldt_base, 3, ss_base, 0xFFFF, 0xF2, data_flags);
                self.write_descriptor_raw(memory, ldt_base, 4, es_base, es_limit, 0xF2, es_flags);

                // Register 16-bit selectors
                self.selectors.push(Selector {
                    selector: cs_selector,
                    base: cs_base,
                    limit: 0xFFFF,
                    access: 0xFA,
                    flags: code_flags,
                    lock_count: 0,
                });
                self.selectors.push(Selector {
                    selector: ds_selector,
                    base: ds_base,
                    limit: 0xFFFF,
                    access: 0xF2,
                    flags: data_flags,
                    lock_count: 0,
                });
                self.selectors.push(Selector {
                    selector: ss_selector,
                    base: ss_base,
                    limit: 0xFFFF,
                    access: 0xF2,
                    flags: data_flags,
                    lock_count: 0,
                });
                self.selectors.push(Selector {
                    selector: es_selector,
                    base: es_base,
                    limit: es_limit,
                    access: 0xF2,
                    flags: es_flags,
                    lock_count: 0,
                });
            }

            // Update next_selector to start after the initial segments.
            // LDT index 0 is reserved and indices 1-4 are initial segments,
            // so the next free selector is index 5 = selector 0x2F.
            self.next_selector = 0x2F;

            // Set GDTR
            cpu.regs.gdtr.base = gdt_base as u32;
            cpu.regs.gdtr.limit = gdt_limit;

            // Register GDT range for write snooping (detects direct GDT
            memory.set_gdt_snoop_range(gdt_base as u32, gdt_limit);
            memory.set_ldt_snoop_range(ldt_base as u32, ldt_limit);
            cpu.mark_descriptor_tables_changed();

            // Debug: verify CS descriptor (GDT[1]) was written correctly
            {
                let cs_desc_addr = gdt_base + 8; // Descriptor 1
                let mut desc_bytes = [0u8; 8];
                for (i, byte) in desc_bytes.iter_mut().enumerate() {
                    *byte = memory.read_u8(cs_desc_addr + i);
                }
                let limit_low = u16::from_le_bytes([desc_bytes[0], desc_bytes[1]]);
                let base_low = u16::from_le_bytes([desc_bytes[2], desc_bytes[3]]);
                let base_mid = desc_bytes[4];
                let access = desc_bytes[5];
                let limit_flags = desc_bytes[6];
                let base_high = desc_bytes[7];
                let base =
                    (base_low as u32) | ((base_mid as u32) << 16) | ((base_high as u32) << 24);
                let limit = (limit_low as u32) | (((limit_flags & 0x0F) as u32) << 16);
                let flags = limit_flags >> 4;
                let d_bit = if (flags & 0x4) != 0 { 32 } else { 16 };
                tracing::debug!(
                    base = format_args!("{:08X}", base),
                    limit = format_args!("{:05X}", limit),
                    access = format_args!("{:02X}", access),
                    flags = format_args!("{:X}", flags),
                    d_bit,
                    expected_cs_base = format_args!("{:08X}", cs_base),
                    ret_cs = format_args!("{:04X}", ret_cs),
                    "DPMI GDT[1] CS descriptor"
                );
            }

            // Safe default IDT: point each vector to a unique stub in the BIOS segment (0x38).
            // This allows the Emulator::step_inner HLE trap to identify the interrupt vector
            // even in Protected Mode.
            // IDT gate type_attr: P(1) DPL(2) 0(1) Type(4)
            // Use DPL=3 so ring 3 code can invoke software interrupts (INT xx instructions).
            // 0xEE = P=1, DPL=3, type=0xE (32-bit interrupt gate)
            // 0xE6 = P=1, DPL=3, type=0x6 (16-bit interrupt gate)
            let idt_type_attr = if is_32bit { 0xEE } else { 0xE6 };
            for vector in 0u16..256 {
                let offset = 0xFE00 + (vector * 2);
                self.write_idt_gate(
                    memory,
                    idt_base,
                    vector as u8,
                    offset,
                    bios_code_sel,
                    idt_type_attr,
                );
                // Also update our internal pm_idt array
                self.pm_idt[vector as usize] = IdtEntry {
                    selector: bios_code_sel,
                    offset: offset as u32,
                    gate_type: if is_32bit { 0x0E } else { 0x06 },
                    dpl: 3,
                    present: true,
                };
            }

            // Enable Protected Mode
            cpu.regs.cr0 |= 1; // Set PE bit
            self.protected_mode = true;
            cpu.dpmi_active = true;

            // Per DPMI 0.9 spec: The environment pointer at PSP[0x2C] is automatically converted
            // to a selector during the mode switch. Allocate an LDT selector for the environment.
            let env_selector = if psp_segment != 0 && env_segment != 0 {
                // Allocate an LDT selector for the environment
                let env_sel = self.allocate_descriptors(cpu, memory, 1);
                if env_sel != 0 {
                    let env_base = (env_segment as u32) << 4;
                    let env_limit = Self::env_limit_from_mcb(memory, env_segment).unwrap_or(0xFFFF);
                    self.set_descriptor_base(cpu, memory, env_sel, env_base);
                    self.set_descriptor_limit(cpu, memory, env_sel, env_limit);

                    // Update PSP[0x2C] with the environment selector
                    let psp_base = (psp_segment as u32) << 4;
                    memory.write_u16((psp_base + 0x2C) as usize, env_sel);

                    tracing::debug!(
                        target: "gp64::dos::dpmi",
                        env_selector = format_args!("{:04X}", env_sel),
                        env_base = format_args!("{:08X}", env_base),
                        psp_segment = format_args!("{:04X}", psp_segment),
                        "Environment selector allocated and written to PSP[0x2C]"
                    );

                    env_sel
                } else {
                    tracing::warn!(
                        target: "gp64::dos::dpmi",
                        "Failed to allocate environment selector, PSP[0x2C] will contain 0"
                    );
                    // Write 0 to PSP[0x2C] to indicate no environment
                    let psp_base = (psp_segment as u32) << 4;
                    memory.write_u16((psp_base + 0x2C) as usize, 0);
                    0
                }
            } else {
                0
            };

            // Track environment selector for later use
            self.env_selector = env_selector;

            // DPMI clients run at ring 3 (CPL=3). All segment selectors use RPL=3.
            // This is required because user-installed interrupt handlers (AX=0205) have RPL=3
            // and interrupt dispatching from CPL=0 to RPL=3 code would violate privilege rules.
            cpu.regs.cpl = 3;

            // Load segment registers with LDT selectors created above
            // LDT selectors already have TI=1 (bit 2) and RPL=3 (bits 0-1) set
            cpu.load_cs(memory, cs_selector)?;
            cpu.load_data_segment(memory, gp64_cpu::DataSegReg::DS, ds_selector);
            cpu.load_ss(memory, ss_selector)?;
            cpu.load_data_segment(memory, gp64_cpu::DataSegReg::ES, es_selector);

            // Initialize FS and GS to null selector
            cpu.load_data_segment(memory, gp64_cpu::DataSegReg::FS, 0);
            cpu.load_data_segment(memory, gp64_cpu::DataSegReg::GS, 0);

            // Set IOPL=3 so ring 3 code can execute STI/CLI and I/O instructions
            cpu.regs.set_iopl(3);

            // Clear TF (Trap Flag) and NT (Nested Task) for clean PM entry.
            // On real hardware, these are always 0 when entering a DPMI client.
            // TF=1 would cause #DB exceptions after every instruction.
            // NT=1 would cause IRET to attempt a task switch, which fails at ring 3.
            cpu.regs.flags.remove(gp64_cpu::Flags::TRAP);
            cpu.regs.flags.remove(gp64_cpu::Flags::NESTED);

            cpu.allow_pm_real_mode_ss = true;

            // Register PM_BIOS_CODE_SEL as an interpreter break selector so FAR JMP/CALL
            // to BIOS HLE stubs (raw mode switch, callbacks) bail out of the interpreter
            // burst loop. Without this, the interpreter executes garbage at ROM addresses
            // instead of dispatching to the Rust HLE handler.
            cpu.add_hle_break_selector(PM_BIOS_CODE_SEL);

            // Both 16-bit and 32-bit clients start with segmented selectors (non-zero bases).
            // EIP/ESP are offsets within their respective segments. The 16-bit stub code
            // is responsible for setting up flat addressing before jumping to 32-bit code.
            cpu.regs.eip = ret_ip as u32;
            cpu.regs.esp = cpu.regs.sp() as u32;
            tracing::debug!(
                target: "gp64::dos::dpmi",
                ret_ip = format_args!("{:04X}", ret_ip),
                sp = format_args!("{:04X}", cpu.regs.sp()),
                cs_base = format_args!("{:08X}", cs_base),
                ss_base = format_args!("{:08X}", ss_base),
                is_32bit,
                "Protected mode entry: segmented model"
            );

            self.gdt_base = gdt_base as u32;
            self.gdt_size = gdt_size as u32;
            self.ldt_base = ldt_base as u32;
            self.ldt_size = ldt_size as u32;
            self.idt_base = idt_base as u32;
            self.idt_size = idt_size as u32;
            // The TSS, its IOPM, the ring-0 stack and the locked exception
            // stack are one contiguous span laid immediately after the IDT.
            // Record it so the linear allocator reserves it: its first-fit
            // search starts just past the IDT and would otherwise hand
            // `tss_base` straight to the client, whose writes then land on the
            // live TSS and the host stacks.
            self.host_control_base = tss_base as u32;
            self.host_control_size = (host_control_end - tss_base) as u32;

            // Debug: dump IDT entries 0 and 33 after setup
            for vec in [0u8, 31, 33, 49] {
                let (sel, off) = self.read_idt_gate(memory, idt_base, vec);
                tracing::debug!(
                    target: "gp64::dos::dpmi",
                    vector = format_args!("{:02X}", vec),
                    selector = format_args!("{:04X}", sel),
                    offset = format_args!("{:08X}", off),
                    "IDT entry after setup"
                );
            }

            tracing::debug!(
                target: "gp64::dos::dpmi",
                cs = format_args!("{:04X}", cpu.regs.cs),
                ip = format_args!("{:04X}", cpu.regs.ip()),
                cr0 = format_args!("{:08X}", cpu.regs.cr0),
                cpl = cpu.regs.cpl,
                "Mode switch complete"
            );

            Ok(())
        })();

        if let Err(err) = result {
            *self = dpmi_snapshot;
            cpu_snapshot.restore(cpu);
            memory.set_a20(a20_before);

            let error_code = match err.downcast_ref::<gp64_cpu::CpuError>() {
                Some(gp64_cpu::CpuError::GeneralProtectionFault)
                | Some(gp64_cpu::CpuError::StackFault) => DPMI_ERR_INVALID_SELECTOR,
                Some(gp64_cpu::CpuError::PageFault(_)) => DPMI_ERR_INVALID_LINEAR,
                _ => DPMI_ERR_UNSUPPORTED,
            };
            cpu.regs.set_ax(error_code);
            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            return Err(err);
        }

        Ok(())
    }
}
