//! Emulated-time capture, independent of windows and host audio devices.
use gp64_capture::encoder::{
    CaptureConfig, CaptureError, CaptureFrame, CaptureSession, ReservedCaptureOutput,
};
use gp64_machine::{AudioSample, Machine};
use gp64_opl2::SAMPLE_RATE;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

/// The chip-rate mix sent to both the speaker resampler and the recorder.
pub fn mixed_sample(sample: &AudioSample) -> f32 {
    ((sample.speaker + f32::from(sample.opl2) / 32768.0 + sample.modem) * 0.5).clamp(-1.0, 1.0)
}

/// An absolute cycle boundary and the recording time accumulated before it.
struct ClockSegment {
    cycle: u64,
    ns: u64,
    rate: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Launch,
    Menu,
}

pub struct Recorder {
    origin: Origin,
    path: PathBuf,
    session: Option<CaptureSession>,
    reserved: Option<ReservedCaptureOutput>,
    clock: Vec<ClockSegment>,
    pending_audio: Vec<AudioSample>,
    stopped: bool,
}
impl Recorder {
    pub fn new(path: PathBuf, machine: &mut Machine) -> Self {
        machine.enable_frame_capture(true);
        Self {
            origin: Origin::Launch,
            path,
            session: None,
            reserved: None,
            clock: vec![ClockSegment {
                cycle: machine.cycle(),
                ns: 0,
                rate: machine.cycles_per_second(),
            }],
            pending_audio: Vec::new(),
            stopped: false,
        }
    }
    /// Emulated milliseconds since this recorder was created.
    pub fn elapsed_ms(&self, cycle: u64) -> u64 {
        self.timestamp(cycle) / 1_000_000
    }
    /// True once the encoder fell behind or failed and capture ended early.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }
    /// Ends capture at once and finalises the file on a short-lived thread,
    /// so the caller never waits for the encoder to drain.
    pub fn stop(mut self, machine: &mut Machine) -> Finaliser {
        machine.enable_frame_capture(false);
        self.stopped = true;
        let session = self.session.take();
        let spawned = thread::Builder::new()
            .name("gp64-finalise".into())
            .spawn(move || {
                session.map_or(Err(CaptureError::NoVideo), |mut session| session.stop())
            });
        match spawned {
            Ok(handle) => Finaliser::Running(handle),
            // The session was dropped with the closure, which still finalises it.
            Err(error) => Finaliser::Done(Some(Err(CaptureError::Io(error)))),
        }
    }
    /// Notify before changing the machine clock, at the current cycle boundary.
    /// Previous segments stay available for audio buffered before the first frame.
    pub fn retime(&mut self, cycle: u64, rate: u64) {
        assert!(rate > 0);
        let last = self.clock.last().unwrap();
        assert!(cycle >= last.cycle);
        if rate != last.rate {
            let ns = self.timestamp(cycle);
            self.clock.push(ClockSegment { cycle, ns, rate });
        }
    }
    fn timestamp(&self, cycle: u64) -> u64 {
        let index = self
            .clock
            .partition_point(|segment| segment.cycle <= cycle)
            .saturating_sub(1);
        let segment = &self.clock[index];
        segment.ns.saturating_add(
            (u128::from(cycle.saturating_sub(segment.cycle)) * 1_000_000_000
                / u128::from(segment.rate)) as u64,
        )
    }
    fn stop_live(&mut self, machine: &mut Machine, reason: &str) {
        if !self.stopped {
            // A GUI launch has no console, so the log is where this is seen.
            tracing::warn!(target: "gp64::recording", "recording stopped early: {reason}");
            eprintln!("gp64: recording stopped: {reason}; finalising queued data");
            self.stopped = true;
            machine.enable_frame_capture(false);
            if let Some(session) = &self.session {
                session.stop_handle().request_stop();
            }
        }
    }
    /// Call after each short emulation slice, using the same drained tap as playback.
    /// No encoder queue wait or finalisation happens on this path.
    pub fn poll(
        &mut self,
        machine: &mut Machine,
        audio: &[AudioSample],
    ) -> Result<(), CaptureError> {
        if self.stopped {
            return Ok(());
        }
        let frames = machine.take_completed_frames();
        if self.session.is_none() {
            self.pending_audio.extend_from_slice(audio);
            if let Some(frame) = frames.first() {
                let config = CaptureConfig {
                    output_path: self.path.clone(),
                    width: frame.width as u32,
                    height: frame.height as u32,
                    sample_rate: SAMPLE_RATE,
                    channels: 2,
                };
                self.session = Some(if let Some(output) = self.reserved.take() {
                    CaptureSession::start_live_reserved(config, output)?
                } else {
                    CaptureSession::start_live(config)?
                });
            } else {
                if self.pending_audio.len() > SAMPLE_RATE as usize * 5 {
                    self.stop_live(machine, "no video frame within five seconds");
                }
                return Ok(());
            }
        }
        let pending = std::mem::take(&mut self.pending_audio);
        let audio = if pending.is_empty() { audio } else { &pending };
        let stereo: Vec<_> = audio
            .iter()
            .flat_map(|sample| [mixed_sample(sample); 2])
            .collect();
        // AudioSample.cycle is the sampling instant, on the same absolute
        // guest clock as completed frames. Subtracting a period can move the
        // next slice's first sample back across an already submitted rollover.
        let timestamp = audio.first().map_or(0, |first| self.timestamp(first.cycle));
        let frames: Vec<_> = frames
            .iter()
            .map(|frame| CaptureFrame {
                pixels: &frame.pixels,
                width: frame.width as u32,
                height: frame.height as u32,
                timestamp_ns: self.timestamp(frame.cycle),
            })
            .collect();
        if let Err(refusal) = self.session.as_ref().unwrap().submit_slice(
            &frames,
            &stereo,
            timestamp,
            self.timestamp(machine.cycle()),
        ) {
            self.stop_live(machine, &format!("the encoder refused a slice: {refusal}"));
        }
        Ok(())
    }
    /// Drain the encoder and write duration, cues and the final segment size.
    pub fn finish(&mut self) -> Result<Vec<PathBuf>, CaptureError> {
        self.session
            .take()
            .map_or(Err(CaptureError::NoVideo), |mut session| session.stop())
    }
}
/// What finalising a recording produced: every file written, or the error.
pub type Finished = Result<Vec<PathBuf>, CaptureError>;

