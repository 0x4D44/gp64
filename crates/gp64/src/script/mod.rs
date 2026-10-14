//! `--script`: runs an emuscru-style script on the emulation thread.
//!
//! The runner is polled between slices of guest time and acts only when the
//! guest's frame count says it is due, so it never blocks the loop. Timing
//! follows emuscru: a frame cursor advances by each `wait` and by the frames
//! a key or click is held, so later steps keep their spacing even when the
//! guest completes several frames in one slice. A step that finds the guest
//! past the cursor runs at once.
mod keys;
mod parse;
mod png;

use anyhow::{Context, Result};
use parse::{Command, MouseButton};
use std::{collections::VecDeque, path::Path};

pub use parse::Script;

/// Frames a key or button stays down, as emuscru holds it.
const HOLD: u64 = 2;
/// Frames between characters of `type`.
const TYPE_GAP: u64 = 2;
/// Frames `wait-stable` allows when the script names none.
const STABLE_TIMEOUT: u32 = 300;
/// Equal hashes in a row that make a screen stable.
const STABLE_THRESHOLD: u32 = 3;
/// Frames between `wait-stable` samples.
const STABLE_INTERVAL: u64 = 10;

/// Reads and parses a script file.
pub fn load(path: &Path) -> Result<Script> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read script {}", path.display()))?;
    parse::parse(&text).with_context(|| format!("script {}", path.display()))
}

/// What the runner needs from the machine.
pub trait Guest {
    /// Frames completed since start.
    fn frame(&self) -> u64;
    fn key(&mut self, bytes: &[u8], index: usize, down: bool);
    /// Puts the pointer over pixel `(x, y)` of the displayed frame.
    fn place(&mut self, x: i32, y: i32);
    fn button(&mut self, button: u8, down: bool);
    /// The displayed frame as `0RGB` pixels, width and height.
    fn picture(&self) -> (&[u32], usize, usize);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Poll {
    Running,
    Finished,
    Failed(String),
}

enum Op {
    Until(u64),
    Key(keys::KeyDef, bool),
    Place(i32, i32),
    Button(MouseButton, bool),
    Stable(Stable),
}

pub struct Runner {
    script: Script,
    pc: usize,
    step: usize,
    cursor: Option<u64>,
    ops: VecDeque<Op>,
}
impl Runner {
    pub fn new(script: Script) -> Self {
        Self {
            script,
            pc: 0,
            step: 0,
            cursor: None,
            ops: VecDeque::new(),
        }
    }

    /// Runs every step that is due at the guest's current frame.
    pub fn poll(&mut self, guest: &mut impl Guest) -> Poll {
        let frame = guest.frame();
        self.cursor.get_or_insert(frame);
        loop {
            if let Some(op) = self.ops.front_mut() {
                let settled = match op {
                    Op::Until(target) if frame < *target => return Poll::Running,
                    Op::Until(_) => None,
                    Op::Key(key, down) => {
                        let (bytes, index) = key.bytes(*down);
                        guest.key(&bytes, index, *down);
                        None
                    }
                    Op::Place(x, y) => {
                        guest.place(*x, *y);
                        None
                    }
                    Op::Button(button, down) => {
                        guest.button(button.number(), *down);
                        None
                    }
                    Op::Stable(stable) => match stable.check(frame, || hash(guest)) {
                        None => return Poll::Running,
                        settled => settled,
                    },
                };
                self.ops.pop_front();
                match settled {
                    None => {}
                    Some(Ok(message)) => {
                        tracing::info!(target: "gp64::script", "[{:03}] {message}", self.step);
                        self.cursor = Some(frame);
                    }
                    Some(Err(message)) => return self.fail(message),
                }
                continue;
            }
            let Some((line, command)) = self.script.commands.get(self.pc).cloned() else {
                tracing::info!(
                    target: "gp64::script",
                    "script finished after {} steps at frame {frame}",
                    self.step
                );
                return Poll::Finished;
            };
            self.pc += 1;
            self.step += 1;
            match self.start(&command, frame, guest) {
                Ok(note) => tracing::info!(
                    target: "gp64::script",
                    "[{:03}] {command} (line {line}) at frame {frame}{note}",
                    self.step
                ),
                Err(error) => {
                    return self.fail(format!("{command} at line {line}: {error:#}"));
                }
            }
        }
    }

    fn fail(&mut self, message: String) -> Poll {
        tracing::warn!(target: "gp64::script", "[{:03}] {message}", self.step);
        self.pc = self.script.commands.len();
        self.ops.clear();
        Poll::Failed(message)
    }

