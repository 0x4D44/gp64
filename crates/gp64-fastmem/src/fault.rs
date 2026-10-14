//! HLE fastmem stage 3 (HLD `2026.09.24 - HLD - HLE fastmem for the DRC`):
//! the host fault handler that sends a faulting JIT fastmem load to its
//! checked fallback.
//!
//! # Site encoding (why no side table can go stale)
//!
//! Every fastmem load the DRC emits is a register-offset load whose base is
//! [`FASTMEM_SITE_BASE_REG`] (the JIT window base, `x9`), followed at once by
//! a never-taken `CBNZ XZR, <fallback>`:
//!
//! ```text
//!     ldr  w10, [x9, x11]        ; the site: faults on a protected page
//!     cbnz xzr, fallback         ; the marker: never taken, names the fallback
//! ```
//!
//! The handler decodes the faulting instruction and the word after it. The
//! code IS the table: whatever bytes sit at the faulting PC now decide the
//! redirect, so a retired block, a flushed code cache or a reused arena range
//! can never leave an entry that redirects a different instruction. Nothing
//! is published, nothing is freed, there is nothing to invalidate.
//!
//! # Signal safety
//!
//! The handler reads the fault address and PC from its arguments, reads a
//! fixed array of atomics (the window registry), reads two instruction words,
//! and writes the PC back. No allocation, no locks, no TLS. The instruction
//! words are read only after the fault address proved to lie inside a
//! registered fastmem window: only emitted code touches a window, so an
//! unrelated crash (a null deref in Rust, a jump to a bad address) never gets
//! as far as reading code and is forwarded untouched.
//!
//! # Chaining
//!
//! [`install_fault_handler`] records the `SIGSEGV`/`SIGBUS` actions in force
//! (the crash-diagnostics handler in `mdpc`, Rust's stack-overflow handler in
//! a test binary) and forwards every fault that is not a fastmem site to them.
//! It must therefore run AFTER those are installed; `mdpc` installs crash
//! diagnostics first thing in `main`, and this runs when the first fastmem
//! view is created. A debugger (lldb) sees `EXC_BAD_ACCESS` before the
//! handler; continue past it (`process handle SIGBUS -s false`).

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Host register every fastmem load site uses as its base (`VALUE_TMP` in the
/// a64 DRC). The handler refuses any other base.
pub const FASTMEM_SITE_BASE_REG: u8 = 9;

/// `CBNZ Xt, #0` with `Rt = XZR`: the site marker before its imm19 is patched.
pub const FASTMEM_SITE_MARKER_BASE: u32 = 0xB500_001F;
const MARKER_MASK: u32 = 0xFF00_001F;

/// Whether `word` is a load with register offset whose base register is
/// [`FASTMEM_SITE_BASE_REG`] (integer `LDR*`/`LDRS*` or SIMD `LDR`).
#[must_use]
pub fn is_site_load(word: u32) -> bool {
    // Load/store register (register offset): size 111 V 00 opc 1 Rm option S 10 Rn Rt.
    if word & 0x3B20_0C00 != 0x3820_0800 {
        return false;
    }
    let simd = word & (1 << 26) != 0;
    let opc = (word >> 22) & 3;
    let load = if simd { opc & 1 == 1 } else { opc != 0 };
    load && ((word >> 5) & 0x1F) == u32::from(FASTMEM_SITE_BASE_REG)
}

/// Decode a fastmem site: `load` at `pc`, `marker` at `pc + 4`. Returns the
/// fallback address the marker names, or `None` when the pair is not a site.
#[must_use]
pub fn decode_site(pc: u64, load: u32, marker: u32) -> Option<u64> {
    if !is_site_load(load) || marker & MARKER_MASK != FASTMEM_SITE_MARKER_BASE {
        return None;
    }
    // imm19, sign-extended, in words, relative to the marker itself.
    let imm19 = ((marker >> 5) & 0x7_FFFF) as i32;
    let words = (imm19 << 13) >> 13;
    Some(
        pc.wrapping_add(4)
            .wrapping_add((i64::from(words) * 4) as u64),
    )
}

// ---------------------------------------------------------------------------
// Window registry
// ---------------------------------------------------------------------------

/// Registered fastmem windows at once. One per live `Memory` with a view, plus
/// the process-lifetime dead window; the test harness runs a few dozen at most.
const MAX_WINDOWS: usize = 128;
/// A slot claimed by a registration in progress or being torn down.
const SLOT_BUSY: usize = 1;

#[allow(clippy::declare_interior_mutable_const)]
const EMPTY: AtomicUsize = AtomicUsize::new(0);
/// Alias-mapping base per slot: 0 free, [`SLOT_BUSY`] claimed, else live.
static WINDOW_BASE: [AtomicUsize; MAX_WINDOWS] = [EMPTY; MAX_WINDOWS];
/// Mapping length per slot, valid while its base is live.
static WINDOW_LEN: [AtomicUsize; MAX_WINDOWS] = [EMPTY; MAX_WINDOWS];

/// Register a fastmem host mapping `[base, base + len)`. `None` when the
/// registry is full: the caller must then not offer fastmem, because a fault
/// in an unregistered window would reach the crash handler.
pub fn register_window(base: usize, len: usize) -> Option<usize> {
    debug_assert!(base > SLOT_BUSY);
    for (slot, cell) in WINDOW_BASE.iter().enumerate() {
        if cell
            .compare_exchange(0, SLOT_BUSY, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            WINDOW_LEN[slot].store(len, Ordering::Relaxed);
            ring_reset(slot);
            cell.store(base, Ordering::Release);
            return Some(slot);
        }
    }
    None
}

/// Remove a registration before its mapping is unmapped.
pub fn unregister_window(slot: usize) {
    WINDOW_BASE[slot].store(SLOT_BUSY, Ordering::Release);
    WINDOW_LEN[slot].store(0, Ordering::Relaxed);
    WINDOW_BASE[slot].store(0, Ordering::Release);
}

/// The registered `(slot, mapping base)` containing `addr`. Signal-safe.
fn window_containing(addr: usize) -> Option<(usize, usize)> {
    for (slot, cell) in WINDOW_BASE.iter().enumerate() {
        let base = cell.load(Ordering::Acquire);
        if base <= SLOT_BUSY || addr < base {
            continue;
        }
        let len = WINDOW_LEN[slot].load(Ordering::Relaxed);
        if addr - base < len {
            return Some((slot, base));
        }
    }
    None
}

/// JIT base of a process-lifetime window that is `PROT_NONE` throughout.
/// Emitted fastmem code runs against it when its `Memory` has no fastmem view
/// (every load faults to its checked fallback: correct, only slow).
#[must_use]
pub fn dead_window_jit_base() -> Option<*const u8> {
    static DEAD: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let base = (*DEAD.get_or_init(|| {
        let len = crate::RAM_RESERVATION_BYTES;
        // SAFETY: a fresh anonymous no-access reservation, never unmapped.
        let raw = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return None;
        }
        let base = raw as usize;
        if register_window(base, len).is_none() {
            // SAFETY: unmaps exactly the reservation made above.
            unsafe { libc::munmap(raw, len) };
            return None;
        }
        install_fault_handler();
        Some(base)
    }))?;
    Some((base + crate::FASTMEM_PHYS_BIAS) as *const u8)
}

