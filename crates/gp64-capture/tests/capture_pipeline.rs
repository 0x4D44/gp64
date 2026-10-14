//! End-to-end oracle for the capture pipeline: the MKV a session writes must
//! contain decodable VP9 whose pixels are the frame that went in.
//!
//! The unit tests in `convert.rs` and `muxer.rs` pin the two halves separately
//! — the colour matrix against canonical BT.601 values, the crop metadata
//! against the Matroska element IDs. Neither can catch a seam bug between
//! them: planes handed over with the wrong stride, chroma planes swapped, the
//! muxer storing frames a decoder can't read, or the coded/display geometry
//! disagreeing. This test closes that gap by decoding the real file.
//!
//! `rvp9-decoder` is the oracle rather than a `Command::new("ffmpeg")` probe,
//! precisely so there is no "tool absent → silently pass" path.

use std::time::Duration;

use gp64_capture::encoder::{CaptureConfig, CaptureSession};

/// 800x600 is deliberate: capture leaves sources ≥ 640 wide unscaled, and 800
/// is not a multiple of 64, so this is the padded-and-cropped case. A test at
/// 1280 wide would exercise no padding at all.
const WIDTH: u32 = 800;
const HEIGHT: u32 = 600;
/// What the encoder must actually code: width up to the superblock grid.
const CODED_WIDTH: u32 = 832;

/// Build a frame the decoder can be checked against meaningfully.
///
/// Flat colour would pass even if the planes were swapped or the stride were
/// wrong, so the pattern varies along both axes and puts a distinct hue in
/// each quadrant: any plane mix-up, stride slip, or row/column transposition
/// moves colour to the wrong quadrant.
fn test_pattern() -> Vec<u32> {
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let ramp = (x * 255) / (WIDTH - 1);
            let fade = (y * 255) / (HEIGHT - 1);
            let pixel = match (x < WIDTH / 2, y < HEIGHT / 2) {
                (true, true) => (ramp << 16) | 0x00_00_20, // red ramp
                (false, true) => (fade << 8) | 0x20_00_00, // green ramp
                (true, false) => ramp | 0x00_20_00,        // blue ramp
                (false, false) => (fade << 16) | (fade << 8) | fade, // grey ramp
            };
            pixels.push(pixel);
        }
    }
    pixels
}

/// Read an EBML VINT at `data[pos]`, returning `(value, total_byte_length)`.
fn read_vint(data: &[u8], pos: usize) -> (u64, usize) {
    let first = data[pos];
    let length = first.leading_zeros() as usize + 1;
    // At the 8-byte width the marker bit occupies the whole first byte, so the
    // payload mask is 0 — `0xFF >> 8` would overflow. The muxer reserves 8-byte
    // sizes for Segment and every Cluster, so this is the common case here, not
    // an edge one.
    let mut value = u64::from(first & ((1u8 << (8 - length)) - 1));
    for byte in &data[pos + 1..pos + length] {
        value = (value << 8) | u64::from(*byte);
    }
    (value, length)
}

/// Read an EBML element ID at `data[pos]`, returning `(id, byte_length)`.
///
/// Unlike a VINT payload, an ID keeps its length-marker bits — that is what
/// makes `0x1F43B675` (Cluster) rather than the stripped `0x0F43B675`.
fn read_id(data: &[u8], pos: usize) -> (u32, usize) {
    let length = data[pos].leading_zeros() as usize + 1;
    let mut id = 0u32;
    for byte in &data[pos..pos + length] {
        id = (id << 8) | u32::from(*byte);
    }
    (id, length)
}

/// Walk the EBML tree and collect every video-track SimpleBlock payload, in
/// file order.
fn extract_vp9_frames(mkv: &[u8]) -> Vec<Vec<u8>> {
    const SEGMENT: u32 = 0x1853_8067;
    const CLUSTER: u32 = 0x1F43_B675;
    const SIMPLE_BLOCK: u32 = 0xA3;

    let mut frames = Vec::new();
    let mut stack = vec![(0usize, mkv.len())];

    while let Some((mut pos, end)) = stack.pop() {
        while pos + 2 < end {
            let (id, id_len) = read_id(mkv, pos);
            let (size, size_len) = read_vint(mkv, pos + id_len);
            let body = pos + id_len + size_len;
            let body_end = (body + size as usize).min(end);

            match id {
                SEGMENT | CLUSTER => stack.push((body, body_end)),
                SIMPLE_BLOCK => {
                    // Block header: track VINT, 2-byte signed relative
                    // timestamp, 1 flags byte, then the frame itself.
                    let (track, track_len) = read_vint(mkv, body);
                    if track == 1 {
                        frames.push(mkv[body + track_len + 3..body_end].to_vec());
                    }
                }
                _ => {}
            }
            pos = body_end;
        }
    }
    frames
}

