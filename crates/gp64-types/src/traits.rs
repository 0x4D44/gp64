//! Cross-crate traits for decoupling dependencies.

/// Memory page access type for address translation.
/// Used by CPU for paging and protection checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageAccess {
    /// Reading from memory
    Read,
    /// Writing to memory
    Write,
    /// Fetching instructions (code execution)
    Fetch,
}

impl PageAccess {
    /// Returns true if this access is a write.
    #[inline]
    pub fn is_write(self) -> bool {
        matches!(self, PageAccess::Write)
    }
}

/// SMC notification for JIT invalidation.
/// Memory calls this when code regions are written to.
pub trait SmcNotify: Send + Sync {
    fn notify_write(&self, address: usize, size: usize);
    fn notify_write_range(&self, start: usize, end: usize);

    /// Whether this notifier is holding an unconsumed write record.
    ///
    /// A recorded write is not yet an invalidated block: recording and draining
    /// are deliberately two-sided, and the drain happens at the engine root. So
    /// a native engine that steps a nested interpreter instruction — which can
    /// reach a device that writes guest RAM holding compiled code — needs to ask
    /// whether anything WAS recorded, and end its native window if so, before a
    /// cached successor block runs from stale source (MDD-BUG-FLUX-00146).
    ///
    /// Defaults to `false`, which is correct for a notifier that keeps no
    /// pending state (`()`, and the test doubles): they cannot be holding a
    /// record, so they can never require an exit.
    fn is_pending(&self) -> bool {
        false
    }
}

impl SmcNotify for () {
    fn notify_write(&self, _: usize, _: usize) {}
    fn notify_write_range(&self, _: usize, _: usize) {}
}

/// Interrupt invocation for hardware devices (mouse, keyboard).
/// Hardware calls this to trigger CPU interrupts.
pub trait InterruptInvoker {
    fn invoke_interrupt(&mut self, vector: u8);
}

/// Snapshot of VGA pipeline registers needed for memory write decoding.
/// Used by the fast interpreter to inline VGA write logic without I/O port reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VgaPipelineState {
    pub video_mode: u8,
    pub chain4: bool,
    /// Sequencer register 4 bit 2: sequential CPU writes instead of odd/even.
    pub seq_odd_even_disabled: bool,
    /// Graphics Controller register 5 bit 4: odd/even CPU reads.
    pub gc_odd_even_read: bool,
    /// True iff the registers describe a real Mode X / Mode Y configuration
    /// (256-colour, chain-4 disabled, AND CR9 max-scan or CR17 byte-mode
    /// reprogrammed). Memory's planar dispatch must NOT trip on the
    /// transient `(video_mode=0x13, chain4=false)` state during register
    /// programming: chain-4 has not been enabled yet, but the remaining
    /// registers still describe standard Mode 13h addressing.
    pub is_mode_x: bool,
    pub plane_mask: u8,
    pub read_plane: u8,
    pub memory_map_select: u8,
    /// Graphics Controller register 6 bit 0. Clear means alphanumeric/text
    /// memory addressing; set means graphics memory addressing.
    pub gc_graphics_mode: bool,
    pub write_mode: u8,
    pub bit_mask: u8,
    pub set_reset: u8,
    pub enable_set_reset: u8,
    pub data_rotate: u8,
    pub read_mode: u8,
    pub color_compare: u8,
    pub color_dont_care: u8,
}

/// I/O port access for CPU instructions (IN, OUT, INS, OUTS).
/// CPU uses this trait; HW implements it for `IoBus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoCpuMode {
    Real,
    Protected,
    V86,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoTraceContext {
    pub cs: u16,
    pub eip: u32,
    pub mode: IoCpuMode,
    pub cpl: Option<u8>,
    pub cr3: Option<u32>,
}

impl IoTraceContext {
    pub const UNKNOWN: Self = Self {
        cs: 0,
        eip: 0,
        mode: IoCpuMode::Unknown,
        cpl: None,
        cr3: None,
    };

    pub const fn unknown_at(cs: u16, eip: u32) -> Self {
        Self {
            cs,
            eip,
            mode: IoCpuMode::Unknown,
            cpl: None,
            cr3: None,
        }
    }
}

pub trait IoPort {
    /// Update the absolute cycle position visible to timing-sensitive devices.
    fn set_cpu_cycles(&mut self, _cycles: u64) {}

    /// Whether an interrupt-enable/shadow boundary needs caller attention.
    /// A bus may return false only while no IRQ or host hook needs polling;
    /// the cycle budget must still stop execution at the next device deadline.
    fn interrupt_poll_wanted(&self) -> bool {
        true
    }

    fn read_u8(&mut self, port: u16) -> u8;
    fn read_u16(&mut self, port: u16) -> u16;
    fn read_u32(&mut self, port: u16) -> u32;
    fn write_u8(&mut self, port: u16, value: u8);
    fn write_u16(&mut self, port: u16, value: u16);
    fn write_u32(&mut self, port: u16, value: u32);

    /// Read a byte from an I/O port with the guest instruction address.
    ///
    /// Default implementations preserve compatibility for I/O backends that do
    /// not need instruction context.
    fn read_u8_at(&mut self, port: u16, _cs: u16, _ip: u32) -> u8 {
        self.read_u8(port)
    }

    /// Read a word from an I/O port with the guest instruction address.
    fn read_u16_at(&mut self, port: u16, _cs: u16, _ip: u32) -> u16 {
        self.read_u16(port)
    }

    /// Read a dword from an I/O port with the guest instruction address.
    fn read_u32_at(&mut self, port: u16, _cs: u16, _ip: u32) -> u32 {
        self.read_u32(port)
    }

