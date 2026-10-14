use super::*;

// ── Descriptor flag bit constants ──────────────────────────────────────────
/// Granularity bit (byte 6, bit 7): limit is in 4K pages when set.
const DESC_G_BIT: u8 = 0x80;
/// Default operand-size / D/B bit (byte 6, bit 6): 32-bit segment when set.
const DESC_D_BIT: u8 = 0x40;
/// Upper nibble mask for the flags byte (G, D/B, L, AVL).
const DESC_FLAG_MASK: u8 = 0xF0;
/// Access byte for a present, DPL=3, read/write data segment.
const DESC_ACCESS_RW_DATA: u8 = 0xF2;
/// Read/write data access rights including the accessed bit.
const DESC_ACCESS_RW_DATA_ACCESSED: u8 = 0xF3;

// ── DPMI result helpers ────────────────────────────────────────────────────
/// Set a DPMI error: store error code in AX and set the carry flag.
#[inline]
fn dpmi_error(cpu: &mut Cpu, error_code: u16) {
    cpu.regs.set_ax(error_code);
    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
}

/// Signal DPMI success: clear the carry flag.
#[inline]
fn dpmi_success(cpu: &mut Cpu) {
    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
}

impl Dpmi {
    pub(super) fn descriptor_services(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        let subfunc = cpu.regs.al();

        match subfunc {
            // Allocate LDT descriptors
            0x00 => {
                let count = cpu.regs.cx();
                if count == 0 {
                    dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                } else {
                    let selector = self.allocate_descriptors(cpu, memory, count);
                    if selector == 0 {
                        dpmi_error(cpu, DPMI_ERR_DESC_UNAVAILABLE);
                    } else {
                        cpu.regs.set_ax(selector);
                        dpmi_success(cpu);
                    }
                }
            }

            // Free LDT descriptor
            0x01 => {
                let Some(sel) = self.validate_ldt_selector(cpu, cpu.regs.bx(), 1) else {
                    return Ok(());
                };
                // Check if selector exists before freeing
                if self.selectors.iter().any(|s| s.selector == sel) {
                    self.write_selector_descriptor(cpu, memory, sel, 0, 0, 0, 0);
                    self.selectors.retain(|s| s.selector != sel);
                    self.free_selectors.push(sel);
                    for entry in self.segment_selectors.iter_mut() {
                        if *entry == sel {
                            *entry = 0;
                        }
                    }
                    // B3: Invalidate segment cache in case selector is still loaded
                    cpu.invalidate_segment_cache(memory, sel);
                    dpmi_success(cpu);
                } else {
                    dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                }
            }

            // Segment to descriptor
            0x02 => {
                // Convert real-mode segment to protected-mode selector
                let segment = cpu.regs.bx();
                let Some(selector) = self.segment_to_descriptor(cpu, memory, segment) else {
                    dpmi_error(cpu, DPMI_ERR_DESC_UNAVAILABLE);
                    return Ok(());
                };
                cpu.regs.set_ax(selector);
                dpmi_success(cpu);
            }

            // Get selector increment value
            0x03 => {
                cpu.regs.set_ax(8);
                dpmi_success(cpu);
            }

            // Lock selector (track count, validate selector)
            0x04 => {
                let Some(sel) = self.validate_ldt_selector(cpu, cpu.regs.bx(), 1) else {
                    return Ok(());
                };
                if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == sel) {
                    s.lock_count = s.lock_count.saturating_add(1);
                    dpmi_success(cpu);
                } else {
                    dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                }
            }

            // Unlock selector (track count, validate selector)
            0x05 => {
                let Some(sel) = self.validate_ldt_selector(cpu, cpu.regs.bx(), 1) else {
                    return Ok(());
                };
                if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == sel) {
                    if s.lock_count > 0 {
                        s.lock_count -= 1;
                    }
                    dpmi_success(cpu);
                } else {
                    dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                }
            }

            // Get segment base address
            0x06 => {
                let Some(sel) = self.validate_ldt_selector(cpu, cpu.regs.bx(), 1) else {
                    return Ok(());
                };
                if let Some(s) = self.selectors.iter().find(|s| s.selector == sel) {
                    cpu.regs.set_cx((s.base >> 16) as u16);
                    cpu.regs.set_dx(s.base as u16);
                    dpmi_success(cpu);
                } else {
                    dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                }
            }

