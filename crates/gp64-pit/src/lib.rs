//! Intel 8253/8254 programmable interval timer core.
//!
//! ## PIT Modes
//!
//! The 8253/8254 PIT supports 6 operating modes:
//!
//! - **Mode 0**: Interrupt on terminal count (one-shot)
//!   - OUT is initially LOW after mode set
//!   - OUT goes HIGH when count reaches 0
//!   - Does NOT auto-reload; must be reprogrammed for next interrupt
//!
//! - **Mode 1**: Hardware retriggerable one-shot
//!   - Similar to mode 0 but triggered by rising edge on GATE
//!   - OUT goes LOW on trigger, HIGH on terminal count
//!
//! - **Mode 2**: Rate generator (divide by N)
//!   - OUT is HIGH for N-1 counts, LOW for 1 count
//!   - Auto-reloads on terminal count (continuous)
//!   - Used for periodic interrupts
//!
//! - **Mode 3**: Square wave generator
//!   - For even divisor N: OUT is HIGH for N/2 counts, LOW for N/2 counts
//!   - For odd divisor N: OUT is HIGH for (N+1)/2 counts, LOW for (N-1)/2 counts
//!   - Auto-reloads (continuous)
//!   - Used for speaker tone generation
//!
//! - **Mode 4**: Software triggered strobe (one-shot)
//!   - OUT is HIGH until terminal count, then pulses LOW for one clock
//!   - Does NOT auto-reload
//!
//! - **Mode 5**: Hardware triggered strobe
//!   - Like mode 4 but triggered by rising edge on GATE
//!   - Does NOT auto-reload after trigger
//!
//! ## Channel Usage
//!
//! - Channel 0: System timer (IRQ 0)
//! - Channel 1: DRAM refresh (not typically used in emulation)
//! - Channel 2: PC speaker

use serde::{Deserialize, Serialize};

/// PIT runs at approximately 1.193182 MHz
pub const PIT_FREQUENCY: u32 = 1193182;

/// PIT operating mode categories for event scheduling.
/// Maps 6 hardware modes to 3 scheduling behaviors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PitMode {
    /// Modes 0, 1, 4, 5: one-shot — fire once, don't reschedule
    OneShot,
    /// Mode 2: periodic rate generator
    RateGenerator,
    /// Mode 3: periodic square wave
    SquareWave,
}

impl PitMode {
    /// Returns `true` for one-shot modes (0, 1, 4, 5) that fire once.
    #[inline]
    pub fn is_one_shot(self) -> bool {
        matches!(self, PitMode::OneShot)
    }
}

/// Timer interrupt frequency (default ~18.2 Hz)
const DEFAULT_DIVISOR: u16 = 0;

fn default_pit_hz() -> u32 {
    PIT_FREQUENCY
}

fn default_ips() -> u64 {
    PIT_FREQUENCY as u64
}

const PIT_STATE_VERSION: u32 = 2;

fn default_pit_state_version() -> u32 {
    1
}

fn default_immediate_reload_mask() -> u8 {
    1 << 2
}

/// Per-channel state for PIT
#[derive(Clone, Serialize, Deserialize)]
struct PitChannel {
    /// Current counter value
    counter: u16,
    /// Active reload value; a completed replacement can wait in deferred_reload.
    reload: u16,
    /// Latched value (for counter latch command)
    latch: Option<u16>,
    /// Operating mode (0-5) used for counting and scheduling.
    mode: u8,
    /// Last programmed mode bits, including aliases 6/7. Older snapshots
    /// omitted this field, so None falls back to their normalized mode.
    #[serde(default)]
    raw_mode: Option<u8>,
    /// Access mode: 1=LSB only, 2=MSB only, 3=LSB then MSB
    access_mode: u8,
    /// Read state for LSB/MSB access (0=LSB next, 1=MSB next)
    read_state: u8,
    /// Write state for LSB/MSB access
    write_state: u8,
    /// OUT pin state (true = HIGH)
    out: bool,
    /// Gate input state (true = HIGH) - used for modes 1, 2, 3, 5
    gate: bool,
    /// Has the counter been loaded? (for modes 0, 4)
    loaded: bool,
    /// Last complete CR write has not transferred to CE. None is legacy state.
    #[serde(default)]
    null_count: Option<bool>,
    /// Has terminal count been reached? (for one-shot modes 0, 1, 4, 5)
    terminal_count_reached: bool,
    /// Counting enabled (gate-controlled for some modes)
    counting: bool,
    /// BCD mode
    bcd: bool,
    /// Latched status byte (for read-back command)
    status_latch: Option<u8>,
    /// Base cycles when the counter was last loaded or triggered
    base_cycles: u64,
    /// Deferred reload value. In mode 2/3, writing a new counter without a
    /// preceding mode command stores the value here. Applied at next cycle
    /// boundary (when tick fires or event fires). In modes 1/5, a completed
    /// replacement waits here until the next rising GATE trigger, preserving
    /// the current shot even after terminal count. None = no pending update.
    #[serde(default)]
    deferred_reload: Option<u16>,
    /// Saved reload value from before an LSB/MSB write pair began. Used to
    /// restore the active reload when a counter write is deferred. This is
    /// live state while `write_state == 1`, so it belongs in PIT snapshots;
    /// old snapshots that predate the field use the explicit zero default.
    #[serde(default)]
    write_pair_old_reload: u16,
}

impl PitChannel {
    fn new(default_mode: u8) -> Self {
        Self {
            counter: DEFAULT_DIVISOR,
            reload: DEFAULT_DIVISOR,
            latch: None,
            mode: default_mode,
            raw_mode: Some(default_mode),
            access_mode: 3, // LSB/MSB
            read_state: 0,
            write_state: 0,
            // Mode 0 (interrupt on terminal count) is the ONLY mode whose OUT
            // starts low. Mode 1 is the hardware retriggerable one-shot: OUT is
            // high until the GATE rising edge fires the shot, then low until
            // terminal count (MDD-BUG-TEM-00001).
            out: default_mode != 0,
            gate: true, // GATE defaults to HIGH (enabled)
            loaded: false,
            null_count: Some(true),
            terminal_count_reached: false,
            counting: true,
            bcd: false,
            status_latch: None,
            base_cycles: 0,
            deferred_reload: None,
            write_pair_old_reload: 0,
        }
    }

    /// The reload value this channel is actually counting on.
    ///
    /// Between the two bytes of an LSB/MSB pair, `reload` holds a hybrid of
    /// the new LSB and the PREVIOUS MSB — a number the 8254 never counts on,
    /// because it keeps the Count Register separate from the Counting Element
    /// and transfers only when the write completes. `write_pair_old_reload`
    /// already captures what the channel must keep running on meanwhile; this
    /// is what finally consults it, so a read or an elapsed-time computation
    /// landing between the two bytes no longer sees the hybrid
    /// (MDD-BUG-TEM-00003).
    #[inline]
    fn active_reload(&self) -> u16 {
        if self.write_state == 1 {
            self.write_pair_old_reload
        } else {
            self.reload
        }
    }

    /// Transfer the last complete count register on a hardware trigger.
    fn load_triggered_reload(&mut self) {
        if let Some(reload) = self.deferred_reload.take() {
            if self.write_state == 1 {
                // A newer LSB is still incomplete. Keep it in `reload` until
                // its MSB arrives; the active count belongs in the saved slot.
                self.write_pair_old_reload = reload;
            } else {
                self.reload = reload;
            }
        }
        if self.loaded {
            self.null_count = Some(false);
        }
    }

    /// Legacy snapshots lack the flag, but preserve pending count/trigger state.
    fn null_count(&self) -> bool {
        self.null_count.unwrap_or_else(|| {
            !self.loaded
                || self.deferred_reload.is_some()
                || (self.is_gate_triggered() && !self.counting)
        })
    }

    /// Check if this mode is one-shot (doesn't auto-reload)
    #[inline]
    fn is_one_shot(&self) -> bool {
        matches!(self.mode, 0 | 1 | 4 | 5)
    }

    /// Check if this mode requires gate trigger
    #[inline]
    fn is_gate_triggered(&self) -> bool {
        matches!(self.mode, 1 | 5)
    }

    /// Elapsed-tick period `compute_counter` reduces against for this mode:
    /// the free-running 16-bit wrap for one-shots, the half divisor for even
    /// mode-3 counts, and the full divisor otherwise. Odd mode 3 needs the
    /// extra HIGH clock when carrying phase across a rate change.
    #[inline]
    fn tick_period(&self) -> u64 {
        let divisor = effective_divisor(self.active_reload(), self.bcd);
        if self.is_one_shot() {
            65536
        } else if self.mode == 3 && divisor.is_multiple_of(2) {
            divisor / 2
        } else {
            divisor
        }
    }

    /// Reset channel state when mode is set
    fn set_mode(&mut self, raw_mode: u8, access_mode: u8, bcd: bool) {
        self.raw_mode = Some(raw_mode);
        let mode = match raw_mode {
            6 => 2,
            7 => 3,
            mode => mode,
        };
        self.mode = mode;
        self.access_mode = access_mode;
        self.bcd = bcd;
        self.latch = None;
        self.read_state = 0;
        self.write_state = 0;
        self.loaded = false;
        self.null_count = Some(true);
        self.terminal_count_reached = false;
        self.deferred_reload = None; // Mode change cancels any pending deferred reload

        // Set initial OUT state based on mode. Only Mode 0 starts LOW; Mode 1
        // is the one-shot and stays HIGH until its GATE trigger
        // (MDD-BUG-TEM-00001).
        self.out = mode != 0;

        // Gate-triggered modes don't start counting until gate trigger
        self.counting = !self.is_gate_triggered();
    }

    fn build_status_with_out(&self, computed_out: bool) -> u8 {
        let mut status: u8 = 0;
        if computed_out {
            status |= 0x80;
        }
        if self.null_count() {
            status |= 0x40;
        }
        status |= (self.access_mode & 0x03) << 4;
        status |= (self.raw_mode.unwrap_or(self.mode) & 0x07) << 1;
        if self.bcd {
            status |= 0x01;
        }
        status
    }
}

#[derive(Clone)]
pub struct Pit {
    /// Per-channel state
    channels: [PitChannel; 3],

    /// PIT base frequency (Hz) used for host timing conversions
    pit_hz: u32,
    /// Total accumulated instruction cycles (absolute time)
    pub total_cycles: u64,

    /// Instructions per second — the CPU speed. Used for all PIT timing conversions.
    /// Defaults to `PIT_FREQUENCY` for 1:1 instruction-to-PIT-tick mapping.
    ips: u64,

    /// Fixed-point ratio for fast instruction→PIT-tick conversion.
    /// `pit_hz << 32 / ips`, updated whenever `ips` changes.
    /// Replaces u128 division in `compute_counter` with u128 multiply + shift.
    pit_ticks_per_instr_fp: u64,

    /// Flag set by PIT I/O writes to signal EventQueue needs updating.
    /// The Emulator checks this after I/O dispatch.
    pub event_reschedule_pending: bool,

    /// Flag set when channel 2 (PC speaker) counter is fully loaded.
    /// The I/O dispatch layer checks this to notify the PcSpeaker of
    /// frequency changes (needed for PCM detection).
    pub speaker_reload_updated: bool,

    /// Channels whose completed counter writes take effect immediately even
    /// while mode 2/3 is running. This is board policy, not 8254 silicon:
    /// The PC speaker uses it for channel 2 because its speaker path has no event that
    /// can consume a deferred reload. Machine-neutral controllers leave it
    /// clear and follow the normal next-cycle-boundary rule on every channel.
    immediate_reload_mask: u8,
}

/// Versioned serialized PIT state.
///
/// The public `Pit` type contains derived timing data and machine-policy fields
/// that need different treatment during restore. Keeping the wire shape in a
/// separate DTO makes newly added live state explicit and gives old snapshots
/// named defaults instead of silently rebuilding it from `Pit::new()`.
#[derive(Serialize, Deserialize)]
struct PitState {
    /// Version 1 is the unversioned format emitted before the DTO was added.
    #[serde(rename = "version", default = "default_pit_state_version")]
    _version: u32,
    channels: [PitChannel; 3],
    #[serde(default = "default_pit_hz")]
    pit_hz: u32,
    #[serde(default)]
    total_cycles: u64,
    // Backward compatibility with snapshots that used the split tick counter.
    #[serde(default, rename = "tick_counter_lo", skip_serializing)]
    _tick_counter_lo: u64,
    #[serde(default, rename = "tick_counter_hi", skip_serializing)]
    _tick_counter_hi: u64,
    // `ips` replaced the old `emulated_ips` field.
    #[serde(default = "default_ips", alias = "emulated_ips")]
    ips: u64,
    // Derived from pit_hz and ips during restore, but retained on the wire for
    // compatibility with existing snapshots.
    #[serde(default, rename = "pit_ticks_per_instr_fp")]
    _serialized_pit_ticks_per_instr_fp: u64,
    #[serde(default)]
    event_reschedule_pending: bool,
    #[serde(default)]
    speaker_reload_updated: bool,
    /// Old snapshots omitted this board-policy field. Their deserializer used
    /// `Pit::new()` as the base, whose historical default is channel 2 only.
    #[serde(default = "default_immediate_reload_mask")]
    immediate_reload_mask: u8,
}

/// Side-effect-free diagnostic state for one PIT channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PitChannelDiagnosticSnapshot {
    pub channel: u8,
    pub current_counter: u16,
    pub reload: u16,
    pub effective_divisor: u64,
    pub latched_counter: Option<u16>,
    pub latched_status: Option<u8>,
    pub mode: u8,
    pub access_mode: u8,
    pub read_state: u8,
    pub write_state: u8,
    pub out: bool,
    pub gate: bool,
    pub loaded: bool,
    pub terminal_count_reached: bool,
    pub counting: bool,
    pub bcd: bool,
    pub base_cycles: u64,
    pub deferred_reload: Option<u16>,
}

/// Side-effect-free diagnostic snapshot of the PIT controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PitDiagnosticSnapshot {
    pub pit_hz: u32,
    pub total_cycles: u64,
    pub emulated_ips: u64,
    pub pit_ticks_per_instr_fp: u64,
    pub event_reschedule_pending: bool,
    pub speaker_reload_updated: bool,
    pub interval_us: u64,
    pub cumulative_ticks: u64,
    pub channels: [PitChannelDiagnosticSnapshot; 3],
}

impl Serialize for Pit {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        PitState {
            _version: PIT_STATE_VERSION,
            channels: self.channels.clone(),
            pit_hz: self.pit_hz,
            total_cycles: self.total_cycles,
            _tick_counter_lo: 0,
            _tick_counter_hi: 0,
            ips: self.ips,
            _serialized_pit_ticks_per_instr_fp: self.pit_ticks_per_instr_fp,
            event_reschedule_pending: self.event_reschedule_pending,
            speaker_reload_updated: self.speaker_reload_updated,
            immediate_reload_mask: self.immediate_reload_mask,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Pit {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let state = PitState::deserialize(deserializer)?;
        let mut pit = Pit::new();
        pit.channels = state.channels;
        pit.pit_hz = state.pit_hz;
        pit.total_cycles = state.total_cycles;
        // tick_counter_lo/hi from old snapshots deliberately ignored
        pit.ips = state.ips;
        pit.pit_ticks_per_instr_fp = compute_fixed_point_ratio(state.pit_hz, state.ips);
        pit.event_reschedule_pending = state.event_reschedule_pending;
        pit.speaker_reload_updated = state.speaker_reload_updated;
        pit.immediate_reload_mask = state.immediate_reload_mask;
        Ok(pit)
    }
}

/// Convert a BCD-encoded u16 value to its binary equivalent.
/// Each nybble represents a decimal digit (0-9).
fn bcd_to_binary(bcd: u16) -> u16 {
    let d0 = bcd & 0x000F;
    let d1 = (bcd >> 4) & 0x000F;
    let d2 = (bcd >> 8) & 0x000F;
    let d3 = (bcd >> 12) & 0x000F;
    d3 * 1000 + d2 * 100 + d1 * 10 + d0
}

/// Convert a binary value (0-9999) to BCD-encoded u16.
fn binary_to_bcd(mut val: u16) -> u16 {
    if val > 9999 {
        val = 9999;
    }
    let d3 = val / 1000;
    val %= 1000;
    let d2 = val / 100;
    val %= 100;
    let d1 = val / 10;
    let d0 = val % 10;
    (d3 << 12) | (d2 << 8) | (d1 << 4) | d0
}

/// Compute the effective binary divisor for a PIT channel, accounting for BCD mode.
/// Precompute fixed-point ratio `(pit_hz << 32) / ips` for fast
/// instruction→PIT-tick conversion without u128 division.
fn compute_fixed_point_ratio(pit_hz: u32, ips: u64) -> u64 {
    ((pit_hz as u128) << 32)
        .checked_div(ips.max(1) as u128)
        .unwrap_or(0) as u64
}

fn effective_divisor(reload: u16, bcd: bool) -> u64 {
    if bcd {
        if reload == 0 {
            10000
        } else {
            bcd_to_binary(reload) as u64
        }
    } else if reload == 0 {
        65536
    } else {
        reload as u64
    }
}

/// Per-channel PIT state carried across a checkpoint.
///
/// A named struct, not a tuple: the tuple this replaced omitted
/// `write_pair_old_reload` (MDD-BUG-CRUCIBLE-00551), and a positional
/// 16-field tuple gives no signal when a field is added and forgotten.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PitChannelState {
    pub counter: u16,
    pub reload: u16,
    pub latch: Option<u16>,
    pub mode: u8,
    /// Original control-word mode bits; None uses mode for legacy state.
    pub raw_mode: Option<u8>,
    pub access_mode: u8,
    pub read_state: u8,
    pub write_state: u8,
    pub out: bool,
    pub gate: bool,
    pub loaded: bool,
    /// None reconstructs the flag from legacy pending count/trigger state.
    pub null_count: Option<bool>,
    pub terminal_count_reached: bool,
    pub counting: bool,
    pub bcd: bool,
    pub status_latch: Option<u8>,
    pub base_cycles: u64,
    pub deferred_reload: Option<u16>,
    /// Divisor that was active when the LSB of an LSB/MSB pair was
    /// written. `write_state == 1` means a pair is half written and this
    /// value is what the channel must keep running on until the MSB
    /// arrives; dropping it made the restored channel fall back to zero
    /// (effective 65,536).
    pub write_pair_old_reload: u16,
}

impl Pit {
    pub fn new() -> Self {
        let ips = PIT_FREQUENCY as u64;
        let mut pit = Self {
            channels: [
                PitChannel::new(3), // Channel 0: Mode 3 (square wave) - system timer
                PitChannel::new(2), // Channel 1: Mode 2 (rate generator) - DRAM refresh
                PitChannel::new(2), // Channel 2: Mode 2 - PC speaker
            ],
            pit_hz: PIT_FREQUENCY,
            total_cycles: 0,
            ips,
            pit_ticks_per_instr_fp: compute_fixed_point_ratio(PIT_FREQUENCY, ips),
            event_reschedule_pending: false,
            speaker_reload_updated: false,
            immediate_reload_mask: 1 << 2,
        };

        // BIOS programs PIT channel 0 during POST; emulate a running system timer by default.
        pit.channels[0].loaded = true;
        pit.channels[0].null_count = Some(false);
        pit.channels[0].counting = true;
        pit.channels[0].base_cycles = pit.total_cycles;

        pit
    }

    /// Construct a machine-neutral, unprogrammed controller.
    ///
    /// The oscillator and caller timebase are board inputs. All channels start
    /// stopped and unloaded, so this constructor makes no claim about undefined
    /// power-on counter contents. A control word and counter data must program a
    /// channel before elapsed time can affect it.
    pub fn new_unprogrammed(oscillator_hz: u32, timebase_hz: u64) -> Self {
        let oscillator_hz = oscillator_hz.max(1);
        let timebase_hz = timebase_hz.max(1);
        let mut channels = [PitChannel::new(0), PitChannel::new(0), PitChannel::new(0)];
        for channel in &mut channels {
            channel.counting = false;
            channel.loaded = false;
        }

        Self {
            channels,
            pit_hz: oscillator_hz,
            total_cycles: 0,
            ips: timebase_hz,
            pit_ticks_per_instr_fp: compute_fixed_point_ratio(oscillator_hz, timebase_hz),
            event_reschedule_pending: false,
            speaker_reload_updated: false,
            immediate_reload_mask: 0,
        }
    }

    /// Write to PIT port
    pub fn write(&mut self, port: u16, value: u8) {
        match port {
            0x40 => self.write_counter(0, value),
            0x41 => self.write_counter(1, value),
            0x42 => self.write_counter(2, value),
            0x43 => self.write_command(value),
            _ => {}
        }
    }

    /// Read counter
    pub fn read_counter(&mut self, channel: u8) -> u8 {
        let ch_idx = channel as usize;

        if ch_idx > 2 {
            return 0xFF;
        }

        // Check for latched status (from read-back command)
        if let Some(status) = self.channels[ch_idx].status_latch.take() {
            return status;
        }

        // Only compute the counter when there is no latched value to return.
        // This avoids expensive u128 arithmetic in the common latch-then-read
        // pattern used for coherent multi-byte counter reads.
        let value_to_read = if let Some(latched) = self.channels[ch_idx].latch {
            latched
        } else {
            let current = self.compute_counter(ch_idx);
            self.channels[ch_idx].counter = current;
            current
        };

        let ch = &mut self.channels[ch_idx];
        match ch.access_mode {
            1 => {
                // LSB only
                ch.latch = None; // Consume latch
                (value_to_read & 0xFF) as u8
            }

            2 => {
                // MSB only
                ch.latch = None; // Consume latch
                (value_to_read >> 8) as u8
            }

            3 => {
                // LSB then MSB
                if ch.read_state == 0 {
                    // Reading LSB — auto-latch if not already latched
                    if ch.latch.is_none() {
                        ch.latch = Some(value_to_read);
                    }
                    ch.read_state = 1;
                    (value_to_read & 0xFF) as u8
                } else {
                    // Reading MSB — consume latch
                    ch.read_state = 0;
                    ch.latch = None;
                    (value_to_read >> 8) as u8
                }
            }

            _ => 0xFF,
        }
    }

    /// Write to counter
    pub fn write_counter(&mut self, channel: u8, value: u8) {
        let ch_idx = channel as usize;
        if ch_idx > 2 {
            return;
        }

        // Mode 0 stops the counting element on the first byte, before the
        // reload register is changed. Preserve the guest-visible old count.
        let freeze_count = {
            let ch = &self.channels[ch_idx];
            if ch.mode == 0 && ch.access_mode == 3 && ch.write_state == 0 {
                Some(self.compute_counter(ch_idx))
            } else {
                None
            }
        };
        let ch = &mut self.channels[ch_idx];
        if let Some(counter) = freeze_count {
            ch.counter = counter;
            ch.base_cycles = self.total_cycles;
            ch.counting = false;
            ch.out = false;
            if ch_idx == 0 {
                self.event_reschedule_pending = true;
            }
        }

        match ch.access_mode {
            1 => {
                // LSB only — a single-byte count load clears the unwritten MSB.
                let old_reload = ch.reload;
                ch.reload = value as u16;
                self.maybe_finish_counter_load(ch_idx, old_reload);
            }
            2 => {
                // MSB only — a single-byte count load clears the unwritten LSB.
                let old_reload = ch.reload;
                ch.reload = (value as u16) << 8;
                self.maybe_finish_counter_load(ch_idx, old_reload);
            }
            3 => {
                // LSB then MSB
                if ch.write_state == 0 {
                    // LSB write — save old reload before first modification
                    ch.write_pair_old_reload = ch.reload;
                    ch.reload = (ch.reload & 0xFF00) | (value as u16);
                    ch.write_state = 1; // Expect MSB next
                } else {
                    // MSB write — completes the pair
                    let old_reload = ch.write_pair_old_reload;
                    ch.reload = (ch.reload & 0x00FF) | ((value as u16) << 8);
                    ch.write_state = 0; // Reset for next pair
                    self.maybe_finish_counter_load(ch_idx, old_reload);
                }
            }
            _ => {} // Should not happen
        }
    }

    /// Called when a counter reload value is fully written
    fn finish_counter_load(&mut self, ch_idx: usize) {
        let ch = &mut self.channels[ch_idx];

        ch.counter = ch.reload;
        ch.base_cycles = self.total_cycles;
        ch.loaded = true;

        if ch.is_one_shot() {
            ch.terminal_count_reached = false;
            if ch.mode == 0 {
                ch.out = false;
            }
        }

        if !ch.is_gate_triggered() {
            ch.null_count = Some(false);
            ch.counting = ch.gate;
        }

        tracing::debug!(
            channel = ch_idx,
            reload = format_args!("{:04X}", ch.reload),
            mode = ch.mode,
            "PIT: counter loaded"
        );

        // Signal event-driven scheduler that channel 0 period changed
        if ch_idx == 0 {
            self.event_reschedule_pending = true;
        }

        // Signal I/O dispatch that channel 2 (speaker) frequency changed
        if self.immediate_reload_mask & (1 << ch_idx) != 0 {
            self.speaker_reload_updated = true;
        }
    }

    /// Decide whether a completed counter write should take effect immediately
    /// or wait for a cycle boundary (2/3) or a rising GATE trigger (1/5).
    ///
    /// In mode 2/3, if the channel is already loaded and counting (i.e. no
    /// preceding mode command via port 0x43), the new reload value is deferred.
    /// Real 8254 hardware does not restart the current count cycle on a
    /// counter-only reprogram — the new value takes effect at the next
    /// terminal count.
    fn maybe_finish_counter_load(&mut self, ch_idx: usize, old_reload: u16) {
        self.channels[ch_idx].null_count = Some(true);
        let ch = &self.channels[ch_idx];
        if ch.is_gate_triggered() && ch.loaded {
            // Count-only writes program the next trigger, never restart an
            // active or completed shot. This silicon rule also applies to
            // channel 2 independently of the mode 2/3 speaker board policy.
            let new_reload = ch.reload;
            self.channels[ch_idx].deferred_reload = Some(new_reload);
            self.channels[ch_idx].reload = old_reload;
            return;
        }
        // Board policy, not 8254 silicon: a channel named by
        // `immediate_reload_mask` applies a completed write at once. The PC speaker sets
        // it for channel 2 because the speaker path has no event that can
        // consume a deferred reload — without it the audio mixer reads the
        // stale pre-deferral value and the speaker sticks at one frequency.
        //
        // This used to hard-code `ch_idx == 2`, which ignored the mask
        // entirely: a machine-neutral controller built by `new_unprogrammed`
        // leaves the mask clear and must defer on EVERY channel, exactly as
        // the mask's own documentation says, but got the PC-speaker policy
        // anyway (MDD-BUG-TEM-00006).
        if self.immediate_reload_mask & (1 << ch_idx) != 0 {
            self.finish_counter_load(ch_idx);
            return;
        }
        // Defer in mode 2/3 when already loaded+counting (no new mode set).
        // set_mode() sets loaded=false, so `loaded` is false after a mode change.
        // NOTE: If GATE is LOW (counting=false), we fall through to immediate load.
        // This is correct — no active cycle to preserve when counting is stopped.
        if (ch.mode == 2 || ch.mode == 3) && ch.loaded && ch.counting {
            // ch.reload was already modified by write_counter to the new value.
            // Store new value as deferred; restore old reload for current cycle.
            // If a previous deferred write is pending, it's overwritten (last write wins).
            let new_reload = self.channels[ch_idx].reload;
            self.channels[ch_idx].deferred_reload = Some(new_reload);
            self.channels[ch_idx].reload = old_reload;
            tracing::debug!(
                channel = ch_idx,
                deferred_reload = format_args!("{:04X}", new_reload),
                mode = self.channels[ch_idx].mode,
                "PIT: counter update deferred to next cycle boundary"
            );
            // Do NOT set event_reschedule_pending.
            // Current period continues; deferred value applies at next boundary.
        } else {
            self.finish_counter_load(ch_idx);
        }
    }

    /// Write command
    pub fn write_command(&mut self, value: u8) {
        let ch_idx = ((value >> 6) & 0x03) as usize;

        // Check for read-back command (bits 7-6 = 11)
        if ch_idx == 3 {
            self.handle_readback_command(value);
            return;
        }

        let access = (value >> 4) & 0x03;
        let mode = (value >> 1) & 0x07;
        let bcd = (value & 0x01) != 0;

        if access == 0 {
            // Latch command - capture current counter value.
            //
            // A latch already holding an unread count wins: "If the Counter is
            // latched and then, some time later, latched again before the count
            // is read, the second Counter Latch Command is ignored. The count
            // read will be the count at the time the first Counter Latch
            // Command was issued." (Intel 8254 datasheet, Counter Latch
            // Command.) The whole command is ignored, `read_state` included —
            // resetting it mid-pair would hand back an LSB where the caller
            // expects the MSB, mixing two counts into one word. This path used
            // to capture unconditionally while `handle_readback_command`
            // already applied the guard (MDD-BUG-TEM-00008).
            if self.channels[ch_idx].latch.is_none() {
                // Sample the counting element at the current timestamp. A
                // latch freezes only the value presented to the read path; it
                // must not rebase the channel clock or alter OUT phase.
                let mut current = self.compute_counter(ch_idx);
                // With divisor 65536, the Mode 3 boundary count truncates
                // to zero in u16. This model exposes 0xFFFE instead when
                // latching that boundary, shifting the sample by one clock.
                if self.channels[ch_idx].mode == 3
                    && self.channels[ch_idx].counting
                    && current == 0
                    && self.channels[ch_idx].active_reload() == 0
                {
                    current = 0xFFFE;
                }
                self.channels[ch_idx].counter = current;
                self.channels[ch_idx].latch = Some(current);
                self.channels[ch_idx].read_state = 0; // Prepare to read LSB
            }
        } else {
            // Mode/Access write - reconfigure channel
            self.channels[ch_idx].set_mode(mode, access, bcd);
            tracing::debug!(channel = ch_idx, mode, access, bcd, "PIT: mode set");
            // Signal event-driven scheduler when channel 0 mode changes
            if ch_idx == 0 {
                self.event_reschedule_pending = true;
            }
        }
    }

    /// Handle read-back command (8254 feature)
    fn handle_readback_command(&mut self, value: u8) {
        let latch_count = (value & 0x20) == 0;
        let latch_status = (value & 0x10) == 0;

        for ch_idx in 0..3 {
            let channel_bit = 1 << (ch_idx + 1);
            if (value & channel_bit) == 0 {
                continue;
            }

            if latch_status && self.channels[ch_idx].status_latch.is_none() {
                let out = self.compute_out(ch_idx);
                let status = self.channels[ch_idx].build_status_with_out(out);
                self.channels[ch_idx].status_latch = Some(status);
            }

            if latch_count && self.channels[ch_idx].latch.is_none() {
                // Like the standard Counter Latch Command, Read-Back captures
                // the current count without changing the channel's live phase.
                let mut current = self.compute_counter(ch_idx);
                // Mode 3 boundary fixup (same as latch command path above)
                if self.channels[ch_idx].mode == 3
                    && self.channels[ch_idx].counting
                    && current == 0
                    && self.channels[ch_idx].active_reload() == 0
                {
                    current = 0xFFFE;
                }
                self.channels[ch_idx].counter = current;
                self.channels[ch_idx].latch = Some(current);
                self.channels[ch_idx].read_state = 0;
            }
        }
    }

