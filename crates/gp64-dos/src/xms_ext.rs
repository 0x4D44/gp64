//! XMS dispatch trait extension
//!
//! Provides dispatch methods for gp64_memory::xms::Xms that integrate
//! with CPU registers and DOS memory management.

use crate::memory::DosMem;
use gp64_cpu::Cpu;
use gp64_memory::xms::{Xms, XmsError};
use gp64_memory::Memory;

// XMS AH=10h marks its UMBs as DOS system-owned to distinguish them from
// allocations made through DOS's own UMB services.
const XMS_UMB_OWNER: u16 = 0x0008;

const XMS_MOVE_CHUNK_SIZE: usize = 64 * 1024;

fn set_xms_failure(cpu: &mut Cpu, error: XmsError) {
    cpu.regs.set_ax(0);
    cpu.regs.set_bl(error.code());
}

fn xms_move_address(
    xms: &Xms,
    memory: &Memory,
    handle: u16,
    offset: u32,
    length: usize,
    invalid_handle: u8,
    invalid_offset: u8,
) -> Result<usize, u8> {
    if handle == 0 {
        let segment = (offset >> 16) as u16;
        let offset = (offset & 0xFFFF) as u16;
        let address = Memory::seg_offset_to_linear(segment, offset);
        return match address.checked_add(length) {
            Some(end) if end <= memory.usable_len() => Ok(address),
            _ => Err(invalid_offset),
        };
    }

    let Some((block_address, size_kb, _)) = xms.find_block(handle) else {
        return Err(invalid_handle);
    };
    let block_size = size_kb as usize * 1024;
    let offset = offset as usize;
    if offset > block_size {
        return Err(invalid_offset);
    }
    match offset.checked_add(length) {
        Some(end) if end <= block_size => (block_address as usize)
            .checked_add(offset)
            .ok_or(invalid_offset),
        _ => Err(0xA7),
    }
}

fn copy_xms_move(memory: &mut Memory, src: usize, dst: usize, length: usize) {
    let copy_chunk = |memory: &mut Memory, src: usize, dst: usize, length: usize, backwards| {
        let direct_ram = memory.direct_host_ram_range(src, length).is_some()
            && memory.direct_host_ram_range(dst, length).is_some();
        if direct_ram && memory.bulk_copy_ram(src, dst, length) {
            return;
        }
        if backwards {
            for index in (0..length).rev() {
                let byte = memory.read_u8(src + index);
                memory.write_u8(dst + index, byte);
            }
        } else {
            for index in 0..length {
                let byte = memory.read_u8(src + index);
                memory.write_u8(dst + index, byte);
            }
        }
    };

    let backwards = src < dst && src + length > dst;
    if backwards {
        let mut remaining = length;
        while remaining > 0 {
            let chunk_len = remaining.min(XMS_MOVE_CHUNK_SIZE);
            let chunk_offset = remaining - chunk_len;
            copy_chunk(
                memory,
                src + chunk_offset,
                dst + chunk_offset,
                chunk_len,
                true,
            );
            remaining = chunk_offset;
        }
    } else {
        let mut offset = 0;
        while offset < length {
            let chunk_len = (length - offset).min(XMS_MOVE_CHUNK_SIZE);
            copy_chunk(memory, src + offset, dst + offset, chunk_len, false);
            offset += chunk_len;
        }
    }
}

/// XMS dispatch trait for handling XMS function calls
pub trait XmsDispatch {
    /// Handle INT 2Fh, AX=4300h (XMS installation check) or AX=4310h (get entry point)
    fn int2f_check(&self, cpu: &mut Cpu) -> bool;

    /// Handle XMS function call (basic dispatch without UMB support)
    fn dispatch(&mut self, cpu: &mut Cpu, memory: &mut Memory);

    /// Handle XMS function call with UMB support
    fn dispatch_with_umb(&mut self, cpu: &mut Cpu, memory: &mut Memory, dos_mem: &mut DosMem);
}

impl XmsDispatch for Xms {
    fn int2f_check(&self, cpu: &mut Cpu) -> bool {
        if cpu.regs.ax() == 0x4300 {
            cpu.regs.set_al(0x80); // XMS driver installed
            return true;
        }
        if cpu.regs.ax() == 0x4310 {
            // Return XMS driver entry point
            // Point to our XMS handler stub at F000:FF30
            cpu.return_data_seg_for_hle(None::<&Memory>, gp64_cpu::DataSegReg::ES, 0xF000, false);
            cpu.regs.set_bx(0xFF30);
            return true;
        }
        false
    }

