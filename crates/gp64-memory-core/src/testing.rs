//! `NullMachine`: the audited test machine (LLD 3.1).
//!
//! A flat, identity-mapped RAM machine with no devices, no A20, no ROM and no
//! fetch alias. It exists so the trait and the core can be exercised without
//! any PC type, and so later steps can drive generic engines over a second
//! machine. Compiled only for this crate's tests and under feature `test-util`,
//! which `tests/config_surface.rs` keeps out of every production graph.
//!
//! **Armed pages (Seam D).** A test can arm a page for reads, writes or both,
//! using the same ownership contract as a device-backed page: the arm commit marks code on the page
//! stale and bumps the
//! memory policy epoch (a full TLB flush), `direct_span`, the TLB refill and
//! `raw_ram_span` withhold the armed views, and every guest access that
//! reaches an armed page through the checked route is logged once per
//! access as an [`ArmedAccess`]. A store that reaches an armed page by any
//! other route is missing from the log. The DRC store-helper proof (LLD
//! 2026.10.08 M3b.6, §6.5) drives every store helper at an armed page and
//! asserts exactly one logged write per guest store.

use std::cell::RefCell;

use gp64_types::{MemoryAccess, PagedMemory};

use crate::machine::__audited::AuditedMachine;
use crate::{
    AccessIntent, AccessPolicyCore, ArchPhys, BackingOffset, BackingPage, DirectPage, DirectSpan,
    GuestPhys, LinearPage, MachineId, MemoryCore, PagePerms, PhysPage, RamBasePtr, TlbFill,
    X86Machine, INSTR_EXEC_PAGE_SIZE,
};

/// `NullMachine`'s per-run policy: only the core-owned part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NullPolicy {
    pub smc_notifier: bool,
    pub descriptor_snoop: bool,
}

impl AccessPolicyCore for NullPolicy {
    #[inline]
    fn smc_notifier(self) -> bool {
        self.smc_notifier
    }

    #[inline]
    fn descriptor_snoop(self) -> bool {
        self.descriptor_snoop
    }

    /// No address-line gate.
    #[inline]
    fn linear_mask(self) -> u32 {
        u32::MAX
    }
}

/// One guest access that reached an armed page through the checked route:
/// its first byte, its width in bytes, and whether it stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArmedAccess {
    pub addr: usize,
    pub width: u8,
    pub write: bool,
}

/// Per-page arm bits.
const ARM_READ: u8 = 1;
const ARM_WRITE: u8 = 2;

/// Flat identity RAM over a [`MemoryCore`].
pub struct NullMachine {
    // Field order: `core` captured `ram`'s heap pointer, which a move of
    // `NullMachine` does not change (the `Vec` buffer stays put).
    core: MemoryCore,
    ram: Vec<u8>,
    fault_pending: bool,
    fault_address: u32,
    fault_error_code: u32,
    /// `ARM_READ` / `ARM_WRITE` per RAM page.
    armed: Vec<u8>,
    /// How many pages are armed, so an unarmed machine skips the page walk.
    armed_pages: usize,
    /// Checked-route accesses that touched an armed page, in order.
    armed_log: RefCell<Vec<ArmedAccess>>,
}

impl NullMachine {
    /// A machine with `ram_bytes` of zeroed RAM (rounded up to whole pages).
    #[must_use]
    pub fn new(ram_bytes: usize) -> Self {
        let len = ram_bytes.next_multiple_of(4096);
        let mut ram = vec![0u8; len];
        let base = RamBasePtr(ram.as_mut_ptr());
        let pages = len / 4096;
        let exec_words = len.div_ceil(INSTR_EXEC_PAGE_SIZE).div_ceil(64);
        Self {
            core: MemoryCore::new(base, len, vec![0; exec_words], pages),
            ram,
            fault_pending: false,
            fault_address: 0,
            fault_error_code: 0,
            armed: vec![0; pages],
            armed_pages: 0,
            armed_log: RefCell::new(Vec::new()),
        }
    }

    /// RAM length in bytes.
    #[must_use]
    pub fn ram_len(&self) -> usize {
        self.ram.len()
    }

