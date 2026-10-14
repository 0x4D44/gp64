//! Standard MIDI File parsing and bounded event transport.

use std::collections::HashSet;

/// Position units selected by a Standard MIDI File's division field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiTimeFormat {
    SongPointer,
    Smpte24,
    Smpte25,
    Smpte30,
    SmpteDropFrame,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TimedMidiEvent {
    at_us: u64,
    bytes: Vec<u8>,
    is_seek_state: bool,
    /// File-order identity of this packet's F0 opener; absent for F7 escapes.
    sysex_opener: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct TempoSegment {
    start_tick: u64,
    /// Sum of `delta_ticks * microseconds_per_quarter` through `start_tick`.
    /// Keeping the numerator avoids rounding at each tempo change.
    elapsed_numerator: u128,
    microseconds_per_quarter: u32,
}

#[derive(Debug, Clone)]
enum SequenceTiming {
    Ppqn {
        ticks_per_quarter: u16,
        tempo_segments: Vec<TempoSegment>,
        final_tick: u64,
    },
    Smpte {
        default_time_format: MidiTimeFormat,
    },
}

#[derive(Debug, Clone)]
enum RawEventKind {
    Midi(Vec<u8>),
    Tempo(u32),
}

#[derive(Debug, Clone)]
struct RawEvent {
    tick: u64,
    order: u64,
    kind: RawEventKind,
    is_seek_state: bool,
    /// File-order identity of this packet's F0 opener; absent for F7 escapes.
    sysex_opener: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct MidiAdvance {
    pub bytes: Vec<u8>,
    pub completed_callback: Option<u32>,
    /// True when a MIDI event remains partially admitted. The transport caller
    /// uses this to keep ownership until the event's remaining bytes drain.
    pub held: bool,
}

/// Loaded Standard MIDI File plus transport state.
#[derive(Debug, Clone)]
pub struct MidiSequence {
    events: Vec<TimedMidiEvent>,
    duration_us: u64,
    timing: SequenceTiming,
    position_us: u64,
    play_to_us: Option<u64>,
    next_event: usize,
    /// Bytes already admitted from `events[next_event]`. Keeping the offset
    /// into the loaded event avoids copying a large SysEx while the port is
    /// full.
    event_offset: usize,
    /// An event that started before a stop/seek/close and must finish before
    /// the transport can emit a later event or its silence block.
    draining_event: Option<(usize, usize)>,
    /// Prior channel-state events replayed after a nonzero seek. These are
    /// indices into `events`, in file order, and never include note events.
    seek_state_events: Vec<usize>,
    seek_state_index: usize,
    seek_state_offset: usize,
    /// Groups whose opening packets precede the most recent seek.
    skipped_sysex: HashSet<u64>,
    playing: bool,
    paused: bool,
    callback: Option<u32>,
}

impl MidiSequence {
    pub fn open(bytes: &[u8]) -> Result<Self, &'static str> {
        let bytes = unwrap_rmid(bytes)?;
        if bytes.get(..4) != Some(b"MThd") {
            return Err("missing MThd");
        }
        let header_len = read_be_u32(bytes, 4)? as usize;
        if header_len < 6 || 8usize.saturating_add(header_len) > bytes.len() {
            return Err("invalid MThd length");
        }
        let format = read_be_u16(bytes, 8)?;
        let track_count = read_be_u16(bytes, 10)?;
        let division = read_be_u16(bytes, 12)?;
        if format > 1 || track_count == 0 || division == 0 {
            return Err("unsupported MIDI header");
        }

        let mut cursor = 8 + header_len;
        let mut raw = Vec::new();
        let mut final_tick = 0u64;
        for track_index in 0..track_count {
            if bytes.get(cursor..cursor + 4) != Some(b"MTrk") {
                return Err("missing MTrk");
            }
            let length = read_be_u32(bytes, cursor + 4)? as usize;
            cursor = cursor.checked_add(8).ok_or("track offset overflow")?;
            let end = cursor.checked_add(length).ok_or("track length overflow")?;
            let track = bytes.get(cursor..end).ok_or("truncated MTrk")?;
            final_tick = final_tick.max(parse_track(track, track_index, &mut raw)?);
            cursor = end;
        }

        raw.sort_by_key(|event| (event.tick, event.order));
        let (events, duration_us, timing) = schedule_events(raw, final_tick, division)?;
        Ok(Self {
            events,
            duration_us,
            timing,
            position_us: 0,
            play_to_us: None,
            next_event: 0,
            event_offset: 0,
            draining_event: None,
            seek_state_events: Vec::new(),
            seek_state_index: 0,
            seek_state_offset: 0,
            skipped_sysex: HashSet::new(),
            playing: false,
            paused: false,
            callback: None,
        })
    }

    pub fn duration_us(&self) -> u64 {
        self.duration_us
    }

    /// Select song-pointer units for PPQN files and the corresponding frame
    /// format for SMPTE files.
    pub fn default_time_format(&self) -> MidiTimeFormat {
        match &self.timing {
            SequenceTiming::Ppqn { .. } => MidiTimeFormat::SongPointer,
            SequenceTiming::Smpte {
                default_time_format,
            } => *default_time_format,
        }
    }

    /// Convert a song-pointer position (sixteenth notes) through the file's
    /// PPQN tempo map. Callers reject this format for SMPTE files.
    pub fn song_pointer_to_us(&self, song_pointer: u32) -> u64 {
        let SequenceTiming::Ppqn {
            ticks_per_quarter,
            tempo_segments,
            final_tick,
        } = &self.timing
        else {
            return 0;
        };
        let ppqn = u128::from(*ticks_per_quarter);
        let denominator = ppqn * 4;
        let target_tick_numerator = u128::from(song_pointer) * ppqn;
        let final_tick_numerator = u128::from(*final_tick) * 4;
        let target_tick_numerator = target_tick_numerator.min(final_tick_numerator);

        let segment_index = tempo_segments
            .partition_point(|segment| u128::from(segment.start_tick) * 4 <= target_tick_numerator)
            .saturating_sub(1);
        let segment = &tempo_segments[segment_index];
        let elapsed_numerator = segment.elapsed_numerator.saturating_mul(4).saturating_add(
            target_tick_numerator
                .saturating_sub(u128::from(segment.start_tick) * 4)
                .saturating_mul(u128::from(segment.microseconds_per_quarter)),
        );
        u64::try_from(elapsed_numerator / denominator)
            .unwrap_or(self.duration_us)
            .min(self.duration_us)
    }

    /// Convert elapsed guest time to whole song-pointer units. The conversion
    /// uses the same piecewise tempo map as event scheduling and floors partial
    /// sixteenth notes, rounded down to a whole position.
    pub fn us_to_song_pointer(&self, position_us: u64) -> u32 {
        let SequenceTiming::Ppqn {
            ticks_per_quarter,
            tempo_segments,
            final_tick,
        } = &self.timing
        else {
            return 0;
        };
        let ppqn = u128::from(*ticks_per_quarter);
        if position_us >= self.duration_us {
            return u32::try_from(u128::from(*final_tick) * 4 / ppqn).unwrap_or(u32::MAX);
        }

        // The stored elapsed numerators all use PPQN as their denominator.
        let target_elapsed_numerator = u128::from(position_us) * ppqn;
        let segment_index = tempo_segments
            .partition_point(|segment| segment.elapsed_numerator / ppqn <= u128::from(position_us))
            .saturating_sub(1);
        let segment = &tempo_segments[segment_index];
        let tempo = u128::from(segment.microseconds_per_quarter);
        let elapsed_delta = target_elapsed_numerator.saturating_sub(segment.elapsed_numerator);
        let song_pointer_numerator = u128::from(segment.start_tick)
            .saturating_mul(tempo)
            .saturating_add(elapsed_delta)
            .saturating_mul(4);
        let song_pointer_denominator = ppqn.saturating_mul(tempo);
        u32::try_from(song_pointer_numerator / song_pointer_denominator).unwrap_or(u32::MAX)
    }

    pub fn position_us(&self) -> u64 {
        self.position_us
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn play(&mut self, from_us: Option<u64>, to_us: Option<u64>, callback: Option<u32>) {
        if let Some(from_us) = from_us {
            self.seek(from_us);
        }
        if self.position_us >= self.duration_us && !self.has_pending_event() {
            self.seek(0);
        }
        self.play_to_us = to_us.map(|value| value.min(self.duration_us));
        self.callback = callback;
        self.paused = false;
        self.playing = true;
    }

    pub fn stop(&mut self) {
        self.preserve_started_event();
        self.playing = false;
        self.paused = false;
        self.callback = None;
    }

    pub fn pause(&mut self) {
        if self.playing {
            self.preserve_started_event();
            self.playing = false;
            self.paused = true;
        }
    }

    pub fn resume(&mut self, callback: Option<u32>) {
        if self.paused {
            self.paused = false;
            self.playing = true;
            if callback.is_some() {
                self.callback = callback;
            }
        }
    }

    pub fn seek(&mut self, position_us: u64) {
        self.preserve_started_event();
        self.position_us = position_us.min(self.duration_us);
        self.next_event = self
            .events
            .partition_point(|event| event.at_us < self.position_us);
        self.event_offset = 0;
        self.skipped_sysex.clear();
        self.skipped_sysex.extend(
            self.events[..self.next_event]
                .iter()
                .filter_map(|event| event.sysex_opener),
        );
        self.seek_state_events.clear();
        self.seek_state_index = 0;
        self.seek_state_offset = 0;
        self.seek_state_events.extend(
            self.events[..self.next_event]
                .iter()
                .enumerate()
                .filter_map(|(index, event)| event.is_seek_state.then_some(index)),
        );
    }

    /// Whether this sequence has bytes from an already-started event that
    /// still need to reach the MIDI port.
    pub fn has_pending_event(&self) -> bool {
        self.event_offset != 0 || self.draining_event.is_some() || self.seek_state_offset != 0
    }

    fn preserve_started_event(&mut self) {
        if self.seek_state_offset != 0 {
            debug_assert!(self.draining_event.is_none());
            self.draining_event = Some((
                self.seek_state_events[self.seek_state_index],
                self.seek_state_offset,
            ));
            self.seek_state_index += 1;
            self.seek_state_offset = 0;
        }
        if self.event_offset != 0 {
            debug_assert!(self.draining_event.is_none());
            self.draining_event = Some((self.next_event, self.event_offset));
            // The old event is now represented by `draining_event`; advance
            // the normal cursor so resume/play does not replay its prefix.
            self.next_event += 1;
            self.event_offset = 0;
        }
    }

    /// Advance the sequence while allowing the caller to admit only a
    /// bounded prefix of each event. The closure returns the number of bytes
    /// it accepted; returning zero parks the event at its current offset.
    ///
    /// The returned `bytes` are a convenience copy for the existing
    /// unbounded API. Callers may limit admission to their destination capacity
    /// so each tick copies at most one bounded chunk.
    pub fn advance_into<F>(&mut self, delta_us: u64, mut admit: F) -> MidiAdvance
    where
        F: FnMut(&[u8]) -> usize,
    {
        self.advance_impl(delta_us, true, &mut admit)
    }

    /// Drain only an event that was started before transport cancellation or
    /// a seek. This intentionally does not release the sequence's old event
    /// cursor into the new timeline.
    pub fn drain_held_into<F>(&mut self, mut admit: F) -> MidiAdvance
    where
        F: FnMut(&[u8]) -> usize,
    {
        self.advance_impl(0, false, &mut admit)
    }

    fn advance_impl<F>(&mut self, delta_us: u64, allow_playback: bool, admit: &mut F) -> MidiAdvance
    where
        F: FnMut(&[u8]) -> usize,
    {
        let mut result = MidiAdvance::default();
        let mut parked_at = None;

        let end = self.play_to_us.unwrap_or(self.duration_us);
        let target = if allow_playback && self.playing {
            self.position_us.saturating_add(delta_us).min(end)
        } else {
            self.position_us
        };

        loop {
            // A cancelled/seeked event has precedence over the new timeline.
            if let Some((event_index, offset)) = self.draining_event {
                let Some(event) = self.events.get(event_index) else {
                    self.draining_event = None;
                    continue;
                };
                let remaining = &event.bytes[offset..];
                if remaining.is_empty() {
                    self.draining_event = None;
                    continue;
                }
                let accepted = admit(remaining).min(remaining.len());
                if accepted != 0 {
                    result.bytes.extend_from_slice(&remaining[..accepted]);
                    if accepted == remaining.len() {
                        self.draining_event = None;
                        continue;
                    }
                    self.draining_event = Some((event_index, offset + accepted));
                }
                result.held = true;
                break;
            }

            if !allow_playback || !self.playing {
                break;
            }

            if self.seek_state_index < self.seek_state_events.len() {
                let event_index = self.seek_state_events[self.seek_state_index];
                let event = &self.events[event_index];
                let remaining = &event.bytes[self.seek_state_offset..];
                let accepted = admit(remaining).min(remaining.len());
                if accepted != 0 {
                    result.bytes.extend_from_slice(&remaining[..accepted]);
                    self.seek_state_offset += accepted;
                    if self.seek_state_offset == event.bytes.len() {
                        self.seek_state_index += 1;
                        self.seek_state_offset = 0;
                        if self.seek_state_index == self.seek_state_events.len() {
                            self.seek_state_events.clear();
                            self.seek_state_index = 0;
                        }
                        continue;
                    }
                }
                result.held = true;
                break;
            }

            let Some(event) = self.events.get(self.next_event) else {
                break;
            };
            if event.at_us > target || event.at_us > end {
                break;
            }
            // A seek that skipped the opening packet must also skip its
            // continuations. Keep the time gate above so suppressed packets
            // retain their timeline boundaries and completion behavior.
            if event
                .sysex_opener
                .is_some_and(|opener| self.skipped_sysex.contains(&opener))
            {
                self.next_event += 1;
                continue;
            }
            let remaining = &event.bytes[self.event_offset..];
            if remaining.is_empty() {
                self.next_event += 1;
                self.event_offset = 0;
                continue;
            }
            let accepted = admit(remaining).min(remaining.len());
            if accepted != 0 {
                result.bytes.extend_from_slice(&remaining[..accepted]);
                self.event_offset += accepted;
                if self.event_offset == event.bytes.len() {
                    self.next_event += 1;
                    self.event_offset = 0;
                    continue;
                }
            }
            // An event that has emitted at least one byte owns the port. An
            // untouched event simply waits for admission without changing
            // the owner, so another sequence cannot be interleaved either.
            parked_at = Some(event.at_us);
            result.held = true;
            break;
        }

        if allow_playback && self.playing {
            // A blocked normal event is the next visible timeline boundary.
            // Keep an explicitly sought position intact while an older
            // cancelled event drains; that path never sets `parked_at`.
            if let Some(parked_at) = parked_at {
                self.position_us = parked_at;
            } else if self.draining_event.is_none() {
                self.position_us = target;
            }
            if target >= end
                && self.draining_event.is_none()
                && self.event_offset == 0
                && self.seek_state_index >= self.seek_state_events.len()
                && self.seek_state_offset == 0
                && self
                    .events
                    .get(self.next_event)
                    .is_none_or(|event| event.at_us > end)
            {
                self.playing = false;
                self.paused = false;
                result.completed_callback = self.callback.take();
            }
        }

        result
    }
}

fn is_seek_state_status(status: u8) -> bool {
    matches!(status & 0xf0, 0xb0 | 0xc0 | 0xd0 | 0xe0)
}

fn unwrap_rmid(bytes: &[u8]) -> Result<&[u8], &'static str> {
    if bytes.get(..4) != Some(b"RIFF") {
        return Ok(bytes);
    }
    if bytes.get(8..12) != Some(b"RMID") {
        return Err("unsupported RIFF form");
    }
    let mut cursor = 12usize;
    while cursor.saturating_add(8) <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let length = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        cursor += 8;
        let end = cursor.checked_add(length).ok_or("RMID chunk overflow")?;
        let data = bytes.get(cursor..end).ok_or("truncated RMID chunk")?;
        if id == b"data" {
            return Ok(data);
        }
        cursor = end.saturating_add(length & 1);
    }
    Err("RMID has no data chunk")
}