    fn dispatch(&mut self, cpu: &mut Cpu, memory: &mut Memory) {
        let func = cpu.regs.ah();

        match func {
            // Get XMS version
            0x00 => {
                cpu.regs.set_ax(0x0300); // XMS 3.0
                cpu.regs.set_bx(0x0100); // Driver version 1.0
                cpu.regs.set_dx(0x0001); // HMA exists
            }

            // Request HMA
            0x01 => match self.request_hma() {
                Ok(()) => {
                    cpu.regs.set_ax(0x0001);
                    cpu.regs.set_bl(0);
                }
                Err(code) => {
                    cpu.regs.set_ax(0x0000);
                    cpu.regs.set_bl(code);
                }
            },

            // Release HMA
            0x02 => match self.release_hma() {
                Ok(()) => cpu.regs.set_ax(0x0001),
                Err(code) => {
                    cpu.regs.set_ax(0x0000);
                    cpu.regs.set_bl(code);
                }
            },

            // Global enable A20 (XMS 3.0 p7).
            //
            // Global ownership is a FLAG, not a rung on the local nesting
            // ladder. Driving the shared count here made repeated global
            // enables behave like nesting, and let a single global disable
            // consume a LOCAL owner's count and drop the gate underneath it
            // (MDD-BUG-CRU-01020).
            0x03 => {
                self.set_a20_global_enabled(true);
                memory.set_a20(true);
                tracing::debug!(
                    "XMS A20 global enable, global={} local_count={}",
                    self.a20_global_enabled(),
                    self.a20_local_count()
                );
                cpu.regs.set_ax(0x0001);
            }

            // Global disable A20: releases global ownership only. A local
            // owner still holds the gate up, so the two cannot rob each other.
            0x04 => {
                self.set_a20_global_enabled(false);
                memory.set_a20(self.xms_holds_a20());
                tracing::debug!(
                    "XMS A20 global disable, global={} local_count={}",
                    self.a20_global_enabled(),
                    self.a20_local_count()
                );
                cpu.regs.set_ax(0x0001);
            }

            // Local enable A20: nested ownership, matched by 06h.
            0x05 => {
                self.increment_a20_local_count();
                memory.set_a20(true);
                tracing::debug!("XMS A20 local enable, count={}", self.a20_local_count());
                cpu.regs.set_ax(0x0001);
            }

            // Local disable A20: unwinds one nesting level. The gate drops
            // only once NEITHER kind of ownership remains.
            0x06 => {
                self.decrement_a20_local_count();
                memory.set_a20(self.xms_holds_a20());
                tracing::debug!("XMS A20 local disable, count={}", self.a20_local_count());
                cpu.regs.set_ax(0x0001);
            }

            // Query A20 state (XMS 3.0 p16): the PHYSICAL gate, not XMS's own
            // bookkeeping. The two genuinely differ — memory boots with A20
            // enabled while XMS has taken no ownership, so reporting the count
            // answered "disabled" for a gate that was open, and it would also
            // miss any change made through the keyboard controller or the
            // fast-A20 port.
            0x07 => {
                cpu.regs.set_ax(u16::from(memory.a20_enabled()));
                cpu.regs.set_bl(0);
            }

            // Query free extended memory
            0x08 => {
                let largest_kb = self.largest_free_kb();
                cpu.regs.set_ax(largest_kb.min(0xFFFF) as u16);
                cpu.regs.set_dx(self.free_kb().min(0xFFFF) as u16);
                cpu.regs.set_bl(0);
            }

            // Allocate extended memory block
            0x09 => {
                let size_kb = cpu.regs.dx() as u32;
                match self.allocate_block(size_kb) {
                    Ok(handle) => {
                        // The block's physical address matters as much as its
                        // size: a DOS extender whose heap does not start at
                        // 1 MiB can behave quite differently (see
                        // MDD-BUG-FLU-00624), and the address is otherwise
                        // invisible in a run log.
                        tracing::debug!(
                            "XMS alloc: handle={} size={}KB at={:#010X} free={}KB",
                            handle,
                            size_kb,
                            self.find_block(handle).map_or(0, |(address, _, _)| address),
                            self.free_kb()
                        );
                        cpu.regs.set_ax(0x0001);
                        cpu.regs.set_dx(handle);
                        cpu.regs.set_bl(0);
                    }
                    Err(error) => {
                        tracing::debug!(
                            "XMS alloc failed: size={}KB free={}KB error={error:?}",
                            size_kb,
                            self.free_kb()
                        );
                        set_xms_failure(cpu, error);
                    }
                }
            }

            // Free extended memory block
            0x0A => {
                let handle = cpu.regs.dx();
                match self.free_block(handle) {
                    Ok(()) => {
                        tracing::debug!("XMS free: handle={} free={}KB", handle, self.free_kb());
                        cpu.regs.set_ax(0x0001);
                        cpu.regs.set_bl(0);
                    }
                    Err(error) => {
                        tracing::debug!("XMS free failed: handle={handle} error={error:?}");
                        set_xms_failure(cpu, error);
                    }
                }
            }

            // Move extended memory block
            0x0B => {
                // Structure at DS:SI contains move parameters
                let struct_addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());
                let length = memory.read_u32(struct_addr) as usize;
                let src_handle = memory.read_u16(struct_addr + 4);
                let src_offset = memory.read_u32(struct_addr + 6);
                let dst_handle = memory.read_u16(struct_addr + 10);
                let dst_offset = memory.read_u32(struct_addr + 12);

                // XMS 3.0 requires an even length. Reject it before resolving
                // either endpoint or changing A20 state.
                if length & 1 != 0 {
                    cpu.regs.set_ax(0);
                    cpu.regs.set_bl(0xA7); // Invalid length
                    return;
                }

                let src_addr = match xms_move_address(
                    self, memory, src_handle, src_offset, length, 0xA3, 0xA4,
                ) {
                    Ok(address) => address,
                    Err(error) => {
                        cpu.regs.set_ax(0);
                        cpu.regs.set_bl(error);
                        return;
                    }
                };
                let dst_addr = match xms_move_address(
                    self, memory, dst_handle, dst_offset, length, 0xA5, 0xA6,
                ) {
                    Ok(address) => address,
                    Err(error) => {
                        cpu.regs.set_ax(0);
                        cpu.regs.set_bl(error);
                        return;
                    }
                };

                // Perform memory copy only after the complete request is valid.
                let prev_a20 = memory.a20_enabled();
                memory.set_a20(true);
                copy_xms_move(memory, src_addr, dst_addr, length);
                memory.set_a20(prev_a20);

                tracing::debug!(
                    "XMS move: {} bytes src_h={} -> dst_h={}",
                    length,
                    src_handle,
                    dst_handle
                );
                cpu.regs.set_ax(0x0001);
                cpu.regs.set_bl(0);
            }

            // Lock extended memory block
            0x0C => {
                let handle = cpu.regs.dx();
                match self.lock_block(handle) {
                    Ok(address) => {
                        cpu.regs.set_ax(0x0001);
                        cpu.regs.set_dx((address >> 16) as u16);
                        cpu.regs.set_bx(address as u16);
                    }
                    Err(error) => set_xms_failure(cpu, error),
                }
            }

            // Unlock extended memory block
            0x0D => {
                let handle = cpu.regs.dx();
                match self.unlock_block(handle) {
                    Ok(()) => {
                        cpu.regs.set_ax(0x0001);
                        cpu.regs.set_bl(0);
                    }
                    Err(error) => set_xms_failure(cpu, error),
                }
            }

            // Get EMB handle information
            0x0E => {
                let handle = cpu.regs.dx();
                if let Some((size_kb, lock_count)) = self.get_block_info(handle) {
                    cpu.regs.set_ax(0x0001);
                    cpu.regs.set_bh(lock_count);
                    cpu.regs
                        .set_bl(128u8.saturating_sub(self.block_count() as u8));
                    cpu.regs.set_dx(size_kb as u16);
                } else {
                    cpu.regs.set_ax(0);
                    cpu.regs.set_bl(0xA2);
                }
            }

            // Reallocate extended memory block
            0x0F => {
                let handle = cpu.regs.dx();
                let new_size = cpu.regs.bx() as u32;
                match self.resize_block(handle, new_size, memory) {
                    Ok(()) => {
                        cpu.regs.set_ax(0x0001);
                        cpu.regs.set_bl(0);
                    }
                    Err(error) => set_xms_failure(cpu, error),
                }
            }

            // Request upper memory block (no UMB support in basic dispatch)
            0x10 => {
                cpu.regs.set_ax(0);
                cpu.regs.set_bl(0xB1); // No UMB available
            }

            // Release upper memory block
            0x11 => {
                cpu.regs.set_ax(0);
                cpu.regs.set_bl(0xB2);
            }

            // XMS 3.0's 32-bit functions require an 80386 or later CPU.
            0x88 | 0x89 | 0x8E | 0x8F if !cpu.get_cpu_type().supports_386() => {
                cpu.regs.set_ax(0);
                cpu.regs.set_bl(0x80); // Function not implemented
            }

            // Query any free extended memory (XMS 3.0)
            0x88 => {
                let largest_kb = self.largest_free_kb();
                cpu.regs.eax = largest_kb;
                cpu.regs.edx = self.free_kb();
                // ECX describes fixed physical topology, not allocation state.
                cpu.regs.ecx = self
                    .total_kb()
                    .checked_mul(1024)
                    .and_then(|pool_bytes| 0x0010_0000u32.checked_add(pool_bytes))
                    .and_then(|pool_end| pool_end.checked_sub(1))
                    .unwrap_or(u32::MAX);
                cpu.regs.set_bl(0);
            }

            // Allocate any extended memory (XMS 3.0)
            0x89 => {
                let size_kb = cpu.regs.edx;
                match self.allocate_block(size_kb) {
                    Ok(handle) => {
                        tracing::debug!(
                            "XMS 3.0 alloc: handle={} size={}KB free={}KB",
                            handle,
                            size_kb,
                            self.free_kb()
                        );
                        cpu.regs.set_ax(0x0001);
                        cpu.regs.set_dx(handle);
                        cpu.regs.set_bl(0);
                    }
                    Err(error) => set_xms_failure(cpu, error),
                }
            }

            // Get extended EMB handle information (XMS 3.0)
            0x8E => {
                let handle = cpu.regs.dx();
                if let Some((size_kb, lock_count)) = self.get_block_info(handle) {
                    cpu.regs.set_ax(0x0001);
                    cpu.regs.set_bh(lock_count);
                    cpu.regs
                        .set_cx(128u16.saturating_sub(self.block_count() as u16));
                    cpu.regs.edx = size_kb;
                } else {
                    cpu.regs.set_ax(0);
                    cpu.regs.set_bl(0xA2);
                }
            }

            // Reallocate any extended memory block (XMS 3.0)
            0x8F => {
                let handle = cpu.regs.dx();
                let new_size = cpu.regs.ebx;
                match self.resize_block(handle, new_size, memory) {
                    Ok(()) => {
                        cpu.regs.set_ax(0x0001);
                    }
                    Err(error) => set_xms_failure(cpu, error),
                }
            }

            _ => {
                tracing::debug!("XMS unhandled function AH={:02X}", func);
                cpu.regs.set_ax(0);
                cpu.regs.set_bl(0x80); // Function not implemented
            }
        }
    }

    fn dispatch_with_umb(&mut self, cpu: &mut Cpu, memory: &mut Memory, dos_mem: &mut DosMem) {
        let func = cpu.regs.ah();

        match func {
            // Request upper memory block (16-bit)
            0x10 => {
                let paragraphs = cpu.regs.dx();
                request_umb(cpu, dos_mem, memory, paragraphs as u32);
            }

            // Release upper memory block (16-bit)
            0x11 => {
                let segment = cpu.regs.dx();
                release_umb(cpu, dos_mem, memory, segment);
            }

            // Reallocate upper memory block (16-bit)
            0x12 => {
                let segment = cpu.regs.dx();
                let new_size = cpu.regs.bx() as u32;
                reallocate_umb(cpu, dos_mem, memory, segment, new_size);
            }

            // All other functions handled by regular dispatch
            _ => {
                self.dispatch(cpu, memory);
            }
        }
    }
}