// ---------------------------------------------------------------------------
// Counters
// ---------------------------------------------------------------------------

static FAULTS_TOTAL: AtomicU64 = AtomicU64::new(0);
static CENSUS_ON: AtomicBool = AtomicBool::new(false);

const CENSUS_SLOTS: usize = 4096;
#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);
/// Census rows keyed by host PC (open addressing, never removed): PC, count,
/// last fault address as a guest linear address.
static CENSUS_PC: [AtomicU64; CENSUS_SLOTS] = [ZERO; CENSUS_SLOTS];
static CENSUS_COUNT: [AtomicU64; CENSUS_SLOTS] = [ZERO; CENSUS_SLOTS];
static CENSUS_LINEAR: [AtomicU64; CENSUS_SLOTS] = [ZERO; CENSUS_SLOTS];
static CENSUS_LOST: AtomicU64 = AtomicU64::new(0);

/// Record per-site fault rows (behind `MDEM_DRC_DIAG=census`; the total is
/// always counted).
pub fn set_fault_census(on: bool) {
    CENSUS_ON.store(on, Ordering::Relaxed);
}

fn census_note(pc: u64, linear: u64) {
    let start = ((pc >> 2).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 52) as usize;
    for probe in 0..CENSUS_SLOTS {
        let slot = (start + probe) % CENSUS_SLOTS;
        let key = CENSUS_PC[slot].load(Ordering::Relaxed);
        let owned = key == pc
            || (key == 0
                && CENSUS_PC[slot]
                    .compare_exchange(0, pc, Ordering::Relaxed, Ordering::Relaxed)
                    .map_or_else(|current| current == pc, |_| true));
        if owned {
            CENSUS_COUNT[slot].fetch_add(1, Ordering::Relaxed);
            CENSUS_LINEAR[slot].store(linear, Ordering::Relaxed);
            return;
        }
    }
    CENSUS_LOST.fetch_add(1, Ordering::Relaxed);
}

