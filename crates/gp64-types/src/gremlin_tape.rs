//! Deterministic RTC/CMOS/PIT/keyboard replay tape (Gremlin HLD Step 2).
//!
//! A tape pins the values returned by a fixed list of host-dependent
//! port/BIOS-call sources so byte-identical replay is possible across
//! machines. See `wrk_docs/2026.05.15 - HLD - Gremlin exploratory
//! testing.md` §"RTC / port / boot-environment determinism".
//!
//! Entries are pre-sorted by `(at_pit_tick, seq)`. The runtime cursor
//! lives in `src/gremlin/rtc_tape.rs`; this crate only owns the data
//! shape + JSON (de)serialisation + load-time invariant checks.
//!
//! ### BDA per-read hooks are out of scope for Step 2
//!
//! Step 2 covers the 14 port/INT-1Ah sources listed in [`SourceTag`].
//! The BIOS Data Area (BDA) tick count at 0040:006Ch *is* a host-time
//! input, but it is already deterministic given a deterministic PIT —
//! the HLE INT 08h handler increments it from the cycle-derived PIT
//! tick stream — and there is no per-address Memory read callback to
//! hook a tape entry on. A future step can add BDA hooks if direct
//! BDA reads (bypassing INT 1Ah) ever show up as a divergence source.
use serde::{Deserialize, Serialize};
use std::fmt;
/// Tag identifying which of the 14 sources a tape entry binds to.
///
/// Kept separate from [`TapeSource`] so the source-vs-cursor match
/// classifier (`coverage_gap` MAJOR / `tape_mismatch` CRITICAL, per
/// HLD) doesn't have to inspect the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTag {
    /// INT 1Ah AH=00h — Get system time.
    Int1aAh00,
    /// INT 1Ah AH=02h, 03h, 04h, 05h — RTC time/date get/set.
    #[serde(rename = "int1a_ah02_05")]
    Int1aAh0205,
    /// IN port 71h (CMOS data register read).
    CmosPortRead,
    /// OUT port 70h (CMOS index + NMI gate) and OUT port 71h (data).
    CmosPortWrite,
    /// IN ports 40h-42h (PIT counter read).
    PitRead,
    /// OUT ports 40h-43h (PIT counter / command write).
    PitWrite,
    /// IN port 60h (keyboard data).
    KbdDataRead,
    /// IN port 64h (keyboard/8042 status).
    KbdStatusRead,
    /// OUT port 64h (keyboard/8042 command).
    KbdCmdWrite,
    /// IN port 61h (PPI control / refresh / speaker).
    PpiPortRead,
    /// OUT port 61h (PPI control / refresh / speaker).
    PpiPortWrite,
    /// IN port 92h (System Control Port A — A20 / fast reset).
    SystemCtlRead,
    /// OUT port 92h.
    SystemCtlWrite,
}
impl SourceTag {
    /// Stable, human-readable name used in classifier diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::Int1aAh00 => "int1a_ah00",
            Self::Int1aAh0205 => "int1a_ah02_05",
            Self::CmosPortRead => "cmos_port_read",
            Self::CmosPortWrite => "cmos_port_write",
            Self::PitRead => "pit_read",
            Self::PitWrite => "pit_write",
            Self::KbdDataRead => "kbd_data_read",
            Self::KbdStatusRead => "kbd_status_read",
            Self::KbdCmdWrite => "kbd_cmd_write",
            Self::PpiPortRead => "ppi_port_read",
            Self::PpiPortWrite => "ppi_port_write",
            Self::SystemCtlRead => "system_ctl_read",
            Self::SystemCtlWrite => "system_ctl_write",
        }
    }
}
/// One concrete recorded source event + its payload.
///
/// Tagged enum over the 14 source variants. The payload shape is
/// minimal and matches the runtime contract: each callsite only
/// needs the bytes it would have otherwise read/observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum TapeSource {
    /// INT 1Ah AH=00h — `{al, cx, dx}` preserves the midnight-clear
    /// side-effect: AL=1 still drives the BDA midnight-flag clear.
    Int1aAh00 { al: u8, cx: u16, dx: u16 },
    /// INT 1Ah AH=02h-05h — `{ah, ch, cl, dh, dl, cf}` covers both
    /// the get (02h/04h) and set (03h/05h) sub-functions. `cf` is the
    /// CARRY-flag value the BIOS sets on return.
    #[serde(rename = "int1a_ah02_05")]
    Int1aAh0205 {
        ah: u8,
        ch: u8,
        cl: u8,
        dh: u8,
        dl: u8,
        cf: bool,
    },
    /// IN port 71h — `{register, value}`. `register` is the latched
    /// CMOS index; recorded for diagnostic / mismatch-classifier use.
    CmosPortRead { register: u8, value: u8 },
    /// OUT port 70h or 71h.
    ///
    /// `port` and `value` are the identity of the write: which port the
    /// guest wrote and the byte it wrote there. Both are matched on replay —
    /// a same-tick CMOS write to the other port, or of a different byte, is a
    /// divergence, not a hit (`MDD-BUG-CRUCIBLE-00177`).
    ///
    /// `register` (the CMOS index in effect) and `nmi_gate` (the bit-7
    /// NMI-gate state the write latched) are diagnostic context for the
    /// mismatch classifier, the same role `register` plays on
    /// [`Self::CmosPortRead`]. The runtime hook still applies the gate to
    /// preserve the real-hardware side-effect.
    CmosPortWrite {
        port: u16,
        register: u8,
        value: u8,
        nmi_gate: bool,
    },
    /// IN port 40h/41h/42h — `{port, value}`.
    PitRead { port: u16, value: u8 },
    /// OUT port 40h/41h/42h/43h — `{port, value}`.
    PitWrite { port: u16, value: u8 },
    /// IN port 60h — `{value}`. Runtime side-effect (IRQ1 lower via
    /// PIC) is still applied.
    KbdDataRead { value: u8 },
    /// IN port 64h — `{value}`.
    KbdStatusRead { value: u8 },
    /// OUT port 64h — `{value}`.
    KbdCmdWrite { value: u8 },
    /// IN port 61h — `{value}`.
    PpiPortRead { value: u8 },
    /// OUT port 61h — `{value}`.
    PpiPortWrite { value: u8 },
    /// IN port 92h — `{value}`.
    SystemCtlRead { value: u8 },
    /// OUT port 92h — `{value}`.
    SystemCtlWrite { value: u8 },
}
impl TapeSource {
    /// Lookup the [`SourceTag`] for this payload (cheap match).
    pub fn tag(&self) -> SourceTag {
        match self {
            Self::Int1aAh00 { .. } => SourceTag::Int1aAh00,
            Self::Int1aAh0205 { .. } => SourceTag::Int1aAh0205,
            Self::CmosPortRead { .. } => SourceTag::CmosPortRead,
            Self::CmosPortWrite { .. } => SourceTag::CmosPortWrite,
            Self::PitRead { .. } => SourceTag::PitRead,
            Self::PitWrite { .. } => SourceTag::PitWrite,
            Self::KbdDataRead { .. } => SourceTag::KbdDataRead,
            Self::KbdStatusRead { .. } => SourceTag::KbdStatusRead,
            Self::KbdCmdWrite { .. } => SourceTag::KbdCmdWrite,
            Self::PpiPortRead { .. } => SourceTag::PpiPortRead,
            Self::PpiPortWrite { .. } => SourceTag::PpiPortWrite,
            Self::SystemCtlRead { .. } => SourceTag::SystemCtlRead,
            Self::SystemCtlWrite { .. } => SourceTag::SystemCtlWrite,
        }
    }
}
/// The runtime source request a replay callsite is trying to satisfy.
///
/// Broad source tags are still enough for sources with one value stream.
/// Sources multiplexed behind one tag carry the discriminator needed to avoid
/// consuming a same-tick entry for the wrong hardware register or BIOS
/// subfunction.
///
/// **Writes carry discriminators too.** Until `MDD-BUG-CRUCIBLE-00177` every
/// write matched on its [`SourceTag`] alone, so a same-tick write to the wrong
/// PIT counter, the wrong CMOS port, or with the wrong byte was recorded as a
/// hit. That is worse than missing the divergence: consuming the row advances
/// the cursor, so the real mismatch is suppressed AND every later row shifts.
/// A write's identity is the port it went to and the byte it carried, and both
/// are matched.
/// A source whose rows carry nothing the REQUEST side can discriminate on, so
/// "the next row of this kind" is the whole request.
///
/// Every other tag has a typed [`TapeRequest`] variant carrying the fields that
/// identify the row — a PIT port, a CMOS register, a written byte. A bare tag
/// for one of those would match any same-tag row and consume it, so a write to
/// the wrong port or of the wrong byte was recorded as a hit, the real
/// divergence was suppressed, and every later row shifted. This type is why
/// that request cannot be written any more: `TapeRequest::Source` takes an
/// `UndiscriminatedTag`, not a `SourceTag` (MDD-BUG-KIL-00820).
///
/// These five are all reads (plus INT 1Ah AH=00h, which is a read of the BIOS
/// tick count): the caller is asking for the value precisely because it does
/// not know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndiscriminatedTag {
    /// INT 1Ah AH=00h — get system time.
    Int1aAh00,
    /// IN port 60h (keyboard data).
    KbdDataRead,
    /// IN port 64h (keyboard/8042 status).
    KbdStatusRead,
    /// IN port 61h (PPI control / refresh / speaker).
    PpiPortRead,
    /// IN port 92h (System Control Port A).
    SystemCtlRead,
}
impl UndiscriminatedTag {
    /// The [`SourceTag`] this stands for.
    pub const fn tag(self) -> SourceTag {
        match self {
            Self::Int1aAh00 => SourceTag::Int1aAh00,
            Self::KbdDataRead => SourceTag::KbdDataRead,
            Self::KbdStatusRead => SourceTag::KbdStatusRead,
            Self::PpiPortRead => SourceTag::PpiPortRead,
            Self::SystemCtlRead => SourceTag::SystemCtlRead,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapeRequest {
    /// Match the next row of this kind, whatever it carries. Restricted to
    /// [`UndiscriminatedTag`] so a discriminated source cannot be requested
    /// this way (MDD-BUG-KIL-00820).
    Source(UndiscriminatedTag),
    Int1aAh0205 {
        ah: u8,
    },
    CmosPortRead {
        register: u8,
    },
    PitRead {
        port: u16,
    },
    CmosPortWrite {
        port: u16,
        value: u8,
    },
    PitWrite {
        port: u16,
        value: u8,
    },
    KbdCmdWrite {
        value: u8,
    },
    PpiPortWrite {
        value: u8,
    },
    SystemCtlWrite {
        value: u8,
    },
}
impl TapeRequest {
    pub fn tag(self) -> SourceTag {
        match self {
            Self::Source(tag) => tag.tag(),
            Self::Int1aAh0205 { .. } => SourceTag::Int1aAh0205,
            Self::CmosPortRead { .. } => SourceTag::CmosPortRead,
            Self::PitRead { .. } => SourceTag::PitRead,
            Self::CmosPortWrite { .. } => SourceTag::CmosPortWrite,
            Self::PitWrite { .. } => SourceTag::PitWrite,
            Self::KbdCmdWrite { .. } => SourceTag::KbdCmdWrite,
            Self::PpiPortWrite { .. } => SourceTag::PpiPortWrite,
            Self::SystemCtlWrite { .. } => SourceTag::SystemCtlWrite,
        }
    }
    fn matches(self, source: &TapeSource) -> bool {
        match (self, source) {
            (Self::Source(tag), source) => source.tag() == tag.tag(),
            (Self::Int1aAh0205 { ah }, TapeSource::Int1aAh0205 { ah: found, .. }) => ah == *found,
            (
                Self::CmosPortRead { register },
                TapeSource::CmosPortRead {
                    register: found, ..
                },
            ) => register == *found,
            (Self::PitRead { port }, TapeSource::PitRead { port: found, .. }) => port == *found,
            (
                Self::CmosPortWrite { port, value },
                TapeSource::CmosPortWrite {
                    port: found_port,
                    value: found_value,
                    ..
                },
            ) => port == *found_port && value == *found_value,
            (
                Self::PitWrite { port, value },
                TapeSource::PitWrite {
                    port: found_port,
                    value: found_value,
                },
            ) => port == *found_port && value == *found_value,
            (Self::KbdCmdWrite { value }, TapeSource::KbdCmdWrite { value: found }) => {
                value == *found
            }
            (Self::PpiPortWrite { value }, TapeSource::PpiPortWrite { value: found }) => {
                value == *found
            }
            (Self::SystemCtlWrite { value }, TapeSource::SystemCtlWrite { value: found }) => {
                value == *found
            }
            _ => false,
        }
    }
}
impl From<UndiscriminatedTag> for TapeRequest {
    fn from(tag: UndiscriminatedTag) -> Self {
        Self::Source(tag)
    }
}
/// One row in the tape: anchored at a PIT-tick boundary, ordered
/// within the tick by `seq`, carrying a tagged source payload.
///
/// `#[serde(flatten)]` on `source` is intentional — the on-disk
/// JSON uses `"source": "...", "value": ...` style (see HLD example)
/// with no extra nesting object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapeEntry {
    /// PIT cumulative tick at which this entry was captured.
    pub at_pit_tick: u64,
    /// Monotonic sequence number within `at_pit_tick`. Disambiguates
    /// multiple sources firing in the same tick.
    pub seq: u32,
    #[serde(flatten)]
    pub source: TapeSource,
}
/// Top-level tape document. Loaded once, never mutated at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtcTape {
    /// Bump on any breaking schema change; current version is 1.
    pub schema_version: u32,
    /// Free-form ISO-8601-ish timestamp from the capture run. Not
    /// validated; purely descriptive.
    #[serde(default)]
    pub capture_date: String,
    /// Free-form description of the workload that produced this tape
    /// (e.g. `"synthetic CPU test @ pentium"`).
    #[serde(default)]
    pub target_workload: String,
    /// Sorted (by `(at_pit_tick, seq)`) entry list.
    pub entries: Vec<TapeEntry>,
}
/// Errors raised at tape load / validation time.
#[derive(Debug)]
pub enum TapeError {
    Parse(serde_json::Error),
    Io(std::io::Error),
    UnsupportedSchema {
        got: u32,
    },
    Unsorted {
        index: usize,
        prev_tick: u64,
        prev_seq: u32,
        this_tick: u64,
        this_seq: u32,
    },
}
impl fmt::Display for TapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "tape JSON parse failed: {error}"),
            Self::Io(error) => write!(f, "tape I/O error: {error}"),
            Self::UnsupportedSchema { got } => {
                write!(f, "tape schema_version {got} not supported (expected 1)")
            }
            Self::Unsorted {
                index,
                prev_tick,
                prev_seq,
                this_tick,
                this_seq,
            } => write!(
                f,
                "tape entries not sorted at index {index}: \
                 ({prev_tick}, {prev_seq}) >= ({this_tick}, {this_seq})"
            ),
        }
    }
}
impl std::error::Error for TapeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::UnsupportedSchema { .. } | Self::Unsorted { .. } => None,
        }
    }
}
impl From<serde_json::Error> for TapeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Parse(error)
    }
}
impl From<std::io::Error> for TapeError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
/// Maximum size (bytes) of a Gremlin artifact loaded from disk. The loaders
/// read at most this much and reject a larger file *without* buffering the whole
/// thing, so a very large or adversarial artifact cannot force an unbounded host
/// allocation + JSON parse before validation runs (MDD-BUG-CRUCIBLE-00042).
/// Generous — real session/tape files are far smaller.
pub(crate) const MAX_ARTIFACT_BYTES: usize = 256 * 1024 * 1024;
/// Read a file into memory, bounded at `max` bytes. Reads at most `max + 1`
/// bytes (so an oversized artifact cannot force an unbounded allocation) and
/// returns an `InvalidData` error if the file exceeds `max`. Shared by the
/// Gremlin tape and session loaders. See MDD-BUG-CRUCIBLE-00042.
pub(crate) fn read_artifact_capped(path: &std::path::Path, max: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut buf)?;
    if buf.len() > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("gremlin artifact exceeds the {max}-byte limit"),
        ));
    }
    Ok(buf)
}
/// Maximum number of JSON *values* a Gremlin artifact may decode to.
///
/// The byte cap above bounds the input; it does NOT bound what the input
/// decodes to. Compact JSON is dense in values — `[0,0,0,…]` spends two bytes
/// per element, and each element costs an order of magnitude more once it is a
/// `serde_json::Value` in a `Vec` — so a document just under the byte cap can
/// still force several GiB before any validation runs (MDD-BUG-CRUCIBLE-00567,
/// the residual of MDD-BUG-CRUCIBLE-00042). This budget caps the decoded shape
/// so peak memory stays the same order as the file, not ten times it.
///
/// Generous: the largest session in `tests/gremlin/sessions` decodes to 91
/// values, so this is ~46,000x real usage and cannot be hit by an honest
/// artifact. It also bounds the deliberately opaque `Reference.config`, which
/// is the one field with no schema of its own to constrain it.
pub(crate) const MAX_JSON_VALUES: usize = 4 * 1024 * 1024;
/// Reject a JSON document that would decode to more than `max` values, WITHOUT
/// building the document.
///
/// Counts by a structural identity: every JSON value is either the root, the
/// first child of a container (so it immediately follows `[` or `{`), or
/// preceded by a comma. Object *keys* are not values and are not counted,
/// because a key is always followed by `:`, never by `,` or an opening bracket.
/// So `commas + openers + 1` is the value count, and the scan only has to know
/// whether it is inside a string literal.
///
/// The one inexactness is an empty container, where the opener has no first
/// child to stand for: `[]` counts 2 where it decodes to 1. That over-counts,
/// never under-counts, which is the safe direction for a ceiling.
///
/// This is a resource guard, not a parser: malformed input is left for
/// `serde_json` to reject with its own error.
pub(crate) fn check_json_value_budget(bytes: &[u8], max: usize) -> std::io::Result<()> {
    fn over_budget(max: usize) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("gremlin artifact decodes to more than {max} JSON values"),
        )
    }
    let mut values: usize = 1;
    if values > max {
        return Err(over_budget(max));
    }
    let mut in_string = false;
    let mut escaped = false;
    for &b in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b',' | b'[' | b'{' => {
                values += 1;
                if values > max {
                    return Err(over_budget(max));
                }
            }
            _ => {}
        }
    }
    Ok(())
}
impl RtcTape {
    /// Parse + validate a tape JSON document. Rejects unsorted
    /// entries (HLD: `at_pit_tick` ascending, `seq` ascending within
    /// each tick).
    pub fn from_json_str(s: &str) -> Result<Self, TapeError> {
        check_json_value_budget(s.as_bytes(), MAX_JSON_VALUES)?;
        let tape: RtcTape = serde_json::from_str(s)?;
        tape.validate_invariants()?;
        Ok(tape)
    }
    /// Load + validate from a filesystem path.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Result<Self, TapeError> {
        let bytes = read_artifact_capped(path.as_ref(), MAX_ARTIFACT_BYTES)?;
        check_json_value_budget(&bytes, MAX_JSON_VALUES)?;
        let tape: RtcTape = serde_json::from_slice(&bytes)?;
        tape.validate_invariants()?;
        Ok(tape)
    }
    /// Validate tape-level invariants (schema version, entries sorted
    /// strictly ascending by `(at_pit_tick, seq)`). Public so callers
    /// that embed an [`RtcTape`] in a larger document (e.g.
    /// `Session`) can re-validate after construction.
    pub fn validate_invariants(&self) -> Result<(), TapeError> {
        if self.schema_version != 1 {
            return Err(TapeError::UnsupportedSchema {
                got: self.schema_version,
            });
        }
        let mut prev: Option<(u64, u32)> = None;
        for (i, e) in self.entries.iter().enumerate() {
            if let Some((pt, ps)) = prev {
                let here = (e.at_pit_tick, e.seq);
                if here <= (pt, ps) {
                    return Err(TapeError::Unsorted {
                        index: i,
                        prev_tick: pt,
                        prev_seq: ps,
                        this_tick: e.at_pit_tick,
                        this_seq: e.seq,
                    });
                }
            }
            prev = Some((e.at_pit_tick, e.seq));
        }
        Ok(())
    }
}
use std::sync::atomic::{AtomicU64, Ordering};
/// Outcome of [`RtcTapeCursor::next_response`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapeOutcome {
    /// The next pending tape entry matched the requested source request.
    Hit(TapeSource),
    /// The cursor's next entry sits in the future (or is exhausted)
    /// and there is no entry at `current_tick` for this source.
    /// Poisoned values must be substituted by the caller.
    CoverageGap,
    /// The next entry sits at or before `current_tick`, but it does not
    /// match the requested source/discriminator. Classified CRITICAL — the
    /// captured guest run is not the one replaying now.
    Mismatch {
        expected: SourceTag,
        found: SourceTag,
    },
}
/// Per-source coverage / divergence counters.
///
/// AtomicU64 so the tape cursor can stay behind a shared mutable
/// borrow without forcing the surrounding emulator state to take
/// extra `&mut` chains. gp64 is single-threaded for guest code
/// (interpreter / JIT both run on the emulator thread), so the
/// counters never see contention; the atomics are purely for the
/// memcell-aliasing-rules property, free on x86.
#[derive(Debug, Default)]
pub struct TapeCounters {
    pub hits: AtomicU64,
    pub coverage_gaps: AtomicU64,
    pub mismatches: AtomicU64,
    pub poisoned_reads: AtomicU64,
}
impl TapeCounters {
    /// Snapshot all counters into plain integers.
    pub fn snapshot(&self) -> TapeCountersSnapshot {
        TapeCountersSnapshot {
            hits: self.hits.load(Ordering::Relaxed),
            coverage_gaps: self.coverage_gaps.load(Ordering::Relaxed),
            mismatches: self.mismatches.load(Ordering::Relaxed),
            poisoned_reads: self.poisoned_reads.load(Ordering::Relaxed),
        }
    }
}
/// Plain-integer snapshot returned by [`TapeCounters::snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TapeCountersSnapshot {
    pub hits: u64,
    pub coverage_gaps: u64,
    pub mismatches: u64,
    pub poisoned_reads: u64,
}
/// Mutable cursor over an [`RtcTape`].
///
/// Held on `EmulatorGremlinConfig` (and from there the `Emulator`).
/// Constructed once at startup from `--gremlin-rtc-tape PATH`; the
/// tape itself is moved in.
#[derive(Debug)]
pub struct RtcTapeCursor {
    tape: RtcTape,
    /// Index of the next entry to consider.
    cursor: usize,
    counters: TapeCounters,
}
impl RtcTapeCursor {
    /// Wrap an already-loaded + validated tape.
    pub fn new(tape: RtcTape) -> Self {
        Self {
            tape,
            cursor: 0,
            counters: TapeCounters::default(),
        }
    }
    /// Borrow the underlying tape (for diagnostics).
    pub fn tape(&self) -> &RtcTape {
        &self.tape
    }
    /// Index of the next unconsumed entry. `tape().entries.len()`
    /// when the tape is exhausted.
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    /// Snapshot of all per-source counters.
    pub fn counters(&self) -> TapeCountersSnapshot {
        self.counters.snapshot()
    }
    /// Look up the next response for the given source request at the given
    /// PIT tick.
    ///
    /// Algorithm:
    /// 1. If the next entry is at `at_pit_tick > current_pit_tick`,
    ///    we haven't yet reached it — return `CoverageGap` (the
    ///    captured run produced no event of this kind in this tick).
    /// 2. If the next entry is at `at_pit_tick <= current_pit_tick`:
    ///    a. If it matches `expected`, consume it and return `Hit(payload)`.
    ///    b. If it differs by tag or discriminator, return `Mismatch`
    ///    *without* consuming the cursor — the entry might be the right
    ///    answer for a different source firing in the same tick. We rely on
    ///    the caller to keep advancing the PIT and trying.
    ///
    /// The cursor never goes backwards. Hit-consuming is the only
    /// place that advances it.
    pub fn next_response(
        &mut self,
        expected: impl Into<TapeRequest>,
        current_pit_tick: u64,
    ) -> TapeOutcome {
        let expected = expected.into();
        let entry = match self.tape.entries.get(self.cursor) {
            Some(e) => e,
            None => {
                self.counters.coverage_gaps.fetch_add(1, Ordering::Relaxed);
                self.counters.poisoned_reads.fetch_add(1, Ordering::Relaxed);
                return TapeOutcome::CoverageGap;
            }
        };
        if entry.at_pit_tick > current_pit_tick {
            self.counters.coverage_gaps.fetch_add(1, Ordering::Relaxed);
            self.counters.poisoned_reads.fetch_add(1, Ordering::Relaxed);
            return TapeOutcome::CoverageGap;
        }
        let found = entry.source.tag();
        if expected.matches(&entry.source) {
            let payload = entry.source.clone();
            self.cursor += 1;
            self.counters.hits.fetch_add(1, Ordering::Relaxed);
            TapeOutcome::Hit(payload)
        } else {
            self.counters.mismatches.fetch_add(1, Ordering::Relaxed);
            self.counters.poisoned_reads.fetch_add(1, Ordering::Relaxed);
            TapeOutcome::Mismatch {
                expected: expected.tag(),
                found,
            }
        }
    }
}
/// Poisoned-value contract (HLD Step 2).
///
/// When the cursor under-runs (coverage gap) or mismatches, the
/// callsite substitutes these bytes so the determinism contract
/// holds (no host fall-through). Status-style ports return `0xFF`
/// (every error bit set); data-style ports return `0x00`.
pub mod poisoned {
    /// CMOS data read (port 71h), PIT counter read (40h-42h), PPI
    /// (port 61h, kept consistent with data-style poisoning),
    /// keyboard data read (port 60h).
    pub const DATA: u8 = 0x00;
    /// Keyboard status (port 64h), System Control (port 92h reads).
    pub const STATUS: u8 = 0xFF;
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn tape_error_display_source_and_from_contract() {
        let parse_cause = serde_json::from_str::<serde_json::Value>("not-json").unwrap_err();
        let parse_error: TapeError = parse_cause.into();
        assert_eq!(
            parse_error.to_string(),
            format!(
                "tape JSON parse failed: {}",
                std::error::Error::source(&parse_error).unwrap()
            )
        );
        assert_eq!(format!("{:>80.3}", parse_error), parse_error.to_string());
        assert!(std::error::Error::source(&parse_error)
            .unwrap()
            .downcast_ref::<serde_json::Error>()
            .is_some());

        let io_cause = std::io::Error::other("disk failure");
        let io_error: TapeError = io_cause.into();
        assert_eq!(io_error.to_string(), "tape I/O error: disk failure");
        assert_eq!(format!("{:>80.3}", io_error), io_error.to_string());
        assert!(std::error::Error::source(&io_error)
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .is_some());

        assert_eq!(
            TapeError::UnsupportedSchema { got: 2 }.to_string(),
            "tape schema_version 2 not supported (expected 1)"
        );
        let unsupported = TapeError::UnsupportedSchema { got: 2 };
        assert_eq!(format!("{:>80.3}", unsupported), unsupported.to_string());
        assert_eq!(
            TapeError::Unsorted {
                index: 7,
                prev_tick: 10,
                prev_seq: 3,
                this_tick: 10,
                this_seq: 3,
            }
            .to_string(),
            "tape entries not sorted at index 7: (10, 3) >= (10, 3)"
        );
        let unsorted = TapeError::Unsorted {
            index: 7,
            prev_tick: 10,
            prev_seq: 3,
            this_tick: 10,
            this_seq: 3,
        };
        assert_eq!(format!("{:>80.3}", unsorted), unsorted.to_string());
        assert!(std::error::Error::source(&TapeError::UnsupportedSchema { got: 2 }).is_none());
        assert!(std::error::Error::source(&TapeError::Unsorted {
            index: 7,
            prev_tick: 10,
            prev_seq: 3,
            this_tick: 10,
            this_seq: 3,
        })
        .is_none());
    }
    #[test]
    fn read_artifact_capped_rejects_oversize() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("artifact.json");
        std::fs::write(&p, vec![0u8; 100]).unwrap();
        assert_eq!(read_artifact_capped(&p, 100).unwrap().len(), 100);
        let err = read_artifact_capped(&p, 50).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
    #[test]
    fn round_trip_minimal_tape() {
        let tape = RtcTape {
            schema_version: 1,
            capture_date: "2026-05-16T12:00:00Z".to_string(),
            target_workload: "test".to_string(),
            entries: vec![TapeEntry {
                at_pit_tick: 5,
                seq: 0,
                source: TapeSource::CmosPortRead {
                    register: 0x0A,
                    value: 0x06,
                },
            }],
        };
        let json = serde_json::to_string(&tape).unwrap();
        let back = RtcTape::from_json_str(&json).unwrap();
        assert_eq!(tape, back);
    }
    #[test]
    fn from_path_round_trips_a_normal_tape() {
        let tape = RtcTape {
            schema_version: 1,
            capture_date: "2026-05-16T12:00:00Z".to_string(),
            target_workload: "test".to_string(),
            entries: vec![TapeEntry {
                at_pit_tick: 5,
                seq: 0,
                source: TapeSource::CmosPortRead {
                    register: 0x0A,
                    value: 0x06,
                },
            }],
        };
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("tape.json");
        std::fs::write(&p, serde_json::to_string(&tape).unwrap()).unwrap();
        let back = RtcTape::from_path(&p).unwrap();
        assert_eq!(tape, back);
    }
    #[test]
    fn rejects_unsupported_schema() {
        let json = r#"{"schema_version":2,"entries":[]}"#;
        let err = RtcTape::from_json_str(json).unwrap_err();
        assert!(matches!(err, TapeError::UnsupportedSchema { got: 2 }));
    }
    #[test]
    fn rejects_unsorted_entries() {
        let tape = RtcTape {
            schema_version: 1,
            capture_date: String::new(),
            target_workload: String::new(),
            entries: vec![
                TapeEntry {
                    at_pit_tick: 10,
                    seq: 0,
                    source: TapeSource::KbdDataRead { value: 1 },
                },
                TapeEntry {
                    at_pit_tick: 5,
                    seq: 0,
                    source: TapeSource::KbdDataRead { value: 2 },
                },
            ],
        };
        let json = serde_json::to_string(&tape).unwrap();
        let err = RtcTape::from_json_str(&json).unwrap_err();
        assert!(matches!(err, TapeError::Unsorted { .. }));
    }
    #[test]
    fn rejects_duplicate_keys() {
        let tape = RtcTape {
            schema_version: 1,
            capture_date: String::new(),
            target_workload: String::new(),
            entries: vec![
                TapeEntry {
                    at_pit_tick: 5,
                    seq: 0,
                    source: TapeSource::KbdDataRead { value: 1 },
                },
                TapeEntry {
                    at_pit_tick: 5,
                    seq: 0,
                    source: TapeSource::KbdDataRead { value: 2 },
                },
            ],
        };
        let json = serde_json::to_string(&tape).unwrap();
        let err = RtcTape::from_json_str(&json).unwrap_err();
        assert!(matches!(err, TapeError::Unsorted { .. }));
    }
    #[test]
    fn tag_matches_payload() {
        let s = TapeSource::Int1aAh00 {
            al: 1,
            cx: 0,
            dx: 0,
        };
        assert_eq!(s.tag(), SourceTag::Int1aAh00);
        assert_eq!(s.tag().name(), "int1a_ah00");
    }
    fn tape_with(entries: Vec<TapeEntry>) -> RtcTape {
        RtcTape {
            schema_version: 1,
            capture_date: String::new(),
            target_workload: String::new(),
            entries,
        }
    }
    #[test]
    fn cursor_empty_tape_always_coverage_gap() {
        let mut cur = RtcTapeCursor::new(tape_with(vec![]));
        assert_eq!(
            cur.next_response(TapeRequest::CmosPortRead { register: 0x0A }, 0),
            TapeOutcome::CoverageGap
        );
        assert_eq!(cur.counters().coverage_gaps, 1);
        assert_eq!(cur.counters().poisoned_reads, 1);
        assert_eq!(cur.counters().hits, 0);
    }
    #[test]
    fn cursor_exact_match_is_hit() {
        let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
            at_pit_tick: 5,
            seq: 0,
            source: TapeSource::CmosPortRead {
                register: 0x0A,
                value: 0x06,
            },
        }]));
        let out = cur.next_response(TapeRequest::CmosPortRead { register: 0x0A }, 5);
        assert!(matches!(out, TapeOutcome::Hit(_)));
        assert_eq!(cur.cursor(), 1);
        assert_eq!(cur.counters().hits, 1);
    }
    #[test]
    fn cursor_future_entry_is_coverage_gap_until_reached() {
        let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
            at_pit_tick: 10,
            seq: 0,
            source: TapeSource::CmosPortRead {
                register: 0x0A,
                value: 0x06,
            },
        }]));
        assert_eq!(
            cur.next_response(TapeRequest::CmosPortRead { register: 0x0A }, 0),
            TapeOutcome::CoverageGap
        );
        assert_eq!(cur.cursor(), 0);
        let out = cur.next_response(TapeRequest::CmosPortRead { register: 0x0A }, 10);
        assert!(matches!(out, TapeOutcome::Hit(_)));
        assert_eq!(cur.cursor(), 1);
    }
    #[test]
    fn cursor_source_mismatch_does_not_advance() {
        let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
            at_pit_tick: 5,
            seq: 0,
            source: TapeSource::KbdDataRead { value: 0x42 },
        }]));
        let out = cur.next_response(TapeRequest::CmosPortRead { register: 0x0A }, 5);
        assert_eq!(
            out,
            TapeOutcome::Mismatch {
                expected: SourceTag::CmosPortRead,
                found: SourceTag::KbdDataRead,
            }
        );
        assert_eq!(cur.cursor(), 0);
        let out2 = cur.next_response(UndiscriminatedTag::KbdDataRead, 5);
        assert!(matches!(out2, TapeOutcome::Hit(_)));
        assert_eq!(cur.cursor(), 1);
        assert_eq!(cur.counters().mismatches, 1);
        assert_eq!(cur.counters().hits, 1);
    }
    /// MDD-BUG-CRUCIBLE-00177, the write-side sibling of
    /// `cursor_discriminator_mismatch_does_not_advance`.
    ///
    /// Every write used to match on its `SourceTag` alone, so a same-tick write
    /// to the wrong PIT counter counted as a hit. That is worse than a missed
    /// divergence: consuming the row advances the cursor, which both suppresses
    /// the real mismatch and shifts every later row.
    #[test]
    fn write_to_the_wrong_port_does_not_advance() {
        let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
            at_pit_tick: 5,
            seq: 0,
            source: TapeSource::PitWrite {
                port: 0x0043,
                value: 0x36,
            },
        }]));
        let out = cur.next_response(
            TapeRequest::PitWrite {
                port: 0x0040,
                value: 0x36,
            },
            5,
        );
        assert_eq!(
            out,
            TapeOutcome::Mismatch {
                expected: SourceTag::PitWrite,
                found: SourceTag::PitWrite,
            }
        );
        assert_eq!(cur.cursor(), 0, "a wrong-port write consumed the row");
        let out = cur.next_response(
            TapeRequest::PitWrite {
                port: 0x0043,
                value: 0x36,
            },
            5,
        );
        assert!(matches!(out, TapeOutcome::Hit(_)));
        assert_eq!(cur.cursor(), 1);
    }
    /// The byte is part of a write's identity: replaying a different value to
    /// the right port is a divergence, not a hit.
    #[test]
    fn write_of_the_wrong_value_does_not_advance() {
        let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
            at_pit_tick: 5,
            seq: 0,
            source: TapeSource::PitWrite {
                port: 0x0043,
                value: 0x36,
            },
        }]));
        let out = cur.next_response(
            TapeRequest::PitWrite {
                port: 0x0043,
                value: 0xB6,
            },
            5,
        );
        assert!(matches!(out, TapeOutcome::Mismatch { .. }));
        assert_eq!(cur.cursor(), 0, "a wrong-value write consumed the row");
        assert_eq!(cur.counters().mismatches, 1);
        assert_eq!(cur.counters().hits, 0);
    }
    /// CMOS multiplexes two ports behind one tag, which is why the schema
    /// needed `port` at all: 70h latches an index, 71h writes data, and before
    /// this they were indistinguishable to the matcher.
    #[test]
    fn cmos_index_and_data_writes_are_not_interchangeable() {
        let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
            at_pit_tick: 5,
            seq: 0,
            source: TapeSource::CmosPortWrite {
                port: 0x0071,
                register: 0x0A,
                value: 0x26,
                nmi_gate: false,
            },
        }]));
        let out = cur.next_response(
            TapeRequest::CmosPortWrite {
                port: 0x0070,
                value: 0x26,
            },
            5,
        );
        assert!(matches!(out, TapeOutcome::Mismatch { .. }));
        assert_eq!(cur.cursor(), 0, "an index write consumed a data-write row");
        let out = cur.next_response(
            TapeRequest::CmosPortWrite {
                port: 0x0071,
                value: 0x26,
            },
            5,
        );
        assert!(matches!(out, TapeOutcome::Hit(_)));
        assert_eq!(cur.cursor(), 1);
    }
    /// The single-port writes have no port to disagree about, so the byte is
    /// their whole discriminator. Table-driven so a new write source cannot be
    /// added with a bare `SourceTag` and go unnoticed.
    #[test]
    fn single_port_writes_discriminate_on_value() {
        let cases: [(TapeSource, TapeRequest, TapeRequest); 3] = [
            (
                TapeSource::KbdCmdWrite { value: 0xAE },
                TapeRequest::KbdCmdWrite { value: 0xAD },
                TapeRequest::KbdCmdWrite { value: 0xAE },
            ),
            (
                TapeSource::PpiPortWrite { value: 0x03 },
                TapeRequest::PpiPortWrite { value: 0x00 },
                TapeRequest::PpiPortWrite { value: 0x03 },
            ),
            (
                TapeSource::SystemCtlWrite { value: 0x02 },
                TapeRequest::SystemCtlWrite { value: 0x00 },
                TapeRequest::SystemCtlWrite { value: 0x02 },
            ),
        ];
        for (source, wrong, right) in cases {
            let tag = source.tag();
            let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
                at_pit_tick: 5,
                seq: 0,
                source,
            }]));
            assert!(
                matches!(cur.next_response(wrong, 5), TapeOutcome::Mismatch { .. }),
                "{tag:?}: a wrong-value write was accepted"
            );
            assert_eq!(cur.cursor(), 0, "{tag:?}: wrong value consumed the row");
            assert!(matches!(cur.next_response(right, 5), TapeOutcome::Hit(_)));
            assert_eq!(cur.cursor(), 1, "{tag:?}");
        }
    }
    /// Every discriminated source must be UNREACHABLE through a bare tag, and
    /// every undiscriminated one must still be reachable.
    ///
    /// The bare form matches on the tag alone, so a bare request for a
    /// discriminated source consumes a same-tag row whatever port or byte it
    /// carries — recording a wrong write as a hit, suppressing the real
    /// divergence, and shifting every later row. `TapeRequest::Source` now
    /// takes an [`UndiscriminatedTag`], so that request no longer compiles;
    /// this drives the partition it rests on (MDD-BUG-KIL-00820).
    ///
    /// The previous guard here asserted only that typed variants report their
    /// own tags, which they can only agree with. It never built a bare
    /// request, a tape row, or checked the cursor.
    #[test]
    fn the_bare_tag_route_admits_exactly_the_undiscriminated_sources() {
        const BARE: [UndiscriminatedTag; 5] = [
            UndiscriminatedTag::Int1aAh00,
            UndiscriminatedTag::KbdDataRead,
            UndiscriminatedTag::KbdStatusRead,
            UndiscriminatedTag::PpiPortRead,
            UndiscriminatedTag::SystemCtlRead,
        ];
        let bare_tags: Vec<SourceTag> = BARE.iter().map(|t| t.tag()).collect();
        let typed: [(SourceTag, TapeRequest); 8] = [
            (
                SourceTag::Int1aAh0205,
                TapeRequest::Int1aAh0205 { ah: 0x02 },
            ),
            (
                SourceTag::CmosPortRead,
                TapeRequest::CmosPortRead { register: 0x0A },
            ),
            (SourceTag::PitRead, TapeRequest::PitRead { port: 0x40 }),
            (
                SourceTag::CmosPortWrite,
                TapeRequest::CmosPortWrite {
                    port: 0x70,
                    value: 0,
                },
            ),
            (
                SourceTag::PitWrite,
                TapeRequest::PitWrite {
                    port: 0x40,
                    value: 0,
                },
            ),
            (
                SourceTag::KbdCmdWrite,
                TapeRequest::KbdCmdWrite { value: 0 },
            ),
            (
                SourceTag::PpiPortWrite,
                TapeRequest::PpiPortWrite { value: 0 },
            ),
            (
                SourceTag::SystemCtlWrite,
                TapeRequest::SystemCtlWrite { value: 0 },
            ),
        ];
        for (tag, request) in typed {
            assert_eq!(request.tag(), tag, "typed request for {tag:?}");
            assert!(
                !bare_tags.contains(&tag),
                "{tag:?} is discriminated, so it must NOT be reachable through \
                 a bare tag — a bare request would match any same-tag row"
            );
        }
        const ALL: [SourceTag; 13] = [
            SourceTag::Int1aAh00,
            SourceTag::Int1aAh0205,
            SourceTag::CmosPortRead,
            SourceTag::CmosPortWrite,
            SourceTag::PitRead,
            SourceTag::PitWrite,
            SourceTag::KbdDataRead,
            SourceTag::KbdStatusRead,
            SourceTag::KbdCmdWrite,
            SourceTag::PpiPortRead,
            SourceTag::PpiPortWrite,
            SourceTag::SystemCtlRead,
            SourceTag::SystemCtlWrite,
        ];
        for tag in ALL {
            match tag {
                SourceTag::Int1aAh00
                | SourceTag::Int1aAh0205
                | SourceTag::CmosPortRead
                | SourceTag::CmosPortWrite
                | SourceTag::PitRead
                | SourceTag::PitWrite
                | SourceTag::KbdDataRead
                | SourceTag::KbdStatusRead
                | SourceTag::KbdCmdWrite
                | SourceTag::PpiPortRead
                | SourceTag::PpiPortWrite
                | SourceTag::SystemCtlRead
                | SourceTag::SystemCtlWrite => {}
            }
            let bare = bare_tags.contains(&tag);
            let typed = typed.iter().any(|(t, _)| *t == tag);
            assert!(
                bare != typed,
                "{tag:?} must be requestable exactly one way: bare={bare} typed={typed}"
            );
        }
    }
    /// A discriminated row is only consumed by a request that matches its
    /// payload, and a near-miss leaves the cursor where it was.
    ///
    /// This is what the bare-tag route defeated: it would have hit on the
    /// first row of the right KIND, consuming it and shifting the rest
    /// (MDD-BUG-KIL-00820).
    #[test]
    fn a_discriminated_row_is_not_consumed_by_a_near_miss() {
        let rows: [(TapeSource, TapeRequest, TapeRequest); 4] = [
            (
                TapeSource::PitWrite {
                    port: 0x43,
                    value: 0x36,
                },
                TapeRequest::PitWrite {
                    port: 0x40,
                    value: 0x36,
                },
                TapeRequest::PitWrite {
                    port: 0x43,
                    value: 0x36,
                },
            ),
            (
                TapeSource::CmosPortWrite {
                    port: 0x71,
                    register: 0x0B,
                    value: 0x02,
                    nmi_gate: false,
                },
                TapeRequest::CmosPortWrite {
                    port: 0x71,
                    value: 0x52,
                },
                TapeRequest::CmosPortWrite {
                    port: 0x71,
                    value: 0x02,
                },
            ),
            (
                TapeSource::KbdCmdWrite { value: 0xAE },
                TapeRequest::KbdCmdWrite { value: 0xAD },
                TapeRequest::KbdCmdWrite { value: 0xAE },
            ),
            (
                TapeSource::PitRead {
                    port: 0x40,
                    value: 0x7F,
                },
                TapeRequest::PitRead { port: 0x41 },
                TapeRequest::PitRead { port: 0x40 },
            ),
        ];
        for (source, wrong, right) in rows {
            let tag = source.tag();
            let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
                at_pit_tick: 5,
                seq: 0,
                source,
            }]));
            assert!(
                !matches!(cur.next_response(wrong, 5), TapeOutcome::Hit(_)),
                "{tag:?}: a near-miss was accepted as a hit"
            );
            assert_eq!(
                cur.cursor(),
                0,
                "{tag:?}: a near-miss consumed the row, so every later row shifts"
            );
            assert!(
                matches!(cur.next_response(right, 5), TapeOutcome::Hit(_)),
                "{tag:?}: the exact request missed"
            );
            assert_eq!(
                cur.cursor(),
                1,
                "{tag:?}: the exact request did not advance"
            );
        }
    }
    /// The undiscriminated route still works: a bare tag hits its own kind and
    /// advances, and misses a row of any other kind.
    #[test]
    fn a_bare_tag_hits_its_own_kind_and_advances() {
        let rows: [(UndiscriminatedTag, TapeSource); 4] = [
            (
                UndiscriminatedTag::KbdDataRead,
                TapeSource::KbdDataRead { value: 0x1C },
            ),
            (
                UndiscriminatedTag::KbdStatusRead,
                TapeSource::KbdStatusRead { value: 0x14 },
            ),
            (
                UndiscriminatedTag::PpiPortRead,
                TapeSource::PpiPortRead { value: 0x30 },
            ),
            (
                UndiscriminatedTag::SystemCtlRead,
                TapeSource::SystemCtlRead { value: 0x02 },
            ),
        ];
        for (bare, source) in rows {
            let tag = bare.tag();
            let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
                at_pit_tick: 5,
                seq: 0,
                source,
            }]));
            let other = if bare == UndiscriminatedTag::KbdDataRead {
                UndiscriminatedTag::PpiPortRead
            } else {
                UndiscriminatedTag::KbdDataRead
            };
            assert!(
                !matches!(cur.next_response(other, 5), TapeOutcome::Hit(_)),
                "{tag:?}: {other:?} consumed a row of the wrong kind"
            );
            assert_eq!(
                cur.cursor(),
                0,
                "{tag:?}: the wrong kind advanced the cursor"
            );
            assert!(
                matches!(cur.next_response(bare, 5), TapeOutcome::Hit(_)),
                "{tag:?}: the bare request missed its own row"
            );
            assert_eq!(cur.cursor(), 1, "{tag:?}: the bare request did not advance");
        }
    }
    #[test]
    fn cursor_discriminator_mismatch_does_not_advance() {
        let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
            at_pit_tick: 5,
            seq: 0,
            source: TapeSource::CmosPortRead {
                register: 0x0A,
                value: 0x42,
            },
        }]));
        let out = cur.next_response(TapeRequest::CmosPortRead { register: 0x0B }, 5);
        assert_eq!(
            out,
            TapeOutcome::Mismatch {
                expected: SourceTag::CmosPortRead,
                found: SourceTag::CmosPortRead,
            }
        );
        assert_eq!(cur.cursor(), 0);
        let out2 = cur.next_response(TapeRequest::CmosPortRead { register: 0x0A }, 5);
        assert!(matches!(out2, TapeOutcome::Hit(_)));
        assert_eq!(cur.cursor(), 1);
        assert_eq!(cur.counters().mismatches, 1);
        assert_eq!(cur.counters().hits, 1);
    }
    #[test]
    fn cursor_only_advances_on_hit() {
        let mut cur = RtcTapeCursor::new(tape_with(vec![TapeEntry {
            at_pit_tick: 100,
            seq: 0,
            source: TapeSource::KbdDataRead { value: 0x42 },
        }]));
        for tick in [0, 1, 50, 99] {
            assert_eq!(
                cur.next_response(UndiscriminatedTag::KbdDataRead, tick),
                TapeOutcome::CoverageGap
            );
            assert_eq!(cur.cursor(), 0);
        }
        assert_eq!(cur.counters().coverage_gaps, 4);
    }
    #[test]
    fn cursor_sequenced_entries_consumed_in_order() {
        let mut cur = RtcTapeCursor::new(tape_with(vec![
            TapeEntry {
                at_pit_tick: 5,
                seq: 0,
                source: TapeSource::CmosPortRead {
                    register: 0x0A,
                    value: 0x06,
                },
            },
            TapeEntry {
                at_pit_tick: 5,
                seq: 1,
                source: TapeSource::Int1aAh00 {
                    al: 0,
                    cx: 0x12,
                    dx: 0x3456,
                },
            },
        ]));
        let a = cur.next_response(TapeRequest::CmosPortRead { register: 0x0A }, 5);
        let b = cur.next_response(UndiscriminatedTag::Int1aAh00, 5);
        assert!(matches!(a, TapeOutcome::Hit(_)));
        assert!(matches!(b, TapeOutcome::Hit(_)));
        assert_eq!(cur.cursor(), 2);
    }
    #[test]
    fn json_format_matches_hld_example() {
        let json = r#"{
            "schema_version": 1,
            "capture_date": "2026-05-16T...",
            "target_workload": "x",
            "entries": [
                {"at_pit_tick": 5, "seq": 0, "source": "cmos_port_read", "register": 10, "value": 6},
                {"at_pit_tick": 5, "seq": 1, "source": "int1a_ah00", "al": 0, "cx": 18, "dx": 13398}
            ]
        }"#;
        let tape = RtcTape::from_json_str(json).expect("parse HLD-shape");
        assert_eq!(tape.entries.len(), 2);
        assert_eq!(tape.entries[0].source.tag(), SourceTag::CmosPortRead);
        assert_eq!(tape.entries[1].source.tag(), SourceTag::Int1aAh00);
    }
    #[test]
    fn json_value_budget_counts_values_exactly() {
        let cases: &[(&str, usize)] = &[
            ("1", 1),
            ("[]", 2),
            ("[0,0,0]", 4),
            (r#"{"a":1,"b":2}"#, 3),
            (r#"{"a":{"b":1}}"#, 3),
            ("[[1,2],[3]]", 6),
        ];
        for (doc, values) in cases {
            assert!(
                check_json_value_budget(doc.as_bytes(), *values).is_ok(),
                "{doc} should fit a budget of exactly {values}"
            );
            assert!(
                check_json_value_budget(doc.as_bytes(), values - 1).is_err(),
                "{doc} should not fit a budget of {}",
                values - 1
            );
        }
    }
    #[test]
    fn json_value_budget_ignores_structure_inside_string_literals() {
        let doc = r#"{"a":"x,[{,,,","b":"he said \"[,[,\" ok"}"#;
        assert!(
            check_json_value_budget(doc.as_bytes(), 3).is_ok(),
            "object + 2 strings is 3 values whatever the strings contain"
        );
        assert!(check_json_value_budget(doc.as_bytes(), 2).is_err());
    }
    #[test]
    fn a_broad_array_is_rejected_before_its_expanded_dom_is_allocated() {
        let mut doc = String::from("[0");
        for _ in 0..64 {
            doc.push_str(",0");
        }
        doc.push(']');
        let err = check_json_value_budget(doc.as_bytes(), 16).expect_err("must reject");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("JSON values"),
            "unexpected error: {err}"
        );
    }
    #[test]
    fn real_sessions_are_far_inside_the_value_budget() {
        for entry in glob_session_files() {
            let bytes = std::fs::read(&entry).expect("read session fixture");
            check_json_value_budget(&bytes, MAX_JSON_VALUES)
                .unwrap_or_else(|e| panic!("{} exceeds the value budget: {e}", entry.display()));
        }
    }
    /// Every checked-in Gremlin session fixture, or an empty list when the
    /// tree is not present (the crate is built standalone in some lanes).
    fn glob_session_files() -> Vec<std::path::PathBuf> {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/gremlin/sessions");
        let mut out = Vec::new();
        let Ok(kinds) = std::fs::read_dir(&root) else {
            return out;
        };
        for kind in kinds.flatten() {
            let Ok(files) = std::fs::read_dir(kind.path()) else {
                continue;
            };
            for f in files.flatten() {
                if f.path().extension().is_some_and(|e| e == "json") {
                    out.push(f.path());
                }
            }
        }
        out
    }
}