    /// Advances the cursor by `frames` and waits for it.
    fn hold(&mut self, frames: u64) {
        let cursor = self.cursor.get_or_insert(0);
        *cursor += frames;
        self.ops.push_back(Op::Until(*cursor));
    }

    fn press(&mut self, key: keys::KeyDef) {
        self.ops.push_back(Op::Key(key, true));
        self.hold(HOLD);
        self.ops.push_back(Op::Key(key, false));
    }

    fn click(&mut self, button: MouseButton) {
        self.ops.push_back(Op::Button(button, true));
        self.hold(HOLD);
        self.ops.push_back(Op::Button(button, false));
    }

    /// Queues or performs `command`; the text is appended to its log line.
    fn start(&mut self, command: &Command, frame: u64, guest: &mut impl Guest) -> Result<String> {
        match command {
            Command::Ignored(_) => return Ok(" (ignored by gp64)".into()),
            Command::Key { key, .. } => self.press(*key),
            Command::Type(text) => {
                for ch in text.chars() {
                    let Some(strokes) = keys::type_char(ch) else {
                        tracing::warn!(target: "gp64::script", "type: no key for {ch:?}; skipped");
                        continue;
                    };
                    for (key, down) in strokes {
                        self.ops.push_back(Op::Key(key, down));
                    }
                    self.hold(TYPE_GAP);
                }
            }
            Command::MouseClick { x, y, button } => {
                self.ops.push_back(Op::Place(*x, *y));
                self.click(*button);
            }
            Command::MouseDoubleClick { x, y, button } => {
                self.ops.push_back(Op::Place(*x, *y));
                for _ in 0..2 {
                    self.click(*button);
                    self.hold(HOLD);
                }
            }
            Command::MouseDown { x, y, button } => {
                self.ops.push_back(Op::Place(*x, *y));
                self.ops.push_back(Op::Button(*button, true));
            }
            Command::MouseUp(button) => self.ops.push_back(Op::Button(*button, false)),
            Command::Wait(frames) => self.hold(u64::from(*frames)),
            Command::WaitStable(timeout) => self.ops.push_back(Op::Stable(Stable::new(
                frame,
                timeout.unwrap_or(STABLE_TIMEOUT),
            ))),
            Command::Screenshot(path) => {
                let path = match path {
                    Some(path) => path
                        .replace("{step}", &format!("{:03}", self.step))
                        .replace("{frame}", &frame.to_string()),
                    None => format!("exec_frame{frame}.png"),
                };
                let path = Path::new(&path);
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("failed to create directory {}", parent.display())
                    })?;
                }
                let (pixels, width, height) = guest.picture();
                std::fs::write(path, png::encode(pixels, width, height))
                    .with_context(|| format!("failed to write {}", path.display()))?;
                return Ok(format!(": saved to {} ({width}x{height})", path.display()));
            }
            Command::FrameHash => {
                let (_, width, height) = guest.picture();
                return Ok(format!(
                    ": crc=0x{:08X} ({width}x{height}, frame {frame})",
                    hash(guest)
                ));
            }
            Command::Log(message) => return Ok(format!(": {message}")),
            Command::Label(_) => {}
            Command::SkipTo(label) => {
                self.pc = self.script.labels[label];
            }
        }
        Ok(String::new())
    }
}

fn hash(guest: &impl Guest) -> u32 {
    gp64_types::util::framebuffer_crc32(guest.picture().0)
}

/// emuscru's `wait-stable`: sample the frame hash every [`STABLE_INTERVAL`]
/// frames until [`STABLE_THRESHOLD`] samples in a row agree, failing once
/// the timeout's frames have passed.
struct Stable {
    start: u64,
    timeout: u32,
    deadline: u64,
    next: u64,
    last: Option<u32>,
    streak: u32,
    best: u32,
    distinct: u32,
}
impl Stable {
    fn new(start: u64, timeout: u32) -> Self {
        Self {
            start,
            timeout,
            deadline: start + u64::from(timeout),
            next: start + STABLE_INTERVAL,
            last: None,
            streak: 0,
            best: 0,
            distinct: 0,
        }
    }