    /// Read a byte from an I/O port with richer guest CPU context.
    ///
    /// Default implementations preserve compatibility for I/O backends that do
    /// not need mode/CPL/CR3 context.
    fn read_u8_ctx(&mut self, port: u16, ctx: IoTraceContext) -> u8 {
        self.read_u8_at(port, ctx.cs, ctx.eip)
    }

    /// Read a word from an I/O port with richer guest CPU context.
    fn read_u16_ctx(&mut self, port: u16, ctx: IoTraceContext) -> u16 {
        self.read_u16_at(port, ctx.cs, ctx.eip)
    }

    /// Read a dword from an I/O port with richer guest CPU context.
    fn read_u32_ctx(&mut self, port: u16, ctx: IoTraceContext) -> u32 {
        self.read_u32_at(port, ctx.cs, ctx.eip)
    }

    /// Write a byte to an I/O port with the guest instruction address.
    fn write_u8_at(&mut self, port: u16, value: u8, _cs: u16, _ip: u32) {
        self.write_u8(port, value);
    }

    /// Write a word to an I/O port with the guest instruction address.
    fn write_u16_at(&mut self, port: u16, value: u16, _cs: u16, _ip: u32) {
        self.write_u16(port, value);
    }

    /// Write a dword to an I/O port with the guest instruction address.
    fn write_u32_at(&mut self, port: u16, value: u32, _cs: u16, _ip: u32) {
        self.write_u32(port, value);
    }

    /// Write a byte to an I/O port with richer guest CPU context.
    fn write_u8_ctx(&mut self, port: u16, value: u8, ctx: IoTraceContext) {
        self.write_u8_at(port, value, ctx.cs, ctx.eip);
    }

    /// Write a word to an I/O port with richer guest CPU context.
    fn write_u16_ctx(&mut self, port: u16, value: u16, ctx: IoTraceContext) {
        self.write_u16_at(port, value, ctx.cs, ctx.eip);
    }

    /// Write a dword to an I/O port with richer guest CPU context.
    fn write_u32_ctx(&mut self, port: u16, value: u32, ctx: IoTraceContext) {
        self.write_u32_at(port, value, ctx.cs, ctx.eip);
    }

    /// Return accumulated I/O delay inside the current execution burst.
    fn io_delay_total(&self) -> u64 {
        0
    }

    /// Returns true if VGA pipeline state changed since last flush.
    fn vga_pipeline_dirty(&self) -> bool {
        false
    }

    /// Snapshot current VGA pipeline state and clear the dirty flag.
    fn take_vga_pipeline_state(&mut self) -> VgaPipelineState {
        VgaPipelineState::default()
    }

    /// Returns true if the current I/O write requires the interpreter to break
    /// the batch immediately so the emulator can perform synchronous dispatch
    /// before the next instruction executes.
    ///
    /// An output may request synchronous device work whose result must be
    /// visible to the next input instruction. Breaking the batch lets the
    /// caller finish that work before execution resumes.
    fn should_break_batch(&self) -> bool {
        false
    }
}

/// Null implementation for testing without real I/O
impl IoPort for () {
    fn read_u8(&mut self, _: u16) -> u8 {
        0xFF
    }
    fn read_u16(&mut self, _: u16) -> u16 {
        0xFFFF
    }
    fn read_u32(&mut self, _: u16) -> u32 {
        0xFFFF_FFFF
    }
    fn write_u8(&mut self, _: u16, _: u8) {}
    fn write_u16(&mut self, _: u16, _: u16) {}
    fn write_u32(&mut self, _: u16, _: u32) {}
}

/// Memory access trait for CPU instruction execution.
/// Provides raw memory read/write operations at linear addresses.
/// Used by execute module to abstract over different memory implementations.
pub trait MemoryAccess {
    /// Read a byte from the given linear address
    fn read_u8(&self, addr: usize) -> u8;

    /// Read a word (16-bit) from the given linear address
    fn read_u16(&self, addr: usize) -> u16;

    /// Read a dword (32-bit) from the given linear address
    fn read_u32(&self, addr: usize) -> u32;

    /// Write a byte to the given linear address
    fn write_u8(&mut self, addr: usize, value: u8);

    /// Write a word (16-bit) to the given linear address
    fn write_u16(&mut self, addr: usize, value: u16);

    /// Write a dword (32-bit) to the given linear address
    fn write_u32(&mut self, addr: usize, value: u32);

    /// Read a qword (64-bit) from the given linear address
    fn read_u64(&self, addr: usize) -> u64;

    /// Write a qword (64-bit) to the given linear address
    fn write_u64(&mut self, addr: usize, value: u64);

    /// Read `dest.len()` consecutive bytes starting at `addr`, byte-for-byte
    /// equivalent to a `read_u8` loop. The default IS that loop — it defines
    /// the semantics. Implementations may override with a bulk fast path only
    /// if it preserves the exact per-byte dispatch (MMIO, overlays, aliases).
    fn read_run(&self, addr: usize, dest: &mut [u8]) {
        for (i, byte) in dest.iter_mut().enumerate() {
            *byte = self.read_u8(addr + i);
        }
    }

    /// Write `data.len()` consecutive bytes starting at `addr`, byte-for-byte
    /// equivalent to a `write_u8` loop. The default IS that loop — it defines
    /// the semantics, so every implementor is unchanged until it opts in.
    /// Implementations may override with a bulk fast path only where that path
    /// preserves the exact per-byte dispatch (apertures, overlays, aliases,
    /// A20 masking) AND every per-byte side effect (SMC snoop, descriptor
    /// snoop, write tracking, armed diagnostics). The write-side peer of
    /// [`MemoryAccess::read_run`].
    fn write_run(&mut self, addr: usize, data: &[u8]) {
        for (i, byte) in data.iter().copied().enumerate() {
            self.write_u8(addr + i, byte);
        }
    }
}

