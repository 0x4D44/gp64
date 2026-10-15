//! Capture session and encoder thread.
//!
//! Provides [`CaptureSession`] which accepts video frames and audio samples,
//! encoding them to VP9 + Opus and muxing into an MKV file on a background
//! thread.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rvp9_encoder::frame_loop::EncoderParallelism;
use rvp9_encoder::{EncoderConfig, RcMode, StreamEncoder, YuvFrameRef};

use crate::convert::{convert_0rgb_to_i420_into, nearest_upscale_0rgb_into, I420Planes};
use crate::muxer::{build_opus_head, MkvMuxer, VideoGeometry};
use crate::resample::LinearResampler;

/// Pick an integer nearest-neighbour upscale factor for a capture source.
///
/// Small retro-era resolutions (Mode 13h's 320×200, 320×240, 400×300, etc.)
/// get scaled so the encoded file is natively ≥ 1280 pixels wide. This
/// preserves the blocky pixel look two ways: players display the file at its
/// native size without interpolating up, and VP9's 4:2:0 chroma subsampling
/// can no longer bleed colour between neighbouring source pixels. Sources at
/// 640 wide or more are left alone — they don't need help.
fn capture_scale_factor(width: u32) -> u32 {
    const THRESHOLD: u32 = 640;
    const TARGET: u32 = 1280;
    if width == 0 || width >= THRESHOLD {
        1
    } else {
        TARGET.div_ceil(width)
    }
}

