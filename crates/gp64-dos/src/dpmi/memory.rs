use super::*;

impl Dpmi {
    pub(super) fn dos_memory_selector_count(&self, paragraphs: u16) -> u16 {
        if self.cpu_mode == 0 {
            (paragraphs as u32).div_ceil(0x1000) as u16
        } else {
            1
        }
    }

    pub(super) fn dos_memory_block_index_for_selector(&self, selector: u16) -> Option<usize> {
        for (idx, block) in self.dos_memory_blocks.iter().enumerate() {
            if block.selector_count == 0 {
                continue;
            }
            if selector < block.selector {
                continue;
            }
            let delta = (selector - block.selector) as u32;
            if !delta.is_multiple_of(8) {
                continue;
            }
            let offset = delta / 8;
            if offset < block.selector_count as u32 {
                return Some(idx);
            }
        }
        None
    }

    pub(super) fn update_dos_memory_descriptors(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        segment: u16,
        paragraphs: u16,
        base_selector: u16,
        selector_count: u16,
    ) {
        let base_linear = (segment as u32) << 4;
        let mut remaining = paragraphs as u32;
        for idx in 0..selector_count {
            if remaining == 0 {
                break;
            }
            // 32-bit clients use a single descriptor that can cover the full
            // allocation (up to 1MB for DOS memory).  Only 16-bit clients
            // (selector_count > 1) need the 64KB-per-descriptor cap.
            let chunk_paragraphs = if selector_count == 1 {
                remaining
            } else {
                remaining.min(0x1000)
            };
            let selector = base_selector.wrapping_add(idx * 8);
            let base = base_linear + (idx as u32) * 0x10000;
            let limit = (chunk_paragraphs * 16).saturating_sub(1);
            if let Some(s) = self.selectors.iter_mut().find(|s| s.selector == selector) {
                s.base = base;
                s.limit = limit;
                s.access = 0xF2;
                s.flags = 0;
            } else {
                self.selectors.push(Selector {
                    selector,
                    base,
                    limit,
                    access: 0xF2,
                    flags: 0,
                    lock_count: 0,
                });
            }
            self.write_selector_descriptor(cpu, memory, selector, base, limit, 0xF2, 0);
            cpu.invalidate_segment_cache(memory, selector);
            remaining -= chunk_paragraphs;
        }
    }

    pub(super) fn free_dos_memory_selectors(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        base_selector: u16,
        selector_count: u16,
    ) {
        for idx in 0..selector_count {
            let selector = base_selector.wrapping_add(idx * 8);
            self.write_selector_descriptor(cpu, memory, selector, 0, 0, 0, 0);
            self.selectors.retain(|s| s.selector != selector);
            if !self.free_selectors.contains(&selector) {
                self.free_selectors.push(selector);
            }
            cpu.invalidate_segment_cache(memory, selector);
        }
    }

    pub(super) fn can_extend_dos_memory_selectors(
        &self,
        cpu: &Cpu,
        base_selector: u16,
        old_count: u16,
        new_count: u16,
    ) -> bool {
        let additional = new_count.saturating_sub(old_count) as usize;
        if !self.selector_table_has_room(additional) {
            return false;
        }
        // Validate the prospective run as a whole. Per-entry wrapping arithmetic
        // must never turn an LDT-end growth into a run beginning at selector 0007h.
        if !self.selector_in_ldt_range(cpu, base_selector, new_count as usize) {
            return false;
        }
        // Growing a run in place is a multi-descriptor allocation by another
        // name, and can straddle a reserved LDT range the same way the bump
        // cursor can (HLD D2).
        if !self.selector_run_admitted(base_selector, new_count) {
            return false;
        }
        for idx in old_count..new_count {
            let selector = base_selector.wrapping_add(idx * 8);
            if !self.selector_in_ldt_range(cpu, selector, 1) {
                return false;
            }
            if self.selectors.iter().any(|s| s.selector == selector) {
                return false;
            }
        }
        true
    }

    pub(super) fn adjust_dos_memory_selectors(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        base_selector: u16,
        old_count: u16,
        new_count: u16,
    ) {
        if new_count > old_count {
            for idx in old_count..new_count {
                let selector = base_selector.wrapping_add(idx * 8);
                if let Some(pos) = self.free_selectors.iter().position(|&s| s == selector) {
                    self.free_selectors.swap_remove(pos);
                }
                self.selectors.push(Selector {
                    selector,
                    base: 0,
                    limit: 0,
                    access: 0xF2,
                    flags: 0,
                    lock_count: 0,
                });
                if selector >= self.next_selector {
                    if let Some(next) = selector.checked_add(8) {
                        self.next_selector = next;
                    }
                }
            }
        } else if new_count < old_count {
            for idx in new_count..old_count {
                let selector = base_selector.wrapping_add(idx * 8);
                self.write_selector_descriptor(cpu, memory, selector, 0, 0, 0, 0);
                self.selectors.retain(|s| s.selector != selector);
                if !self.free_selectors.contains(&selector) {
                    self.free_selectors.push(selector);
                }
                cpu.invalidate_segment_cache(memory, selector);
            }
        }
    }

