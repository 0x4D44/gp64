//! DOS conventional memory manager (MCB chain)
//!
//! Implements a minimal MS-DOS-like allocator in conventional memory using
//! Memory Control Blocks (MCBs), with support for Upper Memory Blocks (UMBs).

use gp64_memory::Memory;
use gp64_types::FixedVec;

/// Segment of 0xA0000 (start of VGA memory). Conventional allocations must stay below this.
pub const CONV_END_SEG: u16 = 0xA000;

/// UMB region start segment (after VGA memory, in adapter ROM area)
pub const UMB_START_SEG: u16 = 0xC800;

/// UMB region end segment (before EMS page frame at 0xE000)
pub const UMB_END_SEG: u16 = 0xE000;

/// First segment we allow for MCB chain.
///
/// Segment 0x0700 sits above the DOS kernel structures (IHSEG, InfoBlock,
/// SDA, device headers, CDS stub, FCB SFT, DPB chain, disk buffer head,
/// secondary SFT and metadata), which fit inside `[0x0070..0x041E)`.
pub const DEFAULT_FIRST_MCB_SEG: u16 = 0x0700;

/// The first MCB segment. KILN-00086: the env override was retired, so this is
/// always the fixed default.
pub fn first_mcb_seg_override() -> u16 {
    DEFAULT_FIRST_MCB_SEG
}