fn parse_track(
    track: &[u8],
    track_index: u16,
    output: &mut Vec<RawEvent>,
) -> Result<u64, &'static str> {
    let mut cursor = 0usize;
    let mut tick = 0u64;
    let mut sequence = 0u64;
    let mut running_status = None;
    let mut sysex_opener = None;
    while cursor < track.len() {
        tick = tick
            .checked_add(u64::from(read_vlq(track, &mut cursor)?))
            .ok_or("SMF tick overflow")?;
        let first = *track.get(cursor).ok_or("missing event status")?;
        let status = if first & 0x80 != 0 {
            cursor += 1;
            if first < 0xf0 {
                running_status = Some(first);
            } else {
                running_status = None;
            }
            first
        } else {
            running_status.ok_or("running status without channel status")?
        };
        let order = (u64::from(track_index) << 32) | sequence;
        sequence += 1;
        match status {
            0x80..=0xef => {
                let length = if status & 0xe0 == 0xc0 { 1 } else { 2 };
                let mut message = Vec::with_capacity(length + 1);
                message.push(status);
                for _ in 0..length {
                    let byte = *track.get(cursor).ok_or("truncated channel event")?;
                    if byte & 0x80 != 0 {
                        return Err("status byte in channel data");
                    }
                    cursor += 1;
                    message.push(byte);
                }
                output.push(RawEvent {
                    tick,
                    order,
                    kind: RawEventKind::Midi(message),
                    is_seek_state: is_seek_state_status(status),
                    sysex_opener: None,
                });
            }
            0xff => {
                let meta_type = *track.get(cursor).ok_or("missing meta type")?;
                cursor += 1;
                let length = read_vlq(track, &mut cursor)? as usize;
                let data = track
                    .get(cursor..cursor.saturating_add(length))
                    .ok_or("truncated meta event")?;
                cursor += length;
                if meta_type == 0x51 && data.len() == 3 {
                    let tempo =
                        (u32::from(data[0]) << 16) | (u32::from(data[1]) << 8) | u32::from(data[2]);
                    if tempo != 0 {
                        output.push(RawEvent {
                            tick,
                            order,
                            kind: RawEventKind::Tempo(tempo),
                            is_seek_state: false,
                            sysex_opener: None,
                        });
                    }
                }
                if meta_type == 0x2f {
                    break;
                }
            }
            0xf0 | 0xf7 => {
                let length = read_vlq(track, &mut cursor)? as usize;
                let data = track
                    .get(cursor..cursor.saturating_add(length))
                    .ok_or("truncated SysEx event")?;
                cursor += length;
                if status == 0xf0 {
                    sysex_opener = Some(order);
                }
                let packet_opener = sysex_opener;
                if data.last() == Some(&0xf7) {
                    sysex_opener = None;
                }
                let mut message = Vec::with_capacity(data.len() + 2);
                if status == 0xf0 {
                    message.push(0xf0);
                }
                message.extend_from_slice(data);
                output.push(RawEvent {
                    tick,
                    order,
                    kind: RawEventKind::Midi(message),
                    is_seek_state: false,
                    sysex_opener: packet_opener,
                });
            }
            _ => return Err("unsupported system event in SMF"),
        }
    }
    Ok(tick)
}

