//! One owner for the DOS machine. Presentation consumes only frozen frames.
use crate::{
    audio::Mixer,
    cli::Options,
    input::Packet,
    output::{frames_for_ms, TARGET_FILL_MS},
    pacing::{Fill, Pacing},
    ring::Producer,
};
use gp64::recording::{self, Finished, Recording, Status};
use gp64_machine::{launcher, modem::ModemLine, DosMachine, DosRunExit};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
        Arc, Mutex, OnceLock,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use winit::event_loop::EventLoopProxy;

pub const SOUND_HINT: &str =
    "Sound Blaster digitised audio is not emulated; FM music and effects play";
pub enum Notice {
    Program(String),
    SoundHint,
    /// A recording was saved or failed; one line for the user.
    Recording(String),
    Done(Result<u8, String>),
}
pub enum Input {
    Key(Packet),
    Motion(i32, i32),
    Button(u8, bool),
    /// Holds the guest still (true) or lets it run on (false).
    Pause(bool),
    /// Starts (true) or stops (false) a recording to the video folder.
    Record(bool),
}
pub struct Frame {
    pub pixels: Vec<u32>,
    pub width: usize,
    pub height: usize,
}
pub struct Emulation {
    pub input: Sender<Input>,
    pub frame: Arc<Mutex<Option<Frame>>>,
    pub recording: Status,
    /// The modem's phone line, once the guest has started.
    pub modem: Arc<OnceLock<ModemLine>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<u8, String>>>,
}
impl Emulation {
    pub fn start(
        options: Options,
        folder: PathBuf,
        program: Option<String>,
        proxy: EventLoopProxy<Notice>,
        audio: Option<(u32, Producer)>,
    ) -> std::io::Result<Self> {
        let (input, receiver) = mpsc::channel();
        let frame = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_frame = frame.clone();
        let worker_stop = stop.clone();
        let status = Status::default();
        let worker_status = status.clone();
        let modem = Arc::new(OnceLock::new());
        let worker_modem = Arc::clone(&modem);
        let thread = thread::Builder::new()
            .name("gp64-emulation".into())
            .spawn(move || {
                set_interactive_qos();
                let mut audio = audio;
                let mut recording = Recording::new(worker_status);
                let result = (|| -> anyhow::Result<u8> {
                    let mut dos = if let Some(program) = program {
                        DosMachine::launch(&folder, &program, &options.args)?
                    } else {
                        launcher::launch_default(&folder)?
                    };
                    dos.machine
                        .set_cycles_per_second(gp64_machine::DEFAULT_CYCLES_PER_SECOND);
                    // The modem is always there so the menu can start a
                    // line later; its COM port is fixed from here on.
                    let line = dos.machine.attach_modem_line(options.com)?;
                    if let Some(connection) = &options.modem {
                        line.start(connection)?;
                    }
                    let _ = worker_modem.set(line);
                    // A failure of the `--record` recording ends the run;
                    // a menu recording's failure is only reported.
                    if let Some(path) = &options.record {
                        recording.start(path.clone(), &mut dos.machine);
                    }
                    let clock = Instant::now();
                    let mut pacing = Pacing::new(
                        gp64_machine::DEFAULT_CYCLES_PER_SECOND,
                        Duration::ZERO,
                        dos.machine.cycle(),
                    );
                    let mut busy = Duration::ZERO;
                    let mut mixer = audio.as_ref().map(|(rate, _)| Mixer::new(*rate));
                    let mut mixed = Vec::new();
                    let mut completed = dos.machine.completed_frames();
                    let mut hint_shown = false;
                    let trace_sb = std::env::var_os("GP64_TRACE_SB").is_some_and(|v| v == "1");
                    let mut name = String::new();
                    while !worker_stop.load(Ordering::Acquire) && !crate::shutdown::requested() {
                        let started = Instant::now();
                        for event in receiver.try_iter() {
                            match event {
                                Input::Key(packet) => {
                                    if !packet.bytes.is_empty() {
                                        dos.machine.io.keyboard.shared_state().inject_scancode(
                                            &packet.bytes,
                                            packet.index,
                                            packet.down,
                                        );
                                        dos.machine
                                            .io
                                            .keyboard
                                            .stage_output(&mut dos.machine.io.pic);
                                    }
                                }
                                Input::Motion(dx, dy) => dos.mouse_move(dx, dy),
                                Input::Button(button, down) => dos.mouse_button(button, down),
                                Input::Pause(paused) => {
                                    pacing.set_paused(paused, clock.elapsed(), dos.machine.cycle());
                                }
                                Input::Record(true) if !recording.is_active() => {
                                    let program =
                                        dos.programs_started().last().cloned().unwrap_or_default();
                                    let result = start_menu_recording(
                                        &mut recording,
                                        &mut dos.machine,
                                        &folder,
                                        &program,
                                        options.record.as_deref(),
                                    );
                                    match result {
                                        Ok(path) => {
                                            eprintln!("gp64: recording to {}", path.display());
                                        }
                                        Err(error) => report(
                                            &proxy,
                                            Err(gp64_capture::encoder::CaptureError::Io(error)),
                                        ),
                                    }
                                }
                                Input::Record(true) => {}
                                Input::Record(false) => {
                                    recording.stop(&mut dos.machine);
                                }
                            }
                        }
                        reap_recordings(&mut recording, |finished| report(&proxy, finished))
                            .map_err(anyhow::Error::msg)?;
                        let now = clock.elapsed();
                        let cycle = dos.machine.cycle();
                        let fill = audio.as_ref().map(|(rate, producer)| Fill {
                            queued: producer.len(),
                            target: frames_for_ms(*rate, TARGET_FILL_MS),
                            rate: *rate,
                            pushed: producer.pushed(),
                            started: producer.started(),
                        });
                        let budget = pacing.budget(now, std::mem::take(&mut busy), cycle, fill);
                        if pacing.rate() != dos.machine.cycles_per_second() {
                            recording.retime(cycle, pacing.rate());
                            dos.machine.set_cycles_per_second(pacing.rate());
                        }
                        if budget == 0 {
                            busy += started.elapsed();
                            // A paused guest polls for input less often. Not
                            // park_timeout: on Windows it waits a whole ~15.6 ms
                            // timer tick, while std's sleep uses a
                            // high-resolution timer there.
                            let wait = if pacing.paused() { 10 } else { 1 };
                            thread::sleep(Duration::from_millis(wait));
                            continue;
                        }
                        let exit = dos.run_for(budget)?;
                        // Capture uses chip-rate taps even with no output device.
                        let samples = dos.machine.take_audio();
                        if let Err(error) = recording.poll(&mut dos.machine, &samples) {
                            report(&proxy, Err(error));
                            check_launch_recording(&recording).map_err(anyhow::Error::msg)?;
                        }
                        if let (Some(mixer), Some((_, producer))) = (&mut mixer, &mut audio) {
                            mixer.render_into(&samples, &mut mixed);
                            producer.push(&mixed);
                        }
                        dos.take_console_output();
                        if let Some(program) = dos.programs_started().last() {
                            if *program != name {
                                name.clone_from(program);
                                let _ = proxy.send_event(Notice::Program(name.clone()));
                            }
                        }
                        for command in dos.machine.io.sound_blaster.take_new_commands() {
                            if trace_sb {
                                eprintln!("gp64: sb dsp command {command:#04X}");
                            }
                        }
                        if !hint_shown && dos.machine.io.sound_blaster.dma_command_seen() {
                            hint_shown = true;
                            eprintln!("gp64: {SOUND_HINT}");
                            let _ = proxy.send_event(Notice::SoundHint);
                        }
                        if dos.machine.completed_frames() != completed {
                            let vga = &dos.machine.io.vga;
                            if vga.has_completed_frame() {
                                if let Ok(mut latest) = worker_frame.try_lock() {
                                    let (width, height) = vga.completed_dimensions();
                                    *latest = Some(Frame {
                                        pixels: vga.completed_framebuffer().to_vec(),
                                        width,
                                        height,
                                    });
                                    completed = dos.machine.completed_frames();
                                }
                            }
                        }
                        busy += started.elapsed();
                        match exit {
                            DosRunExit::Running => {}
                            DosRunExit::Exited(code) => return Ok(code),
                            DosRunExit::Machine(exit) => {
                                anyhow::bail!("emulation stopped: {exit:?}")
                            }
                        }
                    }
                    Ok(0)
                })()
                .map_err(|error| format!("{error:#}").replace(['\n', '\r'], " "));
                let result = finish_recordings(&mut recording, result, |finished| {
                    if let Ok(paths) = finished {
                        for path in paths {
                            eprintln!("gp64: recorded {}", path.display());
                        }
                    }
                });
                let _ = proxy.send_event(Notice::Done(result.clone()));
                result
            })?;
        Ok(Self {
            input,
            frame,
            recording: status,
            modem,
            stop,
            thread: Some(thread),
        })
    }
    pub fn stop(&mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            thread
                .join()
                .map_err(|_| "emulation thread panicked".to_string())??;
        }
        Ok(())
    }
}
fn check_launch_recording(recording: &Recording) -> Result<(), String> {
    recording
        .launch_error()
        .map_or(Ok(()), |error| Err(error.to_owned()))
}

