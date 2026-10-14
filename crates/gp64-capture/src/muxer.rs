//! MKV/EBML muxer for VP9 video + Opus audio capture.
//!
//! Write-only muxer that produces Matroska files with exactly two tracks.
//! No external dependencies — pure Rust implementation of the EBML/MKV subset
//! needed for video capture.

use std::io::{self, Seek, SeekFrom, Write};

// ---------------------------------------------------------------------------
// EBML / Matroska element IDs
// ---------------------------------------------------------------------------

// EBML Header
const EBML: u32 = 0x1A45DFA3;
const EBML_VERSION: u32 = 0x4286;
const EBML_READ_VERSION: u32 = 0x42F7;
const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
const DOC_TYPE: u32 = 0x4282;
const DOC_TYPE_VERSION: u32 = 0x4287;
const DOC_TYPE_READ_VERSION: u32 = 0x4285;

// Segment
const SEGMENT: u32 = 0x18538067;
const SEGMENT_INFO: u32 = 0x1549A966;
const TIMESTAMP_SCALE: u32 = 0x2AD7B1;
const MUXING_APP: u32 = 0x4D80;
const WRITING_APP: u32 = 0x5741;
const DURATION: u32 = 0x4489;

// Tracks
const TRACKS: u32 = 0x1654AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_UID: u32 = 0x73C5;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63A2;

// Video
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const PIXEL_CROP_BOTTOM: u32 = 0x54AA;
const PIXEL_CROP_RIGHT: u32 = 0x54DD;
const DISPLAY_WIDTH: u32 = 0x54B0;
const DISPLAY_HEIGHT: u32 = 0x54BA;

// Audio
const AUDIO: u32 = 0xE1;
const SAMPLING_FREQUENCY: u32 = 0xB5;
const CHANNELS: u32 = 0x9F;
const CODEC_DELAY: u32 = 0x56AA;
const SEEK_PRE_ROLL: u32 = 0x56BB;

/// Opus encoder delay, in nanoseconds.
///
/// Matroska carries the codec's own priming delay in `CodecDelay` so the
/// demuxer can shift block timestamps back by it; the `OpusHead` pre-skip
/// alone only tells the *decoder* how many samples to discard, and leaves the
/// remaining audio sitting one delay too late on the timeline. This must equal
/// the pre-skip the encoder declares — [`OPUS_PRE_SKIP_SAMPLES`] at 48 kHz.
const OPUS_CODEC_DELAY_NS: u64 = OPUS_PRE_SKIP_SAMPLES as u64 * 1_000_000_000 / 48_000;

/// `SeekPreRoll` for Opus: the 80 ms of audio a decoder must consume after a
/// seek before its output is correct. Fixed by RFC 7845 / the Matroska Opus
/// mapping — not a tunable.
const OPUS_SEEK_PRE_ROLL_NS: u64 = 80_000_000;

/// Opus priming samples at 48 kHz. libopus (and `ropus`) report 6.5 ms of
/// algorithmic delay for the Audio application, which is 312 samples.
pub const OPUS_PRE_SKIP_SAMPLES: u16 = 312;

// Cluster
const CLUSTER: u32 = 0x1F43B675;
const TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
/// A block that carries more than a SimpleBlock's flags can express.
const BLOCK_GROUP: u32 = 0xA0;
/// The payload inside a [`BLOCK_GROUP`]; same header layout as a SimpleBlock.
const BLOCK: u32 = 0xA1;
/// Nanoseconds of the decoded block that are padding, not content.
const DISCARD_PADDING: u32 = 0x75A2;

// SeekHead (index at segment start pointing to Info/Tracks/Cues)
const SEEK_HEAD: u32 = 0x114D9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const VOID: u32 = 0xEC;

// Cues (keyframe index for seek)
const CUES: u32 = 0x1C53BB6B;
const CUE_POINT: u32 = 0xBB;
const CUE_TIME: u32 = 0xB3;
const CUE_TRACK_POSITIONS: u32 = 0xB7;
const CUE_TRACK: u32 = 0xF7;
const CUE_CLUSTER_POSITION: u32 = 0xF1;
const CUE_RELATIVE_POSITION: u32 = 0xF0;

/// Maximum duration of a cluster in milliseconds before forcing a new one.
const CLUSTER_MAX_DURATION_MS: u64 = 1000;

/// Size (in bytes) reserved at segment-body start for the SeekHead element.
/// Rewritten at finalise time with pointers to Info / Tracks / Cues; any
/// unused bytes are padded with a Void element. 256 bytes is comfortably
/// larger than we need for three fixed-width (8-byte position) Seek entries
/// and leaves room for future additions.
const SEEKHEAD_RESERVED_BYTES: usize = 256;

// ---------------------------------------------------------------------------
// EBML primitives
// ---------------------------------------------------------------------------

/// Return the byte width needed to encode an element ID.
fn element_id_width(id: u32) -> usize {
    if id <= 0xFF {
        1
    } else if id <= 0xFFFF {
        2
    } else if id <= 0xFF_FFFF {
        3
    } else {
        4
    }
}

/// Write an EBML element ID (1-4 bytes, big-endian).
fn write_element_id<W: Write>(w: &mut W, id: u32) -> io::Result<()> {
    let width = element_id_width(id);
    let bytes = id.to_be_bytes();
    w.write_all(&bytes[4 - width..])
}

/// Return the number of bytes needed to VINT-encode `size`.
fn vint_width(size: u64) -> usize {
    if size < 0x7F {
        1
    } else if size < 0x3FFF {
        2
    } else if size < 0x1F_FFFF {
        3
    } else if size < 0x0FFF_FFFF {
        4
    } else if size < 0x07_FFFF_FFFF {
        5
    } else if size < 0x03FF_FFFF_FFFF {
        6
    } else if size < 0x01_FFFF_FFFF_FFFF {
        7
    } else {
        8
    }
}

/// Write a VINT-encoded size value.
fn write_vint<W: Write>(w: &mut W, size: u64) -> io::Result<()> {
    let width = vint_width(size);
    write_vint_fixed(w, size, width)
}

/// Write a VINT-encoded size using a fixed byte width.
fn write_vint_fixed<W: Write>(w: &mut W, size: u64, width: usize) -> io::Result<()> {
    let marker = 1u64 << (8 * width - width);
    let value = marker | size;
    let bytes = value.to_be_bytes();
    w.write_all(&bytes[8 - width..])
}

/// Write the "unknown size" sentinel (8-byte VINT with all data bits set).
fn write_unknown_size<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])
}

/// Write a VINT-encoded track number (for SimpleBlock header).
fn write_track_vint<W: Write>(w: &mut W, track: u8) -> io::Result<()> {
    // Track numbers 1-127 fit in a single-byte VINT
    debug_assert!(track > 0 && track < 128);
    w.write_all(&[0x80 | track])
}

/// Write an unsigned integer element.
fn write_uint_element<W: Write>(w: &mut W, id: u32, value: u64) -> io::Result<()> {
    write_element_id(w, id)?;
    let width = uint_width(value);
    write_vint(w, width as u64)?;
    let bytes = value.to_be_bytes();
    w.write_all(&bytes[8 - width..])
}

/// Write a signed-integer element, in the fewest bytes that hold it.
///
/// EBML stores signed integers big-endian two's complement, so the shortest
/// form is the one whose leading byte still carries the sign.
fn write_signed_element<W: Write>(w: &mut W, id: u32, value: i64) -> io::Result<()> {
    write_element_id(w, id)?;
    let width = signed_width(value);
    write_vint(w, width as u64)?;
    let bytes = value.to_be_bytes();
    w.write_all(&bytes[8 - width..])
}

/// Return the minimum number of bytes that represent a signed value.
fn signed_width(value: i64) -> usize {
    let mut width = 1;
    while width < 8 {
        let bits = width * 8 - 1;
        let limit = 1i64 << bits;
        if value >= -limit && value < limit {
            break;
        }
        width += 1;
    }
    width
}

/// Return the minimum number of bytes to represent an unsigned integer value.
fn uint_width(value: u64) -> usize {
    if value == 0 {
        1
    } else {
        let bits = 64 - value.leading_zeros() as usize;
        bits.div_ceil(8)
    }
}

/// Write a UTF-8 string element.
fn write_string_element<W: Write>(w: &mut W, id: u32, s: &str) -> io::Result<()> {
    write_element_id(w, id)?;
    write_vint(w, s.len() as u64)?;
    w.write_all(s.as_bytes())
}

/// Write a binary data element.
fn write_binary_element<W: Write>(w: &mut W, id: u32, data: &[u8]) -> io::Result<()> {
    write_element_id(w, id)?;
    write_vint(w, data.len() as u64)?;
    w.write_all(data)
}

/// Write an 8-byte IEEE 754 float element.
fn write_float_element<W: Write>(w: &mut W, id: u32, value: f64) -> io::Result<()> {
    write_element_id(w, id)?;
    write_vint(w, 8)?;
    w.write_all(&value.to_be_bytes())
}

/// Write an unsigned integer element using a fixed-width big-endian payload.
///
/// Callers use this to keep element sizes constant across patches (e.g. to
/// reserve fixed-size SeekPosition slots that can be computed up-front).
fn write_uint_element_fixed<W: Write>(
    w: &mut W,
    id: u32,
    value: u64,
    width: usize,
) -> io::Result<()> {
    debug_assert!((1..=8).contains(&width));
    write_element_id(w, id)?;
    write_vint(w, width as u64)?;
    let bytes = value.to_be_bytes();
    w.write_all(&bytes[8 - width..])
}

/// Serialize a SeekHead master element pointing at the given
/// `(target_element_id, offset_from_segment_body)` entries.
///
/// SeekPosition is written with a fixed 8-byte payload so the total element
/// size is deterministic (21 bytes per Seek + 5-byte wrapper = 68 bytes for
/// three entries). That determinism lets callers pre-reserve space for the
/// SeekHead and pad the remainder with Void.
fn build_seekhead<W: Write>(w: &mut W, entries: &[(u32, u64)]) -> io::Result<()> {
    let mut children = Vec::new();
    for &(target_id, offset) in entries {
        let mut seek_body = Vec::new();
        // SeekID: 4-byte big-endian element ID being pointed at.
        let target_id_bytes = target_id.to_be_bytes();
        write_binary_element(&mut seek_body, SEEK_ID, &target_id_bytes)?;
        // SeekPosition: fixed 8-byte width so SeekHead size is predictable.
        write_uint_element_fixed(&mut seek_body, SEEK_POSITION, offset, 8)?;
        write_master_element(&mut children, SEEK, &seek_body)?;
    }
    write_master_element(w, SEEK_HEAD, &children)
}