    /// Mutable access to the core, for tests that drive core primitives.
    pub fn core_mut(&mut self) -> &mut MemoryCore {
        &mut self.core
    }

    fn in_ram(&self, addr: usize, len: usize) -> bool {
        addr.checked_add(len)
            .is_some_and(|end| end <= self.ram.len())
    }

    /// Arm RAM page `page` for reads, writes or both, replacing its previous
    /// arm, and commit the ownership change:
    /// code on the page is marked stale and the memory policy epoch is bumped
    /// (a full TLB flush, so every view refills through the withholding
    /// `tlb_insert`). Arming nothing disarms the page and republishes.
    ///
    /// # Panics
    /// If `page` is outside RAM.
    pub fn arm_page(&mut self, page: u32, read: bool, write: bool) {
        let index = page as usize;
        assert!(
            index < self.armed.len(),
            "arm_page: page {page:#x} is outside RAM"
        );
        let bits = if read { ARM_READ } else { 0 } | if write { ARM_WRITE } else { 0 };
        match (self.armed[index] != 0, bits != 0) {
            (false, true) => self.armed_pages += 1,
            (true, false) => self.armed_pages -= 1,
            _ => {}
        }
        self.armed[index] = bits;
        let start = index << 12;
        let ram_len = self.ram.len();
        self.core
            .mark_instr_cache_dirty_range(|| ram_len, std::iter::empty, start, 4096);
        self.core.smc_before_write(start, 4096);
        self.core.bump_memory_policy_epoch();
    }

    /// Whether page `page` is armed for `(reads, writes)`.
    #[must_use]
    pub fn page_armed(&self, page: u32) -> (bool, bool) {
        let bits = self.armed.get(page as usize).copied().unwrap_or(0);
        (bits & ARM_READ != 0, bits & ARM_WRITE != 0)
    }

    /// Drain the armed-access log.
    pub fn take_armed_accesses(&mut self) -> Vec<ArmedAccess> {
        std::mem::take(self.armed_log.get_mut())
    }

    /// The union of the arm bits of every RAM page `[addr, addr + len)`
    /// touches (bytes past RAM have none).
    fn armed_bits(&self, addr: usize, len: usize) -> u8 {
        if len == 0 || self.armed_pages == 0 {
            return 0;
        }
        let last = addr.saturating_add(len - 1);
        (addr >> 12..=last >> 12)
            .map_while(|page| self.armed.get(page))
            .fold(0, |acc, &bits| acc | bits)
    }

    /// Log a checked-route access that touches a page armed for its kind.
    fn note_armed(&self, addr: usize, width: u8, write: bool) {
        let want = if write { ARM_WRITE } else { ARM_READ };
        if self.armed_bits(addr, usize::from(width)) & want != 0 {
            self.armed_log
                .borrow_mut()
                .push(ArmedAccess { addr, width, write });
        }
    }

    fn read_byte(&self, addr: usize) -> u8 {
        self.ram.get(addr).copied().unwrap_or(0xFF)
    }

    fn write_byte(&mut self, addr: usize, value: u8) {
        if addr < self.ram.len() {
            self.core.smc_before_write(addr, 1);
            self.ram[addr] = value;
        }
    }
}

impl NullMachine {
    fn read_le<const W: usize>(&self, addr: usize) -> u64 {
        self.note_armed(addr, W as u8, false);
        (0..W).fold(0, |acc, i| {
            acc | (u64::from(self.read_byte(addr.wrapping_add(i))) << (8 * i))
        })
    }

    fn write_le<const W: usize>(&mut self, addr: usize, value: u64) {
        self.note_armed(addr, W as u8, true);
        for i in 0..W {
            self.write_byte(addr.wrapping_add(i), (value >> (8 * i)) as u8);
        }
    }
}

impl MemoryAccess for NullMachine {
    fn read_u8(&self, addr: usize) -> u8 {
        self.read_le::<1>(addr) as u8
    }

    fn read_u16(&self, addr: usize) -> u16 {
        self.read_le::<2>(addr) as u16
    }

    fn read_u32(&self, addr: usize) -> u32 {
        self.read_le::<4>(addr) as u32
    }