/// Report every completion, then enforce the launch recording's failure policy.
fn reap_recordings(
    recording: &mut Recording,
    mut report: impl FnMut(Finished),
) -> Result<(), String> {
    for finished in recording.reap() {
        report(finished);
    }
    check_launch_recording(recording)
}

/// Keep shutdown's existing policy for pending errors, and remember launch
/// errors whose results have already been reported during the run.
fn finish_recordings(
    recording: &mut Recording,
    mut result: Result<u8, String>,
    mut report: impl FnMut(Finished),
) -> Result<u8, String> {
    for finished in recording.finish() {
        if let Err(error) = &finished {
            if let Err(previous) = &result {
                eprintln!("gp64: {previous}");
            }
            result = Err(error.to_string());
        }
        report(finished);
    }
    if result.is_ok() {
        check_launch_recording(recording)?;
    }
    result
}

/// A new, numbered file in the user's video folder, named after the
/// running program. Never the `--record` path and never the program's folder.
fn start_menu_recording(
    recording: &mut Recording,
    machine: &mut gp64_machine::Machine,
    folder: &Path,
    program: &str,
    launch: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let dir = recording::output_dir(
        gp64_types::host_dirs::video_dir(),
        std::env::current_dir().ok(),
        folder,
        gp64_types::host_dirs::home_dir(),
    );
    std::fs::create_dir_all(&dir)?;
    recording.start_menu(
        &dir,
        recording::today(),
        recording::program_stem(program),
        launch,
        machine,
    )
}
/// Reports a recording finalised mid-run on stderr and in the window.
fn report(proxy: &EventLoopProxy<Notice>, finished: Finished) {
    let message = match finished {
        Ok(paths) => {
            let names: Vec<_> = paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            format!("recorded {}", names.join(", "))
        }
        Err(error) => format!("recording failed: {error}"),
    };
    eprintln!("gp64: {message}");
    let _ = proxy.send_event(Notice::Recording(message));
}
/// Asks macOS to keep this thread on performance cores (USER_INTERACTIVE
/// QoS). At default QoS the scheduler moves it to efficiency cores under
/// background load and the audio queue starves. Failure only leaves the
/// default QoS, so the result is ignored. On Windows the thread gets the
/// highest normal priority instead. A no-op on other hosts.
fn set_interactive_qos() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
        // SAFETY: plain libc call that only changes the calling thread's QoS.
        let _ = unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) };
    }
    #[cfg(windows)]
    {
        #[link(name = "Kernel32")]
        unsafe extern "system" {
            fn GetCurrentThread() -> *mut core::ffi::c_void;
            fn SetThreadPriority(thread: *mut core::ffi::c_void, priority: i32) -> i32;
        }
        const THREAD_PRIORITY_HIGHEST: i32 = 2;
        // SAFETY: the pseudo-handle names the calling thread; only its
        // priority changes.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
    }
}
impl Drop for Emulation {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn emulation_thread_asks_for_interactive_qos() {
        unsafe extern "C" {
            fn qos_class_self() -> u32;
        }
        let qos = thread::spawn(|| {
            set_interactive_qos();
            // SAFETY: reads the calling thread's QoS class.
            unsafe { qos_class_self() }
        })
        .join()
        .unwrap();
        assert_eq!(qos, 0x21);
    }

