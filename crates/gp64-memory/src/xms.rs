use crate::{Memory, DOS_RAM_CEILING};
use gp64_types::FixedVec;

const XMS_BASE: u32 = 0x0010_0000;

/// The HMA is the first 64 KiB (less 16 bytes) above 1 MiB. While a client
/// owns it, those bytes are occupied and no extended-memory block may use them.
const HMA_END: u32 = XMS_BASE + 0x1_0000;
const HMA_KB: u32 = (HMA_END - XMS_BASE) / 1024;

/// XMS error: the HMA is already in use (function 01h).
pub const XMS_HMA_IN_USE: u8 = 0x91;
/// XMS error: the HMA was not allocated (function 02h).
pub const XMS_HMA_NOT_ALLOCATED: u8 = 0x93;

/// Maximum number of concurrently allocated XMS blocks (handles). Matches the
/// handle count the XMS dispatch advertises to the guest; allocating past it
/// returns the XMS "all handles in use" (0xA1) error rather than panicking the
/// fixed-capacity block table.
const XMS_MAX_HANDLES: usize = 128;

/// Failure reasons returned by XMS block-management operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmsError {
    /// No contiguous block can satisfy the requested allocation or growth.
    OutOfMemory,
    /// No XMS handle is available for a new block.
    HandlesExhausted,
    /// The supplied handle does not identify an allocated block.
    InvalidHandle,
    /// Unlock was requested for a block whose lock count is zero.
    BlockNotLocked,
    /// Free or resize was requested for a locked block.
    BlockLocked,
    /// Lock was requested for a block whose lock count is already 255.
    LockCountOverflow,
    /// The driver could not complete an internal relocation.
    GeneralFailure,
}

impl XmsError {
    /// XMS 3.0 error code returned in BL.
    pub const fn code(self) -> u8 {
        match self {
            Self::OutOfMemory => 0xA0,
            Self::HandlesExhausted => 0xA1,
            Self::InvalidHandle => 0xA2,
            Self::BlockNotLocked => 0xAA,
            Self::BlockLocked => 0xAB,
            Self::LockCountOverflow => 0xAC,
            Self::GeneralFailure => 0x8E,
        }
    }
}

/// XMS driver state
#[derive(Clone)]
pub struct Xms {
    /// Allocated memory blocks
    blocks: FixedVec<XmsBlock, 256>,
    /// Next handle
    next_handle: u16,
    /// Reusable handles from freed blocks
    free_handles: FixedVec<u16, 256>,
    /// Total extended memory (in KB)
    total_kb: u32,
    /// Available extended memory (in KB)
    free_kb: u32,
    /// HMA allocated flag (true if HMA is in use)
    hma_allocated: bool,
    /// A20 local enable count (XMS A20 local enable/disable, functions 05h/06h).
    ///
    /// Nested ownership: the gate stays XMS-owned until as many local disables
    /// as local enables have arrived.
    a20_local_count: u16,
    /// A20 global ownership flag (XMS A20 global enable/disable, 03h/04h).
    ///
    /// A FLAG, not a count: XMS 3.0 gives global ownership its own state, so
    /// repeated global enables are idempotent and a global disable cannot
    /// consume a local owner's nesting (MDD-BUG-CRU-01020).
    a20_global_enabled: bool,
}

/// XMS memory block
#[derive(Clone)]
struct XmsBlock {
    handle: u16,
    size_kb: u32,
    /// Lock count (0 = unlocked, >0 = locked)
    lock_count: u8,
    address: u32,
}

impl Default for Xms {
    fn default() -> Self {
        Self::new()
    }
}

impl Xms {
    pub fn new() -> Self {
        // Provide extended memory up to DOS_RAM_CEILING (above 1MB)
        let total_kb = (DOS_RAM_CEILING as u32).saturating_sub(XMS_BASE) / 1024;
        Self {
            blocks: FixedVec::new(),
            next_handle: 1,
            free_handles: FixedVec::new(),
            total_kb,
            free_kb: total_kb,
            hma_allocated: false,
            a20_local_count: 0,
            a20_global_enabled: false,
        }
    }

    fn block_bytes(block: &XmsBlock) -> u32 {
        block.size_kb.saturating_mul(1024)
    }

    fn allocate_handle(&mut self) -> Option<u16> {
        if let Some(handle) = self.free_handles.pop() {
            return Some(handle);
        }
        if self.next_handle == 0 {
            return None;
        }
        let handle = self.next_handle;
        self.next_handle = self.next_handle.wrapping_add(1);
        Some(handle)
    }

    fn allocated_ranges(&self, skip_handle: Option<u16>) -> Vec<(u32, u32)> {
        let mut ranges: Vec<(u32, u32)> = self
            .blocks
            .iter()
            .filter(|b| skip_handle != Some(b.handle))
            .map(|b| (b.address, b.address.saturating_add(Self::block_bytes(b))))
            .collect();
        if self.hma_allocated {
            ranges.push((XMS_BASE, HMA_END));
        }
        ranges.sort_by_key(|(start, _)| *start);
        ranges
    }

    fn find_free_range(&self, size_bytes: u32, skip_handle: Option<u16>) -> Option<u32> {
        let ranges = self.allocated_ranges(skip_handle);
        let mut cur = XMS_BASE;

        for (start, end) in ranges {
            if cur.saturating_add(size_bytes) <= start
                && cur.saturating_add(size_bytes) <= DOS_RAM_CEILING as u32
            {
                return Some(cur);
            }
            if end > cur {
                cur = end;
            }
        }

        if cur.saturating_add(size_bytes) <= DOS_RAM_CEILING as u32 {
            Some(cur)
        } else {
            None
        }
    }

