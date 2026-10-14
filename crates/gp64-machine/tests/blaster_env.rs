//! The guest environment advertises the emulated sound card.
use gp64_machine::DosMachine;
use std::{fs, path::PathBuf};

#[test]
fn guest_environment_carries_blaster_variable() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.scratch")
        .join(format!("blaster-env-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    // mov ax,4c00h; int 21h
    fs::write(dir.join("ENV.COM"), [0xb8, 0, 0x4c, 0xcd, 0x21]).unwrap();
    let pc = DosMachine::launch(&dir, "ENV.COM", "").unwrap();
    fs::remove_file(dir.join("ENV.COM")).unwrap();
    fs::remove_dir(&dir).unwrap();
    let base = usize::from(pc.dos.env_segment()) * 16;
    let block = &pc.machine.memory.ram[base..base + 2048];
    let needle = b"BLASTER=A220 I7 D1 T3\0";
    assert!(block.windows(needle.len()).any(|window| window == needle));
}