/// Request a UMB for XMS function 10h.
fn request_umb(cpu: &mut Cpu, dos_mem: &mut DosMem, memory: &mut Memory, paragraphs: u32) {
    // Check if UMB is initialized
    if !dos_mem.is_umb_initialized() {
        cpu.regs.set_ax(0);
        cpu.regs.set_bl(0xB1); // No UMB available
        cpu.regs.set_dx(0);
        return;
    }

    let largest = dos_mem.largest_free_umb();
    if largest == 0 {
        cpu.regs.set_ax(0);
        cpu.regs.set_bl(0xB1); // No UMB available
        cpu.regs.set_dx(0);
        return;
    }

    // Check if requested size is available
    if paragraphs as u16 > largest {
        cpu.regs.set_ax(0);
        cpu.regs.set_bl(0xB0); // Smaller UMB available
        cpu.regs.set_dx(largest);
        cpu.regs.edx = largest as u32;
        return;
    }

    // Try to allocate using UMB-first strategy (0x80)
    match dos_mem.alloc_with_strategy_and_umb(
        memory,
        XMS_UMB_OWNER,
        paragraphs as u16,
        0x80, // First fit, high memory first
        true, // UMB linked
    ) {
        Ok(segment) => {
            cpu.regs.set_ax(0x0001);
            cpu.regs.set_bx(segment);
            cpu.regs.set_dx(paragraphs as u16);
            cpu.regs.edx = paragraphs;
            tracing::debug!(
                target: "gp64::memory",
                "XMS UMB alloc: seg={:04X} paras={}",
                segment,
                paragraphs
            );
        }
        Err(_) => {
            cpu.regs.set_ax(0);
            cpu.regs.set_bl(0xB0); // Smaller UMB available
            cpu.regs.set_dx(dos_mem.largest_free_umb());
            cpu.regs.edx = dos_mem.largest_free_umb() as u32;
        }
    }
}

fn is_xms_owned_umb(dos_mem: &DosMem, segment: u16) -> bool {
    dos_mem
        .block_info_for_segment(segment)
        .is_some_and(|block| {
            block.is_umb
                && block.usable_seg == segment
                && !block.is_free
                && block.owner_psp == XMS_UMB_OWNER
        })
}

/// Release UMB (shared implementation for 16-bit and 32-bit)
fn release_umb(cpu: &mut Cpu, dos_mem: &mut DosMem, memory: &mut Memory, segment: u16) {
    if !is_xms_owned_umb(dos_mem, segment) {
        cpu.regs.set_ax(0);
        cpu.regs.set_bl(0xB2); // Invalid UMB segment
        return;
    }

    match dos_mem.free(memory, segment) {
        Ok(()) => {
            cpu.regs.set_ax(0x0001);
            tracing::debug!(
                target: "gp64::memory",
                "XMS UMB free: seg={:04X}",
                segment
            );
        }
        Err(_) => {
            cpu.regs.set_ax(0);
            cpu.regs.set_bl(0xB2); // Invalid UMB segment
        }
    }
}

