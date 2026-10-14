//! Owned-media acceptance uses only launcher names and generic display properties.
use gp64_dos::filesystem::{FileAttributes, HostDrive, VirtualDrive, VirtualFile};
use gp64_machine::{launcher, DosRunExit, DEFAULT_CYCLES_PER_SECOND};
use std::{path::PathBuf, time::SystemTime};

// The owned acceptance mount never permits guest writes to the player's folder.
struct ReadOnly(HostDrive);
impl VirtualDrive for ReadOnly {
    fn open_file(
        &mut self,
        path: &str,
        write: bool,
        create: bool,
        truncate: bool,
    ) -> anyhow::Result<Box<dyn VirtualFile>> {
        anyhow::ensure!(!write && !create && !truncate, "read-only acceptance mount");
        self.0.open_file(path, false, false, false)
    }
    fn create_dir(&mut self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("read-only acceptance mount")
    }
    fn remove_file(&mut self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("read-only acceptance mount")
    }
    fn remove_dir(&mut self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("read-only acceptance mount")
    }
    fn rename(&mut self, _: &str, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("read-only acceptance mount")
    }
    fn set_attributes(&mut self, _: &str, _: FileAttributes) -> anyhow::Result<()> {
        anyhow::bail!("read-only acceptance mount")
    }
    fn set_file_time(&mut self, _: &str, _: SystemTime) -> anyhow::Result<()> {
        anyhow::bail!("read-only acceptance mount")
    }
    fn get_attributes(&mut self, path: &str) -> anyhow::Result<FileAttributes> {
        self.0.get_attributes(path)
    }
    fn read_dir(&mut self, path: &str) -> anyhow::Result<Vec<(String, FileAttributes)>> {
        self.0.read_dir(path)
    }
    fn get_disk_space(&self) -> anyhow::Result<(u64, u64)> {
        self.0.get_disk_space()
    }
}

fn assert_default_program_started(programs: &[String]) {
    assert!(
        programs
            .iter()
            .any(|name| name == launcher::DEFAULT_PROGRAM),
        "default executable did not start: {programs:?}"
    );
}

#[test]
fn launch_success_accepts_direct_and_arbitrary_batch_positions() {
    for programs in [
        vec![launcher::DEFAULT_PROGRAM.to_owned()],
        vec!["FIRST.COM".to_owned(), launcher::DEFAULT_PROGRAM.to_owned()],
        vec![
            "FIRST.COM".to_owned(),
            "SECOND.COM".to_owned(),
            launcher::DEFAULT_PROGRAM.to_owned(),
        ],
    ] {
        assert_default_program_started(&programs);
    }
}

#[test]
#[should_panic(expected = "default executable did not start")]
fn launch_success_rejects_a_missing_default_program() {
    assert_default_program_started(&["FIRST.COM".to_owned(), "SECOND.COM".to_owned()]);
}

#[test]
#[ignore = "requires GP64_GAME_DIR; run explicitly with --ignored"]
fn owned_default_launch_has_stable_graphics_and_keyboard_response() {
    let Some(folder) = std::env::var_os("GP64_GAME_DIR") else {
        return;
    };
    let folder = PathBuf::from(folder);
    let mut pc = launcher::launch_default(&folder).unwrap();
    pc.dos
        .drives
        .insert(2, Box::new(ReadOnly(HostDrive::new(folder))));
    assert_eq!(pc.machine.cycles_per_second(), DEFAULT_CYCLES_PER_SECOND);
    const STEADY: u64 = 120;
    const GRAPHICS_WARMUP: u64 = 140;
    let mut previous = Vec::new();
    let mut observed = 0;
    let mut graphics = 0;
    let mut steady = 0;
    let mut startup_key = None;
    let mut startup_released = false;
    let mut graphics_start = None;
    let limit = DEFAULT_CYCLES_PER_SECOND * 120;
    while pc.machine.cycle() < limit {
        let result = pc.run_for(DEFAULT_CYCLES_PER_SECOND / 1000).unwrap();
        assert!(
            matches!(result, DosRunExit::Running),
            "unexpected stop: {result:?}"
        );
        pc.machine.take_audio();
        let frame = pc.machine.completed_frames();
        if frame == observed {
            continue;
        }
        observed = frame;
        if observed % 1000 == 0 {
            eprintln!("completed frames={observed}, graphics frames={graphics}, steady={steady}, programs started={:?}", pc.programs_started());
        }
        if !pc
            .programs_started()
            .iter()
            .any(|name| name == launcher::DEFAULT_PROGRAM)
            || pc.machine.io.vga.is_text_mode()
        {
            graphics = 0;
            graphics_start = None;
            steady = 0;
            continue;
        }
        graphics += 1;
        // DOS applications may wait for input during startup. Offer one generic
        // Enter after ten seconds of completed graphics, without inspecting UI.
        let start = *graphics_start.get_or_insert(pc.machine.cycle());
        if pc.machine.cycle() - start >= DEFAULT_CYCLES_PER_SECOND * 10 && startup_key.is_none() {
            pc.key_down(0x1c);
            startup_key = Some(frame);
        }
        if !startup_released && startup_key.is_some_and(|pressed| frame >= pressed + 7) {
            pc.key_up(0x1c);
            startup_released = true;
        }
        let pixels = pc.machine.io.vga.completed_framebuffer();
        let nonblank =
            pixels.iter().any(|&rgb| rgb != 0) && pixels.windows(2).any(|p| p[0] != p[1]);
        if graphics >= GRAPHICS_WARMUP && nonblank && pixels == previous {
            steady += 1
        } else {
            steady = 0
        }
        previous = pixels.to_vec();
        if steady >= STEADY {
            break;
        }
    }
    assert!(steady >= STEADY, "no steady graphics: frames={observed}, graphics={graphics}, steady={steady}, programs={:?}", pc.programs_started());
    assert_default_program_started(pc.programs_started());
    let frames_waited = observed;
    eprintln!("stable frame after {frames_waited} completed frames; steady={steady}");
    pc.key_down(0x4d); // XT Right make code.
    let release_at = pc.machine.cycle() + DEFAULT_CYCLES_PER_SECOND / 10;
    let limit = pc.machine.cycle() + DEFAULT_CYCLES_PER_SECOND * 10;
    let mut released = false;
    let mut changed = false;
    while pc.machine.cycle() < limit {
        let result = pc.run_for(DEFAULT_CYCLES_PER_SECOND / 1000).unwrap();
        assert!(
            matches!(result, DosRunExit::Running),
            "unexpected stop: {result:?}"
        );
        pc.machine.take_audio();
        if pc.machine.cycle() >= release_at && !released {
            pc.key_up(0x4d);
            released = true;
        }
        if pc.machine.completed_frames() > observed {
            observed = pc.machine.completed_frames();
            changed |= pc.machine.io.vga.completed_framebuffer() != previous;
        }
        if changed && released {
            break;
        }
    }
    assert!(changed, "keyboard did not change completed graphics frame");
    eprintln!("frames waited={frames_waited}, steady frames={steady}, response frames={}, programs started={:?}", observed - frames_waited, pc.programs_started());
}
