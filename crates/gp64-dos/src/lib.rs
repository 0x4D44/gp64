//! DOS API emulation (INT 21h, INT 25h/26h, INT 2Fh, DPMI, XMS, memory management)

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod country;
pub mod dpmi;
pub mod dpmi_error;
pub mod files;
pub mod filesystem;
mod host_local_time;
pub mod int21;
pub mod memory;
pub mod network;
pub mod sfn;
pub mod shell;
pub mod snapshot;
pub mod unpacker;
pub mod xms_ext;

pub use xms_ext::XmsDispatch;

/// Default version exposed by INT 21h/AH=30h before a program profile changes it.
pub const DEFAULT_DOS_VERSION: (u8, u8) = (6, 22);

use anyhow::{bail, Result};
use gp64_cpu::Cpu;
use gp64_hw::bda;
use gp64_hw::{keyboard::Keyboard, pic::Pic, vga::Vga};
use gp64_memory::Memory;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

use self::dpmi::{Dpmi, PM_BIOS_CODE_SEL};
use crate::filesystem::{HostDrive, VirtualDrive};
use crate::memory::{first_mcb_seg_override, DosMem, DosMemError};
use gp64_memory::xms::Xms;
use gp64_types::{ByteStreamMarker, FixedString, FixedVec, SoundEnvConfig};

/// Maximum supported DOS path length.
const MAX_DOS_PATH: usize = 260;

// ==================================================================
// DOS kernel structure layout (mirrors the DOS structure layout)
// ==================================================================
//
// Segment  Paras  Purpose                           the DOS model source
// -----------------------------------------------------------------
// 0x0070   0x01   DOS_IHSEG (reserved, zero)         dos.cpp:4495
// 0x0071   0x32   InfoBlock (LoL + primary SFT)      dos.cpp:4512
// 0x00A3   0x56   SDA (0x560 bytes)                  dos.cpp:4513
// 0x00F9   0x04   CON device header                  dos.cpp:4517
// 0x00FD   0x10   CDS stub                           dos.cpp:4518
// 0x010D   0x04   Fake FCB SFT                       dos_tables.cpp:448
// 0x0111   0x36   DPB chain (26 x 33 B)              dos_tables.cpp:454
// 0x0147   0x06   Fake disk buffer head              dos_tables.cpp:482
// 0x014D   0x2D0  Secondary SFT (195 x 59 B)         dos_classes.cpp:139
// 0x041D   0x01   Media ID and empty DBCS table
// -----------------------------------------------------------------
// End of kernel structures: segment 0x041E (linear 0x41E0)
// First MCB (`DEFAULT_FIRST_MCB_SEG`): segment 0x0700
//
// The InfoBlock is exactly 0x32 paragraphs so the SDA (which lives at
// `InfoBlock_seg + 0x32 = 0x00A3`) places its `current_psp` field
// and other "genuine MS-DOS" detectors check this exact linear address.
const DOS_IHSEG: u16 = 0x0070;
const INFOBLOCK_SEG: u16 = 0x0071;
/// The LoL pointer returned by INT 21h/AH=52h is `INFOBLOCK_SEG:LOL_OFFSET`.
const LOL_SEGMENT: u16 = INFOBLOCK_SEG;
const LOL_OFFSET: u16 = 0x0022;
const SDA_SEG: u16 = 0x00A3;
const SDA_SIZE: u16 = 0x0560;
const CONDRV_SEG: u16 = 0x00F9;
const CDS_SEG: u16 = 0x00FD;
const FCBSFT_SEG: u16 = 0x010D;
const DPB_SEG: u16 = 0x0111;
const DISKBUF_SEG: u16 = 0x0147;
const SECSFT_SEG: u16 = 0x014D;
// DOS-owned metadata must not share storage with the caller's movable DTA.
const METADATA_SEG: u16 = 0x041D;
const MEDIA_ID_OFFSET: u16 = 0;
const DBCS_TABLE_OFFSET: u16 = 1;

// DOS kernel counts (match the DOS model defaults).
const DOS_FILES: u16 = 200;
const DOS_FCBS: u16 = 100;
const DOS_DRIVES: u16 = 26;
const DOS_LASTDRIVE: u8 = 1;
const DOS_BUFFERS_X: u16 = 50;
const DOS_BUFFERS_Y: u16 = 50;

/// InDOS flag location in the DOS Swappable Data Area.
///
/// INT 21h/AH=34h returns ES:BX pointing at this byte; the preceding byte is
/// the critical-error flag. Keep these in SDA rather than the 0050h scratch
/// area, which BIOS and protected-mode setup code also use.
pub const INDOS_SEGMENT: u16 = SDA_SEG;
pub const INDOS_OFFSET: u16 = 0x0001;
pub const CRITICAL_ERROR_OFFSET: u16 = 0x0000;

// PSP (Program Segment Prefix) field offsets
pub(crate) const PSP_INT22_TERMINATE: usize = 0x0A;
pub(crate) const PSP_INT23_CTRLC: usize = 0x0E;
pub(crate) const PSP_INT24_CRITICAL: usize = 0x12;
pub(crate) const PSP_PARENT_PSP: usize = 0x16;
pub(crate) const PSP_ENVIRONMENT_SEG: usize = 0x2C;
pub(crate) const PSP_COMMAND_LINE_LEN: u16 = 0x80;
pub(crate) const PSP_COMMAND_LINE: usize = 0x81;
pub(crate) const PSP_LOAD_ADDRESS: u16 = 0x0100;

/// Dump the 256-byte PSP region as a `Vec<u8>` for diagnostic logging.
///
/// Used by the `gp64::dos::psp` trace target to inspect the child PSP at
/// EXEC entry. Callers must gate the call site behind
/// `tracing::enabled!(target: "gp64::dos::psp", Level::TRACE)` so the
/// 256-byte read + allocation cost is zero when the target is not enabled.
///
/// progname.md` Part 0.
pub(crate) fn dump_psp_bytes(memory: &Memory, psp_seg: u16) -> Vec<u8> {
    let base = Memory::seg_offset_to_linear(psp_seg, 0);
    (0..0x100usize)
        .map(|off| memory.read_u8(base + off))
        .collect()
}

/// Walk an env block from `env_seg:0` to the double-NUL terminator, returning
/// the collected ASCIIZ strings (each entry is one `VAR=VALUE` string).
///
/// Stops scanning at the first `0x00` byte that is immediately followed by
/// another `0x00` (the canonical end-of-strings marker per RBIL "DOS
/// Environment Block"). Caps the scan at 32 KiB to bound the walk in case
/// of a malformed env block — well above any plausible real-DOS env block
/// size (typically 1-2 KiB; `init_environment`'s default vars + sound/glide
/// extensions stay under ~4 KiB) but small enough to terminate quickly when
/// the env is corrupt. `read_env_program_name` uses a tighter 4 KiB cap;
/// this helper is diagnostic-only so we trade a few extra reads for a wider
/// safety margin against truncation in corruption scenarios.
pub(crate) fn dump_env_strings(memory: &Memory, env_seg: u16) -> Vec<String> {
    if env_seg == 0 {
        return Vec::new();
    }
    let base = Memory::seg_offset_to_linear(env_seg, 0);
    let mut strings = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    let max_scan = 32 * 1024usize;
    let mut i = 0;
    while i < max_scan {
        let b = memory.read_u8(base + i);
        if b == 0 {
            if buf.is_empty() {
                // Double-NUL — end of strings.
                break;
            }
            strings.push(String::from_utf8_lossy(&buf).into_owned());
            buf.clear();
        } else {
            buf.push(b);
        }
        i += 1;
    }
    strings
}

/// Return the ASCIIZ progname extension of an env block (DOS 3.0+
/// convention: after the double-NUL terminator, a `WORD count` (always 1
/// for real DOS), then an ASCIIZ program pathname, then a trailing NUL).
///
/// Returns `None` if the env block does not contain a recognisable progname
/// (env_seg=0, the double-NUL terminator wasn't found within 32 KiB, or the
/// progname ASCIIZ string overflowed 512 bytes — all malformed cases).
///
/// Scan cap (32 KiB) and rationale match `dump_env_strings`; see that
/// function's doc-comment.
pub(crate) fn dump_env_progname(memory: &Memory, env_seg: u16) -> Option<String> {
    if env_seg == 0 {
        return None;
    }
    let base = Memory::seg_offset_to_linear(env_seg, 0);
    let max_scan = 32 * 1024usize;

    // Scan for the double-NUL terminator that marks the end of the env
    // strings. Match `read_env_program_name`'s shape: look for `0x00` at
    // offset i AND `0x00` at offset i+1, then step past both.
    let mut i = 0;
    let mut found = false;
    while i + 1 < max_scan {
        if memory.read_u8(base + i) == 0 && memory.read_u8(base + i + 1) == 0 {
            i += 2;
            found = true;
            break;
        }
        i += 1;
    }
    if !found {
        return None;
    }

    // WORD count (DOS 3.0+ always 1). Skip without parsing — we just want
    // the progname bytes that follow.
    if i + 2 > max_scan {
        return None;
    }
    i += 2;

    // ASCIIZ progname.
    let mut name: Vec<u8> = Vec::new();
    while i < max_scan && name.len() <= 512 {
        let b = memory.read_u8(base + i);
        if b == 0 {
            return Some(String::from_utf8_lossy(&name).into_owned());
        }
        name.push(b);
        i += 1;
    }
    None
}

use gp64_types::bda::{BDA_TICK_COUNT_ADDR, BIOS_TICKS_PER_DAY};

/// Convert BDA tick count to (hours, minutes, seconds, hundredths).
///
/// Real DOS INT 21h/2Ch reads the BIOS Data Area tick counter at 0040:006C.
/// The tick counter increments at ~18.2065 Hz (1,573,040 ticks per day).
fn bda_ticks_to_time(ticks: u32) -> (u8, u8, u8, u8) {
    let ticks = ticks % BIOS_TICKS_PER_DAY;
    let total_hundredths = (ticks as u64 * 24 * 60 * 60 * 100) / BIOS_TICKS_PER_DAY as u64;
    let hours = (total_hundredths / 360_000) as u8;
    let minutes = ((total_hundredths / 6_000) % 60) as u8;
    let seconds = ((total_hundredths / 100) % 60) as u8;
    let hundredths = (total_hundredths % 100) as u8;
    (hours, minutes, seconds, hundredths)
}

/// Convert (hours, minutes, seconds, hundredths) to BDA tick count.
///
/// This is the exact inverse of `bda_ticks_to_time()`. We compute
/// total_hundredths then reverse the division, ensuring round-trip
/// fidelity within 1 tick.
fn time_to_bda_ticks(hours: u8, minutes: u8, seconds: u8, hundredths: u8) -> u32 {
    let total_hundredths =
        hours as u64 * 360_000 + minutes as u64 * 6_000 + seconds as u64 * 100 + hundredths as u64;
    // Inverse of: total_hundredths = ticks * 8_640_000 / BIOS_TICKS_PER_DAY
    ((total_hundredths * BIOS_TICKS_PER_DAY as u64) / 8_640_000) as u32
}

/// Compute the initial BDA tick count from the host wall clock.
///
/// A real BIOS reads the CMOS RTC during POST and converts it to BDA ticks.
/// This function replicates that: it reads the host local time and returns the
/// corresponding BDA tick count so the emulator can seed `0040:006C` with a
/// value that matches the real time of day. Without this, INT 21h AH=2Ch
/// (Get System Time) then begins at the local time of day.
pub fn boot_time_bda_ticks() -> u32 {
    let (h, m, s, hs) = get_host_time();
    time_to_bda_ticks(h, m, s, hs)
}

/// Compute the initial BDA tick count from a fixed Unix epoch.
///
/// The CMOS fixed-RTC path interprets its epoch in UTC and exposes only
/// whole-second precision. Deriving the BDA seed from the epoch's UTC
/// time-of-day keeps `0040:006C` in lockstep with that same RTC source.
pub fn boot_time_bda_ticks_from_epoch(epoch_secs: u64) -> u32 {
    let seconds_since_midnight = epoch_secs % (24 * 60 * 60);
    let hours = (seconds_since_midnight / (60 * 60)) as u8;
    let minutes = ((seconds_since_midnight / 60) % 60) as u8;
    let seconds = (seconds_since_midnight % 60) as u8;
    time_to_bda_ticks(hours, minutes, seconds, 0)
}

/// The boot date (year, month, day, day_of_week) for a fixed Unix epoch.
///
/// The date twin of [`boot_time_bda_ticks_from_epoch`]: a fixed-RTC run must
/// pin the DOS date to the same UTC epoch as the time of day, or INT 21h
/// AH=2Ah still answers with the host's date.
pub fn boot_date_from_epoch(epoch_secs: u64) -> (u16, u8, u8, u8) {
    let (year, month, day) = Dos::days_to_ymd((epoch_secs / (24 * 60 * 60)) as i64);
    let (year, month, day) = (year as u16, month as u8, day as u8);
    (year, month, day, day_of_week(year, month, day))
}

/// Calculate day of week (0=Sunday..6=Saturday) using Tomohiko Sakamoto's algorithm.
///
/// Valid for any Gregorian date. This matches the DOS convention where
/// INT 21h AH=2A returns AL=0 for Sunday, 1 for Monday, ..., 6 for Saturday.
fn day_of_week(year: u16, month: u8, day: u8) -> u8 {
    // Sakamoto's method: adjust year for Jan/Feb, then index into offset table.
    let mut y = year as i32;
    let m = month as usize;
    let d = day as i32;
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    if m < 3 {
        y -= 1;
    }
    ((y + y / 4 - y / 100 + y / 400 + T[m - 1] + d) % 7) as u8
}