fn schedule_events(
    raw: Vec<RawEvent>,
    final_tick: u64,
    division: u16,
) -> Result<(Vec<TimedMidiEvent>, u64, SequenceTiming), &'static str> {
    let mut events = Vec::new();
    if division & 0x8000 == 0 {
        let ticks_per_quarter = u128::from(division);
        let mut tempo = 500_000u128;
        let mut elapsed_numerator = 0u128;
        let mut previous_tick = 0u64;
        let mut tempo_segments = vec![TempoSegment {
            start_tick: 0,
            elapsed_numerator: 0,
            microseconds_per_quarter: 500_000,
        }];
        for event in raw {
            let tick_delta = event
                .tick
                .checked_sub(previous_tick)
                .ok_or("non-monotonic SMF event time")?;
            elapsed_numerator = elapsed_numerator
                .checked_add(
                    u128::from(tick_delta)
                        .checked_mul(tempo)
                        .ok_or("SMF timing multiplication overflow")?,
                )
                .ok_or("SMF timing accumulation overflow")?;
            previous_tick = event.tick;
            match event.kind {
                RawEventKind::Midi(bytes) => events.push(TimedMidiEvent {
                    at_us: u64::try_from(elapsed_numerator / ticks_per_quarter)
                        .map_err(|_| "SMF duration exceeds microsecond range")?,
                    bytes,
                    is_seek_state: event.is_seek_state,
                    sysex_opener: event.sysex_opener,
                }),
                RawEventKind::Tempo(value) => {
                    tempo = u128::from(value);
                    tempo_segments.push(TempoSegment {
                        start_tick: event.tick,
                        elapsed_numerator,
                        microseconds_per_quarter: value,
                    });
                }
            }
        }
        let final_tick_delta = final_tick
            .checked_sub(previous_tick)
            .ok_or("non-monotonic SMF final time")?;
        elapsed_numerator = elapsed_numerator
            .checked_add(
                u128::from(final_tick_delta)
                    .checked_mul(tempo)
                    .ok_or("SMF timing multiplication overflow")?,
            )
            .ok_or("SMF timing accumulation overflow")?;
        let duration = u64::try_from(elapsed_numerator / ticks_per_quarter)
            .map_err(|_| "SMF duration exceeds microsecond range")?;
        Ok((
            events,
            duration,
            SequenceTiming::Ppqn {
                ticks_per_quarter: division,
                tempo_segments,
                final_tick,
            },
        ))
    } else {
        let fps_code = (division >> 8) as u8 as i8;
        let (fps_numerator, fps_denominator, default_time_format) = match fps_code {
            -24 => (24u64, 1u64, MidiTimeFormat::Smpte24),
            -25 => (25, 1, MidiTimeFormat::Smpte25),
            -29 => (30_000, 1_001, MidiTimeFormat::SmpteDropFrame),
            -30 => (30, 1, MidiTimeFormat::Smpte30),
            _ => return Err("invalid SMPTE division"),
        };
        let ticks_per_frame = u64::from(division & 0xff);
        if ticks_per_frame == 0 {
            return Err("zero SMPTE ticks per frame");
        }
        let denominator = fps_numerator.saturating_mul(ticks_per_frame);
        for event in raw {
            let is_seek_state = event.is_seek_state;
            if let RawEventKind::Midi(bytes) = event.kind {
                let at_us = u128::from(event.tick)
                    .checked_mul(1_000_000)
                    .ok_or("SMF timing multiplication overflow")?
                    .checked_mul(u128::from(fps_denominator))
                    .ok_or("SMF timing multiplication overflow")?
                    .div_ceil(u128::from(denominator));
                events.push(TimedMidiEvent {
                    at_us: u64::try_from(at_us)
                        .map_err(|_| "SMF duration exceeds microsecond range")?,
                    bytes,
                    is_seek_state,
                    sysex_opener: event.sysex_opener,
                });
            }
        }
        let duration = u128::from(final_tick)
            .checked_mul(1_000_000)
            .ok_or("SMF timing multiplication overflow")?
            .checked_mul(u128::from(fps_denominator))
            .ok_or("SMF timing multiplication overflow")?
            .div_ceil(u128::from(denominator));
        Ok((
            events,
            u64::try_from(duration).map_err(|_| "SMF duration exceeds microsecond range")?,
            SequenceTiming::Smpte {
                default_time_format,
            },
        ))
    }
}