    // =========================================================================
    // Accessors for extended implementations
    // =========================================================================

    /// Get the total extended memory in KB
    pub fn total_kb(&self) -> u32 {
        self.total_kb
    }

    /// Get the available extended memory in KB
    pub fn free_kb(&self) -> u32 {
        self.free_kb
    }

    /// Check if HMA is allocated
    pub fn hma_allocated(&self) -> bool {
        self.hma_allocated
    }

    /// XMS function 01h: give the HMA to the caller.
    ///
    /// The HMA's 64 KiB become an occupied allocator range, so they leave
    /// the free count and no block can be placed there. A block already
    /// holding those bytes keeps them, and the request fails as in use.
    pub fn request_hma(&mut self) -> Result<(), u8> {
        let overlaps_block = self
            .allocated_ranges(None)
            .iter()
            .any(|&(start, end)| start < HMA_END && end > XMS_BASE);
        if self.hma_allocated || overlaps_block {
            return Err(XMS_HMA_IN_USE);
        }
        self.hma_allocated = true;
        self.free_kb = self.free_kb.saturating_sub(HMA_KB);
        Ok(())
    }

    /// XMS function 02h: return the HMA to the extended-memory pool.
    pub fn release_hma(&mut self) -> Result<(), u8> {
        if !self.hma_allocated {
            return Err(XMS_HMA_NOT_ALLOCATED);
        }
        self.hma_allocated = false;
        self.free_kb = self.free_kb.saturating_add(HMA_KB);
        Ok(())
    }

    /// Get the A20 local enable count
    pub fn a20_local_count(&self) -> u16 {
        self.a20_local_count
    }

    /// Increment A20 local enable count (saturating)
    pub fn increment_a20_local_count(&mut self) {
        self.a20_local_count = self.a20_local_count.saturating_add(1);
    }

    /// Decrement A20 local enable count (saturating at 0)
    pub fn decrement_a20_local_count(&mut self) {
        self.a20_local_count = self.a20_local_count.saturating_sub(1);
    }

    /// Whether XMS global A20 ownership (function 03h) is held.
    pub fn a20_global_enabled(&self) -> bool {
        self.a20_global_enabled
    }

    /// Take or release XMS global A20 ownership. Idempotent by construction.
    pub fn set_a20_global_enabled(&mut self, enabled: bool) {
        self.a20_global_enabled = enabled;
    }

    /// Whether XMS itself is holding A20 up, from BOTH kinds of ownership.
    ///
    /// This is what the gate should be set to after any 03h-06h call. It is
    /// deliberately not the same question as [`Memory::a20_enabled`], which is
    /// the physical gate and can be changed by the keyboard controller or the
    /// fast-A20 port behind XMS's back — function 07h answers THAT one.
    pub fn xms_holds_a20(&self) -> bool {
        self.a20_global_enabled || self.a20_local_count > 0
    }

    /// Get the number of allocated blocks
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Find a block by handle
    pub fn find_block(&self, handle: u16) -> Option<(u32, u32, u8)> {
        self.blocks
            .iter()
            .find(|b| b.handle == handle)
            .map(|b| (b.address, b.size_kb, b.lock_count))
    }

    /// Allocate an XMS block
    pub fn allocate_block(&mut self, size_kb: u32) -> Result<u16, XmsError> {
        // Refuse once the advertised handle limit is reached, so the guest gets
        // the XMS "all handles in use" (0xA1) error path instead of panicking the
        // fixed-capacity block table (MDD-BUG-ANVIL-00013).
        if self.blocks.len() >= XMS_MAX_HANDLES {
            return Err(XmsError::HandlesExhausted);
        }
        if size_kb > self.free_kb {
            return Err(XmsError::OutOfMemory);
        }

        let size_bytes = size_kb.saturating_mul(1024);
        let address = self
            .find_free_range(size_bytes, None)
            .ok_or(XmsError::OutOfMemory)?;
        let handle = self.allocate_handle().ok_or(XmsError::HandlesExhausted)?;

        self.blocks.push(XmsBlock {
            handle,
            size_kb,
            lock_count: 0,
            address,
        });
        self.free_kb = self.free_kb.saturating_sub(size_kb);

        Ok(handle)
    }

    /// Free an unlocked XMS block by handle.
    pub fn free_block(&mut self, handle: u16) -> Result<(), XmsError> {
        let Some(pos) = self.blocks.iter().position(|b| b.handle == handle) else {
            return Err(XmsError::InvalidHandle);
        };
        if self.blocks[pos].lock_count != 0 {
            return Err(XmsError::BlockLocked);
        }
        let block = self.blocks.remove(pos);
        self.free_kb = self.free_kb.saturating_add(block.size_kb);
        self.free_handles.push(handle);
        Ok(())
    }

    /// Lock a block (increment lock count)
    pub fn lock_block(&mut self, handle: u16) -> Result<u32, XmsError> {
        let block = self
            .blocks
            .iter_mut()
            .find(|b| b.handle == handle)
            .ok_or(XmsError::InvalidHandle)?;
        if block.lock_count == u8::MAX {
            return Err(XmsError::LockCountOverflow);
        }
        block.lock_count += 1;
        Ok(block.address)
    }

