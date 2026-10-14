//! Keyboard emulation

use super::mouse::{Ps2Mouse, SharedMouseState};
use crate::pic::{Pic, PicIrqSource};
use crate::system::GuestResetSource;
use gp64_types::FixedRing;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// BIOS shift_flags bit layout (BDA 0x0040:0x0017). Exposed `pub` so the
/// DOS driver in the emulator crate can compose these without duplicating
/// the constants — they're the IBM-defined BIOS bit positions and never
/// change.
pub const SHIFT_FLAG_SHIFT: u8 = 0x03;
pub const SHIFT_FLAG_CTRL: u8 = 0x04;
pub const SHIFT_FLAG_ALT: u8 = 0x08;
pub const SHIFT_FLAG_SCROLL_LOCK: u8 = 0x10;
pub const SHIFT_FLAG_NUM_LOCK: u8 = 0x20;
pub const SHIFT_FLAG_CAPS_LOCK: u8 = 0x40;

// ============================================================================
// Shared Keyboard State (thread-safe communication between callback and emulator)
// ============================================================================

/// Keys that have been injected via callback (prevents double-injection)
/// Uses a simple bitset for key indices (128 bits)
pub struct InjectedKeySet {
    bits: [AtomicU64; 2], // 128 bits, enough for all Key variants
}

impl InjectedKeySet {
    pub const fn new() -> Self {
        Self {
            bits: [AtomicU64::new(0), AtomicU64::new(0)],
        }
    }

    /// Mark a key as injected
    pub fn mark(&self, key_idx: usize) {
        let word = key_idx / 64;
        let bit = key_idx % 64;
        if word < 2 {
            self.bits[word].fetch_or(1 << bit, Ordering::Release);
        }
    }

    /// Check if a key was injected and clear the flag atomically
    pub fn check_and_clear(&self, key_idx: usize) -> bool {
        let word = key_idx / 64;
        let bit = key_idx % 64;
        if word < 2 {
            let mask = 1 << bit;
            let old = self.bits[word].fetch_and(!mask, Ordering::AcqRel);
            (old & mask) != 0
        } else {
            false
        }
    }

    /// Clear all injection flags
    pub fn clear_all(&self) {
        self.bits[0].store(0, Ordering::Release);
        self.bits[1].store(0, Ordering::Release);
    }
}

impl Default for InjectedKeySet {
    fn default() -> Self {
        Self::new()
    }
}

/// A byte sitting in the unified port-60h FIFO.
///
/// Distinguishes user-input bytes (GUI/host-key scancodes and explicit raw
/// control/debug scancodes) from command-response bytes (synthetic ACKs/replies
/// the controller produces
/// in response to an 8042/keyboard command). The two share storage so the
/// FIFO has a single source of truth for byte ordering, but differ at pop
/// time:
///
/// - `UserInput` bytes are gated on CCB.KBDINT (bit 0). When IRQ1 is
///   disabled the byte must stay in the FIFO — staging it into OBF
///   without an IRQ edge would leave OBF stuck full forever (see the
///   2026-05-07 OBF-stuck-when-IRQ-masked fix, commit 11af66b9).
///   They also go through Set 1→Set 2/3 translation at pop time.
/// - `CommandResponse` bytes drain regardless of KBDINT (the host polls
///   for them) and skip translation (already in their final form, e.g.
///   `0xFA` ACK or self-test 0xAA).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedByte {
    /// Genuine user-input scancode (GUI/HostKey event, explicit raw SCANCODES
    /// IPC, special-key injection). Subject to KBDINT gate and pop-time translation.
    UserInput(u8),
    /// Synthetic ACK or reply from a keyboard command (e.g. 0xED SETLEDS,
    /// 0xF2 IDENTIFY, 0xFF RESET). Bypasses the KBDINT gate and translation.
    CommandResponse(u8),
}

impl QueuedByte {
    /// Returns the raw byte value regardless of variant.
    #[inline]
    pub fn value(self) -> u8 {
        match self {
            QueuedByte::UserInput(b) | QueuedByte::CommandResponse(b) => b,
        }
    }
}

/// Result of attempting to inject host/user scancode bytes into the shared
/// keyboard FIFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScancodeInjectOutcome {
    /// Bytes were queued and IRQ1 was armed.
    Injected,
    /// Keyboard input was blocked by the 8042 interface gate or keyboard
    /// scanning gate.
    DroppedByGate,
    /// The shared FIFO did not have room for the complete scancode sequence.
    QueueFull,
}

/// Shared state between callback thread and main thread.
///
/// Owns the single port-60h FIFO — the unified replacement for what used
/// to be a `(local scancode_queue, shared scancode_queue)` pair with
/// alternation in `stage_output`. Both producers (GUI callback thread
/// via `inject_user_input`; main thread via `enqueue_command_response`
/// from `write_data` and `enqueue_user_input*` from input plumbing)
/// write here; `pop_keyboard_byte` is the single consumer.
pub struct SharedKeyboardState {
    /// Thread-safe FIFO of bytes destined for port 60h.
    /// Capacity: 64 bytes — sized for typematic bursts plus controller
    /// command-reply chains, with headroom for the user holding a key
    /// across a frame boundary.
    /// IMPORTANT: Only consumed by port 0x60 reads, never by BDA mirroring.
    ///
    /// Each entry pairs the byte with an optional host-arrival `Instant`,
    /// stamped only by `inject_scancode` (real host key events). The stamp
    /// never leaves this struct: `pop_byte` consumes it into the
    /// input-latency accumulator and hands callers a plain `QueuedByte`.
    scancode_queue: Mutex<VecDeque<(QueuedByte, Option<Instant>)>>,

    /// Atomic flag: set by callback when scancodes added, cleared by main loop
    /// after raising IRQ1. Re-set by port 0x60 read if more data pending.
    irq1_pending: AtomicBool,

    /// Modifier key state tracked in callback (for programs polling BDA directly)
    /// Bit 0-1: Shift, Bit 2: Ctrl, Bit 3: Alt
    modifier_flags: AtomicU8,

    /// Keys injected via callback this frame (prevents polling double-injection)
    injected_keys: InjectedKeySet,

    /// Timestamp of last key press per key (for typematic repeat coordination)
    /// Has its own mutex (separate from scancode_queue).
    key_press_times: Mutex<[Option<Instant>; 128]>,

    /// Rate-limiting counter for "queue full" warnings (prevents log spam)
    /// Resets every 10000 events to avoid stale counters in long sessions
    queue_full_warnings: AtomicU32,

    /// Input-latency accumulator: host key arrival (`inject_scancode`) to
    /// the byte becoming guest-visible (`pop_byte` staging it toward the
    /// 8042 output buffer). Drained ~once per second by the emulator
    /// thread for the HUD/IPC latency readouts.
    input_lat_sum_us: AtomicU64,
    input_lat_samples: AtomicU64,
    input_lat_max_us: AtomicU64,

    /// Keyboard interface enabled (synced from 8042 0xAD/0xAE commands)
    /// When false, scancodes are dropped at injection time (before queueing)
    keyboard_enabled: AtomicBool,
    /// Keyboard scanning enabled (synced from 0xF4/0xF5 commands)
    /// When false, scancodes are dropped at injection time (before queueing)
    scanning_enabled: AtomicBool,

    /// Typematic delay value (0-3, synced from keyboard controller 0xF3 command)
    /// Maps to: 0=250ms, 1=500ms, 2=750ms, 3=1000ms
    typematic_delay: AtomicU8,
    /// Typematic rate value (0-31, synced from keyboard controller 0xF3 command)
    /// Maps to repeat intervals from ~33ms (30 Hz) to ~500ms (2 Hz)
    typematic_rate: AtomicU8,

    /// Currently held key for typematic repeat (0 = none, else = Set 1 scancode).
    /// Set by accepted host-key input on key-down, cleared on key-up.
    repeat_scancode: AtomicU8,
    /// Whether the repeat key uses the E0 extended prefix.
    repeat_extended: AtomicBool,
    /// Monotonic notification for UI/replay state that must be invalidated
    /// after snapshot or checkpoint restore.
    guest_restore_epoch: AtomicU64,
}

impl SharedKeyboardState {
    pub fn new() -> Self {
        Self {
            scancode_queue: Mutex::new(VecDeque::with_capacity(64)),
            irq1_pending: AtomicBool::new(false),
            modifier_flags: AtomicU8::new(0),
            injected_keys: InjectedKeySet::new(),
            key_press_times: Mutex::new([None; 128]),
            queue_full_warnings: AtomicU32::new(0),
            input_lat_sum_us: AtomicU64::new(0),
            input_lat_samples: AtomicU64::new(0),
            input_lat_max_us: AtomicU64::new(0),
            keyboard_enabled: AtomicBool::new(true), // Enabled by default
            scanning_enabled: AtomicBool::new(true), // Scanning enabled by default
            // Default typematic: delay=1 (500ms), rate=0x0B (~10.9 Hz / 92ms)
            // Matches IBM AT keyboard power-on defaults per IBM Technical Reference.
            typematic_delay: AtomicU8::new(1),
            typematic_rate: AtomicU8::new(0x0B),
            repeat_scancode: AtomicU8::new(0),
            repeat_extended: AtomicBool::new(false),
            guest_restore_epoch: AtomicU64::new(0),
        }
    }

    /// Set keyboard interface enabled state (called from 8042 command handlers)
    /// When disabled, scancodes from GUI callback are silently dropped
    pub fn set_keyboard_enabled(&self, enabled: bool) {
        self.keyboard_enabled.store(enabled, Ordering::Release);
    }

    /// Check if keyboard interface is enabled
    pub fn is_keyboard_enabled(&self) -> bool {
        self.keyboard_enabled.load(Ordering::Acquire)
    }

    /// Set keyboard scanning enabled state (called from 0xF4/0xF5 handlers)
    /// When disabled, scancodes from GUI callback are silently dropped
    pub fn set_scanning_enabled(&self, enabled: bool) {
        self.scanning_enabled.store(enabled, Ordering::Release);
    }

    /// Check if keyboard scanning is enabled
    pub fn is_scanning_enabled(&self) -> bool {
        self.scanning_enabled.load(Ordering::Acquire)
    }

    /// Set typematic settings (called from keyboard controller 0xF3 handler)
    pub fn set_typematic(&self, delay: u8, rate: u8) {
        self.typematic_delay.store(delay & 0x03, Ordering::Release);
        self.typematic_rate.store(rate & 0x1F, Ordering::Release);
    }

    /// Get typematic delay value (0-3)
    pub fn get_typematic_delay(&self) -> u8 {
        self.typematic_delay.load(Ordering::Acquire)
    }

    /// Get typematic rate value (0-31)
    pub fn get_typematic_rate(&self) -> u8 {
        self.typematic_rate.load(Ordering::Acquire)
    }

    /// Get the currently held key for typematic repeat (0 = none).
    pub fn get_repeat_scancode(&self) -> u8 {
        self.repeat_scancode.load(Ordering::Acquire)
    }

    /// Get whether the repeat key uses the E0 extended prefix.
    pub fn is_repeat_extended(&self) -> bool {
        self.repeat_extended.load(Ordering::Acquire)
    }

    /// Clear typematic repeat state unconditionally. Used by the GUI when the
    /// host window loses focus: the OS stops delivering key-up events, so the
    /// shared atomic would otherwise stay armed and `tick_typematic_repeat`
    /// would flood the queue with phantom make codes until the user presses
    /// (and releases) the same key again.
    pub fn clear_repeat_state(&self) {
        self.repeat_scancode.store(0, Ordering::Release);
        self.repeat_extended.store(false, Ordering::Release);
    }

    fn repeat_matches(&self, key_idx: usize, extended: bool) -> bool {
        self.repeat_scancode.load(Ordering::Acquire) == key_idx as u8
            && self.repeat_extended.load(Ordering::Acquire) == extended
    }

    fn clear_repeat_if_current(&self, key_idx: usize, extended: bool) {
        if self.repeat_matches(key_idx, extended) {
            self.clear_repeat_state();
        }
    }

    /// Clear transient host-key side state while preserving the FIFO and
    /// controller configuration restored from a checkpoint/snapshot.
    pub fn clear_host_input_state(&self) {
        self.modifier_flags.store(0, Ordering::Release);
        self.injected_keys.clear_all();
        if let Ok(mut times) = self.key_press_times.lock() {
            *times = [None; 128];
        }
        self.queue_full_warnings.store(0, Ordering::Relaxed);
        self.clear_repeat_state();
    }

    /// Notify host-side producers that the guest keyboard state was restored
    /// from an older snapshot/checkpoint and their held-key mirrors are stale.
    pub fn notify_guest_state_restored(&self) {
        self.guest_restore_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// Current guest-restore epoch observed by UI/replay producers.
    pub fn guest_restore_epoch(&self) -> u64 {
        self.guest_restore_epoch.load(Ordering::Acquire)
    }

    /// Drain pending user-input scancodes from the FIFO. Called on focus
    /// loss to prevent backed-up typematic make codes (queued before focus
    /// changed) from flooding the guest after the user returns. Without
    /// this, a held key during Alt+Tab leaves the queue full of stale make
    /// codes which drain over the next ~second and look like a "stuck"
    /// keyboard. Command-response bytes are preserved — they are mid-flight
    /// host↔controller dialogue, not user input.
    pub fn drain_queue(&self) -> usize {
        let n = match self.scancode_queue.lock() {
            Ok(mut q) => {
                let before = q.len();
                q.retain(|e| matches!(e.0, QueuedByte::CommandResponse(_)));
                before - q.len()
            }
            Err(poisoned) => {
                let mut q = poisoned.into_inner();
                let before = q.len();
                q.retain(|e| matches!(e.0, QueuedByte::CommandResponse(_)));
                before - q.len()
            }
        };
        // Clear irq1_pending only if no command responses remain — those
        // still need delivery so the trigger must stay armed.
        let any_left = match self.scancode_queue.lock() {
            Ok(q) => !q.is_empty(),
            Err(poisoned) => !poisoned.into_inner().is_empty(),
        };
        if !any_left {
            self.irq1_pending.store(false, Ordering::Release);
        }
        n
    }

    /// Update only the typematic repeat state (without injecting scancodes).
    /// Used by the IPC path when scancodes go to the local queue but typematic
    /// still needs to track which key is held.
    pub fn update_repeat_state(&self, key_idx: usize, extended: bool, is_down: bool) {
        if is_down {
            self.repeat_scancode.store(key_idx as u8, Ordering::Release);
            self.repeat_extended.store(extended, Ordering::Release);
        } else {
            self.clear_repeat_if_current(key_idx, extended);
        }
    }

    /// Get typematic delay in milliseconds (for GUI repeat timing)
    pub fn get_typematic_delay_ms(&self) -> u32 {
        Keyboard::typematic_delay_ms(self.typematic_delay.load(Ordering::Acquire))
    }

    /// Get typematic rate in milliseconds per repeat (for GUI repeat timing)
    pub fn get_typematic_rate_ms(&self) -> u32 {
        Keyboard::typematic_rate_ms(self.typematic_rate.load(Ordering::Acquire))
    }

    fn clear_release_state(&self, key_idx: usize, extended: bool) {
        if let Ok(mut times) = self.key_press_times.lock() {
            if key_idx < 128 {
                times[key_idx] = None;
            }
        }
        self.clear_repeat_if_current(key_idx, extended);
    }

    /// Inject host-key scancodes into the shared keyboard FIFO.
    pub fn inject_scancode(
        &self,
        bytes: &[u8],
        key_idx: usize,
        is_down: bool,
    ) -> ScancodeInjectOutcome {
        let extended = bytes.first().is_some_and(|&byte| byte == 0xE0);

        // Check if keyboard interface is enabled (via 8042 0xAD/0xAE commands)
        // When disabled, silently drop scancodes to prevent queue buildup.
        // A gated key-up must still clear the held-key repeat state; otherwise
        // a release during a transient inhibit can produce phantom repeats
        // after the guest re-enables the keyboard interface.
        if !self.keyboard_enabled.load(Ordering::Acquire)
            || !self.scanning_enabled.load(Ordering::Acquire)
        {
            if !is_down {
                self.clear_release_state(key_idx, extended);
            }
            return ScancodeInjectOutcome::DroppedByGate;
        }

        let mut queue = match self.scancode_queue.lock() {
            Ok(q) => q,
            Err(poisoned) => {
                // Mutex poisoned (callback panicked previously) - recover
                tracing::warn!("SharedKeyboardState mutex poisoned, recovering");
                poisoned.into_inner()
            }
        };

        // Inject all bytes atomically (important for E0-prefixed extended keys)
        if queue.len().saturating_add(bytes.len()) > 64 {
            if !is_down {
                self.clear_release_state(key_idx, extended);
            }
            // Rate-limit warnings: log first, then every 100th
            // Reset counter every 10000 to avoid stale counts in long sessions
            let count = self.queue_full_warnings.fetch_add(1, Ordering::Relaxed);
            if count >= 10000 {
                self.queue_full_warnings.store(0, Ordering::Relaxed);
            }
            if count == 0 || count.is_multiple_of(100) {
                tracing::warn!(
                    dropped_bytes = bytes.len(),
                    recent_drops = (count % 10000) + 1,
                    "Keyboard scancode queue full"
                );
            }
            return ScancodeInjectOutcome::QueueFull;
        }

        // One arrival stamp for the whole key event: every byte of an
        // E0-prefixed sequence left the host at the same moment.
        let arrived = Instant::now();
        for &b in bytes {
            queue.push_back((QueuedByte::UserInput(b), Some(arrived)));
        }

        // Trace scancode injection for debugging keyboard sensitivity issues
        if bytes.len() == 2 {
            tracing::debug!(
                target: "gp64::kbd::inject",
                scancode = format_args!("{:02X} {:02X}", bytes[0], bytes[1]),
                key_idx,
                is_down,
                queue_len = queue.len(),
                "SCANCODE_INJECT"
            );
        } else if bytes.len() == 1 {
            tracing::debug!(
                target: "gp64::kbd::inject",
                scancode = format_args!("{:02X}", bytes[0]),
                key_idx,
                is_down,
                queue_len = queue.len(),
                "SCANCODE_INJECT"
            );
        }

        // Update key press time for typematic coordination
        if let Ok(mut times) = self.key_press_times.lock() {
            if key_idx < 128 {
                times[key_idx] = if is_down { Some(Instant::now()) } else { None };
            }
        }

        // Track held key for typematic repeat (emulator thread reads this).
        // Only ONE key repeats at a time (the most recently pressed).
        if is_down {
            let scancode = key_idx as u8;
            self.repeat_scancode.store(scancode, Ordering::Release);
            self.repeat_extended.store(extended, Ordering::Release);
        } else {
            self.clear_repeat_if_current(key_idx, extended);
        }

        drop(queue); // Release lock before atomic ops

        if crate::kbd_flow_trace_enabled() {
            let first = bytes.first().copied().unwrap_or(0);
            let last = bytes.last().copied().unwrap_or(0);
            let pending = self.pending_count();
            tracing::info!(
                target: "gp64::kbd::flow",
                source = "shared",
                count = bytes.len(),
                first = format_args!("{:02X}", first),
                last = format_args!("{:02X}", last),
                key_idx,
                is_down,
                pending,
                "KBD_INJECT"
            );
        }

        // Mark as injected (prevents double-injection from polling fallback)
        // Mark for both press AND release - the same bit can be reused since
        // press and release are separate events and the bit is cleared after each check
        self.injected_keys.mark(key_idx);

        // Signal that IRQ1 should be raised
        self.irq1_pending.store(true, Ordering::Release);
        ScancodeInjectOutcome::Injected
    }

    /// Called from main thread emulator loop
    pub fn take_irq1_pending(&self) -> bool {
        self.irq1_pending.swap(false, Ordering::AcqRel)
    }

    /// Check if IRQ1 is pending without consuming (for snapshots).
    pub fn irq1_pending(&self) -> bool {
        self.irq1_pending.load(Ordering::Acquire)
    }

    /// Set IRQ1 pending flag (used when re-raising after failed delivery)
    pub fn set_irq1_pending(&self) {
        self.irq1_pending.store(true, Ordering::Release);
    }

    /// Pop the next byte for staging.
    ///
    /// `irq1_enabled` reflects CCB.KBDINT (bit 0). When `false`, only
    /// `CommandResponse` bytes are popped — user-input bytes wait until
    /// IRQ1 is re-enabled, otherwise they'd stage to OBF with no IRQ edge
    /// and lock OBF stuck full forever (the 2026-05-07 fix, commit
    /// 11af66b9). Command-response bytes drain regardless: the host is
    /// polling for them and never expects IRQ1 (e.g. `0xFA` ACK from
    /// `0xED` SETLEDS during keyboard reset).
    ///
    /// **Priority**: `CommandResponse` bytes always pop before `UserInput`
    /// bytes regardless of arrival order. This preserves the pre-collapse
    /// invariant (local queue > shared queue in `pop_keyboard_byte`) which
    /// some tests assert — command responses are out-of-band 8042 replies
    /// that the host is timing-sensitively polling for, and must not be
    /// starved by typematic user input. Within each variant, FIFO order
    /// is preserved.
    ///
    /// This is the ONLY consumer of the FIFO.
    pub fn pop_byte(&self, irq1_enabled: bool) -> Option<QueuedByte> {
        let mut queue = match self.scancode_queue.lock() {
            Ok(q) => q,
            Err(poisoned) => {
                tracing::warn!("SharedKeyboardState mutex poisoned, recovering");
                poisoned.into_inner()
            }
        };

        // 1. Prefer the oldest CommandResponse byte (insertion order
        //    within the variant). The KBDINT gate does not affect this —
        //    command responses always drain.
        let cmd_pos = queue
            .iter()
            .position(|e| matches!(e.0, QueuedByte::CommandResponse(_)));
        let entry = if let Some(i) = cmd_pos {
            queue.remove(i)
        } else if irq1_enabled {
            // 2. No command response queued: drain the oldest UserInput
            //    byte if KBDINT permits.
            queue.pop_front()
        } else {
            None
        };

        // If more deliverable data pending after this pop, re-raise IRQ1
        // so the trigger fires again. "Deliverable" = at least one byte
        // we are allowed to pop given the current gate state.
        let more = if irq1_enabled {
            !queue.is_empty()
        } else {
            queue
                .iter()
                .any(|e| matches!(e.0, QueuedByte::CommandResponse(_)))
        };
        if more {
            self.irq1_pending.store(true, Ordering::Release);
        }
        drop(queue);

        let (byte, stamp) = entry?;
        if let Some(arrived) = stamp {
            self.record_input_latency_us(arrived.elapsed().as_micros().min(u64::MAX as u128) as u64);
        }
        Some(byte)
    }

    /// Record one host-arrival→guest-visible input-latency sample.
    fn record_input_latency_us(&self, us: u64) {
        self.input_lat_sum_us.fetch_add(us, Ordering::Relaxed);
        self.input_lat_samples.fetch_add(1, Ordering::Relaxed);
        self.input_lat_max_us.fetch_max(us, Ordering::Relaxed);
    }

    /// Drain the input-latency accumulator: `(sum_us, samples, max_us)`.
    /// Called ~once per second by the emulator thread's HUD updater.
    pub fn take_input_latency_stats(&self) -> (u64, u64, u64) {
        (
            self.input_lat_sum_us.swap(0, Ordering::Relaxed),
            self.input_lat_samples.swap(0, Ordering::Relaxed),
            self.input_lat_max_us.swap(0, Ordering::Relaxed),
        )
    }

    /// Returns true if any byte is queued (user-input or command-response).
    /// Used by the 8042 status path and the keyboard transfer scheduler.
    pub fn has_pending(&self) -> bool {
        match self.scancode_queue.lock() {
            Ok(q) => !q.is_empty(),
            Err(poisoned) => !poisoned.into_inner().is_empty(),
        }
    }

    /// Returns true if a `CommandResponse` byte is queued. Used by
    /// `pop_keyboard_byte` to short-circuit when KBDINT is disabled but
    /// a synthetic ACK is still deliverable.
    pub fn has_pending_command_response(&self) -> bool {
        match self.scancode_queue.lock() {
            Ok(q) => q
                .iter()
                .any(|e| matches!(e.0, QueuedByte::CommandResponse(_))),
            Err(poisoned) => poisoned
                .into_inner()
                .iter()
                .any(|e| matches!(e.0, QueuedByte::CommandResponse(_))),
        }
    }

    /// Total byte count in the FIFO (used for diagnostics and the
    /// queue-full warning gate).
    pub fn pending_count(&self) -> usize {
        match self.scancode_queue.lock() {
            Ok(q) => q.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Count of `UserInput` bytes only (diagnostics).
    pub fn pending_user_input_count(&self) -> usize {
        match self.scancode_queue.lock() {
            Ok(q) => q
                .iter()
                .filter(|e| matches!(e.0, QueuedByte::UserInput(_)))
                .count(),
            Err(poisoned) => poisoned
                .into_inner()
                .iter()
                .filter(|e| matches!(e.0, QueuedByte::UserInput(_)))
                .count(),
        }
    }

    pub fn discard_user_input(&self, count: usize) -> usize {
        if count == 0 {
            return 0;
        }
        let mut queue = match self.scancode_queue.lock() {
            Ok(q) => q,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut remaining = count;
        let mut removed = 0;
        let mut kept = VecDeque::with_capacity(queue.len());
        while let Some(entry) = queue.pop_front() {
            if remaining != 0 && matches!(entry.0, QueuedByte::UserInput(_)) {
                remaining -= 1;
                removed += 1;
            } else {
                kept.push_back(entry);
            }
        }
        *queue = kept;
        if !queue
            .iter()
            .any(|entry| matches!(entry.0, QueuedByte::UserInput(_)))
        {
            self.irq1_pending.store(false, Ordering::Release);
        }
        removed
    }

    /// Count of `CommandResponse` bytes only (diagnostics).
    pub fn pending_command_response_count(&self) -> usize {
        match self.scancode_queue.lock() {
            Ok(q) => q
                .iter()
                .filter(|e| matches!(e.0, QueuedByte::CommandResponse(_)))
                .count(),
            Err(poisoned) => poisoned
                .into_inner()
                .iter()
                .filter(|e| matches!(e.0, QueuedByte::CommandResponse(_)))
                .count(),
        }
    }

    /// Pop the oldest queued `CommandResponse` byte without touching
    /// ordinary user-input bytes.
    ///
    /// This supports port-60h polling loops for multi-byte keyboard command
    /// replies. It must not be used for IRQ1 scancode delivery because it
    /// deliberately bypasses user-input staging and translation.
    pub fn pop_command_response(&self) -> Option<u8> {
        let mut queue = match self.scancode_queue.lock() {
            Ok(q) => q,
            Err(poisoned) => poisoned.into_inner(),
        };
        let pos = queue
            .iter()
            .position(|e| matches!(e.0, QueuedByte::CommandResponse(_)))?;
        match queue.remove(pos)? {
            (QueuedByte::CommandResponse(byte), _) => Some(byte),
            (QueuedByte::UserInput(_), _) => None,
        }
    }

    /// Enqueue a synthetic command-response byte (8042/keyboard command
    /// ACKs and replies). Bypasses the KBDINT gate at pop time — the host
    /// polls port 0x60 for the reply and does not expect IRQ1. Returns
    /// `false` if the FIFO is full; caller drops on overflow.
    ///
    /// Called only from the main emulator thread (write_data handlers).
    pub fn enqueue_command_response(&self, bytes: &[u8]) -> bool {
        let mut queue = match self.scancode_queue.lock() {
            Ok(q) => q,
            Err(poisoned) => poisoned.into_inner(),
        };
        if queue.len().saturating_add(bytes.len()) > 64 {
            return false;
        }
        for &b in bytes {
            queue.push_back((QueuedByte::CommandResponse(b), None));
        }
        // Command responses do not require IRQ1; the host is polling.
        // No irq1_pending flip here.
        true
    }

    /// Enqueue raw user-input scancode bytes (main thread side: special
    /// key sequences, explicit raw IPC scancodes, test force-push). Bypasses
    /// the `scanning_enabled` gate so explicit raw control/debug scancodes
    /// remain available even when the guest disabled scanning via kbd command
    /// 0xF5. Returns `false` on overflow.
    pub fn enqueue_user_input_force(&self, bytes: &[u8]) -> bool {
        let mut queue = match self.scancode_queue.lock() {
            Ok(q) => q,
            Err(poisoned) => poisoned.into_inner(),
        };
        if queue.len().saturating_add(bytes.len()) > 64 {
            return false;
        }
        // Unstamped: these are synthetic main-thread injections (special-key
        // sequences, raw IPC scancodes, typematic), not host key events —
        // stamping them would dilute the input-latency samples.
        for &b in bytes {
            queue.push_back((QueuedByte::UserInput(b), None));
        }
        // User input requires IRQ1 to be deliverable; arm the flag so the
        // main-loop trigger picks it up at the next quantum boundary.
        self.irq1_pending.store(true, Ordering::Release);
        true
    }

    /// Snapshot the FIFO contents (for save/restore). Returns the bytes
    /// with their variant tags so command-response vs. user-input
    /// classification round-trips through a snapshot.
    pub fn queue_snapshot(&self) -> Vec<QueuedByte> {
        // Arrival stamps are host-transient and deliberately dropped: a
        // restored queue has no meaningful host arrival time.
        match self.scancode_queue.lock() {
            Ok(q) => q.iter().map(|e| e.0).collect(),
            Err(poisoned) => poisoned.into_inner().iter().map(|e| e.0).collect(),
        }
    }

    /// Restore the FIFO contents from a snapshot. Existing contents are
    /// discarded.
    pub fn restore_queue(&self, bytes: &[QueuedByte]) {
        let mut queue = match self.scancode_queue.lock() {
            Ok(q) => q,
            Err(poisoned) => poisoned.into_inner(),
        };
        queue.clear();
        for &b in bytes {
            queue.push_back((b, None));
        }
        self.irq1_pending
            .store(!queue.is_empty(), Ordering::Release);
    }

    /// Update modifier flags (called from callback on shift/ctrl/alt press/release)
    pub fn set_modifier(&self, shift: bool, ctrl: bool, alt: bool) {
        let mut flags = 0u8;
        if shift {
            flags |= 0x03;
        } // Both shift bits
        if ctrl {
            flags |= 0x04;
        }
        if alt {
            flags |= 0x08;
        }
        self.modifier_flags.store(flags, Ordering::Release);
    }

    pub fn get_modifier_flags(&self) -> u8 {
        self.modifier_flags.load(Ordering::Acquire)
    }

    /// Check if a key was already injected via callback (prevents double-injection)
    pub fn was_injected(&self, key_idx: usize) -> bool {
        self.injected_keys.check_and_clear(key_idx)
    }

    /// Get key press time for typematic repeat (main thread)
    pub fn get_key_press_time(&self, key_idx: usize) -> Option<Instant> {
        self.key_press_times
            .lock()
            .ok()?
            .get(key_idx)
            .copied()
            .flatten()
    }

    /// Clear all state (for tests or reset)
    pub fn clear(&self) {
        if let Ok(mut q) = self.scancode_queue.lock() {
            q.clear();
        }
        self.irq1_pending.store(false, Ordering::Release);
        self.clear_host_input_state();
    }
}

impl Default for SharedKeyboardState {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Keyboard Controller
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingCommand {
    Controller(u8),
    Keyboard(u8),
}

/// Source of a byte in the 8042 controller output buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSource {
    /// Scancode byte from the keyboard port.
    Keyboard,
    /// Reply byte from the PS/2 keyboard device, such as ACK or IDENTIFY.
    KeyboardReply,
    /// Data byte from the PS/2 auxiliary (mouse) port.
    AuxMouse,
    /// Polled command response from the PS/2 auxiliary port.
    AuxMouseResponse,
    /// Response to an 8042 controller command (e.g. 0x20 read CCB, 0xAA self-test).
    ControllerReply,
}

impl OutputSource {
    fn is_mouse(self) -> bool {
        matches!(
            self,
            OutputSource::AuxMouse | OutputSource::AuxMouseResponse
        )
    }
}

/// A byte staged in the 8042 controller output buffer, tagged with its source.
#[derive(Debug, Clone, Copy)]
pub struct OutputByte {
    pub value: u8,
    pub source: OutputSource,
    pub aux_response_generation: Option<u32>,
}

const STALE_AUX_RESPONSE_KEYBOARD_BLOCK_GRACE_ATTEMPTS: u16 = 1000;

pub struct Keyboard {
    /// Port 0x60 data register (last scancode)
    data_register: u8,
    /// Whether keyboard scanning is enabled (0xF4/0xF5).
    scanning_enabled: bool,
    /// PPI Port B latch (0x61). Used by many DOS keyboard handlers to ACK/strobe.
    port61_latch: u8,
    /// Synthetic "refresh toggle" bit (0x61 bit4), toggled on reads for delay loops.
    port61_refresh: bool,
    /// 8042 output port state (includes active-low reset and A20 gate control).
    output_port: u8,
    /// Pending A20 gate change from 0xD1 writes.
    pending_a20: Option<bool>,
    /// Pending guest system reset from an output-port reset pulse.
    pending_guest_reset: Option<GuestResetSource>,
    /// Unified port-60h FIFO (user-input + command-response bytes,
    /// tagged via `QueuedByte`). Replaced the dual `(local scancode_queue,
    /// shared scancode_queue)` pair in 2026-05-12 (single-port-60h-transport
    /// HLD, Stage 3). Always present — `Keyboard::new()` creates a fresh
    /// `SharedKeyboardState`. `set_shared_state` swaps in a thread-shared
    /// instance (typically owned jointly with the GUI callback thread)
    /// when the window is attached; the freshly-constructed queue is
    /// discarded with no state to migrate.
    shared_state: Arc<SharedKeyboardState>,
    /// 8042 controller RAM (32 bytes, read via commands 0x20-0x3F)
    controller_ram: [u8; 32],
    /// Pending command (controller or keyboard) waiting for a data byte.
    pending_command: Option<PendingCommand>,
    /// Keyboard scancode set (1-3). We emit Set 1 regardless, but track requests.
    scancode_set: u8,
    /// PS/2 mouse device (auxiliary port)
    pub mouse: Ps2Mouse,
    /// Next write to port 0x60 goes to mouse (after 0xD4 command)
    mouse_write_pending: bool,
    /// One-shot budget for polled command-response bytes produced by the
    /// current D4 mouse command while the AUX interface is disabled.
    disabled_aux_polled_response_budget: usize,
    /// Sustained user-input block counter for a visible AUX command response.
    stale_aux_response_keyboard_block_attempts: u16,
    /// 8042 controller output buffer — single source of truth for port 0x60
    /// readable data, OBF status, IRQ source tracking, and consumption side effects.
    output_buffer: Option<OutputByte>,
    /// Tracks whether the last consumed output byte was from the mouse.
    /// Used for keyboard/mouse alternation in `stage_output`.
    data_register_source_was_mouse: bool,
    /// Source of the last fresh port-60h read.
    ///
    /// Used by the real-mode IRQ1 HLE trap: the x86 stub has already read
    /// port 60h into AL before jumping to the trap, so the HLE layer needs
    /// the source tag to avoid dispatching keyboard-device ACKs as scancodes.
    last_port60_read_source: Option<OutputSource>,
    /// Shared mouse state for callback thread communication (None when running headless)
    shared_mouse_state: Option<Arc<SharedMouseState>>,
    /// 8042 input port state (keyboard/mouse data lines, system config)
    input_port: u8,
    /// Input port poll value for status bits 4-7 (set by 0xC1/0xC2 commands)
    input_port_poll: Option<u8>,
    /// LED state (bit 0=ScrollLock, bit 1=NumLock, bit 2=CapsLock)
    led_state: u8,
    /// Typematic delay value (0-3: 0=250ms, 1=500ms, 2=750ms, 3=1000ms)
    typematic_delay: u8,
    /// Typematic rate value (0-31: 0=30Hz fastest, 31=2Hz slowest)
    typematic_rate: u8,
    /// Whether typematic repeat is enabled (can be disabled by INT 16h AH=03h AL=04h)
    typematic_enabled: bool,
    /// Typematic repeat state: scancode currently repeating (0 = none)
    repeat_scancode: u8,
    /// Whether the repeating key uses E0 extended prefix
    repeat_extended: bool,
    /// Countdown in ms until next repeat fires (0 = ready to fire)
    repeat_wait: u32,
    /// Track port 0x60 reads by the emulated CPU (for polling detection).
    port60_read_since_check: bool,
    /// Controller reply byte waiting to be staged into output_buffer.
    /// Takes priority over scancode_queue in try_stage_keyboard(), ensuring
    /// replies are never misattributed when scancodes are already queued.
    pending_controller_reply: Option<u8>,
    /// Input Buffer Full flag (status bit 1). Set when the CPU writes to port
    /// 0x60 or 0x64, cleared when the controller processes the byte. Since our
    /// emulation processes commands instantly, we set this on write and clear it
    /// on the next status read (port 0x64 read).
    input_buffer_full: bool,
    /// Translation output buffer for Set 2/3 multi-byte sequences.
    /// When non-empty, read_keyboard_output drains this before reading new input.
    translation_buffer: FixedRing<u8, 8>,
    /// True when the last byte read from the queue was 0xE0 (extended key prefix).
    /// Used by scancode translation to know the next byte is an extended key.
    translate_e0_pending: bool,
    /// True when the emulator uses event-driven keyboard transfer (DOS path only).
    /// When set, the port 60h read handler does NOT re-raise IRQ1 via PIC —
    /// that is handled by the event system scheduling KeyboardTransfer.
    pub keyboard_event_driven: bool,
    /// Flag-based coupling: set when port 60h is read AND more scancodes are
    /// available AND `keyboard_event_driven` is true. The emulator checks this
    /// flag after I/O and schedules a KeyboardTransfer event.
    pub keyboard_transfer_pending: bool,
    /// Flag-based coupling: set when a 0xFF (Reset) keyboard command is received
    /// in event-driven mode. The emulator picks this up, cancels pending transfers,
    /// and schedules a KeyboardReset event at +400ms.
    pub keyboard_reset_command_pending: bool,
}

/// Scancode Set 1 make code -> Set 2 make code translation table.
/// Index = Set 1 make code (0x00-0x7F), Value = Set 2 make code.
/// Source: IBM PS/2 Technical Reference scancode tables.
#[rustfmt::skip]
const SET1_TO_SET2: [u8; 128] = [
    // 0x00       0x01       0x02       0x03       0x04       0x05       0x06       0x07
    0x00,      0x76,      0x16,      0x1E,      0x26,      0x25,      0x2E,      0x36,      // 0x00: (none), Esc, 1, 2, 3, 4, 5, 6
    // 0x08       0x09       0x0A       0x0B       0x0C       0x0D       0x0E       0x0F
    0x3D,      0x3E,      0x46,      0x45,      0x4E,      0x55,      0x66,      0x0D,      // 0x08: 7, 8, 9, 0, -, =, Backspace, Tab
    // 0x10       0x11       0x12       0x13       0x14       0x15       0x16       0x17
    0x15,      0x1D,      0x24,      0x2D,      0x2C,      0x35,      0x3C,      0x43,      // 0x10: Q, W, E, R, T, Y, U, I
    // 0x18       0x19       0x1A       0x1B       0x1C       0x1D       0x1E       0x1F
    0x44,      0x4D,      0x54,      0x5B,      0x5A,      0x14,      0x1C,      0x1B,      // 0x18: O, P, [, ], Enter, LCtrl, A, S
    // 0x20       0x21       0x22       0x23       0x24       0x25       0x26       0x27
    0x23,      0x2B,      0x34,      0x33,      0x3B,      0x42,      0x4B,      0x4C,      // 0x20: D, F, G, H, J, K, L, ;
    // 0x28       0x29       0x2A       0x2B       0x2C       0x2D       0x2E       0x2F
    0x52,      0x0E,      0x12,      0x5D,      0x1A,      0x22,      0x21,      0x2A,      // 0x28: ', `, LShift, \, Z, X, C, V
    // 0x30       0x31       0x32       0x33       0x34       0x35       0x36       0x37
    0x32,      0x31,      0x3A,      0x41,      0x49,      0x4A,      0x59,      0x7C,      // 0x30: B, N, M, ',', ., /, RShift, KP-*
    // 0x38       0x39       0x3A       0x3B       0x3C       0x3D       0x3E       0x3F
    0x11,      0x29,      0x58,      0x05,      0x06,      0x04,      0x0C,      0x03,      // 0x38: LAlt, Space, CapsLock, F1, F2, F3, F4, F5
    // 0x40       0x41       0x42       0x43       0x44       0x45       0x46       0x47
    0x0B,      0x83,      0x0A,      0x01,      0x09,      0x77,      0x7E,      0x6C,      // 0x40: F6, F7, F8, F9, F10, NumLock, ScrollLock, KP-7/Home
    // 0x48       0x49       0x4A       0x4B       0x4C       0x4D       0x4E       0x4F
    0x75,      0x7D,      0x7B,      0x6B,      0x73,      0x74,      0x79,      0x69,      // 0x48: KP-8/Up, KP-9/PgUp, KP--, KP-4/Left, KP-5, KP-6/Right, KP-+, KP-1/End
    // 0x50       0x51       0x52       0x53       0x54       0x55       0x56       0x57
    0x72,      0x7A,      0x70,      0x71,      0x84,      0x00,      0x61,      0x78,      // 0x50: KP-2/Down, KP-3/PgDn, KP-0/Ins, KP-./Del, SysReq, (none), Europe2, F11
    // 0x58       0x59       0x5A       0x5B       0x5C       0x5D       0x5E       0x5F
    0x07,      0x00,      0x00,      0x00,      0x00,      0x00,      0x00,      0x00,      // 0x58: F12
    // 0x60-0x6F: unused
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 0x70-0x7F: unused
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Scancode Set 1 make code -> Set 3 make code translation table.
/// Index = Set 1 make code (0x00-0x7F), Value = Set 3 make code.
/// Source: IBM PS/2 Technical Reference scancode tables.
#[rustfmt::skip]
const SET1_TO_SET3: [u8; 128] = [
    // 0x00       0x01       0x02       0x03       0x04       0x05       0x06       0x07
    0x00,      0x08,      0x16,      0x1E,      0x26,      0x25,      0x2E,      0x36,      // 0x00: (none), Esc, 1, 2, 3, 4, 5, 6
    // 0x08       0x09       0x0A       0x0B       0x0C       0x0D       0x0E       0x0F
    0x3D,      0x3E,      0x46,      0x45,      0x4E,      0x55,      0x66,      0x0D,      // 0x08: 7, 8, 9, 0, -, =, Backspace, Tab
    // 0x10       0x11       0x12       0x13       0x14       0x15       0x16       0x17
    0x15,      0x1D,      0x24,      0x2D,      0x2C,      0x35,      0x3C,      0x43,      // 0x10: Q, W, E, R, T, Y, U, I
    // 0x18       0x19       0x1A       0x1B       0x1C       0x1D       0x1E       0x1F
    0x44,      0x4D,      0x54,      0x5B,      0x5A,      0x11,      0x1C,      0x1B,      // 0x18: O, P, [, ], Enter, LCtrl, A, S
    // 0x20       0x21       0x22       0x23       0x24       0x25       0x26       0x27
    0x23,      0x2B,      0x34,      0x33,      0x3B,      0x42,      0x4B,      0x4C,      // 0x20: D, F, G, H, J, K, L, ;
    // 0x28       0x29       0x2A       0x2B       0x2C       0x2D       0x2E       0x2F
    0x52,      0x0E,      0x12,      0x5C,      0x1A,      0x22,      0x21,      0x2A,      // 0x28: ', `, LShift, \, Z, X, C, V
    // 0x30       0x31       0x32       0x33       0x34       0x35       0x36       0x37
    0x32,      0x31,      0x3A,      0x41,      0x49,      0x4A,      0x59,      0x7E,      // 0x30: B, N, M, ',', ., /, RShift, KP-*
    // 0x38       0x39       0x3A       0x3B       0x3C       0x3D       0x3E       0x3F
    0x19,      0x29,      0x14,      0x07,      0x0F,      0x17,      0x1F,      0x27,      // 0x38: LAlt, Space, CapsLock, F1, F2, F3, F4, F5
    // 0x40       0x41       0x42       0x43       0x44       0x45       0x46       0x47
    0x2F,      0x37,      0x3F,      0x47,      0x4F,      0x76,      0x5F,      0x6C,      // 0x40: F6, F7, F8, F9, F10, NumLock, ScrollLock, KP-7/Home
    // 0x48       0x49       0x4A       0x4B       0x4C       0x4D       0x4E       0x4F
    0x75,      0x7D,      0x84,      0x6B,      0x73,      0x74,      0x7C,      0x69,      // 0x48: KP-8/Up, KP-9/PgUp, KP--, KP-4/Left, KP-5, KP-6/Right, KP-+, KP-1/End
    // 0x50       0x51       0x52       0x53       0x54       0x55       0x56       0x57
    0x72,      0x7A,      0x70,      0x71,      0x57,      0x00,      0x13,      0x56,      // 0x50: KP-2/Down, KP-3/PgDn, KP-0/Ins, KP-./Del, SysReq, (none), Europe2, F11
    // 0x58       0x59       0x5A       0x5B       0x5C       0x5D       0x5E       0x5F
    0x5E,      0x00,      0x00,      0x00,      0x00,      0x00,      0x00,      0x00,      // 0x58: F12
    // 0x60-0x6F: unused
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 0x70-0x7F: unused
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Extended key (E0-prefixed) Set 1 make code -> Set 2 make code translation table.
/// Index = Set 1 extended make code (low 7 bits), Value = Set 2 make code.
/// The E0 prefix is still used in Set 2 output; this table provides the translated byte.
/// Only covers common extended keys; unmapped entries are 0x00.
#[rustfmt::skip]
const SET1_EXT_TO_SET2: [u8; 128] = [
    // 0x00-0x0F
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 0x10-0x1F                                              0x1C=KP Enter        0x1D=RCtrl
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5A, 0x14, 0x00, 0x00,
    // 0x20-0x2F
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 0x30-0x3F                                  0x35=KP /                  0x38=RAlt
    0x00, 0x00, 0x00, 0x00, 0x00, 0x4A, 0x00, 0x00, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 0x40-0x4F  0x47=Home  0x48=Up    0x49=PgUp            0x4B=Left        0x4D=Right        0x4F=End
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6C, 0x75, 0x7D, 0x00, 0x6B, 0x00, 0x74, 0x00, 0x69,
    // 0x50-0x5F  0x50=Down  0x51=PgDn  0x52=Ins   0x53=Del                            0x5B=LWin  0x5C=RWin  0x5D=Apps
    0x72, 0x7A, 0x70, 0x71, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F, 0x27, 0x2F, 0x00, 0x00,
    // 0x60-0x6F
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 0x70-0x7F
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Extended key (E0-prefixed) Set 1 make code -> Set 3 make code translation table.
/// For Set 3, there are no extended keys (each key has a unique code).
/// Index = Set 1 extended make code (low 7 bits), Value = Set 3 make code.
#[rustfmt::skip]
const SET1_EXT_TO_SET3: [u8; 128] = [
    // 0x00-0x0F
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 0x10-0x1F                                              0x1C=KP Enter        0x1D=RCtrl
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x79, 0x58, 0x00, 0x00,
    // 0x20-0x2F
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 0x30-0x3F                                  0x35=KP /         0x37=PrtSc  0x38=RAlt
    0x00, 0x00, 0x00, 0x00, 0x00, 0x77, 0x00, 0x57, 0x39, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 0x40-0x4F  0x47=Home  0x48=Up    0x49=PgUp            0x4B=Left        0x4D=Right        0x4F=End
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6E, 0x63, 0x6F, 0x00, 0x61, 0x00, 0x6A, 0x00, 0x65,
    // 0x50-0x5F  0x50=Down  0x51=PgDn  0x52=Ins   0x53=Del                            0x5B=LWin  0x5C=RWin  0x5D=Apps
    0x60, 0x6D, 0x67, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x8B, 0x8C, 0x8D, 0x00, 0x00,
    // 0x60-0x6F
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 0x70-0x7F
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Translate an E0-prefixed Set 1 make code to its Set 2 equivalent.
/// Returns 0x00 for unknown/unmapped codes.
fn set1_to_set2_extended(set1_code: u8) -> u8 {
    SET1_EXT_TO_SET2[set1_code as usize & 0x7F]
}

/// Translate an E0-prefixed Set 1 make code to its Set 3 equivalent.
/// Set 3 has no E0 prefix; each key has a unique single-byte code.
/// Returns 0x00 for unknown/unmapped codes.
fn set1_to_set3_extended(set1_code: u8) -> u8 {
    SET1_EXT_TO_SET3[set1_code as usize & 0x7F]
}

impl Keyboard {
    pub fn scancode_for_ascii(ascii: u8) -> u8 {
        match ascii {
            b'\r' | b'\n' => 0x1C, // Enter
            0x08 => 0x0E,          // Backspace
            b'\t' => 0x0F,         // Tab
            b' ' => 0x39,          // Space
            0x1B => 0x01,          // Escape
            // Number row and shifted characters
            b'0' | b')' => 0x0B,
            b'1' | b'!' => 0x02,
            b'2' => 0x03,
            b'3' | b'#' => 0x04,
            b'4' | b'$' => 0x05,
            b'5' | b'%' => 0x06,
            b'6' | b'^' => 0x07,
            b'7' | b'&' => 0x08,
            b'8' | b'*' => 0x09,
            b'9' | b'(' => 0x0A,
            // Punctuation (unshifted and shifted)
            b'-' | b'_' => 0x0C,
            b'=' | b'+' => 0x0D,
            b'[' | b'{' => 0x1A,
            b']' | b'}' => 0x1B,
            b';' | b':' => 0x27,
            b'\'' | b'"' => 0x28,
            b'`' | b'~' => 0x29,
            b'\\' | b'|' => 0x2B,
            b',' | b'<' => 0x33,
            b'.' | b'>' => 0x34,
            b'/' | b'?' => 0x35,
            // Letters
            b'a' | b'A' => 0x1E,
            b'b' | b'B' => 0x30,
            b'c' | b'C' => 0x2E,
            b'd' | b'D' => 0x20,
            b'e' | b'E' => 0x12,
            b'f' | b'F' => 0x21,
            b'g' | b'G' => 0x22,
            b'h' | b'H' => 0x23,
            b'i' | b'I' => 0x17,
            b'j' | b'J' => 0x24,
            b'k' | b'K' => 0x25,
            b'l' | b'L' => 0x26,
            b'm' | b'M' => 0x32,
            b'n' | b'N' => 0x31,
            b'o' | b'O' => 0x18,
            b'p' | b'P' => 0x19,
            b'q' | b'Q' => 0x10,
            b'r' | b'R' => 0x13,
            b's' | b'S' => 0x1F,
            b't' | b'T' => 0x14,
            b'u' | b'U' => 0x16,
            b'v' | b'V' => 0x2F,
            b'w' | b'W' => 0x11,
            b'x' | b'X' => 0x2D,
            b'y' | b'Y' => 0x15,
            b'z' | b'Z' => 0x2C,
            _ => 0x00,
        }
    }

    pub fn new() -> Self {
        // Initialize controller RAM with default values
        // Byte 0 is the Command Byte (CCB):
        // - Bit 0: Enable keyboard interrupt (IRQ1)
        // - Bit 1: Enable mouse interrupt (IRQ12)
        // - Bit 2: System flag (should be set after POST)
        // - Bit 4: Disable keyboard
        // - Bit 5: Disable mouse
        // - Bit 6: Translate scancode set 2 to set 1
        let mut controller_ram = [0u8; 32];
        controller_ram[0] = 0x47; // IRQ1 enabled, system flag set, translation enabled

        Self {
            data_register: 0,
            scanning_enabled: true,
            port61_latch: 0,
            port61_refresh: false,
            output_port: 0xDF,
            pending_a20: None,
            pending_guest_reset: None,
            shared_state: Arc::new(SharedKeyboardState::new()),
            controller_ram,
            pending_command: None,
            scancode_set: 1,
            mouse: Ps2Mouse::new(),
            mouse_write_pending: false,
            disabled_aux_polled_response_budget: 0,
            stale_aux_response_keyboard_block_attempts: 0,
            output_buffer: None,
            data_register_source_was_mouse: false,
            last_port60_read_source: None,
            shared_mouse_state: None,
            // Input port: bit 7=kbd unlocked, bit 4=>256KB RAM, bits 0-1=data lines high
            input_port: 0x93,
            input_port_poll: None,
            led_state: 0,
            // Default typematic: delay=1 (500ms), rate=0x0B (~10.9 Hz / 92ms)
            // Matches IBM AT keyboard power-on defaults per IBM Technical Reference.
            typematic_delay: 1,
            typematic_rate: 0x0B,
            typematic_enabled: true,
            repeat_scancode: 0,
            repeat_extended: false,
            repeat_wait: 0,
            port60_read_since_check: false,
            pending_controller_reply: None,
            input_buffer_full: false,
            translation_buffer: FixedRing::new(),
            translate_e0_pending: false,
            keyboard_event_driven: false,
            keyboard_transfer_pending: false,
            keyboard_reset_command_pending: false,
        }
    }

    /// Returns true if 8042 translation (Set 2 -> Set 1) is enabled.
    /// Bit 6 of controller RAM byte 0.
    fn is_translation_enabled(&self) -> bool {
        self.controller_ram[0] & 0x40 != 0
    }

    /// Determine the effective output scancode set, accounting for 8042 translation.
    /// When 8042 translation is enabled (bit 6 of controller RAM byte 0) and the
    /// keyboard is set to Set 2, the output is Set 1 (the 8042 translates Set 2->Set 1).
    fn effective_output_set(&self) -> u8 {
        match self.scancode_set {
            2 if self.is_translation_enabled() => 1, // 8042 translates Set 2 -> Set 1
            _ => self.scancode_set,
        }
    }

    /// Translate a raw Set 1 byte and push translated bytes into self.translation_buffer.
    /// Handles stateful E0 prefix tracking for extended keys.
    fn translate_and_buffer(&mut self, byte: u8) {
        let output_set = self.effective_output_set();

        if output_set == 1 {
            self.translation_buffer.push_back(byte);
            return;
        }

        // Handle E0 prefix byte
        if byte == 0xE0 {
            self.translate_e0_pending = true;
            if output_set == 2 {
                // Set 2 uses E0 prefix too
                self.translation_buffer.push_back(0xE0);
            }
            // Set 3 has no E0 prefix - absorb it silently
            return;
        }

        // Handle E1 prefix (Pause key) - pass through for now
        if byte == 0xE1 {
            self.translation_buffer.push_back(byte);
            return;
        }

        let is_break = byte & 0x80 != 0;
        let make_code = byte & 0x7F;
        let is_extended = self.translate_e0_pending;
        self.translate_e0_pending = false;

        match output_set {
            2 | 3 => {
                let (ext_fn, table): (fn(u8) -> u8, &[u8; 128]) = if output_set == 2 {
                    (set1_to_set2_extended, &SET1_TO_SET2)
                } else {
                    (set1_to_set3_extended, &SET1_TO_SET3)
                };
                let translated = if is_extended {
                    ext_fn(make_code)
                } else {
                    table[make_code as usize]
                };
                if translated == 0 {
                    // Unknown key - pass through raw
                    self.translation_buffer.push_back(byte);
                    return;
                }
                if is_break {
                    self.translation_buffer.push_back(0xF0);
                }
                self.translation_buffer.push_back(translated);
            }
            _ => {
                self.translation_buffer.push_back(byte);
            }
        }
    }

    /// Set shared state (called during window setup).
    ///
    /// Replaces the default `SharedKeyboardState` (created by `Keyboard::new()`)
    /// with one that's also held by the GUI callback thread. The previous
    /// instance is dropped; its FIFO contents are not migrated because
    /// `set_shared_state` is called once during emulator construction
    /// before any guest code has run.
    pub fn set_shared_state(&mut self, state: Arc<SharedKeyboardState>) {
        // Enforce the "called once before guest code runs" contract:
        // if the previous SharedKeyboardState has any queued bytes,
        // they'd be silently dropped here. Failing fast in debug builds
        // surfaces ordering bugs at the call site instead of as random
        // missing scancodes.
        debug_assert!(
            self.shared_state.pending_count() == 0,
            "set_shared_state called with non-empty FIFO — would drop bytes"
        );
        self.shared_state = state;
        self.sync_shared_mirrors();
    }

    /// Push every field the host input thread mirrors into the shared state.
    ///
    /// `inject_scancode` consults the shared gate, not these fields, so any
    /// path that assigns them wholesale — a checkpoint restore, a shared-state
    /// swap — must reconcile the mirrors or the gate keeps the pre-restore
    /// answer: keys reach a restored scan-disabled guest, or valid keys are
    /// dropped after restoring one that had scanning on.
    fn sync_shared_mirrors(&self) {
        self.shared_state
            .set_keyboard_enabled(!self.is_keyboard_disabled());
        self.shared_state
            .set_scanning_enabled(self.scanning_enabled);
        self.shared_state
            .set_typematic(self.typematic_delay, self.typematic_rate);
    }

    /// Get shared state reference (for check_keyboard_irq and other
    /// operations). Always present — `Keyboard::new()` constructs a
    /// `SharedKeyboardState` and `set_shared_state` only swaps it.
    pub fn shared_state(&self) -> &SharedKeyboardState {
        &self.shared_state
    }

    pub fn shared_keyboard_enabled(&self) -> bool {
        self.shared_state.is_keyboard_enabled()
    }

    fn set_scanning_enabled(&mut self, enabled: bool) {
        self.scanning_enabled = enabled;
        self.shared_state.set_scanning_enabled(enabled);
    }

    /// Set shared mouse state (called during window setup)
    pub fn set_shared_mouse_state(&mut self, state: Arc<SharedMouseState>) {
        self.shared_mouse_state = Some(state);
    }

    /// Get shared mouse state reference
    pub fn shared_mouse_state(&self) -> Option<&SharedMouseState> {
        self.shared_mouse_state.as_deref()
    }

    /// Get LED state (bit 0=ScrollLock, bit 1=NumLock, bit 2=CapsLock)
    pub fn get_led_state(&self) -> u8 {
        self.led_state
    }

    /// Get typematic delay value (0-3)
    pub fn get_typematic_delay(&self) -> u8 {
        self.typematic_delay
    }

    /// Get typematic rate value (0-31)
    pub fn get_typematic_rate(&self) -> u8 {
        self.typematic_rate
    }

    /// Update typematic delay and rate values, sync to SharedKeyboardState.
    /// Does NOT change typematic_enabled (use set_typematic for BIOS path).
    fn update_typematic_values(&mut self, delay: u8, rate: u8) {
        self.typematic_delay = delay & 0x03;
        self.typematic_rate = rate & 0x1F;
        self.shared_state
            .set_typematic(self.typematic_delay, self.typematic_rate);
    }

    /// Set typematic delay (0-3) and rate (0-31).
    /// Syncs to SharedKeyboardState for GUI thread repeat timing.
    /// Re-enables typematic if previously disabled.
    pub fn set_typematic(&mut self, delay: u8, rate: u8) {
        self.update_typematic_values(delay, rate);
        self.typematic_enabled = true;
    }

    /// Disable typematic repeat (INT 16h AH=03h AL=04h).
    /// Clears current repeat state. Re-enabled by set_typematic().
    pub fn disable_typematic_repeat(&mut self) {
        self.typematic_enabled = false;
        self.repeat_scancode = 0;
        self.repeat_wait = 0;
    }

    /// Check if typematic repeat is enabled.
    pub fn is_typematic_enabled(&self) -> bool {
        self.typematic_enabled
    }

    /// Take pending A20 gate update from output port writes.
    pub fn take_pending_a20(&mut self) -> Option<bool> {
        self.pending_a20.take()
    }

    /// Return whether an output-port A20 update is waiting for the outer
    /// execution loop to consume it.
    pub fn has_pending_a20(&self) -> bool {
        self.pending_a20.is_some()
    }

    /// Take a pending guest system reset request from the output-port reset line.
    pub fn take_pending_guest_reset(&mut self) -> Option<GuestResetSource> {
        self.pending_guest_reset.take()
    }

    /// Return whether a guest reset request is waiting for the outer execution
    /// loop to consume it.
    pub fn has_pending_guest_reset(&self) -> bool {
        self.pending_guest_reset.is_some()
    }

    /// Set the 8042 output-port A20 source latch.
    pub fn set_output_port_a20(&mut self, enabled: bool) {
        if enabled {
            self.output_port |= 0x02;
        } else {
            self.output_port &= !0x02;
        }
    }

    /// Return the 8042 output-port A20 source latch state.
    pub fn output_port_a20_enabled(&self) -> bool {
        self.output_port & 0x02 != 0
    }

    pub fn output_port(&self) -> u8 {
        self.output_port
    }

    pub fn controller_command_byte(&self) -> u8 {
        self.controller_ram[0]
    }

    /// Note that the emulated CPU read port 0x60 (polling detection).
    pub fn note_port60_read(&mut self) {
        self.port60_read_since_check = true;
        // In event-driven mode, signal that the emulator should schedule a
        // transfer event if more port-60h data is queued. Mouse packet
        // continuations need this too: they must be deferred so the next
        // stage can raise a fresh IRQ12 edge, rather than being eagerly
        // restocked into OBF with no interrupt.
        if self.keyboard_event_driven && (self.has_pending_scancodes() || self.mouse.has_output()) {
            self.keyboard_transfer_pending = true;
        }
    }

    /// Return whether port 0x60 was read since the last check and clear the flag.
    pub fn take_port60_read_since_check(&mut self) -> bool {
        let v = self.port60_read_since_check;
        self.port60_read_since_check = false;
        v
    }

    /// Count of synthetic command-response bytes (8042/keyboard ACKs)
    /// in the FIFO. Renamed from `pending_local_count` in the
    /// 2026-05-12 dual→single queue collapse — the old name reflected
    /// the pre-collapse separate local ring buffer, which no longer
    /// exists.
    pub fn pending_command_response_count(&self) -> usize {
        self.shared_state.pending_command_response_count()
    }

    /// Count of genuine user-input bytes (GUI/HostKey keystrokes, explicit raw
    /// scancode injection) in the FIFO. Renamed from
    /// `pending_shared_count` in the 2026-05-12 dual→single queue
    /// collapse — the old name reflected the pre-collapse separate
    /// shared FIFO, which is now the unified FIFO.
    pub fn pending_user_input_count(&self) -> usize {
        self.shared_state.pending_user_input_count()
    }

    /// Check if IRQ1 is enabled in the 8042 command byte (bit 0).
    pub fn irq1_enabled(&self) -> bool {
        (self.controller_ram[0] & 0x01) != 0
    }

    /// Check if the output buffer contains a command response (suppresses IRQ1).
    pub fn is_command_response_pending(&self) -> bool {
        self.output_buffer.is_some_and(|ob| {
            matches!(
                ob.source,
                OutputSource::ControllerReply | OutputSource::KeyboardReply
            )
        })
    }

    /// Return true when the controller output buffer contains a keyboard-port
    /// byte that should assert IRQ1 under the current controller command byte.
    pub fn irq1_visible_output_ready(&self) -> bool {
        self.irq1_enabled()
            && self.output_buffer.is_some_and(|ob| {
                matches!(
                    ob.source,
                    OutputSource::Keyboard | OutputSource::KeyboardReply
                )
            })
    }

    /// Assert IRQ1 for an already-visible keyboard-port byte.
    ///
    /// Most bytes assert IRQ1 when `stage_output` fills OBF. Some paths stage
    /// without a `Pic` (polling convenience paths), and KBDINT can also be
    /// re-enabled after OBF is already full. This hook lets the emulator
    /// reconcile the visible OBF state with the PIC at an interrupt boundary.
    pub fn raise_irq1_for_visible_output(&self, pic: &mut Pic) {
        if self.irq1_visible_output_ready() {
            pic.set_irq_source(1, PicIrqSource::Keyboard, true);
        }
    }

    /// Check if IRQ12 is enabled in the 8042 command byte (bit 1).
    pub fn irq12_enabled(&self) -> bool {
        (self.controller_ram[0] & 0x02) != 0
    }

    pub fn irq12_visible_output_ready(&self, pic: &Pic) -> bool {
        !self.is_mouse_disabled()
            && self.irq12_enabled()
            && !pic.is_irq_masked(12)
            && self.output_buffer.is_some_and(|ob| ob.source.is_mouse())
    }

    /// Assert IRQ12 for an already-visible auxiliary-port byte.
    ///
    /// Mouse command responses are staged without a `Pic` so polling BIOS code
    /// can read them immediately. Interrupt-driven guests such as NT still need
    /// an IRQ12 edge once AUXINT and the PIC gate are open.
    pub fn raise_irq12_for_visible_output(&self, pic: &mut Pic) {
        if self.irq12_visible_output_ready(pic) {
            pic.set_irq_source(12, PicIrqSource::AuxMouse, true);
        }
    }

    // -- Snapshot getters (for Time Machine checkpoints) --

    pub fn data_register(&self) -> u8 {
        self.data_register
    }

    pub fn translate_e0_pending(&self) -> bool {
        self.translate_e0_pending
    }

    pub fn translation_buffer_snapshot(&self) -> Vec<u8> {
        self.translation_buffer.iter().copied().collect()
    }

    /// Snapshot of the unified port-60h FIFO contents (untagged bytes,
    /// for tests that don't care about the UserInput/CommandResponse
    /// distinction). The byte order matches pop order ignoring KBDINT
    /// gating, so both user-input and command-response bytes appear in
    /// FIFO position. For snapshot/restore use `scancode_queue_tagged_snapshot`
    /// instead — it preserves the variant tag.
    pub fn scancode_queue_snapshot(&self) -> Vec<u8> {
        self.shared_state
            .queue_snapshot()
            .into_iter()
            .map(|b| b.value())
            .collect()
    }

    pub fn scancode_queue_tagged_snapshot(&self) -> Vec<(u8, u8)> {
        self.shared_state
            .queue_snapshot()
            .into_iter()
            .map(|qb| match qb {
                QueuedByte::UserInput(b) => (b, 0u8),
                QueuedByte::CommandResponse(b) => (b, 1u8),
            })
            .collect()
    }

    /// Pop one raw scancode from the unified FIFO, regardless of
    /// KBDINT or variant. Used by batch-file `CHOICE`/`PAUSE` processing
    /// (`Shell::process_kbd_input`) to consume raw injected scancodes
    /// outside the port-60h IRQ path. Returns the first byte in FIFO order.
    pub fn pop_local_scancode(&mut self) -> Option<u8> {
        // Pop with IRQ1 forced enabled so user-input bytes are visible.
        // The shell's batch consumer is a side channel — it doesn't go
        // through OBF or assert IRQ1 — so KBDINT is irrelevant.
        self.shared_state.pop_byte(true).map(|b| b.value())
    }

    pub fn scancode_set(&self) -> u8 {
        self.scancode_set
    }

    pub fn led_state(&self) -> u8 {
        self.led_state
    }

    pub fn command_response_pending(&self) -> bool {
        self.is_command_response_pending()
    }

    pub fn controller_ram(&self) -> [u8; 32] {
        self.controller_ram
    }

    /// Test-only: white-box CCB mutation. Mirrors what a real DOS keyboard
    /// driver does at init via the i8042 `0x60` + payload command pair.
    /// Used by `--real-boot` test fixtures whose boot path doesn't run a
    /// kbd-enabling driver — see `ensure_kbd_enabled_for_test` in the
    /// `real_boot_rom` test file. Sets exactly the requested CCB byte
    /// (caller composes bit 4 cleared + bit 0 set, for example).
    ///
    /// `#[doc(hidden)]` because this is intentionally not part of the
    /// public API surface — production callers should write CCB through
    /// the i8042 command path (port 0x64 + 0x60), not poke controller
    /// state directly. The method is `pub` only because it crosses crate
    /// boundaries (tests in the `gp64` crate call into `gp64-hw`).
    #[doc(hidden)]
    pub fn set_ccb_for_test(&mut self, ccb: u8) {
        self.controller_ram[0] = ccb;
    }

    pub fn output_from_mouse(&self) -> bool {
        self.output_buffer.is_some_and(|ob| ob.source.is_mouse())
    }

    pub fn output_source(&self) -> Option<OutputSource> {
        self.output_buffer.map(|ob| ob.source)
    }

    pub fn last_port60_read_source(&self) -> Option<OutputSource> {
        self.last_port60_read_source
    }

    pub fn typematic_delay(&self) -> u8 {
        self.typematic_delay
    }

    pub fn typematic_rate(&self) -> u8 {
        self.typematic_rate
    }

    pub fn is_scanning_enabled(&self) -> bool {
        self.scanning_enabled
    }

    /// Check if keyboard is disabled via 8042 command (0xAD)
    /// Controller RAM byte 0 bit 4 = keyboard disabled
    #[inline]
    pub fn is_keyboard_disabled(&self) -> bool {
        (self.controller_ram[0] & 0x10) != 0
    }

    /// Check if mouse is disabled via 8042 command (0xA7)
    /// Controller RAM byte 0 bit 5 = mouse disabled
    #[inline]
    pub fn is_mouse_disabled(&self) -> bool {
        (self.controller_ram[0] & 0x20) != 0
    }

    /// Enqueue raw command-response bytes (synthetic ACKs from the 8042
    /// or keyboard MCU, e.g. `0xFA` after a `0xED` SETLEDS, the
    /// `0xFA 0xAB 0x83` from `0xF2` IDENTIFY). These bypass the KBDINT
    /// gate at pop time and bypass translation (already in their final
    /// form). Does NOT touch `data_register` — staging populates it.
    ///
    /// **ONLY for 8042/keyboard command responses; do not use for user
    /// input.** Tagging user-input bytes as `CommandResponse` would
    /// bypass Set 2/3 scancode translation, invert priority (command
    /// responses jump ahead of user input by design), and ignore the
    /// KBDINT gate (user input must wait for KBDINT to re-enable;
    /// command responses drain regardless). For user-input scancode bytes
    /// use `enqueue_input_scancodes` or `enqueue_input_scancodes_force`
    /// instead, depending on whether `scanning_enabled` should be honoured.
    pub fn enqueue_scancodes(&mut self, bytes: &[u8]) {
        // Drop on overflow (real hardware would also lose bytes if the
        // 8042's tiny reply buffer overflowed). The 64-byte FIFO is
        // sized so this can't happen in practice for ACK chains.
        let _ = self.shared_state.enqueue_command_response(bytes);
    }

    /// Force-enqueue raw user-input scancode bytes from explicit raw
    /// sources (`SCANCODES`, GUI special-key sequences that need to bypass
    /// `scanning_enabled`, force-push in test harnesses).
    ///
    /// Bypasses `scanning_enabled` — per the project's documented
    /// divergence from real hardware for the raw scancode escape hatch
    /// (a real PS/2 keyboard would stop scancode production when
    /// disabled via 0xF5 until 0xF4 re-enables it; we deliberately
    /// allow explicit raw control/debug injection through). But respects
    /// KBDINT gating at pop time and Set 1→Set 2/3 translation at pop time, since
    /// the bytes still represent genuine user input and must obey the
    /// same downstream contract as GUI-thread `inject_scancode`.
    ///
    /// Compare with `enqueue_scancodes` (command-response path) and
    /// `enqueue_input_scancodes` (which additionally honours
    /// `scanning_enabled` + the i8042 keyboard-disable gate).
    pub fn enqueue_input_scancodes_force(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        let ok = self.shared_state.enqueue_user_input_force(bytes);
        if !ok {
            return false;
        }
        if crate::kbd_flow_trace_enabled() {
            let first = bytes.first().copied().unwrap_or(0);
            let last = bytes.last().copied().unwrap_or(0);
            tracing::info!(
                target: "gp64::kbd::flow",
                source = "user_force",
                count = bytes.len(),
                first = format_args!("{:02X}", first),
                last = format_args!("{:02X}", last),
                pending = self.shared_state.pending_count(),
                "KBD_ENQUEUE_INPUT_FORCE"
            );
        }
        true
    }

    /// Enqueue scancodes from input sources (main-thread special-key
    /// injection: Pause/PrintScreen/SysRq, explicit raw IPC scancode push,
    /// CLI keyboard injection). Honours the i8042 keyboard-disable gate
    /// (`is_keyboard_disabled`) and the keyboard's own `scanning_enabled`
    /// state — both real-hardware gates that the GUI callback path
    /// (`SharedKeyboardState::inject_scancode`) also observes.
    ///
    /// Bytes enter the unified FIFO untranslated; pop-time translation
    /// in `pop_keyboard_byte` applies Set 1→Set 2/Set 3 conversion based
    /// on the effective output set at delivery time. This matches the
    /// GUI-callback path's semantics — both producers now push raw Set 1
    /// bytes and the single consumer translates uniformly.
    pub fn enqueue_input_scancodes(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        let kbd_disabled = self.is_keyboard_disabled();
        if !self.scanning_enabled || kbd_disabled {
            if crate::kbd_flow_trace_enabled() {
                tracing::info!(
                    target: "gp64::kbd::flow",
                    source = "user",
                    count = bytes.len(),
                    scanning_enabled = self.scanning_enabled,
                    kbd_disabled,
                    "KBD_INPUT_DROP"
                );
            }
            return false;
        }
        // Use the force variant: scanning_enabled was already checked
        // above; the kbd_disabled gate above mirrors the GUI path's
        // SharedKeyboardState::is_keyboard_enabled check. enqueue_user_input_force
        // skips those gates because the GUI path applies them on its
        // own thread; this main-thread caller has already done the work.
        let ok = self.shared_state.enqueue_user_input_force(bytes);
        if !ok {
            return false;
        }
        if crate::kbd_flow_trace_enabled() {
            let first = bytes.first().copied().unwrap_or(0);
            let last = bytes.last().copied().unwrap_or(0);
            tracing::info!(
                target: "gp64::kbd::flow",
                source = "user",
                count = bytes.len(),
                first = format_args!("{:02X}", first),
                last = format_args!("{:02X}", last),
                pending = self.shared_state.pending_count(),
                "KBD_ENQUEUE_INPUT"
            );
        }
        true
    }

    pub fn has_pending_scancodes(&self) -> bool {
        // Include staged data in the controller output buffer.
        if self.output_buffer.is_some() {
            return true;
        }
        // Translation buffer holds mid-translation continuation bytes
        // (e.g. the second byte of a Set 2 break sequence) and is
        // independent of keyboard-enable gating.
        if !self.translation_buffer.is_empty() {
            return true;
        }
        self.shared_state.has_pending()
    }

    /// Returns true if the controller output buffer has a byte visible to port 0x60.
    pub fn p60_has_unread_data(&self) -> bool {
        self.output_buffer.is_some()
    }

    pub fn age_stale_aux_response_blocking_keyboard_input(&mut self, site: &'static str) -> bool {
        let Some(ob) = self.output_buffer else {
            self.stale_aux_response_keyboard_block_attempts = 0;
            return false;
        };
        if ob.source != OutputSource::AuxMouseResponse {
            self.stale_aux_response_keyboard_block_attempts = 0;
            return false;
        }
        if !self.irq1_enabled()
            || self.is_keyboard_disabled()
            || self.shared_state.pending_user_input_count() == 0
        {
            self.stale_aux_response_keyboard_block_attempts = 0;
            return false;
        }

        self.stale_aux_response_keyboard_block_attempts = self
            .stale_aux_response_keyboard_block_attempts
            .saturating_add(1);
        if self.stale_aux_response_keyboard_block_attempts
            < STALE_AUX_RESPONSE_KEYBOARD_BLOCK_GRACE_ATTEMPTS
        {
            return false;
        }

        self.discard_staged_aux_response(site).is_some()
    }

    fn discard_staged_aux_response(&mut self, site: &'static str) -> Option<OutputByte> {
        let ob = self.output_buffer?;
        if ob.source != OutputSource::AuxMouseResponse {
            return None;
        }

        let dropped = self
            .output_buffer
            .take()
            .expect("checked output_buffer above");
        self.data_register = dropped.value;
        self.data_register_source_was_mouse = true;
        self.disabled_aux_polled_response_budget = 0;
        self.stale_aux_response_keyboard_block_attempts = 0;
        let dropped_queued = dropped
            .aux_response_generation
            .map(|generation| {
                self.mouse
                    .discard_command_responses_with_generation(generation)
            })
            .unwrap_or(0);

        if crate::kbd_flow_trace_enabled() {
            tracing::warn!(
                target: "gp64::kbd::flow",
                byte = format_args!("{:02X}", dropped.value),
                generation = ?dropped.aux_response_generation,
                dropped_queued,
                pending_user_input = self.shared_state.pending_user_input_count(),
                pending_command_response = self.shared_state.pending_command_response_count(),
                site,
                "OBF_DROP_STALE_AUX_RESPONSE"
            );
        }

        Some(dropped)
    }

    // =====================================================================
    // 8042 controller output buffer — stage / consume API
    // =====================================================================

    /// Stage the next available byte into the controller output buffer.
    ///
    /// Does nothing if the buffer is already full.  Selects the next byte
    /// using the existing keyboard/mouse alternation policy.  Asserts the
    /// correct IRQ line if the byte warrants one.
    pub fn stage_output(&mut self, pic: &mut Pic) {
        if self.output_buffer.is_some()
            && !self.age_stale_aux_response_blocking_keyboard_input("stage_output_obf_full")
        {
            self.raise_irq12_for_visible_output(pic);

            return; // Buffer already full
        }

        let kbd_pending = self.has_pending_keyboard_data();
        // Async-path AUXINT gate: only consider the mouse here when IRQ12
        // is enabled at both the 8042 (CCB.AUXINT) AND the 8259 PIC (IMR).
        // If we staged a mouse byte to OBF without an IRQ12 edge that the
        // CPU will service, OBF locks up forever (no ISR consumer signal)
        // and starves keyboard delivery via `KeyboardTransfer`. Bytes wait
        // in the mouse output queue until both gates are open — the
        // controller-RAM write path re-arms `irq12_pending` for AUXINT
        // 0→1 transitions; PIC IMR transitions are caught the next time
        // stage_output is invoked (typically by the next keyboard event).
        // CCB.AUXINT can be set while the slave PIC masks IRQ12. An
        // unsolicited mouse packet must remain queued in that state so
        // the shared output buffer stays available for keyboard bytes.
        // The synchronous sibling paths (`auto_stage_no_pic`,
        // `read_port60` eager restock) deliberately do NOT apply this
        // gate: there the host has just sent a mouse command and is
        // polling for the reply, so we must drain the queue regardless
        // of AUXINT/IMR — that is how BIOS or mouse-driver code enumerates the PS/2
        // mouse with interrupts disabled.
        let mouse_response_pending = self.mouse.front_output_is_command_response();
        let mouse_pending = mouse_response_pending
            || (self.mouse.has_output() && self.irq12_enabled() && !pic.is_irq_masked(12));
        if !kbd_pending && !mouse_pending {
            return;
        }

        // Source policy: keyboard wins when both queues have data. Mouse is
        // staged only when there is no keyboard byte waiting.
        //
        // Alternating sources requires both consumers to drain OBF. An
        // incomplete mouse interrupt handler can leave a mouse byte there
        // indefinitely, blocking keyboard input. Prefer the keyboard when
        // both sources are ready to keep that input available.
        let ob = if mouse_response_pending {
            self.try_stage_mouse().or_else(|| self.try_stage_keyboard())
        } else {
            self.try_stage_keyboard().or_else(|| self.try_stage_mouse())
        };

        if let Some(ob) = ob {
            self.data_register = ob.value;
            self.output_buffer = Some(ob);
            self.stale_aux_response_keyboard_block_attempts = 0;
            if crate::kbd_flow_trace_enabled() {
                tracing::info!(
                    target: "gp64::kbd::flow",
                    byte = format_args!("{:02X}", ob.value),
                    source = ?ob.source,
                    site = "stage_output",
                    "OBF_FILL"
                );
            }
            // Assert IRQ based on source. The pop_keyboard_byte shared-queue
            // gate and the AUXINT pre-check on `mouse_pending` above should
            // prevent shared/aux bytes from reaching here while their IRQ is
            // masked, but ControllerReply and polled response bytes
            // can still stage with no IRQ. Warn loudly if a shared/aux byte
            // slipped through — the OBF would otherwise be stuck full
            // forever, which is the failure shape that masked the real-boot
            // keyboard hang for so long.
            match ob.source {
                OutputSource::Keyboard | OutputSource::KeyboardReply if self.irq1_enabled() => {
                    pic.set_irq_source(1, PicIrqSource::Keyboard, true);
                }
                OutputSource::Keyboard | OutputSource::KeyboardReply => {
                    tracing::warn!(
                        target: "gp64::kbd::flow",
                        byte = format_args!("{:02X}", ob.value),
                        "OBF_STAGED_NO_IRQ1 — KBDINT disabled; OBF will block until re-enabled"
                    );
                }
                OutputSource::AuxMouse if self.irq12_enabled() => {
                    pic.set_irq_source(12, PicIrqSource::AuxMouse, true);
                }
                OutputSource::AuxMouse => {
                    tracing::warn!(
                        target: "gp64::kbd::flow",
                        byte = format_args!("{:02X}", ob.value),
                        "OBF_STAGED_NO_IRQ12 — AUXINT disabled; OBF will block until re-enabled"
                    );
                }
                OutputSource::AuxMouseResponse if self.irq12_visible_output_ready(pic) => {
                    pic.set_irq_source(12, PicIrqSource::AuxMouse, true);
                }
                OutputSource::AuxMouseResponse => {}
                OutputSource::ControllerReply => {}
            }
        }
    }

    /// Consume the current output buffer byte.
    ///
    /// Lowers the IRQ line associated with the consumed byte's source.
    /// Saves the byte as the stale readback value in `data_register`.
    pub fn consume_output(&mut self, pic: &mut Pic) -> Option<OutputByte> {
        let ob = self.output_buffer.take()?;
        self.data_register = ob.value;
        self.data_register_source_was_mouse = ob.source.is_mouse();
        self.stale_aux_response_keyboard_block_attempts = 0;
        match ob.source {
            OutputSource::Keyboard | OutputSource::KeyboardReply => {
                pic.set_irq_source(1, PicIrqSource::Keyboard, false)
            }
            OutputSource::AuxMouse | OutputSource::AuxMouseResponse => {
                pic.set_irq_source(12, PicIrqSource::AuxMouse, false)
            }
            OutputSource::ControllerReply => {}
        }
        if crate::kbd_flow_trace_enabled() {
            tracing::info!(
                target: "gp64::kbd::flow",
                byte = format_args!("{:02X}", ob.value),
                source = ?ob.source,
                site = "consume_output",
                "OBF_DRAIN"
            );
        }
        Some(ob)
    }

    /// Read the controller output buffer as a guest port 0x60 read.
    ///
    /// Consumes the staged byte (with IRQ deassertion). The next byte is NOT
    /// staged eagerly — that happens via the F000:FF10 HLE trap (after EOI)
    /// or via the KeyboardTransfer event scheduled by `note_port60_read`.
    ///
    /// On AT 8042 hardware, draining OBF leaves the last byte latched in
    /// the data register until the next transfer. A second port 0x60 read
    /// before that transfer returns the same byte. Preserve that latch
    /// when an INT 09h handler reads the data port and chains to another
    /// handler that reads it again.
    pub fn read_port60(&mut self, pic: &mut Pic) -> u8 {
        // Clear input port poll mode on data read (per 8042 spec)
        self.input_port_poll = None;

        let consumed = self.consume_output(pic);
        self.last_port60_read_source = consumed.map(|ob| ob.source);
        let value = consumed.map_or(self.data_register, |ob| ob.value);

        // Eager OBF restock for MOUSE command replies after either an earlier
        // mouse-response byte or a foreign keyboard byte is consumed.
        //
        // Mouse commands like RESET (0xFF → FA + AA + 00) and IDENTIFY
        // (0xF2 → FA + 00) produce 2-3 byte replies that the host polls
        // for sequentially via the 0x64 status / 0x60 data pair. Without
        // an immediate restock the second/third byte never reaches OBF
        // and the host (BIOS or mouse-driver code) times out — one of two paths
        // that breaks PS/2 mouse enumeration.
        //
        // A keyboard byte can already own the controller's one shared OBF when
        // a mouse command queues its first response. Once INT 09h consumes that
        // foreign byte, the current aux response must become visible without
        // waiting for an unrelated transfer event. Ordinary queued keyboard
        // bytes remain on the deferred KeyboardTransfer path below.
        let disabled_aux_restocked = consumed.is_some()
            && self.stage_disabled_aux_polled_response_no_pic("read_port60_disabled_aux_restock");
        if !disabled_aux_restocked
            && consumed.is_some_and(|ob| {
                matches!(
                    ob.source,
                    OutputSource::AuxMouseResponse | OutputSource::Keyboard
                )
            })
            && self.mouse.front_output_is_command_response()
        {
            if let Some(next) = self.try_stage_mouse() {
                self.data_register = next.value;
                self.output_buffer = Some(next);
                self.stale_aux_response_keyboard_block_attempts = 0;
                if crate::kbd_flow_trace_enabled() {
                    tracing::info!(
                        target: "gp64::kbd::flow",
                        byte = format_args!("{:02X}", next.value),
                        source = ?next.source,
                        site = "read_port60_mouse_eager",
                        "OBF_FILL"
                    );
                }
                // If AUXINT/PIC are open, command-response continuations are
                // interrupt-delivered like any aux-origin OBF byte. When they
                // are closed, the byte remains visible for polling BIOS/driver
                // code. Ordinary stream packets must not use this eager path;
                // their continuation bytes need their own staged IRQ12 edges.
                self.raise_irq12_for_visible_output(pic);
            }
        }

        // Eager OBF restock for KEYBOARD command-response continuations only.
        //
        // Multi-byte keyboard replies such as IDENTIFY (FA AB 83) are polled
        // just like mouse command replies. The continuation bytes must become
        // visible immediately to the polling guest, but ordinary user-input
        // scancodes must stay on the deferred KeyboardTransfer path so they
        // get their own IRQ1 edge and chained INT 09h handlers keep seeing the
        // latched byte on immediate re-read.
        if consumed.is_some_and(|ob| ob.source == OutputSource::KeyboardReply) {
            if let Some(next) = self.try_stage_polled_keyboard_response() {
                self.data_register = next.value;
                self.output_buffer = Some(next);
                self.stale_aux_response_keyboard_block_attempts = 0;
                if crate::kbd_flow_trace_enabled() {
                    tracing::info!(
                        target: "gp64::kbd::flow",
                        byte = format_args!("{:02X}", next.value),
                        source = ?next.source,
                        site = "read_port60_keyboard_response_eager",
                        "OBF_FILL"
                    );
                }
            }
        }

        // Signal event-driven scheduling — keyboard_transfer_pending will
        // schedule a KeyboardTransfer event at the next quantum boundary if
        // more bytes are queued. The event handler stages the next byte after
        // a small delay (mimicking real 8042 keyboard FIFO transfer time).
        self.note_port60_read();

        if crate::kbd_flow_trace_enabled() {
            tracing::info!(
                target: "gp64::kbd::flow",
                byte = format_args!("{:02X}", value),
                fresh = consumed.is_some(),
                "GUEST_PORT60_READ"
            );
        }

        value
    }

    /// Read the currently visible controller output buffer byte for HLE IRQ1
    /// service.
    ///
    /// Consumes only keyboard-port bytes. Ordinary scancodes return
    /// `Some(byte)`, while keyboard-device replies such as ACK are consumed and
    /// return `None` so HLE INT 09h callers can ignore them. Mouse bytes and
    /// pure 8042 controller replies are left in place for their real consumers.
    pub fn read_irq1_output(&mut self, pic: &mut Pic) -> Option<u8> {
        match self.output_buffer.as_ref().map(|ob| ob.source) {
            Some(OutputSource::Keyboard) => self.consume_output(pic).map(|ob| ob.value),
            Some(OutputSource::KeyboardReply) => {
                self.consume_output(pic);
                None
            }
            _ => None,
        }
    }

    /// Read the currently visible controller output buffer keyboard scancode.
    ///
    /// Unlike `read_irq1_output`, this consumes only ordinary scancodes and
    /// leaves keyboard-device replies in place. Use this when the caller must
    /// decline non-scancode IRQ1 work before acknowledging or ending the PIC
    /// interrupt.
    pub fn read_irq1_scancode_output(&mut self, pic: &mut Pic) -> Option<u8> {
        if self.output_buffer.as_ref().map(|ob| ob.source) != Some(OutputSource::Keyboard) {
            return None;
        }
        self.consume_output(pic).map(|ob| ob.value)
    }

    /// Drop up to `count` host keyboard bytes that belong to a process being
    /// torn down. The owner count includes a byte already staged in OBF and
    /// the remaining FIFO bytes; command replies stay untouched.
    pub fn discard_pending_user_input(&mut self, pic: &mut Pic, count: usize) -> usize {
        if count == 0 {
            return 0;
        }
        let mut remaining = count;
        let mut removed = 0;
        if remaining != 0 && self.output_source() == Some(OutputSource::Keyboard) {
            self.consume_output(pic);
            remaining -= 1;
            removed += 1;
        }
        removed += self.shared_state.discard_user_input(remaining);
        if removed != 0 {
            // Translation continuations originate from the same user byte
            // stream. They must not survive an owner process's teardown.
            self.translation_buffer.clear();
        }
        removed
    }

    /// Read the currently visible controller output buffer byte for HLE
    /// mouse (IRQ12) service.
    ///
    /// Only consumes if OBF already contains an auxiliary-device byte. Keyboard
    /// bytes and controller replies are left in place for their real consumers.
    pub fn read_irq12_output(&mut self, pic: &mut Pic) -> Option<u8> {
        if !self
            .output_buffer
            .as_ref()
            .is_some_and(|ob| ob.source.is_mouse())
        {
            return None;
        }
        self.consume_output(pic).map(|ob| ob.value)
    }

    /// Returns true if keyboard queues have data (not counting mouse or output buffer).
    fn has_pending_keyboard_data(&self) -> bool {
        if self.pending_controller_reply.is_some() {
            return true;
        }
        if !self.translation_buffer.is_empty() {
            return true;
        }
        self.shared_state.has_pending()
    }

    /// Try to pop the next keyboard byte for staging.
    ///
    /// Controller replies take priority over the scancode queue so that a
    /// queued reply is never misattributed as a keyboard scancode even when
    /// scancodes were already in the queue before the reply was generated.
    fn try_stage_keyboard(&mut self) -> Option<OutputByte> {
        if let Some(reply) = self.pending_controller_reply.take() {
            return Some(OutputByte {
                value: reply,
                source: OutputSource::ControllerReply,
                aux_response_generation: None,
            });
        }
        let (byte, is_command_response) = self.pop_keyboard_byte()?;
        // A keyboard-MCU command reply (e.g. 0xFA ACK from 0xED SETLEDS or
        // the byte stream from 0xF2 IDENTIFY) arrives via the keyboard device
        // path. It is not a normal scancode, but it is still a keyboard-port
        // byte and can assert IRQ1 when KBDINT is enabled.
        let source = if is_command_response {
            OutputSource::KeyboardReply
        } else {
            OutputSource::Keyboard
        };
        Some(OutputByte {
            value: byte,
            source,
            aux_response_generation: None,
        })
    }

    /// Stage the next polled keyboard/controller command response without IRQ1.
    ///
    /// This is intentionally narrower than `try_stage_keyboard`: it must not
    /// pop `UserInput` bytes, because ordinary scancodes need the deferred
    /// transfer path to provide a fresh IRQ1 edge and preserve chained INT 09h
    /// latched-read behavior.
    fn try_stage_polled_keyboard_response(&mut self) -> Option<OutputByte> {
        if let Some(reply) = self.pending_controller_reply.take() {
            return Some(OutputByte {
                value: reply,
                source: OutputSource::ControllerReply,
                aux_response_generation: None,
            });
        }
        let byte = self.shared_state.pop_command_response()?;
        Some(OutputByte {
            value: byte,
            source: OutputSource::KeyboardReply,
            aux_response_generation: None,
        })
    }

    /// Try to pop the next mouse byte for staging.
    ///
    /// Honours CCB bit 5 (AUX disable). On real i8042 hardware, when the
    /// auxiliary interface is disabled the mouse clock line is held low and
    /// no mouse data can transfer to the controller output buffer; bytes
    /// that the mouse has already pushed to its internal queue stay there
    /// until the AUX interface is re-enabled. Without this gate, a stale
    /// byte (e.g. the `0xFA` ACK left over from a failed mouse-driver probe)
    /// gets staged to OBF after the mouse driver restores CCB to AUX-disabled +
    /// IRQ12-disabled, no IRQ12 fires, OBF stays full forever, and every
    /// subsequent KeyboardTransfer no-ops — keyboard scancodes pile up in
    /// the shared queue and never reach the guest.
    ///
    /// CCB bit 1 (AUXINT) is intentionally NOT gated here. The async
    /// staging path (`stage_output`) gates on AUXINT itself, so OBF only
    /// receives a mouse byte when IRQ12 will actually fire. The synchronous
    /// callers (`auto_stage_no_pic` after `mouse.handle_command`, and
    /// `read_port60` eager restock for multi-byte replies) need to deliver
    /// command-response bytes to a polling host while AUXINT is masked —
    /// that is how BIOS and mouse drivers enumerate the PS/2 mouse during
    /// POST / driver init, with interrupts disabled.
    fn try_stage_mouse(&mut self) -> Option<OutputByte> {
        if self.is_mouse_disabled() {
            return None;
        }
        let byte = self.mouse.read_output_tagged()?;
        let aux_response_generation = byte.command_generation();
        let source = if byte.is_command_response() {
            OutputSource::AuxMouseResponse
        } else {
            OutputSource::AuxMouse
        };
        Some(OutputByte {
            value: byte.value(),
            source,
            aux_response_generation,
        })
    }

    fn try_stage_disabled_aux_polled_response(&mut self) -> Option<OutputByte> {
        if self.disabled_aux_polled_response_budget == 0 {
            return None;
        }
        if !self.mouse.front_output_is_command_response() {
            self.disabled_aux_polled_response_budget = 0;
            return None;
        }

        let byte = self.mouse.read_output_tagged()?;
        debug_assert!(byte.is_command_response());
        self.disabled_aux_polled_response_budget =
            self.disabled_aux_polled_response_budget.saturating_sub(1);
        Some(OutputByte {
            value: byte.value(),
            source: OutputSource::AuxMouseResponse,
            aux_response_generation: byte.command_generation(),
        })
    }

    fn stage_disabled_aux_polled_response_no_pic(&mut self, site: &'static str) -> bool {
        if self.output_buffer.is_some() {
            return false;
        }
        let Some(ob) = self.try_stage_disabled_aux_polled_response() else {
            return false;
        };

        self.data_register = ob.value;
        self.stale_aux_response_keyboard_block_attempts = 0;
        if crate::kbd_flow_trace_enabled() {
            tracing::info!(
                target: "gp64::kbd::flow",
                byte = format_args!("{:02X}", ob.value),
                source = ?ob.source,
                site,
                "OBF_FILL"
            );
        }
        self.output_buffer = Some(ob);

        true
    }

    /// Pop one keyboard byte from the unified port-60h FIFO.
    ///
    /// Returns `(byte, is_command_response)`. The boolean lets callers
    /// distinguish keyboard-MCU command replies (0xFA ACKs from
    /// `0xED` SETLEDS / `0xF2` IDENTIFY, etc.) from genuine user-input
    /// scancodes when deciding whether to assert IRQ1 or warn about a
    /// staged byte with no consumer. Continuation bytes from translation
    /// are tagged `false` — they are mid-translation continuations of
    /// a UserInput byte that already passed the gate.
    ///
    /// Priority: translation buffer → unified FIFO (gated on KBDINT for
    /// `UserInput`; unconditional for `CommandResponse`).
    ///
    /// **KBDINT-gate invariant** (preserved across the 2026-05-12 dual→single
    /// queue collapse from the 2026-05-07 fix, commit 11af66b9): the async
    /// transfer path must not stage `UserInput` while CCB.KBDINT (bit 0) is
    /// clear. Staging without an IRQ1 edge can leave OBF stuck full forever
    /// when the guest is interrupt-driven. The status-poll path has a narrow
    /// exception: a guest that is actively polling port 0x64 may receive one
    /// user-input byte without IRQ1.
    ///
    /// `CommandResponse` bytes (synthetic ACKs from `0xED` SETLEDS,
    /// `0xF2` IDENTIFY, etc.) drain regardless: the host polls port 0x60
    /// for the reply and never expects IRQ1.
    fn pop_keyboard_byte(&mut self) -> Option<(u8, bool)> {
        let user_input_enabled = self.irq1_enabled() && !self.is_keyboard_disabled();
        self.pop_keyboard_byte_with_gate(user_input_enabled)
    }

    fn pop_keyboard_byte_with_gate(&mut self, user_input_enabled: bool) -> Option<(u8, bool)> {
        // 1. Drain pending translated bytes only when user input delivery is
        //    enabled. These are mid-translation continuation bytes of a user
        //    input byte, so 0xAD must hold them just like queued raw input.
        if user_input_enabled {
            if let Some(b) = self.translation_buffer.pop_front() {
                return Some((b, false));
            }
        }

        // 2. Unified FIFO — KBDINT gate applied at the SharedKeyboardState
        //    level: when `irq1_enabled` is false, only `CommandResponse`
        //    bytes are popped; `UserInput` bytes stay queued until
        //    KBDINT is restored, at which point the controller-RAM write
        //    handler re-arms `irq1_pending` so the main-loop trigger
        //    picks them back up.
        //
        //    The loop handles Set 3's "absorb E0 prefix" case: if a
        //    `UserInput(0xE0)` is popped and translate_and_buffer
        //    consumes it without emitting any output bytes, we try the
        //    next FIFO byte. Without the loop, the caller would see
        //    None even though more bytes are queued.
        loop {
            // Pre-gate diagnostic: WARN once per N attempts when the
            // FIFO has user-input bytes we can't deliver because KBDINT
            // is masked. This is the suspected shape behind "keyboard
            // locks after Alt+Tab" reports — bytes pile up invisibly
            // otherwise.
            if !user_input_enabled {
                let pending_user = self.shared_state.pending_user_input_count();
                let pending_cmd = self.shared_state.pending_command_response_count();
                if pending_user > 0 && pending_cmd == 0 {
                    static GATE_COUNTER: std::sync::atomic::AtomicU32 =
                        std::sync::atomic::AtomicU32::new(0);
                    let n = GATE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if n.is_multiple_of(500) {
                        tracing::warn!(
                            target: "gp64::kbd::flow",
                            pending_shared = pending_user,
                            ccb = format_args!("{:02X}", self.controller_ram[0]),
                            kbdint = self.irq1_enabled(),
                            keyboard_disabled = self.is_keyboard_disabled(),
                            gate_hits = n,
                            "KBD_GATED_USER_INPUT_OFF - user-input bytes blocked by keyboard interrupt/disable gate"
                        );
                    }
                }
            }

            let popped = self.shared_state.pop_byte(user_input_enabled)?;

            match popped {
                QueuedByte::CommandResponse(b) => {
                    if crate::kbd_flow_trace_enabled() {
                        tracing::info!(
                            target: "gp64::kbd::flow",
                            source = "cmdresp",
                            byte = format_args!("{:02X}", b),
                            pending = self.shared_state.pending_count(),
                            "KBD_PORT60_READ"
                        );
                    }
                    // Command responses skip translation — they're already in
                    // their final form (e.g. 0xFA ACK).
                    return Some((b, true));
                }
                QueuedByte::UserInput(b) => {
                    if crate::kbd_flow_trace_enabled() {
                        tracing::info!(
                            target: "gp64::kbd::flow",
                            source = "user",
                            byte = format_args!("{:02X}", b),
                            pending = self.shared_state.pending_count(),
                            "KBD_PORT60_READ"
                        );
                    }
                    self.translate_and_buffer(b);
                    if let Some(translated) = self.translation_buffer.pop_front() {
                        // UserInput post-translation — false (real input).
                        return Some((translated, false));
                    }
                    // Translation consumed the byte without emitting (e.g.,
                    // E0 prefix absorbed in Set 3). Try the next byte.
                    continue;
                }
            }
        }
    }

    /// Stage one user-input keyboard byte for a guest that is polling 0x64
    /// while KBDINT is disabled.
    ///
    /// Real i8042 hardware can fill OBF even when CCB.KBDINT suppresses the
    /// IRQ1 edge. The async event path deliberately avoids that because DOS
    /// guests often depend on an interrupt consumer signal. A protected-mode
    /// driver that reads status first is a different contract: the status read
    /// is the consumer signal, so make exactly one ordinary keyboard byte
    /// visible without touching the PIC.
    fn stage_polled_user_input_no_irq(&mut self, site: &'static str) -> bool {
        if self.output_buffer.is_some()
            || self.irq1_enabled()
            || self.is_keyboard_disabled()
            || self.pending_controller_reply.is_some()
            || self.pending_command.is_some()
            || self.mouse_write_pending
            || self.keyboard_reset_command_pending
            || self.shared_state.has_pending_command_response()
            || (self.translation_buffer.is_empty()
                && self.shared_state.pending_user_input_count() == 0)
        {
            return false;
        }

        let Some((byte, is_command_response)) = self.pop_keyboard_byte_with_gate(true) else {
            return false;
        };
        if is_command_response {
            self.enqueue_scancodes(&[byte]);
            return false;
        }

        self.data_register = byte;
        self.output_buffer = Some(OutputByte {
            value: byte,
            source: OutputSource::Keyboard,
            aux_response_generation: None,
        });
        self.stale_aux_response_keyboard_block_attempts = 0;
        if crate::kbd_flow_trace_enabled() {
            tracing::info!(
                target: "gp64::kbd::flow",
                byte = format_args!("{:02X}", byte),
                site,
                "OBF_FILL_POLLED_KBDINT_OFF"
            );
        }

        true
    }

    /// Returns true if the output buffer is empty AND there are pending scancodes.
    /// Guard for scheduling a KeyboardTransfer event.
    pub fn schedule_transfer_needed(&self) -> bool {
        self.output_buffer.is_none() && self.has_pending_scancodes()
    }

    /// Returns true if a deferred transfer event can stage a byte now.
    ///
    /// This is the event-driven sibling of `stage_output`: keyboard data
    /// uses the existing scancode guard, mouse command responses are polled,
    /// and mouse stream packets only transfer when IRQ12 can be delivered.
    pub fn schedule_transfer_needed_with_pic(&self, pic: &Pic) -> bool {
        if self.output_buffer.is_some() {
            return false;
        }

        self.has_pending_scancodes()
            || (!self.is_mouse_disabled()
                && (self.mouse.front_output_is_command_response()
                    || (self.mouse.has_output() && self.irq12_enabled() && !pic.is_irq_masked(12))))
    }

    /// Reset all event-driven state flags. Called on Time Machine restore.
    pub fn reset_event_driven_state(&mut self) {
        self.keyboard_event_driven = false;
        self.keyboard_transfer_pending = false;
        self.keyboard_reset_command_pending = false;
        self.output_buffer = None;
        self.stale_aux_response_keyboard_block_attempts = 0;
    }

    /// Typematic repeat tick (1ms granularity).
    ///
    /// Reads the currently-held key from SharedKeyboardState (set by host-key
    /// input) and generates repeated make codes at the configured typematic rate,
    /// matching real keyboard controller behavior.
    ///
    /// Flow: accepted host-key input sets repeat_scancode on key-down and clears
    /// it on key-up.
    /// This function counts down in ms and injects make codes when the timer
    /// expires, matching the emulated keyboard's typematic settings.
    pub fn tick_typematic_repeat(&mut self) {
        if !self.typematic_enabled {
            return;
        }

        // Read the currently held key from the host-input boundary.
        let (held_scancode, held_extended) = (
            self.shared_state.get_repeat_scancode(),
            self.shared_state.is_repeat_extended(),
        );

        if held_scancode == 0 {
            // No key held — stop any active repeat
            self.repeat_scancode = 0;
            self.repeat_wait = 0;
            return;
        }

        if held_scancode != self.repeat_scancode {
            // New key pressed — start initial delay
            self.repeat_scancode = held_scancode;
            self.repeat_extended = held_extended;
            self.repeat_wait = Self::typematic_delay_ms(self.typematic_delay);
            // Fall through to countdown below (first tick counts)
        }

        // Hardware-faithful gating: a real keyboard MCU cannot emit make
        // codes while the controller holds the clock low (8042 0xAD) or
        // scanning is disabled (kbd command 0xF5). Treat this as a pause,
        // not as a release: transient BIOS inhibits must not reset the
        // held-key timer and make repeat unreachable.
        if !self.scanning_enabled || self.is_keyboard_disabled() {
            return;
        }

        // Same key still held — count down
        if self.repeat_wait > 0 {
            self.repeat_wait -= 1;
            if self.repeat_wait > 0 {
                return; // Still counting down
            }

            // Timer expired — inject repeat make code through the same shared
            // path the GUI uses, so KBDINT/scanning gates and scancode-set
            // translation are honoured uniformly. inject_scancode also
            // refreshes key_press_times[key_idx] and re-marks injected_keys
            // every tick — benign on this path (production never reads them).
            {
                let key_idx = self.repeat_scancode as usize;
                if self.repeat_extended {
                    self.shared_state
                        .inject_scancode(&[0xE0, self.repeat_scancode], key_idx, true);
                } else {
                    self.shared_state
                        .inject_scancode(&[self.repeat_scancode], key_idx, true);
                }
            }

            // Reset timer to repeat rate (subsequent repeats are faster)
            self.repeat_wait = Self::typematic_rate_ms(self.typematic_rate);
        }
    }

    /// Toggle the ScrollLock LED state. The hardware LED is a real keyboard
    /// register; toggling it does not affect BIOS shift_flags (those live in
    /// the DOS driver's `DosKbdModifierState` post-Stage-7). Production code
    /// no longer calls this directly — INT 09h scancode 0x46 is handled by
    /// the DOS driver, which composes a 0xED Set-LEDs command if it wants the
    /// hardware LED to follow.
    pub fn toggle_scroll_lock(&mut self) {
        self.led_state ^= 0x01;
    }

    /// Check if NumLock LED is on (reflects real keyboard MCU state, not
    /// BIOS shift flags). After Stage 7, the BIOS shift-flag bits live in
    /// the DOS driver — callers wanting BIOS NumLock state should read
    /// BDA 0x0040:0x0017 bit 5 (`bda::kbd_get_shift_flags`).
    pub fn is_num_lock_on(&self) -> bool {
        (self.led_state & 0x02) != 0
    }

    /// Check if CapsLock LED is on (see `is_num_lock_on`).
    pub fn is_caps_lock_on(&self) -> bool {
        (self.led_state & 0x04) != 0
    }

    /// Check if ScrollLock LED is on (see `is_num_lock_on`).
    pub fn is_scroll_lock_on(&self) -> bool {
        (self.led_state & 0x01) != 0
    }

    // =========================================================================
    // Special Key Injection Methods
    // =========================================================================

    /// Inject Pause key make sequence: E1 1D 45 E1 9D C5
    /// Note: Pause key has no break sequence (unique behavior).
    /// Multi-byte scancodes land atomically — a half-sequence corrupts
    /// the stream — so the FIFO push either accepts all six bytes or
    /// drops the event.
    pub fn inject_pause_make(&mut self) {
        let _ = self
            .shared_state
            .enqueue_user_input_force(&[0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5]);
    }

    /// Inject Ctrl+Break make sequence: E0 46
    pub fn inject_ctrl_break_make(&mut self) {
        let _ = self.shared_state.enqueue_user_input_force(&[0xE0, 0x46]);
    }

    /// Inject Ctrl+Break break sequence: E0 C6
    pub fn inject_ctrl_break_break(&mut self) {
        let _ = self.shared_state.enqueue_user_input_force(&[0xE0, 0xC6]);
    }

    /// Inject Print Screen make sequence: E0 2A E0 37
    pub fn inject_print_screen_make(&mut self) {
        let _ = self
            .shared_state
            .enqueue_user_input_force(&[0xE0, 0x2A, 0xE0, 0x37]);
    }

    /// Inject Print Screen break sequence: E0 B7 E0 AA
    pub fn inject_print_screen_break(&mut self) {
        let _ = self
            .shared_state
            .enqueue_user_input_force(&[0xE0, 0xB7, 0xE0, 0xAA]);
    }

    /// Inject SysRq (Alt+PrintScreen) make code: 54
    pub fn inject_sysrq_make(&mut self) {
        let _ = self.shared_state.enqueue_user_input_force(&[0x54]);
    }

    /// Inject SysRq (Alt+PrintScreen) break code: D4
    pub fn inject_sysrq_break(&mut self) {
        let _ = self.shared_state.enqueue_user_input_force(&[0xD4]);
    }

    // =========================================================================
    // Typematic Timing Conversion
    // =========================================================================

    /// Convert typematic delay value (0-3) to milliseconds
    /// 0=250ms, 1=500ms, 2=750ms, 3=1000ms
    pub fn typematic_delay_ms(delay: u8) -> u32 {
        match delay & 0x03 {
            0 => 250,
            1 => 500,
            2 => 750,
            3 => 1000,
            _ => unreachable!(),
        }
    }

    /// Convert typematic rate value (0-31) to milliseconds per repeat
    /// Rate 0 = ~30 Hz (33ms), Rate 31 = ~2 Hz (500ms)
    pub fn typematic_rate_ms(rate: u8) -> u32 {
        // Standard PC keyboard rate table (approximate values in ms)
        const RATE_TABLE: [u32; 32] = [
            33, 37, 42, 46, 50, 54, 58, 63, // Rates 0-7
            67, 75, 83, 92, 100, 109, 116, 125, // Rates 8-15
            133, 149, 167, 182, 200, 217, 232, 250, // Rates 16-23
            270, 303, 333, 370, 400, 435, 470, 500, // Rates 24-31
        ];
        RATE_TABLE[(rate & 0x1F) as usize]
    }

    pub fn save_essential_state(&self) -> (Vec<(u8, u8)>, bool, u8, u8, bool) {
        (
            self.shared_state
                .queue_snapshot()
                .into_iter()
                .map(|qb| match qb {
                    QueuedByte::UserInput(b) => (b, 0u8),
                    QueuedByte::CommandResponse(b) => (b, 1u8),
                })
                .collect(),
            self.scanning_enabled,
            self.typematic_delay,
            self.typematic_rate,
            self.typematic_enabled,
        )
    }

    /// Restore essential keyboard state from checkpoint. Decodes the
    /// `(byte, tag)` pairs back into variant-tagged FIFO entries.
    /// Tag values follow `save_essential_state`: `0` = `UserInput`,
    /// `1` = `CommandResponse`.
    pub fn restore_essential_state(
        &mut self,
        scancode_queue: &[(u8, u8)],
        scanning_enabled: bool,
        typematic_delay: u8,
        typematic_rate: u8,
        typematic_enabled: bool,
    ) {
        let restored: Vec<QueuedByte> = scancode_queue
            .iter()
            .map(|&(b, tag)| match tag {
                1 => QueuedByte::CommandResponse(b),
                _ => QueuedByte::UserInput(b),
            })
            .collect();
        self.shared_state.restore_queue(&restored);
        self.scanning_enabled = scanning_enabled;
        self.update_typematic_values(typematic_delay, typematic_rate);
        self.typematic_enabled = typematic_enabled;
        self.sync_shared_mirrors();
    }

    /// Clear the keyboard buffer
    pub fn clear(&mut self) {
        self.shared_state.clear();
        self.data_register = 0;
        self.translation_buffer.clear();
        self.translate_e0_pending = false;
        self.output_buffer = None;
        self.disabled_aux_polled_response_budget = 0;
        self.stale_aux_response_keyboard_block_attempts = 0;
        self.keyboard_transfer_pending = false;
        self.keyboard_reset_command_pending = false;
        self.pending_controller_reply = None;
    }

    pub fn ascii_for_scancode_set1(make_scancode: u8, shift_flags: u8) -> u8 {
        let shifted = (shift_flags & SHIFT_FLAG_SHIFT) != 0;
        let ctrl = (shift_flags & SHIFT_FLAG_CTRL) != 0;
        let alt = (shift_flags & SHIFT_FLAG_ALT) != 0;
        let caps_lock = (shift_flags & SHIFT_FLAG_CAPS_LOCK) != 0;
        let num_lock = (shift_flags & SHIFT_FLAG_NUM_LOCK) != 0;
        let idx = make_scancode as usize;
        if idx >= 128 {
            return 0;
        }

        // Real PC BIOS INT 09h forces AL=0 for any Alt-modified key. The Alt
        // bit takes precedence over Shift, Ctrl, Caps Lock, and Num Lock for
        // the AL value — Alt+Shift+m, Alt+Ctrl+m, and Alt+m all return AL=0.
        // The AH scancode handling lives in the INT 09h dispatcher (see
        // `Keyboard::alt_remapped_scancode` and its use in
        // `process_scancode_to_bda`) — this function only computes AL.
        //
        // Sole exception per AT BIOS contract (fountainware.com,
        // stanislavs.org, xi8088 scancode.inc): Alt+Space preserves AL=0x20
        // (the space character).
        if alt {
            if make_scancode == 0x39 {
                return 0x20; // Alt+Space — AT BIOS exception preserves space
            }
            return 0;
        }

        // Real PC BIOS INT 09h applies Ctrl-modified ASCII for letters and a
        // handful of punctuation. Ctrl-letter combos produce ASCII control
        // codes 0x01..0x1A (e.g., Ctrl-A=0x01, Ctrl-K=0x0B, Ctrl-Z=0x1A) and
        // support program shortcuts and control input. Ignoring this flag caused
        // Ctrl-letter combinations to deliver the plain letter ASCII to the
        // guest, leaving menu shortcuts unreachable.
        if ctrl {
            return ctrl_ascii_for_scancode(make_scancode);
        }

        let mut ascii = if shifted {
            SCANCODE_TO_ASCII_SHIFT[idx]
        } else {
            SCANCODE_TO_ASCII[idx]
        };

        let is_keypad = matches!(
            make_scancode,
            0x47 | 0x48 | 0x49 | 0x4B | 0x4C | 0x4D | 0x4F | 0x50 | 0x51 | 0x52 | 0x53
        );
        if is_keypad && (!num_lock || shifted) {
            return 0;
        }

        if caps_lock && ascii.is_ascii_alphabetic() {
            if ascii.is_ascii_lowercase() {
                ascii = ascii.to_ascii_uppercase();
            } else {
                ascii = ascii.to_ascii_lowercase();
            }
        }

        ascii
    }

    /// Translate a Set 1 make code (possibly E0-prefixed) to ASCII using the
    /// caller's shift_flags. See `ascii_for_scancode_set1` for the rationale
    /// behind the explicit shift_flags parameter.
    pub fn ascii_for_scancode_set1_extended(
        make_scancode: u8,
        extended: bool,
        shift_flags: u8,
    ) -> u8 {
        if extended {
            // Real PC BIOS INT 09h forces AL=0 for Alt-modified E0-prefixed
            // keys (Alt+arrow, Alt+Home, Alt+End, Alt+PgUp/PgDn,
            // Alt+Ins/Del). Without this, the 0xE0 marker fallthrough below
            // would return 0xE0 for Alt+E0-keys — wrong on AT-class BIOS.
            // The extended branch does not delegate to
            // `ascii_for_scancode_set1`, so it needs its own Alt check.
            if (shift_flags & SHIFT_FLAG_ALT) != 0 {
                return 0;
            }
            match make_scancode {
                0x1C => 0x0D, // NumPad Enter
                0x35 => 0x2F, // NumPad /
                _ => 0xE0,    // Enhanced key prefix
            }
        } else {
            Self::ascii_for_scancode_set1(make_scancode, shift_flags)
        }
    }

    /// Translate a Set-1 make-code under Alt into its BIOS-defined Alt-specific
    /// scancode, if any. Real PC BIOS substitutes the AH value in the INT 16h
    /// `(scancode, AL=0)` pair for Alt+digit, Alt+F-key, Alt+Tab, Alt+`-`,
    /// Alt+`=`, Alt+Enter (AT BIOS), and the entire E0-prefixed navigation
    /// cluster (Alt+arrow, Alt+Home/End, Alt+PgUp/PgDn, Alt+Ins/Del).
    ///
    /// For letters and "boring" keys (Space, Esc, Backspace, punctuation that
    /// doesn't have an Alt remap) AH stays as the base scancode and this
    /// function returns `None`.
    ///
    /// `extended` indicates whether the scancode arrived with an E0 prefix.
    /// E0-prefixed keys ARE remapped (Home→0x97, Up→0x98, PgUp→0x99,
    /// Left→0x9B, Right→0x9D, End→0x9F, Down→0xA0, PgDn→0xA1, Ins→0xA2,
    /// Del→0xA3) per fountainware.com BIOS key codes and stanislavs.org.
    ///
    /// Returns `None` for unmodified base scancodes — callers should fall
    /// back to the original `scan` value:
    ///
    /// ```ignore
    /// let effective_scan = Keyboard::alt_remapped_scancode(scan, extended)
    ///     .unwrap_or(scan);
    /// ```
    ///
    /// References: Ralf Brown's Interrupt List INT 16h / INT 09h "Extended
    /// ASCII" tables; IBM PC AT Technical Reference Manual `ALT_MAP`;
    /// Phoenix BIOS Programmer's Guide §5; fountainware.com BIOS Key Codes;
    /// stanislavs.org HelpPC Scan Codes; xi8088 BIOS `scancode.inc`.
    pub fn alt_remapped_scancode(scan: u8, extended: bool) -> Option<u8> {
        if extended {
            // E0-prefixed: full nav-cluster Alt remap per AT BIOS.
            return match scan {
                0x47 => Some(0x97), // Home
                0x48 => Some(0x98), // Up
                0x49 => Some(0x99), // PgUp
                0x4B => Some(0x9B), // Left
                0x4D => Some(0x9D), // Right
                0x4F => Some(0x9F), // End
                0x50 => Some(0xA0), // Down
                0x51 => Some(0xA1), // PgDn
                0x52 => Some(0xA2), // Insert
                0x53 => Some(0xA3), // Delete
                _ => None,
            };
        }
        match scan {
            // Digits 1..9 → Alt+1..Alt+9 = 0x78..0x80
            0x02 => Some(0x78),
            0x03 => Some(0x79),
            0x04 => Some(0x7A),
            0x05 => Some(0x7B),
            0x06 => Some(0x7C),
            0x07 => Some(0x7D),
            0x08 => Some(0x7E),
            0x09 => Some(0x7F),
            0x0A => Some(0x80),
            // Digit 0 → Alt+0 = 0x81
            0x0B => Some(0x81),
            // - / =  →  Alt+-  = 0x82,  Alt+=  = 0x83
            0x0C => Some(0x82),
            0x0D => Some(0x83),
            // Tab → Alt+Tab = 0xA5
            0x0F => Some(0xA5),
            // Enter → Alt+Enter = 0xA6 (AT BIOS; xi8088 XT variant uses 1CF0h)
            0x1C => Some(0xA6),
            // F1..F10 → 0x68..0x71
            0x3B => Some(0x68),
            0x3C => Some(0x69),
            0x3D => Some(0x6A),
            0x3E => Some(0x6B),
            0x3F => Some(0x6C),
            0x40 => Some(0x6D),
            0x41 => Some(0x6E),
            0x42 => Some(0x6F),
            0x43 => Some(0x70),
            0x44 => Some(0x71),
            // F11 / F12 → 0x8B / 0x8C
            0x57 => Some(0x8B),
            0x58 => Some(0x8C),
            // Letters, Space, Esc, Backspace, and everything else → base
            // scancode preserved (caller falls back to the original scan).
            _ => None,
        }
    }

    /// Read from keyboard data port (0x60) without PIC interaction.
    ///
    /// This is a convenience wrapper for callers that cannot pass `&mut Pic`
    /// (e.g. tests, BDA mirroring). It consumes the output buffer but does
    /// NOT deassert or assert any IRQ lines. Prefer `read_port60` for all
    /// guest I/O and HLE paths.
    pub fn read_data(&mut self) -> u8 {
        self.input_port_poll = None;
        // Auto-stage if the output buffer is empty (no PIC available in this
        // convenience path, so we skip IRQ signalling).
        if self.output_buffer.is_none() {
            self.auto_stage_no_pic();
        }
        let consumed = self.output_buffer.take();
        self.last_port60_read_source = consumed.map(|ob| ob.source);
        let value = consumed.map_or(self.data_register, |ob| {
            self.data_register = ob.value;
            self.data_register_source_was_mouse = ob.source.is_mouse();
            self.stale_aux_response_keyboard_block_attempts = 0;
            ob.value
        });
        // Stage the next byte for the following read.
        if self.output_buffer.is_none() {
            self.auto_stage_no_pic();
        }
        self.note_port60_read();
        value
    }

    /// Internal helper: stage the next available byte into output_buffer
    /// without asserting IRQ (used by the no-PIC read_data convenience path).
    fn auto_stage_no_pic(&mut self) {
        if self.output_buffer.is_some() {
            let command_reply_pending = self.pending_controller_reply.is_some()
                || self.shared_state.has_pending_command_response();
            if command_reply_pending {
                let _ = self.discard_staged_aux_response("auto_stage_no_pic_command_reply");
            }
        }
        if self.output_buffer.is_some() {
            return;
        }
        if self.stage_disabled_aux_polled_response_no_pic("auto_stage_no_pic_disabled_aux") {
            return;
        }
        let last_was_mouse = self.data_register_source_was_mouse;
        let kbd_pending = self.has_pending_keyboard_data();
        let mouse_response_pending = self.mouse.front_output_is_command_response();
        let mouse_pending = self.mouse.has_output();
        let try_mouse_first = if mouse_response_pending {
            true
        } else if kbd_pending && mouse_pending {
            !last_was_mouse
        } else {
            mouse_pending
        };
        let ob = if try_mouse_first {
            self.try_stage_mouse().or_else(|| self.try_stage_keyboard())
        } else {
            self.try_stage_keyboard().or_else(|| self.try_stage_mouse())
        };
        if let Some(ob) = ob {
            self.data_register = ob.value;
            self.stale_aux_response_keyboard_block_attempts = 0;
            if crate::kbd_flow_trace_enabled() {
                tracing::info!(
                    target: "gp64::kbd::flow",
                    byte = format_args!("{:02X}", ob.value),
                    source = ?ob.source,
                    site = "auto_stage_no_pic",
                    "OBF_FILL"
                );
            }
            self.output_buffer = Some(ob);
        }
    }

    /// Read from keyboard status port (0x64)
    pub fn read_status(&mut self) -> u8 {
        // 8042 status register:
        // bit0: output buffer full (data available at port 0x60)
        // bit1: input buffer full (CPU has written, not yet processed)
        // bit2: system flag (set after successful self-test; BIOS expects this = 1)
        // bit5: mouse output buffer full (1 = data is from mouse, 0 = from keyboard)
        self.stage_polled_user_input_no_irq("read_status_poll");
        let status = self.status_snapshot();
        if self.input_buffer_full {
            self.input_buffer_full = false; // Clear on next status read
        }

        status
    }

    fn status_snapshot(&self) -> u8 {
        let mut status = 0x04;

        if self.input_buffer_full {
            status |= 0x02;
        }

        if self.is_keyboard_disabled() {
            status |= 0x10;
        }

        if self.output_buffer.is_some() {
            status |= 0x01;
        }

        if let Some(ob) = &self.output_buffer {
            if ob.source.is_mouse() {
                status |= 0x20;
            }
        }

        if let Some(poll_value) = self.input_port_poll {
            status = (status & 0x0F) | (poll_value << 4);
        }

        status
    }

    /// Read from keyboard control port (0x61) - speaker/misc
    pub fn read_control(&mut self) -> u8 {
        // Historically, port 0x61 is the XT/AT PPI Port B. Keyboard interrupt handlers often
        // pulse bit7 via OUT 61h, and some code uses bit4 toggling as a cheap delay source.
        //
        // We model it as a simple latch with a synthetic bit4 toggle on each read.
        self.port61_refresh = !self.port61_refresh;
        let mut v = self.port61_latch;
        if self.port61_refresh {
            v |= 0x10;
        } else {
            v &= !0x10;
        }
        v
    }

    /// Write to keyboard data port (0x60)
    ///
    /// This handles three cases:
    /// 1. Data for a pending 8042 controller command (e.g., write controller RAM)
    /// 2. Data for mouse (after 0xD4 command)
    /// 3. Commands sent directly to the keyboard (e.g., LED control, identification)
    pub fn write_data(&mut self, value: u8) {
        self.input_buffer_full = true;
        // Check if data should go to the mouse (0xD4 command was sent)
        if self.mouse_write_pending {
            self.mouse_write_pending = false;
            self.disabled_aux_polled_response_budget = 0;
            if self.is_mouse_disabled() {
                self.mouse.discard_pending_command_responses();
                self.mouse.handle_command(value);
                self.disabled_aux_polled_response_budget = self.mouse.command_response_count();
                self.stage_disabled_aux_polled_response_no_pic("write_data_disabled_aux");
            } else {
                self.mouse.handle_command(value);
                // Stage the mouse's reply byte into OBF immediately so the host
                // can poll for it via port 0x64 / read via port 0x60 without
                // waiting for a transfer event. A polling PS/2 command waits
                // for OBF after sending the command; without this, the reply queued by
                // `mouse.handle_command` never surfaces and the host times out
                // (e.g. mouse RESET / 0xF2 IDENTIFY / 0xF4 ENABLE all fail).
                // Mirrors the controller-command path further down.
                self.auto_stage_no_pic();
            }
            tracing::debug!(
                value = format_args!("{:02X}", value),
                "8042: Write to mouse device"
            );

            return;
        }

        self.disabled_aux_polled_response_budget = 0;

        // Check if there's a pending controller command that expects data
        if let Some(cmd) = self.pending_command.take() {
            match cmd {
                PendingCommand::Controller(cmd) => match cmd {
                    // 0x60-0x7F: Write controller RAM byte
                    0x60..=0x7F => {
                        let prev_irq1_enabled = self.irq1_enabled();
                        let prev_irq12_enabled = self.irq12_enabled();
                        let index = (cmd - 0x60) as usize;
                        debug_assert!(index < self.controller_ram.len());
                        self.controller_ram[index] = value;
                        if index == 0 {
                            let kbd_enabled = (value & 0x10) == 0;
                            self.shared_state.set_keyboard_enabled(kbd_enabled);
                            // If KBDINT (bit 0) was just re-enabled and
                            // bytes are queued in the FIFO, kick the
                            // main-loop trigger by re-arming irq1_pending.
                            // Without this, deferred user-input bytes from
                            // the just-closed KBDINT-disabled window never
                            // reach the guest — the trigger at timing.rs only fires on a
                            // fresh irq1_pending edge.
                            if !prev_irq1_enabled
                                && self.irq1_enabled()
                                && self.shared_state.has_pending()
                            {
                                self.shared_state.set_irq1_pending();
                            }
                            // Symmetric mouse path: AUXINT (bit 1) 0→1 with
                            // queued mouse bytes => re-arm IRQ12 so the
                            // emulator main loop schedules its delivery.
                            if !prev_irq12_enabled
                                && self.irq12_enabled()
                                && self.mouse.has_packet_output()
                            {
                                if let Some(shared_mouse) = self.shared_mouse_state.as_ref() {
                                    shared_mouse.set_irq12_pending();
                                }
                            }
                        }
                        tracing::debug!(
                            index = format_args!("{:02X}", index),
                            value = format_args!("{:02X}", value),
                            "8042: Write controller RAM"
                        );
                    }
                    // 0xD1: Write output port (bit 0 reset, bit 1 A20 gate)
                    0xD1 => {
                        let reset_requested = (value & 0x01) == 0;
                        let a20_enabled = (value & 0x02) != 0;
                        self.output_port = value;
                        self.pending_a20 = Some(a20_enabled);
                        if reset_requested {
                            self.pending_guest_reset =
                                Some(GuestResetSource::KeyboardControllerPulseOutputPort);
                        }
                        tracing::debug!(
                            value = format_args!("{:02X}", value),
                            reset = reset_requested,
                            a20 = a20_enabled,
                            "8042: Write output port"
                        );
                    }
                    // 0xD2: Write keyboard output buffer
                    0xD2 => {
                        // Inject byte as if keyboard sent it
                        self.enqueue_scancodes(&[value]);
                        tracing::debug!(
                            value = format_args!("{:02X}", value),
                            "8042: Write keyboard output buffer"
                        );
                    }
                    // 0xD3: Write mouse output buffer
                    0xD3 => {
                        // Inject byte as if mouse sent it (useful for testing)
                        self.mouse.enqueue_output(value);
                        tracing::debug!(
                            value = format_args!("{:02X}", value),
                            "8042: Write mouse output buffer"
                        );
                    }
                    _ => {
                        tracing::debug!(
                            value = format_args!("{:02X}", value),
                            cmd = format_args!("{:02X}", cmd),
                            "8042: Unexpected data for command"
                        );
                    }
                },
                PendingCommand::Keyboard(cmd) => match cmd {
                    0xED => {
                        // LED state byte; store it (only bits 0-2 are valid).
                        // Post-Stage-7 (2026-05-12) the BIOS shift_flags byte
                        // lives in the DOS driver's `DosKbdModifierState` /
                        // BDA, not in this struct, so there is no longer a
                        // hardware-side flag to sync. The DOS driver writes
                        // its lock-key bits directly to BDA, and emits 0xED
                        // separately when it wants the LED to follow.
                        self.led_state = value & 0x07;
                        self.enqueue_scancodes(&[0xFA]);
                        tracing::debug!(
                            value = format_args!("{:02X}", value),
                            "Keyboard: Set LEDs data"
                        );
                    }
                    0xF3 => {
                        self.update_typematic_values((value >> 5) & 0x03, value & 0x1F);
                        self.enqueue_scancodes(&[0xFA]);
                        tracing::debug!(
                            value = format_args!("{:02X}", value),
                            "Keyboard: Set typematic data"
                        );
                    }
                    0xF0 => {
                        // Set or query scancode set (0 = query).
                        self.enqueue_scancodes(&[0xFA]);
                        let requested = value & 0x03;
                        if requested == 0 {
                            self.enqueue_scancodes(&[self.scancode_set]);
                        } else {
                            self.scancode_set = requested;
                            self.translation_buffer.clear();
                            self.translate_e0_pending = false;
                        }
                        tracing::debug!(
                            set = format_args!("{:02X}", requested),
                            "Keyboard: Set scancode set"
                        );
                    }
                    _ => {
                        tracing::debug!(
                            value = format_args!("{:02X}", value),
                            cmd = format_args!("{:02X}", cmd),
                            "Keyboard: Unexpected data for command"
                        );
                    }
                },
            }
            // Stage command responses immediately so they are readable
            // via port 0x60 without waiting for a transfer event.
            self.auto_stage_no_pic();

            return;
        }

        // Otherwise, this is a command to the keyboard itself
        match value {
            // 0xED: Set/reset LEDs (next byte is LED state)
            0xED => {
                // ACK and wait for LED byte
                self.enqueue_scancodes(&[0xFA]); // ACK
                self.pending_command = Some(PendingCommand::Keyboard(0xED));
                tracing::debug!("Keyboard: Set LEDs command, waiting for data");
            }
            // 0xEE: Echo - keyboard echoes back 0xEE
            0xEE => {
                self.enqueue_scancodes(&[0xEE]);
                tracing::debug!("Keyboard: Echo command -> 0xEE");
            }
            // 0xF2: Read keyboard ID
            0xF2 => {
                // Return ACK, then keyboard ID (0xAB 0x83 for MF2 keyboard)
                self.enqueue_scancodes(&[0xFA, 0xAB, 0x83]);
                tracing::debug!("Keyboard: Read ID -> FA AB 83");
            }
            // 0xF3: Set typematic rate/delay (next byte is rate)
            0xF3 => {
                self.enqueue_scancodes(&[0xFA]); // ACK
                self.pending_command = Some(PendingCommand::Keyboard(0xF3));
                tracing::debug!("Keyboard: Set typematic, waiting for rate byte");
            }
            // 0xF0: Set scancode set (next byte selects set or queries current set)
            0xF0 => {
                self.enqueue_scancodes(&[0xFA]); // ACK
                self.pending_command = Some(PendingCommand::Keyboard(0xF0));
                tracing::debug!("Keyboard: Set scancode set, waiting for data");
            }
            // 0xF4: Enable scanning
            0xF4 => {
                self.set_scanning_enabled(true);
                self.enqueue_scancodes(&[0xFA]); // ACK
                tracing::debug!("Keyboard: Enable scanning");
            }
            // 0xF5: Disable scanning
            0xF5 => {
                self.set_scanning_enabled(false);
                self.enqueue_scancodes(&[0xFA]); // ACK
                tracing::debug!("Keyboard: Disable scanning");
            }
            // 0xF6: Set defaults
            0xF6 => {
                self.set_scanning_enabled(true);
                self.translation_buffer.clear();
                self.translate_e0_pending = false;
                // PS/2 spec: 0xF6 restores default settings (power-on defaults)
                self.scancode_set = 2;
                self.update_typematic_values(1, 0x0B); // 500ms delay, ~10.9 Hz (IBM AT defaults)
                self.typematic_enabled = true;
                self.enqueue_scancodes(&[0xFA]); // ACK
                tracing::debug!("Keyboard: Set defaults");
            }
            // 0xFF: Reset keyboard
            0xFF => {
                self.set_scanning_enabled(true);
                self.translation_buffer.clear();
                self.translate_e0_pending = false;
                // PS/2 spec: 0xFF resets to power-on defaults
                self.scancode_set = 2;
                self.led_state = 0;
                self.update_typematic_values(1, 0x0B); // 500ms delay, ~10.9 Hz (IBM AT defaults)
                self.typematic_enabled = true;
                if self.keyboard_event_driven {
                    // Event-driven path: ACK immediately, defer self-test response.
                    // The emulator schedules KeyboardReset at +400ms, which sends 0xAA.
                    self.enqueue_scancodes(&[0xFA]);
                    self.keyboard_reset_command_pending = true;
                    tracing::debug!("Keyboard: Reset -> FA (event-driven, deferred 0xAA)");
                } else {
                    // Synchronous path: ACK + self-test passed immediately
                    self.enqueue_scancodes(&[0xFA, 0xAA]);
                    tracing::debug!("Keyboard: Reset -> FA AA");
                }
            }
            // 0xF7..=0xFD are scan-code-set-3 key-type commands on MF2 keyboards.
            // The emulator does not model per-key type state, but these are valid
            // inputs and should acknowledge rather than fail.
            0xF7..=0xFD => {
                self.enqueue_scancodes(&[0xFA]);
                tracing::debug!(
                    cmd = format_args!("{:02X}", value),
                    "Keyboard: Set-3 key-type command stub -> ACK"
                );
            }
            0xFE => {
                let resend = self.data_register;
                self.enqueue_scancodes(&[resend]);
                tracing::debug!(
                    value = format_args!("{:02X}", resend),
                    "Keyboard: Resend previous output"
                );
            }
            0xEF | 0xF1 | 0x00..=0xEC => {
                self.enqueue_scancodes(&[0xFE]);
                tracing::debug!(
                    cmd = format_args!("{:02X}", value),
                    "Keyboard: Invalid command -> RESEND"
                );
            }
        }

        // Command responses (ACK etc.) must be immediately visible in the
        // output buffer — the guest polls port 60h right after sending a
        // command.  Stage through the shared no-PIC path (keyboard ACKs don't
        // assert IRQ).
        self.auto_stage_no_pic();
    }

    /// Write to keyboard control port (0x61) - speaker/misc
    pub fn write_control(&mut self, value: u8) {
        // Store as a latch; we don't emulate speaker/timer2 yet.
        // Programs that pulse bit7 (keyboard ACK/strobe) will at least observe consistent reads.
        self.port61_latch = value;
    }

    /// Write to keyboard command port (0x64)
    ///
    /// This handles 8042 controller commands (not keyboard commands).
    /// Key commands:
    /// - 0x20-0x3F: Read controller RAM
    /// - 0x60-0x7F: Write controller RAM (data follows via port 0x60)
    /// - 0xAA: Controller self-test
    /// - 0xAB: Keyboard interface test
    /// - 0xAD/0xAE: Disable/enable keyboard
    /// - 0xD1: Write output port (next byte via 0x60)
    /// - 0xD2: Write keyboard output buffer (next byte via 0x60)
    pub fn write_command(&mut self, value: u8) {
        self.disabled_aux_polled_response_budget = 0;
        self.input_buffer_full = true;
        match value {
            // 0x20-0x3F: Read controller RAM
            0x20..=0x3F => {
                let index = (value - 0x20) as usize;
                let data = self.controller_ram[index];
                self.pending_controller_reply = Some(data);
                tracing::debug!(
                    index = format_args!("{:02X}", index),
                    data = format_args!("{:02X}", data),
                    "8042: Read controller RAM"
                );
            }
            // 0x60-0x7F: Write controller RAM (next byte from port 0x60)
            0x60..=0x7F => {
                self.pending_command = Some(PendingCommand::Controller(value));
                tracing::debug!(
                    index = format_args!("{:02X}", value - 0x60),
                    "8042: Write controller RAM, waiting for data"
                );
            }
            // 0xA7: Disable auxiliary (mouse) device
            0xA7 => {
                // Set bit 5 of command byte (disable mouse)
                self.controller_ram[0] |= 0x20;
                tracing::debug!("8042: Disable mouse port");
            }
            // 0xA8: Enable auxiliary (mouse) device
            0xA8 => {
                // Clear bit 5 of command byte (enable mouse)
                self.controller_ram[0] &= !0x20;
                tracing::debug!("8042: Enable mouse port");
            }
            // 0xA9: Test auxiliary (mouse) port
            0xA9 => {
                // Return 0x00 = no error
                self.pending_controller_reply = Some(0x00);
                tracing::debug!("8042: Test mouse port -> 00 (OK)");
            }
            // 0xAA: Controller self-test
            0xAA => {
                // Return 0x55 = self-test passed
                // Clear input port poll mode (self-test resets controller state)
                self.input_port_poll = None;
                self.pending_controller_reply = Some(0x55);
                tracing::debug!("8042: Self-test -> 55 (PASS)");
            }
            // 0xAB: Keyboard interface test
            0xAB => {
                // Return 0x00 = no error
                self.pending_controller_reply = Some(0x00);
                tracing::debug!("8042: Interface test -> 00 (OK)");
            }
            // 0xAD: Disable keyboard
            0xAD => {
                // Set bit 4 of command byte (disable keyboard interface)
                self.controller_ram[0] |= 0x10;
                // Sync to SharedKeyboardState to block scancodes at injection time
                self.shared_state.set_keyboard_enabled(false);
                tracing::debug!("8042: Disable keyboard");
            }
            // 0xAE: Enable keyboard
            0xAE => {
                // Clear bit 4 of command byte (enable keyboard interface)
                self.controller_ram[0] &= !0x10;
                // Sync to SharedKeyboardState to allow scancodes
                self.shared_state.set_keyboard_enabled(true);
                if self.irq1_enabled() && self.shared_state.pending_user_input_count() > 0 {
                    self.shared_state.set_irq1_pending();
                }
                tracing::debug!("8042: Enable keyboard");
            }
            // 0xC0: Read input port
            0xC0 => {
                // Place input port value in output buffer
                self.pending_controller_reply = Some(self.input_port);
                tracing::debug!(
                    value = format_args!("{:02X}", self.input_port),
                    "8042: Read input port"
                );
            }
            // 0xC1: Copy input port low nibble to status bits 4-7
            0xC1 => {
                // Continuous poll mode - low nibble of input port goes to status bits 4-7
                self.input_port_poll = Some(self.input_port & 0x0F);
                tracing::debug!(
                    nibble = format_args!("{:X}", self.input_port & 0x0F),
                    "8042: Input port poll low nibble"
                );
            }
            // 0xC2: Copy input port high nibble to status bits 4-7
            0xC2 => {
                // Continuous poll mode - high nibble of input port goes to status bits 4-7
                self.input_port_poll = Some((self.input_port >> 4) & 0x0F);
                tracing::debug!(
                    nibble = format_args!("{:X}", (self.input_port >> 4) & 0x0F),
                    "8042: Input port poll high nibble"
                );
            }
            // 0xD0: Read output port
            0xD0 => {
                self.pending_controller_reply = Some(self.output_port);
                tracing::debug!(
                    value = format_args!("{:02X}", self.output_port),
                    "8042: Read output port"
                );
            }
            // 0xD1: Write output port (next byte from port 0x60)
            0xD1 => {
                self.pending_command = Some(PendingCommand::Controller(0xD1));
                tracing::debug!("8042: Write output port, waiting for data");
            }
            // 0xD2: Write keyboard output buffer (next byte from port 0x60)
            0xD2 => {
                self.pending_command = Some(PendingCommand::Controller(0xD2));
                tracing::debug!("8042: Write keyboard output buffer, waiting for data");
            }
            // 0xD3: Write mouse output buffer (for testing)
            0xD3 => {
                self.pending_command = Some(PendingCommand::Controller(0xD3));
                tracing::debug!("8042: Write mouse output buffer, waiting for data");
            }
            // 0xD4: Write to auxiliary device (mouse)
            0xD4 => {
                // Next byte written to port 0x60 goes to the mouse
                self.mouse_write_pending = true;
                tracing::debug!("8042: Write to mouse device, waiting for data");
            }
            // 0xF0-0xFF: Pulse output port lines low. Command bits 0-3
            // select which output-port bits remain high; a zero bit pulses
            // that line low. Bit 0 is the CPU reset line.
            0xF0..=0xFF => {
                let pulse_mask = (!value) & 0x0F;
                if (pulse_mask & 0x01) != 0 {
                    self.pending_guest_reset =
                        Some(GuestResetSource::KeyboardControllerPulseOutputPort);
                    tracing::debug!(
                        cmd = format_args!("{:02X}", value),
                        pulse_mask = format_args!("{:X}", pulse_mask),
                        "8042: System reset requested by output-port pulse"
                    );
                } else {
                    tracing::debug!(
                        cmd = format_args!("{:02X}", value),
                        pulse_mask = format_args!("{:X}", pulse_mask),
                        "8042: Pulse output port"
                    );
                }
            }
            _ => {
                tracing::debug!(cmd = format_args!("{:02X}", value), "8042: Unknown command");
            }
        }

        // Command responses must be immediately visible in the output buffer.
        // Only auto-stage when a reply was actually generated.
        if self.pending_controller_reply.is_some() {
            self.auto_stage_no_pic();
        }
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

/// ASCII byte produced by the BIOS INT 09h handler when Ctrl is held and
/// the given make scancode is pressed. Returns 0 for combos that have no
/// ASCII representation (only the scancode is delivered in those cases).
///
/// Reference: IBM PC AT BIOS keyboard handler. Letter keys produce ASCII
/// 0x01..0x1A (Ctrl-A..Ctrl-Z), several punctuation keys produce additional
/// control codes, and Ctrl+Backspace/Enter have specific overrides.
pub(crate) fn ctrl_ascii_for_scancode(scancode: u8) -> u8 {
    match scancode {
        // Letters A-Z mapped to control codes 0x01..0x1A
        0x10 => 0x11, // Q -> DC1
        0x11 => 0x17, // W -> ETB
        0x12 => 0x05, // E -> ENQ
        0x13 => 0x12, // R -> DC2
        0x14 => 0x14, // T -> DC4
        0x15 => 0x19, // Y -> EM
        0x16 => 0x15, // U -> NAK
        0x17 => 0x09, // I -> HT (same as Tab)
        0x18 => 0x0F, // O -> SI
        0x19 => 0x10, // P -> DLE
        0x1E => 0x01, // A -> SOH
        0x1F => 0x13, // S -> DC3
        0x20 => 0x04, // D -> EOT
        0x21 => 0x06, // F -> ACK
        0x22 => 0x07, // G -> BEL
        0x23 => 0x08, // H -> BS
        0x24 => 0x0A, // J -> LF
        0x25 => 0x0B, // K -> VT
        0x26 => 0x0C, // L -> FF
        0x2C => 0x1A, // Z -> SUB
        0x2D => 0x18, // X -> CAN
        0x2E => 0x03, // C -> ETX (Ctrl-C)
        0x2F => 0x16, // V -> SYN
        0x30 => 0x02, // B -> STX
        0x31 => 0x0E, // N -> SO
        0x32 => 0x0D, // M -> CR
        // Punctuation / specials
        0x03 => 0x00, // Ctrl+2/@  -> NUL
        0x07 => 0x1E, // Ctrl+6/^  -> RS
        0x0C => 0x1F, // Ctrl+-/_  -> US
        0x0E => 0x7F, // Ctrl+Backspace -> DEL
        0x1A => 0x1B, // Ctrl+[    -> ESC
        0x1B => 0x1D, // Ctrl+]    -> GS
        0x1C => 0x0A, // Ctrl+Enter -> LF
        0x2B => 0x1C, // Ctrl+\    -> FS
        // No ASCII delivered for other Ctrl combinations (only the scancode).
        _ => 0,
    }
}

/// Scancode to ASCII lookup table (unshifted)
pub const SCANCODE_TO_ASCII: [u8; 128] = [
    0, 27, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', b'-', b'=', 8, 9, b'q',
    b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p', b'[', b']', 13, 0, b'a', b's', b'd',
    b'f', b'g', b'h', b'j', b'k', b'l', b';', b'\'', b'`', 0, b'\\', b'z', b'x', b'c', b'v', b'b',
    b'n', b'm', b',', b'.', b'/', 0, b'*', 0, b' ', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, b'7',
    b'8', b'9', b'-', b'4', b'5', b'6', b'+', b'1', b'2', b'3', b'0', b'.', 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0,
];

/// Scancode to ASCII lookup table (shifted)
pub const SCANCODE_TO_ASCII_SHIFT: [u8; 128] = [
    0, 27, b'!', b'@', b'#', b'$', b'%', b'^', b'&', b'*', b'(', b')', b'_', b'+', 8, 9, b'Q',
    b'W', b'E', b'R', b'T', b'Y', b'U', b'I', b'O', b'P', b'{', b'}', 13, 0, b'A', b'S', b'D',
    b'F', b'G', b'H', b'J', b'K', b'L', b':', b'"', b'~', 0, b'|', b'Z', b'X', b'C', b'V', b'B',
    b'N', b'M', b'<', b'>', b'?', 0, b'*', 0, b' ', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, b'7',
    b'8', b'9', b'-', b'4', b'5', b'6', b'+', b'1', b'2', b'3', b'0', b'.', 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0,
];

// ---------------------------------------------------------------------------
// Snapshot / Restore
// ---------------------------------------------------------------------------

impl Keyboard {}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use tracing_subscriber::EnvFilter;

    #[test]
    fn stamped_host_key_pop_records_an_input_latency_sample() {
        let s = SharedKeyboardState::new();
        assert_eq!(s.take_input_latency_stats(), (0, 0, 0));

        assert_eq!(
            s.inject_scancode(&[0xE0, 0x48], 72, true),
            ScancodeInjectOutcome::Injected
        );
        // Both bytes of the E0-prefixed event become guest-visible.
        assert!(s.pop_byte(true).is_some());
        assert!(s.pop_byte(true).is_some());

        let (_sum, samples, _max) = s.take_input_latency_stats();
        assert_eq!(samples, 2, "one sample per host-injected byte");
        // The take drained the accumulator.
        assert_eq!(s.take_input_latency_stats(), (0, 0, 0));
    }

    #[test]
    fn synthetic_enqueues_record_no_latency_samples() {
        let s = SharedKeyboardState::new();
        // Force-pushed user input (typematic/IPC raw) and command responses
        // are not host key events, so neither may contribute samples.
        assert!(s.enqueue_user_input_force(&[0x1C]));
        assert!(s.enqueue_command_response(&[0xFA]));
        assert!(s.pop_byte(true).is_some());
        assert!(s.pop_byte(true).is_some());
        assert_eq!(s.take_input_latency_stats(), (0, 0, 0));
    }

    #[test]
    fn test_new() {
        let kb = Keyboard::new();
        assert_eq!(kb.data_register, 0);
        assert_eq!(kb.shared_state.pending_count(), 0);
        assert_eq!(kb.port61_latch, 0);
        assert!(!kb.port61_refresh);
    }

    #[test]
    fn test_default() {
        let kb = Keyboard::default();
        assert_eq!(kb.data_register, 0);
    }

    #[test]
    fn test_scancode_for_ascii_letters() {
        assert_eq!(Keyboard::scancode_for_ascii(b'a'), 0x1E);
        assert_eq!(Keyboard::scancode_for_ascii(b'A'), 0x1E);
        assert_eq!(Keyboard::scancode_for_ascii(b'z'), 0x2C);
        assert_eq!(Keyboard::scancode_for_ascii(b'Z'), 0x2C);
        assert_eq!(Keyboard::scancode_for_ascii(b'm'), 0x32);
    }

    #[test]
    fn test_scancode_for_ascii_numbers() {
        assert_eq!(Keyboard::scancode_for_ascii(b'0'), 0x0B);
        assert_eq!(Keyboard::scancode_for_ascii(b'1'), 0x02);
        assert_eq!(Keyboard::scancode_for_ascii(b'9'), 0x0A);
    }

    #[test]
    fn test_scancode_for_ascii_special() {
        assert_eq!(Keyboard::scancode_for_ascii(b'\r'), 0x1C);
        assert_eq!(Keyboard::scancode_for_ascii(b'\n'), 0x1C);
        assert_eq!(Keyboard::scancode_for_ascii(0x08), 0x0E); // Backspace
        assert_eq!(Keyboard::scancode_for_ascii(b'\t'), 0x0F);
        assert_eq!(Keyboard::scancode_for_ascii(b' '), 0x39);
    }

    #[test]
    #[cfg(not(coverage))]
    fn test_scancode_for_ascii_unknown() {
        assert_eq!(Keyboard::scancode_for_ascii(0xFF), 0x00);
        assert_eq!(Keyboard::scancode_for_ascii(b'@'), 0x00);
    }

    #[test]
    fn test_enqueue_scancodes() {
        let mut kb = Keyboard::new();
        kb.enqueue_scancodes(&[0x1E, 0x9E]); // 'a' make and break
        assert!(kb.has_pending_scancodes());
        // enqueue_scancodes does NOT set data_register — that's done by
        // transfer_next_scancode() (event-driven) or read_data() (polling).
        assert_eq!(kb.data_register, 0x00);
    }

    #[test]
    fn test_enqueue_scancodes_overflow() {
        let mut kb = Keyboard::new();
        // Fill queue to capacity. The unified FIFO holds 64 entries.
        for i in 0..70 {
            kb.enqueue_scancodes(&[i as u8]);
        }
        // Queue should be capped at 64
        assert_eq!(kb.shared_state.pending_count(), 64);
    }

    #[test]
    fn test_has_pending_scancodes() {
        let mut kb = Keyboard::new();
        assert!(!kb.has_pending_scancodes());
        kb.enqueue_scancodes(&[0x1E]);
        assert!(kb.has_pending_scancodes());
    }

    #[test]
    fn test_clear() {
        let mut kb = Keyboard::new();
        kb.enqueue_scancodes(&[0x1E, 0x9E]);

        kb.clear();

        assert!(!kb.has_pending_scancodes());
        assert_eq!(kb.data_register, 0);
    }

    #[test]
    fn test_ascii_for_scancode_unshifted() {
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1E, 0), b'a');
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x10, 0), b'q');
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x02, 0), b'1');
    }

    #[test]
    fn test_ascii_for_scancode_shifted() {
        let flags = SHIFT_FLAG_SHIFT;
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1E, flags), b'A');
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x10, flags), b'Q');
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x02, flags), b'!');
    }

    #[test]
    fn test_ascii_for_scancode_caps_lock() {
        let caps = SHIFT_FLAG_CAPS_LOCK;
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1E, caps), b'A');
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x02, caps), b'1');

        let caps_shift = SHIFT_FLAG_CAPS_LOCK | SHIFT_FLAG_SHIFT;
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1E, caps_shift), b'a');
    }

    #[test]
    fn test_ascii_for_scancode_num_lock() {
        let num = SHIFT_FLAG_NUM_LOCK;
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x47, num), b'7');

        let num_shift = SHIFT_FLAG_NUM_LOCK | SHIFT_FLAG_SHIFT;
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x47, num_shift), 0);

        // NumLock off, no shift: keypad keys produce no ASCII.
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x47, 0), 0);
    }

    #[test]
    fn test_ascii_for_scancode_out_of_range() {
        assert_eq!(Keyboard::ascii_for_scancode_set1(128, 0), 0);
        assert_eq!(Keyboard::ascii_for_scancode_set1(255, 0), 0);
    }

    #[test]
    fn test_ascii_for_scancode_set1_extended_paths() {
        assert_eq!(
            Keyboard::ascii_for_scancode_set1_extended(0x1C, true, 0),
            0x0D
        );
        assert_eq!(
            Keyboard::ascii_for_scancode_set1_extended(0x35, true, 0),
            0x2F
        );
        assert_eq!(
            Keyboard::ascii_for_scancode_set1_extended(0x48, true, 0),
            0xE0
        );
        assert_eq!(
            Keyboard::ascii_for_scancode_set1_extended(0x1E, false, 0),
            b'a'
        );
    }

    #[test]
    fn test_ascii_for_scancode_ctrl_letters() {
        // BIOS INT 09h: Ctrl + letter delivers ASCII control codes 0x01..0x1A.
        // Ctrl shortcuts must receive control characters instead of plain letters.
        let ctrl = SHIFT_FLAG_CTRL;

        // Spot-check the full alphabet — each letter must produce its
        // corresponding C0 control code.
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1E, ctrl), 0x01); // Ctrl-A SOH
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x30, ctrl), 0x02); // Ctrl-B STX
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x2E, ctrl), 0x03); // Ctrl-C ETX
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x20, ctrl), 0x04); // Ctrl-D EOT
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x12, ctrl), 0x05); // Ctrl-E ENQ
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x21, ctrl), 0x06); // Ctrl-F ACK
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x22, ctrl), 0x07); // Ctrl-G BEL
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x23, ctrl), 0x08); // Ctrl-H BS
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x17, ctrl), 0x09); // Ctrl-I HT
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x24, ctrl), 0x0A); // Ctrl-J LF
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x25, ctrl), 0x0B); // Ctrl-K VT
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x26, ctrl), 0x0C); // Ctrl-L FF
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x32, ctrl), 0x0D); // Ctrl-M CR
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x31, ctrl), 0x0E); // Ctrl-N SO
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x18, ctrl), 0x0F); // Ctrl-O SI
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x19, ctrl), 0x10); // Ctrl-P DLE
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x10, ctrl), 0x11); // Ctrl-Q DC1
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x13, ctrl), 0x12); // Ctrl-R DC2
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1F, ctrl), 0x13); // Ctrl-S DC3
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x14, ctrl), 0x14); // Ctrl-T DC4
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x16, ctrl), 0x15); // Ctrl-U NAK
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x2F, ctrl), 0x16); // Ctrl-V SYN
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x11, ctrl), 0x17); // Ctrl-W ETB
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x2D, ctrl), 0x18); // Ctrl-X CAN
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x15, ctrl), 0x19); // Ctrl-Y EM
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x2C, ctrl), 0x1A); // Ctrl-Z SUB
    }

    #[test]
    fn test_ascii_for_scancode_ctrl_punctuation() {
        // PC BIOS produces a small set of punctuation control codes when Ctrl
        // is held — these are the ones DOS programs commonly key off.
        let ctrl = SHIFT_FLAG_CTRL;

        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1A, ctrl), 0x1B); // Ctrl-[ ESC
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1B, ctrl), 0x1D); // Ctrl-] GS
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x2B, ctrl), 0x1C); // Ctrl-\ FS
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x07, ctrl), 0x1E); // Ctrl-6/^ RS
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x0C, ctrl), 0x1F); // Ctrl--/_ US
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x03, ctrl), 0x00); // Ctrl-2/@ NUL
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x0E, ctrl), 0x7F); // Ctrl-Backspace DEL
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1C, ctrl), 0x0A); // Ctrl-Enter LF

        // Ctrl + non-mapped key delivers no ASCII (only the scancode is
        // available to the program). Real BIOS returns 0 here.
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x39, ctrl), 0); // Ctrl-Space
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x02, ctrl), 0); // Ctrl-1
    }

    #[test]
    fn test_read_data() {
        let mut kb = Keyboard::new();
        assert!(kb.enqueue_input_scancodes(&[0x1E, 0x9E]));

        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.read_data(), 0x9E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        // After queue empty, returns last data_register value
        assert_eq!(kb.read_data(), 0x9E);
    }

    #[test]
    fn test_kbd_flow_trace_enabled_paths() {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("gp64::kbd::flow=info"))
            .with_writer(std::io::sink)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let shared = Arc::new(SharedKeyboardState::new());
        shared.inject_scancode(&[0x1E], 10, true);

        let mut kb = Keyboard::new();
        kb.set_shared_state(Arc::clone(&shared));

        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0x1E));

        kb.write_command(0xAD);
        assert!(!kb.enqueue_input_scancodes(&[0x1E]));

        kb.write_command(0xAE);
        assert!(kb.enqueue_input_scancodes(&[0x1E]));
        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0x1E));
    }

    #[test]
    fn test_read_keyboard_output_empty_and_data() {
        let mut kb = Keyboard::new();
        assert!(kb.pop_keyboard_byte().is_none());

        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        assert_eq!(kb.pop_keyboard_byte(), Some((0x1E, false)));
        assert!(kb.pop_keyboard_byte().is_none());
    }

    #[test]
    fn test_read_mouse_output_empty_and_data() {
        let mut kb = Keyboard::new();
        assert!(kb.try_stage_mouse().is_none());

        kb.mouse.enqueue_output(0x5A);
        let ob = kb.try_stage_mouse().unwrap();
        assert_eq!(ob.value, 0x5A);
        assert_eq!(ob.source, OutputSource::AuxMouse);
    }

    #[test]
    fn test_read_data_paths_with_keyboard_and_mouse() {
        // When both keyboard and mouse have data, alternation picks
        // the opposite of the last consumed source.
        let mut kb = Keyboard::new();
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        kb.mouse.enqueue_output(0x5A);
        kb.data_register_source_was_mouse = false;
        // Last was keyboard → next should be mouse
        assert_eq!(kb.read_data(), 0x5A);
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));

        let mut kb = Keyboard::new();
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        kb.mouse.enqueue_output(0x5A);
        kb.data_register_source_was_mouse = true;
        // Last was mouse → next should be keyboard
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));

        // Keyboard only
        let mut kb = Keyboard::new();
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));

        // Mouse only
        let mut kb = Keyboard::new();
        kb.mouse.enqueue_output(0x5A);
        assert_eq!(kb.read_data(), 0x5A);
    }

    #[test]
    fn test_read_status_mouse_only_sets_mouse_bit() {
        let mut kb = Keyboard::new();
        kb.mouse.enqueue_output(0x5A);
        let mut pic = Pic::new();
        // stage_output's mouse_pending gate requires IRQ12 unmasked at the
        // PIC (no driver = no listener = OBF would lock up). Slave default
        // mask is 0xFF; unmask IRQ12 (slave bit 4) to simulate a loaded
        // mouse driver so the stage proceeds.
        pic.write_data(1, 0xEF);
        kb.stage_output(&mut pic);

        let status = kb.read_status();
        assert_eq!(status & 0x01, 0x01);
        assert_eq!(status & 0x20, 0x20);
    }

    #[test]
    fn test_read_status_mouse_and_keyboard_pending() {
        let mut kb = Keyboard::new();
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        kb.mouse.enqueue_output(0x5A);
        let mut pic = Pic::new();
        // IRQ12 unmasked to model a loaded mouse driver — without it,
        // the gate in stage_output skips mouse entirely (which is fine
        // for this test, but we want both to be eligible).
        pic.write_data(1, 0xEF);
        kb.stage_output(&mut pic);

        let status = kb.read_status();
        // Output buffer full. New policy: keyboard wins when both
        // sources have data, so the staged byte is from the keyboard
        // (mouse-source bit clear).
        assert_eq!(status & 0x01, 0x01, "OBF should be full");
        assert_eq!(status & 0x20, 0x00, "staged byte should be from keyboard");

        assert_eq!(kb.read_port60(&mut pic), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
    }

    #[test]
    fn test_read_status() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        // No scancodes pending - system flag set (bit 2)
        assert_eq!(kb.read_status(), 0x04);

        // With scancode staged into output buffer - output buffer full (bit 0)
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        kb.stage_output(&mut pic);
        assert_eq!(kb.read_status(), 0x05);

        assert_eq!(kb.read_port60(&mut pic), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
    }

    #[test]
    fn test_read_status_with_input_port_poll() {
        let mut kb = Keyboard::new();
        kb.input_port = 0xA5;

        kb.write_command(0xC1);
        assert_eq!(kb.read_status() & 0xF0, 0x50);

        kb.write_command(0xC2);
        assert_eq!(kb.read_status() & 0xF0, 0xA0);
    }

    #[test]
    fn test_read_control_toggle() {
        let mut kb = Keyboard::new();

        // First read should have bit 4 toggled
        let v1 = kb.read_control();
        let v2 = kb.read_control();

        // Bit 4 should toggle between reads
        assert_ne!(v1 & 0x10, v2 & 0x10);
    }

    #[test]
    fn test_write_control() {
        let mut kb = Keyboard::new();
        kb.write_control(0xAB);

        // Should be latched (minus the toggle bit)
        let v = kb.read_control();
        assert_eq!(v & 0xEF, 0xAB & 0xEF);
    }

    #[test]
    fn test_write_data() {
        let mut kb = Keyboard::new();
        kb.write_data(0xED); // Should not crash (stub)
    }

    #[test]
    fn test_write_command() {
        let mut kb = Keyboard::new();
        kb.write_command(0xAD); // Should not crash (stub)
    }

    #[test]
    fn test_scancode_tables_length() {
        assert_eq!(SCANCODE_TO_ASCII.len(), 128);
        assert_eq!(SCANCODE_TO_ASCII_SHIFT.len(), 128);
    }

    #[test]
    fn test_scancode_table_values() {
        // Check some specific values in unshifted table
        assert_eq!(SCANCODE_TO_ASCII[0x1E], b'a');
        assert_eq!(SCANCODE_TO_ASCII[0x1C], 13); // Enter
        assert_eq!(SCANCODE_TO_ASCII[0x39], b' '); // Space

        // Check shifted table
        assert_eq!(SCANCODE_TO_ASCII_SHIFT[0x1E], b'A');
        assert_eq!(SCANCODE_TO_ASCII_SHIFT[0x02], b'!');
    }

    #[test]
    fn test_scancode_for_ascii_all_letters() {
        // Test all lowercase letters
        assert_eq!(Keyboard::scancode_for_ascii(b'b'), 0x30);
        assert_eq!(Keyboard::scancode_for_ascii(b'c'), 0x2E);
        assert_eq!(Keyboard::scancode_for_ascii(b'd'), 0x20);
        assert_eq!(Keyboard::scancode_for_ascii(b'e'), 0x12);
        assert_eq!(Keyboard::scancode_for_ascii(b'f'), 0x21);
        assert_eq!(Keyboard::scancode_for_ascii(b'g'), 0x22);
        assert_eq!(Keyboard::scancode_for_ascii(b'h'), 0x23);
        assert_eq!(Keyboard::scancode_for_ascii(b'i'), 0x17);
        assert_eq!(Keyboard::scancode_for_ascii(b'j'), 0x24);
        assert_eq!(Keyboard::scancode_for_ascii(b'k'), 0x25);
        assert_eq!(Keyboard::scancode_for_ascii(b'l'), 0x26);
        assert_eq!(Keyboard::scancode_for_ascii(b'n'), 0x31);
        assert_eq!(Keyboard::scancode_for_ascii(b'o'), 0x18);
        assert_eq!(Keyboard::scancode_for_ascii(b'p'), 0x19);
        assert_eq!(Keyboard::scancode_for_ascii(b'q'), 0x10);
        assert_eq!(Keyboard::scancode_for_ascii(b'r'), 0x13);
        assert_eq!(Keyboard::scancode_for_ascii(b's'), 0x1F);
        assert_eq!(Keyboard::scancode_for_ascii(b't'), 0x14);
        assert_eq!(Keyboard::scancode_for_ascii(b'u'), 0x16);
        assert_eq!(Keyboard::scancode_for_ascii(b'v'), 0x2F);
        assert_eq!(Keyboard::scancode_for_ascii(b'w'), 0x11);
        assert_eq!(Keyboard::scancode_for_ascii(b'x'), 0x2D);
        assert_eq!(Keyboard::scancode_for_ascii(b'y'), 0x15);

        // Test uppercase equivalents
        assert_eq!(Keyboard::scancode_for_ascii(b'B'), 0x30);
        assert_eq!(Keyboard::scancode_for_ascii(b'C'), 0x2E);
        assert_eq!(Keyboard::scancode_for_ascii(b'D'), 0x20);
        assert_eq!(Keyboard::scancode_for_ascii(b'E'), 0x12);
        assert_eq!(Keyboard::scancode_for_ascii(b'F'), 0x21);
        assert_eq!(Keyboard::scancode_for_ascii(b'G'), 0x22);
        assert_eq!(Keyboard::scancode_for_ascii(b'H'), 0x23);
        assert_eq!(Keyboard::scancode_for_ascii(b'I'), 0x17);
        assert_eq!(Keyboard::scancode_for_ascii(b'J'), 0x24);
        assert_eq!(Keyboard::scancode_for_ascii(b'K'), 0x25);
        assert_eq!(Keyboard::scancode_for_ascii(b'L'), 0x26);
        assert_eq!(Keyboard::scancode_for_ascii(b'N'), 0x31);
        assert_eq!(Keyboard::scancode_for_ascii(b'O'), 0x18);
        assert_eq!(Keyboard::scancode_for_ascii(b'P'), 0x19);
        assert_eq!(Keyboard::scancode_for_ascii(b'Q'), 0x10);
        assert_eq!(Keyboard::scancode_for_ascii(b'R'), 0x13);
        assert_eq!(Keyboard::scancode_for_ascii(b'S'), 0x1F);
        assert_eq!(Keyboard::scancode_for_ascii(b'T'), 0x14);
        assert_eq!(Keyboard::scancode_for_ascii(b'U'), 0x16);
        assert_eq!(Keyboard::scancode_for_ascii(b'V'), 0x2F);
        assert_eq!(Keyboard::scancode_for_ascii(b'W'), 0x11);
        assert_eq!(Keyboard::scancode_for_ascii(b'X'), 0x2D);
        assert_eq!(Keyboard::scancode_for_ascii(b'Y'), 0x15);
    }

    #[test]
    fn test_scancode_for_ascii_all_numbers() {
        assert_eq!(Keyboard::scancode_for_ascii(b'2'), 0x03);
        assert_eq!(Keyboard::scancode_for_ascii(b'3'), 0x04);
        assert_eq!(Keyboard::scancode_for_ascii(b'4'), 0x05);
        assert_eq!(Keyboard::scancode_for_ascii(b'5'), 0x06);
        assert_eq!(Keyboard::scancode_for_ascii(b'6'), 0x07);
        assert_eq!(Keyboard::scancode_for_ascii(b'7'), 0x08);
        assert_eq!(Keyboard::scancode_for_ascii(b'8'), 0x09);
    }

    #[test]
    fn test_ascii_for_scancode_more_values() {
        // Test more scancodes for coverage (no modifiers).
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x00, 0), 0); // Null
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x01, 0), 27); // Escape
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x0E, 0), 8); // Backspace
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x0F, 0), 9); // Tab
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x29, 0), b'`'); // Backquote
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x2B, 0), b'\\'); // Backslash
    }

    #[test]
    fn test_ascii_for_scancode_shifted_more() {
        let flags = SHIFT_FLAG_SHIFT;

        assert_eq!(Keyboard::ascii_for_scancode_set1(0x03, flags), b'@'); // 2 -> @
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x04, flags), b'#'); // 3 -> #
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x05, flags), b'$'); // 4 -> $
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x06, flags), b'%'); // 5 -> %
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x07, flags), b'^'); // 6 -> ^
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x08, flags), b'&'); // 7 -> &
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x09, flags), b'*'); // 8 -> *
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x0A, flags), b'('); // 9 -> (
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x0B, flags), b')'); // 0 -> )
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x0C, flags), b'_'); // - -> _
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x0D, flags), b'+'); // = -> +
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1A, flags), b'{'); // [ -> {
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x1B, flags), b'}'); // ] -> }
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x27, flags), b':'); // ; -> :
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x28, flags), b'"'); // ' -> "
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x29, flags), b'~'); // ` -> ~
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x2B, flags), b'|'); // \ -> |
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x33, flags), b'<'); // , -> <
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x34, flags), b'>'); // . -> >
        assert_eq!(Keyboard::ascii_for_scancode_set1(0x35, flags), b'?'); // / -> ?
    }

    #[test]
    fn test_read_data_empty_queue() {
        let mut kb = Keyboard::new();

        // Set data_register manually by enqueuing and reading
        kb.enqueue_scancodes(&[0x42]);
        assert_eq!(kb.read_data(), 0x42);

        // Now queue is empty, should return last data_register
        assert_eq!(kb.read_data(), 0x42);
        assert_eq!(kb.read_data(), 0x42);
    }

    #[test]
    fn test_read_status_after_drain() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        assert!(kb.enqueue_input_scancodes(&[0x1E, 0x9E]));

        kb.stage_output(&mut pic);
        assert_eq!(kb.read_status() & 0x01, 0x01); // Data available

        assert_eq!(kb.read_data(), 0x1E); // Consumes 0x1E, auto-stages 0x9E
        assert_eq!(kb.read_status() & 0x01, 0x01); // Still have one

        assert_eq!(kb.read_data(), 0x9E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        assert_eq!(kb.read_status() & 0x01, 0x00); // Queue empty
    }

    #[test]
    fn test_read_control_multiple_toggles() {
        let mut kb = Keyboard::new();

        // Test multiple read cycles to verify toggle pattern
        let v1 = kb.read_control();
        let v2 = kb.read_control();
        let v3 = kb.read_control();
        let v4 = kb.read_control();

        // Should toggle: v1 != v2, v2 != v3, v3 != v4
        assert_ne!(v1 & 0x10, v2 & 0x10);
        assert_ne!(v2 & 0x10, v3 & 0x10);
        assert_ne!(v3 & 0x10, v4 & 0x10);

        // v1 == v3, v2 == v4 (same toggle phase)
        assert_eq!(v1 & 0x10, v3 & 0x10);
        assert_eq!(v2 & 0x10, v4 & 0x10);
    }

    #[test]
    fn test_write_control_preserves_bits() {
        let mut kb = Keyboard::new();

        // Write various values and verify latch
        kb.write_control(0xFF);
        let v = kb.read_control();
        assert_eq!(v & 0xEF, 0xEF); // Bit 4 toggles

        kb.write_control(0x00);
        let v = kb.read_control();
        assert_eq!(v & 0xEF, 0x00);
    }

    // ========================================================================
    // InjectedKeySet Tests
    // ========================================================================

    #[test]
    fn test_injected_key_set_new() {
        let set = InjectedKeySet::new();
        // All bits should be zero initially
        assert!(!set.check_and_clear(0));
        assert!(!set.check_and_clear(63));
        assert!(!set.check_and_clear(64));
        assert!(!set.check_and_clear(127));
    }

    #[test]
    fn test_injected_key_set_mark_and_check() {
        let set = InjectedKeySet::new();

        // Mark key 10
        set.mark(10);
        // First check should return true and clear
        assert!(set.check_and_clear(10));
        // Second check should return false (already cleared)
        assert!(!set.check_and_clear(10));
    }

    #[test]
    fn test_injected_key_set_multiple_keys() {
        let set = InjectedKeySet::new();

        // Mark multiple keys in different words
        set.mark(5);
        set.mark(65);
        set.mark(100);

        // Check they're all marked
        assert!(set.check_and_clear(5));
        assert!(set.check_and_clear(65));
        assert!(set.check_and_clear(100));

        // All should now be cleared
        assert!(!set.check_and_clear(5));
        assert!(!set.check_and_clear(65));
        assert!(!set.check_and_clear(100));
    }

    #[test]
    fn test_injected_key_set_out_of_range() {
        let set = InjectedKeySet::new();

        // Keys >= 128 should be safely ignored
        set.mark(200);
        assert!(!set.check_and_clear(200));
    }

    #[test]
    fn test_injected_key_set_clear_all() {
        let set = InjectedKeySet::new();

        // Mark several keys
        set.mark(1);
        set.mark(50);
        set.mark(70);
        set.mark(120);

        // Clear all
        set.clear_all();

        // All should be cleared
        assert!(!set.check_and_clear(1));
        assert!(!set.check_and_clear(50));
        assert!(!set.check_and_clear(70));
        assert!(!set.check_and_clear(120));
    }

    // ========================================================================
    // SharedKeyboardState Tests
    // ========================================================================

    #[test]
    fn test_shared_keyboard_state_new() {
        let state = SharedKeyboardState::new();
        assert!(!state.has_pending());
        assert_eq!(state.pending_count(), 0);
        assert!(!state.take_irq1_pending());
        assert_eq!(state.get_modifier_flags(), 0);
    }

    #[test]
    fn test_shared_keyboard_state_inject_single() {
        let state = SharedKeyboardState::new();

        // Inject a single scancode
        state.inject_scancode(&[0x1E], 10, true); // 'A' key press

        // Should have pending data
        assert!(state.has_pending());
        assert_eq!(state.pending_count(), 1);

        // IRQ1 should be pending
        assert!(state.take_irq1_pending());
        assert!(!state.take_irq1_pending()); // Should be cleared now

        // Key should be marked as injected
        assert!(state.was_injected(10));
        assert!(!state.was_injected(10)); // Should be cleared after check
    }

    #[test]
    fn test_shared_keyboard_state_inject_extended() {
        let state = SharedKeyboardState::new();

        // Inject an extended scancode (e.g., arrow key)
        state.inject_scancode(&[0xE0, 0x48], 20, true); // Up arrow

        // Should have 2 pending bytes
        assert!(state.has_pending());
        assert_eq!(state.pending_count(), 2);

        // Read both bytes
        assert_eq!(state.pop_byte(true).map(|b| b.value()), Some(0xE0));
        assert_eq!(state.pop_byte(true).map(|b| b.value()), Some(0x48));
        assert_eq!(state.pop_byte(true).map(|b| b.value()), None);
    }

    #[test]
    fn test_shared_keyboard_state_inject_extended_drops_if_insufficient_space() {
        let state = SharedKeyboardState::new();

        for _ in 0..63 {
            state.inject_scancode(&[0x1E], 10, true);
        }
        assert_eq!(state.pending_count(), 63);

        state.inject_scancode(&[0xE0, 0x48], 20, true);

        assert_eq!(state.pending_count(), 63);
    }

    #[test]
    fn test_shared_keyboard_state_queue_full_warning_branches() {
        let state = SharedKeyboardState::new();
        let bytes = [0x1E; 64];

        state.inject_scancode(&bytes, 0, true);
        assert_eq!(state.pending_count(), 64);

        state.queue_full_warnings.store(0, Ordering::Relaxed);
        state.inject_scancode(&[0x30], 1, true);
        assert_eq!(state.pending_count(), 64);
        assert_eq!(state.queue_full_warnings.load(Ordering::Relaxed), 1);

        state.queue_full_warnings.store(1, Ordering::Relaxed);
        state.inject_scancode(&[0x30], 1, true);
        assert_eq!(state.pending_count(), 64);
        assert_eq!(state.queue_full_warnings.load(Ordering::Relaxed), 2);

        state.queue_full_warnings.store(10000, Ordering::Relaxed);
        state.inject_scancode(&[0x30], 1, true);
        assert_eq!(state.pending_count(), 64);
        assert_eq!(state.queue_full_warnings.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_shared_keyboard_state_inject_reports_outcomes() {
        let state = SharedKeyboardState::new();

        assert_eq!(
            state.inject_scancode(&[0x1E], 0x1E, true),
            ScancodeInjectOutcome::Injected
        );

        state.set_scanning_enabled(false);
        assert_eq!(
            state.inject_scancode(&[0x30], 0x30, true),
            ScancodeInjectOutcome::DroppedByGate
        );
        state.set_scanning_enabled(true);

        let filler = [0x20; 63];
        assert_eq!(
            state.inject_scancode(&filler, 0x20, true),
            ScancodeInjectOutcome::Injected
        );
        assert_eq!(state.pending_count(), 64);
        assert_eq!(
            state.inject_scancode(&[0x21], 0x21, true),
            ScancodeInjectOutcome::QueueFull
        );
    }

    #[test]
    fn test_shared_keyboard_state_queue_full_release_clears_repeat() {
        let state = SharedKeyboardState::new();
        let filler = [0x20; 64];
        assert_eq!(
            state.inject_scancode(&filler, 0x20, true),
            ScancodeInjectOutcome::Injected
        );
        state.update_repeat_state(0x1E, false, true);
        assert_eq!(state.get_repeat_scancode(), 0x1E);

        assert_eq!(
            state.inject_scancode(&[0x9E], 0x1E, false),
            ScancodeInjectOutcome::QueueFull
        );

        assert_eq!(
            state.get_repeat_scancode(),
            0,
            "Key-up dropped by queue capacity must still clear typematic state"
        );
    }

    #[test]
    fn test_shared_keyboard_state_release_matches_extended_repeat_identity() {
        let state = SharedKeyboardState::new();

        state.update_repeat_state(0x48, false, true);
        state.update_repeat_state(0x48, true, false);
        assert_eq!(
            state.get_repeat_scancode(),
            0x48,
            "E0 release must not clear plain-key repeat state"
        );
        assert!(!state.is_repeat_extended());

        state.update_repeat_state(0x48, false, false);
        assert_eq!(state.get_repeat_scancode(), 0);
        assert!(!state.is_repeat_extended());

        state.update_repeat_state(0x48, true, true);
        state.update_repeat_state(0x48, false, false);
        assert_eq!(
            state.get_repeat_scancode(),
            0x48,
            "plain release must not clear E0-key repeat state"
        );
        assert!(state.is_repeat_extended());

        state.update_repeat_state(0x48, true, false);
        assert_eq!(state.get_repeat_scancode(), 0);
        assert!(!state.is_repeat_extended());
    }

    #[test]
    fn test_shared_keyboard_state_injected_release_matches_extended_repeat_identity() {
        let state = SharedKeyboardState::new();

        state.inject_scancode(&[0x48], 0x48, true);
        assert_eq!(state.get_repeat_scancode(), 0x48);
        assert!(!state.is_repeat_extended());

        state.inject_scancode(&[0xE0, 0xC8], 0x48, false);
        assert_eq!(
            state.get_repeat_scancode(),
            0x48,
            "E0 break must not clear plain Up/Numpad repeat state"
        );
        assert!(!state.is_repeat_extended());

        state.inject_scancode(&[0xC8], 0x48, false);
        assert_eq!(state.get_repeat_scancode(), 0);
        assert!(!state.is_repeat_extended());

        state.inject_scancode(&[0xE0, 0x48], 0x48, true);
        assert_eq!(state.get_repeat_scancode(), 0x48);
        assert!(state.is_repeat_extended());

        state.inject_scancode(&[0xC8], 0x48, false);
        assert_eq!(
            state.get_repeat_scancode(),
            0x48,
            "plain break must not clear E0 Up repeat state"
        );
        assert!(state.is_repeat_extended());

        state.inject_scancode(&[0xE0, 0xC8], 0x48, false);
        assert_eq!(state.get_repeat_scancode(), 0);
        assert!(!state.is_repeat_extended());
    }

    #[test]
    fn test_shared_keyboard_state_out_of_range_key_press_time_ignored() {
        let state = SharedKeyboardState::new();

        state.inject_scancode(&[0x1E], 200, true);
        assert!(state.get_key_press_time(200).is_none());
    }

    #[test]
    fn test_shared_keyboard_state_irq_re_raise() {
        let state = SharedKeyboardState::new();

        // Inject multiple scancodes
        state.inject_scancode(&[0x1E], 10, true);
        state.inject_scancode(&[0x30], 11, true);

        // Clear the initial IRQ1 pending
        assert!(state.take_irq1_pending());

        // Read first scancode - should re-raise IRQ1 since more data pending
        assert_eq!(state.pop_byte(true).map(|b| b.value()), Some(0x1E));
        assert!(state.take_irq1_pending()); // Should be pending again

        // Read last scancode - should NOT re-raise IRQ1
        assert_eq!(state.pop_byte(true).map(|b| b.value()), Some(0x30));
        assert!(!state.take_irq1_pending()); // No more data, no IRQ
    }

    #[test]
    fn test_shared_keyboard_state_modifier_flags() {
        let state = SharedKeyboardState::new();

        // Set shift only
        state.set_modifier(true, false, false);
        assert_eq!(state.get_modifier_flags(), 0x03);

        // Set ctrl only
        state.set_modifier(false, true, false);
        assert_eq!(state.get_modifier_flags(), 0x04);

        // Set alt only
        state.set_modifier(false, false, true);
        assert_eq!(state.get_modifier_flags(), 0x08);

        // Set all
        state.set_modifier(true, true, true);
        assert_eq!(state.get_modifier_flags(), 0x0F);

        // Clear all
        state.set_modifier(false, false, false);
        assert_eq!(state.get_modifier_flags(), 0x00);
    }

    #[test]
    fn test_shared_keyboard_state_key_release() {
        let state = SharedKeyboardState::new();

        // Key press
        state.inject_scancode(&[0x1E], 10, true);
        assert!(state.get_key_press_time(10).is_some());

        // Key release
        state.inject_scancode(&[0x9E], 10, false);
        assert!(state.get_key_press_time(10).is_none());
    }

    #[test]
    fn test_shared_keyboard_state_clear() {
        let state = SharedKeyboardState::new();

        // Add some state
        state.inject_scancode(&[0x1E], 10, true);
        state.set_modifier(true, true, true);

        // Clear everything
        state.clear();

        // Verify all cleared
        assert!(!state.has_pending());
        assert!(!state.take_irq1_pending());
        assert_eq!(state.get_modifier_flags(), 0);
        assert!(!state.was_injected(10));
        assert!(state.get_key_press_time(10).is_none());
        assert_eq!(state.get_repeat_scancode(), 0);
        assert!(!state.is_repeat_extended());
    }

    #[test]
    fn test_shared_keyboard_state_clear_host_input_state_preserves_queue() {
        let state = SharedKeyboardState::new();

        state.inject_scancode(&[0xE0, 0x48], 0x48, true);
        state.set_modifier(true, true, true);
        assert_eq!(state.pending_count(), 2);
        assert_eq!(state.get_repeat_scancode(), 0x48);

        state.clear_host_input_state();

        assert_eq!(state.pending_count(), 2);
        assert_eq!(state.get_modifier_flags(), 0);
        assert!(!state.was_injected(0x48));
        assert!(state.get_key_press_time(0x48).is_none());
        assert_eq!(state.get_repeat_scancode(), 0);
        assert!(!state.is_repeat_extended());
    }

    #[test]
    fn test_shared_keyboard_state_guest_restore_epoch_increments() {
        let state = SharedKeyboardState::new();

        assert_eq!(state.guest_restore_epoch(), 0);
        state.notify_guest_state_restored();
        assert_eq!(state.guest_restore_epoch(), 1);
        state.notify_guest_state_restored();
        assert_eq!(state.guest_restore_epoch(), 2);
    }

    #[test]
    fn test_shared_keyboard_state_drain_queue() {
        let state = SharedKeyboardState::new();

        // Queue several bytes and arm the IRQ flag.
        state.inject_scancode(&[0x1E], 30, true);
        state.inject_scancode(&[0x9E], 30, false);
        state.inject_scancode(&[0x1F], 31, true);
        assert_eq!(state.pending_count(), 3);
        assert!(state.irq1_pending());

        // Focus loss: drain everything, return the count we threw away.
        let n = state.drain_queue();
        assert_eq!(n, 3);
        assert_eq!(state.pending_count(), 0);
        assert!(!state.irq1_pending());

        // Subsequent injections must re-arm IRQ1 normally.
        state.inject_scancode(&[0x20], 32, true);
        assert!(state.irq1_pending());
        assert_eq!(state.pending_count(), 1);
    }

    #[test]
    fn test_shared_keyboard_state_restore_empty_queue_clears_irq() {
        let state = SharedKeyboardState::new();

        state.inject_scancode(&[0x1E], 0x1E, true);
        assert!(state.irq1_pending());

        state.restore_queue(&[]);

        assert_eq!(state.pending_count(), 0);
        assert!(!state.irq1_pending());
    }

    #[test]
    fn test_shared_keyboard_state_clear_repeat_state() {
        let state = SharedKeyboardState::new();

        // Arm typematic via a press: extended Up arrow, key idx 0x48.
        state.inject_scancode(&[0xE0, 0x48], 0x48, true);
        assert_eq!(state.get_repeat_scancode(), 0x48);
        assert!(state.is_repeat_extended());

        // Focus loss: clear unconditionally so tick_typematic_repeat
        // stops flooding even if the matching key-up never arrives.
        state.clear_repeat_state();
        assert_eq!(state.get_repeat_scancode(), 0);
        assert!(!state.is_repeat_extended());
    }

    #[test]
    fn test_shared_keyboard_state_inject_scancode_with_poisoned_times() {
        let state = Arc::new(SharedKeyboardState::new());
        let state_clone = Arc::clone(&state);

        let _ = std::thread::spawn(move || {
            let _guard = state_clone.key_press_times.lock().unwrap();
            panic!("poison key_press_times");
        })
        .join();

        state.inject_scancode(&[0x1E], 10, true);
        assert!(state.has_pending());
    }

    #[test]
    fn test_shared_keyboard_state_clear_with_poisoned_mutexes() {
        let state = Arc::new(SharedKeyboardState::new());

        let state_clone = Arc::clone(&state);
        let _ = std::thread::spawn(move || {
            let _guard = state_clone.scancode_queue.lock().unwrap();
            panic!("poison scancode_queue");
        })
        .join();

        let state_clone = Arc::clone(&state);
        let _ = std::thread::spawn(move || {
            let _guard = state_clone.key_press_times.lock().unwrap();
            panic!("poison key_press_times");
        })
        .join();

        state.clear();
    }

    #[test]
    fn test_shared_keyboard_state_set_irq1_pending() {
        let state = SharedKeyboardState::new();

        // Manually set IRQ1 pending
        state.set_irq1_pending();
        assert!(state.take_irq1_pending());
        assert!(!state.take_irq1_pending());
    }

    /// A `Keyboard` wired to a shared state the caller keeps a handle on, the
    /// way the host input thread holds it.
    fn keyboard_with_shared_state() -> (Keyboard, Arc<SharedKeyboardState>) {
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());
        (kb, shared)
    }

    #[test]
    fn restoring_scan_disabled_closes_the_shared_host_gate() {
        // Scanning was turned on after the checkpoint was taken. Restoring the
        // scan-disabled image used to assign only the local field, so the host
        // gate stayed open and keys reached a guest that had scanning off.
        let (mut kb, shared) = keyboard_with_shared_state();
        kb.set_scanning_enabled(true);
        assert_eq!(
            shared.inject_scancode(&[0x1E], 0, true),
            ScancodeInjectOutcome::Injected
        );

        kb.restore_essential_state(&[], false, 0, 0, true);

        assert!(!shared.is_scanning_enabled());
        assert_eq!(
            shared.inject_scancode(&[0x1E], 0, true),
            ScancodeInjectOutcome::DroppedByGate,
            "a restored scan-disabled guest must not receive host keys"
        );
    }

    #[test]
    fn restoring_scan_enabled_opens_the_shared_host_gate() {
        // The reverse transition silently dropped valid keys.
        let (mut kb, shared) = keyboard_with_shared_state();
        kb.set_scanning_enabled(false);
        assert_eq!(
            shared.inject_scancode(&[0x1E], 0, true),
            ScancodeInjectOutcome::DroppedByGate
        );

        kb.restore_essential_state(&[], true, 0, 0, true);

        assert!(shared.is_scanning_enabled());
        assert_eq!(
            shared.inject_scancode(&[0x1E], 0, true),
            ScancodeInjectOutcome::Injected,
            "a restored scan-enabled guest must receive host keys"
        );
    }

    #[test]
    fn test_shared_keyboard_state_enabled_by_default() {
        let state = SharedKeyboardState::new();
        assert!(state.is_keyboard_enabled());
    }

    #[test]
    fn test_shared_keyboard_state_set_keyboard_enabled() {
        let state = SharedKeyboardState::new();

        // Disable keyboard
        state.set_keyboard_enabled(false);
        assert!(!state.is_keyboard_enabled());

        // Re-enable keyboard
        state.set_keyboard_enabled(true);
        assert!(state.is_keyboard_enabled());
    }

    #[test]
    fn test_shared_keyboard_state_scanning_enabled_by_default() {
        let state = SharedKeyboardState::new();
        assert!(state.is_scanning_enabled());
    }

    #[test]
    fn test_shared_keyboard_state_set_scanning_enabled() {
        let state = SharedKeyboardState::new();

        // Disable scanning
        state.set_scanning_enabled(false);
        assert!(!state.is_scanning_enabled());

        // Re-enable scanning
        state.set_scanning_enabled(true);
        assert!(state.is_scanning_enabled());
    }

    #[test]
    fn test_shared_keyboard_state_disabled_drops_scancodes() {
        let state = SharedKeyboardState::new();

        // Disable keyboard interface
        state.set_keyboard_enabled(false);

        // Try to inject scancodes - should be silently dropped
        state.inject_scancode(&[0x1E], 0, true); // 'A' key down

        // Queue should be empty
        assert!(!state.has_pending());
        assert!(state.pop_byte(true).is_none());

        // IRQ should not have been raised
        assert!(!state.take_irq1_pending());
    }

    #[test]
    fn test_shared_keyboard_state_scanning_disabled_drops_scancodes() {
        let state = SharedKeyboardState::new();

        // Disable scanning
        state.set_scanning_enabled(false);

        // Try to inject scancodes - should be silently dropped
        state.inject_scancode(&[0x1E], 0, true); // 'A' key down

        // Queue should be empty
        assert!(!state.has_pending());
        assert!(state.pop_byte(true).is_none());

        // IRQ should not have been raised
        assert!(!state.take_irq1_pending());
    }

    #[test]
    fn test_shared_keyboard_state_enabled_accepts_scancodes() {
        let state = SharedKeyboardState::new();

        // Keyboard is enabled by default
        assert!(state.is_keyboard_enabled());

        // Inject scancode
        state.inject_scancode(&[0x1E], 0, true); // 'A' key down

        // Should be queued
        assert!(state.has_pending());
        assert_eq!(state.pop_byte(true).map(|b| b.value()), Some(0x1E));
        assert!(state.take_irq1_pending());
    }

    #[test]
    fn test_shared_keyboard_state_re_enable_accepts_scancodes() {
        let state = SharedKeyboardState::new();

        // Disable then re-enable
        state.set_keyboard_enabled(false);
        state.set_keyboard_enabled(true);

        // Inject scancode
        state.inject_scancode(&[0x1E], 0, true);

        // Should be queued
        assert!(state.has_pending());
        assert_eq!(state.pop_byte(true).map(|b| b.value()), Some(0x1E));
    }

    #[test]
    fn test_shared_keyboard_state_read_scancode_recovers_from_poison() {
        let state = SharedKeyboardState::new();

        let _ = std::panic::catch_unwind(|| {
            let _guard = state.scancode_queue.lock().unwrap();
            panic!("poison");
        });

        state.inject_scancode(&[0x1E], 0, true);
        assert_eq!(state.pop_byte(true).map(|b| b.value()), Some(0x1E));
    }

    // ========================================================================
    // Keyboard with SharedState Tests
    // ========================================================================

    #[test]
    fn test_keyboard_shared_state_always_present() {
        // After the 2026-05-12 dual→single queue collapse the keyboard
        // always has a SharedKeyboardState (constructed in Keyboard::new).
        let kb = Keyboard::new();
        // Queue starts empty.
        assert_eq!(kb.shared_state().pending_count(), 0);
    }

    #[test]
    fn test_keyboard_set_shared_state() {
        let mut kb = Keyboard::new();
        let state = Arc::new(SharedKeyboardState::new());

        kb.set_shared_state(Arc::clone(&state));

        // Verify the swap actually happened — the new state's pointer
        // should match what we passed in.
        assert!(std::ptr::eq(
            kb.shared_state() as *const _,
            Arc::as_ptr(&state)
        ));
    }

    #[test]
    fn test_keyboard_read_data_from_shared() {
        let mut kb = Keyboard::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        // Inject into shared state
        state.inject_scancode(&[0x1E, 0x9E], 10, true);

        // Read should come from shared queue
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.read_data(), 0x9E);
    }

    #[test]
    fn test_keyboard_read_keyboard_output_with_empty_shared_queue() {
        let mut kb = Keyboard::new();
        kb.set_shared_state(Arc::new(SharedKeyboardState::new()));
        assert_eq!(kb.pop_keyboard_byte(), None);
    }

    #[test]
    fn test_keyboard_read_data_priority() {
        let mut kb = Keyboard::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        // Inject into both queues
        state.inject_scancode(&[0xAA], 10, true); // Shared queue
        kb.enqueue_scancodes(&[0xBB]); // Local queue

        // Local queue has priority (command responses should not be starved)
        assert_eq!(kb.read_data(), 0xBB);
        // Then shared queue
        assert_eq!(kb.read_data(), 0xAA);
    }

    #[test]
    fn test_keyboard_mouse_interleaving() {
        let mut kb = Keyboard::new();

        kb.mouse.handle_command(0xF4); // Enable reporting
        kb.mouse.read_output();
        kb.mouse.accumulate_movement(1, 0);
        assert!(kb.mouse.maybe_generate_packet(0));

        kb.enqueue_scancodes(&[0xAA, 0xBB]);

        let _b1 = kb.read_data();
        let from_mouse1 = kb.data_register_source_was_mouse;
        let _b2 = kb.read_data();
        let from_mouse2 = kb.data_register_source_was_mouse;

        assert!(
            from_mouse1 | from_mouse2,
            "mouse data should not be starved"
        );
    }

    #[test]
    fn test_keyboard_read_status_with_shared() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        // Initially no data
        assert_eq!(kb.read_status() & 0x01, 0);

        // Inject into shared queue and stage into output buffer
        state.inject_scancode(&[0x1E], 10, true);
        kb.stage_output(&mut pic);

        // Status should show data available
        assert_eq!(kb.read_status() & 0x01, 1);

        // Read the data
        kb.read_data();

        // Status should be clear again
        assert_eq!(kb.read_status() & 0x01, 0);
    }

    #[test]
    fn test_keyboard_has_pending_with_shared() {
        let mut kb = Keyboard::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        // Initially no pending
        assert!(!kb.has_pending_scancodes());

        // Inject into shared queue
        state.inject_scancode(&[0x1E], 10, true);
        assert!(kb.has_pending_scancodes());

        // Read it
        kb.read_data();
        assert!(!kb.has_pending_scancodes());

        // Inject into local queue
        kb.enqueue_scancodes(&[0xBB]);
        assert!(kb.has_pending_scancodes());
    }

    // ========================================================================
    // 8042 Controller Command Tests
    // ========================================================================

    #[test]
    fn test_8042_controller_ram_initialization() {
        let kb = Keyboard::new();
        // Command byte should be 0x47 (IRQ1 enabled, system flag, translation)
        assert_eq!(kb.controller_ram[0], 0x47);
        assert!(kb.pending_command.is_none());
    }

    #[test]
    fn test_8042_self_test() {
        let mut kb = Keyboard::new();

        // Command 0xAA: Controller self-test
        kb.write_command(0xAA);

        // Should return 0x55 (pass)
        assert_eq!(kb.read_data(), 0x55);
    }

    #[test]
    fn test_8042_interface_test() {
        let mut kb = Keyboard::new();

        // Command 0xAB: Keyboard interface test
        kb.write_command(0xAB);

        // Should return 0x00 (no error)
        assert_eq!(kb.read_data(), 0x00);
    }

    #[test]
    fn test_8042_mouse_port_test() {
        let mut kb = Keyboard::new();

        // Command 0xA9: Mouse port test
        kb.write_command(0xA9);

        // Should return 0x00 (no error)
        assert_eq!(kb.read_data(), 0x00);
    }

    #[test]
    fn test_8042_read_controller_ram() {
        let mut kb = Keyboard::new();

        // Command 0x20: Read command byte (controller RAM[0])
        kb.write_command(0x20);

        // Should return the command byte (0x47)
        assert_eq!(kb.read_data(), 0x47);
    }

    #[test]
    fn test_8042_write_controller_ram() {
        let mut kb = Keyboard::new();

        // Command 0x60: Write command byte (controller RAM[0])
        kb.write_command(0x60);

        // Should be waiting for data
        assert_eq!(kb.pending_command, Some(PendingCommand::Controller(0x60)));

        // Write the new command byte
        kb.write_data(0x65);

        // Pending command should be cleared
        assert!(kb.pending_command.is_none());

        // Verify it was written
        kb.write_command(0x20);
        assert_eq!(kb.read_data(), 0x65);
    }

    #[test]
    fn test_8042_write_controller_ram_nonzero_index() {
        let mut kb = Keyboard::new();

        kb.write_command(0x61);
        kb.write_data(0x12);

        assert_eq!(kb.controller_ram[1], 0x12);
    }

    #[test]
    fn test_8042_disable_enable_keyboard() {
        let mut kb = Keyboard::new();

        // Initial state: keyboard enabled (bit 4 = 0)
        assert_eq!(kb.controller_ram[0] & 0x10, 0);

        // Command 0xAD: Disable keyboard
        kb.write_command(0xAD);

        // Bit 4 should be set
        assert_ne!(kb.controller_ram[0] & 0x10, 0);

        // Command 0xAE: Enable keyboard
        kb.write_command(0xAE);

        // Bit 4 should be clear
        assert_eq!(kb.controller_ram[0] & 0x10, 0);
    }

    #[test]
    fn test_8042_read_output_port() {
        let mut kb = Keyboard::new();

        // Command 0xD0: Read output port
        kb.write_command(0xD0);

        // Should return typical value (0xDF)
        assert_eq!(kb.read_data(), 0xDF);
    }

    #[test]
    fn test_8042_write_output_port() {
        let mut kb = Keyboard::new();

        // Command 0xD1: Write output port
        kb.write_command(0xD1);

        // Should be waiting for data
        assert_eq!(kb.pending_command, Some(PendingCommand::Controller(0xD1)));

        // Write data (A20 gate control)
        kb.write_data(0xDF);

        // Pending command should be cleared
        assert!(kb.pending_command.is_none());
    }

    #[test]
    fn test_8042_write_mouse_output_buffer() {
        let mut kb = Keyboard::new();

        // Command 0xD3: Write mouse output buffer
        kb.write_command(0xD3);
        assert_eq!(kb.pending_command, Some(PendingCommand::Controller(0xD3)));

        kb.write_data(0x5A);
        assert!(kb.pending_command.is_none());

        let status = kb.read_status();
        assert_eq!(status & 0x01, 0x01); // Output buffer full
        assert_eq!(status & 0x20, 0x20); // Mouse data

        assert_eq!(kb.read_data(), 0x5A);
        assert_eq!(kb.read_status() & 0x01, 0x00);
    }

    #[test]
    fn test_8042_write_to_mouse_device() {
        let mut kb = Keyboard::new();

        kb.write_command(0xD4);
        kb.write_data(0xF4); // ENABLE REPORTING — mouse replies 0xFA

        assert!(!kb.mouse_write_pending);

        // After write_data the reply byte must be staged in OBF so the
        // host can read it via port 0x60 (status bit 0 = OBF set, bit 5
        // = mouse source). Without this, BIOS or mouse-driver PS/2 init
        // times out polling for the ACK.
        let status = kb.read_status();
        assert_eq!(status & 0x01, 0x01, "OBF should be set");
        assert_eq!(status & 0x20, 0x20, "mouse-source flag should be set");
        assert_eq!(kb.read_data(), 0xFA, "host should read mouse ACK");
    }

    #[test]
    fn test_8042_write_output_port_tracks_a20() {
        let mut kb = Keyboard::new();

        kb.write_command(0xD1);
        kb.write_data(0xDD); // Clear A20 bit

        assert_eq!(kb.take_pending_a20(), Some(false));
        kb.write_command(0xD0);
        assert_eq!(kb.read_data(), 0xDD);

        kb.write_command(0xD1);
        kb.write_data(0xDF); // Set A20 bit

        assert_eq!(kb.take_pending_a20(), Some(true));
        kb.write_command(0xD0);
        assert_eq!(kb.read_data(), 0xDF);
    }

    #[test]
    fn test_set_output_port_a20() {
        let mut kb = Keyboard::new();

        kb.set_output_port_a20(false);
        assert_eq!(kb.output_port() & 0x02, 0);

        kb.set_output_port_a20(true);
        assert_ne!(kb.output_port() & 0x02, 0);
    }

    #[test]
    fn test_8042_write_keyboard_output() {
        let mut kb = Keyboard::new();

        // Command 0xD2: Write to keyboard output buffer
        kb.write_command(0xD2);

        // Should be waiting for data
        assert_eq!(kb.pending_command, Some(PendingCommand::Controller(0xD2)));

        // Write a scancode to inject
        kb.write_data(0x1E);

        // Pending command should be cleared
        assert!(kb.pending_command.is_none());

        // Scancode should appear in output buffer
        assert_eq!(kb.read_data(), 0x1E);
    }

    #[test]
    fn test_8042_mouse_port_commands() {
        let mut kb = Keyboard::new();

        // These should not panic or cause issues
        kb.write_command(0xA7); // Disable mouse port
        kb.write_command(0xA8); // Enable mouse port
    }

    #[test]
    fn test_8042_system_reset() {
        let mut kb = Keyboard::new();

        let output_port_before = kb.output_port();

        // Command 0xFE: controller output-port pulse requests system reset.
        kb.write_command(0xFE);
        assert_eq!(
            kb.take_pending_guest_reset(),
            Some(GuestResetSource::KeyboardControllerPulseOutputPort)
        );
        assert_eq!(kb.take_pending_guest_reset(), None);
        assert_eq!(kb.take_pending_a20(), None);
        assert_eq!(kb.output_port(), output_port_before);
        assert!(kb.pending_command.is_none());
        assert_eq!(kb.read_status() & 0x01, 0, "0xFE must not enqueue OBF data");
    }

    #[test]
    fn test_8042_pulse_output_noop_ff() {
        let mut kb = Keyboard::new();

        kb.write_command(0xFF);
        assert_eq!(kb.take_pending_guest_reset(), None);
        assert!(kb.pending_command.is_none());
        assert_eq!(kb.read_status() & 0x01, 0, "0xFF must not enqueue OBF data");
    }

    #[test]
    fn test_8042_pulse_output_reset_when_bit0_low() {
        let mut kb = Keyboard::new();

        kb.write_command(0xF0);
        assert_eq!(
            kb.take_pending_guest_reset(),
            Some(GuestResetSource::KeyboardControllerPulseOutputPort)
        );
    }

    #[test]
    fn test_8042_unknown_command() {
        let mut kb = Keyboard::new();

        // Unknown command should not panic
        kb.write_command(0x40);
    }

    // ========================================================================
    // Keyboard Command Tests (via port 0x60)
    // ========================================================================

    #[test]
    fn test_keyboard_echo() {
        let mut kb = Keyboard::new();

        // Command 0xEE: Echo
        kb.write_data(0xEE);

        // Should return 0xEE
        assert_eq!(kb.read_data(), 0xEE);
    }

    #[test]
    fn test_keyboard_read_id() {
        let mut kb = Keyboard::new();

        // Command 0xF2: Read keyboard ID
        kb.write_data(0xF2);

        // Should return ACK, then ID bytes (0xAB 0x83 for MF2 keyboard)
        assert_eq!(kb.read_data(), 0xFA); // ACK
        assert_eq!(kb.read_data(), 0xAB); // ID byte 1
        assert_eq!(kb.read_data(), 0x83); // ID byte 2
    }

    #[test]
    fn test_keyboard_set_leds() {
        let mut kb = Keyboard::new();

        // Command 0xED: Set LEDs
        kb.write_data(0xED);

        // Should return ACK
        assert_eq!(kb.read_data(), 0xFA);

        // Should be waiting for LED byte
        assert_eq!(kb.pending_command, Some(PendingCommand::Keyboard(0xED)));

        // Send LED state (we don't track this, just ensure it doesn't panic)
        kb.write_data(0x07); // All LEDs on
        assert_eq!(kb.read_data(), 0xFA);
    }

    #[test]
    fn test_keyboard_set_typematic() {
        let mut kb = Keyboard::new();

        // Command 0xF3: Set typematic rate/delay
        kb.write_data(0xF3);

        // Should return ACK
        assert_eq!(kb.read_data(), 0xFA);

        // Should be waiting for rate byte
        assert_eq!(kb.pending_command, Some(PendingCommand::Keyboard(0xF3)));

        // Send rate byte
        kb.write_data(0x00); // Fastest rate
        assert_eq!(kb.read_data(), 0xFA);
    }

    #[test]
    fn test_keyboard_set_typematic_updates_shared_state() {
        let mut kb = Keyboard::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        kb.write_data(0xF3);
        assert_eq!(kb.read_data(), 0xFA);

        kb.write_data(0xE3);
        assert_eq!(kb.read_data(), 0xFA);
        assert_eq!(kb.get_typematic_delay(), 0x03);
        assert_eq!(kb.get_typematic_rate(), 0x03);
        assert_eq!(state.get_typematic_delay(), 0x03);
        assert_eq!(state.get_typematic_rate(), 0x03);
    }

    #[test]
    fn test_keyboard_enable_disable_scanning() {
        let mut kb = Keyboard::new();

        // Command 0xF5: Disable scanning
        kb.write_data(0xF5);
        assert_eq!(kb.read_data(), 0xFA); // ACK

        // Command 0xF4: Enable scanning
        kb.write_data(0xF4);
        assert_eq!(kb.read_data(), 0xFA); // ACK
    }

    #[test]
    fn test_keyboard_disable_scanning_blocks_input_scancodes() {
        let mut kb = Keyboard::new();

        // Disable scanning
        kb.write_data(0xF5);
        assert_eq!(kb.read_data(), 0xFA); // ACK

        kb.enqueue_input_scancodes(&[0x1E]);
        assert!(!kb.has_pending_scancodes());

        // Re-enable scanning
        kb.write_data(0xF4);
        assert_eq!(kb.read_data(), 0xFA); // ACK

        kb.enqueue_input_scancodes(&[0x1E]);
        assert!(kb.has_pending_scancodes());
    }

    #[test]
    fn test_enqueue_input_scancodes_rejects_empty_or_disabled() {
        let mut kb = Keyboard::new();

        assert!(!kb.enqueue_input_scancodes(&[]));

        kb.write_command(0xAD);
        assert!(!kb.enqueue_input_scancodes(&[0x1E]));

        kb.write_command(0xAE);
        kb.write_data(0xF5);
        kb.read_data(); // ACK
        assert!(!kb.enqueue_input_scancodes(&[0x1E]));

        kb.write_data(0xF4);
        kb.read_data(); // ACK
        assert!(kb.enqueue_input_scancodes(&[0x1E]));
    }

    #[test]
    fn test_keyboard_set_scancode_set_query() {
        let mut kb = Keyboard::new();

        // Command 0xF0: Set scancode set (query current)
        kb.write_data(0xF0);
        assert_eq!(kb.read_data(), 0xFA);
        assert_eq!(kb.pending_command, Some(PendingCommand::Keyboard(0xF0)));

        // Query current set (0 = query)
        kb.write_data(0x00);
        assert_eq!(kb.read_data(), 0xFA);
        assert_eq!(kb.read_data(), 0x01);

        // Set to set 2 (we track but still emit set 1)
        kb.write_data(0xF0);
        assert_eq!(kb.read_data(), 0xFA);
        kb.write_data(0x02);
        assert_eq!(kb.read_data(), 0xFA);
        assert_eq!(kb.scancode_set, 0x02);
    }

    #[test]
    fn test_keyboard_set_defaults() {
        let mut kb = Keyboard::new();

        // Command 0xF6: Set defaults
        kb.write_data(0xF6);

        // Should return ACK
        assert_eq!(kb.read_data(), 0xFA);

        // Defaults should re-enable scanning.
        kb.write_data(0xF5);
        assert!(!kb.enqueue_input_scancodes(&[0x1E]));
        kb.write_data(0xF6);
        kb.read_data(); // ACK
        assert!(kb.enqueue_input_scancodes(&[0x1E]));
    }

    #[test]
    fn test_keyboard_reset() {
        let mut kb = Keyboard::new();

        // Command 0xFF: Reset keyboard
        kb.write_data(0xFF);

        // Should return ACK, then self-test passed (0xAA)
        assert_eq!(kb.read_data(), 0xFA); // ACK
        assert_eq!(kb.read_data(), 0xAA); // Self-test passed

        // Reset should re-enable scanning.
        kb.write_data(0xF5);
        assert!(!kb.enqueue_input_scancodes(&[0x1E]));
        kb.write_data(0xFF);
        kb.read_data(); // ACK
        kb.read_data(); // Self-test passed
        assert!(kb.enqueue_input_scancodes(&[0x1E]));
    }

    #[test]
    fn test_keyboard_invalid_command_requests_resend() {
        let mut kb = Keyboard::new();

        kb.write_data(0x05);
        assert_eq!(kb.read_data(), 0xFE);

        kb.write_data(0xEF);
        assert_eq!(kb.read_data(), 0xFE);

        kb.write_data(0xF1);
        assert_eq!(kb.read_data(), 0xFE);
    }

    // ========================================================================
    // 8042 Keyboard/Mouse Disable Tests
    // ========================================================================

    #[test]
    fn test_is_keyboard_disabled_initial() {
        let kb = Keyboard::new();
        // Keyboard should be enabled by default
        assert!(!kb.is_keyboard_disabled());
    }

    #[test]
    fn test_is_mouse_disabled_initial() {
        let kb = Keyboard::new();
        // Mouse should be enabled by default
        assert!(!kb.is_mouse_disabled());
    }

    #[test]
    fn test_keyboard_disable_via_command() {
        let mut kb = Keyboard::new();

        // Disable keyboard (0xAD)
        kb.write_command(0xAD);
        assert!(kb.is_keyboard_disabled());

        // Enable keyboard (0xAE)
        kb.write_command(0xAE);
        assert!(!kb.is_keyboard_disabled());
    }

    #[test]
    fn test_keyboard_disable_syncs_to_shared_state() {
        let mut kb = Keyboard::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        // Initially enabled
        assert!(state.is_keyboard_enabled());

        // Disable keyboard via 8042 command
        kb.write_command(0xAD);
        assert!(!state.is_keyboard_enabled());

        // Scancodes should be dropped
        state.inject_scancode(&[0x1E], 0, true);
        assert!(!state.has_pending());

        // Enable keyboard via 8042 command
        kb.write_command(0xAE);
        assert!(state.is_keyboard_enabled());

        // Scancodes should now be accepted
        state.inject_scancode(&[0x1E], 0, true);
        assert!(state.has_pending());
        assert_eq!(state.pop_byte(true).map(|b| b.value()), Some(0x1E));
    }

    #[test]
    fn test_keyboard_scanning_syncs_shared_state() {
        let mut kb = Keyboard::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        kb.write_data(0xF5);
        kb.read_data(); // ACK
        assert!(!state.is_scanning_enabled());

        kb.write_data(0xF4);
        kb.read_data(); // ACK
        assert!(state.is_scanning_enabled());
    }

    #[test]
    fn test_keyboard_command_byte_syncs_shared_state() {
        let mut kb = Keyboard::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        // Disable keyboard via command byte write (bit 4 set).
        kb.write_command(0x60);
        kb.write_data(0x57);
        assert!(!state.is_keyboard_enabled());

        // Re-enable via command byte write (bit 4 clear).
        kb.write_command(0x60);
        kb.write_data(0x47);
        assert!(state.is_keyboard_enabled());
    }

    #[test]
    fn test_mouse_disable_via_command() {
        let mut kb = Keyboard::new();

        // Disable mouse (0xA7)
        kb.write_command(0xA7);
        assert!(kb.is_mouse_disabled());

        // Enable mouse (0xA8)
        kb.write_command(0xA8);
        assert!(!kb.is_mouse_disabled());
    }

    #[test]
    fn test_keyboard_disabled_blocks_read_data() {
        let mut kb = Keyboard::new();

        // Enqueue some scancodes
        assert!(kb.enqueue_input_scancodes(&[0x1E, 0x9E]));

        assert!(kb.has_pending_scancodes());

        // Disable keyboard
        kb.write_command(0xAD);

        // User input stays queued while the keyboard interface is disabled.
        assert!(kb.has_pending_scancodes());
        assert_eq!(kb.read_data(), 0x00);
        assert_eq!(kb.pending_user_input_count(), 2);
        assert_eq!(kb.read_status() & 0x01, 0);

        kb.write_command(0xAE);
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.read_data(), 0x9E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        assert!(!kb.has_pending_scancodes());
    }

    #[test]
    fn test_keyboard_disabled_blocks_status() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        // Enqueue and stage data
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        kb.stage_output(&mut pic);

        // Status should show data available
        assert_eq!(kb.read_status() & 0x01, 0x01);

        // Disable keyboard
        kb.write_command(0xAD);

        // Status should still show keyboard data available when disabled
        assert_eq!(kb.read_status() & 0x01, 0x01);
        assert_eq!(kb.read_status() & 0x10, 0x10);

        // Re-enable
        kb.write_command(0xAE);

        // Status should show data again
        assert_eq!(kb.read_status() & 0x01, 0x01);

        assert_eq!(kb.read_port60(&mut pic), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
    }

    #[test]
    fn test_keyboard_disabled_has_pending_true() {
        let mut kb = Keyboard::new();

        // Enqueue data
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        assert!(kb.has_pending_scancodes());

        // Disable keyboard
        kb.write_command(0xAD);

        // has_pending_scancodes should still return true
        assert!(kb.has_pending_scancodes());
        assert_eq!(kb.read_data(), 0);
        assert_eq!(kb.pending_user_input_count(), 1);
        kb.write_command(0xAE);
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        assert!(!kb.has_pending_scancodes());
    }

    #[test]
    fn test_disable_via_controller_ram_write() {
        let mut kb = Keyboard::new();

        // Write command byte with bit 4 set (keyboard disabled)
        kb.write_command(0x60); // Write command byte
        kb.write_data(0x57); // 0x47 | 0x10 = keyboard disabled

        assert!(kb.is_keyboard_disabled());

        // Write command byte with bit 5 set (mouse disabled)
        kb.write_command(0x60);
        kb.write_data(0x67); // 0x47 | 0x20 = mouse disabled

        assert!(kb.is_mouse_disabled());
    }

    #[test]
    fn test_keyboard_disable_does_not_affect_mouse() {
        let mut kb = Keyboard::new();

        // Disable only keyboard
        kb.write_command(0xAD);

        assert!(kb.is_keyboard_disabled());
        assert!(!kb.is_mouse_disabled());
    }

    #[test]
    fn test_mouse_disable_does_not_affect_keyboard() {
        let mut kb = Keyboard::new();

        // Disable only mouse
        kb.write_command(0xA7);

        assert!(!kb.is_keyboard_disabled());
        assert!(kb.is_mouse_disabled());
    }

    #[test]
    fn test_kbd_trace_disabled_branches() {
        let mut kb = Keyboard::new();

        kb.pending_command = Some(PendingCommand::Controller(0x00));
        kb.write_data(0x12);
        kb.pending_command = Some(PendingCommand::Keyboard(0x00));
        kb.write_data(0x12);

        kb.write_data(0xF7);
        kb.write_command(0xC0);
    }

    // ========================================================================
    // Scancode Set translation table tests
    // ========================================================================

    #[test]
    fn test_set1_to_set2_table_key_mappings() {
        // Verify critical Set 1 -> Set 2 translations
        assert_eq!(SET1_TO_SET2[0x01], 0x76, "Esc");
        assert_eq!(SET1_TO_SET2[0x02], 0x16, "1");
        assert_eq!(SET1_TO_SET2[0x03], 0x1E, "2");
        assert_eq!(SET1_TO_SET2[0x1E], 0x1C, "A");
        assert_eq!(SET1_TO_SET2[0x1C], 0x5A, "Enter");
        assert_eq!(SET1_TO_SET2[0x2A], 0x12, "LShift");
        assert_eq!(SET1_TO_SET2[0x39], 0x29, "Space");
        assert_eq!(SET1_TO_SET2[0x3A], 0x58, "CapsLock");
        assert_eq!(SET1_TO_SET2[0x3B], 0x05, "F1");
        assert_eq!(SET1_TO_SET2[0x45], 0x77, "NumLock");
        assert_eq!(SET1_TO_SET2[0x46], 0x7E, "ScrollLock");
        assert_eq!(SET1_TO_SET2[0x0E], 0x66, "Backspace");
        assert_eq!(SET1_TO_SET2[0x0F], 0x0D, "Tab");
        assert_eq!(SET1_TO_SET2[0x38], 0x11, "LAlt");
        assert_eq!(SET1_TO_SET2[0x1D], 0x14, "LCtrl");
        assert_eq!(SET1_TO_SET2[0x36], 0x59, "RShift");
        assert_eq!(SET1_TO_SET2[0x37], 0x7C, "Keypad *");
    }

    #[test]
    fn test_set1_ext_to_set2_table_key_mappings() {
        assert_eq!(SET1_EXT_TO_SET2[0x4D], 0x74, "Right Arrow");
        assert_eq!(SET1_EXT_TO_SET2[0x4B], 0x6B, "Left Arrow");
        assert_eq!(SET1_EXT_TO_SET2[0x50], 0x72, "Down Arrow");
        assert_eq!(SET1_EXT_TO_SET2[0x48], 0x75, "Up Arrow");
        assert_eq!(SET1_EXT_TO_SET2[0x52], 0x70, "Insert");
        assert_eq!(SET1_EXT_TO_SET2[0x53], 0x71, "Delete");
        assert_eq!(SET1_EXT_TO_SET2[0x47], 0x6C, "Home");
        assert_eq!(SET1_EXT_TO_SET2[0x4F], 0x69, "End");
        assert_eq!(SET1_EXT_TO_SET2[0x49], 0x7D, "PageUp");
        assert_eq!(SET1_EXT_TO_SET2[0x51], 0x7A, "PageDown");
        assert_eq!(SET1_EXT_TO_SET2[0x1D], 0x14, "RCtrl");
        assert_eq!(SET1_EXT_TO_SET2[0x38], 0x11, "RAlt");
        assert_eq!(SET1_EXT_TO_SET2[0x1C], 0x5A, "Keypad Enter");
        assert_eq!(SET1_EXT_TO_SET2[0x35], 0x4A, "Keypad /");
    }

    #[test]
    fn test_set1_to_set3_table_key_mappings() {
        assert_eq!(SET1_TO_SET3[0x01], 0x08, "Esc");
        assert_eq!(SET1_TO_SET3[0x1E], 0x1C, "A");
        assert_eq!(SET1_TO_SET3[0x1C], 0x5A, "Enter");
        assert_eq!(SET1_TO_SET3[0x39], 0x29, "Space");
        assert_eq!(SET1_TO_SET3[0x2A], 0x12, "LShift");
        assert_eq!(SET1_TO_SET3[0x3B], 0x07, "F1");
    }

    #[test]
    fn test_set1_ext_to_set3_table_key_mappings() {
        // Set 3 Right Arrow = 0x6A per IBM PS/2 Technical Reference
        assert_eq!(SET1_EXT_TO_SET3[0x4D], 0x6A, "Right Arrow");
        assert_eq!(SET1_EXT_TO_SET3[0x4B], 0x61, "Left Arrow");
        assert_eq!(SET1_EXT_TO_SET3[0x50], 0x60, "Down Arrow");
        assert_eq!(SET1_EXT_TO_SET3[0x48], 0x63, "Up Arrow");
        assert_eq!(SET1_EXT_TO_SET3[0x52], 0x67, "Insert");
        assert_eq!(SET1_EXT_TO_SET3[0x53], 0x64, "Delete");
        assert_eq!(SET1_EXT_TO_SET3[0x47], 0x6E, "Home");
        assert_eq!(SET1_EXT_TO_SET3[0x4F], 0x65, "End");
        assert_eq!(SET1_EXT_TO_SET3[0x49], 0x6F, "PageUp");
        assert_eq!(SET1_EXT_TO_SET3[0x51], 0x6D, "PageDown");
    }

    #[test]
    fn test_effective_output_set_default() {
        // Default: scancode_set=1, translation enabled -> output Set 1
        let kb = Keyboard::new();
        assert_eq!(kb.effective_output_set(), 1);
    }

    #[test]
    fn test_effective_output_set_set2_with_translation() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        // Translation enabled (default) -> output Set 1
        assert_eq!(kb.effective_output_set(), 1);
    }

    #[test]
    fn test_effective_output_set_set2_without_translation() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable translation
                                       // Translation disabled -> output Set 2
        assert_eq!(kb.effective_output_set(), 2);
    }

    #[test]
    fn test_effective_output_set_set3() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 3;
        // Set 3 is always Set 3 regardless of translation bit
        assert_eq!(kb.effective_output_set(), 3);
    }

    #[test]
    fn test_set1_to_set2_table_no_zero_for_common_keys() {
        // Common keys should all have non-zero Set 2 codes
        let common_set1: &[u8] = &[
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, // Esc, 1-0
            0x0C, 0x0D, 0x0E, 0x0F, // -, =, Backspace, Tab
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, // Q-P
            0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22,
            0x23, // [, ], Enter, LCtrl, A-H
            0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, // J-L, ;, ', `, LShift, backslash
            0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35,
            0x36, // Z-M, ,./,RShift
            0x37, 0x38, 0x39, 0x3A, // Kp*, LAlt, Space, CapsLock
            0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, // F1-F10
            0x45, 0x46, // NumLock, ScrollLock
            0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52,
            0x53, // Keypad
        ];
        for &sc in common_set1 {
            assert_ne!(
                SET1_TO_SET2[sc as usize], 0x00,
                "Set 2 code for Set 1 0x{:02X} should not be 0",
                sc
            );
        }
    }

    // ========================================================================
    // Scancode Set Translation Table Tests
    // ========================================================================

    #[test]
    fn test_set1_to_set2_table_spot_checks() {
        // Verify key entries against IBM PS/2 Technical Reference
        assert_eq!(SET1_TO_SET2[0x01], 0x76, "Esc");
        assert_eq!(SET1_TO_SET2[0x1E], 0x1C, "A");
        assert_eq!(SET1_TO_SET2[0x39], 0x29, "Space");
        assert_eq!(SET1_TO_SET2[0x1C], 0x5A, "Enter");
        assert_eq!(SET1_TO_SET2[0x0E], 0x66, "Backspace");
        assert_eq!(SET1_TO_SET2[0x0F], 0x0D, "Tab");
        assert_eq!(SET1_TO_SET2[0x3B], 0x05, "F1");
        assert_eq!(SET1_TO_SET2[0x44], 0x09, "F10");
        assert_eq!(SET1_TO_SET2[0x57], 0x78, "F11");
        assert_eq!(SET1_TO_SET2[0x58], 0x07, "F12");
        assert_eq!(SET1_TO_SET2[0x41], 0x83, "F7");
        assert_eq!(SET1_TO_SET2[0x45], 0x77, "NumLock");
        assert_eq!(SET1_TO_SET2[0x2A], 0x12, "LShift");
        assert_eq!(SET1_TO_SET2[0x36], 0x59, "RShift");
        assert_eq!(SET1_TO_SET2[0x1D], 0x14, "LCtrl");
        assert_eq!(SET1_TO_SET2[0x38], 0x11, "LAlt");
    }

    #[test]
    fn test_set1_to_set3_table_spot_checks() {
        assert_eq!(SET1_TO_SET3[0x01], 0x08, "Esc");
        assert_eq!(SET1_TO_SET3[0x1E], 0x1C, "A");
        assert_eq!(SET1_TO_SET3[0x39], 0x29, "Space");
        assert_eq!(SET1_TO_SET3[0x1C], 0x5A, "Enter");
        assert_eq!(SET1_TO_SET3[0x3B], 0x07, "F1");
        assert_eq!(SET1_TO_SET3[0x44], 0x4F, "F10");
        assert_eq!(SET1_TO_SET3[0x57], 0x56, "F11");
        assert_eq!(SET1_TO_SET3[0x58], 0x5E, "F12");
        assert_eq!(SET1_TO_SET3[0x45], 0x76, "NumLock");
        assert_eq!(SET1_TO_SET3[0x38], 0x19, "LAlt");
    }

    #[test]
    fn test_set1_to_set3_f12_is_0x5e() {
        // IBM PS/2 Technical Reference: F12 in Set 3 = 0x5E
        assert_eq!(SET1_TO_SET3[0x58], 0x5E, "F12 Set 3 should be 0x5E");
    }

    #[test]
    fn test_set1_to_set3_scroll_lock_is_0x5f() {
        // IBM PS/2 Technical Reference: Scroll Lock in Set 3 = 0x5F
        assert_eq!(SET1_TO_SET3[0x46], 0x5F, "Scroll Lock Set 3 should be 0x5F");
    }

    #[test]
    fn test_set1_to_set2_extended_spot_checks() {
        assert_eq!(set1_to_set2_extended(0x1C), 0x5A, "Keypad Enter");
        assert_eq!(set1_to_set2_extended(0x1D), 0x14, "RCtrl");
        assert_eq!(set1_to_set2_extended(0x35), 0x4A, "Keypad /");
        assert_eq!(set1_to_set2_extended(0x38), 0x11, "RAlt");
        assert_eq!(set1_to_set2_extended(0x48), 0x75, "Up");
        assert_eq!(set1_to_set2_extended(0x50), 0x72, "Down");
        assert_eq!(set1_to_set2_extended(0x4B), 0x6B, "Left");
        assert_eq!(set1_to_set2_extended(0x4D), 0x74, "Right");
        assert_eq!(set1_to_set2_extended(0x52), 0x70, "Insert");
        assert_eq!(set1_to_set2_extended(0x53), 0x71, "Delete");
        assert_eq!(set1_to_set2_extended(0x47), 0x6C, "Home");
        assert_eq!(set1_to_set2_extended(0x4F), 0x69, "End");
        assert_eq!(set1_to_set2_extended(0xFF), 0x00, "Unknown returns 0");
    }

    #[test]
    fn test_set1_to_set3_extended_spot_checks() {
        assert_eq!(set1_to_set3_extended(0x1C), 0x79, "Keypad Enter");
        assert_eq!(set1_to_set3_extended(0x38), 0x39, "RAlt");
        assert_eq!(set1_to_set3_extended(0x48), 0x63, "Up Arrow");
        assert_eq!(set1_to_set3_extended(0x50), 0x60, "Down Arrow");
        assert_eq!(set1_to_set3_extended(0x4B), 0x61, "Left Arrow");
        assert_eq!(set1_to_set3_extended(0x4D), 0x6A, "Right Arrow");
        assert_eq!(set1_to_set3_extended(0x52), 0x67, "Insert");
        assert_eq!(set1_to_set3_extended(0x53), 0x64, "Delete");
        assert_eq!(set1_to_set3_extended(0xFF), 0x00, "Unknown returns 0");
    }

    #[test]
    fn test_is_translation_enabled() {
        let mut kb = Keyboard::new();
        // Default: translation enabled (bit 6 set in controller_ram[0] = 0x47)
        assert!(kb.is_translation_enabled());

        // Disable translation
        kb.controller_ram[0] &= !0x40;
        assert!(!kb.is_translation_enabled());

        // Re-enable
        kb.controller_ram[0] |= 0x40;
        assert!(kb.is_translation_enabled());
    }

    #[test]
    fn test_effective_output_set() {
        let mut kb = Keyboard::new();

        // Default: set 1, translation enabled -> output set 1
        assert_eq!(kb.effective_output_set(), 1);

        // Set 2 with translation enabled -> output set 1 (8042 translates)
        kb.scancode_set = 2;
        assert_eq!(kb.effective_output_set(), 1);

        // Set 2 with translation disabled -> output set 2
        kb.controller_ram[0] &= !0x40;
        assert_eq!(kb.effective_output_set(), 2);

        // Set 3 ignores translation
        kb.scancode_set = 3;
        assert_eq!(kb.effective_output_set(), 3);

        // Set 1 ignores translation
        kb.scancode_set = 1;
        assert_eq!(kb.effective_output_set(), 1);
    }

    // ========================================================================
    // Scancode set translation tests (Stage 2/3)
    // ========================================================================

    #[test]
    fn test_translate_and_buffer_set1_passthrough() {
        let mut kb = Keyboard::new();
        // Default is set 1 with translation - should pass through unchanged
        kb.translate_and_buffer(0x1E); // A make
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x1E));

        kb.translate_and_buffer(0x9E); // A break
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x9E));
    }

    #[test]
    fn test_translate_and_buffer_set2_make() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable translation

        kb.translate_and_buffer(0x1E); // A make in Set 1
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x1C)); // A make in Set 2
        assert!(kb.translation_buffer.is_empty());
    }

    #[test]
    fn test_translate_and_buffer_set2_break() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        kb.translate_and_buffer(0x9E); // A break in Set 1 (0x1E | 0x80)
        assert_eq!(kb.translation_buffer.pop_front(), Some(0xF0)); // Break prefix
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x1C)); // A code
        assert!(kb.translation_buffer.is_empty());
    }

    #[test]
    fn test_translate_and_buffer_set2_extended() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        // Extended key: E0 48 (Up arrow make)
        kb.translate_and_buffer(0xE0);
        assert_eq!(kb.translation_buffer.pop_front(), Some(0xE0)); // E0 prefix preserved in Set 2

        kb.translate_and_buffer(0x48); // Up arrow make
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x75)); // Up in Set 2
        assert!(kb.translation_buffer.is_empty());
    }

    #[test]
    fn test_translate_and_buffer_set2_extended_break() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        // Extended break: E0 C8 (Up arrow break = E0 + 48|80)
        kb.translate_and_buffer(0xE0);
        assert_eq!(kb.translation_buffer.pop_front(), Some(0xE0)); // E0 preserved

        kb.translate_and_buffer(0xC8); // 0x48 | 0x80 = break
        assert_eq!(kb.translation_buffer.pop_front(), Some(0xF0)); // Break prefix
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x75)); // Up code
        assert!(kb.translation_buffer.is_empty());
    }

    #[test]
    fn test_translate_and_buffer_set3_make() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 3;

        kb.translate_and_buffer(0x1E); // A make
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x1C)); // A in Set 3
        assert!(kb.translation_buffer.is_empty());
    }

    #[test]
    fn test_translate_and_buffer_set3_break() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 3;

        kb.translate_and_buffer(0x9E); // A break
        assert_eq!(kb.translation_buffer.pop_front(), Some(0xF0)); // Break prefix
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x1C)); // A code
        assert!(kb.translation_buffer.is_empty());
    }

    #[test]
    fn test_translate_and_buffer_set3_extended_absorbs_e0() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 3;

        // E0 prefix should be absorbed in Set 3
        kb.translate_and_buffer(0xE0);
        assert!(kb.translation_buffer.is_empty()); // E0 absorbed!

        kb.translate_and_buffer(0x48); // Up arrow
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x63)); // Up in Set 3
        assert!(kb.translation_buffer.is_empty());
    }

    #[test]
    fn test_translate_and_buffer_set3_extended_break() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 3;

        kb.translate_and_buffer(0xE0);
        assert!(kb.translation_buffer.is_empty()); // E0 absorbed

        kb.translate_and_buffer(0xC8); // Up break
        assert_eq!(kb.translation_buffer.pop_front(), Some(0xF0));
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x63));
        assert!(kb.translation_buffer.is_empty());
    }

    #[test]
    fn test_translate_set2_with_translation_on_is_set1() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        // Translation is ON by default -> effective set = 1
        assert_eq!(kb.effective_output_set(), 1);

        kb.translate_and_buffer(0x1E); // A make
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x1E)); // Pass-through as Set 1

        kb.translate_and_buffer(0x9E); // A break
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x9E)); // Pass-through as Set 1
    }

    #[test]
    fn test_translate_and_buffer_e1_passthrough() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        kb.translate_and_buffer(0xE1); // E1 prefix (Pause key)
        assert_eq!(kb.translation_buffer.pop_front(), Some(0xE1)); // Passed through
        assert!(kb.translation_buffer.is_empty());
    }

    #[test]
    fn test_translate_and_buffer_unknown_key_passthrough() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        // SET1_TO_SET2[0x00] is 0 (unmapped) - should pass through raw
        kb.translate_and_buffer(0x00);
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x00));
        assert!(kb.translation_buffer.is_empty());
    }

    #[test]
    fn test_enqueue_input_scancodes_translates_set2() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable translation

        kb.enqueue_input_scancodes(&[0x1E]); // A make
        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0x1C)); // Translated to Set 2
    }

    #[test]
    fn test_enqueue_input_scancodes_translates_set2_break() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        kb.enqueue_input_scancodes(&[0x9E]); // A break
        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0xF0)); // Break prefix
        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0x1C)); // A code
    }

    #[test]
    fn test_command_response_not_translated() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        // Command responses use enqueue_scancodes directly, not enqueue_input_scancodes
        kb.enqueue_scancodes(&[0xFA]); // ACK - should NOT be translated
                                       // Variant flag should be `true` (CommandResponse).
        assert_eq!(kb.pop_keyboard_byte(), Some((0xFA, true)));
    }

    #[test]
    fn test_read_keyboard_output_translates_shared_queue() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        let shared = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&shared));

        // Inject Set 1 scancode into shared queue
        shared.inject_scancode(&[0x1E], 10, true); // A make

        // Read should translate to Set 2
        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0x1C));
    }

    #[test]
    fn test_read_keyboard_output_shared_queue_break() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        let shared = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&shared));

        shared.inject_scancode(&[0x9E], 10, false); // A break

        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0xF0));
        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0x1C));
    }

    #[test]
    fn test_read_keyboard_output_shared_set3_absorbs_e0() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 3;

        let shared = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&shared));

        // Inject extended key: E0 48 (Up arrow)
        shared.inject_scancode(&[0xE0, 0x48], 10, true);

        // Set 3: E0 absorbed, key translated to unique code
        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0x63)); // Up in Set 3
    }

    #[test]
    fn test_read_status_sees_translation_buffer() {
        // When a break code from the shared queue is translated to Set 2, it
        // produces two bytes (F0,xx). read_keyboard_output() pops the first byte
        // and leaves the second in translation_buffer. read_status() must report
        // output-buffer-full (bit 0) for that remaining byte.
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable 8042 translation -> raw Set 2 output

        let shared = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&shared));

        // Inject break code for 'A' via shared queue: 0x9E (Set 1 A break)
        // translate_and_buffer(0x9E) produces: F0, 1C (two bytes)
        // First read_data returns F0, leaving 1C in translation_buffer
        shared.inject_scancode(&[0x9E], 10, false);

        // Read first byte via port 0x60 -> F0 (break prefix)
        let b1 = kb.read_data();
        assert_eq!(b1, 0xF0, "First byte should be F0 break prefix");

        // Status should show output buffer full (bit 0) because 0x1C remains
        // in translation_buffer
        let status = kb.read_status();
        assert_eq!(
            status & 0x01,
            0x01,
            "Status bit 0 should be set while translated bytes remain in translation_buffer"
        );
    }

    #[test]
    fn test_set_defaults_clears_translation_state() {
        // 0xF6 (Set Defaults) should clear translate_e0_pending and
        // translation_buffer, just like 0xFF (Reset) does.
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable 8042 translation -> raw Set 2

        // Inject E0 prefix to set translate_e0_pending = true
        kb.translate_and_buffer(0xE0);
        assert!(kb.translate_e0_pending, "E0 should set pending flag");
        // Clear the E0 byte from translation buffer for cleaner test
        kb.translation_buffer.clear();

        // Send 0xF6 Set Defaults command
        kb.write_data(0xF6);

        // Drain the ACK (0xFA) — now in the output buffer (auto-staged)
        assert_eq!(kb.read_data(), 0xFA);

        // Now inject a normal key 'A' make (0x1E in Set 1)
        kb.enqueue_input_scancodes(&[0x1E]);

        // Should get normal Set 2 'A' (0x1C), NOT extended key behavior
        let byte = kb.pop_keyboard_byte().map(|(b, _)| b);
        assert_eq!(
            byte,
            Some(0x1C),
            "After 0xF6, normal key should not be treated as extended"
        );
    }

    #[test]
    fn test_scancode_set_change_clears_translation_state() {
        // Changing the scancode set via 0xF0 should clear translate_e0_pending
        // and translation_buffer to prevent stale state from corrupting output.
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable 8042 translation -> raw Set 2

        // Inject E0 prefix to set translate_e0_pending = true
        kb.translate_and_buffer(0xE0);
        assert!(kb.translate_e0_pending, "E0 should set pending flag");
        kb.translation_buffer.clear();

        // Send 0xF0 command (set scancode set), followed by 0x03 (switch to Set 3)
        kb.write_data(0xF0);
        // Drain ACK for 0xF0 command — auto-staged into output buffer
        assert_eq!(kb.read_data(), 0xFA);

        // Send the set number
        kb.write_data(0x03);
        // Drain ACK for set number — auto-staged into output buffer
        assert_eq!(kb.read_data(), 0xFA);

        // Now inject a normal key 'A' make (0x1E in Set 1)
        kb.enqueue_input_scancodes(&[0x1E]);

        // Should get normal Set 3 'A' (0x1C), NOT extended key behavior
        let byte = kb.pop_keyboard_byte().map(|(b, _)| b);
        assert_eq!(
            byte,
            Some(0x1C),
            "After set change, normal key should not be treated as extended"
        );
    }

    #[test]
    fn test_clear_resets_translation_state() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        // Partially process E0 prefix
        kb.translate_and_buffer(0xE0);
        assert!(!kb.translation_buffer.is_empty());

        kb.clear();

        assert!(kb.translation_buffer.is_empty());
        assert!(!kb.translate_e0_pending);
    }

    #[test]
    fn test_has_pending_includes_translation_buffer() {
        let mut kb = Keyboard::new();
        assert!(!kb.has_pending_scancodes());

        // Put something in the translation buffer directly
        kb.translation_buffer.push_back(0x42);
        assert!(kb.has_pending_scancodes());
    }

    #[test]
    fn test_read_data_checks_translation_buffer() {
        // Bug: read_data() did not include translation_buffer in kbd_pending,
        // so after consuming the first byte of a multi-byte translation from
        // the shared queue, subsequent read_data() calls (without checking
        // status) would return stale data_register instead of the remaining
        // translated bytes.
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Translation OFF -> effective output Set 2

        // Set up shared state and inject Up Arrow release (Set 1: E0 C8)
        let shared = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(shared.clone());
        shared.inject_scancode(&[0xE0, 0xC8], 0, false);

        // First read: read_keyboard_output reads E0 from shared, translates it.
        // Set 2 keeps E0 prefix -> translation_buffer=[0xE0], returns 0xE0.
        let b1 = kb.read_data();
        assert_eq!(
            b1, 0xE0,
            "First read should return E0 (Set 2 extended prefix)"
        );

        // Second read: shared has C8 remaining, translate_and_buffer(0xC8) produces
        // [0xF0, 0x75] (break prefix + Set 2 Up Arrow). Returns 0xF0, leaves 0x75.
        let b2 = kb.read_data();
        assert_eq!(
            b2, 0xF0,
            "Second read should return F0 (Set 2 break prefix)"
        );

        // Third read: shared queue is now empty, scancode_queue is empty.
        // translation_buffer has [0x75]. Without the fix, kbd_pending is false
        // and this returns stale data_register (0xF0).
        let b3 = kb.read_data();
        assert_eq!(
            b3, 0x75,
            "Third read should return 0x75 (Set 2 Up Arrow) from translation_buffer"
        );
    }

    #[test]
    fn test_keyboard_reset_clears_translation_state() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40;

        // Set up some translation state
        kb.translate_e0_pending = true;
        kb.translation_buffer.push_back(0x42);

        // 0xFF reset command
        kb.write_data(0xFF);

        assert!(!kb.translate_e0_pending);
        assert!(kb.translation_buffer.is_empty());
    }

    // =========================================================================
    // Special Key Injection Methods (lines ~1111-1152)
    // =========================================================================

    #[test]
    fn test_inject_pause_make() {
        let mut kb = Keyboard::new();
        kb.inject_pause_make();
        let expected = [0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5];
        let snap = kb.scancode_queue_snapshot();
        assert_eq!(snap.len(), expected.len());
        assert_eq!(snap.as_slice(), expected.as_slice());
    }

    #[test]
    fn test_inject_ctrl_break_make() {
        let mut kb = Keyboard::new();
        kb.inject_ctrl_break_make();
        assert_eq!(kb.scancode_queue_snapshot(), vec![0xE0, 0x46]);
    }

    #[test]
    fn test_inject_ctrl_break_break() {
        let mut kb = Keyboard::new();
        kb.inject_ctrl_break_break();
        assert_eq!(kb.scancode_queue_snapshot(), vec![0xE0, 0xC6]);
    }

    #[test]
    fn test_inject_print_screen_make() {
        let mut kb = Keyboard::new();
        kb.inject_print_screen_make();
        assert_eq!(kb.scancode_queue_snapshot(), vec![0xE0, 0x2A, 0xE0, 0x37]);
    }

    #[test]
    fn test_inject_print_screen_break() {
        let mut kb = Keyboard::new();
        kb.inject_print_screen_break();
        assert_eq!(kb.scancode_queue_snapshot(), vec![0xE0, 0xB7, 0xE0, 0xAA]);
    }

    #[test]
    fn test_inject_sysrq_make() {
        let mut kb = Keyboard::new();
        kb.inject_sysrq_make();
        assert_eq!(kb.scancode_queue_snapshot(), vec![0x54]);
    }

    #[test]
    fn test_inject_sysrq_break() {
        let mut kb = Keyboard::new();
        kb.inject_sysrq_break();
        assert_eq!(kb.scancode_queue_snapshot(), vec![0xD4]);
    }

    // =========================================================================
    // Typematic timing conversion (lines ~1160-1181)
    // =========================================================================

    #[test]
    fn test_typematic_delay_ms() {
        assert_eq!(Keyboard::typematic_delay_ms(0), 250);
        assert_eq!(Keyboard::typematic_delay_ms(1), 500);
        assert_eq!(Keyboard::typematic_delay_ms(2), 750);
        assert_eq!(Keyboard::typematic_delay_ms(3), 1000);
        // Masking: 0x07 & 0x03 = 3 -> 1000
        assert_eq!(Keyboard::typematic_delay_ms(0x07), 1000);
        // 0x04 & 0x03 = 0 -> 250
        assert_eq!(Keyboard::typematic_delay_ms(0x04), 250);
    }

    #[test]
    fn test_typematic_rate_ms() {
        assert_eq!(Keyboard::typematic_rate_ms(0), 33);
        assert_eq!(Keyboard::typematic_rate_ms(15), 125);
        assert_eq!(Keyboard::typematic_rate_ms(31), 500);
        // Masking: 0x20 & 0x1F = 0 -> 33
        assert_eq!(Keyboard::typematic_rate_ms(0x20), 33);
        // rate=8 -> 67
        assert_eq!(Keyboard::typematic_rate_ms(8), 67);
        // rate=24 -> 270
        assert_eq!(Keyboard::typematic_rate_ms(24), 270);
    }

    // =========================================================================
    // SharedKeyboardState typematic methods (lines ~167-174)
    // =========================================================================

    #[test]
    fn test_shared_keyboard_state_typematic_ms_defaults() {
        let state = SharedKeyboardState::new();
        // Default delay=1 -> 500ms (IBM AT power-on default)
        assert_eq!(state.get_typematic_delay_ms(), 500);
        // Default rate=0x0B -> 92ms (~10.9 Hz, IBM AT power-on default)
        assert_eq!(state.get_typematic_rate_ms(), 92);
    }

    #[test]
    fn test_shared_keyboard_state_typematic_ms_after_set() {
        let state = SharedKeyboardState::new();
        state.set_typematic(3, 31);
        assert_eq!(state.get_typematic_delay_ms(), 1000);
        assert_eq!(state.get_typematic_rate_ms(), 500);

        state.set_typematic(0, 0);
        assert_eq!(state.get_typematic_delay_ms(), 250);
        assert_eq!(state.get_typematic_rate_ms(), 33);
    }

    // =========================================================================
    // SharedKeyboardState Default trait (line ~370-374)
    // =========================================================================

    #[test]
    fn test_shared_keyboard_state_default_trait() {
        let state = SharedKeyboardState::default();
        assert!(!state.has_pending());
        assert!(state.is_keyboard_enabled());
        assert!(state.is_scanning_enabled());
    }

    // =========================================================================
    // InjectedKeySet Default trait (line ~64-68)
    // =========================================================================

    #[test]
    fn test_injected_key_set_default_trait() {
        let set = InjectedKeySet::default();
        assert!(!set.check_and_clear(0));
        assert!(!set.check_and_clear(127));
    }

    // =========================================================================
    // scancode_for_ascii - Escape and unmapped
    // =========================================================================

    #[test]
    fn test_scancode_for_ascii_escape() {
        assert_eq!(Keyboard::scancode_for_ascii(0x1B), 0x01);
    }

    #[test]
    fn test_scancode_for_ascii_punctuation_extra() {
        // Shifted number row characters
        assert_eq!(Keyboard::scancode_for_ascii(b'!'), 0x02);
        assert_eq!(Keyboard::scancode_for_ascii(b'#'), 0x04);
        assert_eq!(Keyboard::scancode_for_ascii(b'$'), 0x05);
        assert_eq!(Keyboard::scancode_for_ascii(b'%'), 0x06);
        assert_eq!(Keyboard::scancode_for_ascii(b'^'), 0x07);
        assert_eq!(Keyboard::scancode_for_ascii(b'&'), 0x08);
        assert_eq!(Keyboard::scancode_for_ascii(b'*'), 0x09);
        assert_eq!(Keyboard::scancode_for_ascii(b'('), 0x0A);
        assert_eq!(Keyboard::scancode_for_ascii(b')'), 0x0B);
        // Punctuation pairs
        assert_eq!(Keyboard::scancode_for_ascii(b'_'), 0x0C);
        assert_eq!(Keyboard::scancode_for_ascii(b'+'), 0x0D);
        assert_eq!(Keyboard::scancode_for_ascii(b'{'), 0x1A);
        assert_eq!(Keyboard::scancode_for_ascii(b'}'), 0x1B);
        assert_eq!(Keyboard::scancode_for_ascii(b':'), 0x27);
        assert_eq!(Keyboard::scancode_for_ascii(b'"'), 0x28);
        assert_eq!(Keyboard::scancode_for_ascii(b'~'), 0x29);
        assert_eq!(Keyboard::scancode_for_ascii(b'|'), 0x2B);
        assert_eq!(Keyboard::scancode_for_ascii(b'<'), 0x33);
        assert_eq!(Keyboard::scancode_for_ascii(b'>'), 0x34);
        assert_eq!(Keyboard::scancode_for_ascii(b'?'), 0x35);
    }

    // =========================================================================
    // Keyboard accessors
    // =========================================================================

    #[test]
    fn test_shared_keyboard_enabled_always_some() {
        // After the 2026-05-12 dual→single queue collapse the keyboard
        // always has a SharedKeyboardState. Default keyboard is enabled.
        let kb = Keyboard::new();
        assert!(kb.shared_keyboard_enabled());
    }

    #[test]
    fn test_shared_keyboard_enabled_some() {
        let mut kb = Keyboard::new();
        let shared = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(shared.clone());
        assert!(kb.shared_keyboard_enabled());
        // Disable via 0xAD command
        kb.write_command(0xAD);
        assert!(!kb.shared_keyboard_enabled());
    }

    #[test]
    fn test_irq1_enabled_default() {
        let kb = Keyboard::new();
        assert!(kb.irq1_enabled());
    }

    #[test]
    fn test_irq1_enabled_after_disable() {
        let mut kb = Keyboard::new();
        // Clear IRQ1 enable bit (bit 0 of controller RAM byte 0)
        kb.controller_ram[0] &= !0x01;
        assert!(!kb.irq1_enabled());
    }

    #[test]
    fn test_irq12_enabled_default() {
        let kb = Keyboard::new();
        assert!(kb.irq12_enabled());
    }

    #[test]
    fn test_irq12_enabled_after_toggle() {
        let mut kb = Keyboard::new();
        // Clear IRQ12 enable bit (bit 1)
        kb.controller_ram[0] &= !0x02;
        assert!(!kb.irq12_enabled());
        // Re-enable
        kb.controller_ram[0] |= 0x02;
        assert!(kb.irq12_enabled());
    }

    #[test]
    fn test_is_command_response_pending_after_self_test() {
        let mut kb = Keyboard::new();
        assert!(!kb.is_command_response_pending());
        kb.write_command(0xAA); // Self-test
        assert!(kb.is_command_response_pending());
    }

    #[test]
    fn test_controller_command_byte_default() {
        let kb = Keyboard::new();
        assert_eq!(kb.controller_command_byte(), 0x47);
    }

    #[test]
    fn test_pending_command_response_count() {
        let mut kb = Keyboard::new();
        assert_eq!(kb.pending_command_response_count(), 0);
        kb.enqueue_scancodes(&[0x1E, 0x9E]);
        assert_eq!(kb.pending_command_response_count(), 2);
    }

    #[test]
    fn test_pending_user_input_count_empty() {
        let kb = Keyboard::new();
        assert_eq!(kb.pending_user_input_count(), 0);
    }

    #[test]
    fn test_pending_user_input_count_with_shared() {
        let mut kb = Keyboard::new();
        let shared = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(shared.clone());
        assert_eq!(kb.pending_user_input_count(), 0);
        shared.inject_scancode(&[0x1E], 0, true);
        assert_eq!(kb.pending_user_input_count(), 1);
    }

    // =========================================================================
    // read_status - input_buffer_full flag
    // =========================================================================

    #[test]
    fn test_read_status_input_buffer_full() {
        let mut kb = Keyboard::new();
        // write_data sets input_buffer_full
        kb.write_data(0xEE); // Echo command
        let status = kb.read_status();
        assert_eq!(status & 0x02, 0x02, "Input buffer full flag should be set");
        // Second read should clear it
        let status2 = kb.read_status();
        assert_eq!(
            status2 & 0x02,
            0x00,
            "Input buffer full flag should be cleared"
        );
    }

    #[test]
    fn test_read_status_output_buffer_full() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        // Enqueue and stage so output buffer is full
        kb.enqueue_scancodes(&[0x1E]);
        kb.stage_output(&mut pic);
        let status = kb.read_status();
        assert_eq!(status & 0x01, 0x01, "Output buffer full flag should be set");
    }

    // =========================================================================
    // note_port60_read / take_port60_read_since_check
    // =========================================================================

    #[test]
    fn test_port60_read_tracking() {
        let mut kb = Keyboard::new();
        assert!(!kb.take_port60_read_since_check());
        kb.note_port60_read();
        assert!(kb.take_port60_read_since_check());
        // Should be cleared after take
        assert!(!kb.take_port60_read_since_check());
    }

    // =========================================================================
    // is_num_lock_on, is_caps_lock_on, is_scroll_lock_on — read LED state
    // (post-Stage-7 the BIOS shift_flags bits live in the DOS driver, so
    // these methods reflect the keyboard hardware's own LED register).
    // =========================================================================

    #[test]
    fn test_num_lock_reads_led_bit() {
        let mut kb = Keyboard::new();
        assert!(!kb.is_num_lock_on());
        kb.led_state = 0x02; // NumLock LED bit
        assert!(kb.is_num_lock_on());
    }

    #[test]
    fn test_caps_lock_reads_led_bit() {
        let mut kb = Keyboard::new();
        assert!(!kb.is_caps_lock_on());
        kb.led_state = 0x04; // CapsLock LED bit
        assert!(kb.is_caps_lock_on());
    }

    #[test]
    fn test_scroll_lock_toggle_flips_led() {
        let mut kb = Keyboard::new();
        assert!(!kb.is_scroll_lock_on());
        kb.toggle_scroll_lock();
        assert!(kb.is_scroll_lock_on());
        kb.toggle_scroll_lock();
        assert!(!kb.is_scroll_lock_on());
    }

    // =========================================================================
    // set_shared_mouse_state / shared_mouse_state
    // =========================================================================

    #[test]
    fn test_shared_mouse_state_none() {
        let kb = Keyboard::new();
        assert!(kb.shared_mouse_state().is_none());
    }

    #[test]
    fn test_shared_mouse_state_some() {
        let mut kb = Keyboard::new();
        let mouse_state = Arc::new(SharedMouseState::new());
        kb.set_shared_mouse_state(mouse_state);
        assert!(kb.shared_mouse_state().is_some());
    }

    // =========================================================================
    // read_data clears input_port_poll and command_response_pending
    // =========================================================================

    #[test]
    fn test_read_data_clears_input_port_poll() {
        let mut kb = Keyboard::new();
        kb.write_command(0xC1); // Sets input_port_poll
        assert!(kb.input_port_poll.is_some());
        // Enqueue data so read_data has something to return
        kb.enqueue_scancodes(&[0x42]);
        kb.read_data();
        assert!(kb.input_port_poll.is_none());
    }

    #[test]
    fn test_read_data_clears_command_response_pending() {
        let mut kb = Keyboard::new();
        kb.write_command(0xAA); // Self-test sets command_response_pending
        assert!(kb.is_command_response_pending());
        kb.read_data(); // Should clear it
        assert!(!kb.is_command_response_pending());
    }

    // =========================================================================
    // write_command 0xC1 / 0xC2 - input port poll in status
    // =========================================================================

    #[test]
    fn test_write_command_c1_input_port_poll_low() {
        let mut kb = Keyboard::new();
        // input_port defaults to 0x93
        kb.write_command(0xC1); // Low nibble poll
        let status = kb.read_status();
        // Low nibble of 0x93 = 0x03, placed in status bits 4-7
        assert_eq!(status & 0xF0, 0x03 << 4);
    }

    #[test]
    fn test_write_command_c2_input_port_poll_high() {
        let mut kb = Keyboard::new();
        // input_port defaults to 0x93
        kb.write_command(0xC2); // High nibble poll
        let status = kb.read_status();
        // High nibble of 0x93 = 0x09, placed in status bits 4-7
        assert_eq!(status & 0xF0, 0x09 << 4);
    }

    // =========================================================================
    // write_command 0xAB - keyboard interface test
    // =========================================================================

    #[test]
    fn test_write_command_ab_interface_test() {
        let mut kb = Keyboard::new();
        kb.write_command(0xAB);
        assert!(kb.is_command_response_pending());
        let result = kb.read_data();
        assert_eq!(result, 0x00, "Interface test should return 0x00 (no error)");
    }

    // =========================================================================
    // write_command 0xA9 - mouse port test
    // =========================================================================

    #[test]
    fn test_write_command_a9_mouse_port_test() {
        let mut kb = Keyboard::new();
        kb.write_command(0xA9);
        assert!(kb.is_command_response_pending());
        let result = kb.read_data();
        assert_eq!(
            result, 0x00,
            "Mouse port test should return 0x00 (no error)"
        );
    }

    // =========================================================================
    // write_command 0xC0 - read input port
    // =========================================================================

    #[test]
    fn test_write_command_c0_read_input_port() {
        let mut kb = Keyboard::new();
        kb.write_command(0xC0);
        assert!(kb.is_command_response_pending());
        let result = kb.read_data();
        assert_eq!(result, 0x93, "Input port should return default 0x93");
    }

    // =========================================================================
    // write_command 0xD0 - read output port
    // =========================================================================

    #[test]
    fn test_write_command_d0_read_output_port() {
        let mut kb = Keyboard::new();
        kb.write_command(0xD0);
        assert!(kb.is_command_response_pending());
        let result = kb.read_data();
        assert_eq!(result, 0xDF, "Output port should return default 0xDF");
    }

    // =========================================================================
    // Status bit 4: keyboard disabled flag
    // =========================================================================

    #[test]
    fn test_read_status_keyboard_disabled_flag() {
        let mut kb = Keyboard::new();
        let status = kb.read_status();
        assert_eq!(
            status & 0x10,
            0x00,
            "Bit 4 should be clear when keyboard is enabled"
        );
        kb.write_command(0xAD); // Disable keyboard
        let status = kb.read_status();
        assert_eq!(
            status & 0x10,
            0x10,
            "Bit 4 should be set when keyboard is disabled"
        );
    }

    // =========================================================================
    // Coverage tests: translate_and_buffer invalid scancode set (lines 773-775)
    // =========================================================================

    #[test]
    fn test_translate_and_buffer_invalid_scancode_set_passthrough() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 4; // Invalid, should hit default match arm
        kb.controller_ram[0] &= !0x40; // Disable translation
        kb.translate_and_buffer(0x1E);
        assert_eq!(kb.translation_buffer.pop_front(), Some(0x1E)); // Passed through raw
    }

    // =========================================================================
    // Coverage tests: scancode_for_ascii unmapped bytes (line 653)
    // =========================================================================

    #[test]
    fn test_scancode_for_ascii_unmapped_control_chars() {
        // Control chars not explicitly mapped (NUL, SOH, etc.)
        assert_eq!(Keyboard::scancode_for_ascii(0x00), 0x00);
        assert_eq!(Keyboard::scancode_for_ascii(0x01), 0x00);
        assert_eq!(Keyboard::scancode_for_ascii(0x7F), 0x00); // DEL
    }

    // =========================================================================
    // Coverage tests: tracing in write_data/write_command paths (lines 1791+)
    // =========================================================================

    #[test]
    fn test_write_data_and_command_with_tracing() {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("debug"))
            .with_writer(std::io::sink)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut kb = Keyboard::new();

        // 0xD4 -> mouse write (covers line 1791-1792)
        kb.write_command(0xD4);
        kb.write_data(0xF4);
        kb.mouse.read_output(); // drain ACK

        // 0x60 -> write controller RAM (covers line 1813-1815)
        kb.write_command(0x60);
        kb.write_data(0x47);

        // 0xD1 -> write output port (covers line 1824, 1826)
        kb.write_command(0xD1);
        kb.write_data(0xDF);

        // 0xD2 -> write keyboard output (covers line 1834-1835)
        kb.write_command(0xD2);
        kb.write_data(0x42);
        kb.read_data(); // drain

        // 0xD3 -> write mouse output (covers line 1843-1844)
        kb.write_command(0xD3);
        kb.write_data(0x5A);

        // Unknown controller command data (covers line 1849-1851)
        kb.pending_command = Some(PendingCommand::Controller(0xFF));
        kb.write_data(0x12);

        // 0xED -> LED data (covers line 1863-1864)
        kb.write_data(0xED);
        kb.read_data(); // ACK
        kb.write_data(0x07);
        kb.read_data(); // ACK

        // 0xF3 -> typematic data (covers line 1879-1880)
        kb.write_data(0xF3);
        kb.read_data(); // ACK
        kb.write_data(0x00);
        kb.read_data(); // ACK

        // 0xF0 -> scancode set data (covers line 1895-1896)
        kb.write_data(0xF0);
        kb.read_data(); // ACK
        kb.write_data(0x02);
        kb.read_data(); // ACK

        // Unknown keyboard command data (covers line 1901-1903)
        kb.pending_command = Some(PendingCommand::Keyboard(0x00));
        kb.write_data(0x12);

        // Set-3 key-type command stub (covers the valid-but-unmodelled ACK path)
        kb.write_data(0xF7);
        kb.read_data(); // ACK

        // 8042 commands with tracing
        // 0x20 -> read controller RAM (covers line 2011-2013)
        kb.write_command(0x20);
        kb.read_data();

        // 0x60 pending (covers line 2020-2021)
        kb.write_command(0x61);
        kb.write_data(0x00);

        // 0xC0 -> read input port (covers line 2085-2086)
        kb.write_command(0xC0);
        kb.read_data();

        // 0xC1 -> poll low nibble (covers line 2094-2095)
        kb.write_command(0xC1);

        // 0xC2 -> poll high nibble (covers line 2103-2104)
        kb.write_command(0xC2);

        // 0xD0 -> read output port (covers line 2112-2113)
        kb.write_command(0xD0);
        kb.read_data();
    }

    // =========================================================================
    // Coverage tests: shared inject scancode tracing (lines 221, 224-225, 230+)
    // =========================================================================

    #[test]
    fn test_shared_inject_scancode_with_tracing() {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("gp64::kbd::inject=debug"))
            .with_writer(std::io::sink)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let state = SharedKeyboardState::new();
        // 1-byte scancode (covers lines 230, 233-234)
        state.inject_scancode(&[0x1E], 10, true);
        // 2-byte scancode (covers lines 221, 224-225)
        state.inject_scancode(&[0xE0, 0x48], 20, true);
    }

    // =========================================================================
    // Coverage tests: read_data all paths with tracing (lines 1662-1698)
    // =========================================================================

    #[test]
    fn test_read_data_all_paths_with_tracing() {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("gp64::kbd::port60=trace"))
            .with_writer(std::io::sink)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        // Both pending path
        let mut kb = Keyboard::new();
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        kb.mouse.enqueue_output(0x5A);
        kb.data_register_source_was_mouse = false;
        assert_eq!(kb.read_data(), 0x5A);
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));

        // Kbd-only pending path (lines 1674-1676)
        let mut kb = Keyboard::new();
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));

        // Mouse-only pending path (lines 1686-1688)
        let mut kb = Keyboard::new();
        kb.mouse.enqueue_output(0x5A);
        assert_eq!(kb.read_data(), 0x5A);

        // No data / stale path (lines 1696, 1698)
        let mut kb = Keyboard::new();
        assert_eq!(kb.read_data(), 0);
    }

    // ========================================================================
    // Phase 6e: Event-driven keyboard transfer tests
    // ========================================================================

    #[test]
    fn test_stage_output_from_local_queue() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.keyboard_event_driven = true;
        assert!(kb.enqueue_input_scancodes(&[0x1E, 0x9E])); // A press, A release

        kb.stage_output(&mut pic);
        assert!(kb.output_buffer.is_some());

        assert_eq!(kb.output_buffer.unwrap().value, 0x1E);
        assert!(kb.p60_has_unread_data());

        // read_data consumes 0x1E and auto-stages 0x9E
        let v = kb.read_data();
        assert_eq!(v, 0x1E);
        // 0x9E is now auto-staged in output_buffer
        assert!(kb.p60_has_unread_data());
        assert_eq!(kb.output_buffer.unwrap().value, 0x9E);
        assert_eq!(kb.read_data(), 0x9E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        assert!(!kb.has_pending_scancodes());
    }

    #[test]
    fn test_stage_output_from_shared_queue() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        state.inject_scancode(&[0x1E], 30, true); // A press

        kb.stage_output(&mut pic);
        assert!(kb.output_buffer.is_some());
        assert_eq!(kb.output_buffer.unwrap().value, 0x1E);
        assert!(kb.p60_has_unread_data());
    }

    #[test]
    fn test_stage_output_skips_mouse_when_aux_disabled() {
        // Exercises a mouse-driver failure path under --real-boot:
        //   1. The mouse driver enables AUX, sends mouse RESET (0xD4 + 0xFF).
        //   2. Mouse responds with 0xFA + 0xAA + 0x00 in its output queue.
        //   3. The mouse driver times out reading the response, restores CCB to
        //      "AUX disabled + IRQ12 disabled" and exits.
        //   4. User presses a key; the kbd scancode lands in the local
        //      queue and stage_output runs.
        //
        // Before the fix, stage_output would happily drain a mouse byte
        // into OBF (alternation prefers mouse when last_was_mouse=false),
        // raise no IRQ12 because CCB bit 1 is clear, and OBF is now stuck
        // forever — every subsequent KeyboardTransfer no-ops, so kbd
        // scancodes pile up in the queue until the "queue full" warning
        // fires.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        // Stale mouse-reset response left in the mouse output queue.
        kb.mouse.enqueue_output(0xFA);
        kb.mouse.enqueue_output(0xAA);
        kb.mouse.enqueue_output(0x00);
        // CCB after mouse-driver restore: AUX disabled (bit 5), IRQ12 disabled
        // (bit 1 clear), IRQ1 enabled (bit 0), translation (bit 6).
        kb.controller_ram[0] = 0x61;
        // User keystroke arrives while AUX is disabled.
        // Use force variant for user-input tagging (IRQ1 path).
        kb.enqueue_input_scancodes_force(&[0x1E]);

        kb.stage_output(&mut pic);

        let ob = kb.output_buffer.expect("kbd byte must reach OBF");
        assert_eq!(
            ob.value, 0x1E,
            "OBF must contain the kbd byte, not a mouse byte"
        );
        assert_eq!(ob.source, OutputSource::Keyboard);
        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(1),
            "IRQ1 must fire so the guest BIOS reads port 0x60"
        );
        // Mouse bytes stay in the mouse queue until AUX is re-enabled.
        assert!(
            kb.mouse.has_output(),
            "mouse bytes preserved while AUX disabled"
        );
    }

    #[test]
    fn test_kbd_byte_not_blocked_when_mouse_irq12_masked_at_pic() {
        // CCB.AUXINT enables mouse interrupts at the 8042 while the 8259
        // slave PIC masks IRQ12. Queue mouse data, then a keyboard byte,
        // and verify that the keyboard can use the shared output buffer.
        //
        // Before the IMR-gate fix, stage_output's alternation policy could
        // pick the mouse byte (only the AUXINT side of the gate was
        // checked). The mouse byte landed in OBF, raise_irq(12) fired into
        // a masked line, and OBF was stuck full forever — every subsequent
        // keystroke piled up in the shared FIFO until "queue full" warned.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        // Default Pic: master mask 0xFC (IRQ0/1 unmasked), slave mask 0xFF.
        // IRQ12 is slave bit 4 — masked. Don't unmask it. CCB stays at the
        // boot default (0x47): IRQ1+AUXINT+SYSF+TRANSLATE all set, so the
        // pre-fix `mouse_pending` gate would say "yes, ready to stage."
        assert!(kb.irq12_enabled(), "AUXINT should be set by default");
        assert!(pic.is_irq_masked(12), "IRQ12 should be masked by default");

        // Mouse motion packet sitting in the queue (no driver to drain it).
        kb.mouse.enqueue_output(0x18);
        kb.mouse.enqueue_output(0x00);
        kb.mouse.enqueue_output(0x00);

        // User keystroke arrives.
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));
        state.inject_scancode(&[0x1E], 30, true);

        kb.stage_output(&mut pic);

        let ob = kb.output_buffer.expect("kbd byte must reach OBF");
        assert_eq!(
            ob.value, 0x1E,
            "OBF must contain the kbd byte, not the orphaned mouse byte"
        );
        assert_eq!(ob.source, OutputSource::Keyboard);
        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(1),
            "IRQ1 fires; mouse byte stays queued because no driver listens"
        );
        assert!(
            kb.mouse.has_output(),
            "mouse bytes wait in the mouse queue until IRQ12 is unmasked"
        );
    }

    #[test]
    fn test_shared_keyboard_byte_held_when_irq1_disabled() {
        // Mirrors the AUX-disabled mouse test (commit 11af66b9) for
        // CCB bit 0 (KBDINT). If a user keypress arrives while KBDINT is
        // clear, the byte must NOT be drained into OBF
        // — without an IRQ1 edge, OBF would stay stuck full forever.
        let mut kb = Keyboard::new();
        let shared = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&shared));
        let mut pic = Pic::new();

        // CCB with interrupts disabled: AUXDIS=1 (bit 5), KBDINT=0 (bit 0 clear),
        // translation enabled (bit 6).
        kb.controller_ram[0] = 0x60;

        shared.inject_scancode(&[0x1E], 10, true);
        kb.stage_output(&mut pic);

        assert!(
            !kb.p60_has_unread_data(),
            "shared byte must not stage with IRQ1 disabled"
        );
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "no IRQ may be asserted"
        );
        assert_eq!(
            shared.pending_count(),
            1,
            "byte stays in shared queue until KBDINT is re-enabled"
        );

        // Restoring KBDINT must let a subsequent stage_output succeed
        // and assert IRQ1.
        kb.controller_ram[0] = 0x61;
        kb.stage_output(&mut pic);
        assert!(
            kb.p60_has_unread_data(),
            "byte must reach OBF after KBDINT re-enabled"
        );
        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(1),
            "IRQ1 must fire so the guest ISR drains OBF"
        );
    }

    #[test]
    fn test_keyboard_disable_holds_queued_user_input_until_enable() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        kb.controller_ram[0] = 0x47; // KBDINT + AUXINT + translation enabled.
        assert!(kb.enqueue_input_scancodes_force(&[0x1E]));

        kb.write_command(0xAD);
        kb.stage_output(&mut pic);

        assert!(
            !kb.p60_has_unread_data(),
            "queued user input must not stage while keyboard interface is disabled"
        );
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "disabled keyboard interface must not assert IRQ1 for held input"
        );

        kb.enqueue_scancodes(&[0xFA]);
        kb.stage_output(&mut pic);
        let ob = kb.output_buffer.expect("command response must still stage");
        assert_eq!(ob.value, 0xFA);
        assert_eq!(ob.source, OutputSource::KeyboardReply);
        assert_eq!(kb.read_port60(&mut pic), 0xFA);

        let _ = kb.shared_state().take_irq1_pending();
        assert!(
            !kb.shared_state().irq1_pending(),
            "test must observe re-arm specifically from 0xAE"
        );

        kb.write_command(0xAE);
        assert!(
            kb.shared_state().irq1_pending(),
            "0xAE must re-arm pending user input for event-driven transfer"
        );
        kb.stage_output(&mut pic);

        let ob = kb
            .output_buffer
            .expect("held user input must stage after 0xAE");
        assert_eq!(ob.value, 0x1E);
        assert_eq!(ob.source, OutputSource::Keyboard);
        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(1),
            "re-enabled held input must assert IRQ1"
        );
    }

    #[test]
    fn test_keyboard_disable_holds_translated_continuation_until_enable() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        kb.scancode_set = 2;
        kb.controller_ram[0] = 0x07; // KBDINT + AUXINT, translation disabled.
        assert!(kb.enqueue_input_scancodes_force(&[0x9E]));

        kb.stage_output(&mut pic);
        let ob = kb.output_buffer.expect("first translated byte must stage");
        assert_eq!(ob.value, 0xF0);
        assert_eq!(ob.source, OutputSource::Keyboard);
        assert_eq!(kb.read_port60(&mut pic), 0xF0);
        assert_eq!(kb.translation_buffer_snapshot(), vec![0x1C]);

        kb.write_command(0xAD);
        kb.stage_output(&mut pic);
        assert!(
            !kb.p60_has_unread_data(),
            "0xAD must also hold translated continuation bytes"
        );
        assert_eq!(kb.translation_buffer_snapshot(), vec![0x1C]);

        kb.write_command(0xAE);
        kb.stage_output(&mut pic);
        let ob = kb
            .output_buffer
            .expect("translated continuation must stage after 0xAE");
        assert_eq!(ob.value, 0x1C);
        assert_eq!(ob.source, OutputSource::Keyboard);
    }

    #[test]
    fn test_ccb_write_kbdint_reenable_rearms_irq1_pending() {
        // After a CCB write that re-enables KBDINT, irq1_pending must be
        // re-set if the shared queue has bytes — so the main-loop trigger
        // (timing.rs) can schedule the next KeyboardTransfer. Without this
        // re-arm, deferred bytes from the just-closed disable window never
        // reach the guest until the next inject sets the flag.
        let mut kb = Keyboard::new();
        let shared = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&shared));

        // Disabled-IRQ1 state with a byte queued.
        kb.controller_ram[0] = 0x60; // KBDINT=0
        shared.inject_scancode(&[0x1E], 10, true);
        // Clear the irq1_pending flag set by inject_scancode so we can
        // observe the re-arm specifically from the CCB write path.
        let _ = shared.take_irq1_pending();
        assert!(!shared.irq1_pending());

        // Drive the public 8042 path: write 0x60 to port 0x64 (set up the
        // controller-RAM write command), then 0x47 to port 0x60 (KBDINT=1,
        // AUXINT=1, translation=1).
        kb.write_command(0x60);
        kb.write_data(0x47);

        assert!(kb.irq1_enabled(), "KBDINT must be re-enabled");
        assert!(
            shared.irq1_pending(),
            "irq1_pending must be re-armed by the CCB write"
        );
    }

    #[test]
    fn test_mouse_byte_held_when_irq12_disabled() {
        // Symmetric to test_shared_keyboard_byte_held_when_irq1_disabled.
        // The async-path AUXINT pre-check inside `stage_output` blocks
        // mouse-byte staging when CCB bit 1 (AUXINT) is clear, even if AUX
        // itself is enabled — without an IRQ12 edge, the byte would sit in
        // OBF forever and starve the keyboard via the `KeyboardTransfer`
        // no-op chain. Distinct from `test_stage_output_skips_mouse_when_aux_disabled`,
        // which exercises the CCB-bit-5 (AUX-disabled) gate. The synchronous
        // sibling paths (`auto_stage_no_pic`, `read_port60` eager restock)
        // intentionally bypass this gate — see
        // `test_mouse_command_response_stages_when_auxint_disabled`.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        // Unmask IRQ12 (slave bit 4) and the cascade line (master IRQ2)
        // so PIC state is observable; default Pic::new() masks the entire
        // slave PIC. Use the public 0x21 / 0xA1 mask-write path.
        pic.write_data(0, 0xF8); // master: unmask IRQ0/1/2 (cascade)
        pic.write_data(1, 0xEF); // slave: unmask IRQ12 only

        // CCB: AUX enabled (bit 5 clear), KBDINT=1 (bit 0 set), AUXINT=0
        // (bit 1 clear), translation enabled (bit 6).
        kb.controller_ram[0] = 0x41;

        kb.mouse.enqueue_output(0x08);
        kb.stage_output(&mut pic);

        assert!(
            !kb.p60_has_unread_data(),
            "mouse byte must not stage with IRQ12 disabled"
        );
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "no IRQ may be asserted"
        );
        assert!(
            kb.mouse.has_output(),
            "mouse byte stays in mouse queue until AUXINT is re-enabled"
        );

        // Restoring AUXINT must let the byte stage and assert IRQ12.
        kb.controller_ram[0] = 0x43; // AUXINT=1
        kb.stage_output(&mut pic);
        assert!(
            kb.p60_has_unread_data(),
            "byte must reach OBF after AUXINT re-enabled"
        );
        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(12),
            "IRQ12 must fire so the guest mouse ISR drains OBF"
        );
    }

    #[test]
    fn test_mouse_command_response_stages_when_auxint_disabled() {
        // A polling host can enumerate the PS/2 mouse with CCB.AUXINT clear.
        // It polls OBF and reads the data port directly without relying on
        // IRQ12. Mouse reset must deliver the complete BAT reply into OBF
        // synchronously regardless of AUXINT state.
        //
        // The async gate must not block synchronous staging after a mouse
        // command or eager restocking of multi-byte replies.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        pic.write_data(0, 0xF8);
        pic.write_data(1, 0xEF);

        // CCB: AUX enabled (bit 5 clear), KBDINT=1, AUXINT=0, translation
        // enabled: mouse RESET is serviced through polling rather than IRQ12.
        kb.controller_ram[0] = 0x41;
        assert!(!kb.irq12_enabled(), "precondition: AUXINT must be off");

        // Send mouse RESET via the public 8042 path.
        kb.write_command(0xD4);
        kb.write_data(0xFF);

        // First reply byte (0xFA ACK) must be staged into OBF synchronously
        // by `auto_stage_no_pic` inside `write_data`'s mouse-write tail.
        assert!(
            kb.p60_has_unread_data(),
            "FA ACK must reach OBF on the host's polling read"
        );
        let ack = kb.read_port60(&mut pic);
        assert_eq!(ack, 0xFA, "host must read FA");

        // Second byte (0xAA BAT-pass) must arrive via `read_port60`'s
        // eager restock for multi-byte mouse replies.
        assert!(
            kb.p60_has_unread_data(),
            "AA BAT-pass must reach OBF on the next polling read"
        );
        let bat = kb.read_port60(&mut pic);
        assert_eq!(bat, 0xAA, "host must read AA");

        // Third byte (0x00 device ID = standard PS/2 mouse).
        assert!(
            kb.p60_has_unread_data(),
            "00 device-ID must reach OBF on the third polling read"
        );
        let id = kb.read_port60(&mut pic);
        assert_eq!(id, 0x00, "host must read 00");

        // No IRQ12 should have fired throughout — AUXINT was off, and the
        // synchronous response path delivers via polling alone.
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "no IRQ may be asserted while AUXINT is off"
        );
    }

    #[test]
    fn test_mouse_command_response_raises_irq12_when_interrupt_path_open() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        pic.write_data(0, 0xF8); // master: unmask IRQ0/1/2 (cascade)
        pic.write_data(1, 0xEF); // slave: unmask IRQ12 only
        kb.controller_ram[0] = 0x43; // KBDINT + AUXINT + translation, aux enabled.

        kb.write_command(0xD4);
        kb.write_data(0xF4);

        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));
        assert_eq!(kb.read_status() & 0x20, 0x20);
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "write_data stages without a Pic; the emulator reconciles IRQ12 at the next boundary"
        );

        kb.raise_irq12_for_visible_output(&mut pic);

        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(12),
            "visible aux command response must interrupt when AUXINT/PIC are open"
        );
        assert_eq!(kb.read_irq12_output(&mut pic), Some(0xFA));
        assert!(!kb.p60_has_unread_data());
        assert_eq!(
            pic.diagnostic_snapshot().slave.irq_line & 0x10,
            0,
            "consuming the aux response must drop the IRQ12 line shadow (latched request retained under capture-at-assert)"
        );
    }

    #[test]
    fn test_mouse_command_response_remains_polled_when_auxint_disabled() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        pic.write_data(0, 0xF8);
        pic.write_data(1, 0xEF);
        kb.controller_ram[0] = 0x41; // AUX enabled, AUXINT disabled.

        kb.write_command(0xD4);
        kb.write_data(0xF4);

        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));
        assert_eq!(kb.read_status() & 0x20, 0x20);
        kb.raise_irq12_for_visible_output(&mut pic);
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "AUXINT-disabled command response remains poll-only"
        );
        assert_eq!(kb.read_port60(&mut pic), 0xFA);
        assert!(!kb.p60_has_unread_data());
    }

    #[test]
    fn test_mouse_reset_response_uses_irq12_byte_cadence_when_interrupt_path_open() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        pic.write_data(0, 0xF8);
        pic.write_data(1, 0xEF);
        kb.controller_ram[0] = 0x43; // AUX enabled, AUXINT enabled.

        kb.write_command(0xD4);
        kb.write_data(0xFF);

        kb.raise_irq12_for_visible_output(&mut pic);
        assert_eq!(pic.get_highest_priority_irq(), Some(12));
        assert_eq!(kb.read_irq12_output(&mut pic), Some(0xFA));
        assert!(!kb.p60_has_unread_data());

        kb.stage_output(&mut pic);
        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));
        assert_eq!(pic.get_highest_priority_irq(), Some(12));
        assert_eq!(kb.read_irq12_output(&mut pic), Some(0xAA));

        kb.stage_output(&mut pic);
        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));
        assert_eq!(pic.get_highest_priority_irq(), Some(12));
        assert_eq!(kb.read_irq12_output(&mut pic), Some(0x00));
        assert!(!kb.p60_has_unread_data());
        assert_eq!(
            pic.diagnostic_snapshot().slave.irq_line & 0x10,
            0,
            "IRQ12 line shadow must drop after the final aux byte (latched request retained under capture-at-assert)"
        );
    }

    #[test]
    fn test_aux_disabled_mouse_response_restocks_after_full_obf() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x60;

        kb.write_command(0xAA);
        assert_eq!(kb.output_source(), Some(OutputSource::ControllerReply));
        assert!(kb.p60_has_unread_data());

        kb.write_command(0xD4);
        kb.write_data(0xFF);

        assert_eq!(
            kb.output_source(),
            Some(OutputSource::ControllerReply),
            "existing OBF byte must remain first"
        );
        assert_eq!(kb.read_port60(&mut pic), 0x55);

        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));
        assert_eq!(kb.read_port60(&mut pic), 0xFA);
        assert_eq!(kb.read_port60(&mut pic), 0xAA);
        assert_eq!(kb.read_port60(&mut pic), 0x00);
        assert!(!kb.p60_has_unread_data());
        assert_eq!(pic.get_highest_priority_irq(), None);
    }

    #[test]
    fn test_aux_disabled_mouse_response_restocks_after_full_obf_read_data() {
        let mut kb = Keyboard::new();
        kb.controller_ram[0] = 0x60;

        kb.write_command(0xAA);
        kb.write_command(0xD4);
        kb.write_data(0xFF);

        assert_eq!(kb.read_data(), 0x55);
        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));
        assert_eq!(kb.read_data(), 0xFA);
        assert_eq!(kb.read_data(), 0xAA);
        assert_eq!(kb.read_data(), 0x00);
        assert!(!kb.p60_has_unread_data());
    }

    #[test]
    fn test_clear_cancels_disabled_aux_response_budget() {
        let mut kb = Keyboard::new();
        kb.controller_ram[0] = 0x60;

        kb.write_command(0xAA);
        kb.write_command(0xD4);
        kb.write_data(0xFF);
        assert!(kb.disabled_aux_polled_response_budget > 0);

        kb.clear();

        assert_eq!(kb.disabled_aux_polled_response_budget, 0);
        assert!(!kb.p60_has_unread_data());
        assert_eq!(
            kb.read_data(),
            0,
            "cleared one-shot response must not leak through read_data"
        );
    }

    #[test]
    fn test_aux_disabled_d4_discards_stale_command_response() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x60;

        kb.mouse.handle_command(0x00); // Stale unknown-command FE response.
        assert!(kb.mouse.front_output_is_command_response());

        kb.write_command(0xAA);
        assert_eq!(kb.output_source(), Some(OutputSource::ControllerReply));

        kb.write_command(0xD4);
        kb.write_data(0xE9); // Current status request: FA, status, resolution, sample rate.
        assert_eq!(kb.read_port60(&mut pic), 0x55);

        assert_eq!(
            kb.read_port60(&mut pic),
            0xFA,
            "current command ACK must be visible before stale FE"
        );
        assert_eq!(kb.read_port60(&mut pic), 0x00);
        assert_eq!(kb.read_port60(&mut pic), 0x02);
        assert_eq!(kb.read_port60(&mut pic), 100);
        assert!(!kb.mouse.has_output());
    }

    #[test]
    fn test_stale_aux_command_response_hidden_without_current_budget() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        kb.mouse.handle_command(0x00); // Stale FE command response.
        kb.controller_ram[0] = 0x61; // AUX disabled, KBDINT enabled.
        kb.enqueue_input_scancodes_force(&[0x1E]);

        kb.stage_output(&mut pic);

        let ob = kb.output_buffer.expect("keyboard byte must stage");
        assert_eq!(ob.value, 0x1E);
        assert_eq!(ob.source, OutputSource::Keyboard);
        assert_eq!(pic.get_highest_priority_irq(), Some(1));
        assert!(
            kb.mouse.has_output(),
            "stale aux command response must remain hidden while AUX is disabled"
        );
    }

    #[test]
    fn test_stale_visible_aux_response_dropped_for_user_input() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x47; // KBDINT + AUXINT + translation.

        kb.write_command(0xD4);
        kb.write_data(0xF2);
        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));

        // Movement bytes behind the stale command response must survive.
        kb.mouse.enqueue_output(0x08);
        kb.mouse.enqueue_output(0x00);
        kb.mouse.enqueue_output(0x00);

        assert!(kb.enqueue_input_scancodes_force(&[0x1E]));
        for _ in 1..STALE_AUX_RESPONSE_KEYBOARD_BLOCK_GRACE_ATTEMPTS {
            assert!(!kb.age_stale_aux_response_blocking_keyboard_input("test"));
        }
        assert!(kb.age_stale_aux_response_blocking_keyboard_input("test"));
        kb.stage_output(&mut pic);

        let ob = kb.output_buffer.expect("keyboard byte must stage");
        assert_eq!(ob.value, 0x1E);
        assert_eq!(ob.source, OutputSource::Keyboard);
        assert_eq!(
            pic.get_highest_priority_irq(),
            Some(1),
            "stalled keyboard input must raise IRQ1 after stale AUX reply is dropped"
        );
        assert!(
            kb.mouse.has_output(),
            "AUX movement/report packets must be preserved"
        );
        let packet = kb
            .mouse
            .read_output_tagged()
            .expect("preserved packet byte");
        assert_eq!(packet.value(), 0x08);
        assert!(
            !packet.is_command_response(),
            "queued command-response continuation must be discarded"
        );
    }

    #[test]
    fn test_fresh_aux_response_not_dropped_without_user_input() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x47;

        kb.write_command(0xD4);
        kb.write_data(0xF2);

        kb.stage_output(&mut pic);
        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));
        assert_eq!(kb.read_port60(&mut pic), 0xFA);
        assert_eq!(kb.read_port60(&mut pic), 0x00);
        assert!(!kb.p60_has_unread_data());
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "polled command response should not assert IRQ12"
        );
    }

    #[test]
    fn test_aux_response_does_not_block_keyboard_command_ack() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x43; // Keyboard, mouse, KBDINT, AUXINT enabled.

        kb.write_command(0xD4);
        kb.write_data(0xF4);
        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));
        assert_eq!(
            kb.read_status() & 0x20,
            0x20,
            "mouse ACK starts as an AUX-originated polled byte"
        );

        kb.write_data(0xF3);

        assert_eq!(
            kb.output_source(),
            Some(OutputSource::KeyboardReply),
            "keyboard command ACK must replace the abandoned AUX response"
        );
        assert_eq!(
            kb.read_status() & 0x20,
            0,
            "keyboard ACK must not be reported as AUX data"
        );
        assert_eq!(kb.read_port60(&mut pic), 0xFA);
        assert!(!kb.p60_has_unread_data());
    }

    #[test]
    fn test_fresh_aux_response_gets_grace_when_user_input_arrives() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x47;

        kb.write_command(0xD4);
        kb.write_data(0xF2);
        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));

        assert!(kb.enqueue_input_scancodes_force(&[0x1E]));
        kb.stage_output(&mut pic);

        assert_eq!(
            kb.output_source(),
            Some(OutputSource::AuxMouseResponse),
            "first collision with user input must preserve the fresh polled reply"
        );
        assert_eq!(kb.read_port60(&mut pic), 0xFA);
        assert_eq!(kb.read_port60(&mut pic), 0x00);
    }

    #[test]
    fn test_stale_aux_response_discards_matching_generation_only() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x47;

        kb.write_command(0xD4);
        kb.write_data(0xF2);
        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));

        // A newer command response queues while the older ACK still owns OBF.
        kb.write_command(0xD4);
        kb.write_data(0xF4);

        assert!(kb.enqueue_input_scancodes_force(&[0x1E]));
        for _ in 1..STALE_AUX_RESPONSE_KEYBOARD_BLOCK_GRACE_ATTEMPTS {
            assert!(!kb.age_stale_aux_response_blocking_keyboard_input("test"));
        }
        assert!(kb.age_stale_aux_response_blocking_keyboard_input("test"));

        kb.stage_output(&mut pic);
        assert_eq!(
            kb.output_source(),
            Some(OutputSource::AuxMouseResponse),
            "newer mouse command response must survive stale generation discard"
        );
        assert_eq!(kb.read_port60(&mut pic), 0xFA);

        kb.stage_output(&mut pic);
        assert_eq!(kb.output_source(), Some(OutputSource::Keyboard));
        assert_eq!(pic.get_highest_priority_irq(), Some(1));
    }

    #[test]
    fn test_deferred_mouse_command_response_stages_without_auxint_or_irq12() {
        // The mouse-command staging fix only covered an empty OBF: D4/F2 queued
        // ACK+ID and auto_stage_no_pic immediately staged ACK. In the real
        // run_tick path, OBF can already contain a controller reply when the
        // mouse command completes. The delayed mouse response must still be
        // staged for polling without waiting for AUXINT/IRQ12.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        pic.write_data(0, 0xF8);
        pic.write_data(1, 0xEF);

        // CCB: AUX enabled, KBDINT=1, AUXINT=0, translation enabled.
        kb.controller_ram[0] = 0x41;

        // Occupy OBF with a controller reply.
        kb.write_command(0xAA);
        assert!(kb.p60_has_unread_data());

        // Queue a mouse command response while OBF is still full.
        kb.write_command(0xD4);
        kb.write_data(0xF2);

        // Drain the controller reply first.
        assert_eq!(kb.read_port60(&mut pic), 0x55);
        assert!(!kb.p60_has_unread_data());

        // Later async staging must recognize the front mouse byte as a polled
        // command response and bypass the AUXINT/PIC gate.
        kb.stage_output(&mut pic);
        assert!(kb.p60_has_unread_data());
        assert_eq!(
            kb.read_status() & 0x20,
            0x20,
            "status bit 5 still marks the byte as AUX-originated"
        );
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "polled mouse command response must not require IRQ12"
        );

        assert_eq!(kb.read_port60(&mut pic), 0xFA);
        assert!(
            kb.p60_has_unread_data(),
            "read_port60 should eagerly restock the ID byte"
        );
        assert_eq!(kb.read_port60(&mut pic), 0x00);
    }

    #[test]
    fn test_mouse_command_response_restocks_after_keyboard_obf_is_drained() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x41; // AUX enabled, KBDINT=1, AUXINT=0.

        assert!(kb.enqueue_input_scancodes_force(&[0x1E]));
        kb.stage_output(&mut pic);
        assert_eq!(kb.output_source(), Some(OutputSource::Keyboard));

        // IDENTIFY queues FA + device ID while the keyboard byte still owns the
        // controller's one shared output buffer.
        kb.write_command(0xD4);
        kb.write_data(0xF2);
        assert_eq!(kb.output_source(), Some(OutputSource::Keyboard));
        assert!(kb.mouse.front_output_is_command_response());

        assert_eq!(kb.read_port60(&mut pic), 0x1E);
        assert_eq!(
            kb.output_source(),
            Some(OutputSource::AuxMouseResponse),
            "draining the foreign keyboard byte must expose the queued mouse ACK"
        );
        assert_eq!(
            kb.read_status() & 0x21,
            0x21,
            "the restocked response must publish OBF + AUXDATA"
        );
        assert_eq!(kb.read_port60(&mut pic), 0xFA);
        assert_eq!(kb.read_port60(&mut pic), 0x00);
    }

    #[test]
    fn test_keyboard_command_response_restock_via_read_port60() {
        // Keyboard IDENTIFY produces FA AB 83. Guest code polls these bytes
        // through status/port 60h reads, so command-response continuations
        // must become visible immediately after each read_port60 call. This
        // is intentionally command-response-only; user scancodes still wait
        // for the deferred IRQ1 path.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        kb.write_data(0xF2);

        assert_eq!(kb.output_source(), Some(OutputSource::KeyboardReply));
        assert_eq!(kb.read_port60(&mut pic), 0xFA);
        assert_eq!(pic.get_irr() & 0x02, 0, "ACK must not raise IRQ1");

        assert!(kb.p60_has_unread_data());
        assert_eq!(kb.output_source(), Some(OutputSource::KeyboardReply));
        assert_eq!(kb.read_port60(&mut pic), 0xAB);
        assert_eq!(pic.get_irr() & 0x02, 0, "ID byte must not raise IRQ1");

        assert!(kb.p60_has_unread_data());
        assert_eq!(kb.output_source(), Some(OutputSource::KeyboardReply));
        assert_eq!(kb.read_port60(&mut pic), 0x83);
        assert_eq!(pic.get_irr() & 0x02, 0, "ID byte must not raise IRQ1");
        assert!(!kb.p60_has_unread_data());
    }

    #[test]
    fn test_keyboard_command_response_drain_unblocks_user_input_irq1() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.keyboard_event_driven = true;

        kb.write_data(0xF2);
        assert_eq!(kb.read_port60(&mut pic), 0xFA);

        // User input arrives while command-response continuation bytes
        // still have priority in the keyboard output queue.
        assert!(kb.enqueue_input_scancodes_force(&[0x1C, 0x9C]));
        assert_eq!(kb.pending_user_input_count(), 2);

        assert_eq!(kb.read_port60(&mut pic), 0xAB);
        assert_eq!(kb.read_port60(&mut pic), 0x83);

        assert!(
            !kb.p60_has_unread_data(),
            "final command-response read must not eagerly stage user input"
        );
        assert_eq!(kb.pending_user_input_count(), 2);
        assert!(
            kb.keyboard_transfer_pending,
            "port 60h read should request deferred transfer for queued user input"
        );

        kb.stage_output(&mut pic);
        assert_eq!(kb.output_source(), Some(OutputSource::Keyboard));
        assert_ne!(pic.get_irr() & 0x02, 0, "user input must assert IRQ1");
        assert_eq!(kb.read_port60(&mut pic), 0x1C);
    }

    #[test]
    fn test_mouse_command_response_drops_stale_packet_ahead_of_ack() {
        // Stream packets that never reached OBF are invisible to a polling
        // driver. If such a stale packet remains ahead of a command ACK in
        // the emulated mouse FIFO, the mouse driver reads packet data instead of 0xFA
        // and declares "device not found". Host command traffic should drop
        // those stale reports before queuing the response.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x41; // AUXINT disabled, AUX enabled

        kb.mouse.enqueue_output(0x18);
        kb.mouse.enqueue_output(0x00);
        kb.mouse.enqueue_output(0x00);

        kb.write_command(0xD4);
        kb.write_data(0xF2);

        assert!(kb.p60_has_unread_data());
        assert_eq!(kb.read_port60(&mut pic), 0xFA);
        assert_eq!(kb.read_port60(&mut pic), 0x00);
        assert!(
            !kb.mouse.has_output(),
            "stale movement packet bytes must not remain behind the command response"
        );
    }

    #[test]
    fn test_ccb_write_auxint_reenable_rearms_irq12_pending() {
        // Symmetric to test_ccb_write_kbdint_reenable_rearms_irq1_pending.
        // After a CCB write that re-enables AUXINT (bit 1), irq12_pending
        // must be re-set if the mouse output queue has bytes — so the
        // emulator main loop schedules delivery without waiting for a new
        // mouse packet to set the flag.
        let mut kb = Keyboard::new();
        let shared_mouse = Arc::new(SharedMouseState::new());
        kb.set_shared_mouse_state(Arc::clone(&shared_mouse));

        // Disabled-IRQ12 state with a byte queued in the mouse output queue.
        kb.controller_ram[0] = 0x41; // KBDINT=1, AUXINT=0
        kb.mouse.enqueue_output(0x08);
        // Sanity: irq12_pending starts cleared (no inject path here).
        // SharedMouseState exposes only `take_` — there is no peek-only
        // `irq12_pending()` method (cf. SharedKeyboardState::irq1_pending).
        // Calling take on `false` is a no-op clear, so the precondition
        // is preserved.
        assert!(!shared_mouse.take_irq12_pending());

        // Drive the public 8042 path: write 0x60 to port 0x64, then 0x47
        // to port 0x60 (KBDINT=1, AUXINT=1, translation=1).
        kb.write_command(0x60);
        kb.write_data(0x47);

        assert!(kb.irq12_enabled(), "AUXINT must be re-enabled");
        assert!(
            shared_mouse.take_irq12_pending(),
            "irq12_pending must be re-armed by the CCB write"
        );
    }

    #[test]
    fn test_local_queue_drains_when_irq1_disabled() {
        // Regression: synthetic ACKs from kbd commands (e.g. 0xFA after
        // 0xED SETLEDS) are pushed into the LOCAL scancode_queue. They MUST
        // drain regardless of KBDINT state — the guest polls port 0x60 for
        // the ACK and never expects IRQ1. The shared-queue gate must not
        // affect this path.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x60; // KBDINT disabled

        kb.enqueue_scancodes(&[0xFA]);
        kb.stage_output(&mut pic);

        assert!(kb.p60_has_unread_data(), "synthetic ACK must reach OBF");
        let val = kb.read_port60(&mut pic);
        assert_eq!(val, 0xFA);
    }

    #[test]
    fn test_unified_fifo_command_response_drains_when_kbdint_off() {
        // Stage-3 invariant (2026-05-12): after the dual→single queue
        // collapse, a CommandResponse byte queued into the unified FIFO
        // must still reach port 60h even when CCB.KBDINT is clear —
        // exactly the behaviour the old local-queue path delivered.
        // This is the explicit "preserve the OBF-stuck-when-IRQ-masked
        // fix" check from the HLD's new-tests list.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x60; // KBDINT (bit 0) clear

        // 0xFA: the canonical ACK after a keyboard command (e.g. 0xF4).
        kb.enqueue_scancodes(&[0xFA]);
        assert_eq!(
            kb.shared_state.pending_command_response_count(),
            1,
            "ACK lands as CommandResponse in the unified FIFO"
        );
        assert_eq!(kb.shared_state.pending_user_input_count(), 0);

        kb.stage_output(&mut pic);
        assert!(kb.p60_has_unread_data());
        // No IRQ1 raised because KBDINT is disabled.
        assert_eq!(kb.output_source(), Some(OutputSource::KeyboardReply));
        assert_eq!(pic.get_highest_priority_irq(), None);
        assert_eq!(kb.read_port60(&mut pic), 0xFA);
    }

    #[test]
    fn test_unified_fifo_user_input_gated_by_kbdint() {
        // Stage-3 invariant (2026-05-12): user-input bytes queued via
        // host-key input (or main-thread enqueue_input_scancodes) MUST
        // stay in the unified FIFO when CCB.KBDINT is clear — staging
        // them into OBF without an IRQ1 edge would lock OBF forever
        // (the 2026-05-07 fix, commit 11af66b9). Preserves the gate
        // invariant across the dual→single queue collapse.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        // CCB during a host-controller dialogue: AUXDIS=1, KBDINT=0,
        // translation enabled (mirrors test_shared_keyboard_byte_held_when_irq1_disabled).
        kb.controller_ram[0] = 0x60;

        // Host-key style injection -> UserInput variant.
        kb.shared_state.inject_scancode(&[0x1E], 10, true);
        assert_eq!(kb.shared_state.pending_user_input_count(), 1);
        assert_eq!(kb.shared_state.pending_command_response_count(), 0);

        kb.stage_output(&mut pic);

        assert!(
            !kb.p60_has_unread_data(),
            "user-input byte must not stage with KBDINT disabled"
        );
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "no IRQ may be asserted"
        );
        assert_eq!(
            kb.shared_state.pending_user_input_count(),
            1,
            "byte stays queued until KBDINT re-enables"
        );

        // Re-enable KBDINT, re-arm irq1_pending (mirrors the CCB-write
        // re-arm path). pop_keyboard_byte now delivers the byte.
        kb.controller_ram[0] |= 0x01;
        kb.stage_output(&mut pic);
        assert!(kb.p60_has_unread_data());
        assert_eq!(kb.read_port60(&mut pic), 0x1E);
    }

    #[test]
    fn test_force_input_respects_kbdint_gate() {
        // Devil's-advocate regression check (2026-05-12 review): the
        // `enqueue_input_scancodes_force` method (called by the explicit raw
        // `SCANCODES` path via `Emulator::push_scancodes_force`) routes to the
        // UserInput variant of the unified FIFO. The
        // critical Stage-3 fix is that user input — even when force-injected
        // through the raw scancode escape hatch — must still respect the KBDINT
        // gate at pop time. Pre-fix, force-injection tagged
        // bytes as `CommandResponse` and drained regardless, which
        // caused IPC scancodes to bypass KBDINT and (worse) skip
        // Set 2/3 translation.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        // CCB.KBDINT (bit 0) clear: real-hardware would not see a
        // visible byte until the guest re-enables IRQ1.
        kb.controller_ram[0] = 0x00; // KBDDIS=0, KBDINT=0

        // Disable scanning so we can prove the *force* path bypasses
        // `scanning_enabled` (per the documented raw-scancode escape hatch),
        // but still observes the KBDINT gate.
        kb.write_data(0xF5); // kbd cmd: scanning disabled
                             // Drain the 0xFA ACK queued by 0xF5 before checking gates.
        kb.stage_output(&mut pic);
        let _ = kb.read_port60(&mut pic);
        assert!(!kb.is_scanning_enabled());

        assert!(
            kb.enqueue_input_scancodes_force(&[0x1E]),
            "force-input must bypass scanning_enabled"
        );
        assert_eq!(
            kb.pending_user_input_count(),
            1,
            "byte must land in UserInput variant, not CommandResponse"
        );
        assert_eq!(
            kb.pending_command_response_count(),
            0,
            "byte must NOT be tagged as CommandResponse"
        );

        kb.stage_output(&mut pic);
        assert!(
            !kb.p60_has_unread_data(),
            "byte must hold in the FIFO until KBDINT enables"
        );
        assert_eq!(
            pic.get_highest_priority_irq(),
            None,
            "no IRQ may fire while KBDINT is masked"
        );
        assert_eq!(
            kb.pending_user_input_count(),
            1,
            "byte stays in the FIFO until KBDINT re-enables"
        );

        // Re-enable KBDINT (matches the CCB-write re-arm path).
        kb.controller_ram[0] |= 0x01;
        kb.shared_state.set_irq1_pending();
        kb.stage_output(&mut pic);
        assert!(
            kb.p60_has_unread_data(),
            "byte must stage once KBDINT re-enables"
        );
        assert_eq!(kb.read_port60(&mut pic), 0x1E);
    }

    #[test]
    fn test_unified_fifo_user_input_fifo_order_preserved() {
        // Stage-3 invariant (2026-05-12): within the UserInput variant,
        // the unified FIFO preserves arrival order across both producer
        // paths (GUI-thread inject_scancode and main-thread
        // enqueue_input_scancodes). Cross-variant ordering is
        // CommandResponse-first by policy (see pop_byte doc), but this
        // test only exercises one variant, so insertion order rules.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        // Interleave GUI inject and main-thread enqueue_input_scancodes
        // — both push UserInput bytes to the unified FIFO.
        state.inject_scancode(&[0x1E], 10, true); // 'A' make (GUI)
        assert!(kb.enqueue_input_scancodes(&[0x30])); // 'B' make (main)
        state.inject_scancode(&[0x2E], 12, true); // 'C' make (GUI)
        assert!(kb.enqueue_input_scancodes(&[0x9E])); // 'A' break (main)

        // Drain via stage_output → read_port60 and verify FIFO order.
        for expected in [0x1E, 0x30, 0x2E, 0x9E] {
            kb.stage_output(&mut pic);
            assert!(
                kb.p60_has_unread_data(),
                "expected staged byte at FIFO front"
            );
            assert_eq!(kb.read_port60(&mut pic), expected);
        }
        assert!(!kb.shared_state.has_pending());
    }

    #[test]
    fn test_unified_fifo_command_response_priority_over_user_input() {
        // Stage-3 invariant (2026-05-12): CommandResponse bytes always
        // drain before UserInput, regardless of arrival order. This is
        // the "command responses should not be starved by user input"
        // property that the pre-collapse pop_keyboard_byte priority
        // (local queue > shared queue) provided. Preserved across the
        // dual→single queue collapse via SharedKeyboardState::pop_byte.
        //
        // OutputSource tagging distinguishes synthetic
        // keyboard-command ACKs (0xFA from 0xED SETLEDS, etc.) are now
        // mapped to `OutputSource::KeyboardReply` at staging time. They
        // are not ordinary scancodes, but they are keyboard-port device
        // bytes and raise IRQ1 when KBDINT is enabled.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        // User input arrives FIRST, command response SECOND. The
        // priority policy means the command response still pops first.
        kb.shared_state.inject_scancode(&[0xAA], 10, true);
        kb.enqueue_scancodes(&[0xFA]); // CommandResponse

        kb.stage_output(&mut pic);
        let ob = kb.output_buffer.expect("byte must reach OBF");
        assert_eq!(
            ob.value, 0xFA,
            "command response wins over older user input"
        );
        assert_eq!(ob.source, OutputSource::KeyboardReply);
        assert_ne!(
            pic.get_irr() & 0x02,
            0,
            "keyboard-device replies should assert IRQ1 when KBDINT is enabled"
        );

        // Drain and the user-input byte appears next.
        kb.read_port60(&mut pic);
        kb.stage_output(&mut pic);
        let ob = kb.output_buffer.expect("user-input byte must reach OBF");
        assert_eq!(ob.value, 0xAA);
        assert_eq!(ob.source, OutputSource::Keyboard);
    }

    #[test]
    fn test_stage_output_priority_local_over_shared() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.keyboard_event_driven = true;
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        state.inject_scancode(&[0xAA], 10, true); // Shared queue
        kb.enqueue_scancodes(&[0xBB]); // Local queue

        // Local queue has priority
        kb.stage_output(&mut pic);
        assert_eq!(kb.output_buffer.unwrap().value, 0xBB);
        // Consume, then stage next — shared queue
        kb.read_data(); // Clear output buffer
        kb.stage_output(&mut pic);
        assert_eq!(kb.output_buffer.unwrap().value, 0xAA);
    }

    #[test]
    fn test_stage_output_empty() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.stage_output(&mut pic);
        assert!(kb.output_buffer.is_none());
        assert!(!kb.p60_has_unread_data());
    }

    #[test]
    fn test_output_buffer_set_and_cleared() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        assert!(!kb.p60_has_unread_data());

        // stage_output fills the output buffer
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        kb.stage_output(&mut pic);
        assert!(kb.p60_has_unread_data());

        // read_data clears the output buffer
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        assert!(!kb.p60_has_unread_data());
    }

    #[test]
    fn test_schedule_transfer_needed() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        // No data at all: not needed
        assert!(!kb.schedule_transfer_needed());

        // Data pending, output buffer empty: needed
        assert!(kb.enqueue_input_scancodes(&[0x1E]));

        assert!(kb.schedule_transfer_needed());

        // After stage_output: output buffer full, not needed
        kb.stage_output(&mut pic);
        assert!(!kb.schedule_transfer_needed());

        // After read: output buffer empty, but no more data
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        assert!(!kb.schedule_transfer_needed());
    }

    #[test]
    fn test_schedule_transfer_needed_with_more_data() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.keyboard_event_driven = true;
        assert!(kb.enqueue_input_scancodes(&[0x1E, 0x9E]));

        // Stage first byte into output buffer
        kb.stage_output(&mut pic);
        // Output buffer full — not needed yet
        assert!(!kb.schedule_transfer_needed());

        // consume_output takes the byte but does NOT auto-stage
        let ob = kb.consume_output(&mut pic);
        let ob = ob.unwrap();
        assert_eq!(ob.value, 0x1E);
        assert_eq!(ob.source, OutputSource::Keyboard);
        // Output buffer empty, more data pending — transfer needed
        assert!(kb.schedule_transfer_needed());
    }

    #[test]
    fn test_note_port60_read_sets_transfer_pending_in_event_driven() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.keyboard_event_driven = true;
        assert!(kb.enqueue_input_scancodes(&[0x1E, 0x9E]));

        // Stage first byte into output buffer
        kb.stage_output(&mut pic);

        // Guest reads port 60h — should set transfer_pending
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        kb.note_port60_read();
        assert!(kb.keyboard_transfer_pending);
    }

    #[test]
    fn test_note_port60_read_sets_transfer_pending_for_mouse_packet_continuation() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.keyboard_event_driven = true;
        pic.write_data(0, 0xF8); // master: unmask IRQ0-2 (including cascade)
        pic.write_data(1, 0xEF); // slave: unmask IRQ12
        kb.controller_ram[0] = 0x47; // AUX enabled, AUXINT enabled

        kb.mouse.enqueue_output(0x09);
        kb.mouse.enqueue_output(0x00);
        kb.mouse.enqueue_output(0x00);

        kb.stage_output(&mut pic);
        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouse));

        assert_eq!(kb.read_port60(&mut pic), 0x09);
        assert!(!kb.p60_has_unread_data());
        assert!(kb.keyboard_transfer_pending);
        assert!(kb.schedule_transfer_needed_with_pic(&pic));
    }

    #[test]
    fn test_schedule_transfer_needed_with_pic_respects_irq12_mask_for_mouse_packets() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] = 0x47; // AUX enabled, AUXINT enabled
        kb.mouse.enqueue_output(0x09);

        assert!(
            !kb.schedule_transfer_needed_with_pic(&pic),
            "masked IRQ12 packet data must wait in the mouse queue"
        );

        pic.write_data(0, 0xF8); // master: unmask IRQ0-2 (including cascade)
        pic.write_data(1, 0xEF); // slave: unmask IRQ12
        assert!(kb.schedule_transfer_needed_with_pic(&pic));
    }

    #[test]
    fn test_note_port60_read_no_transfer_pending_without_event_driven() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        // keyboard_event_driven is false by default
        assert!(kb.enqueue_input_scancodes(&[0x1E, 0x9E]));

        kb.stage_output(&mut pic);
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        kb.note_port60_read();
        assert!(!kb.keyboard_transfer_pending);
    }

    #[test]
    fn test_note_port60_read_no_transfer_pending_when_no_more_data() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.keyboard_event_driven = true;
        assert!(kb.enqueue_input_scancodes(&[0x1E])); // Only one byte

        kb.stage_output(&mut pic);
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        kb.note_port60_read();
        // No more data — transfer_pending should be false
        assert!(!kb.keyboard_transfer_pending);
    }

    #[test]
    fn test_keyboard_reset_event_driven_defers_self_test() {
        let mut kb = Keyboard::new();
        kb.keyboard_event_driven = true;

        // Write 0xFF reset command
        kb.write_data(0xFF);

        // Should only get ACK, NOT self-test (0xAA) — that's deferred
        assert_eq!(kb.read_data(), 0xFA); // ACK
        assert!(kb.keyboard_reset_command_pending);

        // No more data (0xAA deferred to event)
        assert!(!kb.has_pending_scancodes());
    }

    #[test]
    fn test_keyboard_reset_synchronous_sends_aa() {
        let mut kb = Keyboard::new();
        // keyboard_event_driven is false (default)

        kb.write_data(0xFF);
        assert_eq!(kb.read_data(), 0xFA); // ACK
        assert_eq!(kb.read_data(), 0xAA); // Self-test passed
        assert!(!kb.keyboard_reset_command_pending);
    }

    #[test]
    fn test_tick_typematic_repeat_no_shared_state() {
        let mut kb = Keyboard::new();
        // No shared state → no-op, just verify it doesn't panic
        kb.tick_typematic_repeat();
        assert_eq!(kb.repeat_scancode, 0);
    }

    #[test]
    fn test_tick_typematic_repeat_generates_make_codes() {
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());

        // Press 'O' key (scancode 0x18) via shared state
        shared.inject_scancode(&[0x18], 0x18, true);
        // Initial press lands in shared queue (1 byte) and sets irq1_pending.
        assert_eq!(shared.pending_count(), 1);
        // Clear irq1_pending so we can verify the typematic tick re-arms it.
        let _ = shared.take_irq1_pending();

        // Tick through initial delay (default: 500ms, delay=1)
        for _ in 0..500 {
            kb.tick_typematic_repeat();
        }
        // After 500 ticks, one repeat make code should have landed in the
        // shared queue (initial + repeat = 2 bytes).
        assert_eq!(
            shared.pending_count(),
            2,
            "Expected repeat make code in shared queue after initial delay"
        );
        assert!(
            shared.irq1_pending(),
            "Typematic should set irq1_pending so the existing scheduler delivers"
        );

        // Drain both bytes out of the shared queue via pop_keyboard_byte.
        let _initial = kb.pop_keyboard_byte();
        let _repeat = kb.pop_keyboard_byte();
        let _ = shared.take_irq1_pending();

        // Tick through repeat rate (default rate 0x0B = ~92ms)
        for _ in 0..92 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(
            shared.pending_count(),
            1,
            "Expected another repeat make code in shared queue"
        );
        assert!(
            shared.irq1_pending(),
            "Subsequent repeat should also re-arm irq1_pending"
        );
    }

    #[test]
    fn test_tick_typematic_repeat_stops_on_release() {
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());

        // Press 'O' key — initial make code lands in shared queue.
        shared.inject_scancode(&[0x18], 0x18, true);
        let pre_release = shared.pending_count();
        assert_eq!(pre_release, 1);

        // Tick 100ms (within initial delay of 500ms)
        for _ in 0..100 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(
            shared.pending_count(),
            pre_release,
            "No repeat before delay expires"
        );

        // Release 'O' key — adds release byte to shared queue.
        shared.inject_scancode(&[0x98], 0x18, false);
        let after_release = shared.pending_count();
        assert_eq!(after_release, 2);

        // Tick past the delay — no repeat should fire.
        for _ in 0..600 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(
            shared.pending_count(),
            after_release,
            "No repeat after key release"
        );
    }

    #[test]
    fn test_tick_typematic_repeat_new_key_resets_delay() {
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());

        // Press 'O' key — initial 'O' make code lands in shared queue.
        shared.inject_scancode(&[0x18], 0x18, true);
        assert_eq!(shared.pending_count(), 1);

        // Tick 400ms (within initial delay of 500ms)
        for _ in 0..400 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(shared.pending_count(), 1);

        // Press 'P' key (scancode 0x19) — should reset delay; adds 1 byte.
        shared.inject_scancode(&[0x19], 0x19, true);
        let pre_repeat = shared.pending_count();
        assert_eq!(pre_repeat, 2);

        // Tick 400ms — still within NEW delay (500ms)
        for _ in 0..400 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(shared.pending_count(), pre_repeat, "Delay reset by new key");

        // Tick 100 more ms — NOW it should fire (total 500ms for 'P').
        for _ in 0..100 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(
            shared.pending_count(),
            pre_repeat + 1,
            "Repeat fires after full delay for new key"
        );

        // Drain the two initial press bytes ('O' then 'P') and verify the
        // next byte is the 'P' repeat (0x19), not anything stale.
        let _o = kb.pop_keyboard_byte().unwrap();
        let _p = kb.pop_keyboard_byte().unwrap();
        let (repeat, _) = kb.pop_keyboard_byte().unwrap();
        assert_eq!(repeat, 0x19);
    }

    #[test]
    fn test_tick_typematic_repeat_skipped_when_kbd_disabled() {
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());

        // Arm the held key while the kbd is enabled and tick once so the
        // new-key branch sets repeat_scancode and repeat_wait to non-zero
        // values. Without this, the post-bail asserts would be vacuous
        // (those fields are zero by default).
        shared.inject_scancode(&[0x18], 0x18, true);
        assert_eq!(shared.pending_count(), 1);
        kb.tick_typematic_repeat();
        assert_eq!(kb.repeat_scancode, 0x18);
        let initial_delay = Keyboard::typematic_delay_ms(kb.typematic_delay);
        assert_eq!(kb.repeat_wait, initial_delay - 1);

        // Now disable the keyboard interface (8042 0xAD) and tick once.
        kb.write_command(0xAD);
        let baseline = shared.pending_count();
        kb.tick_typematic_repeat();

        // The gate pauses repeat generation; it must not forget the held
        // key or reset the countdown. Real BIOS code can inhibit the
        // keyboard transiently, and held-key repeat must survive that.
        assert_eq!(kb.repeat_scancode, 0x18);
        assert_eq!(kb.repeat_wait, initial_delay - 1);

        // Tick well past the typematic delay — no repeats should fire.
        for _ in 0..1000 {
            kb.tick_typematic_repeat();
        }

        assert_eq!(
            shared.pending_count(),
            baseline,
            "No repeats while keyboard interface is disabled"
        );
        assert_eq!(
            kb.pending_command_response_count(),
            0,
            "Typematic must not fall back to local queue when kbd is disabled"
        );
        assert_eq!(kb.repeat_scancode, 0x18);
        assert_eq!(kb.repeat_wait, initial_delay - 1);
    }

    #[test]
    fn test_tick_typematic_repeat_skipped_when_scanning_disabled() {
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());

        // Arm the held key while scanning is enabled and tick once so the
        // new-key branch sets repeat_scancode and repeat_wait to non-zero
        // values. Without this, the post-bail asserts would be vacuous.
        shared.inject_scancode(&[0x18], 0x18, true);
        kb.tick_typematic_repeat();
        assert_eq!(kb.repeat_scancode, 0x18);
        let initial_delay = Keyboard::typematic_delay_ms(kb.typematic_delay);
        assert_eq!(kb.repeat_wait, initial_delay - 1);

        let baseline_shared = shared.pending_count();

        // Disable scanning via kbd command 0xF5 (queues 0xFA ACK locally).
        kb.write_data(0xF5);
        assert!(!kb.is_scanning_enabled());
        let baseline_local = kb.pending_command_response_count();

        // Tick once — the scan gate pauses the countdown without clearing
        // the physical held-key state.
        kb.tick_typematic_repeat();
        assert_eq!(kb.repeat_scancode, 0x18);
        assert_eq!(kb.repeat_wait, initial_delay - 1);

        // Tick well past the typematic delay — no repeats anywhere.
        for _ in 0..1000 {
            kb.tick_typematic_repeat();
        }

        assert_eq!(
            shared.pending_count(),
            baseline_shared,
            "No repeats in shared queue while scanning is disabled"
        );
        assert_eq!(
            kb.pending_command_response_count(),
            baseline_local,
            "No repeats in local queue while scanning is disabled"
        );
        assert_eq!(kb.repeat_scancode, 0x18);
        assert_eq!(kb.repeat_wait, initial_delay - 1);
    }

    #[test]
    fn test_tick_typematic_repeat_resumes_after_reenable() {
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());

        // Press the held key, then disable the keyboard interface.
        shared.inject_scancode(&[0x18], 0x18, true);
        kb.write_command(0xAD);
        let baseline = shared.pending_count();

        // Tick past the delay while disabled — nothing fires.
        for _ in 0..1000 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(shared.pending_count(), baseline);

        // Re-enable the keyboard interface.
        kb.write_command(0xAE);
        assert!(!kb.is_keyboard_disabled());

        // After re-enable, ticks past a fresh initial delay must produce one
        // repeat in the shared queue. Real hardware would also emit a fresh
        // make code on re-enable; we don't (known minor divergence noted
        // in the HLD). 500 ticks = one full initial delay; that's exactly
        // the boundary at which the first repeat fires (any more would
        // start a second repeat at the rate cadence).
        for _ in 0..500 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(
            shared.pending_count(),
            baseline + 1,
            "Repeat resumes after re-enable"
        );
    }

    #[test]
    fn test_tick_typematic_repeat_survives_transient_kbd_inhibits() {
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());

        shared.inject_scancode(&[0x18], 0x18, true);
        let baseline = shared.pending_count();

        // Model BIOS/driver code that briefly inhibits the keyboard clock
        // between enabled periods. The disabled ticks must not reset the
        // held-key countdown; otherwise the first repeat is unreachable.
        for _ in 0..500 {
            kb.write_command(0xAD);
            kb.tick_typematic_repeat();
            kb.write_command(0xAE);
            kb.tick_typematic_repeat();
        }

        assert_eq!(
            shared.pending_count(),
            baseline + 1,
            "Transient keyboard inhibits must not restart typematic delay"
        );
    }

    #[test]
    fn test_tick_typematic_repeat_resumes_after_scanning_reenable() {
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());
        kb.set_typematic(0, kb.typematic_rate); // delay=0 -> 250 ms

        // Press the held key, then disable scanning via kbd command 0xF5.
        shared.inject_scancode(&[0x1E], 0x1E, true);
        kb.write_data(0xF5);
        assert!(!kb.is_scanning_enabled());
        let baseline = shared.pending_count();

        // Tick past the delay while scanning is disabled — nothing fires.
        for _ in 0..500 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(shared.pending_count(), baseline);

        // Re-enable scanning via kbd command 0xF4.
        kb.write_data(0xF4);
        assert!(kb.is_scanning_enabled());

        // After re-enable, ticks past a fresh initial delay (250 ms for
        // delay=0) must produce one repeat in the shared queue.
        for _ in 0..250 {
            kb.tick_typematic_repeat();
        }
        assert!(
            shared.pending_count() > baseline,
            "Repeat resumes after scanning re-enable"
        );
    }

    #[test]
    fn test_tick_typematic_repeat_respects_kbdint_gate() {
        // We drive controller_ram[0] directly instead of write_command(0x60)
        // + write_data(0x46) because the re-enable write_data path calls
        // auto_stage_no_pic after the CCB write, which would stage a byte
        // to OBF without stage_output's IRQ1 raise — defeating this test's
        // assertion. The integration of typematic-accumulated bytes with
        // the production re-arm path is covered by
        // test_ccb_write_kbdint_reenable_rearms_irq1_pending.
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());
        let mut pic = Pic::new();

        // Default CCB is 0x47 (KBDINT=1, AUXDIS=0, translation=1). Clear
        // KBDINT bit 0 -> 0x46.
        kb.controller_ram[0] = 0x46;
        assert!(!kb.irq1_enabled());

        // Press the held key — initial make code lands in shared queue.
        shared.inject_scancode(&[0x18], 0x18, true);
        let baseline = shared.pending_count();
        assert_eq!(baseline, 1);

        // Tick past the initial delay so typematic injects one repeat.
        for _ in 0..500 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(
            shared.pending_count(),
            baseline + 1,
            "Typematic queues into shared even with KBDINT off"
        );

        // stage_output must NOT stage the byte while IRQ1 is masked — the
        // pop_keyboard_byte shared-branch gate returns None, so OBF stays
        // empty and no OBF_STAGED_NO_IRQ1 warning fires.
        kb.stage_output(&mut pic);
        assert!(
            kb.output_buffer.is_none(),
            "OBF must remain empty while KBDINT is masked"
        );
        assert_eq!(pic.get_irr() & 0x02, 0, "IRQ1 must not be raised");
        assert_eq!(
            shared.pending_count(),
            baseline + 1,
            "Bytes stay in shared queue until KBDINT is re-enabled"
        );

        // Re-enable KBDINT directly (avoid the write_data 0x60..=0x7F path
        // which would auto_stage_no_pic and bypass the PIC raise).
        kb.controller_ram[0] = 0x47;
        assert!(kb.irq1_enabled());

        // stage_output now drains the next shared byte and asserts IRQ1.
        kb.stage_output(&mut pic);
        assert!(
            kb.output_buffer.is_some(),
            "OBF should be staged after re-enable"
        );
        assert_ne!(
            pic.get_irr() & 0x02,
            0,
            "IRQ1 should be raised once KBDINT is restored"
        );
    }

    #[test]
    fn test_status_poll_stages_user_input_when_kbdint_disabled() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        // KBDINT off, keyboard interface enabled, translation on.
        kb.controller_ram[0] = 0x46;
        assert!(!kb.irq1_enabled());
        assert!(!kb.is_keyboard_disabled());

        assert!(kb.enqueue_input_scancodes_force(&[0x1E]));
        assert!(!kb.p60_has_unread_data());
        assert_eq!(kb.pending_user_input_count(), 1);

        let status = kb.read_status();

        assert_eq!(
            status & 0x01,
            0x01,
            "status polling must make the queued scancode visible"
        );
        assert_eq!(kb.output_source(), Some(OutputSource::Keyboard));
        assert_eq!(
            pic.get_irr() & 0x02,
            0,
            "polled staging must not assert IRQ1 while KBDINT is disabled"
        );
        assert_eq!(kb.read_port60(&mut pic), 0x1E);
        assert!(!kb.p60_has_unread_data());
        assert_eq!(kb.pending_user_input_count(), 0);
    }

    #[test]
    fn test_status_poll_respects_keyboard_disable_gate() {
        let mut kb = Keyboard::new();

        // KBDINT off, keyboard interface disabled, translation on.
        kb.controller_ram[0] = 0x56;
        assert!(!kb.irq1_enabled());
        assert!(kb.is_keyboard_disabled());

        assert!(kb.enqueue_input_scancodes_force(&[0x1E]));
        assert!(!kb.p60_has_unread_data());

        let status = kb.read_status();

        assert_eq!(
            status & 0x01,
            0,
            "disabled keyboard interface must keep user input out of OBF"
        );
        assert_eq!(kb.pending_user_input_count(), 1);
        assert!(!kb.p60_has_unread_data());
    }

    #[test]
    fn test_tick_typematic_repeat_translates_in_set2() {
        // HLD success criterion #4: typematic repeats must pick up the
        // shared queue's scancode-set translation. Injecting Set 1 0x1E
        // ('A' make) under Set 2 (translation off) must surface as 0x1C
        // when popped — same oracle as test_read_keyboard_output_translates_shared_queue.
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable 8042 translation -> raw Set 2

        // Arm a held 'A'. The initial byte is queued in Set 1 form.
        shared.inject_scancode(&[0x1E], 0x1E, true);

        // Tick past the initial delay so typematic injects a repeat.
        for _ in 0..500 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(
            shared.pending_count(),
            2,
            "Initial press + one typematic repeat in shared queue"
        );

        // Drain the initial 'A'. pop_keyboard_byte translates Set 1 0x1E
        // to Set 2 0x1C via translate_and_buffer in the shared-branch loop.
        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0x1C));
        // The repeat byte must also surface as the Set 2 value, proving
        // the typematic path goes through the same translation pipeline.
        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0x1C));
    }

    #[test]
    fn test_keyboard_event_driven_default_false() {
        let kb = Keyboard::new();
        assert!(!kb.keyboard_event_driven);
        assert!(!kb.keyboard_transfer_pending);
        assert!(!kb.keyboard_reset_command_pending);
        assert!(!kb.p60_has_unread_data());
    }

    // ========================================================================
    // IRQ-edge regression: consume→re-stage must produce a fresh IRQ1 edge
    // ========================================================================

    #[test]
    fn test_irq1_edge_trigger_two_scancodes() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        // Enable IRQ1 in CCB (bit 0)
        kb.controller_ram[0] |= 0x01;

        // Enqueue two keyboard bytes as user input (the IRQ1 path).
        // `enqueue_input_scancodes_force` is the primitive for explicit raw
        // SCANCODES/debug input and GUI special keys that intentionally bypass
        // `scanning_enabled`.
        kb.enqueue_input_scancodes_force(&[0x1E, 0x9E]); // A press, A release

        // Stage first byte → IRQ1 asserted (low→high edge)
        kb.stage_output(&mut pic);
        assert_eq!(kb.output_buffer.unwrap().value, 0x1E);
        assert_ne!(
            pic.get_irr() & 0x02,
            0,
            "IRQ1 should be requested after first stage"
        );

        // Consume first byte → IRQ1 deasserted (line goes low)
        let ob = kb.consume_output(&mut pic);
        assert_eq!(ob.unwrap().value, 0x1E);
        assert_eq!(
            pic.diagnostic_snapshot().master.irq_line & 0x02,
            0,
            "IRQ1 line shadow should drop after consume (latched request retained under capture-at-assert)"
        );

        // Stage second byte → IRQ1 asserted again (fresh low→high edge)
        kb.stage_output(&mut pic);
        assert_eq!(kb.output_buffer.unwrap().value, 0x9E);
        assert_ne!(
            pic.get_irr() & 0x02,
            0,
            "IRQ1 should be re-requested after second stage"
        );
    }

    #[test]
    fn test_read_port60_does_not_eagerly_stage_next_byte() {
        // read_port60 must NOT stage the next byte. On real AT 8042 hardware,
        // after the CPU drains OBF, a subsequent port 0x60 read returns the
        // latched data register value (the same byte) — not the next FIFO byte.
        // The next byte is only delivered after a delay, via either F000:FF10
        // (HLE trap path) or KeyboardTransfer (event-driven path).
        //
        // An INT 09h handler can read the data port and chain to another
        // handler that reads it again. Both reads must see the latched byte
        // until the controller transfers the next queued scancode.
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();
        kb.controller_ram[0] |= 0x01;
        // Use user-input variant: the test is about scancode IRQ1 flow,
        // not 8042/keyboard command-response replies.
        kb.enqueue_input_scancodes_force(&[0x1E, 0x9E]);
        kb.stage_output(&mut pic);
        assert_eq!(kb.output_buffer.unwrap().value, 0x1E);

        // First read consumes 0x1E
        let v = kb.read_port60(&mut pic);
        assert_eq!(v, 0x1E);
        // OB is empty after consume; IRQ1 line was lowered.
        assert!(kb.output_buffer.is_none());
        assert_eq!(
            pic.diagnostic_snapshot().master.irq_line & 0x02,
            0,
            "IRQ1 line shadow should drop after consuming the only staged byte (latched request retained under capture-at-assert)"
        );

        // Chained re-read returns the same byte via the data register —
        // matching real AT 8042 behavior when the chained ISR re-reads.
        let v2 = kb.read_port60(&mut pic);
        assert_eq!(v2, 0x1E, "Chained re-read must return the latched byte");

        // Explicit stage_output (e.g. via F000:FF10 or KeyboardTransfer) is
        // what advances to the next byte.
        kb.stage_output(&mut pic);
        assert_eq!(kb.output_buffer.unwrap().value, 0x9E);
        assert_ne!(
            pic.get_irr() & 0x02,
            0,
            "IRQ1 should be re-asserted after explicit stage_output"
        );
    }

    // ========================================================================
    // pending_controller_reply: replies must not be misattributed as scancodes
    // ========================================================================

    #[test]
    fn test_controller_reply_skips_ahead_of_queued_scancodes() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        // Queue a genuine user-input scancode BEFORE the controller command.
        // (`enqueue_scancodes` is for keyboard-MCU command responses now —
        // for testing controller-reply vs. user-input priority, use the
        // user-input force path.)
        kb.enqueue_input_scancodes_force(&[0x1E]); // A key press in queue

        // Issue a controller command that generates a reply
        kb.write_command(0xAA); // Self-test → reply 0x55

        // The reply should be staged (not the scancode)
        assert!(kb.output_buffer.is_some());
        let ob = kb.output_buffer.unwrap();
        assert_eq!(
            ob.value, 0x55,
            "Controller reply should be staged, not scancode"
        );
        assert!(matches!(ob.source, OutputSource::ControllerReply));

        // Consume the reply, then stage → now the scancode should appear
        kb.consume_output(&mut pic);
        kb.stage_output(&mut pic);
        assert_eq!(
            kb.output_buffer.unwrap().value,
            0x1E,
            "Scancode should be staged after reply is consumed"
        );
        assert!(matches!(
            kb.output_buffer.unwrap().source,
            OutputSource::Keyboard
        ));
    }

    #[test]
    fn test_irq1_output_ignores_controller_reply() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        kb.write_command(0xAA);

        let ob = kb.output_buffer.expect("controller reply must be staged");
        assert_eq!(ob.value, 0x55);
        assert_eq!(ob.source, OutputSource::ControllerReply);
        assert_eq!(
            kb.read_irq1_output(&mut pic),
            None,
            "IRQ1 keyboard service must not consume controller replies"
        );
        assert_eq!(
            kb.output_buffer
                .expect("reply should remain for polling")
                .value,
            0x55
        );
        assert_eq!(kb.read_port60(&mut pic), 0x55);
    }

    #[test]
    fn test_irq1_output_consumes_keyboard_reply_without_scancode() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        kb.enqueue_scancodes(&[0xFA]);
        kb.stage_output(&mut pic);

        let ob = kb.output_buffer.expect("keyboard reply must be staged");
        assert_eq!(ob.value, 0xFA);
        assert_eq!(ob.source, OutputSource::KeyboardReply);
        assert_ne!(
            pic.get_irr() & 0x02,
            0,
            "keyboard replies should request IRQ1 when KBDINT is enabled"
        );
        assert_eq!(
            kb.read_irq1_output(&mut pic),
            None,
            "IRQ1 keyboard service must consume replies but not dispatch scancodes"
        );
        assert!(
            !kb.p60_has_unread_data(),
            "keyboard reply should be drained by the IRQ1 helper"
        );
        assert_eq!(
            pic.diagnostic_snapshot().master.irq_line & 0x02,
            0,
            "IRQ1 line shadow should drop after consuming the reply (latched request retained under capture-at-assert)"
        );
    }

    #[test]
    fn test_visible_keyboard_reply_asserts_irq1_after_kbdint_reenable() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        kb.controller_ram[0] = 0x60; // KBDINT disabled, keyboard interface enabled.
        kb.enqueue_scancodes(&[0xAA]);
        kb.stage_output(&mut pic);

        let ob = kb.output_buffer.expect("keyboard reply must be visible");
        assert_eq!(ob.value, 0xAA);
        assert_eq!(ob.source, OutputSource::KeyboardReply);
        assert_eq!(
            pic.get_irr() & 0x02,
            0,
            "KBDINT disabled must suppress the initial IRQ1 edge"
        );

        kb.controller_ram[0] = 0x61; // Re-enable KBDINT while OBF is already full.
        kb.raise_irq1_for_visible_output(&mut pic);

        assert_ne!(
            pic.get_irr() & 0x02,
            0,
            "visible keyboard reply must assert IRQ1 once KBDINT is enabled"
        );
    }

    #[test]
    fn test_irq12_output_consumes_only_aux_mouse_byte() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        pic.write_data(0, 0xF8); // Unmask IRQ0-2, including slave cascade.
        pic.write_data(1, 0xEF); // Unmask IRQ12 on the slave PIC.
        kb.mouse.enqueue_output(0x09);
        kb.stage_output(&mut pic);

        let ob = kb.output_buffer.expect("aux byte must be staged");
        assert_eq!(ob.value, 0x09);
        assert_eq!(ob.source, OutputSource::AuxMouse);
        assert_eq!(pic.get_highest_priority_irq(), Some(12));
        assert_eq!(kb.read_irq12_output(&mut pic), Some(0x09));
        assert!(
            !kb.p60_has_unread_data(),
            "IRQ12 helper must drain the aux OBF byte"
        );
        assert_eq!(
            pic.diagnostic_snapshot().slave.irq_line & 0x10,
            0,
            "IRQ12 line shadow must drop after consume (latched request retained under capture-at-assert)"
        );
    }

    #[test]
    fn test_irq12_output_consumes_aux_mouse_response() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        pic.write_data(0, 0xF8);
        pic.write_data(1, 0xEF);
        kb.controller_ram[0] = 0x43;

        kb.write_command(0xD4);
        kb.write_data(0xF4);
        kb.raise_irq12_for_visible_output(&mut pic);

        assert_eq!(kb.output_source(), Some(OutputSource::AuxMouseResponse));
        assert_eq!(pic.get_highest_priority_irq(), Some(12));
        assert_eq!(
            kb.read_irq12_output(&mut pic),
            Some(0xFA),
            "IRQ12 helper must consume aux-origin command replies too"
        );
        assert!(!kb.p60_has_unread_data());
        assert_eq!(
            pic.diagnostic_snapshot().slave.irq_line & 0x10,
            0,
            "IRQ12 line shadow must drop after consume (latched request retained under capture-at-assert)"
        );
    }

    #[test]
    fn test_irq12_output_ignores_keyboard_and_controller_reply() {
        let mut kb = Keyboard::new();
        let mut pic = Pic::new();

        kb.enqueue_input_scancodes_force(&[0x1E]);
        kb.stage_output(&mut pic);
        assert_eq!(
            kb.read_irq12_output(&mut pic),
            None,
            "IRQ12 helper must not consume keyboard bytes"
        );
        assert_eq!(
            kb.output_buffer
                .expect("keyboard byte should remain visible")
                .source,
            OutputSource::Keyboard
        );
        assert_eq!(kb.read_port60(&mut pic), 0x1E);

        kb.write_command(0xAA);
        assert_eq!(
            kb.read_irq12_output(&mut pic),
            None,
            "IRQ12 helper must not consume controller replies"
        );
        assert_eq!(
            kb.output_buffer
                .expect("controller reply should remain visible")
                .source,
            OutputSource::ControllerReply
        );
        assert_eq!(kb.read_port60(&mut pic), 0x55);
    }

    #[test]
    fn test_controller_reply_read_ccb_with_queued_scancodes() {
        let mut kb = Keyboard::new();

        // Queue scancodes first
        assert!(kb.enqueue_input_scancodes(&[0x2A, 0x1E]));

        // Read CCB (command 0x20)
        kb.write_command(0x20);

        // Reply must be the CCB value, not a scancode
        let v = kb.read_data();
        assert_eq!(v, kb.controller_ram[0], "Should read CCB, not a scancode");
        assert_eq!(
            kb.last_port60_read_source,
            Some(OutputSource::ControllerReply)
        );

        assert_eq!(kb.read_data(), 0x2A);
        assert_eq!(kb.read_data(), 0x1E);
        assert_eq!(kb.last_port60_read_source, Some(OutputSource::Keyboard));
        assert!(!kb.has_pending_scancodes());
    }

    // =========================================================================
    // INT 16h AH=03h typematic enable/disable (Stage 1)
    // =========================================================================

    #[test]
    fn test_set_typematic_sets_values() {
        let mut kb = Keyboard::new();
        kb.set_typematic(2, 15);
        assert_eq!(kb.get_typematic_delay(), 2);
        assert_eq!(kb.get_typematic_rate(), 15);

        // Verify masking: delay > 3 is masked to 2 bits, rate > 31 is masked to 5 bits
        kb.set_typematic(0xFF, 0xFF);
        assert_eq!(kb.get_typematic_delay(), 3); // 0xFF & 0x03
        assert_eq!(kb.get_typematic_rate(), 31); // 0xFF & 0x1F
    }

    #[test]
    fn test_set_typematic_syncs_shared_state() {
        let mut kb = Keyboard::new();
        let state = Arc::new(SharedKeyboardState::new());
        kb.set_shared_state(Arc::clone(&state));

        kb.set_typematic(3, 20);
        assert_eq!(state.get_typematic_delay(), 3);
        assert_eq!(state.get_typematic_rate(), 20);
    }

    #[test]
    fn test_set_typematic_reenables_after_disable() {
        let mut kb = Keyboard::new();
        assert!(kb.is_typematic_enabled());

        kb.disable_typematic_repeat();
        assert!(!kb.is_typematic_enabled());

        // set_typematic re-enables
        kb.set_typematic(1, 10);
        assert!(kb.is_typematic_enabled());
        assert_eq!(kb.get_typematic_delay(), 1);
        assert_eq!(kb.get_typematic_rate(), 10);
    }

    #[test]
    fn test_disable_typematic_repeat() {
        let mut kb = Keyboard::new();

        // Set up some repeat state
        kb.repeat_scancode = 0x1E;
        kb.repeat_wait = 200;

        kb.disable_typematic_repeat();
        assert!(!kb.is_typematic_enabled());
        assert_eq!(kb.repeat_scancode, 0);
        assert_eq!(kb.repeat_wait, 0);
    }

    #[test]
    fn test_tick_typematic_disabled() {
        let shared = Arc::new(SharedKeyboardState::new());
        let mut kb = Keyboard::new();
        kb.set_shared_state(shared.clone());

        // Press a key
        shared.inject_scancode(&[0x18], 0x18, true);

        // Disable typematic
        kb.disable_typematic_repeat();

        // Tick well past any delay — nothing should happen
        for _ in 0..1000 {
            kb.tick_typematic_repeat();
        }
        assert_eq!(
            kb.pending_command_response_count(),
            0,
            "No repeat when typematic is disabled"
        );
    }

    #[test]
    fn test_default_typematic_values() {
        let kb = Keyboard::new();
        assert_eq!(
            kb.get_typematic_delay(),
            1,
            "Default delay should be 1 (500ms, IBM AT power-on default)"
        );
        assert_eq!(
            kb.get_typematic_rate(),
            0x0B,
            "Default rate should be 0x0B (~10.9 Hz, IBM AT power-on default)"
        );
        assert!(
            kb.is_typematic_enabled(),
            "Typematic should be enabled by default"
        );
    }

    #[test]
    fn test_0xf3_does_not_reenable_typematic() {
        let mut kb = Keyboard::new();
        kb.disable_typematic_repeat();
        assert!(!kb.is_typematic_enabled());

        // Send 0xF3 command followed by data byte
        kb.write_data(0xF3);
        kb.write_data(0x65); // delay=3, rate=5

        // Rate/delay should be updated
        assert_eq!(kb.get_typematic_delay(), 3);
        assert_eq!(kb.get_typematic_rate(), 5);
        // But typematic should still be disabled
        assert!(!kb.is_typematic_enabled());
    }

    #[test]
    fn test_0xf6_reset_reenables_typematic() {
        let mut kb = Keyboard::new();
        kb.disable_typematic_repeat();
        assert!(!kb.is_typematic_enabled());

        kb.write_data(0xF6); // Set defaults
        assert!(kb.is_typematic_enabled());
        assert_eq!(kb.get_typematic_delay(), 1);
        assert_eq!(kb.get_typematic_rate(), 0x0B);
    }

    #[test]
    fn test_0xff_reset_reenables_typematic() {
        let mut kb = Keyboard::new();
        kb.disable_typematic_repeat();
        assert!(!kb.is_typematic_enabled());

        kb.write_data(0xFF); // Reset
        assert!(kb.is_typematic_enabled());
        assert_eq!(kb.get_typematic_delay(), 1);
        assert_eq!(kb.get_typematic_rate(), 0x0B);
    }

    // ==================== 8042 controller commands ====================

    #[test]
    fn test_write_command_read_controller_ram() {
        let mut kb = Keyboard::new();
        // 0x20 reads controller RAM byte 0 (command byte)
        kb.write_command(0x20);
        // Should stage a reply; read it via read_data
        let data = kb.read_data();
        assert_eq!(data, 0x47); // Default CCB: IRQ1 | system | translate
    }

    #[test]
    fn test_write_command_self_test() {
        let mut kb = Keyboard::new();
        kb.write_command(0xAA);
        let data = kb.read_data();
        assert_eq!(data, 0x55); // Self-test passed
    }

    #[test]
    fn test_write_command_interface_test() {
        let mut kb = Keyboard::new();
        kb.write_command(0xAB);
        let data = kb.read_data();
        assert_eq!(data, 0x00); // No error
    }

    #[test]
    fn test_write_command_test_mouse_port() {
        let mut kb = Keyboard::new();
        kb.write_command(0xA9);
        let data = kb.read_data();
        assert_eq!(data, 0x00); // No error
    }

    #[test]
    fn test_write_command_disable_enable_keyboard() {
        let mut kb = Keyboard::new();

        // Disable keyboard (0xAD sets bit 4 of CCB)
        kb.write_command(0xAD);
        assert!(kb.is_keyboard_disabled());

        // Enable keyboard (0xAE clears bit 4)
        kb.write_command(0xAE);
        assert!(!kb.is_keyboard_disabled());
    }

    #[test]
    fn test_write_command_disable_enable_mouse() {
        let mut kb = Keyboard::new();

        // Disable mouse (0xA7)
        kb.write_command(0xA7);
        assert!(kb.is_mouse_disabled());

        // Enable mouse (0xA8)
        kb.write_command(0xA8);
        assert!(!kb.is_mouse_disabled());
    }

    #[test]
    fn test_write_command_read_input_port() {
        let mut kb = Keyboard::new();
        kb.write_command(0xC0);
        let data = kb.read_data();
        assert_eq!(data, 0x93); // Default input port value
    }

    #[test]
    fn test_write_command_input_port_poll_low() {
        let mut kb = Keyboard::new();
        kb.write_command(0xC1);
        // Low nibble of input port (0x93 & 0x0F = 0x03) should be in status bits
        let status = kb.read_status();
        assert_eq!(status & 0xF0, 0x30); // Low nibble shifted to bits 4-7
    }

    #[test]
    fn test_write_command_input_port_poll_high() {
        let mut kb = Keyboard::new();
        kb.write_command(0xC2);
        // High nibble of input port (0x93 >> 4 = 0x09) should be in status bits
        let status = kb.read_status();
        assert_eq!(status & 0xF0, 0x90);
    }

    #[test]
    fn test_write_command_read_output_port() {
        let mut kb = Keyboard::new();
        kb.write_command(0xD0);
        let data = kb.read_data();
        assert_eq!(data, 0xDF); // Default output port
    }

    #[test]
    fn test_write_command_write_controller_ram() {
        let mut kb = Keyboard::new();
        // 0x60 = write controller RAM byte 0 (CCB)
        kb.write_command(0x60);
        // Now write the data byte
        kb.write_data(0x45); // New CCB value
        assert_eq!(kb.controller_ram()[0], 0x45);
    }

    #[test]
    fn test_write_command_write_output_port_a20() {
        let mut kb = Keyboard::new();
        // 0xD1 = write output port
        kb.write_command(0xD1);
        kb.write_data(0x03); // Bit 0 = reset inactive, bit 1 = A20 enabled
        assert_eq!(kb.take_pending_a20(), Some(true));
        assert_eq!(kb.take_pending_guest_reset(), None);
    }

    #[test]
    fn test_write_output_port_reset_bit_low_requests_guest_reset() {
        let mut kb = Keyboard::new();
        let reset_value = kb.output_port() & !0x01;

        kb.write_command(0xD1);
        kb.write_data(reset_value);

        assert_eq!(
            kb.take_pending_guest_reset(),
            Some(GuestResetSource::KeyboardControllerPulseOutputPort)
        );
    }

    #[test]
    fn test_write_command_write_keyboard_output_buffer() {
        let mut kb = Keyboard::new();
        kb.write_command(0xD2);
        kb.write_data(0x42); // Inject as keyboard scancode
        assert!(kb.has_pending_scancodes());
    }

    #[test]
    fn test_write_command_write_mouse_output_buffer() {
        let mut kb = Keyboard::new();
        kb.write_command(0xD3);
        kb.write_data(0x55); // Inject into mouse output buffer
    }

    #[test]
    fn test_write_command_write_to_mouse_d4() {
        let mut kb = Keyboard::new();
        kb.write_command(0xD4);
        // Next byte goes to mouse
        kb.write_data(0xF2); // Mouse identify command
    }

    #[test]
    fn test_write_command_system_reset_requests_guest_reset() {
        let mut kb = Keyboard::new();
        kb.write_command(0xFE);
        assert_eq!(
            kb.take_pending_guest_reset(),
            Some(GuestResetSource::KeyboardControllerPulseOutputPort)
        );
    }

    #[test]
    fn test_keyboard_device_reset_does_not_request_guest_reset() {
        let mut kb = Keyboard::new();
        kb.write_data(0xFF);
        assert_eq!(kb.take_pending_guest_reset(), None);
    }

    #[test]
    fn test_write_command_unknown() {
        let mut kb = Keyboard::new();
        kb.write_command(0x40); // Unknown command
                                // Should not panic
    }

    // ==================== Keyboard device commands (write_data) ====================

    #[test]
    fn test_keyboard_echo_command() {
        let mut kb = Keyboard::new();
        kb.write_data(0xEE);
        // Should echo 0xEE back
        let data = kb.read_data();
        assert_eq!(data, 0xEE);
    }

    #[test]
    fn test_keyboard_read_id_cov() {
        let mut kb = Keyboard::new();
        kb.write_data(0xF2);
        // Should return ACK (0xFA), then keyboard ID (0xAB, 0x83)
        assert_eq!(kb.read_data(), 0xFA);
    }

    #[test]
    fn test_keyboard_enable_disable_scanning_cov() {
        let mut kb = Keyboard::new();
        assert!(kb.is_scanning_enabled());

        kb.write_data(0xF5); // Disable scanning
        assert!(!kb.is_scanning_enabled());
        // Should ACK
        assert_eq!(kb.read_data(), 0xFA);

        kb.write_data(0xF4); // Enable scanning
        assert!(kb.is_scanning_enabled());
        assert_eq!(kb.read_data(), 0xFA);
    }

    #[test]
    fn test_keyboard_set_leds_cov() {
        let mut kb = Keyboard::new();
        kb.write_data(0xED); // Set LEDs
                             // ACK
        assert_eq!(kb.read_data(), 0xFA);
        // Send LED state byte
        kb.write_data(0x07); // All LEDs on
        assert_eq!(kb.get_led_state(), 0x07);
    }

    #[test]
    fn test_keyboard_set_scancode_set() {
        let mut kb = Keyboard::new();
        kb.write_data(0xF0); // Set scancode set
        assert_eq!(kb.read_data(), 0xFA);

        // Query current set (data byte = 0)
        kb.write_data(0x00);
        assert_eq!(kb.read_data(), 0xFA);

        // Select set 2
        kb.write_data(0xF0);
        let _ = kb.read_data(); // ACK
        kb.write_data(0x02);
        assert_eq!(kb.scancode_set(), 2);
    }

    #[test]
    fn test_keyboard_invalid_low_command_resends() {
        let mut kb = Keyboard::new();
        kb.write_data(0xE0); // Unknown keyboard command
        assert_eq!(kb.read_data(), 0xFE); // Should request resend
    }

    #[test]
    fn test_keyboard_set_defaults_f6() {
        let mut kb = Keyboard::new();
        kb.write_data(0xF5); // Disable scanning first
        let _ = kb.read_data(); // consume ACK

        kb.write_data(0xF6); // Set defaults
        assert!(kb.is_scanning_enabled()); // Should re-enable
        assert_eq!(kb.read_data(), 0xFA); // ACK
    }

    // ==================== read_status ====================

    #[test]
    fn test_read_status_basic() {
        let mut kb = Keyboard::new();
        let status = kb.read_status();
        // Bit 2 (system flag) should be set from CCB
        assert_ne!(status & 0x04, 0);
    }

    #[test]
    fn test_read_status_obf_set() {
        let mut kb = Keyboard::new();
        // Stage data in output buffer
        kb.write_command(0xAA); // Self-test -> 0x55 in output buffer
        let status = kb.read_status();
        assert_ne!(status & 0x01, 0); // OBF should be set
    }

    #[test]
    fn test_read_status_ibf_clears() {
        let mut kb = Keyboard::new();
        kb.write_command(0xAA);
        // IBF should be set after write_command
        let status = kb.read_status();
        // IBF is cleared on read
        let status2 = kb.read_status();
        assert_eq!(status2 & 0x02, 0);
        let _ = status; // suppress warning
    }

    // ==================== write_control (port 0x61) ====================

    #[test]
    fn test_write_control_port61() {
        let mut kb = Keyboard::new();
        kb.write_control(0x03);
        assert_eq!(kb.port61_latch, 0x03);
    }

    // ==================== translation (scancode set conversion) ====================

    #[test]
    fn test_translate_and_buffer_set1_passthrough_cov() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 1;
        kb.translate_and_buffer(0x1E); // 'A' make code
        assert_eq!(kb.translation_buffer.len(), 1);
        assert_eq!(kb.translation_buffer[0], 0x1E);
    }

    #[test]
    fn test_translate_and_buffer_set2_e0_prefix() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable translation so effective_output_set is 2

        kb.translate_and_buffer(0xE0);
        assert!(kb.translate_e0_pending);
        // In set 2, E0 prefix is passed through
        assert_eq!(kb.translation_buffer.len(), 1);
        assert_eq!(kb.translation_buffer[0], 0xE0);
    }

    #[test]
    fn test_translate_and_buffer_e1_passthrough_cov() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable translation
        kb.translate_and_buffer(0xE1);
        assert_eq!(kb.translation_buffer.len(), 1);
        assert_eq!(kb.translation_buffer[0], 0xE1);
    }

    #[test]
    fn test_translate_and_buffer_set2_break_code() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable translation

        kb.translate_and_buffer(0x9E); // Break code for 'A' (0x1E | 0x80)
                                       // Should produce F0 prefix + translated make code
        assert!(kb.translation_buffer.len() >= 2);
        assert_eq!(kb.translation_buffer[0], 0xF0);
    }

    // ==================== enqueue_input_scancodes ====================

    #[test]
    fn test_enqueue_input_scancodes_empty() {
        let mut kb = Keyboard::new();
        assert!(!kb.enqueue_input_scancodes(&[]));
    }

    #[test]
    fn test_enqueue_input_scancodes_disabled() {
        let mut kb = Keyboard::new();
        kb.write_data(0xF5); // Disable scanning
        let _ = kb.read_data(); // consume ACK
        assert!(!kb.enqueue_input_scancodes(&[0x1E]));
    }

    #[test]
    fn test_enqueue_input_scancodes_set1_fast_path() {
        let mut kb = Keyboard::new();
        assert!(kb.enqueue_input_scancodes(&[0x1E, 0x9E]));
        assert_eq!(kb.shared_state.pending_count(), 2);
    }

    #[test]
    fn test_enqueue_input_scancodes_set2_translation() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable translation -> effective set = 2
        assert!(kb.enqueue_input_scancodes(&[0x1E])); // 'A' make
                                                      // Translation now happens at pop time, so the byte sits in the
                                                      // unified FIFO as raw Set 1 (0x1E). Pop with KBDINT enabled
                                                      // returns the translated Set 2 byte (0x1C).
        assert!(kb.shared_state.has_pending());
        assert_eq!(kb.pop_keyboard_byte().map(|(b, _)| b), Some(0x1C));
    }

    // ==================== InjectedKeySet ====================

    #[test]
    fn test_injected_key_set_mark_and_check_cov() {
        let set = InjectedKeySet::new();
        assert!(!set.check_and_clear(10));

        set.mark(10);
        assert!(set.check_and_clear(10));
        // Should be cleared after check
        assert!(!set.check_and_clear(10));
    }

    #[test]
    fn test_injected_key_set_clear_all_cov() {
        let set = InjectedKeySet::new();
        set.mark(5);
        set.mark(70); // In second word
        set.clear_all();
        assert!(!set.check_and_clear(5));
        assert!(!set.check_and_clear(70));
    }

    #[test]
    fn test_injected_key_set_out_of_range_cov() {
        let set = InjectedKeySet::new();
        set.mark(200); // word index 3, beyond array
        assert!(!set.check_and_clear(200));
    }

    // ==================== SharedKeyboardState ====================

    #[test]
    fn test_shared_keyboard_state_modifiers() {
        let shared = SharedKeyboardState::new();
        shared.set_modifier(true, false, false);
        assert_eq!(shared.get_modifier_flags() & 0x03, 0x03); // Both shift bits
    }

    #[test]
    fn test_shared_keyboard_state_typematic() {
        let shared = SharedKeyboardState::new();
        shared.set_typematic(2, 15);
        assert_eq!(shared.get_typematic_delay(), 2);
        assert_eq!(shared.get_typematic_rate(), 15);
    }

    #[test]
    fn test_shared_keyboard_state_typematic_ms() {
        let shared = SharedKeyboardState::new();
        assert!(shared.get_typematic_delay_ms() > 0);
        assert!(shared.get_typematic_rate_ms() > 0);
    }

    #[test]
    fn test_shared_keyboard_state_update_repeat() {
        let shared = SharedKeyboardState::new();
        shared.update_repeat_state(0x1E, false, true);
        assert_eq!(shared.get_repeat_scancode(), 0x1E);
        assert!(!shared.is_repeat_extended());

        shared.update_repeat_state(0x1E, false, false);
        assert_eq!(shared.get_repeat_scancode(), 0);
    }

    #[test]
    fn test_shared_keyboard_state_update_repeat_different_key() {
        let shared = SharedKeyboardState::new();
        shared.update_repeat_state(0x1E, false, true);
        // Release a different key - should not clear
        shared.update_repeat_state(0x20, false, false);
        assert_eq!(shared.get_repeat_scancode(), 0x1E);
    }

    #[test]
    fn test_shared_keyboard_state_clear_cov() {
        let shared = SharedKeyboardState::new();
        shared.inject_scancode(&[0x1E], 0x1E, true);
        shared.set_modifier(true, true, true);
        shared.clear();
        assert!(!shared.has_pending());
        assert_eq!(shared.get_modifier_flags(), 0);
    }

    #[test]
    fn test_shared_keyboard_state_inject_disabled() {
        let shared = SharedKeyboardState::new();
        shared.set_keyboard_enabled(false);
        shared.inject_scancode(&[0x1E], 0x1E, true);
        assert!(!shared.has_pending()); // Should be dropped
    }

    #[test]
    fn test_shared_keyboard_state_inject_scanning_disabled() {
        let shared = SharedKeyboardState::new();
        shared.set_scanning_enabled(false);
        shared.inject_scancode(&[0x1E], 0x1E, true);
        assert!(!shared.has_pending());
    }

    #[test]
    fn test_shared_keyboard_state_gated_release_clears_repeat() {
        let shared = SharedKeyboardState::new();
        shared.inject_scancode(&[0x1E], 0x1E, true);
        assert_eq!(shared.get_repeat_scancode(), 0x1E);

        shared.set_keyboard_enabled(false);
        shared.inject_scancode(&[0x9E], 0x1E, false);

        assert_eq!(
            shared.get_repeat_scancode(),
            0,
            "Key-up during keyboard inhibit must clear typematic state"
        );
        assert_eq!(
            shared.pending_count(),
            1,
            "Gated release should not enqueue a break byte"
        );
    }

    #[test]
    fn test_shared_keyboard_state_scanning_gated_release_clears_repeat() {
        let shared = SharedKeyboardState::new();
        shared.inject_scancode(&[0x1E], 0x1E, true);
        assert_eq!(shared.get_repeat_scancode(), 0x1E);

        shared.set_scanning_enabled(false);
        shared.inject_scancode(&[0x9E], 0x1E, false);

        assert_eq!(
            shared.get_repeat_scancode(),
            0,
            "Key-up while scanning is disabled must clear typematic state"
        );
        assert_eq!(
            shared.pending_count(),
            1,
            "Gated release should not enqueue a break byte"
        );
    }

    #[test]
    fn test_shared_keyboard_state_inject_extended_key() {
        let shared = SharedKeyboardState::new();
        // Extended key (E0-prefixed, 2 bytes)
        shared.inject_scancode(&[0xE0, 0x48], 0x48, true);
        assert!(shared.has_pending());
        assert!(shared.is_repeat_extended());
    }

    // ==================== effective_output_set ====================

    #[test]
    fn test_effective_output_set_default_cov() {
        let kb = Keyboard::new();
        // Default: set 1, translation enabled -> output set 1
        assert_eq!(kb.effective_output_set(), 1);
    }

    #[test]
    fn test_effective_output_set_set2_with_translation_cov() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        // Translation enabled (bit 6) -> effective is set 1
        assert_eq!(kb.effective_output_set(), 1);
    }

    #[test]
    fn test_effective_output_set_set2_without_translation_cov() {
        let mut kb = Keyboard::new();
        kb.scancode_set = 2;
        kb.controller_ram[0] &= !0x40; // Disable translation
        assert_eq!(kb.effective_output_set(), 2);
    }

    // ==================== various keyboard getters ====================

    #[test]
    fn test_keyboard_getters() {
        let kb = Keyboard::new();
        assert_eq!(kb.data_register(), 0);
        assert!(!kb.translate_e0_pending());
        assert!(kb.translation_buffer_snapshot().is_empty());
        assert!(kb.scancode_queue_snapshot().is_empty());
        assert_eq!(kb.scancode_set(), 1);
        assert_eq!(kb.led_state(), 0);
        assert!(!kb.command_response_pending());
        assert!(!kb.output_from_mouse());
        assert_eq!(kb.typematic_delay(), 1);
        assert_eq!(kb.typematic_rate(), 0x0B);
    }

    #[test]
    fn test_irq1_enabled() {
        let kb = Keyboard::new();
        assert!(kb.irq1_enabled()); // Bit 0 of default CCB is set
    }

    #[test]
    fn test_irq12_enabled() {
        let kb = Keyboard::new();
        assert!(kb.irq12_enabled()); // Bit 1 of default CCB is set
    }

    #[test]
    fn test_pop_local_scancode() {
        let mut kb = Keyboard::new();
        kb.enqueue_scancodes(&[0x1E, 0x9E]);
        assert_eq!(kb.pop_local_scancode(), Some(0x1E));
        assert_eq!(kb.pop_local_scancode(), Some(0x9E));
        assert_eq!(kb.pop_local_scancode(), None);
    }
}
