//! Synthetic VGA changes retained within one frontend polling interval.
#[path = "support/decode.rs"]
mod decode;
#[path = "support/guest.rs"]
mod guest;
use gp64::recording::Recorder;
use gp64_machine::{AudioSample, DosMachine, DosRunExit};
use std::{fs, path::PathBuf};

fn next_frame(dos: &mut DosMachine, audio: &mut Vec<AudioSample>) {
    let start = dos.machine.completed_frames();
    let rate = dos.machine.cycles_per_second();
    for _ in 0..100 {
        assert!(matches!(
            dos.run_for(rate / 1000).unwrap(),
            DosRunExit::Running
        ));
        audio.extend(dos.machine.take_audio());
        if dos.machine.completed_frames() > start {
            return;
        }
    }
    panic!("synthetic VGA frame did not complete");
}

#[test]
fn one_poll_routes_audio_to_each_completed_resolution_segment() {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.scratch")
        .join(format!("poll-rollover-{}", std::process::id()));
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("tone.com"), guest::program()).unwrap();
    let mut dos = DosMachine::launch(&folder, "tone.com", "").unwrap();
    dos.run_for(dos.machine.cycles_per_second() / 1000).unwrap();
    assert_eq!(dos.machine.io.vga.mode(), 0x13);
    dos.machine.take_audio();
    let mut recorder = Recorder::new(folder.join("capture.mkv"), &mut dos.machine);
    let mut audio = Vec::new();
    next_frame(&mut dos, &mut audio);
    assert!(dos.machine.io.vga.set_mode_bios(0x10));
    next_frame(&mut dos, &mut audio);
    assert!(dos.machine.io.vga.set_mode_bios(0x13));
    next_frame(&mut dos, &mut audio);

    // All mode boundaries and the audio that spans them reach the real poll
    // together; encoder scheduling must not decide which file gets the audio.
    recorder.poll(&mut dos.machine, &audio).unwrap();
    let paths = recorder.finish().unwrap();
    let summaries: Vec<_> = paths.iter().map(|path| decode::read(path)).collect();
    for path in paths {
        fs::remove_file(path).unwrap();
    }
    fs::remove_file(folder.join("tone.com")).unwrap();
    fs::remove_dir(folder).unwrap();

    assert_eq!(summaries.len(), 3);
    assert_eq!(
        summaries
            .iter()
            .map(|s| (s.width, s.height))
            .collect::<Vec<_>>(),
        [(320, 200), (640, 350), (320, 200)]
    );
    for (index, summary) in summaries.iter().enumerate() {
        assert!(!summary.audio.is_empty(), "no audio in segment {index}");
        assert!(!decode::decode_audio(summary).is_empty());
        if index > 0 {
            let boundary = summary.video[0].0;
            assert!(
                summary
                    .audio
                    .iter()
                    .all(|(timestamp, _)| *timestamp >= boundary),
                "audio before rollover boundary in segment {index}"
            );
        }
        if summary.height == 200 {
            assert_eq!(decode::decode_video(summary).len(), 1);
        }
    }
}