    /// Unlock a block (decrement lock count)
    pub fn unlock_block(&mut self, handle: u16) -> Result<(), XmsError> {
        let block = self
            .blocks
            .iter_mut()
            .find(|b| b.handle == handle)
            .ok_or(XmsError::InvalidHandle)?;
        if block.lock_count == 0 {
            return Err(XmsError::BlockNotLocked);
        }
        block.lock_count -= 1;
        Ok(())
    }

    /// Resize a block
    pub fn resize_block(
        &mut self,
        handle: u16,
        new_size_kb: u32,
        memory: &mut Memory,
    ) -> Result<(), XmsError> {
        let Some(pos) = self.blocks.iter().position(|b| b.handle == handle) else {
            return Err(XmsError::InvalidHandle);
        };

        let block = &self.blocks[pos];
        if block.lock_count > 0 {
            return Err(XmsError::BlockLocked);
        }

        let old_size_kb = block.size_kb;
        if new_size_kb == old_size_kb {
            return Ok(());
        }

        if new_size_kb > old_size_kb {
            // Growing - need more memory
            let extra_kb = new_size_kb - old_size_kb;
            if extra_kb > self.free_kb {
                return Err(XmsError::OutOfMemory);
            }

            let new_size_bytes = new_size_kb.saturating_mul(1024);
            let Some(new_address) = self.find_free_range(new_size_bytes, Some(handle)) else {
                return Err(XmsError::OutOfMemory);
            };

            let old_address = self.blocks[pos].address;
            if new_address != old_address {
                let old_size_bytes = old_size_kb.saturating_mul(1024) as usize;
                let old_address = old_address as usize;
                let new_address_usize = new_address as usize;
                // Back both spans first: `bulk_copy_ram` reports success
                // without copying when a range lies past committed RAM, which
                // would move the block onto zeros (MDD-BUG-KIL-00813). Blocks
                // stay below `DOS_RAM_CEILING`, whose capacity is reserved.
                memory.ensure_ram_size(
                    (old_address + old_size_bytes).max(new_address_usize + old_size_bytes),
                );
                let prev_a20 = memory.a20_enabled();
                memory.set_a20(true);
                let copied = memory.bulk_copy_ram(old_address, new_address_usize, old_size_bytes);
                memory.set_a20(prev_a20);
                if !copied {
                    return Err(XmsError::GeneralFailure);
                }
                self.blocks[pos].address = new_address;
            }

            self.blocks[pos].size_kb = new_size_kb;
            self.free_kb = self.free_kb.saturating_sub(extra_kb);
            Ok(())
        } else {
            // Shrinking - always possible
            let freed_kb = old_size_kb - new_size_kb;
            self.blocks[pos].size_kb = new_size_kb;
            self.free_kb = self.free_kb.saturating_add(freed_kb);
            Ok(())
        }
    }

    /// Get block info (`size_kb`, `lock_count`)
    pub fn get_block_info(&self, handle: u16) -> Option<(u32, u8)> {
        self.blocks
            .iter()
            .find(|b| b.handle == handle)
            .map(|b| (b.size_kb, b.lock_count))
    }