/// Get current local date as (year, month, day, day_of_week).
fn get_system_date() -> (u16, u8, u8, u8) {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct SystemTime {
            w_year: u16,
            w_month: u16,
            w_day_of_week: u16,
            w_day: u16,
            w_hour: u16,
            w_minute: u16,
            w_second: u16,
            w_milliseconds: u16,
        }
        extern "system" {
            fn GetLocalTime(lp_system_time: *mut SystemTime);
        }
        let mut st = SystemTime {
            w_year: 0,
            w_month: 0,
            w_day_of_week: 0,
            w_day: 0,
            w_hour: 0,
            w_minute: 0,
            w_second: 0,
            w_milliseconds: 0,
        };
        unsafe {
            GetLocalTime(&mut st);
        }
        (
            st.w_year,
            st.w_month as u8,
            st.w_day as u8,
            st.w_day_of_week as u8,
        )
    }
    #[cfg(not(windows))]
    {
        use std::time::{SystemTime as StdSystemTime, UNIX_EPOCH};
        let duration = StdSystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO);
        let epoch_secs = duration.as_secs() as i64;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            libc::localtime_r(&epoch_secs as *const i64, &mut tm);
        }
        (
            (tm.tm_year + 1900) as u16,
            (tm.tm_mon + 1) as u8,
            tm.tm_mday as u8,
            tm.tm_wday as u8,
        )
    }
}

/// Read the host clock as (hours, minutes, seconds, hundredths).
///
/// Used ONLY to seed the BDA tick counter at boot, the way a real BIOS reads
/// the CMOS RTC during POST. It must never back a guest-visible time service:
/// guest time is cycles-derived, and a guest reading the host clock sees time
/// advance at the host's rate rather than its own.
fn get_host_time() -> (u8, u8, u8, u8) {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct SystemTime {
            w_year: u16,
            w_month: u16,
            w_day_of_week: u16,
            w_day: u16,
            w_hour: u16,
            w_minute: u16,
            w_second: u16,
            w_milliseconds: u16,
        }
        extern "system" {
            fn GetLocalTime(lp_system_time: *mut SystemTime);
        }
        let mut st = SystemTime {
            w_year: 0,
            w_month: 0,
            w_day_of_week: 0,
            w_day: 0,
            w_hour: 0,
            w_minute: 0,
            w_second: 0,
            w_milliseconds: 0,
        };
        unsafe {
            GetLocalTime(&mut st);
        }
        (
            st.w_hour as u8,
            st.w_minute as u8,
            st.w_second as u8,
            (st.w_milliseconds / 10) as u8,
        )
    }
    #[cfg(not(windows))]
    {
        use std::time::{SystemTime as StdSystemTime, UNIX_EPOCH};
        let duration = StdSystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO);
        let epoch_secs = duration.as_secs() as i64;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            libc::localtime_r(&epoch_secs as *const i64, &mut tm);
        }
        let millis = (duration.subsec_millis() / 10) as u8;
        (tm.tm_hour as u8, tm.tm_min as u8, tm.tm_sec as u8, millis)
    }
}

/// Read the host date once at boot as (year, month, day, day_of_week).
/// Used to seed the emulated date field for deterministic execution.
fn boot_date_from_host() -> (u16, u8, u8, u8) {
    get_system_date()
}

/// Special MCB owner value for system/TSR memory (won't be freed by normal operations)
pub const MCB_OWNER_SYSTEM: u16 = 0x0008;

/// Process state for EXEC/exit handling
#[derive(Debug, Clone)]
pub(crate) struct ProcessState {
    pub(crate) psp_segment: u16,
    pub(crate) registers: gp64_cpu::Registers,
    #[allow(dead_code)]
    pub(crate) parent_psp: u16,
    pub(crate) exec_ret_ip: u16,
    pub(crate) exec_ret_cs: u16,
    pub(crate) exec_ret_flags: u16,
    /// The parent called EXEC from DPMI protected mode. The host ran the
    /// child in real mode with the DPMI session intact; the child's exit
    /// resumes the parent in protected mode (MDD-00017).
    pub(crate) pm_parent: bool,
}

#[derive(Debug, Clone)]
pub enum Int21Disposition {
    Iret,
    NoIret,
    /// INT23 owns the newly pushed frame; the machine must execute the handler
    /// and restart this console service at its retained INT21 frame on return.
    CtrlC,
    /// A critical I/O error occurred — the HLE dispatcher must invoke INT 24h.
    /// `leave_dos()` is NOT called; it is deferred until after INT 24h completes.
    CriticalError {
        ah_function: u8,
        drive: u8,      // 0=A, 1=B, 2=C, ...
        error_code: u8, // DOS error code 19-31
        is_write: bool,
        entry_snapshot: Box<gp64_cpu::Registers>,
    },
    /// Redirect to the installed BIOS assembler keyboard wait stub.
    /// The stub loops until BDA has key data, then re-enters INT 21h.
    KbdWaitStub,
}

/// Manual PartialEq: compares all fields except `entry_snapshot` (Registers lacks PartialEq).
/// Existing code only compares against Iret/NoIret, but tests use assert_eq!/assert_ne!.
impl PartialEq for Int21Disposition {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Iret, Self::Iret)
            | (Self::NoIret, Self::NoIret)
            | (Self::CtrlC, Self::CtrlC)
            | (Self::KbdWaitStub, Self::KbdWaitStub) => true,
            (
                Self::CriticalError {
                    ah_function: a1,
                    drive: d1,
                    error_code: e1,
                    is_write: w1,
                    ..
                },
                Self::CriticalError {
                    ah_function: a2,
                    drive: d2,
                    error_code: e2,
                    is_write: w2,
                    ..
                },
            ) => a1 == a2 && d1 == d2 && e1 == e2 && w1 == w2,
            _ => false,
        }
    }
}

impl Eq for Int21Disposition {}

/// State saved when INT 24h is dispatched.
/// Contains both the INT 24h parameters and saved register state.
///
/// Two levels of register preservation:
/// - `saved_bx..saved_es`: Caller registers that the INT 24h handler may clobber.
///   Restored on Ignore, Fail, and Abort. (DOS spec: INT 24h may destroy all
///   registers except SS:SP, and DOS restores BX/CX/DX/SI/DI/BP/DS/ES.)
/// - `saved_cpu_snapshot`: Full CPU register snapshot from INT 21h entry, BEFORE
///   any function-specific logic ran. Used only for Retry, which must re-execute
///   the entire INT 21h function from scratch.
#[derive(Clone, Debug)]
pub struct PendingCriticalError {
    pub ah_function: u8,
    pub drive: u8,
    pub error_code: u8,
    pub is_write: bool,
    /// Whether the caller was in protected mode when INT 21h was called
    pub caller_was_pm: bool,
    /// Whether the INT 21h was dispatched via the DPMI HLE path (no IRET frame)
    pub caller_is_dpmi_hle: bool,

    // Saved caller registers (BX-ES) — restored on Ignore/Fail/Abort
    pub saved_bx: u16,
    pub saved_cx: u16,
    pub saved_dx: u16,
    pub saved_si: u16,
    pub saved_di: u16,
    pub saved_bp: u16,
    pub saved_ds: u16,
    pub saved_es: u16,

    // Saved PM segment caches — restored when returning from RM handler to PM caller
    pub saved_ds_cache: Option<gp64_cpu::SegmentCache>,
    pub saved_es_cache: Option<gp64_cpu::SegmentCache>,
    pub saved_ss_cache: Option<gp64_cpu::SegmentCache>,
    pub saved_cs_cache: Option<gp64_cpu::SegmentCache>,

    // Full CPU snapshot for Retry — taken at INT 21h entry, before function dispatch
    pub saved_cpu_snapshot: gp64_cpu::Registers,
}

/// Result of a keyboard character read attempt (used by INT 21h AH=01/06/07/08).
pub(crate) enum KeyboardReadResult {
    /// Character (or extended scancode) ready; AL has been set.
    Ready,
    /// Ctrl-C was detected; "^C" echoed and INT 23h triggered.
    CtrlC,
    /// No key available in the buffer.
    NoKey,
}

#[derive(Clone, Copy)]
struct AhRange {
    start: u8,
    end: u8,
}

// KILN-00086: the AH-range DOS trace filter was env-configured; that override was
// retired. Parsing is removed; the filter is permanently absent, so `should_trace_ah`
// traces every AH when the RUST_LOG DOS trace is enabled.
fn parse_trace_ah() -> Option<Vec<AhRange>> {
    None
}

fn should_trace_ah(ah: u8) -> bool {
    if cfg!(test) || cfg!(coverage) {
        return match parse_trace_ah() {
            None => true,
            Some(ranges) => ranges.iter().any(|r| ah >= r.start && ah <= r.end),
        };
    }
    static FILTER: OnceLock<Option<Vec<AhRange>>> = OnceLock::new();
    let filter = FILTER.get_or_init(parse_trace_ah);
    match filter {
        None => true,
        Some(ranges) => ranges.iter().any(|r| ah >= r.start && ah <= r.end),
    }
}

use gp64_types::define_trace_checker;

// Enable via `RUST_LOG=gp64::dos=debug`.
define_trace_checker!(dos_trace_enabled, "gp64::dos", tracing::Level::DEBUG);

// KILN-00086: the InDOS-flag trace env override was retired; permanently off.
fn dos_indos_trace_enabled() -> bool {
    false
}

// Enable via `RUST_LOG=gp64::dos::int=debug`.
define_trace_checker!(
    dos_int_trace_enabled,
    "gp64::dos::int",
    tracing::Level::DEBUG
);

#[cfg(any(test, coverage))]
fn dos_trace_value(key: &str) -> Option<String> {
    std::env::var("GP64_DOS_TRACE").ok().and_then(|value| {
        value.split(',').find_map(|item| {
            let (name, value) = item.trim().split_once('=')?;
            (name == key).then(|| value.to_string())
        })
    })
}

#[cfg(not(any(test, coverage)))]
fn dos_trace_value(_key: &str) -> Option<String> {
    None
}

fn dos_trace_limit(key: &str, default: usize) -> usize {
    dos_trace_value(key)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn dos_int_trace_max() -> usize {
    dos_trace_limit("int", 2000)
}

// Enable via `RUST_LOG=gp64::dos::memory=debug`.
define_trace_checker!(
    dos_mem_trace_enabled,
    "gp64::dos::memory",
    tracing::Level::DEBUG
);

// Enable via `RUST_LOG=gp64::dos::read=debug`.
define_trace_checker!(
    dos_read_trace_enabled,
    "gp64::dos::read",
    tracing::Level::DEBUG
);

fn dos_read_trace_max() -> usize {
    dos_trace_limit("read", 2000)
}

fn trace_dos_read(
    handle: u16,
    path: &str,
    requested: usize,
    bytes_read: usize,
    ok: bool,
    pos: Option<u64>,
) {
    if !dos_read_trace_enabled() {
        return;
    }
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let max = dos_read_trace_max();
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if max != 0 && n >= max {
        return;
    }
    let status = if ok { "OK" } else { "ERR" };
    tracing::debug!(
        target: "gp64::dos::read",
        status,
        handle = format_args!("{:04X}", handle),
        requested,
        bytes_read,
        pos = pos.map(|p| p as i64).unwrap_or(-1),
        path,
        "DOS read"
    );
}

fn trace_dos_int(cpu: &Cpu, ah: u8, caller_cs: u16, caller_ip: u16) {
    if !dos_int_trace_enabled() || !should_trace_ah(ah) {
        return;
    }
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let max = dos_int_trace_max();
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if max != 0 && n >= max {
        return;
    }
    tracing::debug!(
        target: "gp64::dos::int",
        cs = format_args!("{:04X}", caller_cs),
        ip = format_args!("{:04X}", caller_ip),
        ah = format_args!("{:02X}", ah),
        al = format_args!("{:02X}", cpu.regs.al()),
        ax = format_args!("{:04X}", cpu.regs.ax()),
        bx = format_args!("{:04X}", cpu.regs.bx()),
        cx = format_args!("{:04X}", cpu.regs.cx()),
        dx = format_args!("{:04X}", cpu.regs.dx()),
        ds = format_args!("{:04X}", cpu.regs.ds),
        es = format_args!("{:04X}", cpu.regs.es),
        ss = format_args!("{:04X}", cpu.regs.ss),
        sp = format_args!("{:04X}", cpu.regs.sp()),
        "DOS INT 21h"
    );
}

// Enable via `RUST_LOG=gp64::dos::write=debug`.
define_trace_checker!(
    dos_write_trace_enabled,
    "gp64::dos::write",
    tracing::Level::DEBUG
);

fn dos_write_trace_max() -> usize {
    dos_trace_limit("write", 200)
}

#[cfg(test)]
fn trace_dos_write(handle: u16, buf: &[u8]) {
    trace_dos_write_preview(handle, buf.len(), buf);
}

fn trace_dos_write_preview(handle: u16, len: usize, preview: &[u8]) {
    if !dos_write_trace_enabled() {
        return;
    }
    let max = dos_write_trace_max();
    let (s, sample_truncated) = format_dos_bytes(preview, max);
    let truncated = sample_truncated || preview.len() < len;
    tracing::debug!(
        target: "gp64::dos::write",
        handle = format_args!("{:04X}", handle),
        len,
        text = format_args!("{}{}", s, if truncated { "..." } else { "" }),
        "DOS write"
    );
}

// Enable via `RUST_LOG=gp64::dos::seek=debug`.
define_trace_checker!(
    dos_seek_trace_enabled,
    "gp64::dos::seek",
    tracing::Level::DEBUG
);

fn dos_seek_trace_max() -> usize {
    dos_trace_limit("seek", 2000)
}

fn trace_dos_seek(handle: u16, path: &str, origin: u8, offset: i64, ok: bool, pos: Option<u64>) {
    if !dos_seek_trace_enabled() {
        return;
    }
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let max = dos_seek_trace_max();
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if max != 0 && n >= max {
        return;
    }
    let status = if ok { "OK" } else { "ERR" };
    tracing::debug!(
        target: "gp64::dos::seek",
        status,
        handle = format_args!("{:04X}", handle),
        origin,
        offset,
        pos = pos.map(|p| p as i64).unwrap_or(-1),
        path,
        "DOS seek"
    );
}

// Disabled-by-default structured DOS resource trace for correlating file
// operations with external lifecycle markers (for example SB playback
// completion). Keep this diagnostic generic: it does not know about phases.
define_trace_checker!(
    dos_resource_trace_log_enabled,
    "gp64::diag::dos_resource",
    tracing::Level::DEBUG
);

fn dos_resource_trace_event_limit() -> usize {
    dos_trace_limit("resource_events", 512)
}

struct DosResourceEventState {
    event_count: AtomicUsize,
    dropped_count: AtomicUsize,
    limit_reported: AtomicBool,
}

impl DosResourceEventState {
    const fn new() -> Self {
        Self {
            event_count: AtomicUsize::new(0),
            dropped_count: AtomicUsize::new(0),
            limit_reported: AtomicBool::new(false),
        }
    }

    fn next_event(&self, max: usize) -> Option<usize> {
        let n = self.event_count.fetch_add(1, Ordering::Relaxed);
        if max != 0 && n >= max {
            let dropped = self.dropped_count.fetch_add(1, Ordering::Relaxed) + 1;
            if !self.limit_reported.swap(true, Ordering::Relaxed) {
                tracing::debug!(
                    target: "gp64::diag::dos_resource",
                    event_limit = max,
                    attempted_event_index = n,
                    dropped_events = dropped,
                    overflow = true,
                    "dos_resource_limit_reached"
                );
            }
            return None;
        }
        Some(n)
    }

    fn dropped_events(&self) -> usize {
        self.dropped_count.load(Ordering::Relaxed)
    }
}

static DOS_RESOURCE_EVENT_STATE: DosResourceEventState = DosResourceEventState::new();

#[cfg(any(test, coverage))]
fn parse_env_flag(name: &str) -> bool {
    match dos_trace_value(name) {
        Some(v) => {
            let v = v.trim();
            !(v.is_empty()
                || v == "0"
                || v.eq_ignore_ascii_case("false")
                || v.eq_ignore_ascii_case("no")
                || v.eq_ignore_ascii_case("off"))
        }
        None => false,
    }
}

#[cfg(not(any(test, coverage)))]
fn parse_env_flag(_name: &str) -> bool {
    false
}

fn dos_resource_trace_env_enabled() -> bool {
    if cfg!(test) || cfg!(coverage) {
        return parse_env_flag("resource");
    }
    static VAL: OnceLock<bool> = OnceLock::new();
    *VAL.get_or_init(|| parse_env_flag("resource"))
}

fn dos_resource_trace_include_all() -> bool {
    if cfg!(test) || cfg!(coverage) {
        return parse_env_flag("resource_all");
    }
    static VAL: OnceLock<bool> = OnceLock::new();
    *VAL.get_or_init(|| parse_env_flag("resource_all"))
}

fn dos_resource_trace_sample_bytes() -> usize {
    16
}

pub(crate) fn dos_resource_trace_active() -> bool {
    dos_resource_trace_env_enabled() && dos_resource_trace_log_enabled()
}

fn dos_resource_basename(path: &str) -> String {
    path.rsplit(['\\', '/', ':'])
        .next()
        .unwrap_or(path)
        .to_ascii_uppercase()
}

fn dos_resource_path_matches(path: &str) -> bool {
    dos_resource_trace_include_all() || !path.is_empty()
}

fn dos_resource_display_path(
    guest_path: &str,
    resolved_drive: Option<u8>,
    resolved_path: Option<&str>,
) -> String {
    match (resolved_drive, resolved_path) {
        (Some(drive), Some(path)) => format!("{}:{}", (drive + b'A') as char, path),
        _ => guest_path.to_string(),
    }
}

fn dos_resource_trace_start_once() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    tracing::debug!(
        target: "gp64::diag::dos_resource",
        event_limit = dos_resource_trace_event_limit(),
        sample_bytes = dos_resource_trace_sample_bytes(),
        include_all = dos_resource_trace_include_all(),
        dropped_events = 0usize,
        overflow = false,
        phase_source = "external_log_order",
        "dos_resource_start"
    );
}