    fn write_u8(&mut self, addr: usize, value: u8) {
        self.write_le::<1>(addr, u64::from(value));
    }

    fn write_u16(&mut self, addr: usize, value: u16) {
        self.write_le::<2>(addr, u64::from(value));
    }

    fn write_u32(&mut self, addr: usize, value: u32) {
        self.write_le::<4>(addr, u64::from(value));
    }

    fn read_u64(&self, addr: usize) -> u64 {
        self.read_le::<8>(addr)
    }

    fn write_u64(&mut self, addr: usize, value: u64) {
        self.write_le::<8>(addr, value);
    }
}

impl PagedMemory for NullMachine {
    fn map_linear_physical(&self, _linear: u32) -> Option<usize> {
        None
    }

    fn tlb_lookup(&self, linear_page: u32) -> Option<(u32, bool, bool, bool)> {
        self.core.tlb_lookup(linear_page)
    }

    fn tlb_insert(
        &mut self,
        linear_page: u32,
        physical_page: u32,
        writable: bool,
        user: bool,
        dirty: bool,
    ) {
        let in_ram = self.in_ram((physical_page as usize) << 12, 4096);
        // An armed page withholds the view of each armed kind (seam D U-D1).
        let (read_armed, write_armed) = self.page_armed(physical_page);
        let direct = (in_ram && !(read_armed && write_armed))
            .then(|| DirectPage::new(BackingPage(physical_page), !read_armed, !write_armed));
        // SAFETY: a proof is issued only for a whole page inside `ram`, which
        // is plain identity RAM with no side effects (V1-V4, D2), and never
        // for a view of an armed kind.
        unsafe {
            self.core.tlb_fill(TlbFill {
                linear: LinearPage(linear_page),
                phys: PhysPage(physical_page),
                perms: PagePerms {
                    writable,
                    user,
                    dirty,
                },
                direct,
            });
        }
    }

    fn read_u32_physical(&self, addr: usize) -> u32 {
        self.read_u32(addr)
    }

    fn read_u8_physical_checked(&self, addr: usize) -> Option<u8> {
        self.ram.get(addr).copied()
    }

    fn write_u32_physical(&mut self, addr: usize, value: u32) {
        self.write_u32(addr, value);
    }

    fn set_page_fault(&mut self, linear_address: u32, error_code: u32) {
        self.fault_pending = true;
        self.fault_address = linear_address;
        self.fault_error_code = error_code;
    }

    fn clear_page_fault(&mut self) {
        self.fault_pending = false;
        self.fault_address = 0;
        self.fault_error_code = 0;
    }

    fn page_fault_pending(&self) -> bool {
        self.fault_pending
    }

    fn page_fault_address(&self) -> u32 {
        self.fault_address
    }

    fn page_fault_error_code(&self) -> u32 {
        self.fault_error_code
    }

    fn invalidate_tlb(&mut self) {
        self.core.invalidate_tlb();
    }

    fn invalidate_tlb_page(&mut self, linear_addr: u32) {
        self.core.invalidate_tlb_page(linear_addr);
    }
}

// SAFETY: test machine on the LLD 3.1 allow-list.
unsafe impl AuditedMachine for NullMachine {}

// SAFETY: every proof covers bytes inside `ram`, a plain heap buffer with no
// device or ROM behaviour; backing offset == guest physical (D2).
unsafe impl X86Machine for NullMachine {
    const MACHINE_ID: MachineId = MachineId::NULL;

    type AccessPolicy = NullPolicy;
    type Posted = ();

    const DEVICE_DISPATCH: bool = false;
    const NATIVE_DEVICE_COMMIT: bool = false;
    const WRITE_TRACKING: bool = false;
    const DPMI_MAPPINGS: bool = false;
    const QUIET_POLL_BOUNDARY: bool = false;
    /// Open bus above RAM, as the PC.
    const OPEN_BUS_FETCH_ABOVE_RAW_LIMIT: bool = true;

    fn core(&self) -> &MemoryCore {
        &self.core
    }

    const CORE_OFFSET: usize = core::mem::offset_of!(NullMachine, core);

    fn access_policy(&self) -> NullPolicy {
        NullPolicy {
            smc_notifier: self.core.has_smc_notifier(),
            descriptor_snoop: self.core.has_descriptor_snoop(),
        }
    }