/// A recording being finalised off the emulation thread.
pub enum Finaliser {
    Running(JoinHandle<Finished>),
    Done(Option<Finished>),
}
impl Finaliser {
    pub fn is_finished(&self) -> bool {
        match self {
            Finaliser::Running(handle) => handle.is_finished(),
            Finaliser::Done(_) => true,
        }
    }
    /// Waits for finalisation to complete.
    pub fn join(self) -> Finished {
        match self {
            Finaliser::Running(handle) => handle
                .join()
                .unwrap_or_else(|_| Err(CaptureError::Encoder("finaliser panicked".into()))),
            Finaliser::Done(result) => result.unwrap_or(Err(CaptureError::NoVideo)),
        }
    }
}

/// Recording state shared with the window: emulated milliseconds since the
/// active recording started, or nothing when not recording.
#[derive(Clone, Debug)]
pub struct Status(Arc<AtomicU64>);
const OFF: u64 = u64::MAX;
impl Default for Status {
    fn default() -> Self {
        Self(Arc::new(AtomicU64::new(OFF)))
    }
}
impl Status {
    pub fn get(&self) -> Option<u64> {
        Some(self.0.load(Ordering::Relaxed)).filter(|&ms| ms != OFF)
    }
    fn set(&self, ms: Option<u64>) {
        self.0
            .store(ms.map_or(OFF, |ms| ms.min(OFF - 1)), Ordering::Relaxed);
    }
}