fn dos_resource_next_event() -> Option<usize> {
    if !dos_resource_trace_active() {
        return None;
    }
    dos_resource_trace_start_once();
    DOS_RESOURCE_EVENT_STATE.next_event(dos_resource_trace_event_limit())
}

fn dos_resource_dropped_events() -> usize {
    DOS_RESOURCE_EVENT_STATE.dropped_events()
}

fn dos_resource_handle_value(handle: Option<u16>) -> i64 {
    handle.map(|v| v as i64).unwrap_or(-1)
}

fn dos_resource_handle_hex(handle: Option<u16>) -> String {
    handle
        .map(|v| format!("{v:04X}"))
        .unwrap_or_else(|| "NONE".to_string())
}

fn dos_resource_sample(buf: &[u8]) -> (String, usize, bool, u32) {
    let max = dos_resource_trace_sample_bytes();
    let sample_len = buf.len().min(max);
    let mut sample = String::with_capacity(sample_len * 2);
    {
        use std::fmt::Write as _;
        for byte in &buf[..sample_len] {
            let _ = write!(sample, "{:02X}", byte);
        }
    }
    let checksum = buf.iter().fold(0x811C_9DC5u32, |acc, byte| {
        (acc ^ *byte as u32).wrapping_mul(0x0100_0193)
    });
    (sample, sample_len, buf.len() > sample_len, checksum)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_dos_resource_open(
    ah: u8,
    guest_path: &str,
    resolved_drive: Option<u8>,
    resolved_path: Option<&str>,
    access_mode: u8,
    share_mode: u8,
    effective_write: Option<bool>,
    handle: Option<u16>,
    error_code: Option<u16>,
    caller_cs: u16,
    caller_ip: u16,
) {
    trace_dos_resource_open_common(
        ah,
        guest_path,
        resolved_drive,
        resolved_path,
        access_mode,
        share_mode,
        effective_write,
        handle,
        error_code,
        None,
        None,
        caller_cs,
        caller_ip,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_dos_resource_open_ex(
    ah: u8,
    guest_path: &str,
    resolved_drive: Option<u8>,
    resolved_path: Option<&str>,
    access_mode: u8,
    share_mode: u8,
    effective_write: Option<bool>,
    handle: Option<u16>,
    error_code: Option<u16>,
    extended_action: u8,
    action_taken: Option<u16>,
    caller_cs: u16,
    caller_ip: u16,
) {
    trace_dos_resource_open_common(
        ah,
        guest_path,
        resolved_drive,
        resolved_path,
        access_mode,
        share_mode,
        effective_write,
        handle,
        error_code,
        Some(extended_action),
        action_taken,
        caller_cs,
        caller_ip,
    );
}

#[allow(clippy::too_many_arguments)]
fn trace_dos_resource_open_common(
    ah: u8,
    guest_path: &str,
    resolved_drive: Option<u8>,
    resolved_path: Option<&str>,
    access_mode: u8,
    share_mode: u8,
    effective_write: Option<bool>,
    handle: Option<u16>,
    error_code: Option<u16>,
    extended_action: Option<u8>,
    action_taken: Option<u16>,
    caller_cs: u16,
    caller_ip: u16,
) {
    if !dos_resource_trace_active() {
        return;
    }
    let path = dos_resource_display_path(guest_path, resolved_drive, resolved_path);
    let filter_match = dos_resource_path_matches(&path) || dos_resource_path_matches(guest_path);
    if !filter_match && !dos_resource_trace_include_all() {
        return;
    }
    let Some(event_index) = dos_resource_next_event() else {
        return;
    };
    let basename = dos_resource_basename(&path);
    let ok = error_code.is_none();
    tracing::debug!(
        target: "gp64::diag::dos_resource",
        op = "open",
        event_index,
        ah = format_args!("{:02X}", ah),
        result = if ok { "ok" } else { "error" },
        carry = !ok,
        error_code = error_code.map(|v| v as i64).unwrap_or(-1),
        handle = dos_resource_handle_value(handle),
        handle_hex = %dos_resource_handle_hex(handle),
        guest_path,
        path = %path,
        basename = %basename,
        filter_match,
        access_mode = format_args!("{:02X}", access_mode),
        share_mode = format_args!("{:02X}", share_mode),
        effective_write_known = effective_write.is_some(),
        effective_write = effective_write.unwrap_or(false),
        extended_action = extended_action.map(|v| v as i64).unwrap_or(-1),
        action_taken = action_taken.map(|v| v as i64).unwrap_or(-1),
        open_if_exists = extended_action.is_some_and(|v| (v & 0x01) != 0),
        create_if_not_exists = extended_action.is_some_and(|v| (v & 0x10) != 0),
        truncate_if_exists = extended_action.is_some_and(|v| (v & 0x02) != 0),
        dropped_events = dos_resource_dropped_events(),
        overflow = dos_resource_dropped_events() > 0,
        caller_cs = format_args!("{:04X}", caller_cs),
        caller_ip = format_args!("{:04X}", caller_ip),
        "dos_resource_open"
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_dos_resource_read(
    handle: u16,
    path: &str,
    requested_len: usize,
    returned_len: usize,
    before_pos: Option<u64>,
    after_pos: Option<u64>,
    error_code: Option<u16>,
    data: &[u8],
    final_int21_semantics: bool,
    caller_cs: u16,
    caller_ip: u16,
) {
    if !dos_resource_trace_active() {
        return;
    }
    let filter_match = dos_resource_path_matches(path);
    if !filter_match && !dos_resource_trace_include_all() {
        return;
    }
    let Some(event_index) = dos_resource_next_event() else {
        return;
    };
    let basename = dos_resource_basename(path);
    let (sample_hex, sample_len, sample_truncated, sample_checksum) = dos_resource_sample(data);
    let sample_truncated = sample_truncated || returned_len > data.len();
    let ok = error_code.is_none();
    tracing::debug!(
        target: "gp64::diag::dos_resource",
        op = "read",
        event_index,
        result = if ok { "ok" } else { "error" },
        carry = !ok,
        error_code = error_code.map(|v| v as i64).unwrap_or(-1),
        handle = dos_resource_handle_value(Some(handle)),
        handle_hex = %dos_resource_handle_hex(Some(handle)),
        path,
        basename = %basename,
        filter_match,
        requested_len,
        returned_len,
        before_pos = before_pos.map(|p| p as i64).unwrap_or(-1),
        after_pos = after_pos.map(|p| p as i64).unwrap_or(-1),
        short_read = ok && returned_len < requested_len,
        eof_like = ok && requested_len > 0 && returned_len == 0,
        sample_len,
        sample_truncated,
        sample_checksum = format_args!("{:08X}", sample_checksum),
        sample_hex = %sample_hex,
        final_int21_semantics,
        error_stage = if final_int21_semantics { "final" } else { "pre_critical_error" },
        dropped_events = dos_resource_dropped_events(),
        overflow = dos_resource_dropped_events() > 0,
        caller_cs = format_args!("{:04X}", caller_cs),
        caller_ip = format_args!("{:04X}", caller_ip),
        "dos_resource_read"
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_dos_resource_seek(
    handle: u16,
    path: &str,
    origin: u8,
    requested_offset: i64,
    before_pos: Option<u64>,
    after_pos: Option<u64>,
    error_code: Option<u16>,
    caller_cs: u16,
    caller_ip: u16,
) {
    if !dos_resource_trace_active() {
        return;
    }
    let filter_match = dos_resource_path_matches(path);
    if !filter_match && !dos_resource_trace_include_all() {
        return;
    }
    let Some(event_index) = dos_resource_next_event() else {
        return;
    };
    let basename = dos_resource_basename(path);
    let ok = error_code.is_none();
    tracing::debug!(
        target: "gp64::diag::dos_resource",
        op = "seek",
        event_index,
        result = if ok { "ok" } else { "error" },
        carry = !ok,
        error_code = error_code.map(|v| v as i64).unwrap_or(-1),
        handle = dos_resource_handle_value(Some(handle)),
        handle_hex = %dos_resource_handle_hex(Some(handle)),
        path,
        basename = %basename,
        filter_match,
        origin,
        requested_offset,
        before_pos = before_pos.map(|p| p as i64).unwrap_or(-1),
        after_pos = after_pos.map(|p| p as i64).unwrap_or(-1),
        dropped_events = dos_resource_dropped_events(),
        overflow = dos_resource_dropped_events() > 0,
        caller_cs = format_args!("{:04X}", caller_cs),
        caller_ip = format_args!("{:04X}", caller_ip),
        "dos_resource_seek"
    );
}

pub(crate) fn trace_dos_resource_close(
    handle: u16,
    path: Option<&str>,
    error_code: Option<u16>,
    caller_cs: u16,
    caller_ip: u16,
) {
    if !dos_resource_trace_active() {
        return;
    }
    let display_path = path.unwrap_or("<unknown>");
    let filter_match = path.is_some_and(dos_resource_path_matches);
    if !filter_match && !dos_resource_trace_include_all() {
        return;
    }
    let Some(event_index) = dos_resource_next_event() else {
        return;
    };
    let basename = if filter_match {
        dos_resource_basename(display_path)
    } else {
        String::new()
    };
    let ok = error_code.is_none();
    tracing::debug!(
        target: "gp64::diag::dos_resource",
        op = "close",
        event_index,
        result = if ok { "ok" } else { "error" },
        carry = !ok,
        error_code = error_code.map(|v| v as i64).unwrap_or(-1),
        handle = dos_resource_handle_value(Some(handle)),
        handle_hex = %dos_resource_handle_hex(Some(handle)),
        path = display_path,
        basename = %basename,
        filter_match,
        dropped_events = dos_resource_dropped_events(),
        overflow = dos_resource_dropped_events() > 0,
        caller_cs = format_args!("{:04X}", caller_cs),
        caller_ip = format_args!("{:04X}", caller_ip),
        "dos_resource_close"
    );
}

// Enable via `RUST_LOG=gp64::dos::find=debug`.
define_trace_checker!(
    dos_find_trace_enabled,
    "gp64::dos::find",
    tracing::Level::DEBUG
);

fn dos_find_trace_max() -> usize {
    dos_trace_limit("find", 2000)
}

fn trace_dos_find(
    action: &str,
    pattern: &str,
    attr_filter: u8,
    entry: Option<&FindEntry>,
    ok: bool,
) {
    if !dos_find_trace_enabled() {
        return;
    }
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let max = dos_find_trace_max();
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if max != 0 && n >= max {
        return;
    }
    let status = if ok { "OK" } else { "ERR" };
    tracing::debug!(
        target: "gp64::dos::find",
        status,
        action,
        attr = format_args!("0x{:02X}", attr_filter),
        pattern,
        name = entry.map(|e| e.name.as_str()).unwrap_or(""),
        size = entry.map(|e| e.size as i64).unwrap_or(-1),
        "DOS find"
    );
}

// Enable via `RUST_LOG=gp64::dos::print=debug`.
define_trace_checker!(
    dos_print_trace_enabled,
    "gp64::dos::print",
    tracing::Level::DEBUG
);

// Enable via `RUST_LOG=gp64::dos::stdout=debug`.
// Emits one event per `\n`-terminated line written through INT 21h AH=06
// (Direct Console I/O, output mode). Used to surface extender fatal-exception
// register dumps that are otherwise lost in `--headless` runs. See
// `wrk_docs/2026.05.23 - HLD - INT 21h AH=06 console output log routing.md`.
define_trace_checker!(
    dos_ah06_trace_enabled,
    "gp64::dos::stdout",
    tracing::Level::DEBUG
);

/// Hard cap on `Ah06LineBuffer` so a non-newlining caller cannot grow the
/// buffer without bound. When this cap is hit we flush as if a newline had
/// arrived (but with `newline = false` on the emitted event).
const AH06_MAX_LINE: usize = 512;

/// Per-Dos-instance accumulator state for AH=06 output.
///
/// Bytes are pushed one at a time as the guest issues `INT 21h AH=06`. A
/// `tracing::debug!` event is emitted at target `gp64::dos::stdout` when
/// the buffer hits a `\n` (the natural line terminator) or grows past
/// [`AH06_MAX_LINE`]. Bare `\r` bytes are dropped so DOS `\r\n` pairs produce
/// exactly one log line.
///
/// The buffer is `Default`-initialised and allocates lazily — there is no
/// per-call cost when the `gp64::dos::stdout` trace is disabled because
/// the call site is gated by `dos_ah06_trace_enabled()`.
#[derive(Default)]
pub(crate) struct Ah06LineBuffer {
    buf: Vec<u8>,
}

impl Ah06LineBuffer {
    /// Push one byte. If the byte completes a line (or the line cap is hit),
    /// emit a tracing event and reset the buffer.
    pub(crate) fn push(&mut self, ch: u8) {
        // Drop bare '\r' so DOS "\r\n" pairs don't produce two log lines.
        if ch == b'\r' {
            return;
        }
        if ch == b'\n' {
            self.flush(true);
            return;
        }
        self.buf.push(ch);
        if self.buf.len() >= AH06_MAX_LINE {
            self.flush(false);
        }
    }

    fn flush(&mut self, had_newline: bool) {
        // Render printable ASCII as-is, escape control bytes as \xNN — same
        // policy as `format_dos_bytes()` so AH=06 output matches the
        // existing AH=09 / AH=40 log shape.
        let (text, _truncated) = format_dos_bytes(&self.buf, AH06_MAX_LINE);
        tracing::debug!(
            target: "gp64::dos::stdout",
            len = self.buf.len(),
            newline = had_newline,
            text = %text,
            "DOS AH=06 output"
        );
        self.buf.clear();
    }
}

fn dos_print_trace_max() -> usize {
    dos_trace_limit("print", 200)
}

fn trace_dos_path(op: &str, segment: u16, offset: u32, path: &str) {
    tracing::debug!(
        target: "gp64::dos::path",
        op,
        segment = format_args!("{:04X}", segment),
        offset = format_args!("{:08X}", offset),
        path,
        "DOS path"
    );
}

/// Runaway bound for INT 21h AH=09h: stop printing after this many bytes
/// when the guest string has no `$` terminator.
const DOS_AH09_MAX: usize = 65535;

fn format_dos_bytes(buf: &[u8], max: usize) -> (String, bool) {
    let mut s = String::new();
    let limit = buf.len().min(max);
    for &ch in buf.iter().take(limit) {
        if (0x20..=0x7E).contains(&ch) {
            s.push(ch as char);
        } else if ch == b'\r' {
            s.push_str("\\r");
        } else if ch == b'\n' {
            s.push_str("\\n");
        } else if ch == b'\t' {
            s.push_str("\\t");
        } else {
            use std::fmt::Write;
            let _ = write!(s, "\\x{ch:02X}");
        }
    }
    (s, buf.len() > max)
}

// Enable via `RUST_LOG=gp64::dos::exec=debug`.
define_trace_checker!(exec_trace_enabled, "gp64::dos::exec", tracing::Level::DEBUG);

pub(crate) type FindSearchEntries =
    FixedVec<(FixedString<13>, crate::filesystem::FileAttributes), 4096>;

pub(crate) type LfnFindSearchEntries = FixedVec<LfnFindSearchEntry, 4096>;

#[derive(Clone, Copy)]
pub(crate) struct LfnFindSearchEntry {
    pub(crate) long_name: FixedString<260>,
    pub(crate) short_name: FixedString<13>,
    pub(crate) attrs: crate::filesystem::FileAttributes,
    pub(crate) creation_time: std::time::SystemTime,
    pub(crate) last_access_time: std::time::SystemTime,
    pub(crate) last_write_time: std::time::SystemTime,
}

/// Search state allocated by INT 21h/AX=714Eh and released by AX=71A1h.
pub(crate) struct LfnFindSearchState {
    pub(crate) pattern: FixedString<260>,
    pub(crate) drive: u8,
    pub(crate) dir_path: FixedString<260>,
    pub(crate) attr_filter: u16,
    pub(crate) entries: Box<LfnFindSearchEntries>,
    /// Index of the next result to return; FindFirst publishes entry zero.
    pub(crate) index: usize,
    /// Search handles are process resources and are retired with their PSP.
    pub(crate) owner_psp: u16,
}

impl Clone for LfnFindSearchState {
    fn clone(&self) -> Self {
        let mut entries = LfnFindSearchEntries::new_boxed();
        for entry in self.entries.iter() {
            entries.push(*entry);
        }
        Self {
            pattern: self.pattern,
            drive: self.drive,
            dir_path: self.dir_path,
            attr_filter: self.attr_filter,
            entries,
            index: self.index,
            owner_psp: self.owner_psp,
        }
    }
}

/// Search state stored for Find First/Next operations
/// Each active search gets a unique ID stored in the DTA
#[allow(dead_code)] // Fields stored for debugging/future use
pub(crate) struct FindSearchState {
    /// Search pattern (FCB format or wildcard)
    pub(crate) pattern: FixedString<260>,
    /// Drive number
    pub(crate) drive: u8,
    /// Directory path being searched
    pub(crate) dir_path: FixedString<260>,
    /// Attribute filter from Find First
    pub(crate) attr_filter: u8,
    /// Matching entries found (8.3 filenames + attributes)
    ///
    /// Large directory listings need room beyond the previous 128-entry limit.
    /// Heap allocation keeps this capacity out of the parent `Dos` struct.
    pub(crate) entries: Box<FindSearchEntries>,
    /// Current index into entries
    pub(crate) index: usize,
}

impl Clone for FindSearchState {
    fn clone(&self) -> Self {
        let mut entries = FindSearchEntries::new_boxed();
        for entry in self.entries.iter() {
            entries.push(*entry);
        }

        Self {
            pattern: self.pattern,
            drive: self.drive,
            dir_path: self.dir_path,
            attr_filter: self.attr_filter,
            entries,
            index: self.index,
        }
    }
}

/// DOS state
pub struct Dos {
    /// Mounted drives (0=A, 1=B, 2=C, etc.)
    pub drives: BTreeMap<u8, Box<dyn VirtualDrive>>,
    /// Drive indices whose entry in `drives` was created by the shell's SUBST
    /// builtin (MDD-BUG-KILN-00634). SUBST may only delete mappings it created,
    /// so a real mount can never be unmounted by `subst X: /d`.
    pub subst_drives: std::collections::BTreeSet<u8>,
    /// DOS pathname retained for LFN-created SUBST mappings. Host roots remain
    /// the mounting source, but LFN true-name/query calls must report DOS
    /// syntax instead of leaking the host's native path.
    pub(crate) subst_targets: std::collections::BTreeMap<u8, String>,
    /// The host directory each SUBST drive maps, so a snapshot can recreate
    /// the mapping without asking the drive (MDD-BUG-KIL-00775).
    pub(crate) subst_roots: std::collections::BTreeMap<u8, PathBuf>,
    /// DOS version to report
    pub(crate) version_major: u8,
    pub(crate) version_minor: u8,
    /// Environment segment
    pub(crate) env_segment: u16,
    /// Disk Transfer Area segment:offset
    pub dta_segment: u16,
    pub dta_offset: u16,
    /// Disk Transfer Area linear address (computed at set-time for protected mode compatibility)
    pub(crate) dta_linear: usize,
    /// Highest offset in the DTA's segment: 0xFFFF for a real-mode or
    /// 16-bit caller, the whole 4 GiB for a 32-bit client, whose DTA offset
    /// can exceed the 16 bits kept in `dta_offset` (MDD-BUG-CRU-01393).
    pub(crate) dta_segment_limit: u32,
    /// Active find searches keyed by search ID (stored in DTA reserved bytes)
    pub(crate) find_searches: FixedVec<(u32, Box<FindSearchState>), 32>,
    /// Next search ID to assign
    pub(crate) next_search_id: u32,
    /// Active DOS 7 long-filename searches keyed by their explicit handle.
    pub(crate) lfn_find_searches: FixedVec<(u16, Box<LfnFindSearchState>), 32>,
    /// Next candidate LFN search handle; zero is always invalid.
    pub(crate) next_lfn_search_handle: u16,
    /// Current FCB search ID (FCB operations use separate DTA format)
    pub(crate) fcb_search_id: u32,
    /// Result form the active FCB search must publish (MDD-BUG-CRUCIBLE-00447):
    /// an extended FCB asks for the seven-byte-prefixed layout, and AH=12h has
    /// no FCB pointer of its own to re-derive it from.
    pub(crate) fcb_search_extended: bool,
    /// Search attribute echoed into the extended prefix.
    pub(crate) fcb_search_attr: u8,
    /// DOS conventional memory manager (MCB chain)
    pub dos_mem: DosMem,
    /// Current PSP segment
    pub(crate) current_psp: u16,
    /// Process stack for EXEC/exit
    pub(crate) process_stack: FixedVec<ProcessState, 16>,
    /// The EXEC return of each process that went resident (INT 21h 31h),
    /// keyed by its PSP. DOS keeps a TSR's parent linkage in its PSP, so a
    /// returns to that parent again and releases its memory.
    pub(crate) resident_returns: FixedVec<(u16, ProcessState), 8>,
    /// New file table system
    pub file_table: files::FileTable,
    /// Path resolver for DOS to host path translation
    pub path_resolver: files::PathResolver,
    /// DPMI host
    pub dpmi: Box<Dpmi>,
    /// XMS driver
    pub xms: Xms,
    /// Counter for generating unique temp filenames
    pub(crate) temp_file_counter: u32,
    /// Last program exit code (for INT 21h/4Dh)
    pub(crate) last_exit_code: u8,
    /// Last termination type: 0=normal, 1=Ctrl-C, 2=critical error, 3=TSR
    pub(crate) last_termination_type: u8,
    /// Pending critical error state (active during INT 24h handler execution)
    pub pending_critical_error: Option<PendingCriticalError>,
    /// Drives (0 = A:) fitted but holding no media, which a path call
    /// reports as drive not ready: a critical error (extended error 15h)
    /// for the session handler. This configured drive policy is not snapshotted.
    pub not_ready_drives: std::collections::BTreeSet<u8>,
    /// Select the alternate DOS sharing matrix and critical-error policy.
    /// Refused opens return AX=5 with extended error 20h; compatibility
    /// opens and deletes consult the critical-error handler first.
    /// This startup policy is not captured in snapshots.
    pub vshare: bool,
    /// Files held outside the DOS handle table, each read-only, deny-write.
    /// Consulted under the alternate sharing policy; not snapshotted.
    pub(crate) held_files: Vec<files::FileIdentity>,
    /// The session's hold generation `held_files` was last mirrored from.
    pub held_files_generation: u32,
    /// Memory allocation strategy (for INT 21h/58h)
    /// 0 = First fit, 1 = Best fit, 2 = Last fit
    /// High bit: 0x80 = high memory first, 0x40 = best fit in high
    pub(crate) memory_strategy: u8,
    /// UMB (Upper Memory Block) link state
    /// false = UMBs not part of DOS memory chain
    /// true = UMBs are part of DOS memory chain
    pub(crate) umb_linked: bool,
    /// Whether EMS should be visible through DOS device probes.
    pub(crate) ems_present: bool,
    /// Ctrl-Break/Ctrl-C check flag (INT 21h/33h)
    pub(crate) ctrl_break_check: bool,
    /// Pending extended key scan code (for function keys, arrows, etc.)
    /// When a key with ASCII=0 is read, we return 0 first, then the scan code
    pub(crate) pending_extended_scancode: Option<u8>,
    /// Extended error information (for INT 21h/59h)
    /// Format: (error_code, error_class, suggested_action, locus)
    pub(crate) extended_error: (u16, u8, u8, u8),
    /// Verify flag for disk writes (INT 21h/2Eh to set, 54h to get)
    pub(crate) verify_flag: bool,
    /// Network function handler
    pub(crate) network: network::NetworkHandler,
    /// Optional buffer to capture stdout (handle 1) and stderr (handle 2) output.
    /// When Some, DOS writes to these handles are appended to this buffer.
    /// Captures output for differential tests.
    pub(crate) stdout_capture: Option<Vec<u8>>,
    /// Country/locale information for INT 21h AH=38h, AH=65h, AH=66h
    pub(crate) country_info: country::DosCountryInfo,
    /// Optional synthetic overlay contents exposed through DOS file operations.
    /// When Some, INT 21h AH=3Dh open calls for GLIDE2X.OVL will be intercepted
    /// and served from this in-memory data instead of hitting the filesystem.
    pub glide_ovl_data: Option<std::sync::Arc<[u8]>>,
    /// In-memory files keyed by canonical DOS path.
    pub virtual_files: std::collections::BTreeMap<String, std::sync::Arc<[u8]>>,
    /// Accumulated HDD seek click count, drained by root crate after INT 21h dispatch
    pub hdd_seek_clicks: u8,
    /// Bytes in flight between the `AUX`/`PRN` character devices and COM1/LPT1,
    /// drained by the root crate after INT 21h dispatch. Shared with the device
    /// objects held in the SFT, which have no other way to reach `Dos`.
    pub device_bridge: filesystem::SharedDeviceBridge,
    /// Whether HDD seek sound emission is enabled (set by emulator menu toggle)
    pub hdd_sound_enabled: bool,
    /// Cached emulated date: (year, month, day, day_of_week).
    /// Seeded from the host clock once at boot, used by INT 21h AH=2A.
    pub emulated_date: (u16, u8, u8, u8),
    /// Exit code from the most recent DPMI child program termination.
    /// Set by terminate_process() when a DPMI program exits, checked by the
    /// emulator main loop to detect program completion.
    pub dpmi_program_exit_code: Option<u8>,
    /// When true, `int21()` skips `sync_iret_carry()` and `prep_input_wait()`
    /// because no IRET frame exists on the stack. Set by the DPMI HLE path
    /// before calling `int21()`, cleared after the call returns.
    pub skip_iret_sync: bool,
    /// True only while DPMI simulated-real-mode HLE dispatch is calling INT 21h.
    /// The CPU and DPMI host remain mechanically protected-mode, but DOS-visible
    /// vector services must use real-mode semantics.
    simulated_real_mode_hle: bool,
    /// Characters being accumulated for cooked-mode stdin line input (AH=3Fh handle 0).
    /// Grows as the user types; cleared when CR completes the line.
    pub(crate) stdin_line_acc: Vec<u8>,
    /// Completed cooked-mode stdin data ready for reading (includes CR+LF).
    /// AH=3Fh drains from this buffer before collecting new input.
    pub(crate) stdin_line_buf: Vec<u8>,
    /// Gremlin Step 1 hook: image CS of the most recent INT 21h AH=4Bh
    /// (EXEC) child to be loaded but not yet observed by the root crate.
    /// Set in `exec_load_com` / `exec_load_exe`; drained by
    /// `Emulator::step_inner` (root crate) which forwards it to
    /// `EpochState::record_image_load`. This indirection keeps
    /// `gp64-dos` free of a `crate::gremlin` dependency. `None` when
    /// no child load has happened since the last drain.
    pub last_exec_image_cs: Option<u16>,

    /// Optional bounded matcher for an exact guest-work checkpoint. Armed
    /// only by the hidden gremlin-spike CLI; normal runs retain `None`.
    dos_write_marker: Option<ByteStreamMarker>,

    /// Per-instance accumulator for INT 21h AH=06 (Direct Console I/O,
    /// output mode). Only used when `RUST_LOG=gp64::dos::stdout=debug`
    /// is set; otherwise the buffer stays empty and allocates nothing. See
    /// `wrk_docs/2026.05.23 - HLD - INT 21h AH=06 console output log routing.md`.
    pub(crate) ah06_line_buf: Ah06LineBuffer,

    /// Fill byte for the DOS BSS-init pass run before each EXEC
    /// (`INT 21h AH=4Bh`) child load. Mirrors the root crate's
    /// `Emulator::bss_init` setting (kept as a raw byte here so the
    /// `gp64-dos` crate has no dependency back on the root). Default
    /// `0xCF` (IRET) matches `BssInitMode::default()`. The top-level
    /// load path (`load_with_drive_update`) runs its own fill from the
    /// root crate and does not consult this field. See HLD
    /// `wrk_docs/2026.05.17 - HLD - DOS BSS init policy.md` §4 / §10.
    pub bss_init_fill_byte: u8,
}

/// Set `name` in an ordered DOS environment the way `DOS command interpreter`'s `SET`
/// does, byte for byte (MDD-BUG-FLU-03529): the name is upper-cased (ASCII
/// letters only) and matched without ASCII case; an existing variable keeps
/// its place and takes the upper-cased name and the new value; a new one goes
/// to the end; an empty value deletes every variable of that name and never
/// stores `NAME=`. The master environment block and batch SET share this rule.
pub fn set_env_var(vars: &mut Vec<(Vec<u8>, Vec<u8>)>, name: &[u8], value: &[u8]) {
    let name = name.to_ascii_uppercase();
    if value.is_empty() {
        vars.retain(|(existing, _)| !existing.eq_ignore_ascii_case(&name));
        return;
    }
    match vars
        .iter_mut()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(&name))
    {
        Some(entry) => *entry = (name, value.to_vec()),
        None => vars.push((name, value.to_vec())),
    }
}

/// Build sound-related environment variable strings from hardware config.
pub fn build_sound_env_vars(config: &SoundEnvConfig) -> Vec<String> {
    let mut vars = Vec::new();
    if let Some(sb) = &config.sound_blaster {
        let mut blaster = format!("BLASTER=A{:X} I{} D{}", sb.port, sb.irq, sb.dma);
        if sb.hdma != 0 {
            blaster.push_str(&format!(" H{}", sb.hdma));
        }
        blaster.push_str(&format!(" T{}", sb.sb_type));
        if sb.emu8000_port != 0 {
            blaster.push_str(&format!(" E{:X}", sb.emu8000_port));
        }
        if sb.mpu_port != 0 {
            blaster.push_str(&format!(" P{:X}", sb.mpu_port));
        }
        vars.push(blaster);
    }
    if let Some(gus) = &config.gravis_ultrasound {
        vars.push(format!(
            "ULTRASND={:X},{},{},{},{}",
            gus.port, gus.play_dma, gus.rec_dma, gus.play_irq, gus.rec_irq
        ));
        vars.push("ULTRADIR=C:\\ULTRASND".to_string());
    }
    vars
}

/// Build Glide-related environment variables for 3dfx HLE.
///
/// When Glide is enabled, sets:
/// - `GLIDE=600` — I/O port for Glide dispatch (hex, parsed by OVL via strtol)
/// - `SST_GRXCLK=57` — graphics clock speed in MHz (standard SST-1)
/// - `FX_GLIDE_NO_SPLASH=1` — suppresses the 3dfx splash screen
pub fn build_glide_env_vars(glide_enabled: bool) -> Vec<String> {
    if !glide_enabled {
        return Vec::new();
    }
    vec![
        "GLIDE=600".to_string(),
        "SST_GRXCLK=57".to_string(),
        "FX_GLIDE_NO_SPLASH=1".to_string(),
    ]
}

impl Dos {
    /// Set the simulated-real-mode HLE INT 21h context and return the old value.
    ///
    /// This is used by the root emulator's direct DPMI HLE path. It deliberately
    /// does not alter CPU protected-mode state or DPMI process state.
    pub fn set_simulated_real_mode_hle(&mut self, enabled: bool) -> bool {
        let previous = self.simulated_real_mode_hle;
        self.simulated_real_mode_hle = enabled;
        previous
    }

    pub fn new() -> Self {
        let dos_mem = DosMem::new(first_mcb_seg_override());

        let mut drives: BTreeMap<u8, Box<dyn VirtualDrive>> = BTreeMap::new();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        drives.insert(2, Box::new(HostDrive::new(cwd)));

        Self {
            drives,
            subst_drives: std::collections::BTreeSet::new(),
            subst_targets: std::collections::BTreeMap::new(),
            subst_roots: std::collections::BTreeMap::new(),
            version_major: DEFAULT_DOS_VERSION.0,
            version_minor: DEFAULT_DOS_VERSION.1,
            env_segment: 0x0000,
            dta_segment: 0x0100, // Default DTA in PSP
            dta_offset: 0x0080,
            dta_linear: Memory::seg_offset_to_linear(0x0100, 0x0080), // Real mode default
            dta_segment_limit: 0xFFFF,
            find_searches: FixedVec::new(),
            next_search_id: 1, // Start at 1 so 0 means "no valid search"
            lfn_find_searches: FixedVec::new(),
            next_lfn_search_handle: 1,
            fcb_search_id: 0, // No active FCB search
            fcb_search_extended: false,
            fcb_search_attr: 0,
            dos_mem,
            current_psp: 0x0100, // Default PSP
            process_stack: FixedVec::new(),
            resident_returns: FixedVec::new(),
            file_table: files::FileTable::new(),
            path_resolver: files::PathResolver::new(),
            dpmi: Box::new(Dpmi::new()),
            xms: Xms::new(),
            temp_file_counter: 0,
            last_exit_code: 0,
            last_termination_type: 0,
            pending_critical_error: None,
            not_ready_drives: std::collections::BTreeSet::new(),
            vshare: false,
            held_files: Vec::new(),
            held_files_generation: 0,
            memory_strategy: 0, // First fit (low memory)
            umb_linked: false,  // UMBs not linked by default
            ems_present: false,
            ctrl_break_check: false,
            pending_extended_scancode: None,
            extended_error: (0, 0, 0, 0), // No error
            verify_flag: false,           // Verify off by default
            network: network::NetworkHandler::new(),
            stdout_capture: None,
            country_info: country::DosCountryInfo::us(),
            glide_ovl_data: None,
            virtual_files: std::collections::BTreeMap::new(),
            hdd_seek_clicks: 0,
            device_bridge: filesystem::SharedDeviceBridge::default(),
            hdd_sound_enabled: false,
            emulated_date: boot_date_from_host(),
            dpmi_program_exit_code: None,
            skip_iret_sync: false,
            simulated_real_mode_hle: false,
            stdin_line_acc: Vec::new(),
            stdin_line_buf: Vec::new(),
            last_exec_image_cs: None,
            dos_write_marker: None,
            ah06_line_buf: Ah06LineBuffer::default(),
            // Default to BssInitMode::Iret (0xCF). The root crate
            // overwrites this from `Emulator::bss_init` once the user's
            // CLI / env-var preference has been resolved.
            bss_init_fill_byte: 0xCF,
        }
    }

    /// SUBST: map a drive letter to a host directory (`subst X: <path>`,
    /// MDD-BUG-KILN-00634). Refuses a letter that is already mounted — the
    /// same rule the DOS model applies — so a SUBST can never silently displace a
    /// real mount; delete an existing SUBST mapping first (`subst X: /d`).
    /// Error strings are DOS-style messages ready for teletype output.
    pub fn subst_create(&mut self, drive_idx: u8, host_dir: PathBuf) -> Result<(), String> {
        self.subst_create_with_target(drive_idx, host_dir, None)
    }

    /// Create a SUBST mapping and optionally retain the DOS pathname supplied
    /// by an LFN caller for true-name and query results.
    pub(crate) fn subst_create_with_target(
        &mut self,
        drive_idx: u8,
        host_dir: PathBuf,
        target: Option<String>,
    ) -> Result<(), String> {
        if drive_idx >= 26 {
            return Err("Invalid parameter".to_string());
        }
        let letter = (b'A' + drive_idx) as char;
        if self.subst_drives.contains(&drive_idx) {
            return Err(format!("Drive already SUBSTed - {letter}:"));
        }
        if self.drives.contains_key(&drive_idx) {
            return Err(format!("Invalid parameter - {letter}:"));
        }
        self.drives
            .insert(drive_idx, Box::new(HostDrive::new(host_dir.clone())));
        self.subst_drives.insert(drive_idx);
        self.subst_roots.insert(drive_idx, host_dir);
        if let Some(target) = target {
            self.subst_targets.insert(drive_idx, target);
        }
        Ok(())
    }

    /// SUBST /D: delete a SUBST-created mapping. Refuses drives that were not
    /// created by `subst_create` (real DOS reports `Invalid parameter` there
    /// too — the launcher idiom `subst a: /d > nul` relies on that being a
    /// harmless failure), and refuses the current drive, matching real DOS.
    pub fn subst_delete(&mut self, drive_idx: u8) -> Result<(), String> {
        let letter = (b'A' + drive_idx.min(25)) as char;
        if !self.subst_drives.contains(&drive_idx) {
            return Err(format!("Invalid parameter - {letter}:"));
        }
        if self.path_resolver.get_current_drive() == drive_idx {
            return Err(format!("Invalid parameter - {letter}:"));
        }
        self.subst_drives.remove(&drive_idx);
        self.subst_targets.remove(&drive_idx);
        self.subst_roots.remove(&drive_idx);
        self.drives.remove(&drive_idx);
        Ok(())
    }

    /// The current SUBST mappings as (drive index, host root) pairs, for the
    /// bare `subst` listing.
    pub fn subst_mappings(&self) -> Vec<(u8, PathBuf)> {
        self.subst_drives
            .iter()
            .filter_map(|&idx| {
                self.drives
                    .get(&idx)
                    .and_then(|d| d.resolve_host_path("\\"))
                    .map(|root| (idx, root))
            })
            .collect()
    }

    /// Enable stdout/stderr capture. Subsequent DOS writes to handles 1 and 2
    /// will be appended to an internal buffer (in addition to VGA teletype).
    pub fn enable_stdout_capture(&mut self) {
        self.stdout_capture = Some(Vec::new());
    }

    /// Retrieve and clear captured stdout/stderr bytes.
    /// Returns the accumulated bytes, or an empty vec if capture was not enabled.
    pub fn take_stdout_capture(&mut self) -> Vec<u8> {
        self.stdout_capture.take().unwrap_or_default()
    }

    /// Arm a one-shot bounded matcher over successful INT 21h/AH=40h output.
    pub fn configure_dos_write_marker(&mut self, marker: &[u8]) -> Result<(), &'static str> {
        self.dos_write_marker = Some(ByteStreamMarker::new(marker)?);
        Ok(())
    }

    /// Consume a newly recognized marker. Recognition remains permanently
    /// complete, so repeated guest output cannot create a second checkpoint.
    pub fn take_dos_write_marker_hit(&mut self) -> bool {
        let Some(marker) = self.dos_write_marker.as_mut() else {
            return false;
        };
        marker.take_hit()
    }

    /// Returns a clone of the synthetic GLIDE2X.OVL bytes, if configured.
    /// Used by INT 21h AH=3Dh to intercept file opens for GLIDE2X.OVL.
    pub(crate) fn get_glide_ovl_bytes(&self) -> Option<std::sync::Arc<[u8]>> {
        self.glide_ovl_data.clone()
    }

    /// Normalise a DOS path (with drive letter) to the canonical key used by
    /// `virtual_files` — uppercased, backslash separators, drive prefix.
    /// Returns `None` if the input is empty or contains no drive letter.
    fn virtual_file_key(drive: u8, dos_path: &str) -> String {
        // dos_path is expected to be an already-resolved absolute path
        // upper-case the whole thing so lookup is case-insensitive.
        let drive_letter = (b'A' + drive) as char;
        let mut path = dos_path.replace('/', "\\");
        if !path.starts_with('\\') {
            path.insert(0, '\\');
        }
        format!("{drive_letter}:{}", path.to_uppercase())
    }

    /// Register an in-memory virtual file at the given canonical DOS path.
    /// The bytes are served via `SyntheticFile` when INT 21h AH=3Dh opens
    /// the same path. Overwrites any existing entry for that path.
    ///
    /// `dos_canonical_path` must include a drive letter (e.g.
    pub fn register_virtual_file(
        &mut self,
        dos_canonical_path: &str,
        bytes: impl Into<std::sync::Arc<[u8]>>,
    ) {
        let key = dos_canonical_path.replace('/', "\\").to_uppercase();
        self.virtual_files.insert(key, bytes.into());
    }

    /// Look up a registered virtual file by drive + resolved DOS path.
    /// Returns a SHARED handle on the bytes if a match exists — cloning the
    /// `Arc` is a refcount bump, so N simultaneous opens of a tens-of-MiB
    /// decompressed image cost one allocation, not N (MDD-BUG-CRUCIBLE-00380).
    ///
    /// `dos_path` is the resolved absolute DOS path WITHOUT the drive
    /// shape `PathResolver::resolve` returns.
    pub fn get_virtual_file_bytes(
        &self,
        drive: u8,
        dos_path: &str,
    ) -> Option<std::sync::Arc<[u8]>> {
        let key = Self::virtual_file_key(drive, dos_path);
        self.virtual_files.get(&key).cloned()
    }

    /// Returns true if a virtual file is registered for the given drive +
    /// resolved DOS path (without cloning the payload).
    pub fn has_virtual_file(&self, drive: u8, dos_path: &str) -> bool {
        let key = Self::virtual_file_key(drive, dos_path);
        self.virtual_files.contains_key(&key)
    }

    /// Accumulate HDD seek clicks (if HDD sound is enabled).
    /// Uses `saturating_add` to prevent wrapping past 255.
    pub fn emit_hdd_clicks(&mut self, clicks: u8) {
        if self.hdd_sound_enabled {
            self.hdd_seek_clicks = self.hdd_seek_clicks.saturating_add(clicks);
        }
    }

    /// Apply the explicit DOS country, or the same US default on every host.
    ///
    /// - `country_override`: if Some, use this DOS country code from `--country`.
    ///   If the code is not in the table, logs a warning
    ///   and falls back to US defaults.
    /// - If None, uses US defaults.
    pub fn apply_country_settings(&mut self, country_override: Option<u16>) {
        if let Some(code) = country_override {
            if let Some(info) = country::country_from_code(code) {
                self.country_info = info;
            } else {
                tracing::warn!("Unknown DOS country code {code}, falling back to US defaults");
                self.country_info = country::DosCountryInfo::us();
            }
        } else {
            self.country_info = country::DosCountryInfo::us();
        }
    }

    /// Get the current DOS country/locale information.
    pub fn country_info(&self) -> &country::DosCountryInfo {
        &self.country_info
    }

    /// Set DTA address and recalculate cached linear address.
    pub fn set_dta(&mut self, segment: u16, offset: u16) {
        self.dta_segment = segment;
        self.dta_offset = offset;
        self.dta_linear = Memory::seg_offset_to_linear(segment, offset);
        self.dta_segment_limit = 0xFFFF;
    }

    /// Set DTA address using a protected-mode selector and pre-computed linear base.
    ///
    /// In protected mode the segment field is a selector (not a real-mode segment),
    /// so `seg_offset_to_linear` would produce a wrong linear address. This variant
    /// accepts the selector's base address directly.
    pub fn set_dta_pm(&mut self, selector: u16, offset: u16, selector_base: u32) {
        self.dta_segment = selector;
        self.dta_offset = offset;
        self.dta_linear = selector_base as usize + offset as usize;
        self.dta_segment_limit = 0xFFFF;
    }

    /// Get the cached DTA linear address.
    pub fn dta_linear(&self) -> usize {
        self.dta_linear
    }

    // ========================================================================
    // Protected-mode-aware register accessors for DPMI clients
    // ========================================================================
    //
    // for offsets in INT 21h calls. 16-bit DPMI clients and real-mode programs
    // use 16-bit register halves. These helpers return the correct width.

    pub fn env_segment(&self) -> u16 {
        self.env_segment
    }

    pub fn current_psp(&self) -> u16 {
        self.current_psp
    }

    pub fn verify_flag(&self) -> bool {
        self.verify_flag
    }

    pub fn set_verify_flag(&mut self, flag: bool) {
        self.verify_flag = flag;
    }

    /// DOS version reported by INT 21h/30h.
    pub fn version(&self) -> (u8, u8) {
        (self.version_major, self.version_minor)
    }

    /// Set the DOS version reported by INT 21h/30h.
    pub fn set_version(&mut self, major: u8, minor: u8) {
        self.version_major = major;
        self.version_minor = minor;
    }

    /// Set the UMB link state and rewrite the MCB chain to match.
    pub fn set_umb_linked(&mut self, memory: &mut Memory, linked: bool) {
        self.umb_linked = linked;
        self.dos_mem.write_umb_link(memory, linked);
    }

    /// Disable UMBs entirely — destroy UMB blocks and unlink from MCB chain.
    pub fn disable_umb(&mut self, memory: &mut Memory) {
        self.umb_linked = false;
        self.dos_mem.deinit_umb(memory);
    }

    /// Override whether EMS is visible through DOS device-name probes.
    pub fn set_ems_present(&mut self, _present: bool) {
        self.ems_present = false;
    }

    /// Return whether EMS is visible through DOS device-name probes.
    pub fn ems_present(&self) -> bool {
        self.ems_present
    }

    pub fn ctrl_break_check(&self) -> bool {
        self.ctrl_break_check
    }

    pub fn set_ctrl_break_check(&mut self, flag: bool) {
        self.ctrl_break_check = flag;
    }

    /// Set last exit code (for batch resumption when INT 21h/4Ch path is bypassed).
    pub fn set_last_exit_code(&mut self, code: u8) {
        self.last_exit_code = code;
    }

    /// Last program exit code, as reported by INT 21h AH=4Dh (Get Return Code).
    pub fn last_exit_code(&self) -> u8 {
        self.last_exit_code
    }

    /// Set last termination type (for batch resumption when INT 21h/4Ch path is bypassed).
    /// 0=normal, 1=Ctrl-C, 2=critical error, 3=TSR
    pub fn set_last_termination_type(&mut self, ttype: u8) {
        self.last_termination_type = ttype;
    }

    /// Last termination type: 0=normal, 1=Ctrl-C, 2=critical error, 3=TSR.
    pub fn last_termination_type(&self) -> u8 {
        self.last_termination_type
    }

    /// Restore current_psp without resetting DTA (for checkpoint restore).
    pub fn restore_current_psp(&mut self, segment: u16) {
        self.current_psp = segment;
    }

    pub fn set_current_psp(&mut self, segment: u16) {
        self.current_psp = segment;
        // Default DTA lives at PSP:0080
        self.dta_segment = segment;
        self.dta_offset = 0x0080;
        // Real mode calculation - this is called before entering protected mode
        self.dta_linear = Memory::seg_offset_to_linear(segment, 0x0080);
        self.dta_segment_limit = 0xFFFF;
    }

    /// Create Program Segment Prefix
    ///
    /// When `parent_psp` is `Some(parent_seg)`, the parent's 20-byte JFT is
    /// copied into the child PSP and SFT reference counts are incremented for
    /// each inherited handle. When `None` (root process), the default JFT
    /// (stdin/stdout/stderr/stdaux/stdprn) is written.
    ///
    /// The full 256-byte PSP region is zero-filled BEFORE any field-specific
    /// writes. This matches the DOS model `DOS_PSP::MakeNew()` verbatim
    /// (`for (i=0; i < sizeof(sPSP); i++) mem_writeb(pt+i, 0); /* Clear it
    /// first */`) and closes the BSS-init-fill (0xCF) leakage into PSP:0x04,
    /// 0x3C-0x4F, 0x53-0x5B, 0x5C-0x6B (FCB1), 0x6C-0x7B (FCB2), and
    /// 0x7C-0x7F that broke programs which walk their PSP looking for argv
    /// (C runtime fallback, DOS extenders, etc.). See HLD
    /// progname.md`.
    pub fn create_psp(
        &mut self,
        memory: &mut Memory,
        segment: u16,
        mem_end_segment: u16,
        env_segment: u16,
        parent_psp: Option<u16>,
        args: &[String],
    ) {
        let base = Memory::seg_offset_to_linear(segment, 0);

        // Zero-fill the entire 256-byte PSP region BEFORE any field-specific
        // writes. Matches the DOS model `DOS_PSP::MakeNew()` verbatim. Field
        // writes that follow (INT 20h at 0x00-0x01, FAR CALL at 0x05-0x09,
        // JFT at 0x18-0x2B, env-seg at 0x2C, JFT-size at 0x32, JFT-ptr at
        // 0x34, INT 21h trampoline at 0x50-0x52, cmdtail length at 0x80,
        // cmdtail bytes at 0x81..) intentionally overwrite many of these
        // zeros — order matters: zero FIRST, field writes SECOND. Reversing
        // it would shadow field writes back to zero.
        for off in 0..0x100usize {
            memory.write_u8(base + off, 0);
        }

        // INT 20h at start (CD 20)
        memory.write_u8(base, 0xCD);
        memory.write_u8(base + 1, 0x20);

        // Memory size in paragraphs / end segment (top of memory for this PSP)
        memory.write_u16(base + 2, mem_end_segment);

        // CP/M-heritage CALL 5 entry at PSP:05h (MDD-BUG-KILN-00645).
        //
        // PSP:0006 is TWO things at once, and that overlap is deliberate in
        // MS-DOS: it is the offset operand of the `CALL FAR` at PSP:0005 AND
        // the CP/M "number of bytes available in the first segment" field. A
        // program that sizes its own segment reads the word at PSP:0006.
        //
        // The byte count saturates at the largest value representable by
        // this DOS field. The overlapping CALL FAR operand uses that count
        // and the CP/M dispatch segment. The INT 21h/RETF thunk at PSP:0050
        // is populated separately below.
        let bytes_in_segment = u32::from(mem_end_segment.saturating_sub(segment)) * 16;
        memory.write_u8(base + 0x05, 0x9A); // CALL FAR
        memory.write_u16(base + 0x06, bytes_in_segment.min(0xFFFF) as u16);
        memory.write_u16(base + 0x08, 0x000C); // CP/M dispatch segment, as MS-DOS writes

        // File handle table (JFT) — 20 entries at PSP:18h
        if let Some(parent_seg) = parent_psp {
            // Inherit the parent's JFT bytes, which programs read. They are a
            // guest-visible copy only: the emulator keeps one handle table
            // that parent and child share, so inheriting takes no SFT
            // reference. Counting one here leaked it on every EXEC, since
            // nothing releases it, and a file the shell held open across
            // children (a batch `>> LOG`) could never be reopened.
            let parent_base = Memory::seg_offset_to_linear(parent_seg, 0);
            for i in 0..20 {
                let sft_index = memory.read_u8(parent_base + 0x18 + i);
                memory.write_u8(base + 0x18 + i, sft_index);
            }
        } else {
            // Root process: default JFT
            for i in 0..20 {
                memory.write_u8(base + 0x18 + i, 0xFF);
            }
            // Standard handles (DOS-4: correct SFT mapping)
            memory.write_u8(base + 0x18, 0x00); // stdin  -> SFT 0
            memory.write_u8(base + 0x19, 0x01); // stdout -> SFT 1
            memory.write_u8(base + 0x1A, 0x02); // stderr -> SFT 2
            memory.write_u8(base + 0x1B, 0x03); // stdaux -> SFT 3
            memory.write_u8(base + 0x1C, 0x04); // stdprn -> SFT 4
        }

        // Environment segment
        memory.write_u16(base + PSP_ENVIRONMENT_SEG, env_segment);

        // JFT size at PSP:32h (20 entries)
        memory.write_u16(base + 0x32, 20);

        // JFT pointer at PSP:34h (far pointer to JFT at PSP:18h)
        memory.write_u16(base + 0x34, 0x0018); // offset
        memory.write_u16(base + 0x36, segment); // segment

        // FCB 1 (PSP:0x5C) and FCB 2 (PSP:0x6C) are left zeroed here (from
        // the 256-byte zero-fill above). The EXEC handler (see
        // `exec_copy_fcbs_to_psp` in `int21/process_ops.rs`) copies the
        // parent's parsed FCBs over them per RBIL INT 21h/AH=4Bh, and the
        // command-interpreter load path parses the tail into them via
        // `parse_default_fcbs_from_args` (MDD-BUG-KILN-00633). Programs
        // that walk their own PSP looking for argv (C runtime fallback,
        // some DOS extenders) thus see zero (guest EXEC with no FCBs
        // supplied), the parent's parsed FCB, or the interpreter's parse of
        // the tail — never the BSS-init 0xCF residue that previously
        // leaked through.

        // INT 21h/RETF trampoline at PSP:50h
        memory.write_u8(base + 0x50, 0xCD); // INT 21h
        memory.write_u8(base + 0x51, 0x21);
        memory.write_u8(base + 0x52, 0xCB); // RETF

        // Command tail
        // Format: [len: u8] [space] [args...] [CR]
        let mut cmd_tail = String::new();
        if !args.is_empty() {
            cmd_tail.push(' '); // Leading space
            cmd_tail.push_str(&args.join(" "));
        }

        let bytes = cmd_tail.as_bytes();
        let len = bytes.len().min(126) as u8; // Max 126 chars + CR

        memory.write_u8(base + PSP_COMMAND_LINE_LEN as usize, len);
        for (i, &byte) in bytes.iter().enumerate().take(len as usize) {
            memory.write_u8(base + PSP_COMMAND_LINE + i, byte);
        }
        memory.write_u8(base + PSP_COMMAND_LINE + len as usize, 0x0D); // CR terminator
    }

    /// DOS command interpreter's pre-EXEC step (MDD-BUG-KILN-00633): parse the first two
    /// whitespace-delimited command-tail arguments into the child PSP's two
    /// default FCBs at PSP:5Ch / PSP:6Ch. Early DOS programs written against
    /// the FCB API take argv[1]/argv[2] from these; leaving them zero made
    /// e.g. an FCB-based application see an empty disk-image name and exit 2.
    ///
    /// Only the command-interpreter load path calls this (mirroring the DOS model,
    /// whose shell always builds FCB1/FCB2 via `FCB_Parsename`). A guest EXEC
    /// (INT 21h AH=4Bh) instead copies the FCBs its parameter block supplies —
    /// see `exec_copy_fcbs_to_psp` — and keeps zeros when none are given.
    ///
    /// Missing arguments parse as real DOS parses an empty string: default
    /// drive (0) and an all-blank name, never zeros.
    pub fn parse_default_fcbs_from_args(&self, memory: &mut Memory, psp_seg: u16, args: &[String]) {
        let base = Memory::seg_offset_to_linear(psp_seg, 0);
        for (slot, fcb_off) in [(0usize, 0x5Cusize), (1, 0x6C)] {
            let arg = args.get(slot).map(String::as_str).unwrap_or("");
            let (parsed, _) = Self::parse_fcb_name(arg.as_bytes());
            for (i, b) in parsed.iter().enumerate() {
                memory.write_u8(base + fcb_off + i, *b);
            }
        }
    }

    /// Parse a filename into the 12-byte FCB parse result: drive byte
    /// (0 = default, 1 = A:) followed by the blank-padded 8.3 name — INT 21h
    /// AH=29h semantics, including `*` expanding to `?` for the rest of its
    /// field and parsing stopping at an invalid FCB char. Also returns the
    /// number of input bytes consumed so the AH=29h arm can advance SI past
    /// the parsed text. Shared by that interrupt arm and the DOS command interpreter
    /// pre-EXEC default-FCB fill (MDD-BUG-CRU-01391).
    pub(crate) fn parse_fcb_name(bytes: &[u8]) -> ([u8; 12], usize) {
        // Characters that terminate an FCB filename field per AH=29h.
        const INVALID: &[u8] = b":;,=+<>|\"[]/\\";

        let mut fcb = [0u8; 12];
        fcb[1..12].fill(b' ');

        let mut pos = 0usize;

        // Optional drive prefix "X:".
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            fcb[0] = bytes[0].to_ascii_uppercase() - b'A' + 1;
            pos = 2;
        }

        // Name field (8 chars at fcb[1..9]), then extension (3 at fcb[9..12]).
        let mut field_start = 1usize;
        let mut field_len = 8usize;
        let mut idx = 0usize;
        while pos < bytes.len() {
            let c = bytes[pos];
            if c == b'.' {
                if field_start == 1 {
                    // Switch from name to extension.
                    field_start = 9;
                    field_len = 3;
                    idx = 0;
                    pos += 1;
                    continue;
                }
                break; // a second '.' terminates the parse
            }
            if c <= b' ' || INVALID.contains(&c) {
                break;
            }
            if c == b'*' {
                while idx < field_len {
                    fcb[field_start + idx] = b'?';
                    idx += 1;
                }
                pos += 1;
                continue;
            }
            if idx < field_len {
                fcb[field_start + idx] = c.to_ascii_uppercase();
                idx += 1;
            }
            pos += 1;
        }
        (fcb, pos)
    }

    /// Initialize all DOS kernel structures (List of Lists, SDA, device
    /// chain, CDS stub, FCB SFT, DPB chain, disk buffer head, secondary
    /// SFT) in a single pass.
    ///
    /// Stable layout and field values let programs walk DOS kernel internals.
    pub fn init_kernel_structures(&self, memory: &mut Memory) {
        // IHSEG is a reserved 1-paragraph region that stays all zeros;
        // the DOS model reserves it before the InfoBlock so HMA callers can
        // find kernel entry points via a well-known segment. Clear it
        // here so a previously-dirty page cannot leak stale bytes.
        let ihseg_base = Memory::seg_offset_to_linear(DOS_IHSEG, 0);
        for i in 0..0x10 {
            memory.write_u8(ihseg_base + i, 0);
        }

        self.init_infoblock(memory);
        self.init_sda(memory);
        self.init_con_device(memory);
        self.init_cds_stub(memory);
        self.init_fcb_sft(memory);
        self.init_dpb_chain(memory);
        self.init_disk_buffer_head(memory);
        self.init_secondary_sft(memory);
        let metadata_base = Memory::seg_offset_to_linear(METADATA_SEG, 0);
        memory.write_u8(metadata_base + usize::from(MEDIA_ID_OFFSET), 0xF8);
        memory.write_u16(metadata_base + usize::from(DBCS_TABLE_OFFSET), 0);
    }

    /// Fill the InfoBlock segment (0x32 paragraphs = 0x320 bytes) with
    /// 0xFF, then write the List of Lists fields and the primary SFT
    /// header/entries at offset 0xCC.
    ///
    /// The initial 0xFF fill matches the DOS model and is load-bearing: the
    /// LoL's `activeClock` and `activeCon` fields (+0x08 / +0x0C from
    /// the LoL pointer) are never written again and intentionally stay
    /// at 0xFFFFFFFF.
    #[allow(clippy::identity_op)] // offsets kept explicit to mirror the HLD field table
    fn init_infoblock(&self, memory: &mut Memory) {
        let seg_base = Memory::seg_offset_to_linear(INFOBLOCK_SEG, 0);

        // Three-pass init mirroring the DOS model's `DOS_InfoBlock::SetLocation`
        // (`dos_classes.cpp:72-158`):
        //
        // 1. Memset the entire 0x320-byte InfoBlock segment to 0xFF. This
        //    leaves activeClock / activeCon (LoL+0x08, +0x0C — segment
        //    offsets 0x2A, 0x2E) as the load-bearing 0xFFFFFFFF sentinel
        //    that the DOS model never overwrites.
        for i in 0..0x320 {
            memory.write_u8(seg_base + i, 0xFF);
        }
        // 2. Zero the first 14 bytes of the InfoBlock struct (segment
        //    offsets 0x00..0x0D) per `dos_classes.cpp:77`. activeClock /
        //    activeCon sit at segment offsets 0x2A / 0x2E and are well
        //    outside this range, so they stay 0xFF from pass 1.
        for i in 0..14 {
            memory.write_u8(seg_base + i, 0x00);
        }
        // 3. Zero the primary SFT region (segment offsets 0xCC..0x200)
        //    per `dos_classes.cpp:131`, so the SFT header and entries
        //    start from a clean slate before we write the filename
        //    DWORDs.
        for i in 0xCC..0x200 {
            memory.write_u8(seg_base + i, 0x00);
        }
        let lol_base = Memory::seg_offset_to_linear(LOL_SEGMENT, LOL_OFFSET);

        // -0x02: first MCB segment (used by programs that know the LoL
        // layout and peek two bytes below the advertised pointer).
        memory.write_u16(lol_base.wrapping_sub(2), self.dos_mem.first_mcb_segment());

        // +0x00: far pointer to first DPB (DPB_SEG:0000)
        memory.write_u16(lol_base + 0x00, 0x0000);
        memory.write_u16(lol_base + 0x02, DPB_SEG);

        // +0x04: far pointer to first SFT (InfoBlock_seg:00CC)
        memory.write_u16(lol_base + 0x04, 0x00CC);
        memory.write_u16(lol_base + 0x06, INFOBLOCK_SEG);

        // +0x08 activeClock and +0x0C activeCon stay 0xFFFFFFFF from the
        // 0xFF pre-fill — the DOS model never writes them.

        // +0x10: maximum sector length (any block device)
        memory.write_u16(lol_base + 0x10, 0x0200);

        // +0x12: disk info buffer pointer (InfoBlock_seg:006D self-ref)
        memory.write_u16(lol_base + 0x12, 0x006D);
        memory.write_u16(lol_base + 0x14, INFOBLOCK_SEG);

        // +0x16: current directory structure (CDS_SEG:0000)
        memory.write_u16(lol_base + 0x16, 0x0000);
        memory.write_u16(lol_base + 0x18, CDS_SEG);

        // +0x1A: FCB table (FCBSFT_SEG:0000)
        memory.write_u16(lol_base + 0x1A, 0x0000);
        memory.write_u16(lol_base + 0x1C, FCBSFT_SEG);

        // +0x1E: protected FCB count
        memory.write_u16(lol_base + 0x1E, 0);

        // +0x20: block device count. the DOS model initializes this to 0
        // (`dos_classes.cpp:93`) and refreshes it on every AH=52h call
        // (`dos.cpp:2433`). We do the same — the AH=52h handler in
        // `int21/mod.rs` updates LoL+0x20 with `self.drives.len()` on
        // each request.
        memory.write_u8(lol_base + 0x20, 0);

        // +0x21: LASTDRIVE (the DOS model default = 1 for "A")
        memory.write_u8(lol_base + 0x21, DOS_LASTDRIVE);

        // +0x22: nulNextDriver -> CONDRV_SEG:0000
        memory.write_u16(lol_base + 0x22, 0x0000);
        memory.write_u16(lol_base + 0x24, CONDRV_SEG);

        // +0x26: NUL device attributes (char | special | stdin | stdout)
        memory.write_u16(lol_base + 0x26, 0x8004);

        // +0x28: strategy / interrupt entry points (both 0)
        memory.write_u16(lol_base + 0x28, 0x0000);
        memory.write_u16(lol_base + 0x2A, 0x0000);

        // +0x2C: NUL device name (8 bytes, padded with spaces)
        let nul_name = b"NUL     ";
        for (i, &b) in nul_name.iter().enumerate() {
            memory.write_u8(lol_base + 0x2C + i, b);
        }

        // +0x34: number of JOIN'd drives
        memory.write_u8(lol_base + 0x34, 0);

        // +0x35..+0x3E: unused gap in our LoL. After the three-pass
        // InfoBlock init, segment offsets 0x57..0x60 sit at 0xFF from
        // the pass-1 memset (outside both the first-14-byte zero zone
        // and the SFT zero zone), so we explicitly clear them here to
        // give the gap deterministic zero contents.
        for off in 0x35..=0x3E {
            memory.write_u8(lol_base + off, 0);
        }

        // +0x3F: BUFFERS x (primary buffer count)
        memory.write_u16(lol_base + 0x3F, DOS_BUFFERS_X);

        // +0x41: BUFFERS y (secondary buffer count)
        memory.write_u16(lol_base + 0x41, DOS_BUFFERS_Y);

        // +0x43: boot drive (3 = C:)
        memory.write_u8(lol_base + 0x43, 3);

        // +0x44: useDwordMov flag (CPU >= 386, always 1 for us)
        memory.write_u8(lol_base + 0x44, 1);

        // +0x45: extended memory size in KB (above 1MB)
        // Clamp to u16 — the DOS model reports only the low 16 bits here.
        let ext_kb = self.xms.total_kb().min(u16::MAX as u32) as u16;
        memory.write_u16(lol_base + 0x45, ext_kb);

        // +0x47: disk buffer head (DISKBUF_SEG:0000)
        memory.write_u16(lol_base + 0x47, 0x0000);
        memory.write_u16(lol_base + 0x49, DISKBUF_SEG);

        // +0x4B..+0x67: another unused gap (segment offsets 0x6D..0x89).
        // Same reasoning as the +0x35..+0x3E gap above — these are 0xFF
        // from the pass-1 memset of the InfoBlock segment, so we
        // explicitly zero them so the gap contents are deterministic.
        for off in 0x4B..0x68 {
            memory.write_u8(lol_base + off, 0);
        }

        // +0x68: memAllocScanStart. the DOS model writes the stale default
        // `DOS_MEM_START = 0x0158` here before the MCB chain is built,
        // and never updates it. We mirror that exactly.
        memory.write_u16(lol_base + 0x68, 0x0158);

        // Zero-fill the remaining LoL bytes up to the primary SFT
        // header. lol_base + 0x6A corresponds to segment offset 0x8C,
        // so this clears the gap between the LoL trailer and the SFT
        // header at 0xCC. These bytes are 0xFF from the pass-1 memset
        // (they sit between the first-14-byte zero zone and the
        // 0xCC..0x200 SFT zero zone), so we deliberately zero them
        // here.
        let sft_start_linear = seg_base + 0x00CC;
        let mut cursor = lol_base + 0x6A;
        while cursor < sft_start_linear {
            memory.write_u8(cursor, 0);
            cursor += 1;
        }

        // Primary SFT header at segment offset 0x00CC.
        // +0x00 next_ptr (far) -> SECSFT_SEG:0000
        memory.write_u16(sft_start_linear + 0x00, 0x0000);
        memory.write_u16(sft_start_linear + 0x02, SECSFT_SEG);
        // +0x04 entry count (5)
        memory.write_u16(sft_start_linear + 0x04, 5);

        // 5 primary SFT entries, each 59 bytes, starting after the
        // 6-byte header. Only the filename DWORD at entry+0x20 is
        // populated (plus trailing space-padding, written as zeros —
        // matches the DOS model which only writes the 4-byte DWORD).
        //
        // Standard input, output and error are CON; the other devices are AUX and PRN.
        let entry_names: [&[u8; 4]; 5] = [b"CON ", b"CON ", b"CON ", b"AUX ", b"PRN "];
        for (i, name) in entry_names.iter().enumerate() {
            let entry_base = sft_start_linear + 6 + i * 59;
            memory.write_u8(entry_base + 0x20, name[0]);
            memory.write_u8(entry_base + 0x21, name[1]);
            memory.write_u8(entry_base + 0x22, name[2]);
            memory.write_u8(entry_base + 0x23, name[3]);
        }
    }

    /// Initialize the Swappable Data Area (segment 0x00A3, 0x560 bytes).
    ///
    /// Zero-fill the whole segment then write only `drive_crit_error` at
    /// +0x02. This also places `current_psp` (+0x10) at the magic linear
    fn init_sda(&self, memory: &mut Memory) {
        let base = Memory::seg_offset_to_linear(SDA_SEG, 0);
        for i in 0..(SDA_SIZE as usize) {
            memory.write_u8(base + i, 0);
        }
        memory.write_u8(base + 0x02, 0xFF);
    }

    /// Initialize the CON device header (segment 0x00F9, 0x40 bytes).
    ///
    /// NUL -> CON -> end. the DOS model writes 0xFFFFFFFF as a DWORD at
    /// offset 0x06, conflating the strategy (u16) and interrupt (u16)
    /// entry points. We match.
    #[allow(clippy::identity_op)] // offsets kept explicit to mirror the HLD field table
    fn init_con_device(&self, memory: &mut Memory) {
        let base = Memory::seg_offset_to_linear(CONDRV_SEG, 0);
        for i in 0..0x40 {
            memory.write_u8(base + i, 0);
        }
        // +0x00 next_ptr = 0xFFFFFFFF (end of chain)
        memory.write_u32(base + 0x00, 0xFFFF_FFFF);
        // +0x04 attributes (char | special | stdout | stdin)
        memory.write_u16(base + 0x04, 0x8013);
        // +0x06 strategy+interrupt (conflated DWORD matching the DOS model)
        memory.write_u32(base + 0x06, 0xFFFF_FFFF);
        // +0x0A device name "CON     " (8 bytes)
        let name = b"CON     ";
        for (i, &b) in name.iter().enumerate() {
            memory.write_u8(base + 0x0A + i, b);
        }
    }

    /// Initialize the CDS stub (segment 0x00FD, 0x100 bytes).
    ///
    /// Just four bytes `"C:\0"` at offset 0 — the DOS model has no per-drive
    /// CDS entries, and callers that walk CDS get a stub that shows "C:"
    /// exists without usable per-drive state.
    #[allow(clippy::identity_op)] // offsets kept explicit to mirror the HLD field table
    fn init_cds_stub(&self, memory: &mut Memory) {
        let base = Memory::seg_offset_to_linear(CDS_SEG, 0);
        for i in 0..0x100 {
            memory.write_u8(base + i, 0);
        }
        memory.write_u8(base + 0x00, b'C');
        memory.write_u8(base + 0x01, b':');
        memory.write_u8(base + 0x02, b'\\');
        memory.write_u8(base + 0x03, 0x00);
    }

    /// Initialize the FCB System File Table (segment 0x010D, 0x40 bytes).
    ///
    /// the DOS model allocates a 4-paragraph region holding only the header:
    /// `next_ptr = 0xFFFFFFFF`, `count = DOS_FCBS`. No backing entries.
    #[allow(clippy::identity_op)] // offsets kept explicit to mirror the HLD field table
    fn init_fcb_sft(&self, memory: &mut Memory) {
        let base = Memory::seg_offset_to_linear(FCBSFT_SEG, 0);
        for i in 0..0x40 {
            memory.write_u8(base + i, 0);
        }
        memory.write_u32(base + 0x00, 0xFFFF_FFFF);
        memory.write_u16(base + 0x04, DOS_FCBS);
    }

    /// Initialize the DPB chain (segment 0x0111, 26 x 33 bytes).
    ///
    /// Only `drive_number`, `unit_number`, `bytes_sector`, `reserved_sec`,
    /// `media_id`, the `next_dpb` far pointer at +0x19, and
    /// `free_clusters` are written — matches `dos_tables.cpp:463-476`.
    /// All BPB geometry fields are zero.
    #[allow(clippy::identity_op)] // offsets kept explicit to mirror the HLD field table
    fn init_dpb_chain(&self, memory: &mut Memory) {
        let base = Memory::seg_offset_to_linear(DPB_SEG, 0);
        let total_bytes = (DOS_DRIVES as usize) * 33;
        for i in 0..total_bytes {
            memory.write_u8(base + i, 0);
        }
        for n in 0..(DOS_DRIVES as usize) {
            let entry = base + n * 33;
            memory.write_u8(entry + 0x00, n as u8); // drive number
            memory.write_u8(entry + 0x01, n as u8); // unit number
            memory.write_u16(entry + 0x02, 0x0200); // bytes per sector
            memory.write_u16(entry + 0x06, 0x0001); // reserved sectors
            memory.write_u8(entry + 0x17, 0x00); // media id
                                                 // +0x19: next DPB far pointer
            if n + 1 < DOS_DRIVES as usize {
                memory.write_u16(entry + 0x19, ((n as u16) + 1) * 33);
                memory.write_u16(entry + 0x1B, DPB_SEG);
            } else {
                memory.write_u32(entry + 0x19, 0xFFFF_FFFF);
            }
            memory.write_u16(entry + 0x1F, 0xFFFF); // free clusters
        }
    }

    /// Initialize the fake disk buffer head (segment 0x0147, first 0x20
    /// bytes). Matches `dos_tables.cpp:482-490`.
    #[allow(clippy::identity_op)] // offsets kept explicit to mirror the HLD field table
    fn init_disk_buffer_head(&self, memory: &mut Memory) {
        let base = Memory::seg_offset_to_linear(DISKBUF_SEG, 0);
        for i in 0..0x20 {
            memory.write_u8(base + i, 0);
        }
        memory.write_u16(base + 0x00, 0xFFFF); // forward pointer
        memory.write_u16(base + 0x02, 0xFFFF); // backward pointer
        memory.write_u8(base + 0x04, 0xFF); // not in use
        memory.write_u8(base + 0x0A, 0x01); // num FATs
        memory.write_u32(base + 0x0D, 0xFFFF_FFFF); // DPB pointer
    }

    /// Initialize the secondary System File Table (segment 0x014D,
    /// `next_ptr = 0xFFFFFFFF`, `count = DOS_FILES - 5`). Entries are
    /// zeroed — matches `dos_classes.cpp:139-168`.
    #[allow(clippy::identity_op)] // offsets kept explicit to mirror the HLD field table
    fn init_secondary_sft(&self, memory: &mut Memory) {
        let base = Memory::seg_offset_to_linear(SECSFT_SEG, 0);
        let entry_count = DOS_FILES - 5; // 200 - 5 primary SFT entries
        let total_bytes = 6 + (entry_count as usize) * 59;
        for i in 0..total_bytes {
            memory.write_u8(base + i, 0);
        }
        memory.write_u32(base + 0x00, 0xFFFF_FFFF);
        memory.write_u16(base + 0x04, entry_count);
    }

    /// Initialize environment block
    pub fn init_environment(&mut self, memory: &mut Memory, sound_config: &SoundEnvConfig) {
        self.init_environment_with(memory, sound_config, &[]);
    }

    /// Initialize the master environment block, then apply `guest_vars` as
    /// AUTOEXEC.BAT `SET` lines would: each name is upper-cased, replaces a
    /// default of the same name in place, or is appended; an empty value
    /// deletes the variable. The block grows
    /// past its default 1 KB to hold them (as `DOS command interpreter /E:` sizes it),
    /// keeping 256 bytes free for later `SET`s.
    pub fn init_environment_with(
        &mut self,
        memory: &mut Memory,
        sound_config: &SoundEnvConfig,
        guest_vars: &[(String, String)],
    ) {
        // Initialize MCB chain once we have access to guest memory.
        self.dos_mem.init(memory);

        // Initialize UMBs at boot (emulates EMM386 loaded in CONFIG.SYS).
        // UMBs span 0xC800-0xDFFF (96KB), linked into MCB chain by default.
        self.dos_mem.init_umb(memory);
        self.umb_linked = true;
        self.dos_mem.write_umb_link(memory, true);

        // Initialize DOS kernel structures (LoL, SDA, device chain,
        // CDS stub, FCB SFT, DPB chain, disk buffer head, secondary SFT).
        self.init_kernel_structures(memory);

        // Create basic environment strings (null-terminated, double-null at end)
        let mut defaults = vec![
            "PATH=C:\\DOS;C:\\".to_string(),
            "PROMPT=$P$G".to_string(),
            "TEMP=C:\\TEMP".to_string(),
            "TMP=C:\\TEMP".to_string(),
            "COMSPEC=C:\\COMMAND.COM".to_string(),
        ];

        // Append sound-related environment variables from hardware config
        defaults.extend(build_sound_env_vars(sound_config));

        // Append Glide/3dfx environment variables if OVL data is configured
        defaults.extend(build_glide_env_vars(self.glide_ovl_data.is_some()));

        let mut env_vars: Vec<(Vec<u8>, Vec<u8>)> = defaults
            .iter()
            .filter_map(|var| var.split_once('='))
            .map(|(name, value)| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
            .collect();
        for (name, value) in guest_vars {
            set_env_var(&mut env_vars, name.as_bytes(), value.as_bytes());
        }

        // Allocate environment block.
        // Owned by "system" (PSP 0x0008 is a conventional DOS value; we just use non-zero).
        // 1024 bytes unless the strings need more; the program name follows the
        // double null, and later SETs need room.
        // The caller bounds `guest_vars` to DOS command interpreter's 32 KB /E: maximum.
        let strings_bytes: usize = env_vars
            .iter()
            .map(|(name, value)| name.len() + 1 + value.len() + 1)
            .sum::<usize>()
            + 1;
        let env_paras = (strings_bytes + 256).div_ceil(16).clamp(0x40, 0x800) as u16;
        assert!(
            strings_bytes <= usize::from(env_paras) * 16,
            "DOS environment strings ({strings_bytes} bytes) exceed the 32 KB block"
        );
        let env_seg = self
            .dos_mem
            .alloc(memory, 0x0008, env_paras)
            .unwrap_or(0x0050); // fallback (legacy) if allocator is broken early
        self.env_segment = env_seg;

        let env_base = Memory::seg_offset_to_linear(env_seg, 0);

        let mut offset = 0;
        for (name, value) in &env_vars {
            for &byte in name.iter().chain(b"=").chain(value) {
                memory.write_u8(env_base + offset, byte);
                offset += 1;
            }
            memory.write_u8(env_base + offset, 0); // Null terminator
            offset += 1;
        }

        // Final double-null to mark end of environment
        memory.write_u8(env_base + offset, 0);
    }

    /// Store the program name after the environment block (used by many DOS extenders).
    pub fn set_env_program_name(&mut self, memory: &mut Memory, program_name: &str) {
        if self.env_segment == 0 {
            return;
        }

        let env_base = Memory::seg_offset_to_linear(self.env_segment, 0);
        let max_bytes = self
            .dos_mem
            .block_end_segment(self.env_segment)
            .map(|end_seg| end_seg.saturating_sub(self.env_segment) as usize * 16)
            .unwrap_or(usize::MAX);

        let mut offset = 0usize;
        while offset + 1 < max_bytes {
            let b = memory.read_u8(env_base + offset);
            if b == 0 {
                let next = memory.read_u8(env_base + offset + 1);
                if next == 0 {
                    offset += 2;
                    break;
                }
            }
            offset += 1;
        }

        if offset + 3 >= max_bytes {
            return;
        }

        // DOS convention: word count of following strings, then ASCIIZ program name.
        memory.write_u16(env_base + offset, 1);
        offset += 2;

        let bytes = program_name.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() && offset + i + 1 < max_bytes {
            memory.write_u8(env_base + offset + i, bytes[i]);
            i += 1;
        }
        memory.write_u8(env_base + offset + i, 0);
    }
}

/// Find entry for DTA
pub(crate) struct FindEntry {
    pub(crate) name: String,
    pub(crate) size: u32,
    pub(crate) attr: u8,
    pub(crate) time: u16,
    pub(crate) date: u16,
}

impl Default for Dos {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod virtual_file_sharing_tests;

/// SUBST drive mappings (MDD-BUG-KILN-00634).
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod subst_tests;

/// Default-FCB command-tail parsing (MDD-BUG-KILN-00633).
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod default_fcb_tests;