    fn phys_mask(_p: NullPolicy, a: ArchPhys) -> GuestPhys {
        GuestPhys(a.0)
    }

    fn raw_limit(&self) -> usize {
        self.ram.len()
    }

    /// Conservative: no bulk raw window (the checked route is always correct).
    fn bulk_raw_limit(_p: NullPolicy) -> Option<usize> {
        None
    }

    fn fetch_alias(&self, a: GuestPhys) -> GuestPhys {
        a
    }

    fn direct_span(&self, a: GuestPhys, len: usize, intent: AccessIntent) -> Option<DirectSpan> {
        if !self.in_ram(a.0, len) {
            return None;
        }
        // Armed pages refuse every span of an armed kind; a read-armed page
        // refuses `Fetch` too (seam D addendum, "Fetch").
        let armed = self.armed_bits(a.0, len);
        if (intent.writes() && armed & ARM_WRITE != 0)
            || ((intent.reads() || intent == AccessIntent::Fetch) && armed & ARM_READ != 0)
        {
            return None;
        }
        // SAFETY: the span lies inside `ram` (V1-V4); D2 holds by identity.
        unsafe { self.core.direct_span(BackingOffset(a.0), len, intent) }
    }

    fn fetch_checked(&self, a: GuestPhys, dst: &mut [u8]) {
        for (i, byte) in dst.iter_mut().enumerate() {
            *byte = self.read_byte(a.0.wrapping_add(i));
        }
    }

    fn backing_page(&self, a: PhysPage) -> Option<BackingPage> {
        self.in_ram((a.0 as usize) << 12, 1)
            .then_some(BackingPage(a.0))
    }

    fn set_page_watched(&mut self, page: BackingPage, on: bool) {
        self.core.set_page_watched(page.0, on);
    }

    fn smc_bind_notifier(&mut self, notifier: Option<std::sync::Arc<dyn gp64_types::SmcNotify>>) {
        self.core.bind_smc(notifier);
    }

    fn smc_set_has_code_pages(&mut self, has_pages: bool) {
        self.core.set_smc_has_code_pages(has_pages);
    }

    fn pt_tripwire_note_physical_write(&mut self, a: GuestPhys, len: usize) {
        self.core.pt_tripwire_note_physical_write(a.0, len);
    }

    fn smc_before_write(&mut self, a: GuestPhys, len: usize) -> bool {
        self.core.smc_before_write(a.0, len)
    }

    fn note_direct_write(&mut self, a: GuestPhys, len: usize) {
        // The LLD 1.4 V6 core duties, in the funnel's order.
        self.core.smc_before_write(a.0, len);
        self.core.snoop_descriptor_write(a.0, len);
        let ram_len = self.ram.len();
        self.core
            .mark_instr_cache_dirty_range(|| ram_len, std::iter::empty, a.0, len);
    }

    const DEVICE_WRITE_DELAY: bool = false;

    fn device_transaction_pending(&self) -> bool {
        false
    }

    /// Armed pages are not passive: a page-walk read of one goes through the
    /// checked route.
    fn physical_span_is_passive(&self, a: GuestPhys, len: usize) -> bool {
        self.armed_bits(a.0, len) == 0
    }

    fn descriptor_snoop_set_gdt(&mut self, base: u32, limit: u16) {
        self.core.set_gdt_snoop_range(base, limit);
    }

    fn descriptor_snoop_set_ldt(&mut self, base: u32, limit: u32) {
        self.core.set_ldt_snoop_range(base, limit);
    }

    fn descriptor_snoop_clear_ldt(&mut self) {
        self.core.clear_ldt_snoop_range();
    }

    /// All of `NullMachine`'s unarmed RAM is plain identity RAM, so its bulk
    /// paths may run raw; a span over an armed page takes the per-element
    /// checked route (seam D addendum, "REP bulk").
    fn raw_ram_span(&self, a: GuestPhys, len: usize) -> bool {
        a.0.checked_add(len)
            .is_some_and(|end| end <= self.ram.len())
            && self.armed_bits(a.0, len) == 0
    }
}