/// Null implementation for testing without real memory
impl MemoryAccess for () {
    #[inline]
    fn read_u8(&self, _: usize) -> u8 {
        0
    }
    #[inline]
    fn read_u16(&self, _: usize) -> u16 {
        0
    }
    #[inline]
    fn read_u32(&self, _: usize) -> u32 {
        0
    }
    #[inline]
    fn write_u8(&mut self, _: usize, _: u8) {}
    #[inline]
    fn write_u16(&mut self, _: usize, _: u16) {}
    #[inline]
    fn write_u32(&mut self, _: usize, _: u32) {}
    #[inline]
    fn read_u64(&self, _: usize) -> u64 {
        0
    }
    #[inline]
    fn write_u64(&mut self, _: usize, _: u64) {}
}

/// Leaf source accompanying a page-walk TLB fill.
#[derive(Clone, Copy)]
pub struct PagingTlbSource {
    pub address: u32,
    pub large: bool,
    pub global: bool,
}

/// A retained global translation denies the requested write.
#[derive(Debug)]
pub struct PagingWriteProtection;

/// Memory operations shared by every production page walker.
pub trait PagedMemory: MemoryAccess {
    /// Whether translated scalar operands take the physical-device bridge
    /// ([`read_translated_scalar`](Self::read_translated_scalar) /
    /// [`write_translated_scalar`](Self::write_translated_scalar)) instead of
    /// the byte-wise page walk (contract LLD §3.9). A monomorphisation-time
    /// constant: `false` compiles the bridge out.
    const SCALAR_DISPATCH: bool = false;

    /// Read one already-translated scalar operand of 1, 2, 4 or 8 bytes.
    ///
    /// `physical_addresses` holds exactly one address per byte, in operand lane
    /// order. The caller translates and permission-preflights every lane first,
    /// including write eligibility for a read-modify-write. The default is
    /// byte-wise; bulk accesses never come here.
    #[inline]
    fn read_translated_scalar(&self, physical_addresses: &[usize]) -> u64 {
        debug_assert!(matches!(physical_addresses.len(), 1 | 2 | 4 | 8));
        physical_addresses
            .iter()
            .enumerate()
            .fold(0u64, |value, (lane, &address)| {
                value | (u64::from(self.read_u8(address)) << (lane * 8))
            })
    }

    /// Write one already-translated scalar operand of 1, 2, 4 or 8 bytes, in
    /// operand lane order, after the caller translated and write-preflighted
    /// every lane. The default is byte-wise.
    #[inline]
    fn write_translated_scalar(&mut self, physical_addresses: &[usize], value: u64) {
        debug_assert!(matches!(physical_addresses.len(), 1 | 2 | 4 | 8));
        for (lane, &address) in physical_addresses.iter().enumerate() {
            self.write_u8(address, (value >> (lane * 8)) as u8);
        }
    }

    /// Check DPMI linear-to-physical mapping.
    /// Returns `Some(physical_address)` if a mapping exists, None otherwise.
    fn map_linear_physical(&self, linear: u32) -> Option<usize>;

    /// Look up a linear page in the TLB.
    /// Returns `Some((physical_page, writable, user_allowed, dirty))` if cached.
    fn tlb_lookup(&self, linear_page: u32) -> Option<(u32, bool, bool, bool)>;

    /// Insert a translation into the TLB.
    fn tlb_insert(
        &mut self,
        linear_page: u32,
        physical_page: u32,
        writable: bool,
        user: bool,
        dirty: bool,
    );

    /// Insert one 4 KiB slice of a 4 MiB PSE mapping. INVLPG of any address
    /// in the large page must drop every slice, so a TLB that caches slices
    /// records them; the default is an ordinary insert.
    fn tlb_insert_large_page_slice(
        &mut self,
        linear_page: u32,
        physical_page: u32,
        writable: bool,
        user: bool,
        dirty: bool,
    ) {
        self.tlb_insert(linear_page, physical_page, writable, user, dirty);
    }

    /// Mark a cached page global and retain its final PTE/PDE address so a
    /// first write after CR3 changes can update the original entry's D bit.
    fn mark_tlb_global(&mut self, _linear_page: u32, _backing_addr: u32, _large: bool) {}

    /// Backing PTE/PDE for a cached global page; bool identifies a 4 MiB PDE.
    fn tlb_global_source(&self, _linear_page: u32) -> Option<(u32, bool)> {
        None
    }

    /// Compare the retained leaf's present/frame/permission/page-size fields
    /// before a clean global translation can update that leaf's dirty bit.
    fn tlb_global_backing_matches(&self, _linear_page: u32, _backing: u32) -> bool {
        false
    }

    /// Fill the authoritative TLB and its projections with the same global
    /// metadata in both the CPU and HLE page walkers.
    fn cache_paging_translation(
        &mut self,
        linear_page: u32,
        physical_page: u32,
        writable: bool,
        user: bool,
        dirty: bool,
        source: PagingTlbSource,
    ) {
        if source.large {
            self.tlb_insert_large_page_slice(linear_page, physical_page, writable, user, dirty);
        } else {
            self.tlb_insert(linear_page, physical_page, writable, user, dirty);
        }
        if source.global {
            self.mark_tlb_global(linear_page, source.address, source.large);
        }
    }