/// Per-window fault rings. The handler appends `(pc, linear | width << 32)`
/// for every redirected fault (one atomic add, two stores); the owner of the
/// window drains its own ring outside signal context
/// ([`drain_window_faults`]), classifies each fault against the guest state
/// of that moment and may demote the site. A ring is per window so a drain
/// never classifies another `Memory`'s fault against the wrong page tables.
const RING: usize = 1024;
#[allow(clippy::declare_interior_mutable_const)]
const ZERO_RING: [AtomicU64; RING] = [ZERO; RING];
static RING_PC: [[AtomicU64; RING]; MAX_WINDOWS] = [ZERO_RING; MAX_WINDOWS];
static RING_ADDR: [[AtomicU64; RING]; MAX_WINDOWS] = [ZERO_RING; MAX_WINDOWS];
static RING_HEAD: [AtomicU64; MAX_WINDOWS] = [ZERO; MAX_WINDOWS];
/// Consumer position; written only by the window's single drainer.
static RING_TAIL: [AtomicU64; MAX_WINDOWS] = [ZERO; MAX_WINDOWS];
/// Faults a full ring dropped before a drain (never demoted from that entry;
/// the site faults again and is seen then).
static RING_OVERRUN: AtomicU64 = AtomicU64::new(0);

/// Signal-safe append.
fn ring_push(window: usize, pc: u64, linear: u64, width: u32) {
    let index = RING_HEAD[window].fetch_add(1, Ordering::AcqRel);
    let cell = index as usize % RING;
    RING_ADDR[window][cell].store(linear | (u64::from(width) << 32), Ordering::Relaxed);
    RING_PC[window][cell].store(pc, Ordering::Release);
}

/// Discard a slot's queued faults (a new registration of a reused slot).
fn ring_reset(window: usize) {
    let head = RING_HEAD[window].load(Ordering::Acquire);
    for cell in 0..RING {
        RING_PC[window][cell].store(0, Ordering::Relaxed);
    }
    RING_TAIL[window].store(head, Ordering::Release);
}

/// Bytes a site load reads: `1 << size`, or 16 for a SIMD `LDR Q`.
#[must_use]
pub fn site_load_width(word: u32) -> u32 {
    let simd = word & (1 << 26) != 0;
    if simd && (word >> 23) & 1 == 1 {
        16
    } else {
        1 << (word >> 30)
    }
}

/// True when window `window` holds faults not yet drained (two relaxed
/// loads; the dispatch loop polls it).
#[inline]
#[must_use]
pub fn window_faults_pending(window: usize) -> bool {
    window < MAX_WINDOWS
        && RING_HEAD[window].load(Ordering::Relaxed) != RING_TAIL[window].load(Ordering::Relaxed)
}

/// One queued fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedFault {
    /// Host PC of the faulting site load.
    pub pc: u64,
    /// Guest linear address of the access.
    pub linear: u32,
    /// Bytes the load reads.
    pub width: u32,
}