struct Pending {
    origin: Origin,
    finaliser: Finaliser,
}

/// Owns at most one live [`Recorder`] plus the recordings still being
/// finalised, and keeps [`Status`] current. Nothing here blocks except
/// [`Recording::finish`], which is for exit.
#[derive(Default)]
pub struct Recording {
    recorder: Option<Recorder>,
    finalisers: Vec<Pending>,
    launch_error: Option<String>,
    status: Status,
}
impl Recording {
    pub fn new(status: Status) -> Self {
        Self {
            recorder: None,
            finalisers: Vec::new(),
            launch_error: None,
            status,
        }
    }
    /// The first launch recording failure, retained even after its result is reaped.
    pub fn launch_error(&self) -> Option<&str> {
        self.launch_error.as_deref()
    }
    fn remember_error(&mut self, origin: Origin, error: &CaptureError) {
        if origin == Origin::Launch && self.launch_error.is_none() {
            self.launch_error = Some(error.to_string());
        }
    }
    pub fn is_active(&self) -> bool {
        self.recorder.is_some()
    }
    /// Starts recording to `path`; does nothing if already recording.
    pub fn start(&mut self, path: PathBuf, machine: &mut Machine) {
        if self.recorder.is_none() {
            self.recorder = Some(Recorder::new(path, machine));
            self.status.set(Some(0));
        }
    }
    /// Reserve a menu filename before the first frame or encoder worker.
    /// Empty reservations remain on disk if stopped before video arrives.
    pub fn start_menu(
        &mut self,
        dir: &Path,
        date: (i32, u32, u32),
        stem: &str,
        launch: Option<&Path>,
        machine: &mut Machine,
    ) -> std::io::Result<PathBuf> {
        if let Some(recorder) = &self.recorder {
            return Ok(recorder.path.clone());
        }
        let base = numbered_path(dir, date, stem, |_| false);
        let output = ReservedCaptureOutput::reserve(&base, launch)?;
        let path = output.path().to_path_buf();
        let mut recorder = Recorder::new(path.clone(), machine);
        recorder.origin = Origin::Menu;
        recorder.reserved = Some(output);
        self.recorder = Some(recorder);
        self.status.set(Some(0));
        Ok(path)
    }
    /// Preserve recording time at a CPU rate change, before retiming the machine.
    pub fn retime(&mut self, cycle: u64, rate: u64) {
        if let Some(recorder) = &mut self.recorder {
            recorder.retime(cycle, rate);
        }
    }
    /// Stops the active recording without waiting for its encoder. Returns
    /// whether one was active.
    pub fn stop(&mut self, machine: &mut Machine) -> bool {
        let Some(recorder) = self.recorder.take() else {
            return false;
        };
        self.finalisers.push(Pending {
            origin: recorder.origin,
            finaliser: recorder.stop(machine),
        });
        self.status.set(None);
        true
    }
    /// Feeds one slice to the active recording. An error ends that recording
    /// (nothing was written for it) and is returned. A recording whose
    /// encoder fell behind is stopped and handed to a finaliser.
    pub fn poll(
        &mut self,
        machine: &mut Machine,
        audio: &[AudioSample],
    ) -> Result<(), CaptureError> {
        let Some(recorder) = &mut self.recorder else {
            return Ok(());
        };
        if let Err(error) = recorder.poll(machine, audio) {
            let origin = recorder.origin;
            self.remember_error(origin, &error);
            self.recorder = None;
            machine.enable_frame_capture(false);
            self.status.set(None);
            return Err(error);
        }
        if recorder.is_stopped() {
            self.stop(machine);
        } else {
            self.status.set(Some(recorder.elapsed_ms(machine.cycle())));
        }
        Ok(())
    }
    /// Results of finalisers that have already completed; never waits.
    pub fn reap(&mut self) -> Vec<Finished> {
        let (done, running): (Vec<_>, Vec<_>) = std::mem::take(&mut self.finalisers)
            .into_iter()
            .partition(|pending| pending.finaliser.is_finished());
        self.finalisers = running;
        done.into_iter()
            .map(|pending| {
                let result = pending.finaliser.join();
                if let Err(error) = &result {
                    self.remember_error(pending.origin, error);
                }
                result
            })
            .collect()
    }
    /// For exit: finalises the active recording and waits for every one.
    pub fn finish(&mut self) -> Vec<Finished> {
        let mut results = Vec::new();
        for pending in std::mem::take(&mut self.finalisers) {
            let result = pending.finaliser.join();
            if let Err(error) = &result {
                self.remember_error(pending.origin, error);
            }
            results.push(result);
        }
        if let Some(mut recorder) = self.recorder.take() {
            let result = recorder.finish();
            if let Err(error) = &result {
                self.remember_error(recorder.origin, error);
            }
            results.push(result);
        }
        self.status.set(None);
        results
    }
}