fn read_vlq(bytes: &[u8], cursor: &mut usize) -> Result<u32, &'static str> {
    let mut value = 0u32;
    for _ in 0..4 {
        let byte = *bytes.get(*cursor).ok_or("truncated variable integer")?;
        *cursor += 1;
        value = value.checked_shl(7).ok_or("variable integer overflow")? | u32::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("variable integer exceeds four bytes")
}

fn read_be_u16(bytes: &[u8], offset: usize) -> Result<u16, &'static str> {
    Ok(u16::from_be_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or("truncated u16")?
            .try_into()
            .unwrap(),
    ))
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Result<u32, &'static str> {
    Ok(u32::from_be_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or("truncated u32")?
            .try_into()
            .unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequence_with_events(events: impl IntoIterator<Item = (u64, Vec<u8>)>) -> MidiSequence {
        let events: Vec<TimedMidiEvent> = events
            .into_iter()
            .map(|(at_us, bytes)| TimedMidiEvent {
                at_us,
                is_seek_state: bytes
                    .first()
                    .is_some_and(|status| is_seek_state_status(*status)),
                bytes,
                sysex_opener: None,
            })
            .collect();
        let duration_us = events.last().map(|event| event.at_us).unwrap_or(0);
        MidiSequence {
            events,
            duration_us,
            timing: SequenceTiming::Ppqn {
                ticks_per_quarter: 96,
                tempo_segments: vec![TempoSegment {
                    start_tick: 0,
                    elapsed_numerator: 0,
                    microseconds_per_quarter: 500_000,
                }],
                final_tick: 0,
            },
            position_us: 0,
            play_to_us: None,
            next_event: 0,
            event_offset: 0,
            draining_event: None,
            seek_state_events: Vec::new(),
            seek_state_index: 0,
            seek_state_offset: 0,
            skipped_sysex: HashSet::new(),
            playing: false,
            paused: false,
            callback: None,
        }
    }

    fn midi_with_track(division: u16, track: &[u8]) -> Vec<u8> {
        let mut midi = b"MThd\0\0\0\x06\0\0\0\x01".to_vec();
        midi.extend_from_slice(&division.to_be_bytes());
        midi.extend_from_slice(b"MTrk");
        midi.extend_from_slice(&(track.len() as u32).to_be_bytes());
        midi.extend_from_slice(track);
        midi
    }

    #[test]
    fn default_time_format_matches_ppqn_and_smpte_divisions() {
        let ppqn = midi_with_track(96, &[0, 0xff, 0x2f, 0]);
        assert_eq!(
            MidiSequence::open(&ppqn)
                .expect("valid PPQN SMF")
                .default_time_format(),
            MidiTimeFormat::SongPointer
        );

        for (division, expected) in [
            (0xe8_40, MidiTimeFormat::Smpte24),
            (0xe7_40, MidiTimeFormat::Smpte25),
            (0xe3_40, MidiTimeFormat::SmpteDropFrame),
            (0xe2_40, MidiTimeFormat::Smpte30),
        ] {
            let smpte = midi_with_track(division, &[0, 0xff, 0x2f, 0]);
            assert_eq!(
                MidiSequence::open(&smpte)
                    .expect("valid SMPTE SMF")
                    .default_time_format(),
                expected,
                "division {division:#06x}"
            );
        }
    }

    #[test]
    fn smpte_drop_frame_uses_the_standard_2997_rate() {
        let midi = midi_with_track(0xe3_28, &[0x1e, 0x90, 0x3c, 0x40, 0, 0xff, 0x2f, 0]);
        let sequence = MidiSequence::open(&midi).expect("valid SMPTE drop-frame SMF");

        assert_eq!(sequence.duration_us(), 25_025);
    }

    #[test]
    fn smpte_frame_timing_rounds_up_to_retain_one_frame() {
        let midi = midi_with_track(0xe8_01, &[1, 0x90, 0x3c, 0x40, 0, 0xff, 0x2f, 0]);
        let mut sequence = MidiSequence::open(&midi).expect("valid SMPTE 24 SMF");

        assert_eq!(sequence.duration_us(), 41_667);
        sequence.play(Some(0), None, None);
        assert!(sequence
            .advance_into(41_666, |bytes| bytes.len())
            .bytes
            .is_empty());
        assert_eq!(
            sequence.advance_into(1, |bytes| bytes.len()).bytes,
            [0x90, 0x3c, 0x40]
        );
    }

    #[test]
    fn song_pointer_conversion_uses_each_tempo_map_segment() {
        // At 96 PPQN, one quarter note is four song-pointer units. The tempo
        // changes from 120 BPM to 60 BPM after the first quarter note.
        let track = [
            0x00, 0xff, 0x51, 0x03, 0x07, 0xa1, 0x20, // 500,000 us / quarter
            0x60, 0xff, 0x51, 0x03, 0x0f, 0x42, 0x40, // after one quarter: 1,000,000
            0x60, 0xff, 0x2f, 0x00, // another quarter, then end
        ];
        let midi = midi_with_track(96, &track);
        let sequence = MidiSequence::open(&midi).expect("valid tempo-map SMF");

        assert_eq!(sequence.duration_us(), 1_500_000);
        assert_eq!(sequence.song_pointer_to_us(2), 250_000);
        assert_eq!(sequence.song_pointer_to_us(4), 500_000);
        assert_eq!(sequence.song_pointer_to_us(5), 750_000);
        assert_eq!(sequence.song_pointer_to_us(6), 1_000_000);
        assert_eq!(sequence.song_pointer_to_us(8), 1_500_000);
        assert_eq!(sequence.song_pointer_to_us(u32::MAX), 1_500_000);
        assert_eq!(sequence.us_to_song_pointer(250_000), 2);
        assert_eq!(sequence.us_to_song_pointer(500_000), 4);
        assert_eq!(sequence.us_to_song_pointer(750_000), 5);
        assert_eq!(sequence.us_to_song_pointer(1_000_000), 6);
        assert_eq!(sequence.us_to_song_pointer(1_500_000), 8);
        assert_eq!(sequence.us_to_song_pointer(u64::MAX), 8);
    }

    #[test]
    fn song_pointer_conversion_keeps_fractional_ppqn_ticks() {
        let midi = midi_with_track(3, &[3, 0xff, 0x2f, 0]);
        let sequence = MidiSequence::open(&midi).expect("valid low-PPQN SMF");

        assert_eq!(sequence.duration_us(), 500_000);
        assert_eq!(sequence.song_pointer_to_us(1), 125_000);
        assert_eq!(sequence.us_to_song_pointer(125_000), 1);
        assert_eq!(sequence.song_pointer_to_us(4), 500_000);
        assert_eq!(sequence.us_to_song_pointer(500_000), 4);
    }

    #[test]
    fn seek_replays_prior_channel_state_without_replaying_notes() {
        let track = [
            0, 0xb0, 0, 2, // bank MSB
            0, 0xc0, 5, // program
            0, 0xb0, 32, 3, // bank LSB
            0, 0xe0, 0, 0x40, // pitch bend
            0, 0xd0, 0x20, // channel pressure
            24, 0xb0, 0, 4, // later bank MSB change
            72, 0x90, 60, 100, // note on at 500 ms
            12, 0x80, 60, 0, // note off at 562.5 ms
            84, 0x90, 64, 100, // future note on at 1 s
            96, 0xff, 0x2f, 0,
        ];
        let midi = midi_with_track(96, &track);
        let mut sequence = MidiSequence::open(&midi).expect("valid channel-state SMF");
        sequence.seek(750_000);
        assert!(
            sequence
                .advance_into(0, |bytes| bytes.len())
                .bytes
                .is_empty(),
            "seek state is emitted only after playback starts"
        );
        sequence.play(None, None, None);

        assert_eq!(
            sequence.advance_into(0, |bytes| bytes.len()).bytes,
            [0xb0, 0, 2, 0xc0, 5, 0xb0, 32, 3, 0xe0, 0, 0x40, 0xd0, 0x20, 0xb0, 0, 4,]
        );
        assert!(sequence
            .advance_into(249_999, |bytes| bytes.len())
            .bytes
            .is_empty());
        assert_eq!(
            sequence.advance_into(1, |bytes| bytes.len()).bytes,
            [0x90, 64, 100]
        );
    }

    #[test]
    fn split_sysex_fragments_keep_their_original_timestamps() {
        let track = [
            0, 0xf0, 2, 0x41, 0x10, // start packet, continued one quarter later
            0x60, 0xf7, 2, 0x20, 0xf7, // continuation and terminator
            0x60, 0xff, 0x2f, 0,
        ];
        let midi = midi_with_track(96, &track);
        let mut sequence = MidiSequence::open(&midi).expect("valid split SysEx SMF");
        sequence.play(Some(0), None, None);

        assert_eq!(
            sequence.advance_into(0, |bytes| bytes.len()).bytes,
            [0xf0, 0x41, 0x10]
        );
        assert!(sequence
            .advance_into(499_999, |bytes| bytes.len())
            .bytes
            .is_empty());
        assert_eq!(
            sequence.advance_into(1, |bytes| bytes.len()).bytes,
            [0x20, 0xf7]
        );
    }

    fn split_sysex_seek_fixture(division: u16) -> Vec<u8> {
        midi_with_track(
            division,
            &[
                0, 0xc0, 5, // channel state must still replay
                10, 0xf0, 2, 0x41, 0x10, 10, 0xf7, 1, 0x20, // middle fragment, still open
                10, 0xf7, 2, 0x30, 0xf7, 10, 0xf7, 3, 0x90, 60,
                64, // independent escape after completion
                10, 0xf0, 1, 0x42, // future complete split group
                10, 0xf7, 2, 0x40, 0xf7, 10, 0x90, 62, 65, 10, 0xff, 0x2f, 0,
            ],
        )
    }

    // Capture the actual bytes offered to the sink, independently of the
    // convenience copy returned by MidiAdvance.
    fn collect_sink(sequence: &mut MidiSequence, delta_us: u64) -> Vec<u8> {
        let mut emitted = Vec::new();
        sequence.advance_into(delta_us, |bytes| {
            emitted.extend_from_slice(bytes);
            bytes.len()
        });
        emitted
    }

    #[test]
    fn seeking_past_sysex_opener_suppresses_all_its_continuations() {
        for division in [100, 0xe7_08] {
            // both clocks give 50 ms per ten ticks
            let midi = split_sysex_seek_fixture(division);
            for position in [75_000, 100_000, 125_000, 150_000] {
                let mut sequence = MidiSequence::open(&midi).unwrap();
                sequence.seek(position);
                sequence.play(None, None, None);
                assert_eq!(collect_sink(&mut sequence, 0), [0xc0, 5]);
                assert_eq!(
                    collect_sink(&mut sequence, 150_000 - position),
                    Vec::<u8>::new(),
                    "orphan reached sink: division {division:#x}, seek {position}"
                );
                assert_eq!(collect_sink(&mut sequence, 50_000), [0x90, 60, 64]);
                assert_eq!(collect_sink(&mut sequence, 50_000), [0xf0, 0x42]);
                assert_eq!(collect_sink(&mut sequence, 50_000), [0x40, 0xf7]);
                assert_eq!(collect_sink(&mut sequence, 50_000), [0x90, 62, 65]);
            }
        }
    }

    #[test]
    fn repeated_seeks_restore_sysex_at_zero_and_exact_opener() {
        for division in [100, 0xe7_08] {
            let midi = split_sysex_seek_fixture(division);
            for restored_position in [0, 50_000] {
                let mut sequence = MidiSequence::open(&midi).unwrap();
                sequence.seek(125_000);
                sequence.seek(restored_position);
                sequence.play(None, None, None);
                assert_eq!(
                    collect_sink(&mut sequence, 50_000 - restored_position),
                    [0xc0, 5, 0xf0, 0x41, 0x10]
                );
                assert_eq!(collect_sink(&mut sequence, 49_999), Vec::<u8>::new());
                assert_eq!(collect_sink(&mut sequence, 1), [0x20]);
                assert_eq!(collect_sink(&mut sequence, 49_999), Vec::<u8>::new());
                assert_eq!(collect_sink(&mut sequence, 1), [0x30, 0xf7]);
                assert_eq!(collect_sink(&mut sequence, 50_000), [0x90, 60, 64]);
            }
            let midi = midi_with_track(
                division,
                &[
                    10, 0xf0, 1, 0x41, 0, 0xf7, 1, 0x20, 0, 0xf7, 2, 0x30, 0xf7, 10, 0xff, 0x2f, 0,
                ],
            );
            let mut sequence = MidiSequence::open(&midi).unwrap();
            sequence.seek(75_000);
            sequence.seek(50_000);
            sequence.play(None, None, None);
            assert_eq!(
                collect_sink(&mut sequence, 0),
                [0xf0, 0x41, 0x20, 0x30, 0xf7]
            );
        }
    }

    #[test]
    fn seek_sysex_membership_is_per_track_after_merging() {
        for division in [100u16, 0xe7_08] {
            let tracks: [&[u8]; 3] = [
                &[
                    10, 0xf0, 1, 0x41, 20, 0xf7, 2, 0x10, 0xf7, 50, 0xff, 0x2f, 0,
                ],
                &[
                    40, 0xf0, 1, 0x42, // both tracks' openers have event sequence zero
                    10, 0xf7, 2, 0x20, 0xf7, 10, 0xf7, 3, 0x90, 62,
                    65, // standalone after this track's group
                    20, 0xff, 0x2f, 0,
                ],
                &[20, 0xf7, 3, 0x90, 60, 64, 60, 0xff, 0x2f, 0],
            ];
            let mut midi = b"MThd\0\0\0\x06\0\x01\0\x03".to_vec();
            midi.extend_from_slice(&division.to_be_bytes());
            for track in tracks {
                midi.extend_from_slice(b"MTrk");
                midi.extend_from_slice(&(track.len() as u32).to_be_bytes());
                midi.extend_from_slice(track);
            }
            let mut sequence = MidiSequence::open(&midi).unwrap();
            sequence.seek(75_000);
            sequence.play(None, None, None);
            assert_eq!(collect_sink(&mut sequence, 25_000), [0x90, 60, 64]);
            assert_eq!(collect_sink(&mut sequence, 50_000), Vec::<u8>::new());
            assert_eq!(collect_sink(&mut sequence, 50_000), [0xf0, 0x42]);
            assert_eq!(collect_sink(&mut sequence, 50_000), [0x20, 0xf7]);
            assert_eq!(collect_sink(&mut sequence, 50_000), [0x90, 62, 65]);
        }
    }

    #[test]
    fn format_one_tracks_merge_on_the_shared_tempo_map() {
        let midi = b"MThd\0\0\0\x06\0\x01\0\x02\0\x60\
                 MTrk\0\0\0\x0b\0\xff\x51\x03\x03\xd0\x90\x60\xff\x2f\0\
                 MTrk\0\0\0\x0c\0\x90\x3c\x40\x60\x80\x3c\x40\0\xff\x2f\0";
        let mut sequence = MidiSequence::open(midi).expect("valid format-one SMF");
        assert_eq!(sequence.duration_us(), 250_000);
        sequence.play(Some(0), None, None);
        assert_eq!(
            sequence.advance_into(0, |bytes| bytes.len()).bytes,
            [0x90, 0x3c, 0x40]
        );
        assert_eq!(
            sequence.advance_into(250_000, |bytes| bytes.len()).bytes,
            [0x80, 0x3c, 0x40]
        );
        assert!(!sequence.is_playing());
    }

    #[test]
    fn running_status_is_expanded_for_the_synth_byte_stream() {
        let midi =
            b"MThd\0\0\0\x06\0\0\0\x01\0\x60MTrk\0\0\0\x0b\0\x90\x3c\x40\x01\x3d\x41\0\xff\x2f\0";
        let mut sequence = MidiSequence::open(midi).expect("valid running-status SMF");
        sequence.play(Some(0), None, None);
        assert_eq!(
            sequence.advance_into(6_000, |bytes| bytes.len()).bytes,
            [0x90, 0x3c, 0x40, 0x90, 0x3d, 0x41]
        );
    }

    #[test]
    fn large_sysex_does_not_escape_the_transport_budget() {
        let mut track = vec![0, 0xf0, 0xa7, 0x09];
        track.extend(std::iter::repeat_n(1u8, 5_000));
        track.push(0xf7);
        track.extend([0, 0xff, 0x2f, 0]);
        let mut midi = b"MThd\0\0\0\x06\0\0\0\x01\0\x60MTrk".to_vec();
        midi.extend_from_slice(&(track.len() as u32).to_be_bytes());
        midi.extend_from_slice(&track);

        let mut sequence = MidiSequence::open(&midi).expect("valid large SysEx SMF");
        sequence.play(Some(0), None, None);
        let mut first = Vec::new();
        let first_advance = sequence.advance_into(0, |bytes| {
            let accepted = bytes.len().min(4_096);
            first.extend_from_slice(&bytes[..accepted]);
            accepted
        });
        assert_eq!(first.len(), 4_096);
        assert!(
            first_advance.held,
            "the event must remain owned while partial"
        );

        let mut second = Vec::new();
        let second_advance = sequence.advance_into(0, |bytes| {
            second.extend_from_slice(bytes);
            bytes.len()
        });
        assert_eq!(second.len(), 5_002 - 4_096);
        assert!(!second_advance.held);
        assert_eq!(first.len() + second.len(), 5_002);
    }

    #[test]
    fn seek_keeps_a_started_event_ahead_of_the_new_cursor() {
        let mut sequence =
            sequence_with_events([(0, vec![0xf0; 5_000]), (1_000, vec![0x90, 0x40, 0x7f])]);
        sequence.play(Some(0), None, None);

        let first = sequence.advance_into(0, |bytes| bytes.len().min(4_000));
        assert!(first.held);
        sequence.seek(1_000);

        let drained = sequence.drain_held_into(|bytes| bytes.len());
        assert_eq!(drained.bytes.len(), 1_000);
        assert_eq!(drained.bytes, vec![0xf0; 1_000]);

        let resumed = sequence.advance_into(0, |bytes| bytes.len());
        assert_eq!(resumed.bytes, [0x90, 0x40, 0x7f]);
    }

    #[test]
    fn pause_drain_resume_does_not_replay_started_event() {
        let mut sequence =
            sequence_with_events([(0, vec![0xf0, 1, 2, 3, 4, 5]), (0, vec![0x90, 0x40, 0x7f])]);
        sequence.play(Some(0), None, None);

        let first = sequence.advance_into(0, |bytes| bytes.len().min(3));
        assert_eq!(first.bytes, [0xf0, 1, 2]);
        assert!(first.held);

        sequence.pause();
        let drained = sequence.drain_held_into(|bytes| bytes.len());
        assert_eq!(drained.bytes, [3, 4, 5]);
        assert!(!drained.held);

        sequence.resume(None);
        let resumed = sequence.advance_into(0, |bytes| bytes.len());
        assert_eq!(resumed.bytes, [0x90, 0x40, 0x7f]);
    }

    #[test]
    fn play_does_not_rewind_a_pending_final_event() {
        let mut sequence = sequence_with_events([(0, vec![0xf0, 1, 2, 3, 4, 5])]);
        sequence.play(Some(0), None, None);

        let first = sequence.advance_into(0, |bytes| bytes.len().min(3));
        assert_eq!(first.bytes, [0xf0, 1, 2]);
        assert!(first.held);

        sequence.stop();
        sequence.play(None, None, None);
        let resumed = sequence.advance_into(0, |bytes| bytes.len());
        assert_eq!(resumed.bytes, [3, 4, 5]);
        assert!(!resumed.held);
    }

    #[test]
    fn blocked_event_parks_position_and_defers_completion_without_losing_time() {
        let mut sequence = sequence_with_events([
            (0, vec![0xf0, 1, 2, 3, 4, 5]),
            (1_000, vec![0x90, 0x40, 0x7f]),
            (2_000, vec![0x80, 0x40, 0]),
        ]);
        sequence.play(Some(0), None, Some(0x1234));

        let blocked = sequence.advance_into(1_000, |_| 0);
        assert!(blocked.bytes.is_empty());
        assert!(blocked.held);
        assert_eq!(sequence.position_us(), 0);
        assert!(sequence.is_playing());
        assert_eq!(blocked.completed_callback, None);

        let caught_up = sequence.advance_into(1_000, |bytes| bytes.len());
        assert_eq!(caught_up.bytes, [0xf0, 1, 2, 3, 4, 5, 0x90, 0x40, 0x7f]);
        assert_eq!(sequence.position_us(), 1_000);
        assert!(sequence.is_playing());
        assert_eq!(caught_up.completed_callback, None);

        let completed = sequence.advance_into(1_000, |bytes| bytes.len());
        assert_eq!(completed.bytes, [0x80, 0x40, 0]);
        assert_eq!(completed.completed_callback, Some(0x1234));
        assert!(!sequence.is_playing());
    }

    #[test]
    fn explicit_seek_position_survives_a_blocked_cancelled_event() {
        let mut sequence = sequence_with_events([
            (0, vec![0xf0, 1, 2, 3, 4, 5]),
            (1_000, vec![0x90, 0x40, 0x7f]),
            (2_000, vec![0x80, 0x40, 0]),
        ]);
        sequence.play(Some(0), None, None);

        let first = sequence.advance_into(0, |bytes| bytes.len().min(3));
        assert_eq!(first.bytes, [0xf0, 1, 2]);
        assert!(first.held);

        sequence.seek(1_000);
        assert_eq!(sequence.position_us(), 1_000);
        let blocked = sequence.advance_into(1_000, |_| 0);
        assert!(blocked.bytes.is_empty());
        assert!(blocked.held);
        // The cancelled event remains ahead of the explicit seek, but it
        // must not consume host time or move the visible cursor.
        assert_eq!(sequence.position_us(), 1_000);

        let blocked_again = sequence.advance_into(5_000, |_| 0);
        assert!(blocked_again.bytes.is_empty());
        assert!(blocked_again.held);
        assert_eq!(sequence.position_us(), 1_000);
    }

    #[test]
    fn same_timestamp_events_remain_contiguous_across_bounded_chunks() {
        let expected: Vec<u8> = (0..2_000)
            .flat_map(|index| [0x90, (index & 0x7f) as u8, 0x40])
            .collect();
        let mut sequence = sequence_with_events(
            (0..2_000).map(|index| (0, vec![0x90, (index & 0x7f) as u8, 0x40])),
        );
        sequence.play(Some(0), None, None);

        let mut actual = Vec::new();
        let mut chunks = 0;
        loop {
            let mut budget = 4_096;
            let advance = sequence.advance_into(0, |bytes| {
                let accepted = bytes.len().min(budget);
                actual.extend_from_slice(&bytes[..accepted]);
                budget -= accepted;
                accepted
            });
            chunks += 1;
            if !advance.held {
                break;
            }
        }
        assert!(chunks > 1);
        assert_eq!(actual, expected);
    }
}
