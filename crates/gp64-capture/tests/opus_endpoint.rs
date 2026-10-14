//! Endpoint oracle for the actual recording encoder, independent of timestamps.
use gp64_capture::encoder::{CaptureConfig, CaptureSession};
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new(frames: usize) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch")
            .join(format!(
                "opus_endpoint_{}_{}_{}",
                std::process::id(),
                frames,
                unique
            ));
        std::fs::create_dir_all(directory.parent().unwrap()).unwrap();
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let path = self.0.join("capture.mkv");
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
        std::fs::remove_dir(&self.0).unwrap();
    }
}

fn vint(bytes: &[u8], pos: usize, strip: bool) -> (u64, usize) {
    let width = bytes[pos].leading_zeros() as usize + 1;
    assert!(width <= 8);
    let first = if strip {
        bytes[pos] & (0xffu8.checked_shr(width as u32).unwrap_or(0))
    } else {
        bytes[pos]
    };
    (
        bytes[pos + 1..pos + width]
            .iter()
            .fold(u64::from(first), |v, b| v * 256 + u64::from(*b)),
        width,
    )
}
fn uint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |v, b| v * 256 + u64::from(*b))
}
#[derive(Default)]
struct Audio {
    pre_skip: usize,
    delay_ns: u64,
    packets: Vec<(Vec<u8>, u64)>,
}
fn walk(bytes: &[u8], audio: &mut Audio) {
    let mut pos = 0;
    while pos < bytes.len() {
        let (id, ni) = vint(bytes, pos, false);
        let (size, ns) = vint(bytes, pos + ni, true);
        let body = pos + ni + ns;
        let end = body + size as usize;
        assert!(end <= bytes.len());
        let data = &bytes[body..end];
        match id {
            0x18538067 | 0x1654ae6b | 0xae | 0x1f43b675 => walk(data, audio),
            0x63a2 if data.starts_with(b"OpusHead") => {
                audio.pre_skip = usize::from(u16::from_le_bytes([data[10], data[11]]))
            }
            0x56aa => audio.delay_ns = uint(data),
            0xa0 => {
                let mut group = Audio::default();
                walk(data, &mut group);
                assert_eq!(group.packets.len(), 1);
                audio.packets.extend(group.packets);
            }
            0x75a2 => {
                assert_eq!(data[0] & 0x80, 0, "positive end padding only");
                audio.packets.last_mut().unwrap().1 = uint(data);
            }
            0xa3 | 0xa1 => {
                let (track, n) = vint(data, 0, true);
                assert_eq!(data[n + 2] & 0x06, 0, "no lacing");
                if track == 2 {
                    audio.packets.push((data[n + 3..].to_vec(), 0));
                }
            }
            _ => {}
        }
        pos = end;
    }
}
fn presented(audio: &Audio) -> Vec<f32> {
    assert_eq!(
        audio.delay_ns,
        audio.pre_skip as u64 * 1_000_000_000 / 48_000
    );
    let mut decoder = ropus::Decoder::new(48_000, ropus::Channels::Stereo).unwrap();
    let mut buffer = vec![0.0; 5760 * 2];
    let mut pcm = Vec::new();
    for (packet, padding_ns) in &audio.packets {
        let frames = decoder
            .decode_float(packet, &mut buffer, ropus::DecodeMode::Normal)
            .unwrap();
        // The writer truncates sample durations to ns: round back to frames.
        let discard = ((padding_ns * 48_000 + 500_000_000) / 1_000_000_000) as usize;
        assert!(discard <= frames);
        pcm.extend_from_slice(&buffer[..(frames - discard) * 2]);
    }
    // CodecDelay and OpusHead describe the same start trim, applied once.
    if !pcm.is_empty() {
        pcm.drain(..(audio.pre_skip * 2).min(pcm.len()));
    }
    pcm
}

fn check_recordings(inputs: &[usize], check_length: bool) {
    // Empty stream, held interpolation frame, short/partial, exact packets,
    // residual+lookahead exactly one packet, and residual requiring two.
    for &input_frames in inputs {
        let fixture = Fixture::new(input_frames);
        let path = fixture.0.join("capture.mkv");
        let expected_frames = input_frames.saturating_sub(1);
        let mut samples = Vec::new();
        for frame in 0..input_frames {
            let value = if frame >= expected_frames.saturating_sub(192) {
                0.6 * (std::f32::consts::TAU * 1000.0 * frame as f32 / 48_000.0).sin()
            } else {
                0.0
            };
            samples.extend_from_slice(&[value, value]);
        }
        let mut session = CaptureSession::start_live(CaptureConfig {
            output_path: path.clone(),
            width: 64,
            height: 64,
            sample_rate: 48_000,
            channels: 2,
        })
        .unwrap();
        session.submit_frame(&vec![0; 64 * 64], 64, 64, 0);
        if input_frames > 0 {
            session.submit_audio(&samples, 0);
        }
        session.stop().unwrap();
        let mut audio = Audio::default();
        walk(&std::fs::read(path).unwrap(), &mut audio);
        let pcm = presented(&audio);
        if check_length {
            assert_eq!(
                pcm.len() / 2,
                expected_frames,
                "presented endpoint for {input_frames} input frames"
            );
        }
        if expected_frames == 0 {
            assert!(audio.packets.is_empty(), "no artificial delay-only audio");
        }
        if expected_frames >= 960 {
            let end = pcm.len() / 2 - 32; // away from the held interpolation endpoint
            let start = end - 96;
            let energy = pcm[start * 2..end * 2].iter().map(|x| x * x).sum::<f32>() / 192.0;
            assert!(
                energy > 0.03,
                "final synthetic marker missing for {input_frames} frames: {energy}"
            );
        }
    }
}

#[test]
fn shutdown_without_resampled_audio_writes_no_delay_only_packet() {
    check_recordings(&[0, 1], true);
}
#[test]
fn shutdown_preserves_short_and_partial_audio() {
    check_recordings(&[2, 241], true);
}
#[test]
fn shutdown_preserves_exact_packet_audio_and_marker() {
    check_recordings(&[961, 1921], true);
}
#[test]
fn shutdown_preserves_near_full_tail_audio_and_marker() {
    check_recordings(&[2570, 2879, 2880], true);
}
#[test]
fn shutdown_preserves_tail_plus_delay_at_packet_boundary() {
    check_recordings(&[2569], true);
}

#[test]
fn shutdown_retains_final_synthetic_marker() {
    check_recordings(&[961, 2570], false);
}