    /// Upgrade a clean retained global without walking a different CR3.
    ///
    /// Intel SDM Vol. 3A §§4.8, 4.10.2.3 allow stale cached translations but
    /// also eviction at any time. We conservatively evict a changed leaf
    /// before setting D, then let the caller re-walk the current CR3. Dirty
    /// hits retain their ordinary TLB staleness semantics.
    /// https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf
    fn upgrade_clean_global_translation(
        &mut self,
        linear: u32,
        user: bool,
        wp: bool,
    ) -> Result<Option<(usize, bool)>, PagingWriteProtection> {
        let linear_page = linear >> 12;
        let Some((physical_page, writable, user_allowed, false)) = self.tlb_lookup(linear_page)
        else {
            return Ok(None);
        };
        let Some((address, large)) = self.tlb_global_source(linear_page) else {
            return Ok(None);
        };
        let backing = self.read_u32_physical(address as usize);
        if !self.tlb_global_backing_matches(linear_page, backing) {
            self.invalidate_tlb_page(linear);
            return Ok(None);
        }
        if !writable && (user || wp) {
            return Err(PagingWriteProtection);
        }
        if user && !user_allowed {
            return Err(PagingWriteProtection);
        }
        let first_write = backing & 0x40 == 0;
        if first_write {
            self.write_u32_physical_ad(address as usize, backing | 0x40);
        }
        self.cache_paging_translation(
            linear_page,
            physical_page,
            writable,
            user_allowed,
            true,
            PagingTlbSource {
                address,
                large,
                global: true,
            },
        );
        Ok(Some((
            ((physical_page as usize) << 12) | ((linear & 0xfff) as usize),
            first_write,
        )))
    }

    /// CR3 writes preserve globals when CR4.PGE is enabled. Implementations
    /// without global-page storage can use the ordinary flush.
    fn invalidate_tlb_non_global(&mut self) {
        self.invalidate_tlb();
    }

    /// Full CR4 flush for identities supporting global pages. Implementations
    /// may retain a separate lifetime epoch for native translation premises.
    fn invalidate_tlb_global_flush(&mut self) {
        self.invalidate_tlb();
    }

    /// Read a 32-bit value from physical memory (bypasses segmentation/paging).
    /// Used for page table walks.
    fn read_u32_physical(&self, addr: usize) -> u32;

    /// Read one byte from physical memory when the address is backed.
    ///
    /// Returns `None` instead of fabricating a value for an address outside
    /// physical RAM. CPU-internal table reads use this to fail closed.
    fn read_u8_physical_checked(&self, addr: usize) -> Option<u8>;

    /// Write a 32-bit value to physical memory (bypasses segmentation/paging).
    /// Used for setting accessed/dirty bits in page tables.
    fn write_u32_physical(&mut self, addr: usize, value: u32);

    /// Record that a hardware page-table walk READ a paging-structure entry
    /// (PDE/PTE) from this physical frame.
    ///
    /// EXP-N005 (dbx link keep): the dyncore keeps resolved block links across
    /// translation-epoch edges only while no guest write has landed in any
    /// recorded frame. A tracking implementation records the frame and diverts
    /// stores to it off the write fast path so its tripwire sees them; the
    /// default is a no-op (test memories, non-dyncore configurations), which
    /// is vacuously safe because without the tracking backend no link is ever
    /// kept.
    fn record_pt_frame(&mut self, _physical_page: u32) {}

    /// Physical u32 write used exclusively by the page walker's
    /// accessed/dirty-bit write-backs.
    ///
    /// Identical to [`PagedMemory::write_u32_physical`] except that a tracking
    /// implementation must NOT classify it as a guest page-table edit: A/D
    /// write-backs only OR-in status bits 5/6, which no walk conclusion reads,
    /// so they cannot stale a translation premise (EXP-N005 amendment A3).
    /// Every other side effect (SMC notification, snoops) is preserved.
    fn write_u32_physical_ad(&mut self, addr: usize, value: u32) {
        self.write_u32_physical(addr, value);
    }

    /// Set page fault pending state.
    fn set_page_fault(&mut self, linear_address: u32, error_code: u32);

    /// Clear page fault pending state.
    fn clear_page_fault(&mut self);

    /// Check if a page fault is pending.
    fn page_fault_pending(&self) -> bool;

    /// Get the pending page fault address.
    fn page_fault_address(&self) -> u32;

    /// Get the pending page fault error code.
    fn page_fault_error_code(&self) -> u32;

    /// Invalidate/flush the TLB (called on CR3 write or paging enable change).
    fn invalidate_tlb(&mut self);

    /// Invalidate a single TLB page entry (called by INVLPG instruction).
    fn invalidate_tlb_page(&mut self, linear_addr: u32);
}

/// Null implementation for testing without paging
impl PagedMemory for () {
    fn map_linear_physical(&self, _: u32) -> Option<usize> {
        None
    }
    fn tlb_lookup(&self, _: u32) -> Option<(u32, bool, bool, bool)> {
        None
    }
    fn tlb_insert(&mut self, _: u32, _: u32, _: bool, _: bool, _: bool) {}
    fn read_u32_physical(&self, _: usize) -> u32 {
        0
    }
    fn read_u8_physical_checked(&self, _: usize) -> Option<u8> {
        None
    }
    fn write_u32_physical(&mut self, _: usize, _: u32) {}
    fn set_page_fault(&mut self, _: u32, _: u32) {}
    fn clear_page_fault(&mut self) {}
    fn page_fault_pending(&self) -> bool {
        false
    }
    fn page_fault_address(&self) -> u32 {
        0
    }
    fn page_fault_error_code(&self) -> u32 {
        0
    }
    fn invalidate_tlb(&mut self) {}
    fn invalidate_tlb_page(&mut self, _: u32) {}
}

// =============================================================================
// Hardware Abstraction Traits
// =============================================================================
// These traits allow cross-crate access to HW components without tight coupling.
// Primary use cases:
// 1. DOS crate accessing VGA/keyboard for INT 21h functions
// 2. Unit testing with mock implementations
// 3. Future: alternative HW implementations

/// Video memory access for BIOS/DOS screen operations.
/// Provides text-mode operations used by INT 10h and DOS output functions.
#[allow(clippy::too_many_arguments)]
pub trait VideoAccess {
    /// Write a character and attribute at the given text position.
    /// Row and column are 0-based. Page is the active video page (usually 0).
    fn write_text_char(&mut self, page: u8, row: u8, col: u8, ch: u8, attr: u8);

