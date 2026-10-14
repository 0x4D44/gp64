//! Recordings started and stopped while the guest keeps running.
#[path = "support/decode.rs"]
mod decode;
#[path = "support/guest.rs"]
mod guest;
use gp64::recording::{Finaliser, Recording, Status};
use gp64_machine::{DosMachine, DosRunExit};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Runs `ms` of guest time in 1 ms slices, paced to real time like the
/// frontend, feeding the recording.
fn run(dos: &mut DosMachine, recording: &mut Recording, ms: u64) {
    let rate = dos.machine.cycles_per_second();
    let start = Instant::now();
    for slice in 0..ms {
        assert!(matches!(
            dos.run_for(rate / 1000).unwrap(),
            DosRunExit::Running
        ));
        let audio = dos.machine.take_audio();
        recording.poll(&mut dos.machine, &audio).unwrap();
        if let Some(delay) = Duration::from_millis(slice + 1).checked_sub(start.elapsed()) {
            std::thread::sleep(delay);
        }
    }
}

fn check(path: &Path, frame_rate: f64) {
    let summary = decode::read(path);
    assert_eq!((summary.width, summary.height), (320, 200));
    // Time starts at the recording's own start, not the guest's.
    assert!(
        summary.video[0].0 < 50,
        "first frame at {} ms",
        summary.video[0].0
    );
    assert!(
        (summary.video.len() as f64 - frame_rate).abs() <= 2.0,
        "{} frames",
        summary.video.len()
    );
    assert!(
        (summary.duration_ms - 1000.0).abs() < 100.0,
        "{}",
        summary.duration_ms
    );
    assert!(summary.cues);
    // Audio from the recording's start too: about one second of 48 kHz stereo.
    let audio_ms = decode::decode_audio(&summary).len() as f64 / 96.0;
    assert!((audio_ms - 1000.0).abs() < 100.0, "{audio_ms} ms of audio");
    assert!(decode::decode_video(&summary)
        .windows(2)
        .any(|pair| pair[0].abs_diff(pair[1]) > 10000));
}

#[test]
fn menu_recordings_start_mid_run_stop_without_waiting_and_restart() {
    let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
    let folder = scratch.join(format!("live-capture-{}", std::process::id()));
    let videos = folder.join("videos");
    fs::create_dir_all(&videos).unwrap();
    fs::write(folder.join("tone.com"), guest::program()).unwrap();
    let mut dos = DosMachine::launch(&folder, "tone.com", "").unwrap();
    let frame_rate =
        dos.machine.cycles_per_second() as f64 / dos.machine.io.vga.timing_cycles_per_frame as f64;
    let status = Status::default();
    let mut recording = Recording::new(status.clone());

    // The guest runs a while before anything records.
    run(&mut dos, &mut recording, 300);
    assert!(dos.machine.take_completed_frames().is_empty());
    let first = recording
        .start_menu(&videos, (2026, 1, 2), "TONE", None, &mut dos.machine)
        .unwrap();
    // Recorder.poll must transfer the owned inode instead of reopening its name.
    let owned = videos.join("owned-first.mkv");
    fs::rename(&first, &owned).unwrap();
    fs::write(&first, b"replacement menu sentinel").unwrap();
    assert_eq!(status.get(), Some(0));
    run(&mut dos, &mut recording, 1000);
    let elapsed = status.get().unwrap();
    assert!((990..=1000).contains(&elapsed), "{elapsed}");

    assert!(recording.stop(&mut dos.machine));
    assert_eq!(status.get(), None);
    assert!(!recording.stop(&mut dos.machine));
    // The tap is off at once: frames no longer pile up for a recorder.
    run(&mut dos, &mut recording, 100);
    assert!(dos.machine.take_completed_frames().is_empty());
    let mut saved = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while saved.is_empty() {
        assert!(Instant::now() < deadline, "finaliser never completed");
        saved = recording.reap();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(saved.pop().unwrap().unwrap(), vec![first.clone()]);
    assert!(
        fs::read(&first).unwrap() == b"replacement menu sentinel",
        "menu Recorder must transfer its reserved file to the encoder"
    );
    check(&owned, frame_rate);

    // A second start gets a new numbered file and re-enables the tap.
    let second = recording
        .start_menu(&videos, (2026, 1, 2), "TONE", None, &mut dos.machine)
        .unwrap();
    assert_eq!(second, videos.join("2026.01.02 - TONE (2).mkv"));
    run(&mut dos, &mut recording, 1000);
    let results = recording.finish();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results.into_iter().next().unwrap().unwrap(),
        vec![second.clone()]
    );
    check(&second, frame_rate);

    if std::env::var_os("GP64_KEEP_CAPTURE_FIXTURE").is_none() {
        fs::remove_file(first).unwrap();
        fs::remove_file(owned).unwrap();
        fs::remove_file(second).unwrap();
        fs::remove_dir(videos).unwrap();
        fs::remove_file(folder.join("tone.com")).unwrap();
        fs::remove_dir(folder).unwrap();
    }
}

#[test]
fn stop_hands_finalisation_to_another_thread() {
    let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
    let folder = scratch.join(format!("live-stop-{}", std::process::id()));
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("tone.com"), guest::program()).unwrap();
    let mut dos = DosMachine::launch(&folder, "tone.com", "").unwrap();
    let path = folder.join("stop.mkv");
    let mut recorder = gp64::recording::Recorder::new(path.clone(), &mut dos.machine);
    let rate = dos.machine.cycles_per_second();
    for _ in 0..200 {
        dos.run_for(rate / 1000).unwrap();
        let audio = dos.machine.take_audio();
        recorder.poll(&mut dos.machine, &audio).unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    let finaliser = recorder.stop(&mut dos.machine);
    assert!(matches!(finaliser, Finaliser::Running(_)));
    // The caller keeps running the guest while the file is finalised.
    dos.run_for(rate / 100).unwrap();
    assert!(dos.machine.take_completed_frames().is_empty());
    assert_eq!(finaliser.join().unwrap(), vec![path.clone()]);
    assert!(decode::read(&path).cues);
    fs::remove_file(path).unwrap();
    fs::remove_file(folder.join("tone.com")).unwrap();
    fs::remove_dir(folder).unwrap();
}