/// Drain window `window`'s queued faults through `handle`, which returns the
/// census reason index of each (see [`FAULT_REASONS`]). Outside signal
/// context, by the window's owner only.
pub fn drain_window_faults(window: usize, mut handle: impl FnMut(QueuedFault) -> usize) {
    if window >= MAX_WINDOWS {
        return;
    }
    let head = RING_HEAD[window].load(Ordering::Acquire);
    let mut tail = RING_TAIL[window].load(Ordering::Relaxed);
    if head.saturating_sub(tail) > RING as u64 {
        RING_OVERRUN.fetch_add(head - tail - RING as u64, Ordering::Relaxed);
        tail = head - RING as u64;
    }
    let census = CENSUS_ON.load(Ordering::Relaxed);
    let mut classifications = Vec::new();
    while tail < head {
        let cell = tail as usize % RING;
        tail += 1;
        let pc = RING_PC[window][cell].swap(0, Ordering::Acquire);
        if pc == 0 {
            // Not yet written by a racing append, or overwritten: dropped.
            RING_OVERRUN.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let addr = RING_ADDR[window][cell].load(Ordering::Relaxed);
        let fault = QueuedFault {
            pc,
            linear: addr as u32,
            width: (addr >> 32) as u32,
        };
        let reason = handle(fault).min(FAULT_REASONS - 1);
        if census {
            classifications.push((pc, reason));
        }
    }
    // Callbacks may inspect diagnostic snapshots. Acquire the census mutex
    // only after every callback has returned, then merge the bounded batch.
    if !classifications.is_empty() {
        let mut state = RING_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.by_slot.is_empty() {
            state.by_slot = vec![[0; FAULT_REASONS]; CENSUS_SLOTS];
        }
        for (pc, reason) in classifications {
            state.totals[reason] += 1;
            if let Some(slot) = census_slot_of(pc) {
                state.by_slot[slot][reason] += 1;
            }
        }
    }
    RING_TAIL[window].store(tail, Ordering::Release);
}

/// The census row of `pc`, if it has one (not signal context).
fn census_slot_of(pc: u64) -> Option<usize> {
    let start = ((pc >> 2).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 52) as usize;
    for probe in 0..CENSUS_SLOTS {
        let slot = (start + probe) % CENSUS_SLOTS;
        match CENSUS_PC[slot].load(Ordering::Relaxed) {
            key if key == pc => return Some(slot),
            0 => return None,
            _ => {}
        }
    }
    None
}

/// Census reason rows: [`FAULT_REASONS`] indices.
pub const FAULT_REASONS: usize = 8;

struct RingState {
    by_slot: Vec<[u64; FAULT_REASONS]>,
    totals: [u64; FAULT_REASONS],
}

static RING_STATE: std::sync::Mutex<RingState> = std::sync::Mutex::new(RingState {
    by_slot: Vec::new(),
    totals: [0; FAULT_REASONS],
});

/// Classified census faults: totals by reason index, per-site dominant reason
/// keyed by host PC, and faults the rings dropped before a drain.
#[must_use]
pub fn fault_reason_census() -> ([u64; FAULT_REASONS], Vec<(u64, usize)>, u64) {
    let state = RING_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sites = state
        .by_slot
        .iter()
        .enumerate()
        .filter_map(|(slot, counts)| {
            let (reason, &count) = counts.iter().enumerate().max_by_key(|&(_, c)| *c)?;
            (count != 0).then(|| (CENSUS_PC[slot].load(Ordering::Relaxed), reason))
        })
        .collect();
    (state.totals, sites, RING_OVERRUN.load(Ordering::Relaxed))
}

/// One faulting fastmem site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FastmemFaultSite {
    /// Host PC of the faulting load.
    pub host_pc: u64,
    /// Faults taken there.
    pub faults: u64,
    /// Guest linear address of the most recent fault.
    pub last_linear: u32,
}

/// Fault-handler counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FastmemFaultStats {
    /// Faults redirected to a fallback, process-wide.
    pub faults: u64,
    /// Census rows (empty unless the census is on), most faults first.
    pub sites: Vec<FastmemFaultSite>,
    /// Faults the full census table could not record.
    pub census_lost: u64,
}