    /// Read character and attribute from the given text position.
    /// Returns (character, attribute).
    fn read_text_char(&self, page: u8, row: u8, col: u8) -> (u8, u8);

    /// Get the current video mode number.
    fn get_video_mode(&self) -> u8;

    /// Set the video mode. Returns true if mode change succeeded.
    fn set_video_mode(&mut self, mode: u8) -> bool;

    /// Get cursor position for the given page. Returns (row, col).
    fn get_cursor_pos(&self, page: u8) -> (u8, u8);

    /// Set cursor position for the given page.
    fn set_cursor_pos(&mut self, page: u8, row: u8, col: u8);

    /// Get the number of text columns in the current mode.
    fn get_columns(&self) -> u8;

    /// Get the number of text rows in the current mode.
    fn get_rows(&self) -> u8;

    /// Write a character in teletype mode (handles CR, LF, BS, BEL, scrolling).
    fn teletype(&mut self, ch: u8, page: u8);

    /// Scroll a region of the screen up by the given number of lines.
    /// If lines is 0, clear the region.
    fn scroll_up(
        &mut self,
        page: u8,
        lines: u8,
        top: u8,
        left: u8,
        bottom: u8,
        right: u8,
        attr: u8,
    );

    /// Scroll a region of the screen down by the given number of lines.
    fn scroll_down(
        &mut self,
        page: u8,
        lines: u8,
        top: u8,
        left: u8,
        bottom: u8,
        right: u8,
        attr: u8,
    );
}

/// Null implementation for testing without real video hardware
impl VideoAccess for () {
    fn write_text_char(&mut self, _: u8, _: u8, _: u8, _: u8, _: u8) {}
    fn read_text_char(&self, _: u8, _: u8, _: u8) -> (u8, u8) {
        (0x20, 0x07) // Space with default attribute
    }
    fn get_video_mode(&self) -> u8 {
        0x03 // 80x25 text mode
    }
    fn set_video_mode(&mut self, _: u8) -> bool {
        true
    }
    fn get_cursor_pos(&self, _: u8) -> (u8, u8) {
        (0, 0)
    }
    fn set_cursor_pos(&mut self, _: u8, _: u8, _: u8) {}
    fn get_columns(&self) -> u8 {
        80
    }
    fn get_rows(&self) -> u8 {
        25
    }
    fn teletype(&mut self, _: u8, _: u8) {}
    fn scroll_up(&mut self, _: u8, _: u8, _: u8, _: u8, _: u8, _: u8, _: u8) {}
    fn scroll_down(&mut self, _: u8, _: u8, _: u8, _: u8, _: u8, _: u8, _: u8) {}
}

/// Keyboard access for BIOS/DOS input operations.
/// Used by INT 16h and DOS input functions (AH=01h, 07h, 08h, etc.).
pub trait KeyboardAccess {
    /// Read and remove a key from the buffer.
    /// Returns Some((scancode, ascii)) if a key is available, None otherwise.
    fn read_key(&mut self) -> Option<(u8, u8)>;

    /// Peek at the next key without removing it.
    /// Returns Some((scancode, ascii)) if a key is available, None otherwise.
    fn peek_key(&self) -> Option<(u8, u8)>;

    /// Get the current shift/modifier flags.
    /// Bit layout matches BIOS: bit 0-1 = shift, bit 2 = ctrl, bit 3 = alt, etc.
    fn get_shift_flags(&self) -> u8;

    /// Check if a key is available in the buffer.
    fn key_available(&self) -> bool;

    /// Clear the keyboard buffer.
    fn clear_buffer(&mut self);
}

/// Null implementation for testing without real keyboard
impl KeyboardAccess for () {
    fn read_key(&mut self) -> Option<(u8, u8)> {
        None
    }
    fn peek_key(&self) -> Option<(u8, u8)> {
        None
    }
    fn get_shift_flags(&self) -> u8 {
        0
    }
    fn key_available(&self) -> bool {
        false
    }
    fn clear_buffer(&mut self) {}
}

/// Timer access for time services.
/// Used by BIOS INT 1Ah and DOS date/time functions.
pub trait TimerAccess {
    /// Get the BIOS tick count (incremented ~18.2 times/second).
    fn get_tick_count(&self) -> u32;

    /// Read the current counter value for the given PIT channel (0-2).
    fn read_counter(&self, channel: u8) -> u16;
}

/// Null implementation for testing without real timer
impl TimerAccess for () {
    fn get_tick_count(&self) -> u32 {
        0
    }
    fn read_counter(&self, _: u8) -> u16 {
        0
    }
}

/// Speaker access for PC speaker beep operations.
/// Used by BIOS keyboard beep and DOS speaker functions.
pub trait SpeakerAccess {
    /// Start playing a tone at the given frequency (Hz).
    fn beep(&mut self, freq: u16);

    /// Stop any currently playing tone.
    fn stop(&mut self);
}

/// Null implementation for testing without real speaker
impl SpeakerAccess for () {
    fn beep(&mut self, _: u16) {}
    fn stop(&mut self) {}
}

// =============================================================================
// MIDI Synthesizer Traits
// =============================================================================

/// Accepts raw MIDI bytes (status, data, or sysex fragments).
pub trait MidiWriter {
    fn write_byte(&mut self, data: u8);

    /// Try to pass one MIDI byte to the device without silently dropping it.
    ///
    /// Existing writers are lossless and therefore use the default, which
    /// forwards to [`Self::write_byte`] and reports success. Hardware-facing
    /// writers can override this to expose a finite FIFO or input-credit
    /// budget; `false` leaves ownership with the caller so it can retry the
    /// byte at the next device boundary.
    fn try_write_byte(&mut self, data: u8) -> bool {
        self.write_byte(data);
        true
    }
}

/// Null implementation for tests that don't care about MIDI.
impl MidiWriter for () {
    fn write_byte(&mut self, _data: u8) {}
}