    fn resync_counter_start(&mut self, ch_idx: usize) {
        let ch = &mut self.channels[ch_idx];
        let divisor = effective_divisor(ch.active_reload(), ch.bcd);
        // `counter` is the guest-visible encoding. BCD timing arithmetic must
        // use its decoded decimal value; treating 0x1134 as binary 4404 makes
        // a stopped one-shot resume from the reload value instead of its
        // frozen phase.
        let counter_value = if ch.bcd {
            bcd_to_binary(ch.counter) as u64
        } else {
            ch.counter as u64
        };

        // ticks_offset: how many PIT ticks into the current cycle we are
        let ticks_offset = if ch.is_one_shot() {
            // A programmed zero is the encoded form of the full divisor.
            let remaining =
                if ch.counter == 0 && ch.active_reload() == 0 && !ch.terminal_count_reached {
                    divisor
                } else {
                    counter_value
                };
            divisor.saturating_sub(remaining)
        } else {
            let remaining = if ch.counter == 0 {
                divisor
            } else {
                counter_value
            };
            divisor.saturating_sub(remaining) % divisor
        };

        // Convert PIT-tick offset to instruction offset:
        // instruction_offset = ticks_offset * ips / pit_hz
        let instruction_offset =
            ticks_offset as u128 * self.ips.max(1) as u128 / self.pit_hz as u128;

        ch.base_cycles = self.total_cycles.saturating_sub(instruction_offset as u64);
    }

    /// Set GATE input for a channel
    ///
    /// GATE affects different modes differently:
    /// - Mode 0: GATE enables/disables counting
    /// - Mode 1: Rising edge triggers one-shot
    /// - Mode 2, 3: GATE LOW disables counting; rising edge reloads counter
    /// - Mode 4: GATE enables/disables counting
    /// - Mode 5: Rising edge triggers one-shot
    pub fn set_gate(&mut self, channel: u8, high: bool) {
        let ch_idx = channel as usize;
        if ch_idx > 2 {
            return;
        }

        let (was_counting, was_low, mode) = {
            let ch = &self.channels[ch_idx];
            (ch.counting, !ch.gate, ch.mode)
        };

        if was_low == high {
            tracing::debug!(channel = ch_idx, gate = high, mode, "PIT: gate change");
        }
        let snapshot_before_stop = was_counting && matches!(mode, 0 | 2 | 3 | 4) && !high;
        let snapshot_value = if snapshot_before_stop {
            Some(self.compute_counter(ch_idx))
        } else {
            None
        };

        let ch = &mut self.channels[ch_idx];
        ch.gate = high;

        // Track whether the rising-edge handler already set up counting
        let mut rising_edge_handled = false;

        // Handle rising edge trigger
        if was_low && high {
            match ch.mode {
                1 => {
                    ch.load_triggered_reload();
                    // Mode 1: Rising edge starts one-shot
                    // OUT goes LOW, counting starts
                    ch.out = false;
                    ch.counting = true;
                    ch.terminal_count_reached = false;
                    ch.counter = ch.active_reload();
                    ch.base_cycles = self.total_cycles;
                    rising_edge_handled = true;
                }
                2 | 3 => {
                    ch.load_triggered_reload();
                    // Modes 2, 3: Rising edge reloads counter and restarts counting.
                    // Per 8254 spec, gate LOW->HIGH reloads from the reload register
                    // (not a resume from frozen position).
                    // Both modes restart their output in the HIGH phase.
                    ch.counting = true;
                    ch.out = true;
                    ch.counter = ch.active_reload();
                    ch.base_cycles = self.total_cycles;
                    rising_edge_handled = true;
                }
                5 => {
                    ch.load_triggered_reload();
                    // Mode 5: Rising edge triggers strobe
                    // OUT stays HIGH until terminal count
                    ch.counting = true;
                    ch.terminal_count_reached = false;
                    ch.counter = ch.active_reload();
                    ch.base_cycles = self.total_cycles;
                    rising_edge_handled = true;
                }
                _ => {}
            }
        }

        // Update counting state based on gate and mode
        // (skip if rising edge already configured counting above)
        if !rising_edge_handled {
            match ch.mode {
                0 => {
                    // GATE cannot resume Mode 0 while the MSB is missing.
                    ch.counting = high && ch.write_state == 0;
                }
                4 => {
                    // Mode 4: GATE enables/disables counting.
                    ch.counting = high;
                }
                2 | 3 => {
                    // Modes 2, 3: GATE LOW stops counting
                    ch.counting = high;
                    if !high {
                        // The 8254 forces OUT HIGH while GATE is low, even if
                        // the transition interrupted the low phase.
                        ch.out = true;
                    }
                }
                // Modes 1, 5: Gate trigger handled above
                _ => {}
            }
        }

        if let Some(current) = snapshot_value {
            ch.counter = current;
        }

        if !rising_edge_handled && !was_counting && ch.counting && !ch.is_gate_triggered() {
            self.resync_counter_start(ch_idx);
        }
    }

    /// Get the OUT pin state for a channel (computed on demand).
    ///
    /// Like `compute_counter()`, this uses the demand-driven timing model to
    /// determine the output pin state from elapsed time since `base_cycles`.
    /// Used to sample the timer output without advancing the scheduler
    /// that poll port 61h bit 5 waiting for the channel 2 OUT pin to
    /// transition at terminal count.
    pub fn get_out(&self, channel: u8) -> bool {
        if channel > 2 {
            return false;
        }
        self.compute_out(channel as usize)
    }

    /// Compute the OUT pin state on demand from elapsed time.
    fn compute_out(&self, ch_idx: usize) -> bool {
        let ch = &self.channels[ch_idx];

        // Not counting — return static OUT (set at mode init or gate change)
        if !ch.counting {
            return ch.out;
        }

        // Terminal count already flagged by the event-driven path. This is
        // Modes 0 and 1 ONLY: those latch OUT high at terminal count and hold
        // it. Modes 4 and 5 are the strobes — OUT drops for exactly one clock
        // AT terminal count and returns high — so they must fall through to
        // the elapsed-derived arm below, which already computes that pulse.
        // Testing `is_one_shot()` here (0, 1, 4, 5) swallowed the strobe
        // entirely: OUT read high before and after, and the pulse never
        // existed (MDD-BUG-TEM-00004).
        if matches!(ch.mode, 0 | 1) && ch.terminal_count_reached {
            return true;
        }

        let divisor = effective_divisor(ch.active_reload(), ch.bcd);
        if divisor == 0 {
            return ch.out;
        }

        let elapsed = self.total_cycles.saturating_sub(ch.base_cycles);
        let elapsed_pit_ticks =
            (elapsed as u128 * self.pit_hz as u128 / self.ips.max(1) as u128) as u64;

        match ch.mode {
            0 | 1 => {
                // Mode 0 (Interrupt on TC) / Mode 1 (HW Retriggerable One-Shot):
                // OUT is LOW during counting, HIGH at terminal count.
                elapsed_pit_ticks >= divisor
            }
            2 => {
                // Mode 2 (Rate Generator):
                // OUT HIGH for (N-1) ticks, LOW for 1 tick when counter = 1.
                (elapsed_pit_ticks % divisor) != (divisor - 1)
            }
            3 => {
                // Mode 3 (Square Wave): first half HIGH, second half LOW.
                // ch.out records the phase at base_cycles. Half-cycle events
                // can start LOW; full-period hooks and demand-driven channels
                // start HIGH.
                //
                // 8254 odd-divisor asymmetry: for odd divisor N, the HIGH phase
                // lasts (N+1)/2 clocks and the LOW phase lasts (N-1)/2 clocks.
                // For even divisors both halves are N/2 (unchanged).
                let pos = elapsed_pit_ticks % divisor;
                let high_half = divisor.div_ceil(2);
                if high_half == 0 {
                    return ch.out;
                }
                // When ch.out is true the first half is HIGH (the longer one);
                // when false the first half is LOW (the shorter one).
                let first_half_len = if ch.out { high_half } else { divisor / 2 };
                let in_first_half = pos < first_half_len;
                if ch.out {
                    in_first_half
                } else {
                    !in_first_half
                }
            }
            4 | 5 => {
                // Mode 4/5 (Software/Hardware Triggered Strobe):
                // OUT HIGH, LOW for 1 clock at terminal count, then HIGH.
                elapsed_pit_ticks != divisor
            }
            _ => ch.out,
        }
    }

    /// Get configured frequency divisor for a channel
    pub fn get_divisor(&self, channel: u8) -> u16 {
        if channel > 2 {
            return 0;
        }
        self.channels[channel as usize].active_reload()
    }

    /// Wall-clock interval between channel 0 interrupts, in microseconds.
    /// Pure computation from the current divisor and PIT frequency.
    pub fn interval_us(&self) -> u64 {
        let divisor = effective_divisor(self.channels[0].active_reload(), self.channels[0].bcd);
        let interval = (divisor * 1_000_000) / self.pit_hz.max(1) as u64;
        interval.clamp(1, 1_000_000)
    }

    /// Override PIT base frequency (Hz) used for host timing conversions.
    pub fn set_pit_hz(&mut self, pit_hz: u32) {
        let new_hz = pit_hz.max(1);
        let old_hz = self.pit_hz.max(1);
        if new_hz == old_hz {
            return;
        }
        let ips = self.ips.max(1);

        // `pit_ticks_per_instr_fp` caches pit_hz/ips, and `compute_counter`
        // works from that cache — so changing the oscillator without
        // recomputing it left one controller REPORTING at the new rate
        // (interval_us, the conversion APIs) while ADVANCING at the old one.
        // The comment here used to claim there was nothing to recalculate;
        // `set_emulated_speed` recomputes the same cache when it changes the
        // other half of the ratio (MDD-BUG-TEM-00007).
        //
        // Phase is carried across in PIT TICKS, the unit `compute_counter`
        // works in, for the reason set_emulated_speed documents: ticks already
        // counted did happen, and rebasing on `total_cycles` instead would
        // throw the phase away and snap every counting channel back to its
        // reload value (MDD-BUG-CRUCIBLE-00558).
        let mut elapsed_ticks = [0u128; 3];
        for i in 0..3 {
            let elapsed_cycles = self
                .total_cycles
                .saturating_sub(self.channels[i].base_cycles);
            elapsed_ticks[i] = u128::from(elapsed_cycles) * u128::from(old_hz) / u128::from(ips);
            // A stopped channel reads back this stored value verbatim.
            let counter = self.compute_counter(i);
            self.channels[i].counter = counter;
        }

        self.pit_hz = new_hz;
        self.pit_ticks_per_instr_fp = compute_fixed_point_ratio(new_hz, ips);

        for i in 0..3 {
            let hz = u128::from(new_hz);
            let mut offset = elapsed_ticks[i] * u128::from(ips) / hz;
            if offset > u128::from(self.total_cycles) {
                // More elapsed cycles than the clock has run: keep the phase
                // within the current period and drop the whole periods that
                // will not fit, exactly as set_emulated_speed does.
                let period = u128::from(self.channels[i].tick_period());
                offset = (elapsed_ticks[i] % period) * u128::from(ips) / hz;
            }
            self.channels[i].base_cycles = self.total_cycles.saturating_sub(offset as u64);
        }

        // Channel 0's period in CPU cycles just moved, so any armed IRQ0 event
        // is stale.
        self.event_reschedule_pending = true;
    }

    pub fn pit_hz(&self) -> u32 {
        self.pit_hz
    }

    /// Get the current mode for a channel
    pub fn get_mode(&self, channel: u8) -> u8 {
        if channel > 2 {
            return 0;
        }
        self.channels[channel as usize].mode
    }

    /// Get whether a channel's counter has been loaded
    pub fn get_loaded(&self, channel: u8) -> bool {
        if channel > 2 {
            return false;
        }
        self.channels[channel as usize].loaded
    }

    /// Get whether a channel is currently counting
    pub fn get_counting(&self, channel: u8) -> bool {
        if channel > 2 {
            return false;
        }
        self.channels[channel as usize].counting
    }

    /// Get the count waiting for a cycle boundary (2/3) or GATE trigger (1/5).
    pub fn get_deferred_reload(&self, channel: u8) -> Option<u16> {
        if channel > 2 {
            return None;
        }
        self.channels[channel as usize].deferred_reload
    }

    /// Capture PIT state without consuming counter or status latches.
    pub fn diagnostic_snapshot(&self) -> PitDiagnosticSnapshot {
        let channels = [0usize, 1, 2].map(|idx| {
            let ch = &self.channels[idx];
            PitChannelDiagnosticSnapshot {
                channel: idx as u8,
                current_counter: self.compute_counter(idx),
                reload: ch.reload,
                effective_divisor: effective_divisor(ch.active_reload(), ch.bcd),
                latched_counter: ch.latch,
                latched_status: ch.status_latch,
                mode: ch.mode,
                access_mode: ch.access_mode,
                read_state: ch.read_state,
                write_state: ch.write_state,
                out: self.compute_out(idx),
                gate: ch.gate,
                loaded: ch.loaded,
                terminal_count_reached: ch.terminal_count_reached,
                counting: ch.counting,
                bcd: ch.bcd,
                base_cycles: ch.base_cycles,
                deferred_reload: ch.deferred_reload,
            }
        });

        PitDiagnosticSnapshot {
            pit_hz: self.pit_hz,
            total_cycles: self.total_cycles,
            emulated_ips: self.ips,
            pit_ticks_per_instr_fp: self.pit_ticks_per_instr_fp,
            event_reschedule_pending: self.event_reschedule_pending,
            speaker_reload_updated: self.speaker_reload_updated,
            interval_us: self.interval_us(),
            cumulative_ticks: self.cumulative_ticks(),
            channels,
        }
    }

    /// Apply a deferred counter reload at cycle boundary (event-driven path).
    /// Called when a PIT event fires, applying any pending reload from a
    /// counter-only write (no preceding mode command in mode 2/3).
    pub fn apply_deferred_reload(&mut self, ch_idx: usize) {
        if ch_idx > 2 || self.channels[ch_idx].is_gate_triggered() {
            return;
        }
        let ch = &mut self.channels[ch_idx];
        if ch.deferred_reload.is_some() {
            ch.load_triggered_reload();
            ch.counter = ch.active_reload();
        }
    }

    /// Transfer a Mode 3 count at the end of either half-cycle and start
    /// the new half in the phase delivered by the scheduler.
    pub fn mode3_half_cycle_boundary(&mut self, ch_idx: usize, scheduled_cycle: u64, rising: bool) {
        if ch_idx > 2 || self.channels[ch_idx].mode != 3 {
            return;
        }
        self.apply_deferred_reload(ch_idx);
        let ch = &mut self.channels[ch_idx];
        ch.base_cycles = scheduled_cycle;
        ch.out = rising;
    }

    /// Get current tick count (stub - maintained in BIOS data area)
    pub fn get_ticks(&self) -> u32 {
        // Tick count is maintained in BIOS data area at 0040:006C
        // This is a stub for compatibility
        0
    }

    // THROWAWAY-SPIKE: Gremlin determinism Step-0.
    // `cumulative_ticks()` was previously stubbed; this implementation derives
    // the count purely from `total_cycles`, `pit_hz`, `ips`, and channel 0's
    // current divisor — no host clock involvement, so the result is
    // bit-deterministic across runs given the same instruction stream.
    // Remove if and only if no non-spike caller has adopted it by the time
    // the Step-0 shim is torn down.

    /// Cumulative count of channel-0 PIT periods elapsed since boot,
    /// derived purely from `total_cycles` and the current divisor.
    ///
    /// This matches the BIOS 18.2 Hz tick rate when the BIOS-default divisor
    /// (0 → 65536) is programmed. Note: when guest code reprograms channel 0
    /// mid-run the value reflects the *current* divisor only and does not
    /// retroactively account for prior divisor values.
    pub fn cumulative_ticks(&self) -> u64 {
        // elapsed PIT oscillator ticks since boot
        let elapsed_pit_ticks =
            ((self.total_cycles as u128 * self.pit_ticks_per_instr_fp as u128) >> 32) as u64;
        let divisor = effective_divisor(self.channels[0].active_reload(), self.channels[0].bcd);
        elapsed_pit_ticks / divisor.max(1)
    }

    /// Set the emulated CPU speed. Determines the ratio between CPU
    /// guest CPU cycles and PIT oscillator ticks.
    ///
    /// `guest_cycles_per_second`: How many guest CPU cycles = one second
    /// of emulated time. Derived from cycles_per_frame * target_fps.
    pub fn set_emulated_speed(&mut self, guest_cycles_per_second: u64) {
        let new_ips = guest_cycles_per_second.max(1);
        let old_ips = self.ips.max(1);

        // Carry each channel's PHASE across the rate change, measured in PIT
        // ticks — the unit `compute_counter` actually works in. Setting
        // base_cycles = total_cycles threw the phase away: elapsed became zero
        // and every counting channel's guest-visible count jumped straight back
        // to its reload value (MDD-BUG-CRUCIBLE-00558). Working in ticks rather
        // than inverting the count keeps this independent of mode, BCD and the
        // mode-3 decrement-by-two encoding.
        let mut elapsed_ticks = [0u128; 3];
        for i in 0..3 {
            let elapsed_cycles = self
                .total_cycles
                .saturating_sub(self.channels[i].base_cycles);
            elapsed_ticks[i] =
                u128::from(elapsed_cycles) * u128::from(self.pit_hz) / u128::from(old_ips);
            // A stopped channel reads back this stored value verbatim.
            let counter = self.compute_counter(i);
            self.channels[i].counter = counter;
        }

        self.ips = new_ips;
        self.pit_ticks_per_instr_fp = compute_fixed_point_ratio(self.pit_hz, new_ips);

        for i in 0..3 {
            let hz = u128::from(self.pit_hz);
            let mut offset = elapsed_ticks[i] * u128::from(new_ips) / hz;
            if offset > u128::from(self.total_cycles) {
                // The new rate needs more elapsed cycles than the clock has run
                // to express the whole tick history. `compute_counter` only
                // reads the phase within the current period, so keep that and
                // drop the whole periods that will not fit. Only reachable very
                // early in a run, or after a large rate increase.
                let period = u128::from(self.channels[i].tick_period());
                offset = (elapsed_ticks[i] % period) * u128::from(new_ips) / hz;
            }
            self.channels[i].base_cycles = self.total_cycles.saturating_sub(offset as u64);
        }
    }

    /// Convert guest CPU cycles to PIT oscillator ticks as a rational number.
    ///
    /// Carry the returned remainder across periodic conversions when the
    /// conversion represents a repeated clock interval.
    pub fn instructions_to_oscillator_ticks_ratio(&self, guest_cycles: u64) -> (u128, u64) {
        (guest_cycles as u128 * self.pit_hz as u128, self.ips.max(1))
    }

    /// Convert guest CPU cycles to PIT oscillator ticks using the current speed setting.
    pub fn instructions_to_oscillator_ticks(&self, guest_cycles: u64) -> u64 {
        let (numerator, denominator) = self.instructions_to_oscillator_ticks_ratio(guest_cycles);
        (numerator / denominator as u128) as u64
    }

    /// Get the configured emulated speed in guest cycles per second.
    pub fn emulated_ips(&self) -> u64 {
        self.ips
    }

    fn compute_counter(&self, ch_idx: usize) -> u16 {
        let ch = &self.channels[ch_idx];

        if !ch.counting {
            return ch.counter;
        }

        let divisor = effective_divisor(ch.active_reload(), ch.bcd);

        // Convert elapsed guest cycles to PIT oscillator ticks using precomputed
        // fixed-point ratio (avoids expensive u128 division on every read).
        let elapsed = self.total_cycles.saturating_sub(ch.base_cycles);
        let elapsed_pit_ticks =
            ((elapsed as u128 * self.pit_ticks_per_instr_fp as u128) >> 32) as u64;

        let max_count = if ch.bcd { 10000u64 } else { 65536u64 };

        let binary_result = if ch.is_one_shot() {
            // Real 8254: the counting element is a free-running 16-bit
            // down-counter that wraps past 0 → 0xFFFF → 0xFFFE …
            // After terminal count (elapsed >= divisor), the counter keeps
            // going; polling loops detect completion by observing
            // the wrap (counter > previous_reading).
            if ch.bcd {
                // BCD counts wrap through 9999, not through binary 0xFFFF.
                // Keep the timing value decoded until the I/O-boundary
                // conversion below so values such as 9998 are not clamped to
                // 9999 by binary_to_bcd().
                let modulus = 10_000;
                ((divisor + modulus - (elapsed_pit_ticks % modulus)) % modulus) as u16
            } else {
                (divisor as u16).wrapping_sub((elapsed_pit_ticks % 65536) as u16)
            }
        } else if ch.mode == 3 {
            // Count 1 is illegal in Mode 3 on the 8254. Preserve the
            // existing compatibility value for callers that program it.
            if divisor == 1 {
                return 2;
            }
            // Intel 8254 Mode 3: even counts reload N and odd counts
            // reload N-1, then decrement by two. Odd HIGH holds zero
            // for one clock before the LOW reload, so a half-divisor
            // modulus loses that clock and drifts relative to OUT.
            let pos = elapsed_pit_ticks % divisor;
            let first_half_len = if ch.out {
                divisor.div_ceil(2)
            } else {
                divisor / 2
            };
            let half_pos = if pos < first_half_len {
                pos
            } else {
                pos - first_half_len
            };
            let counter = ((divisor & !1) - 2 * half_pos) as u16;
            return if ch.bcd {
                binary_to_bcd(counter)
            } else {
                counter
            };
        } else {
            // Continuous modes 2, 4, 5
            let remaining = divisor - (elapsed_pit_ticks % divisor);
            if remaining > max_count {
                0
            } else if ch.mode == 2 && remaining == divisor && divisor > 65535 {
                // The divisor-65536 reload count truncates to zero in u16.
                // This model exposes 0xFFFF instead, shifting the sample by
                // one clock rather than reporting the encoded reload value.
                0xFFFF
            } else {
                remaining as u16
            }
        };

        // BCD mode: convert binary counter to BCD encoding
        if ch.bcd {
            binary_to_bcd(binary_result)
        } else {
            binary_result
        }
    }

    /// Returns the PIT scheduling mode for channel 0.
    pub fn channel0_mode(&self) -> PitMode {
        match self.channels[0].mode {
            2 => PitMode::RateGenerator,
            3 => PitMode::SquareWave,
            _ => PitMode::OneShot, // modes 0, 1, 4, 5
        }
    }

    /// Channel 0 period as a rational number of CPU cycles.
    ///
    /// The caller can carry the returned remainder between periods instead of
    /// flooring every interval independently. `count 0` means divisor 65536,
    /// as on the 8254. The denominator uses `self.pit_hz` rather than the
    /// default constant so differential harness overrides remain exact.
    pub fn channel0_period_cycles_ratio(&self, cycles_per_ms: u64) -> (u128, u64) {
        let ch = &self.channels[0];
        let divisor = effective_divisor(ch.active_reload(), ch.bcd);
        let numerator = divisor as u128 * cycles_per_ms as u128 * 1000;
        (numerator, self.pit_hz.max(1) as u64)
    }

    /// Whether channel 0 can currently produce an OUT transition, and so
    /// whether a periodic IRQ0 event may be scheduled from it.
    ///
    /// Two conditions, both real 8254 behaviour:
    ///
    /// - `loaded` — a complete count has been written. A channel that has
    ///   been reprogrammed but not yet reloaded has no period to fire on.
    /// - `counting` — for the gate-triggered one-shots (Modes 1 and 5) this
    ///   stays false until the GATE rising edge fires the shot, and for Modes
    ///   2 and 3 it goes false while GATE is held low. Either way an
    ///   un-triggered or gated-off channel reaches no terminal count.
    ///
    /// Schedulers previously derived a period from the divisor alone
    /// (`channel0_period_cycles` still does — it is pure arithmetic and has no
    /// view of state), so an unloaded or un-triggered channel 0 still raised
    /// IRQ0 every period (MDD-BUG-TEM-00002).
    #[must_use]
    pub fn channel0_is_schedulable(&self) -> bool {
        let ch = &self.channels[0];
        ch.loaded && ch.counting
    }

    /// Channel 0 period in CPU cycles (instructions).
    ///
    /// This compatibility view floors the rational period. Periodic event
    /// schedulers should use [`Self::channel0_period_cycles_ratio`] and carry
    /// its remainder across events to avoid systematic drift.
    pub fn channel0_period_cycles(&self, cycles_per_ms: u64) -> u64 {
        let (numerator, denominator) = self.channel0_period_cycles_ratio(cycles_per_ms);
        let period = (numerator / denominator as u128) as u64;
        period.max(10) // Clamp to minimum 10 cycles (safety)
    }

    /// Advance PIT internal state to `scheduled_cycle` for event-driven timing.
    /// Mode 3 callers use this compatibility hook at a full-period rising
    /// edge. Half-cycle schedulers use [`Self::mode3_half_cycle_boundary`].
    /// For all modes, advances base_cycles to the scheduled fire time.
    pub fn track_time(&mut self, ch_idx: usize, scheduled_cycle: u64) {
        let ch = &mut self.channels[ch_idx];
        ch.base_cycles = scheduled_cycle;
        // Mode 3: a new period starts in the HIGH half.
        //
        // This used to TOGGLE, described as a half-period hook — but both
        // production callers are the channel 0 IRQ boundary, which the
        // scheduler places at the FULL divisor (`channel0_period_cycles`).
        // Toggling there inverted the phase every period: `compute_out` reads
        // `ch.out` to decide which half comes first, so period N ended LOW and
        // period N+1 began LOW, welding adjacent half-periods into one
        // double-width low pulse and destroying the square wave
        // (MDD-BUG-TEM-00005).
        //
        // Setting it rather than toggling also makes channel 0 agree with
        // channels 1 and 2, which never call this and whose `out` stays HIGH
        // precisely so the elapsed-time expression yields a clean wave.
        if ch.mode == 3 {
            ch.out = true;
        }
    }

    /// Mark channel 0 as having reached terminal count (used by event-driven
    /// one-shot handling). In Mode 0, OUT goes HIGH at terminal count.
    pub fn set_channel0_terminal_count(&mut self) {
        self.channels[0].terminal_count_reached = true;
        self.channels[0].out = true;
    }

    // ── Checkpoint save/restore ──────────────────────────────────────────

    /// Save per-channel state for checkpoint.
    pub fn save_channel_state(&self, ch_idx: usize) -> PitChannelState {
        let ch = &self.channels[ch_idx.min(2)];
        PitChannelState {
            counter: ch.counter,
            reload: ch.reload,
            latch: ch.latch,
            mode: ch.mode,
            raw_mode: ch.raw_mode,
            access_mode: ch.access_mode,
            read_state: ch.read_state,
            write_state: ch.write_state,
            out: ch.out,
            gate: ch.gate,
            loaded: ch.loaded,
            null_count: ch.null_count,
            terminal_count_reached: ch.terminal_count_reached,
            counting: ch.counting,
            bcd: ch.bcd,
            status_latch: ch.status_latch,
            base_cycles: ch.base_cycles,
            deferred_reload: ch.deferred_reload,
            write_pair_old_reload: ch.write_pair_old_reload,
        }
    }

    /// Restore per-channel state from checkpoint.
    pub fn restore_channel_state(&mut self, ch_idx: usize, state: &PitChannelState) {
        let ch = &mut self.channels[ch_idx.min(2)];
        ch.counter = state.counter;
        ch.reload = state.reload;
        ch.latch = state.latch;
        ch.mode = state.mode;
        ch.raw_mode = state.raw_mode;
        ch.access_mode = state.access_mode;
        ch.read_state = state.read_state;
        ch.write_state = state.write_state;
        ch.out = state.out;
        ch.gate = state.gate;
        ch.loaded = state.loaded;
        ch.null_count = state.null_count;
        ch.terminal_count_reached = state.terminal_count_reached;
        ch.counting = state.counting;
        ch.bcd = state.bcd;
        ch.status_latch = state.status_latch;
        ch.base_cycles = state.base_cycles;
        ch.deferred_reload = state.deferred_reload;
        ch.write_pair_old_reload = state.write_pair_old_reload;
    }

    /// Save controller-level state for checkpoint.
    /// Returns: (pit_hz, total_cycles, ips)
    pub fn save_controller_state(&self) -> (u32, u64, u64) {
        (self.pit_hz, self.total_cycles, self.ips)
    }

    /// Restore controller-level state from checkpoint.
    pub fn restore_controller_state(&mut self, pit_hz: u32, total_cycles: u64, ips: u64) {
        self.pit_hz = pit_hz;
        self.total_cycles = total_cycles;
        self.ips = ips;
        // Recompute the derived instruction->PIT-tick ratio. Counter math
        // (compute_counter) uses this cached ratio, and it is NOT part of the
        // checkpoint tuple, so restoring into a PIT that previously had a different
        // speed would otherwise keep the stale ratio and advance counters at the
        // wrong rate. MDD-BUG-CRUCIBLE-00032.
        self.pit_ticks_per_instr_fp = compute_fixed_point_ratio(pit_hz, ips);
        self.event_reschedule_pending = true; // Signal re-scheduling needed
    }
}

impl Default for Pit {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod counter_latch_command_tests {
    use super::*;

    /// Channel 0 in Mode 0 with the full 16-bit count: a monotonically
    /// falling counter, so two latches taken at different times differ.
    fn counting_channel0() -> Pit {
        let mut pit = Pit::new_unprogrammed(1_193_182, 1_000_000);
        pit.write_command(0x30); // channel 0, LSB/MSB, mode 0, binary
        pit.write_counter(0, 0xFF);
        pit.write_counter(0, 0xFF);
        pit
    }

    /// Reads a full LSB/MSB pair and assembles it.
    fn read_word(pit: &mut Pit) -> u16 {
        let lsb = u16::from(pit.read_counter(0));
        let msb = u16::from(pit.read_counter(0));
        (msb << 8) | lsb
    }

    /// Program one channel through its I/O ports in Mode 0 using the selected
    /// count format.
    fn program_channel(pit: &mut Pit, channel: u8, access: u8, count: u16) {
        pit.write(0x43, (channel << 6) | (access << 4));
        let port = 0x40 + u16::from(channel);
        match access {
            1 => pit.write(port, count as u8),
            2 => pit.write(port, (count >> 8) as u8),
            3 => {
                pit.write(port, count as u8);
                pit.write(port, (count >> 8) as u8);
            }
            _ => unreachable!("test uses a valid access mode"),
        }
    }

    fn read_programmed_count(pit: &mut Pit, channel: u8, access: u8) -> u16 {
        match access {
            1 => u16::from(pit.read_counter(channel)),
            2 => u16::from(pit.read_counter(channel)) << 8,
            3 => {
                let lsb = u16::from(pit.read_counter(channel));
                let msb = u16::from(pit.read_counter(channel));
                (msb << 8) | lsb
            }
            _ => unreachable!("test uses a valid access mode"),
        }
    }

    /// Reprogramming a counter releases its old unread count, even when the
    /// new control word uses the same mode and access format.
    #[test]
    fn mode_reprogram_releases_unread_count_for_each_channel_and_access_mode() {
        for channel in 0..3 {
            for access in 1..=3 {
                let mut pit = Pit::new_unprogrammed(1_193_182, 1_000_000);
                program_channel(&mut pit, channel, access, 0x1234);
                pit.write(0x43, channel << 6); // latch old count

                program_channel(&mut pit, channel, access, 0x5678);
                pit.write(0x43, channel << 6); // latch new count

                let expected = match access {
                    1 => 0x0078,
                    2 => 0x5600,
                    3 => 0x5678,
                    _ => unreachable!("test uses a valid access mode"),
                };
                assert_eq!(
                    read_programmed_count(&mut pit, channel, access),
                    expected,
                    "channel {channel}, access mode {access}"
                );
            }
        }
    }

