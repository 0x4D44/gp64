//! Far CALL through a call gate, with today's root semantics.

use super::{DeliveryMemory, DeliveryObserver};
use crate::{Cpu, CpuError, Result};

impl Cpu {
    // =========================================================================
    // Call gate transitions (16-bit and 32-bit call gates, types 0x04/0x0C)
    // =========================================================================

    /// Perform a call gate transition.
    ///
    /// Call gates allow ring-3 code to call ring-0 services via CALL FAR.
    /// The gate descriptor specifies the target CS:IP/EIP and an optional
    /// parameter count for copying words/dwords between stacks on privilege
    /// change. The gate type determines the return-frame size.
    ///
    /// `gate_selector`: the selector that pointed to the call gate descriptor
    /// `return_cs`: CS to push as the return address
    /// `return_eip`: EIP to push as the return address
    #[cold]
    ///
    /// Observer-free entry: constructs `O::default()` for this delivery. Kept out of line
    /// (`#[inline(never)]`) to restore the baseline call structure at its callers.
    #[inline(never)]
    pub fn call_through_gate<M: DeliveryMemory<O>, O: DeliveryObserver<M> + Default>(
        &mut self,
        memory: &mut M,
        gate_selector: u16,
        return_cs: u16,
        return_eip: u32,
    ) -> Result<()> {
        self.call_through_gate_obs(
            memory,
            &mut O::default(),
            gate_selector,
            return_cs,
            return_eip,
        )
    }