    pub fn allocate_dos_memory_block(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        dos_mem: &mut DosMem,
        owner_psp: u16,
        paragraphs: u16,
    ) -> std::result::Result<(u16, u16), DosMemoryBlockError> {
        if paragraphs == 0 {
            return Err(DosMemoryBlockError::InsufficientMemory {
                largest_available: 0,
            });
        }

        // Refuse before mutating DOS memory/descriptors when the fixed-capacity
        // tracking table is full, so a leaking or hostile client cannot panic
        // FixedVec::push after state has already changed. MDD-BUG-FLUXHOMEARPA-00016.
        if self.dos_memory_blocks.is_full() {
            return Err(DosMemoryBlockError::InsufficientMemory {
                largest_available: 0,
            });
        }

        let selector_count = self.dos_memory_selector_count(paragraphs);
        let segment = match dos_mem.alloc(memory, owner_psp, paragraphs) {
            Ok(segment) => segment,
            Err(DosMemError::InsufficientMemory { largest_available }) => {
                return Err(DosMemoryBlockError::InsufficientMemory { largest_available });
            }
            Err(DosMemError::InvalidBlock) => {
                return Err(DosMemoryBlockError::InvalidBlock);
            }
        };

        let selector = self.allocate_descriptors(cpu, memory, selector_count);
        if selector == 0 {
            let _ = dos_mem.free(memory, segment);
            return Err(DosMemoryBlockError::DescriptorUnavailable);
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
        Ok((segment, selector))
    }

    pub fn free_dos_memory_block(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        dos_mem: &mut DosMem,
        selector: u16,
    ) -> std::result::Result<(), DosMemoryBlockError> {
        let Some(selector) = self.canonical_ldt_selector(selector) else {
            return Err(DosMemoryBlockError::InvalidBlock);
        };
        if !self.selector_in_ldt_range(cpu, selector, 1) {
            return Err(DosMemoryBlockError::InvalidBlock);
        }
        let Some(pos) = self.dos_memory_block_index_for_selector(selector) else {
            return Err(DosMemoryBlockError::InvalidBlock);
        };
        let (segment, base_selector, selector_count) = {
            let block = &self.dos_memory_blocks[pos];
            (block.segment, block.selector, block.selector_count)
        };

        dos_mem
            .free(memory, segment)
            .map_err(|_| DosMemoryBlockError::InvalidBlock)?;
        self.dos_memory_blocks.remove(pos);
        self.free_dos_memory_selectors(cpu, memory, base_selector, selector_count);
        Ok(())
    }

    pub fn resize_dos_memory_block(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        dos_mem: &mut DosMem,
        selector: u16,
        new_paragraphs: u16,
    ) -> std::result::Result<(), DosMemoryBlockError> {
        if new_paragraphs == 0 {
            return Err(DosMemoryBlockError::InsufficientMemory {
                largest_available: 0,
            });
        }

        let Some(selector) = self.canonical_ldt_selector(selector) else {
            return Err(DosMemoryBlockError::InvalidBlock);
        };
        if !self.selector_in_ldt_range(cpu, selector, 1) {
            return Err(DosMemoryBlockError::InvalidBlock);
        }
        let Some(pos) = self.dos_memory_block_index_for_selector(selector) else {
            return Err(DosMemoryBlockError::InvalidBlock);
        };

        let (segment, base_selector, old_count) = {
            let block = &self.dos_memory_blocks[pos];
            (block.segment, block.selector, block.selector_count)
        };
        let new_count = self.dos_memory_selector_count(new_paragraphs);
        if new_count > old_count
            && !self.can_extend_dos_memory_selectors(cpu, base_selector, old_count, new_count)
        {
            return Err(DosMemoryBlockError::DescriptorUnavailable);
        }

        match dos_mem.resize(memory, segment, new_paragraphs) {
            Ok(()) => {
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
                Ok(())
            }
            Err(DosMemError::InsufficientMemory { largest_available }) => {
                Err(DosMemoryBlockError::InsufficientMemory { largest_available })
            }
            Err(DosMemError::InvalidBlock) => Err(DosMemoryBlockError::InvalidBlock),
        }
    }

    pub(super) fn dos_memory_services(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        dos_mem: Option<&mut DosMem>,
        owner_psp: u16,
    ) -> Result<()> {
        let subfunc = cpu.regs.al();

        match subfunc {
            // Allocate DOS memory
            0x00 => {
                let paragraphs = cpu.regs.bx();
                // Reject 0-paragraph allocation to prevent limit underflow
                if paragraphs == 0 {
                    cpu.regs.set_ax(DOS_ERR_INSUFFICIENT_MEMORY);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                // Refuse before mutating when the fixed-capacity tracking table is
                // full (covers both the dos_mem-backed and stub branches below), so
                // FixedVec::push cannot panic. MDD-BUG-FLUXHOMEARPA-00016.
                if self.dos_memory_blocks.is_full() {
                    cpu.regs.set_ax(DOS_ERR_INSUFFICIENT_MEMORY);
                    cpu.regs.set_bx(0);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let selector_count = self.dos_memory_selector_count(paragraphs);
                if let Some(dos_mem) = dos_mem {
                    match dos_mem.alloc(memory, owner_psp, paragraphs) {
                        Ok(segment) => {
                            let selector = self.allocate_descriptors(cpu, memory, selector_count);
                            if selector == 0 {
                                let _ = dos_mem.free(memory, segment);
                                cpu.regs.set_ax(DOS_ERR_INSUFFICIENT_MEMORY);
                                cpu.regs.set_bx(0);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                return Ok(());
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
                            cpu.regs.set_ax(segment);
                            cpu.regs.set_dx(selector);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                        Err(DosMemError::InsufficientMemory { largest_available }) => {
                            cpu.regs.set_ax(DOS_ERR_INSUFFICIENT_MEMORY);
                            cpu.regs.set_bx(largest_available);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                        Err(DosMemError::InvalidBlock) => {
                            cpu.regs.set_ax(DOS_ERR_MCB_DESTROYED);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                } else {
                    let segment = 0x2000 + (self.dos_memory_blocks.len() as u16) * 0x100;
                    let selector = self.allocate_descriptors(cpu, memory, selector_count);
                    if selector == 0 {
                        cpu.regs.set_ax(DOS_ERR_INSUFFICIENT_MEMORY);
                        cpu.regs.set_bx(0);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
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
                    cpu.regs.set_ax(segment);
                    cpu.regs.set_dx(selector);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                }
            }

            // Free DOS memory
            0x01 => {
                let Some(selector) = self.canonical_ldt_selector(cpu.regs.dx()) else {
                    cpu.regs.set_ax(DOS_ERR_INVALID_MEMORY_BLOCK);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                };
                if !self.selector_in_ldt_range(cpu, selector, 1) {
                    cpu.regs.set_ax(DOS_ERR_INVALID_MEMORY_BLOCK);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                if let Some(pos) = self.dos_memory_block_index_for_selector(selector) {
                    let (segment, base_selector, selector_count) = {
                        let block = &self.dos_memory_blocks[pos];
                        (block.segment, block.selector, block.selector_count)
                    };
                    if let Some(dos_mem) = dos_mem {
                        match dos_mem.free(memory, segment) {
                            Ok(()) => {
                                self.dos_memory_blocks.remove(pos);
                                self.free_dos_memory_selectors(
                                    cpu,
                                    memory,
                                    base_selector,
                                    selector_count,
                                );
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                            Err(_) => {
                                cpu.regs.set_ax(DOS_ERR_MCB_DESTROYED);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            }
                        }
                    } else {
                        self.dos_memory_blocks.remove(pos);
                        self.free_dos_memory_selectors(cpu, memory, base_selector, selector_count);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                } else {
                    cpu.regs.set_ax(DOS_ERR_INVALID_MEMORY_BLOCK);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Resize DOS memory
            0x02 => {
                let Some(selector) = self.canonical_ldt_selector(cpu.regs.dx()) else {
                    cpu.regs.set_ax(DOS_ERR_INVALID_MEMORY_BLOCK);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                };
                if !self.selector_in_ldt_range(cpu, selector, 1) {
                    cpu.regs.set_ax(DOS_ERR_INVALID_MEMORY_BLOCK);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let new_paragraphs = cpu.regs.bx();
                // Reject 0-paragraph resize to prevent limit underflow
                if new_paragraphs == 0 {
                    cpu.regs.set_ax(DOS_ERR_INSUFFICIENT_MEMORY);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                if let Some(pos) = self.dos_memory_block_index_for_selector(selector) {
                    let (segment, base_selector, old_count) = {
                        let block = &self.dos_memory_blocks[pos];
                        (block.segment, block.selector, block.selector_count)
                    };
                    let new_count = self.dos_memory_selector_count(new_paragraphs);
                    if new_count > old_count
                        && !self.can_extend_dos_memory_selectors(
                            cpu,
                            base_selector,
                            old_count,
                            new_count,
                        )
                    {
                        cpu.regs.set_ax(DOS_ERR_INSUFFICIENT_MEMORY);
                        cpu.regs.set_bx(0);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                    if let Some(dos_mem) = dos_mem {
                        let resize_result = match dos_mem.resize(memory, segment, new_paragraphs) {
                            Err(DosMemError::InvalidBlock) => {
                                dos_mem.resize_umb(memory, segment, new_paragraphs)
                            }
                            other => other,
                        };
                        match resize_result {
                            Ok(()) => {
                                self.adjust_dos_memory_selectors(
                                    cpu,
                                    memory,
                                    base_selector,
                                    old_count,
                                    new_count,
                                );
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
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                            Err(DosMemError::InsufficientMemory { largest_available }) => {
                                cpu.regs.set_ax(DOS_ERR_INSUFFICIENT_MEMORY);
                                cpu.regs.set_bx(largest_available);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            }
                            Err(DosMemError::InvalidBlock) => {
                                cpu.regs.set_ax(DOS_ERR_MCB_DESTROYED);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            }
                        }
                    } else {
                        self.adjust_dos_memory_selectors(
                            cpu,
                            memory,
                            base_selector,
                            old_count,
                            new_count,
                        );
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
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                } else {
                    cpu.regs.set_ax(DOS_ERR_INVALID_MEMORY_BLOCK);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            _ => {
                cpu.regs.set_ax(DPMI_ERR_UNSUPPORTED);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }
        }

        Ok(())
    }

    pub(super) fn align_up(value: u32, align: u32) -> u32 {
        if align == 0 {
            return value;
        }
        // Use checked arithmetic to prevent overflow
        value
            .checked_add(align - 1)
            .map(|v| v & !(align - 1))
            .unwrap_or(!(align - 1))
    }

    pub(super) fn validate_linear_region(
        &self,
        memory: &Memory,
        linear: u32,
        size: u32,
    ) -> std::result::Result<(), u16> {
        let end = linear.checked_add(size).ok_or(DPMI_ERR_INVALID_LINEAR)?;
        if end as usize > memory.ram_size() {
            return Err(DPMI_ERR_INVALID_LINEAR);
        }
        Ok(())
    }

    /// Check if a physical address range overlaps with existing physical mappings (B19)
    /// Drop every physical mapping whose linear base lies in `[base, base + size)`,
    /// from both the DPMI table and `Memory`. A 1.0 block that is freed or
    /// shrunk takes the device windows mapped into it with it; leaving them
    /// stranded the linear range and, 64 at a time, the mapping table
    /// (MDD-BUG-CRU-01402).
    pub(super) fn release_physical_mappings_in(
        &mut self,
        memory: &mut Memory,
        base: u32,
        size: u32,
    ) {
        let end = u64::from(base) + u64::from(size);
        let inside = |m: &PhysicalMapping| {
            u64::from(m.linear) >= u64::from(base) && u64::from(m.linear) < end
        };
        while let Some(pos) = self.physical_mappings.iter().position(inside) {
            let mapping = self.physical_mappings.remove(pos);
            memory.remove_physical_mapping(mapping.linear);
        }
    }

    /// Move every physical mapping inside `[old_base, old_base + size)` by the
    /// same offset as the block that holds it, which is where a host's page
    /// table entries go when `0505h` moves the block (MDD-BUG-CRU-01402).
    pub(super) fn relocate_physical_mappings(
        &mut self,
        memory: &mut Memory,
        old_base: u32,
        size: u32,
        new_base: u32,
    ) {
        let end = u64::from(old_base) + u64::from(size);
        for mapping in self.physical_mappings.iter_mut() {
            if u64::from(mapping.linear) < u64::from(old_base) || u64::from(mapping.linear) >= end {
                continue;
            }
            memory.remove_physical_mapping(mapping.linear);
            mapping.linear = new_base + (mapping.linear - old_base);
            memory.add_physical_mapping(mapping.linear, mapping.physical, mapping.size);
        }
    }

    pub(super) fn physical_mappings_overlap(&self, physical: u32, size: u32) -> bool {
        let end = physical.saturating_add(size);
        self.physical_mappings
            .iter()
            .any(|m| physical < m.physical.saturating_add(m.size) && end > m.physical)
    }

    pub(super) fn collect_allocated_ranges(&self) -> Vec<(u32, u32)> {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for block in &self.memory_blocks {
            // Use capacity (reserved range) rather than size (client-visible)
            // so that find_free_linear_range doesn't place new blocks inside
            // an over-allocated reservation.
            ranges.push((block.address, block.capacity));
        }
        // Include DPMI 1.0 linear memory blocks
        for block in &self.linear_memory_blocks {
            ranges.push((block.base, block.size));
        }
        for mapping in &self.physical_mappings {
            ranges.push((mapping.linear, mapping.size));
        }
        self.push_descriptor_ranges(&mut ranges);
        ranges.sort_by_key(|(start, _)| *start);
        ranges
    }

    /// Like `collect_allocated_ranges`, but uses each `MemoryBlock`'s `size`
    /// (client-visible) rather than `capacity` (reserved range). Used by
    /// `Get Free Memory Information` (INT 31h AX=0500h) to report the
    /// memory the client can actually still allocate.
    ///
    /// The first DPMI 0501 allocation may reserve the entire remaining RAM
    /// as `capacity` (matching real DPMI hosts so 0503 resize succeeds in
    /// place). Reporting `capacity` to clients via 0500 wrongly shows zero
    /// 0500 right after their initial alloc) to abort with "out of memory".
    pub(super) fn collect_client_visible_ranges(&self) -> Vec<(u32, u32)> {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for block in &self.memory_blocks {
            ranges.push((block.address, block.size));
        }
        for block in &self.linear_memory_blocks {
            ranges.push((block.base, block.size));
        }
        for mapping in &self.physical_mappings {
            ranges.push((mapping.linear, mapping.size));
        }
        self.push_descriptor_ranges(&mut ranges);
        ranges.sort_by_key(|(start, _)| *start);
        ranges
    }

    /// Walk client-visible ranges from 1MB to `ram_end` and compute
    /// `(largest_free, free_bytes)` over the gaps between allocations.
    ///
    /// Shared by INT 31h AX=0500h (DPMI 0.9 Get Free Memory Information)
    /// and AX=050Bh (DPMI 1.0 Get Memory Information). Both must report
    /// the memory a client can actually still allocate, NOT the reserved
    /// capacity of over-allocated blocks; `collect_client_visible_ranges`
    /// already filters that distinction. The loop also defends against
    /// nested or out-of-order ranges via `cur.max(...)` so a later range
    /// ending earlier than an earlier range cannot pull `cur` backwards.
    pub(super) fn compute_free_memory_above_1mb(&self, ram_end: u32) -> (u32, u32) {
        let ranges = self.collect_client_visible_ranges();
        let mut largest_free = 0u32;
        let mut free_bytes = 0u32;
        let mut cur = 0x100000u32;
        for (start, span) in ranges {
            if start > cur {
                let gap = start - cur;
                largest_free = largest_free.max(gap);
                free_bytes = free_bytes.saturating_add(gap);
            }
            cur = cur.max(start.saturating_add(span));
        }
        if cur < ram_end {
            let gap = ram_end - cur;
            largest_free = largest_free.max(gap);
            free_bytes = free_bytes.saturating_add(gap);
        }
        (largest_free, free_bytes)
    }

    pub(super) fn find_free_linear_range(&self, size: u32, align: u32) -> Option<u32> {
        let size = Self::align_up(size, align);
        let ranges = self.collect_allocated_ranges();

        let mut cur = Self::align_up(0x100000u32, align);
        for (start, span) in ranges {
            if cur.saturating_add(size) <= start {
                return Some(cur);
            }
            // A range nested inside an earlier one must not pull the cursor
            // back into it (MDD-BUG-CRU-01400).
            cur = cur.max(Self::align_up(start.saturating_add(span), align));
        }

        if cur.saturating_add(size) > DOS_RAM_CEILING as u32 {
            return None;
        }

        Some(cur)
    }

    pub(super) fn handle_in_use(&self, handle: u32) -> bool {
        self.memory_blocks.iter().any(|b| b.handle == handle)
            || self.linear_memory_blocks.iter().any(|b| b.handle == handle)
    }

    pub(super) fn allocate_mem_handle(&mut self) -> Option<u32> {
        let start = if self.next_mem_handle == 0 {
            1
        } else {
            self.next_mem_handle
        };
        let mut handle = start;
        loop {
            if handle != 0 && !self.handle_in_use(handle) {
                let mut next = handle.wrapping_add(1);
                if next == 0 {
                    next = 1;
                }
                self.next_mem_handle = next;
                return Some(handle);
            }
            handle = handle.wrapping_add(1);
            if handle == 0 {
                handle = 1;
            }
            if handle == start {
                return None;
            }
        }
    }

    pub(super) fn alloc_linear_block(
        &mut self,
        memory: &mut Memory,
        size: u32,
    ) -> Result<(u32, u32), u16> {
        let align = 0x1000u32;
        let size = Self::align_up(size, align);
        let base = self
            .find_free_linear_range(size, align)
            .ok_or(DPMI_ERR_LINEAR_UNAVAILABLE)?;

        // Capacity == size: allocate exactly what was requested.
        // The old over-allocation logic reserved ALL remaining linear space
        // caused subsequent 0501 calls to fail with 0x8012 because
        // find_free_linear_range sees block.capacity as occupied space.
        // The AX=0503 resize path handles grow-beyond-capacity correctly.
        let capacity = size;

        // Diagnostic: log all allocations to help debug DPMI memory layout issues
        tracing::debug!(
            "[DPMI] alloc base={:#X} size={:#X} capacity={:#X} end={:#X} ram={:#X} blocks={}",
            base,
            size,
            capacity,
            base.saturating_add(capacity),
            memory.ram_size(),
            self.memory_blocks.len()
        );

        // The memory-block table is fixed-capacity (FixedVec<_, 512>). A guest can
        // allocate 512 small page-sized blocks well below the RAM ceiling; refuse
        // the next allocation with a DPMI error instead of panicking in
        // FixedVec::push. MDD-BUG-CRUCIBLE-00056.
        if self.memory_blocks.is_full() {
            return Err(DPMI_ERR_HANDLE_UNAVAILABLE);
        }
        let handle = self
            .allocate_mem_handle()
            .ok_or(DPMI_ERR_HANDLE_UNAVAILABLE)?;
        self.memory_blocks.push(MemoryBlock {
            handle,
            address: base,
            size,
            capacity,
        });
        memory.ensure_ram_size((base + capacity) as usize);
        Ok((base, handle))
    }

    /// Check if linear range is free (for DPMI 1.0 allocations)
    pub(super) fn is_linear_range_free_for_1_0(&self, base: u32, size: u32) -> bool {
        let end = base.saturating_add(size);

        // Check against existing memory_blocks (DPMI 0.9) — use capacity for reserved range
        let overlaps_09 = self
            .memory_blocks
            .iter()
            .any(|b| base < b.address.saturating_add(b.capacity) && end > b.address);

        // Check against existing linear_memory_blocks (DPMI 1.0)
        let overlaps_10 = self
            .linear_memory_blocks
            .iter()
            .any(|b| base < b.base.saturating_add(b.size) && end > b.base);

        // Check against physical mappings
        let overlaps_phys = self
            .physical_mappings
            .iter()
            .any(|m| base < m.linear.saturating_add(m.size) && end > m.linear);

        let overlaps_tables = self.overlaps_descriptor_tables(base, end);

        !overlaps_09 && !overlaps_10 && !overlaps_phys && !overlaps_tables
    }

    /// Find free linear range (for DPMI 1.0 allocations)
    pub(super) fn find_free_linear_range_for_1_0(&self, size: u32, align: u32) -> Option<u32> {
        let size = Self::align_up(size, align);
        let ranges = self.collect_allocated_ranges();

        let mut cur = Self::align_up(0x100000u32, align);
        for (start, span) in ranges {
            if cur.saturating_add(size) <= start {
                return Some(cur);
            }
            // A range nested inside an earlier one must not pull the cursor
            // back into it (MDD-BUG-CRU-01400).
            cur = cur.max(Self::align_up(start.saturating_add(span), align));
        }

        if cur.saturating_add(size) > DOS_RAM_CEILING as u32 {
            return None;
        }

        Some(cur)
    }

    /// Read handle from ESI (32-bit) or SI:DI (16-bit)
    /// DPMI 0.9 §4.4: 32-bit programs use ESI instead of SI:DI pair
    pub(super) fn read_handle(&self, cpu: &Cpu) -> u32 {
        if self.cpu_mode == 1 {
            cpu.regs.esi
        } else {
            ((cpu.regs.si() as u32) << 16) | (cpu.regs.di() as u32)
        }
    }

    /// Write handle to ESI (32-bit) or SI:DI (16-bit)
    /// DPMI 0.9 §4.4: 32-bit programs use ESI instead of SI:DI pair
    pub(super) fn write_handle(&self, cpu: &mut Cpu, handle: u32) {
        if self.cpu_mode == 1 {
            cpu.regs.esi = handle;
        } else {
            cpu.regs.set_si((handle >> 16) as u16);
            cpu.regs.set_di((handle & 0xFFFF) as u16);
        }
    }

    /// Check if linear range is free, excluding a specific handle (for resize)
    pub(super) fn is_linear_range_free_for_resize(
        &self,
        base: u32,
        size: u32,
        exclude_handle: u32,
    ) -> bool {
        let end = base.saturating_add(size);

        // Check against existing memory_blocks (DPMI 0.9) — use capacity for reserved range
        let overlaps_09 = self.memory_blocks.iter().any(|b| {
            b.handle != exclude_handle
                && base < b.address.saturating_add(b.capacity)
                && end > b.address
        });

        // Check against existing linear_memory_blocks (DPMI 1.0), excluding the one being resized
        let overlaps_10 = self.linear_memory_blocks.iter().any(|b| {
            b.handle != exclude_handle && base < b.base.saturating_add(b.size) && end > b.base
        });

        // Check against physical mappings
        let overlaps_phys = self
            .physical_mappings
            .iter()
            .any(|m| base < m.linear.saturating_add(m.size) && end > m.linear);

        let overlaps_tables = self.overlaps_descriptor_tables(base, end);

        !overlaps_09 && !overlaps_10 && !overlaps_phys && !overlaps_tables
    }

    /// Find free linear range, excluding a specific handle (for resize)
    pub(super) fn find_free_linear_range_for_resize(
        &self,
        size: u32,
        align: u32,
        exclude_handle: u32,
    ) -> Option<u32> {
        let size = Self::align_up(size, align);

        // Collect ranges excluding the block being resized
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for block in &self.memory_blocks {
            ranges.push((block.address, block.size));
        }
        for block in &self.linear_memory_blocks {
            if block.handle != exclude_handle {
                ranges.push((block.base, block.size));
            }
        }
        for mapping in &self.physical_mappings {
            ranges.push((mapping.linear, mapping.size));
        }
        self.push_descriptor_ranges(&mut ranges);
        ranges.sort_by_key(|(start, _)| *start);

        let mut cur = Self::align_up(0x100000u32, align);
        for (start, span) in ranges {
            if cur.saturating_add(size) <= start {
                return Some(cur);
            }
            // A range nested inside an earlier one must not pull the cursor
            // back into it (MDD-BUG-CRU-01400).
            cur = cur.max(Self::align_up(start.saturating_add(span), align));
        }

        if cur.saturating_add(size) > DOS_RAM_CEILING as u32 {
            return None;
        }

        Some(cur)
    }

    pub(super) fn memory_services(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        let subfunc = cpu.regs.al();

        match subfunc {
            // Get free memory information
            0x00 => {
                let Some(buffer) = self.client_pointer_with_access(
                    cpu,
                    memory,
                    cpu.regs.es,
                    self.client_offset(cpu),
                    PageAccess::Write,
                ) else {
                    return Ok(());
                };
                let page_size = 4096u32;
                let ram_end = memory.ram_size() as u32;
                let total_bytes = ram_end.saturating_sub(0x100000);

                // Reports client-visible free memory (NOT over-allocated
                // 0500 right after its 47KB allocation and aborts "out of
                // memory" if we wrongly return zero free. See HLD:
                // wrk_docs/2026.05.06 - HLD - Phar Lap RTM extender support
                let (largest_free, free_bytes) = self.compute_free_memory_above_1mb(ram_end);

                // Fill in the 48-byte DPMI memory info structure
                memory.write_u32(buffer, largest_free); // Offset 00h
                memory.write_u32(buffer + 0x04, largest_free / page_size); // Offset 04h
                memory.write_u32(buffer + 0x08, largest_free / page_size); // Offset 08h
                memory.write_u32(buffer + 0x0C, total_bytes / page_size); // Offset 0Ch
                memory.write_u32(buffer + 0x10, total_bytes / page_size); // Offset 10h
                memory.write_u32(buffer + 0x14, free_bytes / page_size); // Offset 14h
                memory.write_u32(buffer + 0x18, total_bytes / page_size); // Offset 18h
                memory.write_u32(buffer + 0x1C, free_bytes / page_size); // Offset 1Ch
                memory.write_u32(buffer + 0x20, 0); // Offset 20h
                                                    // Offset 24h-2Fh: Reserved (12 bytes)
                for i in 0..12 {
                    memory.write_u8(buffer + 0x24 + i, 0xFF);
                }

                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Allocate memory block
            0x01 => {
                let size = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
                if size == 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let (base, handle) = match self.alloc_linear_block(memory, size) {
                    Ok(v) => v,
                    Err(code) => {
                        cpu.regs.set_ax(code);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };

                let ebx_before = cpu.regs.ebx;
                let ecx_before = cpu.regs.ecx;
                cpu.regs.set_bx((base >> 16) as u16);
                cpu.regs.set_cx(base as u16);
                tracing::debug!(
                    "[DPMI 0501] base={:#X} size={:#X} EBX {:#010X}->{:#010X} ECX {:#010X}->{:#010X} CR0={:#010X}",
                    base,
                    size,
                    ebx_before,
                    cpu.regs.ebx,
                    ecx_before,
                    cpu.regs.ecx,
                    cpu.regs.cr0
                );
                self.write_handle(cpu, handle);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Free memory block
            0x02 => {
                let handle = self.read_handle(cpu);

                // Check DPMI 0.9 blocks first
                if self.memory_blocks.iter().any(|b| b.handle == handle) {
                    self.memory_blocks.retain(|b| b.handle != handle);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // Check DPMI 1.0 linear blocks
                if let Some(block) = self
                    .linear_memory_blocks
                    .iter()
                    .find(|b| b.handle == handle)
                {
                    let (base, size) = (block.base, block.size);
                    self.release_physical_mappings_in(memory, base, size);
                    self.linear_memory_blocks.retain(|b| b.handle != handle);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // Handle not found
                cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }

            // Resize memory block
            0x03 => {
                let handle = self.read_handle(cpu);
                let new_size = ((cpu.regs.bx() as u32) << 16) | (cpu.regs.cx() as u32);
                tracing::debug!(
                    "[DPMI 0503] resize handle={} new_size={:#X}",
                    handle,
                    new_size
                );
                if new_size == 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let new_size = Self::align_up(new_size, 0x1000);

                // First find the block and check if resize is possible
                let block_idx = self.memory_blocks.iter().position(|b| b.handle == handle);
                if let Some(idx) = block_idx {
                    let address = self.memory_blocks[idx].address;
                    let old_size = self.memory_blocks[idx].size;
                    let capacity = self.memory_blocks[idx].capacity;
                    let end = address.saturating_add(new_size);

                    // Fast path: resize fits within pre-reserved capacity
                    if new_size <= capacity {
                        let block = &mut self.memory_blocks[idx];
                        block.size = new_size;
                        cpu.regs.set_bx((block.address >> 16) as u16);
                        cpu.regs.set_cx(block.address as u16);
                        self.write_handle(cpu, handle);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        tracing::debug!(
                            "[DPMI 0503] OK (within capacity): addr={:#X} old_size={:#X} new_size={:#X} capacity={:#X}",
                            address,
                            old_size,
                            new_size,
                            capacity
                        );
                    } else if end > DOS_RAM_CEILING as u32 {
                        tracing::warn!(
                            "[DPMI 0503] FAIL: end={:#X} > DOS_RAM_CEILING={:#X}",
                            end,
                            DOS_RAM_CEILING
                        );
                        cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    } else {
                        let overlaps =
                            !self.is_linear_range_free_for_resize(address, new_size, handle);

                        if overlaps {
                            tracing::warn!(
                                "[DPMI 0503] FAIL: overlap at addr={:#X} end={:#X}",
                                address,
                                end
                            );
                            cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        } else {
                            let block = &mut self.memory_blocks[idx];
                            block.size = new_size;
                            block.capacity = new_size; // Update capacity to match
                            memory.ensure_ram_size(end as usize);
                            cpu.regs.set_bx((block.address >> 16) as u16);
                            cpu.regs.set_cx(block.address as u16);
                            self.write_handle(cpu, handle);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            tracing::debug!(
                                "[DPMI 0503] OK: addr={:#X} old_size={:#X} new_size={:#X} end={:#X}",
                                address,
                                old_size,
                                new_size,
                                end
                            );
                        }
                    }
                } else {
                    tracing::warn!("[DPMI 0503] FAIL: handle {} not found", handle);
                    cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE); // Invalid handle
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Allocate linear memory block (DPMI 1.0)
            // EBX = linear address (0 = any), ECX = size in bytes, EDX = flags
            // Returns: EBX = linear base, ESI = handle
            0x04 => {
                if self.cpu_mode != 1 {
                    tracing::debug!("DPMI 1.0 function 0504h called by 16-bit client");
                }
                let requested_addr = cpu.regs.ebx;
                let size = cpu.regs.ecx;
                let flags = cpu.regs.edx;
                // Bit 0: 0 = uncommitted, 1 = committed (per DPMI spec)
                let commit_pages = (flags & 1) == 1;

                if size == 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // Page-align the size
                let aligned_size = Self::align_up(size, 0x1000);

                // Determine base address
                let base = if requested_addr == 0 {
                    // Find any free range
                    match self.find_free_linear_range_for_1_0(aligned_size, 0x1000) {
                        Some(b) => b,
                        None => {
                            cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                    }
                } else {
                    // Use requested address (must be page-aligned)
                    if (requested_addr & 0xFFF) != 0 {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                    if !self.is_linear_range_free_for_1_0(requested_addr, aligned_size) {
                        cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                    requested_addr
                };

                // A DPMI 1.0 linear block must be backed by real RAM: reject a
                // base + size that overflows or exceeds DOS_RAM_CEILING. The memory
                // layer clamps growth (ensure_ram_size) rather than rejecting, so
                // without this a requested base near/beyond the ceiling would hand
                // back a handle based beyond emulated RAM. MDD-BUG-CRUCIBLE-00055.
                match base.checked_add(aligned_size) {
                    Some(end) if end <= DOS_RAM_CEILING as u32 => {}
                    _ => {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                }

                // Check capacity for linear memory block table
                if self.linear_memory_blocks.is_full() {
                    cpu.regs.set_ax(DPMI_ERR_HANDLE_UNAVAILABLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // Generate handle
                let handle = match self.allocate_mem_handle() {
                    Some(handle) => handle,
                    None => {
                        cpu.regs.set_ax(DPMI_ERR_HANDLE_UNAVAILABLE);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };

                // Ensure memory is available if committing
                if commit_pages {
                    memory.ensure_ram_size((base + aligned_size) as usize);
                }

                // Add to linear memory blocks
                self.linear_memory_blocks.push(LinearMemoryBlock::new(
                    handle,
                    base,
                    aligned_size,
                    commit_pages,
                ));

                // Return results
                cpu.regs.ebx = base;
                // DPMI-131: For 32-bit clients, handle goes in ESI alone (not split)
                cpu.regs.esi = handle;
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get memory block size and base (DPMI 1.0)
            // Handle in ESI (32-bit) or SI:DI (16-bit)
            // Returns: BX:CX = base, SI:DI = size (per DPMI 1.0 spec)
            0x0A => {
                let handle = self.read_handle(cpu);
                let Some(block) = self
                    .linear_memory_blocks
                    .iter()
                    .find(|b| b.handle == handle)
                else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                };

                if self.cpu_mode == 1 {
                    cpu.regs.ebx = block.base;
                    cpu.regs.ecx = block.size;
                } else {
                    cpu.regs.set_bx((block.base >> 16) as u16);
                    cpu.regs.set_cx(block.base as u16);
                    cpu.regs.set_si((block.size >> 16) as u16);
                    cpu.regs.set_di(block.size as u16);
                }
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get memory information (DPMI 1.0)
            // ES:(E)DI = pointer to 128-byte buffer
            0x0B => {
                let Some(buffer) = self.client_pointer_with_access(
                    cpu,
                    memory,
                    cpu.regs.es,
                    self.client_offset(cpu),
                    PageAccess::Write,
                ) else {
                    return Ok(());
                };

                let ram_end = memory.ram_size() as u32;
                let total_bytes = ram_end.saturating_sub(0x100000);
                // Same client-visible computation as AX=0500. See that
                let (largest_free, free_bytes) = self.compute_free_memory_above_1mb(ram_end);

                let allocated_bytes = total_bytes.saturating_sub(free_bytes);
                let highest_linear = ram_end.saturating_sub(1);
                let min_unit = 0x1000u32;
                let align_unit = 0x1000u32;

                memory.write_u32(buffer, ram_end); // Offset 00h
                memory.write_u32(buffer + 0x04, ram_end); // Offset 04h
                memory.write_u32(buffer + 0x08, ram_end); // Offset 08h
                memory.write_u32(buffer + 0x0C, total_bytes); // Offset 0Ch
                memory.write_u32(buffer + 0x10, total_bytes); // Offset 10h
                memory.write_u32(buffer + 0x14, allocated_bytes); // Offset 14h
                memory.write_u32(buffer + 0x18, free_bytes); // Offset 18h
                memory.write_u32(buffer + 0x1C, 0); // Offset 1Ch
                memory.write_u32(buffer + 0x20, total_bytes); // Offset 20h
                memory.write_u32(buffer + 0x24, highest_linear); // Offset 24h
                memory.write_u32(buffer + 0x28, largest_free); // Offset 28h
                memory.write_u32(buffer + 0x2C, min_unit); // Offset 2Ch
                memory.write_u32(buffer + 0x30, align_unit); // Offset 30h
                for i in 0..76 {
                    memory.write_u8(buffer + 0x34 + i, 0);
                }

                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Resize linear memory block (DPMI 1.0)
            // Handle in ESI (32-bit) or SI:DI (16-bit), ECX = new size, EDX = flags
            // Returns: EBX = new base address (may move), handle in ESI/SI:DI
            0x05 => {
                let handle = self.read_handle(cpu);
                let new_size = cpu.regs.ecx;
                let flags = cpu.regs.edx;
                // Bit 0: 0 = uncommitted, 1 = committed (per DPMI spec)
                let commit_new = (flags & 1) == 1;

                if new_size == 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                let aligned_new_size = Self::align_up(new_size, 0x1000);
                let new_num_pages = (aligned_new_size / 0x1000) as usize;

                // Find the block
                let block_idx = self
                    .linear_memory_blocks
                    .iter()
                    .position(|b| b.handle == handle);

                match block_idx {
                    Some(idx) => {
                        let old_base = self.linear_memory_blocks[idx].base;
                        let old_size = self.linear_memory_blocks[idx].size;
                        let old_num_pages = (old_size as usize).div_ceil(0x1000);

                        // Check if we can resize in place. Range-overlap checks
                        // alone do not enforce the RAM ceiling.
                        let fits_ram = old_base
                            .checked_add(aligned_new_size)
                            .is_some_and(|end| end <= DOS_RAM_CEILING as u32);
                        let can_resize_in_place = fits_ram
                            && if aligned_new_size <= old_size {
                                true // Shrinking always works
                            } else {
                                // Check if expanded range is free
                                let extra_start = old_base + old_size;
                                let extra_size = aligned_new_size - old_size;
                                self.is_linear_range_free_for_resize(
                                    extra_start,
                                    extra_size,
                                    handle,
                                )
                            };

                        if can_resize_in_place {
                            // Resize in place. A shrink drops the pages past
                            // the new end, and the mappings in them with it.
                            if aligned_new_size < old_size {
                                self.release_physical_mappings_in(
                                    memory,
                                    old_base + aligned_new_size,
                                    old_size - aligned_new_size,
                                );
                            }
                            let block = &mut self.linear_memory_blocks[idx];
                            block.size = aligned_new_size;
                            block.resize_pages(new_num_pages, commit_new);

                            if commit_new && aligned_new_size > old_size {
                                memory.ensure_ram_size((old_base + aligned_new_size) as usize);
                            }

                            cpu.regs.ebx = old_base;
                            self.write_handle(cpu, handle);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            // Need to move block - find new location
                            match self.find_free_linear_range_for_resize(
                                aligned_new_size,
                                0x1000,
                                handle,
                            ) {
                                Some(new_base) => {
                                    // Copy committed pages to the new location.
                                    //
                                    // Consecutive committed pages are coalesced
                                    // into one span and moved with
                                    // `bulk_copy_ram`, which is the same
                                    // checked in-RAM copy the rest of the
                                    // emulator uses (it keeps the instruction
                                    // cache and SMC bookkeeping honest). Page
                                    // at a time with `read_u8`/`write_u8` cost
                                    // 4,096 scalar emulator-memory calls per
                                    // page — 67 million for a 32 MiB block
                                    // (MDD-BUG-CRUCIBLE-00649).
                                    //
                                    // `bulk_copy_ram` returns false when a span
                                    // touches something it must not memcpy
                                    // (VGA aperture, EMS frame, LFB, option
                                    // ROM); that span falls back to the scalar
                                    // path. Uncommitted pages stay holes: they
                                    // end a span and are never copied.
                                    let block = &self.linear_memory_blocks[idx];
                                    let pages_to_copy = old_num_pages.min(new_num_pages);
                                    // The destination must exist before anything
                                    // is copied into it. `bulk_copy_ram` clamps
                                    // to the current RAM length and reports
                                    // success for a span entirely past the end,
                                    // so without this the move would silently
                                    // copy nothing. (The old byte loop grew RAM
                                    // implicitly through `write_u8`; the
                                    // `ensure_ram_size` further down runs only
                                    // after the copy, and only when committing.)
                                    memory.ensure_ram_size(
                                        new_base as usize + pages_to_copy * 0x1000,
                                    );
                                    let mut run_start: Option<usize> = None;
                                    for i in 0..=pages_to_copy {
                                        let committed =
                                            i < pages_to_copy && block.is_page_committed(i);
                                        match (committed, run_start) {
                                            (true, None) => run_start = Some(i),
                                            (false, Some(first)) => {
                                                copy_committed_span(
                                                    memory, old_base, new_base, first, i,
                                                );
                                                run_start = None;
                                            }
                                            _ => {}
                                        }
                                    }

                                    // The mapped pages move with the block.
                                    self.relocate_physical_mappings(
                                        memory, old_base, old_size, new_base,
                                    );

                                    // Update block
                                    let block = &mut self.linear_memory_blocks[idx];
                                    block.base = new_base;
                                    block.size = aligned_new_size;
                                    block.resize_pages(new_num_pages, commit_new);

                                    if commit_new {
                                        memory.ensure_ram_size(
                                            (new_base + aligned_new_size) as usize,
                                        );
                                    }

                                    cpu.regs.ebx = new_base;
                                    self.write_handle(cpu, handle);
                                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                }
                                None => {
                                    cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                }
                            }
                        }
                    }
                    None => {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Get page attributes (DPMI 1.0)
            // ESI = handle, EBX = offset in block, ECX = page count, ES:EDX = buffer for attributes
            0x06 => {
                let handle = self.read_handle(cpu);
                let offset = cpu.regs.ebx;
                let page_count = cpu.regs.ecx as usize;
                if page_count == 0 {
                    cpu.regs.ecx = 0;
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // Validate offset is page-aligned
                if (offset & 0xFFF) != 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                let Some(buffer) = self.client_pointer_with_access(
                    cpu,
                    memory,
                    cpu.regs.es,
                    self.client_offset_reg(cpu),
                    PageAccess::Write,
                ) else {
                    return Ok(());
                };
                let total_bytes = match page_count.checked_mul(2) {
                    Some(total) => total,
                    None => {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };
                let end = match buffer.checked_add(total_bytes) {
                    Some(end) => end,
                    None => {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };
                if end > memory.ram_size() {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                let Some(block) = self
                    .linear_memory_blocks
                    .iter()
                    .find(|b| b.handle == handle)
                else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                };

                let rel_page = (offset / 0x1000) as usize;
                if rel_page.saturating_add(page_count) > block.page_count {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                for i in 0..page_count {
                    let page_idx = rel_page + i;
                    let attr_addr = match i
                        .checked_mul(2)
                        .and_then(|offset| buffer.checked_add(offset))
                    {
                        Some(addr) => addr,
                        None => {
                            cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                    };
                    let attr = if block.is_page_committed(page_idx) {
                        0x0001 // Type=1 (committed)
                    } else {
                        0x0000 // Type=0 (uncommitted)
                    };
                    memory.write_u16(attr_addr, attr);
                }
                cpu.regs.ecx = page_count as u32;
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Set page attributes (DPMI 1.0)
            // ESI = handle, EBX = offset in block, ECX = page count, ES:EDX = buffer with attributes
            0x07 => {
                let handle = self.read_handle(cpu);
                let offset = cpu.regs.ebx;
                let page_count = cpu.regs.ecx as usize;
                if page_count == 0 {
                    cpu.regs.ecx = 0;
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // Validate offset is page-aligned
                if (offset & 0xFFF) != 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                let Some(buffer) =
                    self.client_pointer(cpu, memory, cpu.regs.es, self.client_offset_reg(cpu))
                else {
                    return Ok(());
                };
                let total_bytes = match page_count.checked_mul(2) {
                    Some(total) => total,
                    None => {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };
                let end = match buffer.checked_add(total_bytes) {
                    Some(end) => end,
                    None => {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };
                if end > memory.ram_size() {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                let Some(block_idx) = self
                    .linear_memory_blocks
                    .iter()
                    .position(|b| b.handle == handle)
                else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                };

                let rel_page = (offset / 0x1000) as usize;
                if rel_page.saturating_add(page_count)
                    > self.linear_memory_blocks[block_idx].page_count
                {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                let block_base = self.linear_memory_blocks[block_idx].base;
                for i in 0..page_count {
                    let attr_addr = match i
                        .checked_mul(2)
                        .and_then(|offset| buffer.checked_add(offset))
                    {
                        Some(addr) => addr,
                        None => {
                            cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                    };
                    let attr = memory.read_u16(attr_addr);
                    let attr_type = attr & 0x0007;
                    if attr_type > 1 {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                }

                let block = &mut self.linear_memory_blocks[block_idx];
                for i in 0..page_count {
                    let page_idx = rel_page + i;
                    let attr_addr = match i
                        .checked_mul(2)
                        .and_then(|offset| buffer.checked_add(offset))
                    {
                        Some(addr) => addr,
                        None => {
                            cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(());
                        }
                    };
                    let attr = memory.read_u16(attr_addr);
                    let attr_type = attr & 0x0007;
                    let is_committed = attr_type == 1;
                    block.set_page_committed(page_idx, is_committed);

                    // If committing, ensure memory exists
                    if is_committed {
                        let page_addr = block_base + (page_idx as u32 * 0x1000);
                        memory.ensure_ram_size((page_addr + 0x1000) as usize);
                    }
                }

                cpu.regs.ecx = page_count as u32;
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Map device in memory block (DPMI 1.0)
            // ESI = handle, EBX = offset in block, ECX = page count, EDX = physical address
            0x08 => {
                let handle = self.read_handle(cpu);
                let offset = cpu.regs.ebx;
                let page_count = cpu.regs.ecx;
                let physical = cpu.regs.edx;

                if page_count == 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                if (offset & 0xFFF) != 0 || (physical & 0xFFF) != 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let size = match page_count.checked_mul(0x1000) {
                    Some(sz) => sz,
                    None => {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };

                let Some(block_idx) = self
                    .linear_memory_blocks
                    .iter()
                    .position(|b| b.handle == handle)
                else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                };

                let rel_page = offset / 0x1000;
                let block_pages = self.linear_memory_blocks[block_idx].page_count as u32;
                if rel_page.saturating_add(page_count) > block_pages {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                let Some(linear) = self.linear_memory_blocks[block_idx]
                    .base
                    .checked_add(offset)
                else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                };
                let phys_end = physical.saturating_add(size);
                // A device above the DOS RAM ceiling is admitted on the same
                // terms as 0800h: registered MMIO, or the LFB aperture
                // (MDD-BUG-CRU-01488).
                let is_device = memory.is_mmio_address(physical as usize)
                    || memory.is_lfb_aperture_address(physical as usize);
                if phys_end > DOS_RAM_CEILING as u32 && !is_device {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                if self.physical_mappings_overlap(physical, size) {
                    cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // The physical-mapping table is fixed-capacity (FixedVec<_, 64>);
                // refuse a new mapping when full instead of panicking in
                // FixedVec::push. MDD-BUG-CRUCIBLE-00057.
                if self.physical_mappings.is_full() {
                    cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // Memory also contains system/LFB mappings. Secure its shared
                // slot before committing the DPMI-private record or page state.
                if !memory.add_physical_mapping(linear, physical, size) {
                    cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                self.physical_mappings.push(PhysicalMapping {
                    physical,
                    linear,
                    size,
                });

                let block = &mut self.linear_memory_blocks[block_idx];
                for i in 0..page_count {
                    block.set_page_committed((rel_page + i) as usize, true);
                }

                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Map conventional memory in memory block (DPMI 1.0)
            // ESI = handle, EBX = offset in block, ECX = page count, EDX = conventional linear
            0x09 => {
                let handle = self.read_handle(cpu);
                let offset = cpu.regs.ebx;
                let page_count = cpu.regs.ecx;
                let physical = cpu.regs.edx;

                if page_count == 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                if (offset & 0xFFF) != 0 || (physical & 0xFFF) != 0 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                let size = match page_count.checked_mul(0x1000) {
                    Some(sz) => sz,
                    None => {
                        cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(());
                    }
                };

                let Some(block_idx) = self
                    .linear_memory_blocks
                    .iter()
                    .position(|b| b.handle == handle)
                else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_HANDLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                };

                let rel_page = offset / 0x1000;
                let block_pages = self.linear_memory_blocks[block_idx].page_count as u32;
                if rel_page.saturating_add(page_count) > block_pages {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                let Some(linear) = self.linear_memory_blocks[block_idx]
                    .base
                    .checked_add(offset)
                else {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_VALUE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                };
                let phys_end = physical.saturating_add(size);
                if phys_end > DOS_RAM_CEILING as u32 {
                    cpu.regs.set_ax(DPMI_ERR_INVALID_LINEAR);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                if self.physical_mappings_overlap(physical, size) {
                    cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // The physical-mapping table is fixed-capacity (FixedVec<_, 64>);
                // refuse a new mapping when full instead of panicking in
                // FixedVec::push. MDD-BUG-CRUCIBLE-00057.
                if self.physical_mappings.is_full() {
                    cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }

                // Memory also contains system/LFB mappings. Secure its shared
                // slot before committing the DPMI-private record or page state.
                if !memory.add_physical_mapping(linear, physical, size) {
                    cpu.regs.set_ax(DPMI_ERR_LINEAR_UNAVAILABLE);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
                self.physical_mappings.push(PhysicalMapping {
                    physical,
                    linear,
                    size,
                });

                let block = &mut self.linear_memory_blocks[block_idx];
                for i in 0..page_count {
                    block.set_page_committed((rel_page + i) as usize, true);
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
}

/// Move the committed page span `[first_page, end_page)` of a DPMI linear block
/// from `old_base` to `new_base`.
///
/// Prefers `Memory::bulk_copy_ram`, the validated in-RAM copy that keeps
/// instruction-cache and self-modifying-code bookkeeping correct. It refuses
/// spans overlapping the VGA aperture, EMS frame, LFB or option ROM — those are
/// device windows, not plain RAM — and returns false, which drops this span to
/// the original byte-at-a-time path (MDD-BUG-CRUCIBLE-00649).
fn copy_committed_span(
    memory: &mut Memory,
    old_base: u32,
    new_base: u32,
    first_page: usize,
    end_page: usize,
) {
    debug_assert!(end_page > first_page);
    let offset = first_page * 0x1000;
    let len = (end_page - first_page) * 0x1000;
    let src = old_base as usize + offset;
    let dst = new_base as usize + offset;
    if memory.bulk_copy_ram(src, dst, len) {
        return;
    }
    for i in 0..len {
        let b = memory.read_u8(src + i);
        memory.write_u8(dst + i, b);
    }
}