/// Write a Void element whose total serialized size (ID + size VINT + content)
/// is exactly `total_bytes`.
///
/// Panics if `total_bytes == 1` or `total_bytes == 129` — those are unreachable
/// with a single Void under the VINT-width boundary rule. Callers must pick
/// reservation sizes that avoid those values.
fn write_fixed_void<W: Write>(w: &mut W, total_bytes: usize) -> io::Result<()> {
    assert!(
        total_bytes != 1 && total_bytes != 129,
        "write_fixed_void: {total_bytes} bytes is unreachable in a single Void"
    );
    if total_bytes == 0 {
        return Ok(());
    }
    // Pick content size so that 1 (ID) + vint_width(content) + content == total.
    let content_size = if total_bytes <= 128 {
        total_bytes - 2 // vint_width(content) = 1 for content <= 126
    } else {
        total_bytes - 3 // vint_width(content) = 2 for content >= 127
    };
    write_element_id(w, VOID)?;
    write_vint(w, content_size as u64)?;
    // Write zeros in chunks to avoid N single-byte writes.
    let zeros = [0u8; 64];
    let mut remaining = content_size;
    while remaining > 0 {
        let chunk = remaining.min(zeros.len());
        w.write_all(&zeros[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

/// Serialize master element children into a temporary buffer, then write
/// the master element with a known size. Returns the byte count written.
fn write_master_element<W: Write>(w: &mut W, id: u32, children: &[u8]) -> io::Result<()> {
    write_element_id(w, id)?;
    write_vint(w, children.len() as u64)?;
    w.write_all(children)
}

// ---------------------------------------------------------------------------
// OpusHead
// ---------------------------------------------------------------------------

/// Build an OpusHead structure for the MKV CodecPrivate element.
///
/// Format (19 bytes):
/// ```text
/// [8 bytes] "OpusHead"
/// u8  version = 1
/// u8  channels
/// u16 pre_skip (little-endian)
/// u32 original_sample_rate (little-endian)
/// u16 output_gain = 0 (little-endian)
/// u8  channel_mapping_family = 0
/// ```
pub fn build_opus_head(channels: u8, pre_skip: u16, sample_rate: u32) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(channels);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&sample_rate.to_le_bytes());
    head.extend_from_slice(&0u16.to_le_bytes()); // output_gain
    head.push(0); // channel_mapping_family
    head
}

// ---------------------------------------------------------------------------
// MKV Muxer
// ---------------------------------------------------------------------------

/// One Cues index entry — a seek target for the start of a cluster.
#[derive(Clone, Copy)]
struct CueEntry {
    /// Keyframe presentation timestamp in milliseconds.
    timestamp_ms: u64,
    /// Offset of the Cluster element (ID byte) relative to Segment body start.
    cluster_position: u64,
    /// Offset of the Block relative to the Cluster's data start (i.e. from
    /// the first byte after the Cluster's size VINT).
    relative_position: u64,
}

/// Coded and displayed geometry for the video track.
///
/// VP9 codes whole 64-pixel superblock columns, so the encoder rounds the
/// coded width up to a multiple of 64 and the coded height up to an even
/// number. `display_*` is the un-padded picture; the muxer turns the
/// difference into Matroska crop elements so players show only the real
/// frame.
#[derive(Clone, Copy, Debug)]
pub struct VideoGeometry {
    /// Coded luma width handed to the encoder.
    pub coded_width: u32,
    /// Coded luma height handed to the encoder.
    pub coded_height: u32,
    /// Width the player should display (`<= coded_width`).
    pub display_width: u32,
    /// Height the player should display (`<= coded_height`).
    pub display_height: u32,
}

/// Write-only MKV muxer for exactly two tracks: VP9 video + Opus audio.
pub struct MkvMuxer<W: Write + Seek> {
    writer: W,
    /// File offset of the Segment element's size field (for patching on finalise).
    segment_size_pos: u64,
    /// File offset of the Segment element's body start (right after the size field).
    segment_body_pos: u64,
    /// File offset of the Duration element's 8-byte float value (for patching).
    duration_pos: u64,
    /// File offset of the reserved SeekHead placeholder (right after Segment size).
    seekhead_pos: u64,
    /// File offset of the SegmentInfo element (ID byte).
    info_pos: u64,
    /// File offset of the Tracks element (ID byte).
    tracks_pos: u64,
    /// Track number for video (always 1).
    video_track: u8,
    /// Track number for audio (always 2).
    audio_track: u8,
    /// File position just after the current Cluster element ID byte (i.e.
    /// where the size VINT starts). `None` if no cluster is currently open.
    cluster_start_pos: Option<u64>,
    /// File offset of the current Cluster element's ID byte (what Cues points at).
    current_cluster_element_pos: Option<u64>,
    /// Timestamp of the current cluster in milliseconds.
    cluster_timestamp_ms: u64,
    /// Maximum presented endpoint, in the same zero-origin coordinates as blocks and cues.
    presented_end_ns: u128,
    has_video: bool,
    /// Whether we need to start a new cluster before the next block.
    needs_new_cluster: bool,
    /// Recorded keyframe positions for emitting Cues on finalise.
    cue_entries: Vec<CueEntry>,
}

impl<W: Write + Seek> MkvMuxer<W> {
    /// Create a new MKV file with video + audio tracks.
    ///
    /// - `geometry`: coded vs displayed video dimensions
    /// - `audio_codec_private`: `OpusHead` bytes
    /// - `sample_rate`: audio sample rate in Hz (e.g. 48000.0)
    /// - `channels`: number of audio channels
    ///
    /// The video track carries no CodecPrivate: VP9-in-Matroska needs none,
    /// every stream parameter travels in the frames' own uncompressed headers.
    pub fn new(
        mut writer: W,
        geometry: VideoGeometry,
        audio_codec_private: &[u8],
        sample_rate: f64,
        channels: u8,
    ) -> io::Result<Self> {
        // --- EBML Header ---
        let mut ebml_body = Vec::new();
        write_uint_element(&mut ebml_body, EBML_VERSION, 1)?;
        write_uint_element(&mut ebml_body, EBML_READ_VERSION, 1)?;
        write_uint_element(&mut ebml_body, EBML_MAX_ID_LENGTH, 4)?;
        write_uint_element(&mut ebml_body, EBML_MAX_SIZE_LENGTH, 8)?;
        write_string_element(&mut ebml_body, DOC_TYPE, "matroska")?;
        write_uint_element(&mut ebml_body, DOC_TYPE_VERSION, 4)?;
        write_uint_element(&mut ebml_body, DOC_TYPE_READ_VERSION, 2)?;
        write_master_element(&mut writer, EBML, &ebml_body)?;

        // --- Segment (unknown size — patched on finalise) ---
        write_element_id(&mut writer, SEGMENT)?;
        let segment_size_pos = writer.stream_position()?;
        write_unknown_size(&mut writer)?;
        let segment_body_pos = writer.stream_position()?;

        // --- SeekHead placeholder (reserved at segment body start) ---
        // Fill the reservation with a single Void element. On finalise we
        // overwrite it with a real SeekHead pointing to Info / Tracks / Cues
        // and pad the remainder with another Void.
        let seekhead_pos = writer.stream_position()?;
        write_fixed_void(&mut writer, SEEKHEAD_RESERVED_BYTES)?;

        // --- SegmentInfo ---
        let mut info_body = Vec::new();
        // TimestampScale: 1,000,000 ns = 1 ms resolution
        write_uint_element(&mut info_body, TIMESTAMP_SCALE, 1_000_000)?;
        write_string_element(&mut info_body, MUXING_APP, "gp64-capture")?;
        write_string_element(&mut info_body, WRITING_APP, "gp64-capture")?;
        // Duration placeholder (8-byte float, value 0.0 — patched on finalise)
        write_element_id(&mut info_body, DURATION)?;
        write_vint(&mut info_body, 8)?;
        let duration_offset_in_info = info_body.len() as u64;
        info_body.extend_from_slice(&0.0f64.to_be_bytes());

        // Write SegmentInfo master — we need to know where Duration lands in the file
        let info_pos = writer.stream_position()?;
        write_element_id(&mut writer, SEGMENT_INFO)?;
        write_vint(&mut writer, info_body.len() as u64)?;
        let info_start_pos = writer.stream_position()?;
        writer.write_all(&info_body)?;
        let duration_pos = info_start_pos + duration_offset_in_info;

        // --- Tracks ---
        let mut tracks_body = Vec::new();

        // Video track (track 1)
        let mut video_entry = Vec::new();
        write_uint_element(&mut video_entry, TRACK_NUMBER, 1)?;
        write_uint_element(&mut video_entry, TRACK_UID, 1)?;
        write_uint_element(&mut video_entry, TRACK_TYPE, 1)?; // 1 = video
        write_string_element(&mut video_entry, CODEC_ID, "V_VP9")?;

        let mut video_settings = Vec::new();
        write_uint_element(
            &mut video_settings,
            PIXEL_WIDTH,
            u64::from(geometry.coded_width),
        )?;
        write_uint_element(
            &mut video_settings,
            PIXEL_HEIGHT,
            u64::from(geometry.coded_height),
        )?;
        // Crop the superblock padding away. Emitted only when there is padding
        // to hide, so an exactly-sized capture produces the same Video element
        // it always did. DisplayWidth/Height are written alongside because the
        // Matroska default for them is the UNCROPPED PixelWidth/Height, which
        // would otherwise letterbox the cropped picture back out to the padded
        // size in players that honour the default.
        let crop_right = geometry.coded_width.saturating_sub(geometry.display_width);
        let crop_bottom = geometry
            .coded_height
            .saturating_sub(geometry.display_height);
        if crop_right > 0 || crop_bottom > 0 {
            if crop_right > 0 {
                write_uint_element(&mut video_settings, PIXEL_CROP_RIGHT, u64::from(crop_right))?;
            }
            if crop_bottom > 0 {
                write_uint_element(
                    &mut video_settings,
                    PIXEL_CROP_BOTTOM,
                    u64::from(crop_bottom),
                )?;
            }
            write_uint_element(
                &mut video_settings,
                DISPLAY_WIDTH,
                u64::from(geometry.display_width),
            )?;
            write_uint_element(
                &mut video_settings,
                DISPLAY_HEIGHT,
                u64::from(geometry.display_height),
            )?;
        }
        write_master_element(&mut video_entry, VIDEO, &video_settings)?;

        write_master_element(&mut tracks_body, TRACK_ENTRY, &video_entry)?;

        // Audio track (track 2)
        let mut audio_entry = Vec::new();
        write_uint_element(&mut audio_entry, TRACK_NUMBER, 2)?;
        write_uint_element(&mut audio_entry, TRACK_UID, 2)?;
        write_uint_element(&mut audio_entry, TRACK_TYPE, 2)?; // 2 = audio
        write_string_element(&mut audio_entry, CODEC_ID, "A_OPUS")?;
        write_binary_element(&mut audio_entry, CODEC_PRIVATE, audio_codec_private)?;
        // Both are required of an A_OPUS track by the Matroska Opus mapping.
        // Without CodecDelay the demuxer has no way to undo the encoder's
        // priming delay, so every audio block sits ~6.5 ms later than the
        // content it carries.
        write_uint_element(&mut audio_entry, CODEC_DELAY, OPUS_CODEC_DELAY_NS)?;
        write_uint_element(&mut audio_entry, SEEK_PRE_ROLL, OPUS_SEEK_PRE_ROLL_NS)?;

        let mut audio_settings = Vec::new();
        write_float_element(&mut audio_settings, SAMPLING_FREQUENCY, sample_rate)?;
        write_uint_element(&mut audio_settings, CHANNELS, u64::from(channels))?;
        write_master_element(&mut audio_entry, AUDIO, &audio_settings)?;

        write_master_element(&mut tracks_body, TRACK_ENTRY, &audio_entry)?;

        let tracks_pos = writer.stream_position()?;
        write_master_element(&mut writer, TRACKS, &tracks_body)?;

        Ok(Self {
            writer,
            segment_size_pos,
            segment_body_pos,
            duration_pos,
            seekhead_pos,
            info_pos,
            tracks_pos,
            video_track: 1,
            audio_track: 2,
            cluster_start_pos: None,
            current_cluster_element_pos: None,
            cluster_timestamp_ms: 0,
            presented_end_ns: 0,
            has_video: false,
            needs_new_cluster: true,
            cue_entries: Vec::new(),
        })
    }

    /// Write a video frame.
    ///
    /// `data` is one VP9 frame exactly as the encoder emitted it — Matroska
    /// stores VP9 frames raw, with no length prefix or container of its own.
    /// `timestamp_ms` is the presentation timestamp in milliseconds.
    /// `keyframe` indicates whether this is a VP9 keyframe.
    pub fn write_video_frame(
        &mut self,
        data: &[u8],
        timestamp_ms: u64,
        keyframe: bool,
    ) -> io::Result<()> {
        // Start a new cluster on keyframes or when the current cluster is too long
        if keyframe || self.needs_new_cluster || self.cluster_exceeds_duration(timestamp_ms) {
            self.start_cluster(timestamp_ms)?;
        }
        self.write_simple_block(self.video_track, data, timestamp_ms, keyframe)?;
        self.has_video = true;
        self.presented_end_ns = self
            .presented_end_ns
            .max(u128::from(timestamp_ms) * 1_000_000);
        Ok(())
    }

    /// Write an audio packet.
    ///
    /// `data` is a single Opus packet. `timestamp_ms` is the presentation
    /// timestamp in milliseconds.
    pub fn write_audio_packet(&mut self, data: &[u8], timestamp_ms: u64) -> io::Result<()> {
        if self.needs_new_cluster {
            self.start_cluster(timestamp_ms)?;
        }
        // If audio timestamp would push us past the cluster duration limit,
        // defer the new cluster to the next keyframe rather than splitting mid-GOP.
        // Every Opus packet is a random access point, and RFC 9559 section
        // 10.4 says a RAP SimpleBlock sets its keyframe flag. Clearing it
        // advertised every audio block as depending on something earlier, which
        // is not true of this codec and costs a seeking demuxer accuracy
        // (MDD-BUG-CRU-00815). The decoder warm-up that Opus does need is
        // already declared on the track, as CodecDelay and SeekPreRoll, so the
        // flag is the only part that was missing.
        self.write_simple_block(self.audio_track, data, timestamp_ms, true)?;
        self.record_audio_endpoint(data, timestamp_ms, 0);
        Ok(())
    }

    /// Finalise the MKV file: close the last cluster, write Cues, then patch
    /// SeekHead / Segment size / Duration.
    ///
    /// Returns the underlying writer so it can be flushed/closed.
    pub fn finalise(mut self) -> io::Result<W> {
        // Blocks and cues retain their segment timestamps, including leading gaps.
        let duration_ms = self.presented_end_ns as f64 / 1_000_000.0;

        self.finish_current_cluster()?;

        // Append Cues at the end (if we have any keyframes).
        self.writer.seek(SeekFrom::End(0))?;
        let cues_pos = self.writer.stream_position()?;
        let cues_written = self.write_cues()?;

        // Patch Duration in SegmentInfo
        self.writer.seek(SeekFrom::Start(self.duration_pos))?;
        self.writer.write_all(&duration_ms.to_be_bytes())?;

        // Patch the reserved SeekHead with pointers to Info / Tracks / Cues.
        // Writing a fresh SeekHead + a Void filler keeps the reservation's
        // total size constant so later offsets don't shift.
        let info_offset = self.info_pos - self.segment_body_pos;
        let tracks_offset = self.tracks_pos - self.segment_body_pos;
        let cues_offset = cues_pos - self.segment_body_pos;

        // `cues_written == false` still emits a Cues pointer — the Cues element
        // was written (as an empty master), the SeekHead entry keeps the
        // layout uniform.
        let _ = cues_written;
        let mut seekhead_buf = Vec::with_capacity(SEEKHEAD_RESERVED_BYTES);
        build_seekhead(
            &mut seekhead_buf,
            &[
                (SEGMENT_INFO, info_offset),
                (TRACKS, tracks_offset),
                (CUES, cues_offset),
            ],
        )?;
        assert!(
            seekhead_buf.len() <= SEEKHEAD_RESERVED_BYTES,
            "SeekHead {} bytes exceeds reserved {}",
            seekhead_buf.len(),
            SEEKHEAD_RESERVED_BYTES,
        );
        self.writer.seek(SeekFrom::Start(self.seekhead_pos))?;
        self.writer.write_all(&seekhead_buf)?;
        let void_bytes = SEEKHEAD_RESERVED_BYTES - seekhead_buf.len();
        if void_bytes > 0 {
            write_fixed_void(&mut self.writer, void_bytes)?;
        }

        // Calculate and patch Segment size (now that Cues has been appended)
        let end_pos = self.writer.seek(SeekFrom::End(0))?;
        let segment_size = end_pos - self.segment_body_pos;

        self.writer.seek(SeekFrom::Start(self.segment_size_pos))?;
        // Write the size as an 8-byte VINT (same width as the unknown-size sentinel)
        write_vint_fixed(&mut self.writer, segment_size, 8)?;

        // Seek back to end
        self.writer.seek(SeekFrom::End(0))?;

        Ok(self.writer)
    }

    /// Write the Cues master (keyframe index) at the current file position.
    /// Returns whether any cues were emitted (false → caller may decide not
    /// to add the Cues SeekHead pointer, but we still emit the pointer so the
    /// file structure is uniform).
    fn write_cues(&mut self) -> io::Result<bool> {
        if self.cue_entries.is_empty() {
            // Emit an empty Cues master so readers that trust the SeekHead
            // pointer don't chase a nonexistent element.
            write_master_element(&mut self.writer, CUES, &[])?;
            return Ok(false);
        }

        let mut body = Vec::new();
        for entry in &self.cue_entries {
            let mut positions = Vec::new();
            write_uint_element(&mut positions, CUE_TRACK, u64::from(self.video_track))?;
            write_uint_element(&mut positions, CUE_CLUSTER_POSITION, entry.cluster_position)?;
            write_uint_element(
                &mut positions,
                CUE_RELATIVE_POSITION,
                entry.relative_position,
            )?;

            let mut point = Vec::new();
            write_uint_element(&mut point, CUE_TIME, entry.timestamp_ms)?;
            write_master_element(&mut point, CUE_TRACK_POSITIONS, &positions)?;

            write_master_element(&mut body, CUE_POINT, &point)?;
        }
        write_master_element(&mut self.writer, CUES, &body)?;
        Ok(true)
    }

    /// Start a new cluster at the given timestamp.
    fn start_cluster(&mut self, timestamp_ms: u64) -> io::Result<()> {
        self.finish_current_cluster()?;

        self.cluster_timestamp_ms = timestamp_ms;
        self.needs_new_cluster = false;

        // Record the position of the Cluster element ID byte (what Cues
        // references) before writing it.
        let cluster_element_pos = self.writer.stream_position()?;

        // Reserve an 8-byte Cluster size field; finish_current_cluster patches
        // it once the next Cluster or Cues boundary is known.
        write_element_id(&mut self.writer, CLUSTER)?;
        let cluster_pos = self.writer.stream_position()?;
        write_unknown_size(&mut self.writer)?;
        self.cluster_start_pos = Some(cluster_pos);
        self.current_cluster_element_pos = Some(cluster_element_pos);

        // Cluster Timestamp
        write_uint_element(&mut self.writer, TIMESTAMP, timestamp_ms)?;

        Ok(())
    }

    /// Patch the current Cluster's reserved 8-byte size field and leave the
    /// writer positioned where it was.
    fn finish_current_cluster(&mut self) -> io::Result<()> {
        let Some(cluster_size_pos) = self.cluster_start_pos.take() else {
            return Ok(());
        };

        let end_pos = self.writer.stream_position()?;
        let data_start = cluster_size_pos + 8;
        let cluster_size = end_pos.checked_sub(data_start).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "cluster end precedes cluster data start",
            )
        })?;

        self.writer.seek(SeekFrom::Start(cluster_size_pos))?;
        write_vint_fixed(&mut self.writer, cluster_size, 8)?;
        self.writer.seek(SeekFrom::Start(end_pos))?;
        self.current_cluster_element_pos = None;

        Ok(())
    }

    /// Check if adding a block at `timestamp_ms` would exceed the cluster duration limit.
    fn cluster_exceeds_duration(&self, timestamp_ms: u64) -> bool {
        if self.cluster_start_pos.is_none() {
            return false;
        }
        timestamp_ms.saturating_sub(self.cluster_timestamp_ms) >= CLUSTER_MAX_DURATION_MS
    }

    /// The offset a block at `timestamp_ms` gets, rotating the cluster if the
    /// current one cannot reach it.
    ///
    /// A block carries its timestamp as a signed 16-bit offset from its
    /// cluster's, so one cluster cannot hold a block more than about 32.7
    /// seconds away from where it starts. That is the format's limit, not a
    /// preference, and clamping to it produced a file that still parsed while
    /// every later packet claimed the same instant (MDD-BUG-CRU-00812). Video
    /// rotates on keyframes long before this matters; audio has no keyframes,
    /// so an audio-only stretch is what reaches it.
    ///
    /// Starting a cluster here puts the block at offset zero by construction,
    /// so there is no second way for the conversion to fail.
    fn cluster_relative_timestamp(&mut self, timestamp_ms: u64) -> io::Result<i16> {
        match i16::try_from(timestamp_ms as i64 - self.cluster_timestamp_ms as i64) {
            Ok(relative) => Ok(relative),
            Err(_) => {
                self.start_cluster(timestamp_ms)?;
                Ok(0)
            }
        }
    }

    /// Write the last audio packet, saying how much of it is padding.
    ///
    /// Opus encodes whole 20 ms frames, so a recording that stops mid-frame has
    /// to pad its tail with silence to encode it at all. `discard_padding_ns`
    /// tells the reader how much of the decoded result is that padding, so the
    /// tail plays at its true length instead of being rounded up
    /// (MDD-BUG-CRU-00817).
    ///
    /// This is a BlockGroup rather than a SimpleBlock because DiscardPadding is
    /// only valid inside one - the compact form has nowhere to put it.
    pub fn write_audio_packet_with_discard_padding(
        &mut self,
        data: &[u8],
        timestamp_ms: u64,
        discard_padding_ns: u64,
    ) -> io::Result<()> {
        if self.needs_new_cluster {
            self.start_cluster(timestamp_ms)?;
        }
        let relative = self.cluster_relative_timestamp(timestamp_ms)?;

        let mut block = Vec::with_capacity(4 + data.len());
        write_track_vint(&mut block, self.audio_track)?;
        block.extend_from_slice(&relative.to_be_bytes());
        // A Block has no keyframe bit: inside a BlockGroup, dependency is
        // expressed by ReferenceBlock, and its absence means none.
        block.push(0x00);
        block.extend_from_slice(data);

        let mut group = Vec::new();
        write_element_id(&mut group, BLOCK)?;
        write_vint(&mut group, block.len() as u64)?;
        group.extend_from_slice(&block);
        write_signed_element(
            &mut group,
            DISCARD_PADDING,
            i64::try_from(discard_padding_ns).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "discard padding does not fit a signed element",
                )
            })?,
        )?;

        write_master_element(&mut self.writer, BLOCK_GROUP, &group)?;
        self.record_audio_endpoint(data, timestamp_ms, discard_padding_ns);
        Ok(())
    }

    /// Write a SimpleBlock element.
    fn write_simple_block(
        &mut self,
        track: u8,
        data: &[u8],
        timestamp_ms: u64,
        keyframe: bool,
    ) -> io::Result<()> {
        // A SimpleBlock carries its timestamp as a signed 16-bit offset from
        // its cluster's, so one cluster cannot hold a block more than about
        // 32.7 seconds away from where it starts. That is the format's limit,
        // not a preference, and clamping to it produced a file that still
        // parsed while every later packet claimed the same instant
        // (MDD-BUG-CRU-00812). Video rotates on keyframes long before this
        // matters; audio has no keyframes, so an audio-only stretch is what
        // reaches it.
        let relative = self.cluster_relative_timestamp(timestamp_ms)?;

        // SimpleBlock header: track VINT (1 byte) + relative timestamp (2 bytes) + flags (1 byte)
        let header_size = 1 + 2 + 1;
        let block_size = header_size + data.len();

        let block_element_pos = self.writer.stream_position()?;
        write_element_id(&mut self.writer, SIMPLE_BLOCK)?;
        write_vint(&mut self.writer, block_size as u64)?;

        // Track number as VINT
        write_track_vint(&mut self.writer, track)?;

        // Relative timestamp (big-endian i16)
        self.writer.write_all(&relative.to_be_bytes())?;

        // Flags: bit 7 (0x80) = keyframe, bits 6-4 reserved, bit 3 (0x08) = invisible,
        //        bits 2-1 = lacing, bit 0 (0x01) = discardable
        let flags: u8 = if keyframe { 0x80 } else { 0x00 };
        self.writer.write_all(&[flags])?;

        // Frame data
        self.writer.write_all(data)?;

        // Record a Cues entry for video keyframes so seeking works. The
        // relative position is measured from the start of the cluster's
        // *data* (the byte immediately after the cluster's size VINT); we
        // reserve an 8-byte Cluster size field, so that's
        // `cluster_start_pos + 8`.
        if keyframe && track == self.video_track {
            if let (Some(cluster_size_pos), Some(cluster_element_pos)) =
                (self.cluster_start_pos, self.current_cluster_element_pos)
            {
                let cluster_data_start = cluster_size_pos + 8;
                self.cue_entries.push(CueEntry {
                    timestamp_ms,
                    cluster_position: cluster_element_pos - self.segment_body_pos,
                    relative_position: block_element_pos - cluster_data_start,
                });
            }
        }

        Ok(())
    }

    /// Extend the last displayed frame through a known capture/rollover endpoint.
    pub(crate) fn record_presentation_end_ns(&mut self, timestamp_ns: u64) {
        if self.has_video {
            self.presented_end_ns = self.presented_end_ns.max(u128::from(timestamp_ns));
        }
    }

    fn record_audio_endpoint(&mut self, data: &[u8], timestamp_ms: u64, padding_ns: u64) {
        let start_ns = u128::from(timestamp_ms) * 1_000_000;
        // Preserve acceptance of opaque bytes: when packet duration cannot be
        // determined from its header, its block timestamp is the known bound.
        let end_ns = match ropus::opus::decoder::opus_packet_get_nb_samples(data, 48_000) {
            Ok(samples) => (start_ns + samples as u128 * 1_000_000_000 / 48_000)
                .saturating_sub(u128::from(OPUS_CODEC_DELAY_NS) + u128::from(padding_ns)),
            Err(_) => start_ns,
        };
        self.presented_end_ns = self.presented_end_ns.max(end_ns);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // -- VINT encoding tests ------------------------------------------------

    #[test]
    fn vint_width_boundaries() {
        assert_eq!(vint_width(0), 1);
        assert_eq!(vint_width(0x7E), 1); // max 1-byte
        assert_eq!(vint_width(0x7F), 2); // overflows to 2-byte
        assert_eq!(vint_width(0x3FFE), 2); // max 2-byte
        assert_eq!(vint_width(0x3FFF), 3);
        assert_eq!(vint_width(0x1F_FFFE), 3);
        assert_eq!(vint_width(0x1F_FFFF), 4);
        assert_eq!(vint_width(0x0FFF_FFFE), 4);
        assert_eq!(vint_width(0x0FFF_FFFF), 5);
    }

    #[test]
    fn vint_encoding_one_byte() {
        let mut buf = Vec::new();
        write_vint(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x80]); // 1000_0000

        buf.clear();
        write_vint(&mut buf, 1).unwrap();
        assert_eq!(buf, [0x81]); // 1000_0001

        buf.clear();
        write_vint(&mut buf, 126).unwrap();
        assert_eq!(buf, [0xFE]); // 1111_1110
    }

    #[test]
    fn vint_encoding_two_bytes() {
        let mut buf = Vec::new();
        write_vint(&mut buf, 127).unwrap();
        // 0100_0000 0111_1111
        assert_eq!(buf, [0x40, 0x7F]);

        buf.clear();
        write_vint(&mut buf, 0x3FFE).unwrap();
        // 0111_1111 1111_1110
        assert_eq!(buf, [0x7F, 0xFE]);
    }

    #[test]
    fn vint_encoding_three_bytes() {
        let mut buf = Vec::new();
        write_vint(&mut buf, 0x3FFF).unwrap();
        // 0010_0000 0011_1111 1111_1111
        assert_eq!(buf, [0x20, 0x3F, 0xFF]);
    }

    #[test]
    fn vint_fixed_width_eight_bytes() {
        let mut buf = Vec::new();
        write_vint_fixed(&mut buf, 42, 8).unwrap();
        assert_eq!(buf.len(), 8);
        // Marker bit for 8-byte VINT is bit 0 of the first byte
        assert_eq!(buf[0], 0x01);
        assert_eq!(buf[7], 42);
    }

    #[test]
    fn unknown_size_sentinel() {
        let mut buf = Vec::new();
        write_unknown_size(&mut buf).unwrap();
        assert_eq!(buf, [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    }

    // -- Element ID tests ---------------------------------------------------

    #[test]
    fn element_id_one_byte() {
        let mut buf = Vec::new();
        write_element_id(&mut buf, 0xAE).unwrap(); // TrackEntry
        assert_eq!(buf, [0xAE]);
    }

    #[test]
    fn element_id_two_bytes() {
        let mut buf = Vec::new();
        write_element_id(&mut buf, 0x4286).unwrap(); // EBMLVersion
        assert_eq!(buf, [0x42, 0x86]);
    }

    #[test]
    fn element_id_three_bytes() {
        let mut buf = Vec::new();
        write_element_id(&mut buf, 0x2AD7B1).unwrap(); // TimestampScale
        assert_eq!(buf, [0x2A, 0xD7, 0xB1]);
    }

    #[test]
    fn element_id_four_bytes() {
        let mut buf = Vec::new();
        write_element_id(&mut buf, 0x1A45DFA3).unwrap(); // EBML
        assert_eq!(buf, [0x1A, 0x45, 0xDF, 0xA3]);
    }

    // -- Element writing tests ----------------------------------------------

    #[test]
    fn uint_element_small() {
        let mut buf = Vec::new();
        write_uint_element(&mut buf, TRACK_NUMBER, 1).unwrap();
        // ID 0xD7 (1 byte) + size VINT 0x81 (1 byte, value=1) + data 0x01 (1 byte)
        assert_eq!(buf, [0xD7, 0x81, 0x01]);
    }

    #[test]
    fn uint_element_large() {
        let mut buf = Vec::new();
        write_uint_element(&mut buf, TIMESTAMP_SCALE, 1_000_000).unwrap();
        // ID 0x2AD7B1 + size 3 + 3 bytes of 1_000_000 (0x0F_4240)
        assert_eq!(buf, [0x2A, 0xD7, 0xB1, 0x83, 0x0F, 0x42, 0x40]);
    }

    #[test]
    fn string_element() {
        let mut buf = Vec::new();
        write_string_element(&mut buf, DOC_TYPE, "matroska").unwrap();
        // ID 0x4282 + size 8 + "matroska"
        let mut expected = vec![0x42, 0x82, 0x88];
        expected.extend_from_slice(b"matroska");
        assert_eq!(buf, expected);
    }

    #[test]
    fn float_element() {
        let mut buf = Vec::new();
        write_float_element(&mut buf, DURATION, 0.0).unwrap();
        // ID 0x4489 + size 8 + 8 bytes of 0.0
        let mut expected = vec![0x44, 0x89, 0x88];
        expected.extend_from_slice(&0.0f64.to_be_bytes());
        assert_eq!(buf, expected);
    }

    #[test]
    fn binary_element() {
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut buf = Vec::new();
        write_binary_element(&mut buf, CODEC_PRIVATE, &data).unwrap();
        // ID 0x63A2 + size 4 + data
        assert_eq!(buf, [0x63, 0xA2, 0x84, 0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn master_element() {
        let mut children = Vec::new();
        write_uint_element(&mut children, TRACK_NUMBER, 1).unwrap();

        let mut buf = Vec::new();
        write_master_element(&mut buf, TRACK_ENTRY, &children).unwrap();

        // TrackEntry ID (0xAE) + size VINT (3 bytes for children) + children
        assert_eq!(buf[0], 0xAE); // TrackEntry ID
                                  // size of children = 3 bytes (D7 81 01)
        assert_eq!(buf[1], 0x83); // VINT encoding of 3
        assert_eq!(&buf[2..], &[0xD7, 0x81, 0x01]);
    }

    // -- OpusHead tests -----------------------------------------------------

    #[test]
    fn opus_head_structure() {
        let head = build_opus_head(2, 312, 48000);

        assert_eq!(head.len(), 19);
        assert_eq!(&head[0..8], b"OpusHead");
        assert_eq!(head[8], 1); // version
        assert_eq!(head[9], 2); // channels
        assert_eq!(u16::from_le_bytes([head[10], head[11]]), 312); // pre_skip
        assert_eq!(
            u32::from_le_bytes([head[12], head[13], head[14], head[15]]),
            48000
        ); // sample_rate
        assert_eq!(u16::from_le_bytes([head[16], head[17]]), 0); // output_gain
        assert_eq!(head[18], 0); // channel_mapping_family
    }

    #[test]
    fn opus_head_mono() {
        let head = build_opus_head(1, 0, 44100);
        assert_eq!(head[9], 1);
        assert_eq!(
            u32::from_le_bytes([head[12], head[13], head[14], head[15]]),
            44100
        );
    }

    // -- MKV muxer integration tests ----------------------------------------

    /// Helper: create a muxer with default parameters.
    fn make_test_muxer() -> MkvMuxer<Cursor<Vec<u8>>> {
        make_test_muxer_with_geometry(VideoGeometry {
            coded_width: 320,
            coded_height: 200,
            display_width: 320,
            display_height: 200,
        })
    }

    fn make_test_muxer_with_geometry(geometry: VideoGeometry) -> MkvMuxer<Cursor<Vec<u8>>> {
        let opus_head = build_opus_head(2, 312, 48000);
        MkvMuxer::new(Cursor::new(Vec::new()), geometry, &opus_head, 48000.0, 2).unwrap()
    }

    /// Find a two-byte-ID EBML element and read its 1-byte-size unsigned value.
    fn find_u8_sized_uint(data: &[u8], id: [u8; 2]) -> Option<u64> {
        data.windows(2).position(|w| w == id).map(|pos| {
            let size = data[pos + 2] as usize & 0x7F;
            data[pos + 3..pos + 3 + size]
                .iter()
                .fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
        })
    }

    #[test]
    fn muxer_declares_the_vp9_codec_and_no_video_codec_private() {
        let data = make_test_muxer().finalise().unwrap().into_inner();
        assert!(
            data.windows(5).any(|w| w == b"V_VP9"),
            "video track must declare CodecID V_VP9"
        );
        // The Opus track keeps its CodecPrivate (OpusHead); the video track
        // must not have one, so exactly one CodecPrivate ID (0x63A2) appears.
        let codec_private_count = data.windows(2).filter(|w| *w == [0x63, 0xA2]).count();
        assert_eq!(
            codec_private_count, 1,
            "only the Opus track carries a CodecPrivate"
        );
    }

    #[test]
    fn muxer_crops_superblock_padding_away() {
        // 800x600 padded out to VP9's 64-pixel superblock column grid.
        let data = make_test_muxer_with_geometry(VideoGeometry {
            coded_width: 832,
            coded_height: 600,
            display_width: 800,
            display_height: 600,
        })
        .finalise()
        .unwrap()
        .into_inner();

        assert_eq!(
            find_u8_sized_uint(&data, [0x54, 0xDD]),
            Some(32),
            "PixelCropRight must hide the 32 pad columns"
        );
        assert_eq!(
            find_u8_sized_uint(&data, [0x54, 0xB0]),
            Some(800),
            "DisplayWidth must be the un-padded width"
        );
        assert!(
            find_u8_sized_uint(&data, [0x54, 0xAA]).is_none(),
            "no vertical padding, so no PixelCropBottom"
        );
    }

    #[test]
    fn muxer_omits_crop_elements_when_nothing_is_padded() {
        let data = make_test_muxer().finalise().unwrap().into_inner();
        assert!(find_u8_sized_uint(&data, [0x54, 0xDD]).is_none());
        assert!(find_u8_sized_uint(&data, [0x54, 0xAA]).is_none());
        assert!(find_u8_sized_uint(&data, [0x54, 0xB0]).is_none());
    }

    #[test]
    fn muxer_creates_valid_ebml_header() {
        let muxer = make_test_muxer();
        let data = muxer.finalise().unwrap().into_inner();

        // EBML header starts with the EBML element ID
        assert_eq!(&data[0..4], &[0x1A, 0x45, 0xDF, 0xA3]);

        // Parse EBML header size (VINT)
        let (header_size, header_size_len) = parse_test_vint(&data[4..]);
        let header_end = 4 + header_size_len + header_size as usize;

        // After EBML header, Segment element should follow
        assert_eq!(&data[header_end..header_end + 4], &[0x18, 0x53, 0x80, 0x67]);
    }

    #[test]
    fn muxer_writes_doctype_matroska() {
        let muxer = make_test_muxer();
        let data = muxer.finalise().unwrap().into_inner();

        // Search for "matroska" in the EBML header
        let pos = find_bytes(&data, b"matroska");
        assert!(pos.is_some(), "DocType 'matroska' not found in output");
    }

    #[test]
    fn muxer_writes_codec_ids() {
        let muxer = make_test_muxer();
        let data = muxer.finalise().unwrap().into_inner();

        assert!(
            find_bytes(&data, b"V_VP9").is_some(),
            "Video codec ID not found"
        );
        assert!(
            find_bytes(&data, b"A_OPUS").is_some(),
            "Audio codec ID not found"
        );
    }

    #[test]
    fn muxer_single_video_frame() {
        let mut muxer = make_test_muxer();

        let frame_data = [0xDE, 0xAD, 0xBE, 0xEF];
        muxer.write_video_frame(&frame_data, 0, true).unwrap();

        let data = muxer.finalise().unwrap().into_inner();

        // The frame data should appear in the output
        assert!(
            find_bytes(&data, &frame_data).is_some(),
            "Video frame data not found"
        );
    }

    #[test]
    fn muxer_single_audio_packet() {
        let mut muxer = make_test_muxer();

        // Need at least one video frame to start a cluster
        muxer.write_video_frame(&[0x01], 0, true).unwrap();

        let audio_data = [0xCA, 0xFE, 0xBA, 0xBE];
        muxer.write_audio_packet(&audio_data, 0).unwrap();

        let data = muxer.finalise().unwrap().into_inner();
        assert!(
            find_bytes(&data, &audio_data).is_some(),
            "Audio packet data not found"
        );
    }

    #[test]
    fn muxer_interleaved_av() {
        let mut muxer = make_test_muxer();

        // Interleave video and audio
        muxer.write_video_frame(&[0x10], 0, true).unwrap();
        muxer.write_audio_packet(&[0x20], 0).unwrap();
        muxer.write_video_frame(&[0x11], 33, false).unwrap();
        muxer.write_audio_packet(&[0x21], 20).unwrap();
        muxer.write_video_frame(&[0x12], 66, false).unwrap();
        muxer.write_audio_packet(&[0x22], 40).unwrap();

        let data = muxer.finalise().unwrap().into_inner();

        // All data should be present
        for needle in &[&[0x10u8][..], &[0x11], &[0x12], &[0x20], &[0x21], &[0x22]] {
            assert!(
                find_bytes(&data, needle).is_some(),
                "Data {:?} not found",
                needle
            );
        }
    }

    /// Rebuilds every block the way a demuxer would: track, timestamp, flags.
    ///
    /// The timestamp is the cluster's plus the block's signed offset. Walks the exact
    /// layout this muxer writes - cluster ID, the reserved eight-byte size,
    /// the Timestamp element, then SimpleBlocks - and asserts its way through
    /// rather than guessing, so a layout change fails here instead of
    /// silently returning nothing.
    fn demuxed_blocks(data: &[u8]) -> Vec<(u8, i64, u8)> {
        let mut blocks = Vec::new();
        for cluster in find_all_bytes(data, &[0x1F, 0x43, 0xB6, 0x75]) {
            let mut pos = cluster + 4 + 8;
            assert_eq!(data[pos], 0xE7, "a cluster opens with its Timestamp");
            let length = usize::from(data[pos + 1] & 0x7f);
            assert!(
                data[pos + 1] & 0x80 != 0,
                "Timestamp length is one VINT byte"
            );
            let mut cluster_timestamp = 0i64;
            for byte in &data[pos + 2..pos + 2 + length] {
                cluster_timestamp = (cluster_timestamp << 8) | i64::from(*byte);
            }
            pos += 2 + length;

            while pos < data.len() && data[pos] == 0xA3 {
                assert!(data[pos + 1] & 0x80 != 0, "block size is one VINT byte");
                let size = usize::from(data[pos + 1] & 0x7f);
                let track = data[pos + 2] & 0x7f;
                let relative = i16::from_be_bytes([data[pos + 3], data[pos + 4]]);
                let flags = data[pos + 5];
                blocks.push((track, cluster_timestamp + i64::from(relative), flags));
                pos += 2 + size;
            }
        }
        blocks
    }

    /// MDD-BUG-CRU-00817: the padded tail block carries its DiscardPadding.
    ///
    /// DiscardPadding is only valid inside a BlockGroup, so the tail cannot be
    /// a SimpleBlock - the compact form has nowhere to put it.
    ///
    /// Asserted as the exact bytes rather than by searching for the element
    /// ids: BlockGroup's id is the single byte 0xA0, which turns up in payload
    /// and in other elements' lengths by coincidence, so a scan for it proves
    /// nothing.
    #[test]
    fn the_padded_audio_tail_is_a_block_group_with_discard_padding() {
        let mut muxer = make_test_muxer();
        muxer.write_video_frame(&[0x01], 0, true).unwrap();
        // 7 ms of the final 20 ms frame is padding.
        muxer
            .write_audio_packet_with_discard_padding(&[0x03], 20, 7_000_000)
            .unwrap();
        let data = muxer.finalise().unwrap().into_inner();

        let expected = [
            0xA0, // BlockGroup
            0x8D, // ... 13 bytes of children
            0xA1, // Block
            0x85, // ... 5 bytes
            0x82, // track 2 as a VINT
            0x00, 0x14, // +20 ms from the cluster at 0
            0x00, // no flags: a Block states dependency with ReferenceBlock
            0x03, // the packet
            0x75, 0xA2, // DiscardPadding
            0x83, // ... 3 bytes
            0x6A, 0xCF, 0xC0, // 7_000_000 ns
        ];
        assert_eq!(
            count_bytes(&data, &expected),
            1,
            "the tail block is written exactly once, exactly like this"
        );

        // And an ordinary packet still takes the compact form, so the heavier
        // one is reserved for the tail that needs it.
        let mut plain = make_test_muxer();
        plain.write_video_frame(&[0x01], 0, true).unwrap();
        plain.write_audio_packet(&[0x03], 20).unwrap();
        let plain = plain.finalise().unwrap().into_inner();
        assert_eq!(
            count_bytes(&plain, &[0x75, 0xA2]),
            0,
            "a whole frame has no padding to declare"
        );
        assert_eq!(
            count_bytes(&data, &[0x75, 0xA2]),
            1,
            "and the tail declares its padding once"
        );
    }

    /// A signed element is as short as it can be while keeping its sign
    /// (MDD-BUG-CRU-00817).
    #[test]
    fn signed_elements_keep_their_sign_in_the_shortest_form() {
        assert_eq!(signed_width(0), 1);
        assert_eq!(signed_width(127), 1);
        assert_eq!(signed_width(-128), 1);
        assert_eq!(
            signed_width(128),
            2,
            "128 needs a second byte to stay positive"
        );
        assert_eq!(signed_width(-129), 2);
        assert_eq!(signed_width(32_767), 2);
        assert_eq!(signed_width(32_768), 3);

        let mut buf = Vec::new();
        write_signed_element(&mut buf, 0x75A2, -1).unwrap();
        assert_eq!(buf, [0x75, 0xA2, 0x81, 0xFF]);
        buf.clear();
        write_signed_element(&mut buf, 0x75A2, 128).unwrap();
        assert_eq!(buf, [0x75, 0xA2, 0x82, 0x00, 0x80]);
    }

    /// MDD-BUG-CRU-00815: Opus blocks are random access points and say so.
    ///
    /// RFC 9559 section 10.4: audio blocks are normally RAPs, and a RAP
    /// SimpleBlock sets its keyframe flag. Every Opus block used to clear it,
    /// advertising a dependency this codec does not have and leaving a
    /// seeking demuxer nothing in the audio track to land on.
    #[test]
    fn opus_blocks_are_flagged_as_random_access_points() {
        const KEYFRAME_FLAG: u8 = 0x80;
        const VIDEO_TRACK: u8 = 1;
        const AUDIO_TRACK: u8 = 2;

        let mut muxer = make_test_muxer();
        muxer.write_video_frame(&[0x01, 0x02], 0, true).unwrap();
        muxer.write_audio_packet(&[0x03, 0x04], 0).unwrap();
        muxer.write_video_frame(&[0x05, 0x06], 20, false).unwrap();
        muxer.write_audio_packet(&[0x07, 0x08], 20).unwrap();
        let data = muxer.finalise().unwrap().into_inner();

        let blocks = demuxed_blocks(&data);
        let audio: Vec<u8> = blocks
            .iter()
            .filter(|(track, _, _)| *track == AUDIO_TRACK)
            .map(|(_, _, flags)| *flags)
            .collect();
        assert_eq!(audio.len(), 2, "both audio packets are in the file");
        assert!(
            audio.iter().all(|flags| flags & KEYFRAME_FLAG != 0),
            "every Opus block is a random access point: {audio:?}"
        );

        // The flag still means something: video keeps its own answer, so this
        // is not simply every block claiming to be a keyframe.
        let video: Vec<u8> = blocks
            .iter()
            .filter(|(track, _, _)| *track == VIDEO_TRACK)
            .map(|(_, _, flags)| *flags)
            .collect();
        assert_eq!(video.len(), 2);
        assert!(
            video[0] & KEYFRAME_FLAG != 0,
            "the video keyframe is still flagged"
        );
        assert!(
            video[1] & KEYFRAME_FLAG == 0,
            "the video inter frame is still not"
        );
    }

    /// The warm-up Opus genuinely needs is declared on the track, which is
    /// what makes flagging every packet a RAP honest (MDD-BUG-CRU-00815).
    #[test]
    fn the_opus_track_declares_its_codec_delay_and_seek_pre_roll() {
        let mut muxer = make_test_muxer();
        muxer.write_video_frame(&[0x01], 0, true).unwrap();
        muxer.write_audio_packet(&[0x02], 0).unwrap();
        let data = muxer.finalise().unwrap().into_inner();

        assert!(
            find_bytes(&data, &[0x56, 0xAA]).is_some(),
            "CodecDelay undoes the encoder priming a RAP would otherwise replay"
        );
        assert!(
            find_bytes(&data, &[0x56, 0xBB]).is_some(),
            "SeekPreRoll says how much to decode and discard after seeking"
        );
    }

    /// MDD-BUG-CRU-00812: no block outlives its cluster's 16-bit reach.
    ///
    /// A SimpleBlock's timestamp is a signed 16-bit offset from its cluster,
    /// and audio never rotated a cluster for duration - it waited for a video
    /// keyframe. Past 32.767 seconds without one, every later audio packet was
    /// clamped to the same offset, so the file stayed superficially valid
    /// while its audio timeline stopped moving.
    #[test]
    fn audio_beyond_the_cluster_reach_keeps_its_real_timestamp() {
        let mut muxer = make_test_muxer();
        // One second apart, well past the limit, and no video at all - which
        // is the capture that has no keyframe to wait for.
        let written: Vec<u64> = (0..=45).map(|second| second * 1000).collect();
        for timestamp in &written {
            muxer.write_audio_packet(&[0x01, 0x02], *timestamp).unwrap();
        }
        let data = muxer.finalise().unwrap().into_inner();

        let blocks = demuxed_blocks(&data);
        assert_eq!(
            blocks.len(),
            written.len(),
            "every packet written must come back out"
        );
        let recovered: Vec<i64> = blocks.iter().map(|(_, timestamp, _)| *timestamp).collect();
        let expected: Vec<i64> = written.iter().map(|value| *value as i64).collect();
        assert_eq!(
            recovered, expected,
            "a demuxer must read back the timestamps that were written"
        );

        // The last packet is the one the clamp used to swallow: without the
        // fix it reads 32767, not 45000.
        assert_eq!(recovered.last(), Some(&45_000));
    }

    /// The rotation happens because it has to, not on a whim.
    ///
    /// Audio deliberately does not split on the one-second soft limit, to
    /// avoid cutting a GOP in half, and that stays true (MDD-BUG-CRU-00812).
    #[test]
    fn audio_within_the_cluster_reach_stays_in_one_cluster() {
        let mut muxer = make_test_muxer();
        for second in 0..=30 {
            muxer
                .write_audio_packet(&[0x01, 0x02], second * 1000)
                .unwrap();
        }
        let data = muxer.finalise().unwrap().into_inner();

        assert_eq!(
            count_bytes(&data, &[0x1F, 0x43, 0xB6, 0x75]),
            1,
            "thirty seconds is within reach, so nothing needed splitting"
        );
        let recovered: Vec<i64> = demuxed_blocks(&data)
            .iter()
            .map(|(_, timestamp, _)| *timestamp)
            .collect();
        assert_eq!(recovered.first(), Some(&0));
        assert_eq!(recovered.last(), Some(&30_000));
    }

    #[test]
    fn muxer_keyframe_forces_new_cluster() {
        let mut muxer = make_test_muxer();

        // Frame 1: keyframe (starts first cluster)
        muxer.write_video_frame(&[0x01], 0, true).unwrap();
        // Frame 2: non-keyframe
        muxer.write_video_frame(&[0x02], 33, false).unwrap();
        // Frame 3: keyframe (should force new cluster)
        muxer.write_video_frame(&[0x03], 66, true).unwrap();

        let data = muxer.finalise().unwrap().into_inner();

        // Count Cluster element IDs (0x1F43B675)
        let cluster_count = count_bytes(&data, &[0x1F, 0x43, 0xB6, 0x75]);
        assert_eq!(cluster_count, 2, "Expected 2 clusters (one per keyframe)");
    }

    #[test]
    fn muxer_single_cluster_size_is_patched_before_cues() {
        let mut muxer = make_test_muxer();
        muxer.write_video_frame(&[0x01], 0, true).unwrap();
        muxer.write_audio_packet(&[0x02], 20).unwrap();

        let data = muxer.finalise().unwrap().into_inner();
        let clusters = find_all_bytes(&data, &[0x1F, 0x43, 0xB6, 0x75]);
        assert_eq!(clusters.len(), 1, "expected a single Cluster element");

        let (cluster_size, size_width) = parse_cluster_size(&data, clusters[0]);
        assert_eq!(size_width, 8, "Cluster size field should stay fixed-width");
        assert_ne!(
            &data[clusters[0] + 4..clusters[0] + 12],
            &[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            "Cluster size must not remain the unknown-size sentinel",
        );

        let cluster_end = clusters[0] + 4 + size_width + cluster_size as usize;
        let cues_pos = actual_cues_position(&data);
        assert_eq!(
            cluster_end, cues_pos,
            "final Cluster must end immediately before Cues",
        );
    }

    #[test]
    fn muxer_multiple_cluster_sizes_are_patched() {
        let mut muxer = make_test_muxer();
        muxer.write_video_frame(&[0x01], 0, true).unwrap();
        muxer.write_video_frame(&[0x02], 33, false).unwrap();
        muxer.write_video_frame(&[0x03], 2000, true).unwrap();

        let data = muxer.finalise().unwrap().into_inner();
        let clusters = find_all_bytes(&data, &[0x1F, 0x43, 0xB6, 0x75]);
        assert_eq!(clusters.len(), 2, "expected two Cluster elements");

        for &cluster_pos in &clusters {
            assert_ne!(
                &data[cluster_pos + 4..cluster_pos + 12],
                &[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
                "Cluster at {cluster_pos} must not keep the unknown-size sentinel",
            );
        }

        let (first_size, first_width) = parse_cluster_size(&data, clusters[0]);
        assert_eq!(first_width, 8);
        assert_eq!(
            clusters[0] + 4 + first_width + first_size as usize,
            clusters[1],
            "first Cluster must end exactly at the second Cluster",
        );

        let (second_size, second_width) = parse_cluster_size(&data, clusters[1]);
        assert_eq!(second_width, 8);
        assert_eq!(
            clusters[1] + 4 + second_width + second_size as usize,
            actual_cues_position(&data),
            "last Cluster must end exactly at Cues",
        );
    }

    #[test]
    fn muxer_duration_cluster_forces_new_cluster() {
        let mut muxer = make_test_muxer();

        // Write frames spanning > 1 second to trigger cluster split
        muxer.write_video_frame(&[0x01], 0, true).unwrap();
        muxer.write_video_frame(&[0x02], 500, false).unwrap();
        // This should be in a new cluster (1001 ms after cluster start)
        muxer.write_video_frame(&[0x03], 1001, false).unwrap();

        let data = muxer.finalise().unwrap().into_inner();

        let cluster_count = count_bytes(&data, &[0x1F, 0x43, 0xB6, 0x75]);
        assert_eq!(
            cluster_count, 2,
            "Expected 2 clusters (split at ~1s boundary)"
        );
    }

    #[test]
    fn duration_covers_presented_opus_endpoint_in_segment_coordinates() {
        for frame_samples in [960, 1920] {
            let mut encoder =
                ropus::Encoder::builder(48_000, ropus::Channels::Stereo, ropus::Application::Audio)
                    .build()
                    .unwrap();
            let mut packet = vec![0; 4000];
            let size = encoder
                .encode_float(&vec![0.0; frame_samples * 2], &mut packet)
                .unwrap();
            packet.truncate(size);
            let samples =
                ropus::opus::decoder::opus_packet_get_nb_samples(&packet, 48_000).unwrap();
            assert_eq!(samples as usize, frame_samples);
            let mut muxer = make_test_muxer();
            muxer.write_video_frame(&[0x01], 100, true).unwrap();
            muxer.write_audio_packet(&packet, 80).unwrap();
            muxer
                .write_audio_packet_with_discard_padding(&packet, 300, 7_000_001)
                .unwrap();
            let data = muxer.finalise().unwrap().into_inner();
            let at = find_bytes(&data, &[0x44, 0x89]).unwrap() + 3;
            let duration = f64::from_be_bytes(data[at..at + 8].try_into().unwrap());
            let element_value = |id: &[u8]| {
                let at = find_bytes(&data, id).unwrap() + id.len();
                let (size, width) = parse_test_vint(&data[at..]);
                data[at + width..at + width + size as usize]
                    .iter()
                    .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
            };
            let delay_ns = element_value(&[0x56, 0xAA]);
            let padding_ns = element_value(&[0x75, 0xA2]);
            assert_eq!(delay_ns, 6_500_000);
            assert_eq!(padding_ns, 7_000_001);
            let presented_end =
                300.0 + f64::from(samples) / 48.0 - (delay_ns + padding_ns) as f64 / 1_000_000.0;
            assert!(
                (duration - presented_end).abs() < 0.000_001,
                "Duration {duration} must equal playable Opus endpoint {presented_end} in segment coordinates"
            );
        }
    }

    #[test]
    fn duration_keeps_earlier_audio_and_video_in_existing_segment_coordinates() {
        let mut muxer = make_test_muxer();
        muxer.write_video_frame(&[0x01], 100, true).unwrap();
        muxer.write_audio_packet(&[0x00], 80).unwrap();
        let data = muxer.finalise().unwrap().into_inner();
        let at = find_bytes(&data, &[0x44, 0x89]).unwrap() + 3;
        let duration = f64::from_be_bytes(data[at..at + 8].try_into().unwrap());
        assert_eq!(
            duration, 100.0,
            "Duration must cover video at segment timestamp 100 after earlier audio insertion"
        );
        let cues = actual_cues_position(&data);
        let cue_time = find_bytes(&data[cues..], &[0xB3, 0x81]).unwrap() + cues;
        assert_eq!(data[cue_time + 2], 100);
    }

    #[test]
    fn muxer_duration_patched_on_finalise() {
        let mut muxer = make_test_muxer();

        muxer.write_video_frame(&[0x01], 100, true).unwrap();
        muxer.write_video_frame(&[0x02], 200, false).unwrap();
        muxer.write_audio_packet(&[0x03], 300).unwrap();

        let data = muxer.finalise().unwrap().into_inner();

        // Duration uses the same zero-origin coordinates as the blocks: 300 ms.
        // Find the Duration element (ID 0x4489)
        let duration_pos = find_bytes(&data, &[0x44, 0x89]).unwrap();
        // Skip ID (2 bytes) + size VINT (1 byte, value 8) to get to the float
        let float_start = duration_pos + 3;
        let duration_bytes: [u8; 8] = data[float_start..float_start + 8].try_into().unwrap();
        let duration = f64::from_be_bytes(duration_bytes);
        assert!(
            (duration - 300.0).abs() < f64::EPSILON,
            "Duration should be 300.0 ms, got {}",
            duration
        );
    }

    #[test]
    fn muxer_segment_size_patched_on_finalise() {
        let mut muxer = make_test_muxer();
        muxer.write_video_frame(&[0x01], 0, true).unwrap();

        let data = muxer.finalise().unwrap().into_inner();

        // Find Segment element ID
        let seg_pos = find_bytes(&data, &[0x18, 0x53, 0x80, 0x67]).unwrap();
        let size_start = seg_pos + 4;

        // Size is an 8-byte VINT
        let (segment_size, _) = parse_test_vint(&data[size_start..]);

        // Segment body starts after the 8-byte size
        let body_start = size_start + 8;
        let expected_size = data.len() - body_start;
        assert_eq!(
            segment_size as usize, expected_size,
            "Segment size mismatch"
        );
    }

    #[test]
    fn muxer_empty_file_finalises() {
        let muxer = make_test_muxer();
        let data = muxer.finalise().unwrap().into_inner();

        // Should still have valid EBML header + Segment + SegmentInfo + Tracks
        assert!(!data.is_empty());
        assert_eq!(&data[0..4], &[0x1A, 0x45, 0xDF, 0xA3]); // EBML ID
    }

    #[test]
    fn muxer_zero_timestamp_frame() {
        let mut muxer = make_test_muxer();
        muxer.write_video_frame(&[0xFF], 0, true).unwrap();

        let data = muxer.finalise().unwrap().into_inner();
        assert!(find_bytes(&data, &[0xFF]).is_some());
    }

    #[test]
    fn muxer_empty_frame_data() {
        let mut muxer = make_test_muxer();
        muxer.write_video_frame(&[], 0, true).unwrap();

        // Should not panic; the SimpleBlock has a valid header but no payload
        let _data = muxer.finalise().unwrap().into_inner();
    }

    #[test]
    fn muxer_many_frames_across_clusters() {
        let mut muxer = make_test_muxer();

        // Write 100 frames at 30fps (33ms apart), with keyframes every 30 frames
        for i in 0..100u64 {
            let ts = i * 33;
            let keyframe = i % 30 == 0;
            muxer.write_video_frame(&[i as u8], ts, keyframe).unwrap();
        }

        let data = muxer.finalise().unwrap().into_inner();

        // Should have multiple clusters (keyframes at 0, 990ms, 1980ms, plus
        // duration-based splits)
        let cluster_count = count_bytes(&data, &[0x1F, 0x43, 0xB6, 0x75]);
        assert!(
            cluster_count >= 3,
            "Expected at least 3 clusters, got {}",
            cluster_count
        );
    }

    #[test]
    fn muxer_audio_before_video_starts_cluster() {
        let mut muxer = make_test_muxer();

        // Audio packet as the very first write — should start a cluster
        muxer.write_audio_packet(&[0xAA], 0).unwrap();

        let data = muxer.finalise().unwrap().into_inner();
        let cluster_count = count_bytes(&data, &[0x1F, 0x43, 0xB6, 0x75]);
        assert_eq!(cluster_count, 1);
    }

    #[test]
    fn muxer_writing_app_present() {
        let muxer = make_test_muxer();
        let data = muxer.finalise().unwrap().into_inner();

        assert!(find_bytes(&data, b"gp64-capture").is_some());
    }

    #[test]
    fn muxer_timestamp_scale_is_one_millisecond() {
        let muxer = make_test_muxer();
        let data = muxer.finalise().unwrap().into_inner();

        // TimestampScale element: ID 0x2AD7B1, followed by size VINT and value 1_000_000
        // 1_000_000 = 0x0F4240 (3 bytes)
        let ts_pos = find_bytes(&data, &[0x2A, 0xD7, 0xB1]).unwrap();
        // ID (3 bytes) + size VINT (1 byte, value 3) + 3 bytes of value
        assert_eq!(data[ts_pos + 3], 0x83); // VINT encoding of 3
        assert_eq!(&data[ts_pos + 4..ts_pos + 7], &[0x0F, 0x42, 0x40]);
    }

    #[test]
    fn muxer_seekhead_and_cues_present_after_keyframe() {
        let mut muxer = make_test_muxer();
        // Two keyframes + one non-keyframe in between.
        muxer.write_video_frame(&[0x01], 0, true).unwrap();
        muxer.write_video_frame(&[0x02], 33, false).unwrap();
        muxer.write_video_frame(&[0x03], 2000, true).unwrap();

        let data = muxer.finalise().unwrap().into_inner();

        // SeekHead ID (0x114D9B74) must appear exactly once (nothing else in
        // our file embeds these bytes as a SeekID target).
        assert_eq!(
            count_bytes(&data, &[0x11, 0x4D, 0x9B, 0x74]),
            1,
            "expected one SeekHead"
        );
        // Cues ID (0x1C53BB6B) appears twice: once as the SeekID payload in
        // the SeekHead and once as the actual Cues element header.
        assert_eq!(
            count_bytes(&data, &[0x1C, 0x53, 0xBB, 0x6B]),
            2,
            "expected SeekID + Cues element occurrences",
        );
        // Find the Cues element (the one AFTER the SeekHead).
        let seekhead_end =
            find_bytes(&data, &[0x11, 0x4D, 0x9B, 0x74]).unwrap() + SEEKHEAD_RESERVED_BYTES;
        let cues_pos = seekhead_end
            + data[seekhead_end..]
                .windows(4)
                .position(|w| w == [0x1C, 0x53, 0xBB, 0x6B])
                .expect("Cues element after SeekHead");
        let cues_tail = &data[cues_pos..];
        // CueTime element ID 0xB3 followed by size VINT then the value. t=0
        // encodes as [0xB3, 0x81, 0x00] (size=1, value=0).
        assert!(
            cues_tail.windows(3).any(|w| w == [0xB3, 0x81, 0x00]),
            "CueTime for t=0 not found in Cues",
        );
        // t=2000 encodes as 0x07D0 (2 bytes): [0xB3, 0x82, 0x07, 0xD0].
        assert!(
            cues_tail.windows(4).any(|w| w == [0xB3, 0x82, 0x07, 0xD0]),
            "CueTime for t=2000 not found in Cues",
        );
    }

    #[test]
    fn muxer_seekhead_offsets_land_on_expected_elements() {
        let mut muxer = make_test_muxer();
        muxer.write_video_frame(&[0x01], 0, true).unwrap();
        let data = muxer.finalise().unwrap().into_inner();

        // Segment body starts right after Segment ID (4 bytes) + 8-byte size VINT
        let seg_pos = find_bytes(&data, &[0x18, 0x53, 0x80, 0x67]).unwrap();
        let segment_body = seg_pos + 4 + 8;

        // SeekHead starts at segment body (we reserve it first).
        assert_eq!(
            &data[segment_body..segment_body + 4],
            &[0x11, 0x4D, 0x9B, 0x74],
            "SeekHead not at segment body start"
        );

        // Walk the Seek entries and verify each SeekPosition lands on the
        // target element ID we claimed it pointed to.
        //
        // SeekHead layout: ID(4) + size VINT(1) + body. Body is 3 × Seek.
        // Each Seek: ID(0x4DBB, 2) + size(1) + SeekID(2+1+4=7) + SeekPosition(2+1+8=11)
        let body_start = segment_body + 4 + 1; // past size VINT (1-byte)
        for i in 0..3 {
            let seek_start = body_start + i * 21;
            assert_eq!(&data[seek_start..seek_start + 2], &[0x4D, 0xBB]);
            let seek_id_start = seek_start + 3; // past Seek ID(2) + size(1)
            assert_eq!(&data[seek_id_start..seek_id_start + 2], &[0x53, 0xAB]);
            let target_id = &data[seek_id_start + 3..seek_id_start + 7];

            let seek_pos_start = seek_id_start + 7;
            assert_eq!(&data[seek_pos_start..seek_pos_start + 2], &[0x53, 0xAC]);
            let payload = &data[seek_pos_start + 3..seek_pos_start + 11];
            let offset = u64::from_be_bytes(payload.try_into().unwrap());

            let target_pos = segment_body + offset as usize;
            assert_eq!(
                &data[target_pos..target_pos + 4],
                target_id,
                "Seek #{i} offset {offset} points at wrong element",
            );
        }
    }

    #[test]
    fn muxer_cues_absent_keyframes_still_emits_empty_cues() {
        // No keyframes written (no video at all) — finalise should still
        // produce a well-formed file with a (empty) Cues master so the
        // SeekHead pointer doesn't dangle. (The ID appears twice: once as
        // SeekID payload, once as the actual Cues element.)
        let muxer = make_test_muxer();
        let data = muxer.finalise().unwrap().into_inner();
        assert_eq!(
            count_bytes(&data, &[0x1C, 0x53, 0xBB, 0x6B]),
            2,
            "expected SeekID + empty Cues element"
        );
    }

    #[test]
    fn muxer_negative_relative_timestamp() {
        let mut muxer = make_test_muxer();

        // Start cluster at t=100ms (keyframe)
        muxer
            .write_video_frame(&[0x82, 0x49, 0x83, 0x42, 0x00, 0x07], 100, true)
            .unwrap();
        // Audio packet with earlier timestamp (relative = -20ms)
        muxer.write_audio_packet(&[0x01, 0x02], 80).unwrap();
        // Should not panic; file should be valid
        let data = muxer.finalise().unwrap().into_inner();
        assert!(!data.is_empty());
    }

    // -- Test helpers -------------------------------------------------------

    /// Parse a VINT from a byte slice. Returns (value, bytes_consumed).
    fn parse_test_vint(data: &[u8]) -> (u64, usize) {
        let first = data[0];
        let width = first.leading_zeros() as usize + 1;
        assert!(width <= 8, "Invalid VINT width");
        assert!(data.len() >= width, "Not enough bytes for VINT");

        let mask = (1u8 << (8 - width)) - 1;
        let mut value = u64::from(first & mask);
        for byte in &data[1..width] {
            value = (value << 8) | u64::from(*byte);
        }
        (value, width)
    }

    /// Find the first occurrence of `needle` in `haystack`.
    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// Find all non-overlapping occurrences of `needle` in `haystack`.
    fn find_all_bytes(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
        let mut positions = Vec::new();
        let mut start = 0;
        while start + needle.len() <= haystack.len() {
            if &haystack[start..start + needle.len()] == needle {
                positions.push(start);
                start += needle.len();
            } else {
                start += 1;
            }
        }
        positions
    }

    fn parse_cluster_size(data: &[u8], cluster_pos: usize) -> (u64, usize) {
        assert_eq!(
            &data[cluster_pos..cluster_pos + 4],
            &[0x1F, 0x43, 0xB6, 0x75],
        );
        parse_test_vint(&data[cluster_pos + 4..])
    }

    fn actual_cues_position(data: &[u8]) -> usize {
        let seg_pos = find_bytes(data, &[0x18, 0x53, 0x80, 0x67]).unwrap();
        let segment_body = seg_pos + 4 + 8;
        let seekhead_end = segment_body + SEEKHEAD_RESERVED_BYTES;
        seekhead_end
            + data[seekhead_end..]
                .windows(4)
                .position(|w| w == [0x1C, 0x53, 0xBB, 0x6B])
                .expect("Cues element after SeekHead")
    }

    /// Count non-overlapping occurrences of `needle` in `haystack`.
    fn count_bytes(haystack: &[u8], needle: &[u8]) -> usize {
        let mut count = 0;
        let mut start = 0;
        while start + needle.len() <= haystack.len() {
            if &haystack[start..start + needle.len()] == needle {
                count += 1;
                start += needle.len();
            } else {
                start += 1;
            }
        }
        count
    }

    // -- Extra branch-coverage tests --------------------------------------------

    #[test]
    fn vint_width_covers_5_to_8_byte_widths() {
        assert_eq!(vint_width(0x0FFF_FFFF), 5);
        assert_eq!(vint_width(0x07_FFFF_FFFE), 5);
        assert_eq!(vint_width(0x07_FFFF_FFFF), 6);
        assert_eq!(vint_width(0x03FF_FFFF_FFFE), 6);
        assert_eq!(vint_width(0x03FF_FFFF_FFFF), 7);
        assert_eq!(vint_width(0x01_FFFF_FFFF_FFFE), 7);
        assert_eq!(vint_width(0x01_FFFF_FFFF_FFFF), 8);
        assert_eq!(vint_width(u64::MAX), 8);
    }

    #[test]
    fn vint_encoding_large_widths_round_trip_via_fixed() {
        // Round-trip via write_vint_fixed for 5..=8 byte widths — ensures the
        // marker bit and padding land in the right positions for the big
        // widths that `write_vint` selects at these sizes.
        for width in 5..=8 {
            let mut buf = Vec::new();
            let value = (1u64 << ((8 * width - width) - 1)) | 0x42;
            write_vint_fixed(&mut buf, value, width).unwrap();
            assert_eq!(buf.len(), width, "emitted byte count matches width {width}");
            // The leading bit of the first byte is the width marker.
            let expected_marker_msb = 1u8 << (8 - width);
            assert_eq!(
                buf[0] & expected_marker_msb,
                expected_marker_msb,
                "width-{width} marker bit set",
            );
        }
    }
}