    /// Observer-carrying form of [`Self::call_through_gate`].
    pub fn call_through_gate_obs<M: DeliveryMemory<O>, O: DeliveryObserver<M>>(
        &mut self,
        memory: &mut M,
        _obs: &mut O,
        gate_selector: u16,
        return_cs: u16,
        return_eip: u32,
    ) -> Result<()> {
        // Step 1: Read the gate descriptor raw bytes from GDT/LDT
        let gate_desc = match self.read_raw_descriptor(memory, gate_selector) {
            Some(desc) => desc,
            None => {
                tracing::error!(
                    target: "gp64::cpu",
                    selector = format_args!("{:04X}", gate_selector),
                    "call_through_gate: cannot read gate descriptor"
                );
                return Err(CpuError::GeneralProtectionFault);
            }
        };

        // Step 2: Extract gate fields
        let target_offset_low = u16::from_le_bytes([gate_desc[0], gate_desc[1]]);
        let target_cs = u16::from_le_bytes([gate_desc[2], gate_desc[3]]);
        let param_count = (gate_desc[4] & 0x1F) as u32;
        let gate_access = gate_desc[5];
        let gate_type = gate_access & 0x0F;
        let gate_dpl = (gate_access >> 5) & 3;
        let target_offset_high = u16::from_le_bytes([gate_desc[6], gate_desc[7]]);
        let is_32bit_gate = match gate_type {
            0x04 => false,
            0x0C => true,
            _ => {
                tracing::error!(
                    target: "gp64::cpu",
                    gate_selector = format_args!("{:04X}", gate_selector),
                    gate_access = format_args!("{:02X}", gate_access),
                    gate_type = format_args!("{:02X}", gate_type),
                    "call_through_gate: invalid descriptor type"
                );
                return Err(CpuError::GeneralProtectionFault);
            }
        };
        if (gate_access & 0x80) == 0 {
            return Err(CpuError::SegmentNotPresent(gate_selector));
        }
        let target_offset = if is_32bit_gate {
            (target_offset_low as u32) | ((target_offset_high as u32) << 16)
        } else {
            target_offset_low as u32
        };

        tracing::debug!(
            target: "gp64::cpu",
            gate_selector = format_args!("{:04X}", gate_selector),
            gate_kind = if is_32bit_gate { "32-bit" } else { "16-bit" },
            target_cs = format_args!("{:04X}", target_cs),
            target_offset = format_args!("{:08X}", target_offset),
            param_count,
            gate_dpl,
            cpl = self.regs.cpl,
            return_cs = format_args!("{:04X}", return_cs),
            return_eip = format_args!("{:08X}", return_eip),
            "call_through_gate"
        );

        // Step 3: Privilege check - max(CPL, gate selector RPL) <= gate DPL.
        let gate_rpl = (gate_selector & 0x03) as u8;
        if self.regs.cpl > gate_dpl || gate_rpl > gate_dpl {
            tracing::error!(
                target: "gp64::cpu",
                cpl = self.regs.cpl,
                gate_rpl,
                gate_dpl,
                gate_selector = format_args!("{:04X}", gate_selector),
                "call_through_gate: privilege exceeds gate DPL"
            );
            return Err(CpuError::GeneralProtectionFault);
        }

        // Step 4: Read the target CS descriptor
        let target_desc = match self.read_descriptor(memory, target_cs) {
            Some(desc) => desc,
            None => {
                tracing::error!(
                    target: "gp64::cpu",
                    target_cs = format_args!("{:04X}", target_cs),
                    "call_through_gate: cannot read target CS descriptor"
                );
                return Err(CpuError::GeneralProtectionFault);
            }
        };
        if !target_desc.present {
            return Err(CpuError::SegmentNotPresent(target_cs));
        }
        if !target_desc.is_code {
            return Err(CpuError::GeneralProtectionFault);
        }
        let target_dpl = target_desc.dpl;
        tracing::debug!(
            target: "gp64::cpu",
            target_cs = format_args!("{:04X}", target_cs),
            target_access = format_args!("{:02X}", target_desc.access),
            target_dpl,
            target_present = target_desc.present,
            target_conforming = target_desc.is_conforming,
            target_base = format_args!("{:08X}", target_desc.base),
            target_limit = format_args!("{:08X}", target_desc.effective_limit()),
            "call_through_gate target descriptor"
        );

        // Step 5: Determine if privilege change
        let old_cpl = self.regs.cpl;
        if target_dpl > old_cpl {
            return Err(CpuError::GeneralProtectionFault);
        }
        let privilege_change = !target_desc.is_conforming && target_dpl < old_cpl;
        if privilege_change {
            // Privilege change: ring 3 -> ring 0 (or similar)
            // Read SS0:ESP0 from TSS for the target ring
            let (new_esp, new_ss) = match self.read_tss_stack(memory, target_dpl) {
                Ok(stack) => stack,
                Err(err) => {
                    tracing::error!(
                        target: "gp64::cpu",
                        gate_selector = format_args!("{:04X}", gate_selector),
                        target_cpl = target_dpl,
                        old_cpl,
                        tr = format_args!("{:04X}", self.regs.tr),
                        tss_base = format_args!("{:08X}", self.regs.tss_base),
                        tss_limit = format_args!("{:08X}", self.regs.tss_limit),
                        cached_tss_ss0 = format_args!("{:04X}", self.regs.tss_ss0),
                        cached_tss_esp0 = format_args!("{:08X}", self.regs.tss_esp0),
                        err = ?err,
                        "call_through_gate: failed reading TSS stack"
                    );
                    return Err(err);
                }
            };
            tracing::debug!(
                target: "gp64::cpu",
                gate_selector = format_args!("{:04X}", gate_selector),
                target_cpl = target_dpl,
                old_cpl,
                new_ss = format_args!("{:04X}", new_ss),
                new_esp = format_args!("{:08X}", new_esp),
                tr = format_args!("{:04X}", self.regs.tr),
                tss_base = format_args!("{:08X}", self.regs.tss_base),
                tss_limit = format_args!("{:08X}", self.regs.tss_limit),
                cached_tss_ss0 = format_args!("{:04X}", self.regs.tss_ss0),
                cached_tss_esp0 = format_args!("{:08X}", self.regs.tss_esp0),
                "call_through_gate TSS stack"
            );

            // Save old SS:ESP before switching stacks
            let old_ss = self.regs.ss;
            let old_esp = self.regs.esp;
            let old_sp = self.regs.sp();
            let old_ss_cache = self.regs.ss_cache;
            let old_stack_offset = if old_ss_cache.big {
                old_esp
            } else {
                old_sp as u32
            };
            let restore_privilege_change = |cpu: &mut Cpu| {
                cpu.regs.set_cpl(old_cpl);
                cpu.regs.ss = old_ss;
                cpu.regs.ss_cache = old_ss_cache;
                cpu.regs.esp = old_esp;
                cpu.regs.set_sp(old_sp);
            };

            // Load new SS from TSS. Setter enforces SDM §20.1 V86 invariant.
            self.regs.set_cpl(target_dpl);
            if let Err(err) = self.deliver_load_ss(memory, new_ss) {
                tracing::error!(
                    target: "gp64::cpu",
                    gate_selector = format_args!("{:04X}", gate_selector),
                    new_ss = format_args!("{:04X}", new_ss),
                    new_esp = format_args!("{:08X}", new_esp),
                    target_cpl = target_dpl,
                    old_cpl,
                    err = ?err,
                    "call_through_gate: failed loading privilege stack SS"
                );
                restore_privilege_change(self);
                return Err(err);
            }
            if self.regs.ss_cache.big {
                self.regs.esp = new_esp;
            } else {
                self.regs.set_sp(new_esp as u16);
            }

            // Push old SS and old stack pointer onto the new stack.
            let stack_switch_result = if is_32bit_gate {
                self.deliver_try_push32(memory, old_ss as u32)
                    .and_then(|()| self.deliver_try_push32(memory, old_esp))
            } else {
                self.deliver_try_push16(memory, old_ss)
                    .and_then(|()| self.deliver_try_push16(memory, old_sp))
            };
            if let Err(err) = stack_switch_result {
                tracing::error!(
                    target: "gp64::cpu",
                    gate_selector = format_args!("{:04X}", gate_selector),
                    ss = format_args!("{:04X}", self.regs.ss),
                    sp = format_args!("{:04X}", self.regs.sp()),
                    esp = format_args!("{:08X}", self.regs.esp),
                    old_ss = format_args!("{:04X}", old_ss),
                    old_sp = format_args!("{:04X}", old_sp),
                    old_esp = format_args!("{:08X}", old_esp),
                    err = ?err,
                    "call_through_gate: failed pushing old stack"
                );
                restore_privilege_change(self);
                return Err(err);
            }

            // Copy parameters from the old stack to the new stack. Copy in
            // reverse push order so the first caller parameter remains closest
            // to the callee's return address.
            if param_count > 0 {
                // Read parameters from the OLD stack (above old SP/ESP).
                // Parameters are pushed by the caller before CALL FAR,
                // so they sit at old_ss:old_stack_offset upward.
                let old_ss_base = old_ss_cache.base;
                let param_size = if is_32bit_gate { 4 } else { 2 };
                for i in (0..param_count).rev() {
                    let param_addr = old_ss_base
                        .wrapping_add(old_stack_offset)
                        .wrapping_add(i * param_size);
                    if is_32bit_gate {
                        let param = memory.read_u32(param_addr as usize);
                        if let Err(err) = self.deliver_try_push32(memory, param) {
                            restore_privilege_change(self);
                            return Err(err);
                        }
                    } else {
                        let param = memory.read_u16(param_addr as usize);
                        if let Err(err) = self.deliver_try_push16(memory, param) {
                            restore_privilege_change(self);
                            return Err(err);
                        }
                    }
                }
            }

            // Push return CS and return offset.
            let return_push_result = if is_32bit_gate {
                self.deliver_try_push32(memory, return_cs as u32)
                    .and_then(|()| self.deliver_try_push32(memory, return_eip))
            } else {
                self.deliver_try_push16(memory, return_cs)
                    .and_then(|()| self.deliver_try_push16(memory, return_eip as u16))
            };
            if let Err(err) = return_push_result {
                tracing::error!(
                    target: "gp64::cpu",
                    gate_selector = format_args!("{:04X}", gate_selector),
                    ss = format_args!("{:04X}", self.regs.ss),
                    sp = format_args!("{:04X}", self.regs.sp()),
                    esp = format_args!("{:08X}", self.regs.esp),
                    err = ?err,
                    "call_through_gate: failed pushing return frame"
                );
                restore_privilege_change(self);
                return Err(err);
            }
        } else {
            // Same privilege: just push return address on current stack.
            if is_32bit_gate {
                self.deliver_try_push32(memory, return_cs as u32)?;
                self.deliver_try_push32(memory, return_eip)?;
            } else {
                self.deliver_try_push16(memory, return_cs)?;
                self.deliver_try_push16(memory, return_eip as u16)?;
            }
        }

        // Step 6: Load CS from the target code segment.
        // For non-conforming code, the visible CS RPL becomes the target CPL.
        // For conforming code, CPL remains unchanged.
        let target_cpl = if target_desc.is_conforming {
            old_cpl
        } else {
            target_dpl
        };
        let target_load_selector = (target_cs & !0x03) | target_cpl as u16;
        if let Err(err) = self.deliver_load_cs(memory, target_load_selector) {
            // load_cs may fail again with CallGate if target_cs itself is another gate.
            // That would be an error condition - treat as #GP.
            tracing::error!(
                target: "gp64::cpu",
                target_cs = format_args!("{:04X}", target_load_selector),
                err = %err,
                "call_through_gate: failed to load target CS"
            );
            return Err(CpuError::GeneralProtectionFault);
        }

        // Step 7: Set IP/EIP to the gate's target offset.
        if is_32bit_gate {
            self.regs.eip = target_offset;
        } else {
            self.regs.set_ip(target_offset as u16);
        }

        tracing::debug!(
            target: "gp64::cpu",
            cs = format_args!("{:04X}", self.regs.cs),
            eip = format_args!("{:08X}", self.regs.eip),
            ss = format_args!("{:04X}", self.regs.ss),
            esp = format_args!("{:08X}", self.regs.esp),
            cpl = self.regs.cpl,
            "call_through_gate complete"
        );

        Ok(())
    }
}