/// Where recordings started from the menu go: the user's video folder, else
/// the current folder, but never the program's own folder (then the home
/// folder, then the temporary folder).
pub fn output_dir(
    video: Option<PathBuf>,
    current: Option<PathBuf>,
    program: &Path,
    home: Option<PathBuf>,
) -> PathBuf {
    let same = |dir: &Path| {
        dir.canonicalize()
            .ok()
            .zip(program.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
    };
    video
        .into_iter()
        .chain(current)
        .chain(home)
        .find(|dir| !same(dir))
        .unwrap_or_else(std::env::temp_dir)
}

/// `<dir>/YYYY.MM.DD - <stem>.mkv`, then ` (2)`, ` (3)`… while `taken`.
pub fn numbered_path(
    dir: &Path,
    date: (i32, u32, u32),
    stem: &str,
    taken: impl Fn(&Path) -> bool,
) -> PathBuf {
    let (year, month, day) = date;
    let stem: String = stem
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let stem = if stem.trim().is_empty() {
        "gp64"
    } else {
        stem.trim()
    };
    let base = format!("{year:04}.{month:02}.{day:02} - {stem}");
    let mut path = dir.join(format!("{base}.mkv"));
    let mut number = 2u32;
    while taken(&path) {
        path = dir.join(format!("{base} ({number}).mkv"));
        number += 1;
    }
    path
}

/// The file stem of a program name as the guest gave it (`DIR\NAME.EXT`).
pub fn program_stem(name: &str) -> &str {
    let file = name.rsplit(['\\', '/', ':']).next().unwrap_or(name);
    file.rsplit_once('.').map_or(file, |(stem, _)| stem)
}

/// `mm:ss` for a millisecond count; minutes grow past 99.
pub fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// Today's local date, or the UTC date where local time is unavailable.
pub fn today() -> (i32, u32, u32) {
    local_date().unwrap_or_else(|| {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        civil_from_days((seconds / 86_400) as i64)
    })
}

/// Proleptic Gregorian date of a day count from 1970-01-01.
pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year as i32, month, day)
}