#[test]
fn a_capture_file_decodes_back_to_the_frame_that_was_submitted() {
    let path =
        std::env::temp_dir().join(format!("gp64_capture_roundtrip_{}.mkv", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let pattern = test_pattern();
    let mut session = CaptureSession::start(CaptureConfig {
        output_path: path.clone(),
        width: WIDTH,
        height: HEIGHT,
        sample_rate: 44100,
        channels: 2,
    })
    .expect("capture session starts");

    // Paced: this test is about fidelity, not about backpressure (that is
    // `a_recording_keeps_every_frame_and_sample_when_the_encoder_falls_behind`).
    //
    // More than the queue holds, deliberately: the encoder has to give each
    // slot back as it consumes it, and if it does not, nothing after the
    // fourth frame is recorded (MDD-BUG-CRU-00816).
    const SUBMITTED: u64 = 10;
    const VIDEO_QUEUE_CAPACITY: usize = 4;
    for frame in 0..SUBMITTED {
        session.submit_frame(&pattern, WIDTH, HEIGHT, frame * 33_000_000);
        std::thread::sleep(Duration::from_millis(120));
    }
    let segments = session.stop().expect("capture session finalises");
    // One resolution throughout, so one file; stop() reports them all
    // (MDD-BUG-CRU-00814) and the count is part of what this checks.
    assert_eq!(segments.len(), 1, "expected one segment, got {segments:?}");
    let finished = &segments[0];

    let mkv = std::fs::read(finished).expect("finalised capture file");
    let _ = std::fs::remove_file(finished);

    let frames = extract_vp9_frames(&mkv);
    assert!(
        !frames.is_empty(),
        "the capture file carries no video-track SimpleBlock"
    );
    assert!(
        frames.len() > VIDEO_QUEUE_CAPACITY,
        "only {} of {SUBMITTED} frames reached the file; a queue slot that is never given back caps capture at its depth",
        frames.len()
    );

    // The file must tell a player to display the frame we submitted, not the
    // padded frame we coded. The muxer's own unit tests pin how it renders a
    // given geometry; only here is the geometry the ENCODER chose under test,
    // which is where a coded-vs-display mix-up actually lives.
    assert_eq!(
        find_uint_element(&mkv, [0x54, 0xB0]),
        Some(u64::from(WIDTH)),
        "DisplayWidth must be the submitted width, not the coded one"
    );
    assert_eq!(
        find_uint_element(&mkv, [0x54, 0xDD]),
        Some(u64::from(CODED_WIDTH - WIDTH)),
        "PixelCropRight must hide exactly the superblock padding"
    );

    // Decode the first frame — a keyframe, since VP9 always opens with one.
    let mut decoder = rvp9_decoder::Decoder::new();
    let packet = decoder
        .parse_packet(&frames[0])
        .expect("first video frame parses as VP9");
    let decoded = decoder
        .decode_frame(&packet.frames()[0])
        .expect("first frame decodes");

    let y = decoded.y_plane();
    assert_eq!(
        y.width() as u32,
        CODED_WIDTH,
        "coded width must be padded up to the 64-pixel superblock grid"
    );
    assert_eq!(y.height() as u32, HEIGHT);

    // Compare the VISIBLE region against the luma we asked for. The padded
    // columns beyond WIDTH are the encoder's business and the muxer crops them
    // away, so they are excluded here on purpose.
    //
    // The tolerance covers lossy coding only. VP9 at capture's bitrate is
    // near-transparent on a smooth pattern, so a genuine seam bug — swapped
    // planes, a stride slip, a transposed axis — moves samples by far more
    // than this and still fails.
    let mut worst = 0i32;
    let mut total = 0i64;
    for row in 0..HEIGHT as usize {
        let decoded_row = y.row(row);
        for column in 0..WIDTH as usize {
            let (r, g, b) = rgb(pattern[row * WIDTH as usize + column]);
            let expected = ((16829 * r + 33039 * g + 6416 * b + (1 << 15)) >> 16) + 16;
            let delta = (i32::from(decoded_row[column]) - expected).abs();
            worst = worst.max(delta);
            total += i64::from(delta);
        }
    }
    let mean = total as f64 / f64::from(WIDTH * HEIGHT);
    assert!(
        mean < 2.0 && worst < 24,
        "decoded luma does not match the submitted frame \
         (mean |delta| {mean:.2}, worst {worst}) — the encode/mux seam is wrong, \
         not merely lossy"
    );

    // Chroma, same comparison at half resolution. Luma alone cannot see a
    // U/V swap — Y is identical either way — and the quadrant hues make that
    // swap a large, unmissable error here.
    let u = decoded.u_plane();
    let v = decoded.v_plane();
    let mut worst_chroma = 0i32;
    for row in 0..(HEIGHT / 2) as usize {
        let decoded_u = u.row(row);
        let decoded_v = v.row(row);
        for column in 0..(WIDTH / 2) as usize {
            // Mirror the encoder's 2x2 box average over the source RGB.
            let mut sums = (0i32, 0i32, 0i32);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (r, g, b) = rgb(pattern[(row * 2 + dy) * WIDTH as usize + column * 2 + dx]);
                sums = (sums.0 + r, sums.1 + g, sums.2 + b);
            }
            let (r, g, b) = (sums.0 / 4, sums.1 / 4, sums.2 / 4);
            let expected_u = ((-9714 * r - 19070 * g + 28784 * b + (1 << 15)) >> 16) + 128;
            let expected_v = ((28784 * r - 24103 * g - 4681 * b + (1 << 15)) >> 16) + 128;
            worst_chroma = worst_chroma
                .max((i32::from(decoded_u[column]) - expected_u).abs())
                .max((i32::from(decoded_v[column]) - expected_v).abs());
        }
    }
    assert!(
        worst_chroma < 24,
        "decoded chroma does not match the submitted frame (worst |delta| \
         {worst_chroma}) — planes are swapped, mis-strided, or mis-sampled"
    );
}

/// Frames the throughput probe submits, and the rate it submits them at.
///
/// 70 Hz is what the old H.264 configuration declared as its ceiling, which
/// makes it the rate capture is expected to survive; 60 frames is long enough
/// for the encoder to reach steady state and short enough to stay under a
/// second.
const THROUGHPUT_FRAMES: usize = 60;
const SUBMIT_PERIOD: Duration = Duration::from_micros(14_286);

/// How close to the real-time schedule the submissions must finish. A
/// submitter waits for an encoder that cannot keep up (MDD-BUG-FLU-02234), so
/// a slow encoder shows as a submission loop that overruns its schedule. The
/// slack absorbs a scheduling hiccup on a loaded machine without letting a
/// real regression through: an encoder even 2x too slow takes twice as long,
/// nowhere near this bar.
const MIN_REALTIME_FRACTION: f64 = 0.9;

/// Whether the VP9 encoder keeps up with capture at its working resolution.
///
/// Capture waits for an encoder that is behind rather than dropping frames,
/// so a slow encoder slows the emulator in wall time while it records.
/// Nobody would notice that regressing, which is why it is worth a test.
///
/// Measured through the shipped API, with frames submitted at a realistic 70 Hz
/// rather than flat out: submitting as fast as the loop can copy buffers just
/// measures how long the encode takes. Pacing at the real rate makes the
/// overrun mean what it should — whether a 70 fps capture holds real time.
///
/// Ignored by default and calibrated for `--release` only — rvp9 leans on
/// inlining and bounds-check elision, so a debug build runs an order of
/// magnitude slower and the bar would be meaningless.
///
/// ```text
/// cargo test -p gp64-capture --release --test capture_pipeline -- \
///     --ignored --nocapture
/// ```
#[test]
#[ignore = "measurement; release-calibrated — full_test (--include-ignored)"]
fn vp9_encode_keeps_up_with_capture_frame_rates() {
    // 320x200 upscaled 4x — Mode 13h, the commonest thing this emulator
    // records, and what `capture_scale_factor` turns it into.
    const SOURCE_WIDTH: u32 = 320;
    const SOURCE_HEIGHT: u32 = 200;

    let path = std::env::temp_dir().join(format!(
        "gp64_capture_throughput_{}.mkv",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    // Content with real motion, so inter frames do actual work. A static frame
    // would encode as all-skip and measure nothing.
    let sources: Vec<Vec<u32>> = (0..THROUGHPUT_FRAMES)
        .map(|index| {
            let shift = (index * 7) as u32;
            (0..SOURCE_HEIGHT)
                .flat_map(|y| {
                    (0..SOURCE_WIDTH).map(move |x| {
                        let r = x.wrapping_add(shift) % 256;
                        let g = y.wrapping_add(shift * 2) % 256;
                        let b = (x ^ y).wrapping_add(shift) % 256;
                        (r << 16) | (g << 8) | b
                    })
                })
                .collect()
        })
        .collect();

    let mut session = CaptureSession::start(CaptureConfig {
        output_path: path.clone(),
        width: SOURCE_WIDTH,
        height: SOURCE_HEIGHT,
        sample_rate: 44100,
        channels: 2,
    })
    .expect("capture session starts");

    let start = std::time::Instant::now();
    for (index, source) in sources.iter().enumerate() {
        session.submit_frame(
            source,
            SOURCE_WIDTH,
            SOURCE_HEIGHT,
            index as u64 * SUBMIT_PERIOD.as_nanos() as u64,
        );
        // Sleep to the schedule rather than by a fixed amount, so the copy the
        // submit does and any scheduler overshoot come out of the interval
        // instead of stretching the run.
        let due = start + SUBMIT_PERIOD * (index as u32 + 1);
        if let Some(remaining) = due.checked_duration_since(std::time::Instant::now()) {
            std::thread::sleep(remaining);
        }
    }
    // The submissions alone: a submit waits only while the encoder is behind.
    let submitting = start.elapsed();
    let segments = session.stop().expect("capture session finalises");
    // One resolution throughout, so one file; stop() reports them all
    // (MDD-BUG-CRU-00814) and the count is part of what this checks.
    assert_eq!(segments.len(), 1, "expected one segment, got {segments:?}");
    let finished = &segments[0];
    let elapsed = start.elapsed();

    let mkv = std::fs::read(finished).expect("finalised capture file");
    let _ = std::fs::remove_file(finished);
    let encoded = extract_vp9_frames(&mkv);
    assert!(!encoded.is_empty(), "no frames reached the capture file");

    let schedule = SUBMIT_PERIOD * THROUGHPUT_FRAMES as u32;
    let realtime = schedule.as_secs_f64() / submitting.as_secs_f64();
    let bytes: usize = encoded.iter().map(Vec::len).sum();
    println!(
        "vp9 {}x{} at {:.0} fps: {} of {THROUGHPUT_FRAMES} frames kept, submitted \
         at {:.0}% of real time, {:.2} s including finalise, {:.0} kbit/frame",
        SOURCE_WIDTH * 4,
        SOURCE_HEIGHT * 4,
        1.0 / SUBMIT_PERIOD.as_secs_f64(),
        encoded.len(),
        realtime * 100.0,
        elapsed.as_secs_f64(),
        (bytes as f64 * 8.0 / encoded.len() as f64) / 1000.0,
    );

    assert_eq!(
        encoded.len(),
        THROUGHPUT_FRAMES,
        "every submitted frame must reach the file (MDD-BUG-FLU-02234)"
    );
    assert!(
        realtime >= MIN_REALTIME_FRACTION,
        "VP9 encode held a {:.0} fps capture at {}x{} to only {:.0}% of real \
         time — the encoder is not keeping up and is slowing the emulator",
        1.0 / SUBMIT_PERIOD.as_secs_f64(),
        SOURCE_WIDTH * 4,
        SOURCE_HEIGHT * 4,
        realtime * 100.0,
    );
}

/// Find a two-byte-ID EBML element and read its unsigned integer value.
fn find_uint_element(mkv: &[u8], id: [u8; 2]) -> Option<u64> {
    mkv.windows(2).position(|w| w == id).map(|pos| {
        let size = usize::from(mkv[pos + 2] & 0x7F);
        mkv[pos + 3..pos + 3 + size]
            .iter()
            .fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
    })
}

/// Split a 0RGB pixel into its components.
fn rgb(pixel: u32) -> (i32, i32, i32) {
    (
        ((pixel >> 16) & 0xFF) as i32,
        ((pixel >> 8) & 0xFF) as i32,
        (pixel & 0xFF) as i32,
    )
}

/// Every block in file order as `(track, absolute pts in ms)`: each
/// SimpleBlock, and each BlockGroup's Block (the muxer writes the audio tail
/// as one, to carry its DiscardPadding).
///
/// Unlike [`extract_vp9_frames`], this keeps file order across clusters and
/// resolves each block's relative timestamp against its cluster's, which is
/// what a check on gaps between frames needs.
fn extract_block_timestamps(mkv: &[u8]) -> Vec<(u64, i64)> {
    const SEGMENT: u32 = 0x1853_8067;
    const CLUSTER: u32 = 0x1F43_B675;
    const CLUSTER_TIMESTAMP: u32 = 0xE7;
    const SIMPLE_BLOCK: u32 = 0xA3;
    const BLOCK_GROUP: u32 = 0xA0;
    const BLOCK: u32 = 0xA1;

    fn walk(
        mkv: &[u8],
        mut pos: usize,
        end: usize,
        cluster_ts: &mut i64,
        out: &mut Vec<(u64, i64)>,
    ) {
        while pos + 2 < end {
            let (id, id_len) = read_id(mkv, pos);
            let (size, size_len) = read_vint(mkv, pos + id_len);
            let body = pos + id_len + size_len;
            let body_end = (body + size as usize).min(end);
            match id {
                SEGMENT | CLUSTER | BLOCK_GROUP => walk(mkv, body, body_end, cluster_ts, out),
                CLUSTER_TIMESTAMP => {
                    *cluster_ts = mkv[body..body_end]
                        .iter()
                        .fold(0i64, |acc, &b| (acc << 8) | i64::from(b));
                }
                SIMPLE_BLOCK | BLOCK => {
                    let (track, track_len) = read_vint(mkv, body);
                    let at = body + track_len;
                    let relative = i16::from_be_bytes([mkv[at], mkv[at + 1]]);
                    out.push((track, *cluster_ts + i64::from(relative)));
                }
                _ => {}
            }
            pos = body_end;
        }
    }

    let mut out = Vec::new();
    let mut cluster_ts = 0;
    walk(mkv, 0, mkv.len(), &mut cluster_ts, &mut out);
    out
}

/// A recording keeps every frame and every audio chunk when the encoder
/// falls behind (MDD-BUG-FLU-02234).
///
/// Frames go in back to back, far faster than VP9 can encode them, and a
/// burst of audio larger than the whole audio queue goes in while the encoder
/// is busy with a frame. The session used to drop whatever did not fit in the
/// 4-deep video queue or the 4096-deep audio queue, silently; now the
/// submitter waits, as the emulator does while recording. So every frame must
/// reach the file at the guest time it was stamped with, with no gap wider
/// than one submission interval, and the audio track must be as long as the
/// audio that went in.
#[test]
fn a_recording_keeps_every_frame_and_sample_when_the_encoder_falls_behind() {
    // Wide enough that capture does not upscale it, small enough to encode
    // quickly in a debug build.
    const W: u32 = 640;
    const H: u32 = 360;
    const FRAMES: u64 = 24;
    // One 70 Hz vblank, in whole guest milliseconds.
    const FRAME_MS: u64 = 14;
    // Well past the audio queue's 4096 one-tick messages.
    const AUDIO_BURST_MS: u64 = 6_000;
    const RATE: u32 = 48_000;
    const SAMPLES_PER_MS: usize = (RATE / 1000) as usize * 2;

    let path = std::env::temp_dir().join(format!(
        "gp64_capture_backpressure_{}.mkv",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let mut session = CaptureSession::start(CaptureConfig {
        output_path: path.clone(),
        width: W,
        height: H,
        sample_rate: RATE,
        channels: 2,
    })
    .expect("capture session starts");

    // Moving content, so no frame is a cheap all-skip encode.
    let frame = |index: u64| -> Vec<u32> {
        let shift = (index * 13) as u32;
        (0..H)
            .flat_map(|y| {
                (0..W).map(move |x| {
                    let r = x.wrapping_add(shift) % 256;
                    let g = y.wrapping_add(shift * 3) % 256;
                    let b = (x ^ y).wrapping_add(shift) % 256;
                    (r << 16) | (g << 8) | b
                })
            })
            .collect()
    };
    let tone: Vec<f32> = (0..SAMPLES_PER_MS)
        .map(|i| (i as f32 * 0.1).sin() * 0.25)
        .collect();

    // The first frame opens the file; the audio burst then arrives while the
    // encoder is still busy encoding it.
    session.submit_frame(&frame(0), W, H, 0);
    for ms in 0..AUDIO_BURST_MS {
        session.submit_audio(&tone, ms * 1_000_000);
    }
    for index in 1..FRAMES {
        session.submit_frame(&frame(index), W, H, index * FRAME_MS * 1_000_000);
    }
    let segments = session.stop().expect("capture session finalises");
    assert_eq!(segments.len(), 1, "expected one segment, got {segments:?}");
    let mkv = std::fs::read(&segments[0]).expect("finalised capture file");
    let _ = std::fs::remove_file(&segments[0]);

    let blocks = extract_block_timestamps(&mkv);
    let video: Vec<i64> = blocks.iter().filter(|b| b.0 == 1).map(|b| b.1).collect();
    let audio: Vec<i64> = blocks.iter().filter(|b| b.0 == 2).map(|b| b.1).collect();

    assert_eq!(
        video.len() as u64,
        FRAMES,
        "{} of {FRAMES} frames reached the file; a full queue dropped the rest \
         (video pts {video:?})",
        video.len()
    );
    let expected: Vec<i64> = (0..FRAMES).map(|i| (i * FRAME_MS) as i64).collect();
    assert_eq!(
        video, expected,
        "every frame keeps its guest-time pts, in order"
    );

    // 20 ms Opus packets; the final partial one is flushed, not dropped.
    let expected_packets = AUDIO_BURST_MS.div_ceil(20) as usize;
    assert!(
        audio.len() >= expected_packets,
        "{} Opus packets for {AUDIO_BURST_MS} ms of submitted audio, expected at \
         least {expected_packets}; a full audio queue dropped the rest",
        audio.len()
    );
    assert!(
        audio.windows(2).all(|pair| pair[1] > pair[0]),
        "audio pts must rise monotonically"
    );
    let last_audio_ms = *audio.last().expect("audio packets") as u64;
    assert!(
        last_audio_ms + 20 >= AUDIO_BURST_MS,
        "the audio track ends at {last_audio_ms} ms, short of the {AUDIO_BURST_MS} ms submitted"
    );
}

/// A submitter can never hang on an encoder that has died: once the encoder
/// thread is gone, every submit returns at once, even with both queues full,
/// and `stop` reports the failure (MDD-BUG-FLU-02234).
///
/// The encoder dies here the way a real one can, by failing to create its
/// output file when the first encoded frame arrives.
#[test]
fn a_dead_encoder_ends_the_recording_without_hanging_the_submitter() {
    const W: u32 = 640;
    const H: u32 = 360;
    let path = std::env::temp_dir()
        .join(format!("gp64_capture_no_such_dir_{}", std::process::id()))
        .join("capture.mkv");
    let mut session = CaptureSession::start(CaptureConfig {
        output_path: path,
        width: W,
        height: H,
        sample_rate: 48_000,
        channels: 2,
    })
    .expect("capture session starts; the path is only opened at the first frame");

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let submitter = std::thread::spawn(move || {
        let pixels = vec![0x0040_8020u32; (W * H) as usize];
        let samples = vec![0.0f32; 96];
        // Many times what either queue holds, so a submitter that waited for
        // a dead encoder to free a slot would wait for ever.
        for index in 0..64u64 {
            session.submit_frame(&pixels, W, H, index * 14_000_000);
        }
        for ms in 0..10_000u64 {
            session.submit_audio(&samples, ms * 1_000_000);
        }
        let result = session.stop();
        let _ = done_tx.send(result.is_err());
    });

    let stop_failed = done_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("submitting to a dead encoder hung instead of returning");
    submitter.join().expect("submitter thread");
    assert!(
        stop_failed,
        "stop must report the encoder's failure, not a recording"
    );
}