/// Reallocate UMB
fn reallocate_umb(
    cpu: &mut Cpu,
    dos_mem: &mut DosMem,
    memory: &mut Memory,
    segment: u16,
    new_paragraphs: u32,
) {
    if !is_xms_owned_umb(dos_mem, segment) {
        cpu.regs.set_ax(0);
        cpu.regs.set_bl(0xB2); // Invalid UMB segment
        return;
    }

    match dos_mem.resize_umb(memory, segment, new_paragraphs as u16) {
        Ok(()) => {
            cpu.regs.set_ax(0x0001);
            cpu.regs.set_dx(new_paragraphs as u16);
            tracing::debug!(
                target: "gp64::memory",
                "XMS UMB realloc: seg={:04X} new_size={}",
                segment,
                new_paragraphs
            );
        }
        Err(_) => {
            cpu.regs.set_ax(0);
            cpu.regs.set_bl(0xB2); // Invalid UMB segment
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn new_cpu_mem() -> (Cpu, Memory) {
        let mut cpu = Cpu::new();
        cpu.regs.ds = 0;
        cpu.regs.es = 0;
        cpu.regs.ss = 0;
        (cpu, Memory::new())
    }

    fn write_move_descriptor(
        memory: &mut Memory,
        addr: usize,
        length: u32,
        src_handle: u16,
        src_offset: u32,
        dst_handle: u16,
        dst_offset: u32,
    ) {
        memory.write_u32(addr, length);
        memory.write_u16(addr + 4, src_handle);
        memory.write_u32(addr + 6, src_offset);
        memory.write_u16(addr + 10, dst_handle);
        memory.write_u32(addr + 12, dst_offset);
    }

    fn umb_arena_end(dos_mem: &DosMem) -> u16 {
        let last = dos_mem
            .mcb_summary()
            .into_iter()
            .rfind(|entry| entry.is_umb)
            .expect("UMB arena should contain an MCB");
        last.seg.wrapping_add(1).wrapping_add(last.size_paras)
    }

    fn assert_guest_umb_chain_contiguous(memory: &Memory, dos_mem: &DosMem, expected_end: u16) {
        let blocks: Vec<_> = dos_mem
            .mcb_summary()
            .into_iter()
            .filter(|entry| entry.is_umb)
            .collect();
        assert!(!blocks.is_empty());

        let mut next_seg = blocks[0].seg;
        for (index, block) in blocks.iter().enumerate() {
            assert_eq!(block.seg, next_seg, "gap before UMB MCB {index}");
            let address = Memory::seg_offset_to_linear(block.seg, 0);
            let expected_tag = if index + 1 == blocks.len() {
                b'Z'
            } else {
                b'M'
            };
            assert_eq!(memory.read_u8(address), expected_tag);
            assert_eq!(memory.read_u16(address + 1), block.owner_psp);
            assert_eq!(memory.read_u16(address + 3), block.size_paras);
            next_seg = block.seg.wrapping_add(1).wrapping_add(block.size_paras);
        }

        assert_eq!(next_seg, expected_end, "UMB arena extent changed");
    }

    #[test]
    fn test_int2f_check_install_and_entry() {
        let (mut cpu, _mem) = new_cpu_mem();
        let xms = Xms::new();

        cpu.regs.set_ax(0x4300);
        assert!(xms.int2f_check(&mut cpu));
        assert_eq!(cpu.regs.al(), 0x80);

        cpu.regs.set_ax(0x4310);
        assert!(xms.int2f_check(&mut cpu));
        assert_eq!(cpu.regs.es, 0xF000);
        assert_eq!(cpu.regs.bx(), 0xFF30);

        cpu.regs.set_ax(0x0000);
        assert!(!xms.int2f_check(&mut cpu));
    }

    #[test]
    fn test_dispatch_hma_alloc_release() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0x01);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_eq!(cpu.regs.bl(), 0x00);

        cpu.regs.set_ah(0x02);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);

        cpu.regs.set_ah(0x02);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0x93);
    }

    #[test]
    fn test_dispatch_a20_local_toggle() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        memory.set_a20(false);

        cpu.regs.set_ah(0x05); // Local enable
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 1);
        assert!(memory.a20_enabled());
        assert_eq!(xms.a20_local_count(), 1);

        cpu.regs.set_ah(0x06); // Local disable
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 1);
        assert!(!memory.a20_enabled());
        assert_eq!(xms.a20_local_count(), 0);
    }

    /// MDD-BUG-CRU-01020: this test used to assert `a20_local_count() == 2`
    /// after one LOCAL and one GLOBAL enable — it pinned the defect, because
    /// 03h drove the same counter as 05h. XMS 3.0 (p7) gives global ownership
    /// its own flag, so the two are independent and neither can consume the
    /// other's claim.
    #[test]
    fn test_dispatch_a20_nested_count_keeps_gate_enabled() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        memory.set_a20(false);

        cpu.regs.set_ah(0x05); // Local enable
        xms.dispatch(&mut cpu, &mut memory);
        cpu.regs.set_ah(0x03); // Global enable
        xms.dispatch(&mut cpu, &mut memory);
        assert!(memory.a20_enabled());
        assert_eq!(xms.a20_local_count(), 1, "the global enable is not a rung");
        assert!(xms.a20_global_enabled());

        // The global disable releases only global ownership; the local owner
        // still holds the gate up. Under the old shared counter this consumed
        // the local owner's count instead.
        cpu.regs.set_ah(0x04); // Global disable
        xms.dispatch(&mut cpu, &mut memory);
        assert!(memory.a20_enabled(), "a local owner still holds A20");
        assert_eq!(xms.a20_local_count(), 1);
        assert!(!xms.a20_global_enabled());

        cpu.regs.set_ah(0x06); // Local disable
        xms.dispatch(&mut cpu, &mut memory);
        assert!(!memory.a20_enabled());
        assert_eq!(xms.a20_local_count(), 0);
    }

    /// The mirror ordering: releasing LOCAL first must not drop a gate the
    /// global owner still holds. Together with the test above this pins both
    /// release orders, which a single shared counter cannot satisfy.
    #[test]
    fn test_dispatch_a20_global_owner_survives_the_local_release() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        memory.set_a20(false);

        cpu.regs.set_ah(0x03); // Global enable
        xms.dispatch(&mut cpu, &mut memory);
        cpu.regs.set_ah(0x05); // Local enable
        xms.dispatch(&mut cpu, &mut memory);

        cpu.regs.set_ah(0x06); // Local disable
        xms.dispatch(&mut cpu, &mut memory);
        assert!(memory.a20_enabled(), "the global owner still holds A20");
        assert_eq!(xms.a20_local_count(), 0);

        cpu.regs.set_ah(0x04); // Global disable
        xms.dispatch(&mut cpu, &mut memory);
        assert!(!memory.a20_enabled(), "both owners released");
    }

    /// Global calls are IDEMPOTENT (XMS 3.0 p7): three enables are one claim,
    /// released by a single disable. Driving the nesting counter made them
    /// behave like three nested claims, so one disable left the gate up.
    #[test]
    fn test_dispatch_a20_repeated_global_calls_are_idempotent() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        memory.set_a20(false);

        for _ in 0..3 {
            cpu.regs.set_ah(0x03); // Global enable
            xms.dispatch(&mut cpu, &mut memory);
        }
        assert!(memory.a20_enabled());
        assert_eq!(xms.a20_local_count(), 0, "global ownership is not a count");

        cpu.regs.set_ah(0x04); // Global disable
        xms.dispatch(&mut cpu, &mut memory);
        assert!(!memory.a20_enabled(), "one disable releases the one claim");

        // And a repeated disable is equally harmless.
        cpu.regs.set_ah(0x04);
        xms.dispatch(&mut cpu, &mut memory);
        assert!(!memory.a20_enabled());
        assert!(!xms.a20_global_enabled());
    }

    /// An unmatched local disable saturates at zero and must not manufacture
    /// ownership or disturb the global flag.
    #[test]
    fn test_dispatch_a20_unmatched_local_disable_is_harmless() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        memory.set_a20(false);

        cpu.regs.set_ah(0x03); // Global enable
        xms.dispatch(&mut cpu, &mut memory);

        cpu.regs.set_ah(0x06); // Local disable with no matching enable
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 1);
        assert_eq!(xms.a20_local_count(), 0);
        assert!(
            memory.a20_enabled(),
            "an unmatched local disable must not release the GLOBAL owner"
        );
        assert!(xms.a20_global_enabled());
    }

    /// MDD-BUG-CRU-01020: Query A20 (07h) answers the PHYSICAL gate, per XMS
    /// 3.0 p16 — not XMS's own ownership bookkeeping.
    ///
    /// This test used to open by asserting Query returns 0 on a fresh
    /// `Memory`, which pinned the defect from the other side: memory boots
    /// with A20 ENABLED, so the honest answer there is 1. Reporting the count
    /// meant Query disagreed with the gate the guest could actually observe.
    #[test]
    fn test_dispatch_query_a20() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        assert!(
            memory.a20_enabled(),
            "memory boots with A20 enabled — the premise this test turns on"
        );
        cpu.regs.set_ah(0x07);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 1, "the gate is open, whoever opened it");
        assert_eq!(cpu.regs.bl(), 0);

        // Take it down physically, behind XMS's back, and Query follows.
        memory.set_a20(false);
        cpu.regs.set_ah(0x07);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0);

        // Local enable raises the gate; Query still just reads it.
        cpu.regs.set_ah(0x05);
        xms.dispatch(&mut cpu, &mut memory);
        cpu.regs.set_ah(0x07);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 1);

        cpu.regs.set_ah(0x06);
        xms.dispatch(&mut cpu, &mut memory);
        cpu.regs.set_ah(0x07);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0);
    }

    /// The gate can move without XMS: the keyboard controller and the fast
    /// A20 port both reach it. Query must report what the guest would
    /// observe, even when that contradicts XMS's own ownership state.
    #[test]
    fn test_dispatch_query_a20_follows_an_externally_changed_gate() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        memory.set_a20(false);

        cpu.regs.set_ah(0x05); // Local enable: XMS now owns A20
        xms.dispatch(&mut cpu, &mut memory);
        assert!(xms.xms_holds_a20());

        // Something outside XMS drops the gate.
        memory.set_a20(false);
        cpu.regs.set_ah(0x07);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(
            cpu.regs.ax(),
            0,
            "Query reports the gate, not the ownership XMS still believes it holds"
        );
        assert!(xms.xms_holds_a20(), "ownership itself is unchanged");
    }

    #[test]
    fn test_dispatch_alloc_free_block() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0x09);
        cpu.regs.set_dx(1);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        let handle = cpu.regs.dx();
        assert_ne!(handle, 0);

        cpu.regs.set_ah(0x0A);
        cpu.regs.set_dx(handle);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);

        cpu.regs.set_ah(0x0A);
        cpu.regs.set_dx(0xFFFF);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA2);
    }

    #[test]
    fn test_dispatch_reports_specific_standard_xms_failures() {
        let (mut cpu, mut memory) = new_cpu_mem();

        let mut xms = Xms::new();
        cpu.regs.set_ah(0x0D);
        cpu.regs.set_dx(0xFFFF);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA2);

        let mut xms = Xms::new();
        let handle = xms.allocate_block(1).expect("allocation should succeed");
        cpu.regs.set_ah(0x0D);
        cpu.regs.set_dx(handle);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xAA);
        assert_eq!(xms.get_block_info(handle), Some((1, 0)));

        let mut xms = Xms::new();
        let handle = xms.allocate_block(1).expect("allocation should succeed");
        xms.lock_block(handle).expect("lock should succeed");
        let locked_info = xms.get_block_info(handle);
        cpu.regs.set_ah(0x0A);
        cpu.regs.set_dx(handle);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xAB);
        assert_eq!(xms.get_block_info(handle), locked_info);

        cpu.regs.set_ah(0x0F);
        cpu.regs.set_dx(handle);
        cpu.regs.set_bx(2);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xAB);
        assert_eq!(xms.get_block_info(handle), locked_info);

        let mut xms = Xms::new();
        let handle = xms.allocate_block(1).expect("allocation should succeed");
        for _ in 0..u8::MAX {
            xms.lock_block(handle).expect("lock should succeed");
        }
        cpu.regs.set_ah(0x0C);
        cpu.regs.set_dx(handle);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xAC);
        assert_eq!(xms.get_block_info(handle), Some((1, u8::MAX)));

        let mut xms = Xms::new();
        cpu.regs.set_ah(0x0F);
        cpu.regs.set_dx(0xFFFF);
        cpu.regs.set_bx(1);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA2);

        let mut xms = Xms::new();
        let handle = xms.allocate_block(1).expect("allocation should succeed");
        xms.allocate_block(xms.free_kb())
            .expect("remaining memory allocation should succeed");
        let block_before = xms.get_block_info(handle);
        cpu.regs.set_ah(0x0F);
        cpu.regs.set_dx(handle);
        cpu.regs.set_bx(2);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA0);
        assert_eq!(xms.get_block_info(handle), block_before);

        let mut xms = Xms::new();
        for _ in 0..128 {
            xms.allocate_block(0)
                .expect("allocation within the handle limit should succeed");
        }
        cpu.regs.set_ah(0x09);
        cpu.regs.set_dx(0);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA1);
    }

    #[test]
    fn test_dispatch_xms30_allocation_reports_handle_exhaustion() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        for _ in 0..128 {
            xms.allocate_block(0)
                .expect("allocation within the handle limit should succeed");
        }

        cpu.regs.set_ah(0x89);
        cpu.regs.edx = 0;
        xms.dispatch(&mut cpu, &mut memory);

        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA1);
    }

    #[test]
    fn test_dispatch_move_block_conventional() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.ds = 0x0000;
        cpu.regs.set_si(0x0100);
        let struct_addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());

        memory.write_u32(struct_addr, 4);
        memory.write_u16(struct_addr + 4, 0);
        memory.write_u32(struct_addr + 6, 0x0000_0200);
        memory.write_u16(struct_addr + 10, 0);
        memory.write_u32(struct_addr + 12, 0x0000_0300);

        memory.write_u8(0x0200, 0xAA);
        memory.write_u8(0x0201, 0xBB);
        memory.write_u8(0x0202, 0xCC);
        memory.write_u8(0x0203, 0xDD);

        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_eq!(memory.read_u8(0x0300), 0xAA);
        assert_eq!(memory.read_u8(0x0301), 0xBB);
        assert_eq!(memory.read_u8(0x0302), 0xCC);
        assert_eq!(memory.read_u8(0x0303), 0xDD);
    }

    /// MDD-BUG-CRU-01470: an XMS move whose handle-0 destination is the BIOS
    /// F-segment (F000:0000) must not overwrite the HLE stubs there. Ordinary
    /// stores to that segment are dropped, and the bulk path used to write
    /// straight through them; the IRET stub at F000:FF00 must survive.
    #[test]
    fn test_move_block_into_the_bios_segment_leaves_the_hle_stubs() {
        let (mut cpu, mut mem) = new_cpu_mem();
        let mut xms = Xms::new();
        let iret = Memory::seg_offset_to_linear(0xF000, 0xFF00);
        assert_eq!(mem.read_u8(iret), 0xCF, "precondition: the IRET stub");
        for offset in 0..0x1000 {
            mem.write_u8(0x1000 + offset, 0x90);
        }
        let desc_addr = 0x5000usize;
        // Cover F000:F000..F000:FFFF, which includes the stub.
        write_move_descriptor(&mut mem, desc_addr, 0x1000, 0, 0x0000_1000, 0, 0xF000_F000);
        cpu.regs.set_ah(0x0B);
        cpu.regs.set_si(desc_addr as u16);

        xms.dispatch(&mut cpu, &mut mem);
        assert_eq!(cpu.regs.ax(), 0x0001, "the move itself succeeds");

        assert_eq!(
            mem.read_u8(iret),
            0xCF,
            "the IRET stub at F000:FF00 survives"
        );
    }

    #[test]
    fn test_move_block_rejects_odd_length_without_touching_memory_or_a20() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let descriptor = 0x0100;

        write_move_descriptor(&mut memory, descriptor, 1, 0, 0x0200, 0, 0x0300);
        memory.write_u8(0x0200, 0xAA);
        memory.write_u8(0x0300, 0x55);
        memory.set_a20(false);
        assert!(!memory.a20_enabled());

        cpu.regs.set_si(descriptor as u16);
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);

        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA7);
        assert_eq!(memory.read_u8(0x0300), 0x55);
        assert!(!memory.a20_enabled());
    }

    #[test]
    fn test_move_block_rejects_out_of_range_conventional_spans() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let descriptor = 0x0100;
        memory.resize_ram(0x10_0000);

        write_move_descriptor(&mut memory, descriptor, 2, 0, 0xFFFF_FFFF, 0, 0x0200);
        cpu.regs.set_si(descriptor as u16);
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA4);

        write_move_descriptor(&mut memory, descriptor, 2, 0, 0x0200, 0, 0xFFFF_FFFF);
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA6);
    }

    #[test]
    fn test_move_block_rejects_maximum_dword_lengths_promptly() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let descriptor = 0x0100;

        write_move_descriptor(&mut memory, descriptor, u32::MAX, 0, 0x0200, 0, 0x0300);
        cpu.regs.set_si(descriptor as u16);
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA7);

        write_move_descriptor(&mut memory, descriptor, u32::MAX - 1, 0, 0x0200, 0, 0x0300);
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA4);
    }

    #[test]
    fn test_move_block_validates_emb_offset_and_length_boundaries() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let descriptor = 0x0100;

        cpu.regs.set_ah(0x09);
        cpu.regs.set_dx(1);
        xms.dispatch(&mut cpu, &mut memory);
        let handle = cpu.regs.dx();

        write_move_descriptor(&mut memory, descriptor, 0, handle, 1024, 0, 0x0300);
        cpu.regs.set_si(descriptor as u16);
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);

        write_move_descriptor(&mut memory, descriptor, 0, handle, 1025, 0, 0x0300);
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA4);

        write_move_descriptor(&mut memory, descriptor, 0, 0, 0x0200, handle, 1025);
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA6);

        write_move_descriptor(&mut memory, descriptor, 2, handle, 1024, 0, 0x0300);
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA7);
    }

    #[test]
    fn test_move_block_overlap_across_bulk_chunk_boundary() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let descriptor = 0x0100;
        let source = 0x20_000;
        let destination = source + 2;
        let length = XMS_MOVE_CHUNK_SIZE + 2;

        for (offset, value) in [
            (0, 0x11),
            (1, 0x22),
            (XMS_MOVE_CHUNK_SIZE - 2, 0x33),
            (XMS_MOVE_CHUNK_SIZE - 1, 0x44),
            (XMS_MOVE_CHUNK_SIZE, 0x55),
            (XMS_MOVE_CHUNK_SIZE + 1, 0x66),
        ] {
            memory.write_u8(source + offset, value);
        }
        write_move_descriptor(
            &mut memory,
            descriptor,
            length as u32,
            0,
            0x2000_0000,
            0,
            0x2000_0002,
        );

        cpu.regs.set_si(descriptor as u16);
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);

        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_eq!(memory.read_u8(destination), 0x11);
        assert_eq!(memory.read_u8(destination + 1), 0x22);
        assert_eq!(memory.read_u8(destination + XMS_MOVE_CHUNK_SIZE - 2), 0x33);
        assert_eq!(memory.read_u8(destination + XMS_MOVE_CHUNK_SIZE - 1), 0x44);
        assert_eq!(memory.read_u8(destination + XMS_MOVE_CHUNK_SIZE), 0x55);
        assert_eq!(memory.read_u8(destination + XMS_MOVE_CHUNK_SIZE + 1), 0x66);
    }

    #[test]
    fn test_dispatch_move_block_overlap_backwards() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.ds = 0x0000;
        cpu.regs.set_si(0x0200);
        let struct_addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());

        memory.write_u32(struct_addr, 4);
        memory.write_u16(struct_addr + 4, 0);
        memory.write_u32(struct_addr + 6, 0x0000_0400);
        memory.write_u16(struct_addr + 10, 0);
        memory.write_u32(struct_addr + 12, 0x0000_0402);

        memory.write_u8(0x0400, 0x11);
        memory.write_u8(0x0401, 0x22);
        memory.write_u8(0x0402, 0x33);
        memory.write_u8(0x0403, 0x44);
        memory.write_u8(0x0404, 0x55);

        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_eq!(memory.read_u8(0x0402), 0x11);
        assert_eq!(memory.read_u8(0x0403), 0x22);
        assert_eq!(memory.read_u8(0x0404), 0x33);
        assert_eq!(memory.read_u8(0x0405), 0x44);
    }

    #[test]
    fn test_dispatch_move_block_invalid_length() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0x09);
        cpu.regs.set_dx(1);
        xms.dispatch(&mut cpu, &mut memory);
        let handle = cpu.regs.dx();

        cpu.regs.ds = 0x0000;
        cpu.regs.set_si(0x0300);
        let struct_addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());

        memory.write_u32(struct_addr, 4);
        memory.write_u16(struct_addr + 4, handle);
        memory.write_u32(struct_addr + 6, 1022);
        memory.write_u16(struct_addr + 10, 0);
        memory.write_u32(struct_addr + 12, 0x0000_0500);

        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA7);
    }

    #[test]
    fn test_dispatch_with_umb_no_init() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);

        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(0x100);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xB1);
    }

    #[test]
    fn test_dispatch_with_umb_request_release() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);

        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(0x0010);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);
        let umb_seg = cpu.regs.bx();
        assert_ne!(umb_seg, 0);

        cpu.regs.set_ah(0x11);
        cpu.regs.set_dx(umb_seg);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);
    }

    #[test]
    fn test_release_umb_rejects_conventional_dos_allocation() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);
        let segment = dos_mem.alloc(&mut memory, 0x1234, 0x20).unwrap();

        cpu.regs.set_ah(0x11);
        cpu.regs.set_dx(segment);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);

        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xB2);
        let block = dos_mem.block_info_for_segment(segment).unwrap();
        assert!(!block.is_umb);
        assert_eq!(block.owner_psp, 0x1234);
        assert_eq!(block.size_paras, 0x20);
    }

    #[test]
    fn test_reallocate_umb_rejects_dos_owned_umb() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);
        let segment = dos_mem
            .alloc_with_strategy_and_umb(&mut memory, 0x1234, 0x20, 0x80, true)
            .unwrap();

        cpu.regs.set_ah(0x12);
        cpu.regs.set_dx(segment);
        cpu.regs.set_bx(0x10);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);

        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xB2);
        let block = dos_mem.block_info_for_segment(segment).unwrap();
        assert!(block.is_umb);
        assert_eq!(block.owner_psp, 0x1234);
        assert_eq!(block.size_paras, 0x20);
    }

    #[test]
    fn test_dispatch_version_query_free_and_a20_global() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0x00);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0300);
        assert_eq!(cpu.regs.bx(), 0x0100);
        assert_eq!(cpu.regs.dx(), 0x0001);

        cpu.regs.set_ah(0x08);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.bl(), 0x00);
        assert!(cpu.regs.ax() > 0);
        assert!(cpu.regs.dx() > 0);

        memory.set_a20(false);

        cpu.regs.set_ah(0x03); // Global enable
        xms.dispatch(&mut cpu, &mut memory);
        assert!(memory.a20_enabled());
        assert_eq!(cpu.regs.ax(), 0x0001);
        // Global ownership is its own flag and leaves the LOCAL nesting count
        // alone; this used to read `a20_local_count() == 1` (MDD-BUG-CRU-01020).
        assert!(xms.a20_global_enabled());
        assert_eq!(xms.a20_local_count(), 0);

        cpu.regs.set_ah(0x07); // Query A20 — reads the gate, which is up
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 1);

        cpu.regs.set_ah(0x04); // Global disable
        xms.dispatch(&mut cpu, &mut memory);
        assert!(!memory.a20_enabled());
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert!(!xms.a20_global_enabled());
        assert_eq!(xms.a20_local_count(), 0);

        cpu.regs.set_ah(0x07); // Query A20 — reads the gate, which is down
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0);
    }

    #[test]
    fn test_dispatch_lock_info_unlock_resize() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0x09);
        cpu.regs.set_dx(2);
        xms.dispatch(&mut cpu, &mut memory);
        let handle = cpu.regs.dx();
        assert_ne!(handle, 0);

        cpu.regs.set_ah(0x0C);
        cpu.regs.set_dx(handle);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);

        cpu.regs.set_ah(0x0E);
        cpu.regs.set_dx(handle);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_eq!(cpu.regs.bh(), 1);
        assert_eq!(cpu.regs.dx(), 2);

        cpu.regs.set_ah(0x0D);
        cpu.regs.set_dx(handle);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);

        cpu.regs.set_ah(0x0F);
        cpu.regs.set_dx(handle);
        cpu.regs.set_bx(1);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
    }

    #[test]
    fn test_dispatch_xms30_extended_info_and_resize() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0x88);
        xms.dispatch(&mut cpu, &mut memory);
        assert!(cpu.regs.eax > 0);
        assert!(cpu.regs.edx > 0);

        cpu.regs.set_ah(0x89);
        cpu.regs.edx = 70_000;
        xms.dispatch(&mut cpu, &mut memory);
        let handle = cpu.regs.dx();
        assert_ne!(handle, 0);

        cpu.regs.set_ah(0x8E);
        cpu.regs.set_dx(handle);
        cpu.regs.ebx = 0x1234_56A5;
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_eq!(cpu.regs.bh(), 0);
        assert_eq!(cpu.regs.bl(), 0xA5);
        assert_eq!(cpu.regs.cx(), 127);
        assert_eq!(cpu.regs.edx, 70_000);

        cpu.regs.set_ah(0x8F);
        cpu.regs.set_dx(handle);
        cpu.regs.ebx = 70_001;
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_eq!(cpu.regs.ebx, 70_001);
        assert_eq!(xms.get_block_info(handle), Some((70_001, 0)));
    }

    #[test]
    fn test_xms30_query_reports_fixed_highest_physical_address() {
        fn query(xms: &mut Xms, cpu: &mut Cpu, memory: &mut Memory) -> (u32, u32, u32) {
            cpu.regs.set_ah(0x88);
            xms.dispatch(cpu, memory);
            (cpu.regs.eax, cpu.regs.edx, cpu.regs.ecx)
        }

        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let pool_ceiling = u32::try_from(gp64_memory::DOS_RAM_CEILING)
            .expect("DOS RAM ceiling must fit the XMS 3.0 register");
        let expected_highest = pool_ceiling
            .checked_sub(1)
            .expect("the XMS pool ceiling must be nonzero");
        let total_kb = xms.total_kb();

        let (largest_before, free_before, highest_before) = query(&mut xms, &mut cpu, &mut memory);
        assert_eq!(largest_before, total_kb);
        assert_eq!(free_before, total_kb);
        assert_eq!(highest_before, expected_highest);
        assert_eq!(highest_before.checked_add(1), Some(pool_ceiling));

        let partial = xms.allocate_block(1).expect("partial allocation");
        let (_, free_after_allocation, highest_after_allocation) =
            query(&mut xms, &mut cpu, &mut memory);
        assert_eq!(free_after_allocation, total_kb - 1);
        assert_eq!(highest_after_allocation, expected_highest);

        assert!(xms.free_block(partial).is_ok());
        let (_, free_after_free, highest_after_free) = query(&mut xms, &mut cpu, &mut memory);
        assert_eq!(free_after_free, total_kb);
        assert_eq!(highest_after_free, expected_highest);

        let full = xms.allocate_block(total_kb).expect("full-pool allocation");
        let (largest_exhausted, free_exhausted, highest_exhausted) =
            query(&mut xms, &mut cpu, &mut memory);
        assert_eq!(largest_exhausted, 0);
        assert_eq!(free_exhausted, 0);
        assert_eq!(highest_exhausted, expected_highest);
        assert!(xms.free_block(full).is_ok());
    }

    #[test]
    fn test_xms30_extended_functions_require_386() {
        for function in [0x88, 0x89, 0x8E, 0x8F] {
            let (mut cpu, mut memory) = new_cpu_mem();
            cpu.set_cpu_type(gp64_cpu::CpuType::I80286);
            let mut xms = Xms::new();
            let handle = xms
                .allocate_block(2)
                .expect("XMS allocation should succeed");

            cpu.regs.set_ah(function);
            cpu.regs.set_dx(handle);
            cpu.regs.ebx = 1;
            xms.dispatch(&mut cpu, &mut memory);

            assert_eq!(cpu.regs.ax(), 0, "function {function:02X}h");
            assert_eq!(cpu.regs.bl(), 0x80, "function {function:02X}h");
            assert_eq!(xms.get_block_info(handle), Some((2, 0)));
        }
    }

    // =========================================================================
    // Branch-coverage tests below
    // =========================================================================

    #[test]
    fn test_hma_alloc_when_already_allocated() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        // First alloc succeeds
        cpu.regs.set_ah(0x01);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);

        // Second alloc fails (already allocated branch)
        cpu.regs.set_ah(0x01);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0x91);
    }

    #[test]
    fn test_alloc_failure_exhausted_memory() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        // Query free memory using XMS 3.0 32-bit API (AH=88h) to get the
        // full free KB without the 16-bit 0xFFFF cap of AH=08h.
        cpu.regs.set_ah(0x88);
        xms.dispatch(&mut cpu, &mut memory);
        let total_free_kb = cpu.regs.edx; // EDX = total free KB (uncapped)
        assert!(total_free_kb > 0);

        // Allocate all memory in one big block using XMS 3.0 (AH=89h)
        cpu.regs.set_ah(0x89);
        cpu.regs.edx = total_free_kb;
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);

        // Now try to allocate more — should fail
        cpu.regs.set_ah(0x89);
        cpu.regs.edx = 1;
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA0);
    }

    #[test]
    fn test_move_block_invalid_src_handle() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.ds = 0x0000;
        cpu.regs.set_si(0x0100);
        let sa = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());

        // length=4, src_handle=0xBEEF (invalid), src_offset=0, dst_handle=0, dst_offset=0x0500
        memory.write_u32(sa, 4);
        memory.write_u16(sa + 4, 0xBEEF); // invalid handle
        memory.write_u32(sa + 6, 0);
        memory.write_u16(sa + 10, 0);
        memory.write_u32(sa + 12, 0x0000_0500);

        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA3); // Invalid source handle
    }

    #[test]
    fn test_move_block_src_offset_exceeds_size() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        // Allocate 1KB block
        cpu.regs.set_ah(0x09);
        cpu.regs.set_dx(1);
        xms.dispatch(&mut cpu, &mut memory);
        let handle = cpu.regs.dx();

        cpu.regs.ds = 0x0000;
        cpu.regs.set_si(0x0100);
        let sa = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());

        // length=512, src_handle=handle, src_offset=768 (768+512=1280 > 1024)
        memory.write_u32(sa, 512);
        memory.write_u16(sa + 4, handle);
        memory.write_u32(sa + 6, 768);
        memory.write_u16(sa + 10, 0);
        memory.write_u32(sa + 12, 0x0000_0500);

        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA7); // Invalid length
    }

    #[test]
    fn test_move_block_invalid_dst_handle() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.ds = 0x0000;
        cpu.regs.set_si(0x0100);
        let sa = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());

        // src from conventional, dst handle invalid
        memory.write_u32(sa, 4);
        memory.write_u16(sa + 4, 0); // conventional src
        memory.write_u32(sa + 6, 0x0000_0500);
        memory.write_u16(sa + 10, 0xDEAD); // invalid dst handle
        memory.write_u32(sa + 12, 0);

        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA5); // Invalid destination handle
    }

    #[test]
    fn test_move_block_dst_offset_exceeds_size() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        // Allocate 1KB block
        cpu.regs.set_ah(0x09);
        cpu.regs.set_dx(1);
        xms.dispatch(&mut cpu, &mut memory);
        let handle = cpu.regs.dx();

        cpu.regs.ds = 0x0000;
        cpu.regs.set_si(0x0100);
        let sa = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());

        // src from conventional, dst handle valid but offset+length > block_size
        memory.write_u32(sa, 512);
        memory.write_u16(sa + 4, 0); // conventional src
        memory.write_u32(sa + 6, 0x0000_0500);
        memory.write_u16(sa + 10, handle);
        memory.write_u32(sa + 12, 768); // 768+512=1280 > 1024

        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA7); // Invalid length
    }

    #[test]
    fn test_move_block_zero_length() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.ds = 0x0000;
        cpu.regs.set_si(0x0100);
        let sa = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());

        // length=0, conventional to conventional
        memory.write_u32(sa, 0); // zero length
        memory.write_u16(sa + 4, 0);
        memory.write_u32(sa + 6, 0x0000_0400);
        memory.write_u16(sa + 10, 0);
        memory.write_u32(sa + 12, 0x0000_0500);

        // Write sentinel to ensure it's NOT overwritten
        memory.write_u8(0x0500, 0xFF);

        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_eq!(memory.read_u8(0x0500), 0xFF); // untouched
    }

    #[test]
    fn test_lock_block_invalid_handle() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0x0C);
        cpu.regs.set_dx(0xFFFF); // invalid handle
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA2);
    }

    #[test]
    fn test_unlock_block_invalid_handle() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0x0D);
        cpu.regs.set_dx(0xFFFF); // invalid handle
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA2);
    }

    #[test]
    fn test_get_block_info_invalid_handle() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0x0E);
        cpu.regs.set_dx(0xFFFF); // invalid handle
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA2);
    }

    #[test]
    fn test_resize_block_invalid_handle() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0x0F);
        cpu.regs.set_dx(0xFFFF); // invalid handle
        cpu.regs.set_bx(1);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA2);
    }

    #[test]
    fn test_dispatch_umb_stubs_no_umb() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        // 0x10 - Request UMB (basic dispatch, no UMB)
        cpu.regs.set_ah(0x10);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xB1);

        // 0x11 - Release UMB (basic dispatch, no UMB)
        cpu.regs.set_ah(0x11);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xB2);
    }

    #[test]
    fn test_dispatch_xms30_extended_errors() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        // Extended handle information rejects an invalid handle.
        cpu.regs.set_ah(0x8E);
        cpu.regs.set_dx(0xFFFF);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA2);

        // Reallocation distinguishes invalid and locked handles.
        cpu.regs.set_ah(0x8F);
        cpu.regs.set_dx(0xFFFF);
        cpu.regs.ebx = 1;
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA2);

        let handle = xms
            .allocate_block(2)
            .expect("XMS allocation should succeed");
        assert!(xms.lock_block(handle).is_ok());
        cpu.regs.set_ah(0x8F);
        cpu.regs.set_dx(handle);
        cpu.regs.ebx = 1;
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xAB);
        assert_eq!(xms.get_block_info(handle), Some((2, 1)));

        cpu.regs.set_ah(0x0D);
        cpu.regs.set_dx(handle);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        let block_before = xms.get_block_info(handle);

        cpu.regs.set_ah(0x8F);
        cpu.regs.set_dx(handle);
        cpu.regs.ebx = xms.total_kb() + 1;
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA0);
        assert_eq!(xms.get_block_info(handle), block_before);
    }

    #[test]
    fn test_xms30_alloc_failure() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        // Query free memory via XMS 3.0 (0x88) to get actual total
        cpu.regs.set_ah(0x88);
        xms.dispatch(&mut cpu, &mut memory);
        let total_free_kb = cpu.regs.edx;
        assert!(total_free_kb > 0);

        // Exhaust all memory using XMS 3.0 alloc
        cpu.regs.set_ah(0x89);
        cpu.regs.edx = total_free_kb;
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);

        // XMS 3.0 alloc should fail
        cpu.regs.set_ah(0x89);
        cpu.regs.edx = 1;
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA0);
    }

    #[test]
    fn test_xms30_undefined_functions_are_side_effect_free() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let handle = xms
            .allocate_block(2)
            .expect("XMS allocation should succeed");
        let initial_free_kb = xms.free_kb();
        let initial_info = xms.get_block_info(handle);

        for function in 0x8A..=0x8D {
            cpu.regs.set_ah(function);
            cpu.regs.set_dx(handle);
            cpu.regs.ebx = 1;
            xms.dispatch(&mut cpu, &mut memory);
            assert_eq!(cpu.regs.ax(), 0x0000, "function {function:02X}h");
            assert_eq!(cpu.regs.bl(), 0x80, "function {function:02X}h");
            assert_eq!(xms.free_kb(), initial_free_kb, "function {function:02X}h");
            assert_eq!(
                xms.get_block_info(handle),
                initial_info,
                "function {function:02X}h"
            );
        }
    }

    #[test]
    fn test_dispatch_unknown_function() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        cpu.regs.set_ah(0xFE); // unrecognized function
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0x80); // Not implemented
    }

    #[test]
    fn test_dispatch_with_umb_reallocate() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);

        // Allocate a UMB first
        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(0x0020);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);
        let umb_seg = cpu.regs.bx();

        // Reallocate (func 0x12) to smaller
        cpu.regs.set_ah(0x12);
        cpu.regs.set_dx(umb_seg);
        cpu.regs.set_bx(0x0010);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);
    }

    #[test]
    fn xms_umb_shrink_one_paragraph_moves_free_successor() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);
        let arena_end = umb_arena_end(&dos_mem);

        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(0x0020);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        let segment = cpu.regs.bx();

        cpu.regs.set_ah(0x12);
        cpu.regs.set_dx(segment);
        cpu.regs.set_bx(0x001F);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);

        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_guest_umb_chain_contiguous(&memory, &dos_mem, arena_end);
    }

    #[test]
    fn xms_umb_shrink_one_paragraph_inserts_before_allocated_successor() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);
        let arena_end = umb_arena_end(&dos_mem);

        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(0x0020);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        let first_segment = cpu.regs.bx();
        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(0x0020);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        let second_segment = cpu.regs.bx();

        cpu.regs.set_ah(0x12);
        cpu.regs.set_dx(first_segment);
        cpu.regs.set_bx(0x001F);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);

        assert_eq!(cpu.regs.ax(), 0x0001);
        let blocks: Vec<_> = dos_mem
            .mcb_summary()
            .into_iter()
            .filter(|entry| entry.is_umb)
            .collect();
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[1].owner_psp, 0);
        assert_eq!(blocks[1].size_paras, 0);
        assert_eq!(blocks[2].seg.wrapping_add(1), second_segment);
        assert_guest_umb_chain_contiguous(&memory, &dos_mem, arena_end);
    }

    #[test]
    fn xms_umb_shrink_one_paragraph_inserts_after_last_block() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);
        let arena_end = umb_arena_end(&dos_mem);
        let full_size = dos_mem.largest_free_umb();

        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(full_size);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        let segment = cpu.regs.bx();

        cpu.regs.set_ah(0x12);
        cpu.regs.set_dx(segment);
        cpu.regs.set_bx(full_size - 1);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);

        assert_eq!(cpu.regs.ax(), 0x0001);
        let blocks: Vec<_> = dos_mem
            .mcb_summary()
            .into_iter()
            .filter(|entry| entry.is_umb)
            .collect();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].owner_psp, 0);
        assert_eq!(blocks[1].size_paras, 0);
        assert_guest_umb_chain_contiguous(&memory, &dos_mem, arena_end);
    }

    #[test]
    fn test_dispatch_with_umb_delegates_xms30_extended_functions() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);
        let initial_umb_free = dos_mem.largest_free_umb();

        cpu.regs.set_ah(0x89);
        cpu.regs.edx = 70_000;
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);
        let handle = cpu.regs.dx();

        cpu.regs.set_ah(0x0C);
        cpu.regs.set_dx(handle);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);

        // 8Eh is extended XMS handle information, not a 32-bit UMB request.
        cpu.regs.set_ah(0x8E);
        cpu.regs.edx = handle as u32;
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_eq!(cpu.regs.bh(), 1);
        assert_eq!(cpu.regs.cx(), 127);
        assert_eq!(cpu.regs.edx, 70_000);
        assert_eq!(dos_mem.largest_free_umb(), initial_umb_free);

        cpu.regs.set_ah(0x0D);
        cpu.regs.set_dx(handle);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);

        // 8Fh reallocates XMS using EBX, without touching the UMB arena.
        cpu.regs.set_ah(0x8F);
        cpu.regs.set_dx(handle);
        cpu.regs.ebx = 70_001;
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert_eq!(xms.get_block_info(handle), Some((70_001, 0)));
        assert_eq!(dos_mem.largest_free_umb(), initial_umb_free);
    }

    #[test]
    fn test_xms30_reallocate_does_not_release_umb() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);

        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(0x0020);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);
        let umb_segment = cpu.regs.bx();
        let free_after_allocation = dos_mem.largest_free_umb();

        cpu.regs.set_ah(0x8F);
        cpu.regs.set_dx(umb_segment);
        cpu.regs.ebx = 1;
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xA2);
        assert_eq!(dos_mem.largest_free_umb(), free_after_allocation);
    }

    #[test]
    fn test_dispatch_with_umb_fallthrough_to_dispatch() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);

        // Non-UMB function through dispatch_with_umb should fall through to dispatch
        cpu.regs.set_ah(0x00);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0300); // XMS version from regular dispatch
    }

    #[test]
    fn test_release_umb_invalid_segment() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);

        // Release with invalid segment
        cpu.regs.set_ah(0x11);
        cpu.regs.set_dx(0xFFFF); // invalid segment
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xB2);
    }

    #[test]
    fn test_reallocate_umb_invalid_segment() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);

        // Reallocate with invalid segment
        cpu.regs.set_ah(0x12);
        cpu.regs.set_dx(0xFFFF); // invalid segment
        cpu.regs.set_bx(0x0010);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xB2);
    }

    #[test]
    fn test_request_umb_no_free_space() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);

        // First, find out how much UMB space is available and allocate it all
        let largest = dos_mem.largest_free_umb();
        assert!(largest > 0, "UMB should have free space after init");

        // Allocate the entire UMB space
        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(largest);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0001);

        // Now try to allocate when there's no free space (largest == 0 branch)
        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(1);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xB1);
    }

    #[test]
    fn test_dispatch_move_block_xms_to_xms_forward() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        // Allocate source block (4KB)
        cpu.regs.set_ah(0x09);
        cpu.regs.set_dx(4);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        let src_handle = cpu.regs.dx();

        // Allocate destination block (4KB)
        cpu.regs.set_ah(0x09);
        cpu.regs.set_dx(4);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001);
        let dst_handle = cpu.regs.dx();

        // Write test data directly into source block's memory region.
        // A20 must be enabled to access above 1MB; set it temporarily.
        let (src_addr, _, _) = xms.find_block(src_handle).expect("source block must exist");
        let prev_a20 = memory.a20_enabled();
        memory.set_a20(true);
        for i in 0..128u8 {
            memory.write_u8(src_addr as usize + i as usize, 0xA0 + (i & 0x0F));
        }
        memory.set_a20(prev_a20);

        // Build MoveBlock structure at DS:SI = 0000:0100
        cpu.regs.ds = 0x0000;
        cpu.regs.set_si(0x0100);
        let sa = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());

        memory.write_u32(sa, 128); // length = 128 bytes
        memory.write_u16(sa + 4, src_handle); // src_handle = XMS block
        memory.write_u32(sa + 6, 0); // src_offset = 0
        memory.write_u16(sa + 10, dst_handle); // dst_handle = XMS block
        memory.write_u32(sa + 12, 0); // dst_offset = 0

        // Dispatch move_block (function 0x0B)
        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001, "move_block should succeed");
        assert_eq!(cpu.regs.bl(), 0x00);

        // Verify data was copied to destination block
        let (dst_addr, _, _) = xms.find_block(dst_handle).expect("dest block must exist");
        memory.set_a20(true);
        for i in 0..128u8 {
            assert_eq!(
                memory.read_u8(dst_addr as usize + i as usize),
                0xA0 + (i & 0x0F),
                "byte {} mismatch",
                i
            );
        }
        memory.set_a20(prev_a20);
    }

    #[test]
    fn test_dispatch_move_block_conventional_forward_no_overlap() {
        // Exercises line 200 false branch: non-overlapping forward copy (src >= dst)
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();

        // Write source data at conventional address 0x2000
        let src_base = 0x2000usize;
        for i in 0..64u8 {
            memory.write_u8(src_base + i as usize, 0x50 + (i & 0x0F));
        }

        // Destination at 0x1000 (lower than source, so src >= dst → forward copy)
        let dst_base = 0x1000usize;

        // Build MoveBlock structure at 0000:0100
        cpu.regs.ds = 0x0000;
        cpu.regs.set_si(0x0100);
        let sa = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());
        memory.write_u32(sa, 64); // length
        memory.write_u16(sa + 4, 0); // src_handle = 0 (conventional)
                                     // conventional offset is encoded as seg:off in u32: high16=seg, low16=off
        memory.write_u32(sa + 6, (src_base as u32 >> 4) << 16);
        memory.write_u16(sa + 10, 0); // dst_handle = 0 (conventional)
        memory.write_u32(sa + 12, (dst_base as u32 >> 4) << 16);

        cpu.regs.set_ah(0x0B);
        xms.dispatch(&mut cpu, &mut memory);
        assert_eq!(cpu.regs.ax(), 0x0001, "move should succeed");

        // Verify data was copied
        for i in 0..64u8 {
            assert_eq!(
                memory.read_u8(dst_base + i as usize),
                0x50 + (i & 0x0F),
                "byte {} mismatch",
                i
            );
        }
    }

    #[test]
    fn test_request_umb_too_large() {
        let (mut cpu, mut memory) = new_cpu_mem();
        let mut xms = Xms::new();
        let mut dos_mem = DosMem::new(0x0100);
        dos_mem.init(&mut memory);
        dos_mem.init_umb(&mut memory);

        let largest = dos_mem.largest_free_umb();
        assert!(largest > 0);

        // Request more than available — hits paragraphs > largest branch
        cpu.regs.set_ah(0x10);
        cpu.regs.set_dx(largest + 1);
        xms.dispatch_with_umb(&mut cpu, &mut memory, &mut dos_mem);
        assert_eq!(cpu.regs.ax(), 0x0000);
        assert_eq!(cpu.regs.bl(), 0xB0); // Smaller UMB available
        assert_eq!(cpu.regs.dx(), largest); // reports what IS available
    }
}