/// Snapshot the fault counters.
#[must_use]
pub fn fault_stats() -> FastmemFaultStats {
    let mut sites: Vec<FastmemFaultSite> = (0..CENSUS_SLOTS)
        .filter_map(|slot| {
            let host_pc = CENSUS_PC[slot].load(Ordering::Relaxed);
            let faults = CENSUS_COUNT[slot].load(Ordering::Relaxed);
            (host_pc != 0 && faults != 0).then(|| FastmemFaultSite {
                host_pc,
                faults,
                last_linear: CENSUS_LINEAR[slot].load(Ordering::Relaxed) as u32,
            })
        })
        .collect();
    sites.sort_by(|a, b| b.faults.cmp(&a.faults).then(a.host_pc.cmp(&b.host_pc)));
    FastmemFaultStats {
        faults: FAULTS_TOTAL.load(Ordering::Relaxed),
        sites,
        census_lost: CENSUS_LOST.load(Ordering::Relaxed),
    }
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

const SIGNALS: [libc::c_int; 2] = [libc::SIGSEGV, libc::SIGBUS];

/// The actions in force before ours, per [`SIGNALS`] entry. Written once,
/// before our handler is installed (the `sigaction` call orders the writes
/// before any delivery), and only read afterwards.
struct PreviousActions(core::cell::UnsafeCell<[core::mem::MaybeUninit<libc::sigaction>; 2]>);
// SAFETY: see the type comment: one write under `Once`, then read-only.
unsafe impl Sync for PreviousActions {}
static PREVIOUS: PreviousActions = PreviousActions(core::cell::UnsafeCell::new(
    [core::mem::MaybeUninit::uninit(); 2],
));
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install the fastmem fault handler ahead of whatever `SIGSEGV`/`SIGBUS`
/// handlers are in force, chaining to them. Idempotent.
pub fn install_fault_handler() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: first installation; see `install_over_current`.
        unsafe { install_over_current() };
        INSTALLED.store(true, Ordering::Release);
    });
}

/// Record the current actions as the chain targets and install ours.
///
/// # Safety
///
/// No fault may be delivered to this handler concurrently (the chain targets
/// are rewritten non-atomically).
unsafe fn install_over_current() {
    {
        // SAFETY: the previous actions are captured before our handler can
        // run; the new action uses a valid `extern "C"` handler.
        unsafe {
            let previous = &mut *PREVIOUS.0.get();
            for (index, &sig) in SIGNALS.iter().enumerate() {
                let mut old: libc::sigaction = core::mem::zeroed();
                libc::sigaction(sig, core::ptr::null(), &mut old);
                previous[index] = core::mem::MaybeUninit::new(old);
            }
            for &sig in &SIGNALS {
                let mut action: libc::sigaction = core::mem::zeroed();
                action.sa_sigaction = fault_handler
                    as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void)
                    as usize;
                action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(sig, &action, core::ptr::null_mut());
            }
        }
    }
}

/// Whether [`install_fault_handler`] has run.
#[must_use]
pub fn fault_handler_installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

/// The fallback for a fault at `pc` on `addr`, if it is a fastmem site.
///
/// # Safety
///
/// `pc` must be the PC of a faulting data access; its instruction and the
/// following word are read only when `addr` lies in a registered window.
unsafe fn site_fallback(pc: u64, addr: usize) -> Option<SiteFault> {
    let (slot, window) = window_containing(addr)?;
    if pc & 3 != 0 || pc == addr as u64 {
        return None;
    }
    // SAFETY: the fault is a data access to a fastmem window, which only
    // emitted code performs, so `pc` is readable JIT code; a site's marker is
    // emitted in the same buffer right after it.
    let (load, marker) = unsafe {
        (
            core::ptr::read_volatile(pc as *const u32),
            core::ptr::read_volatile((pc + 4) as *const u32),
        )
    };
    let target = decode_site(pc, load, marker)?;
    let linear = (addr - window).wrapping_sub(crate::FASTMEM_PHYS_BIAS) as u64;
    Some(SiteFault {
        target,
        linear,
        slot,
        width: site_load_width(load),
    })
}

struct SiteFault {
    target: u64,
    linear: u64,
    slot: usize,
    width: u32,
}