#[cfg(unix)]
fn local_date() -> Option<(i32, u32, u32)> {
    // The common prefix of `struct tm` on the Unix hosts gp64 builds for,
    // with both trailing fields so the size matches.
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: std::ffi::c_long,
        zone: *const std::ffi::c_char,
    }
    unsafe extern "C" {
        fn time(out: *mut std::ffi::c_long) -> std::ffi::c_long;
        fn localtime_r(time: *const std::ffi::c_long, out: *mut Tm) -> *mut Tm;
    }
    // SAFETY: `time` accepts null; `localtime_r` writes only into `tm`,
    // which is a correctly sized, writable `struct tm`.
    unsafe {
        let now = time(std::ptr::null_mut());
        let mut tm: Tm = std::mem::zeroed();
        if localtime_r(&now, &mut tm).is_null() {
            return None;
        }
        Some((tm.year + 1900, (tm.mon + 1) as u32, tm.mday as u32))
    }
}
#[cfg(windows)]
fn local_date() -> Option<(i32, u32, u32)> {
    #[repr(C)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(out: *mut SystemTime);
    }
    // SAFETY: GetLocalTime fills the caller's SYSTEMTIME and cannot fail.
    let now = unsafe {
        let mut now: SystemTime = std::mem::zeroed();
        GetLocalTime(&mut now);
        now
    };
    Some((
        i32::from(now.year),
        u32::from(now.month),
        u32::from(now.day),
    ))
}
#[cfg(not(any(unix, windows)))]
fn local_date() -> Option<(i32, u32, u32)> {
    None
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.session.is_some() {
            if let Err(error) = self.finish() {
                eprintln!("gp64: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn the_modem_speaker_mixes_at_the_pc_speakers_weight() {
        let sample = |speaker, modem| AudioSample {
            cycle: 0,
            speaker,
            opl2: 0,
            modem,
        };
        assert_eq!(mixed_sample(&sample(0.0, 0.5)), 0.25);
        assert_eq!(mixed_sample(&sample(0.5, 0.5)), 0.5);
        assert_eq!(mixed_sample(&sample(1.0, 1.0)), 1.0, "clamped");
    }

    mod decode {
        include!("../tests/support/decode.rs");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch")
            .join(format!("{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn next_poll_sample_instant_stays_after_the_submitted_rollover() {
        let dir = scratch("sample-instant");
        let mut machine = Machine::with_rate(20_000_000);
        let mut recorder = Recorder::new(dir.join("capture.mkv"), &mut machine);
        let session = CaptureSession::start_live(CaptureConfig {
            output_path: recorder.path.clone(),
            width: 320,
            height: 200,
            sample_rate: SAMPLE_RATE,
            channels: 2,
        })
        .unwrap();
        let pixels = vec![0; 640 * 200];
        let frames = [
            CaptureFrame {
                pixels: &pixels[..320 * 200],
                width: 320,
                height: 200,
                timestamp_ns: 0,
            },
            CaptureFrame {
                pixels: &pixels,
                width: 640,
                height: 200,
                timestamp_ns: 1_000_000,
            },
        ];
        assert!(session.try_submit_slice(&frames, &[], 0, 1_000_000));
        recorder.session = Some(session);
        // The first sample in the next poll is just one CPU cycle after the
        // already submitted boundary. Subtracting its source period would
        // timestamp it in the preceding millisecond and the preceding file.
        let audio: Vec<_> = (0..960u64)
            .map(|i| AudioSample {
                cycle: 20_001 + i * 20_000_000 / u64::from(SAMPLE_RATE),
                speaker: 0.5,
                opl2: 0,
                modem: 0.0,
            })
            .collect();
        machine.cpu.cycles = audio.last().unwrap().cycle;
        recorder.poll(&mut machine, &audio).unwrap();
        assert!(!recorder.is_stopped());
        let paths = recorder.finish().unwrap();
        assert_eq!(paths.len(), 2);
        let summaries: Vec<_> = paths.iter().map(|path| decode::read(path)).collect();
        assert_eq!(
            summaries[1].audio[0].0, 1,
            "new segment PCM must use its sampling instant"
        );
        assert!(!decode::decode_audio(&summaries[1]).is_empty());
        for summary in &summaries {
            assert_eq!(decode::decode_video(summary).len(), 1);
        }
        for path in paths {
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn recording_time_integrates_rate_changes_and_retains_pending_timestamps() {
        let mut machine = Machine::with_rate(20_000_000);
        let mut recorder = Recorder::new(PathBuf::from("unused.mkv"), &mut machine);
        machine.cpu.cycles += 2_000_000;
        recorder.retime(machine.cycle(), 40_000_000);
        machine.set_cycles_per_second(40_000_000);
        machine.cpu.cycles += 4_000_000;
        recorder.retime(machine.cycle(), 8_000_000);
        machine.set_cycles_per_second(8_000_000);
        machine.cpu.cycles += 800_000;
        // Buffered chip samples and completed frames can predate multiple boundaries.
        for (cycle, ns) in [
            (1_000_000, 50_000_000),
            (2_000_000, 100_000_000),
            (4_000_000, 150_000_000),
            (6_000_000, 200_000_000),
            (6_800_000, 300_000_000),
        ] {
            assert_eq!(recorder.timestamp(cycle), ns);
        }
        assert_eq!(recorder.elapsed_ms(machine.cycle()), 300);
        assert_eq!(machine.take_audio().len(), (SAMPLE_RATE as usize / 5));
    }

    #[test]
    fn names_are_dated_and_numbered_on_collision() {
        let dir = scratch("naming");
        let name = |dir: &Path| numbered_path(dir, (2026, 3, 7), "PROG", |path| path.exists());
        let first = name(&dir);
        assert_eq!(first, dir.join("2026.03.07 - PROG.mkv"));
        fs::write(&first, b"").unwrap();
        let second = name(&dir);
        assert_eq!(second, dir.join("2026.03.07 - PROG (2).mkv"));
        fs::write(&second, b"").unwrap();
        assert_eq!(name(&dir), dir.join("2026.03.07 - PROG (3).mkv"));
        // A path in use but not yet on disk is skipped too.
        let held = dir.join("2026.03.07 - OTHER.mkv");
        assert_eq!(
            numbered_path(&dir, (2026, 3, 7), "OTHER", |path| path == held),
            dir.join("2026.03.07 - OTHER (2).mkv")
        );
        assert_eq!(
            numbered_path(&dir, (2026, 12, 31), "a/b:c", |_| false),
            dir.join("2026.12.31 - a_b_c.mkv")
        );
        assert_eq!(
            numbered_path(&dir, (2026, 1, 1), " ", |_| false),
            dir.join("2026.01.01 - gp64.mkv")
        );
        fs::remove_file(first).unwrap();
        fs::remove_file(second).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn output_falls_back_and_avoids_the_program_folder() {
        let program = scratch("program");
        let other = scratch("other");
        let video = PathBuf::from("/videos");
        assert_eq!(
            output_dir(Some(video.clone()), Some(other.clone()), &program, None),
            video
        );
        assert_eq!(output_dir(None, Some(other.clone()), &program, None), other);
        assert_eq!(
            output_dir(None, Some(program.clone()), &program, Some(other.clone())),
            other
        );
        assert_eq!(
            output_dir(Some(program.clone()), Some(program.clone()), &program, None),
            std::env::temp_dir()
        );
        fs::remove_dir(program).unwrap();
        fs::remove_dir(other).unwrap();
    }

    #[test]
    fn stems_and_clock_text() {
        assert_eq!(program_stem("PROG.EXE"), "PROG");
        assert_eq!(program_stem("C:\\DIR\\RUN.COM"), "RUN");
        assert_eq!(program_stem("NOEXT"), "NOEXT");
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(59_999), "00:59");
        assert_eq!(clock(61_000), "01:01");
        assert_eq!(clock(6_000_000), "100:00");
    }

    #[test]
    fn dates_from_day_counts() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_737), (2026, 10, 11));
        let (year, month, day) = today();
        assert!(year >= 2026 && (1..=12).contains(&month) && (1..=31).contains(&day));
    }

    #[test]
    fn status_reports_off_and_elapsed() {
        let status = Status::default();
        assert_eq!(status.get(), None);
        status.set(Some(1234));
        assert_eq!(status.clone().get(), Some(1234));
        status.set(None);
        assert_eq!(status.get(), None);
    }
}
