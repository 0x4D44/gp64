use gp64::recording::Recording;
use gp64_machine::Machine;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
};
fn fixture(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.scratch")
        .join(format!("menu-allocation-{name}-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    dir
}
fn start(recording: &mut Recording, dir: &Path, machine: &mut Machine) -> PathBuf {
    recording
        .start_menu(dir, (2026, 1, 2), "TONE", None, machine)
        .unwrap()
}
#[test]
fn menu_base_collision_preserves_bytes_and_reserves_numbered_file() {
    let dir = fixture("base");
    let base = dir.join("2026.01.02 - TONE.mkv");
    fs::write(&base, b"existing base sentinel").unwrap();
    let mut recording = Recording::default();
    let mut machine = Machine::new();
    let path = start(&mut recording, &dir, &mut machine);
    let reserved = path.exists();
    let bytes = fs::read(&base).unwrap();
    recording.finish();
    if path.exists() {
        fs::remove_file(&path).unwrap();
    }
    fs::remove_file(base).unwrap();
    fs::remove_dir(dir).unwrap();
    assert_eq!(bytes, b"existing base sentinel");
    assert_eq!(path.file_name().unwrap(), "2026.01.02 - TONE (2).mkv");
    assert!(reserved, "menu base must be reserved before first frame");
}
#[test]
fn stop_restart_before_first_frame_reserves_distinct_files() {
    let dir = fixture("restart");
    let mut machine = Machine::new();
    let mut recording = Recording::default();
    let first = start(&mut recording, &dir, &mut machine);
    recording.stop(&mut machine);
    let second = start(&mut recording, &dir, &mut machine);
    recording.finish();
    for path in [&first, &second] {
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    fs::remove_dir(dir).unwrap();
    assert_ne!(
        first, second,
        "restart before first frame must not reuse reservation"
    );
}
#[test]
fn concurrent_menu_writers_reserve_distinct_files() {
    let dir = fixture("concurrent");
    let barrier = Arc::new(Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let dir = dir.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut recording = Recording::default();
                let mut machine = Machine::new();
                barrier.wait();
                let path = start(&mut recording, &dir, &mut machine);
                barrier.wait();
                recording.finish();
                path
            })
        })
        .collect();
    let mut paths: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    paths.sort();
    paths.dedup();
    for path in &paths {
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    fs::remove_dir(dir).unwrap();
    assert_eq!(
        paths.len(),
        4,
        "concurrent writers must own distinct reserved files"
    );
}