extern "C" fn fault_handler(sig: libc::c_int, info: *mut libc::siginfo_t, uc: *mut libc::c_void) {
    if !info.is_null() && !uc.is_null() {
        // SAFETY: the kernel passes a valid siginfo and ucontext to an
        // SA_SIGINFO handler; `uc_mcontext` points at the saved state.
        unsafe {
            let addr = (*info).si_addr() as usize;
            let mcontext = (*uc.cast::<libc::ucontext_t>()).uc_mcontext;
            if !mcontext.is_null() {
                let pc = (*mcontext).__ss.__pc;
                if let Some(fault) = site_fallback(pc, addr) {
                    (*mcontext).__ss.__pc = fault.target;
                    FAULTS_TOTAL.fetch_add(1, Ordering::Relaxed);
                    ring_push(fault.slot, pc, fault.linear, fault.width);
                    if CENSUS_ON.load(Ordering::Relaxed) {
                        census_note(pc, fault.linear);
                    }
                    return;
                }
            }
        }
    }
    // SAFETY: forwards to the action recorded at install time.
    unsafe { chain(sig, info, uc) };
}

/// Forward a fault that is not a fastmem site to the previous action.
unsafe fn chain(sig: libc::c_int, info: *mut libc::siginfo_t, uc: *mut libc::c_void) {
    let Some(index) = SIGNALS.iter().position(|&s| s == sig) else {
        return;
    };
    // SAFETY: written once before this handler was installed.
    let previous = unsafe { (*PREVIOUS.0.get())[index].assume_init() };
    let handler = previous.sa_sigaction;
    if handler == libc::SIG_DFL || handler == libc::SIG_IGN {
        // Restore the default and return: the faulting instruction re-runs
        // and the default action (terminate with the signal) applies.
        // SAFETY: `sigaction` is async-signal-safe.
        unsafe {
            let mut action: libc::sigaction = core::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(sig, &action, core::ptr::null_mut());
        }
        return;
    }
    if previous.sa_flags & libc::SA_SIGINFO != 0 {
        // SAFETY: an SA_SIGINFO action's handler has this signature.
        let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
            unsafe { core::mem::transmute(handler) };
        f(sig, info, uc);
    } else {
        // SAFETY: a plain action's handler takes the signal number only.
        let f: extern "C" fn(libc::c_int) = unsafe { core::mem::transmute(handler) };
        f(sig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fresh test process isolates the process-wide census and bounds even a
    // regressed callback that blocks while taking a diagnostic snapshot.
    fn isolated_census_test(name: &str) -> bool {
        const CHILD: &str = "GP64_FASTMEM_CENSUS_TEST";
        if std::env::var(CHILD).as_deref() == Ok(name) {
            return false;
        }
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", name, "--nocapture"])
            .env(CHILD, name)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn census test");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().expect("poll census test") {
                let output = child.wait_with_output().expect("read census test output");
                let stdout = String::from_utf8_lossy(&output.stdout);
                print!("{stdout}");
                assert!(status.success(), "isolated census test failed: {status}");
                assert!(
                    stdout.contains("running 1 test") && stdout.contains(name),
                    "isolated census test must select and execute exactly one test"
                );
                return true;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().expect("kill blocked census test");
                let status = child.wait().expect("reap blocked census test");
                panic!("census callback did not finish within 10 seconds; child reaped: {status}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_census_callback_can_query_the_public_snapshot() {
        if isolated_census_test("fault::tests::a_census_callback_can_query_the_public_snapshot") {
            return;
        }
        set_fault_census(true);
        let pc = 0x7_0000_0080;
        let slot = register_window(0x7200_0000_0000, 0x1000).expect("slot");
        let before = fault_reason_census().0;
        census_note(pc, 0x1000);
        ring_push(slot, pc, 0x1000, 4);
        let mut calls = 0;
        drain_window_faults(slot, |fault| {
            // Fail promptly on the original held mutex rather than hanging.
            // Drop this witness guard before calling the public API itself.
            {
                let _guard = RING_STATE
                    .try_lock()
                    .expect("classification callback must run outside RING_STATE lock");
            }
            assert_eq!(fault_reason_census().0, before);
            assert_eq!(
                fault,
                QueuedFault {
                    pc,
                    linear: 0x1000,
                    width: 4
                }
            );
            calls += 1;
            2
        });
        assert_eq!(calls, 1, "the selected child must execute its callback");
        assert!(!window_faults_pending(slot));
        let (after, sites, _) = fault_reason_census();
        let mut expected = before;
        expected[2] += 1;
        assert_eq!(after, expected);
        assert!(sites.contains(&(pc, 2)));
        unregister_window(slot);
        set_fault_census(false);
    }

    #[test]
    fn draining_preserves_census_mode_counts_and_window_ownership() {
        if isolated_census_test(
            "fault::tests::draining_preserves_census_mode_counts_and_window_ownership",
        ) {
            return;
        }
        let slot = register_window(0x7300_0000_0000, 0x1000).expect("slot");
        let other = register_window(0x7400_0000_0000, 0x1000).expect("other slot");
        let pc = 0x7_0000_00c0;
        set_fault_census(false);
        let before = fault_reason_census();
        ring_push(slot, pc, 0x1000, 4);
        ring_push(other, pc, 0x2000, 8);
        let mut seen = Vec::new();
        drain_window_faults(slot, |fault| {
            seen.push(fault.linear);
            ring_push(slot, pc, 0x1004, 4);
            usize::MAX
        });
        assert_eq!(seen, [0x1000]);
        assert_eq!(
            fault_reason_census(),
            before,
            "census-off must not classify"
        );
        assert!(window_faults_pending(slot), "callback append stays pending");
        assert!(
            window_faults_pending(other),
            "another owner's ring stays pending"
        );

        set_fault_census(true);
        census_note(pc, 0x1004);
        drain_window_faults(slot, |fault| {
            assert_eq!(fault.linear, 0x1004);
            usize::MAX
        });
        let mut expected = before.0;
        expected[FAULT_REASONS - 1] += 1;
        assert_eq!(fault_reason_census().0, expected, "reason is clamped");
        assert!(!window_faults_pending(slot));
        assert!(window_faults_pending(other));
        drain_window_faults(other, |fault| {
            assert_eq!(fault.linear, 0x2000);
            assert_eq!(fault.width, 8);
            2
        });
        expected[2] += 1;
        assert_eq!(
            fault_reason_census().0,
            expected,
            "merge preserves previous totals"
        );
        assert!(!window_faults_pending(other));
        let sites = fault_reason_census().1;
        assert!(
            sites.contains(&(pc, FAULT_REASONS - 1)),
            "merge preserves per-site counts"
        );
        unregister_window(slot);
        unregister_window(other);
        set_fault_census(false);
    }

    fn ldr_w_reg(rt: u32, rn: u32, rm: u32) -> u32 {
        0xB860_6800 | (rm << 16) | (rn << 5) | rt
    }

    fn marker(words: i32) -> u32 {
        FASTMEM_SITE_MARKER_BASE | (((words as u32) & 0x7_FFFF) << 5)
    }

    #[test]
    fn the_census_ring_classifies_each_fault_by_its_own_address() {
        set_fault_census(true);
        let pc = 0x7_0000_0040u64;
        let base = 0x7100_0000_0000usize;
        let slot = register_window(base, 0x1000).expect("slot");
        let before = fault_reason_census().0;
        for linear in [0x1000u64, 0x2000, 0x1004] {
            census_note(pc, linear);
            ring_push(slot, pc, linear, 4);
        }
        let mut seen = Vec::new();
        drain_window_faults(slot, |fault| {
            seen.push(fault);
            if fault.linear >> 12 == 1 {
                2
            } else {
                5
            }
        });
        assert!(!window_faults_pending(slot));
        unregister_window(slot);
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|fault| fault.pc == pc && fault.width == 4));
        let (after, sites, _) = fault_reason_census();
        assert_eq!(after[2] - before[2], 2);
        assert_eq!(after[5] - before[5], 1);
        assert!(sites.contains(&(pc, 2)), "dominant reason per site");
    }

    #[test]
    fn a_site_decodes_to_its_forward_or_backward_fallback() {
        let pc = 0x1_0000_1000u64;
        assert_eq!(
            decode_site(pc, ldr_w_reg(10, 9, 11), marker(7)),
            Some(pc + 4 + 28)
        );
        assert_eq!(
            decode_site(pc, ldr_w_reg(10, 9, 11), marker(-3)),
            Some(pc + 4 - 12)
        );
    }

    #[test]
    fn only_a_base_x9_load_followed_by_the_marker_is_a_site() {
        let pc = 0x4000u64;
        // Other base register.
        assert_eq!(decode_site(pc, ldr_w_reg(10, 8, 11), marker(2)), None);
        // A store with the same shape.
        let str_w = 0xB820_6800 | (11 << 16) | (9 << 5) | 10;
        assert_eq!(decode_site(pc, str_w, marker(2)), None);
        // A real CBNZ on a live register is not a marker.
        let cbnz_x3 = 0xB500_0003 | (2 << 5);
        assert_eq!(decode_site(pc, ldr_w_reg(10, 9, 11), cbnz_x3), None);
        // Byte, sign-extending and SIMD loads are sites.
        let ldrb = 0x3860_6800 | (11 << 16) | (9 << 5) | 2;
        let ldrsh = 0x78A0_6800 | (11 << 16) | (9 << 5) | 2;
        let ldr_d = 0xFC60_6800 | (11 << 16) | (9 << 5) | 2;
        let ldr_x = 0xF860_6800 | (11 << 16) | (9 << 5) | 2;
        for load in [ldrb, ldrsh, ldr_d, ldr_x] {
            assert!(is_site_load(load), "{load:#010x}");
        }
        let str_d = 0xFC20_6800 | (11 << 16) | (9 << 5) | 2;
        assert!(!is_site_load(str_d));
    }

    /// A fault that is not a fastmem site reaches the handler that was in
    /// force before ours (in `mdpc`, crash diagnostics): a null dereference,
    /// and a Rust read of a registered window (the right address range, but
    /// not a site). Each runs in a forked child that dies through the stand-in
    /// crash handler's exit code.
    #[test]
    fn a_fault_that_is_not_a_site_reaches_the_previous_handler() {
        extern "C" fn stand_in_crash_handler(
            _sig: libc::c_int,
            _info: *mut libc::siginfo_t,
            _uc: *mut libc::c_void,
        ) {
            // SAFETY: async-signal-safe process exit.
            unsafe { libc::_exit(42) };
        }

        fn run_child(fault: fn()) -> libc::c_int {
            // SAFETY: the child only installs handlers and faults.
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                // SAFETY: plain signal-handler installation in the child.
                unsafe {
                    for &sig in &SIGNALS {
                        let mut action: libc::sigaction = core::mem::zeroed();
                        action.sa_sigaction = stand_in_crash_handler
                            as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void)
                            as usize;
                        action.sa_flags = libc::SA_SIGINFO;
                        libc::sigemptyset(&mut action.sa_mask);
                        libc::sigaction(sig, &action, core::ptr::null_mut());
                    }
                    install_over_current();
                }
                fault();
                // SAFETY: reached only if the fault did not happen.
                unsafe { libc::_exit(7) };
            }
            let mut status = 0;
            // SAFETY: waits for our own child.
            unsafe { libc::waitpid(pid, &mut status, 0) };
            assert!(libc::WIFEXITED(status), "child status {status:#x}");
            libc::WEXITSTATUS(status)
        }

        fn null_deref() {
            // SAFETY: deliberately faults.
            unsafe { core::ptr::read_volatile(0x10 as *const u32) };
        }

        fn rust_read_of_a_window() {
            let len = 1 << 20;
            // SAFETY: fresh no-access reservation.
            let base = unsafe {
                libc::mmap(
                    core::ptr::null_mut(),
                    len,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                )
            };
            assert_ne!(base, libc::MAP_FAILED);
            register_window(base as usize, len).expect("slot");
            // SAFETY: deliberately faults inside the window.
            unsafe { core::ptr::read_volatile((base as *const u8).add(0x40).cast::<u32>()) };
        }

        assert_eq!(run_child(null_deref), 42, "null deref chained");
        assert_eq!(
            run_child(rust_read_of_a_window),
            42,
            "non-site window fault chained"
        );
    }

    #[test]
    fn the_registry_finds_windows_and_forgets_them() {
        let base = 0x7000_0000_0000usize;
        let slot = register_window(base, 0x1000).expect("slot");
        assert_eq!(window_containing(base + 0xFFF), Some((slot, base)));
        assert_eq!(window_containing(base + 0x1000), None);
        unregister_window(slot);
        assert_eq!(window_containing(base), None);
    }
}
