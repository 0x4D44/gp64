#[path = "support/decode.rs"]
mod decode;
#[path = "support/guest.rs"]
mod guest;
use gp64::recording::Recorder;
use gp64_machine::{DosMachine, DosRunExit};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

#[test]
fn three_seconds_of_guest_video_and_tone_decode_and_finalise() {
    let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
    let folder = scratch.join(format!("capture-{}", std::process::id()));
    fs::create_dir_all(&folder).unwrap();
    let program = folder.join("tone.com");
    fs::write(&program, guest::program()).unwrap();
    let path = folder.join("record.mkv");
    let mut dos = DosMachine::launch(&folder, "tone.com", "").unwrap();
    let rate = dos.machine.cycles_per_second();
    let mut recorder = Recorder::new(path.clone(), &mut dos.machine);
    let start = Instant::now();
    for slice in 0..3000 {
        assert!(matches!(
            dos.run_for(rate / 1000).unwrap(),
            DosRunExit::Running
        ));
        let audio = dos.machine.take_audio();
        recorder.poll(&mut dos.machine, &audio).unwrap();
        // The headless test supplies frontend-style pacing; capture itself never waits.
        let deadline = Duration::from_millis(slice + 1);
        if let Some(delay) = deadline.checked_sub(start.elapsed()) {
            std::thread::sleep(delay);
        }
    }
    let frame_rate = rate as f64 / dos.machine.io.vga.timing_cycles_per_frame as f64;
    assert_eq!(recorder.finish().unwrap(), vec![path.clone()]);
    let summary = decode::read(&path);
    assert_eq!((summary.width, summary.height), (640, 400));
    assert!(
        (summary.video.len() as f64 - 3.0 * frame_rate).abs() <= 1.0,
        "{} frames at {frame_rate} Hz",
        summary.video.len()
    );
    assert!(summary.video.windows(2).all(|pair| {
        let delta = (pair[1].0 - pair[0].0) as f64;
        (delta - 1000.0 / frame_rate).abs() <= 1.0
    }));
    assert!(summary
        .audio
        .windows(2)
        .all(|pair| pair[1].0 - pair[0].0 == 20));
    let luma = decode::decode_video(&summary);
    assert!(
        luma.windows(2)
            .any(|pair| pair[0].abs_diff(pair[1]) > 10000),
        "frames must change"
    );
    let pcm = decode::decode_audio(&summary);
    let rms = |a: f64, b: f64| {
        let samples = &pcm[(a * 96000.0) as usize..(b * 96000.0) as usize];
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    };
    assert!(rms(0.8, 1.8) > 100.0, "tone RMS {}", rms(0.8, 1.8));
    assert!(rms(0.1, 0.35) < 10.0, "pre-tone RMS {}", rms(0.1, 0.35));
    assert!(rms(2.5, 2.8) < 10.0, "post-tone RMS {}", rms(2.5, 2.8));
    let video_ms = summary.video.last().unwrap().0 as f64 + 1000.0 / frame_rate;
    let audio_ms = pcm.len() as f64 / 96.0;
    assert!((video_ms - audio_ms).abs() < 100.0);
    assert!(summary.cues);
    assert!((summary.duration_ms - 3000.0).abs() < 100.0);
    println!("{} bytes; {} VP9 frames ({video_ms:.1} ms), {} Opus packets ({audio_ms:.1} ms), RMS tone {:.1}, before {:.1}, after {:.1}; duration {:.1} ms, cues present", fs::metadata(&path).unwrap().len(), summary.video.len(), summary.audio.len(), rms(0.8,1.8), rms(0.1,0.35), rms(2.5,2.8), summary.duration_ms);
    if std::env::var_os("GP64_KEEP_CAPTURE_FIXTURE").is_none() {
        fs::remove_file(path).unwrap();
        fs::remove_file(program).unwrap();
        fs::remove_dir(folder).unwrap();
    }
}