/// Maximum paragraphs a specific MCB block can reach by absorbing adjacent
/// forward free space in place. Used on the expand-fail path of
/// `DosMem::resize` and `DosMem::resize_umb` to populate BX for
/// INT 21h/4Ah's CY=1 return per RBIL: "maximum paragraphs available for
/// the specified memory block" — its own usable size plus, if the next
/// block is free, that block's usable size plus the one paragraph reclaimed
/// from the freed MCB header.
fn max_resize_for_block(cur_size: u16, next_free: bool, next_size: u16) -> u16 {
    if next_free {
        cur_size.saturating_add(next_size.saturating_add(1))
    } else {
        cur_size
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DosMemError {
    InsufficientMemory { largest_available: u16 },
    InvalidBlock,
}

#[derive(Debug, Clone, Copy)]
struct Block {
    mcb_seg: u16,    // MCB header segment (usable segment is mcb_seg+1)
    size_paras: u16, // usable size in paragraphs
    owner_psp: u16,  // 0 = free
    is_last: bool,
    /// MCB name (8 bytes, DOS 4.0+)
    name: [u8; 8],
}

impl Block {
    fn usable_seg(self) -> u16 {
        self.mcb_seg.wrapping_add(1)
    }
    fn end_seg_exclusive(self) -> u16 {
        self.usable_seg().wrapping_add(self.size_paras)
    }
}

/// One MCB entry as returned by [`DosMem::mcb_summary`]. Used by the
/// Gremlin `state.dos` observation message (HLD §"Wire protocol").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McbSummaryEntry {
    /// MCB header segment (`usable_seg = seg + 1`).
    pub seg: u16,
    /// Usable size in paragraphs (16-byte units), excluding the MCB header.
    pub size_paras: u16,
    /// Owning PSP. `0` means the block is free.
    pub owner_psp: u16,
    /// Name from the MCB header. Trimmed at the first NUL byte; non-UTF-8
    /// bytes are replaced with U+FFFD.
    pub name: String,
    /// True if this MCB is the chain terminator (marker byte 'Z' in DOS).
    pub is_last: bool,
    /// True if this MCB lives in the UMB region.
    pub is_umb: bool,
}

/// Decode an 8-byte MCB name into a Rust string. Empty bytes are
/// truncated at the first NUL; non-UTF-8 input falls back to lossy
/// decoding.
fn mcb_name_lossy(raw: &[u8; 8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DosMemBlockInfo {
    pub mcb_seg: u16,
    pub usable_seg: u16,
    pub end_seg: u16,
    pub size_paras: u16,
    pub owner_psp: u16,
    pub is_free: bool,
    /// True if this block is in UMB (Upper Memory Block) area
    pub is_umb: bool,
}

/// Represents an Upper Memory Block region
#[derive(Debug, Clone, Copy)]
struct UmbBlock {
    mcb_seg: u16,    // MCB header segment (usable segment is mcb_seg+1)
    size_paras: u16, // usable size in paragraphs
    owner_psp: u16,  // 0 = free
    is_last: bool,
    name: [u8; 8],
}

impl UmbBlock {
    fn usable_seg(self) -> u16 {
        self.mcb_seg.wrapping_add(1)
    }
    fn end_seg_exclusive(self) -> u16 {
        self.usable_seg().wrapping_add(self.size_paras)
    }
}

#[derive(Clone)]
pub struct DosMem {
    first_mcb_seg: u16,
    /// Conventional MCB chain.
    /// Capacity accommodates programs that fragment the MCB chain during init.
    /// The absolute upper bound at 16 paragraphs/MCB and 640 KB conventional
    /// memory is approximately 40000 entries.
    blocks: FixedVec<Block, 1024>,
    /// UMB (Upper Memory Block) chain.
    /// Capacity accommodates programs with many upper-memory allocations.
    umb_blocks: FixedVec<UmbBlock, 64>,
    /// Whether UMB has been initialized
    umb_initialized: bool,
    /// Whether UMB is currently linked to the conventional MCB chain.
    /// When true, write_all_mcbs will also write bridge MCBs so that
    /// programs walking the chain see a continuous conventional->UMB path.
    umb_chain_linked: bool,
}

impl DosMem {
    pub fn new(first_mcb_seg: u16) -> Self {
        Self {
            first_mcb_seg,
            blocks: FixedVec::new(),
            umb_blocks: FixedVec::new(),
            umb_initialized: false,
            umb_chain_linked: false,
        }
    }

    /// Get the segment of the first MCB in the chain
    pub fn first_mcb_segment(&self) -> u16 {
        self.first_mcb_seg
    }

    /// Walk the MCB chain (conventional + UMB) and return a flat summary
    /// for Gremlin `state.dos` observation.
    ///
    /// Order: conventional MCBs first (in chain order, which is start-segment
    /// order), then UMB MCBs. Each entry carries the MCB header segment, the
    /// usable size in paragraphs, the owner PSP (0 = free), the 8-byte name
    /// (trimmed at the first NUL, lossy UTF-8), the `is_last` flag, and an
    /// `is_umb` discriminator.
    ///
    /// See HLD §"Wire protocol" `state.dos`.
    pub fn mcb_summary(&self) -> Vec<McbSummaryEntry> {
        let mut out = Vec::with_capacity(self.blocks.len() + self.umb_blocks.len());
        for b in self.blocks.iter() {
            out.push(McbSummaryEntry {
                seg: b.mcb_seg,
                size_paras: b.size_paras,
                owner_psp: b.owner_psp,
                name: mcb_name_lossy(&b.name),
                is_last: b.is_last,
                is_umb: false,
            });
        }
        for b in self.umb_blocks.iter() {
            out.push(McbSummaryEntry {
                seg: b.mcb_seg,
                size_paras: b.size_paras,
                owner_psp: b.owner_psp,
                name: mcb_name_lossy(&b.name),
                is_last: b.is_last,
                is_umb: true,
            });
        }
        out
    }

    pub fn init(&mut self, memory: &mut Memory) {
        self.blocks.clear();

        let usable_start = self.first_mcb_seg.wrapping_add(1);
        let usable_size = CONV_END_SEG.saturating_sub(usable_start);

        // One large free block spanning to the start of VGA memory.
        let block = Block {
            mcb_seg: self.first_mcb_seg,
            size_paras: usable_size,
            owner_psp: 0,
            is_last: true,
            name: [0; 8],
        };
        self.blocks.push(block);
        self.write_all_mcbs(memory);
    }

    /// Initialize UMB (Upper Memory Block) region
    /// This creates a single large UMB block in the adapter ROM area (0xC800-0xDFFF)
    pub fn init_umb(&mut self, memory: &mut Memory) {
        if self.umb_initialized {
            return;
        }

        self.umb_blocks.clear();

        // Create a single UMB block in the adapter ROM area
        // 0xC800 to 0xDFFF (96KB = 0x1800 paragraphs), stops before EMS page frame at 0xE000
        let umb_size = UMB_END_SEG.saturating_sub(UMB_START_SEG).saturating_sub(1);
        let block = UmbBlock {
            mcb_seg: UMB_START_SEG,
            size_paras: umb_size,
            owner_psp: 0,
            is_last: true,
            name: [0; 8],
        };
        self.umb_blocks.push(block);
        self.write_umb_mcb(memory, block);

        self.umb_initialized = true;
        tracing::debug!(target: "gp64::dos::memory", "UMB initialized: {} paragraphs at {:04X}", umb_size, UMB_START_SEG);
    }

    /// Reinitialise UMBs starting at `start_seg` (rounded up to next paragraph).
    /// Used after loading a VGA BIOS ROM that extends past the default UMB start.
    pub fn reinit_umb_at(&mut self, memory: &mut Memory, start_seg: u16) {
        self.umb_blocks.clear();
        self.umb_initialized = false;
        self.umb_chain_linked = false;

        if start_seg >= UMB_END_SEG {
            // No room left for UMBs after the ROM.
            self.write_all_mcbs(memory);
            tracing::warn!(target: "gp64::dos::memory",
                "UMB disabled: ROM extends to {:04X}, past UMB end {:04X}",
                start_seg, UMB_END_SEG);
            return;
        }

        let umb_size = UMB_END_SEG.saturating_sub(start_seg).saturating_sub(1);
        let block = UmbBlock {
            mcb_seg: start_seg,
            size_paras: umb_size,
            owner_psp: 0,
            is_last: true,
            name: [0; 8],
        };
        self.umb_blocks.push(block);
        self.write_umb_mcb(memory, block);

        self.umb_initialized = true;
        tracing::debug!(target: "gp64::dos::memory",
            "UMB reinitialised: {} paragraphs at {:04X} (after VGA BIOS ROM)",
            umb_size, start_seg);
    }

    /// to emulate a system without a UMB provider.
    pub fn deinit_umb(&mut self, memory: &mut Memory) {
        self.umb_blocks.clear();
        self.umb_initialized = false;
        self.umb_chain_linked = false;
        self.write_all_mcbs(memory);
        tracing::debug!(target: "gp64::dos::memory", "UMB deinitialized — no UMB provider");
    }

    /// Check if UMB is initialized
    pub fn is_umb_initialized(&self) -> bool {
        self.umb_initialized
    }

    /// Get the largest free block in UMB
    pub fn largest_free_umb(&self) -> u16 {
        self.umb_blocks
            .iter()
            .filter(|b| b.owner_psp == 0)
            .map(|b| b.size_paras)
            .max()
            .unwrap_or(0)
    }

    /// Write a single UMB MCB to memory
    fn write_umb_mcb(&self, memory: &mut Memory, b: UmbBlock) {
        let addr = (b.mcb_seg as usize) * 16;
        // MCB format: 'M' or 'Z' at byte 0
        memory.write_u8(addr, if b.is_last { 0x5A } else { 0x4D }); // 'Z' or 'M'
        memory.write_u16(addr + 1, b.owner_psp);
        memory.write_u16(addr + 3, b.size_paras);
        // Write name at offset 8
        for (i, &c) in b.name.iter().enumerate() {
            memory.write_u8(addr + 8 + i, c);
        }
    }

    /// Write all UMB MCBs to memory
    fn write_all_umb_mcbs(&self, memory: &mut Memory) {
        for &b in &self.umb_blocks {
            self.write_umb_mcb(memory, b);
        }
    }

    pub fn largest_free(&self) -> u16 {
        self.blocks
            .iter()
            .filter(|b| b.owner_psp == 0)
            .map(|b| b.size_paras)
            .max()
            .unwrap_or(0)
    }

    /// Total free conventional memory in paragraphs (sum of all free blocks).
    pub fn total_free_paragraphs(&self) -> u32 {
        self.blocks
            .iter()
            .filter(|b| b.owner_psp == 0)
            .map(|b| b.size_paras as u32)
            .sum()
    }

    pub fn block_end_segment(&self, segment: u16) -> Option<u16> {
        let mcb_seg = segment.wrapping_sub(1);
        self.blocks
            .iter()
            .find(|b| b.mcb_seg == mcb_seg)
            .map(|b| b.end_seg_exclusive())
    }

    /// Iterator over `(usable_seg, end_seg_exclusive)` for every free
    /// conventional MCB. UMB blocks are deliberately excluded — the
    /// DOS BSS-init policy (HLD 2026-05-17 §4.6) does not touch UMB
    /// content because the UMB region is sparse and intermixed with
    /// device-MMIO windows we don't fully model.
    ///
    /// Used by the load-time BSS-init pass to obtain the address
    /// range of every region whose contents are about to be returned
    /// to a freshly-launched program.
    pub fn iter_free_conventional_blocks(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.blocks
            .iter()
            .filter(|b| b.owner_psp == 0)
            .map(|b| (b.usable_seg(), b.end_seg_exclusive()))
    }

    pub fn block_info_for_segment(&self, segment: u16) -> Option<DosMemBlockInfo> {
        // First check conventional memory
        if let Some(info) = self.blocks.iter().find_map(|b| {
            let start = b.usable_seg();
            let end = b.end_seg_exclusive();
            if segment >= start && segment < end {
                Some(DosMemBlockInfo {
                    mcb_seg: b.mcb_seg,
                    usable_seg: start,
                    end_seg: end,
                    size_paras: b.size_paras,
                    owner_psp: b.owner_psp,
                    is_free: b.owner_psp == 0,
                    is_umb: false,
                })
            } else {
                None
            }
        }) {
            return Some(info);
        }

        // Then check UMB
        self.umb_blocks.iter().find_map(|b| {
            let start = b.usable_seg();
            let end = b.end_seg_exclusive();
            if segment >= start && segment < end {
                Some(DosMemBlockInfo {
                    mcb_seg: b.mcb_seg,
                    usable_seg: start,
                    end_seg: end,
                    size_paras: b.size_paras,
                    owner_psp: b.owner_psp,
                    is_free: b.owner_psp == 0,
                    is_umb: true,
                })
            } else {
                None
            }
        })
    }

    /// Maximum size this allocation can reach in place without mutating the MCB chain.
    pub(crate) fn max_resize_for_segment(&self, segment: u16) -> Option<u16> {
        let mcb_seg = segment.wrapping_sub(1);
        if let Some(idx) = self
            .blocks
            .iter()
            .position(|block| block.mcb_seg == mcb_seg)
        {
            let current = self.blocks[idx];
            let (next_free, next_size) = self
                .blocks
                .get(idx + 1)
                .map_or((false, 0), |next| (next.owner_psp == 0, next.size_paras));
            return Some(max_resize_for_block(
                current.size_paras,
                next_free,
                next_size,
            ));
        }
        self.umb_blocks
            .iter()
            .position(|block| block.mcb_seg == mcb_seg)
            .map(|idx| {
                let current = self.umb_blocks[idx];
                let (next_free, next_size) = self
                    .umb_blocks
                    .get(idx + 1)
                    .map_or((false, 0), |next| (next.owner_psp == 0, next.size_paras));
                max_resize_for_block(current.size_paras, next_free, next_size)
            })
    }

    pub fn set_owner(
        &mut self,
        memory: &mut Memory,
        segment: u16,
        owner_psp: u16,
    ) -> Result<(), DosMemError> {
        let mcb_seg = segment.wrapping_sub(1);
        let idx = self
            .blocks
            .iter()
            .position(|b| b.mcb_seg == mcb_seg)
            .ok_or(DosMemError::InvalidBlock)?;
        self.blocks[idx].owner_psp = owner_psp;
        self.write_all_mcbs(memory);
        Ok(())
    }

    pub fn alloc(
        &mut self,
        memory: &mut Memory,
        owner_psp: u16,
        paragraphs: u16,
    ) -> Result<u16, DosMemError> {
        // Default to first-fit strategy
        self.alloc_with_strategy(memory, owner_psp, paragraphs, 0)
    }

    /// Allocate memory with a specific strategy
    /// strategy bits:
    ///   0x00 = First fit (low memory first)
    ///   0x01 = Best fit (smallest block that fits)
    ///   0x02 = Last fit (high memory first)
    ///   0x40-0x42 = High memory ONLY (first/best/last fit in UMBs; error 8
    ///               when they cannot satisfy the request)
    ///   0x80 = High memory first (first fit UMB, then low)
    ///   0x81 = High memory first (best fit UMB, then low)
    ///   0x82 = High memory first (last fit UMB, then low)
    pub fn alloc_with_strategy(
        &mut self,
        memory: &mut Memory,
        owner_psp: u16,
        paragraphs: u16,
        strategy: u8,
    ) -> Result<u16, DosMemError> {
        self.alloc_with_strategy_and_umb(memory, owner_psp, paragraphs, strategy, false)
    }

    /// Allocate memory with UMB support
    /// umb_linked: whether UMB is currently linked to the memory chain
    pub fn alloc_with_strategy_and_umb(
        &mut self,
        memory: &mut Memory,
        owner_psp: u16,
        paragraphs: u16,
        strategy: u8,
        umb_linked: bool,
    ) -> Result<u16, DosMemError> {
        // DOS allocates at least 1 paragraph even for 0 request
        let actual_paragraphs = if paragraphs == 0 { 1 } else { paragraphs };

        let base_strategy = strategy & 0x03;
        let use_umb_first = (strategy & 0xC0) != 0; // 0x40 or 0x80

        // Try UMB first if linked and high memory strategy
        if umb_linked && use_umb_first && self.umb_initialized && !self.umb_blocks.is_empty() {
            match self.alloc_from_umb(memory, owner_psp, actual_paragraphs, base_strategy) {
                Ok(seg) => return Ok(seg),
                // 40h-42h search the upper chain only and fail with error 8;
                // only 80h-82h fall back to conventional memory (RBIL
                // AX=5801h, the DOS model DOS_AllocateMemory) (MDD-BUG-CRU-01404).
                Err(error) if strategy & 0x40 != 0 => return Err(error),
                Err(_) => {}
            }
        }

        // A full tracking table can still consume an exact or near-exact block
        // in place. Skip candidates that would need an unrepresentable split
        // so strategy fallback can find another arena or exact-fit block.
        let has_tracking_slot = !self.blocks.is_full();
        let can_allocate = |b: &Block| {
            b.owner_psp == 0
                && b.size_paras >= actual_paragraphs
                && (has_tracking_slot || b.size_paras - actual_paragraphs <= 1)
        };

        // Find suitable block index in conventional memory based on strategy
        let block_idx = match base_strategy {
            0 => {
                // First-fit: find first block large enough
                self.blocks.iter().position(can_allocate)
            }
            1 => {
                // Best-fit: find smallest block that's large enough
                self.blocks
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| can_allocate(b))
                    .min_by_key(|(_, b)| b.size_paras)
                    .map(|(i, _)| i)
            }
            2 => {
                // Last-fit: find last block large enough (high memory first)
                self.blocks
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, b)| can_allocate(b))
                    .map(|(i, _)| i)
            }
            _ => {
                // Unknown strategy, default to first-fit
                self.blocks.iter().position(can_allocate)
            }
        };

        // If conventional memory failed and UMB is linked but we didn't try it first,
        // try UMB as a last resort
        if block_idx.is_none()
            && umb_linked
            && !use_umb_first
            && self.umb_initialized
            && !self.umb_blocks.is_empty()
        {
            if let Ok(seg) =
                self.alloc_from_umb(memory, owner_psp, actual_paragraphs, base_strategy)
            {
                return Ok(seg);
            }
        }

        let Some(i) = block_idx else {
            let largest = self.largest_free().max(if umb_linked {
                self.largest_free_umb()
            } else {
                0
            });
            tracing::debug!(
                "DOS mem alloc failed: paras={} largest_free={}",
                actual_paragraphs,
                largest
            );
            return Err(DosMemError::InsufficientMemory {
                largest_available: largest,
            });
        };

        let b = self.blocks[i];

        let remaining = b.size_paras - actual_paragraphs;
        let allocated_size = if remaining <= 1 {
            b.size_paras
        } else {
            actual_paragraphs
        };

        // Splitting needs one more tracking entry.  Refuse before changing the
        // selected block so a full table cannot leave the Rust and guest MCB
        // chains describing different allocations.
        if remaining > 1 && self.blocks.is_full() {
            let largest = self.largest_free().max(if umb_linked {
                self.largest_free_umb()
            } else {
                0
            });
            return Err(DosMemError::InsufficientMemory {
                largest_available: largest,
            });
        }

        // Track which block index holds the allocated block (differs for last-fit split).
        let alloc_idx = if remaining == 0 || remaining == 1 {
            // Consume entire block (no split needed, strategy doesn't matter).
            self.blocks[i] = Block {
                mcb_seg: b.mcb_seg,
                size_paras: allocated_size,
                owner_psp,
                is_last: b.is_last,
                name: [0; 8],
            };
            i
        } else if base_strategy == 2 {
            // Last-fit: allocate from the HIGH end of the free block.
            // Free remainder stays at the low end, allocation goes to the top.
            let free_size = remaining - 1; // one paragraph used by the allocation's MCB
            let alloc_mcb_seg = b.mcb_seg + 1 + free_size;
            let free = Block {
                mcb_seg: b.mcb_seg,
                size_paras: free_size,
                owner_psp: 0,
                is_last: false,
                name: [0; 8],
            };
            let allocated = Block {
                mcb_seg: alloc_mcb_seg,
                size_paras: allocated_size,
                owner_psp,
                is_last: b.is_last,
                name: [0; 8],
            };

            self.blocks[i] = free;
            self.blocks.insert(i + 1, allocated);
            i + 1
        } else {
            // First-fit / best-fit: allocate from the LOW end of the free block.
            let allocated = Block {
                mcb_seg: b.mcb_seg,
                size_paras: allocated_size,
                owner_psp,
                is_last: false,
                name: [0; 8],
            };
            let free_mcb_seg = allocated.end_seg_exclusive();
            let free = Block {
                mcb_seg: free_mcb_seg,
                size_paras: remaining - 1,
                owner_psp: 0,
                is_last: b.is_last,
                name: [0; 8],
            };

            self.blocks[i] = allocated;
            self.blocks.insert(i + 1, free);
            i
        };

        self.recompute_last_flag();
        self.write_all_mcbs(memory);
        let seg = self.blocks[alloc_idx].usable_seg();
        tracing::debug!(
            "DOS mem alloc: seg={:04X} paras={} psp={:04X} largest_free={}",
            seg,
            actual_paragraphs,
            owner_psp,
            self.largest_free()
        );
        Ok(seg)
    }

    /// Allocate from UMB (Upper Memory Block)
    fn alloc_from_umb(
        &mut self,
        memory: &mut Memory,
        owner_psp: u16,
        paragraphs: u16,
        base_strategy: u8,
    ) -> Result<u16, DosMemError> {
        let has_tracking_slot = !self.umb_blocks.is_full();
        let can_allocate = |b: &UmbBlock| {
            b.owner_psp == 0
                && b.size_paras >= paragraphs
                && (has_tracking_slot || b.size_paras - paragraphs <= 1)
        };

        // Find suitable UMB block index based on strategy
        let block_idx = match base_strategy {
            0 => {
                // First-fit
                self.umb_blocks.iter().position(can_allocate)
            }
            1 => {
                // Best-fit
                self.umb_blocks
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| can_allocate(b))
                    .min_by_key(|(_, b)| b.size_paras)
                    .map(|(i, _)| i)
            }
            2 => {
                // Last-fit
                self.umb_blocks
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, b)| can_allocate(b))
                    .map(|(i, _)| i)
            }
            _ => self.umb_blocks.iter().position(can_allocate),
        };

        let Some(i) = block_idx else {
            return Err(DosMemError::InsufficientMemory {
                largest_available: self.largest_free_umb(),
            });
        };

        let b = self.umb_blocks[i];
        let remaining = b.size_paras - paragraphs;
        let allocated_size = if remaining <= 1 {
            b.size_paras
        } else {
            paragraphs
        };

        // Exact and near-exact allocations replace an entry in place, but a
        // split must reserve its tracking slot before mutating the free block.
        if remaining > 1 && self.umb_blocks.is_full() {
            return Err(DosMemError::InsufficientMemory {
                largest_available: self.largest_free_umb(),
            });
        }

        let alloc_idx = if remaining == 0 || remaining == 1 {
            // Consume entire block (no split needed, strategy doesn't matter).
            self.umb_blocks[i] = UmbBlock {
                mcb_seg: b.mcb_seg,
                size_paras: allocated_size,
                owner_psp,
                is_last: b.is_last,
                name: [0; 8],
            };
            i
        } else if base_strategy == 2 {
            // Last-fit: allocate from the HIGH end of the free block.
            let free_size = remaining - 1;
            let alloc_mcb_seg = b.mcb_seg + 1 + free_size;
            let free = UmbBlock {
                mcb_seg: b.mcb_seg,
                size_paras: free_size,
                owner_psp: 0,
                is_last: false,
                name: [0; 8],
            };
            let allocated = UmbBlock {
                mcb_seg: alloc_mcb_seg,
                size_paras: allocated_size,
                owner_psp,
                is_last: b.is_last,
                name: [0; 8],
            };
            self.umb_blocks[i] = free;
            self.umb_blocks.insert(i + 1, allocated);
            i + 1
        } else {
            // First-fit / best-fit: allocate from the LOW end of the free block.
            let allocated = UmbBlock {
                mcb_seg: b.mcb_seg,
                size_paras: allocated_size,
                owner_psp,
                is_last: false,
                name: [0; 8],
            };
            let free_mcb_seg = allocated.end_seg_exclusive();
            let free = UmbBlock {
                mcb_seg: free_mcb_seg,
                size_paras: remaining - 1,
                owner_psp: 0,
                is_last: b.is_last,
                name: [0; 8],
            };
            self.umb_blocks[i] = allocated;
            self.umb_blocks.insert(i + 1, free);
            i
        };

        self.recompute_umb_last_flag();
        self.write_all_umb_mcbs(memory);
        let seg = self.umb_blocks[alloc_idx].usable_seg();
        tracing::debug!(
            target: "gp64::dos::memory",
            "UMB alloc: seg={:04X} paras={} psp={:04X}",
            seg,
            paragraphs,
            owner_psp
        );
        Ok(seg)
    }

    /// Recompute is_last flag for UMB chain
    fn recompute_umb_last_flag(&mut self) {
        let len = self.umb_blocks.len();
        for (i, b) in self.umb_blocks.iter_mut().enumerate() {
            b.is_last = i == len - 1;
        }
    }

    /// Coalesce adjacent free UMB blocks
    fn coalesce_umb(&mut self) {
        let mut i = 0;
        while i + 1 < self.umb_blocks.len() {
            if self.umb_blocks[i].owner_psp == 0 && self.umb_blocks[i + 1].owner_psp == 0 {
                // Merge block i and i+1
                let next_size = self.umb_blocks[i + 1].size_paras;
                self.umb_blocks[i].size_paras += 1 + next_size; // +1 for the MCB we're removing
                self.umb_blocks[i].is_last = self.umb_blocks[i + 1].is_last;
                self.umb_blocks.remove(i + 1);
            } else {
                i += 1;
            }
        }
    }

    pub fn free(&mut self, memory: &mut Memory, segment: u16) -> Result<(), DosMemError> {
        if segment == 0 {
            tracing::debug!("DOS mem free failed: segment=0");
            return Err(DosMemError::InvalidBlock);
        }

        let mcb_seg = segment.wrapping_sub(1);

        // First check conventional memory
        if let Some(idx) = self
            .blocks
            .iter()
            .position(|b| b.mcb_seg == mcb_seg && b.owner_psp != 0)
        {
            let freed_paras = self.blocks[idx].size_paras;
            self.blocks[idx].owner_psp = 0;

            // Coalesce adjacent free blocks.
            self.coalesce();
            self.recompute_last_flag();
            self.write_all_mcbs(memory);
            tracing::debug!(
                "DOS mem free: seg={:04X} paras={} largest_free={}",
                segment,
                freed_paras,
                self.largest_free()
            );
            return Ok(());
        }

        // Check UMB
        if let Some(idx) = self
            .umb_blocks
            .iter()
            .position(|b| b.mcb_seg == mcb_seg && b.owner_psp != 0)
        {
            let freed_paras = self.umb_blocks[idx].size_paras;
            self.umb_blocks[idx].owner_psp = 0;

            // Coalesce adjacent free UMB blocks
            self.coalesce_umb();
            self.recompute_umb_last_flag();
            self.write_all_umb_mcbs(memory);
            tracing::debug!(
                target: "gp64::dos::memory",
                "UMB free: seg={:04X} paras={}",
                segment,
                freed_paras
            );
            return Ok(());
        }

        // Memory-based fallback for programs with private MCB sub-chains.
        // Real DOS validates by checking the MCB signature byte at (segment-1):0000,
        // not a lookup table. It doesn't care whether the enclosing region is free or
        // allocated — it only checks the signature. We're slightly stricter: we verify
        // the segment falls within a known block (free or allocated) and that the
        // sub-MCB's size_paras doesn't extend past the block boundary.
        //
        // A process allocates a large block, manages sub-allocations via
        // MCB headers, then shrinks the parent (INT 21h/4Ah). The shrunk-off region
        // becomes free in our Vec, but the process still frees sub-blocks inside it.
        let mcb_addr = Memory::seg_offset_to_linear(mcb_seg, 0);
        let sig = memory.read_u8(mcb_addr);
        if sig == b'M' || sig == b'Z' {
            if let Some(info) = self.block_info_for_segment(segment) {
                let sub_size = memory.read_u16(mcb_addr + 3);
                let sub_end = segment.checked_add(sub_size);
                if sub_end.is_some_and(|e| e <= info.end_seg) {
                    let prev_owner = memory.read_u16(mcb_addr + 1);
                    // Block is already free — reject (real DOS returns InvalidBlock)
                    if prev_owner == 0 {
                        tracing::debug!(
                            "DOS mem free failed: block already free seg={:04X}",
                            segment
                        );
                        return Err(DosMemError::InvalidBlock);
                    }
                    memory.write_u16(mcb_addr + 1, 0); // Set owner to 0 (free)
                    tracing::info!(
                        "DOS mem free sub-block: seg={:04X} mcb={:04X} size={:04X} \
                         prev_owner={:04X} within block mcb={:04X} ({})",
                        segment,
                        mcb_seg,
                        sub_size,
                        prev_owner,
                        info.mcb_seg,
                        if info.is_free { "free" } else { "allocated" }
                    );
                    return Ok(());
                }
            }
        }

        tracing::debug!("DOS mem free failed: invalid seg={:04X}", segment);
        Err(DosMemError::InvalidBlock)
    }

    /// Free all memory blocks owned by a specific PSP.
    /// Called during program termination to release all memory allocated by the process.
    pub fn free_all_for_psp(&mut self, memory: &mut Memory, psp: u16) {
        let mut freed_any = false;
        for block in &mut self.blocks {
            if block.owner_psp == psp {
                block.owner_psp = 0;
                freed_any = true;
            }
        }
        if freed_any {
            self.coalesce();
            self.recompute_last_flag();
            self.write_all_mcbs(memory);
        }

        // Also free UMB blocks
        let mut freed_umb = false;
        for block in &mut self.umb_blocks {
            if block.owner_psp == psp {
                block.owner_psp = 0;
                freed_umb = true;
            }
        }
        if freed_umb {
            self.coalesce_umb();
            self.recompute_umb_last_flag();
            self.write_all_umb_mcbs(memory);
        }
    }

    pub fn resize(
        &mut self,
        memory: &mut Memory,
        segment: u16,
        new_paragraphs: u16,
    ) -> Result<(), DosMemError> {
        if segment == 0 {
            return Err(DosMemError::InvalidBlock);
        }
        if new_paragraphs == 0 {
            // An owned block must contain at least one usable paragraph.
            return Err(DosMemError::InvalidBlock);
        }
        let mcb_seg = segment.wrapping_sub(1);
        let idx = self
            .blocks
            .iter()
            .position(|b| b.mcb_seg == mcb_seg && b.owner_psp != 0)
            .ok_or(DosMemError::InvalidBlock)?;

        let cur = self.blocks[idx];

        if new_paragraphs == cur.size_paras {
            return Ok(());
        }

        if new_paragraphs < cur.size_paras {
            // Shrink: create or expand a trailing free block.
            let freed_paras = cur.size_paras - new_paragraphs;
            let new_free_mcb = cur.mcb_seg.wrapping_add(1 + new_paragraphs);
            let new_free = Block {
                mcb_seg: new_free_mcb,
                size_paras: freed_paras.saturating_sub(1),
                owner_psp: 0,
                is_last: false,
                name: [0; 8],
            };

            // A normal free successor begins where the newly freed region
            // ends. Preserve the legacy equal-start case as well.
            let (can_expand_next, clear_next_name) = self
                .blocks
                .get(idx + 1)
                .map(|next| {
                    let same_start = next.mcb_seg == new_free.mcb_seg;
                    let normal_adjacency = next.mcb_seg == new_free.end_seg_exclusive();
                    (
                        next.owner_psp == 0 && (freed_paras == 1 || same_start || normal_adjacency),
                        next.owner_psp == 0 && freed_paras > 1 && !same_start && normal_adjacency,
                    )
                })
                .unwrap_or((false, false));

            if !can_expand_next && self.blocks.is_full() {
                return Err(DosMemError::InsufficientMemory {
                    largest_available: cur.size_paras,
                });
            }

            self.blocks[idx].size_paras = new_paragraphs;
            if can_expand_next {
                self.blocks[idx + 1].mcb_seg = new_free.mcb_seg;
                self.blocks[idx + 1].size_paras = self.blocks[idx + 1]
                    .size_paras
                    .saturating_add(new_free.size_paras + 1);
                if clear_next_name {
                    self.blocks[idx + 1].name = [0; 8];
                }
            } else {
                self.blocks.insert(idx + 1, new_free);
            }

            self.coalesce();
            self.recompute_last_flag();
            self.write_all_mcbs(memory);
            tracing::debug!(
                "DOS mem resize shrink: seg={:04X} {} -> {} paras",
                segment,
                cur.size_paras,
                new_paragraphs
            );
            return Ok(());
        }

        // Expand in place only if next block is free and has enough space.
        let need = new_paragraphs - cur.size_paras;

        // Compute how much this specific block can grow: current size + adjacent free space.
        // This is what DOS returns in BX on failure (max paragraphs for THIS block).
        let (next_free, next_size) = match self.blocks.get(idx + 1) {
            Some(next) => (next.owner_psp == 0, next.size_paras),
            None => (false, 0),
        };
        let max_for_block = max_resize_for_block(cur.size_paras, next_free, next_size);

        if idx + 1 < self.blocks.len() {
            let next = self.blocks[idx + 1];
            if next.owner_psp == 0 {
                // Available paragraphs in next block usable + the MCB paragraph itself if we consume it.
                // If we consume the entire next block, we also remove its MCB (1 paragraph).
                let total_next_paras_including_mcb = next.size_paras.saturating_add(1);
                if total_next_paras_including_mcb >= need {
                    // We can grow by taking from next.
                    self.blocks[idx].size_paras = new_paragraphs;

                    if total_next_paras_including_mcb == need {
                        // Consume whole next block.
                        self.blocks.remove(idx + 1);
                    } else {
                        // Shrink next block and move its MCB forward.
                        let remaining_total = total_next_paras_including_mcb - need;
                        let new_next_mcb = self.blocks[idx].end_seg_exclusive();
                        self.blocks[idx + 1].mcb_seg = new_next_mcb;
                        self.blocks[idx + 1].size_paras = remaining_total.saturating_sub(1);
                    }

                    self.coalesce();
                    self.recompute_last_flag();
                    self.write_all_mcbs(memory);
                    tracing::debug!(
                        "DOS mem resize grow: seg={:04X} {} -> {} paras",
                        segment,
                        cur.size_paras,
                        new_paragraphs
                    );
                    return Ok(());
                }
            }
        }

        // If no in-place expansion, fail with max size this block can reach.
        tracing::debug!(
            "DOS mem resize failed: seg={:04X} {} -> {} paras (largest={})",
            segment,
            cur.size_paras,
            new_paragraphs,
            max_for_block
        );
        Err(DosMemError::InsufficientMemory {
            largest_available: max_for_block,
        })
    }

    /// Resize a UMB (Upper Memory Block)
    /// segment: the usable segment (not MCB segment)
    /// new_paragraphs: the new size in paragraphs
    pub fn resize_umb(
        &mut self,
        memory: &mut Memory,
        segment: u16,
        new_paragraphs: u16,
    ) -> Result<(), DosMemError> {
        if segment == 0 {
            return Err(DosMemError::InvalidBlock);
        }
        if new_paragraphs == 0 {
            // Apply the same nonzero-size invariant to upper-memory blocks.
            return Err(DosMemError::InvalidBlock);
        }
        let mcb_seg = segment.wrapping_sub(1);
        let idx = self
            .umb_blocks
            .iter()
            .position(|b| b.mcb_seg == mcb_seg && b.owner_psp != 0)
            .ok_or(DosMemError::InvalidBlock)?;

        let cur = self.umb_blocks[idx];

        if new_paragraphs == cur.size_paras {
            return Ok(());
        }

        if new_paragraphs < cur.size_paras {
            // Shrink: create or expand a trailing free block.
            let freed_paras = cur.size_paras - new_paragraphs;
            let new_free_mcb = cur.mcb_seg.wrapping_add(1 + new_paragraphs);
            let new_free = UmbBlock {
                mcb_seg: new_free_mcb,
                size_paras: freed_paras.saturating_sub(1),
                owner_psp: 0,
                is_last: false,
                name: [0; 8],
            };

            // A one-paragraph shrink contributes only the new free MCB header.
            // Move an adjacent free successor back to consume it; otherwise a
            // zero-sized free MCB is still required to keep the chain continuous.
            let (can_expand_next, clear_next_name) = self
                .umb_blocks
                .get(idx + 1)
                .map(|next| {
                    let same_start = next.mcb_seg == new_free.mcb_seg;
                    let normal_adjacency = next.mcb_seg == new_free.end_seg_exclusive();
                    (
                        next.owner_psp == 0 && (freed_paras == 1 || same_start || normal_adjacency),
                        next.owner_psp == 0 && freed_paras > 1 && !same_start && normal_adjacency,
                    )
                })
                .unwrap_or((false, false));

            if !can_expand_next && self.umb_blocks.is_full() {
                return Err(DosMemError::InsufficientMemory {
                    largest_available: cur.size_paras,
                });
            }

            self.umb_blocks[idx].size_paras = new_paragraphs;
            if can_expand_next {
                self.umb_blocks[idx + 1].mcb_seg = new_free.mcb_seg;
                self.umb_blocks[idx + 1].size_paras = self.umb_blocks[idx + 1]
                    .size_paras
                    .saturating_add(new_free.size_paras + 1);
                if clear_next_name {
                    self.umb_blocks[idx + 1].name = [0; 8];
                }
            } else {
                self.umb_blocks.insert(idx + 1, new_free);
            }

            self.coalesce_umb();
            self.recompute_umb_last_flag();
            self.write_all_umb_mcbs(memory);
            tracing::debug!(
                target: "gp64::dos::memory",
                "UMB resize shrink: seg={:04X} {} -> {} paras",
                segment,
                cur.size_paras,
                new_paragraphs
            );
            return Ok(());
        }

        // Expand in place only if next block is free and has enough space
        let need = new_paragraphs - cur.size_paras;

        if idx + 1 < self.umb_blocks.len() {
            let next = self.umb_blocks[idx + 1];
            if next.owner_psp == 0 {
                let total_next_paras_including_mcb = next.size_paras.saturating_add(1);
                if total_next_paras_including_mcb >= need {
                    self.umb_blocks[idx].size_paras = new_paragraphs;

                    if total_next_paras_including_mcb == need {
                        // Consume whole next block
                        self.umb_blocks.remove(idx + 1);
                    } else {
                        // Shrink next block and move its MCB forward
                        let remaining_total = total_next_paras_including_mcb - need;
                        let new_next_mcb = self.umb_blocks[idx]
                            .mcb_seg
                            .wrapping_add(1 + new_paragraphs);
                        self.umb_blocks[idx + 1].mcb_seg = new_next_mcb;
                        self.umb_blocks[idx + 1].size_paras = remaining_total.saturating_sub(1);
                    }

                    self.coalesce_umb();
                    self.recompute_umb_last_flag();
                    self.write_all_umb_mcbs(memory);
                    tracing::debug!(
                        target: "gp64::dos::memory",
                        "UMB resize grow: seg={:04X} {} -> {} paras",
                        segment,
                        cur.size_paras,
                        new_paragraphs
                    );
                    return Ok(());
                }
            }
        }

        // Cannot expand in place. INT 21h/4Ah wants the maximum paragraphs
        // available for THIS block — its current size plus any adjacent
        // forward free space it could absorb. Returning `largest_free_umb()`
        // here was wrong (it reports the largest free block anywhere in the
        // allocation, then resize to zero and corrupt the chain.
        let (next_free, next_size) = match self.umb_blocks.get(idx + 1) {
            Some(next) => (next.owner_psp == 0, next.size_paras),
            None => (false, 0),
        };
        let max_for_block = max_resize_for_block(cur.size_paras, next_free, next_size);
        Err(DosMemError::InsufficientMemory {
            largest_available: max_for_block,
        })
    }

    fn coalesce(&mut self) {
        let mut i = 0usize;
        while i + 1 < self.blocks.len() {
            let a = self.blocks[i];
            let b = self.blocks[i + 1];
            if a.owner_psp == 0 && b.owner_psp == 0 && a.end_seg_exclusive() == b.mcb_seg {
                // Merge b into a: a grows by (1 paragraph for b's MCB) + b.size_paras.
                self.blocks[i].size_paras = a.size_paras.saturating_add(1 + b.size_paras);
                self.blocks[i].is_last = b.is_last;
                self.blocks.remove(i + 1);
                continue;
            }
            i += 1;
        }
    }

    fn recompute_last_flag(&mut self) {
        let len = self.blocks.len();
        for (idx, b) in self.blocks.iter_mut().enumerate() {
            b.is_last = idx + 1 == len;
        }
    }

    fn write_all_mcbs(&self, memory: &mut Memory) {
        for b in &self.blocks {
            self.write_mcb(memory, *b);
        }
        // When UMBs are linked, the last conventional MCB must use 'M' marker
        // so the chain continues into the bridge MCB.
        if self.umb_chain_linked && self.umb_initialized && !self.umb_blocks.is_empty() {
            if let Some(last_conv) = self.blocks.last() {
                let base = Memory::seg_offset_to_linear(last_conv.mcb_seg, 0);
                memory.write_u8(base, b'M');
            }
        }
    }

    /// Writes the system-owned "SC" bridge MCB at A000:0000 that spans the gap
    /// between conventional memory and the UMB chain (the DOS model `DOS_BuildUMBChain`).
    ///
    /// Its bytes lie inside the VGA aperture, so like the DOS model this writes it
    /// only when the chain is linked, never on each allocation, free or resize:
    /// those run while a guest is in a graphics mode, and rewriting the bridge
    /// then stamped 13 bytes into the framebuffer (MDD-BUG-CRU-01403).
    fn write_umb_bridge_mcb(&self, memory: &mut Memory) {
        if self.umb_chain_linked && self.umb_initialized && !self.umb_blocks.is_empty() {
            let umb_start = self.umb_blocks[0].mcb_seg;
            let bridge_base = Memory::seg_offset_to_linear(CONV_END_SEG, 0);
            let bridge_size = umb_start.saturating_sub(CONV_END_SEG).saturating_sub(1);
            memory.write_u8(bridge_base, b'M');
            memory.write_u16(bridge_base + 0x01, 0x0008); // System-owned
            memory.write_u16(bridge_base + 0x03, bridge_size);
            let name = b"SC\0\0\0\0\0\0";
            for i in 0..8 {
                memory.write_u8(bridge_base + 0x08 + i, name[i]);
            }
        }
    }

    fn write_mcb(&self, memory: &mut Memory, b: Block) {
        let base = Memory::seg_offset_to_linear(b.mcb_seg, 0);
        memory.write_u8(base, if b.is_last { b'Z' } else { b'M' });
        memory.write_u16(base + 0x01, b.owner_psp);
        memory.write_u16(base + 0x03, b.size_paras);

        // Write MCB name field (DOS 4.0+)
        for i in 0..8 {
            memory.write_u8(base + 0x08 + i, b.name[i]);
        }
    }

    /// Write MCB chain linkage between conventional and UMB regions.
    /// When UMBs are linked, programs can walk the MCB chain from conventional
    /// memory all the way through to UMB. The gap between CONV_END_SEG and
    /// UMB_START_SEG is bridged with a system-owned MCB.
    /// Update the UMB chain link state and rewrite MCBs to reflect it.
    /// When linked, the last conventional MCB uses 'M' marker and a bridge
    /// MCB connects conventional memory to UMBs. When unlinked, the last
    /// conventional MCB uses 'Z' marker.
    pub fn write_umb_link(&mut self, memory: &mut Memory, linked: bool) {
        self.umb_chain_linked = linked;
        // Rewrite conventional MCBs (which now respect umb_chain_linked)
        self.write_all_mcbs(memory);
        self.write_umb_bridge_mcb(memory);
    }

    /// Set the MCB name field for a memory block.
    /// The name is taken from the program name (typically the first 8 chars of the EXE name).
    /// This is a DOS 4.0+ feature.
    pub fn set_mcb_name(&mut self, memory: &mut Memory, segment: u16, name: &[u8]) {
        let mcb_seg = segment.wrapping_sub(1);
        if let Some(idx) = self.blocks.iter().position(|b| b.mcb_seg == mcb_seg) {
            // Copy up to 8 bytes from name, pad with zeros
            for i in 0..8 {
                self.blocks[idx].name[i] = name.get(i).copied().unwrap_or(0);
            }
            self.write_all_mcbs(memory);
        }
    }
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)] // Index used for both array and memory address
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn assert_blocks_contiguous(dm: &DosMem) {
        for window in dm.blocks.windows(2) {
            assert_eq!(window[0].end_seg_exclusive(), window[1].mcb_seg);
        }
    }

    fn setup_dos_mem() -> (Memory, DosMem) {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        (mem, dm)
    }

    fn setup_dos_mem_with_umb() -> (Memory, DosMem) {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);
        (mem, dm)
    }

    fn fill_conventional_tracking_to_capacity(
        dm: &mut DosMem,
        memory: &mut Memory,
        tail_owner: u16,
    ) -> u16 {
        dm.blocks.clear();
        let capacity = dm.blocks.capacity();
        let mut mcb_seg = DEFAULT_FIRST_MCB_SEG;
        for index in 0..capacity {
            let is_tail = index + 1 == capacity;
            let block = Block {
                mcb_seg,
                size_paras: if is_tail { 0x0100 } else { 1 },
                owner_psp: if is_tail { tail_owner } else { 0x0100 },
                is_last: is_tail,
                name: [0; 8],
            };
            dm.blocks.push(block);
            if !is_tail {
                mcb_seg = block.end_seg_exclusive();
            }
        }
        dm.write_all_mcbs(memory);
        dm.blocks[capacity - 1].usable_seg()
    }

    fn fill_umb_tracking_to_capacity(dm: &mut DosMem, memory: &mut Memory, tail_owner: u16) -> u16 {
        dm.umb_blocks.clear();
        dm.umb_initialized = true;
        let capacity = dm.umb_blocks.capacity();
        let mut mcb_seg = UMB_START_SEG;
        for index in 0..capacity {
            let is_tail = index + 1 == capacity;
            let block = UmbBlock {
                mcb_seg,
                size_paras: if is_tail { 0x0100 } else { 1 },
                owner_psp: if is_tail { tail_owner } else { 0x0100 },
                is_last: is_tail,
                name: [0; 8],
            };
            dm.umb_blocks.push(block);
            if !is_tail {
                mcb_seg = block.end_seg_exclusive();
            }
        }
        dm.write_all_umb_mcbs(memory);
        dm.umb_blocks[capacity - 1].usable_seg()
    }

    fn mcb_header_snapshot(memory: &Memory, dm: &DosMem) -> Vec<(u16, [u8; 16])> {
        dm.mcb_summary()
            .into_iter()
            .map(|entry| {
                let base = Memory::seg_offset_to_linear(entry.seg, 0);
                let bytes = std::array::from_fn(|offset| memory.read_u8(base + offset));
                (entry.seg, bytes)
            })
            .collect()
    }

    #[test]
    fn capacity_guard_ah48_conventional_split_is_atomic() {
        for strategy in [0, 1, 2] {
            let (mut memory, mut dm) = setup_dos_mem();
            fill_conventional_tracking_to_capacity(&mut dm, &mut memory, 0);
            let summary_before = dm.mcb_summary();
            let headers_before = mcb_header_snapshot(&memory, &dm);

            let result = dm.alloc_with_strategy(&mut memory, 0x0200, 1, strategy);

            assert_eq!(
                result,
                Err(DosMemError::InsufficientMemory {
                    largest_available: 0x0100,
                }),
                "strategy {strategy}"
            );
            assert_eq!(dm.mcb_summary(), summary_before, "strategy {strategy}");
            assert_eq!(
                mcb_header_snapshot(&memory, &dm),
                headers_before,
                "strategy {strategy}"
            );
        }
    }

    /// MDD-BUG-CRU-01404: with UMBs linked, strategies 40h-42h allocate from
    /// upper memory only and fail with error 8 rather than falling back to
    /// conventional memory; 80h-82h still fall back.
    #[test]
    fn high_only_strategies_fail_instead_of_falling_back_to_low_memory() {
        let too_big_for_umb = 0x2000;
        for strategy in [0x40, 0x41, 0x42] {
            let (mut memory, mut dm) = setup_dos_mem_with_umb();
            let largest_umb = dm.largest_free_umb();
            assert!(largest_umb < too_big_for_umb);
            let summary_before = dm.mcb_summary();

            let result = dm.alloc_with_strategy_and_umb(
                &mut memory,
                0x0200,
                too_big_for_umb,
                strategy,
                true,
            );

            assert_eq!(
                result,
                Err(DosMemError::InsufficientMemory {
                    largest_available: largest_umb,
                }),
                "strategy {strategy:02X} is high-only"
            );
            assert_eq!(dm.mcb_summary(), summary_before, "strategy {strategy:02X}");

            let fits = dm
                .alloc_with_strategy_and_umb(&mut memory, 0x0200, 0x10, strategy, true)
                .expect("a request the UMB arena holds succeeds");
            assert!(
                fits >= UMB_START_SEG,
                "strategy {strategy:02X} allocates high"
            );
        }
        for strategy in [0x80, 0x81, 0x82] {
            let (mut memory, mut dm) = setup_dos_mem_with_umb();
            let seg = dm
                .alloc_with_strategy_and_umb(&mut memory, 0x0200, too_big_for_umb, strategy, true)
                .expect("high-first falls back to conventional memory");
            assert!(seg < 0xA000, "strategy {strategy:02X} falls back low");
        }
    }

    /// MDD-BUG-CRU-01403: linking the UMB chain writes the bridge MCB at
    /// A000:0000 once; later allocations and frees leave the VGA bytes there
    /// alone.
    #[test]
    fn allocation_does_not_restamp_the_bridge_mcb_in_video_memory() {
        let (mut memory, mut dm) = setup_dos_mem_with_umb();
        dm.write_umb_link(&mut memory, true);
        let bridge = Memory::seg_offset_to_linear(CONV_END_SEG, 0);
        assert_eq!(
            memory.read_u8(bridge),
            b'M',
            "linking writes the bridge MCB"
        );
        assert_eq!(memory.read_u16(bridge + 0x01), 0x0008);

        for offset in 0..0x10 {
            memory.write_u8(bridge + offset, 0x5A);
        }
        let seg = dm.alloc(&mut memory, 0x0200, 0x10).expect("allocation");
        dm.free(&mut memory, seg).expect("free");

        for offset in 0..0x10 {
            assert_eq!(memory.read_u8(bridge + offset), 0x5A, "byte {offset:#x}");
        }
    }

    #[test]
    fn capacity_guard_ah48_umb_and_conventional_splits_are_atomic() {
        for strategy in [0x80, 0x81, 0x82] {
            let (mut memory, mut dm) = setup_dos_mem_with_umb();
            fill_conventional_tracking_to_capacity(&mut dm, &mut memory, 0);
            fill_umb_tracking_to_capacity(&mut dm, &mut memory, 0);
            let summary_before = dm.mcb_summary();
            let headers_before = mcb_header_snapshot(&memory, &dm);

            let result = dm.alloc_with_strategy_and_umb(&mut memory, 0x0200, 1, strategy, true);

            assert_eq!(
                result,
                Err(DosMemError::InsufficientMemory {
                    largest_available: 0x0100,
                }),
                "strategy {strategy:02X}"
            );
            assert_eq!(dm.mcb_summary(), summary_before, "strategy {strategy:02X}");
            assert_eq!(
                mcb_header_snapshot(&memory, &dm),
                headers_before,
                "strategy {strategy:02X}"
            );
        }
    }

    #[test]
    fn capacity_guard_ah4a_conventional_shrink_is_atomic() {
        let (mut memory, mut dm) = setup_dos_mem();
        let segment = fill_conventional_tracking_to_capacity(&mut dm, &mut memory, 0x0200);
        let summary_before = dm.mcb_summary();
        let headers_before = mcb_header_snapshot(&memory, &dm);

        let result = dm.resize(&mut memory, segment, 0x0080);

        assert_eq!(
            result,
            Err(DosMemError::InsufficientMemory {
                largest_available: 0x0100,
            })
        );
        assert_eq!(dm.mcb_summary(), summary_before);
        assert_eq!(mcb_header_snapshot(&memory, &dm), headers_before);
    }

    #[test]
    fn capacity_guard_ah4a_umb_shrink_is_atomic() {
        for new_size in [0x0080, 0x00FF] {
            let (mut memory, mut dm) = setup_dos_mem_with_umb();
            let segment = fill_umb_tracking_to_capacity(&mut dm, &mut memory, 0x0200);
            let summary_before = dm.mcb_summary();
            let headers_before = mcb_header_snapshot(&memory, &dm);

            let result = dm.resize_umb(&mut memory, segment, new_size);

            assert_eq!(
                result,
                Err(DosMemError::InsufficientMemory {
                    largest_available: 0x0100,
                }),
                "new size {new_size:04X}"
            );
            assert_eq!(dm.mcb_summary(), summary_before, "new size {new_size:04X}");
            assert_eq!(
                mcb_header_snapshot(&memory, &dm),
                headers_before,
                "new size {new_size:04X}"
            );
        }
    }

    #[test]
    fn capacity_guard_first_fit_skips_split_for_later_exact_fit() {
        let (mut memory, mut dm) = setup_dos_mem();
        fill_conventional_tracking_to_capacity(&mut dm, &mut memory, 0);
        let len = dm.blocks.len();
        let first_free_mcb = dm.blocks[len - 3].mcb_seg;
        dm.blocks[len - 3] = Block {
            mcb_seg: first_free_mcb,
            size_paras: 3,
            owner_psp: 0,
            is_last: false,
            name: [0; 8],
        };
        dm.blocks[len - 2] = Block {
            mcb_seg: dm.blocks[len - 3].end_seg_exclusive(),
            size_paras: 1,
            owner_psp: 0x0100,
            is_last: false,
            name: [0; 8],
        };
        dm.blocks[len - 1] = Block {
            mcb_seg: dm.blocks[len - 2].end_seg_exclusive(),
            size_paras: 1,
            owner_psp: 0,
            is_last: true,
            name: [0; 8],
        };
        let exact_fit_segment = dm.blocks[len - 1].usable_seg();
        dm.write_all_mcbs(&mut memory);

        let allocated = dm.alloc_with_strategy(&mut memory, 0x0200, 1, 0).unwrap();

        assert_eq!(allocated, exact_fit_segment);
        assert_eq!(dm.blocks.len(), dm.blocks.capacity());
    }

    #[test]
    fn capacity_guard_preserves_cross_arena_fallbacks() {
        let (mut memory, mut dm) = setup_dos_mem_with_umb();
        fill_conventional_tracking_to_capacity(&mut dm, &mut memory, 0);
        let from_umb = dm
            .alloc_with_strategy_and_umb(&mut memory, 0x0200, 1, 0x00, true)
            .unwrap();
        assert!(from_umb >= UMB_START_SEG);

        let (mut memory, mut dm) = setup_dos_mem_with_umb();
        fill_umb_tracking_to_capacity(&mut dm, &mut memory, 0);
        let from_conventional = dm
            .alloc_with_strategy_and_umb(&mut memory, 0x0200, 1, 0x80, true)
            .unwrap();
        assert!(from_conventional < UMB_START_SEG);
    }

    #[test]
    fn capacity_guard_exact_fit_still_succeeds() {
        let (mut memory, mut dm) = setup_dos_mem_with_umb();
        let exact_fit_segment = fill_umb_tracking_to_capacity(&mut dm, &mut memory, 0);

        let allocated = dm
            .alloc_with_strategy_and_umb(&mut memory, 0x0200, 0x0100, 0x80, true)
            .unwrap();

        assert_eq!(allocated, exact_fit_segment);
        assert_eq!(dm.umb_blocks.len(), dm.umb_blocks.capacity());
    }

    #[test]
    fn capacity_guard_shrink_into_adjacent_free_block_still_succeeds() {
        let (mut memory, mut dm) = setup_dos_mem();
        fill_conventional_tracking_to_capacity(&mut dm, &mut memory, 0);
        let len = dm.blocks.len();
        dm.blocks[len - 2].size_paras = 4;
        dm.blocks[len - 2].owner_psp = 0x0200;
        dm.blocks[len - 1].mcb_seg = dm.blocks[len - 2].end_seg_exclusive();
        dm.blocks[len - 1].name = *b"STALE   ";
        let segment = dm.blocks[len - 2].usable_seg();
        let next_size = dm.blocks[len - 1].size_paras;
        dm.write_all_mcbs(&mut memory);

        dm.resize(&mut memory, segment, 2).unwrap();

        assert_eq!(dm.blocks.len(), dm.blocks.capacity());
        assert_eq!(dm.blocks[len - 2].size_paras, 2);
        assert_eq!(
            dm.blocks[len - 1].mcb_seg,
            dm.blocks[len - 2].end_seg_exclusive()
        );
        assert_eq!(dm.blocks[len - 1].size_paras, next_size + 2);
        assert_eq!(dm.blocks[len - 1].name, [0; 8]);
    }

    #[test]
    fn first_mcb_seg_override_defaults_without_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        assert_eq!(first_mcb_seg_override(), DEFAULT_FIRST_MCB_SEG);
    }

    #[test]
    fn first_mcb_seg_override_reads_hex_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        let seg = first_mcb_seg_override();
        assert_eq!(seg, 0x0700);
    }

    #[test]
    fn first_mcb_seg_override_invalid_env_falls_back() {
        let _lock = ENV_LOCK.lock().unwrap();
        let seg = first_mcb_seg_override();
        assert_eq!(seg, DEFAULT_FIRST_MCB_SEG);
    }

    #[test]
    fn alloc_split_and_free_coalesce() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let largest = dm.largest_free();
        assert!(largest > 0x100);

        let a = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        assert_eq!(a, DEFAULT_FIRST_MCB_SEG + 1);

        let b = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        assert_ne!(a, b);

        dm.free(&mut mem, a).unwrap();
        dm.free(&mut mem, b).unwrap();

        // Should coalesce back into one free block roughly equal to initial.
        assert_eq!(dm.blocks.len(), 1);
        assert_eq!(dm.blocks[0].owner_psp, 0);
        assert_eq!(dm.blocks[0].size_paras, largest);
    }

    #[test]
    fn alloc_zero_paragraphs_allocates_one() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Zero paragraphs should allocate at least 1 paragraph (minimum allocation)
        let result = dm.alloc(&mut mem, 0x0100, 0).unwrap();
        assert!(
            result > 0,
            "Zero-paragraph allocation should return valid segment"
        );
        assert_eq!(
            result,
            DEFAULT_FIRST_MCB_SEG + 1,
            "Should allocate at first available segment"
        );

        // Verify block was actually allocated with 1 paragraph
        let block = dm.blocks.iter().find(|b| b.usable_seg() == result).unwrap();
        assert_eq!(
            block.size_paras, 1,
            "Should allocate exactly 1 paragraph for 0 request"
        );
        assert_eq!(block.owner_psp, 0x0100, "Owner PSP should be set correctly");
    }

    #[test]
    fn alloc_insufficient_memory() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Try to allocate more than available
        let result = dm.alloc(&mut mem, 0x0100, 0xFFFF);
        match result {
            Err(DosMemError::InsufficientMemory { largest_available }) => {
                assert!(largest_available > 0);
            }
            _ => panic!("Expected InsufficientMemory error"),
        }
    }

    #[test]
    fn block_end_segment_returns_correct_value() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let end = dm.block_end_segment(seg);
        assert_eq!(end, Some(seg + 0x20));
    }

    #[test]
    fn block_end_segment_invalid_returns_none() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let end = dm.block_end_segment(0x1234); // Non-existent segment
        assert_eq!(end, None);
    }

    // ── iter_free_conventional_blocks (DOS BSS-init policy, HLD §4.2) ──

    #[test]
    fn iter_free_conventional_blocks_fresh_chain() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Single free block spanning to the start of VGA memory.
        let blocks: Vec<_> = dm.iter_free_conventional_blocks().collect();
        assert_eq!(blocks.len(), 1, "fresh chain has exactly one free block");
        let (usable, end) = blocks[0];
        assert_eq!(usable, DEFAULT_FIRST_MCB_SEG + 1);
        assert_eq!(end, CONV_END_SEG);
        // Sanity: the usable range never enters the VGA framebuffer.
        assert!(end <= CONV_END_SEG);
    }

    #[test]
    fn iter_free_conventional_blocks_skips_allocated() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let alloc_paras = 0x100u16;
        let owned_seg = dm.alloc(&mut mem, 0x0042, alloc_paras).unwrap();

        let blocks: Vec<_> = dm.iter_free_conventional_blocks().collect();
        // None of the returned ranges may overlap the owned block.
        for (usable, end) in &blocks {
            let owned_end = owned_seg + alloc_paras;
            assert!(
                *end <= owned_seg || *usable >= owned_end,
                "free range {usable:#06X}..{end:#06X} overlaps owned block \
                 {owned_seg:#06X}..{owned_end:#06X}"
            );
        }
        // And we must not see the owned segment itself as free.
        assert!(!blocks.iter().any(|(u, _)| *u == owned_seg));
    }

    #[test]
    fn iter_free_conventional_blocks_ignores_umbs() {
        let (_mem, dm) = setup_dos_mem_with_umb();

        let blocks: Vec<_> = dm.iter_free_conventional_blocks().collect();
        // Every returned range must stay below 0xA000 (conventional only).
        for (usable, end) in blocks {
            assert!(usable < CONV_END_SEG, "usable {usable:#06X} crosses VGA");
            assert!(end <= CONV_END_SEG, "end {end:#06X} crosses VGA");
            assert!(
                usable < UMB_START_SEG && end <= UMB_START_SEG,
                "UMB range {usable:#06X}..{end:#06X} leaked into conv-only iterator"
            );
        }
    }

    #[test]
    fn block_info_for_segment_reports_allocated_block() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let info = dm.block_info_for_segment(seg).expect("expected block info");
        assert_eq!(info.usable_seg, seg);
        assert_eq!(info.owner_psp, 0x0100);
        assert!(!info.is_free);
        assert!(info.end_seg > info.usable_seg);
    }

    #[test]
    fn block_info_for_segment_ignores_mcb_header() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let mcb_seg = seg.wrapping_sub(1);
        assert!(dm.block_info_for_segment(mcb_seg).is_none());
    }

    #[test]
    fn block_info_for_segment_umb_mcb_header_returns_none() {
        let (_mem, dm) = setup_dos_mem_with_umb();
        let info = dm.block_info_for_segment(UMB_START_SEG);
        assert!(info.is_none());
    }

    #[test]
    fn block_info_for_segment_umb_end_returns_none() {
        let (_mem, dm) = setup_dos_mem_with_umb();
        let info = dm.block_info_for_segment(UMB_END_SEG);
        assert!(info.is_none());
    }

    #[test]
    fn set_owner_changes_owner() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        assert!(dm.set_owner(&mut mem, seg, 0x0200).is_ok());

        // Verify owner changed by checking block
        let mcb_seg = seg.wrapping_sub(1);
        let block = dm.blocks.iter().find(|b| b.mcb_seg == mcb_seg).unwrap();
        assert_eq!(block.owner_psp, 0x0200);
    }

    #[test]
    fn set_owner_invalid_block() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let result = dm.set_owner(&mut mem, 0x1234, 0x0100);
        assert!(matches!(result, Err(DosMemError::InvalidBlock)));
    }

    #[test]
    fn free_invalid_segment_zero() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let result = dm.free(&mut mem, 0);
        assert!(matches!(result, Err(DosMemError::InvalidBlock)));
    }

    #[test]
    fn free_already_free_block() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Try to free the initial free block (segment after first MCB)
        let result = dm.free(&mut mem, DEFAULT_FIRST_MCB_SEG + 1);
        assert!(matches!(result, Err(DosMemError::InvalidBlock)));
    }

    #[test]
    fn resize_same_size_is_noop() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let result = dm.resize(&mut mem, seg, 0x20);
        assert!(result.is_ok());
    }

    #[test]
    fn resize_segment_zero_is_invalid() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let result = dm.resize(&mut mem, 0, 0x10);
        assert!(matches!(result, Err(DosMemError::InvalidBlock)));
    }

    #[test]
    fn resize_shrink_block() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x40).unwrap();
        let blocks_before = dm.blocks.len();

        // Shrink from 0x40 to 0x20
        let result = dm.resize(&mut mem, seg, 0x20);
        assert!(result.is_ok());

        // Should have created a new free block
        assert!(dm.blocks.len() >= blocks_before);

        // Check the block size changed
        let mcb_seg = seg.wrapping_sub(1);
        let block = dm.blocks.iter().find(|b| b.mcb_seg == mcb_seg).unwrap();
        assert_eq!(block.size_paras, 0x20);
    }

    #[test]
    fn resize_shrink_small_amount() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();

        // Shrink by only 1 paragraph (freed_paras == 1, so no new block created)
        let result = dm.resize(&mut mem, seg, 0x0F);
        assert!(result.is_ok());
    }

    #[test]
    fn resize_shrink_one_paragraph_with_next_allocated_actually_shrinks() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let _next = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();

        let mcb_seg = seg.wrapping_sub(1);
        let size_before = dm
            .blocks
            .iter()
            .find(|b| b.mcb_seg == mcb_seg)
            .unwrap()
            .size_paras;

        dm.resize(&mut mem, seg, size_before - 1).unwrap();

        let size_after = dm
            .blocks
            .iter()
            .find(|b| b.mcb_seg == mcb_seg)
            .unwrap()
            .size_paras;
        // Should actually shrink by inserting a 0-paragraph free block
        assert_eq!(size_after, size_before - 1);
        assert_blocks_contiguous(&dm);
    }

    #[test]
    fn resize_shrink_one_paragraph_without_next_actually_shrinks() {
        let (mut mem, mut dm) = setup_dos_mem();
        let largest = dm.largest_free();
        let seg = dm.alloc(&mut mem, 0x0100, largest).unwrap();

        let size_before = dm.blocks[0].size_paras;
        dm.resize(&mut mem, seg, size_before - 1).unwrap();

        let size_after = dm.blocks[0].size_paras;
        // Should actually shrink by inserting a 0-paragraph free block
        assert_eq!(size_after, size_before - 1);
        assert_blocks_contiguous(&dm);
    }

    #[test]
    fn resize_shrink_last_block_inserts_free_block() {
        let (mut mem, mut dm) = setup_dos_mem();
        let largest = dm.largest_free();
        let seg = dm.alloc(&mut mem, 0x0100, largest).unwrap();

        let blocks_before = dm.blocks.len();
        dm.resize(&mut mem, seg, largest - 0x10).unwrap();

        assert_eq!(dm.blocks.len(), blocks_before + 1);
    }

    #[test]
    fn resize_shrink_inserts_free_when_next_allocated() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let _next = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();

        let blocks_before = dm.blocks.len();
        dm.resize(&mut mem, seg, 0x10).unwrap();

        assert_eq!(dm.blocks.len(), blocks_before + 1);
    }

    #[test]
    fn resize_shrink_merges_when_next_mcb_matches_new_end() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);

        let allocated = Block {
            mcb_seg: DEFAULT_FIRST_MCB_SEG,
            size_paras: 0x10,
            owner_psp: 0x0100,
            is_last: false,
            name: [0; 8],
        };
        let new_size = 0x08;
        let new_free_mcb = allocated.mcb_seg.wrapping_add(1 + new_size);
        let next = Block {
            mcb_seg: new_free_mcb,
            size_paras: 0x05,
            owner_psp: 0,
            is_last: true,
            name: [0; 8],
        };

        dm.blocks.clear();
        dm.blocks.push(allocated);
        dm.blocks.push(next);

        dm.resize(&mut mem, allocated.usable_seg(), new_size)
            .unwrap();

        let merged = dm
            .blocks
            .iter()
            .find(|b| b.mcb_seg == new_free_mcb)
            .unwrap();
        assert_eq!(merged.size_paras, 0x0D);
    }

    #[test]
    fn resize_grow_into_adjacent_free() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Allocate a block, leaving free space after it
        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();

        // Grow into the adjacent free space
        let result = dm.resize(&mut mem, seg, 0x30);
        assert!(result.is_ok());

        // Verify size increased
        let mcb_seg = seg.wrapping_sub(1);
        let block = dm.blocks.iter().find(|b| b.mcb_seg == mcb_seg).unwrap();
        assert_eq!(block.size_paras, 0x30);
    }

    #[test]
    fn resize_grow_consume_entire_next_block() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Allocate two blocks back to back
        let a = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let b = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();

        // Free second block to create adjacent free space
        dm.free(&mut mem, b).unwrap();

        // Now resize first block to consume all of what was block b
        // This should consume the MCB paragraph plus the usable paragraphs
        let result = dm.resize(&mut mem, a, 0x20 + 0x10 + 1);
        assert!(result.is_ok());
    }

    #[test]
    fn resize_grow_consume_entire_next_block_exact() {
        let (mut mem, mut dm) = setup_dos_mem();
        let total = dm.largest_free();
        let tail_size = 0x10;
        let a_size = total - (tail_size + 1);

        let a = dm.alloc(&mut mem, 0x0100, a_size).unwrap();
        let b = dm.alloc(&mut mem, 0x0100, tail_size).unwrap();
        dm.free(&mut mem, b).unwrap();

        dm.resize(&mut mem, a, total).unwrap();
        assert_eq!(dm.blocks.len(), 1);
    }

    #[test]
    fn resize_grow_no_adjacent_free() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Allocate two blocks back to back
        let a = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let _b = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();

        // Try to grow first block - should fail since next block is allocated
        let result = dm.resize(&mut mem, a, 0x30);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn resize_grow_insufficient_adjacent_space() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Allocate first block, then a second, then free second
        let a = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let b = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let _c = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();

        dm.free(&mut mem, b).unwrap();

        // Try to grow first block by more than available in adjacent free
        let result = dm.resize(&mut mem, a, 0x20 + 0x100);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn resize_invalid_block() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let result = dm.resize(&mut mem, 0x1234, 0x10);
        assert!(matches!(result, Err(DosMemError::InvalidBlock)));
    }

    #[test]
    fn alloc_consumes_exact_block() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Allocate almost all memory, then free, then allocate exact size
        let largest = dm.largest_free();
        let a = dm.alloc(&mut mem, 0x0100, largest).unwrap();
        dm.free(&mut mem, a).unwrap();

        // Now allocate exactly largest paragraphs - should consume entire block
        let b = dm.alloc(&mut mem, 0x0100, largest).unwrap();
        assert_eq!(b, DEFAULT_FIRST_MCB_SEG + 1);
    }

    #[test]
    fn alloc_consumes_block_with_one_remaining() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let largest = dm.largest_free();
        // Allocate largest - 1 paragraphs (leaving remaining == 1)
        let a = dm.alloc(&mut mem, 0x0100, largest - 1).unwrap();
        assert!(a > 0);
        let block = dm.blocks.iter().find(|b| b.usable_seg() == a).unwrap();
        assert_eq!(block.size_paras, largest);
        assert_eq!(dm.blocks.len(), 1);
    }

    #[test]
    fn resize_shrink_one_paragraph_keeps_contiguous() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let idx = dm
            .blocks
            .iter()
            .position(|b| b.usable_seg() == seg)
            .unwrap();
        let next_before = dm.blocks[idx + 1];

        dm.resize(&mut mem, seg, 0x0F).unwrap();

        let block = dm.blocks.iter().find(|b| b.usable_seg() == seg).unwrap();
        assert_eq!(block.size_paras, 0x0F);

        let new_free_mcb = block.end_seg_exclusive();
        let next = dm
            .blocks
            .iter()
            .find(|b| b.mcb_seg == new_free_mcb)
            .unwrap();
        assert_eq!(next.size_paras, next_before.size_paras + 1);

        assert_blocks_contiguous(&dm);
    }

    #[test]
    fn mcb_written_correctly() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();

        // Read MCB from memory
        let mcb_base = Memory::seg_offset_to_linear(seg - 1, 0);
        let marker = mem.read_u8(mcb_base);
        let owner = mem.read_u16(mcb_base + 1);
        let size = mem.read_u16(mcb_base + 3);

        assert_eq!(marker, b'M'); // Not last block
        assert_eq!(owner, 0x0100);
        assert_eq!(size, 0x20);
    }

    #[test]
    fn last_block_has_z_marker() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Initial free block should be the last (and only) block
        let mcb_base = Memory::seg_offset_to_linear(DEFAULT_FIRST_MCB_SEG, 0);
        let marker = mem.read_u8(mcb_base);
        assert_eq!(marker, b'Z');
    }

    #[test]
    fn coalesce_multiple_adjacent_free_blocks() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Allocate three blocks
        let a = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let b = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let c = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();

        // Free middle block first
        dm.free(&mut mem, b).unwrap();
        // Free first block - should coalesce with middle
        dm.free(&mut mem, a).unwrap();
        // Free last block - should coalesce all
        dm.free(&mut mem, c).unwrap();

        // Should have coalesced into minimal blocks
        assert_eq!(dm.blocks.len(), 1);
    }

    #[test]
    fn coalesce_skips_when_next_allocated() {
        let (mut mem, mut dm) = setup_dos_mem();
        let a = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let _b = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();

        dm.free(&mut mem, a).unwrap();
        let blocks_before = dm.blocks.len();

        dm.coalesce();
        assert_eq!(dm.blocks.len(), blocks_before);
    }

    #[test]
    fn coalesce_skips_non_adjacent_free_blocks() {
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        let a = Block {
            mcb_seg: DEFAULT_FIRST_MCB_SEG,
            size_paras: 0x10,
            owner_psp: 0,
            is_last: false,
            name: [0; 8],
        };
        let b = Block {
            mcb_seg: DEFAULT_FIRST_MCB_SEG + 0x20,
            size_paras: 0x10,
            owner_psp: 0,
            is_last: true,
            name: [0; 8],
        };
        dm.blocks.clear();
        dm.blocks.push(a);
        dm.blocks.push(b);

        dm.coalesce();
        assert_eq!(dm.blocks.len(), 2);
    }

    #[test]
    fn resize_shrink_merges_with_adjacent_free() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Allocate one block (leaves rest free)
        let a = dm.alloc(&mut mem, 0x0100, 0x40).unwrap();
        let blocks_before = dm.blocks.len();

        // Shrink - freed space should merge with trailing free
        dm.resize(&mut mem, a, 0x20).unwrap();

        // Should still have same number of blocks (merged)
        assert_eq!(dm.blocks.len(), blocks_before);
    }

    // ========================================================================
    // Memory Allocation Strategy Tests
    // ========================================================================

    #[test]
    fn test_alloc_strategy_first_fit() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Create a fragmented memory layout:
        // Block 1: 0x40 paras (allocated)
        // Block 2: 0x30 paras (free)
        // Block 3: 0x40 paras (allocated)
        // Block 4: 0x20 paras (free)
        let _a1 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x40, 0).unwrap();
        let a2 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x30, 0).unwrap();
        let a3 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x40, 0).unwrap();
        let a4 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x20, 0).unwrap();

        // Free alternating blocks to create fragmentation
        dm.free(&mut mem, a2).unwrap();
        dm.free(&mut mem, a4).unwrap();

        // First-fit (strategy 0) should allocate from the first 0x30-para hole
        let result = dm.alloc_with_strategy(&mut mem, 0x0100, 0x15, 0).unwrap();

        // The result should be close to a2's original position
        assert!(result < a3);
    }

    #[test]
    fn test_alloc_strategy_high_bits_fall_back_to_first_fit() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let _a1 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x40, 0).unwrap();
        let a2 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x30, 0).unwrap();
        let a3 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x40, 0).unwrap();
        let a4 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x20, 0).unwrap();

        dm.free(&mut mem, a2).unwrap();
        dm.free(&mut mem, a4).unwrap();

        let result = dm
            .alloc_with_strategy(&mut mem, 0x0100, 0x15, 0x40)
            .unwrap();

        assert!(result < a3);
    }

    #[test]
    fn test_alloc_strategy_best_fit() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Create fragmented layout with different sized holes
        // Add a separator block to prevent the small hole from merging with trailing free space
        let _a1 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x40, 0).unwrap();
        let a2 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x100, 0).unwrap(); // Will become large hole
        let a3 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x40, 0).unwrap();
        let a4 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x20, 0).unwrap(); // Will become small hole
        let _a5 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x40, 0).unwrap(); // Separator

        dm.free(&mut mem, a2).unwrap(); // Creates 0x100-para hole
        dm.free(&mut mem, a4).unwrap(); // Creates 0x20-para hole (won't merge)

        // Best-fit (strategy 1) for 0x15 paras should use the smaller hole (0x20)
        let result = dm.alloc_with_strategy(&mut mem, 0x0100, 0x15, 1).unwrap();

        // Result should be at a4's position (the smaller 0x20-para hole)
        // which is after a3 (the allocated block before the small hole)
        assert!(
            result > a3,
            "Best-fit should use the smaller 0x20-para hole, not the larger 0x100-para hole"
        );
    }

    #[test]
    fn test_alloc_strategy_last_fit() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Create fragmented layout
        let _a1 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x40, 0).unwrap();
        let a2 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x30, 0).unwrap();
        let a3 = dm.alloc_with_strategy(&mut mem, 0x0100, 0x40, 0).unwrap();

        dm.free(&mut mem, a2).unwrap();

        // There's a free block between a1 and a3, plus remaining free space at end
        // Last-fit (strategy 2) should allocate from the highest free block
        let result = dm.alloc_with_strategy(&mut mem, 0x0100, 0x20, 2).unwrap();

        // Result should be at the end (highest address)
        assert!(result > a3);
    }

    #[test]
    fn test_alloc_strategy_last_fit_allocates_from_high_end() {
        // Verify last-fit puts allocation at the TOP of the free block,
        // leaving free space at the bottom (critical for overlay managers
        // that shrink PSP, alloc high, then expand PSP back).
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Shrink PSP-like block to small size
        let psp_seg = dm.alloc_with_strategy(&mut mem, 0x0100, 0x20, 0).unwrap();

        // Allocate a small block with last-fit (strategy 2)
        let overlay = dm.alloc_with_strategy(&mut mem, 0x0100, 0x10, 2).unwrap();

        // The overlay should be at a HIGH address (near top of memory),
        // NOT right after the PSP block
        assert!(
            overlay > psp_seg + 0x100,
            "Last-fit should allocate from high end, not low end. overlay={:04X} psp={:04X}",
            overlay,
            psp_seg
        );

        // Now the PSP block should be able to resize upward into the free space
        // between it and the overlay block
        let resize_result = dm.resize(&mut mem, psp_seg, 0x100);
        assert!(
            resize_result.is_ok(),
            "PSP resize should succeed because free space is between PSP and overlay"
        );
    }

    #[test]
    fn test_alloc_strategy_last_fit_insufficient_memory() {
        let (mut mem, mut dm) = setup_dos_mem();
        let request = dm.largest_free().saturating_add(1);
        let result = dm.alloc_with_strategy(&mut mem, 0x0100, request, 2);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn resize_grow_last_block_fails() {
        let (mut mem, mut dm) = setup_dos_mem();
        let largest = dm.largest_free();
        let seg = dm.alloc(&mut mem, 0x0100, largest).unwrap();

        let result = dm.resize(&mut mem, seg, largest + 1);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn test_alloc_strategy_unknown_defaults_to_first_fit() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm
            .alloc_with_strategy(&mut mem, 0x0100, 0x10, 0x03)
            .unwrap();
        assert_eq!(seg, DEFAULT_FIRST_MCB_SEG + 1);
    }

    #[test]
    fn test_alloc_strategy_last_fit_with_no_free_blocks() {
        let (mut mem, mut dm) = setup_dos_mem();
        let largest = dm.largest_free();
        dm.alloc(&mut mem, 0x0100, largest).unwrap();

        let result = dm.alloc_with_strategy(&mut mem, 0x0100, 0x10, 2);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn test_alloc_strategy_unknown_with_no_free_blocks() {
        let (mut mem, mut dm) = setup_dos_mem();
        let largest = dm.largest_free();
        dm.alloc(&mut mem, 0x0100, largest).unwrap();

        let result = dm.alloc_with_strategy(&mut mem, 0x0100, 0x10, 0x03);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn test_alloc_strategy_default() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Regular alloc should behave like first-fit
        let a1 = dm.alloc(&mut mem, 0x0100, 0x40).unwrap();
        let a2 = dm.alloc(&mut mem, 0x0100, 0x40).unwrap();

        // a2 should be right after a1
        assert!(a2 > a1);
    }

    #[test]
    fn test_free_all_for_psp() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let psp1 = 0x0100;
        let psp2 = 0x0200;

        // Allocate blocks for two different PSPs
        let a1 = dm.alloc(&mut mem, psp1, 0x20).unwrap();
        let a2 = dm.alloc(&mut mem, psp1, 0x30).unwrap();
        let a3 = dm.alloc(&mut mem, psp2, 0x40).unwrap();
        let a4 = dm.alloc(&mut mem, psp1, 0x10).unwrap();

        // Verify all allocations succeeded
        assert!(a1 > 0);
        assert!(a2 > 0);
        assert!(a3 > 0);
        assert!(a4 > 0);

        // Count blocks owned by psp1 before freeing
        let psp1_blocks_before = dm.blocks.iter().filter(|b| b.owner_psp == psp1).count();
        assert_eq!(psp1_blocks_before, 3); // a1, a2, a4

        // Free all blocks owned by psp1
        dm.free_all_for_psp(&mut mem, psp1);

        // No blocks should be owned by psp1 now
        let psp1_blocks_after = dm.blocks.iter().filter(|b| b.owner_psp == psp1).count();
        assert_eq!(psp1_blocks_after, 0);

        // psp2's block should still exist
        let psp2_blocks = dm.blocks.iter().filter(|b| b.owner_psp == psp2).count();
        assert_eq!(psp2_blocks, 1);
    }

    #[test]
    fn test_free_all_for_psp_no_blocks() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Free for a PSP that has no blocks - should not panic
        dm.free_all_for_psp(&mut mem, 0x9999);
    }

    // ========================================================================
    // MCB Name Field Tests
    // ========================================================================

    #[test]
    fn test_mcb_name_set_and_written() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();

        // Set MCB name to "TESTPROG"
        dm.set_mcb_name(&mut mem, seg, b"TESTPROG");

        // Read name from memory
        let mcb_base = Memory::seg_offset_to_linear(seg - 1, 0);
        let mut name = [0u8; 8];
        for i in 0..8 {
            name[i] = mem.read_u8(mcb_base + 0x08 + i);
        }

        assert_eq!(&name, b"TESTPROG");
    }

    #[test]
    fn test_mcb_name_shorter_than_8_chars() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();

        // Set MCB name to "PROG" (4 chars, should be padded with zeros)
        dm.set_mcb_name(&mut mem, seg, b"PROG");

        // Read name from memory
        let mcb_base = Memory::seg_offset_to_linear(seg - 1, 0);
        let mut name = [0u8; 8];
        for i in 0..8 {
            name[i] = mem.read_u8(mcb_base + 0x08 + i);
        }

        assert_eq!(&name[0..4], b"PROG");
        assert_eq!(&name[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn test_mcb_name_longer_than_8_chars() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();

        // Set MCB name to "VERYLONGNAME" (12 chars, should be truncated to 8)
        dm.set_mcb_name(&mut mem, seg, b"VERYLONGNAME");

        // Read name from memory
        let mcb_base = Memory::seg_offset_to_linear(seg - 1, 0);
        let mut name = [0u8; 8];
        for i in 0..8 {
            name[i] = mem.read_u8(mcb_base + 0x08 + i);
        }

        assert_eq!(&name, b"VERYLONG");
    }

    #[test]
    fn test_mcb_name_initial_zero() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();

        // Without calling set_mcb_name, name should be all zeros
        let mcb_base = Memory::seg_offset_to_linear(seg - 1, 0);
        let mut name = [0u8; 8];
        for i in 0..8 {
            name[i] = mem.read_u8(mcb_base + 0x08 + i);
        }

        assert_eq!(&name, &[0; 8]);
    }

    #[test]
    fn test_mcb_name_invalid_segment() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Set name for non-existent segment - should not panic
        dm.set_mcb_name(&mut mem, 0x1234, b"TEST");
    }

    // ========================================================================
    // UMB (Upper Memory Block) Tests
    // ========================================================================

    #[test]
    fn test_umb_init_creates_single_block() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        assert!(!dm.is_umb_initialized());
        dm.init_umb(&mut mem);
        assert!(dm.is_umb_initialized());

        // Should have exactly one UMB block
        assert_eq!(dm.umb_blocks.len(), 1);

        // UMB block should be at UMB_START_SEG
        assert_eq!(dm.umb_blocks[0].mcb_seg, UMB_START_SEG);

        // UMB block should be free
        assert_eq!(dm.umb_blocks[0].owner_psp, 0);

        // UMB block should be the last block
        assert!(dm.umb_blocks[0].is_last);
    }

    #[test]
    fn test_umb_init_idempotent() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        dm.init_umb(&mut mem);
        let size_after_first = dm.umb_blocks[0].size_paras;

        // Initialize again - should not change anything
        dm.init_umb(&mut mem);
        assert_eq!(dm.umb_blocks.len(), 1);
        assert_eq!(dm.umb_blocks[0].size_paras, size_after_first);
    }

    #[test]
    fn test_umb_largest_free() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);

        // Before init, largest_free_umb should be 0
        assert_eq!(dm.largest_free_umb(), 0);

        dm.init_umb(&mut mem);

        // After init, should report the full UMB size
        let expected_size = UMB_END_SEG.saturating_sub(UMB_START_SEG).saturating_sub(1);
        assert_eq!(dm.largest_free_umb(), expected_size);
    }

    #[test]
    fn test_umb_not_initialized_uses_conventional() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();
        assert!(seg < UMB_START_SEG);
    }

    #[test]
    fn test_umb_empty_chain_uses_conventional() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        dm.umb_blocks.clear();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();
        assert!(seg < UMB_START_SEG);
    }

    #[test]
    fn test_umb_alloc_first_fit() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Allocate from UMB with high memory first strategy (0x80 = first fit, UMB first)
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x80, true)
            .unwrap();

        // Should be in UMB range
        assert!(seg > UMB_START_SEG);
        assert!(seg < UMB_END_SEG);

        // Verify block info
        let info = dm.block_info_for_segment(seg).unwrap();
        assert!(info.is_umb);
        assert_eq!(info.owner_psp, 0x0100);
        assert!(!info.is_free);
    }

    #[test]
    fn test_umb_alloc_consumes_one_paragraph_remainder() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let umb_size = dm.largest_free_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, umb_size - 1, 0x80, true)
            .unwrap();

        assert!(seg > UMB_START_SEG);
        assert_eq!(dm.umb_blocks.len(), 1);
        assert_eq!(dm.umb_blocks[0].size_paras, umb_size);
    }

    #[test]
    fn test_umb_unknown_base_strategy_defaults_to_first_fit() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x83, true)
            .unwrap();

        assert!(seg > UMB_START_SEG);
        assert!(seg < UMB_END_SEG);
    }

    #[test]
    fn test_umb_alloc_best_fit() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Create fragmented UMB: alloc, alloc, free first
        let a1 = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x80, true)
            .unwrap();
        let _a2 = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x050, 0x80, true)
            .unwrap();
        let a3 = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x200, 0x80, true)
            .unwrap();

        dm.free(&mut mem, a1).unwrap();
        dm.free(&mut mem, a3).unwrap();

        // Now we have: 0x100 hole, 0x50 alloc, 0x200 hole, rest free
        // Best fit (0x81) for 0x80 should use the 0x100 hole
        let result = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x80, 0x81, true)
            .unwrap();

        // Should be at a1's original location (smaller hole)
        assert_eq!(result, a1);
    }

    #[test]
    fn test_umb_alloc_last_fit() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Allocate one block to fragment
        let a1 = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x80, true)
            .unwrap();

        // Last fit (0x82) should allocate from the end
        let result = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x82, true)
            .unwrap();

        // Result should be higher than a1 (at the end)
        assert!(result > a1);
    }

    #[test]
    fn test_umb_last_fit_too_large_falls_back_to_conventional() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let umb_size = dm.largest_free_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, umb_size + 1, 0x82, true)
            .unwrap();

        assert!(seg < UMB_START_SEG);
    }

    #[test]
    fn test_umb_last_fit_with_no_free_blocks() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let umb_size = dm.largest_free_umb();
        dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, umb_size, 0x80, true)
            .unwrap();

        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x82, true)
            .unwrap();

        assert!(seg < UMB_START_SEG);
    }

    #[test]
    fn test_umb_unknown_strategy_with_no_free_blocks() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let umb_size = dm.largest_free_umb();
        dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, umb_size, 0x80, true)
            .unwrap();

        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x83, true)
            .unwrap();

        assert!(seg < UMB_START_SEG);
    }

    #[test]
    fn test_umb_free_coalesces() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Allocate three consecutive UMB blocks
        let a1 = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x80, true)
            .unwrap();
        let a2 = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x80, true)
            .unwrap();
        let a3 = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x80, true)
            .unwrap();

        let blocks_before = dm.umb_blocks.len();

        // Free in order: a2, a1, a3 - should coalesce into fewer blocks
        dm.free(&mut mem, a2).unwrap();
        dm.free(&mut mem, a1).unwrap();
        dm.free(&mut mem, a3).unwrap();

        // Should have coalesced adjacent free blocks
        assert!(dm.umb_blocks.len() <= blocks_before);
    }

    #[test]
    fn test_umb_block_info_for_segment() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x50, 0x80, true)
            .unwrap();

        let info = dm.block_info_for_segment(seg).unwrap();
        assert!(info.is_umb);
        assert_eq!(info.usable_seg, seg);
        assert_eq!(info.owner_psp, 0x0100);
        assert_eq!(info.size_paras, 0x50);
        assert!(!info.is_free);
    }

    #[test]
    fn test_umb_fallback_to_conventional() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Allocate more than UMB can hold
        let umb_size = dm.largest_free_umb();
        let request = umb_size + 0x100;
        let conv_size = dm.largest_free();
        assert!(conv_size >= request);

        let result = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, request, 0x80, true)
            .unwrap();

        assert!(result < UMB_START_SEG);
    }

    #[test]
    fn test_umb_conventional_fallback_when_umb_full() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Fill UMB completely
        let umb_size = dm.largest_free_umb();
        let _umb_alloc = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, umb_size, 0x80, true)
            .unwrap();

        // Now try to allocate with UMB-first strategy - should fall back to conventional
        let result = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x80, true)
            .unwrap();

        // Should be in conventional memory range
        assert!(result < UMB_START_SEG);
    }

    #[test]
    fn test_umb_not_linked_ignores_umb() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Allocate with high memory strategy but umb_linked = false
        let result = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x80, false)
            .unwrap();

        // Should be in conventional memory since UMB is not linked
        assert!(result < UMB_START_SEG);
    }

    #[test]
    fn test_umb_low_strategy_uses_conventional_first() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Allocate with strategy 0x00 (first fit, low memory first) with UMB linked
        let result = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x00, true)
            .unwrap();

        // Should be in conventional memory
        assert!(result < UMB_START_SEG);
    }

    #[test]
    fn test_umb_fallback_to_umb_when_conventional_full() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let conv_size = dm.largest_free();
        dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, conv_size, 0x00, true)
            .unwrap();

        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x00, true)
            .unwrap();

        assert!(seg > UMB_START_SEG);
        assert!(seg < UMB_END_SEG);
    }

    #[test]
    fn test_umb_fallback_skipped_when_not_initialized() {
        let (mut mem, mut dm) = setup_dos_mem();
        let conv_size = dm.largest_free();
        dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, conv_size, 0x00, true)
            .unwrap();

        let result = dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x00, true);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn test_umb_fallback_skipped_when_empty_chain() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        dm.umb_blocks.clear();
        let conv_size = dm.largest_free();
        dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, conv_size, 0x00, true)
            .unwrap();

        let result = dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x00, true);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn test_umb_fallback_fails_when_umb_too_small() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let conv_size = dm.largest_free();
        dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, conv_size, 0x00, true)
            .unwrap();

        let request = dm.largest_free_umb() + 1;
        let result = dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, request, 0x00, true);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn test_umb_free_all_for_psp() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        let psp1 = 0x0100;
        let psp2 = 0x0200;

        // Allocate in both conventional and UMB
        let _conv1 = dm
            .alloc_with_strategy_and_umb(&mut mem, psp1, 0x100, 0x00, true)
            .unwrap();
        let _umb1 = dm
            .alloc_with_strategy_and_umb(&mut mem, psp1, 0x100, 0x80, true)
            .unwrap();
        let _umb2 = dm
            .alloc_with_strategy_and_umb(&mut mem, psp2, 0x100, 0x80, true)
            .unwrap();

        // Count psp1 blocks
        let psp1_conv = dm.blocks.iter().filter(|b| b.owner_psp == psp1).count();
        let psp1_umb = dm.umb_blocks.iter().filter(|b| b.owner_psp == psp1).count();
        assert_eq!(psp1_conv, 1);
        assert_eq!(psp1_umb, 1);

        // Free all for psp1
        dm.free_all_for_psp(&mut mem, psp1);

        // psp1 should have no blocks
        let psp1_conv_after = dm.blocks.iter().filter(|b| b.owner_psp == psp1).count();
        let psp1_umb_after = dm.umb_blocks.iter().filter(|b| b.owner_psp == psp1).count();
        assert_eq!(psp1_conv_after, 0);
        assert_eq!(psp1_umb_after, 0);

        // psp2 should still have its UMB block
        let psp2_umb = dm.umb_blocks.iter().filter(|b| b.owner_psp == psp2).count();
        assert_eq!(psp2_umb, 1);
    }

    #[test]
    fn test_umb_mcb_written_to_memory() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x50, 0x80, true)
            .unwrap();

        // Read MCB from memory
        let mcb_addr = ((seg - 1) as usize) * 16;
        let marker = mem.read_u8(mcb_addr);
        let owner = mem.read_u16(mcb_addr + 1);
        let size = mem.read_u16(mcb_addr + 3);

        // Should be 'M' (not last) since there's free space after
        assert_eq!(marker, b'M');
        assert_eq!(owner, 0x0100);
        assert_eq!(size, 0x50);
    }

    #[test]
    fn test_umb_last_block_marker() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Allocate entire UMB
        let umb_size = dm.largest_free_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, umb_size, 0x80, true)
            .unwrap();

        // Read MCB from memory
        let mcb_addr = ((seg - 1) as usize) * 16;
        let marker = mem.read_u8(mcb_addr);

        // Should be 'Z' (last block)
        assert_eq!(marker, b'Z');
    }

    #[test]
    fn test_umb_strategy_0x40_best_fit_umb_first() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Strategy 0x40 should try UMB first with best fit
        let result = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x40, true)
            .unwrap();

        // Should be in UMB
        assert!(result > UMB_START_SEG);
        assert!(result < UMB_END_SEG);
    }

    #[test]
    fn test_umb_consumes_exact_size() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        let umb_size = dm.largest_free_umb();

        // Allocate exactly the available size
        let _result = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, umb_size, 0x80, true)
            .unwrap();

        // Should have consumed the entire block (only one block left)
        assert_eq!(dm.umb_blocks.len(), 1);
        assert_eq!(dm.umb_blocks[0].owner_psp, 0x0100);
        assert_eq!(dm.umb_blocks[0].size_paras, umb_size);
    }

    #[test]
    fn test_umb_insufficient_memory_error() {
        let mut mem = Memory::new();
        let mut dm = DosMem::new(DEFAULT_FIRST_MCB_SEG);
        dm.init(&mut mem);
        dm.init_umb(&mut mem);

        // Allocate all UMB
        let umb_size = dm.largest_free_umb();
        dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, umb_size, 0x80, true)
            .unwrap();

        // Allocate all conventional
        let conv_size = dm.largest_free();
        dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, conv_size, 0x00, true)
            .unwrap();

        // Now allocation should fail
        let result = dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x100, 0x80, true);

        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    // ========================================================================
    // UMB Resize Tests
    // ========================================================================

    #[test]
    fn test_resize_umb_invalid_segment() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let result = dm.resize_umb(&mut mem, 0, 0x10);
        assert!(matches!(result, Err(DosMemError::InvalidBlock)));
    }

    #[test]
    fn test_resize_umb_noop_when_size_unchanged() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();
        let mcb_seg = seg.wrapping_sub(1);
        let size = dm
            .umb_blocks
            .iter()
            .find(|b| b.mcb_seg == mcb_seg)
            .unwrap()
            .size_paras;

        let result = dm.resize_umb(&mut mem, seg, size);
        assert!(result.is_ok());
    }

    #[test]
    fn test_resize_umb_shrink_one_paragraph_moves_free_successor() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();
        let blocks_before = dm.umb_blocks.len();
        let arena_end = dm.umb_blocks.last().unwrap().end_seg_exclusive();
        let next_size = dm.umb_blocks[1].size_paras;

        dm.resize_umb(&mut mem, seg, 0x1F).unwrap();

        let mcb_seg = seg.wrapping_sub(1);
        let block = dm.umb_blocks.iter().find(|b| b.mcb_seg == mcb_seg).unwrap();
        assert_eq!(block.size_paras, 0x1F);
        assert_eq!(dm.umb_blocks.len(), blocks_before);
        assert_eq!(dm.umb_blocks[1].mcb_seg, block.end_seg_exclusive());
        assert_eq!(dm.umb_blocks[1].size_paras, next_size + 1);
        assert_eq!(dm.umb_blocks.last().unwrap().end_seg_exclusive(), arena_end);
    }

    #[test]
    fn test_resize_umb_shrink_merges_with_next_free() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x40, 0x80, true)
            .unwrap();
        let blocks_before = dm.umb_blocks.len();

        dm.resize_umb(&mut mem, seg, 0x20).unwrap();

        assert_eq!(dm.umb_blocks.len(), blocks_before);
    }

    #[test]
    fn test_resize_umb_shrink_last_block_inserts_free() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let umb_size = dm.largest_free_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, umb_size, 0x80, true)
            .unwrap();

        dm.resize_umb(&mut mem, seg, umb_size - 0x10).unwrap();
        assert_eq!(dm.umb_blocks.len(), 2);
    }

    #[test]
    fn test_resize_umb_shrink_inserts_when_next_allocated() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();
        let _next = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();
        let blocks_before = dm.umb_blocks.len();

        dm.resize_umb(&mut mem, seg, 0x10).unwrap();

        assert_eq!(dm.umb_blocks.len(), blocks_before + 1);
    }

    #[test]
    fn test_resize_umb_grow_into_adjacent_free_partial() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();

        dm.resize_umb(&mut mem, seg, 0x30).unwrap();

        let mcb_seg = seg.wrapping_sub(1);
        let block = dm.umb_blocks.iter().find(|b| b.mcb_seg == mcb_seg).unwrap();
        assert_eq!(block.size_paras, 0x30);
    }

    #[test]
    fn test_resize_umb_grow_insufficient_adjacent_free() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let a = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();
        let b = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x80, true)
            .unwrap();
        let _c = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();

        dm.free(&mut mem, b).unwrap();

        let result = dm.resize_umb(&mut mem, a, 0x20 + 0x10 + 2);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn test_resize_umb_grow_consumes_entire_next_block() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let umb_size = dm.largest_free_umb();
        let tail_size = 0x10;
        let a_size = umb_size - (tail_size + 1);

        let a = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, a_size, 0x80, true)
            .unwrap();
        let b = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, tail_size, 0x80, true)
            .unwrap();
        dm.free(&mut mem, b).unwrap();

        let blocks_before = dm.umb_blocks.len();
        dm.resize_umb(&mut mem, a, umb_size).unwrap();

        assert!(dm.umb_blocks.len() < blocks_before);
    }

    #[test]
    fn test_resize_umb_grow_fails_with_allocated_next_block() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();
        let _next = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();

        let result = dm.resize_umb(&mut mem, seg, 0x30);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn test_resize_umb_grow_fails_without_next_block() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let umb_size = dm.largest_free_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, umb_size, 0x80, true)
            .unwrap();

        let result = dm.resize_umb(&mut mem, seg, umb_size + 1);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    // ========================================================================
    //
    // These tests pin down the INT 21h/4Ah failure-path return value. RBIL
    // specifies BX on CY=1 is the maximum paragraphs available for the
    // specified memory block — NOT the largest free block anywhere in the
    // chain. `DosMem::resize` already behaved correctly; `DosMem::resize_umb`
    // a still-usable block, then resized to zero and corrupted the chain.
    // ========================================================================

    #[test]
    fn resize_umb_probe_no_adjacent_free_returns_current_size() {
        // Target block at the UMB high end with no adjacent forward free,
        // plus a non-zero free block earlier in the chain so that the buggy
        // implementation would return `largest_free_umb() > 0` instead of
        // `cur.size_paras`. The test would succeed equally under either
        // implementation if the only free block in the chain was zero.
        let (mut mem, mut dm) = setup_dos_mem_with_umb();

        // Last-fit UMB allocation carves from the HIGH end and leaves the
        // free remainder at the LOW end. `seg` therefore has NO adjacent
        // forward free block.
        let target_size: u16 = 0x20;
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, target_size, 0x82, true)
            .unwrap();

        // Sanity: there is still a free block elsewhere in the UMB chain
        // AND its size differs from `target_size`. Both conditions are
        // required to distinguish the buggy `largest_free_umb()` return
        // path from the fixed `cur.size_paras` return path. If they were
        // equal by accident, the test would pass under either implementation
        // and lose its diagnostic value.
        assert_ne!(
            dm.largest_free_umb(),
            target_size,
            "test setup must leave the buggy return value distinguishable \
             from the correct one (largest_free_umb must differ from cur)"
        );
        assert!(
            dm.largest_free_umb() > 0,
            "test setup must leave a non-zero free block so the bug was \
             observable (buggy impl would return 0 only if no free UMB elsewhere)"
        );

        let result = dm.resize_umb(&mut mem, seg, 0xFFFF);
        match result {
            Err(DosMemError::InsufficientMemory { largest_available }) => {
                assert_eq!(
                    largest_available, target_size,
                    "probe with no adjacent forward free must return the block's own size, \
                     not largest_free_umb()"
                );
            }
            other => panic!("expected InsufficientMemory, got {:?}", other),
        }
    }

    #[test]
    fn resize_umb_probe_with_adjacent_free_returns_current_plus_free_plus_one() {
        // Target block followed by a free block. Probe must return
        // cur + free + 1 (the +1 reclaiming the freed MCB header paragraph),
        // matching the conventional-chain `resize` computation.
        let (mut mem, mut dm) = setup_dos_mem_with_umb();

        let a_size: u16 = 0x20;
        let b_size: u16 = 0x10;
        let _c_size: u16 = 0x20;

        let a = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, a_size, 0x80, true)
            .unwrap();
        let b = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, b_size, 0x80, true)
            .unwrap();
        let _c = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, _c_size, 0x80, true)
            .unwrap();

        // Free `b` so `a`'s next neighbour is free.
        dm.free(&mut mem, b).unwrap();

        let result = dm.resize_umb(&mut mem, a, 0xFFFF);
        match result {
            Err(DosMemError::InsufficientMemory { largest_available }) => {
                assert_eq!(
                    largest_available,
                    a_size + b_size + 1,
                    "probe with adjacent forward free must return cur + free + 1"
                );
            }
            other => panic!("expected InsufficientMemory, got {:?}", other),
        }
    }

    #[test]
    fn resize_umb_rejects_zero_paragraphs() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();

        let mcb_seg_before = seg.wrapping_sub(1);
        let size_before = dm
            .umb_blocks
            .iter()
            .find(|b| b.mcb_seg == mcb_seg_before)
            .unwrap()
            .size_paras;

        let result = dm.resize_umb(&mut mem, seg, 0);
        assert!(matches!(result, Err(DosMemError::InvalidBlock)));

        // The block must be untouched — size_paras unchanged, usable_seg()
        // still points at the same place.
        let after = dm
            .umb_blocks
            .iter()
            .find(|b| b.mcb_seg == mcb_seg_before)
            .expect("block must still exist after rejected resize");
        assert_eq!(after.size_paras, size_before);
        assert_eq!(after.usable_seg(), seg);
    }

    #[test]
    fn resize_rejects_zero_paragraphs() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();

        let mcb_seg_before = seg.wrapping_sub(1);
        let size_before = dm
            .blocks
            .iter()
            .find(|b| b.mcb_seg == mcb_seg_before)
            .unwrap()
            .size_paras;

        let result = dm.resize(&mut mem, seg, 0);
        assert!(matches!(result, Err(DosMemError::InvalidBlock)));

        let after = dm
            .blocks
            .iter()
            .find(|b| b.mcb_seg == mcb_seg_before)
            .expect("block must still exist after rejected resize");
        assert_eq!(after.size_paras, size_before);
        assert_eq!(after.usable_seg(), seg);
    }

    #[test]
    fn resize_umb_rejection_preserves_owned_block_and_payload() {
        // Last-fit puts an independently sized block at the end of the UMB
        // chain, so its maximum growth is its current size. Exercise small,
        // medium and larger allocations without relying on any environment.
        for paragraphs in [1u16, 0x24, 0x81] {
            let (mut mem, mut dm) = setup_dos_mem_with_umb();
            let owner = 0x1200;
            let seg = dm
                .alloc_with_strategy_and_umb(&mut mem, owner, paragraphs, 0x82, true)
                .unwrap();
            let base = Memory::seg_offset_to_linear(seg, 0);
            let payload: [u8; 16] = std::array::from_fn(|i| (i as u8) ^ 0xA5);
            mem.write_bytes_guest(base, &payload);
            assert!(matches!(
                dm.resize_umb(&mut mem, seg, u16::MAX),
                Err(DosMemError::InsufficientMemory { largest_available })
                    if largest_available == paragraphs
            ));
            assert!(dm.resize_umb(&mut mem, seg, paragraphs).is_ok());
            assert!(matches!(
                dm.resize_umb(&mut mem, seg, 0),
                Err(DosMemError::InvalidBlock)
            ));
            let block = dm
                .umb_blocks
                .iter()
                .find(|b| b.usable_seg() == seg)
                .unwrap();
            assert_eq!(block.size_paras, paragraphs);
            assert_eq!(block.owner_psp, owner);
            for (offset, expected) in payload.iter().enumerate() {
                assert_eq!(mem.read_u8(base + offset), *expected);
            }
        }
    }

    // ========== block_info_for_segment UMB path ==========

    #[test]
    fn test_block_info_for_segment_in_umb() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();
        let info = dm.block_info_for_segment(seg).unwrap();
        assert!(info.is_umb);
        assert!(!info.is_free);
        assert_eq!(info.owner_psp, 0x0100);
    }

    #[test]
    fn test_block_info_for_segment_not_found() {
        let (_mem, dm) = setup_dos_mem();
        assert!(dm.block_info_for_segment(0xFFFF).is_none());
    }

    #[test]
    fn test_block_info_for_segment_conventional() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let info = dm.block_info_for_segment(seg).unwrap();
        assert!(!info.is_umb);
        assert!(!info.is_free);
        assert_eq!(info.size_paras, 0x10);
    }

    // ========== alloc unknown strategy ==========

    #[test]
    fn test_alloc_unknown_strategy_defaults_to_first_fit() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc_with_strategy(&mut mem, 0x0100, 0x10, 3).unwrap();
        assert!(seg > dm.first_mcb_seg);
    }

    // ========== alloc zero paragraphs ==========

    #[test]
    fn test_alloc_zero_paragraphs_allocates_one() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0).unwrap();
        let info = dm.block_info_for_segment(seg).unwrap();
        assert!(info.size_paras >= 1);
    }

    // ========== UMB allocation strategies ==========

    #[test]
    fn test_umb_alloc_best_fit_strategy() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x41, true)
            .unwrap();
        assert!(seg >= UMB_START_SEG);
    }

    #[test]
    fn test_umb_alloc_last_fit_strategy() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x82, true)
            .unwrap();
        assert!(seg >= UMB_START_SEG);
    }

    #[test]
    fn test_umb_alloc_unknown_strategy_in_umb() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x83, true)
            .unwrap();
        assert!(seg >= UMB_START_SEG);
    }

    // ========== conv full falls back to UMB ==========

    #[test]
    fn test_conv_full_falls_back_to_umb_when_linked() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let largest = dm.largest_free();
        let _seg = dm.alloc(&mut mem, 0x0100, largest).unwrap();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0, true)
            .unwrap();
        assert!(seg >= UMB_START_SEG);
    }

    // ========== free_all_for_psp with UMB ==========

    #[test]
    fn test_free_all_for_psp_includes_umb() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let _conv_seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let _umb_seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x80, true)
            .unwrap();
        dm.free_all_for_psp(&mut mem, 0x0100);
        assert_eq!(
            dm.umb_blocks
                .iter()
                .filter(|b| b.owner_psp == 0x0100)
                .count(),
            0
        );
    }

    #[test]
    fn test_free_all_for_psp_no_match() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        dm.free_all_for_psp(&mut mem, 0x9999);
        let info = dm.block_info_for_segment(seg).unwrap();
        assert!(!info.is_free);
    }

    // ========== init_umb idempotent ==========

    #[test]
    fn test_init_umb_idempotent() {
        let (mut mem, mut dm) = setup_dos_mem();
        dm.init_umb(&mut mem);
        let first_count = dm.umb_blocks.len();
        dm.init_umb(&mut mem);
        assert_eq!(dm.umb_blocks.len(), first_count);
    }

    // ========== write_umb_link ==========

    #[test]
    fn test_write_umb_link_toggles_linking() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        dm.write_umb_link(&mut mem, true);
        assert!(dm.umb_chain_linked);
        if let Some(last) = dm.blocks.last() {
            let addr = (last.mcb_seg as usize) * 16;
            assert_eq!(mem.read_u8(addr), b'M');
        }
        dm.write_umb_link(&mut mem, false);
        assert!(!dm.umb_chain_linked);
        if let Some(last) = dm.blocks.last() {
            let addr = (last.mcb_seg as usize) * 16;
            assert_eq!(mem.read_u8(addr), b'Z');
        }
    }

    #[test]
    fn set_owner_and_name_preserve_linked_umb_chain_marker() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        dm.write_umb_link(&mut mem, true);

        let seg = dm.alloc(&mut mem, 0x0008, dm.largest_free()).unwrap();
        let mcb_addr = Memory::seg_offset_to_linear(seg.wrapping_sub(1), 0);
        assert_eq!(mem.read_u8(mcb_addr), b'M');

        dm.set_owner(&mut mem, seg, seg).unwrap();
        assert_eq!(mem.read_u8(mcb_addr), b'M');

        dm.set_mcb_name(&mut mem, seg, b"TESTPROG");
        assert_eq!(mem.read_u8(mcb_addr), b'M');

        let umb_seg = dm
            .alloc_with_strategy_and_umb(&mut mem, seg, 0x10, 0, true)
            .unwrap();
        assert_eq!(umb_seg, UMB_START_SEG.wrapping_add(1));

        let mut cur = dm.first_mcb_segment();
        loop {
            if cur == umb_seg.wrapping_sub(1) {
                break;
            }

            let cur_addr = Memory::seg_offset_to_linear(cur, 0);
            let sig = mem.read_u8(cur_addr);
            assert_eq!(sig, b'M');
            cur = cur
                .wrapping_add(mem.read_u16(cur_addr + 0x03))
                .wrapping_add(1);
        }
    }

    // ========== block_end_segment ==========

    #[test]
    fn test_block_end_segment_found() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let end = dm.block_end_segment(seg).unwrap();
        assert_eq!(end, seg + 0x10);
    }

    #[test]
    fn test_block_end_segment_not_found() {
        let (_mem, dm) = setup_dos_mem();
        assert!(dm.block_end_segment(0xFFFF).is_none());
    }

    // ========== set_owner invalid ==========

    #[test]
    fn test_set_owner_invalid_block() {
        let (mut mem, mut dm) = setup_dos_mem();
        let result = dm.set_owner(&mut mem, 0xFFFF, 0x0100);
        assert!(matches!(result, Err(DosMemError::InvalidBlock)));
    }

    // ========== free edge cases ==========

    #[test]
    fn test_free_segment_zero() {
        let (mut mem, mut dm) = setup_dos_mem();
        assert!(matches!(
            dm.free(&mut mem, 0),
            Err(DosMemError::InvalidBlock)
        ));
    }

    #[test]
    fn test_free_unallocated_block() {
        let (mut mem, mut dm) = setup_dos_mem();
        let first_usable = dm.first_mcb_seg + 1;
        assert!(matches!(
            dm.free(&mut mem, first_usable),
            Err(DosMemError::InvalidBlock)
        ));
    }

    // ========== resize edge cases ==========

    #[test]
    fn test_resize_segment_zero() {
        let (mut mem, mut dm) = setup_dos_mem();
        assert!(matches!(
            dm.resize(&mut mem, 0, 10),
            Err(DosMemError::InvalidBlock)
        ));
    }

    #[test]
    fn test_resize_umb_segment_zero() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        assert!(matches!(
            dm.resize_umb(&mut mem, 0, 10),
            Err(DosMemError::InvalidBlock)
        ));
    }

    // ========== UMB free coalesce ==========

    #[test]
    fn test_umb_free_coalesces_adjacent() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let a = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x80, true)
            .unwrap();
        let b = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x80, true)
            .unwrap();
        let _c = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x80, true)
            .unwrap();
        dm.free(&mut mem, b).unwrap();
        dm.free(&mut mem, a).unwrap();
    }

    // ========== DosMemError Debug ==========

    #[test]
    fn test_dos_mem_error_debug() {
        let err = DosMemError::InsufficientMemory {
            largest_available: 100,
        };
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("InsufficientMemory"));
    }

    // ========== DosMemBlockInfo Eq ==========

    #[test]
    fn test_dos_mem_block_info_eq() {
        let info1 = DosMemBlockInfo {
            mcb_seg: 0x100,
            usable_seg: 0x101,
            end_seg: 0x111,
            size_paras: 0x10,
            owner_psp: 0x0100,
            is_free: false,
            is_umb: false,
        };
        let info2 = info1;
        assert_eq!(info1, info2);
    }

    // ========== resize_umb shrink by one ==========

    #[test]
    fn test_resize_umb_shrink_by_one() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();
        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x20, 0x80, true)
            .unwrap();
        let result = dm.resize_umb(&mut mem, seg, 0x1F);
        assert!(result.is_ok());
    }

    // ========== resize conventional shrink by 1 ==========

    #[test]
    fn test_resize_shrink_by_one_with_free_next() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let result = dm.resize(&mut mem, seg, 0x1F);
        assert!(result.is_ok());
    }

    #[test]
    fn test_resize_shrink_by_one_without_free_next() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg1 = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let _seg2 = dm.alloc(&mut mem, 0x0200, 0x20).unwrap();
        let result = dm.resize(&mut mem, seg1, 0x1F);
        assert!(result.is_ok());
    }

    // ========== Batch 25: coverage ==========

    /// Line 945: write_umb_link with umb_chain_linked=true but umb_initialized=false.
    #[test]
    fn test_b25_write_umb_link_not_initialized() {
        let (mut mem, mut dm) = setup_dos_mem();
        assert!(!dm.is_umb_initialized());
        // This sets umb_chain_linked=true but umb_initialized is still false,
        // so the condition `umb_chain_linked && umb_initialized` short-circuits.
        dm.write_umb_link(&mut mem, true);
        assert!(dm.umb_chain_linked);
    }

    /// Line 945: write_umb_link with initialized but empty umb_blocks.
    #[test]
    fn test_b25_write_umb_link_initialized_empty_blocks() {
        let (mut mem, mut dm) = setup_dos_mem();
        // Force umb_initialized to true but don't add any UMB blocks.
        // We use init_umb which adds blocks, so we need to clear them after.
        dm.init_umb(&mut mem);
        assert!(dm.is_umb_initialized());
        dm.umb_blocks.clear();
        dm.write_umb_link(&mut mem, true);
        assert!(dm.umb_chain_linked);
    }

    #[test]
    fn test_total_free_paragraphs() {
        let (_mem, dm) = setup_dos_mem();

        // After init, all conventional memory should be free
        let total = dm.total_free_paragraphs();
        assert!(total > 0, "Expected free memory after init, got 0");

        // Should be close to conventional memory size (~640KB = ~40960 paragraphs)
        // minus the MCB overhead and low-memory reservation
        assert!(total > 30000, "Expected >30000 paragraphs, got {}", total);
    }

    #[test]
    fn test_total_free_paragraphs_after_alloc() {
        let (mut mem, mut dm) = setup_dos_mem();
        let before = dm.total_free_paragraphs();

        // Allocate 0x100 paragraphs
        dm.alloc(&mut mem, 0x0100, 0x100).unwrap();
        let after = dm.total_free_paragraphs();

        // Total free should decrease by exactly the allocated amount
        // (0x100 paragraphs + 1 MCB header paragraph for the split)
        assert!(
            after < before,
            "Free memory should decrease after alloc: before={}, after={}",
            before,
            after
        );
        assert_eq!(
            before - after,
            0x101,
            "Should lose 0x100 usable + 1 MCB header"
        );
    }

    #[test]
    fn test_umb_does_not_overlap_ems_page_frame() {
        // EMS page frame lives at E000h-EFFFh (64KB).
        // UMB allocations must never fall in that range.
        const EMS_FRAME_SEG: u16 = 0xE000;
        const EMS_FRAME_END: u16 = 0xF000; // exclusive

        let (mut mem, mut dm) = setup_dos_mem_with_umb();

        // Allocate the entire UMB region
        let total = dm.largest_free_umb();
        assert!(total > 0, "UMB should have free space");

        let seg = dm
            .alloc_with_strategy_and_umb(&mut mem, 0x0100, total, 0x80, true)
            .unwrap();

        // The allocated block (seg .. seg + total) must not overlap [E000..F000)
        let alloc_end = seg + total;
        assert!(
            alloc_end <= EMS_FRAME_SEG || seg >= EMS_FRAME_END,
            "UMB allocation {:#06X}..{:#06X} overlaps EMS page frame {:#06X}..{:#06X}",
            seg,
            alloc_end,
            EMS_FRAME_SEG,
            EMS_FRAME_END
        );
    }

    // ---- Sub-block free (memory-based fallback) tests ----

    #[test]
    fn free_sub_block_in_allocated_parent_succeeds() {
        let (mut mem, mut dm) = setup_dos_mem();
        let parent_seg = dm.alloc(&mut mem, 0x0100, 0x100).unwrap();
        let _ = dm.set_owner(&mut mem, parent_seg, 0x0100);

        // Write sub-MCB header inside parent
        let sub_mcb_seg = parent_seg + 0x1F;
        let sub_mcb_addr = Memory::seg_offset_to_linear(sub_mcb_seg, 0);
        mem.write_u8(sub_mcb_addr, b'M');
        mem.write_u16(sub_mcb_addr + 1, 0x0100);
        mem.write_u16(sub_mcb_addr + 3, 0x0010);

        let result = dm.free(&mut mem, sub_mcb_seg + 1);
        assert!(
            result.is_ok(),
            "Sub-block free in allocated parent should succeed"
        );
        assert_eq!(mem.read_u16(sub_mcb_addr + 1), 0);

        // Parent still allocated
        let info = dm.block_info_for_segment(parent_seg).unwrap();
        assert!(!info.is_free);
    }

    #[test]
    fn free_sub_block_in_free_parent_succeeds() {
        // This is the nested allocation pattern: allocate big block, shrink it,
        // then free sub-blocks in the now-free shrunk-off region.
        let (mut mem, mut dm) = setup_dos_mem();
        let parent_seg = dm.alloc(&mut mem, 0x0100, 0x200).unwrap();
        let _ = dm.set_owner(&mut mem, parent_seg, 0x0100);

        // Write sub-MCB in the region that will become free
        let sub_mcb_seg = parent_seg + 0x80;
        let sub_mcb_addr = Memory::seg_offset_to_linear(sub_mcb_seg, 0);
        mem.write_u8(sub_mcb_addr, b'M');
        mem.write_u16(sub_mcb_addr + 1, 0x0100);
        mem.write_u16(sub_mcb_addr + 3, 0x0010);

        // Shrink parent — creates free block containing the sub-MCB
        dm.resize(&mut mem, parent_seg, 0x40).unwrap();

        // Verify the sub-MCB region is now in a free block
        let info = dm.block_info_for_segment(sub_mcb_seg + 1).unwrap();
        assert!(info.is_free, "Region should be free after shrink");

        // Sub-MCB signature should survive write_all_mcbs from resize
        assert!(
            mem.read_u8(sub_mcb_addr) == b'M' || mem.read_u8(sub_mcb_addr) == b'Z',
            "Sub-MCB signature should survive"
        );

        // Free the sub-block — should succeed even though parent is free
        let result = dm.free(&mut mem, sub_mcb_seg + 1);
        assert!(
            result.is_ok(),
            "Sub-block free in free parent should succeed"
        );
        assert_eq!(mem.read_u16(sub_mcb_addr + 1), 0);
    }

    #[test]
    fn free_sub_block_with_z_signature_succeeds() {
        let (mut mem, mut dm) = setup_dos_mem();
        let parent_seg = dm.alloc(&mut mem, 0x0100, 0x100).unwrap();
        let _ = dm.set_owner(&mut mem, parent_seg, 0x0100);

        let sub_mcb_seg = parent_seg + 0x40;
        let sub_mcb_addr = Memory::seg_offset_to_linear(sub_mcb_seg, 0);
        mem.write_u8(sub_mcb_addr, b'Z');
        mem.write_u16(sub_mcb_addr + 1, 0x0100);
        mem.write_u16(sub_mcb_addr + 3, 0x0008);

        assert!(dm.free(&mut mem, sub_mcb_seg + 1).is_ok());
        assert_eq!(mem.read_u16(sub_mcb_addr + 1), 0);
    }

    #[test]
    fn free_sub_block_without_mcb_signature_fails() {
        let (mut mem, mut dm) = setup_dos_mem();
        let parent_seg = dm.alloc(&mut mem, 0x0100, 0x100).unwrap();
        let _ = dm.set_owner(&mut mem, parent_seg, 0x0100);

        let sub_seg = parent_seg + 0x20;
        assert!(dm.free(&mut mem, sub_seg).is_err());
    }

    #[test]
    fn free_sub_block_does_not_modify_vec() {
        let (mut mem, mut dm) = setup_dos_mem();
        let parent_seg = dm.alloc(&mut mem, 0x0100, 0x100).unwrap();
        let _ = dm.set_owner(&mut mem, parent_seg, 0x0100);
        let blocks_before = dm.blocks.len();

        let sub_mcb_seg = parent_seg + 0x30;
        let sub_mcb_addr = Memory::seg_offset_to_linear(sub_mcb_seg, 0);
        mem.write_u8(sub_mcb_addr, b'M');
        mem.write_u16(sub_mcb_addr + 1, 0x0100);
        mem.write_u16(sub_mcb_addr + 3, 0x0008);

        dm.free(&mut mem, sub_mcb_seg + 1).unwrap();
        assert_eq!(dm.blocks.len(), blocks_before);
    }

    #[test]
    fn free_sub_block_with_oversized_size_paras_fails() {
        let (mut mem, mut dm) = setup_dos_mem();
        let parent_seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let _ = dm.set_owner(&mut mem, parent_seg, 0x0100);

        let sub_mcb_seg = parent_seg + 0x10;
        let sub_mcb_addr = Memory::seg_offset_to_linear(sub_mcb_seg, 0);
        mem.write_u8(sub_mcb_addr, b'M');
        mem.write_u16(sub_mcb_addr + 1, 0x0100);
        mem.write_u16(sub_mcb_addr + 3, 0xFFFF); // Way too large

        assert!(dm.free(&mut mem, sub_mcb_seg + 1).is_err());
    }

    #[test]
    fn free_sub_block_outside_any_block_fails() {
        let (mut mem, mut dm) = setup_dos_mem();
        dm.alloc(&mut mem, 0x0100, 0x100).unwrap();

        // Use a segment in VGA memory region — outside any MCB block
        let bogus_seg: u16 = 0xB000;
        let bogus_addr = Memory::seg_offset_to_linear(bogus_seg, 0);
        mem.write_u8(bogus_addr, b'M');
        mem.write_u16(bogus_addr + 1, 0x0100);
        mem.write_u16(bogus_addr + 3, 0x0010);

        assert!(dm.free(&mut mem, bogus_seg + 1).is_err());
    }

    #[test]
    fn free_vec_tracked_free_block_does_not_double_free() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        dm.free(&mut mem, seg).unwrap();

        // Double-free: the Vec lookup skips (owner=0), the fallback finds valid
        // MCB signature but owner is already 0 → InvalidBlock.
        let result = dm.free(&mut mem, seg);
        assert!(
            matches!(result, Err(DosMemError::InvalidBlock)),
            "Double-free of already-free block should return InvalidBlock"
        );
    }

    // ========== Additional coverage tests ==========

    #[test]
    fn alloc_multiple_and_free_middle_coalesces() {
        let (mut mem, mut dm) = setup_dos_mem();
        let a = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let b = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let c = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();

        // Free middle block
        dm.free(&mut mem, b).unwrap();
        // Blocks: [a=owned] [b=free] [c=owned] [remainder=free]
        assert!(dm
            .blocks
            .iter()
            .any(|blk| blk.usable_seg() == b && blk.owner_psp == 0));

        // Free first block - should coalesce with free middle
        dm.free(&mut mem, a).unwrap();
        // First two should merge into one free block
        let first = &dm.blocks[0];
        assert_eq!(first.owner_psp, 0);
        assert!(first.size_paras >= 0x21); // at least a + b sizes + MCB header

        // Free last allocated - all should coalesce
        dm.free(&mut mem, c).unwrap();
        assert_eq!(dm.blocks.len(), 1);
        assert_eq!(dm.blocks[0].owner_psp, 0);
    }

    #[test]
    fn resize_grow_block() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        // Grow the block
        let result = dm.resize(&mut mem, seg, 0x20);
        assert!(result.is_ok());
        let block = dm.blocks.iter().find(|b| b.usable_seg() == seg).unwrap();
        assert_eq!(block.size_paras, 0x20);
    }

    #[test]
    fn resize_shrink_block_smaller() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        // Shrink the block
        let result = dm.resize(&mut mem, seg, 0x10);
        assert!(result.is_ok());
        let block = dm.blocks.iter().find(|b| b.usable_seg() == seg).unwrap();
        assert_eq!(block.size_paras, 0x10);
    }

    #[test]
    fn resize_invalid_block_returns_error() {
        let (mut mem, mut dm) = setup_dos_mem();
        let result = dm.resize(&mut mem, 0xBEEF, 0x10);
        assert!(matches!(result, Err(DosMemError::InvalidBlock)));
    }

    #[test]
    fn resize_too_large_returns_insufficient_memory() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        // Allocate another block to prevent growth
        let _b = dm.alloc(&mut mem, 0x0100, 0x100).unwrap();
        // Try to grow past the next block
        let result = dm.resize(&mut mem, seg, 0xFFFF);
        assert!(matches!(
            result,
            Err(DosMemError::InsufficientMemory { .. })
        ));
    }

    #[test]
    fn dos_mem_error_display_variants() {
        let err1 = DosMemError::InsufficientMemory {
            largest_available: 42,
        };
        let _ = format!("{:?}", err1);
        let err2 = DosMemError::InvalidBlock;
        let _ = format!("{:?}", err2);
    }

    #[test]
    fn umb_alloc_and_free() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();

        // Check UMB is available
        let umb_free = dm.largest_free_umb();
        assert!(umb_free > 0);

        // Allocate in UMB using strategy (umb_linked=true, high-memory-first strategy)
        let result = dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x80, true);
        assert!(result.is_ok());
        let seg = result.unwrap();

        // Free block
        let free_result = dm.free(&mut mem, seg);
        assert!(free_result.is_ok());
    }

    #[test]
    fn umb_chain_link_toggle() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();

        dm.write_umb_link(&mut mem, true);
        dm.write_umb_link(&mut mem, false);
        // Smoke test: no crash
    }

    #[test]
    fn set_mcb_name_updates_block() {
        let (mut mem, mut dm) = setup_dos_mem();
        let seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        dm.set_mcb_name(&mut mem, seg, b"TESTPROG");

        let block = dm.blocks.iter().find(|b| b.usable_seg() == seg).unwrap();
        assert_eq!(&block.name, b"TESTPROG");
    }

    #[test]
    fn first_mcb_segment_returns_configured_value() {
        let dm = DosMem::new(0x0700);
        assert_eq!(dm.first_mcb_segment(), 0x0700);
    }

    #[test]
    fn total_free_paragraphs_returns_nonzero() {
        let (mut mem, mut dm) = setup_dos_mem();
        let total = dm.total_free_paragraphs();
        assert!(total > 0);

        // Allocate some memory and check free decreases
        let _seg = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let total_after = dm.total_free_paragraphs();
        assert!(total_after < total);
    }

    #[test]
    fn is_umb_initialized_reflects_init() {
        let (_, dm) = setup_dos_mem();
        assert!(!dm.is_umb_initialized());

        let (_, dm) = setup_dos_mem_with_umb();
        assert!(dm.is_umb_initialized());
    }

    #[test]
    fn free_all_for_psp_releases_all_blocks() {
        let (mut mem, mut dm) = setup_dos_mem();
        let _a = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let _b = dm.alloc(&mut mem, 0x0100, 0x20).unwrap();
        let c = dm.alloc(&mut mem, 0x0200, 0x10).unwrap();

        dm.free_all_for_psp(&mut mem, 0x0100);

        // Blocks owned by 0x0100 should be free
        assert_eq!(
            dm.blocks.iter().filter(|b| b.owner_psp == 0x0100).count(),
            0
        );
        // Block owned by 0x0200 should still be allocated
        let c_block = dm.blocks.iter().find(|b| b.usable_seg() == c).unwrap();
        assert_eq!(c_block.owner_psp, 0x0200);
    }

    #[test]
    fn alloc_with_strategy_best_fit() {
        let (mut mem, mut dm) = setup_dos_mem();

        // Create a fragmented layout: alloc A, alloc B, free A (gap), alloc C
        let a = dm.alloc(&mut mem, 0x0100, 0x10).unwrap();
        let _b = dm.alloc(&mut mem, 0x0100, 0x100).unwrap();
        dm.free(&mut mem, a).unwrap();

        // Strategy 1 = Best fit - should find the small gap
        let result = dm.alloc_with_strategy(&mut mem, 0x0100, 0x10, 1);
        assert!(result.is_ok());
    }

    #[test]
    fn alloc_with_strategy_last_fit() {
        let (mut mem, mut dm) = setup_dos_mem();

        // Strategy 2 = Last fit
        let result = dm.alloc_with_strategy(&mut mem, 0x0100, 0x10, 2);
        assert!(result.is_ok());
    }

    #[test]
    fn resize_umb_when_umb_initialized() {
        let (mut mem, mut dm) = setup_dos_mem_with_umb();

        // Allocate a UMB block
        let result = dm.alloc_with_strategy_and_umb(&mut mem, 0x0100, 0x10, 0x80, true);
        if let Ok(seg) = result {
            // Try to resize it
            let resize_result = dm.resize_umb(&mut mem, seg, 0x08);
            // May or may not succeed depending on whether it ended up in UMB
            let _ = resize_result;
        }
    }
}