    #[cfg(windows)]
    #[test]
    fn emulation_thread_asks_for_highest_priority() {
        #[link(name = "Kernel32")]
        unsafe extern "system" {
            fn GetCurrentThread() -> *mut core::ffi::c_void;
            fn GetThreadPriority(thread: *mut core::ffi::c_void) -> i32;
        }
        let priority = thread::spawn(|| {
            set_interactive_qos();
            // SAFETY: reads the calling thread's priority.
            unsafe { GetThreadPriority(GetCurrentThread()) }
        })
        .join()
        .unwrap();
        assert_eq!(priority, 2);
    }

    mod guest {
        include!("../tests/support/guest.rs");
    }

    #[test]
    fn asynchronous_launch_failure_reaped_before_guest_success_has_failure_exit() {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch")
            .join(format!("launch-failure-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let program = folder.join("tone.com");
        std::fs::write(&program, guest::program()).unwrap();
        let mut dos = DosMachine::launch(&folder, "tone.com", "").unwrap();
        let mut recording = Recording::default();
        recording.start(folder.join("missing/capture.mkv"), &mut dos.machine);
        let rate = dos.machine.cycles_per_second();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut reaped = false;
        let mut result: Result<u8, String> = Ok(0);
        while !reaped {
            assert!(
                Instant::now() < deadline,
                "encoder failure was never reaped"
            );
            assert!(matches!(
                dos.run_for(rate / 1000).unwrap(),
                DosRunExit::Running
            ));
            let samples = dos.machine.take_audio();
            recording.poll(&mut dos.machine, &samples).unwrap();
            if let Err(error) = reap_recordings(&mut recording, |finished| {
                assert!(finished.is_err(), "missing parent must fail asynchronously");
                reaped = true;
            }) {
                result = Err(error);
            }
            thread::sleep(Duration::from_millis(1));
        }
        // The failed result has left Recording before this guest succeeds.
        assert!(recording.finish().is_empty());
        loop {
            match dos.run_for(rate / 100).unwrap() {
                DosRunExit::Running => {}
                DosRunExit::Exited(code) => {
                    assert_eq!(code, 0);
                    break;
                }
                exit => panic!("unexpected guest exit {exit:?}"),
            }
        }
        std::fs::remove_file(program).unwrap();
        std::fs::remove_dir(folder).unwrap();
        let result = finish_recordings(&mut recording, result, |_| panic!("already reaped"));
        let mut stderr = Vec::new();
        assert_eq!(
            crate::report_result(result.map_err(anyhow::Error::msg), &mut stderr),
            1,
            "a launch recording failure reaped before guest success must fail the CLI"
        );
    }

    fn reap_until_done(recording: &mut Recording) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut reaped = false;
            let result = reap_recordings(recording, |finished| {
                assert!(finished.is_err());
                reaped = true;
            });
            if reaped {
                return result;
            }
            assert!(Instant::now() < deadline, "finaliser did not complete");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn pending_launch_failure_remains_fatal_after_menu_starts() {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch")
            .join(format!("pending-launch-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut machine = gp64_machine::Machine::new();
        let mut recording = Recording::default();
        recording.start(folder.join("launch.mkv"), &mut machine);
        recording.stop(&mut machine);
        let menu = recording
            .start_menu(&folder, (2026, 1, 2), "MENU", None, &mut machine)
            .unwrap();
        assert!(recording.is_active());
        let error = gp64_capture::encoder::CaptureError::NoVideo.to_string();
        let reaped = reap_until_done(&mut recording);
        recording.stop(&mut machine);
        let _ = reap_until_done(&mut recording);
        let finished = finish_recordings(&mut recording, Ok(0), |_| panic!("already reaped"));
        std::fs::remove_file(menu).unwrap();
        std::fs::remove_dir(folder).unwrap();
        assert_eq!(
            reaped,
            Err(error.clone()),
            "starting a menu recording must preserve the pending launch's origin"
        );
        assert_eq!(
            finished,
            Err(error),
            "shutdown must retain a launch failure already reaped mid-run"
        );
    }

    #[test]
    fn reaped_menu_failure_remains_nonfatal() {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch")
            .join(format!("menu-failure-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut machine = gp64_machine::Machine::new();
        let mut recording = Recording::default();
        let menu = recording
            .start_menu(&folder, (2026, 1, 2), "MENU", None, &mut machine)
            .unwrap();
        recording.stop(&mut machine);
        let reaped = reap_until_done(&mut recording);
        let result = finish_recordings(&mut recording, Ok(0), |_| panic!("already reaped"));
        std::fs::remove_file(menu).unwrap();
        std::fs::remove_dir(folder).unwrap();
        assert_eq!(
            reaped,
            Ok(()),
            "menu recording failures must remain nonfatal"
        );
        assert_eq!(
            crate::report_result(result.map_err(anyhow::Error::msg), &mut Vec::new()),
            0
        );
    }

    #[test]
    fn shutdown_join_preserves_finalisation_error() {
        let (input, _) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (ready, started) = mpsc::channel();
        let mut emulation = Emulation {
            input,
            frame: Arc::new(Mutex::new(None)),
            recording: Status::default(),
            modem: Arc::default(),
            stop,
            thread: Some(thread::spawn(move || {
                let mut machine = gp64_machine::Machine::new();
                let mut recorder =
                    gp64::recording::Recorder::new(PathBuf::from("unused.mkv"), &mut machine);
                ready.send(()).unwrap();
                while !worker_stop.load(Ordering::Acquire) {
                    thread::park_timeout(Duration::from_millis(1));
                }
                recorder
                    .finish()
                    .map(|_| 0)
                    .map_err(|error| error.to_string())
            })),
        };
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let result = emulation.stop();
        let error = gp64_capture::encoder::CaptureError::NoVideo.to_string();
        assert_eq!(result, Err(error.clone()));
        let mut stderr = Vec::new();
        assert_eq!(
            crate::report_result(result.map(|()| 0).map_err(anyhow::Error::msg), &mut stderr),
            1
        );
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            format!("gp64: {error}\n")
        );
    }
}