/// The upscale factor for a live recording, which must encode in real time.
///
/// [`capture_scale_factor`]'s 4× for 320×200 encodes 16 times the pixels and
/// fell to 45–95% of real time on 70 fps race footage, so a live recording
/// overflowed its queue and stopped within seconds. Doubling small sources
/// held real time with room to spare. It still gives every source pixel its
/// own 4:2:0 chroma sample, so colour no longer bleeds between neighbours.
fn live_scale_factor(width: u32) -> u32 {
    if width == 0 || width >= 640 {
        1
    } else {
        2
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during capture.
#[derive(Debug)]
pub enum CaptureError {
    /// I/O error (file creation, writing).
    Io(std::io::Error),
    /// Encoder error (VP9 or Opus initialisation/encoding failure).
    Encoder(String),
    /// The encoder channel was closed unexpectedly.
    ChannelClosed,
    /// The requested [`CaptureConfig`] is not one this pipeline can encode.
    Config(String),
    /// The session ended before any video frame arrived, so no file was ever
    /// created. Reporting the configured path as a success handed the caller a
    /// path that does not exist (MDD-BUG-CRU-00818).
    NoVideo,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "capture I/O error: {e}"),
            Self::Encoder(e) => write!(f, "capture encoder error: {e}"),
            Self::ChannelClosed => write!(f, "capture channel closed"),
            Self::Config(e) => write!(f, "unsupported capture configuration: {e}"),
            Self::NoVideo => write!(
                f,
                "capture produced no video frames, so no file was written"
            ),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Messages between emulator thread and encoder thread
// ---------------------------------------------------------------------------

struct VideoMessage {
    pixels: Vec<u32>,
    width: u32,
    height: u32,
    timestamp_ns: u64,
}

struct AudioMessage {
    samples: Vec<f32>,
    /// Guest-time timestamp of the first sample in this message, in nanoseconds.
    /// Used to anchor the Opus timeline so a gap in the audio becomes silence at the
    /// correct position rather than shortening the audio track relative to video.
    timestamp_ns: u64,
}

/// A completed frame in one coherent emulation slice.
pub struct CaptureFrame<'a> {
    pub pixels: &'a [u32],
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: u64,
}

enum EncoderMessage {
    Video(VideoMessage),
    Slice(QueuedSlice),
}

struct QueuedSlice {
    events: VecDeque<SliceEvent>,
    end_ns: u64,
    video_slots: usize,
    audio_slots: usize,
}

enum SliceEvent {
    Video(VideoMessage),
    Audio(AudioMessage),
}

/// Split using original source indices: rounding each successive piece's
/// duration instead would accumulate drift at non-integral sample boundaries.
fn slice_events(
    frames: &[CaptureFrame<'_>],
    samples: &[f32],
    timestamp_ns: u64,
    rate: u32,
) -> VecDeque<SliceEvent> {
    let mut events = VecDeque::new();
    let total = samples.len() / 2;
    let mut consumed = 0;
    for frame in frames {
        let before = (u128::from(frame.timestamp_ns.saturating_sub(timestamp_ns))
            * u128::from(rate))
        .div_ceil(1_000_000_000)
        .min(total as u128) as usize;
        if before > consumed {
            events.push_back(SliceEvent::Audio(AudioMessage {
                samples: samples[consumed * 2..before * 2].to_vec(),
                timestamp_ns: timestamp_ns
                    .saturating_add((consumed as u128 * 1_000_000_000 / u128::from(rate)) as u64),
            }));
            consumed = before;
        }
        events.push_back(SliceEvent::Video(VideoMessage {
            pixels: frame.pixels.to_vec(),
            width: frame.width,
            height: frame.height,
            timestamp_ns: frame.timestamp_ns,
        }));
    }
    if consumed < total {
        events.push_back(SliceEvent::Audio(AudioMessage {
            samples: samples[consumed * 2..].to_vec(),
            timestamp_ns: timestamp_ns
                .saturating_add((consumed as u128 * 1_000_000_000 / u128::from(rate)) as u64),
        }));
    }
    events
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The only audio channel count this pipeline can encode.
///
/// The resampler consumes interleaved L/R pairs, the Opus encoder is built as
/// [`ropus::Channels::Stereo`], and the frame arithmetic below counts two
/// samples per frame. Stereo is a property of the pipeline, not a default.
pub const SUPPORTED_CHANNELS: u16 = 2;

/// The source sample rates the resampler will accept, in Hz.
///
/// The floor is the 8 kHz `mdem-host` falls back to when it has no audio
/// device; the ceiling is generous headroom above any real capture device.
/// Outside this band the resampling ratio stops being meaningful — and at zero
/// it is infinite, which is the hang described in MDD-BUG-CRU-00809's sibling
/// MDD-BUG-CRU-00810.
pub const SUPPORTED_SAMPLE_RATES: std::ops::RangeInclusive<u32> = 8_000..=192_000;

const OPUS_SAMPLE_RATE_HZ: u32 = 48_000;
const OPUS_FRAME_SAMPLES_PER_CHANNEL: usize = 960;
const OPUS_FRAME_DURATION_MS: u64 = 20;
const SILENCE_FILL_TOLERANCE_MS: u64 = 20;

/// Long timestamp gaps are treated as a capture timeline failure rather than
/// materialised as an unbounded amount of silence. Gaps within this limit are
/// streamed through the normal 20 ms Opus frame path.
const MAX_SILENCE_FILL_GAP_MS: u64 = 60_000;

/// Configuration for a capture session.
pub struct CaptureConfig {
    /// Output MKV file path.
    pub output_path: PathBuf,
    /// Initial video width in pixels.
    pub width: u32,
    /// Initial video height in pixels.
    pub height: u32,
    /// Source audio sample rate in Hz (typically 44100).
    pub sample_rate: u32,
    /// Number of audio channels. Must be [`SUPPORTED_CHANNELS`].
    pub channels: u16,
}

impl CaptureConfig {
    /// Check that this configuration is one the pipeline can actually encode.
    ///
    /// MDD-BUG-CRU-00810: the audio fields were accepted verbatim and only
    /// consulted deep inside the encoder thread, where a bad value could no
    /// longer be reported. A `sample_rate` of zero made the resampling ratio
    /// infinite and its step zero, so the worker looped without advancing;
    /// `channels` of zero divided by zero in the timeline arithmetic; and any
    /// other channel count wrote an OpusHead that contradicted the stereo
    /// payload underneath it. All three surfaced as a hang, a panic, or a
    /// silently desynchronised file, long after the caller could act.
    ///
    /// Callers do not have to call this: [`CaptureSession::start`] rejects a
    /// bad configuration before it spawns anything. It is public so a UI can
    /// check a setting before offering it.
    pub fn validate(&self) -> Result<(), CaptureError> {
        if self.channels != SUPPORTED_CHANNELS {
            return Err(CaptureError::Config(format!(
                "{} audio channel(s) requested, but the encoder pipeline is \
                 stereo end to end and can only encode {SUPPORTED_CHANNELS}",
                self.channels
            )));
        }
        if !SUPPORTED_SAMPLE_RATES.contains(&self.sample_rate) {
            return Err(CaptureError::Config(format!(
                "source sample rate {} Hz is outside the supported {}..={} Hz",
                self.sample_rate,
                SUPPORTED_SAMPLE_RATES.start(),
                SUPPORTED_SAMPLE_RATES.end()
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A cross-thread request to stop a capture encoder.
///
/// The emulator owns the [`CaptureSession`] and normally stops it itself. The
/// UI can retain this handle during shutdown so a wedged emulator thread does
/// not leave the encoder waiting forever for its senders to be dropped.
#[derive(Clone)]
pub struct CaptureStopHandle {
    stop_requested: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

impl CaptureStopHandle {
    /// Request that the encoder drain queued messages and finalise the file.
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
    }

    /// Wait until the encoder has returned from its finalisation path.
    pub fn wait_for_finish(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.finished.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
        true
    }
}

/// Marks the encoder finished when its thread leaves, however it leaves, and
/// wakes any submitter waiting for a queue slot so it sees that at once.
struct CaptureCompletionGuard {
    finished: Arc<AtomicBool>,
    queue_depth: QueueDepth,
}

impl Drop for CaptureCompletionGuard {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
        self.queue_depth.wake_waiters();
    }
}

/// How many frames the video queue holds before a submitter waits.
const VIDEO_QUEUE_CAPACITY: usize = 4;

/// How many audio messages the queue holds before a submitter waits.
const AUDIO_QUEUE_CAPACITY: usize = 4096;

/// Longest a waiting submitter sleeps before it re-checks whether the session
/// is over. Freed slots and the encoder's exit wake it directly; only an
/// emergency stop from another thread relies on this re-check.
const SLOT_WAIT_RECHECK: Duration = Duration::from_millis(20);

/// How full each bounded queue is, so a sender can ask before it builds.
///
/// `SyncSender::try_send` only answers once the message exists, and building a
/// video message copies the whole frame - about 1.9 MiB at 800x600. Claiming a
/// slot first means a submitter that has to wait, or that gives up because the
/// session ended, has copied nothing (MDD-BUG-CRU-00816).
#[derive(Clone)]
struct QueueDepth {
    video: Arc<AtomicUsize>,
    audio: Arc<AtomicUsize>,
    slot_freed: Arc<SlotFreed>,
}

/// What a submitter blocked on a full queue sleeps on.
#[derive(Default)]
struct SlotFreed {
    lock: Mutex<()>,
    signal: Condvar,
}

impl QueueDepth {
    fn new() -> Self {
        Self {
            video: Arc::new(AtomicUsize::new(0)),
            audio: Arc::new(AtomicUsize::new(0)),
            slot_freed: Arc::new(SlotFreed::default()),
        }
    }

    /// Give a slot back and wake any submitter waiting for one.
    fn release(&self, depth: &AtomicUsize) {
        release_queue_slot(depth);
        self.wake_waiters();
    }

    fn release_many(&self, depth: &AtomicUsize, slots: usize) {
        let _ = depth.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            Some(current.saturating_sub(slots))
        });
        self.wake_waiters();
    }

    fn wake_waiters(&self) {
        // Taken so a waiter between its last check and its wait cannot miss
        // this notification.
        let _guard = self
            .slot_freed
            .lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.slot_freed.signal.notify_all();
    }

    /// Take a slot, waiting for the encoder to free one while the queue is
    /// full. Returns `false` only once `session_over` says so - the recording
    /// was stopped or its encoder has gone - so it cannot wait forever.
    ///
    /// Waiting rather than dropping is what keeps a recording complete
    /// (MDD-BUG-FLU-02234). Every submitter stamps its payload with guest
    /// time, so a wait slows the guest in wall time and costs the file
    /// nothing, where a drop left a visible hole in it.
    fn blocking_claim_queue_slot(
        &self,
        depth: &AtomicUsize,
        capacity: usize,
        session_over: impl Fn() -> bool,
    ) -> bool {
        loop {
            if claim_queue_slot(depth, capacity) {
                return true;
            }
            let guard = self
                .slot_freed
                .lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if session_over() {
                return false;
            }
            // A slot freed between the claim above and taking the lock sent
            // its notification before this wait began; look before sleeping.
            if depth.load(Ordering::Acquire) < capacity {
                continue;
            }
            let _ = self
                .slot_freed
                .signal
                .wait_timeout(guard, SLOT_WAIT_RECHECK)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Takes a slot if the queue has one, reporting whether it did.
///
/// The count is incremented before the payload is built, so a full queue is
/// answered with nothing copied. It is an upper bound on the channel's true
/// occupancy - never lower - so it can refuse a fraction early but can never
/// oversubscribe.
fn claim_queue_slot(depth: &AtomicUsize, capacity: usize) -> bool {
    claim_queue_slots(depth, capacity, 1)
}

fn claim_queue_slots(depth: &AtomicUsize, capacity: usize, slots: usize) -> bool {
    depth
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(slots).filter(|next| *next <= capacity)
        })
        .is_ok()
}

/// Gives a slot back, once the message that held it has left the queue.
///
/// Saturating rather than wrapping: a stray release must not make the count
/// enormous, because that would read as permanently full and drop everything
/// from then on.
fn release_queue_slot(depth: &AtomicUsize) {
    let _ = depth.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_sub(1))
    });
}

/// A running capture session.
///
/// Use [`try_submit_slice`](Self::try_submit_slice) for coherent live capture
/// with resolution changes. Single-resolution bulk callers can submit frames
/// via [`submit_frame`](Self::submit_frame) and audio via
/// [`submit_audio`](Self::submit_audio). Call [`stop`](Self::stop) to
/// finalise the MKV file and get the output path.
///
/// Dropping a `CaptureSession` without calling `stop()` will still finalise
/// the file, but any errors are silently ignored.
pub struct CaptureSession {
    video_tx: Option<SyncSender<EncoderMessage>>,
    sample_rate: u32,
    audio_tx: Option<SyncSender<AudioMessage>>,
    thread: Option<JoinHandle<Result<Vec<PathBuf>, CaptureError>>>,
    elapsed_ns: Arc<AtomicU64>,
    video_capacity: usize,
    stop_handle: CaptureStopHandle,
    queue_depth: QueueDepth,
    /// Set once the submit side has said the encoder is gone, so a dead
    /// encoder is reported once rather than once per frame.
    encoder_loss_reported: AtomicBool,
}

/// An exclusively created menu output, owned before encoding starts.
/// A recording stopped before its first frame leaves this empty file reserved.
/// Dropping the handle never unlinks the path, which may have been replaced.
pub struct ReservedCaptureOutput {
    path: PathBuf,
    file: File,
    excluded: Option<PathBuf>,
}
impl ReservedCaptureOutput {
    /// Reserve `path`, adding ` (2)`, ` (3)`… on collisions. Never uses `excluded`.
    pub fn reserve(path: &Path, excluded: Option<&Path>) -> std::io::Result<Self> {
        let mut candidate = path.to_path_buf();
        let mut number = 2u64;
        loop {
            if excluded != Some(candidate.as_path()) {
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&candidate)
                {
                    Ok(file) => {
                        return Ok(Self {
                            path: candidate,
                            file,
                            excluded: excluded.map(Path::to_path_buf),
                        })
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let extension = path
                .extension()
                .map(|ext| format!(".{}", ext.to_string_lossy()))
                .unwrap_or_default();
            candidate = path.with_file_name(format!("{stem} ({number}){extension}"));
            number = number
                .checked_add(1)
                .ok_or_else(|| std::io::Error::other("recording filename numbers exhausted"))?;
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl CaptureSession {
    /// Start a new capture session.
    ///
    /// Spawns an encoder thread that consumes video frames and audio samples,
    /// encodes them, and writes them to the configured MKV file.
    pub fn start(config: CaptureConfig) -> Result<Self, CaptureError> {
        Self::start_with_scale(config, capture_scale_factor, VIDEO_QUEUE_CAPACITY, None)
    }

    /// Record a running emulator, upscaled by [`live_scale_factor`].
    /// Budgets of 64 video frames and 4096 PCM messages absorb short stalls.
    /// Each coherent slice holds
    /// at most 64 frames and five seconds of source stereo PCM.
    /// Live callers use the nonblocking submission methods and stop on overflow.
    pub fn start_live(config: CaptureConfig) -> Result<Self, CaptureError> {
        Self::start_with_scale(config, live_scale_factor, 64, None)
    }

    /// Live menu recording with an already owned base file and exclusive rollovers.
    /// The reserved path is authoritative even if the config names another file.
    pub fn start_live_reserved(
        mut config: CaptureConfig,
        output: ReservedCaptureOutput,
    ) -> Result<Self, CaptureError> {
        config.output_path = output.path.clone();
        Self::start_with_scale(config, live_scale_factor, 64, Some(output))
    }

    fn start_with_scale(
        config: CaptureConfig,
        scale: fn(u32) -> u32,
        video_capacity: usize,
        reserved: Option<ReservedCaptureOutput>,
    ) -> Result<Self, CaptureError> {
        // Before anything is spawned or created: an unencodable configuration
        // has to fail HERE, where the caller can still report it, rather than
        // inside the encoder thread where its only exits are a hang, a panic,
        // or a malformed file (MDD-BUG-CRU-00810).
        config.validate()?;

        let sample_rate = config.sample_rate;
        let (video_tx, video_rx) =
            mpsc::sync_channel::<EncoderMessage>(video_capacity + AUDIO_QUEUE_CAPACITY);
        let (audio_tx, audio_rx) = mpsc::sync_channel::<AudioMessage>(AUDIO_QUEUE_CAPACITY);
        let queue_depth = QueueDepth::new();
        let queue_depth_for_thread = queue_depth.clone();
        let elapsed_ns = Arc::new(AtomicU64::new(0));
        let elapsed_clone = elapsed_ns.clone();
        let stop_requested = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let stop_requested_for_thread = Arc::clone(&stop_requested);
        let finished_for_thread = Arc::clone(&finished);

        let thread = thread::Builder::new()
            .name("capture-encoder".into())
            .spawn(move || {
                let result = encoder_thread(
                    (config, scale, reserved),
                    video_rx,
                    audio_rx,
                    elapsed_clone,
                    stop_requested_for_thread,
                    finished_for_thread,
                    queue_depth_for_thread,
                );
                if let Err(ref error) = result {
                    tracing::error!(
                        error = %error,
                        "CAPTURE ENCODER: encoder failed; the recording has ended"
                    );
                }
                result
            })
            .map_err(|e| CaptureError::Io(std::io::Error::other(e)))?;

        Ok(Self {
            video_tx: Some(video_tx),
            sample_rate,
            audio_tx: Some(audio_tx),
            thread: Some(thread),
            elapsed_ns,
            video_capacity,
            stop_handle: CaptureStopHandle {
                stop_requested,
                finished,
            },
            queue_depth,
            encoder_loss_reported: AtomicBool::new(false),
        })
    }

    /// Submit a complete emulation slice without waiting. All frame boundaries
    /// must be included before its audio is submitted. Frames are ordered by
    /// timestamp; PCM starts at `timestamp_ns`, with its last sample at or
    /// before `end_ns`, within one source frame for rounded sample deadlines.
    /// The caller must include every completed frame through that boundary.
    /// Samples before a frame boundary belong to the preceding segment; samples
    /// at the boundary belong to the new segment. Chunks spanning it are split
    /// at the source rate. Use this API exclusively for a session: the legacy
    /// frame/audio APIs are intended for single-resolution bulk callers.
    ///
    /// At most the session frame budget (four standard, 64 native) and five
    /// seconds of stereo source PCM are accepted
    /// per slice. A full queue or ended encoder returns false before copying.
    /// Invalid or oversized slices also return false; the live caller must stop.
    pub fn try_submit_slice(
        &self,
        frames: &[CaptureFrame<'_>],
        samples: &[f32],
        timestamp_ns: u64,
        end_ns: u64,
    ) -> bool {
        if frames.len() > self.video_capacity
            || samples.len() > self.sample_rate as usize * 2 * 5
            || !samples.len().is_multiple_of(2)
            || frames
                .windows(2)
                .any(|pair| pair[0].timestamp_ns > pair[1].timestamp_ns)
            || frames.iter().any(|frame| frame.timestamp_ns > end_ns)
            || (!samples.is_empty()
                && (timestamp_ns > end_ns
                    || (samples.len() as u128 / 2 - 1) * 1_000_000_000
                        > u128::from(end_ns - timestamp_ns) * u128::from(self.sample_rate)
                            + 1_000_000_000))
            || self.session_over()
        {
            return false;
        }
        // Even an empty slice advances the final displayed frame's endpoint.
        let video_slots = frames.len();
        let audio_slots = usize::from(!samples.is_empty());
        if !claim_queue_slots(&self.queue_depth.video, self.video_capacity, video_slots) {
            return false;
        }
        if !claim_queue_slots(&self.queue_depth.audio, AUDIO_QUEUE_CAPACITY, audio_slots) {
            self.queue_depth
                .release_many(&self.queue_depth.video, video_slots);
            return false;
        }
        let events = slice_events(frames, samples, timestamp_ns, self.sample_rate);
        if self.video_tx.as_ref().is_some_and(|tx| {
            tx.try_send(EncoderMessage::Slice(QueuedSlice {
                events,
                end_ns,
                video_slots,
                audio_slots,
            }))
            .is_ok()
        }) {
            true
        } else {
            self.queue_depth
                .release_many(&self.queue_depth.video, video_slots);
            self.queue_depth
                .release_many(&self.queue_depth.audio, audio_slots);
            false
        }
    }

    /// Get a handle that can stop the encoder without owning this session.
    pub fn stop_handle(&self) -> CaptureStopHandle {
        self.stop_handle.clone()
    }

    /// Submit a video frame for encoding.
    ///
    /// `pixels` is a slice of 0RGB u32 values (`0x00RRGGBB`).
    ///
    /// When the encoder is behind, this waits for it rather than dropping the
    /// frame (MDD-BUG-FLU-02234). It returns without submitting only once the
    /// recording is over: stopped, or its encoder gone (reported once).
    pub fn submit_frame(&self, pixels: &[u32], width: u32, height: u32, timestamp_ns: u64) {
        if self.session_over() {
            return;
        }
        let Some(ref tx) = self.video_tx else {
            return;
        };
        // Wait for the slot first: past this point the frame gets copied, and
        // a submission that gives up should not cost a copy (MDD-BUG-CRU-00816).
        if !self.queue_depth.blocking_claim_queue_slot(
            &self.queue_depth.video,
            self.video_capacity,
            || self.session_over(),
        ) {
            return;
        }
        if tx
            .try_send(EncoderMessage::Video(VideoMessage {
                pixels: pixels.to_vec(),
                width,
                height,
                timestamp_ns,
            }))
            .is_err()
        {
            // The encoder is gone. Hand the slot back rather than leaving the
            // queue looking permanently full.
            release_queue_slot(&self.queue_depth.video);
            self.report_encoder_loss();
        }
    }

    /// Submit audio samples for encoding.
    ///
    /// `samples` is interleaved stereo f32 data at the configured sample rate.
    ///
    /// Waits for the encoder when the queue is full, as
    /// [`submit_frame`](Self::submit_frame) does (MDD-BUG-FLU-02234).
    pub fn submit_audio(&self, samples: &[f32], timestamp_ns: u64) {
        if self.session_over() {
            return;
        }
        let Some(ref tx) = self.audio_tx else {
            return;
        };
        if !self.queue_depth.blocking_claim_queue_slot(
            &self.queue_depth.audio,
            AUDIO_QUEUE_CAPACITY,
            || self.session_over(),
        ) {
            return;
        }
        if tx
            .try_send(AudioMessage {
                samples: samples.to_vec(),
                timestamp_ns,
            })
            .is_err()
        {
            release_queue_slot(&self.queue_depth.audio);
            self.report_encoder_loss();
        }
    }

    /// Nonblocking submission. A full queue is an explicit failure; callers can
    /// stop the recording cleanly rather than omit frames from a live session.
    pub fn try_submit_frame(
        &self,
        pixels: &[u32],
        width: u32,
        height: u32,
        timestamp_ns: u64,
    ) -> bool {
        if self.session_over() || !claim_queue_slot(&self.queue_depth.video, self.video_capacity) {
            return false;
        }
        if self.video_tx.as_ref().is_some_and(|tx| {
            tx.try_send(EncoderMessage::Video(VideoMessage {
                pixels: pixels.to_vec(),
                width,
                height,
                timestamp_ns,
            }))
            .is_ok()
        }) {
            true
        } else {
            release_queue_slot(&self.queue_depth.video);
            false
        }
    }

    /// Nonblocking counterpart of `submit_audio`, with explicit backpressure.
    pub fn try_submit_audio(&self, samples: &[f32], timestamp_ns: u64) -> bool {
        if self.session_over() || !claim_queue_slot(&self.queue_depth.audio, AUDIO_QUEUE_CAPACITY) {
            return false;
        }
        if self.audio_tx.as_ref().is_some_and(|tx| {
            tx.try_send(AudioMessage {
                samples: samples.to_vec(),
                timestamp_ns,
            })
            .is_ok()
        }) {
            true
        } else {
            release_queue_slot(&self.queue_depth.audio);
            false
        }
    }

    /// Whether nothing more can be recorded: the session was asked to stop,
    /// or the encoder thread has left. A submitter waiting for a queue slot
    /// gives up on either, so neither a stop nor a dead encoder can hang it.
    fn session_over(&self) -> bool {
        if self.stop_handle.stop_requested.load(Ordering::Acquire) {
            return true;
        }
        if self.stop_handle.finished.load(Ordering::Acquire) {
            self.report_encoder_loss();
            return true;
        }
        false
    }

    /// Say once that the encoder ended before the recording was stopped. The
    /// encoder thread logs why; this says that capture has stopped with it.
    fn report_encoder_loss(&self) {
        if self.stop_handle.stop_requested.load(Ordering::Acquire) {
            return;
        }
        if !self.encoder_loss_reported.swap(true, Ordering::AcqRel) {
            tracing::error!(
                "CAPTURE: the encoder stopped before the recording did; nothing \
                 after this point is recorded"
            );
        }
    }

    /// Stop the capture session and finalise the MKV file.
    ///
    /// Returns every file this session wrote, in the order it wrote them, and
    /// never an empty list on success. A resolution change starts a new file,
    /// and the configured path is not always among them: a first frame whose
    /// size already differs from the configured one rolls over before anything
    /// is created, so the only file is the suffixed one. Returning the
    /// configured path regardless named a file that need not exist and hid
    /// every later one (MDD-BUG-CRU-00814).
    pub fn stop(&mut self) -> Result<Vec<PathBuf>, CaptureError> {
        self.stop_handle.request_stop();
        // Drop senders to signal the encoder thread to exit.
        self.video_tx.take();
        self.audio_tx.take();

        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| CaptureError::Encoder("Encoder thread panicked".into()))?
        } else {
            Err(CaptureError::Encoder("Already stopped".into()))
        }
    }

    /// Elapsed time since the capture started.
    pub fn elapsed(&self) -> Duration {
        Duration::from_nanos(self.elapsed_ns.load(Ordering::Relaxed))
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.stop_handle.request_stop();
        // Ensure senders are dropped so the thread can exit.
        self.video_tx.take();
        self.audio_tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Encoder thread
// ---------------------------------------------------------------------------

/// Nominal frame rate handed to the VP9 rate controller.
///
/// Capture is inherently variable-rate — frames arrive when the emulator
/// renders them, stamped in guest time — so there is no true frame rate to
/// declare. The rate controller only uses this to divide the
/// target bitrate into a per-frame bit budget, so a fixed nominal value gives
/// every frame the same budget and therefore a stable quality, whatever rate
/// the session actually runs at.
const NOMINAL_FRAMERATE: f64 = 30.0;

/// Target bitrate. Matches what the H.264 path asked for before the VP9
/// switch; at 30 fps nominal that is ~533 kbit per frame, which is generous
/// for the flat, hard-edged content a DOS-era capture produces.
const TARGET_BITRATE_BPS: u32 = 16_000_000;

/// How much pre-video audio to hold while waiting for the first video frame.
///
/// Audio is resampled and Opus-encoded from the moment capture starts, but
/// nothing can be written until the first video frame establishes the muxer's
/// geometry. A source that is slow to produce a frame — or never produces one —
/// would otherwise grow the pending vector for the life of the session
/// (MDD-BUG-CRU-00818). Five seconds is far longer than any real emulator takes
/// to present a frame, and bounds the buffer at ~250 packets (~80 KB at the
/// 128 kbit/s the Opus encoder is configured for).
const MAX_PENDING_AUDIO_MS: u64 = 5_000;

/// Each Opus frame is 20 ms, so the duration bound is also a packet count.
const MAX_PENDING_OPUS_PACKETS: usize = (MAX_PENDING_AUDIO_MS / 20) as usize;

/// Append a pre-video Opus packet, dropping the oldest past the bound.
///
/// Returns `true` if this call dropped anything, so the caller can warn once
/// rather than every 20 ms.
///
/// The oldest go, not the newest: packets carry absolute timestamps, so the
/// track simply starts later, and the audio worth keeping is whatever is
/// adjacent to the video that is about to start rather than the pre-roll
/// before it.
fn push_pending_opus_packet(pending: &mut Vec<(Vec<u8>, u64)>, packet: (Vec<u8>, u64)) -> bool {
    pending.push(packet);
    let Some(excess) = pending.len().checked_sub(MAX_PENDING_OPUS_PACKETS) else {
        return false;
    };
    if excess == 0 {
        return false;
    }
    pending.drain(..excess);
    true
}

/// Keyframe interval, in frames.
///
/// rvp9 has no force-a-keyframe-now entry point, so the interval is a frame
/// count rather than the previous path's 2-second wall-clock rule. 60 frames
/// is 2 s at [`NOMINAL_FRAMERATE`]; a session running faster or slower than
/// nominal gets proportionally closer or wider keyframes, which affects only
/// seek granularity.
const GOP_LENGTH: u32 = 60;

/// Round a coded dimension up to what VP9 needs.
///
/// The encoder codes whole 64-pixel superblock columns, so the width must be a
/// multiple of 64; 4:2:0 chroma needs an even height. The muxer crops the
/// difference back off for display, so this is invisible in the output file.
fn coded_dimensions(width: u32, height: u32) -> (u32, u32) {
    (width.div_ceil(64) * 64, (height + 1) & !1)
}

/// Create a VP9 encoder for the given coded dimensions.
fn create_vp9_encoder(coded_width: u32, coded_height: u32) -> Result<StreamEncoder, CaptureError> {
    let mut config = EncoderConfig::new(coded_width, coded_height);
    config.framerate = NOMINAL_FRAMERATE;
    config.target_bitrate_bps = TARGET_BITRATE_BPS;
    config.gop_length = GOP_LENGTH;
    config.rc_mode = RcMode::Cbr;

    // Tile columns are the encoder's parallelism unit, and VP9 requires each
    // to span at least 256 luma pixels — so the request is clamped down for
    // narrow frames and this is a ceiling, not a promise. Capture competes with
    // the emulator for cores, so ask for four rather than the machine's width.
    let parallelism = EncoderParallelism {
        max_threads: 4,
        requested_log2_tile_cols: 2,
    };

    StreamEncoder::new_with_parallelism(config, parallelism)
        .map_err(|e| CaptureError::Encoder(format!("VP9 init: {e:?}")))
}

fn buffered_audio_ms(samples: usize, channels: u16) -> Result<u64, CaptureError> {
    let samples = u64::try_from(samples).map_err(|_| {
        CaptureError::Encoder("buffered audio length does not fit in the timeline".into())
    })?;
    samples
        .checked_mul(1_000)
        .and_then(|value| value.checked_div(u64::from(OPUS_SAMPLE_RATE_HZ)))
        .and_then(|value| value.checked_div(u64::from(channels)))
        .ok_or_else(|| CaptureError::Encoder("buffered audio timeline overflow".into()))
}

fn silence_samples_for_gap(gap_ms: u64, channels: u16) -> Result<usize, CaptureError> {
    let frames = gap_ms
        .checked_mul(u64::from(OPUS_SAMPLE_RATE_HZ))
        .and_then(|value| value.checked_div(1_000))
        .ok_or_else(|| CaptureError::Encoder("audio timestamp gap arithmetic overflow".into()))?;
    let samples = frames
        .checked_mul(u64::from(channels))
        .ok_or_else(|| CaptureError::Encoder("audio timestamp gap sample count overflow".into()))?;
    usize::try_from(samples)
        .map_err(|_| CaptureError::Encoder("audio timestamp gap is too large to represent".into()))
}

fn encode_opus_frame(
    frame: &[f32],
    opus_encoder: &mut ropus::Encoder,
    opus_output_buf: &mut [u8],
    muxer: &mut Option<MkvMuxer<BufWriter<File>>>,
    pending_opus_packets: &mut Vec<(Vec<u8>, u64)>,
    pending_audio_overflowed: &mut bool,
    opus_wall_clock_ms: &mut Option<u64>,
) -> Result<(), CaptureError> {
    let timestamp = opus_wall_clock_ms
        .ok_or_else(|| CaptureError::Encoder("audio frame has no timeline anchor".into()))?;
    let next_timestamp = timestamp
        .checked_add(OPUS_FRAME_DURATION_MS)
        .ok_or_else(|| CaptureError::Encoder("audio timeline overflow".into()))?;
    let encoded_len = opus_encoder
        .encode_float(frame, opus_output_buf)
        .map_err(|e| CaptureError::Encoder(format!("Opus encode: {e}")))?;

    let pkt = opus_output_buf[..encoded_len].to_vec();
    if let Some(ref mut m) = muxer {
        m.write_audio_packet(&pkt, timestamp)
            .map_err(CaptureError::Io)?;
    } else {
        let dropped = push_pending_opus_packet(pending_opus_packets, (pkt, timestamp));
        if dropped && !*pending_audio_overflowed {
            *pending_audio_overflowed = true;
            tracing::warn!(
                target: "gp64::capture",
                "no video frame after {} ms of audio; dropping the oldest \
                 buffered Opus packets to stay bounded",
                MAX_PENDING_AUDIO_MS
            );
        }
    }
    *opus_wall_clock_ms = Some(next_timestamp);
    Ok(())
}

fn drain_opus_frames(
    opus_buffer: &mut Vec<f32>,
    frame_samples: usize,
    frame: &mut Vec<f32>,
    opus_encoder: &mut ropus::Encoder,
    opus_output_buf: &mut [u8],
    muxer: &mut Option<MkvMuxer<BufWriter<File>>>,
    pending_opus_packets: &mut Vec<(Vec<u8>, u64)>,
    pending_audio_overflowed: &mut bool,
    opus_wall_clock_ms: &mut Option<u64>,
) -> Result<(), CaptureError> {
    while opus_buffer.len() >= frame_samples {
        frame.clear();
        frame.extend(opus_buffer.drain(..frame_samples));
        encode_opus_frame(
            frame,
            opus_encoder,
            opus_output_buf,
            muxer,
            pending_opus_packets,
            pending_audio_overflowed,
            opus_wall_clock_ms,
        )?;
    }
    Ok(())
}

/// Flush the real tail and the encoder lookahead, then trim packet rounding.
/// Start pre-skip removes the same lookahead once, leaving all real PCM.
fn flush_final_opus_frame(
    opus_buffer: &mut Vec<f32>,
    frame_samples: usize,
    channels: u16,
    has_audio: bool,
    opus_encoder: &mut ropus::Encoder,
    opus_output_buf: &mut [u8],
    muxer: &mut Option<MkvMuxer<BufWriter<File>>>,
    opus_wall_clock_ms: &mut Option<u64>,
) -> Result<(), CaptureError> {
    if !has_audio {
        return Ok(());
    }
    // No file to write to: a rollover with no frame after it finalised the
    // last segment already, so there is nothing this tail can join.
    let Some(m) = muxer.as_mut() else {
        opus_buffer.clear();
        return Ok(());
    };
    let mut timestamp = opus_wall_clock_ms
        .ok_or_else(|| CaptureError::Encoder("audio tail has no timeline anchor".into()))?;
    if opus_buffer.len() > frame_samples {
        return Err(CaptureError::Encoder(
            "audio tail is longer than one Opus frame".into(),
        ));
    }
    let target_samples =
        opus_buffer.len() + opus_encoder.lookahead() as usize * usize::from(channels);
    let encoded_samples = target_samples.div_ceil(frame_samples) * frame_samples;
    let padding_frames = (encoded_samples - target_samples) / usize::from(channels);
    let padding_ns = (padding_frames as u64)
        .checked_mul(1_000_000_000)
        .map(|scaled| scaled / u64::from(OPUS_SAMPLE_RATE_HZ))
        .ok_or_else(|| CaptureError::Encoder("audio tail padding overflow".into()))?;
    opus_buffer.resize(encoded_samples, 0.0);
    for (index, frame) in opus_buffer.chunks_exact(frame_samples).enumerate() {
        let next_timestamp = timestamp
            .checked_add(OPUS_FRAME_DURATION_MS)
            .ok_or_else(|| CaptureError::Encoder("audio timeline overflow".into()))?;
        let encoded_len = opus_encoder
            .encode_float(frame, opus_output_buf)
            .map_err(|e| CaptureError::Encoder(format!("Opus encode of the audio tail: {e}")))?;
        let packet = &opus_output_buf[..encoded_len];
        if (index + 1) * frame_samples == encoded_samples {
            m.write_audio_packet_with_discard_padding(packet, timestamp, padding_ns)
        } else {
            m.write_audio_packet(packet, timestamp)
        }
        .map_err(CaptureError::Io)?;
        timestamp = next_timestamp;
    }
    opus_buffer.clear();
    *opus_wall_clock_ms = Some(timestamp);
    Ok(())
}

fn append_silence_samples(
    samples: usize,
    opus_buffer: &mut Vec<f32>,
    frame_samples: usize,
    frame: &mut Vec<f32>,
    opus_encoder: &mut ropus::Encoder,
    opus_output_buf: &mut [u8],
    muxer: &mut Option<MkvMuxer<BufWriter<File>>>,
    pending_opus_packets: &mut Vec<(Vec<u8>, u64)>,
    pending_audio_overflowed: &mut bool,
    opus_wall_clock_ms: &mut Option<u64>,
) -> Result<(), CaptureError> {
    let mut remaining = samples;
    while remaining > 0 {
        if opus_buffer.len() >= frame_samples {
            drain_opus_frames(
                opus_buffer,
                frame_samples,
                frame,
                opus_encoder,
                opus_output_buf,
                muxer,
                pending_opus_packets,
                pending_audio_overflowed,
                opus_wall_clock_ms,
            )?;
        }
        if opus_buffer.len() >= frame_samples {
            return Err(CaptureError::Encoder(
                "Opus buffer exceeded one frame before silence fill".into(),
            ));
        }
        let room = frame_samples - opus_buffer.len();
        let chunk = remaining.min(room);
        opus_buffer.extend(std::iter::repeat_n(0.0, chunk));
        remaining -= chunk;
        drain_opus_frames(
            opus_buffer,
            frame_samples,
            frame,
            opus_encoder,
            opus_output_buf,
            muxer,
            pending_opus_packets,
            pending_audio_overflowed,
            opus_wall_clock_ms,
        )?;
    }
    Ok(())
}

fn encoder_thread(
    (config, scale_factor, mut reserved): (
        CaptureConfig,
        fn(u32) -> u32,
        Option<ReservedCaptureOutput>,
    ),
    video_rx: Receiver<EncoderMessage>,
    audio_rx: Receiver<AudioMessage>,
    elapsed_ns: Arc<AtomicU64>,
    stop_requested: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    queue_depth: QueueDepth,
) -> Result<Vec<PathBuf>, CaptureError> {
    let _completion_guard = CaptureCompletionGuard {
        finished,
        queue_depth: queue_depth.clone(),
    };

    // Initialize the VP9 encoder at the (possibly upscaled) encode dimensions.
    let initial_scale = scale_factor(config.width);
    let (mut coded_width, mut coded_height) =
        coded_dimensions(config.width * initial_scale, config.height * initial_scale);
    let mut vp9_encoder = create_vp9_encoder(coded_width, coded_height)?;

    // Initialize Opus encoder (48000 Hz, stereo, audio application)
    let mut opus_encoder = ropus::Encoder::builder(
        OPUS_SAMPLE_RATE_HZ,
        ropus::Channels::Stereo,
        ropus::Application::Audio,
    )
    .bitrate(ropus::Bitrate::Bits(128000))
    .build()
    .map_err(|e| CaptureError::Encoder(format!("Opus init: {e}")))?;

    if opus_encoder.lookahead() != u32::from(crate::muxer::OPUS_PRE_SKIP_SAMPLES) {
        return Err(CaptureError::Encoder(
            "Opus encoder lookahead disagrees with declared pre-skip and CodecDelay".into(),
        ));
    }

    // Initialize resampler (44100 -> 48000)
    let mut resampler = LinearResampler::new(config.sample_rate, OPUS_SAMPLE_RATE_HZ);
    let mut opus_buffer: Vec<f32> = Vec::new();
    let mut has_opus_audio = false;
    let mut opus_output_buf = vec![0u8; 4000];
    let frame_samples = OPUS_FRAME_SAMPLES_PER_CHANNEL
        .checked_mul(usize::from(config.channels))
        .ok_or_else(|| CaptureError::Encoder("Opus frame sample count overflow".into()))?;
    let mut opus_frame = Vec::with_capacity(frame_samples);

    // MKV muxer (deferred until the first frame establishes its geometry)
    let mut muxer: Option<MkvMuxer<BufWriter<File>>> = None;
    let mut current_width = config.width;
    let mut current_height = config.height;
    let mut planes = I420Planes::default();
    let mut upscale_buf: Vec<u32> = Vec::new();
    let mut file_counter = 1u32;
    let base_path = config.output_path.clone();
    let exclusive = reserved.is_some();
    let excluded = reserved.as_ref().and_then(|output| output.excluded.clone());
    let mut opus_wall_clock_ms: Option<u64> = None;
    let mut pending_opus_packets: Vec<(Vec<u8>, u64)> = Vec::new();
    // Warn once, not every 20 ms, when the pre-video audio bound starts biting.
    let mut pending_audio_overflowed = false;
    // Every file created, in order. A resolution rollover leaves `muxer` at
    // `None` while earlier files exist, so "muxer is None at the end" is not
    // the same question as "did anything get written" (MDD-BUG-CRU-00818) -
    // and the caller needs the names anyway (MDD-BUG-CRU-00814).
    let mut segments: Vec<PathBuf> = Vec::new();

    let start = Instant::now();

    let mut slice: Option<QueuedSlice> = None;
    loop {
        if slice.as_ref().is_some_and(|slice| slice.events.is_empty()) {
            let completed = slice.take().expect("completed slice exists");
            if let Some(m) = &mut muxer {
                m.record_presentation_end_ns(completed.end_ns);
            }
            queue_depth.release_many(&queue_depth.video, completed.video_slots);
            queue_depth.release_many(&queue_depth.audio, completed.audio_slots);
        }
        let mut receiving_slice = slice.is_some();
        let mut slice_audio = None;
        let mut next_frame = if receiving_slice {
            Ok(None)
        } else {
            match video_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(EncoderMessage::Video(frame)) => Ok(Some(frame)),
                Ok(EncoderMessage::Slice(events)) => {
                    slice = Some(events);
                    receiving_slice = true;
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        };
        if let Some(events) = &mut slice {
            match events.events.pop_front() {
                Some(SliceEvent::Video(frame)) => next_frame = Ok(Some(frame)),
                Some(SliceEvent::Audio(audio)) => slice_audio = Some(audio),
                None => {}
            }
        }
        let should_finish = match next_frame {
            Ok(Some(msg)) => {
                if !receiving_slice {
                    queue_depth.release(&queue_depth.video);
                }
                elapsed_ns.store(start.elapsed().as_nanos() as u64, Ordering::Relaxed);

                // Resolution change detection
                if msg.width != current_width || msg.height != current_height {
                    if let Some(m) = &mut muxer {
                        m.record_presentation_end_ns(msg.timestamp_ns);
                    }
                    flush_final_opus_frame(
                        &mut opus_buffer,
                        frame_samples,
                        config.channels,
                        has_opus_audio,
                        &mut opus_encoder,
                        &mut opus_output_buf,
                        &mut muxer,
                        &mut opus_wall_clock_ms,
                    )?;
                    if let Some(m) = muxer.take() {
                        // MDD-BUG-CRU-00813: finalisation writes the segment's
                        // Cues, Duration and SeekHead. Discarding this left a
                        // damaged earlier segment behind a healthy final one,
                        // and still reported success.
                        m.finalise()
                            .map_err(CaptureError::Io)?
                            .flush()
                            .map_err(CaptureError::Io)?;
                    }
                    current_width = msg.width;
                    current_height = msg.height;
                    // The reserved base belongs to the first written segment,
                    // even if its first frame differs from configured geometry.
                    if !exclusive || !segments.is_empty() {
                        file_counter += 1;
                    }

                    // Recreate the VP9 encoder at the new upscaled dimensions.
                    let scale = scale_factor(current_width);
                    (coded_width, coded_height) =
                        coded_dimensions(current_width * scale, current_height * scale);
                    vp9_encoder = create_vp9_encoder(coded_width, coded_height)?;

                    opus_encoder = ropus::Encoder::builder(
                        OPUS_SAMPLE_RATE_HZ,
                        ropus::Channels::Stereo,
                        ropus::Application::Audio,
                    )
                    .bitrate(ropus::Bitrate::Bits(128000))
                    .build()
                    .map_err(|e| CaptureError::Encoder(format!("Opus rollover init: {e}")))?;
                    resampler = LinearResampler::new(config.sample_rate, OPUS_SAMPLE_RATE_HZ);
                    // Reset audio state for the new file
                    opus_wall_clock_ms = None;
                    has_opus_audio = false;
                    opus_buffer.clear();
                    pending_opus_packets.clear();
                }

                // Nearest-neighbour upscale small sources before encoding.
                let scale = scale_factor(current_width);
                let (source_pixels, frame_w, frame_h): (&[u32], u32, u32) = if scale > 1 {
                    let (w, h) = nearest_upscale_0rgb_into(
                        &msg.pixels,
                        current_width,
                        current_height,
                        scale,
                        &mut upscale_buf,
                    );
                    (&upscale_buf, w, h)
                } else {
                    (&msg.pixels[..], current_width, current_height)
                };

                // Convert 0RGB straight to the I420 planes the encoder reads,
                // padded out to the coded superblock grid.
                convert_0rgb_to_i420_into(
                    source_pixels,
                    frame_w,
                    frame_h,
                    coded_width,
                    coded_height,
                    &mut planes,
                );

                let yuv = YuvFrameRef {
                    y: &planes.y,
                    u: &planes.u,
                    v: &planes.v,
                    y_stride: coded_width as usize,
                    u_stride: coded_width as usize / 2,
                    v_stride: coded_width as usize / 2,
                };
                let encoded = vp9_encoder
                    .encode_frame(&yuv)
                    .map_err(|e| CaptureError::Encoder(format!("VP9 encode: {e:?}")))?;

                if !encoded.data.is_empty() {
                    let is_keyframe = encoded.frame_kind.is_key();
                    let timestamp_ms = msg.timestamp_ns / 1_000_000;

                    // VP9 carries every stream parameter in the frame itself, so
                    // unlike the H.264 path there is nothing to wait for: the
                    // muxer opens on the first frame, which is always a keyframe.
                    if muxer.is_none() {
                        let opus_head = build_opus_head(
                            config.channels as u8,
                            crate::muxer::OPUS_PRE_SKIP_SAMPLES,
                            OPUS_SAMPLE_RATE_HZ,
                        );

                        let path = if file_counter == 1 {
                            base_path.clone()
                        } else {
                            let stem = base_path.file_stem().unwrap_or_default().to_string_lossy();
                            let ext = base_path.extension().unwrap_or_default().to_string_lossy();
                            base_path.with_file_name(format!("{stem}-{file_counter}.{ext}"))
                        };

                        let (path, file) = if let Some(output) = reserved.take() {
                            (output.path, output.file)
                        } else if exclusive {
                            let output = ReservedCaptureOutput::reserve(&path, excluded.as_deref())
                                .map_err(CaptureError::Io)?;
                            (output.path, output.file)
                        } else {
                            let file = File::create(&path).map_err(CaptureError::Io)?;
                            (path, file)
                        };
                        segments.push(path.clone());
                        let writer = BufWriter::new(file);

                        let mut m = MkvMuxer::new(
                            writer,
                            VideoGeometry {
                                coded_width,
                                coded_height,
                                display_width: frame_w,
                                display_height: frame_h,
                            },
                            &opus_head,
                            f64::from(OPUS_SAMPLE_RATE_HZ),
                            config.channels as u8,
                        )
                        .map_err(CaptureError::Io)?;

                        // Flush any Opus packets buffered before the muxer existed
                        for (pkt, ts) in pending_opus_packets.drain(..) {
                            m.write_audio_packet(&pkt, ts).map_err(CaptureError::Io)?;
                        }

                        muxer = Some(m);
                    }

                    if let Some(ref mut m) = muxer {
                        m.write_video_frame(&encoded.data, timestamp_ms, is_keyframe)
                            .map_err(CaptureError::Io)?;
                    }
                }
                false
            }
            Ok(None) => false,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                elapsed_ns.store(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
                stop_requested.load(Ordering::Acquire)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => true,
        };

        // Drain audio messages.
        //
        // Timestamps are interpreted as positions on the audio/video timeline
        // (emulator-time milliseconds since capture started, driven by the
        // caller). Gaps in the incoming timestamp stream are filled with
        // silence so the audio track keeps its length in lock-step with
        // whatever the caller's clock reports. The inverse case — the buffer
        // outrunning the timestamp stream — is never reconciled by dropping
        // samples: with an emulator-time clock the audio timeline advances at
        // the same rate as the content, so there is nothing to drop, and the
        // previous drop-on-overshoot behaviour permanently lost emulator audio
        // during tick-scheduler catch-up bursts.
        loop {
            let audio_msg = if receiving_slice {
                let Some(audio) = slice_audio.take() else {
                    break;
                };
                audio
            } else {
                let Ok(audio) = audio_rx.try_recv() else {
                    break;
                };
                queue_depth.release(&queue_depth.audio);
                audio
            };
            let msg_ts_ms = audio_msg.timestamp_ns / 1_000_000;
            let anchor_ms = *opus_wall_clock_ms.get_or_insert(msg_ts_ms);

            // `audio_end_ms` is the timestamp at which the newest queued
            // sample would play. If the incoming message sits noticeably
            // later on the timeline, fill the gap with silence so the track
            // retains its length relative to video.
            let opus_buffer_ms = buffered_audio_ms(opus_buffer.len(), config.channels)?;
            let audio_end_ms = anchor_ms.checked_add(opus_buffer_ms).ok_or_else(|| {
                CaptureError::Encoder("audio timestamp overflow while finding buffered end".into())
            })?;
            let tolerance_end_ms = audio_end_ms
                .checked_add(SILENCE_FILL_TOLERANCE_MS)
                .ok_or_else(|| {
                    CaptureError::Encoder(
                        "audio timestamp overflow while applying fill tolerance".into(),
                    )
                })?;
            if msg_ts_ms > tolerance_end_ms {
                let gap_ms = msg_ts_ms.checked_sub(audio_end_ms).ok_or_else(|| {
                    CaptureError::Encoder(
                        "audio timestamp subtraction overflow while filling gap".into(),
                    )
                })?;
                if gap_ms > MAX_SILENCE_FILL_GAP_MS {
                    return Err(CaptureError::Encoder(format!(
                        "audio timestamp gap of {gap_ms} ms exceeds maximum fillable gap of \
                         {MAX_SILENCE_FILL_GAP_MS} ms"
                    )));
                }
                let pad_samples = silence_samples_for_gap(gap_ms, config.channels)?;
                has_opus_audio |= pad_samples > 0;
                append_silence_samples(
                    pad_samples,
                    &mut opus_buffer,
                    frame_samples,
                    &mut opus_frame,
                    &mut opus_encoder,
                    &mut opus_output_buf,
                    &mut muxer,
                    &mut pending_opus_packets,
                    &mut pending_audio_overflowed,
                    &mut opus_wall_clock_ms,
                )?;
            }

            // Resample to 48000 Hz (always, even before muxer exists)
            let resampled = resampler.process(&audio_msg.samples);
            has_opus_audio |= !resampled.is_empty();
            opus_buffer.extend_from_slice(&resampled);

            // Encode complete Opus frames (960 samples per channel = 20ms at 48kHz)
            drain_opus_frames(
                &mut opus_buffer,
                frame_samples,
                &mut opus_frame,
                &mut opus_encoder,
                &mut opus_output_buf,
                &mut muxer,
                &mut pending_opus_packets,
                &mut pending_audio_overflowed,
                &mut opus_wall_clock_ms,
            )?;
        }

        if should_finish {
            break;
        }
    }

    // Flush encoder delay even when the last real PCM ended on a whole frame.
    flush_final_opus_frame(
        &mut opus_buffer,
        frame_samples,
        config.channels,
        has_opus_audio,
        &mut opus_encoder,
        &mut opus_output_buf,
        &mut muxer,
        &mut opus_wall_clock_ms,
    )?;

    // Finalise
    if let Some(m) = muxer {
        m.finalise()
            .map_err(CaptureError::Io)?
            .flush()
            .map_err(CaptureError::Io)?;
    }

    if segments.is_empty() {
        // No video frame ever arrived, so no file was created. Returning
        // `base_path` here handed the caller a successful-looking path to a
        // file that does not exist (MDD-BUG-CRU-00818).
        return Err(CaptureError::NoVideo);
    }

    Ok(segments)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_covers_silent_slice_end_and_resolution_boundary() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../.scratch/duration_extent_{nonce}.mkv"));
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        let (vtx, vrx) = mpsc::sync_channel(4);
        let (atx, arx) = mpsc::sync_channel(4);
        let depth = QueueDepth::new();
        let session = CaptureSession {
            video_tx: Some(vtx),
            audio_tx: None,
            sample_rate: 48_000,
            thread: None,
            elapsed_ns: Arc::new(AtomicU64::new(0)),
            video_capacity: 4,
            stop_handle: CaptureStopHandle {
                stop_requested: Arc::new(AtomicBool::new(false)),
                finished: Arc::new(AtomicBool::new(false)),
            },
            queue_depth: depth.clone(),
            encoder_loss_reported: AtomicBool::new(false),
        };
        for (width, timestamp_ns, end_ns) in
            [(64, 10_000_000, 30_000_000), (32, 60_000_000, 95_500_000)]
        {
            let pixels = vec![0; width as usize * 64];
            let frame = CaptureFrame {
                pixels: &pixels,
                width,
                height: 64,
                timestamp_ns,
            };
            assert!(session.try_submit_slice(&[frame], &[], 0, end_ns));
        }
        assert!(session.try_submit_slice(&[], &[], 95_500_000, 110_250_000));
        drop(session);
        drop(atx);
        let segments = encoder_thread(
            (
                CaptureConfig {
                    output_path: base,
                    width: 64,
                    height: 64,
                    sample_rate: 48_000,
                    channels: 2,
                },
                |_| 1,
                None,
            ),
            vrx,
            arx,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            depth,
        )
        .unwrap();
        let durations: Vec<_> = segments
            .iter()
            .map(|path| {
                let data = std::fs::read(path).unwrap();
                std::fs::remove_file(path).unwrap();
                let at = data
                    .windows(3)
                    .position(|w| w == [0x44, 0x89, 0x88])
                    .unwrap()
                    + 3;
                f64::from_be_bytes(data[at..at + 8].try_into().unwrap())
            })
            .collect();
        assert_eq!(durations, [60.0, 110.25], "Duration must cover rollover boundary and silent capture endpoint in unchanged segment coordinates");
    }

    fn vint(bytes: &[u8], at: usize) -> (usize, usize) {
        let len = bytes[at].leading_zeros() as usize + 1;
        let mut n = usize::from(bytes[at] & 0xffu8.checked_shr(len as u32).unwrap_or(0));
        for byte in &bytes[at + 1..at + len] {
            n = (n << 8) | usize::from(*byte);
        }
        (n, len)
    }
    fn packets(bytes: &[u8], out: &mut Vec<(u64, Vec<u8>)>, mut cluster_ms: u64) {
        let mut at = 0;
        while at < bytes.len() {
            let len = bytes[at].leading_zeros() as usize + 1;
            let mut id = 0u32;
            for byte in &bytes[at..at + len] {
                id = (id << 8) | u32::from(*byte);
            }
            let (size, n) = vint(bytes, at + len);
            let body = &bytes[at + len + n..at + len + n + size];
            match id {
                0x18538067 | 0x1f43b675 | 0xa0 => packets(body, out, cluster_ms),
                0xe7 => {
                    cluster_ms = body
                        .iter()
                        .fold(0, |value, byte| (value << 8) | u64::from(*byte));
                }
                0xa3 | 0xa1 if body[0] == 0x82 => {
                    let relative = i16::from_be_bytes([body[1], body[2]]);
                    out.push((
                        (cluster_ms as i64 + i64::from(relative)) as u64,
                        body[4..].to_vec(),
                    ));
                }
                _ => {}
            }
            at += len + n + size;
        }
    }
    #[test]
    fn stalled_coherent_queue_preserves_separate_frame_and_audio_budgets() {
        // Hold the receiver unread: a deterministic encoder stall, with no
        // scheduling assumptions and only one source frame per PCM message.
        fn stalled_session() -> (CaptureSession, Receiver<EncoderMessage>) {
            let (tx, rx) = mpsc::sync_channel(64 + AUDIO_QUEUE_CAPACITY);
            let session = CaptureSession {
                video_tx: Some(tx),
                audio_tx: None,
                sample_rate: 48_000,
                thread: None,
                elapsed_ns: Arc::new(AtomicU64::new(0)),
                video_capacity: 64,
                stop_handle: CaptureStopHandle {
                    stop_requested: Arc::new(AtomicBool::new(false)),
                    finished: Arc::new(AtomicBool::new(false)),
                },
                queue_depth: QueueDepth::new(),
                encoder_loss_reported: AtomicBool::new(false),
            };
            (session, rx)
        }
        let (session, _rx) = stalled_session();
        for _ in 0..AUDIO_QUEUE_CAPACITY {
            assert!(session.try_submit_slice(&[], &[0.0, 0.0], 0, 0));
        }
        assert!(!session.try_submit_slice(&[], &[0.0, 0.0], 0, 0));
        let frames: Vec<_> = (0..4)
            .map(|_| CaptureFrame {
                pixels: &[0],
                width: 1,
                height: 1,
                timestamp_ns: 0,
            })
            .collect();
        assert!(!session.try_submit_slice(&frames, &[0.0, 0.0], 0, 0));
        assert_eq!(
            session.queue_depth.video.load(Ordering::Relaxed),
            0,
            "failed audio claim must return frame credits"
        );
        for _ in 0..16 {
            assert!(session.try_submit_slice(&frames, &[], 0, 0));
        }
        assert!(!session.try_submit_slice(&frames, &[], 0, 0));
        assert!(
            session.try_submit_slice(&[], &[], 0, 0),
            "empty polls consume neither budget"
        );
        assert_eq!(session.queue_depth.video.load(Ordering::Relaxed), 64);
        assert_eq!(
            session.queue_depth.audio.load(Ordering::Relaxed),
            AUDIO_QUEUE_CAPACITY
        );
        let (session, _rx) = stalled_session();
        let frames: Vec<_> = (0..64)
            .map(|_| CaptureFrame {
                pixels: &[0],
                width: 1,
                height: 1,
                timestamp_ns: 0,
            })
            .collect();
        assert!(
            session.try_submit_slice(&frames, &[], 0, 0),
            "one native slice may use the complete 64-frame budget"
        );
        assert!(!session.try_submit_slice(&frames, &[], 0, 0));
        assert!(session.try_submit_slice(&[], &[0.0, 0.0], 0, 0));
    }

    #[test]
    fn coherent_slices_drain_audio_only_polls_before_stop() {
        let path = temp_capture_path("slice_stop");
        let mut session = CaptureSession::start_live(CaptureConfig {
            output_path: path.clone(),
            width: 64,
            height: 64,
            sample_rate: 48_000,
            channels: 2,
        })
        .unwrap();
        let pixels = vec![0; 64 * 64];
        let audio = vec![0.25; 960 * 2];
        let frame = |width, timestamp_ns| CaptureFrame {
            pixels: &pixels[..width as usize * 64],
            width,
            height: 64,
            timestamp_ns,
        };
        assert!(session.try_submit_slice(&[frame(64, 0)], &audio, 0, 20_000_000));
        assert!(session.try_submit_slice(&[], &audio, 20_000_000, 40_000_000));
        assert!(session.try_submit_slice(&[frame(32, 40_000_000)], &audio, 40_000_000, 60_000_000));
        let paths = session.stop().unwrap();
        assert_eq!(paths.len(), 2);
        let counts: Vec<_> = paths
            .iter()
            .map(|path| {
                let bytes = std::fs::read(path).unwrap();
                std::fs::remove_file(path).unwrap();
                // Final padded blocks are BlockGroups, not SimpleBlocks. The
                // unpadded packets prove both queued intervals reached the old
                // file and that the new interval survived immediate stop.
                let mut out = Vec::new();
                packets(&bytes, &mut out, 0);
                out.len()
            })
            .collect();
        assert_eq!(counts, vec![3, 2]);
        assert!(!session.try_submit_slice(&[], &audio, 60_000_000, 80_000_000));
    }

    // Fill both queues before the consumer runs: a deterministic encoder stall.
    #[test]
    fn queued_audio_is_split_across_resolution_boundaries() {
        for rate in [48_000u32, 44_100] {
            check_queued_audio_boundaries(rate);
        }
    }

    fn check_queued_audio_boundaries(rate: u32) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../.scratch/audio_boundaries_{nonce}.mkv"));
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        let (vtx, vrx) = mpsc::sync_channel::<EncoderMessage>(4);
        let (atx, arx) = mpsc::sync_channel(4);
        let depth = QueueDepth::new();
        // One source chunk spans the first boundary; the next starts exactly
        // at the second. Every segment has a distinct synthetic tone marker.
        let segment_frames = rate as usize * 60 / 1000;
        let boundaries = if rate == 44_100 {
            [60_011_337, 120_019_771]
        } else {
            [60_000_000, 120_000_000]
        };
        let tones: Vec<f32> = (0..segment_frames * 3)
            .flat_map(|i| {
                let time = i as u128 * 1_000_000_000 / u128::from(rate);
                let segment = boundaries.partition_point(|boundary| u128::from(*boundary) <= time);
                let hz = [400.0, 1200.0, 2400.0][segment];
                let value = (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.5;
                [value, value]
            })
            .collect();
        let pixels = [vec![0; 64 * 64], vec![0; 32 * 64], vec![0; 64 * 64]];
        let frames: Vec<_> = [(64, 0), (32, boundaries[0]), (64, boundaries[1])]
            .iter()
            .enumerate()
            .map(|(i, &(width, timestamp_ns))| CaptureFrame {
                pixels: &pixels[i],
                width,
                height: 64,
                timestamp_ns,
            })
            .collect();
        depth.video.fetch_add(3, Ordering::Relaxed);
        depth.audio.fetch_add(1, Ordering::Relaxed);
        vtx.send(EncoderMessage::Slice(QueuedSlice {
            events: slice_events(&frames, &tones, 0, rate),
            end_ns: 180_000_000,
            video_slots: 3,
            audio_slots: 1,
        }))
        .unwrap();
        drop(vtx);
        drop(atx);
        let segments = encoder_thread(
            (
                CaptureConfig {
                    output_path: base,
                    width: 64,
                    height: 64,
                    sample_rate: rate,
                    channels: 2,
                },
                |_| 1,
                None,
            ),
            vrx,
            arx,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            depth,
        )
        .unwrap();
        let encoded: Vec<_> = segments
            .iter()
            .map(|path| {
                let bytes = std::fs::read(path).unwrap();
                std::fs::remove_file(path).unwrap();
                let mut out = Vec::new();
                packets(&bytes, &mut out, 0);
                out
            })
            .collect();
        assert_eq!(
            encoded.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![4, 4, 4],
            "each 60 ms source interval belongs to its own segment"
        );
        assert_eq!(
            encoded
                .iter()
                .map(|segment| segment.iter().map(|packet| packet.0).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![
                vec![0, 20, 40, 60],
                vec![60, 80, 100, 120],
                vec![120, 140, 160, 180]
            ]
        );
        for (index, packets) in encoded.iter().enumerate() {
            let mut decoder = ropus::Decoder::new(48000, ropus::Channels::Stereo).unwrap();
            let mut pcm = Vec::new();
            let mut buf = vec![0i16; 5760 * 2];
            for (_, packet) in packets {
                let n = decoder
                    .decode(packet, &mut buf, ropus::DecodeMode::Normal)
                    .unwrap();
                pcm.extend_from_slice(&buf[..n * 2]);
            }
            // Use the interior, away from codec delay and partial-tail trimming.
            let power: Vec<_> = [400.0f64, 1200.0, 2400.0]
                .iter()
                .map(|hz| {
                    let (mut re, mut im) = (0.0, 0.0);
                    for (i, pair) in pcm
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .enumerate()
                        .skip(700)
                        .take(1400)
                    {
                        let angle = i as f64 * hz * std::f64::consts::TAU / 48000.0;
                        re += f64::from(pair[0]) * angle.cos();
                        im += f64::from(pair[0]) * angle.sin();
                    }
                    re * re + im * im
                })
                .collect();
            for other in 0..3 {
                if other != index {
                    assert!(
                        power[index] > 100.0 * power[other],
                        "wrong marker in segment {index}: {power:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn scale_factor_upscales_small_sources_near_to_1280() {
        assert_eq!(capture_scale_factor(320), 4); // 1280
        assert_eq!(capture_scale_factor(400), 4); // 1600
        assert_eq!(capture_scale_factor(256), 5); // 1280
        assert_eq!(capture_scale_factor(200), 7); // 1400
    }

    #[test]
    fn live_scale_factor_doubles_only_small_sources() {
        assert_eq!(live_scale_factor(320), 2); // 640
        assert_eq!(live_scale_factor(360), 2); // 720
        assert_eq!(live_scale_factor(640), 1);
        assert_eq!(live_scale_factor(720), 1);
        assert_eq!(live_scale_factor(0), 1);
    }

    #[test]
    fn scale_factor_leaves_large_sources_alone() {
        assert_eq!(capture_scale_factor(640), 1);
        assert_eq!(capture_scale_factor(800), 1);
        assert_eq!(capture_scale_factor(1280), 1);
        assert_eq!(capture_scale_factor(1920), 1);
    }

    #[test]
    fn scale_factor_handles_zero() {
        assert_eq!(capture_scale_factor(0), 1);
    }

    /// Encoding stays on the dedicated worker thread, keeping encoder work
    /// off the emulator thread.
    ///
    /// Both halves are pinned:
    /// - Runtime: `CaptureSession::start` spawns a distinct thread, and a real
    ///   frame round-trips through it into a finalized non-empty file.
    ///   Caller-side submission only copies buffers into channels.
    /// - Source: encoder-crate references stay in this file, with encoding
    ///   entry points reachable only from `encoder_thread`.
    #[test]
    fn encoding_is_confined_to_the_dedicated_encoder_thread() {
        // SOURCE half: the encoder crate must not leak into any other module,
        // where a caller-thread code path could reach it. The needle is the
        // UNDERSCORED crate path, which is the only spelling Rust code can use
        // — so prose mentioning "rvp9-encoder" doesn't trip the guard, but any
        // `use rvp9_encoder::…` or inline path does.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(&src).expect("crate src dir") {
            let path = entry.expect("src entry").path();
            if path.extension().is_none_or(|extension| extension != "rs")
                || path.file_name().is_some_and(|name| name == "encoder.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("crate source");
            assert!(
                !text.contains("rvp9_encoder"),
                "{} references rvp9_encoder — encoding must stay confined to \
                 encoder.rs's spawned thread",
                path.display()
            );
        }

        // RUNTIME half.
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "gp64_capture_containment_{}.mkv",
            std::process::id()
        ));
        let mut session = CaptureSession::start(CaptureConfig {
            output_path: path.clone(),
            width: 320,
            height: 200,
            sample_rate: 44100,
            channels: 2,
        })
        .expect("capture session starts");

        let caller = std::thread::current().id();
        let encoder = session
            .thread
            .as_ref()
            .expect("session holds the encoder JoinHandle")
            .thread();
        assert_ne!(
            encoder.id(),
            caller,
            "the encode path must run on its own thread, never the submitter's"
        );
        assert_eq!(
            encoder.name(),
            Some("capture-encoder"),
            "the dedicated encoder thread carries its pinned name"
        );

        let frame = vec![0x0000_3060u32; 320 * 200];
        session.submit_frame(&frame, 320, 200, 0);
        session.submit_frame(&frame, 320, 200, 33_000_000);
        let finished = only_segment(session.stop().expect("capture session finalises"));
        let bytes = std::fs::metadata(&finished)
            .expect("finalised capture file exists")
            .len();
        assert!(bytes > 0, "the encoder thread produced a non-empty file");
        let _ = std::fs::remove_file(&finished);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn session_start_and_stop() {
        let path = temp_capture_path("capture");

        let config = CaptureConfig {
            output_path: path.clone(),
            width: 320,
            height: 200,
            sample_rate: 44100,
            channels: 2,
        };

        let mut session = CaptureSession::start(config).expect("Failed to start capture");

        // Submit a few synthetic frames (solid color)
        let pixels = vec![0x00FF0000u32; 320 * 200]; // solid red
        for i in 0..5 {
            session.submit_frame(&pixels, 320, 200, i * 33_333_333); // ~30fps
        }

        // Submit some silence
        let silence = vec![0.0f32; 4410 * 2]; // 100ms of stereo silence
        session.submit_audio(&silence, 0);

        // Give the encoder thread a moment to process
        std::thread::sleep(Duration::from_millis(200));

        let result = session.stop();
        assert!(result.is_ok(), "Stop should succeed: {result:?}");

        // Clean up
        let _ = std::fs::remove_file(&path);
    }

    /// A unique temp path per test, so parallel runs cannot collide.
    fn temp_capture_path(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("gp64_test_capture_{tag}_{nonce}.mkv"))
    }

    /// A session that never receives a video frame must FAIL, not hand back a
    /// path to a file that was never created.
    ///
    /// The muxer is deferred until the first frame establishes its geometry, so
    /// an immediate stop — or an audio-only interval — left `muxer` at `None`
    /// and nothing on disk, while `stop()` still returned `Ok(base_path)`. The
    /// caller then reported "SAVED <path>" for a file that does not exist
    /// (`crates/mdem-host/src/window.rs`), and any later open of it fails with
    /// a confusing not-found (MDD-BUG-CRU-00818).
    #[test]
    fn a_session_with_no_video_frame_reports_failure_not_a_phantom_path() {
        let path = temp_capture_path("no_video");
        let config = CaptureConfig {
            output_path: path.clone(),
            width: 320,
            height: 200,
            sample_rate: 44100,
            channels: 2,
        };

        let mut session = CaptureSession::start(config).expect("capture starts");

        // Audio only — the case the record names. 100 ms of stereo silence.
        session.submit_audio(&vec![0.0f32; 4410 * 2], 0);
        std::thread::sleep(Duration::from_millis(200));

        let result = session.stop();
        assert!(
            matches!(result, Err(CaptureError::NoVideo)),
            "a capture with no video frame must report NoVideo, got {result:?}"
        );
        assert!(
            !path.exists(),
            "nothing should have been written to {}",
            path.display()
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The pre-video bound's exact contract, with no threads or wall clock in
    /// it: the backlog stops at the cap and what it keeps is the NEWEST
    /// packets (MDD-BUG-CRU-00818).
    #[test]
    fn the_pending_bound_keeps_the_newest_packets() {
        let mut pending: Vec<(Vec<u8>, u64)> = Vec::new();

        // Fill to exactly the cap: nothing is dropped, nothing is reordered.
        for i in 0..MAX_PENDING_OPUS_PACKETS {
            let dropped = push_pending_opus_packet(&mut pending, (vec![i as u8], i as u64));
            assert!(!dropped, "packet {i} filled the buffer but reported a drop");
        }
        assert_eq!(pending.len(), MAX_PENDING_OPUS_PACKETS);
        assert_eq!(pending[0].1, 0, "the first packet is still the oldest held");

        // One past the cap: the length holds and the OLDEST leaves.
        let overshoot = MAX_PENDING_OPUS_PACKETS as u64;
        assert!(
            push_pending_opus_packet(&mut pending, (vec![0xAA], overshoot)),
            "the packet that exceeded the cap must report the drop"
        );
        assert_eq!(pending.len(), MAX_PENDING_OPUS_PACKETS);
        assert_eq!(
            pending[0].1, 1,
            "packet 0 should have been dropped, not kept"
        );
        assert_eq!(
            pending.last().expect("non-empty").1,
            overshoot,
            "the newest packet must be the one retained"
        );

        // Far past the cap: still bounded, and the retained window is exactly
        // the last MAX_PENDING_OPUS_PACKETS timestamps, in order.
        let total = MAX_PENDING_OPUS_PACKETS as u64 * 4;
        for ts in (overshoot + 1)..total {
            push_pending_opus_packet(&mut pending, (vec![0xBB], ts));
        }
        assert_eq!(pending.len(), MAX_PENDING_OPUS_PACKETS);
        let expected_first = total - MAX_PENDING_OPUS_PACKETS as u64;
        let observed: Vec<u64> = pending.iter().map(|(_, ts)| *ts).collect();
        let expected: Vec<u64> = (expected_first..total).collect();
        assert_eq!(
            observed, expected,
            "the backlog must hold the newest {MAX_PENDING_OPUS_PACKETS} packets \
             in arrival order"
        );
    }

    /// The single file a session that never changed resolution wrote.
    ///
    /// `stop()` reports every segment (MDD-BUG-CRU-00814). These tests hold one
    /// resolution throughout, so "there is exactly one" is part of what they
    /// are asserting rather than a detail to index past.
    fn only_segment(segments: Vec<PathBuf>) -> PathBuf {
        assert_eq!(
            segments.len(),
            1,
            "one resolution throughout should write one file, got {segments:?}"
        );
        segments.into_iter().next().expect("exactly one segment")
    }

    /// The suffixed name a rollover produces, for a given base and counter.
    ///
    /// Spelled out here rather than imported so the test states the layout it
    /// depends on: if the encoder renames its segments, this disagrees.
    fn rolled_over_path(base: &std::path::Path, counter: u32) -> PathBuf {
        let stem = base.file_stem().unwrap_or_default().to_string_lossy();
        let ext = base.extension().unwrap_or_default().to_string_lossy();
        base.with_file_name(format!("{stem}-{counter}.{ext}"))
    }

    /// MDD-BUG-CRU-00814: stop() names every file the session actually wrote.
    ///
    /// A resolution change starts a new file, and the first frame is allowed
    /// to differ from the configured size - which rolls over before anything
    /// is created, so the configured path is never written at all. Returning
    /// it regardless named a file that does not exist and said nothing about
    /// the one that does.
    #[test]
    fn stop_reports_a_segment_that_never_used_the_configured_path() {
        let base = temp_capture_path("cru_00814_first");
        let rolled = rolled_over_path(&base, 2);
        for path in [&base, &rolled] {
            let _ = std::fs::remove_file(path);
        }

        let mut session = CaptureSession::start(CaptureConfig {
            output_path: base.clone(),
            width: 64,
            height: 64,
            sample_rate: 44100,
            channels: 2,
        })
        .expect("capture session starts");

        // The first frame is a different size from the configured one, which
        // is the case that rolls over before any file exists.
        let pixels = vec![0x0000_2040u32; 32 * 32];
        session.submit_frame(&pixels, 32, 32, 0);

        let segments = session.stop().expect("capture session finalises");
        assert_eq!(
            segments,
            vec![rolled.clone()],
            "the only file written is the suffixed one"
        );
        assert!(
            !base.exists(),
            "the configured path was never written, so it must not be reported"
        );
        for path in &segments {
            assert!(
                path.exists(),
                "a reported segment exists: {}",
                path.display()
            );
        }

        for path in [&base, &rolled] {
            let _ = std::fs::remove_file(path);
        }
    }

    /// A mid-stream resolution change is reported as both files, in order
    /// (MDD-BUG-CRU-00814).
    #[test]
    fn stop_reports_every_segment_a_resolution_change_produced() {
        let base = temp_capture_path("cru_00814_change");
        let rolled = rolled_over_path(&base, 2);
        for path in [&base, &rolled] {
            let _ = std::fs::remove_file(path);
        }

        let mut session = CaptureSession::start(CaptureConfig {
            output_path: base.clone(),
            width: 64,
            height: 64,
            sample_rate: 44100,
            channels: 2,
        })
        .expect("capture session starts");

        let first = vec![0x0000_2040u32; 64 * 64];
        session.submit_frame(&first, 64, 64, 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !base.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(base.exists(), "the first size opened the configured file");

        let second = vec![0x0000_4080u32; 32 * 32];
        session.submit_frame(&second, 32, 32, 33_000_000);

        let segments = session.stop().expect("capture session finalises");
        assert_eq!(
            segments,
            vec![base.clone(), rolled.clone()],
            "both files, in the order they were written"
        );
        for path in &segments {
            assert!(
                path.exists(),
                "a reported segment exists: {}",
                path.display()
            );
        }

        for path in [&base, &rolled] {
            let _ = std::fs::remove_file(path);
        }
    }

    /// One resolution throughout still reports exactly one file, so the list
    /// is not merely always longer (MDD-BUG-CRU-00814).
    #[test]
    fn stop_reports_one_segment_when_nothing_changed() {
        let base = temp_capture_path("cru_00814_single");
        let _ = std::fs::remove_file(&base);

        let mut session = CaptureSession::start(CaptureConfig {
            output_path: base.clone(),
            width: 64,
            height: 64,
            sample_rate: 44100,
            channels: 2,
        })
        .expect("capture session starts");
        let pixels = vec![0x0000_2040u32; 64 * 64];
        session.submit_frame(&pixels, 64, 64, 0);
        session.submit_frame(&pixels, 64, 64, 33_000_000);

        let segments = session.stop().expect("capture session finalises");
        assert_eq!(segments, vec![base.clone()]);
        assert!(!rolled_over_path(&base, 2).exists());
        let _ = std::fs::remove_file(&base);
    }

    #[test]
    fn emergency_stop_finalises_while_session_owner_is_stalled() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "gp64_test_capture_emergency_{}.mkv",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut session = CaptureSession::start(CaptureConfig {
            output_path: path.clone(),
            width: 64,
            height: 64,
            sample_rate: 44100,
            channels: 2,
        })
        .expect("capture session starts");
        let pixels = vec![0x0000_2040u32; 64 * 64];
        session.submit_frame(&pixels, 64, 64, 0);

        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            path.exists(),
            "encoder must open the capture before the stop"
        );

        // Keep `session` alive, as a wedged emulator thread would, while the
        // external owner asks only the encoder to drain and finalise.
        let stop_handle = session.stop_handle();
        stop_handle.request_stop();
        assert!(
            stop_handle.wait_for_finish(Duration::from_secs(5)),
            "encoder must finish without waiting for the session owner"
        );

        let finished = only_segment(
            session
                .stop()
                .expect("session joins the already-finished encoder"),
        );
        assert!(
            std::fs::metadata(&finished)
                .expect("finalised capture exists")
                .len()
                > 0
        );
        let _ = std::fs::remove_file(&finished);
    }

    #[test]
    fn session_drop_without_stop() {
        let path = temp_capture_path("capture_drop");

        let config = CaptureConfig {
            output_path: path.clone(),
            width: 64,
            height: 64,
            sample_rate: 44100,
            channels: 2,
        };

        {
            let session = CaptureSession::start(config).expect("Failed to start capture");
            let pixels = vec![0x00000000u32; 64 * 64]; // black
            session.submit_frame(&pixels, 64, 64, 0);
            std::thread::sleep(Duration::from_millis(100));
            // Session dropped here without calling stop()
        }

        // Should not panic
        let _ = std::fs::remove_file(&path);
    }

    /// MDD-BUG-CRU-00816: the budget is spent and returned exactly once.
    #[test]
    fn queue_slots_are_claimed_up_to_capacity_and_given_back() {
        let depth = AtomicUsize::new(0);
        for taken in 0..3 {
            assert!(
                claim_queue_slot(&depth, 3),
                "slot {taken} is within the budget"
            );
        }
        assert!(!claim_queue_slot(&depth, 3), "a full queue refuses");
        assert_eq!(depth.load(Ordering::Acquire), 3, "a refusal costs nothing");

        release_queue_slot(&depth);
        assert!(
            claim_queue_slot(&depth, 3),
            "a freed slot can be taken again"
        );

        // A release with nothing outstanding must not wrap: an enormous count
        // would read as permanently full and drop capture for good.
        let stray = AtomicUsize::new(0);
        release_queue_slot(&stray);
        assert_eq!(stray.load(Ordering::Acquire), 0);
        assert!(claim_queue_slot(&stray, 1));
    }

    /// The count says what the channel would say, which is the point: it is
    /// asked instead of the channel, before there is a payload to ask with
    /// (MDD-BUG-CRU-00816).
    #[test]
    fn the_claim_count_agrees_with_the_channel_it_stands_in_for() {
        const CAPACITY: usize = 4;
        let (tx, rx) = mpsc::sync_channel::<u32>(CAPACITY);
        let depth = AtomicUsize::new(0);

        for value in 0..CAPACITY as u32 {
            assert!(claim_queue_slot(&depth, CAPACITY));
            tx.try_send(value).expect("a claimed slot always sends");
        }

        assert!(!claim_queue_slot(&depth, CAPACITY), "the count says full");
        assert!(tx.try_send(99).is_err(), "and so does the channel");

        assert_eq!(rx.recv().unwrap(), 0);
        release_queue_slot(&depth);
        assert!(
            claim_queue_slot(&depth, CAPACITY),
            "the count says there is room"
        );
        tx.try_send(99).expect("and so does the channel");
    }

    /// The claim comes before the copy, which is the whole fix.
    ///
    /// Nothing observable distinguishes "dropped the frame" from "copied 1.9
    /// MiB and then dropped the frame", so this reads the shape of the two
    /// submit paths the way the MDD-BUG-CRU-00813 guard below reads the
    /// encoder thread's (MDD-BUG-CRU-00816).
    #[test]
    fn the_submit_paths_claim_a_slot_before_copying_the_payload() {
        let source = include_str!("encoder.rs");
        for (function, copy) in [
            ("pub fn submit_frame(", "pixels.to_vec()"),
            ("pub fn submit_audio(", "samples.to_vec()"),
        ] {
            let start = source.find(function).expect("{function} must exist");
            let body = &source[start..];
            let end = body.find("\n    }").expect("the function must end");
            let body = &body[..end];

            let claim = body
                .find("claim_queue_slot(")
                .unwrap_or_else(|| panic!("{function} does not claim a queue slot"));
            let copied = body.find(copy).unwrap_or_else(|| {
                panic!(
                    "{function} no longer copies {copy} - \
                     if that is deliberate, this guard needs rewriting"
                )
            });
            assert!(
                claim < copied,
                "{function} copies the payload before asking for a slot, so a \
                 dropped submission still pays for it (MDD-BUG-CRU-00816)"
            );
        }
    }

    /// MDD-BUG-CRU-00817: the last partial Opus frame is not thrown away.
    ///
    /// Opus encodes whole 20 ms frames and both callers stop at arbitrary
    /// offsets, so the residual 1-19 ms used to be dropped at finalise -
    /// silently, on essentially every recording.
    #[test]
    fn a_partial_final_opus_frame_is_encoded_rather_than_dropped() {
        let path = temp_capture_path("cru_00817_tail");
        let _ = std::fs::remove_file(&path);

        let mut session = CaptureSession::start(CaptureConfig {
            output_path: path.clone(),
            width: 64,
            height: 64,
            sample_rate: 48000,
            channels: 2,
        })
        .expect("capture session starts");

        session.submit_frame(&vec![0u32; 64 * 64], 64, 64, 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }

        // One whole 20 ms frame plus 5 ms, so exactly one tail frame is owed.
        // Non-zero samples, so a dropped tail is a dropped sound rather than a
        // dropped silence.
        let samples = vec![0.25f32; (960 + 240) * 2];
        session.submit_audio(&samples, 0);

        let segments = session.stop().expect("capture session finalises");
        let data = std::fs::read(&segments[0]).expect("finalised capture file");
        let _ = std::fs::remove_file(&segments[0]);

        // DiscardPadding appears only for a padded tail, so finding it is the
        // tail having been written at all - and its VALUE is the point: a
        // tail declared as zero padding plays 15 ms of silence that was
        // never recorded.
        let found: Vec<usize> = (0..data.len().saturating_sub(1))
            .filter(|index| data[*index] == 0x75 && data[index + 1] == 0xA2)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "expected exactly one DiscardPadding; the partial tail frame never \
             reached the file (MDD-BUG-CRU-00817)"
        );
        let at = found[0];
        assert!(data[at + 2] & 0x80 != 0, "length is one VINT byte");
        let length = usize::from(data[at + 2] & 0x7f);
        let mut padding_ns = 0i64;
        for byte in &data[at + 3..at + 3 + length] {
            padding_ns = (padding_ns << 8) | i64::from(*byte);
        }
        // About 5 ms of real audio and 6.5 ms of encoder delay remain, so
        // about 8.5 ms of packet rounding must be discarded. Not exactly 8.5:
        // the linear resampler yields one frame fewer
        // than it is given at start-up, having no earlier sample to
        // interpolate from, so the figure lands a sample period away.
        let sample_period_ns = 1_000_000_000 / 48_000;
        assert!(
            (padding_ns - 8_500_000).abs() <= 2 * sample_period_ns,
            "the reader is told to discard {padding_ns} ns of a 20 ms frame, not the ~8.5 ms that was padding"
        );
        assert!(
            padding_ns > 0 && padding_ns < 20_000_000,
            "a tail declared as no padding, or as a whole frame of it, is not a tail at all"
        );
    }

    #[test]
    fn submit_frame_no_panic_with_odd_dimensions() {
        let path = temp_capture_path("odd_dim");

        let config = CaptureConfig {
            output_path: path.clone(),
            width: 321,
            height: 201,
            sample_rate: 44100,
            channels: 2,
        };

        let mut session = CaptureSession::start(config).expect("Failed to start capture");

        // Odd dimensions should not panic
        let pixels = vec![0x0000FF00u32; 321 * 201]; // green
        session.submit_frame(&pixels, 321, 201, 0);
        std::thread::sleep(Duration::from_millis(200));

        let _ = session.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn elapsed_increases() {
        // A per-run path: a fixed name let two concurrent runs of this suite
        // write the same file (MDD-BUG-CRU-01527).
        let path = temp_capture_path("elapsed");

        let config = CaptureConfig {
            output_path: path.clone(),
            width: 64,
            height: 64,
            sample_rate: 44100,
            channels: 2,
        };

        let mut session = CaptureSession::start(config).expect("Failed to start capture");
        let pixels = vec![0u32; 64 * 64];
        session.submit_frame(&pixels, 64, 64, 0);
        // `elapsed` is stored by the encoder thread when it takes the frame,
        // so wait for that rather than a fixed 200 ms, which a loaded parallel
        // suite can outlast (MDD-BUG-CRU-01527). The bound only caps a real
        // failure; the claim is unchanged.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while session.elapsed() == Duration::ZERO && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }

        let elapsed = session.elapsed();
        assert!(
            elapsed > Duration::ZERO,
            "Elapsed should be non-zero after processing"
        );

        let _ = session.stop();
        let _ = std::fs::remove_file(&path);
    }

    /// Regression: the encoder must not drop buffered audio when many
    /// messages arrive with clustered timestamps.
    ///
    /// When the emulator's scheduler bursts through a catch-up batch of ticks
    /// in a short wall-clock window, every tick submits ~1 ms of audio with
    /// the same wall-clock timestamp. The old code compared the buffer's end
    /// timestamp against the message's submission time and truncated the
    /// buffer tail when the buffer outran wall-clock by > 20 ms — which
    /// permanently lost emulator audio content. Over a long recording, the
    /// drops accumulated and audio fell progressively behind on-screen action
    /// ("video ahead of audio").
    ///
    /// This test submits 100 × 1 ms audio chunks all at ts=0 and asserts the
    /// content is preserved in the output MKV. Before the fix, only the very
    /// first 20 ms packet survived; after, all five packets span ts 0 → 80 ms.
    #[test]
    fn audio_clustered_timestamps_preserve_content() {
        let path = temp_capture_path("audio_clustered_ts");
        let _ = std::fs::remove_file(&path);

        let config = CaptureConfig {
            output_path: path.clone(),
            width: 64,
            height: 64,
            sample_rate: 48000, // skip resampling
            channels: 2,
        };

        let mut session = CaptureSession::start(config).expect("Failed to start capture");

        // Open the muxer with a single video frame at ts=0.
        let pixels = vec![0u32; 64 * 64];
        session.submit_frame(&pixels, 64, 64, 0);

        // 100 × 1 ms audio messages, all timestamped at ts=0. Mirrors a
        // burst where wall-clock barely advances but the emulator produced
        // 100 ms of content.
        let one_ms = vec![0.0f32; 48 * 2]; // 48 stereo frames at 48 kHz
        for _ in 0..100 {
            session.submit_audio(&one_ms, 0);
        }

        std::thread::sleep(Duration::from_millis(500));
        let result = session.stop();
        assert!(result.is_ok(), "Stop should succeed: {result:?}");

        // Parse Duration element (EBML ID 0x4489, 8-byte IEEE-754 double).
        let data = std::fs::read(&path).expect("output mkv should exist");
        let duration_pos = data
            .windows(2)
            .position(|w| w == [0x44, 0x89])
            .expect("Duration element not found in MKV");
        let float_start = duration_pos + 3;
        let duration_bytes: [u8; 8] = data[float_start..float_start + 8]
            .try_into()
            .expect("Duration payload too short");
        let duration_ms = f64::from_be_bytes(duration_bytes);

        let _ = std::fs::remove_file(&path);

        // 100 ms of content → 5 opus packets at ts 0, 20, 40, 60, 80.
        // With the old drop-on-overshoot logic only the first packet
        // survived (duration ≈ 0). After the fix, at least four packets
        // land in the file so the Duration reaches ≥60 ms.
        assert!(
            duration_ms >= 60.0,
            "Duration should be ≥60 ms (audio content preserved across a \
             ts burst), got {duration_ms} ms. Encoder is dropping buffered \
             samples when msg_ts clusters."
        );
    }

    /// Regression: audio timestamps must follow wall-clock, not packet count.
    ///
    /// If the encoder timestamps audio from an internal counter incremented per
    /// emitted Opus frame, then a gap in `submit_audio` calls (e.g. the emulator
    /// dropping audio during a long host stall) makes the audio timeline shorter
    /// than the wall-clock timeline — permanent A/V drift. This test feeds a
    /// 500 ms submission gap and asserts the muxed file's Duration reflects the
    /// full wall-clock span (via silence-fill), not just the duration of
    /// delivered samples.
    #[test]
    fn audio_timestamps_follow_wall_clock_across_submission_gap() {
        let path = temp_capture_path("audio_wall_clock");
        let _ = std::fs::remove_file(&path);

        let config = CaptureConfig {
            output_path: path.clone(),
            width: 64,
            height: 64,
            sample_rate: 48000,
            channels: 2,
        };

        let mut session = CaptureSession::start(config).expect("Failed to start capture");

        // Open the muxer and provide video spanning ~200 ms of wall clock.
        let pixels = vec![0x00000000u32; 64 * 64];
        for i in 0..=10 {
            let ts_ns = i * 20_000_000; // 0, 20, 40, ..., 200 ms
            session.submit_frame(&pixels, 64, 64, ts_ns);
            std::thread::sleep(Duration::from_millis(15));
        }

        // 100 ms of audio starting at wall-clock 0 (48 kHz stereo → 4800 frames).
        let chunk = vec![0.0f32; 4800 * 2];
        session.submit_audio(&chunk, 0);

        // Simulate a 500 ms audio submission gap (no audio messages sent).
        std::thread::sleep(Duration::from_millis(50));

        // 100 ms of audio at wall-clock 600..700 ms (the emulator's own
        // wall-clock counter continued advancing during the gap).
        session.submit_audio(&chunk, 600_000_000);

        // Let the encoder thread drain.
        std::thread::sleep(Duration::from_millis(200));

        let result = session.stop();
        assert!(result.is_ok(), "Stop should succeed: {result:?}");

        // Parse Duration element (EBML ID 0x4489, 8-byte IEEE-754 double).
        let data = std::fs::read(&path).expect("output mkv should exist");
        let duration_pos = data
            .windows(2)
            .position(|w| w == [0x44, 0x89])
            .expect("Duration element not found in MKV");
        // Skip 2-byte ID + 1-byte size VINT → 8-byte float payload.
        let float_start = duration_pos + 3;
        let duration_bytes: [u8; 8] = data[float_start..float_start + 8]
            .try_into()
            .expect("Duration payload too short");
        let duration_ms = f64::from_be_bytes(duration_bytes);

        let _ = std::fs::remove_file(&path);

        // With wall-clock-anchored audio, Duration reflects the full ~700 ms
        // span (last audio packet at ~700 ms, first timestamp at 0). With the
        // packet-count counter, audio only produces ~200 ms of timeline and
        // Duration stays near the last video frame (~200 ms).
        assert!(
            duration_ms >= 500.0,
            "Duration should be >= 500 ms (wall-clock span), got {duration_ms} ms. \
             Audio timeline is not anchored to wall-clock — drift will accumulate."
        );
    }

    /// Regression: a pathological but monotonic timestamp jump must fail
    /// before it can turn into a host-sized silence allocation.
    #[test]
    fn absurd_audio_timestamp_gap_is_rejected_before_materializing_silence() {
        let path = std::env::temp_dir().join(format!(
            "gp64_capture_audio_gap_bound_{}.mkv",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut session = CaptureSession::start(CaptureConfig {
            output_path: path.clone(),
            width: 64,
            height: 64,
            sample_rate: 48_000,
            channels: 2,
        })
        .expect("capture session starts");
        let pixels = vec![0u32; 64 * 64];
        session.submit_frame(&pixels, 64, 64, 0);

        let sample = [0.0f32, 0.0];
        session.submit_audio(&sample, 0);
        session.submit_audio(&sample, u64::MAX);

        let error = session
            .stop()
            .expect_err("an unfillable audio timestamp gap must fail clearly");
        match error {
            CaptureError::Encoder(message) => {
                assert!(
                    message.contains("audio timestamp gap")
                        && message.contains("maximum fillable gap"),
                    "unexpected encoder error: {message}"
                );
            }
            other => panic!("unexpected capture error: {other}"),
        }

        let _ = std::fs::remove_file(path);
    }

    /// Count Opus packets in a finalised capture by scanning its SimpleBlocks
    /// for track 2. Cheap enough to avoid pulling in a demuxer, and exact:
    /// every audio packet is written as one SimpleBlock whose first payload
    /// byte is the track VINT `0x82`.
    ///
    /// The scan keys on the SimpleBlock element ID (0xA3) followed by a
    /// 1-byte size VINT and the track VINT, which is the shape
    /// `write_simple_block` emits for every packet small enough to size in one
    /// byte — true of Opus at 128 kbit/s (≈320 bytes/packet needs a 2-byte
    /// VINT, so accept both widths).
    fn count_opus_packets(data: &[u8]) -> usize {
        let mut count = 0;
        let mut i = 0;
        while i + 4 < data.len() {
            if data[i] == 0xA3 {
                // 1-byte size VINT has the high bit set; 2-byte starts 0b01.
                let size_width = if data[i + 1] & 0x80 != 0 {
                    1
                } else if data[i + 1] & 0x40 != 0 {
                    2
                } else {
                    i += 1;
                    continue;
                };
                if data[i + 1 + size_width] == 0x82 {
                    count += 1;
                }
            }
            i += 1;
        }
        count
    }

    /// Regression (A/V rate desync): the audio timeline must be governed by
    /// the SOURCE sample rate the session was told about, so that one second
    /// of submitted content becomes one second of Opus timeline.
    ///
    /// `src/main.rs` used to hand `CaptureSession` a hard-coded 44100 while
    /// the emulator's mixer renders at the host device's rate. On a 48 kHz
    /// device — every macOS output, and most modern Windows ones — the encoder
    /// upsampled already-48 kHz content by 48000/44100, producing 8.84% more
    /// Opus packets than the content deserved. Audio and video share one tick
    /// clock, so the audio track simply grew longer than the video and lagged
    /// further behind every second.
    ///
    /// The check is rate-relative on purpose: for each source rate, submitting
    /// exactly one second of content at that rate must yield ~50 Opus packets
    /// (1000 ms / 20 ms). A wrongly-declared rate shows up immediately as a
    /// packet count scaled by the ratio of the two rates.
    #[test]
    fn audio_timeline_length_follows_the_declared_source_rate() {
        for rate in [44100u32, 48000] {
            let path = std::env::temp_dir().join(format!(
                "gp64_capture_rate_{rate}_{}.mkv",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);

            let mut session = CaptureSession::start(CaptureConfig {
                output_path: path.clone(),
                width: 64,
                height: 64,
                sample_rate: rate,
                channels: 2,
            })
            .expect("capture session starts");

            // Open the muxer so audio is written rather than buffered.
            let pixels = vec![0u32; 64 * 64];
            session.submit_frame(&pixels, 64, 64, 0);

            // Exactly one second of content, delivered as 1 ms chunks stamped
            // on the same 1 ms-per-tick clock the emulator uses.
            let frames_per_ms = rate as usize / 1000;
            let chunk = vec![0.0f32; frames_per_ms * 2];
            for ms in 0..1000u64 {
                session.submit_audio(&chunk, ms * 1_000_000);
            }

            let finished = only_segment(session.stop().expect("capture finalises"));
            let data = std::fs::read(&finished).expect("output mkv");
            let packets = count_opus_packets(&data);
            let _ = std::fs::remove_file(&finished);

            // 1000 ms of content at 20 ms per packet = 50 packets. The
            // integer frames_per_ms truncation (44 vs 44.1) loses ~2 ms over
            // the second, so allow a couple of packets either way.
            assert!(
                (48..=52).contains(&packets),
                "one second of {rate} Hz content produced {packets} Opus packets \
                 ({} ms of audio timeline) — expected ~50 (1000 ms). The declared \
                 source rate is not governing the resampler, so the audio track \
                 drifts against the video.",
                packets * 20,
            );
        }
    }

    /// The `OpusHead` pre-skip and the Matroska `CodecDelay` describe the same
    /// priming delay and must agree, or the demuxer un-shifts the audio by a
    /// different amount than the decoder discards.
    #[test]
    fn opus_codec_delay_matches_the_declared_pre_skip() {
        let path = std::env::temp_dir().join(format!(
            "gp64_capture_codec_delay_{}.mkv",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut session = CaptureSession::start(CaptureConfig {
            output_path: path.clone(),
            width: 64,
            height: 64,
            sample_rate: 48000,
            channels: 2,
        })
        .expect("capture session starts");
        session.submit_frame(&vec![0u32; 64 * 64], 64, 64, 0);
        let finished = only_segment(session.stop().expect("capture finalises"));
        let data = std::fs::read(&finished).expect("output mkv");
        let _ = std::fs::remove_file(&finished);

        // OpusHead pre-skip: little-endian u16 at offset 10 of the header.
        let head = data
            .windows(8)
            .position(|w| w == b"OpusHead")
            .expect("OpusHead in CodecPrivate");
        let pre_skip = u16::from_le_bytes([data[head + 10], data[head + 11]]);

        // CodecDelay (0x56AA): 2-byte ID, 1-byte size VINT, then the value.
        let delay_pos = data
            .windows(2)
            .position(|w| w == [0x56, 0xAA])
            .expect("CodecDelay element must be present on an A_OPUS track");
        let len = (data[delay_pos + 2] & 0x7F) as usize;
        let mut delay_ns = 0u64;
        for byte in &data[delay_pos + 3..delay_pos + 3 + len] {
            delay_ns = (delay_ns << 8) | u64::from(*byte);
        }

        assert_eq!(
            delay_ns,
            u64::from(pre_skip) * 1_000_000_000 / 48_000,
            "CodecDelay ({delay_ns} ns) must equal the OpusHead pre-skip \
             ({pre_skip} samples at 48 kHz)"
        );
    }
    // ------------------------------------------------------------------
    // MDD-BUG-CRU-00810: unencodable audio configuration
    //
    // These assert on what `start` RETURNS and never submit a sample. That
    // is deliberate: the pre-fix behaviour of a zero sample rate is a
    // non-advancing worker loop, so a test that drove the pipeline would
    // hang rather than fail, and a hang is not a usable red.
    // ------------------------------------------------------------------

    fn config_at(name: &str, sample_rate: u32, channels: u16) -> CaptureConfig {
        CaptureConfig {
            output_path: std::env::temp_dir().join(format!(
                "gp64_capture_cfg_{}_{}_{}.mkv",
                std::process::id(),
                name,
                channels
            )),
            width: 320,
            height: 200,
            sample_rate,
            channels,
        }
    }

    /// A rejection has to happen before anything is spawned or created, so a
    /// refused start must leave no file behind either.
    fn assert_rejected(config: CaptureConfig, what: &str) {
        let path = config.output_path.clone();
        let _ = std::fs::remove_file(&path);

        match CaptureSession::start(config) {
            Ok(_) => panic!("{what} must be rejected by CaptureSession::start, but it succeeded"),
            Err(CaptureError::Config(message)) => {
                assert!(
                    !message.is_empty(),
                    "{what} was rejected without saying why"
                );
            }
            Err(other) => panic!("{what} must be rejected as a Config error, got: {other}"),
        }

        assert!(
            !path.exists(),
            "{what} was rejected but the encoder had already created {}",
            path.display()
        );
    }

    #[test]
    fn a_zero_sample_rate_is_rejected_rather_than_hanging_the_encoder() {
        // ratio = 48000/0 = inf, so the resampler's step is zero and its loop
        // never advances.
        assert_rejected(config_at("rate0", 0, 2), "a zero sample rate");
    }

    #[test]
    fn a_zero_channel_count_is_rejected_rather_than_dividing_by_zero() {
        // The Opus timeline arithmetic divides by the channel count.
        assert_rejected(config_at("chan0", 44_100, 0), "a zero channel count");
    }

    #[test]
    fn non_stereo_channel_counts_are_rejected_because_the_pipeline_is_stereo() {
        // Anything but 2 writes an OpusHead that contradicts the stereo
        // payload underneath it, and mis-strides the frame arithmetic.
        for channels in [1u16, 3, 6, 8] {
            assert_rejected(
                config_at("chanN", 44_100, channels),
                &format!("{channels} audio channels"),
            );
        }
    }

    #[test]
    fn sample_rates_outside_the_supported_band_are_rejected() {
        for rate in [1u32, 7_999, 192_001, u32::MAX] {
            assert_rejected(
                config_at("rateN", rate, 2),
                &format!("a source sample rate of {rate} Hz"),
            );
        }
    }

    #[test]
    fn the_stereo_configurations_the_hosts_actually_use_are_accepted() {
        // The positive control. 8 kHz is `mdem-host`'s no-device fallback,
        // 44.1 kHz the emulated cards, 48 kHz the identity path; validation
        // must not have narrowed the band under any of them.
        for rate in [
            *SUPPORTED_SAMPLE_RATES.start(),
            44_100,
            48_000,
            *SUPPORTED_SAMPLE_RATES.end(),
        ] {
            let config = config_at("ok", rate, SUPPORTED_CHANNELS);
            let path = config.output_path.clone();
            config
                .validate()
                .unwrap_or_else(|e| panic!("{rate} Hz stereo must be supported: {e}"));

            let mut session =
                CaptureSession::start(config).expect("a supported configuration must start");
            let _ = session.stop();
            let _ = std::fs::remove_file(&path);
        }
    }
    // ------------------------------------------------------------------
    // MDD-BUG-CRU-00813: encoder and muxer failures must reach the caller
    // ------------------------------------------------------------------

    /// The plumbing half, behaviourally: an error raised inside the encoder
    /// thread has to come back out of `stop`.
    ///
    /// This is a positive control, not the regression test for the four
    /// discarded results — it travels the same return path they now use, but
    /// it is raised by a failing `File::create`, which always propagated. A
    /// genuine mid-file muxer write failure needs a full filesystem or a
    /// production fault seam; the structural test below covers those sites.
    #[test]
    fn a_fatal_encoder_thread_error_is_reported_by_stop() {
        let path = std::env::temp_dir()
            .join(format!("gp64_capture_no_such_dir_{}", std::process::id()))
            .join("out.mkv");
        assert!(
            !path.parent().expect("the path has a parent").exists(),
            "the fixture needs a parent directory that does not exist"
        );

        let mut session = CaptureSession::start(CaptureConfig {
            output_path: path,
            width: 320,
            height: 200,
            sample_rate: 44_100,
            channels: 2,
        })
        .expect("the configuration is valid, so the session starts");

        // The muxer, and its file, are created on the first video frame.
        session.submit_frame(&vec![0u32; 320 * 200], 320, 200, 0);

        match session.stop() {
            Ok(segments) => panic!(
                "stop() reported success at {segments:?} although the encoder \
                 thread could not create the file"
            ),
            Err(CaptureError::Io(e)) => {
                assert_eq!(
                    e.kind(),
                    std::io::ErrorKind::NotFound,
                    "unexpected error: {e}"
                );
            }
            Err(other) => panic!("expected the underlying I/O error, got: {other}"),
        }
    }

    /// The regression test proper, and a source guard by necessity.
    ///
    /// MDD-BUG-CRU-00813: the rollover finalisation, both audio-packet writes,
    /// and the Opus encode all threw their `Result` away, so a damaged segment
    /// or missing packets were followed by a clean final segment and a
    /// successful `stop()`. The video write on the same path always
    /// propagated — the asymmetry was the whole defect.
    ///
    /// A behavioural test would need a mid-file write to fail on demand, which
    /// portably means adding a fault seam to shipped code. Reading the encoder
    /// thread's own source is the cheaper honest check: it proves no result in
    /// there is discarded, which is exactly the property that was violated.
    #[test]
    fn the_encoder_thread_discards_no_muxer_or_codec_result() {
        let source = include_str!("encoder.rs");
        let start = source
            .find("\nfn encoder_thread(")
            .expect("encoder_thread must exist");
        let end = source[start..]
            .find("\n#[cfg(test)]")
            .map(|offset| start + offset)
            .expect("the tests module must follow encoder_thread");
        let body = &source[start..end];

        // `let _ = <fallible call>` is the exact shape the defect took. The
        // encoder thread is the one place it must never appear: it is the only
        // code whose errors nothing else can observe.
        assert!(
            !body.contains("let _ ="),
            "encoder_thread discards a Result. Every failure in there is \
             invisible to the caller — propagate it with `?` so stop() can \
             report it (MDD-BUG-CRU-00813)."
        );

        // And the specific calls are still there, each handled. Without this,
        // deleting a call would satisfy the assertion above. Opus encoding is
        // factored into a helper so the normal and streamed-silence paths share
        // the same timestamp and write-error handling; inspect that helper too.
        let fallible = ["finalise()", "write_audio_packet(", "write_video_frame("];
        for call in fallible {
            assert!(
                body.contains(call),
                "encoder_thread no longer calls {call} — if that is deliberate, \
                 update this guard; if not, a capture path has been lost"
            );
        }

        let body_lines: Vec<&str> = body.lines().collect();
        for (i, line) in body_lines.iter().enumerate() {
            let Some(call) = fallible.iter().find(|c| line.contains(**c)) else {
                continue;
            };
            let window = body_lines[i..(i + 4).min(body_lines.len())].join("\n");
            assert!(
                window.contains('?'),
                "the {call} at encoder_thread line {} does not propagate its \
                 error:\n{window}",
                i + 1
            );
        }

        let encode_start = source
            .find("\nfn encode_opus_frame(")
            .expect("Opus frame helper must exist");
        let encode_end = source[encode_start..]
            .find("\nfn drain_opus_frames(")
            .map(|offset| encode_start + offset)
            .expect("Opus frame helper must precede the frame drain helper");
        let encode_body = &source[encode_start..encode_end];
        assert!(
            encode_body.contains("encode_float(") && encode_body.contains('?'),
            "Opus encoding must remain fallible and propagate its error"
        );
        assert!(
            !encode_body.contains("let _ ="),
            "Opus encoding helper must not discard a Result"
        );
    }
}