    /// Get the largest contiguous free block in KB
    pub fn largest_free_kb(&self) -> u32 {
        let ranges = self.allocated_ranges(None);
        let mut cur = XMS_BASE;
        let mut max_bytes = 0u32;

        for (start, end) in ranges {
            if start > cur {
                let gap = start - cur;
                if gap > max_bytes {
                    max_bytes = gap;
                }
            }
            if end > cur {
                cur = end;
            }
        }

        // Check tail space after last block
        let tail = (DOS_RAM_CEILING as u32).saturating_sub(cur);
        if tail > max_bytes {
            max_bytes = tail;
        }

        max_bytes / 1024
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn xms_error_codes_match_the_xms_3_specification() {
        assert_eq!(XmsError::OutOfMemory.code(), 0xA0);
        assert_eq!(XmsError::HandlesExhausted.code(), 0xA1);
        assert_eq!(XmsError::InvalidHandle.code(), 0xA2);
        assert_eq!(XmsError::BlockNotLocked.code(), 0xAA);
        assert_eq!(XmsError::BlockLocked.code(), 0xAB);
        assert_eq!(XmsError::LockCountOverflow.code(), 0xAC);
        assert_eq!(XmsError::GeneralFailure.code(), 0x8E);
    }

    #[test]
    fn allocate_block_reports_handle_exhaustion_when_handle_table_is_full() {
        // MDD-BUG-ANVIL-00013: allocation must return the "all handles in use"
        // error once the handle table is full, instead of panicking FixedVec::push.
        // Zero-size allocations exercise the handle-count limit without consuming
        // free_kb.
        let mut xms = Xms::new();
        for i in 0..XMS_MAX_HANDLES {
            assert!(
                xms.allocate_block(0).is_ok(),
                "allocation {i} within the handle limit should succeed"
            );
        }
        assert_eq!(xms.block_count(), XMS_MAX_HANDLES);
        assert_eq!(
            xms.allocate_block(0),
            Err(XmsError::HandlesExhausted),
            "allocation past the handle limit must report handle exhaustion"
        );
        assert_eq!(
            xms.block_count(),
            XMS_MAX_HANDLES,
            "no block is added when the allocation is refused"
        );
    }

    #[test]
    fn resize_block_grow_over_successor_relocates_and_preserves_handle() {
        // Relocate a grow-reallocated block when it
        // cannot expand in place but another free range can hold it.
        let mut xms = Xms::new();
        let mut memory = Memory::new();
        let a = xms.allocate_block(512).expect("alloc A");
        let b = xms.allocate_block(512).expect("alloc B");
        let c = xms.allocate_block(1024).expect("alloc C");
        let (a_addr, a_size, _) = xms.find_block(a).expect("A exists");
        let (b_addr, b_size, _) = xms.find_block(b).expect("B exists");
        assert_eq!(
            b_addr,
            a_addr + a_size * 1024,
            "B must be immediately after A for this test"
        );
        let (c_addr, _, _) = xms.find_block(c).expect("C exists");
        assert_eq!(
            c_addr,
            b_addr + b_size * 1024,
            "C must be immediately after B for this test"
        );

        for (offset, byte) in [0xDE, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
            memory.write_u8(a_addr as usize + offset, byte);
        }
        assert!(
            xms.free_block(c).is_ok(),
            "free C to create a relocation target"
        );

        assert!(
            xms.resize_block(a, 1024, &mut memory).is_ok(),
            "growing A should relocate it rather than overlap B"
        );
        let (a_addr_after, a_size_after, _) = xms.find_block(a).expect("A still exists");
        assert_ne!(a_addr_after, a_addr, "A should move to the free range");
        assert_eq!(
            a_size_after, 1024,
            "A should keep the same handle with the requested size"
        );
        assert_eq!(
            xms.find_block(b),
            Some((b_addr, b_size, 0)),
            "B must be unchanged"
        );
        for (offset, byte) in [0xDE, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
            assert_eq!(memory.read_u8(a_addr_after as usize + offset), byte);
        }
        assert_eq!(
            xms.lock_block(a),
            Ok(a_addr_after),
            "future locks on the preserved handle must return the relocated address"
        );
    }

    #[test]
    fn resize_block_relocation_above_committed_ram_preserves_contents() {
        // MDD-BUG-KIL-00813: a relocation target above the committed RAM
        // (DEFAULT_RAM_SIZE) was "copied" by a bulk copy that did nothing.
        let mut xms = Xms::new();
        let mut memory = Memory::new();
        assert_eq!(memory.ram_size(), crate::DEFAULT_RAM_SIZE);
        let a = xms.allocate_block(4).expect("alloc A");
        let low_kb = (crate::DEFAULT_RAM_SIZE as u32 - XMS_BASE) / 1024;
        let filler = xms.allocate_block(low_kb).expect("fill committed RAM");
        let (a_addr, _, _) = xms.find_block(a).expect("A exists");
        for (offset, byte) in [0xDE, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
            memory.write_u8(a_addr as usize + offset, byte);
        }

        assert!(xms.resize_block(a, 8, &mut memory).is_ok());

        let (a_addr_after, _, _) = xms.find_block(a).expect("A relocated");
        assert!(
            a_addr_after as usize >= crate::DEFAULT_RAM_SIZE,
            "A must move above committed RAM for this test"
        );
        for (offset, byte) in [0xDE, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
            assert_eq!(memory.read_u8(a_addr_after as usize + offset), byte);
        }
        assert!(xms.free_block(filler).is_ok());
    }

    #[test]
    fn resize_block_large_tracked_relocation_has_bounded_write_records() {
        // MDD-BUG-CRUCIBLE-00149: relocation used to call write_u8 once per
        // byte, retaining 1,048,576 PendingWrite records for this 1 MiB block
        // until the enclosing XMS handler returned.
        const OLD_SIZE_KB: u32 = 1024;
        const NEW_SIZE_KB: u32 = 2048;
        const OLD_SIZE_BYTES: usize = OLD_SIZE_KB as usize * 1024;

        let mut xms = Xms::new();
        let mut memory = Memory::new();
        let a = xms.allocate_block(OLD_SIZE_KB).expect("alloc A");
        let b = xms.allocate_block(OLD_SIZE_KB).expect("alloc B");
        let relocation_target = xms.allocate_block(NEW_SIZE_KB).expect("alloc target");
        let initial_free_kb = xms.free_kb();
        let (old_address, _, _) = xms.find_block(a).expect("A exists");
        let (b_address, b_size_kb, _) = xms.find_block(b).expect("B exists");
        let (target_address, _, _) = xms.find_block(relocation_target).expect("target exists");

        for offset in 0..OLD_SIZE_BYTES {
            memory.ram[old_address as usize + offset] = (offset as u8).wrapping_mul(37);
        }
        assert!(xms.free_block(relocation_target).is_ok());

        memory.mark_instr_exec_page(target_address as usize + OLD_SIZE_BYTES - 1);
        assert!(!memory.is_instr_cache_dirty());
        memory.enable_write_tracking();
        memory.set_write_context(0x1234, 0x5678);
        memory.set_hle_handler_active(true);
        assert!(xms.resize_block(a, NEW_SIZE_KB, &mut memory).is_ok());

        let (new_address, new_size_kb, lock_count) = xms.find_block(a).expect("A relocated");
        assert_eq!(new_address, target_address);
        assert_eq!(new_size_kb, NEW_SIZE_KB);
        assert_eq!(lock_count, 0);
        assert_eq!(xms.find_block(b), Some((b_address, b_size_kb, 0)));
        assert_eq!(
            xms.free_kb(),
            initial_free_kb + NEW_SIZE_KB - (NEW_SIZE_KB - OLD_SIZE_KB)
        );
        for offset in 0..OLD_SIZE_BYTES {
            assert_eq!(
                memory.ram[new_address as usize + offset],
                (offset as u8).wrapping_mul(37),
                "relocated byte {offset:#x}"
            );
        }
        assert_eq!(
            memory.pending_write_count(),
            1,
            "a bulk relocation must retain one pending range record"
        );
        let pending = memory.drain_pending_writes();
        let write = &pending[0];
        assert_eq!(write.source_cs, 0x1234);
        assert_eq!(write.source_ip, 0x5678);
        assert_eq!(write.linear_address, new_address);
        assert_eq!(write.size, 8);
        assert_eq!(write.affected_len, OLD_SIZE_BYTES as u32);
        assert_eq!(write.old_value, 0);
        assert_eq!(
            write.new_value,
            u64::from_le_bytes([0, 37, 74, 111, 148, 185, 222, 3])
        );
        assert!(write.is_hle_handler);
        assert!(
            memory.is_instr_cache_dirty(),
            "the bulk destination range must invalidate interpreter code"
        );
    }

    #[test]
    fn test_xms_new() {
        let xms = Xms::new();
        assert!(xms.total_kb() > 0);
        assert_eq!(xms.free_kb(), xms.total_kb());
        assert!(!xms.hma_allocated());
        assert_eq!(xms.a20_local_count(), 0);
        assert_eq!(xms.block_count(), 0);
    }

    #[test]
    fn test_xms_default() {
        let xms = Xms::default();
        assert!(xms.total_kb() > 0);
    }

    #[test]
    fn test_xms_hma() {
        let mut xms = Xms::new();
        assert!(!xms.hma_allocated());
        assert_eq!(xms.release_hma(), Err(XMS_HMA_NOT_ALLOCATED));

        assert_eq!(xms.request_hma(), Ok(()));
        assert!(xms.hma_allocated());
        assert_eq!(xms.request_hma(), Err(XMS_HMA_IN_USE));

        assert_eq!(xms.release_hma(), Ok(()));
        assert!(!xms.hma_allocated());
    }

    /// MDD-BUG-CRU-01034: an owned HMA is an occupied range. A block
    /// allocated afterwards lands above it, and the free and largest-free
    /// counts leave out its 64 KiB.
    #[test]
    fn a_block_allocated_after_the_hma_does_not_overlap_it() {
        let mut xms = Xms::new();
        let total = xms.total_kb();
        assert_eq!(xms.request_hma(), Ok(()));
        assert_eq!(xms.free_kb(), total - HMA_KB);
        assert_eq!(xms.largest_free_kb(), total - HMA_KB);

        let handle = xms.allocate_block(64).expect("block after the HMA");
        let (address, _, _) = xms.find_block(handle).expect("block exists");
        assert_eq!(address, HMA_END, "the block must start above the HMA");
        assert_eq!(xms.free_kb(), total - HMA_KB - 64);
        assert_eq!(xms.largest_free_kb(), total - HMA_KB - 64);
    }

    /// MDD-BUG-CRU-01034: the reverse order. A block already holding the
    /// HMA's bytes keeps them, and the HMA request fails as in use.
    #[test]
    fn the_hma_is_refused_while_a_block_holds_its_bytes() {
        let mut xms = Xms::new();
        let total = xms.total_kb();
        let handle = xms.allocate_block(4).expect("block at 1 MiB");
        assert_eq!(xms.find_block(handle).map(|b| b.0), Some(XMS_BASE));

        assert_eq!(xms.request_hma(), Err(XMS_HMA_IN_USE));
        assert!(!xms.hma_allocated());
        assert_eq!(xms.free_kb(), total - 4);

        assert_eq!(xms.free_block(handle), Ok(()));
        assert_eq!(xms.request_hma(), Ok(()));
    }

    /// MDD-BUG-CRU-01034: releasing the HMA returns its 64 KiB to the pool,
    /// and the next block can start at 1 MiB again.
    #[test]
    fn a_released_hma_returns_to_the_pool() {
        let mut xms = Xms::new();
        let total = xms.total_kb();
        assert_eq!(xms.request_hma(), Ok(()));
        assert_eq!(xms.release_hma(), Ok(()));
        assert_eq!(xms.free_kb(), total);
        assert_eq!(xms.largest_free_kb(), total);

        let handle = xms.allocate_block(64).expect("block at 1 MiB");
        assert_eq!(xms.find_block(handle).map(|b| b.0), Some(XMS_BASE));
    }

    #[test]
    fn test_xms_a20_local_count() {
        let mut xms = Xms::new();

        xms.increment_a20_local_count();
        assert_eq!(xms.a20_local_count(), 1);

        xms.increment_a20_local_count();
        assert_eq!(xms.a20_local_count(), 2);

        xms.decrement_a20_local_count();
        assert_eq!(xms.a20_local_count(), 1);

        xms.decrement_a20_local_count();
        assert_eq!(xms.a20_local_count(), 0);

        // Decrementing below 0 should saturate
        xms.decrement_a20_local_count();
        assert_eq!(xms.a20_local_count(), 0);
    }

    #[test]
    fn test_xms_allocate_block() {
        let mut xms = Xms::new();
        let initial_free = xms.free_kb();

        let handle = xms.allocate_block(1024).unwrap(); // 1MB
        assert!(handle > 0);
        assert_eq!(xms.block_count(), 1);
        assert_eq!(xms.free_kb(), initial_free - 1024);

        // Allocate another block
        let handle2 = xms.allocate_block(512).unwrap(); // 512KB
        assert!(handle2 > 0);
        assert_ne!(handle, handle2);
        assert_eq!(xms.block_count(), 2);
    }

    #[test]
    fn test_xms_allocate_too_large() {
        let mut xms = Xms::new();
        let total = xms.total_kb();

        // Try to allocate more than available
        let result = xms.allocate_block(total + 1);
        assert_eq!(result, Err(XmsError::OutOfMemory));
    }

    #[test]
    fn test_xms_free_block() {
        let mut xms = Xms::new();
        let initial_free = xms.free_kb();

        let handle = xms.allocate_block(1024).unwrap();
        assert_eq!(xms.free_kb(), initial_free - 1024);

        let freed = xms.free_block(handle);
        assert!(freed.is_ok());
        assert_eq!(xms.free_kb(), initial_free);
        assert_eq!(xms.block_count(), 0);
    }

    #[test]
    fn test_xms_free_invalid_handle() {
        let mut xms = Xms::new();
        let freed = xms.free_block(9999);
        assert_eq!(freed, Err(XmsError::InvalidHandle));
    }

    #[test]
    fn free_locked_block_preserves_the_block() {
        let mut xms = Xms::new();
        let handle = xms.allocate_block(64).expect("allocation should succeed");
        xms.lock_block(handle).expect("lock should succeed");
        let free_before = xms.free_kb();
        let block_before = xms.find_block(handle);

        assert_eq!(xms.free_block(handle), Err(XmsError::BlockLocked));
        assert_eq!(xms.free_kb(), free_before);
        assert_eq!(xms.find_block(handle), block_before);
    }

    #[test]
    fn test_xms_find_block() {
        let mut xms = Xms::new();
        let handle = xms.allocate_block(256).unwrap();

        let info = xms.find_block(handle);
        assert!(info.is_some());
        let (addr, size_kb, lock_count) = info.unwrap();
        assert!(addr >= XMS_BASE);
        assert_eq!(size_kb, 256);
        assert_eq!(lock_count, 0);

        // Non-existent handle
        assert!(xms.find_block(9999).is_none());
    }

    #[test]
    fn test_xms_lock_unlock() {
        let mut xms = Xms::new();
        let handle = xms.allocate_block(128).unwrap();

        // Lock
        let addr = xms.lock_block(handle);
        assert!(addr.is_ok());

        let info = xms.find_block(handle).unwrap();
        assert_eq!(info.2, 1); // lock_count = 1

        // Lock again (nested)
        let addr2 = xms.lock_block(handle);
        assert!(addr2.is_ok());
        assert_eq!(addr, addr2);

        let info = xms.find_block(handle).unwrap();
        assert_eq!(info.2, 2); // lock_count = 2

        // Unlock
        assert!(xms.unlock_block(handle).is_ok());
        let info = xms.find_block(handle).unwrap();
        assert_eq!(info.2, 1);

        assert!(xms.unlock_block(handle).is_ok());
        let info = xms.find_block(handle).unwrap();
        assert_eq!(info.2, 0);

        assert_eq!(xms.unlock_block(handle), Err(XmsError::BlockNotLocked));
    }

    #[test]
    fn test_xms_unlock_invalid_handle() {
        let mut xms = Xms::new();
        assert_eq!(xms.unlock_block(9999), Err(XmsError::InvalidHandle));
    }

    #[test]
    fn test_xms_lock_invalid_handle() {
        let mut xms = Xms::new();
        assert_eq!(xms.lock_block(9999), Err(XmsError::InvalidHandle));
    }

    #[test]
    fn test_xms_resize_block_shrink() {
        let mut xms = Xms::new();
        let mut memory = Memory::new();
        let initial_free = xms.free_kb();
        let handle = xms.allocate_block(1024).unwrap();

        // Shrink
        assert!(xms.resize_block(handle, 512, &mut memory).is_ok());

        let info = xms.find_block(handle).unwrap();
        assert_eq!(info.1, 512); // size_kb = 512
        assert_eq!(xms.free_kb(), initial_free - 512);
    }

    #[test]
    fn test_xms_resize_block_grow() {
        let mut xms = Xms::new();
        let mut memory = Memory::new();
        let handle = xms.allocate_block(512).unwrap();

        // Grow
        assert!(xms.resize_block(handle, 1024, &mut memory).is_ok());

        let info = xms.find_block(handle).unwrap();
        assert_eq!(info.1, 1024);
    }

    #[test]
    fn test_xms_resize_block_same_size() {
        let mut xms = Xms::new();
        let mut memory = Memory::new();
        let handle = xms.allocate_block(512).unwrap();

        // Resize to same size
        assert!(xms.resize_block(handle, 512, &mut memory).is_ok());

        let info = xms.find_block(handle).unwrap();
        assert_eq!(info.1, 512);
    }

    #[test]
    fn test_xms_resize_locked_block() {
        let mut xms = Xms::new();
        let mut memory = Memory::new();
        let handle = xms.allocate_block(512).unwrap();
        xms.lock_block(handle).expect("lock should succeed");

        assert_eq!(
            xms.resize_block(handle, 1024, &mut memory),
            Err(XmsError::BlockLocked)
        );
    }

    #[test]
    fn test_xms_resize_invalid_handle() {
        let mut xms = Xms::new();
        let mut memory = Memory::new();
        assert_eq!(
            xms.resize_block(9999, 512, &mut memory),
            Err(XmsError::InvalidHandle)
        );
    }

    #[test]
    fn test_xms_resize_too_large() {
        let mut xms = Xms::new();
        let mut memory = Memory::new();
        let handle = xms.allocate_block(512).unwrap();
        let total = xms.total_kb();

        // Try to grow beyond available
        assert_eq!(
            xms.resize_block(handle, total + 1, &mut memory),
            Err(XmsError::OutOfMemory)
        );
    }

    #[test]
    fn test_xms_get_block_info() {
        let mut xms = Xms::new();
        let handle = xms.allocate_block(256).unwrap();

        let info = xms.get_block_info(handle);
        assert!(info.is_some());
        let (size_kb, lock_count) = info.unwrap();
        assert_eq!(size_kb, 256);
        assert_eq!(lock_count, 0);

        // Lock and check again
        xms.lock_block(handle).expect("lock should succeed");
        let (_, lock_count) = xms.get_block_info(handle).unwrap();
        assert_eq!(lock_count, 1);

        // Invalid handle
        assert!(xms.get_block_info(9999).is_none());
    }

    #[test]
    fn test_xms_largest_free_kb() {
        let mut xms = Xms::new();
        let total = xms.total_kb();

        // Initially, largest free = total
        assert_eq!(xms.largest_free_kb(), total);

        // Allocate a block
        let _h1 = xms.allocate_block(1024).unwrap();
        assert!(xms.largest_free_kb() < total);
    }

    #[test]
    fn test_xms_largest_free_kb_with_leading_gap() {
        let mut xms = Xms::new();
        xms.blocks.clear();
        xms.blocks.push(XmsBlock {
            handle: 1,
            size_kb: 64,
            lock_count: 0,
            address: XMS_BASE + (128 * 1024),
        });

        // Largest free region should account for the gap from XMS_BASE to first block.
        assert!(xms.largest_free_kb() >= 128);
    }

    #[test]
    fn test_xms_handle_reuse() {
        let mut xms = Xms::new();

        // Allocate and free a block
        let handle1 = xms.allocate_block(128).unwrap();
        xms.free_block(handle1).expect("free should succeed");

        // Allocate another - should reuse handle
        let handle2 = xms.allocate_block(256).unwrap();
        assert_eq!(handle1, handle2);
    }

    #[test]
    fn test_xms_multiple_allocations() {
        let mut xms = Xms::new();

        // Allocate multiple blocks
        let handles: Vec<u16> = (0..10)
            .filter_map(|_| xms.allocate_block(64).ok())
            .collect();

        assert_eq!(handles.len(), 10);
        assert_eq!(xms.block_count(), 10);

        // Free all
        for h in handles {
            assert!(xms.free_block(h).is_ok());
        }
        assert_eq!(xms.block_count(), 0);
    }

    #[test]
    fn test_xms_fragmentation() {
        let mut xms = Xms::new();

        // Allocate 3 blocks
        let h1 = xms.allocate_block(1024).unwrap();
        let h2 = xms.allocate_block(1024).unwrap();
        let h3 = xms.allocate_block(1024).unwrap();

        // Free the middle one
        xms.free_block(h2).expect("free should succeed");

        // Allocate a block that fits in the gap
        let h4 = xms.allocate_block(512).unwrap();
        assert!(h4 > 0);

        // Clean up
        xms.free_block(h1).expect("free should succeed");
        xms.free_block(h3).expect("free should succeed");
        xms.free_block(h4).expect("free should succeed");
    }

    #[test]
    fn test_xms_lock_overflow() {
        let mut xms = Xms::new();
        let handle = xms.allocate_block(64).unwrap();

        // Lock 255 times (max)
        for _ in 0..255 {
            assert!(xms.lock_block(handle).is_ok());
        }

        assert_eq!(xms.lock_block(handle), Err(XmsError::LockCountOverflow));
    }

    #[test]
    fn test_xms_allocate_handle_returns_none_at_wrap() {
        let mut xms = Xms::new();
        xms.next_handle = 0;
        xms.free_handles.clear();

        assert!(xms.allocate_handle().is_none());
    }

    #[test]
    fn test_xms_find_free_range_returns_none_when_full() {
        let mut xms = Xms::new();
        xms.blocks.push(XmsBlock {
            handle: 1,
            size_kb: xms.total_kb,
            lock_count: 0,
            address: XMS_BASE,
        });

        assert!(xms.find_free_range(1024, None).is_none());
    }

    #[test]
    fn test_xms_find_free_range_advances_cur_and_handles_overlap() {
        let mut xms = Xms::new();
        xms.blocks.clear();
        xms.blocks.push(XmsBlock {
            handle: 1,
            size_kb: 128,
            lock_count: 0,
            address: XMS_BASE,
        });
        // Overlaps inside the first block so `end > cur` is false on second range.
        xms.blocks.push(XmsBlock {
            handle: 2,
            size_kb: 32,
            lock_count: 0,
            address: XMS_BASE + (64 * 1024),
        });

        let addr = xms.find_free_range(16 * 1024, None).unwrap();
        assert_eq!(addr, XMS_BASE + (128 * 1024));
    }

    #[test]
    fn test_xms_find_free_range_rejects_gap_after_dos_ram_ceiling() {
        let mut xms = Xms::new();
        let first_size_kb = xms.total_kb + 64;
        let first_end = XMS_BASE + first_size_kb * 1024;
        let second_start = first_end + (64 * 1024);
        xms.blocks.clear();
        xms.blocks.push(XmsBlock {
            handle: 1,
            size_kb: first_size_kb,
            lock_count: 0,
            address: XMS_BASE,
        });
        xms.blocks.push(XmsBlock {
            handle: 2,
            size_kb: 64,
            lock_count: 0,
            address: second_start,
        });

        assert_eq!(xms.find_free_range(1024, None), None);
    }

    #[test]
    fn test_xms_allocate_block_reports_out_of_memory_without_contiguous_range() {
        let mut xms = Xms::new();
        xms.free_kb = 64;
        xms.blocks.push(XmsBlock {
            handle: 1,
            size_kb: xms.total_kb,
            lock_count: 0,
            address: XMS_BASE,
        });

        assert_eq!(xms.allocate_block(1), Err(XmsError::OutOfMemory));
        assert_eq!(xms.block_count(), 1);
    }

    #[test]
    fn test_xms_allocate_block_reports_handle_exhaustion_at_handle_wrap() {
        let mut xms = Xms::new();
        xms.next_handle = 0;
        xms.free_handles.clear();

        assert_eq!(xms.allocate_block(1), Err(XmsError::HandlesExhausted));
        assert_eq!(xms.block_count(), 0);
    }

    #[test]
    fn test_xms_resize_growth_fails_when_no_contiguous_range_exists() {
        let mut xms = Xms::new();
        let mut memory = Memory::new();
        let total_kb = xms.total_kb();

        // Create a fragmented layout with enough total free KB but no contiguous
        // range large enough for the requested new block size.
        xms.blocks.clear();
        xms.blocks.push(XmsBlock {
            handle: 1,
            size_kb: 900,
            lock_count: 0,
            address: XMS_BASE,
        });
        xms.blocks.push(XmsBlock {
            handle: 2,
            size_kb: total_kb - 950,
            lock_count: 0,
            address: XMS_BASE + (950 * 1024),
        });
        xms.free_kb = 100;

        assert_eq!(
            xms.resize_block(1, 1000, &mut memory),
            Err(XmsError::OutOfMemory)
        );
        assert_eq!(xms.get_block_info(1).unwrap().0, 900);
    }

    #[test]
    fn test_xms_largest_free_kb_false_branch_paths() {
        let mut xms = Xms::new();
        let end = DOS_RAM_CEILING as u32;
        xms.blocks.clear();
        xms.blocks.push(XmsBlock {
            handle: 1,
            size_kb: 64,
            lock_count: 0,
            address: XMS_BASE + (64 * 1024),
        });
        xms.blocks.push(XmsBlock {
            handle: 2,
            size_kb: 64,
            lock_count: 0,
            address: end - (96 * 1024),
        });
        xms.blocks.push(XmsBlock {
            handle: 3,
            size_kb: 8,
            lock_count: 0,
            address: end - (24 * 1024),
        });
        // Overlap with block 3 so `end > cur` can evaluate false.
        xms.blocks.push(XmsBlock {
            handle: 4,
            size_kb: 2,
            lock_count: 0,
            address: end - (20 * 1024),
        });

        assert!(xms.largest_free_kb() > 0);
    }

    #[test]
    fn the_lfb_aperture_sits_outside_the_extended_memory_pool() {
        let aperture_start = crate::LFB_BASE as u32;
        let aperture_end = aperture_start + crate::LFB_MAX_SIZE as u32;
        assert!(
            aperture_start >= DOS_RAM_CEILING as u32,
            "aperture {aperture_start:#X}..{aperture_end:#X} overlaps the XMS pool \
             {XMS_BASE:#X}..{:#X}",
            DOS_RAM_CEILING as u32
        );
    }

    /// Drain the XMS pool and assert each allocated block stays below the DOS
    /// RAM ceiling, that the pool really was drained, and that no two blocks
    /// share an address. Without the last two, an allocator that allocates
    /// nothing, or stacks every block at XMS_BASE, passed (MDD-BUG-CRU-01490).
    #[test]
    fn xms_allocations_stay_below_dos_ram_ceiling() {
        let mut xms = Xms::new();
        let mut placed = Vec::new();
        let mut place = |xms: &mut Xms, handle| {
            let (address, kb, _) = xms.find_block(handle).expect("just allocated");
            let end = address + kb * 1024;
            assert!(
                end <= DOS_RAM_CEILING as u32,
                "block {address:#X}..{end:#X} exceeds DOS_RAM_CEILING"
            );
            placed.push((address, end));
        };
        for size_kb in [4096, 1024] {
            while let Ok(handle) = xms.allocate_block(size_kb) {
                place(&mut xms, handle);
            }
        }
        let rest = xms.free_kb();
        if rest > 0 {
            let handle = xms
                .allocate_block(rest)
                .expect("the remainder fits one block");
            place(&mut xms, handle);
        }

        assert_eq!(xms.free_kb(), 0, "the pool must actually drain");
        assert_eq!(xms.largest_free_kb(), 0, "no free range may remain");
        placed.sort_unstable();
        for pair in placed.windows(2) {
            assert!(
                pair[0].1 <= pair[1].0,
                "blocks {:#X}..{:#X} and {:#X}..{:#X} overlap",
                pair[0].0,
                pair[0].1,
                pair[1].0,
                pair[1].1
            );
        }
    }
}