/// A MIDI synthesizer: accepts MIDI bytes and produces audio.
pub trait MidiSynth: MidiWriter + Send + 'static {
    /// Perform fallible worker-side startup before the device reports ready.
    ///
    /// Synths whose real backend starts lazily (for example an out-of-process
    /// helper) override this so construction success means the device is usable.
    fn prepare(&mut self, _sample_rate: u32) -> Result<(), String> {
        Ok(())
    }
    /// Render `count` stereo frames at `sample_rate` Hz.
    /// Samples are ADDED to `output[..count * 2]` (not overwritten).
    fn generate_samples(&mut self, count: usize, sample_rate: u32, output: &mut [f32]);
    fn name(&self) -> &'static str;
    fn active_voice_count(&self) -> usize {
        0
    }
    fn needs_boot(&self) -> bool {
        false
    }
    /// Whether the synth is suspended in exact silence and needs no render
    /// until its next MIDI byte (the SC-55's idle suspension,
    /// MDD-BUG-NMI-00283). A deterministic MIDI worker may then stop
    /// requesting renders, and reports the skipped guest time through
    /// [`Self::advance_idle`] before the next render.
    fn is_idle(&self) -> bool {
        false
    }
    /// Account `frames` of guest time that passed with no render. It is
    /// called before the render that ends the gap; MIDI bytes written during
    /// the gap belong to the start of that render. The default does nothing,
    /// which is right for a synth that never reports [`Self::is_idle`] and
    /// keeps the pre-activity skip's existing behaviour.
    fn advance_idle(&mut self, _frames: usize, _sample_rate: u32) {}
    /// Downcast support for synth-specific methods (e.g. MT-32 ROM identity).
    fn as_any(&self) -> &dyn std::any::Any;
    /// Mutable downcast support.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

// =============================================================================
// MIDI Synthesizer Identification
// =============================================================================

/// Identifies a MIDI synthesizer device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SynthId {
    Mt32 = 0,
    Sc55 = 1,
    Syxg50 = 3,
    Ferrosintesis = 4,
}

impl SynthId {
    pub fn label(self) -> &'static str {
        match self {
            SynthId::Mt32 => "MT-32",
            SynthId::Sc55 => "SC-55",
            SynthId::Syxg50 => "S-YXG50",
            SynthId::Ferrosintesis => "Ferrosintesis",
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Mt32),
            1 => Some(Self::Sc55),
            // Legacy MU-50 control value migrates to the supported GS module.
            2 => Some(Self::Sc55),
            3 => Some(Self::Syxg50),
            4 => Some(Self::Ferrosintesis),
            _ => None,
        }
    }

    /// Human-friendly string for TOML settings persistence.
    pub fn as_settings_str(self) -> &'static str {
        match self {
            SynthId::Mt32 => "mt-32",
            SynthId::Sc55 => "sc-55",
            SynthId::Syxg50 => "syxg-50",
            SynthId::Ferrosintesis => "ferrosintesis",
        }
    }

    /// Parse from a human-friendly settings string.
    pub fn from_settings_str(s: &str) -> Option<SynthId> {
        match s {
            "mt-32" => Some(SynthId::Mt32),
            "sc-55" => Some(SynthId::Sc55),
            "mu50" | "mu-50" => Some(SynthId::Sc55),
            "syxg-50" => Some(SynthId::Syxg50),
            "ferrosintesis" => Some(SynthId::Ferrosintesis),
            _ => None,
        }
    }
}