    /// `None` while waiting; then the outcome's log text.
    fn check(&mut self, frame: u64, hash: impl FnOnce() -> u32) -> Option<Result<String, String>> {
        if frame >= self.deadline {
            let hint = if self.distinct > 5 && self.best <= 1 {
                " (screen was actively changing -- likely animated content; consider 'wait <N>' instead)"
            } else {
                ""
            };
            return Some(Err(format!(
                "wait-stable timed out after {} frames: last_crc=0x{:08X}, distinct_crcs={}, best_streak={}{hint}",
                self.timeout,
                self.last.unwrap_or(0),
                self.distinct,
                self.best,
            )));
        }
        if frame < self.next {
            return None;
        }
        self.next = frame + STABLE_INTERVAL;
        let crc = hash();
        if self.last == Some(crc) {
            self.streak += 1;
        } else {
            self.last = Some(crc);
            self.streak = 1;
            self.distinct += 1;
        }
        self.best = self.best.max(self.streak);
        (self.streak >= STABLE_THRESHOLD).then(|| {
            Ok(format!(
                "stable after {} frames (crc=0x{crc:08X})",
                frame - self.start
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Event {
        Key(Vec<u8>, bool),
        Place(i32, i32),
        Button(u8, bool),
    }

    /// A guest whose frame and picture the test sets.
    struct Fake {
        frame: u64,
        pixels: Vec<u32>,
        events: Vec<(u64, Event)>,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                frame: 0,
                pixels: vec![0; 4],
                events: Vec::new(),
            }
        }
    }
    impl Guest for Fake {
        fn frame(&self) -> u64 {
            self.frame
        }
        fn key(&mut self, bytes: &[u8], _index: usize, down: bool) {
            self.events
                .push((self.frame, Event::Key(bytes.to_vec(), down)));
        }
        fn place(&mut self, x: i32, y: i32) {
            self.events.push((self.frame, Event::Place(x, y)));
        }
        fn button(&mut self, button: u8, down: bool) {
            self.events.push((self.frame, Event::Button(button, down)));
        }
        fn picture(&self) -> (&[u32], usize, usize) {
            (&self.pixels, 2, 2)
        }
    }

    fn runner(text: &str) -> Runner {
        Runner::new(parse::parse(text).unwrap())
    }

    /// Polls once per frame until the script ends or `limit` frames pass.
    fn run(runner: &mut Runner, guest: &mut Fake, limit: u64) -> Poll {
        loop {
            let poll = runner.poll(guest);
            if poll != Poll::Running || guest.frame >= limit {
                return poll;
            }
            guest.frame += 1;
        }
    }

    #[test]
    fn steps_land_on_the_frame_cursor() {
        let mut guest = Fake::new();
        let mut script = runner(
            "launch X.EXE\nwait 10\nkey Down\nwait 5\nmouse-click 160 148\nmouse-double-click 1 2 right\nmouse-down 3 4\nwait 1\nmouse-up\n",
        );
        assert_eq!(run(&mut script, &mut guest, 100), Poll::Finished);
        use Event::*;
        assert_eq!(
            guest.events,
            vec![
                (10, Key(vec![0xE0, 0x50], true)),
                (12, Key(vec![0xE0, 0xD0], false)),
                (17, Place(160, 148)),
                (17, Button(0, true)),
                (19, Button(0, false)),
                (19, Place(1, 2)),
                (19, Button(1, true)),
                (21, Button(1, false)),
                (23, Button(1, true)),
                (25, Button(1, false)),
                (27, Place(3, 4)),
                (27, Button(0, true)),
                (28, Button(0, false)),
            ]
        );
        assert_eq!(guest.frame, 28);
    }

    #[test]
    fn a_late_poll_keeps_later_steps_on_the_cursor() {
        let mut guest = Fake::new();
        let mut script = runner("wait 10\nkey a\nwait 10\nkey b");
        assert_eq!(script.poll(&mut guest), Poll::Running);
        // The guest jumps three frames past the first target.
        guest.frame = 13;
        assert_eq!(script.poll(&mut guest), Poll::Running);
        guest.frame = 21;
        assert_eq!(script.poll(&mut guest), Poll::Running);
        guest.frame = 22;
        assert_eq!(script.poll(&mut guest), Poll::Running);
        guest.frame = 24;
        assert_eq!(script.poll(&mut guest), Poll::Finished);
        // `a` runs late at 13 and its release, due at 12, follows at once;
        // `b` still lands on the cursor at 22.
        let frames: Vec<u64> = guest.events.iter().map(|(frame, _)| *frame).collect();
        assert_eq!(frames, [13, 13, 22, 24]);
    }

    #[test]
    fn typing_shifts_and_spaces_characters() {
        let mut guest = Fake::new();
        let mut script = runner("type \"aB\"");
        assert_eq!(run(&mut script, &mut guest, 100), Poll::Finished);
        use Event::*;
        assert_eq!(
            guest.events,
            vec![
                (0, Key(vec![0x1E], true)),
                (0, Key(vec![0x9E], false)),
                (2, Key(vec![0x2A], true)),
                (2, Key(vec![0x30], true)),
                (2, Key(vec![0xB0], false)),
                (2, Key(vec![0xAA], false)),
            ]
        );
        assert_eq!(guest.frame, 4);
    }

    #[test]
    fn skip_to_jumps_forward() {
        let mut guest = Fake::new();
        let mut script = runner("skip-to done\nkey a\nlabel: done\nkey b");
        assert_eq!(run(&mut script, &mut guest, 100), Poll::Finished);
        assert_eq!(guest.events[0], (0, Event::Key(vec![0x30], true)));
    }

    #[test]
    fn wait_stable_needs_three_equal_samples_ten_frames_apart() {
        let mut stable = Stable::new(100, 300);
        assert_eq!(stable.check(109, || unreachable!()), None);
        assert_eq!(stable.check(110, || 1), None);
        assert_eq!(stable.check(119, || unreachable!()), None);
        assert_eq!(stable.check(120, || 2), None);
        assert_eq!(stable.check(130, || 2), None);
        assert_eq!(
            stable.check(140, || 2),
            Some(Ok("stable after 40 frames (crc=0x00000002)".into()))
        );
    }

    #[test]
    fn wait_stable_times_out_with_emuscru_detail() {
        let mut stable = Stable::new(0, 60);
        for (crc, frame) in (10..60).step_by(10).enumerate() {
            assert_eq!(stable.check(frame, || crc as u32 + 1), None);
        }
        assert_eq!(
            stable.check(60, || unreachable!()),
            Some(Err("wait-stable timed out after 60 frames: last_crc=0x00000005, distinct_crcs=5, best_streak=1".into()))
        );
    }

    #[test]
    fn wait_stable_resets_the_cursor_to_where_it_settled() {
        let mut guest = Fake::new();
        let mut script = runner("wait-stable\nwait 5\nkey a");
        assert_eq!(run(&mut script, &mut guest, 1000), Poll::Finished);
        // Stable at frame 30 (samples at 10, 20, 30); key at 35.
        assert_eq!(guest.events[0], (35, Event::Key(vec![0x1E], true)));
    }

    #[test]
    fn a_changing_screen_fails_wait_stable() {
        struct Animated(Fake);
        impl Guest for Animated {
            fn frame(&self) -> u64 {
                self.0.frame
            }
            fn key(&mut self, _: &[u8], _: usize, _: bool) {}
            fn place(&mut self, _: i32, _: i32) {}
            fn button(&mut self, _: u8, _: bool) {}
            fn picture(&self) -> (&[u32], usize, usize) {
                (&self.0.pixels, 2, 2)
            }
        }
        let mut guest = Animated(Fake::new());
        let mut script = runner("wait-stable 100\nkey a");
        loop {
            guest.0.pixels[0] = guest.0.frame as u32;
            match script.poll(&mut guest) {
                Poll::Running => guest.0.frame += 1,
                Poll::Failed(message) => {
                    assert!(message.starts_with("wait-stable timed out after 100 frames"));
                    assert_eq!(guest.0.frame, 100);
                    break;
                }
                Poll::Finished => panic!("an animated screen never settles"),
            }
        }
    }

    #[test]
    fn screenshots_write_pngs_and_report_failures() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch")
            .join(format!("script-shot-{}", std::process::id()));
        let path = dir.join("sub/shot_{step}.png");
        let mut guest = Fake::new();
        let mut script = runner(&format!("wait 3\nscreenshot {}", path.display()));
        assert_eq!(run(&mut script, &mut guest, 10), Poll::Finished);
        let written = std::fs::read(dir.join("sub/shot_002.png")).unwrap();
        assert_eq!(written, png::encode(&guest.pixels, 2, 2));
        // A directory where the file should go makes the write fail.
        let mut script = runner(&format!("screenshot {}", dir.join("sub").display()));
        let Poll::Failed(message) = script.poll(&mut guest) else {
            panic!("writing over a directory must fail");
        };
        assert!(message.contains("failed to write"), "{message}");
        assert!(message.contains("at line 1"), "{message}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