    /// A mode write also restarts an LSB/MSB read pair after its LSB has been
    /// consumed, so the next pair comes wholly from the newly programmed count.
    #[test]
    fn mode_reprogram_releases_a_partially_read_count_for_each_channel() {
        for channel in 0..3 {
            let mut pit = Pit::new_unprogrammed(1_193_182, 1_000_000);
            program_channel(&mut pit, channel, 3, 0x1234);
            pit.write(0x43, channel << 6); // latch old count
            assert_eq!(pit.read_counter(channel), 0x34);

            program_channel(&mut pit, channel, 3, 0x5678);
            pit.write(0x43, channel << 6); // latch new count

            assert_eq!(
                read_programmed_count(&mut pit, channel, 3),
                0x5678,
                "channel {channel}"
            );
        }
    }

    /// Ordinary and Read-Back count latches capture the new count after
    /// reprogramming invalidates either kind of prior latch.
    #[test]
    fn readback_count_after_mode_reprogram_captures_the_new_count() {
        for channel in 0..3 {
            for old_latch_is_readback in [true, false] {
                for new_latch_is_readback in [false, true] {
                    let mut pit = Pit::new_unprogrammed(1_193_182, 1_000_000);
                    program_channel(&mut pit, channel, 3, 0x1234);
                    if old_latch_is_readback {
                        pit.write(0x43, 0xD0 | (1 << (channel + 1))); // old read-back count
                    } else {
                        pit.write(0x43, channel << 6); // old ordinary count latch
                    }

                    program_channel(&mut pit, channel, 3, 0x5678);
                    if new_latch_is_readback {
                        pit.write(0x43, 0xD0 | (1 << (channel + 1))); // new read-back count
                    } else {
                        pit.write(0x43, channel << 6); // new ordinary count latch
                    }

                    assert_eq!(
                        read_programmed_count(&mut pit, channel, 3),
                        0x5678,
                        "channel {channel}, old read-back latch: {old_latch_is_readback}, \
                         new read-back latch: {new_latch_is_readback}"
                    );
                }
            }
        }
    }

    /// **A second Counter Latch Command is ignored until the first is read.**
    ///
    /// Intel 8254, Counter Latch Command: "If the Counter is latched and then,
    /// some time later, latched again before the count is read, the second
    /// Counter Latch Command is ignored. The count read will be the count at
    /// the time the first Counter Latch Command was issued."
    ///
    /// `write_command` captured unconditionally, so the second latch replaced
    /// the unread value and a program that re-latches defensively read a
    /// LATER count than the one it latched (MDD-BUG-TEM-00008). The read-back
    /// command path already had this guard.
    #[test]
    fn a_second_latch_command_does_not_replace_an_unread_value() {
        let mut pit = counting_channel0();

        pit.total_cycles += 1_000;
        pit.write_command(0x00); // first latch
        let first = pit.channels[0]
            .latch
            .expect("the first latch captured a count");

        pit.total_cycles += 10_000;
        assert_ne!(
            pit.compute_counter(0),
            first,
            "the counter must genuinely move between the two latches, \
             or this test cannot tell the two values apart"
        );

        pit.write_command(0x00); // second latch, before any read
        assert_eq!(
            read_word(&mut pit),
            first,
            "the read must return the count latched by the FIRST command"
        );
    }

    /// The ignored command must not restart the read sequence either.
    ///
    /// A mid-read latch that resets `read_state` makes the next read hand back
    /// an LSB where the caller is expecting the MSB of the pair it started, so
    /// the assembled word mixes two different counts.
    #[test]
    fn a_second_latch_command_does_not_restart_a_partial_read() {
        let mut pit = counting_channel0();

        pit.total_cycles += 1_000;
        pit.write_command(0x00);
        let first = pit.channels[0]
            .latch
            .expect("the first latch captured a count");
        let lsb = u16::from(pit.read_counter(0));

        pit.total_cycles += 10_000;
        pit.write_command(0x00); // ignored: the pair is still half-read
        let msb = u16::from(pit.read_counter(0));

        assert_eq!(
            (msb << 8) | lsb,
            first,
            "both halves must come from the first latched count"
        );
    }

    /// The guard suppresses a duplicate, not every later latch.
    #[test]
    fn a_latch_after_the_value_is_consumed_captures_a_fresh_count() {
        let mut pit = counting_channel0();

        pit.total_cycles += 1_000;
        pit.write_command(0x00);
        let first = read_word(&mut pit); // consumes the latch

        pit.total_cycles += 10_000;
        pit.write_command(0x00);
        let second = read_word(&mut pit);

        assert!(
            second < first,
            "once read, the next latch must capture the current count: \
             {first} then {second}"
        );
    }
}

#[cfg(test)]
mod counter_latch_phase_tests {
    use super::*;

    const CLOCKS: &[(&str, u32, u64, bool)] = &[
        (
            "default 1:1 oscillator",
            PIT_FREQUENCY,
            PIT_FREQUENCY as u64,
            true,
        ),
        (
            "default oscillator at 25.2M PC",
            PIT_FREQUENCY,
            25_200_000,
            true,
        ),
        (
            "fractional default oscillator",
            PIT_FREQUENCY,
            3_600_000,
            false,
        ),
        (
            "fractional alternate oscillator",
            4_000_000,
            10_000_000,
            false,
        ),
        ("zero-clock fallback", 0, 0, false),
    ];

    #[derive(Clone, Copy)]
    enum LatchKind {
        Ordinary,
        ReadBack,
    }

    impl LatchKind {
        fn command(self, channel: u8) -> u8 {
            match self {
                Self::Ordinary => channel << 6,
                Self::ReadBack => 0xD0 | (1 << (channel + 1)),
            }
        }

        fn name(self) -> &'static str {
            match self {
                Self::Ordinary => "ordinary",
                Self::ReadBack => "read-back",
            }
        }
    }

    fn setup_mode3(
        channel: u8,
        reload: u16,
        oscillator_hz: u32,
        ips: u64,
        default_clock: bool,
    ) -> Pit {
        let mut pit = if default_clock {
            let mut pit = Pit::new();
            pit.set_emulated_speed(ips);
            pit
        } else {
            Pit::new_unprogrammed(oscillator_hz, ips)
        };
        pit.write_command((channel << 6) | 0x36);
        pit.write_counter(channel, reload as u8);
        pit.write_counter(channel, (reload >> 8) as u8);
        pit
    }

    fn read_latched_word(pit: &mut Pit, channel: u8, kind: LatchKind) -> u16 {
        pit.write_command(kind.command(channel));
        let low = u16::from(pit.read_counter(channel));
        let high = u16::from(pit.read_counter(channel));
        (high << 8) | low
    }

    fn cycles_at_tick(pit: &Pit, tick: u64) -> u64 {
        let pit_hz = u128::from(pit.pit_hz.max(1));
        let ips = u128::from(pit.ips.max(1));
        (u128::from(tick) * ips).div_ceil(pit_hz) as u64
    }

    fn assert_mode3_latch_preserves_phase(
        channel: u8,
        reload: u16,
        oscillator_hz: u32,
        ips: u64,
        default_clock: bool,
        clock_name: &str,
        kind: LatchKind,
    ) {
        let mut pit = setup_mode3(channel, reload, oscillator_hz, ips, default_clock);
        let mut control = pit.clone();
        let first_half_ticks = u64::from(reload.div_ceil(2));
        let anchor_tick = if reload.is_multiple_of(2) {
            u64::from(reload / 2)
        } else {
            u64::from(reload)
        };
        pit.total_cycles = cycles_at_tick(&pit, anchor_tick);
        control.total_cycles = pit.total_cycles;

        let controller_before = pit.diagnostic_snapshot();
        let channel_before = controller_before.channels[channel as usize];
        let latched_count = read_latched_word(&mut pit, channel, kind);
        assert_eq!(
            latched_count,
            channel_before.current_counter,
            "{} latch changed channel {channel} Mode 3 count {reload} at {anchor_tick} PIT ticks ({clock_name})",
            kind.name(),
        );

        let controller_after = pit.diagnostic_snapshot();
        let channel_after = controller_after.channels[channel as usize];
        assert_eq!(
            channel_after.current_counter,
            channel_before.current_counter,
            "{} latch changed the live Mode 3 count ({clock_name})",
            kind.name(),
        );
        assert_eq!(
            channel_after.out,
            channel_before.out,
            "{} latch changed Mode 3 OUT at the sample clock ({clock_name})",
            kind.name(),
        );
        assert_eq!(
            channel_after.base_cycles,
            channel_before.base_cycles,
            "{} latch rebased the channel clock ({clock_name})",
            kind.name(),
        );
        assert_eq!(
            controller_after.event_reschedule_pending,
            controller_before.event_reschedule_pending,
            "{} latch changed channel 0 scheduler state ({clock_name})",
            kind.name(),
        );
        assert_eq!(
            pit.channel0_period_cycles_ratio(1_000_000),
            control.channel0_period_cycles_ratio(1_000_000),
            "{} latch changed the channel 0 period ratio ({clock_name})",
            kind.name(),
        );

        for transition in [
            anchor_tick + first_half_ticks,
            anchor_tick + u64::from(reload),
            anchor_tick + u64::from(reload) + first_half_ticks,
            anchor_tick + 2 * u64::from(reload),
        ] {
            for tick in transition.saturating_sub(1)..=transition + 1 {
                let cycles = cycles_at_tick(&pit, tick);
                pit.total_cycles = cycles;
                control.total_cycles = cycles;
                assert_eq!(
                    pit.get_out(channel),
                    control.get_out(channel),
                    "{} latch changed channel {channel} Mode 3 OUT at {tick} PIT ticks ({clock_name})",
                    kind.name()
                );
                assert_eq!(
                    pit.diagnostic_snapshot().channels[channel as usize].current_counter,
                    control.diagnostic_snapshot().channels[channel as usize].current_counter,
                    "{} latch changed channel {channel} Mode 3 count at {tick} PIT ticks ({clock_name})",
                    kind.name()
                );
            }
        }
    }

    #[test]
    fn ordinary_latches_preserve_mode3_counter_and_waveform() {
        for channel in [2, 0] {
            for reload in [4, 6, 3, 5] {
                for &(clock_name, oscillator_hz, ips, default_clock) in CLOCKS {
                    assert_mode3_latch_preserves_phase(
                        channel,
                        reload,
                        oscillator_hz,
                        ips,
                        default_clock,
                        clock_name,
                        LatchKind::Ordinary,
                    );
                }
            }
        }
    }

    #[test]
    fn readback_count_latches_preserve_mode3_counter_and_waveform() {
        for channel in [2, 0] {
            for reload in [4, 6, 3, 5] {
                for &(clock_name, oscillator_hz, ips, default_clock) in CLOCKS {
                    assert_mode3_latch_preserves_phase(
                        channel,
                        reload,
                        oscillator_hz,
                        ips,
                        default_clock,
                        clock_name,
                        LatchKind::ReadBack,
                    );
                }
            }
        }
    }

    fn assert_latch_preserves_channel_sample(
        pit: &mut Pit,
        channel: u8,
        kind: LatchKind,
        description: &str,
    ) {
        let before = pit.diagnostic_snapshot().channels[channel as usize];
        let sampled = read_latched_word(pit, channel, kind);
        assert_eq!(sampled, before.current_counter, "{description}");
        let after = pit.diagnostic_snapshot().channels[channel as usize];
        assert_eq!(
            after.current_counter, before.current_counter,
            "{description}"
        );
        assert_eq!(after.out, before.out, "{description}");
        assert_eq!(after.base_cycles, before.base_cycles, "{description}");
    }