// =============================================================================
// Tests for null implementations
// =============================================================================
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // ---- PageAccess tests ----
    #[test]
    fn test_page_access_is_write() {
        assert!(!PageAccess::Read.is_write());
        assert!(PageAccess::Write.is_write());
        assert!(!PageAccess::Fetch.is_write());
    }

    #[test]
    fn test_page_access_debug() {
        assert_eq!(format!("{:?}", PageAccess::Read), "Read");
        assert_eq!(format!("{:?}", PageAccess::Write), "Write");
        assert_eq!(format!("{:?}", PageAccess::Fetch), "Fetch");
    }

    #[test]
    fn test_page_access_clone_eq() {
        let a = PageAccess::Read;
        let b = a;
        assert_eq!(a, b);
        assert_eq!(PageAccess::Write, PageAccess::Write);
        assert_ne!(PageAccess::Read, PageAccess::Write);
    }

    // ---- SmcNotify null impl tests ----
    #[test]
    fn test_smc_notify_unit() {
        let smc: () = ();
        smc.notify_write(0x1000, 4);
        smc.notify_write_range(0x1000, 0x2000);
        // Just verify they don't panic
    }

    // ---- IoPort null impl tests ----
    #[test]
    fn test_ioport_unit_read() {
        let mut io: () = ();
        assert_eq!(IoPort::read_u8(&mut io, 0x60), 0xFF);
        assert_eq!(IoPort::read_u16(&mut io, 0x60), 0xFFFF);
        assert_eq!(IoPort::read_u32(&mut io, 0x60), 0xFFFFFFFF);
    }

    #[test]
    fn test_ioport_unit_write() {
        let mut io: () = ();
        IoPort::write_u8(&mut io, 0x60, 0xAA);
        IoPort::write_u16(&mut io, 0x60, 0xAABB);
        IoPort::write_u32(&mut io, 0x60, 0xAABBCCDD);
        // Just verify they don't panic
    }

    // ---- MemoryAccess null impl tests ----
    #[test]
    fn test_memory_access_unit_read() {
        let mem: () = ();
        assert_eq!(MemoryAccess::read_u8(&mem, 0x1000), 0);
        assert_eq!(MemoryAccess::read_u16(&mem, 0x1000), 0);
        assert_eq!(MemoryAccess::read_u32(&mem, 0x1000), 0);
        assert_eq!(MemoryAccess::read_u64(&mem, 0x1000), 0);
    }

    #[test]
    fn test_memory_access_unit_write() {
        let mut mem: () = ();
        MemoryAccess::write_u8(&mut mem, 0x1000, 0xAA);
        MemoryAccess::write_u16(&mut mem, 0x1000, 0xAABB);
        MemoryAccess::write_u32(&mut mem, 0x1000, 0xAABBCCDD);
        MemoryAccess::write_u64(&mut mem, 0x1000, 0xAABBCCDDEEFF0011);
        // Just verify they don't panic
    }

    // ---- PagedMemory null impl tests ----
    #[test]
    fn test_paged_memory_unit() {
        let mut paged: () = ();

        assert_eq!(PagedMemory::map_linear_physical(&paged, 0x1000), None);
        assert_eq!(PagedMemory::tlb_lookup(&paged, 0x1000), None);
        PagedMemory::tlb_insert(&mut paged, 0x1000, 0x2000, true, true, false);
        assert_eq!(PagedMemory::read_u32_physical(&paged, 0x1000), 0);
        PagedMemory::write_u32_physical(&mut paged, 0x1000, 0xDEADBEEF);
        PagedMemory::set_page_fault(&mut paged, 0x1000, 0x0002);
        assert!(!PagedMemory::page_fault_pending(&paged));
        assert_eq!(PagedMemory::page_fault_address(&paged), 0);
        assert_eq!(PagedMemory::page_fault_error_code(&paged), 0);
        PagedMemory::clear_page_fault(&mut paged);
        PagedMemory::invalidate_tlb(&mut paged);
        PagedMemory::invalidate_tlb_page(&mut paged, 0x1000);
        // Just verify they don't panic
    }

    // ---- VideoAccess null impl tests ----
    #[test]
    fn test_video_access_unit() {
        let mut video: () = ();

        video.write_text_char(0, 0, 0, b'A', 0x07);
        assert_eq!(video.read_text_char(0, 0, 0), (0x20, 0x07));
        assert_eq!(video.get_video_mode(), 0x03);
        assert!(video.set_video_mode(0x13));
        assert_eq!(video.get_cursor_pos(0), (0, 0));
        video.set_cursor_pos(0, 10, 20);
        assert_eq!(video.get_columns(), 80);
        assert_eq!(video.get_rows(), 25);
        video.teletype(b'X', 0);
        video.scroll_up(0, 1, 0, 0, 24, 79, 0x07);
        video.scroll_down(0, 1, 0, 0, 24, 79, 0x07);
        // Just verify they don't panic
    }

    // ---- KeyboardAccess null impl tests ----
    #[test]
    fn test_keyboard_access_unit() {
        let mut kb: () = ();

        assert_eq!(kb.read_key(), None);
        assert_eq!(kb.peek_key(), None);
        assert_eq!(kb.get_shift_flags(), 0);
        assert!(!kb.key_available());
        kb.clear_buffer();
        // Just verify they don't panic
    }

    // ---- TimerAccess null impl tests ----
    #[test]
    fn test_timer_access_unit() {
        let timer: () = ();

        assert_eq!(timer.get_tick_count(), 0);
        assert_eq!(timer.read_counter(0), 0);
        assert_eq!(timer.read_counter(1), 0);
        assert_eq!(timer.read_counter(2), 0);
        // Just verify they don't panic
    }

    // ---- SpeakerAccess null impl tests ----
    #[test]
    fn test_speaker_access_unit() {
        let mut speaker: () = ();

        speaker.beep(440);
        speaker.stop();
        // Just verify they don't panic
    }

    // ---- MidiWriter null impl tests ----
    #[test]
    fn test_midi_writer_unit() {
        let mut writer: () = ();
        writer.write_byte(0x90);
        writer.write_byte(60);
        writer.write_byte(100);
        assert!(writer.try_write_byte(0xF8));
        // Just verify they don't panic
    }

    // ---- MidiSynth default method tests ----

    /// Minimal MidiSynth impl that uses all default methods.
    struct TestSynth;

    impl MidiWriter for TestSynth {
        fn write_byte(&mut self, _data: u8) {}
    }

    impl MidiSynth for TestSynth {
        fn generate_samples(&mut self, _count: usize, _sample_rate: u32, _output: &mut [f32]) {}
        fn name(&self) -> &'static str {
            "test"
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn test_midi_synth_default_active_voice_count() {
        let synth = TestSynth;
        assert_eq!(synth.active_voice_count(), 0);
    }

    #[test]
    fn test_midi_synth_default_needs_boot() {
        let synth = TestSynth;
        assert!(!synth.needs_boot());
    }

    // ---- IoPort default method tests ----
    #[test]
    fn test_ioport_default_set_cpu_cycles() {
        let mut io: () = ();
        IoPort::set_cpu_cycles(&mut io, 12345);
        // Just verify it doesn't panic - default is a no-op
    }

    #[test]
    fn test_ioport_default_io_delay_total() {
        let io: () = ();
        assert_eq!(IoPort::io_delay_total(&io), 0);
    }

    #[test]
    fn test_ioport_default_vga_pipeline_dirty() {
        let io: () = ();
        assert!(!IoPort::vga_pipeline_dirty(&io));
    }

    #[test]
    fn test_ioport_default_take_vga_pipeline_state() {
        let mut io: () = ();
        let state = IoPort::take_vga_pipeline_state(&mut io);
        assert_eq!(state, VgaPipelineState::default());
    }

    #[test]
    fn test_ioport_default_should_break_batch() {
        let io: () = ();
        assert!(!IoPort::should_break_batch(&io));
    }

    // ---- VgaPipelineState tests ----
    #[test]
    fn test_vga_pipeline_state_default() {
        let state = VgaPipelineState::default();
        assert_eq!(state.video_mode, 0);
        assert!(!state.chain4);
        assert_eq!(state.plane_mask, 0);
        assert_eq!(state.write_mode, 0);
        assert_eq!(state.bit_mask, 0);
    }

    // ---- SynthId label and from_u8 tests ----
    #[test]
    fn test_synth_id_label() {
        assert_eq!(SynthId::Mt32.label(), "MT-32");
        assert_eq!(SynthId::Sc55.label(), "SC-55");
        assert_eq!(SynthId::Syxg50.label(), "S-YXG50");
        assert_eq!(SynthId::Ferrosintesis.label(), "Ferrosintesis");
    }

    #[test]
    fn test_synth_id_from_u8() {
        assert_eq!(SynthId::from_u8(0), Some(SynthId::Mt32));
        assert_eq!(SynthId::from_u8(1), Some(SynthId::Sc55));
        assert_eq!(SynthId::from_u8(2), Some(SynthId::Sc55));
        assert_eq!(SynthId::from_u8(3), Some(SynthId::Syxg50));
        assert_eq!(SynthId::from_u8(4), Some(SynthId::Ferrosintesis));
        assert_eq!(SynthId::from_u8(5), None);
        assert_eq!(SynthId::from_u8(255), None);
    }

    // ---- SynthId settings round-trip tests ----
    #[test]
    fn test_synth_settings_round_trip() {
        let variants = [
            SynthId::Mt32,
            SynthId::Sc55,
            SynthId::Syxg50,
            SynthId::Ferrosintesis,
        ];
        for v in variants {
            let s = v.as_settings_str();
            let parsed = SynthId::from_settings_str(s);
            assert_eq!(parsed, Some(v), "round-trip failed for {:?} -> {:?}", v, s);
        }
    }

    #[test]
    fn test_synth_settings_unknown_returns_none() {
        assert_eq!(SynthId::from_settings_str("unknown"), None);
        assert_eq!(SynthId::from_settings_str(""), None);
        assert_eq!(SynthId::from_settings_str("MT-32"), None); // case-sensitive
        assert_eq!(SynthId::from_settings_str("awe-32"), None);
    }

    #[test]
    fn test_legacy_mu50_settings_migrate_to_sc55() {
        assert_eq!(SynthId::from_settings_str("mu50"), Some(SynthId::Sc55));
        assert_eq!(SynthId::from_settings_str("mu-50"), Some(SynthId::Sc55));
    }

    // ---- IoPort default method coverage ----

    #[test]
    fn test_ioport_default_methods() {
        let mut io: () = ();
        // set_cpu_cycles has a default no-op impl
        IoPort::set_cpu_cycles(&mut io, 12345);
        // io_delay_total default returns 0
        assert_eq!(IoPort::io_delay_total(&io), 0);
        // vga_pipeline_dirty default returns false
        assert!(!IoPort::vga_pipeline_dirty(&io));
        // take_vga_pipeline_state default returns default
        let state = IoPort::take_vga_pipeline_state(&mut io);
        assert_eq!(state, VgaPipelineState::default());
        // should_break_batch default returns false
        assert!(!IoPort::should_break_batch(&io));
    }

    // ---- VgaPipelineState coverage ----

    #[test]
    fn test_vga_pipeline_state_debug_clone_eq() {
        let s1 = VgaPipelineState::default();
        let s2 = s1;
        assert_eq!(s1, s2);
        let dbg = format!("{s1:?}");
        assert!(dbg.contains("VgaPipelineState"));
    }

    #[test]
    fn test_vga_pipeline_state_fields() {
        let state = VgaPipelineState {
            video_mode: 0x13,
            chain4: true,
            seq_odd_even_disabled: true,
            gc_odd_even_read: false,
            is_mode_x: false,
            plane_mask: 0x0F,
            read_plane: 0,
            memory_map_select: 1,
            gc_graphics_mode: true,
            write_mode: 0,
            bit_mask: 0xFF,
            set_reset: 0,
            enable_set_reset: 0,
            data_rotate: 0,
            read_mode: 0,
            color_compare: 0,
            color_dont_care: 0x0F,
        };
        assert_eq!(state.video_mode, 0x13);
        assert!(state.chain4);
        assert_eq!(state.plane_mask, 0x0F);
    }

    // ---- SynthId coverage ----

    #[test]
    fn test_synth_id_label_v2() {
        assert_eq!(SynthId::Mt32.label(), "MT-32");
        assert_eq!(SynthId::Sc55.label(), "SC-55");
        assert_eq!(SynthId::Syxg50.label(), "S-YXG50");
        assert_eq!(SynthId::Ferrosintesis.label(), "Ferrosintesis");
    }

    #[test]
    fn test_synth_id_from_u8_all() {
        assert_eq!(SynthId::from_u8(0), Some(SynthId::Mt32));
        assert_eq!(SynthId::from_u8(1), Some(SynthId::Sc55));
        assert_eq!(SynthId::from_u8(2), Some(SynthId::Sc55));
        assert_eq!(SynthId::from_u8(3), Some(SynthId::Syxg50));
        assert_eq!(SynthId::from_u8(4), Some(SynthId::Ferrosintesis));
        assert_eq!(SynthId::from_u8(5), None);
        assert_eq!(SynthId::from_u8(255), None);
    }

    #[test]
    fn test_synth_id_debug_clone_eq() {
        let id = SynthId::Sc55;
        let cloned = id;
        assert_eq!(id, cloned);
        let dbg = format!("{id:?}");
        assert!(dbg.contains("Sc55"));
    }

    #[test]
    fn test_synth_id_repr_u8() {
        assert_eq!(SynthId::Mt32 as u8, 0);
        assert_eq!(SynthId::Sc55 as u8, 1);
        assert_eq!(SynthId::Syxg50 as u8, 3);
        assert_eq!(SynthId::Ferrosintesis as u8, 4);
    }
}