            // Set segment base address
            0x07 => {
                let Some(sel) = self.validate_ldt_selector(cpu, cpu.regs.bx(), 1) else {
                    return Ok(());
                };
                let base = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
                if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == sel) {
                    s.base = base;
                    let (b, l, a, f) = (s.base, s.limit, s.access, s.flags);
                    self.write_selector_descriptor(cpu, memory, sel, b, l, a, f);
                    dpmi_trace_selector_update(cpu, "desc_set_access", sel, b, l, a, f);
                    dpmi_trace_selector_update(cpu, "desc_set_limit", sel, b, l, a, f);
                    dpmi_trace_selector_update(cpu, "desc_set_base", sel, b, l, a, f);
                    // Per HLD §11.1: Invalidate CPU segment caches if this selector is loaded
                    cpu.invalidate_segment_cache(memory, sel);
                    dpmi_success(cpu);
                } else {
                    dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                }
            }

            // Set segment limit
            0x08 => {
                let Some(sel) = self.validate_ldt_selector(cpu, cpu.regs.bx(), 1) else {
                    return Ok(());
                };
                let limit = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
                if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == sel) {
                    s.limit = limit;
                    // Note: We do NOT modify s.flags here.
                    // The G bit (granularity) is computed automatically in encode_descriptor_bytes
                    // when writing to memory, based on the limit value.
                    // User-set flags via SetAccess (INT 31h/0009) are preserved.
                    let (b, l, a, f) = (s.base, s.limit, s.access, s.flags);
                    self.write_selector_descriptor(cpu, memory, sel, b, l, a, f);
                    // Per HLD §11.1: Invalidate CPU segment caches if this selector is loaded
                    cpu.invalidate_segment_cache(memory, sel);
                    dpmi_success(cpu);
                } else {
                    dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                }
            }

            // Set descriptor access rights
            0x09 => {
                let Some(sel) = self.validate_ldt_selector(cpu, cpu.regs.bx(), 1) else {
                    return Ok(());
                };
                let access = cpu.regs.cl();
                let flags = cpu.regs.ch();

                if (access & 0x80) == 0 || (access & 0x10) == 0 {
                    dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                    return Ok(());
                }
                if (flags & 0x20) != 0 {
                    dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                    return Ok(());
                }
                if (access & 0x08) != 0 && ((access & 0x02) == 0 || (access & 0x04) != 0) {
                    dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                    return Ok(());
                }
                // DPMI spec: DPL must equal CPL
                let dpl = (access >> 5) & 3;
                if dpl != cpu.regs.cpl {
                    dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                    return Ok(());
                }

                // DPMI 0.9 spec §4.4: When CH=0, "the DPMI host will automatically
                // set the 'B' bit of data segments and the 'D' bit of executable
                // segments based on the default size of the DPMI client."
                //
                // For 16-bit clients (cpu_mode=0): D/B=0 (default 16-bit).
                // For 32-bit clients (cpu_mode=1): D/B=1 (default 32-bit).
                //
                // but passes CH=0 in function 0009 after setting up descriptors via
                // 000C (which also has D/B=0 in the buffer). Without auto-setting
                // D/B=1, all segments stay USE16 and 32-bit code GP-faults instantly.
                //
                // When CH != 0, use the explicit flags from the caller.
                // Preserve existing G bit and limit high bits when auto-setting.
                let (flags_to_store, flags_high, limit_hi) = if flags == 0 {
                    // CH=0: preserve existing flags (G, D/B, L, AVL) from the
                    // descriptor. Only the access byte (CL) is being changed.
                    //
                    // The DPMI 0.9 spec suggests auto-setting D/B from the
                    // this to convert 16-bit data segments to 16-bit code
                    // segments (e.g. selector 0047). Auto-setting D/B=1 would
                    // corrupt those segments. protected-mode clients also preserves existing
                    // flags when CH=0.
                    if let Some(s) = self.selectors.iter().find(|s| s.selector == sel) {
                        let existing_flags = s.flags;
                        let old_limit20 = if (existing_flags & DESC_G_BIT) != 0 {
                            s.limit >> 12
                        } else {
                            s.limit
                        };
                        (
                            existing_flags,
                            existing_flags & DESC_FLAG_MASK,
                            (old_limit20 >> 16) & 0x0F,
                        )
                    } else {
                        // Selector not in tracking list (shouldn't happen for
                        // allocated selectors). Fall back to client default.
                        let default_db = if self.cpu_mode == 1 { DESC_D_BIT } else { 0 };
                        (default_db, default_db & DESC_FLAG_MASK, 0u32)
                    }
                } else {
                    (flags, flags & DESC_FLAG_MASK, (flags & 0x0F) as u32)
                };

                let is_32bit = (flags_high & DESC_D_BIT) != 0;
                let is_code_segment = (access & 0x08) != 0;
                tracing::debug!(
                    target: "gp64::dos::dpmi",
                    sel = format_args!("{:04X}", sel),
                    access = format_args!("{:02X}", access),
                    flags = format_args!("{:02X}", flags_to_store),
                    is_32bit,
                    is_code = is_code_segment,
                    "DPMI 0009 Set access rights"
                );
                if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == sel) {
                    let old_limit20 = if (s.flags & DESC_G_BIT) != 0 {
                        s.limit >> 12
                    } else {
                        s.limit
                    };
                    let new_limit20 = (old_limit20 & 0xFFFF) | (limit_hi << 16);
                    let new_limit = if (flags_high & DESC_G_BIT) != 0 {
                        (new_limit20 << 12) | 0x0FFF
                    } else {
                        new_limit20
                    };
                    s.access = access;
                    // Store only the flags high nibble (G/D/L/AVL). The low nibble
                    // of the CH byte contains limit bits 16-19, which are already
                    // encoded in s.limit. Storing them in s.flags would leak stale
                    // limit bits that persist across free/realloc cycles.
                    s.flags = flags_to_store & DESC_FLAG_MASK;
                    s.limit = new_limit;
                    let (b, l, a, f) = (s.base, s.limit, s.access, s.flags);
                    self.write_selector_descriptor(cpu, memory, sel, b, l, a, f);
                    dpmi_trace_selector_update(cpu, "desc_set_access_rights", sel, b, l, a, f);
                    // Per HLD §11.1: Invalidate CPU segment caches if this selector is loaded
                    cpu.invalidate_segment_cache(memory, sel);
                    dpmi_success(cpu);
                } else {
                    dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                }
            }

            // Create code segment alias
            0x0A => {
                let Some(sel) = self.validate_ldt_selector(cpu, cpu.regs.bx(), 1) else {
                    return Ok(());
                };
                if let Some(s) = self.selectors.iter().find(|s| s.selector == sel) {
                    let (base, limit, flags) = (s.base, s.limit, s.flags);
                    let new_sel = self.allocate_descriptors(cpu, memory, 1);
                    if new_sel == 0 {
                        dpmi_error(cpu, DPMI_ERR_DESC_UNAVAILABLE);
                        return Ok(());
                    }
                    // allocate_descriptors already added the selector to self.selectors with base=0.
                    // We need to update it with the correct values from the source descriptor.
                    if let Some(slot) = self.selectors.iter_mut().find(|s| s.selector == new_sel) {
                        slot.base = base;
                        slot.limit = limit;
                        slot.access = DESC_ACCESS_RW_DATA;
                        slot.flags = flags;
                        self.write_selector_descriptor(
                            cpu,
                            memory,
                            new_sel,
                            base,
                            limit,
                            DESC_ACCESS_RW_DATA,
                            flags,
                        );
                        cpu.regs.set_ax(new_sel);
                        dpmi_success(cpu);
                    } else {
                        dpmi_error(cpu, DPMI_ERR_DESC_UNAVAILABLE);
                    }
                } else {
                    dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                }
            }

            // Get descriptor
            0x0B => {
                let selector = cpu.regs.bx();
                let Some(buffer) = self.client_pointer_with_access(
                    cpu,
                    memory,
                    cpu.regs.es,
                    self.client_offset(cpu),
                    PageAccess::Write,
                ) else {
                    return Ok(());
                };

                if (selector & 0x04) == 0 {
                    // GDT selector — read descriptor directly from GDT memory.
                    let index = (selector >> 3) as usize;
                    let required_limit = (index * 8 + 7) as u16;
                    if index == 0 || required_limit as usize >= self.gdt_size as usize {
                        tracing::debug!(
                            selector,
                            index,
                            gdt_limit = cpu.regs.gdtr.limit,
                            "INT31 000B: GDT selector rejected"
                        );
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    }
                    // Auto-expand GDTR limit if needed. The GDT memory region
                    // was allocated with gdt_size bytes; we just need to tell
                    // the CPU about the larger limit so read_descriptor works.
                    if required_limit > cpu.regs.gdtr.limit {
                        cpu.regs.gdtr.limit = required_limit;
                        cpu.mark_gdt_changed();
                        memory.set_gdt_snoop_range(cpu.regs.gdtr.base, cpu.regs.gdtr.limit);
                    }
                    let gdt_addr = cpu.regs.gdtr.base as usize + index * 8;
                    for i in 0..8 {
                        memory.write_u8(buffer + i, memory.read_u8(gdt_addr + i));
                    }
                    tracing::debug!(
                        selector,
                        index,
                        gdt_addr,
                        "INT31 000B: GDT descriptor read OK"
                    );
                    dpmi_success(cpu);
                } else {
                    // LDT selector — look up in our selector table.
                    let Some(sel) = self.canonical_ldt_selector(selector) else {
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    };
                    if !self.selector_in_ldt_range(cpu, sel, 1) {
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    }
                    if let Some(s) = self.selectors.iter().find(|s| s.selector == sel) {
                        // Write 8-byte descriptor to buffer
                        let (limit_lo, base_lo, base_mid, access, limit_flags, base_hi) =
                            self.encode_descriptor_bytes(s.base, s.limit, s.access, s.flags);
                        memory.write_u16(buffer, limit_lo);
                        memory.write_u16(buffer + 2, base_lo);
                        memory.write_u8(buffer + 4, base_mid);
                        memory.write_u8(buffer + 5, access);
                        memory.write_u8(buffer + 6, limit_flags);
                        memory.write_u8(buffer + 7, base_hi);
                        dpmi_success(cpu);
                    } else {
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                    }
                }
            }

            // Set descriptor
            0x0C => {
                let selector = cpu.regs.bx();

                if (selector & 0x04) == 0 {
                    // GDT selector — write descriptor directly to GDT memory.
                    let index = (selector >> 3) as usize;
                    let required_limit = (index * 8 + 7) as u16;
                    if index == 0 || required_limit as usize >= self.gdt_size as usize {
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    }
                    // Auto-expand GDTR limit if needed. The GDT memory region
                    // was allocated with gdt_size bytes; we can safely expand
                    // GDT descriptors for per-module LE object selectors.
                    if required_limit > cpu.regs.gdtr.limit {
                        cpu.regs.gdtr.limit = required_limit;
                        cpu.mark_gdt_changed();
                        memory.set_gdt_snoop_range(cpu.regs.gdtr.base, cpu.regs.gdtr.limit);
                        tracing::debug!(
                            target: "gp64::dos::dpmi",
                            selector,
                            index,
                            new_limit = required_limit,
                            "GDT auto-expanded for DPMI 000C"
                        );
                    }
                    let Some(buffer) = self.client_pointer_with_access(
                        cpu,
                        memory,
                        cpu.regs.es,
                        self.client_offset(cpu),
                        PageAccess::Read,
                    ) else {
                        return Ok(());
                    };
                    let limit_lo = memory.read_u16(buffer);
                    let base_lo = memory.read_u16(buffer + 2);
                    let base_mid = memory.read_u8(buffer + 4);
                    let mut access = memory.read_u8(buffer + 5);
                    let limit_flags = memory.read_u8(buffer + 6);
                    let base_hi = memory.read_u8(buffer + 7);
                    let base =
                        (base_lo as u32) | ((base_mid as u32) << 16) | ((base_hi as u32) << 24);
                    if (access & 0x10) == 0 {
                        dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                        return Ok(());
                    }
                    access = (access & !0x60) | ((cpu.regs.cpl & 0x03) << 5);
                    let limit20 = (limit_lo as u32) | (((limit_flags & 0x0F) as u32) << 16);
                    // Function 000C takes a complete 8-byte descriptor — respect the
                    // caller's flags byte as-is. Auto-setting D/B is only specified
                    // 32-bit flat segments (with D/B=1 in byte 6) and 16-bit thunks
                    // (with D/B=0). Overriding D/B here corrupts 16-bit segments.
                    let flags = limit_flags;
                    let limit = if (flags & DESC_G_BIT) != 0 {
                        (limit20 << 12) | 0x0FFF
                    } else {
                        limit20
                    };
                    let gdt_addr = cpu.regs.gdtr.base as usize + index * 8;
                    memory.write_u16(gdt_addr, limit_lo);
                    memory.write_u16(gdt_addr + 2, base_lo);
                    memory.write_u8(gdt_addr + 4, base_mid);
                    memory.write_u8(gdt_addr + 5, access);
                    memory.write_u8(gdt_addr + 6, flags);
                    memory.write_u8(gdt_addr + 7, base_hi);
                    cpu.mark_gdt_changed();
                    dpmi_trace_selector_update(
                        cpu,
                        "desc_set_gdt",
                        selector,
                        base,
                        limit,
                        access,
                        flags,
                    );
                    tracing::debug!(
                        selector,
                        index,
                        base,
                        limit,
                        access,
                        flags,
                        "INT31 000C: GDT descriptor written OK"
                    );
                    cpu.invalidate_segment_cache(memory, selector);
                    dpmi_success(cpu);
                } else {
                    // LDT selector — validate first, then read buffer and update.
                    let Some(sel) = self.canonical_ldt_selector(selector) else {
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    };
                    if !self.selector_in_ldt_range(cpu, sel, 1) {
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    }
                    let Some(buffer) = self.client_pointer_with_access(
                        cpu,
                        memory,
                        cpu.regs.es,
                        self.client_offset(cpu),
                        PageAccess::Read,
                    ) else {
                        return Ok(());
                    };
                    let limit_lo = memory.read_u16(buffer);
                    let base_lo = memory.read_u16(buffer + 2);
                    let base_mid = memory.read_u8(buffer + 4);
                    let mut access = memory.read_u8(buffer + 5);
                    let limit_flags = memory.read_u8(buffer + 6);
                    let base_hi = memory.read_u8(buffer + 7);
                    let base =
                        (base_lo as u32) | ((base_mid as u32) << 16) | ((base_hi as u32) << 24);
                    if (access & 0x10) == 0 {
                        dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                        return Ok(());
                    }
                    access = (access & !0x60) | ((cpu.regs.cpl & 0x03) << 5);
                    let limit20 = (limit_lo as u32) | (((limit_flags & 0x0F) as u32) << 16);
                    // Function 000C takes a complete 8-byte descriptor — respect the
                    // caller's flags byte as-is (same reasoning as GDT path above).
                    let flags = limit_flags;
                    let limit = if (flags & DESC_G_BIT) != 0 {
                        (limit20 << 12) | 0x0FFF
                    } else {
                        limit20
                    };
                    if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == sel) {
                        s.base = base;
                        s.limit = limit;
                        s.access = access;
                        s.flags = flags;
                        self.write_selector_descriptor(
                            cpu, memory, sel, base, limit, access, flags,
                        );
                        dpmi_trace_selector_update(
                            cpu,
                            "desc_set_raw",
                            sel,
                            base,
                            limit,
                            access,
                            flags,
                        );
                        // Per HLD §11.1: Invalidate CPU segment caches if this selector is loaded
                        cpu.invalidate_segment_cache(memory, sel);
                        dpmi_success(cpu);
                    } else {
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                    }
                }
            }

            // Allocate specific LDT descriptor
            0x0D => {
                let raw_bx = cpu.regs.bx();
                let Some(selector) = self.validate_ldt_selector(cpu, raw_bx, 1) else {
                    tracing::warn!(
                        target: "gp64::dos::dpmi",
                        raw_bx = format_args!("{:04X}", raw_bx),
                        "INT 31h/000Dh: validate_ldt_selector failed"
                    );
                    return Ok(());
                };
                // 000Dh reaches any selector the guest names, and advances the
                // cursor by its own route below, so it must ask the same
                // admission chokepoint the bump and free-list paths do.
                if !self.selector_run_admitted(selector, 1) {
                    tracing::warn!(
                        target: "gp64::dos::dpmi",
                        selector = format_args!("{:04X}", selector),
                        reserved = self.reserved_ldt_index,
                        "INT 31h/000Dh: selector lies in a reserved LDT range"
                    );
                    dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                } else if (selector >> 3) > 0x0F && !self.free_selectors.contains(&selector) {
                    tracing::warn!(
                        target: "gp64::dos::dpmi",
                        raw_bx = format_args!("{:04X}", raw_bx),
                        selector = format_args!("{:04X}", selector),
                        "INT 31h/000Dh: index > 0x0F and not in free_selectors"
                    );
                    dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                } else if self.selectors.iter().any(|s| s.selector == selector) {
                    // initial mode-switch selectors via 000Dh, expecting success
                    // even though the DPMI host already set them up.  Return the
                    // existing selector as-is.
                    cpu.regs.set_bx(selector);
                    dpmi_success(cpu);
                } else if !self.selector_table_has_room(1) {
                    // A full table refuses the claim, as AX=0000h does; the
                    // push below would otherwise panic (MDD-BUG-CRU-01401).
                    dpmi_error(cpu, DPMI_ERR_DESC_UNAVAILABLE);
                } else {
                    self.free_selectors.retain(|&sel| sel != selector);
                    if selector >= self.next_selector {
                        if let Some(next) = selector.checked_add(8) {
                            self.next_selector = next;
                        }
                    }
                    self.selectors.push(Selector {
                        selector,
                        base: 0,
                        limit: 0,
                        access: DESC_ACCESS_RW_DATA,
                        flags: 0,
                        lock_count: 0,
                    });
                    self.write_selector_descriptor(
                        cpu,
                        memory,
                        selector,
                        0,
                        0,
                        DESC_ACCESS_RW_DATA,
                        0,
                    );
                    cpu.invalidate_segment_cache(memory, selector);
                    cpu.regs.set_bx(selector);
                    dpmi_success(cpu);
                }
            }

            // Get multiple descriptors
            0x0E => {
                let count = cpu.regs.cx();
                if count == 0 {
                    cpu.regs.set_cx(0);
                    dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                    return Ok(());
                }
                let Some(buffer) = self.client_pointer_with_access(
                    cpu,
                    memory,
                    cpu.regs.es,
                    self.client_offset(cpu),
                    PageAccess::Write,
                ) else {
                    return Ok(());
                };

                let entry_size = 10usize;
                let total_bytes = match (count as usize).checked_mul(entry_size) {
                    Some(total) => total,
                    None => {
                        cpu.regs.set_cx(0);
                        dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                        return Ok(());
                    }
                };
                let end = match buffer.checked_add(total_bytes) {
                    Some(end) => end,
                    None => {
                        cpu.regs.set_cx(0);
                        dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                        return Ok(());
                    }
                };
                if end > memory.ram_size() {
                    cpu.regs.set_cx(0);
                    dpmi_error(cpu, DPMI_ERR_INVALID_LINEAR);
                    return Ok(());
                }

                for i in 0..count as usize {
                    // DPMI spec: 10-byte entries (selector[2] + descriptor[8])
                    let entry_addr = match i
                        .checked_mul(10)
                        .and_then(|offset| buffer.checked_add(offset))
                    {
                        Some(addr) => addr,
                        None => {
                            cpu.regs.set_cx(i as u16);
                            dpmi_error(cpu, DPMI_ERR_INVALID_LINEAR);
                            return Ok(());
                        }
                    };
                    let raw_sel = memory.read_u16(entry_addr);
                    let Some(sel) = self.canonical_ldt_selector(raw_sel) else {
                        cpu.regs.set_cx(i as u16);
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    };
                    if !self.selector_in_ldt_range(cpu, sel, 1) {
                        cpu.regs.set_cx(i as u16);
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    }

                    if let Some(s) = self.selectors.iter().find(|s| s.selector == sel) {
                        // Write 8-byte descriptor at offset 2 (after selector)
                        let (limit_lo, base_lo, base_mid, access, limit_flags, base_hi) =
                            self.encode_descriptor_bytes(s.base, s.limit, s.access, s.flags);
                        memory.write_u16(entry_addr + 2, limit_lo);
                        memory.write_u16(entry_addr + 4, base_lo);
                        memory.write_u8(entry_addr + 6, base_mid);
                        memory.write_u8(entry_addr + 7, access);
                        memory.write_u8(entry_addr + 8, limit_flags);
                        memory.write_u8(entry_addr + 9, base_hi);
                    } else {
                        cpu.regs.set_cx(i as u16);
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    }
                }
                dpmi_success(cpu);
            }

            // Set multiple descriptors
            0x0F => {
                let count = cpu.regs.cx();
                if count == 0 {
                    cpu.regs.set_cx(0);
                    dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                    return Ok(());
                }
                let Some(buffer) = self.client_pointer_with_access(
                    cpu,
                    memory,
                    cpu.regs.es,
                    self.client_offset(cpu),
                    PageAccess::Read,
                ) else {
                    return Ok(());
                };

                let entry_size = 10usize;
                let total_bytes = match (count as usize).checked_mul(entry_size) {
                    Some(total) => total,
                    None => {
                        cpu.regs.set_cx(0);
                        dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                        return Ok(());
                    }
                };
                let end = match buffer.checked_add(total_bytes) {
                    Some(end) => end,
                    None => {
                        cpu.regs.set_cx(0);
                        dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                        return Ok(());
                    }
                };
                if end > memory.ram_size() {
                    cpu.regs.set_cx(0);
                    dpmi_error(cpu, DPMI_ERR_INVALID_LINEAR);
                    return Ok(());
                }

                for i in 0..count as usize {
                    // DPMI spec: 10-byte entries (selector[2] + descriptor[8])
                    let entry_addr = match i
                        .checked_mul(10)
                        .and_then(|offset| buffer.checked_add(offset))
                    {
                        Some(addr) => addr,
                        None => {
                            cpu.regs.set_cx(i as u16);
                            dpmi_error(cpu, DPMI_ERR_INVALID_LINEAR);
                            return Ok(());
                        }
                    };
                    let raw_sel = memory.read_u16(entry_addr);
                    let Some(sel) = self.canonical_ldt_selector(raw_sel) else {
                        cpu.regs.set_cx(i as u16);
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    };

                    // Validate selector before reading descriptor data
                    if !self.selector_in_ldt_range(cpu, sel, 1) {
                        cpu.regs.set_cx(i as u16);
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    }
                    if !self.selectors.iter().any(|s| s.selector == sel) {
                        cpu.regs.set_cx(i as u16);
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    }

                    // Read 8-byte descriptor from buffer (starting at entry_addr + 2)
                    let limit_lo = memory.read_u16(entry_addr + 2);
                    let base_lo = memory.read_u16(entry_addr + 4);
                    let base_mid = memory.read_u8(entry_addr + 6);
                    let mut access = memory.read_u8(entry_addr + 7);
                    let flags_limit_hi = memory.read_u8(entry_addr + 8);
                    let base_hi = memory.read_u8(entry_addr + 9);
                    let base =
                        (base_lo as u32) | ((base_mid as u32) << 16) | ((base_hi as u32) << 24);
                    let limit20 = (limit_lo as u32) | (((flags_limit_hi & 0x0F) as u32) << 16);
                    let flags = flags_limit_hi;
                    // G-bit expansion: if granularity bit set, limit is in 4K pages
                    let limit = if (flags & DESC_G_BIT) != 0 {
                        (limit20 << 12) | 0x0FFF
                    } else {
                        limit20
                    };

                    if (access & 0x10) == 0 {
                        cpu.regs.set_cx(i as u16);
                        dpmi_error(cpu, DPMI_ERR_INVALID_VALUE);
                        return Ok(());
                    }
                    access = (access & !0x60) | ((cpu.regs.cpl & 0x03) << 5);

                    if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == sel) {
                        s.base = base;
                        s.limit = limit;
                        s.access = access;
                        s.flags = flags;
                        self.write_selector_descriptor(
                            cpu, memory, sel, base, limit, access, flags,
                        );
                        cpu.invalidate_segment_cache(memory, sel);
                    } else {
                        cpu.regs.set_cx(i as u16);
                        dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
                        return Ok(());
                    }
                }
                dpmi_success(cpu);
            }

            _ => {
                tracing::warn!("Unhandled DPMI descriptor function AL={subfunc:02X}");
                dpmi_error(cpu, DPMI_ERR_UNSUPPORTED);
            }
        }

        Ok(())
    }

    pub(super) fn selector_cache_from_ldt(&self, selector: u16) -> Option<SegmentCache> {
        let selector = self.canonical_ldt_selector(selector)?;
        let entry = self.selectors.iter().find(|s| s.selector == selector)?;
        let (limit_lo, base_lo, base_mid, access, limit_flags, base_hi) =
            self.encode_descriptor_bytes(entry.base, entry.limit, entry.access, entry.flags);
        let desc = [
            (limit_lo & 0xFF) as u8,
            (limit_lo >> 8) as u8,
            (base_lo & 0xFF) as u8,
            (base_lo >> 8) as u8,
            base_mid,
            access,
            limit_flags,
            base_hi,
        ];
        Some(SegmentCache::from_descriptor(&desc))
    }

    /// Validate a selector value: canonicalize it and check it fits in the LDT.
    /// On failure, sets the DPMI error on the CPU and returns `None`.
    /// `count` is the number of consecutive LDT entries required.
    pub(super) fn validate_ldt_selector(
        &self,
        cpu: &mut Cpu,
        raw_selector: u16,
        count: usize,
    ) -> Option<u16> {
        let Some(sel) = self.canonical_ldt_selector(raw_selector) else {
            dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
            return None;
        };
        if !self.selector_in_ldt_range(cpu, sel, count) {
            dpmi_error(cpu, DPMI_ERR_INVALID_SELECTOR);
            return None;
        }
        Some(sel)
    }

    pub(super) fn canonical_ldt_selector(&self, selector: u16) -> Option<u16> {
        // TI bit must be set (LDT selector). GDT selectors are not managed here.
        if (selector & 0x04) == 0 {
            return None;
        }
        // LDT index 0 is valid — unlike GDT[0] (the null descriptor), LDT[0]
        Some((selector & !0x7) | 0x7)
    }

    pub(super) fn ldt_entry_count(&self, cpu: &Cpu) -> usize {
        if cpu.regs.ldt_limit == 0 {
            // Treat unset LDT limit as effectively unbounded for early tests.
            return 0x10000 / 8;
        }
        (cpu.regs.ldt_limit as usize).saturating_add(1) / 8
    }

    pub(super) fn selector_in_ldt_range(&self, cpu: &Cpu, selector: u16, count: usize) -> bool {
        if (selector & 0x04) == 0 {
            return false;
        }
        let max_entries = self.ldt_entry_count(cpu);
        if max_entries == 0 {
            return false;
        }
        let index = (selector >> 3) as usize;
        index.saturating_add(count) <= max_entries
    }

    /// Encode selector fields into an 8-byte descriptor layout.
    pub(super) fn encode_descriptor_bytes(
        &self,
        base: u32,
        limit: u32,
        access: u8,
        flags: u8,
    ) -> (u16, u16, u8, u8, u8, u8) {
        let mut flags_n = (flags >> 4) & 0x0F;
        let mut limit20 = limit;

        if (flags_n & 0x8) != 0 {
            limit20 >>= 12;
        } else if limit > 0x000F_FFFF {
            flags_n |= 0x8;
            limit20 >>= 12;
        }

        let limit_lo = (limit20 & 0xFFFF) as u16;
        let limit_hi = ((limit20 >> 16) & 0x0F) as u8;
        let base_lo = (base & 0xFFFF) as u16;
        let base_mid = ((base >> 16) & 0xFF) as u8;
        let base_hi = ((base >> 24) & 0xFF) as u8;
        let limit_flags = limit_hi | ((flags_n & 0x0F) << 4);

        (limit_lo, base_lo, base_mid, access, limit_flags, base_hi)
    }

    pub(super) fn push_descriptor_ranges(&self, ranges: &mut Vec<(u32, u32)>) {
        if self.gdt_base != 0 && self.gdt_size != 0 {
            ranges.push((self.gdt_base, self.gdt_size));
        }
        if self.ldt_base != 0 && self.ldt_size != 0 {
            ranges.push((self.ldt_base, self.ldt_size));
        }
        if self.idt_base != 0 && self.idt_size != 0 {
            ranges.push((self.idt_base, self.idt_size));
        }
        // The TSS, its I/O-permission bitmap and the ring-0 stack. The CPU
        // reads the TSS out of guest memory on every privilege transition, so
        // this span is as unavailable to a client as the descriptor tables are.
        if self.host_control_base != 0 && self.host_control_size != 0 {
            ranges.push((self.host_control_base, self.host_control_size));
        }
    }

    pub(super) fn overlaps_descriptor_tables(&self, base: u32, end: u32) -> bool {
        let tables = [
            (self.gdt_base, self.gdt_size),
            (self.ldt_base, self.ldt_size),
            (self.idt_base, self.idt_size),
            (self.host_control_base, self.host_control_size),
        ];
        for (table_base, table_size) in tables {
            if table_base == 0 || table_size == 0 {
                continue;
            }
            let table_end = table_base.saturating_add(table_size);
            if base < table_end && end > table_base {
                return true;
            }
        }
        false
    }

    /// Helper to write a descriptor into a table (GDT/LDT).
    /// `limit` is the 20-bit descriptor limit field (already shifted if G=1).
    /// `flags` is the high nibble of descriptor byte 6 (G/D/L/AVL).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn write_descriptor_raw(
        &self,
        memory: &mut Memory,
        table_base: usize,
        index: usize,
        base: u32,
        limit: u32,
        access: u8,
        flags: u8,
    ) {
        let addr = table_base + index * 8;
        let limit20 = limit & 0x000F_FFFF;

        memory.write_u16(addr, (limit20 & 0xFFFF) as u16);
        memory.write_u16(addr + 2, (base & 0xFFFF) as u16);
        memory.write_u8(addr + 4, ((base >> 16) & 0xFF) as u8);
        memory.write_u8(addr + 5, access);
        memory.write_u8(
            addr + 6,
            (((limit20 >> 16) as u8) & 0x0F) | ((flags & 0x0F) << 4),
        );
        memory.write_u8(addr + 7, ((base >> 24) & 0xFF) as u8);
    }

    pub(super) fn write_idt_gate(
        &self,
        memory: &mut Memory,
        idt_base: usize,
        vector: u8,
        offset: u16,
        selector: u16,
        type_attr: u8,
    ) {
        let addr = idt_base + (vector as usize) * 8;
        memory.write_u16(addr, offset);
        memory.write_u16(addr + 2, selector);
        memory.write_u8(addr + 4, 0);
        memory.write_u8(addr + 5, type_attr);
        memory.write_u16(addr + 6, 0);
    }

    pub(super) fn write_idt_gate_full(
        &self,
        memory: &mut Memory,
        idt_base: usize,
        vector: u8,
        offset: u32,
        selector: u16,
        type_attr: u8,
    ) {
        let addr = idt_base + (vector as usize) * 8;
        memory.write_u16(addr, (offset & 0xFFFF) as u16);
        memory.write_u16(addr + 2, selector);
        memory.write_u8(addr + 4, 0);
        memory.write_u8(addr + 5, type_attr);
        memory.write_u16(addr + 6, ((offset >> 16) & 0xFFFF) as u16);
    }

    pub fn read_idt_gate(&self, memory: &Memory, idt_base: usize, vector: u8) -> (u16, u32) {
        let addr = idt_base + (vector as usize) * 8;
        let offset_low = memory.read_u16(addr) as u32;
        let selector = memory.read_u16(addr + 2);
        let offset_high = memory.read_u16(addr + 6) as u32;
        let offset = offset_low | (offset_high << 16);
        (selector, offset)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn write_selector_descriptor(
        &self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        selector: u16,
        base: u32,
        limit: u32,
        access: u8,
        flags: u8,
    ) -> bool {
        // Only LDT selectors are supported here (TI=1).
        if ((selector >> 2) & 1) == 0 {
            return false;
        }
        if cpu.regs.ldt_base == 0 {
            return false;
        }

        let index = (selector >> 3) as usize;
        let ldt_base = cpu.regs.ldt_base as usize;
        let ldt_limit = cpu.regs.ldt_limit as usize;
        let desc_off = index * 8;
        if desc_off + 7 > ldt_limit {
            return false;
        }

        let (limit_lo, _base_lo, _base_mid, access_b, limit_flags, _base_hi) =
            self.encode_descriptor_bytes(base, limit, access, flags);
        let limit20 = (limit_lo as u32) | (((limit_flags & 0x0F) as u32) << 16);
        let flags_n = limit_flags >> 4;

        let Some(phys) = cpu.translate_system_linear_write(memory, (ldt_base + desc_off) as u32)
        else {
            return false;
        };
        self.write_descriptor_raw(memory, phys, 0, base, limit20, access_b, flags_n);
        cpu.mark_ldt_changed();
        true
    }

    /// Get selector base address
    pub fn get_selector_base(&self, selector: u16) -> Option<u32> {
        let selector = self.canonical_ldt_selector(selector)?;
        self.selectors
            .iter()
            .find(|s| s.selector == selector)
            .map(|s| s.base)
    }

    /// Get selector info (base, limit, access, flags) for debugging
    pub fn get_selector_info(&self, selector: u16) -> Option<(u32, u32, u8, u8)> {
        let selector = self.canonical_ldt_selector(selector)?;
        self.selectors
            .iter()
            .find(|s| s.selector == selector)
            .map(|s| (s.base, s.limit, s.access, s.flags))
    }

    /// Check if selector has D-bit set (32-bit code/data segment)
    /// Returns true if D=1 (32-bit), false if D=0 (16-bit) or selector not found
    #[allow(dead_code)]
    pub(super) fn is_selector_32bit(&self, selector: u16) -> bool {
        // Check if it's an LDT selector (TI bit = 1)
        if (selector & 0x04) != 0 {
            // LDT selector - look up in our allocated selectors
            let canonical = (selector & !0x7) | 0x7;
            if let Some(s) = self.selectors.iter().find(|s| s.selector == canonical) {
                return (s.flags & DESC_D_BIT) != 0; // D-bit is bit 6
            }
            // Unknown LDT selector - default to 16-bit (safer)
            false
        } else {
            // GDT selector - check specific GDT indices
            // GDT layout set up during enter_protected_mode:
            //   Index 0: NULL
            //   Index 1: Client CS - ALWAYS 16-bit (returns to 16-bit DOS extender stub)
            //   Index 2: Client DS - matches cpu_mode
            //   Index 3: Client SS - matches cpu_mode
            //   Index 4: 16-bit ES - always 16-bit
            //   Index 5: Flat code - always 32-bit
            //   Index 6: Flat data - always 32-bit
            //   Index 7: BIOS code - always 16-bit
            let index = (selector >> 3) as usize;
            match index {
                0 => false,                  // NULL
                1 => false,                  // Client CS - always 16-bit
                4 => false,                  // 16-bit ES - always 16-bit
                7 => false,                  // BIOS code - always 16-bit
                5 | 6 => true,               // Flat segments - always 32-bit
                2 | 3 => self.cpu_mode == 1, // Client DS/SS - matches cpu_mode
                _ => self.cpu_mode == 1,     // Other GDT entries - assume matches cpu_mode
            }
        }
    }

    /// Check if selector has D-bit set, reading from memory if needed
    /// This is more accurate than is_selector_32bit() for selectors allocated by the
    /// DPMI client and configured via INT 31h/0009.
    #[allow(dead_code)]
    pub(super) fn is_selector_32bit_mem(&self, cpu: &Cpu, memory: &Memory, selector: u16) -> bool {
        // First try our internal tracking
        if (selector & 0x04) != 0 {
            // LDT selector
            let canonical = (selector & !0x7) | 0x7;
            if let Some(s) = self.selectors.iter().find(|s| s.selector == canonical) {
                let is_32 = (s.flags & DESC_D_BIT) != 0;
                tracing::debug!(
                    selector = format_args!("{:04X}", selector),
                    flags = format_args!("{:02X}", s.flags),
                    is_32,
                    "DPMI is_selector_32bit_mem found in internal list"
                );
                return is_32;
            }
            // Not in our internal list - read from LDT in memory
            tracing::debug!(
                selector = format_args!("{:04X}", selector),
                ldt_base = format_args!("{:08X}", cpu.regs.ldt_base),
                "DPMI is_selector_32bit_mem not in internal list"
            );
            if cpu.regs.ldt_base != 0 {
                let index = (selector >> 3) as usize;
                let Some(desc_addr) = cpu
                    .translate_system_linear_read(memory, cpu.regs.ldt_base + (index * 8) as u32)
                else {
                    return false;
                };
                // Descriptor byte 6 has the flags (G, D/B, L, AVL in high nibble)
                let flags_byte = memory.read_u8(desc_addr + 6);
                let is_32bit = (flags_byte & DESC_D_BIT) != 0;
                tracing::debug!(
                    selector = format_args!("{:04X}", selector),
                    ldt_base = format_args!("{:08X}", cpu.regs.ldt_base),
                    desc_addr = format_args!("{:08X}", desc_addr),
                    flags = format_args!("{:02X}", flags_byte),
                    is_32bit,
                    "DPMI is_selector_32bit_mem from LDT"
                );
                return is_32bit;
            }
            false
        } else {
            // GDT selector - use existing logic
            self.is_selector_32bit(selector)
        }
    }

    /// Create a selector for a DOS memory allocation (INT 21h AH=48h in PM).
    ///
    /// Allocates and tracks the same selector run used by DPMI function 0100h so
    /// AH=49h (free) and AH=4Ah (resize) can translate any run member back to the
    /// DOS segment. 16-bit clients receive one descriptor per 64 KiB; 32-bit
    /// clients retain the single-descriptor policy. This ownership record stays
    /// separate from INT 31h/0002h's permanent segment-to-descriptor cache.
    /// Returns `Some(selector)` on success, `None` if tracking or descriptors are unavailable.
    pub fn dos_alloc_selector(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        segment: u16,
        paragraphs: u16,
    ) -> Option<u16> {
        let paragraphs = paragraphs.max(1);
        if self.dos_memory_blocks.is_full() {
            return None;
        }
        let selector_count = self.dos_memory_selector_count(paragraphs);
        let selector = self.allocate_descriptors(cpu, memory, selector_count);
        if selector == 0 {
            return None;
        }
        self.dos_memory_blocks.push(DosMemoryBlock {
            segment,
            selector,
            paragraphs,
            selector_count,
        });
        self.update_dos_memory_descriptors(
            cpu,
            memory,
            segment,
            paragraphs,
            selector,
            selector_count,
        );
        Some(selector)
    }

    /// DPMI 0002h: the one selector that maps real-mode `segment` (64 KB
    /// read/write data), allocated on first use and reused after. `None` when
    /// no descriptor is free.
    pub fn segment_to_descriptor(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        segment: u16,
    ) -> Option<u16> {
        let cached_sel = self.segment_selectors[segment as usize];
        if cached_sel != 0 {
            if self.selectors.iter().any(|s| s.selector == cached_sel) {
                return Some(cached_sel);
            }
            self.segment_selectors[segment as usize] = 0;
        }
        let selector = self.allocate_descriptors(cpu, memory, 1);
        if selector == 0 {
            return None;
        }
        if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == selector) {
            s.base = (segment as u32) << 4;
            s.limit = 0xFFFF;
            s.access = DESC_ACCESS_RW_DATA;
            s.flags = 0;
        }
        self.write_selector_descriptor(
            cpu,
            memory,
            selector,
            (segment as u32) << 4,
            0xFFFF,
            DESC_ACCESS_RW_DATA,
            0,
        );
        self.segment_selectors[segment as usize] = selector;
        Some(selector)
    }

    /// A data selector for real-mode `segment`, for pointers a translating
    /// host hands a PM INT 21h caller (the PSP from 51h/62h, the default DTA
    /// from 2Fh). The client's own PSP selector, or any data selector already
    /// based on `segment`, is reused so the identity round trips; otherwise a
    /// DPMI 0002h selector is made.
    pub fn data_selector_for_segment(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        segment: u16,
    ) -> Option<u16> {
        let base = (segment as u32) << 4;
        let existing = self
            .selectors
            .iter()
            .find(|s| s.base == base && s.access & 0x18 == 0x10)
            .map(|s| s.selector);
        existing.or_else(|| self.segment_to_descriptor(cpu, memory, segment))
    }

    /// The real-mode segment a PM caller's selector maps (a PSP or an EXEC
    /// environment), when its base is a paragraph-aligned conventional-memory
    /// address.
    pub fn selector_to_conventional_segment(&self, selector: u16) -> Option<u16> {
        let selector = self.canonical_ldt_selector(selector)?;
        let base = self.selectors.iter().find(|s| s.selector == selector)?.base;
        (base % 16 == 0 && base < 0x10_0000).then_some((base >> 4) as u16)
    }

    /// Look up the real-mode segment for a selector created by `dos_alloc_selector`.
    /// Returns `Some(segment)` if found, `None` otherwise.
    pub fn dos_selector_to_segment(&self, selector: u16) -> Option<u16> {
        let selector = self.canonical_ldt_selector(selector)?;
        if let Some(pos) = self.dos_memory_block_index_for_selector(selector) {
            return Some(self.dos_memory_blocks[pos].segment);
        }
        self.segment_selectors
            .iter()
            .enumerate()
            .find(|(_, &sel)| sel == selector)
            .map(|(seg, _)| seg as u16)
    }

    /// Free a selector previously created by `dos_alloc_selector`.
    pub fn dos_free_selector(&mut self, cpu: &mut Cpu, memory: &mut Memory, selector: u16) {
        let Some(selector) = self.canonical_ldt_selector(selector) else {
            return;
        };
        if let Some(pos) = self.dos_memory_block_index_for_selector(selector) {
            let block = self.dos_memory_blocks.remove(pos);
            self.free_dos_memory_selectors(cpu, memory, block.selector, block.selector_count);
            return;
        }

        // Remove the segment↔selector mapping
        for entry in self.segment_selectors.iter_mut() {
            if *entry == selector {
                *entry = 0;
            }
        }
        // Free the LDT descriptor
        if let Some(pos) = self.selectors.iter().position(|s| s.selector == selector) {
            self.selectors.swap_remove(pos);
        }
        // Zero out the LDT entry and recycle the selector
        self.write_selector_descriptor(cpu, memory, selector, 0, 0, 0, 0);
        if !self.free_selectors.contains(&selector) {
            self.free_selectors.push(selector);
        }
    }

    /// Check whether AH=4Ah can grow an existing selector run without mutating
    /// either the DOS allocation or descriptor state.
    pub(crate) fn dos_can_resize_selector(
        &self,
        cpu: &Cpu,
        selector: u16,
        new_paragraphs: u16,
    ) -> bool {
        self.dos_max_resize_selector_paragraphs(cpu, selector)
            .is_some_and(|maximum| new_paragraphs <= maximum)
    }

    /// Maximum size a tracked DOS allocation can represent without relocating
    /// or colliding with another descriptor. Used for AH=4Ah's BX failure result.
    pub(crate) fn dos_max_resize_selector_paragraphs(
        &self,
        cpu: &Cpu,
        selector: u16,
    ) -> Option<u16> {
        let selector = self.canonical_ldt_selector(selector)?;
        let Some(pos) = self.dos_memory_block_index_for_selector(selector) else {
            // Legacy single-selector mappings do not need adjacent descriptors.
            return self
                .segment_selectors
                .contains(&selector)
                .then_some(u16::MAX);
        };
        let block = &self.dos_memory_blocks[pos];
        if self.cpu_mode != 0 {
            return Some(u16::MAX);
        }

        let mut maximum_count = block.selector_count;
        while maximum_count < 16 {
            let candidate = maximum_count + 1;
            if !self.can_extend_dos_memory_selectors(
                cpu,
                block.selector,
                block.selector_count,
                candidate,
            ) {
                break;
            }
            maximum_count = candidate;
        }
        Some(
            (maximum_count as u32)
                .saturating_mul(0x1000)
                .min(u16::MAX as u32) as u16,
        )
    }

    /// Update the selector run for AH=4Ah after the DOS allocation resized.
    pub fn dos_resize_selector(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        selector: u16,
        new_paragraphs: u16,
    ) {
        let Some(selector) = self.canonical_ldt_selector(selector) else {
            return;
        };
        if let Some(pos) = self.dos_memory_block_index_for_selector(selector) {
            let new_paragraphs = new_paragraphs.max(1);
            let (segment, base_selector, old_count) = {
                let block = &self.dos_memory_blocks[pos];
                (block.segment, block.selector, block.selector_count)
            };
            let new_count = self.dos_memory_selector_count(new_paragraphs);
            debug_assert!(
                new_count <= old_count
                    || self.can_extend_dos_memory_selectors(
                        cpu,
                        base_selector,
                        old_count,
                        new_count
                    ),
                "INT 21h selector-run resize must be preflighted"
            );
            self.adjust_dos_memory_selectors(cpu, memory, base_selector, old_count, new_count);
            self.dos_memory_blocks[pos].paragraphs = new_paragraphs;
            self.dos_memory_blocks[pos].selector_count = new_count;
            self.update_dos_memory_descriptors(
                cpu,
                memory,
                segment,
                new_paragraphs,
                base_selector,
                new_count,
            );
            return;
        }

        let new_limit = ((new_paragraphs as u32) << 4).saturating_sub(1);
        if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == selector) {
            s.limit = new_limit;
            let base = s.base;
            let access = s.access;
            let flags = s.flags;
            self.write_selector_descriptor(cpu, memory, selector, base, new_limit, access, flags);
        }
    }

    /// Reserve every LDT index from `first_index` upward for another owner, so
    /// DPMI never hands one out.
    ///
    /// Set by the root when a 32-bit process hosts a 16-bit DLL: the flat-thunk
    /// window's LDT has its upper indices claimed by the `MapLS` table and the
    /// hosted 16-bit HLE code selectors, and DPMI's own cursor starts at index
    /// 32 with no ceiling of its own. Idempotent; the lower (more restrictive)
    /// reservation wins, so two owners cannot widen each other's range.
    ///
    pub fn reserve_ldt_indices_from(&mut self, first_index: u16) {
        self.reserved_ldt_index = Some(match self.reserved_ldt_index {
            Some(existing) => existing.min(first_index),
            None => first_index,
        });
    }

    /// Release the reservation set by [`Self::reserve_ldt_indices_from`].
    pub fn clear_ldt_reservation(&mut self) {
        self.reserved_ldt_index = None;
    }

    /// The first LDT index DPMI must not use, if any.
    pub fn reserved_ldt_index(&self) -> Option<u16> {
        self.reserved_ldt_index
    }

    /// **The single admission chokepoint.** May DPMI hand out `count`
    /// consecutive descriptors beginning at selector `first`?
    ///
    /// Every allocation path calls this — the bump cursor, both free-list reuse
    /// routes, the multi-descriptor run search, `INT 31h/000Dh`, and DOS-memory
    /// selector-run growth. A run that *starts* below the reservation and ends
    /// at or above it is refused as a whole, which is the case a cursor cap
    /// silently admits.
    pub fn selector_run_admitted(&self, first: u16, count: u16) -> bool {
        let first_index = u32::from(first >> 3);
        // A zero count is a degenerate single entry everywhere else in this
        // module, so treat it the same way rather than admitting it vacuously.
        let last_index = first_index + u32::from(count.max(1)) - 1;
        if self.run_hits_system_slots(first_index, last_index) {
            return false;
        }
        let Some(reserved) = self.reserved_ldt_index else {
            return true;
        };
        last_index < u32::from(reserved)
    }

    /// Mark the LDT as a 32-bit system LDT whose null entry and flat code/data
    /// entries (`flat_code_index` and the one after it) DPMI never hands out,
    /// whatever route asks (bump cursor, free list, `INT 31h/000Dh`).
    ///
    pub fn reserve_system_ldt_slots(&mut self, flat_code_index: u16) {
        self.system_flat_ldt_index = Some(flat_code_index);
    }

    /// The 32-bit system LDT's flat code index, if the LDT is one.
    pub fn system_flat_ldt_index(&self) -> Option<u16> {
        self.system_flat_ldt_index
    }

    /// Whether indices `first..=last` touch a 32-bit system slot.
    fn run_hits_system_slots(&self, first: u32, last: u32) -> bool {
        let Some(flat) = self.system_flat_ldt_index else {
            return false;
        };
        let flat = u32::from(flat);
        first == 0 || (first <= flat + 1 && last >= flat)
    }

    /// Allocate LDT descriptors (helper for 16-bit loader)
    /// Returns the first selector, or 0 on failure
    /// Whether `additional` more selectors fit in the fixed selector table.
    ///
    /// Every route that records a new selector checks this first, because
    /// the table's push panics at capacity (MDD-BUG-CRU-01401).
    pub(crate) fn selector_table_has_room(&self, additional: usize) -> bool {
        self.selectors.len().saturating_add(additional) <= self.selectors.capacity()
    }

    pub fn allocate_descriptors(&mut self, cpu: &mut Cpu, memory: &mut Memory, count: u16) -> u16 {
        if count == 0 {
            return 0;
        }

        // Check capacity — return 0 (failure) if we'd exceed the selector table
        if !self.selector_table_has_room(count as usize) {
            return 0;
        }

        if count == 1 {
            while let Some(selector) = self.free_selectors.pop() {
                // A reserved-range selector must never have entered this list
                // (DPMI can only free what it allocated, and allocation is
                // gated below), so dropping one here is the same defensive
                // discard the out-of-LDT-range case already performs.
                if !self.selector_in_ldt_range(cpu, selector, 1)
                    || !self.selector_run_admitted(selector, 1)
                {
                    continue;
                }
                self.selectors.push(Selector {
                    selector,
                    base: 0,
                    limit: 0xFFFF,
                    access: DESC_ACCESS_RW_DATA,
                    flags: 0,
                    lock_count: 0,
                });
                self.write_selector_descriptor(
                    cpu,
                    memory,
                    selector,
                    0,
                    0xFFFF,
                    DESC_ACCESS_RW_DATA,
                    0,
                );
                // B21: Invalidate segment cache when reusing freed selector
                cpu.invalidate_segment_cache(memory, selector);
                return selector;
            }
        }

        if count > 1 && !self.free_selectors.is_empty() {
            let max_entries = self.ldt_entry_count(cpu);
            let reserved = self.reserved_ldt_index;
            self.free_selectors.retain(|&selector| {
                if (selector & 0x04) == 0 || max_entries == 0 {
                    return false;
                }
                let index = (selector >> 3) as usize;
                if let Some(reserved) = reserved {
                    if index >= reserved as usize {
                        return false;
                    }
                }
                index.saturating_add(1) <= max_entries
            });
            if self.free_selectors.len() >= count as usize {
                let mut sorted = self.free_selectors.clone();
                sorted.sort_unstable();
                let mut run_len = 1usize;
                for i in 1..sorted.len() {
                    if sorted[i] == sorted[i - 1].wrapping_add(8) {
                        run_len += 1;
                    } else {
                        run_len = 1;
                    }
                    if run_len == count as usize {
                        let start = sorted[i + 1 - run_len];
                        for idx in 0..count {
                            let selector = start.wrapping_add(idx * 8);
                            if let Some(pos) =
                                self.free_selectors.iter().position(|&s| s == selector)
                            {
                                self.free_selectors.swap_remove(pos);
                            }
                            self.selectors.push(Selector {
                                selector,
                                base: 0,
                                limit: 0xFFFF,
                                access: DESC_ACCESS_RW_DATA,
                                flags: 0,
                                lock_count: 0,
                            });
                            self.write_selector_descriptor(
                                cpu,
                                memory,
                                selector,
                                0,
                                0xFFFF,
                                DESC_ACCESS_RW_DATA,
                                0,
                            );
                            cpu.invalidate_segment_cache(memory, selector);
                        }
                        return start;
                    }
                }
            }
        }

        // On a 32-bit system LDT the bump cursor steps over the flat code/data
        // pair: a run that would touch it starts just past it instead.
        if let Some(flat) = self.system_flat_ldt_index {
            let first_index = u32::from(self.next_selector >> 3);
            let last_index = first_index + u32::from(count.max(1)) - 1;
            if first_index <= u32::from(flat) + 1 && last_index >= u32::from(flat) {
                self.next_selector = ((flat + 2) << 3) | 7;
            }
        }
        if !self.selector_in_ldt_range(cpu, self.next_selector, count as usize) {
            return 0;
        }

        // The bump path is the one a run can straddle: `next_selector` may sit
        // below the reservation while `next_selector + count*8` runs past it.
        // Admit the whole run or none of it.
        if !self.selector_run_admitted(self.next_selector, count) {
            return 0;
        }

        // Check for overflow before allocating (use u32 to avoid truncation)
        let required_selectors = count as u32 * 8;
        if (self.next_selector as u32)
            .checked_add(required_selectors)
            .is_none_or(|end| end > 0xFFFF)
        {
            return 0;
        }

        let first = self.next_selector;
        for _ in 0..count {
            let selector = self.next_selector;
            self.selectors.push(Selector {
                selector,
                base: 0,
                limit: 0xFFFF,
                access: DESC_ACCESS_RW_DATA,
                flags: 0,
                lock_count: 0,
            });
            self.write_selector_descriptor(
                cpu,
                memory,
                selector,
                0,
                0xFFFF,
                DESC_ACCESS_RW_DATA,
                0,
            );
            self.next_selector += 8;
        }

        first
    }

    /// Number of 64 KB tiles required to back a block of `size` bytes.
    ///
    /// A global block larger than one 64 KB tile is backed with
    /// `ceil(size / 64 KB)` contiguous LDT selectors (stride `__AHINCR` = 8), one
    /// per 64 KB window. A zero-size block still owns one (degenerate) tile.
    #[inline]
    pub fn tile_count_for_size(size: u32) -> u16 {
        // ceil(size / 64 KB), at least 1.  Clamp to u16::MAX so a pathological
        // u32 size can never wrap the count (the LDT can hold ≤ 8192 selectors
        // anyway, so allocate_descriptors rejects anything this large).
        let tiles = (((size as u64) + 0xFFFF) >> 16).max(1);
        tiles.min(u16::MAX as u64) as u16
    }

    /// Allocate a tiled LDT selector run and return its base selector, or 0
    /// on failure. This implementation assigns consecutive selectors with
    /// stride 8, one per 64 KB window, for huge-pointer arithmetic.
    ///
    /// Each tile `t` has base `base + t*0x10000` and limit
    /// `(size - 1) - t*0x10000`. Tile 0 therefore covers the whole allocation
    /// for 32-bit offsets; subsequent tiles cover the remaining bytes for
    /// 16-bit offsets. This is allocation policy, not a DPMI requirement.
    ///
    /// The caller records the base selector and tile count. Free the entire
    /// run with [`free_tiled_descriptors`].
    pub fn alloc_tiled_block_selector(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        base: u32,
        size: u32,
    ) -> u16 {
        let tiles = Self::tile_count_for_size(size);
        let first = self.allocate_descriptors(cpu, memory, tiles);
        if first == 0 {
            return 0;
        }
        // Whole-block limit (size - 1), shrinking by one 64 KB window per tile so
        // tile 0 spans the entire allocation and each later tile spans from its
        // own base to the block end, following the allocation policy above.
        let full_limit = size.saturating_sub(1);
        for t in 0..tiles {
            let sel = first.wrapping_add(t * 8);
            self.set_descriptor_base(cpu, memory, sel, base + (t as u32) * 0x10000);
            let lim = full_limit.saturating_sub((t as u32) * 0x10000);
            self.set_descriptor_limit(cpu, memory, sel, lim);
            // Newly allocated global-block data descriptors include the accessed
            // bit, so LAR reports the same rights before and after loading.
            self.set_descriptor_access(cpu, memory, sel, DESC_ACCESS_RW_DATA_ACCESSED, 0);
        }
        first
    }

    /// Free a tiled selector run allocated by [`alloc_tiled_block_selector`].
    ///
    /// Releases `tiles` selectors starting at `first` (stride `__AHINCR` = 8) so
    /// they return to `free_selectors` as a contiguous run — the consecutive-run
    /// search in [`allocate_descriptors`] can then re-find them for a later large
    /// allocation. `tiles == 1` is exactly one [`free_descriptor`] call.
    pub fn free_tiled_descriptors(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        first: u16,
        tiles: u16,
    ) {
        for t in 0..tiles.max(1) {
            self.free_descriptor(cpu, memory, first.wrapping_add(t * 8));
        }
    }

    /// Free a single LDT descriptor (helper for 16-bit)
    pub fn free_descriptor(&mut self, cpu: &mut Cpu, memory: &mut Memory, selector: u16) -> bool {
        let Some(sel) = self.canonical_ldt_selector(selector) else {
            return false;
        };
        if !self.selector_in_ldt_range(cpu, sel, 1) {
            return false;
        }
        if self.selectors.iter().any(|s| s.selector == sel) {
            self.write_selector_descriptor(cpu, memory, sel, 0, 0, 0, 0);
            self.selectors.retain(|s| s.selector != sel);
            self.free_selectors.push(sel);
            // Do NOT invalidate_segment_cache here. On real x86 hardware, the
            // CPU's hidden descriptor cache registers are only updated when a
            // segment register is explicitly reloaded (MOV DS, LDS, POP DS, etc.).
            // Zeroing the LDT entry prevents future loads, but existing cached
            // values must remain valid to match real hardware behavior.
            return true;
        }
        false
    }

    /// Set descriptor base address (helper for 16-bit loader)
    /// Set or clear a descriptor's present bit, leaving the rest of the access
    /// byte alone.
    ///
    /// This is how a selector is *parked* rather than freed: the LDT slot stays
    /// owned by its allocator, but every load through it faults, which is what
    /// a discarded 16-bit global block needs (see `discard_global_block`). A
    /// freed descriptor cannot express that, because the slot may be handed to
    /// somebody else; a present descriptor with a zero base would silently read
    /// physical address 0 (the IVT) instead of faulting.
    ///
    /// Returns false when the selector is not an LDT selector this allocator
    /// owns.
    pub fn set_descriptor_present(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        selector: u16,
        present: bool,
    ) -> bool {
        let Some(canonical) = self.canonical_ldt_selector(selector) else {
            return false;
        };
        let Some(idx) = self.selectors.iter().position(|s| s.selector == canonical) else {
            return false;
        };
        let access = self.selectors[idx].access;
        let flags = self.selectors[idx].flags;
        let new_access = if present {
            access | 0x80
        } else {
            access & !0x80
        };
        if new_access == access {
            return true;
        }
        self.set_descriptor_access(cpu, memory, canonical, new_access, flags);
        true
    }

    pub fn set_descriptor_base(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        selector: u16,
        base: u32,
    ) {
        let Some(selector) = self.canonical_ldt_selector(selector) else {
            return;
        };
        let Some(idx) = self.selectors.iter().position(|s| s.selector == selector) else {
            return;
        };
        let (limit, access, flags) = {
            let s = &self.selectors[idx];
            (s.limit, s.access, s.flags)
        };
        let ldt_ready = cpu.regs.ldt_base != 0;
        if ldt_ready
            && !self.write_selector_descriptor(cpu, memory, selector, base, limit, access, flags)
        {
            return;
        }
        self.selectors[idx].base = base;
        if ldt_ready {
            cpu.invalidate_segment_cache(memory, selector);
        }
    }

    /// Set descriptor limit (helper for 16-bit loader)
    pub fn set_descriptor_limit(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        selector: u16,
        limit: u32,
    ) {
        let Some(selector) = self.canonical_ldt_selector(selector) else {
            return;
        };
        let Some(idx) = self.selectors.iter().position(|s| s.selector == selector) else {
            return;
        };
        let (base, access, flags) = {
            let s = &self.selectors[idx];
            (s.base, s.access, s.flags)
        };
        // Note: We do NOT modify s.flags here.
        // The G bit (granularity) is computed automatically in encode_descriptor_bytes
        // when writing to memory, based on the limit value.
        // User-set flags are preserved.
        let ldt_ready = cpu.regs.ldt_base != 0;
        if ldt_ready
            && !self.write_selector_descriptor(cpu, memory, selector, base, limit, access, flags)
        {
            return;
        }
        self.selectors[idx].limit = limit;
        if ldt_ready {
            cpu.invalidate_segment_cache(memory, selector);
        }
    }

    /// Get descriptor limit for a selector.
    pub fn get_descriptor_limit(&self, selector: u16) -> Option<u32> {
        let selector = self.canonical_ldt_selector(selector)?;
        self.selectors
            .iter()
            .find(|s| s.selector == selector)
            .map(|s| s.limit)
    }

    /// Get descriptor access rights for a selector.
    /// Returns (access_byte, flags_byte) or None if selector not found.
    /// Returns (access_byte, flags_byte) where flags_byte is the user-set value
    /// from SetAccessRights or SetDescriptor, preserved across SetLimit calls.
    pub fn get_descriptor_access(&self, selector: u16) -> Option<(u8, u8)> {
        let selector = self.canonical_ldt_selector(selector)?;
        self.selectors
            .iter()
            .find(|s| s.selector == selector)
            .map(|s| (s.access, s.flags))
    }

    /// Set descriptor access rights (helper for 16-bit loader)
    pub fn set_descriptor_access(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        selector: u16,
        access: u8,
        flags: u8,
    ) {
        let Some(selector) = self.canonical_ldt_selector(selector) else {
            return;
        };
        let Some(idx) = self.selectors.iter().position(|s| s.selector == selector) else {
            return;
        };
        let (base, limit) = {
            let s = &self.selectors[idx];
            (s.base, s.limit)
        };
        // Write the descriptor
        let ldt_ready = cpu.regs.ldt_base != 0;
        if ldt_ready
            && !self.write_selector_descriptor(cpu, memory, selector, base, limit, access, flags)
        {
            return;
        }
        self.selectors[idx].access = access;
        self.selectors[idx].flags = flags;
        if ldt_ready {
            cpu.invalidate_segment_cache(memory, selector);
        }
    }
}