    #[test]
    fn other_mode_latches_preserve_running_phase_and_stopped_channel_state() {
        for kind in [LatchKind::Ordinary, LatchKind::ReadBack] {
            let mut mode2 = Pit::new_unprogrammed(PIT_FREQUENCY, PIT_FREQUENCY as u64);
            mode2.write_command(0xB4); // Channel 2, Mode 2, LSB/MSB.
            mode2.write_counter(2, 5);
            mode2.write_counter(2, 0);
            mode2.total_cycles = 5;
            assert_latch_preserves_channel_sample(
                &mut mode2,
                2,
                kind,
                "latching a running Mode 2 channel changed its live phase",
            );

            let mut unloaded = Pit::new_unprogrammed(PIT_FREQUENCY, PIT_FREQUENCY as u64);
            assert!(!unloaded.get_loaded(0));
            assert_latch_preserves_channel_sample(
                &mut unloaded,
                0,
                kind,
                "latching an unloaded channel changed its sample",
            );

            let mut gated_off = setup_mode3(2, 6, PIT_FREQUENCY, PIT_FREQUENCY as u64, false);
            gated_off.total_cycles = 3;
            gated_off.set_gate(2, false);
            assert!(!gated_off.get_counting(2));
            assert_latch_preserves_channel_sample(
                &mut gated_off,
                2,
                kind,
                "latching a gated-off channel changed its frozen sample",
            );
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod set_pit_hz_tests {
    use super::*;

    /// Advances by a fixed number of guest cycles and reports how far the
    /// channel 0 counter moved.
    fn counter_drop_over(pit: &mut Pit, cycles: u64) -> i32 {
        let before = i32::from(pit.compute_counter(0));
        pit.total_cycles += cycles;
        let after = i32::from(pit.compute_counter(0));
        before - after
    }

    /// **Counter advance must follow the oscillator the controller reports.**
    ///
    /// `compute_counter` works from the cached `pit_ticks_per_instr_fp` ratio.
    /// `set_pit_hz` updated `pit_hz` but not that cache, so one controller
    /// REPORTED the new frequency through `interval_us` and the conversion
    /// APIs while still ADVANCING its counters at the old rate
    /// (MDD-BUG-TEM-00007).
    #[test]
    fn counter_deltas_scale_with_a_new_oscillator_frequency() {
        let mut pit = Pit::new_unprogrammed(1_000_000, 1_000_000);
        pit.write_command(0x34); // channel 0, LSB/MSB, mode 2, binary
        pit.write_counter(0, 0xFF);
        pit.write_counter(0, 0xFF); // a large divisor, so no wrap interferes

        let before = counter_drop_over(&mut pit, 1_000);

        // Double the oscillator: the same guest-cycle advance must now consume
        // twice as many PIT ticks.
        pit.set_pit_hz(2_000_000);
        let after = counter_drop_over(&mut pit, 1_000);

        assert!(
            before > 0,
            "the baseline advance must actually move: {before}"
        );
        assert_eq!(
            after,
            before * 2,
            "after doubling the oscillator the same cycle span must consume \
             twice the ticks; equal deltas mean the cached ratio went stale"
        );
    }

    /// The reported interval and the advance rate must agree.
    #[test]
    fn the_reported_interval_matches_the_rate_actually_used() {
        let mut pit = Pit::new_unprogrammed(1_000_000, 1_000_000);
        pit.write_command(0x34);
        pit.write_counter(0, 0xFF);
        pit.write_counter(0, 0xFF);

        pit.set_pit_hz(2_000_000);

        // The cache is what compute_counter consults; it must describe the
        // frequency the controller now reports.
        assert_eq!(pit.pit_hz(), 2_000_000);
        assert_eq!(
            pit.pit_ticks_per_instr_fp,
            compute_fixed_point_ratio(2_000_000, pit.ips.max(1)),
            "the cached ratio must be recomputed for the new oscillator"
        );
    }

    /// Changing the oscillator must not snap a running counter back to reload.
    ///
    /// The phase already counted really happened; rebasing on `total_cycles`
    /// would discard it, which is the failure MDD-BUG-CRUCIBLE-00558 records
    /// for the sibling `set_emulated_speed`.
    #[test]
    fn a_rate_change_carries_the_current_phase() {
        let mut pit = Pit::new_unprogrammed(1_000_000, 1_000_000);
        pit.write_command(0x34);
        pit.write_counter(0, 0xFF);
        pit.write_counter(0, 0xFF);

        pit.total_cycles += 1_000;
        let before = pit.compute_counter(0);
        assert_ne!(before, 0xFFFF, "the counter has genuinely advanced");

        pit.set_pit_hz(2_000_000);
        let after = pit.compute_counter(0);

        assert_ne!(
            after, 0xFFFF,
            "the change must not throw the phase away and snap back to reload"
        );
    }

    /// A no-op change stays a no-op.
    #[test]
    fn setting_the_same_frequency_changes_nothing() {
        let mut pit = Pit::new_unprogrammed(1_000_000, 1_000_000);
        pit.write_command(0x34);
        pit.write_counter(0, 0xFF);
        pit.write_counter(0, 0xFF);
        pit.total_cycles += 1_000;

        let before = pit.compute_counter(0);
        let base = pit.channels[0].base_cycles;
        pit.set_pit_hz(1_000_000);

        assert_eq!(pit.compute_counter(0), before);
        assert_eq!(pit.channels[0].base_cycles, base);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod immediate_reload_mask_tests {
    use super::*;

    /// Programs channel 2 in Mode 2 with a divisor, then rewrites the count
    /// only. Returns the divisor visible immediately after that rewrite.
    fn divisor_after_counter_only_rewrite(pit: &mut Pit) -> u16 {
        pit.write_command(0xB4); // channel 2, LSB/MSB, mode 2, binary
        pit.write_counter(2, 0x00);
        pit.write_counter(2, 0x10); // divisor 0x1000, loaded and counting

        // Counter-only rewrite: no control word, so the channel stays loaded.
        pit.write_counter(2, 0x34);
        pit.write_counter(2, 0x12); // would be 0x1234
        pit.get_divisor(2)
    }

    /// **The mask, not the channel number, decides immediate commit.**
    ///
    /// A clear immediate-reload mask defers every running periodic channel;
    /// the PC speaker mask instead commits channel 2 at the completed write.
    #[test]
    fn a_neutral_controller_defers_a_channel_2_rewrite_where_the_pc_commits_it() {
        // PC speaker: mask names channel 2, so the write lands at once.
        let mut pc = Pit::new();
        assert_eq!(
            divisor_after_counter_only_rewrite(&mut pc),
            0x1234,
            "the PC speaker path must still apply a channel 2 rewrite immediately"
        );

        // Machine-neutral: mask is clear, so the same sequence defers to the
        // next cycle boundary and the channel keeps its current divisor.
        let mut neutral = Pit::new_unprogrammed(1_193_182, 1_000_000);
        assert_eq!(
            divisor_after_counter_only_rewrite(&mut neutral),
            0x1000,
            "a neutral controller must defer the rewrite, not restart the cycle"
        );
    }

    /// The deferral is a deferral, not a discard: the value is staged.
    #[test]
    fn the_neutral_controllers_deferred_value_is_retained() {
        let mut neutral = Pit::new_unprogrammed(1_193_182, 1_000_000);
        divisor_after_counter_only_rewrite(&mut neutral);

        assert_eq!(
            neutral.get_deferred_reload(2),
            Some(0x1234),
            "the new count waits at the next cycle boundary rather than being lost"
        );
    }

    /// Channels 0 and 1 were never hard-coded and must be unaffected.
    #[test]
    fn the_other_channels_defer_under_both_constructors() {
        for (name, mut pit) in [
            ("pc", Pit::new()),
            ("neutral", Pit::new_unprogrammed(1_193_182, 1_000_000)),
        ] {
            pit.write_command(0x74); // channel 1, LSB/MSB, mode 2, binary
            pit.write_counter(1, 0x00);
            pit.write_counter(1, 0x10);
            pit.write_counter(1, 0x34);
            pit.write_counter(1, 0x12);

            assert_eq!(
                pit.get_divisor(1),
                0x1000,
                "{name}: channel 1 defers under either board policy"
            );
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod mode3_square_wave_phase_tests {
    use super::*;

    const DIVISOR: u64 = 0x0020;

    fn at_pit_tick(pit: &mut Pit, pit_ticks: u64) {
        let ips = pit.ips.max(1);
        let hz = u64::from(pit.pit_hz.max(1));
        pit.total_cycles = pit.channels[0].base_cycles + (pit_ticks * ips + ips / 2) / hz;
    }

    /// **Every Mode 3 period starts HIGH and goes LOW only in its second half.**
    ///
    /// `track_time` toggled `ch.out`, described as a half-period hook — but
    /// the channel 0 scheduler calls it at FULL-divisor IRQ boundaries. Since
    /// `compute_out` reads `ch.out` to decide which half comes first, period N
    /// ended LOW and period N+1 began LOW: two adjacent low half-periods
    /// welded into one double-width pulse, which is not a square wave at all
    /// (MDD-BUG-TEM-00005).
    #[test]
    fn every_period_starts_high_and_falls_only_in_its_second_half() {
        let mut pit = Pit::new();
        pit.write_command(0x36); // channel 0, LSB/MSB, mode 3, binary
        pit.write_counter(0, (DIVISOR & 0xFF) as u8);
        pit.write_counter(0, (DIVISOR >> 8) as u8);

        // Walk three whole periods, crossing each boundary the way the
        // scheduler does.
        for period in 0..3u64 {
            at_pit_tick(&mut pit, 1);
            assert!(
                pit.get_out(0),
                "period {period}: the wave must start in its HIGH half"
            );

            at_pit_tick(&mut pit, DIVISOR / 2 + 1);
            assert!(
                !pit.get_out(0),
                "period {period}: and be LOW in its second half"
            );

            // The IRQ boundary the scheduler places at the full divisor.
            let boundary = pit.channels[0].base_cycles
                + (DIVISOR * pit.ips.max(1)) / u64::from(pit.pit_hz.max(1));
            pit.track_time(0, boundary);
        }
    }

    /// The two halves must be adjacent within one period, not merged.
    ///
    /// Sampling right before and right after the midpoint catches the welded
    /// pulse directly: with the phase inverted, the "first half" reads LOW.
    #[test]
    fn the_high_half_precedes_the_low_half_within_a_period() {
        let mut pit = Pit::new();
        pit.write_command(0x36);
        pit.write_counter(0, (DIVISOR & 0xFF) as u8);
        pit.write_counter(0, (DIVISOR >> 8) as u8);

        // Cross one boundary first, so this is NOT the pristine initial state.
        let boundary =
            pit.channels[0].base_cycles + (DIVISOR * pit.ips.max(1)) / u64::from(pit.pit_hz.max(1));
        pit.track_time(0, boundary);

        at_pit_tick(&mut pit, DIVISOR / 2 - 1);
        let before_mid = pit.get_out(0);
        at_pit_tick(&mut pit, DIVISOR / 2 + 1);
        let after_mid = pit.get_out(0);

        assert_eq!(
            (before_mid, after_mid),
            (true, false),
            "after a period boundary the wave must still read HIGH then LOW"
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod strobe_mode_tests {
    use super::*;

    /// Positions the channel at exactly `pit_ticks` oscillator ticks elapsed.
    fn at_pit_tick(pit: &mut Pit, ch: usize, pit_ticks: u64) {
        let ips = pit.ips.max(1);
        let hz = u64::from(pit.pit_hz.max(1));
        // Land mid-tick so integer division cannot fall a tick short.
        pit.total_cycles = pit.channels[ch].base_cycles + (pit_ticks * ips + ips / 2) / hz;
    }

    /// **Mode 4's strobe must actually happen.**
    ///
    /// Mode 4 holds OUT high, drops it for exactly ONE clock at terminal
    /// count, then returns high. `compute_out` computes that correctly — but
    /// an earlier short-circuit returned high for every `is_one_shot()` mode
    /// once `terminal_count_reached` was flagged, and that set is 0, 1, 4, 5.
    /// So the strobe was swallowed: OUT read high before and after, and the
    /// pulse never existed (MDD-BUG-TEM-00004).
    #[test]
    fn mode4_emits_exactly_one_low_clock_at_terminal_count() {
        let mut pit = Pit::new();
        pit.write_command(0x38); // channel 0, LSB/MSB, mode 4, binary
        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00); // divisor 0x0010

        // The event-driven path flags terminal count; this is the flag that
        // used to freeze OUT high for the rest of time.
        pit.set_channel0_terminal_count();

        at_pit_tick(&mut pit, 0, 8);
        assert!(pit.get_out(0), "before terminal count, OUT is HIGH");

        at_pit_tick(&mut pit, 0, 0x10);
        assert!(
            !pit.get_out(0),
            "at terminal count the strobe drives OUT LOW for one clock"
        );

        at_pit_tick(&mut pit, 0, 0x11);
        assert!(pit.get_out(0), "one clock later OUT is HIGH again");
    }

    /// Mode 5 is the gate-triggered strobe and behaves identically once fired.
    #[test]
    fn mode5_emits_exactly_one_low_clock_at_terminal_count() {
        let mut pit = Pit::new();
        pit.set_gate(0, false);
        pit.write_command(0x3A); // channel 0, LSB/MSB, mode 5, binary
        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00);
        pit.set_gate(0, true); // the rising edge fires the shot

        pit.set_channel0_terminal_count();

        at_pit_tick(&mut pit, 0, 0x10);
        assert!(!pit.get_out(0), "Mode 5 strobes LOW at terminal count");

        at_pit_tick(&mut pit, 0, 0x11);
        assert!(pit.get_out(0), "and returns HIGH one clock later");
    }

    /// **The counting element keeps running past terminal count.**
    ///
    /// The 8254's CE is a free-running down-counter; programs detect a
    /// completed one-shot by seeing the read WRAP upward. `compute_counter`
    /// models that, but the same short-circuit returned zero forever for
    /// Modes 4 and 5, so the wrap was invisible.
    #[test]
    fn a_strobe_channel_keeps_counting_past_terminal_count() {
        let mut pit = Pit::new();
        pit.write_command(0x38); // mode 4
        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00);
        pit.set_channel0_terminal_count();

        at_pit_tick(&mut pit, 0, 0x14); // four ticks PAST terminal count
        let counter = pit.compute_counter(0);
        assert_ne!(
            counter, 0,
            "the counting element wraps past zero rather than parking at it"
        );
    }

    /// Modes 0 and 1 DO park at terminal count — the contrast that shows the
    /// fix narrowed the short-circuit rather than deleting it.
    #[test]
    fn mode0_still_parks_at_terminal_count() {
        let mut pit = Pit::new();
        pit.write_command(0x30); // channel 0, LSB/MSB, mode 0
        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00);
        pit.set_channel0_terminal_count();

        at_pit_tick(&mut pit, 0, 0x14);
        assert!(
            pit.get_out(0),
            "Mode 0 latches OUT HIGH at terminal count and holds it"
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod half_written_pair_tests {
    use super::*;

    /// **A half-written LSB/MSB pair must not change what the channel counts on.**
    ///
    /// `write_counter` folds the LSB into `reload` immediately, so between the
    /// two bytes `reload` is a hybrid of the NEW low byte and the PREVIOUS
    /// high byte — a divisor the guest never asked for and the 8254 never
    /// counts on. Every divisor read took that field directly, so a counter
    /// read or an elapsed-time computation landing mid-pair saw the hybrid
    /// (MDD-BUG-TEM-00003).
    #[test]
    fn a_half_written_pair_keeps_the_previous_divisor() {
        let mut pit = Pit::new();
        // Mode 0, not 2: modes 2 and 3 DEFER a completed write to the next
        // cycle boundary, which is separate, deliberate behaviour. Using a
        // non-deferring mode isolates the staging question this test is about.
        pit.write_command(0x30); // channel 0, LSB/MSB, mode 0, binary
        pit.write_counter(0, 0x00);
        pit.write_counter(0, 0x10); // divisor 0x1000

        assert_eq!(pit.get_divisor(0), 0x1000);

        // First byte of a NEW pair: 0x34 low. `reload` now reads 0x1034 — a
        // number neither the old nor the new programming ever named.
        pit.write_counter(0, 0x34);
        assert_eq!(
            pit.channels[0].reload, 0x1034,
            "the staging hybrid is still present in the raw field"
        );
        assert_eq!(
            pit.get_divisor(0),
            0x1000,
            "the active divisor remains previous until the MSB arrives; \
             Mode 0 pauses counting during the incomplete pair"
        );

        // The MSB completes the transfer.
        pit.write_counter(0, 0x12);
        assert_eq!(pit.get_divisor(0), 0x1234);
    }

    /// The same guarantee for the OUT/counter computation, not just the
    /// divisor accessor — that is the path a guest actually observes.
    #[test]
    fn a_half_written_pair_does_not_disturb_the_counter_read() {
        let mut pit = Pit::new();
        pit.write_command(0x34); // channel 0, LSB/MSB, mode 2
        pit.write_counter(0, 0x00);
        pit.write_counter(0, 0x10); // divisor 0x1000

        let before_lsb = pit.read_counter(0);
        let before_msb = pit.read_counter(0);

        // Begin a new pair; nothing has completed, so the running counter must
        // be unchanged.
        pit.write_counter(0, 0xFF);

        assert_eq!(
            (pit.read_counter(0), pit.read_counter(0)),
            (before_lsb, before_msb),
            "an incomplete count write must not perturb the running counter"
        );
    }

    /// A control word abandons a half-written pair.
    ///
    /// `set_mode` resets `write_state`, so the channel returns to counting on
    /// `reload` rather than the abandoned staging.
    #[test]
    fn a_control_word_abandons_a_half_written_pair() {
        let mut pit = Pit::new();
        pit.write_command(0x34);
        pit.write_counter(0, 0x00);
        pit.write_counter(0, 0x10);

        pit.write_counter(0, 0x34); // half a pair
        assert_eq!(pit.channels[0].write_state, 1);

        pit.write_command(0x34); // control word: start over
        assert_eq!(
            pit.channels[0].write_state, 0,
            "a control word clears the half-written pair"
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod channel0_schedulability_tests {
    use super::*;

    /// **A reprogrammed channel 0 is not schedulable until it is loaded.**
    ///
    /// The event scheduler derived a period from the divisor alone, which is
    /// pure arithmetic and true of any channel — so a channel written with a
    /// mode but no count still armed a periodic IRQ0 (MDD-BUG-TEM-00002).
    #[test]
    fn channel0_is_not_schedulable_until_a_complete_count_is_written() {
        let mut pit = Pit::new();
        pit.write_command(0x34); // channel 0, LSB/MSB, mode 2, binary

        assert!(
            !pit.channel0_is_schedulable(),
            "a mode write alone leaves the channel unloaded, with no period to fire on"
        );

        pit.write_counter(0, 0x9C); // LSB only — the load is still incomplete
        assert!(
            !pit.channel0_is_schedulable(),
            "half a count is not a load: the 8254 latches on the second byte"
        );

        pit.write_counter(0, 0x2E); // MSB completes it
        assert!(
            pit.channel0_is_schedulable(),
            "a complete load makes the channel schedulable"
        );
    }

    /// **Modes 1 and 5 are not schedulable until their GATE trigger.**
    ///
    /// Both are one-shots that start on a GATE rising edge. Arming a periodic
    /// event for them raised IRQ0 from a shot that had never been fired.
    #[test]
    fn gate_triggered_modes_are_not_schedulable_until_triggered() {
        for command in [0x32_u8, 0x3A] {
            // channel 0, LSB/MSB, mode 1 then mode 5
            let mut pit = Pit::new();
            pit.set_gate(0, false);
            pit.write_command(command);
            pit.write_counter(0, 0x10);
            pit.write_counter(0, 0x00);

            assert!(
                !pit.channel0_is_schedulable(),
                "command {command:#04x}: loaded but never triggered — the one-shot has not started"
            );

            pit.set_gate(0, true);
            assert!(
                pit.channel0_is_schedulable(),
                "command {command:#04x}: the GATE rising edge starts it, so now it can fire"
            );
        }
    }

    /// Holding GATE low stops Modes 2 and 3 reaching terminal count.
    #[test]
    fn a_gated_off_rate_generator_is_not_schedulable() {
        let mut pit = Pit::new();
        pit.write_command(0x34); // channel 0, LSB/MSB, mode 2
        pit.write_counter(0, 0x9C);
        pit.write_counter(0, 0x2E);
        assert!(pit.channel0_is_schedulable());

        pit.set_gate(0, false);
        assert!(
            !pit.channel0_is_schedulable(),
            "GATE low halts counting, so no terminal count arrives to schedule for"
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod mode1_out_polarity_tests {
    use super::*;

    /// Advances simulated time by `pit_ticks` oscillator ticks.
    ///
    /// `compute_out` derives elapsed ticks as `cycles * pit_hz / ips`, so the
    /// guest-cycle count has to be scaled by the inverse of that ratio.
    fn advance_pit_ticks(pit: &mut Pit, pit_ticks: u64) {
        let ips = pit.ips.max(1);
        let hz = u64::from(pit.pit_hz.max(1));
        pit.total_cycles += pit_ticks * ips / hz + ips / hz + 1;
    }

    /// **Mode 1's OUT is HIGH until the GATE edge that starts the one-shot.**
    ///
    /// Mode 1 is the hardware retriggerable one-shot: after the mode is
    /// written OUT is high, it goes low on the GATE rising edge that triggers
    /// the shot, and returns high at terminal count. Both `PitChannel::new`
    /// and `set_mode` grouped Mode 1 with Mode 0 — where OUT genuinely does
    /// start low — so a guest reading OUT, or the status byte, before
    /// triggering saw an asserted one-shot that had never fired
    /// (MDD-BUG-TEM-00001).
    #[test]
    fn mode1_out_is_high_before_the_trigger_low_after_it_and_high_at_terminal_count() {
        let mut pit = Pit::new();

        // Gate low first, so programming the mode cannot itself read as a
        // rising edge and trigger the shot we are about to test.
        pit.set_gate(0, false);
        pit.write_command(0x32); // channel 0, LSB/MSB, mode 1, binary
        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00);

        assert!(
            pit.get_out(0),
            "Mode 1 OUT must be HIGH after the mode write, before any GATE edge"
        );

        pit.set_gate(0, true);
        assert!(
            !pit.get_out(0),
            "the GATE rising edge starts the shot and drives OUT LOW"
        );

        advance_pit_ticks(&mut pit, 0x20);
        assert!(
            pit.get_out(0),
            "OUT returns HIGH once the one-shot reaches terminal count"
        );
    }

    /// Mode 0 is the contrast, and must keep starting LOW.
    ///
    /// The two modes were grouped precisely because Mode 0 does start low, so
    /// this pins that the fix separates them rather than flipping both.
    #[test]
    fn mode0_out_still_starts_low() {
        let mut pit = Pit::new();
        pit.write_command(0x30); // channel 0, LSB/MSB, mode 0, binary
        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00);

        assert!(
            !pit.get_out(0),
            "Mode 0 OUT starts LOW and rises only at terminal count"
        );
    }

    /// A fresh channel defaults to Mode 1 only if someone asks for it, but the
    /// constructor must agree with `set_mode` either way.
    #[test]
    fn a_channel_constructed_in_mode1_also_starts_high() {
        let channel = PitChannel::new(1);
        assert!(
            channel.out,
            "PitChannel::new(1) must agree with set_mode(1): OUT starts HIGH"
        );

        let channel = PitChannel::new(0);
        assert!(!channel.out, "PitChannel::new(0) keeps Mode 0's LOW start");
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_pit_creation() {
        let pit = Pit::new();
        assert_eq!(pit.channels[0].reload, DEFAULT_DIVISOR);
        assert_eq!(pit.channels[0].mode, 3);
    }

    #[test]
    fn test_binary_to_bcd_clamps_values_above_9999() {
        assert_eq!(binary_to_bcd(10_000), 0x9999);
    }

    #[test]
    fn test_effective_divisor_bcd_non_zero_reload_path() {
        assert_eq!(effective_divisor(0x1234, true), 1234);
    }

    #[test]
    fn test_pit_channel_build_status_bits() {
        let mut ch = PitChannel::new(2);
        ch.out = true;
        ch.loaded = false;
        ch.access_mode = 3;
        ch.mode = 5;
        ch.bcd = true;

        let status = ch.build_status_with_out(ch.out);
        assert_eq!(status & 0x80, 0x80);
        assert_eq!(status & 0x40, 0x40);
        assert_eq!(status & 0x01, 0x01);

        ch.out = false;
        ch.loaded = true;
        ch.null_count = Some(false);
        ch.access_mode = 1;
        ch.mode = 2;
        ch.bcd = false;

        let status = ch.build_status_with_out(ch.out);
        assert_eq!(status & 0x80, 0);
        assert_eq!(status & 0x40, 0);
        assert_eq!(status & 0x01, 0);
    }

    #[test]
    fn test_pit_command_write() {
        let mut pit = Pit::new();
        // Channel 0, LSB/MSB access, Mode 3
        pit.write_command(0x36);
        assert_eq!(pit.channels[0].mode, 3);
        assert_eq!(pit.channels[0].access_mode, 3);
    }

    #[test]
    fn test_pit_counter_write() {
        let mut pit = Pit::new();
        // Mode command first (sets loaded=false → immediate load path)
        pit.write_command(0x36); // Channel 0, mode 3, LSB/MSB
                                 // Write LSB then MSB to channel 0
        pit.write_counter(0, 0x00); // LSB
        pit.write_counter(0, 0x10); // MSB = 0x1000
        assert_eq!(pit.channels[0].reload, 0x1000);
    }

    #[test]
    fn test_pit_latch() {
        let mut pit = Pit::new();
        // Set the reload value - compute_counter uses this to calculate current counter
        pit.channels[0].reload = 0x1234;
        pit.channels[0].base_cycles = pit.total_cycles;
        // Latch command for channel 0
        pit.write_command(0x00);
        // The latched value should be close to the reload value
        // (within 1 due to timing - counter counts down from reload)
        let latched = pit.channels[0].latch.unwrap();
        assert!(latched > 0, "Latched value {latched} should be > 0");
        assert!(
            latched <= 0x1234,
            "Latched value {latched} should be <= 0x1234"
        );
    }

    #[test]
    fn test_read_counter_returns_status_latch() {
        let mut pit = Pit::new();
        pit.channels[0].out = true;
        pit.channels[0].loaded = false;
        pit.channels[0].null_count = Some(true);
        pit.channels[0].bcd = true;
        pit.channels[0].status_latch = None;

        pit.write_command(0xC2);
        let status = pit.read_counter(0);
        assert_ne!(status & 0x80, 0);
        assert_ne!(status & 0x40, 0);
        assert_ne!(status & 0x01, 0);
    }

    #[test]
    fn test_readback_command_skips_existing_latches() {
        let mut pit = Pit::new();
        pit.channels[0].status_latch = Some(0xAA);
        pit.channels[0].latch = Some(0x1234);

        pit.write_command(0xC2);
        assert_eq!(pit.channels[0].status_latch, Some(0xAA));
        assert_eq!(pit.channels[0].latch, Some(0x1234));
    }

    #[test]
    fn test_pit_read_counter_latches_lsb_msb_pair() {
        let mut pit = Pit::new();
        pit.channels[0].reload = 0x0100;
        pit.channels[0].base_cycles = 0;
        pit.total_cycles = 0;

        let lsb = pit.read_counter(0);
        pit.total_cycles = 1;
        let msb = pit.read_counter(0);

        assert_eq!(lsb, 0x00);
        assert_eq!(msb, 0x01, "MSB should be latched from initial read");
    }

    #[test]
    fn test_pit_read_counter_prefers_existing_latch() {
        let mut pit = Pit::new();
        pit.channels[0].access_mode = 3;
        pit.channels[0].read_state = 0;
        pit.channels[0].latch = Some(0x1234);

        let lsb = pit.read_counter(0);
        assert_eq!(lsb, 0x34);
        assert_eq!(pit.channels[0].read_state, 1);
        assert_eq!(pit.channels[0].latch, Some(0x1234));
    }

    #[test]
    fn test_pit_gate_stops_counter() {
        let mut pit = Pit::new();
        pit.channels[0].reload = 0x0100;
        pit.channels[0].base_cycles = 0;
        pit.total_cycles = 0;

        pit.set_gate(0, false);
        pit.total_cycles = 1;

        let lsb = pit.read_counter(0);
        let msb = pit.read_counter(0);
        let value = u16::from_le_bytes([lsb, msb]);

        assert_eq!(value, 0x0100);
    }

    #[test]
    fn test_set_gate_invalid_channel_noop() {
        let mut pit = Pit::new();
        let counter_before = pit.channels[0].counter;
        pit.set_gate(3, false);
        assert_eq!(pit.channels[0].counter, counter_before);
    }

    #[test]
    fn test_diagnostic_snapshot_preserves_latches_and_deferred_reload() {
        let mut pit = Pit::new();
        pit.channels[0].latch = Some(0x1234);
        pit.channels[0].status_latch = Some(0xA5);
        pit.channels[0].deferred_reload = Some(0x2222);

        let snapshot = pit.diagnostic_snapshot();

        assert_eq!(snapshot.channels[0].latched_counter, Some(0x1234));
        assert_eq!(snapshot.channels[0].latched_status, Some(0xA5));
        assert_eq!(snapshot.channels[0].deferred_reload, Some(0x2222));
        assert_eq!(pit.channels[0].latch, Some(0x1234));
        assert_eq!(pit.channels[0].status_latch, Some(0xA5));
        assert_eq!(pit.channels[0].deferred_reload, Some(0x2222));
    }

    #[test]
    fn test_set_gate_snapshots_counter_when_stopping() {
        let mut pit = Pit::new();
        pit.channels[0].mode = 2;
        pit.channels[0].reload = 10;
        pit.channels[0].counter = 10;
        pit.channels[0].counting = true;
        pit.channels[0].gate = true;
        pit.channels[0].base_cycles = 0;
        pit.total_cycles = 3;

        pit.set_gate(0, false);

        assert!(!pit.channels[0].counting);
        assert_eq!(pit.channels[0].counter, 7);
    }

    #[test]
    fn test_set_gate_no_change_keeps_state() {
        let mut pit = Pit::new();
        pit.channels[0].mode = 2;
        pit.channels[0].reload = 10;
        pit.channels[0].counter = 10;
        pit.channels[0].counting = true;
        pit.channels[0].gate = true;
        pit.channels[0].base_cycles = 0;
        pit.total_cycles = 5;

        pit.set_gate(0, true);

        assert!(pit.channels[0].counting);
        assert_eq!(pit.channels[0].base_cycles, 0);
    }

    #[test]
    fn test_set_gate_rising_edge_resyncs_continuous() {
        let mut pit = Pit::new();
        pit.channels[0].mode = 2;
        pit.channels[0].reload = 10;
        pit.channels[0].counter = 10;
        pit.channels[0].counting = false;
        pit.channels[0].gate = false;
        pit.channels[0].base_cycles = 0;
        pit.total_cycles = 123;

        pit.set_gate(0, true);

        assert!(pit.channels[0].counting);
        assert_eq!(pit.channels[0].base_cycles, 123);
    }

    #[test]
    fn pit_terminal_readback_terminal_zero_then_wraps() {
        let mut pit = Pit::new();
        pit.channels[0].mode = 0;
        pit.channels[0].reload = 5;
        pit.channels[0].counting = true;
        pit.channels[0].terminal_count_reached = true;
        pit.channels[0].out = true;
        pit.total_cycles = pit.channels[0].base_cycles + 5;
        assert_eq!(pit.compute_counter(0), 0);
        pit.total_cycles += 4;
        assert_eq!(pit.compute_counter(0), 0xfffc);
        pit.set_gate(0, false);
        pit.total_cycles += 20;
        assert_eq!(
            pit.compute_counter(0),
            0xfffc,
            "GATE low freezes the wrapped count"
        );
        assert!(pit.get_out(0));
    }

    #[test]
    fn test_compute_counter_one_shot_elapsed_past_divisor_wraps() {
        // Real 8254: counter is a 16-bit down-counter that wraps past 0.
        // Divisor=5, elapsed=10 → 5 ticks past terminal count → counter wraps.
        let mut pit = Pit::new();
        pit.channels[0].mode = 0;
        pit.channels[0].reload = 5;
        pit.channels[0].base_cycles = 0;
        pit.total_cycles = 10;

        // 5 as u16 wrapping_sub (10 % 65536) as u16 = 5 - 10 = 0xFFFB = 65531
        assert_eq!(pit.compute_counter(0), 65531);
    }

    #[test]
    fn test_compute_counter_one_shot_elapsed_before_divisor() {
        let mut pit = Pit::new();
        pit.channels[0].mode = 0;
        pit.channels[0].reload = 5;
        pit.channels[0].base_cycles = 0;
        pit.total_cycles = 2;

        assert_eq!(pit.compute_counter(0), 3);
    }

    #[test]
    fn test_compute_counter_continuous_wraps_zero_divisor() {
        let mut pit = Pit::new();
        pit.channels[0].mode = 2;
        pit.channels[0].reload = 0;
        pit.channels[0].base_cycles = 0;
        pit.channels[0].counting = true;
        pit.channels[0].loaded = true;
        pit.total_cycles = 0;

        // Mode 2 with divisor 65536 at reload boundary:
        // remaining=65536 would truncate to 0 in u16, but Mode 2 must never
        // return 0 — returns 0xFFFF instead (one tick past reload).
        assert_eq!(pit.compute_counter(0), 0xFFFF);
    }

    #[test]
    fn test_interval_us_clamps_sub_microsecond() {
        let mut pit = Pit::new();
        // Divisor=1 at 1.193182 MHz gives ~0.84 us, which should be clamped to 1
        pit.channels[0].reload = 1;
        assert_eq!(pit.interval_us(), 1);

        // Divisor=1000 gives ~838 us
        pit.channels[0].reload = 1000;
        let interval = pit.interval_us();
        assert!(interval > 800 && interval < 900, "interval={interval}");
    }

    #[test]
    fn test_write_counter_invalid_channel_noop() {
        let mut pit = Pit::new();
        let reload_before = pit.channels[0].reload;
        pit.write_counter(3, 0x12);
        assert_eq!(pit.channels[0].reload, reload_before);
    }

    #[test]
    fn test_write_command_mode_7_aliases_to_mode_3() {
        let mut pit = Pit::new();
        pit.write_command(0x3E);
        assert_eq!(pit.channels[0].mode, 3);
    }

    // =========================================================================
    // Mode-specific tests
    // =========================================================================

    #[test]
    fn test_pit_channel_is_one_shot() {
        let mut ch = PitChannel::new(0);
        assert!(ch.is_one_shot()); // Mode 0

        ch.mode = 1;
        assert!(ch.is_one_shot()); // Mode 1

        ch.mode = 2;
        assert!(!ch.is_one_shot()); // Mode 2 is continuous

        ch.mode = 3;
        assert!(!ch.is_one_shot()); // Mode 3 is continuous

        ch.mode = 4;
        assert!(ch.is_one_shot()); // Mode 4

        ch.mode = 5;
        assert!(ch.is_one_shot()); // Mode 5
    }

    #[test]
    fn test_pit_channel_is_gate_triggered() {
        let mut ch = PitChannel::new(0);
        assert!(!ch.is_gate_triggered()); // Mode 0

        ch.mode = 1;
        assert!(ch.is_gate_triggered()); // Mode 1

        ch.mode = 5;
        assert!(ch.is_gate_triggered()); // Mode 5

        ch.mode = 2;
        assert!(!ch.is_gate_triggered()); // Mode 2
    }

    #[test]
    fn test_mode_0_one_shot() {
        let mut pit = Pit::new();

        // Set channel 0 to Mode 0 (interrupt on terminal count)
        pit.write_command(0x30); // Channel 0, LSB/MSB, Mode 0

        assert_eq!(pit.channels[0].mode, 0);
        assert!(!pit.channels[0].out); // OUT starts LOW in mode 0

        // Write a count value
        pit.write_counter(0, 0x00);
        pit.write_counter(0, 0x01); // Count = 0x0100

        assert_eq!(pit.channels[0].reload, 0x0100);
        assert!(!pit.channels[0].terminal_count_reached);
    }

    #[test]
    fn test_mode_0_out_state() {
        let mut pit = Pit::new();

        // Set channel 0 to Mode 0
        pit.write_command(0x30);
        assert!(!pit.get_out(0)); // OUT should be LOW after mode set

        // Write count - OUT goes LOW
        pit.write_counter(0, 0x00);
        pit.write_counter(0, 0x01);
        assert!(!pit.get_out(0)); // OUT stays LOW until TC
    }

    #[test]
    fn test_mode_2_continuous() {
        let mut pit = Pit::new();

        // Set channel 0 to Mode 2 (rate generator)
        pit.write_command(0x34); // Channel 0, LSB/MSB, Mode 2

        assert_eq!(pit.channels[0].mode, 2);
        assert!(pit.channels[0].out); // OUT starts HIGH in mode 2
        assert!(!pit.channels[0].is_one_shot());
    }

    #[test]
    fn test_mode_6_aliases_to_mode_2() {
        let mut pit = Pit::new();

        // Mode 6 should alias to Mode 2
        pit.write_command(0x3C); // Channel 0, LSB/MSB, Mode 6

        assert_eq!(pit.channels[0].mode, 2);
        assert!(pit.channels[0].out);
        assert!(!pit.channels[0].is_one_shot());
    }

    #[test]
    fn test_mode_3_square_wave() {
        let mut pit = Pit::new();

        // Set channel 0 to Mode 3 (square wave)
        pit.write_command(0x36); // Channel 0, LSB/MSB, Mode 3

        assert_eq!(pit.channels[0].mode, 3);
        assert!(pit.channels[0].out); // OUT starts HIGH
        assert!(!pit.channels[0].is_one_shot());
    }

    #[test]
    fn test_mode_4_software_strobe() {
        let mut pit = Pit::new();

        // Set channel 0 to Mode 4 (software triggered strobe)
        pit.write_command(0x38); // Channel 0, LSB/MSB, Mode 4

        assert_eq!(pit.channels[0].mode, 4);
        assert!(pit.channels[0].out); // OUT starts HIGH in mode 4
        assert!(pit.channels[0].is_one_shot());
    }

    #[test]
    fn test_mode_5_hardware_strobe() {
        let mut pit = Pit::new();

        // Set channel 0 to Mode 5 (hardware triggered strobe)
        pit.write_command(0x3A); // Channel 0, LSB/MSB, Mode 5

        assert_eq!(pit.channels[0].mode, 5);
        assert!(pit.channels[0].out); // OUT starts HIGH
        assert!(pit.channels[0].is_one_shot());
        assert!(pit.channels[0].is_gate_triggered());
    }

    #[test]
    fn test_gate_trigger_mode_1() {
        let mut pit = Pit::new();

        // Set channel 2 to Mode 1 (hardware retriggerable one-shot)
        pit.write_command(0xB2); // Channel 2, LSB/MSB, Mode 1

        assert_eq!(pit.channels[2].mode, 1);
        assert!(pit.channels[2].out); // Mode 1 OUT starts HIGH (TEM-00001)

        // Write a count
        pit.write_counter(2, 0x00);
        pit.write_counter(2, 0x10);

        // Counting shouldn't start until gate trigger
        assert!(!pit.channels[2].counting);

        // Trigger gate (rising edge)
        pit.set_gate(2, false);
        pit.set_gate(2, true);

        // Now counting should be enabled
        assert!(pit.channels[2].counting);
        assert!(!pit.channels[2].out); // OUT goes LOW on trigger
    }

    #[test]
    fn test_gate_trigger_mode_5() {
        let mut pit = Pit::new();

        // Set channel 2 to Mode 5 (hardware triggered strobe)
        pit.write_command(0xBA); // Channel 2, LSB/MSB, Mode 5

        assert_eq!(pit.channels[2].mode, 5);
        assert!(pit.channels[2].out); // OUT starts HIGH

        // Write a count
        pit.write_counter(2, 0x00);
        pit.write_counter(2, 0x10);

        // Counting shouldn't start until gate trigger
        assert!(!pit.channels[2].counting);

        // Trigger gate
        pit.set_gate(2, false);
        pit.set_gate(2, true);

        // Now counting should be enabled, OUT stays HIGH until TC
        assert!(pit.channels[2].counting);
        assert!(pit.channels[2].out);
    }

    #[test]
    fn test_one_shot_fires_once() {
        let mut pit = Pit::new();

        // Set channel 0 to Mode 0 (interrupt on terminal count)
        pit.write_command(0x30);
        pit.write_counter(0, 0x10); // Count = 16
        pit.write_counter(0, 0x00);

        // Advance past the count; in the event-driven model, the emulator
        // would have dispatched the terminal count by now.
        pit.total_cycles = 20;

        // After 20 ticks with divisor 16: 4 ticks past terminal count.
        // Real 8254: counter wraps past 0 → 16 - (20 % 65536) mod 65536.
        let counter = pit.compute_counter(0);
        // 16u16.wrapping_sub(20u16) = 0xFFFC = 65532
        assert_eq!(counter, 65532, "Counter should wrap past terminal count");
    }

    #[test]
    fn test_one_shot_reprogram_fires_again() {
        let mut pit = Pit::new();

        // Set channel 0 to Mode 0
        pit.write_command(0x30);
        pit.write_counter(0, 0x01);
        pit.write_counter(0, 0x00);

        // Mark as already fired
        pit.channels[0].terminal_count_reached = true;

        // Reprogram should reset terminal_count_reached
        pit.write_counter(0, 0x01);
        pit.write_counter(0, 0x00);

        assert!(!pit.channels[0].terminal_count_reached);
    }

    #[test]
    fn test_get_mode() {
        let mut pit = Pit::new();

        pit.write_command(0x34); // Mode 2
        assert_eq!(pit.get_mode(0), 2);

        pit.write_command(0x36); // Mode 3
        assert_eq!(pit.get_mode(0), 3);

        pit.write_command(0x30); // Mode 0
        assert_eq!(pit.get_mode(0), 0);
    }

    #[test]
    fn test_get_divisor() {
        let mut pit = Pit::new();
        // Mode command first (sets loaded=false → immediate load path)
        pit.write_command(0x36); // Channel 0, mode 3, LSB/MSB
        pit.write_counter(0, 0x34);
        pit.write_counter(0, 0x12);

        assert_eq!(pit.get_divisor(0), 0x1234);
    }

    #[test]
    fn test_invalid_channel_accessors() {
        let mut pit = Pit::new();
        assert_eq!(pit.read_counter(3), 0xFF);
        assert!(!pit.get_out(3));
        assert_eq!(pit.get_divisor(3), 0);
        assert_eq!(pit.get_mode(3), 0);
    }

    #[test]
    fn test_access_mode_lsb_only() {
        let mut pit = Pit::new();
        // Channel 0, LSB only, Mode 2
        pit.write_command(0x14);
        pit.channels[0].reload = 0x5500;
        pit.write_counter(0, 0xAA);
        assert_eq!(pit.channels[0].access_mode, 1);
        assert_eq!(pit.channels[0].reload, 0x00AA);
    }

    #[test]
    fn test_access_mode_msb_only() {
        let mut pit = Pit::new();
        pit.channels[0].reload = 0x00FF;
        // Channel 0, MSB only, Mode 2
        pit.write_command(0x24);
        pit.write_counter(0, 0xBB);
        assert_eq!(pit.channels[0].access_mode, 2);
        assert_eq!(pit.channels[0].reload, 0xBB00);
    }

    #[test]
    fn test_read_counter_lsb_only() {
        let mut pit = Pit::new();
        pit.channels[0].reload = 0x1234;
        pit.channels[0].access_mode = 1; // LSB only

        let value = pit.read_counter(0);
        assert_eq!(value, 0x34);
    }

    #[test]
    fn test_read_counter_msb_only() {
        let mut pit = Pit::new();
        pit.channels[0].reload = 0x1234;
        pit.channels[0].access_mode = 2; // MSB only

        let value = pit.read_counter(0);
        assert_eq!(value, 0x12);
    }

    #[test]
    fn test_read_counter_invalid_access_mode_returns_ff() {
        let mut pit = Pit::new();
        pit.channels[0].access_mode = 9;

        assert_eq!(pit.read_counter(0), 0xFF);
    }

    #[test]
    fn test_write_invalid_port_is_noop() {
        let mut pit = Pit::new();
        let reload_before = pit.channels[0].reload;
        pit.write(0x44, 0xFF);
        assert_eq!(pit.channels[0].reload, reload_before);
    }

    #[test]
    fn test_readback_command_respects_latch_flags() {
        let mut pit = Pit::new();
        pit.channels[0].status_latch = None;
        pit.channels[0].latch = None;

        // Disable status latch (bit 4 set), keep count latch enabled.
        pit.write_command(0xD2);
        assert!(pit.channels[0].status_latch.is_none());

        // Disable count latch (bit 5 set), keep status latch enabled.
        pit.channels[0].latch = None;
        pit.channels[0].status_latch = None;
        pit.write_command(0xE2);
        assert!(pit.channels[0].latch.is_none());
        assert!(pit.channels[0].status_latch.is_some());
    }

    #[test]
    fn test_resync_counter_start_one_shot_and_continuous() {
        let mut pit = Pit::new();
        pit.total_cycles = 1000;

        pit.channels[0].mode = 0;
        pit.channels[0].reload = 0;
        pit.channels[0].counter = 10;
        pit.resync_counter_start(0);
        assert!(pit.channels[0].base_cycles <= pit.total_cycles);

        pit.channels[0].mode = 2;
        pit.channels[0].reload = 0;
        pit.channels[0].counter = 0;
        pit.resync_counter_start(0);
        assert_eq!(pit.channels[0].base_cycles, pit.total_cycles);

        pit.channels[0].mode = 2;
        pit.channels[0].reload = 10;
        pit.channels[0].counter = 5;
        pit.resync_counter_start(0);
        assert!(pit.channels[0].base_cycles <= pit.total_cycles);
    }

    #[test]
    fn test_pit_write_dispatch() {
        let mut pit = Pit::new();
        // Test 0x40 (Channel 0)
        pit.write(0x40, 0x12);
        assert_eq!(pit.channels[0].reload & 0xFF, 0x12);

        // Test 0x41 (Channel 1)
        pit.write(0x41, 0x34);
        assert_eq!(pit.channels[1].reload & 0xFF, 0x34);

        // Test 0x42 (Channel 2)
        pit.write(0x42, 0x56);
        assert_eq!(pit.channels[2].reload & 0xFF, 0x56);

        // Test 0x43 (Command) - set mode 0 for channel 0
        pit.write(0x43, 0x30);
        assert_eq!(pit.channels[0].mode, 0);
    }

    #[test]
    fn test_set_pit_hz() {
        let mut pit = Pit::new();
        pit.channels[0].reload = 1000;

        // Default frequency is 1.193182 MHz
        // Interval = (1000 * 1_000_000) / 1193182 ~= 838 us

        pit.set_pit_hz(2_000_000);
        assert_eq!(pit.pit_hz(), 2_000_000);

        // New interval = (1000 * 1_000_000) / 2_000_000 = 500 us
        assert_eq!(pit.interval_us(), 500);
    }

    #[test]
    fn test_pit_default() {
        let pit = Pit::default();
        // Should behave like new()
        assert_eq!(pit.pit_hz(), PIT_FREQUENCY);
    }

    // =========================================================================
    // PITCONST / emulated speed tests
    // =========================================================================

    #[test]
    fn test_ips_default_is_pit_frequency() {
        let pit = Pit::new();
        assert_eq!(pit.emulated_ips(), PIT_FREQUENCY as u64);
        assert_eq!(pit.ips, PIT_FREQUENCY as u64);
    }

    #[test]
    fn test_set_emulated_speed_updates_ips() {
        let mut pit = Pit::new();
        pit.set_emulated_speed(24_999_960);
        assert_eq!(pit.ips, 24_999_960);
        assert_eq!(pit.emulated_ips(), 24_999_960);
    }

    #[test]
    fn test_compute_counter_with_scaling() {
        let mut pit = Pit::new();
        pit.set_emulated_speed(24_999_960); // Normal speed

        // Program channel 0 with divisor 1000
        pit.write_command(0x34);
        pit.write_counter(0, 0xE8); // 1000 = 0x03E8
        pit.write_counter(0, 0x03);

        // After some instructions, counter should reflect scaled time
        pit.total_cycles = pit.channels[0].base_cycles + 100;
        // 100 instructions * pit_hz / ips = 100 * 1193182 / 24999960 ≈ 4.77 PIT ticks
        // Counter should be 1000 - ~5 = ~995
        let counter = pit.compute_counter(0);
        assert!(
            counter >= 990 && counter <= 1000,
            "Counter {counter} should be close to 1000 after 100 instructions"
        );
    }

    #[test]
    fn test_set_emulated_speed_snapshots_channels() {
        let mut pit = Pit::new();
        // Program and advance channel 0
        pit.write_command(0x34);
        pit.write_counter(0, 100);
        pit.write_counter(0, 0);
        pit.total_cycles += 2_000_050;

        let before = pit.compute_counter(0);
        pit.set_emulated_speed(24_999_960);

        // This used to assert base_cycles == total_cycles, which is precisely
        // the phase loss of MDD-BUG-CRUCIBLE-00558. base_cycles is now
        // back-dated so the same phase survives the new rate; what the guest
        // sees is the invariant worth pinning.
        assert!(pit.channels[0].base_cycles < pit.total_cycles);
        let after = pit.compute_counter(0);
        assert!(
            (i64::from(before) - i64::from(after)).abs() <= 2,
            "count moved {before} -> {after}"
        );
    }

    #[test]
    fn speed_change_preserves_the_visible_count() {
        // A runtime speed change must not disturb a counting channel's phase.
        // Before the fix elapsed went to zero and the count snapped back to the
        // reload value (MDD-BUG-CRUCIBLE-00558).
        for &(mode_cmd, label) in &[(0x34u8, "mode 2"), (0x36u8, "mode 3")] {
            for &divisor in &[100u16, 1000, 11_932] {
                for &new_ips in &[5_000_000u64, 50_000_000, 12_600_000] {
                    let mut pit = Pit::new();
                    pit.write_command(mode_cmd);
                    pit.write_counter(0, (divisor & 0xFF) as u8);
                    pit.write_counter(0, (divisor >> 8) as u8);
                    // Advance well into the period, not onto a boundary, and
                    // far enough that the clock can express the phase under the
                    // faster rate too.
                    pit.total_cycles += 3_000_007;

                    let before = pit.compute_counter(0);
                    assert_ne!(
                        before, divisor,
                        "{label} divisor {divisor}: the probe must sit mid-period"
                    );

                    pit.set_emulated_speed(new_ips);
                    let after = pit.compute_counter(0);

                    // One tick of rounding is inherent in re-basing across two
                    // rates; a reload-value snap-back is orders of magnitude
                    // bigger than that.
                    let slack = if mode_cmd == 0x36 { 4 } else { 2 };
                    let delta = (i64::from(before) - i64::from(after)).abs();
                    assert!(
                        delta <= slack,
                        "{label} divisor {divisor} -> {new_ips} ips: count moved \
                         {before} -> {after} (delta {delta}, slack {slack})"
                    );
                }
            }
        }
    }

    #[test]
    fn speed_change_leaves_a_stopped_channel_alone() {
        let mut pit = Pit::new();
        pit.write_command(0x34);
        pit.write_counter(0, 0x00);
        pit.write_counter(0, 0x10); // divisor 0x1000
        pit.set_gate(0, false); // stop counting
        let before = pit.compute_counter(0);

        pit.total_cycles += 5_000;
        pit.set_emulated_speed(50_000_000);

        assert_eq!(
            pit.compute_counter(0),
            before,
            "a gated channel must freeze"
        );
    }

    // =========================================================================
    // Event-driven scheduling prerequisite tests
    // =========================================================================

    #[test]
    fn test_channel0_mode_classification() {
        let mut pit = Pit::new();

        // Mode 0: one-shot
        pit.write_command(0x30); // Channel 0, LSB/MSB, Mode 0
        assert_eq!(pit.channel0_mode(), PitMode::OneShot);

        // Mode 1: one-shot (gate-triggered)
        pit.write_command(0x32); // Channel 0, LSB/MSB, Mode 1
        assert_eq!(pit.channel0_mode(), PitMode::OneShot);

        // Mode 2: rate generator
        pit.write_command(0x34); // Channel 0, LSB/MSB, Mode 2
        assert_eq!(pit.channel0_mode(), PitMode::RateGenerator);

        // Mode 3: square wave
        pit.write_command(0x36); // Channel 0, LSB/MSB, Mode 3
        assert_eq!(pit.channel0_mode(), PitMode::SquareWave);

        // Mode 4: one-shot (software strobe)
        pit.write_command(0x38); // Channel 0, LSB/MSB, Mode 4
        assert_eq!(pit.channel0_mode(), PitMode::OneShot);

        // Mode 5: one-shot (hardware strobe)
        pit.write_command(0x3A); // Channel 0, LSB/MSB, Mode 5
        assert_eq!(pit.channel0_mode(), PitMode::OneShot);
    }

    #[test]
    fn test_channel0_period_cycles_default_divisor() {
        let mut pit = Pit::new();
        // Default: reload=0 (divisor 65536), mode 3
        pit.channels[0].reload = 0;
        pit.channels[0].bcd = false;

        let cycles_per_ms = 1200u64;
        let period = pit.channel0_period_cycles(cycles_per_ms);
        // Expected: (65536 * 1200 * 1000) / 1193182 ≈ 65,910
        // u128: (65536 * 1200 * 1000) = 78,643,200,000
        //        78,643,200,000 / 1,193,182 ≈ 65,910
        assert!(
            period >= 65_900 && period <= 65_920,
            "Expected ~65910, got {period}"
        );
    }

    #[test]
    fn test_channel0_period_cycles_fast_rate() {
        let mut pit = Pit::new();
        pit.channels[0].reload = 1000;
        pit.channels[0].bcd = false;

        let cycles_per_ms = 1200u64;
        let period = pit.channel0_period_cycles(cycles_per_ms);
        // Expected: (1000 * 1200 * 1000) / 1193182 ≈ 1005
        // More precisely: 1,200,000,000 / 1,193,182 ≈ 1005.7
        assert!(
            period >= 1005 && period <= 1006,
            "Expected ~1005, got {period}"
        );
    }

    #[test]
    fn test_channel0_period_cycles_u128_precision() {
        let mut pit = Pit::new();
        // Large cycles_per_ms (e.g., 100MHz = 100_000 cycles/ms)
        // with max divisor (0 = 65536)
        pit.channels[0].reload = 0;
        pit.channels[0].bcd = false;

        let cycles_per_ms = 100_000u64;
        let period = pit.channel0_period_cycles(cycles_per_ms);
        // (65536 * 100000 * 1000) / 1193182 = 6,553,600,000,000 / 1,193,182 ≈ 5,492,520
        // This would overflow u32 intermediate math, proving u128 is necessary
        assert!(
            period >= 5_492_000 && period <= 5_493_000,
            "Expected ~5,492,520, got {period}"
        );
        // Verify the value is sensible (wouldn't fit in u32 intermediate product)
        let intermediate_product = 65536u64 * 100_000u64 * 1000;
        assert!(
            intermediate_product > u32::MAX as u64,
            "This test specifically validates u128 prevents truncation"
        );
    }

    #[test]
    fn test_track_time_advances_base_cycles() {
        let mut pit = Pit::new();
        pit.channels[0].base_cycles = 0;
        pit.channels[0].mode = 2; // Rate generator (not mode 3)

        pit.track_time(0, 12345);

        assert_eq!(pit.channels[0].base_cycles, 12345);
        // Mode 2: out should NOT toggle
        assert!(pit.channels[0].out, "Mode 2 should not toggle out pin");
    }

    #[test]
    fn test_track_time_mode3_restarts_the_period_high() {
        // Renamed and corrected. This pinned the toggle, which is the defect:
        // `track_time` is called at FULL-period IRQ boundaries, where a square
        // wave restarts in its HIGH half. Toggling inverted the phase every
        // period (MDD-BUG-TEM-00005).
        let mut pit = Pit::new();
        pit.channels[0].mode = 3; // Square wave
        pit.channels[0].out = true; // Start HIGH

        pit.track_time(0, 1000);
        assert!(
            pit.channels[0].out,
            "a period boundary restarts the wave in its HIGH half"
        );
        assert_eq!(pit.channels[0].base_cycles, 1000);

        pit.track_time(0, 2000);
        assert!(
            pit.channels[0].out,
            "and every later boundary does the same, rather than alternating"
        );
        assert_eq!(pit.channels[0].base_cycles, 2000);
    }

    #[test]
    fn test_event_reschedule_pending_on_counter_load() {
        let mut pit = Pit::new();
        assert!(!pit.event_reschedule_pending, "Should start false");

        // Program channel 0 with a new counter value
        pit.write_command(0x34); // Channel 0, LSB/MSB, Mode 2
                                 // write_command for channel 0 sets the flag (mode change)
        assert!(
            pit.event_reschedule_pending,
            "Mode change on channel 0 should set flag"
        );
        pit.event_reschedule_pending = false; // Clear for next check

        pit.write_counter(0, 0x00); // LSB — partial write, no flag yet
        pit.write_counter(0, 0x10); // MSB — completes counter load

        assert!(
            pit.event_reschedule_pending,
            "Channel 0 counter load should set event_reschedule_pending"
        );

        // Verify channel 2 does NOT set the flag
        pit.event_reschedule_pending = false;
        pit.write_command(0xB4); // Channel 2, LSB/MSB, Mode 2
        pit.write_counter(2, 0x00);
        pit.write_counter(2, 0x10);
        assert!(
            !pit.event_reschedule_pending,
            "Channel 2 counter load should NOT set event_reschedule_pending"
        );
    }

    #[test]
    fn test_event_reschedule_pending_cleared_manually() {
        let mut pit = Pit::new();

        // Trigger the flag
        pit.write_command(0x34); // Channel 0 mode change
        assert!(pit.event_reschedule_pending);

        // Caller clears it after consuming
        pit.event_reschedule_pending = false;
        assert!(!pit.event_reschedule_pending);
    }

    // ── Checkpoint save/restore round-trip tests ─────────────────────────

    #[test]
    fn test_controller_checkpoint_recomputes_ratio() {
        // MDD-BUG-CRUCIBLE-00032: the time-machine controller checkpoint must
        // recompute the derived pit_ticks_per_instr_fp on restore (it is not part of
        // the checkpoint tuple).
        let mut pit = Pit::new();
        // A non-default speed makes the derived ratio distinct from the default 1:1.
        let ips = PIT_FREQUENCY as u64 * 3;
        pit.ips = ips;
        pit.pit_ticks_per_instr_fp = compute_fixed_point_ratio(pit.pit_hz, ips);

        let expected_ratio = pit.pit_ticks_per_instr_fp;
        assert_ne!(
            expected_ratio,
            Pit::new().pit_ticks_per_instr_fp,
            "precondition: non-default ratio"
        );

        let (pit_hz, total_cycles, ips_saved) = pit.save_controller_state();

        // Restore into a FRESH pit (default ratio).
        let mut restored = Pit::new();
        restored.restore_controller_state(pit_hz, total_cycles, ips_saved);

        assert_eq!(
            restored.pit_ticks_per_instr_fp, expected_ratio,
            "pit_ticks_per_instr_fp must be recomputed from the restored pit_hz/ips"
        );
        assert_eq!(restored.ips, ips);
    }

    #[test]
    fn test_save_restore_channel_round_trip() {
        let mut pit = Pit::new();
        // Configure channel 2 with specific state
        pit.write_command(0xB6); // Channel 2, mode 3, LSB/MSB
        pit.write_counter(2, 0x34); // LSB
        pit.write_counter(2, 0x12); // MSB → reload = 0x1234
        pit.total_cycles = 100;

        // Save all channels
        let saved: Vec<_> = (0..3).map(|ch| pit.save_channel_state(ch)).collect();
        let (pit_hz, total_cycles, ips) = pit.save_controller_state();

        // Create a fresh PIT and restore
        let mut pit2 = Pit::new();
        pit2.restore_controller_state(pit_hz, total_cycles, ips);
        for ch in 0..3 {
            pit2.restore_channel_state(ch, &saved[ch]);
        }

        // Verify controller state
        assert_eq!(pit2.total_cycles, total_cycles);
        assert_eq!(pit2.pit_hz(), pit_hz);

        // Verify channel 2 state
        let s2 = pit2.save_channel_state(2);
        assert_eq!(s2.reload, 0x1234);
        assert_eq!(s2.mode, 3);
        assert_eq!(s2.access_mode, 3); // LSB/MSB
        assert!(s2.loaded);
    }

    #[test]
    fn test_save_restore_preserves_channel0_mode0_terminal_count() {
        let mut pit = Pit::new();
        // Mode 0 (interrupt on terminal count) for channel 0
        pit.write_command(0x30); // Channel 0, mode 0, LSB/MSB
        pit.write_counter(0, 10);
        pit.write_counter(0, 0); // reload = 10

        // Run until terminal count fires
        pit.total_cycles += 50;
        // assert!(ticks removed);

        // Save and restore
        let saved = pit.save_channel_state(0);
        let ctrl = pit.save_controller_state();

        let mut pit2 = Pit::new();
        pit2.restore_controller_state(ctrl.0, ctrl.1, ctrl.2);
        pit2.restore_channel_state(0, &saved);

        // Mode 0 terminal count should NOT fire again after time advance
        let out_before = pit2.get_out(0);
        pit2.total_cycles += 100;
        assert_eq!(
            pit2.get_out(0),
            out_before,
            "Mode 0 output should not change after time advance on restored PIT"
        );
    }

    // ── Deferred counter update tests ────────────────

    #[test]
    fn test_immediate_reload_with_new_mode() {
        // Writing port 0x43 then port 0x40 should load immediately (no deferral).
        let mut pit = Pit::new();

        // Initial program: mode 2, counter = 1000
        pit.write_command(0x34);
        pit.write_counter(0, 0xE8);
        pit.write_counter(0, 0x03);

        pit.total_cycles += 500; // Run to mid-cycle

        // Now reprogram with a NEW mode command (port 0x43) then counter
        pit.event_reschedule_pending = false; // Clear from initial load
        pit.write_command(0x34); // Mode 2 again, but via port 0x43
        pit.write_counter(0, 0xF4); // LSB of 500
        pit.write_counter(0, 0x01); // MSB of 500

        // Should be immediate — no deferral
        assert_eq!(
            pit.get_deferred_reload(0),
            None,
            "Mode command should cause immediate load, not deferred"
        );
        assert_eq!(
            0, /* tick_counter removed */ 0,
            "tick_counter should be reset"
        );
        assert!(
            pit.event_reschedule_pending,
            "Immediate load should trigger event reschedule"
        );
    }

    #[test]
    fn test_mode0_one_shot_not_deferred() {
        // Mode 0 (one-shot) should ALWAYS load immediately, even without port 0x43.
        let mut pit = Pit::new();

        // Mode 0, counter = 100
        pit.write_command(0x30); // Channel 0, mode 0, LSB/MSB
        pit.write_counter(0, 100);
        pit.write_counter(0, 0);
        pit.event_reschedule_pending = false;

        // Run partway — don't fire yet
        pit.total_cycles += 50;

        // Write new counter without port 0x43
        pit.write_counter(0, 200);
        pit.write_counter(0, 0);

        // Mode 0 should load immediately, not defer
        assert_eq!(
            pit.get_deferred_reload(0),
            None,
            "Mode 0 must not defer counter loads"
        );
        assert_eq!(
            0, /* tick_counter removed */ 0,
            "tick_counter should be reset"
        );
        assert!(
            pit.event_reschedule_pending,
            "Immediate load should set reschedule flag"
        );
    }

    #[test]
    fn test_deferred_reload_save_restore_round_trip() {
        // deferred_reload must survive checkpoint save/restore.
        let mut pit = Pit::new();

        // Mode 2, counter = 1000, then deferred write to 500
        pit.write_command(0x34);
        pit.write_counter(0, 0xE8);
        pit.write_counter(0, 0x03);
        pit.total_cycles += 500;

        // Deferred write
        pit.write_counter(0, 0xF4);
        pit.write_counter(0, 0x01);
        assert_eq!(pit.get_deferred_reload(0), Some(500));

        // Save and restore
        let saved = pit.save_channel_state(0);
        let ctrl = pit.save_controller_state();

        let mut pit2 = Pit::new();
        pit2.restore_controller_state(ctrl.0, ctrl.1, ctrl.2);
        pit2.restore_channel_state(0, &saved);

        assert_eq!(
            pit2.get_deferred_reload(0),
            Some(500),
            "deferred_reload must survive save/restore"
        );
    }

    #[test]
    fn test_apply_deferred_reload_event_driven_path() {
        // Test the apply_deferred_reload() method used by the event-driven path.
        let mut pit = Pit::new();

        // Mode 2, counter = 1000
        pit.write_command(0x34);
        pit.write_counter(0, 0xE8);
        pit.write_counter(0, 0x03);

        pit.total_cycles += 500;

        // Deferred write: new counter = 750
        pit.write_counter(0, 0xEE); // LSB of 750
        pit.write_counter(0, 0x02); // MSB of 750
        assert_eq!(pit.get_deferred_reload(0), Some(750));

        // Simulate event-driven path: apply_deferred_reload at cycle boundary
        pit.apply_deferred_reload(0);

        assert_eq!(pit.get_deferred_reload(0), None, "Should be consumed");
        assert_eq!(pit.get_divisor(0), 750, "New reload value applied");
    }

    #[test]
    fn test_mode_change_clears_stale_deferred_reload() {
        // A mode command (port 0x43) must cancel any pending deferred reload.
        // Otherwise the stale deferred value would override the new counter.
        let mut pit = Pit::new();

        // Mode 2, counter = 1000
        pit.write_command(0x34);
        pit.write_counter(0, 0xE8);
        pit.write_counter(0, 0x03);
        pit.total_cycles += 500;

        // Deferred write: new counter = 500 (no port 0x43)
        pit.write_counter(0, 0xF4);
        pit.write_counter(0, 0x01);
        assert_eq!(pit.get_deferred_reload(0), Some(500));

        // Now a full reprogram via port 0x43 — should cancel the deferred value
        pit.write_command(0x34); // Mode 2 again via port 0x43
        assert_eq!(
            pit.get_deferred_reload(0),
            None,
            "Mode change must cancel stale deferred reload"
        );

        // Write new counter = 2000
        pit.write_counter(0, 0xD0);
        pit.write_counter(0, 0x07);
        assert_eq!(
            pit.get_divisor(0),
            2000,
            "New counter should load immediately"
        );
        assert_eq!(
            pit.get_deferred_reload(0),
            None,
            "No deferral after mode command"
        );
    }

    #[test]
    fn test_channel2_counter_writes_apply_immediately() {
        // Channel 2 (PC speaker) must never defer counter writes because no
        // event scheduler exists to consume deferred reloads. Without this,
        // the speaker stays stuck at the first frequency ever programmed.
        let mut pit = Pit::new();

        // Program channel 2: mode 3 (square wave), LSB/MSB, binary (0xB6)
        pit.write_command(0xB6);
        pit.write_counter(2, 0xE8); // LSB of 1000
        pit.write_counter(2, 0x03); // MSB of 1000
        assert_eq!(pit.get_divisor(2), 1000);
        assert!(pit.speaker_reload_updated);
        pit.speaker_reload_updated = false;

        pit.total_cycles += 500;

        // Counter-only write (no mode command) — channel 0 would defer this,
        // but channel 2 must apply immediately.
        pit.write_counter(2, 0xF4); // LSB of 500
        pit.write_counter(2, 0x01); // MSB of 500
        assert_eq!(
            pit.get_divisor(2),
            500,
            "Channel 2 counter writes must apply immediately, not defer"
        );
        assert_eq!(
            pit.get_deferred_reload(2),
            None,
            "Channel 2 must not have deferred reloads"
        );
        assert!(pit.speaker_reload_updated);
        pit.speaker_reload_updated = false;

        // A third frequency change should also apply immediately
        pit.total_cycles += 500;
        pit.write_counter(2, 0x2C); // LSB of 300
        pit.write_counter(2, 0x01); // MSB of 300
        assert_eq!(pit.get_divisor(2), 300);
        assert!(pit.speaker_reload_updated);
    }

    // ── Gate rising edge reload tests (Mode 2/3) ────────────────────────

    #[test]
    fn test_mode2_gate_rising_edge_reloads_counter() {
        // Per 8254 spec: Mode 2 gate LOW->HIGH reloads counter from reload register.
        let mut pit = Pit::new();

        // Program channel 2: mode 2, counter = 1000
        pit.write_command(0xB4); // Channel 2, LSB/MSB, Mode 2
        pit.write_counter(2, 0xE8); // LSB of 1000
        pit.write_counter(2, 0x03); // MSB of 1000

        // Let some time pass so counter has decremented
        pit.total_cycles = 500;

        // Gate LOW — stops counting, snapshots counter
        pit.set_gate(2, false);
        assert!(!pit.channels[2].counting);
        let frozen = pit.channels[2].counter;
        assert!(frozen < 1000, "Counter should have decremented: {frozen}");

        // Gate HIGH — should reload counter to full reload value
        pit.set_gate(2, true);
        assert!(pit.channels[2].counting);
        assert_eq!(
            pit.channels[2].counter, 1000,
            "Rising edge must reload counter"
        );
        assert_eq!(
            pit.channels[2].base_cycles, pit.total_cycles,
            "base_cycles must reset to current time on rising edge"
        );
    }

    #[test]
    fn test_mode3_gate_rising_edge_reloads_counter() {
        // Mode 3 (square wave) also reloads on gate rising edge.
        let mut pit = Pit::new();

        pit.write_command(0xB6); // Channel 2, LSB/MSB, Mode 3
        pit.write_counter(2, 0xE8);
        pit.write_counter(2, 0x03); // reload = 1000

        pit.total_cycles = 300;

        pit.set_gate(2, false);
        assert!(!pit.channels[2].counting);

        pit.set_gate(2, true);
        assert!(pit.channels[2].counting);
        assert_eq!(pit.channels[2].counter, 1000);
        assert_eq!(pit.channels[2].base_cycles, pit.total_cycles);
    }

    #[test]
    fn test_mode2_and_mode3_gate_low_forces_out_high_and_restarts_high() {
        for command in [0x34_u8, 0x36] {
            let mut pit = Pit::new();
            pit.write_command(command); // channel 0, LSB/MSB, Mode 2 or 3
            pit.write_counter(0, 10);
            pit.write_counter(0, 0);

            // Simulate the stored OUT state at the low phase that a gate
            // transition can interrupt. GATE low must force the external pin
            // high immediately, regardless of that prior phase.
            pit.channels[0].out = false;
            pit.set_gate(0, false);
            assert!(
                pit.get_out(0),
                "Mode {} must force OUT HIGH while GATE is low",
                command >> 1 & 0x07
            );

            // GATE rising starts a fresh high phase, not the stale low phase.
            pit.set_gate(0, true);
            assert!(
                pit.get_out(0),
                "Mode {} must restart with OUT HIGH",
                command >> 1 & 0x07
            );
            assert!(pit.channels[0].out, "stored OUT must restart HIGH");
        }
    }

    #[test]
    fn channel2_mode2_latch_reports_elapsed_oscillator_ticks() {
        const DIVISOR: u16 = 1024;
        const TICKS: u64 = 29;
        let mut pit = Pit::new();
        pit.write_command(0xB4); // channel 2, mode 2, low/high byte
        for byte in DIVISOR.to_le_bytes() {
            pit.write_counter(2, byte);
        }
        pit.set_gate(2, false);
        pit.set_gate(2, true);
        pit.total_cycles += TICKS;
        pit.write_command(0x80);
        let low = pit.read_counter(2);
        let high = pit.read_counter(2);
        assert_eq!(u16::from_le_bytes([low, high]), DIVISOR - TICKS as u16);
    }

    #[test]
    fn test_mode2_reload_boundary_divisor_65536_never_zero() {
        // This model reports a nonzero count at the Mode 2 reload boundary.
        // For divisor 65536, the reload boundary has remaining = 65536 which
        // would truncate to 0 in u16. The model returns 0xFFFF instead,
        // keeping the periodic count distinct from terminal count.
        let mut pit = Pit::new();
        pit.channels[0].mode = 2;
        pit.channels[0].reload = 0; // 0 means divisor 65536
        pit.channels[0].base_cycles = 0;
        pit.channels[0].counting = true;
        pit.channels[0].loaded = true;

        // At elapsed=0 (reload boundary): remaining = 65536, Mode 2 returns 0xFFFF
        pit.total_cycles = 0;
        assert_eq!(
            pit.compute_counter(0),
            0xFFFF,
            "Mode 2 counter must never be 0 at reload boundary"
        );

        // At exact divisor boundary (65536 PIT ticks later): same reload boundary
        pit.total_cycles = 65536;
        assert_eq!(
            pit.compute_counter(0),
            0xFFFF,
            "Mode 2 counter must never be 0 at cycle boundary"
        );

        // One tick before boundary: remaining = 1
        pit.total_cycles = 65535;
        assert_eq!(pit.compute_counter(0), 1);

        // Midpoint: remaining = 32768
        pit.total_cycles = 32768;
        assert_eq!(pit.compute_counter(0), 32768);
    }

    #[test]
    fn test_mode0_reload_boundary_divisor_65536_returns_zero() {
        // Mode 0 (one-shot) is NOT affected by the Mode 2 fix.
        // Counter reaching 0 is correct for Mode 0.
        let mut pit = Pit::new();
        pit.channels[0].mode = 0;
        pit.channels[0].reload = 0; // divisor 65536
        pit.channels[0].base_cycles = 0;
        pit.channels[0].counting = true;
        pit.channels[0].loaded = true;

        // Mode 0 at elapsed=0: counter = divisor as u16 = 0 (correct)
        pit.total_cycles = 0;
        assert_eq!(pit.compute_counter(0), 0);
    }

    #[test]
    fn test_mode3_reload_boundary_divisor_65536() {
        // Mode 3 (square wave) uses the continuous expression but is NOT Mode 2.
        // The Mode 2 fix should NOT apply to Mode 3.
        let mut pit = Pit::new();
        pit.channels[0].mode = 3;
        pit.channels[0].reload = 0; // divisor 65536
        pit.channels[0].base_cycles = 0;
        pit.channels[0].counting = true;
        pit.channels[0].loaded = true;

        // Mode 3 at reload boundary: returns 0 (65536 as u16), unchanged
        pit.total_cycles = 0;
        assert_eq!(pit.compute_counter(0), 0);
    }

    #[test]
    fn channel2_mode2_count_scales_with_instruction_to_clock_ratio() {
        const TICKS: u64 = 31;
        for divisor in [0_u16, 1024] {
            for ratio in [2_u64, 4, 8] {
                let mut pit = Pit::new();
                pit.set_emulated_speed(u64::from(PIT_FREQUENCY) * ratio);
                pit.write_command(0xB4);
                for byte in divisor.to_le_bytes() {
                    pit.write_counter(2, byte);
                }
                pit.set_gate(2, false);
                pit.set_gate(2, true);
                pit.total_cycles += TICKS * ratio;
                pit.write_command(0x80);
                let low = pit.read_counter(2);
                let high = pit.read_counter(2);
                assert_eq!(
                    u16::from_le_bytes([low, high]),
                    divisor.wrapping_sub(TICKS as u16)
                );
            }
        }
    }

    #[test]
    fn test_timer_output_mode0_ch2_out_transitions_at_terminal_count() {
        // This test verifies that get_out(2) transitions from LOW to HIGH
        // when the counter reaches terminal count.
        let mut pit = Pit::new();

        // Program channel 2: mode 0, access mode 3 (LSB/MSB)
        pit.write_command(0xB0); // 1011_0000 = channel 2, LSB/MSB, mode 0
        assert!(!pit.get_out(2), "Mode 0 OUT should start LOW");

        // Load count of 100
        pit.write_counter(2, 100);
        pit.write_counter(2, 0); // reload = 100
        assert!(!pit.get_out(2), "OUT should still be LOW after load");

        // Gate LOW→HIGH
        pit.set_gate(2, false);
        pit.set_gate(2, true);
        assert!(!pit.get_out(2), "OUT should be LOW after gate rising edge");

        // Advance time but not enough for terminal count
        // With default ips = PIT_FREQUENCY, 1 CPU cycle = 1 PIT tick
        pit.total_cycles += 50;
        assert!(
            !pit.get_out(2),
            "OUT should be LOW before terminal count (50 of 100 ticks)"
        );

        // Advance past terminal count
        pit.total_cycles += 60; // total = 110 > 100
        assert!(
            pit.get_out(2),
            "OUT should be HIGH after terminal count (110 ticks >= 100 divisor)"
        );
    }

    #[test]
    fn test_mode0_counter_wraps_past_terminal_count() {
        for divisor in [13_u16, 257] {
            let mut pit = Pit::new();
            pit.write_command(0xB0); // channel 2, mode 0, low/high byte
            for byte in divisor.to_le_bytes() {
                pit.write_counter(2, byte);
            }
            pit.set_gate(2, false);
            pit.set_gate(2, true);
            let base = pit.channels[2].base_cycles;
            pit.total_cycles = base + u64::from(divisor) - 1;
            assert_eq!(pit.compute_counter(2), 1);
            assert!(!pit.get_out(2));
            pit.total_cycles += 1;
            assert_eq!(pit.compute_counter(2), 0);
            assert!(pit.get_out(2));
            for ticks_past in [1_u16, 2, 7, 19] {
                pit.total_cycles = base + u64::from(divisor) + u64::from(ticks_past);
                assert_eq!(pit.compute_counter(2), 0_u16.wrapping_sub(ticks_past));
                assert!(pit.get_out(2));
                let previous = pit.compute_counter(2);
                pit.total_cycles += 1;
                assert_eq!(pit.compute_counter(2), previous - 1);
            }
        }
    }

    #[test]
    fn test_mode0_counter_wraps_divisor_65536() {
        // Edge case: Mode 0 with count=0 (= 65536 divisor).
        // Counter wraps: 0 (=65536), 0xFFFF, ..., 1, 0, 0xFFFF, ...
        let mut pit = Pit::new();

        pit.write_command(0xB0); // Channel 2, Mode 0
        pit.write_counter(2, 0x00);
        pit.write_counter(2, 0x00); // count = 0 → divisor 65536

        pit.set_gate(2, false);
        pit.set_gate(2, true);

        // At start: counter = divisor as u16 = 0 (represents 65536)
        assert_eq!(pit.compute_counter(2), 0);

        // Mid-count
        pit.total_cycles += 100;
        assert_eq!(pit.compute_counter(2), 65436); // 65536 - 100

        // At terminal count
        pit.total_cycles = pit.channels[2].base_cycles + 65536;
        assert_eq!(pit.compute_counter(2), 0);

        // Past terminal count: wraps
        pit.total_cycles = pit.channels[2].base_cycles + 65537;
        assert_eq!(pit.compute_counter(2), 0xFFFF);
    }

    #[test]
    fn test_mode2_counter_never_zero_small_divisor() {
        // Mode 2 with a small divisor (100): counter cycles from 100 to 1,
        // never showing 0. This verifies the general Mode 2 invariant.
        let mut pit = Pit::new();

        pit.write_command(0xB4); // Channel 2, LSB/MSB, Mode 2
        pit.write_counter(2, 100);
        pit.write_counter(2, 0); // reload = 100

        pit.set_gate(2, false);
        pit.set_gate(2, true);

        // Sweep through multiple full cycles — counter must never be 0
        let base = pit.channels[2].base_cycles;
        for tick in 0..500 {
            pit.total_cycles = base + tick;
            let counter = pit.compute_counter(2);
            assert_ne!(
                counter, 0,
                "Mode 2 counter must never be 0 (tick={tick}, expected range 1..=100)"
            );
            assert!(
                counter >= 1 && counter <= 100,
                "Mode 2 counter out of range: {counter} at tick {tick}"
            );
        }
    }

    #[test]
    fn test_mode2_counter_never_zero_divisor_65536() {
        // Mode 2 with divisor 65536 (reload=0): the counter must NEVER return 0.
        // At the reload boundary, remaining=65536 would truncate to 0 in u16.
        let mut pit = Pit::new();

        pit.write_command(0xB4); // Channel 2, LSB/MSB, Mode 2
        pit.write_counter(2, 0x00);
        pit.write_counter(2, 0x00); // reload = 0 → divisor 65536

        pit.set_gate(2, false);
        pit.set_gate(2, true);

        let base = pit.channels[2].base_cycles;

        // Check around the reload boundaries (tick 0, 65536, 131072)
        for &boundary in &[0u64, 65536, 131072] {
            for offset in 0..=2 {
                pit.total_cycles = base + boundary + offset;
                let counter = pit.compute_counter(2);
                assert_ne!(
                    counter, 0,
                    "Mode 2 counter must never be 0 (boundary={boundary}, offset={offset})"
                );
            }
        }

        // Mid-cycle values should be correct
        pit.total_cycles = base + 1;
        assert_eq!(pit.compute_counter(2), 65535);

        pit.total_cycles = base + 32768;
        assert_eq!(pit.compute_counter(2), 32768);

        pit.total_cycles = base + 65535;
        assert_eq!(pit.compute_counter(2), 1);
    }

    #[test]
    fn test_mode2_polling_gate_on_immediate_read_not_zero() {
        // A periodic counter remains nonzero during reads immediately after
        // its gate rises, including when the encoded divisor is zero.
        let mut pit = Pit::new();
        pit.set_emulated_speed(u64::from(PIT_FREQUENCY) * 8);

        pit.write_command(0xB4); // Channel 2, LSB/MSB, Mode 2
        pit.write_counter(2, 0x00);
        pit.write_counter(2, 0x00); // reload = 0 → divisor 65536

        // Gate OFF then ON (restart counting)
        pit.set_gate(2, false);
        pit.set_gate(2, true);

        // Read across a small interval after restarting the channel.
        for step in 0..5 {
            pit.total_cycles += 5; // ~5 instructions per loop iteration
            pit.write_command(0x80); // Latch channel 2
            let lsb = pit.read_counter(2);
            let msb = pit.read_counter(2);
            let counter = u16::from_le_bytes([lsb, msb]);

            assert_ne!(
                counter, 0,
                "Mode 2 counter must not be 0 immediately after gate ON \
                 (step={step}, total_cycles={})",
                pit.total_cycles
            );
        }
    }

    #[test]
    fn test_mode2_ch2_out_demand_driven() {
        // Mode 2 (rate generator): OUT is HIGH for (N-1) ticks,
        // LOW for 1 tick at end of period.
        let mut pit = Pit::new();

        pit.write_command(0xB4); // Channel 2, LSB/MSB, Mode 2
        pit.write_counter(2, 10);
        pit.write_counter(2, 0); // reload = 10

        // At tick 0: OUT should be HIGH (not at end-of-period)
        assert!(pit.get_out(2), "Mode 2 OUT should be HIGH at start");

        // At tick 8: still HIGH (counter=2, not at the single LOW tick)
        pit.total_cycles += 8;
        assert!(pit.get_out(2), "OUT should be HIGH at tick 8 of 10");

        // At tick 9: LOW (counter=1, the one-tick LOW pulse)
        pit.total_cycles += 1;
        assert!(
            !pit.get_out(2),
            "OUT should be LOW at tick 9 of 10 (terminal pulse)"
        );

        // At tick 10: HIGH again (counter reloaded to 10)
        pit.total_cycles += 1;
        assert!(pit.get_out(2), "OUT should be HIGH after reload at tick 10");
    }

    #[test]
    fn test_mode3_ch2_out_square_wave_demand_driven() {
        // Mode 3 (square wave) on channel 2: OUT toggles at half-period.
        // Without event-driven track_time(), ch.out stays at initial HIGH,
        // and compute_out produces a clean square wave from elapsed time.
        let mut pit = Pit::new();

        pit.write_command(0xB6); // Channel 2, LSB/MSB, Mode 3
        pit.write_counter(2, 100);
        pit.write_counter(2, 0); // reload = 100, half = 50

        // First quarter: HIGH
        pit.total_cycles = 25;
        assert!(pit.get_out(2), "First quarter should be HIGH");

        // Just before half: still HIGH
        pit.total_cycles = 49;
        assert!(pit.get_out(2), "Just before half should be HIGH");

        // Second half: LOW
        pit.total_cycles = 50;
        assert!(!pit.get_out(2), "Second half should be LOW");

        pit.total_cycles = 75;
        assert!(!pit.get_out(2), "Third quarter should be LOW");

        // Next period: HIGH again
        pit.total_cycles = 100;
        assert!(pit.get_out(2), "Start of second period should be HIGH");

        pit.total_cycles = 149;
        assert!(pit.get_out(2), "Just before second half of second period");

        pit.total_cycles = 150;
        assert!(
            !pit.get_out(2),
            "Second half of second period should be LOW"
        );
    }

    #[test]
    fn test_mode0_gate_resumes_not_reloads() {
        // Mode 0: gate LOW->HIGH should resume counting from frozen position,
        // NOT reload the counter. This is different from modes 2/3.
        let mut pit = Pit::new();

        pit.write_command(0x30); // Channel 0, LSB/MSB, Mode 0
        pit.write_counter(0, 0xE8);
        pit.write_counter(0, 0x03); // reload = 1000

        pit.total_cycles = 200;

        // Gate LOW — stops counting
        pit.set_gate(0, false);
        let frozen = pit.channels[0].counter;
        assert!(frozen < 1000);

        // Gate HIGH — should resume from frozen, not reload
        pit.set_gate(0, true);
        assert!(pit.channels[0].counting);
        // Counter should still reflect the frozen value, not be reloaded to 1000
        let after_gate = pit.compute_counter(0);
        assert!(
            after_gate <= frozen,
            "Mode 0 should resume from frozen position ({frozen}), not reload. Got {after_gate}"
        );
    }

    // =========================================================================
    // BCD conversion tests
    // =========================================================================

    #[test]
    fn test_bcd_to_binary_round_trip() {
        // 0x0000 → 0, 0x9999 → 9999
        assert_eq!(bcd_to_binary(0x0000), 0);
        assert_eq!(bcd_to_binary(0x0001), 1);
        assert_eq!(bcd_to_binary(0x0099), 99);
        assert_eq!(bcd_to_binary(0x0100), 100);
        assert_eq!(bcd_to_binary(0x9999), 9999);
    }

    #[test]
    fn test_binary_to_bcd_round_trip() {
        assert_eq!(binary_to_bcd(0), 0x0000);
        assert_eq!(binary_to_bcd(1), 0x0001);
        assert_eq!(binary_to_bcd(99), 0x0099);
        assert_eq!(binary_to_bcd(100), 0x0100);
        assert_eq!(binary_to_bcd(9999), 0x9999);
        assert_eq!(binary_to_bcd(1234), 0x1234);
    }

    #[test]
    fn test_bcd_binary_round_trip_all_digits() {
        for val in [0, 1, 10, 42, 100, 256, 999, 5000, 9999] {
            let bcd = binary_to_bcd(val);
            let back = bcd_to_binary(bcd);
            assert_eq!(back, val, "Round-trip failed for {val}: bcd={bcd:#06X}");
        }
    }

    #[test]
    fn test_effective_divisor_binary_zero_is_65536() {
        assert_eq!(effective_divisor(0, false), 65536);
    }

    #[test]
    fn test_effective_divisor_binary_nonzero() {
        assert_eq!(effective_divisor(1000, false), 1000);
        assert_eq!(effective_divisor(1, false), 1);
        assert_eq!(effective_divisor(0xFFFF, false), 65535);
    }

    #[test]
    fn test_effective_divisor_bcd_zero_is_10000() {
        assert_eq!(effective_divisor(0, true), 10000);
    }

    // =========================================================================
    // BCD mode counting
    // =========================================================================

    #[test]
    fn test_compute_counter_bcd_mode() {
        let mut pit = Pit::new();
        // Channel 0, Mode 2, BCD
        pit.write_command(0x35); // 0x35 = channel 0, LSB/MSB, mode 2, BCD=1
                                 // Write BCD count 0x1000 (= decimal 1000)
        pit.write_counter(0, 0x00); // LSB
        pit.write_counter(0, 0x10); // MSB → reload = 0x1000

        assert!(pit.channels[0].bcd);
        assert_eq!(pit.channels[0].reload, 0x1000);

        // No elapsed time → counter should equal reload
        let counter = pit.compute_counter(0);
        assert_eq!(counter, 0x1000);

        // After 500 PIT ticks (1:1 mapping), counter should be ~500 in BCD
        pit.total_cycles = pit.channels[0].base_cycles + 500;
        let counter = pit.compute_counter(0);
        assert_eq!(counter, 0x0500, "500 remaining in BCD = 0x0500");
    }

    #[test]
    fn test_compute_counter_bcd_mode0_one_shot() {
        let mut pit = Pit::new();
        // Channel 0, Mode 0, BCD
        pit.write_command(0x31); // channel 0, LSB/MSB, mode 0, BCD=1
        pit.write_counter(0, 0x50); // BCD reload = 0x0050 (50 decimal)
        pit.write_counter(0, 0x00);

        assert!(pit.channels[0].bcd);
        assert_eq!(pit.channels[0].reload, 0x0050);

        // After 30 ticks, counter should be 20 in BCD = 0x0020
        pit.total_cycles = pit.channels[0].base_cycles + 30;
        let counter = pit.compute_counter(0);
        assert_eq!(counter, 0x0020);

        // After 60 ticks (10 past divisor=50), counter wraps.
        // binary: 50u16.wrapping_sub(60 % 65536) = 50 - 60 = 0xFFF6 = 65526
        // BCD encode 65526... but BCD mode wraps through BCD space.
        // Actually, the raw binary result wraps, then gets BCD-encoded.
        pit.total_cycles = pit.channels[0].base_cycles + 60;
        let counter = pit.compute_counter(0);
        // binary_result = 50u16.wrapping_sub(60u16) = 65526
        // binary_to_bcd(65526) encodes the BCD representation
        assert_ne!(counter, 0, "Counter should wrap, not clamp to 0");
    }

    #[test]
    fn test_compute_counter_bcd_mode0_wraps_in_decimal_space() {
        let mut pit = Pit::new();
        pit.write_command(0x31); // channel 0, LSB/MSB, mode 0, BCD
        pit.write_counter(0, 0x34); // BCD reload = 0x1234 (1234 decimal)
        pit.write_counter(0, 0x12);

        // Two ticks after terminal count, a BCD one-shot wraps 1234 -> 9999
        // -> 9998. It must not use the binary 16-bit wrap or clamp at 9999.
        pit.total_cycles = pit.channels[0].base_cycles + 1236;
        assert_eq!(pit.compute_counter(0), 0x9998);
    }

    #[test]
    fn test_bcd_one_shot_gate_resume_keeps_decimal_phase() {
        for (command, mode) in [(0x31, 0), (0x39, 4)] {
            let mut pit = Pit::new();
            pit.write_command(command); // channel 0, BCD one-shot
            pit.write_counter(0, 0x34); // BCD reload = 0x1234 (1234 decimal)
            pit.write_counter(0, 0x12);

            pit.total_cycles = pit.channels[0].base_cycles + 100;
            pit.set_gate(0, false);
            let frozen = pit.channels[0].counter;
            assert_eq!(frozen, 0x1134, "mode {mode} must snapshot 1134 BCD");

            pit.set_gate(0, true);
            assert_eq!(
                pit.compute_counter(0),
                frozen,
                "mode {mode} must resume from the frozen BCD phase"
            );
        }
    }

    // =========================================================================
    // Mode 1: Hardware retriggerable one-shot
    // =========================================================================

    #[test]
    fn test_mode1_out_starts_high_until_its_gate_trigger() {
        // Renamed and corrected: Mode 1 is the hardware retriggerable
        // one-shot, whose OUT is HIGH after the mode write and goes LOW only
        // on the GATE rising edge (MDD-BUG-TEM-00001). The old assertion
        // codified the defect — and made the mode indistinguishable from Mode
        // 0, since a one-shot that starts low and ends high emits no pulse.
        let mut pit = Pit::new();
        pit.write_command(0x32); // Channel 0, LSB/MSB, Mode 1
        assert!(pit.get_out(0), "Mode 1 OUT should start HIGH");
    }

    #[test]
    fn test_mode1_gate_triggered_counting_does_not_start() {
        let mut pit = Pit::new();
        pit.write_command(0x32); // Channel 0, LSB/MSB, Mode 1
        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00);
        assert!(
            !pit.channels[0].counting,
            "Mode 1 should not count until gate trigger"
        );
    }

    #[test]
    fn test_mode1_rising_edge_starts_counting() {
        let mut pit = Pit::new();
        pit.write_command(0x32); // Channel 0, LSB/MSB, Mode 1
        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00); // reload = 16

        // Ensure gate is low first
        pit.set_gate(0, false);
        assert!(!pit.channels[0].counting);

        // Rising edge triggers one-shot
        pit.set_gate(0, true);
        assert!(pit.channels[0].counting);
        assert!(!pit.channels[0].out, "Mode 1 OUT goes LOW on trigger");
        assert!(!pit.channels[0].terminal_count_reached);
        assert_eq!(pit.channels[0].counter, 16);
    }

    #[test]
    fn test_mode1_retrigger_resets_count() {
        let mut pit = Pit::new();
        pit.write_command(0x32); // Mode 1
        pit.write_counter(0, 100);
        pit.write_counter(0, 0); // reload = 100

        // First trigger
        pit.set_gate(0, false);
        pit.set_gate(0, true);
        pit.total_cycles += 50; // Half-way through

        // Re-trigger: should restart from reload
        pit.set_gate(0, false);
        pit.set_gate(0, true);
        assert_eq!(
            pit.channels[0].counter, 100,
            "Retrigger should reload counter"
        );
        assert_eq!(pit.channels[0].base_cycles, pit.total_cycles);
    }

    // =========================================================================
    // Mode 4: Software triggered strobe
    // =========================================================================

    #[test]
    fn test_mode4_out_starts_high() {
        let mut pit = Pit::new();
        pit.write_command(0x38); // Channel 0, LSB/MSB, Mode 4
        assert!(pit.get_out(0), "Mode 4 OUT should start HIGH");
    }

    #[test]
    fn test_mode4_is_one_shot_not_gate_triggered() {
        let mut pit = Pit::new();
        pit.write_command(0x38);
        assert!(pit.channels[0].is_one_shot());
        assert!(!pit.channels[0].is_gate_triggered());
    }

    #[test]
    fn test_mode4_counting_starts_on_load() {
        let mut pit = Pit::new();
        pit.write_command(0x38); // Mode 4
        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00); // reload = 16
        assert!(
            pit.channels[0].counting,
            "Mode 4 should start counting on load"
        );
        assert!(pit.channels[0].loaded);
    }

    #[test]
    fn test_mode4_gate_enables_disables_counting() {
        let mut pit = Pit::new();
        pit.write_command(0x38); // Mode 4
        pit.write_counter(0, 100);
        pit.write_counter(0, 0);

        // Gate LOW stops counting
        pit.set_gate(0, false);
        assert!(!pit.channels[0].counting);

        // Gate HIGH resumes counting
        pit.set_gate(0, true);
        assert!(pit.channels[0].counting);
    }

    // =========================================================================
    // Mode 5: Hardware triggered strobe
    // =========================================================================

    #[test]
    fn test_mode5_out_starts_high() {
        let mut pit = Pit::new();
        pit.write_command(0x3A); // Channel 0, LSB/MSB, Mode 5
        assert!(pit.get_out(0), "Mode 5 OUT should start HIGH");
    }

    #[test]
    fn test_mode5_does_not_count_until_gate() {
        let mut pit = Pit::new();
        pit.write_command(0x3A); // Mode 5
        pit.write_counter(0, 50);
        pit.write_counter(0, 0);
        assert!(!pit.channels[0].counting);
    }

    #[test]
    fn test_mode5_rising_edge_triggers() {
        let mut pit = Pit::new();
        pit.write_command(0x3A); // Mode 5
        pit.write_counter(0, 50);
        pit.write_counter(0, 0);

        pit.set_gate(0, false);
        pit.set_gate(0, true);
        assert!(pit.channels[0].counting);
        assert!(pit.channels[0].out, "Mode 5 OUT stays HIGH until TC");
        assert!(!pit.channels[0].terminal_count_reached);
        assert_eq!(pit.channels[0].counter, 50);
    }

    // =========================================================================
    // Counter latch operations
    // =========================================================================

    #[test]
    fn test_latch_command_all_channels() {
        let mut pit = Pit::new();

        // Latch channel 0: command bits 7:6 = 00, access = 00
        pit.write_command(0x00);
        assert!(pit.channels[0].latch.is_some());

        // Latch channel 1: command bits 7:6 = 01, access = 00
        pit.write_command(0x40);
        assert!(pit.channels[1].latch.is_some());

        // Latch channel 2: command bits 7:6 = 10, access = 00
        pit.write_command(0x80);
        assert!(pit.channels[2].latch.is_some());
    }

    #[test]
    fn test_latch_resets_read_state() {
        let mut pit = Pit::new();
        pit.channels[0].read_state = 1; // Mid-read
        pit.write_command(0x00); // Latch channel 0
        assert_eq!(
            pit.channels[0].read_state, 0,
            "Latch should reset read_state"
        );
    }

    #[test]
    fn test_latch_read_consumes_latch_lsb_only() {
        let mut pit = Pit::new();
        pit.channels[0].access_mode = 1; // LSB only
        pit.channels[0].latch = Some(0xABCD);
        let val = pit.read_counter(0);
        assert_eq!(val, 0xCD);
        assert!(pit.channels[0].latch.is_none(), "Latch should be consumed");
    }

    #[test]
    fn test_latch_read_consumes_latch_msb_only() {
        let mut pit = Pit::new();
        pit.channels[0].access_mode = 2; // MSB only
        pit.channels[0].latch = Some(0xABCD);
        let val = pit.read_counter(0);
        assert_eq!(val, 0xAB);
        assert!(pit.channels[0].latch.is_none(), "Latch should be consumed");
    }

    #[test]
    fn test_latch_read_lsb_msb_pair_consumes_on_msb() {
        let mut pit = Pit::new();
        pit.channels[0].access_mode = 3; // LSB/MSB
        pit.channels[0].read_state = 0;
        pit.channels[0].latch = Some(0xABCD);

        let lsb = pit.read_counter(0);
        assert_eq!(lsb, 0xCD);
        assert!(
            pit.channels[0].latch.is_some(),
            "Latch should persist until MSB read"
        );

        let msb = pit.read_counter(0);
        assert_eq!(msb, 0xAB);
        assert!(
            pit.channels[0].latch.is_none(),
            "Latch consumed after full read"
        );
    }

    // =========================================================================
    // Read-back command (8254)
    // =========================================================================

    #[test]
    fn test_readback_multiple_channels() {
        let mut pit = Pit::new();
        // Read-back: latch status + count for channels 0 and 2
        // Bit 7:6 = 11, bit 5 = 0 (latch count), bit 4 = 0 (latch status),
        // bit 1 = channel 0, bit 3 = channel 2
        pit.write_command(0xCA); // 0b1100_1010 = ch0 + ch2, latch both

        assert!(pit.channels[0].status_latch.is_some());
        assert!(pit.channels[0].latch.is_some());
        assert!(pit.channels[2].status_latch.is_some());
        assert!(pit.channels[2].latch.is_some());
        // Channel 1 should be untouched
        assert!(pit.channels[1].status_latch.is_none());
        assert!(pit.channels[1].latch.is_none());
    }

    #[test]
    fn test_readback_status_only() {
        let mut pit = Pit::new();
        // Read-back: latch status only for channel 0
        // bit 5 = 1 (don't latch count), bit 4 = 0 (latch status), bit 1 = ch0
        pit.write_command(0xE2); // 0b1110_0010

        assert!(pit.channels[0].status_latch.is_some());
        assert!(pit.channels[0].latch.is_none());
    }

    #[test]
    fn test_readback_count_only() {
        let mut pit = Pit::new();
        // Read-back: latch count only for channel 0
        // bit 5 = 0 (latch count), bit 4 = 1 (don't latch status), bit 1 = ch0
        pit.write_command(0xD2); // 0b1101_0010

        assert!(pit.channels[0].status_latch.is_none());
        assert!(pit.channels[0].latch.is_some());
    }

    #[test]
    fn test_readback_status_consumed_on_read() {
        let mut pit = Pit::new();
        pit.channels[0].out = true;
        pit.channels[0].mode = 2;
        pit.channels[0].access_mode = 3;
        pit.channels[0].loaded = true;
        pit.channels[0].bcd = false;

        // Latch status
        pit.write_command(0xE2);
        assert!(pit.channels[0].status_latch.is_some());

        // Reading counter should return status first (and consume it)
        let status = pit.read_counter(0);
        assert!(
            pit.channels[0].status_latch.is_none(),
            "Status latch consumed"
        );
        // OUT=1 → bit 7 set, loaded=true → bit 6 clear, mode=2 → bits 3:1 = 010
        assert_ne!(status & 0x80, 0, "OUT bit should be set");
        assert_eq!(status & 0x40, 0, "Null count bit should be clear (loaded)");
    }

    // =========================================================================
    // set_channel0_terminal_count
    // =========================================================================

    #[test]
    fn test_set_channel0_terminal_count() {
        let mut pit = Pit::new();
        pit.channels[0].terminal_count_reached = false;
        pit.channels[0].out = false;

        pit.set_channel0_terminal_count();

        assert!(pit.channels[0].terminal_count_reached);
        assert!(
            pit.channels[0].out,
            "Mode 0 OUT goes HIGH at terminal count"
        );
    }

    // =========================================================================
    // instructions_to_oscillator_ticks
    // =========================================================================

    #[test]
    fn test_instructions_to_oscillator_ticks_1_to_1() {
        let pit = Pit::new(); // Default: ips = pit_hz → 1:1 mapping
        assert_eq!(pit.instructions_to_oscillator_ticks(1000), 1000);
        assert_eq!(pit.instructions_to_oscillator_ticks(0), 0);
    }

    #[test]
    fn test_instructions_to_oscillator_ticks_scaled() {
        let mut pit = Pit::new();
        pit.set_emulated_speed(12_600_000); // 486 speed
                                            // 1000 instructions * 1193182 / 12600000 ≈ 94
        let ticks = pit.instructions_to_oscillator_ticks(1000);
        assert!(ticks >= 90 && ticks <= 100, "Expected ~94, got {ticks}");
    }

    // =========================================================================
    // get_loaded / get_counting accessors
    // =========================================================================

    #[test]
    fn test_get_loaded_valid_channels() {
        let mut pit = Pit::new();
        pit.write_command(0x34); // Mode 2, channel 0
        assert!(!pit.get_loaded(0), "Not loaded until counter written");

        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00);
        assert!(pit.get_loaded(0), "Loaded after counter write");
    }

    #[test]
    fn test_get_loaded_invalid_channel() {
        let pit = Pit::new();
        assert!(!pit.get_loaded(3));
        assert!(!pit.get_loaded(255));
    }

    #[test]
    fn test_get_counting_valid_channels() {
        let mut pit = Pit::new();
        // Channel 0 is pre-loaded and counting by default
        assert!(pit.get_counting(0));

        // Set mode 1 (gate-triggered) — counting stops
        pit.write_command(0x32);
        assert!(!pit.get_counting(0));
    }

    #[test]
    fn test_get_counting_invalid_channel() {
        let pit = Pit::new();
        assert!(!pit.get_counting(3));
    }

    // =========================================================================
    // apply_deferred_reload edge cases
    // =========================================================================

    #[test]
    fn test_apply_deferred_reload_invalid_channel() {
        let mut pit = Pit::new();
        // Should be a no-op for invalid channels
        pit.apply_deferred_reload(3);
        pit.apply_deferred_reload(255);
    }

    #[test]
    fn test_apply_deferred_reload_no_pending() {
        let mut pit = Pit::new();
        pit.channels[0].reload = 1000;
        pit.channels[0].deferred_reload = None;
        pit.apply_deferred_reload(0);
        assert_eq!(
            pit.channels[0].reload, 1000,
            "No change when no deferred reload"
        );
    }

    // =========================================================================
    // Deferred reload for mode 3
    // =========================================================================

    #[test]
    fn test_deferred_reload_mode3_channel0() {
        let mut pit = Pit::new();
        // Program channel 0 mode 3
        pit.write_command(0x36);
        pit.write_counter(0, 0xE8);
        pit.write_counter(0, 0x03); // reload = 1000

        pit.total_cycles += 500;

        // Counter-only write — should defer
        pit.write_counter(0, 0xF4);
        pit.write_counter(0, 0x01); // new value = 500
        assert_eq!(pit.get_deferred_reload(0), Some(500));
        assert_eq!(pit.get_divisor(0), 1000, "Old reload preserved");
    }

    // =========================================================================
    // Write dispatch via ports
    // =========================================================================

    #[test]
    fn test_write_port_0x40_channel_0() {
        let mut pit = Pit::new();
        pit.write_command(0x34); // mode 2, ch0, LSB/MSB
        pit.write(0x40, 0x00);
        pit.write(0x40, 0x10);
        assert_eq!(pit.channels[0].reload, 0x1000);
    }

    #[test]
    fn test_write_port_0x41_channel_1() {
        let mut pit = Pit::new();
        pit.write_command(0x74); // mode 2, ch1, LSB/MSB
        pit.write(0x41, 0x34);
        pit.write(0x41, 0x12);
        assert_eq!(pit.channels[1].reload, 0x1234);
    }

    #[test]
    fn test_write_port_0x42_channel_2() {
        let mut pit = Pit::new();
        pit.write_command(0xB4); // mode 2, ch2, LSB/MSB
        pit.write(0x42, 0x56);
        pit.write(0x42, 0x78);
        assert_eq!(pit.channels[2].reload, 0x7856);
    }

    #[test]
    fn test_write_port_0x43_command() {
        let mut pit = Pit::new();
        pit.write(0x43, 0x34); // mode 2, ch0
        assert_eq!(pit.channels[0].mode, 2);
    }

    // =========================================================================
    // set_mode resets state correctly
    // =========================================================================

    #[test]
    fn test_set_mode_resets_write_state() {
        let mut pit = Pit::new();
        pit.channels[0].write_state = 1; // Mid-write
        pit.write_command(0x34); // Mode 2
        assert_eq!(pit.channels[0].write_state, 0);
    }

    #[test]
    fn test_set_mode_resets_loaded_and_terminal_count() {
        let mut pit = Pit::new();
        pit.channels[0].loaded = true;
        pit.channels[0].terminal_count_reached = true;
        pit.write_command(0x30); // Mode 0
        assert!(!pit.channels[0].loaded);
        assert!(!pit.channels[0].terminal_count_reached);
    }

    #[test]
    fn test_set_mode_initial_out_by_mode() {
        let mut pit = Pit::new();

        // Mode 0: OUT starts LOW
        pit.write_command(0x30);
        assert!(!pit.get_out(0));

        // Mode 2: OUT starts HIGH
        pit.write_command(0x34);
        assert!(pit.get_out(0));

        // Mode 3: OUT starts HIGH
        pit.write_command(0x36);
        assert!(pit.get_out(0));

        // Mode 4: OUT starts HIGH
        pit.write_command(0x38);
        assert!(pit.get_out(0));

        // Mode 5: OUT starts HIGH
        pit.write_command(0x3A);
        assert!(pit.get_out(0));

        // Mode 1: OUT starts HIGH — the one-shot idles high and pulses low
        // only between its GATE trigger and terminal count (MDD-BUG-TEM-00001).
        pit.write_command(0x32);
        assert!(pit.get_out(0));
    }

    #[test]
    fn test_set_mode_gate_triggered_disables_counting() {
        let mut pit = Pit::new();
        // Mode 1 is gate-triggered — counting should be false after mode set
        pit.write_command(0x32);
        assert!(!pit.channels[0].counting);

        // Mode 5 is also gate-triggered
        pit.write_command(0x3A);
        assert!(!pit.channels[0].counting);

        // Mode 0 is NOT gate-triggered — counting should be true
        pit.write_command(0x30);
        assert!(pit.channels[0].counting);
    }

    // =========================================================================
    // finish_counter_load: mode-specific behavior
    // =========================================================================

    #[test]
    fn test_finish_counter_load_mode0_sets_out_low() {
        let mut pit = Pit::new();
        pit.write_command(0x30); // Mode 0
        pit.channels[0].out = true; // Force HIGH
        pit.write_counter(0, 10);
        pit.write_counter(0, 0);
        // Mode 0: OUT should go LOW on counter load
        assert!(!pit.get_out(0));
    }

    #[test]
    fn test_finish_counter_load_mode4_does_not_change_out() {
        let mut pit = Pit::new();
        pit.write_command(0x38); // Mode 4
                                 // Mode 4 OUT starts HIGH and stays HIGH until TC
        pit.write_counter(0, 10);
        pit.write_counter(0, 0);
        assert!(pit.get_out(0), "Mode 4 OUT should remain HIGH after load");
    }

    #[test]
    fn test_finish_counter_load_sets_base_cycles() {
        let mut pit = Pit::new();
        pit.total_cycles = 5000;
        pit.write_command(0x34); // Mode 2
        pit.write_counter(0, 100);
        pit.write_counter(0, 0);
        assert_eq!(pit.channels[0].base_cycles, 5000);
    }

    // =========================================================================
    // Counter write: LSB-only and MSB-only modes clear the unwritten byte
    // =========================================================================

    #[test]
    fn test_write_counter_lsb_only_clears_msb() {
        let mut pit = Pit::new();
        pit.write_command(0x14); // Channel 0, LSB only, Mode 2
        pit.channels[0].reload = 0xFF00;
        pit.write_counter(0, 0xAB);
        assert_eq!(pit.channels[0].reload, 0x00AB);
    }

    #[test]
    fn test_write_counter_msb_only_clears_lsb() {
        let mut pit = Pit::new();
        pit.write_command(0x24); // Channel 0, MSB only, Mode 2
        pit.channels[0].reload = 0x00FF;
        pit.write_counter(0, 0xCD);
        assert_eq!(pit.channels[0].reload, 0xCD00);
    }

    // =========================================================================
    // Multi-channel operations
    // =========================================================================

    #[test]
    fn test_read_counter_all_channels() {
        let mut pit = Pit::new();

        // Program channel 1
        pit.write_command(0x74); // Ch1, LSB/MSB, Mode 2
        pit.write_counter(1, 0x34);
        pit.write_counter(1, 0x12); // reload = 0x1234

        // Program channel 2
        pit.write_command(0xB4); // Ch2, LSB/MSB, Mode 2
        pit.write_counter(2, 0x78);
        pit.write_counter(2, 0x56); // reload = 0x5678

        // Latch and read channel 1
        pit.write_command(0x40); // Latch ch1
        let lsb = pit.read_counter(1);
        let msb = pit.read_counter(1);
        assert_eq!(u16::from_le_bytes([lsb, msb]), 0x1234);

        // Latch and read channel 2
        pit.write_command(0x80); // Latch ch2
        let lsb = pit.read_counter(2);
        let msb = pit.read_counter(2);
        assert_eq!(u16::from_le_bytes([lsb, msb]), 0x5678);
    }

    // =========================================================================
    // PitMode classification
    // =========================================================================

    #[test]
    fn test_pit_mode_is_one_shot() {
        assert!(PitMode::OneShot.is_one_shot());
        assert!(!PitMode::RateGenerator.is_one_shot());
        assert!(!PitMode::SquareWave.is_one_shot());
    }

    // =========================================================================
    // PitChannel::new default states
    // =========================================================================

    #[test]
    fn test_pit_channel_new_defaults() {
        // Mode 0: OUT starts LOW
        let ch0 = PitChannel::new(0);
        assert!(!ch0.out);
        assert!(ch0.gate);
        assert!(!ch0.loaded);
        assert!(!ch0.bcd);
        assert_eq!(ch0.access_mode, 3);
        assert_eq!(ch0.counter, DEFAULT_DIVISOR);

        // Mode 1: OUT starts HIGH — it is the one-shot, and only Mode 0
        // starts low (MDD-BUG-TEM-00001).
        let ch1 = PitChannel::new(1);
        assert!(ch1.out);

        // Mode 2: OUT starts HIGH
        let ch2 = PitChannel::new(2);
        assert!(ch2.out);

        // Mode 3: OUT starts HIGH
        let ch3 = PitChannel::new(3);
        assert!(ch3.out);

        // Mode 4: OUT starts HIGH
        let ch4 = PitChannel::new(4);
        assert!(ch4.out);

        // Mode 5: OUT starts HIGH
        let ch5 = PitChannel::new(5);
        assert!(ch5.out);
    }

    // =========================================================================
    // set_emulated_speed edge cases
    // =========================================================================

    #[test]
    fn test_set_emulated_speed_clamps_to_1() {
        let mut pit = Pit::new();
        pit.set_emulated_speed(0);
        assert_eq!(pit.ips, 1, "Speed 0 should clamp to 1");
    }

    // =========================================================================
    // set_pit_hz edge cases
    // =========================================================================

    #[test]
    fn test_set_pit_hz_clamps_to_1() {
        let mut pit = Pit::new();
        pit.set_pit_hz(0);
        assert_eq!(pit.pit_hz(), 1, "Frequency 0 should clamp to 1");
    }

    // =========================================================================
    // compute_counter: not counting returns frozen value
    // =========================================================================

    #[test]
    fn test_compute_counter_not_counting_returns_frozen() {
        let mut pit = Pit::new();
        pit.channels[0].mode = 2;
        pit.channels[0].counting = false;
        pit.channels[0].counter = 42;
        pit.total_cycles = 999;
        assert_eq!(pit.compute_counter(0), 42);
    }

    // =========================================================================
    // restore_controller_state sets event_reschedule_pending
    // =========================================================================

    #[test]
    fn test_restore_controller_state_sets_reschedule_flag() {
        let mut pit = Pit::new();
        pit.event_reschedule_pending = false;
        pit.restore_controller_state(PIT_FREQUENCY, 1000, PIT_FREQUENCY as u64);
        assert!(pit.event_reschedule_pending);
    }

    // =========================================================================
    // get_ticks stub
    // =========================================================================

    #[test]
    fn test_get_ticks_stub() {
        let pit = Pit::new();
        assert_eq!(pit.get_ticks(), 0);
    }

    // =========================================================================
    // cumulative_ticks: cycle-derived BIOS-tick counter
    // THROWAWAY-SPIKE: delete with the rest of the spike shim once Step-0 closes.
    // =========================================================================

    #[test]
    fn test_cumulative_ticks_zero_at_boot() {
        let pit = Pit::new();
        assert_eq!(pit.cumulative_ticks(), 0);
    }

    #[test]
    fn test_cumulative_ticks_bios_default_divisor_at_18hz() {
        // BIOS default: channel 0 reload = 0 → divisor 65536, ips == pit_hz.
        // At ips == pit_hz, one CPU cycle == one PIT oscillator tick.
        // So after `divisor - 1` cycles we should still have zero ticks, and
        // after exactly `divisor` cycles we should have exactly one tick.
        let mut pit = Pit::new();
        pit.set_emulated_speed(PIT_FREQUENCY as u64);
        // After construction reload defaults to 0 — i.e. effective divisor 65536.
        let divisor: u64 = 65536;
        pit.total_cycles = divisor - 1;
        assert_eq!(pit.cumulative_ticks(), 0, "Before first boundary");
        pit.total_cycles = divisor;
        assert_eq!(pit.cumulative_ticks(), 1, "Exactly one tick");
        pit.total_cycles = divisor * 5;
        assert_eq!(pit.cumulative_ticks(), 5, "Five ticks");
    }

    #[test]
    fn test_cumulative_ticks_deterministic_across_calls() {
        let mut pit = Pit::new();
        pit.set_emulated_speed(PIT_FREQUENCY as u64);
        pit.total_cycles = 1_234_567;
        let a = pit.cumulative_ticks();
        let b = pit.cumulative_ticks();
        let c = pit.cumulative_ticks();
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn test_cumulative_ticks_scales_with_speed() {
        // At 2x ips the same cycle count corresponds to half as many PIT
        // oscillator ticks, so cumulative_ticks() should roughly halve.
        let mut pit = Pit::new();
        pit.set_emulated_speed(PIT_FREQUENCY as u64);
        let divisor: u64 = 65536;
        pit.total_cycles = divisor * 4;
        let t1 = pit.cumulative_ticks();

        let mut pit2 = Pit::new();
        pit2.set_emulated_speed(PIT_FREQUENCY as u64 * 2);
        pit2.total_cycles = divisor * 4;
        let t2 = pit2.cumulative_ticks();

        assert_eq!(t1, 4);
        assert_eq!(t2, 2);
    }

    // =========================================================================
    // speaker_reload_updated flag
    // =========================================================================

    #[test]
    fn test_speaker_reload_updated_set_on_channel2_load() {
        let mut pit = Pit::new();
        assert!(!pit.speaker_reload_updated);

        pit.write_command(0xB4); // Ch2, Mode 2, LSB/MSB
        pit.write_counter(2, 0x10);
        pit.write_counter(2, 0x00);
        assert!(pit.speaker_reload_updated);
    }

    #[test]
    fn test_speaker_reload_not_set_on_channel0_load() {
        let mut pit = Pit::new();
        pit.speaker_reload_updated = false;
        pit.write_command(0x34); // Ch0
        pit.write_counter(0, 0x10);
        pit.write_counter(0, 0x00);
        assert!(!pit.speaker_reload_updated);
    }

    // =========================================================================
    // Channel 2 mode 0/4 gate behavior
    // =========================================================================

    #[test]
    fn test_mode0_gate_low_stops_counting_snapshots_counter() {
        let mut pit = Pit::new();
        pit.write_command(0xB0); // Ch2, Mode 0, LSB/MSB
        pit.write_counter(2, 100);
        pit.write_counter(2, 0);
        pit.total_cycles = pit.channels[2].base_cycles + 30;

        pit.set_gate(2, false);
        assert!(!pit.channels[2].counting);
        // Counter should be snapshotted to 70 (100 - 30)
        assert_eq!(pit.channels[2].counter, 70);
    }

    #[test]
    fn test_mode4_gate_low_stops_counting_snapshots_counter() {
        let mut pit = Pit::new();
        pit.write_command(0xB8); // Ch2, Mode 4, LSB/MSB
        pit.write_counter(2, 100);
        pit.write_counter(2, 0);
        pit.total_cycles = pit.channels[2].base_cycles + 20;

        pit.set_gate(2, false);
        assert!(!pit.channels[2].counting);
        assert_eq!(pit.channels[2].counter, 80);
    }

    #[test]
    fn test_channel2_mode4_load_while_gate_low_does_not_count() {
        let mut pit = Pit::new();
        pit.set_gate(2, false);
        pit.write_command(0xB8); // Ch2, Mode 4, LSB/MSB
        pit.write_counter(2, 100);
        pit.write_counter(2, 0);

        pit.total_cycles = pit.channels[2].base_cycles + 20;

        assert!(!pit.channels[2].counting);
        assert_eq!(pit.compute_counter(2), 100);
    }

    #[test]
    fn test_channel2_mode4_gate_high_starts_from_loaded_count() {
        let mut pit = Pit::new();
        pit.set_gate(2, false);
        pit.write_command(0xB8); // Ch2, Mode 4, LSB/MSB
        pit.write_counter(2, 100);
        pit.write_counter(2, 0);
        pit.total_cycles = pit.channels[2].base_cycles + 20;

        pit.set_gate(2, true);

        assert!(pit.channels[2].counting);
        assert_eq!(pit.compute_counter(2), 100);
        pit.total_cycles += 1;
        assert_eq!(pit.compute_counter(2), 99);
    }

    #[test]
    fn test_channel2_mode4_reload_zero_counts_as_65536() {
        let mut pit = Pit::new();
        pit.set_gate(2, false);
        pit.write_command(0xB8); // Ch2, Mode 4, LSB/MSB
        pit.write_counter(2, 0);
        pit.write_counter(2, 0);
        pit.total_cycles = pit.channels[2].base_cycles + 20;

        pit.set_gate(2, true);

        assert_eq!(pit.compute_counter(2), 0);
        pit.total_cycles += 1;
        assert_eq!(pit.compute_counter(2), 0xFFFF);
        pit.total_cycles += 65534;
        assert_eq!(pit.compute_counter(2), 1);
        pit.total_cycles += 1;
        assert_eq!(pit.compute_counter(2), 0);
    }

    #[test]
    fn test_channel2_mode4_out_strobes_low_for_one_tick() {
        let mut pit = Pit::new();
        pit.write_command(0xB8); // Ch2, Mode 4, LSB/MSB
        pit.write_counter(2, 3);
        pit.write_counter(2, 0);

        let base_cycles = pit.channels[2].base_cycles;
        pit.total_cycles = base_cycles;
        assert!(pit.get_out(2));
        pit.total_cycles = base_cycles + 2;
        assert!(pit.get_out(2));
        pit.total_cycles = base_cycles + 3;
        assert!(!pit.get_out(2));
        pit.total_cycles = base_cycles + 4;
        assert!(pit.get_out(2));
    }

    // =========================================================================
    // Mode command: mode 6 → 2, mode 7 → 3 aliases
    // =========================================================================

    #[test]
    fn test_write_command_mode_6_aliases_to_mode_2() {
        let mut pit = Pit::new();
        // Mode 6 = bits 3:1 = 110
        pit.write_command(0x3C); // 0011_1100 → ch0, LSB/MSB, mode 6
        assert_eq!(pit.channels[0].mode, 2);
    }

    // =========================================================================
    // Deferred reload: last write wins
    // =========================================================================

    #[test]
    fn test_deferred_reload_last_write_wins() {
        let mut pit = Pit::new();
        pit.write_command(0x34); // Mode 2, ch0
        pit.write_counter(0, 0xE8);
        pit.write_counter(0, 0x03); // reload = 1000
        pit.total_cycles += 100;

        // First deferred write: 500
        pit.write_counter(0, 0xF4);
        pit.write_counter(0, 0x01);
        assert_eq!(pit.get_deferred_reload(0), Some(500));

        // Second deferred write: 750 — should overwrite
        pit.write_counter(0, 0xEE);
        pit.write_counter(0, 0x02);
        assert_eq!(pit.get_deferred_reload(0), Some(750));
    }

    // =========================================================================
    // compute_counter: continuous mode wrapping
    // =========================================================================

    #[test]
    fn test_compute_counter_continuous_wraps_around() {
        let mut pit = Pit::new();
        pit.channels[0].mode = 2;
        pit.channels[0].reload = 100;
        pit.channels[0].base_cycles = 0;
        pit.channels[0].counting = true;
        pit.channels[0].loaded = true;

        // At t=0, counter = 100 (full divisor)
        pit.total_cycles = 0;
        assert_eq!(pit.compute_counter(0), 100);

        // At t=50, counter = 50
        pit.total_cycles = 50;
        assert_eq!(pit.compute_counter(0), 50);

        // At t=99, counter = 1
        pit.total_cycles = 99;
        assert_eq!(pit.compute_counter(0), 1);

        // At t=100, counter wraps back to 100
        pit.total_cycles = 100;
        assert_eq!(pit.compute_counter(0), 100);

        // At t=150, counter = 50 again
        pit.total_cycles = 150;
        assert_eq!(pit.compute_counter(0), 50);
    }

    // =========================================================================
    // build_status: full bit layout
    // =========================================================================

    #[test]
    fn test_build_status_full_encoding() {
        let mut ch = PitChannel::new(4);
        // Set everything to known values
        ch.out = false;
        ch.loaded = true; // A count has transferred to CE.
        ch.null_count = Some(false);
        ch.access_mode = 2; // MSB only
        ch.bcd = false;

        let status = ch.build_status_with_out(ch.out);
        // Bit 7: OUT=0 → 0x00
        // Bit 6: the completed CR has transferred to CE → 0x00
        // Bits 5:4: access_mode=2 → 0x20
        // Bits 3:1: mode=4 → 0x08
        // Bit 0: BCD=0 → 0x00
        assert_eq!(status, 0x28);
    }

    // =========================================================================
    // Channel 0 defaults from Pit::new()
    // =========================================================================

    #[test]
    fn test_pit_new_channel0_preloaded() {
        let pit = Pit::new();
        assert!(pit.channels[0].loaded, "Channel 0 should be pre-loaded");
        assert!(pit.channels[0].counting, "Channel 0 should be counting");
        assert_eq!(pit.channels[0].mode, 3, "Channel 0 default is mode 3");
        assert_eq!(pit.channels[1].mode, 2, "Channel 1 default is mode 2");
        assert_eq!(pit.channels[2].mode, 2, "Channel 2 default is mode 2");
    }

    // =========================================================================
    // Gate: mode 2/3 gate HIGH → already counting, not re-triggered
    // =========================================================================

    #[test]
    fn test_gate_high_to_high_mode2_no_reload() {
        let mut pit = Pit::new();
        pit.channels[0].mode = 2;
        pit.channels[0].reload = 1000;
        pit.channels[0].counter = 500;
        pit.channels[0].counting = true;
        pit.channels[0].gate = true;
        pit.channels[0].base_cycles = 0;
        pit.total_cycles = 100;

        // Gate already HIGH, set HIGH again — should not reload
        pit.set_gate(0, true);
        // base_cycles should not change
        assert_eq!(pit.channels[0].base_cycles, 0);
    }

    // =========================================================================
    // Save/restore: save_channel_state clamping for invalid index
    // =========================================================================

    #[test]
    fn checkpoint_between_split_counter_writes_keeps_the_old_divisor() {
        // Mode 2, divisor 0x1234, loaded and counting.
        let mut pit = Pit::new();
        pit.write_command(0x34); // ch0, LSB/MSB, mode 2
        pit.write_counter(0, 0x34);
        pit.write_counter(0, 0x12);
        pit.total_cycles += 500; // mid-cycle, so a new divisor defers
        assert_eq!(pit.save_channel_state(0).reload, 0x1234);

        // Guest writes only the LSB of the next divisor, then we checkpoint.
        pit.write_counter(0, 0x78);
        let saved = pit.save_channel_state(0);
        assert_eq!(saved.write_state, 1, "the pair must be half written");

        let mut restored = Pit::new();
        let ctrl = pit.save_controller_state();
        restored.restore_controller_state(ctrl.0, ctrl.1, ctrl.2);
        restored.restore_channel_state(0, &saved);

        // The MSB arrives after the restore and completes 0x5678.
        restored.write_counter(0, 0x56);

        // The new divisor defers to the next cycle boundary; until then the
        // channel must still run on 0x1234. Dropping write_pair_old_reload
        // left the active divisor at zero — an effective 65,536.
        assert_eq!(
            restored.get_deferred_reload(0),
            Some(0x5678),
            "the new divisor must be deferred, not applied mid-cycle"
        );
        assert_eq!(
            restored.save_channel_state(0).reload,
            0x1234,
            "the pre-write divisor must survive the checkpoint"
        );
    }

    #[test]
    fn test_save_channel_state_clamps_index() {
        let pit = Pit::new();
        // Index 5 should be clamped to 2
        let saved = pit.save_channel_state(5);
        let normal = pit.save_channel_state(2);
        assert_eq!(saved.reload, normal.reload); // must match channel 2
    }

    #[test]
    fn test_restore_channel_state_clamps_index() {
        let mut pit = Pit::new();
        // Restore to index 5 — should go to channel 2
        pit.restore_channel_state(
            5,
            &PitChannelState {
                counter: 42,
                reload: 100,
                mode: 2,
                access_mode: 3,
                out: true,
                gate: true,
                loaded: true,
                counting: true,
                ..PitChannelState::default()
            },
        );
        assert_eq!(pit.channels[2].counter, 42);
        assert_eq!(pit.channels[2].reload, 100);
    }

    // =========================================================================
    // get_deferred_reload: invalid channel
    // =========================================================================

    #[test]
    fn test_get_deferred_reload_invalid_channel() {
        let pit = Pit::new();
        assert_eq!(pit.get_deferred_reload(3), None);
        assert_eq!(pit.get_deferred_reload(255), None);
    }

    // =========================================================================
    // PIT port I/O (write/read via port numbers)
    // =========================================================================

    #[test]
    fn test_pit_write_port_routing() {
        let mut pit = Pit::new();
        // Write to port 0x40 = channel 0
        pit.write_command(0x30); // ch0, LSB/MSB, mode 0
        pit.write(0x40, 0x10);
        pit.write(0x40, 0x00);
        assert_eq!(pit.channels[0].reload, 0x0010);

        // Write to port 0x41 = channel 1
        pit.write_command(0x74); // ch1, LSB/MSB, mode 2
        pit.write(0x41, 0x20);
        pit.write(0x41, 0x00);
        assert_eq!(pit.channels[1].reload, 0x0020);

        // Write to port 0x42 = channel 2
        pit.write_command(0xB6); // ch2, LSB/MSB, mode 3
        pit.write(0x42, 0x30);
        pit.write(0x42, 0x00);
        assert_eq!(pit.channels[2].reload, 0x0030);

        // Command port
        pit.write(0x43, 0x36); // Channel 0, mode 3
        assert_eq!(pit.channels[0].mode, 3);

        // Invalid port - should be ignored
        pit.write(0x44, 0xFF);
    }

    #[test]
    fn test_pit_read_counter_all_channels() {
        let mut pit = Pit::new();
        let _v0 = pit.read_counter(0);
        let _v1 = pit.read_counter(1);
        let _v2 = pit.read_counter(2);
        assert_eq!(pit.read_counter(3), 0xFF);
    }

    #[test]
    fn test_read_counter_invalid_channel() {
        let mut pit = Pit::new();
        assert_eq!(pit.read_counter(3), 0xFF);
    }

    // =========================================================================
    // BCD mode tests
    // =========================================================================

    #[test]
    fn test_bcd_to_binary_roundtrip() {
        // 0x1234 BCD = 1234 decimal
        assert_eq!(bcd_to_binary(0x1234), 1234);
        // 0x0000 BCD = 0
        assert_eq!(bcd_to_binary(0x0000), 0);
        // 0x9999 BCD = 9999
        assert_eq!(bcd_to_binary(0x9999), 9999);
    }

    #[test]
    fn test_effective_divisor_bcd_zero() {
        assert_eq!(effective_divisor(0, true), 10000);
    }

    #[test]
    fn test_effective_divisor_binary_zero() {
        assert_eq!(effective_divisor(0, false), 65536);
    }

    #[test]
    fn test_effective_divisor_binary_nonzero_cov() {
        assert_eq!(effective_divisor(100, false), 100);
    }

    #[test]
    fn test_pit_bcd_mode_write_command() {
        let mut pit = Pit::new();
        // Set channel 0, BCD mode (bit 0 = 1)
        pit.write_command(0x31); // Channel 0, LSB/MSB, Mode 0, BCD
        assert!(pit.channels[0].bcd);
    }

    // =========================================================================
    // Access modes: LSB only, MSB only
    // =========================================================================

    #[test]
    fn test_read_counter_lsb_only_cov() {
        let mut pit = Pit::new();
        pit.write_command(0x10); // Channel 0, LSB only, Mode 0
        pit.channels[0].access_mode = 1;
        pit.channels[0].reload = 0x1234;
        pit.channels[0].latch = Some(0x1234);

        let lsb = pit.read_counter(0);
        assert_eq!(lsb, 0x34);
        assert!(pit.channels[0].latch.is_none()); // Consumed
    }

    #[test]
    fn test_read_counter_msb_only_cov() {
        let mut pit = Pit::new();
        pit.channels[0].access_mode = 2;
        pit.channels[0].latch = Some(0x1234);

        let msb = pit.read_counter(0);
        assert_eq!(msb, 0x12);
        assert!(pit.channels[0].latch.is_none());
    }

    // =========================================================================
    // Speaker reload flag
    // =========================================================================

    #[test]
    fn test_speaker_reload_flag() {
        let mut pit = Pit::new();
        assert!(!pit.speaker_reload_updated);

        // Write counter to channel 2 (speaker)
        pit.write_command(0xB6); // ch2, LSB/MSB, mode 3
        pit.write_counter(2, 0x00);
        pit.write_counter(2, 0x10); // Completes the write
        assert!(pit.speaker_reload_updated);
    }

    // =========================================================================
    // get_out accessor
    // =========================================================================

    #[test]
    fn test_get_out_channel_valid() {
        let mut pit = Pit::new();
        pit.channels[0].out = true;
        assert!(pit.get_out(0));

        pit.channels[0].out = false;
        assert!(!pit.get_out(0));
    }

    #[test]
    fn test_get_out_invalid_channel() {
        let pit = Pit::new();
        assert!(!pit.get_out(3));
    }

    // =========================================================================
    // set_ips
    // =========================================================================

    #[test]
    fn test_set_emulated_speed() {
        let mut pit = Pit::new();
        pit.set_emulated_speed(10_000_000);
        assert_eq!(pit.emulated_ips(), 10_000_000);
    }

    // =========================================================================
    // Serialization roundtrip
    // =========================================================================

    // =========================================================================
    // Mode 3: odd-divisor asymmetric square wave (8254 hardware behavior)
    // =========================================================================

    #[test]
    fn test_mode3_odd_divisor_asymmetric_square_wave() {
        // On a real 8254, Mode 3 with odd divisor N produces an asymmetric
        // waveform: HIGH for (N+1)/2 clocks, LOW for (N-1)/2 clocks.
        // Total period is still N clocks.
        let mut pit = Pit::new();

        pit.write_command(0xB6); // Channel 2, LSB/MSB, Mode 3
        pit.write_counter(2, 3); // divisor = 3
        pit.write_counter(2, 0); // HIGH half = 2, LOW half = 1

        // ch.out starts HIGH, so first half is HIGH with length (3+1)/2 = 2
        assert!(pit.channels[2].out, "OUT should start HIGH");

        // Tick 0: HIGH (first half, pos=0 < 2)
        pit.total_cycles = 0;
        assert!(pit.get_out(2), "Tick 0: should be HIGH");

        // Tick 1: HIGH (first half, pos=1 < 2)
        pit.total_cycles = 1;
        assert!(pit.get_out(2), "Tick 1: should be HIGH");

        // Tick 2: LOW (second half, pos=2 >= 2)
        pit.total_cycles = 2;
        assert!(!pit.get_out(2), "Tick 2: should be LOW");

        // Tick 3: next period starts, HIGH again (pos=0)
        pit.total_cycles = 3;
        assert!(pit.get_out(2), "Tick 3: next period, should be HIGH");

        // Tick 4: HIGH (pos=1 < 2)
        pit.total_cycles = 4;
        assert!(pit.get_out(2), "Tick 4: should be HIGH");

        // Tick 5: LOW (pos=2 >= 2)
        pit.total_cycles = 5;
        assert!(!pit.get_out(2), "Tick 5: should be LOW");
    }

    #[test]
    fn test_mode3_odd_divisor_5_asymmetric() {
        // Divisor 5: HIGH for 3 clocks, LOW for 2 clocks.
        let mut pit = Pit::new();

        pit.write_command(0xB6); // Channel 2, LSB/MSB, Mode 3
        pit.write_counter(2, 5);
        pit.write_counter(2, 0);

        // HIGH phase: ticks 0, 1, 2  (3 ticks = (5+1)/2)
        // LOW phase:  ticks 3, 4     (2 ticks = (5-1)/2)
        for tick in 0..3 {
            pit.total_cycles = tick;
            assert!(
                pit.get_out(2),
                "Tick {tick}: should be HIGH (first half of period)"
            );
        }
        for tick in 3..5 {
            pit.total_cycles = tick;
            assert!(
                !pit.get_out(2),
                "Tick {tick}: should be LOW (second half of period)"
            );
        }
        // Next period
        pit.total_cycles = 5;
        assert!(pit.get_out(2), "Tick 5: next period HIGH");
    }

    #[test]
    fn test_mode3_even_divisor_still_symmetric() {
        // Even divisors should remain symmetric (unchanged behavior).
        let mut pit = Pit::new();

        pit.write_command(0xB6); // Channel 2, LSB/MSB, Mode 3
        pit.write_counter(2, 4);
        pit.write_counter(2, 0); // divisor = 4, half = 2

        // HIGH: ticks 0, 1
        // LOW:  ticks 2, 3
        pit.total_cycles = 0;
        assert!(pit.get_out(2), "Tick 0: HIGH");
        pit.total_cycles = 1;
        assert!(pit.get_out(2), "Tick 1: HIGH");
        pit.total_cycles = 2;
        assert!(!pit.get_out(2), "Tick 2: LOW");
        pit.total_cycles = 3;
        assert!(!pit.get_out(2), "Tick 3: LOW");
        pit.total_cycles = 4;
        assert!(pit.get_out(2), "Tick 4: next period HIGH");
    }

    #[test]
    fn test_mode3_divisor_1_odd_edge_case() {
        // Divisor 1: HIGH for (1+1)/2 = 1 clock, LOW for 0 clocks.
        // The waveform is always HIGH (degenerate case).
        let mut pit = Pit::new();

        pit.write_command(0xB6); // Channel 2, LSB/MSB, Mode 3
        pit.write_counter(2, 1);
        pit.write_counter(2, 0);

        pit.total_cycles = 0;
        assert!(pit.get_out(2), "Divisor 1: always HIGH");
        pit.total_cycles = 1;
        assert!(pit.get_out(2), "Divisor 1: always HIGH (next period)");
    }

    #[test]
    fn test_pit_serialize_deserialize() {
        let mut pit = Pit::new();
        pit.channels[0].reload = 0x1234;
        pit.total_cycles = 42;
        pit.ips = 5_000_000;
        pit.event_reschedule_pending = true;

        let json = serde_json::to_string(&pit).unwrap();
        let pit2: Pit = serde_json::from_str(&json).unwrap();

        assert_eq!(pit2.channels[0].reload, 0x1234);
        assert_eq!(pit2.total_cycles, 42);
        assert_eq!(pit2.ips, 5_000_000);
        assert!(pit2.event_reschedule_pending);
    }

    #[test]
    fn test_pit_serialize_deserialize_preserves_live_and_board_state() {
        // A machine-neutral controller has a different board policy from Pit::new:
        // channel 2 reloads are not applied immediately. Keep that zero mask visible
        // so deserialization cannot silently replace it with Pit::new's default.
        let mut pit = Pit::new_unprogrammed(2_000_000, 3_000_000);
        pit.total_cycles = 42;
        pit.event_reschedule_pending = true;
        pit.speaker_reload_updated = true;

        // Capture both a deferred reload and the context needed while an LSB/MSB
        // pair is half written. The latter is not derivable from `reload` alone.
        pit.channels[0].counter = 0x0A0B;
        pit.channels[0].reload = 0x1234;
        pit.channels[0].deferred_reload = Some(0x3456);
        pit.channels[0].write_state = 1;
        pit.channels[0].write_pair_old_reload = 0x2345;

        let json = serde_json::to_value(&pit).unwrap();
        assert_eq!(json["version"], serde_json::json!(2));
        assert_eq!(json["immediate_reload_mask"], serde_json::json!(0));
        assert_eq!(json["speaker_reload_updated"], serde_json::json!(true));
        assert_eq!(
            json["channels"][0]["write_pair_old_reload"],
            serde_json::json!(0x2345)
        );

        // A version-1 snapshot did not carry the fields added above. Its
        // defaults must match the historical deserializer behavior.
        let mut legacy_json = json.clone();
        let legacy = legacy_json.as_object_mut().unwrap();
        legacy.remove("version");
        legacy.remove("speaker_reload_updated");
        legacy.remove("immediate_reload_mask");
        legacy["channels"][0]
            .as_object_mut()
            .unwrap()
            .remove("write_pair_old_reload");
        let legacy_restored: Pit = serde_json::from_value(legacy_json).unwrap();
        assert_eq!(legacy_restored.immediate_reload_mask, 1 << 2);
        assert!(!legacy_restored.speaker_reload_updated);
        assert_eq!(legacy_restored.channels[0].write_pair_old_reload, 0);

        let restored: Pit = serde_json::from_value(json).unwrap();
        assert_eq!(restored.total_cycles, 42);
        assert_eq!(restored.pit_hz, 2_000_000);
        assert_eq!(restored.ips, 3_000_000);
        assert!(restored.event_reschedule_pending);
        assert!(restored.speaker_reload_updated);
        assert_eq!(restored.immediate_reload_mask, 0);
        assert_eq!(restored.channels[0].counter, 0x0A0B);
        assert_eq!(restored.channels[0].reload, 0x1234);
        assert_eq!(restored.channels[0].deferred_reload, Some(0x3456));
        assert_eq!(restored.channels[0].write_state, 1);
        assert_eq!(restored.channels[0].write_pair_old_reload, 0x2345);
    }

    // ---- Tests for Mode 3 counter read fix (HLD 2026.03.29) ----

    /// Helper: latch channel 0 and read the 16-bit counter (LSB/MSB).
    fn latch_and_read(pit: &mut Pit) -> u16 {
        pit.write_command(0x00); // latch channel 0
        let lsb = pit.read_counter(0) as u16;
        let msb = pit.read_counter(0) as u16;
        (msb << 8) | lsb
    }

    /// Helper: set up channel 0 as Mode 3 with given reload and IPS.
    fn setup_mode3(pit: &mut Pit, reload: u16, ips: u64) {
        pit.set_emulated_speed(ips);
        pit.write_command(0x36); // Channel 0, LSB/MSB, Mode 3
        pit.write_counter(0, reload as u8);
        pit.write_counter(0, (reload >> 8) as u8);
        pit.channels[0].base_cycles = pit.total_cycles;
    }

    #[test]
    fn test_mode3_counter_always_even() {
        let mut pit = Pit::new();
        setup_mode3(&mut pit, 0, 25_200_000); // divisor 65536
        for offset in [1u64, 37, 100, 503, 1000, 5000, 12345] {
            pit.total_cycles = offset;
            let counter = latch_and_read(&mut pit);
            assert_eq!(
                counter & 1,
                0,
                "counter {counter} not even at cycle {offset}"
            );
        }
    }

    #[test]
    fn test_mode3_latch_monotonic_within_period() {
        let mut pit = Pit::new();
        setup_mode3(&mut pit, 0, 25_200_000); // divisor 65536
        pit.total_cycles = 0;
        let mut prev = latch_and_read(&mut pit);
        for i in 1..=10 {
            pit.total_cycles = i * 100;
            let cur = latch_and_read(&mut pit);
            assert!(cur <= prev, "counter went up: {prev} -> {cur} at step {i}");
            prev = cur;
        }
    }

    #[test]
    fn test_mode3_period_boundary_positive_delta() {
        let mut pit = Pit::new();
        let divisor = 1000u64;
        setup_mode3(&mut pit, divisor as u16, 1_193_182); // 1:1 PIT
        pit.total_cycles = 0;
        let start = latch_and_read(&mut pit);
        // At the third half-period boundary, Mode 3 reloads the count while
        // OUT enters LOW. The next clock decrements the reloaded count by two.
        pit.total_cycles = 1500;
        let boundary = latch_and_read(&mut pit);
        assert_eq!(boundary, divisor as u16);
        assert!(!pit.get_out(0));

        pit.total_cycles = 1501;
        let end = latch_and_read(&mut pit);
        // One clock after the reload boundary, the counter has decremented by
        // two and remains in the LOW half.
        assert_eq!(end, (divisor - 2) as u16);
        assert!(!pit.get_out(0));
        let delta = start.wrapping_sub(end);
        assert!(
            delta == 2,
            "bad delta: start={start} end={end} delta={delta}"
        );
    }

    #[test]
    fn test_mode3_various_ips() {
        for ips in [25_200_000u64, 3_600_000, 1_193_182] {
            let mut pit = Pit::new();
            setup_mode3(&mut pit, 0, ips); // divisor 65536
                                           // Advance by ~100 PIT ticks worth of CPU cycles
            pit.total_cycles = 100 * ips / 1_193_182;
            let counter = latch_and_read(&mut pit);
            assert_eq!(counter & 1, 0, "counter {counter} not even at ips={ips}");
            assert!(
                counter <= 65534,
                "counter {counter} out of range at ips={ips}"
            );
        }
    }

    #[test]
    fn test_mode3_divisor_1_no_panic() {
        let mut pit = Pit::new();
        setup_mode3(&mut pit, 1, 25_200_000); // effective divisor=1
        pit.total_cycles = 100;
        let _counter = latch_and_read(&mut pit); // must not panic
    }

    /// Mode 3 decrements an even count by two per clock. Latching must keep
    /// the low and high bytes coherent across intervening elapsed time and
    /// must continue to report that count after multiple complete periods.
    #[test]
    fn channel0_mode3_latches_remain_coherent_across_periods() {
        const DIVISOR: u16 = 512;
        const STEP: u64 = 17;
        let mut pit = Pit::new();
        setup_mode3(&mut pit, DIVISOR, PIT_FREQUENCY as u64);
        for period in [0, 1, 4] {
            pit.total_cycles = period * u64::from(DIVISOR);
            let start = latch_and_read(&mut pit);
            pit.total_cycles += STEP;
            pit.write_command(0x00);
            let low = pit.read_counter(0);
            pit.total_cycles += STEP;
            let high = pit.read_counter(0);
            assert_eq!(
                u16::from_le_bytes([low, high]),
                start.wrapping_sub(2 * STEP as u16)
            );
            assert_eq!(
                latch_and_read(&mut pit),
                start.wrapping_sub(4 * STEP as u16)
            );
        }
    }
}

#[cfg(test)]
mod mode3_full_period_counter_tests {
    use super::*;

    fn established(reload: u16, bcd: bool) -> Pit {
        let mut pit = Pit::new();
        pit.write_command(0x36 | u8::from(bcd));
        for byte in reload.to_le_bytes() {
            pit.write_counter(0, byte);
        }
        // Begin at an established HIGH reload; initial CR-to-CE delay is
        // outside these steady-state counter tests.
        pit.channels[0].base_cycles = pit.total_cycles;
        pit
    }

    // Intel 8254, September 1993, Mode 3, pp. 13-14:
    // odd counts load N-1 and hold zero for one extra HIGH clock before
    // reloading LOW. These are hand-derived complete periods, not an
    // elapsed-time formula shared with the implementation.
    const ODD_PERIODS: &[(u16, &[(u16, bool)])] = &[
        (3, &[(2, true), (0, true), (2, false)]),
        (
            5,
            &[(4, true), (2, true), (0, true), (4, false), (2, false)],
        ),
        (
            7,
            &[
                (6, true),
                (4, true),
                (2, true),
                (0, true),
                (6, false),
                (4, false),
                (2, false),
            ],
        ),
    ];

    const EVEN_PERIODS: &[(u16, &[(u16, bool)])] = &[
        (4, &[(4, true), (2, true), (4, false), (2, false)]),
        (
            6,
            &[
                (6, true),
                (4, true),
                (2, true),
                (6, false),
                (4, false),
                (2, false),
            ],
        ),
    ];

    fn check_direct(reload: u16, bcd: bool, samples: &[(u16, bool)]) {
        let mut pit = established(reload, bcd);
        for period in [1, 2, 5] {
            for (tick, &(count, out)) in samples.iter().enumerate() {
                pit.total_cycles = period * samples.len() as u64 + tick as u64;
                assert_eq!(
                    (pit.compute_counter(0), pit.compute_out(0)),
                    (count, out),
                    "reload {reload:#06x}, period {period}, tick {tick}"
                );
            }
        }
    }

    #[test]
    fn odd_direct_counts_follow_complete_periods() {
        for &(reload, samples) in ODD_PERIODS.iter().chain(EVEN_PERIODS) {
            check_direct(reload, false, samples);
        }
        let mut pit = established(0, false);
        for (tick, count, out) in [
            (65536, 0, true),
            (65537, 65534, true),
            (98303, 2, true),
            (98304, 0, false),
            (131071, 2, false),
            (131072, 0, true),
        ] {
            pit.total_cycles = tick;
            assert_eq!((pit.compute_counter(0), pit.compute_out(0)), (count, out));
        }
    }

    #[test]
    fn odd_automatic_word_latches_hold_across_clock_edges() {
        for &(reload, samples) in ODD_PERIODS.iter().chain(EVEN_PERIODS) {
            let mut pit = established(reload, false);
            for period in [1, 2, 5] {
                for (tick, &(count, out)) in samples.iter().enumerate() {
                    pit.total_cycles = period * samples.len() as u64 + tick as u64;
                    assert_eq!(pit.compute_out(0), out);
                    let low = pit.read_counter(0);
                    pit.total_cycles += 1;
                    let high = pit.read_counter(0);
                    assert_eq!(
                        u16::from_le_bytes([low, high]),
                        count,
                        "reload {reload}, period {period}, tick {tick}"
                    );
                }
            }
        }
    }

    #[test]
    fn odd_bcd_counts_hold_high_zero() {
        // Decimal 11 crosses a decimal borrow while counting down by two.
        check_direct(
            0x0011,
            true,
            &[
                (0x10, true),
                (8, true),
                (6, true),
                (4, true),
                (2, true),
                (0, true),
                (0x10, false),
                (8, false),
                (6, false),
                (4, false),
                (2, false),
            ],
        );
    }

    #[test]
    fn odd_phase_survives_speed_change_period_reduction() {
        let mut pit = established(7, false);
        pit.total_cycles = 19; // Third period, second LOW clock: CE=4.
        pit.set_emulated_speed(u64::from(PIT_FREQUENCY) * 2);
        assert_eq!((pit.compute_counter(0), pit.compute_out(0)), (4, false));
        pit.total_cycles += 2;
        assert_eq!((pit.compute_counter(0), pit.compute_out(0)), (2, false));
        pit.total_cycles += 2;
        assert_eq!((pit.compute_counter(0), pit.compute_out(0)), (6, true));
    }

    #[test]
    fn odd_phase_survives_oscillator_change_period_reduction() {
        let mut pit = established(7, false);
        pit.set_pit_hz(PIT_FREQUENCY * 2);
        pit.set_emulated_speed(u64::from(PIT_FREQUENCY) * 2);
        pit.total_cycles = 19;
        pit.set_pit_hz(PIT_FREQUENCY);
        assert_eq!((pit.compute_counter(0), pit.compute_out(0)), (4, false));
        pit.total_cycles += 4;
        assert_eq!((pit.compute_counter(0), pit.compute_out(0)), (6, true));
        // The event-driven channel 0 boundary restarts an established HIGH.
        pit.track_time(0, pit.total_cycles);
        assert_eq!((pit.compute_counter(0), pit.compute_out(0)), (6, true));
    }
}

#[cfg(test)]
mod triggered_count_only_tests {
    use super::*;

    fn write_count(pit: &mut Pit, access: u8, count: u16) {
        match access {
            1 => pit.write_counter(2, count as u8),
            2 => pit.write_counter(2, (count >> 8) as u8),
            3 => {
                pit.write_counter(2, count as u8);
                pit.write_counter(2, (count >> 8) as u8);
            }
            _ => unreachable!(),
        }
    }

    fn armed(mode: u8, access: u8, count: u16) -> Pit {
        let mut pit = Pit::new();
        pit.write_command(0x80 | (access << 4) | (mode << 1));
        write_count(&mut pit, access, count);
        pit
    }

    fn trigger(pit: &mut Pit) {
        pit.set_gate(2, false);
        pit.set_gate(2, true);
    }

    fn active_shot(mode: u8, access: u8) {
        let (original, replacement) = if access == 2 { (512, 1024) } else { (40, 90) };
        let mut pit = armed(mode, access, original);
        trigger(&mut pit);
        pit.total_cycles = u64::from(original / 2);
        pit.event_reschedule_pending = false;
        pit.speaker_reload_updated = false;
        write_count(&mut pit, access, replacement);
        pit.total_cycles = u64::from(original);
        assert_eq!(
            pit.get_out(2),
            mode == 1,
            "original terminal edge must survive count write"
        );
        assert_eq!(pit.compute_counter(2), 0);
        assert_eq!(pit.get_divisor(2), original);
        assert!(!pit.event_reschedule_pending);
        assert!(!pit.speaker_reload_updated);
        pit.total_cycles += 1;
        assert!(pit.get_out(2));
        trigger(&mut pit);
        assert_eq!(pit.get_divisor(2), replacement);
        assert_eq!(pit.get_deferred_reload(2), None);
        let start = pit.total_cycles;
        pit.total_cycles = start + u64::from(replacement) - 1;
        assert_eq!(pit.get_out(2), mode == 5);
        pit.total_cycles += 1;
        assert_eq!(pit.get_out(2), mode == 1);
    }

    #[test]
    fn triggered_count_only_mode1_lsb() {
        active_shot(1, 1);
    }
    #[test]
    fn triggered_count_only_mode1_msb() {
        active_shot(1, 2);
    }
    #[test]
    fn triggered_count_only_mode1_pair() {
        active_shot(1, 3);
    }
    #[test]
    fn triggered_count_only_mode5_lsb() {
        active_shot(5, 1);
    }
    #[test]
    fn triggered_count_only_mode5_msb() {
        active_shot(5, 2);
    }
    #[test]
    fn triggered_count_only_mode5_pair() {
        active_shot(5, 3);
    }

    fn completed_shot(mode: u8) {
        let mut pit = armed(mode, 3, 40);
        trigger(&mut pit);
        pit.total_cycles = 45;
        let before = pit.compute_counter(2);
        write_count(&mut pit, 3, 90);
        assert_eq!(
            pit.compute_counter(2),
            before,
            "completed shot must keep its counting phase"
        );
        assert!(pit.get_out(2));
        pit.total_cycles += 90;
        assert!(pit.get_out(2), "write must not create another strobe");
        // Event-driven completion marks terminal count. A replacement must
        // preserve that state too, until a fresh trigger clears it.
        pit.channels[2].terminal_count_reached = true;
        let completed_counter = pit.compute_counter(2);
        write_count(&mut pit, 3, 110);
        assert!(pit.channels[2].terminal_count_reached);
        assert_eq!(pit.compute_counter(2), completed_counter);
        trigger(&mut pit);
        assert!(!pit.channels[2].terminal_count_reached);
        assert_eq!(pit.get_divisor(2), 110, "latest complete count wins");
    }
    #[test]
    fn triggered_count_only_mode1_after_completion() {
        completed_shot(1);
    }
    #[test]
    fn triggered_count_only_mode5_after_completion() {
        completed_shot(5);
    }

    #[test]
    fn triggered_count_only_snapshot_round_trips() {
        for mode in [1, 5] {
            let mut pit = armed(mode, 3, 40);
            trigger(&mut pit);
            pit.total_cycles = 20;
            write_count(&mut pit, 3, 90);
            let json = serde_json::to_string(&pit).unwrap();
            let mut serialized: Pit = serde_json::from_str(&json).unwrap();
            let mut checkpoint = Pit::new();
            checkpoint.restore_channel_state(2, &pit.save_channel_state(2));
            let (hz, cycles, ips) = pit.save_controller_state();
            checkpoint.restore_controller_state(hz, cycles, ips);
            for restored in [&mut serialized, &mut checkpoint] {
                restored.total_cycles = 40;
                assert_eq!(
                    restored.get_out(2),
                    mode == 1,
                    "restored active shot keeps original edge"
                );
                assert_eq!(
                    restored.diagnostic_snapshot().channels[2].deferred_reload,
                    Some(90)
                );
                trigger(restored);
                assert_eq!(restored.get_divisor(2), 90);
            }
        }
    }

    #[test]
    fn triggered_count_only_trigger_between_pair_bytes() {
        for mode in [1, 5] {
            let mut pit = armed(mode, 3, 40);
            trigger(&mut pit);
            pit.total_cycles = 20;
            write_count(&mut pit, 3, 90);
            pit.write_counter(2, 0x34);
            // Both checkpoint formats must retain the completed replacement and
            // the independent incomplete LSB while a trigger consumes the former.
            let mut serialized: Pit =
                serde_json::from_str(&serde_json::to_string(&pit).unwrap()).unwrap();
            let mut checkpoint = Pit::new();
            checkpoint.restore_channel_state(2, &pit.save_channel_state(2));
            let (hz, cycles, ips) = pit.save_controller_state();
            checkpoint.restore_controller_state(hz, cycles, ips);
            for restored in [&mut serialized, &mut checkpoint] {
                trigger(restored);
                assert_eq!(
                    restored.get_divisor(2),
                    90,
                    "trigger must use last complete count"
                );
                restored.write_counter(2, 0x01);
                assert_eq!(
                    restored.get_divisor(2),
                    90,
                    "MSB must not restart current shot"
                );
                assert_eq!(restored.get_deferred_reload(2), Some(0x0134));
                restored.total_cycles = 110;
                assert_eq!(restored.get_out(2), mode == 1);
                trigger(restored);
                assert_eq!(
                    restored.get_divisor(2),
                    0x0134,
                    "partial LSB must survive trigger"
                );
            }
        }
    }

    #[test]
    fn triggered_count_only_waits_for_trigger_before_first_shot_and_boundary() {
        for mode in [1, 5] {
            let mut pit = armed(mode, 3, 40);
            write_count(&mut pit, 3, 90);
            pit.apply_deferred_reload(2);
            assert_eq!(
                pit.get_divisor(2),
                40,
                "cycle boundary must not transfer triggered count"
            );
            assert_eq!(pit.get_deferred_reload(2), Some(90));
            assert!(!pit.get_counting(2));
            trigger(&mut pit);
            assert_eq!(pit.get_divisor(2), 90);
            write_count(&mut pit, 3, 110);
            pit.write_command(0xb4); // A new mode cancels the triggered replacement.
            assert_eq!(pit.get_deferred_reload(2), None);
        }
    }
}

#[cfg(test)]
mod mode0_first_byte_tests {
    use super::*;

    fn loaded(bcd: bool) -> Pit {
        let mut pit = Pit::new();
        pit.write_command(0xb0 | u8::from(bcd));
        pit.write_counter(2, if bcd { 0x40 } else { 40 });
        pit.write_counter(2, 0);
        pit.set_gate(2, true);
        pit
    }

    #[test]
    fn mode0_first_byte_active_count_freezes() {
        for bcd in [false, true] {
            let mut pit = loaded(bcd);
            pit.total_cycles = 20;
            let frozen = pit.compute_counter(2);
            pit.write_counter(2, if bcd { 0x90 } else { 90 });
            assert!(
                !pit.get_counting(2),
                "first byte must disable the counting element"
            );
            assert!(!pit.get_out(2));
            for cycle in [40, 100, 100_000] {
                pit.total_cycles = cycle;
                assert_eq!(
                    pit.compute_counter(2),
                    frozen,
                    "incomplete pair keeps the old count frozen"
                );
                assert!(!pit.get_out(2));
            }
            pit.write_counter(2, 0);
            assert!(pit.get_counting(2));
            assert_eq!(pit.compute_counter(2), if bcd { 0x90 } else { 90 });
            pit.total_cycles += 89;
            assert!(!pit.get_out(2));
            pit.total_cycles += 1;
            assert!(pit.get_out(2));
        }
        // Other software-counted modes keep their active sequence on an
        // incomplete pair. Compare with an independently programmed control.
        for mode in [2, 3, 4] {
            let mut actual = Pit::new();
            let mut expected = Pit::new();
            for pit in [&mut actual, &mut expected] {
                pit.write_command(0xb0 | (mode << 1));
                pit.write_counter(2, 40);
                pit.write_counter(2, 0);
                pit.set_gate(2, true);
                pit.total_cycles = 20;
            }
            actual.write_counter(2, 90);
            for cycle in [20, 39, 40, 41] {
                actual.total_cycles = cycle;
                expected.total_cycles = cycle;
                assert!(actual.get_counting(2), "mode {mode} must keep counting");
                assert_eq!(
                    actual.compute_counter(2),
                    expected.compute_counter(2),
                    "mode {mode} count"
                );
                assert_eq!(actual.get_out(2), expected.get_out(2), "mode {mode} OUT");
            }
        }
        // A one-byte Mode 0 write completes and starts a load immediately.
        for access in [1, 2] {
            let mut pit = Pit::new();
            pit.write_command(0x80 | (access << 4));
            pit.write_counter(2, 40);
            pit.set_gate(2, true);
            pit.total_cycles = 20;
            pit.write_counter(2, 90);
            assert!(pit.get_counting(2));
            assert_eq!(
                pit.compute_counter(2),
                if access == 1 { 90 } else { 90 << 8 }
            );
            assert!(!pit.get_out(2));
        }
    }

    #[test]
    fn mode0_first_byte_terminal_out_drops() {
        for bcd in [false, true] {
            let mut pit = loaded(bcd);
            pit.total_cycles = 44;
            assert!(pit.get_out(2));
            let frozen = pit.compute_counter(2);
            pit.write_counter(2, if bcd { 0x90 } else { 90 });
            assert!(
                !pit.get_out(2),
                "first byte must lower OUT after terminal count"
            );
            pit.total_cycles = 100_000;
            assert_eq!(pit.compute_counter(2), frozen);
            assert!(!pit.get_out(2));
            pit.write_counter(2, 0);
            assert!(pit.get_counting(2));
            assert!(!pit.get_out(2));
        }
    }

    #[test]
    fn mode0_first_byte_gate_cannot_resume_half_write() {
        let mut pit = loaded(false);
        pit.total_cycles = 20;
        pit.write_counter(2, 90);
        pit.set_gate(2, false);
        pit.set_gate(2, true);
        assert!(
            !pit.get_counting(2),
            "GATE cannot resume an incomplete paired count write"
        );
        pit.total_cycles = 100_000;
        assert!(!pit.get_out(2));
        pit.write_counter(2, 0);
        assert!(pit.get_counting(2));
        assert_eq!(pit.compute_counter(2), 90);
    }
}

#[cfg(test)]
mod mode_alias_tests {
    use super::*;

    fn programmed(channel: u8, mode: u8, access: u8, bcd: bool) -> Pit {
        let mut pit = Pit::new_unprogrammed(PIT_FREQUENCY, u64::from(PIT_FREQUENCY));
        pit.write_command((channel << 6) | (access << 4) | (mode << 1) | u8::from(bcd));
        write_count(
            &mut pit,
            channel,
            access,
            if access == 2 { 0x0200 } else { 12 },
        );
        pit
    }

    fn write_count(pit: &mut Pit, channel: u8, access: u8, count: u16) {
        if access != 2 {
            pit.write_counter(channel, count as u8);
        }
        if access != 1 {
            pit.write_counter(channel, (count >> 8) as u8);
        }
    }

    fn status(pit: &mut Pit, channel: u8) -> u8 {
        pit.write_command(0xE0 | (1 << (channel + 1)));
        pit.read_counter(channel)
    }

    #[test]
    fn alias_status_preserves_control_bits_all_channels_access_and_bcd() {
        for channel in 0..3 {
            for mode in [2, 3, 6, 7] {
                for access in 1..=3 {
                    for bcd in [false, true] {
                        let mut pit = programmed(channel, mode, access, bcd);
                        assert_eq!(
                            status(&mut pit, channel) & 0x3F,
                            (access << 4) | (mode << 1) | u8::from(bcd),
                            "last control word: channel {channel}, mode {mode}, access {access}, BCD {bcd}"
                        );
                        assert_eq!(
                            pit.get_mode(channel),
                            if mode >= 6 { mode - 4 } else { mode }
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn alias_latches_preserve_encoding_until_status_consumed() {
        for channel in 0..3 {
            for mode in [6, 7] {
                let mut pit = programmed(channel, mode, 3, false);
                // Mode/BCD bits in a count latch command are ignored.
                pit.write_command((channel << 6) | 3);
                pit.read_counter(channel);
                pit.read_counter(channel);
                // Read-back count latching also leaves the control encoding intact.
                pit.write_command(0xD0 | (1 << (channel + 1)));
                pit.read_counter(channel);
                pit.read_counter(channel);
                assert_eq!(status(&mut pit, channel) & 0x3F, 0x30 | (mode << 1));
                pit.write_command(0xE0 | (1 << (channel + 1)));
                pit.write_command((channel << 6) | 0x34);
                // A second readback cannot replace an unread status latch.
                pit.write_command(0xE0 | (1 << (channel + 1)));
                assert_eq!(pit.read_counter(channel) & 0x3F, 0x30 | (mode << 1));
                assert_eq!(status(&mut pit, channel) & 0x3F, 0x34);
            }
        }
    }

    #[test]
    fn alias_execution_matches_normalized_modes_across_cycles_and_gates() {
        for channel in 0..3 {
            for (mode, alias) in [(2, 6), (3, 7)] {
                for bcd in [false, true] {
                    let mut normal = programmed(channel, mode, 3, bcd);
                    let mut encoded = programmed(channel, alias, 3, bcd);
                    assert_eq!(status(&mut encoded, channel) & 0x0E, alias << 1);
                    for cycle in 0..80 {
                        normal.total_cycles = cycle;
                        encoded.total_cycles = cycle;
                        if cycle == 15 || cycle == 21 {
                            for pit in [&mut normal, &mut encoded] {
                                pit.set_gate(channel, cycle == 21);
                            }
                        }
                        if cycle == 31 {
                            for pit in [&mut normal, &mut encoded] {
                                write_count(pit, channel, 3, 8);
                            }
                        }
                        // Model the same scheduler boundary for both encodings.
                        if cycle == 36 {
                            for pit in [&mut normal, &mut encoded] {
                                pit.apply_deferred_reload(usize::from(channel));
                                pit.track_time(usize::from(channel), cycle);
                            }
                        }
                        assert_eq!(
                            encoded.diagnostic_snapshot(),
                            normal.diagnostic_snapshot(),
                            "mode {alias}/{mode}, channel {channel}, BCD {bcd}, cycle {cycle}"
                        );
                        assert_eq!(
                            encoded.channel0_period_cycles_ratio(1000),
                            normal.channel0_period_cycles_ratio(1000)
                        );
                        assert_eq!(
                            encoded.channel0_is_schedulable(),
                            normal.channel0_is_schedulable()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn alias_json_and_channel_checkpoint_preserve_live_and_latched_encoding() {
        for channel in 0..3 {
            for alias in [6, 7] {
                let mut pit = programmed(channel, alias, 3, false);
                pit.write_command(0xE0 | (1 << (channel + 1)));
                pit.write_command((channel << 6) | 0x34);
                let mut json: Pit =
                    serde_json::from_value(serde_json::to_value(&pit).unwrap()).unwrap();
                let mut checkpoint = Pit::new();
                checkpoint.restore_channel_state(
                    usize::from(channel),
                    &pit.save_channel_state(usize::from(channel)),
                );
                for restored in [&mut json, &mut checkpoint] {
                    assert_eq!(restored.read_counter(channel) & 0x3F, 0x30 | (alias << 1));
                    assert_eq!(status(restored, channel) & 0x3F, 0x34);
                }
                let source = programmed(channel, alias, 3, true);
                let mut json: Pit =
                    serde_json::from_value(serde_json::to_value(&source).unwrap()).unwrap();
                checkpoint.restore_channel_state(
                    usize::from(channel),
                    &source.save_channel_state(usize::from(channel)),
                );
                for restored in [&mut json, &mut checkpoint] {
                    assert_eq!(status(restored, channel) & 0x3F, 0x31 | (alias << 1));
                    assert_eq!(restored.get_mode(channel), alias - 4);
                }
            }
        }
    }

    #[test]
    fn legacy_missing_raw_mode_falls_back_to_normalized_mode() {
        for mode in [2, 3] {
            for channel in 0..3 {
                let source = programmed(channel, mode, 3, false);
                let mut value = serde_json::to_value(&source).unwrap();
                for saved in value["channels"].as_array_mut().unwrap() {
                    saved.as_object_mut().unwrap().remove("raw_mode");
                }
                let mut legacy_state = source.save_channel_state(usize::from(channel));
                legacy_state.raw_mode = None;
                let mut checkpoint = Pit::new();
                checkpoint.restore_channel_state(usize::from(channel), &legacy_state);
                let checkpoint_status = status(&mut checkpoint, channel) & 0x3F;
                assert_eq!(checkpoint.get_mode(channel), mode);
                assert_eq!(
                    checkpoint.compute_counter(usize::from(channel)),
                    source.compute_counter(usize::from(channel))
                );
                assert_eq!(checkpoint.get_out(channel), source.get_out(channel));
                let mut restored: Pit = serde_json::from_value(value).unwrap();
                assert_eq!(
                    (checkpoint_status, status(&mut restored, channel) & 0x3F),
                    (0x30 | (mode << 1), 0x30 | (mode << 1)),
                    "legacy checkpoint and JSON must both retain normalized mode {mode}"
                );
                assert_eq!(restored.get_mode(channel), mode);
                assert_eq!(
                    restored.compute_counter(usize::from(channel)),
                    source.compute_counter(usize::from(channel))
                );
                assert_eq!(restored.get_out(channel), source.get_out(channel));
            }
        }
    }
}

#[cfg(test)]
mod null_count_regressions {
    use super::*;

    fn status(pit: &mut Pit, channel: u8) -> u8 {
        pit.write_command(0xE0 | (1 << (channel + 1)));
        pit.read_counter(channel) & 0x40
    }

    fn write(pit: &mut Pit, channel: u8, access: u8, count: u16) {
        if access != 2 {
            pit.write_counter(channel, count as u8);
        }
        if access != 1 {
            pit.write_counter(channel, (count >> 8) as u8);
        }
    }

    fn programmed(channel: u8, mode: u8, access: u8) -> Pit {
        let mut pit = Pit::new_unprogrammed(PIT_FREQUENCY, PIT_FREQUENCY as u64);
        pit.write_command((channel << 6) | (access << 4) | (mode << 1));
        write(&mut pit, channel, access, 1000);
        pit
    }

    #[test]
    fn completed_deferred_write_keeps_null_until_boundary() {
        let mut immediate = Pit::new();
        assert_eq!(
            status(&mut immediate, 0),
            0,
            "BIOS bootstrap count has transferred"
        );
        for mode in [0, 4] {
            immediate.write_command(0x30 | (mode << 1));
            assert_eq!(
                status(&mut immediate, 0),
                0x40,
                "mode control must set NULL COUNT"
            );
            immediate.write_counter(0, 100);
            assert_eq!(status(&mut immediate, 0), 0x40);
            immediate.write_counter(0, 0);
            assert_eq!(
                status(&mut immediate, 0),
                0,
                "modeled immediate load must clear NULL COUNT"
            );
            immediate.write_counter(0, 50);
            assert_eq!(
                status(&mut immediate, 0),
                0,
                "Mode 0 first byte stops CE but must retain clear NULL COUNT"
            );
            immediate.write_counter(0, 0);
        }
        for channel in 0..3 {
            for mode in [2, 3, 6, 7] {
                for access in 1..=3 {
                    let mut pit = programmed(channel, mode, access);
                    assert_eq!(status(&mut pit, channel), 0);
                    pit.total_cycles = 5;
                    let old_reload = pit.channels[channel as usize].reload;
                    let old_count = pit.compute_counter(channel as usize);
                    if access == 3 {
                        pit.write_counter(channel, 0xF4);
                        assert_eq!(
                            status(&mut pit, channel),
                            0,
                            "first byte must retain NULL COUNT"
                        );
                        pit.write_counter(channel, 1);
                    } else {
                        write(&mut pit, channel, access, 500);
                    }
                    assert_eq!(
                        status(&mut pit, channel),
                        0x40,
                        "completed deferred CR write must set NULL COUNT"
                    );
                    assert_eq!(pit.channels[channel as usize].reload, old_reload);
                    assert_eq!(pit.compute_counter(channel as usize), old_count);
                    assert!(pit.get_counting(channel));
                    let other = (channel + 1) % 3;
                    pit.write_command((other << 6) | 0x30);
                    assert_eq!(
                        status(&mut pit, channel),
                        0x40,
                        "another channel mode word must preserve pending NULL COUNT"
                    );
                    pit.write_command(channel << 6);
                    // Consume both latched count bytes under the programmed format.
                    pit.read_counter(channel);
                    if access == 3 {
                        pit.read_counter(channel);
                    }
                    assert_eq!(
                        status(&mut pit, channel),
                        0x40,
                        "count latch must preserve NULL COUNT"
                    );
                    pit.write_command(0xE0 | (1 << (channel + 1)));
                    pit.apply_deferred_reload(channel as usize);
                    assert_eq!(
                        pit.read_counter(channel) & 0x40,
                        0x40,
                        "latched pending status must survive transfer until read"
                    );
                    assert_eq!(
                        status(&mut pit, channel),
                        0,
                        "boundary transfer must clear NULL COUNT"
                    );
                }
            }
        }
    }

    #[test]
    fn gate_transfers_initial_and_replacement_counts() {
        for mode in [1, 5] {
            let mut unfilled = Pit::new_unprogrammed(PIT_FREQUENCY, PIT_FREQUENCY as u64);
            unfilled.write_command(0xB0 | (mode << 1));
            unfilled.set_gate(2, false);
            unfilled.set_gate(2, true);
            assert_eq!(
                status(&mut unfilled, 2),
                0x40,
                "GATE without a completed CR must retain NULL COUNT"
            );
            unfilled.write_counter(2, 0xE8);
            assert_eq!(
                status(&mut unfilled, 2),
                0x40,
                "initial first byte must retain NULL COUNT"
            );
            let mut pit = programmed(2, mode, 3);
            assert_eq!(
                status(&mut pit, 2),
                0x40,
                "initial CR waits for GATE transfer"
            );
            pit.set_gate(2, false);
            pit.set_gate(2, true);
            assert_eq!(status(&mut pit, 2), 0);
            pit.total_cycles = 5;
            let old_count = pit.compute_counter(2);
            write(&mut pit, 2, 3, 500);
            assert_eq!(status(&mut pit, 2), 0x40);
            assert_eq!(pit.compute_counter(2), old_count);
            pit.set_gate(2, false);
            pit.set_gate(2, true);
            assert_eq!(status(&mut pit, 2), 0);
            assert_eq!(pit.compute_counter(2), 500);
        }
        for mode in [2, 3] {
            let mut pit = programmed(2, mode, 3);
            write(&mut pit, 2, 3, 500);
            pit.set_gate(2, false);
            pit.set_gate(2, true);
            assert_eq!(status(&mut pit, 2), 0);
            assert_eq!(
                pit.channels[2].reload, 500,
                "GATE must transfer completed pending CR"
            );
        }
    }

    #[test]
    fn pending_transfer_during_new_half_write_retains_new_lsb() {
        for mode in [1, 2, 3, 5] {
            let mut pit = programmed(2, mode, 3);
            pit.set_gate(2, false);
            pit.set_gate(2, true);
            write(&mut pit, 2, 3, 500);
            pit.write_counter(2, 0x34);
            assert_eq!(
                status(&mut pit, 2),
                0x40,
                "first byte must retain pending NULL COUNT"
            );
            if mode == 1 || mode == 5 {
                pit.set_gate(2, false);
                pit.set_gate(2, true);
            } else {
                pit.apply_deferred_reload(2);
            }
            assert_eq!(
                status(&mut pit, 2),
                0,
                "older complete CR transfer clears NULL COUNT even during newer half write"
            );
            assert_eq!(pit.channels[2].active_reload(), 500);
            pit.write_counter(2, 0x01);
            assert_eq!(pit.get_deferred_reload(2), Some(0x0134));
            assert_eq!(status(&mut pit, 2), 0x40);
        }
    }

    #[test]
    fn pending_null_count_survives_json_and_checkpoint() {
        for mode in [1, 2, 3, 5] {
            let mut source = programmed(0, mode, 3);
            source.set_gate(0, false);
            source.set_gate(0, true);
            write(&mut source, 0, 3, 500);
            source.write_counter(0, 0x34);
            let value = serde_json::to_value(&source).unwrap();
            assert_eq!(
                value["channels"][0]["null_count"],
                serde_json::json!(true),
                "modern JSON must persist the distinct flag"
            );
            let saved = source.save_channel_state(0);
            assert_eq!(
                saved.null_count,
                Some(true),
                "modern checkpoint must persist the distinct flag"
            );
            let mut json: Pit = serde_json::from_value(value).unwrap();
            let mut checkpoint = Pit::new();
            checkpoint.restore_channel_state(0, &saved);
            for restored in [&mut json, &mut checkpoint] {
                assert_eq!(
                    status(restored, 0),
                    0x40,
                    "restored pending CR must retain NULL COUNT"
                );
                if mode == 1 || mode == 5 {
                    restored.set_gate(0, false);
                    restored.set_gate(0, true);
                } else {
                    restored.apply_deferred_reload(0);
                }
                assert_eq!(status(restored, 0), 0);
                restored.write_counter(0, 1);
                assert_eq!(restored.get_deferred_reload(0), Some(0x0134));
                assert_eq!(status(restored, 0), 0x40);
            }
        }
    }

    #[test]
    fn legacy_json_recovers_pending_transfer_flag() {
        for loaded in [false, true] {
            let source = if loaded {
                programmed(0, 2, 3)
            } else {
                Pit::new_unprogrammed(PIT_FREQUENCY, PIT_FREQUENCY as u64)
            };
            let mut value = serde_json::to_value(&source).unwrap();
            value["channels"][0]
                .as_object_mut()
                .unwrap()
                .remove("null_count");
            let mut json: Pit = serde_json::from_value(value).unwrap();
            let mut state = source.save_channel_state(0);
            state.null_count = None;
            let mut checkpoint = Pit::new();
            checkpoint.restore_channel_state(0, &state);
            for restored in [&mut json, &mut checkpoint] {
                assert_eq!(status(restored, 0), if loaded { 0 } else { 0x40 });
            }
        }
        for mode in [1, 2, 3, 5] {
            let mut source = programmed(0, mode, 3);
            if mode == 2 || mode == 3 {
                write(&mut source, 0, 3, 500);
            }
            let mut json = serde_json::to_value(&source).unwrap();
            for channel in json["channels"].as_array_mut().unwrap() {
                channel.as_object_mut().unwrap().remove("null_count");
            }
            let mut restored: Pit = serde_json::from_value(json).unwrap();
            assert_eq!(
                status(&mut restored, 0),
                0x40,
                "legacy pending CR must reconstruct NULL COUNT"
            );
            let mut state = source.save_channel_state(0);
            state.null_count = None;
            let mut checkpoint = Pit::new();
            checkpoint.restore_channel_state(0, &state);
            assert_eq!(
                status(&mut checkpoint, 0),
                0x40,
                "legacy checkpoint must reconstruct pending NULL COUNT"
            );
        }
    }
}
