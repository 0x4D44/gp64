use gp64_capture::encoder::{CaptureConfig, CaptureSession, ReservedCaptureOutput};
use std::{fs, path::PathBuf};

fn fixture(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.scratch")
        .join(format!("no-clobber-{name}-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    dir
}
fn config(path: PathBuf) -> CaptureConfig {
    CaptureConfig {
        output_path: path,
        width: 64,
        height: 64,
        sample_rate: 44100,
        channels: 2,
    }
}
fn start_menu(path: PathBuf) -> CaptureSession {
    let output = ReservedCaptureOutput::reserve(&path, None).unwrap();
    CaptureSession::start_live_reserved(config(path), output).unwrap()
}
fn assert_video(path: &std::path::Path) {
    let bytes = fs::read(path).unwrap();
    assert_eq!(&bytes[..4], &[0x1a, 0x45, 0xdf, 0xa3]);
    assert!(bytes.len() > 200);
}
#[test]
fn menu_rollover_preserves_existing_suffix_bytes() {
    let dir = fixture("rollover");
    let base = dir.join("TONE.mkv");
    let sentinel = dir.join("TONE-2.mkv");
    fs::write(&sentinel, b"existing rollover sentinel").unwrap();
    let mut session = start_menu(base.clone());
    session.submit_frame(&vec![0x2040; 64 * 64], 64, 64, 0);
    session.submit_frame(&vec![0x4020; 128 * 64], 128, 64, 33_000_000);
    let paths = session.stop().unwrap();
    let preserved = fs::read(&sentinel).unwrap();
    for path in &paths {
        assert_video(path);
    }
    for path in &paths {
        if path != &sentinel {
            fs::remove_file(path).unwrap();
        }
    }
    fs::remove_file(&sentinel).unwrap();
    fs::remove_dir(dir).unwrap();
    assert_eq!(
        preserved, b"existing rollover sentinel",
        "rollover must preserve existing bytes"
    );
    assert_eq!(paths[1].file_name().unwrap(), "TONE-2 (2).mkv");
}
#[test]
fn explicit_cli_output_keeps_overwrite_policy() {
    let dir = fixture("cli");
    let base = dir.join("explicit.mkv");
    fs::write(&base, b"CLI replacement sentinel").unwrap();
    let mut session = CaptureSession::start_live(config(base.clone())).unwrap();
    session.submit_frame(&vec![0x2040; 64 * 64], 64, 64, 0);
    let paths = session.stop().unwrap();
    let bytes = fs::read(&base).unwrap();
    for path in &paths {
        if path != &base {
            fs::remove_file(path).unwrap();
        }
    }
    fs::remove_file(&base).unwrap();
    fs::remove_dir(dir).unwrap();
    assert_eq!(
        &bytes[..4],
        &[0x1a, 0x45, 0xdf, 0xa3],
        "CLI must replace the explicit file"
    );
    assert!(bytes.len() > 200);
    assert_eq!(paths, vec![base]);
}

#[test]
fn reserved_handle_survives_first_geometry_mismatch_and_path_replacement() {
    let dir = fixture("owned-geometry");
    let base = dir.join("TONE.mkv");
    let held = dir.join("owned.mkv");
    let unused = dir.join("config.mkv");
    let output = ReservedCaptureOutput::reserve(&base, None).unwrap();
    // Renaming the reserved inode and replacing its old path detects reopening.
    fs::rename(&base, &held).unwrap();
    fs::write(&base, b"replacement sentinel").unwrap();
    let mut session = CaptureSession::start_live_reserved(config(unused.clone()), output).unwrap();
    session.submit_frame(&vec![0x2040; 128 * 64], 128, 64, 0);
    session.submit_frame(&vec![0x4020; 192 * 64], 192, 64, 33_000_000);
    let paths = session.stop().unwrap();
    let held_bytes = fs::read(&held).unwrap();
    let replacement = fs::read(&base).unwrap();
    let unexpected_config = unused.exists();
    for path in &paths {
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    fs::remove_file(&held).unwrap();
    if base.exists() {
        fs::remove_file(&base).unwrap();
    }
    if unused.exists() {
        fs::remove_file(&unused).unwrap();
    }
    fs::remove_dir(dir).unwrap();
    assert!(
        held_bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
        "encoder must write the owned reserved inode"
    );
    assert_eq!(
        replacement, b"replacement sentinel",
        "first muxer must not reopen the reserved pathname"
    );
    assert!(
        !unexpected_config,
        "reserved path must override config path"
    );
    assert_eq!(
        paths,
        vec![base.clone(), base.with_file_name("TONE-2.mkv")],
        "first geometry mismatch must use reserved base before rollover numbering"
    );
}
#[test]
fn menu_rollover_skips_missing_explicit_launch_path() {
    let dir = fixture("excluded");
    let base = dir.join("TONE.mkv");
    let launch = dir.join("TONE-2.mkv");
    let output = ReservedCaptureOutput::reserve(&base, Some(&launch)).unwrap();
    let mut session = CaptureSession::start_live_reserved(config(base.clone()), output).unwrap();
    session.submit_frame(&vec![0x2040; 64 * 64], 64, 64, 0);
    session.submit_frame(&vec![0x4020; 128 * 64], 128, 64, 33_000_000);
    let paths = session.stop().unwrap();
    let launch_created = launch.exists();
    for path in &paths {
        fs::remove_file(path).unwrap();
    }
    fs::remove_dir(dir).unwrap();
    assert!(
        !launch_created,
        "menu rollover must never take the explicit launch path"
    );
    assert_eq!(paths[1].file_name().unwrap(), "TONE-2 (2).mkv");
}
