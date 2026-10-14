mod audio;
mod cli;
mod cpu;
mod crt;
mod detach;
mod device_audio;
mod discovery;
mod emulation;
mod input;
mod link;
mod logging;
mod output;
mod overlay;
mod pacing;
mod ring;
mod scaler;
mod script;
mod settings;
mod shutdown;

use anyhow::{Context, Result};
use cli::{Command, Options};
use emulation::{Emulation, Input, Notice, SOUND_HINT};
use gp64_machine::modem::{LineStatus, ModemConnection, ModemLine};
use std::{
    net::SocketAddr,
    num::NonZeroU32,
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{CursorGrabMode, Fullscreen, Window, WindowId},
};

fn main() {
    let result = run();
    let code = if detach::is_detached() {
        let mut message = Vec::new();
        let code = report_result(result, &mut message);
        if !message.is_empty() {
            detach::show_error(&String::from_utf8_lossy(&message));
        }
        code
    } else {
        report_result(result, &mut std::io::stderr().lock())
    };
    std::process::exit(i32::from(code));
}
fn report_result(result: Result<u8>, stderr: &mut impl std::io::Write) -> u8 {
    match result {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(
                stderr,
                "gp64: {}",
                format!("{error:#}").replace(['\n', '\r'], " ")
            );
            1
        }
    }
}
fn run() -> Result<u8> {
    let options = match cli::parse(std::env::args_os().skip(1))? {
        Command::Help => {
            println!("{}", cli::help_text(cli::stdout_wants_color()));
            return Ok(0);
        }
        Command::Version => {
            println!("gp64 {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        Command::Run(options) => options,
    };
    let (folder, program) = if let Some(path) = &options.program {
        let absolute = path
            .canonicalize()
            .with_context(|| format!("cannot open {}", path.display()))?;
        let folder = absolute
            .parent()
            .context("program has no parent folder")?
            .to_path_buf();
        let name = absolute
            .file_name()
            .and_then(|name| name.to_str())
            .context("program name must be text")?
            .to_owned();
        (folder, Some(name))
    } else {
        let beside = cli::executable_directory(&std::env::current_exe()?)?;
        let current = std::env::current_dir().ok();
        (cli::default_folder(&beside, current.as_deref())?, None)
    };
    // A script is parsed whole before anything starts.
    let script = options.script.as_deref().map(script::load).transpose()?;
    // The command line and program are checked; the rest runs detached.
    // A script run stays attached so its caller sees how it ended.
    if !options.wait && script.is_none() && detach::relaunch().context("cannot start detached")? {
        return Ok(0);
    }
    shutdown::install()?;
    logging::install();
    let saved = settings::load();
    let events = EventLoop::<Notice>::with_user_event().build()?;
    // `--mute` opens no device at all; a saved mute opens it silenced so
    // the menu can unmute.
    let audio = if options.mute {
        None
    } else {
        Some(device_audio::Output::open(saved.mute)?)
    };
    let mut app = App {
        menu: overlay::Menu::default(),
        // No socket opens until a link is enabled.
        discovery: discovery::Discovery::new(),
        serial: link::AutoLink::new("serial", saved.serial_link),
        modem: link::AutoLink::new("modem", saved.modem_link),
        guest_paused: false,
        launch: Some((options, folder, program, script)),
        proxy: events.create_proxy(),
        audio,
        window: None,
        surface: None,
        emulation: None,
        keyboard: input::Keyboard::default(),
        modifiers: ModifiersState::empty(),
        hotkeys: input::Hotkeys::default(),
        captured: false,
        buttons: [false; 3],
        motion: (0.0, 0.0),
        scaler: saved.scaler,
        blitter: scaler::Blitter::default(),
        saved,
        program: String::new(),
        hint_until: None,
        message: None,
        cpu: cpu::Meter::new(),
        usage: None,
        next_usage: Instant::now(),
        opened: Instant::now(),
        next_redraw: Instant::now(),
        result: Ok(0),
    };
    events.run_app(&mut app)?;
    if let Some(mut emulation) = app.emulation.take() {
        emulation.stop().map_err(anyhow::Error::msg)?;
    }
    app.result.map_err(anyhow::Error::msg)
}
/// Shown in the title while the mouse is captured.
const MOUSE_HINT: &str = "Ctrl+F10 frees mouse";
/// The window title: the program, CPU usage, then any hints and messages.
fn title_text(
    program: &str,
    usage: Option<u32>,
    captured: bool,
    sound_hint: bool,
    message: Option<&str>,
) -> String {
    let mut title = if program.is_empty() {
        "gp64".into()
    } else {
        format!("gp64 - {program}")
    };
    if let Some(usage) = usage {
        title.push_str(&format!(" - CPU {usage}%"));
    }
    if captured {
        title.push_str(" - ");
        title.push_str(MOUSE_HINT);
    }
    if sound_hint {
        title.push_str(" - ");
        title.push_str(SOUND_HINT);
    }
    if let Some(message) = message {
        title.push_str(" - ");
        title.push_str(message);
    }
    title
}
/// Shows the executable's icon in the title bar and taskbar at the window's
/// display scale. Builds without the resource (see build.rs) keep the default.
#[cfg(windows)]
fn set_icons(window: &Window) {
    use winit::platform::windows::{IconExtWindows, WindowExtWindows};
    use winit::{dpi::PhysicalSize, window::Icon};
    /// The group icon ID build.rs embeds.
    const ICON_GROUP_ID: u16 = 1;
    let icon = |size: f64| {
        let size = (size * window.scale_factor()).round() as u32;
        Icon::from_resource(ICON_GROUP_ID, Some(PhysicalSize::new(size, size))).ok()
    };
    if let (Some(small), Some(large)) = (icon(16.0), icon(32.0)) {
        window.set_window_icon(Some(small));
        window.set_taskbar_icon(Some(large));
    }
}
#[cfg(not(windows))]
fn set_icons(_window: &Window) {}
struct App {
    launch: Option<(Options, PathBuf, Option<String>, Option<script::Script>)>,
    proxy: winit::event_loop::EventLoopProxy<Notice>,
    audio: Option<device_audio::Output>,
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    emulation: Option<Emulation>,
    keyboard: input::Keyboard,
    modifiers: ModifiersState,
    hotkeys: input::Hotkeys,
    captured: bool,
    buttons: [bool; 3],
    motion: (f64, f64),
    scaler: scaler::Scaler,
    blitter: scaler::Blitter,
    /// The persisted choices; only menu and hotkey changes update them, so
    /// one-run command-line overrides never reach the file.
    saved: settings::Settings,
    menu: overlay::Menu,
    discovery: discovery::Discovery,
    /// The COM1 null-modem link to another gp64 on the LAN.
    serial: link::AutoLink,
    /// The COM2 modem link to another gp64 on the LAN.
    modem: link::AutoLink,
    /// The menu holds the guest still. It does so only while no link is in
    /// use; see [`App::sync_pause`].
    guest_paused: bool,
    program: String,
    hint_until: Option<Instant>,
    /// A recording result shown in the title until the given time.
    message: Option<(String, Instant)>,
    cpu: cpu::Meter,
    /// This process's CPU usage, shown after the program name.
    usage: Option<u32>,
    next_usage: Instant,
    /// Drives the REC indicator's pulse.
    opened: Instant,
    next_redraw: Instant,
    result: std::result::Result<u8, String>,
}
impl App {
    fn send(&self, input: Input) {
        if let Some(emulation) = &self.emulation {
            let _ = emulation.input.send(input);
        }
    }
    fn title(&self) {
        if let Some(window) = &self.window {
            window.set_title(&title_text(
                &self.program,
                self.usage,
                self.captured,
                self.hint_until.is_some(),
                self.message.as_ref().map(|(message, _)| message.as_str()),
            ));
        }
    }
    fn release_mouse(&mut self) {
        if let Some(window) = &self.window {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            window.set_cursor_visible(true);
        }
        let was_captured = std::mem::replace(&mut self.captured, false);
        self.motion = (0.0, 0.0);
        for button in 0..3 {
            if self.buttons[button] {
                self.send(Input::Button(button as u8, false));
                self.buttons[button] = false;
            }
        }
        if was_captured {
            self.title();
        }
    }
    fn capture_mouse(&mut self) {
        if let Some(window) = &self.window {
            let result = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
            if result.is_ok() {
                self.captured = true;
                window.set_cursor_visible(false);
            }
        }
        if self.captured {
            self.title();
        }
    }
    fn fullscreen(&self) -> bool {
        self.window
            .as_ref()
            .is_some_and(|window| window.fullscreen().is_some())
    }
    fn set_fullscreen(&mut self, on: bool) {
        if let Some(window) = &self.window {
            window.set_fullscreen(on.then_some(Fullscreen::Borderless(None)));
        }
        self.saved.fullscreen = on;
        settings::save(&self.saved);
    }
    fn set_scaler(&mut self, scaler: scaler::Scaler) {
        self.scaler = scaler;
        self.saved.scaler = scaler;
        settings::save(&self.saved);
    }
    /// Emulated milliseconds into the active recording, if any.
    fn recording(&self) -> Option<u64> {
        self.emulation
            .as_ref()
            .and_then(|emulation| emulation.recording.get())
    }
    fn links(&self) -> Option<&emulation::Links> {
        self.emulation
            .as_ref()
            .and_then(|emulation| emulation.links.get())
    }
    /// A link as the menu shows it, named after its peer when known.
    fn link_view(&self, auto: &link::AutoLink, line: Option<&ModemLine>) -> overlay::Link {
        let status = line.map(ModemLine::status).unwrap_or_default();
        let peer = match &status {
            LineStatus::Connected(address) => {
                address.parse::<SocketAddr>().ok().and_then(|address| {
                    self.discovery
                        .peers()
                        .into_iter()
                        .find(|peer| peer.ip == address.ip())
                        .map(|peer| peer.name)
                })
            }
            _ => None,
        };
        overlay::Link {
            enabled: auto.enabled,
            status,
            peer,
            com: line.map_or(0, ModemLine::com),
        }
    }
    fn menu_values(&self) -> overlay::Values {
        let muted = self.audio.as_ref().map(|audio| &audio.muted);
        overlay::Values {
            scaler: self.scaler,
            fullscreen: self.fullscreen(),
            mute: muted.is_none_or(|muted| muted.load(Ordering::Relaxed)),
            mute_locked: muted.is_none(),
            recording: self.recording(),
            serial: self.link_view(&self.serial, self.links().map(|links| &links.serial)),
            modem: self.link_view(&self.modem, self.links().map(|links| &links.modem)),
        }
    }
    /// Opens discovery while a link is enabled, and moves each link one
    /// step: listen, call a peer, or hang up. Runs on every frame.
    fn drive_links(&mut self) {
        self.discovery
            .set_active(self.serial.enabled || self.modem.enabled);
        let Some(links) = self.emulation.as_ref().and_then(|e| e.links.get()) else {
            return;
        };
        let now = Instant::now();
        let own_id = self.discovery.instance_id();
        let peers = self.discovery.peers();
        type PortOf = fn(&discovery::Ports) -> u16;
        for (auto, line, port_of, name) in [
            (
                &mut self.serial,
                &links.serial,
                (|ports| ports.serial) as PortOf,
                "serial",
            ),
            (&mut self.modem, &links.modem, |ports| ports.modem, "modem"),
        ] {
            let candidates = peers
                .iter()
                .filter(|peer| port_of(&peer.ports) != 0)
                .map(|peer| (peer.id, SocketAddr::new(peer.ip, port_of(&peer.ports))));
            let result = match auto.step(&line.status(), own_id, candidates, now) {
                link::Step::None => Ok(()),
                link::Step::Listen => line.start(&ModemConnection::Host(0)).map(drop),
                link::Step::Call(address) => line.call(&address.to_string()),
                link::Step::HangUp => {
                    line.hang_up();
                    Ok(())
                }
            };
            if let Err(error) = result {
                tracing::warn!(target: "gp64::link", "{name} link: {error}");
            }
        }
        let listening = |line: &ModemLine| match line.status() {
            LineStatus::Listening(port) => port,
            _ => 0,
        };
        self.discovery.set_ports(discovery::Ports {
            serial: listening(&links.serial),
            modem: listening(&links.modem),
        });
        self.discovery.tick();
    }
    /// Holds the guest while the menu is open and no link is in use; links
    /// connect and drop while the menu is open, so this runs after every
    /// menu change and on every frame.
    fn sync_pause(&mut self) {
        let pause = self.menu.is_open() && self.menu_values().pauses();
        if pause != self.guest_paused {
            self.guest_paused = pause;
            self.send(Input::Pause(pause));
        }
    }
    /// Releases everything the guest holds, as on focus loss, without
    /// forgetting consumed hotkeys (the menu key's release must stay
    /// swallowed).
    fn release_guest_input(&mut self) {
        for packet in self.keyboard.release_all() {
            self.send(Input::Key(packet));
        }
        self.release_mouse();
    }
    fn toggle_menu(&mut self) {
        if self.menu.toggle() {
            self.release_guest_input();
        }
        self.sync_pause();
    }
    fn menu_key(&mut self, key: overlay::Key) {
        let values = self.menu_values();
        match self.menu.handle(key, &values) {
            overlay::Action::None | overlay::Action::Close => {}
            overlay::Action::Scaler(scaler) => self.set_scaler(scaler),
            overlay::Action::Fullscreen(on) => self.set_fullscreen(on),
            overlay::Action::Mute(on) => {
                if let Some(audio) = &self.audio {
                    audio.muted.store(on, Ordering::Relaxed);
                    self.saved.mute = on;
                    settings::save(&self.saved);
                }
            }
            overlay::Action::Record(on) => self.send(Input::Record(on)),
            overlay::Action::Serial(on) => {
                self.serial.enabled = on;
                self.saved.serial_link = on;
                settings::save(&self.saved);
            }
            overlay::Action::Modem(on) => {
                self.modem.enabled = on;
                self.saved.modem_link = on;
                settings::save(&self.saved);
            }
        }
        self.drive_links();
        self.sync_pause();
    }
    fn draw(&mut self) -> Result<()> {
        let values = self.menu_values();
        let Some(window) = &self.window else {
            return Ok(());
        };
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return Ok(());
        };
        let Some(surface) = &mut self.surface else {
            return Ok(());
        };
        surface
            .resize(width, height)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut buffer = surface
            .buffer_mut()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        buffer.fill(0);
        if let Some(emulation) = &self.emulation {
            // The emulator drops a presentation update if this lock is held.
            // Drawing always uses the latest complete frame, never a partial raster.
            if let Ok(latest) = emulation.frame.lock() {
                if let Some(frame) = latest.as_ref() {
                    self.blitter.blit(
                        &frame.pixels,
                        frame.width,
                        frame.height,
                        &mut buffer,
                        size.width as usize,
                        size.height as usize,
                        self.scaler,
                    );
                }
            }
        }
        // Drawn into the window buffer only, after scaling: never into the
        // guest frame, so recordings never contain the menu.
        self.menu.render(
            &mut buffer,
            size.width as usize,
            size.height as usize,
            &values,
        );
        overlay::render_recording(
            &mut buffer,
            size.width as usize,
            size.height as usize,
            values.recording,
            self.opened.elapsed().as_millis() as u64,
        );
        buffer
            .present()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(())
    }
    fn fail(&mut self, events: &ActiveEventLoop, error: impl std::fmt::Display) {
        self.result = Err(error.to_string());
        events.exit();
    }
}
impl ApplicationHandler<Notice> for App {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let result = (|| -> Result<()> {
            let window = Arc::new(
                events.create_window(
                    Window::default_attributes()
                        .with_title("gp64")
                        .with_inner_size(winit::dpi::LogicalSize::new(960.0, 720.0))
                        .with_fullscreen(
                            self.saved
                                .fullscreen
                                .then_some(Fullscreen::Borderless(None)),
                        ),
                )?,
            );
            set_icons(&window);
            let context = softbuffer::Context::new(window.clone())
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            self.surface = Some(
                softbuffer::Surface::new(&context, window.clone())
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?,
            );
            self.window = Some(window);
            let (options, folder, program, script) =
                self.launch.take().context("missing launch parameters")?;
            let audio = self.audio.as_mut().and_then(|audio| {
                let rate = audio.rate;
                audio.producer.take().map(|producer| (rate, producer))
            });
            self.emulation = Some(Emulation::start(
                options,
                folder,
                program,
                script,
                self.proxy.clone(),
                audio,
            )?);
            Ok(())
        })();
        if let Err(error) = result {
            self.fail(events, error);
        }
    }
    fn user_event(&mut self, events: &ActiveEventLoop, notice: Notice) {
        match notice {
            Notice::Program(program) => {
                self.program = program;
                self.title();
            }
            Notice::SoundHint => {
                self.hint_until = Some(Instant::now() + Duration::from_secs(10));
                self.title();
            }
            Notice::Recording(message) => {
                self.message = Some((message, Instant::now() + Duration::from_secs(10)));
                self.title();
            }
            Notice::Done(result) => {
                self.result = result;
                events.exit();
            }
        }
    }
    fn window_event(&mut self, events: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.window.as_ref().is_none_or(|window| window.id() != id) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => events.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.draw() {
                    self.fail(events, error);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::Focused(false) => {
                for packet in self.keyboard.release_all() {
                    self.send(Input::Key(packet));
                }
                self.hotkeys.clear();
                self.modifiers = ModifiersState::empty();
                self.release_mouse();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(key) = event.physical_key {
                    let down = event.state == ElementState::Pressed;
                    let action = self.hotkeys.event(key, down, event.repeat, self.modifiers);
                    if action == input::KeyAction::Consume {
                        return;
                    }
                    if action == input::KeyAction::Hotkey {
                        match key {
                            KeyCode::F12 => self.toggle_menu(),
                            KeyCode::F11 => self.set_scaler(self.scaler.cycle(true)),
                            KeyCode::Enter => self.set_fullscreen(!self.fullscreen()),
                            KeyCode::F10 => self.release_mouse(),
                            _ => {}
                        }
                    } else if self.menu.is_open() {
                        // While the menu is open no key reaches the guest.
                        if let Some(key) = overlay::Key::from_code(key).filter(|_| down) {
                            self.menu_key(key);
                        }
                    } else if let Some(packet) = self.keyboard.event(key, down) {
                        self.send(Input::Key(packet));
                    }
                }
            }
            WindowEvent::MouseInput { .. } if self.menu.is_open() => {}
            WindowEvent::MouseInput { state, button, .. } => {
                let down = state == ElementState::Pressed;
                if down && !self.captured {
                    self.capture_mouse();
                }
                let index = match button {
                    MouseButton::Left => Some(0),
                    MouseButton::Right => Some(1),
                    MouseButton::Middle => Some(2),
                    _ => None,
                };
                if let Some(index) = index {
                    if self.captured {
                        self.buttons[index] = down;
                        self.send(Input::Button(index as u8, down));
                    }
                }
            }
            _ => {}
        }
    }
    fn device_event(
        &mut self,
        _events: &ActiveEventLoop,
        _id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        if self.captured {
            if let DeviceEvent::MouseMotion { delta } = event {
                self.motion.0 += delta.0;
                self.motion.1 += delta.1;
                let dx = self.motion.0 as i32;
                let dy = self.motion.1 as i32;
                self.motion.0 -= f64::from(dx);
                self.motion.1 -= f64::from(dy);
                if dx != 0 || dy != 0 {
                    self.send(Input::Motion(dx, dy));
                }
            }
        }
    }
    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        if shutdown::requested() {
            events.exit();
        }
        self.drive_links();
        self.sync_pause();
        let now = Instant::now();
        if self.hint_until.is_some_and(|until| now >= until) {
            self.hint_until = None;
            self.title();
        }
        if self
            .message
            .as_ref()
            .is_some_and(|(_, until)| now >= *until)
        {
            self.message = None;
            self.title();
        }
        if now >= self.next_usage {
            let usage = self.cpu.sample(now);
            self.next_usage = now + cpu::INTERVAL;
            if usage != self.usage {
                self.usage = usage;
                self.title();
            }
        }
        if now >= self.next_redraw {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            self.next_redraw = now + Duration::from_millis(16);
        }
        events.set_control_flow(ControlFlow::WaitUntil(self.next_redraw));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_shows_the_mouse_hint_only_while_captured() {
        assert_eq!(
            title_text("GP.EXE", Some(80), true, false, None),
            "gp64 - GP.EXE - CPU 80% - Ctrl+F10 frees mouse"
        );
        assert_eq!(
            title_text("GP.EXE", Some(80), false, false, None),
            "gp64 - GP.EXE - CPU 80%"
        );
        assert_eq!(
            title_text("", None, true, false, Some("saved")),
            "gp64 - Ctrl+F10 frees mouse - saved"
        );
    }

    #[test]
    fn shutdown_error_is_reported_with_failure_exit() {
        let mut stderr = Vec::new();
        assert_eq!(
            report_result(
                Err(anyhow::anyhow!("encoder finalisation failed")),
                &mut stderr
            ),
            1
        );
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "gp64: encoder finalisation failed\n"
        );
    }
}
