//! Direct TCP phone line: Hayes bytes at the UART, 16 kHz PCM on the socket.
//! DSP cadence and socket buffering share the same PCM sample clock.
use gp64_hw::serial::{mcr, SerialPorts};
use gp64_modem::{
    wire::{self, WireBuffer, CHUNK_SAMPLES, SILENCE},
    ModemConfig, ModemDsp, ModemPhase,
};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const CAPACITY: usize = 4096;
/// Selects the time source for the same DSP and TCP pump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModemClock {
    RealTime,
    /// Advance both peers in slices shorter than 10 ms. Each tick consumes
    /// exactly the preceding peer PCM frame, so socket scheduling cannot add
    /// silence or change DSP time. The first tick starts with silence.
    /// Both peers must take the line on the same tick: a dial blocks its
    /// engine's next tick until it connects or fails, and a new listener's
    /// first tick waits for its caller. Start both lines before advancing
    /// either peer.
    EmulatedTime,
}
struct EmulatedTime {
    engine: Box<Engine>,
    remainder: u128,
    rate: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModemConnection {
    Host(u16),
    Join(String),
}
/// What the phone line is doing. Shared with the frontend; the DSP thread
/// owns the sockets and is the only writer once a line has started.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LineStatus {
    #[default]
    Off,
    /// Waiting for a caller on this TCP port.
    Listening(u16),
    /// Resolving and connecting to this `host:port`.
    Connecting(String),
    /// Carrying PCM to and from this peer address.
    Connected(String),
    /// The last start or dial failed; a short reason.
    Failed(String),
}
impl LineStatus {
    /// A line is up or coming up: a new one cannot start until it is hung up.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Listening(_) | Self::Connecting(_) | Self::Connected(_)
        )
    }
}
impl std::fmt::Display for LineStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("Off"),
            Self::Listening(port) => write!(f, "Listening on :{port}"),
            Self::Connecting(target) => write!(f, "Connecting to {target}"),
            Self::Connected(peer) => write!(f, "Connected to {peer}"),
            Self::Failed(reason) => write!(f, "Failed: {reason}"),
        }
    }
}
/// A few words for a socket error, short enough for one menu row.
pub(crate) fn short_reason(error: &io::Error) -> String {
    use io::ErrorKind::{
        AddrInUse, AddrNotAvailable, ConnectionAborted, ConnectionRefused, ConnectionReset,
        HostUnreachable, InvalidInput, NetworkDown, NetworkUnreachable, PermissionDenied, TimedOut,
    };
    match error.kind() {
        ConnectionRefused => "refused".into(),
        TimedOut => "timed out".into(),
        AddrInUse => "port in use".into(),
        AddrNotAvailable => "address unavailable".into(),
        PermissionDenied => "permission denied".into(),
        HostUnreachable | NetworkUnreachable => "unreachable".into(),
        NetworkDown => "network down".into(),
        ConnectionReset | ConnectionAborted => "connection reset".into(),
        InvalidInput => "bad address".into(),
        _ if error.raw_os_error().is_none() => "host not found".into(),
        _ => {
            let text = error.to_string();
            text.chars().take(24).collect()
        }
    }
}
pub(crate) enum LineCommand {
    Listen(TcpListener),
    Dial(PendingConnection),
    HangUp,
}
/// Starts and ends the phone line of an attached modem from any thread.
/// Sockets are bound and dialled off the caller's thread except the
/// listener bind, which never blocks. The guest-visible modem keeps its
/// Hayes state across lines; a hang-up drops carrier like a lost line.
#[derive(Clone)]
pub struct ModemLine {
    commands: mpsc::Sender<LineCommand>,
    status: Arc<Mutex<LineStatus>>,
    shutdown: Arc<AtomicBool>,
    com: u8,
}
impl ModemLine {
    /// A line handle and the receiver its owner applies commands from.
    pub(crate) fn new(
        com: u8,
        shutdown: Arc<AtomicBool>,
    ) -> (Self, Receiver<LineCommand>, Arc<Mutex<LineStatus>>) {
        let (commands, receiver) = mpsc::channel();
        let status = Arc::<Mutex<LineStatus>>::default();
        let line = Self {
            commands,
            status: Arc::clone(&status),
            shutdown,
            com,
        };
        (line, receiver, status)
    }
    /// The one-based COM port the modem answers on, fixed for the run.
    pub fn com(&self) -> u8 {
        self.com
    }
    pub fn status(&self) -> LineStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Listens (bound before returning) or starts an asynchronous dial.
    /// Refused while a line is up or coming up. Host port zero selects an
    /// ephemeral port; the bound address is returned for hosts.
    pub fn start(&self, connection: &ModemConnection) -> io::Result<Option<SocketAddr>> {
        self.start_unless(connection, LineStatus::is_active)
    }
    /// Dials `target`, replacing a listener that has no caller yet.
    /// Refused while a dial or call is up.
    pub fn call(&self, target: &str) -> io::Result<()> {
        self.start_unless(&ModemConnection::Join(target.to_owned()), |status| {
            matches!(status, LineStatus::Connecting(_) | LineStatus::Connected(_))
        })
        .map(drop)
    }
    fn start_unless(
        &self,
        connection: &ModemConnection,
        busy: impl FnOnce(&LineStatus) -> bool,
    ) -> io::Result<Option<SocketAddr>> {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if busy(&status) {
            return Err(io::Error::new(
                io::ErrorKind::ResourceBusy,
                "the modem line is in use; hang up first",
            ));
        }
        let gone = || io::Error::new(io::ErrorKind::BrokenPipe, "modem stopped");
        match connection {
            ModemConnection::Host(port) => {
                let listener = TcpListener::bind(("0.0.0.0", *port))
                    .and_then(|listener| listener.set_nonblocking(true).map(|()| listener))
                    .and_then(|listener| listener.local_addr().map(|address| (listener, address)));
                let (listener, address) = match listener {
                    Ok(bound) => bound,
                    Err(error) => {
                        *status = LineStatus::Failed(short_reason(&error));
                        return Err(error);
                    }
                };
                self.commands
                    .send(LineCommand::Listen(listener))
                    .map_err(|_| gone())?;
                *status = LineStatus::Listening(address.port());
                Ok(Some(address))
            }
            ModemConnection::Join(target) => {
                let dial = PendingConnection::start(target.clone(), Arc::clone(&self.shutdown))?;
                self.commands
                    .send(LineCommand::Dial(dial))
                    .map_err(|_| gone())?;
                *status = LineStatus::Connecting(target.clone());
                Ok(None)
            }
        }
    }
    /// Ends any listener, dial or call. The guest sees NO CARRIER if a call
    /// was up. The status becomes `Off` when the modem processes it.
    pub fn hang_up(&self) {
        let _ = self.commands.send(LineCommand::HangUp);
    }
}
#[derive(Default)]
struct Queues {
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    dtr_drop: bool,
    signals: (bool, bool, bool, bool),
    /// Speaker audio at [`SPEAKER_RATE`], only while the speaker sounds.
    speaker: VecDeque<f32>,
}
/// The DSP's speaker sample rate.
const SPEAKER_RATE: u32 = 8_000;
/// Speaker samples held for playback before the oldest are dropped: a
/// stalled guest must not replay a stale handshake later.
const SPEAKER_BACKLOG: usize = SPEAKER_RATE as usize / 5;
/// Linear interpolation from the 8 kHz speaker to the chip sample rate.
#[derive(Default)]
struct SpeakerPlayback {
    previous: f32,
    next: f32,
    /// Progress from `previous` to `next`, in chip-rate units.
    phase: u32,
}
/// Owns a DSP thread. Dropping the attachment closes the phone line and joins it.
pub struct TcpModem {
    port: usize,
    queues: Arc<Mutex<Queues>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    last_cycle: u64,
    emulated: Option<EmulatedTime>,
    dtr: bool,
    cts: bool,
    line: ModemLine,
    speaker: SpeakerPlayback,
}
impl TcpModem {
    /// Bind before returning; resolve and connect asynchronously with a deadline.
    /// Port is the one-based COM number. Host port zero selects an ephemeral port.
    pub fn attach(
        connection: &ModemConnection,
        com: u8,
        serial: &mut SerialPorts,
    ) -> io::Result<(Self, Option<SocketAddr>)> {
        Self::attach_with_clock(connection, com, serial, ModemClock::RealTime)
    }
    pub fn attach_with_clock(
        connection: &ModemConnection,
        com: u8,
        serial: &mut SerialPorts,
        clock: ModemClock,
    ) -> io::Result<(Self, Option<SocketAddr>)> {
        let modem = Self::attach_line(com, serial, clock)?;
        let address = modem.line.start(connection)?;
        Ok((modem, address))
    }
    /// A modem on the COM port with no phone line yet; see [`TcpModem::line`].
    pub fn attach_line(com: u8, serial: &mut SerialPorts, clock: ModemClock) -> io::Result<Self> {
        if !(1..=2).contains(&com) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "COM must be 1 or 2",
            ));
        }
        let queues = Arc::new(Mutex::new(Queues::default()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (line, receiver, status) = ModemLine::new(com, Arc::clone(&shutdown));
        let mut engine = Box::new(Engine::new(Arc::clone(&queues), None, None, clock));
        engine.commands = receiver;
        engine.status = status;
        let (thread, emulated) = match clock {
            ModemClock::RealTime => {
                let stop = Arc::clone(&shutdown);
                let thread = thread::Builder::new()
                    .name("modem-dsp".into())
                    .spawn(move || run(stop, engine))?;
                (Some(thread), None)
            }
            ModemClock::EmulatedTime => (
                None,
                Some(EmulatedTime {
                    engine,
                    remainder: 0,
                    rate: 0,
                }),
            ),
        };
        let port = usize::from(com - 1);
        serial.set_present(port, true);
        serial.ports[port].external_tx = true;
        Ok(Self {
            port,
            queues,
            shutdown,
            thread,
            emulated,
            last_cycle: 0,
            dtr: false,
            cts: true,
            line,
            speaker: SpeakerPlayback::default(),
        })
    }
    /// A handle that starts and ends this modem's phone line.
    pub fn line(&self) -> ModemLine {
        self.line.clone()
    }
    /// Fills each chip-rate sample's `modem` with the speaker, interpolated
    /// up from 8 kHz. When the speaker is quiet or behind, it is silent.
    pub(crate) fn mix_speaker(&mut self, audio: &mut [crate::AudioSample]) {
        if audio.is_empty() {
            return;
        }
        let mut q = self.queues.lock().unwrap_or_else(|e| e.into_inner());
        let playback = &mut self.speaker;
        for sample in audio {
            playback.phase += SPEAKER_RATE;
            while playback.phase >= gp64_opl2::SAMPLE_RATE {
                playback.phase -= gp64_opl2::SAMPLE_RATE;
                playback.previous = playback.next;
                playback.next = q.speaker.pop_front().unwrap_or(0.0);
            }
            let t = playback.phase as f32 / gp64_opl2::SAMPLE_RATE as f32;
            sample.modem = playback.previous + (playback.next - playback.previous) * t;
        }
    }
    pub(crate) fn start_at(&mut self, cycle: u64) {
        self.last_cycle = cycle;
    }
    pub(crate) fn pump(&mut self, serial: &mut SerialPorts, cycle: u64, rate: u64) {
        if cycle.saturating_sub(self.last_cycle) < (rate / 1000).max(1) {
            return;
        }
        let elapsed = cycle.saturating_sub(self.last_cycle);
        self.last_cycle = cycle;
        let uart = &mut serial.ports[self.port];
        let mut q = self.queues.lock().unwrap_or_else(|e| e.into_inner());
        let dtr = uart.mcr() & mcr::DTR != 0;
        q.dtr_drop |= self.dtr && !dtr;
        self.dtr = dtr;
        if q.tx.len() >= CAPACITY * 3 / 4 {
            self.cts = false;
        } else if q.tx.len() <= CAPACITY / 4 {
            self.cts = true;
        }
        while q.tx.len() < CAPACITY {
            let Some(byte) = uart.take_completed_tx_byte() else {
                break;
            };
            q.tx.push_back(byte);
        }
        let (dsr, cts, dcd, ri) = q.signals;
        uart.update_msr_signals(dsr, cts && self.cts, dcd, ri);
        while uart.line_rx_capacity() > 0 {
            let Some(byte) = q.rx.pop_front() else {
                break;
            };
            assert!(uart.enqueue_line_rx(byte));
        }
        serial.event_reschedule_pending = true;
        drop(q);
        if let Some(time) = &mut self.emulated {
            if time.rate != 0 && time.rate != rate {
                time.remainder = time.remainder * u128::from(rate) / u128::from(time.rate);
            }
            time.rate = rate;
            time.remainder += u128::from(elapsed) * 100;
            while time.remainder >= u128::from(rate) {
                time.remainder -= u128::from(rate);
                time.engine.tick();
            }
        }
    }
}
impl Drop for TcpModem {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
const CONNECT_LIMIT: Duration = Duration::from_secs(10);
// Longer than Windows takes to report a refused connection (it retries the
// SYN for about 2 s), so a refusal surfaces as "refused" instead of each
// slice timing out. A hang-up does not wait for the slice: dropping the
// pending dial sets the line Off at once and the worker exits after it.
const CONNECT_SLICE: Duration = Duration::from_secs(4);

// The resolver and every retry share the original setup deadline.
fn connect_addresses<T>(
    addresses: Vec<SocketAddr>,
    deadline: Instant,
    shutdown: &AtomicBool,
    cancelled: &AtomicBool,
    mut now: impl FnMut() -> Instant,
    mut connect: impl FnMut(&SocketAddr, Duration) -> io::Result<T>,
) -> io::Result<T> {
    let mut addresses: VecDeque<_> = addresses.into();
    let mut result = Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "no TCP addresses",
    ));
    while let Some(address) = addresses.pop_front() {
        let now = now();
        if shutdown.load(Ordering::Acquire) || cancelled.load(Ordering::Acquire) || now >= deadline
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TCP setup cancelled or timed out",
            ));
        }
        let remaining = deadline.saturating_duration_since(now).min(CONNECT_SLICE);
        result = connect(&address, remaining);
        match &result {
            Ok(_) => return result,
            // Give later resolver entries a turn before retrying this one.
            Err(e) if e.kind() == io::ErrorKind::TimedOut => addresses.push_back(address),
            Err(_) => {}
        }
    }
    result
}

pub(crate) struct PendingConnection {
    receiver: Receiver<io::Result<TcpStream>>,
    deadline: Instant,
    /// Set when the dial is dropped (hang-up or a new line) so the worker
    /// stops retrying; a late stream is dropped with the channel.
    cancel: Arc<AtomicBool>,
}
impl Drop for PendingConnection {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl PendingConnection {
    pub(crate) fn start(target: String, shutdown: Arc<AtomicBool>) -> io::Result<Self> {
        Self::resolve(
            move || target.to_socket_addrs().map(Iterator::collect),
            shutdown,
        )
    }

    fn resolve(
        resolve: impl FnOnce() -> io::Result<Vec<SocketAddr>> + Send + 'static,
        shutdown: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let deadline = Instant::now() + CONNECT_LIMIT;
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&cancel);
        // The host resolver has no cancellation API. Its worker owns no
        // emulation state and is never joined during shutdown. The receiver's
        // deadline includes resolution; a late result is simply dropped.
        thread::Builder::new()
            .name("modem-connect".into())
            .spawn(move || {
                let result = resolve().and_then(|addresses| {
                    let stream = connect_addresses(
                        addresses,
                        deadline,
                        &shutdown,
                        &cancelled,
                        Instant::now,
                        TcpStream::connect_timeout,
                    )?;
                    stream.set_nonblocking(true)?;
                    stream.set_nodelay(true)?;
                    Ok(stream)
                });
                let _ = sender.send(result);
            })?;
        Ok(Self {
            receiver,
            deadline,
            cancel,
        })
    }

    pub(crate) fn poll(&self) -> Option<io::Result<TcpStream>> {
        if Instant::now() >= self.deadline {
            return Some(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TCP setup timed out",
            )));
        }
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(Self::stopped())),
        }
    }
    /// The result, waiting for it until the deadline.
    fn wait(&self) -> io::Result<TcpStream> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        match self.receiver.recv_timeout(remaining) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TCP setup timed out",
            )),
            Err(RecvTimeoutError::Disconnected) => Err(Self::stopped()),
        }
    }
    fn stopped() -> io::Error {
        io::Error::new(io::ErrorKind::ConnectionAborted, "TCP setup worker stopped")
    }
}

struct Engine {
    queues: Arc<Mutex<Queues>>,
    commands: Receiver<LineCommand>,
    status: Arc<Mutex<LineStatus>>,
    listener: Option<TcpListener>,
    connection: Option<PendingConnection>,
    stream: Option<TcpStream>,
    dsp: ModemDsp,
    wire: WireBuffer,
    pending: VecDeque<u8>,
    clock: ModemClock,
    first_tick: bool,
    /// A new listener has not yet taken a call (see [`ModemClock::EmulatedTime`]).
    first_accept: bool,
    discard_bytes: usize,
    health: LineHealth,
}
/// How well the real-time PCM stream kept pace over the last few seconds.
#[derive(Default)]
struct LineHealth {
    ticks: u32,
    /// Ticks that found no complete peer frame and played silence.
    starved: u32,
    /// Ticks the DSP thread woke too late to run.
    lost: u32,
    /// Most peer frames waiting at the start of a tick.
    peak: usize,
}
impl LineHealth {
    /// Ticks between reports: five seconds.
    const PERIOD: u32 = 500;
    fn tick(&mut self, queued: usize, starved: bool) {
        self.ticks += 1;
        self.peak = self.peak.max(queued);
        self.starved += u32::from(starved);
        if self.ticks >= Self::PERIOD {
            tracing::info!(
                target: "gp64::modem::line",
                "modem line: {} ms of peer audio queued (peak {} ms), {} ticks starved, {} ticks lost late",
                queued * 10,
                self.peak * 10,
                self.starved,
                self.lost,
            );
            *self = Self::default();
        }
    }
}
impl Engine {
    fn new(
        queues: Arc<Mutex<Queues>>,
        listener: Option<TcpListener>,
        stream: Option<TcpStream>,
        clock: ModemClock,
    ) -> Self {
        if let Some(socket) = &stream {
            Self::configure_pcm_socket(socket, clock);
        }
        Self {
            queues,
            // Replaced by an attachment; a lone engine has no line control.
            commands: mpsc::channel().1,
            status: Arc::default(),
            listener,
            connection: None,
            stream,
            dsp: ModemDsp::new(ModemConfig {
                max_speed: 14400,
                ..ModemConfig::default()
            }),
            wire: WireBuffer::new(),
            pending: VecDeque::new(),
            clock,
            first_tick: true,
            first_accept: false,
            discard_bytes: 0,
            health: LineHealth::default(),
        }
    }
    fn configure_pcm_socket(socket: &TcpStream, clock: ModemClock) {
        if clock == ModemClock::EmulatedTime {
            socket.set_nonblocking(false).expect("blocking PCM socket");
            socket
                .set_read_timeout(Some(Duration::from_secs(600)))
                .expect("PCM read hang guard");
            socket
                .set_write_timeout(Some(Duration::from_secs(600)))
                .expect("PCM write hang guard");
        }
    }
    fn set_status(&self, status: LineStatus) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
    }
    fn close_stream(
        stream: &mut Option<TcpStream>,
        listener: Option<&TcpListener>,
        status: &Mutex<LineStatus>,
    ) {
        if stream.take().is_some() {
            // A host keeps listening for the next call; a dial is over.
            let next = listener
                .and_then(|listener| listener.local_addr().ok())
                .map_or(LineStatus::Off, |address| {
                    LineStatus::Listening(address.port())
                });
            *status.lock().unwrap_or_else(|e| e.into_inner()) = next;
        }
    }
    /// Drops any listener, dial and call; a dropped call reaches the guest
    /// as a lost line.
    fn drop_line(&mut self) {
        self.listener = None;
        self.connection = None;
        if self.stream.take().is_some() {
            self.pending.clear();
            self.wire.clear();
            self.discard_bytes = 0;
            self.dsp.notify_tcp_disconnected();
        }
    }
    /// Waits for a caller on a non-blocking listener until the deadline.
    fn accept_by(listener: &TcpListener, deadline: Instant) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            match listener.accept() {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1));
                }
                result => return result,
            }
        }
    }
    fn apply_commands(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            self.drop_line();
            match command {
                LineCommand::Listen(listener) => {
                    self.listener = Some(listener);
                    self.first_accept = true;
                }
                LineCommand::Dial(dial) => self.connection = Some(dial),
                LineCommand::HangUp => self.set_status(LineStatus::Off),
            }
        }
    }
    fn tick(&mut self) {
        self.apply_commands();
        let Self {
            queues,
            commands: _,
            status,
            listener,
            connection,
            stream,
            dsp,
            wire,
            pending,
            clock,
            first_tick,
            first_accept,
            discard_bytes,
            health,
        } = self;
        let result = connection.as_ref().and_then(|dial| match clock {
            ModemClock::RealTime => dial.poll(),
            ModemClock::EmulatedTime => Some(dial.wait()),
        });
        if let Some(result) = result {
            *connection = None;
            let next = match result {
                Ok(incoming) => {
                    Self::configure_pcm_socket(&incoming, *clock);
                    *first_tick = true;
                    let peer = incoming
                        .peer_addr()
                        .map_or_else(|_| "peer".into(), |peer| peer.to_string());
                    *stream = Some(incoming);
                    LineStatus::Connected(peer)
                }
                Err(error) => {
                    eprintln!("TCP modem connection failed: {error}");
                    LineStatus::Failed(short_reason(&error))
                }
            };
            *status.lock().unwrap_or_else(|e| e.into_inner()) = next;
        }
        if stream.is_none() {
            if let Some(listener) = listener.as_ref() {
                let accepted = if std::mem::take(first_accept) && *clock == ModemClock::EmulatedTime
                {
                    Self::accept_by(listener, Instant::now() + CONNECT_LIMIT)
                } else {
                    listener.accept()
                };
                if let Ok((incoming, peer)) = accepted {
                    if incoming.set_nonblocking(true).is_ok() && incoming.set_nodelay(true).is_ok()
                    {
                        Self::configure_pcm_socket(&incoming, *clock);
                        *first_tick = true;
                        *stream = Some(incoming);
                        *status.lock().unwrap_or_else(|e| e.into_inner()) =
                            LineStatus::Connected(peer.to_string());
                    }
                }
            }
        }
        let before = dsp.phase();
        let mut disconnect = false;
        {
            let mut q = queues.lock().unwrap_or_else(|e| e.into_inner());
            if std::mem::take(&mut q.dtr_drop) {
                dsp.notify_tcp_disconnected();
                disconnect = true;
            }
            while let Some(byte) = q.tx.pop_front() {
                dsp.receive_from_serial(byte);
            }
        }
        // Preserve ATH/ATZ even if a later UART command starts the next call.
        // Close before PCM I/O (which can block), and don't report carrier loss
        // back into the new DSP call state.
        if dsp.take_disconnect_request() {
            Self::close_stream(stream, listener.as_ref(), status);
            pending.clear();
            wire.clear();
            *discard_bytes = 0;
        }
        let mut input = [0u8; 4096];
        if dsp.take_flush_wire_request() {
            if *clock == ModemClock::RealTime {
                // A flush may stop inside a frame. Discard its remaining tail
                // when it arrives so the next frame starts at a PCM boundary.
                let mut offset = (wire.buffered_bytes() + wire::CHUNK_BYTES - *discard_bytes)
                    % wire::CHUNK_BYTES;
                if let Some(socket) = stream.as_mut() {
                    loop {
                        match socket.read(&mut input) {
                            Ok(0) => {
                                disconnect = true;
                                break;
                            }
                            Ok(n) => offset = (offset + n) % wire::CHUNK_BYTES,
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                            Err(_) => {
                                disconnect = true;
                                break;
                            }
                        }
                    }
                }
                *discard_bytes = (wire::CHUNK_BYTES - offset) % wire::CHUNK_BYTES;
            }
            wire.clear();
        }
        let line_in = if *clock == ModemClock::EmulatedTime {
            if *first_tick {
                *first_tick = false;
                SILENCE
            } else if let Some(socket) = stream.as_mut() {
                let mut encoded = [0u8; wire::CHUNK_BYTES];
                match socket.read_exact(&mut encoded) {
                    Ok(()) => {
                        wire.push(&encoded);
                        wire.take_chunk().expect("complete PCM frame")
                    }
                    Err(e)
                        if e.kind() == io::ErrorKind::UnexpectedEof
                            || e.kind() == io::ErrorKind::ConnectionReset
                            // Windows reports a peer hang-up as an abort.
                            || e.kind() == io::ErrorKind::ConnectionAborted =>
                    {
                        disconnect = true;
                        SILENCE
                    }
                    Err(e) => panic!("emulated-time TCP PCM receive failed: {e}"),
                }
            } else {
                SILENCE
            }
        } else {
            if let Some(socket) = stream.as_mut() {
                match socket.read(&mut input) {
                    Ok(0) => disconnect = true,
                    Ok(n) => {
                        let skipped = n.min(*discard_bytes);
                        *discard_bytes -= skipped;
                        wire.push(&input[skipped..n]);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => disconnect = true,
                }
            }
            let queued = wire.chunks_available();
            let chunk = wire.take_chunk();
            if stream.is_some() {
                health.tick(queued, chunk.is_none());
            }
            chunk.unwrap_or(SILENCE)
        };
        let mut line_out = [0i16; CHUNK_SAMPLES];
        dsp.tick_dsp(&line_in, &mut line_out);
        disconnect |= before != ModemPhase::Idle && dsp.phase() == ModemPhase::Idle;
        if let Some(socket) = stream.as_mut() {
            // Keep partial writes queued; never change PCM sample alignment.
            pending.extend(wire::encode_chunk(&line_out));
            while !pending.is_empty() {
                let (bytes, _) = pending.as_slices();
                match socket.write(bytes) {
                    Ok(0) => {
                        disconnect = true;
                        break;
                    }
                    Ok(n) => {
                        pending.drain(..n);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        disconnect = true;
                        break;
                    }
                }
            }
            if pending.len() > wire::CHUNK_BYTES * 204 {
                disconnect = true;
            }
        }
        if disconnect {
            Self::close_stream(stream, listener.as_ref(), status);
            pending.clear();
            wire.clear();
            *discard_bytes = 0;
            dsp.notify_tcp_disconnected();
        }
        {
            let mut q = queues.lock().unwrap_or_else(|e| e.into_inner());
            let s = dsp.signals();
            q.signals = (s.dsr, s.cts, s.dcd, s.ri);
            // Stop draining rather than discard serial result or payload bytes.
            while q.rx.len() < CAPACITY {
                let Some(byte) = dsp.send_to_serial() else {
                    break;
                };
                q.rx.push_back(byte);
            }
            // Always drain the DSP; keep what the speaker would play.
            let mut speaker = [0f32; 80];
            let count = dsp.speaker_samples_raw(&mut speaker);
            if speaker_audible(dsp.speaker_mode(), dsp.is_online()) {
                q.speaker.extend(&speaker[..count]);
                let excess = q.speaker.len().saturating_sub(SPEAKER_BACKLOG);
                q.speaker.drain(..excess);
            }
        }
    }
}
/// Whether the speaker sounds, by the Hayes `M` register: M0 never, M2
/// always, M1 and M3 until the call is online, as mdpc plays it.
fn speaker_audible(mode: u8, online: bool) -> bool {
    match mode {
        0 => false,
        2 => true,
        _ => !online,
    }
}
fn run(shutdown: Arc<AtomicBool>, mut engine: Box<Engine>) {
    let mut next_tick = Instant::now();
    while !shutdown.load(Ordering::Acquire) {
        engine.tick();
        next_tick += Duration::from_millis(10);
        let now = Instant::now();
        if next_tick > now {
            thread::sleep(next_tick - now);
        } else {
            // Only a call has a peer to fall behind.
            let late = if engine.stream.is_some() {
                (now - next_tick).as_millis() / 10
            } else {
                0
            };
            engine.health.lost += u32::try_from(late).unwrap_or(u32::MAX);
            next_tick = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DialFixture {
        start: Instant,
        elapsed: std::cell::Cell<Duration>,
        shutdown: AtomicBool,
        cancelled: AtomicBool,
    }

    impl DialFixture {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                elapsed: std::cell::Cell::new(Duration::ZERO),
                shutdown: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
            }
        }

        fn run(
            &self,
            count: u16,
            mut connector: impl FnMut(SocketAddr, Duration) -> io::Result<u8>,
        ) -> (io::Result<u8>, Vec<(u16, Duration)>) {
            let addresses = (0..count)
                .map(|port| SocketAddr::from(([127, 0, 0, 1], port)))
                .collect();
            let mut attempts = Vec::new();
            let result = connect_addresses(
                addresses,
                self.start + CONNECT_LIMIT,
                &self.shutdown,
                &self.cancelled,
                || self.start + self.elapsed.get(),
                |address, budget| {
                    attempts.push((address.port(), budget));
                    connector(*address, budget)
                },
            );
            (result, attempts)
        }

        fn timeout(&self, budget: Duration) -> io::Result<u8> {
            self.elapsed.set(self.elapsed.get() + budget);
            Err(io::ErrorKind::TimedOut.into())
        }
    }

    #[test]
    fn dial_scheduler_reaches_second_address_after_timeout() {
        let fixture = DialFixture::new();
        let (result, attempts) = fixture.run(2, |address, budget| {
            if address.port() == 0 {
                fixture.timeout(budget)
            } else {
                Ok(17)
            }
        });
        assert_eq!(result.unwrap(), 17);
        assert_eq!(attempts, [(0, CONNECT_SLICE), (1, CONNECT_SLICE)]);
        assert_eq!(fixture.elapsed.get(), CONNECT_SLICE);
    }

    #[test]
    fn dial_scheduler_retries_rounds_and_retires_permanent_errors() {
        let fixture = DialFixture::new();
        let mut retries = 0;
        let (result, attempts) = fixture.run(3, |address, budget| {
            if address.port() == 0 {
                return Err(io::ErrorKind::ConnectionRefused.into());
            }
            retries += 1;
            if retries == 3 {
                Ok(23)
            } else {
                fixture.timeout(budget)
            }
        });
        assert_eq!(result.unwrap(), 23);
        assert_eq!(
            attempts,
            [
                (0, CONNECT_SLICE),
                (1, CONNECT_SLICE),
                (2, CONNECT_SLICE),
                (1, Duration::from_secs(2))
            ]
        );
    }

    #[test]
    fn dial_scheduler_bounds_total_timeout_and_resolution_time() {
        let fixture = DialFixture::new();
        let (result, attempts) = fixture.run(2, |_, budget| fixture.timeout(budget));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            attempts,
            [
                (0, CONNECT_SLICE),
                (1, CONNECT_SLICE),
                (0, Duration::from_secs(2))
            ]
        );
        assert_eq!(fixture.elapsed.get(), CONNECT_LIMIT);

        let fixture = DialFixture::new();
        fixture.elapsed.set(Duration::from_secs(7));
        let (result, attempts) = fixture.run(2, |_, budget| fixture.timeout(budget));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(attempts, [(0, Duration::from_secs(3))]);
        assert_eq!(fixture.elapsed.get(), CONNECT_LIMIT);
    }

    #[test]
    fn dial_scheduler_preserves_empty_and_refusal_errors() {
        let fixture = DialFixture::new();
        let (result, attempts) = fixture.run(0, |_, _| panic!("empty resolver connected"));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(attempts.is_empty());

        let fixture = DialFixture::new();
        let (result, attempts) = fixture.run(2, |address, _| {
            Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("refused {}", address.port()),
            ))
        });
        let error = result.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
        assert_eq!(error.to_string(), "refused 1");
        assert_eq!(attempts, [(0, CONNECT_SLICE), (1, CONNECT_SLICE)]);
    }

    #[test]
    fn dial_scheduler_stops_for_cancellation_shutdown_and_expired_deadline() {
        for guard in 0..3 {
            for during_attempt in [false, true] {
                let fixture = DialFixture::new();
                let stop = || match guard {
                    0 => fixture.cancelled.store(true, Ordering::Release),
                    1 => fixture.shutdown.store(true, Ordering::Release),
                    _ => fixture.elapsed.set(CONNECT_LIMIT),
                };
                if !during_attempt {
                    stop();
                }
                let mut calls = 0;
                let (result, attempts) = fixture.run(2, |_, _| {
                    calls += 1;
                    stop();
                    if calls == 1 {
                        Err(io::ErrorKind::TimedOut.into())
                    } else {
                        Ok(99)
                    }
                });
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
                assert_eq!(attempts.len(), usize::from(during_attempt), "guard {guard}");
            }
        }
    }

    fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (socket, _) = listener.accept().unwrap();
        socket.set_nonblocking(true).unwrap();
        (socket, peer)
    }

    fn escaped_online_dsp() -> ModemDsp {
        let config = || ModemConfig {
            max_speed: 14400,
            ..ModemConfig::default()
        };
        let mut caller = ModemDsp::new(config());
        let mut answerer = ModemDsp::new(config());
        for &byte in b"ATD5551234\r" {
            caller.receive_from_serial(byte);
        }
        for &byte in b"ATA\r" {
            answerer.receive_from_serial(byte);
        }
        let mut caller_pcm = SILENCE;
        let mut answerer_pcm = SILENCE;
        let tick = |caller: &mut ModemDsp,
                    answerer: &mut ModemDsp,
                    caller_pcm: &mut [i16; CHUNK_SAMPLES],
                    answerer_pcm: &mut [i16; CHUNK_SAMPLES]| {
            let previous_caller = *caller_pcm;
            caller.tick_dsp(answerer_pcm, caller_pcm);
            answerer.tick_dsp(&previous_caller, answerer_pcm);
        };
        for _ in 0..3000 {
            tick(
                &mut caller,
                &mut answerer,
                &mut caller_pcm,
                &mut answerer_pcm,
            );
            if caller.is_online() && answerer.is_online() {
                break;
            }
        }
        assert!(
            caller.is_online() && answerer.is_online(),
            "handshake failed"
        );
        for _ in 0..120 {
            tick(
                &mut caller,
                &mut answerer,
                &mut caller_pcm,
                &mut answerer_pcm,
            );
        }
        for &byte in b"+++" {
            caller.receive_from_serial(byte);
        }
        for _ in 0..120 {
            tick(
                &mut caller,
                &mut answerer,
                &mut caller_pcm,
                &mut answerer_pcm,
            );
        }
        // An accepted AT response proves escape succeeded without losing carrier.
        while caller.send_to_serial().is_some() {}
        for &byte in b"AT\r" {
            caller.receive_from_serial(byte);
        }
        let response: Vec<_> = std::iter::from_fn(|| caller.send_to_serial()).collect();
        assert!(String::from_utf8_lossy(&response).contains("OK"));
        assert_eq!(caller.phase(), ModemPhase::Online);
        caller.take_flush_wire_request();
        caller
    }

    fn hangup_then_answer_closes_original_stream(commands: &[u8]) {
        for clock in [ModemClock::RealTime, ModemClock::EmulatedTime] {
            let (socket, mut original_peer) = socket_pair();
            original_peer
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let queues = Arc::new(Mutex::new(Queues::default()));
            let mut engine = Engine::new(Arc::clone(&queues), Some(listener), Some(socket), clock);
            engine.dsp = escaped_online_dsp();
            engine.set_status(LineStatus::Connected("original peer".into()));
            engine.first_tick = false;
            // Original code must fail promptly even if it tries blocking PCM I/O.
            engine
                .stream
                .as_ref()
                .unwrap()
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            engine.wire.push(&[1, 2, 3]);
            engine.pending.extend([4, 5, 6]);
            engine.discard_bytes = 7;
            queues.lock().unwrap().tx.extend(commands);
            engine.tick();
            assert!(engine.stream.is_none(), "original TCP call survived hangup");
            assert_eq!(
                original_peer.read(&mut [0; 1]).unwrap(),
                0,
                "old peer must receive EOF"
            );
            assert_eq!(engine.dsp.phase(), ModemPhase::CallSetup);
            assert_eq!(
                *engine.status.lock().unwrap(),
                LineStatus::Listening(address.port())
            );
            assert!(engine.pending.is_empty());
            assert_eq!(engine.wire.buffered_bytes(), 0);
            assert_eq!(engine.discard_bytes, 0);
            // A fresh accepted socket belongs to the later ATA, not the old call.
            let _next_peer = TcpStream::connect(address).unwrap();
            engine.tick();
            assert!(engine.stream.is_some());
            assert!(matches!(
                *engine.status.lock().unwrap(),
                LineStatus::Connected(_)
            ));
            assert_eq!(engine.dsp.phase(), ModemPhase::CallSetup);
        }
    }

    #[test]
    fn same_line_hangup_answer_closes_original_stream() {
        hangup_then_answer_closes_original_stream(b"ATHA\r");
    }

    #[test]
    fn batched_hangup_answer_closes_original_stream() {
        hangup_then_answer_closes_original_stream(b"ATH\rATA\r");
    }

    fn wait_bytes(socket: &TcpStream, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut bytes = [0; 4096];
        loop {
            if matches!(socket.peek(&mut bytes), Ok(n) if n >= count) {
                return;
            }
            assert!(Instant::now() < deadline, "fragment was not delivered");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn shutdown_and_deadline_do_not_wait_for_resolution() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let (release, blocked) = mpsc::channel();
        let (entered, resolving) = mpsc::channel();
        let mut connection = PendingConnection::resolve(
            move || {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(Vec::new())
            },
            Arc::clone(&shutdown),
        )
        .unwrap();
        resolving.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(connection.poll().is_none());
        connection.deadline = Instant::now();
        assert_eq!(
            connection.poll().unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        // Keep resolution blocked while the DSP observes cancellation.
        connection.deadline = Instant::now() + CONNECT_LIMIT;
        let mut engine = Box::new(Engine::new(
            Arc::new(Mutex::new(Queues::default())),
            None,
            None,
            ModemClock::RealTime,
        ));
        engine.connection = Some(connection);
        let stop = Arc::clone(&shutdown);
        let dsp = thread::spawn(move || run(stop, engine));
        let started = Instant::now();
        shutdown.store(true, Ordering::Release);
        dsp.join().unwrap();
        assert!(started.elapsed() < Duration::from_millis(200));
        release.send(()).unwrap();
    }

    fn wait_status(line: &ModemLine, done: impl Fn(&LineStatus) -> bool) -> LineStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = line.status();
            if done(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "line stuck at {status:?}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn hang_up_then_host_again_on_the_same_port() {
        let mut serial = SerialPorts::new();
        let modem = TcpModem::attach_line(2, &mut serial, ModemClock::RealTime).unwrap();
        let line = modem.line();
        assert_eq!((line.status(), line.com()), (LineStatus::Off, 2));
        let port = line
            .start(&ModemConnection::Host(0))
            .unwrap()
            .unwrap()
            .port();
        assert_eq!(line.status(), LineStatus::Listening(port));
        let peer = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let status = wait_status(&line, |s| matches!(s, LineStatus::Connected(_)));
        assert_eq!(
            status,
            LineStatus::Connected(peer.local_addr().unwrap().to_string())
        );
        // The host side closes first, so its port has a closing connection.
        line.hang_up();
        wait_status(&line, |s| *s == LineStatus::Off);
        let mut byte = [0];
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = &peer;
        loop {
            match reader.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let again = line.start(&ModemConnection::Host(port)).unwrap().unwrap();
        assert_eq!(again.port(), port);
        assert_eq!(line.status(), LineStatus::Listening(port));
        drop(modem);
    }

    #[test]
    fn a_caller_that_hangs_up_leaves_the_host_listening() {
        let mut serial = SerialPorts::new();
        let modem = TcpModem::attach_line(1, &mut serial, ModemClock::RealTime).unwrap();
        let line = modem.line();
        let port = line
            .start(&ModemConnection::Host(0))
            .unwrap()
            .unwrap()
            .port();
        let peer = TcpStream::connect(("127.0.0.1", port)).unwrap();
        wait_status(&line, |s| matches!(s, LineStatus::Connected(_)));
        drop(peer);
        wait_status(&line, |s| *s == LineStatus::Listening(port));
    }

    #[test]
    fn join_to_a_closed_port_fails_without_blocking_the_caller() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = closed.local_addr().unwrap().to_string();
        drop(closed);
        let mut serial = SerialPorts::new();
        let modem = TcpModem::attach_line(2, &mut serial, ModemClock::RealTime).unwrap();
        let line = modem.line();
        let started = Instant::now();
        assert_eq!(
            line.start(&ModemConnection::Join(target.clone())).unwrap(),
            None
        );
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(line.status(), LineStatus::Connecting(target));
        let status = wait_status(&line, |s| !s.is_active());
        assert_eq!(status, LineStatus::Failed("refused".into()));
        assert_eq!(status.to_string(), "Failed: refused");
        // A failed line can be replaced without a hang-up.
        line.start(&ModemConnection::Host(0)).unwrap();
    }

    #[test]
    fn a_line_in_use_refuses_a_new_one_until_hung_up() {
        let mut serial = SerialPorts::new();
        let modem = TcpModem::attach_line(2, &mut serial, ModemClock::RealTime).unwrap();
        let line = modem.line();
        // TEST-NET-1 never answers, so the dial stays in flight.
        let dial = ModemConnection::Join("192.0.2.1:9".into());
        line.start(&dial).unwrap();
        for connection in [&dial, &ModemConnection::Host(0)] {
            let error = line.start(connection).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::ResourceBusy);
        }
        assert_eq!(line.status(), LineStatus::Connecting("192.0.2.1:9".into()));
        line.hang_up();
        wait_status(&line, |s| *s == LineStatus::Off);
        let port = line
            .start(&ModemConnection::Host(0))
            .unwrap()
            .unwrap()
            .port();
        assert!(line.start(&dial).is_err());
        line.hang_up();
        wait_status(&line, |s| *s == LineStatus::Off);
        // The listener is closed: nothing answers on its port.
        assert!(TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_secs(1)
        )
        .is_err());
    }

    #[test]
    fn a_port_in_use_reports_failed() {
        let taken = TcpListener::bind("0.0.0.0:0").unwrap();
        let mut serial = SerialPorts::new();
        let modem = TcpModem::attach_line(2, &mut serial, ModemClock::RealTime).unwrap();
        let line = modem.line();
        let port = taken.local_addr().unwrap().port();
        assert!(line.start(&ModemConnection::Host(port)).is_err());
        assert_eq!(line.status(), LineStatus::Failed("port in use".into()));
    }

    #[test]
    fn status_text() {
        assert_eq!(LineStatus::Off.to_string(), "Off");
        assert_eq!(LineStatus::Listening(23).to_string(), "Listening on :23");
        assert_eq!(
            LineStatus::Connecting("h:1".into()).to_string(),
            "Connecting to h:1"
        );
        assert_eq!(
            LineStatus::Connected("1.2.3.4:5".into()).to_string(),
            "Connected to 1.2.3.4:5"
        );
        assert!(!LineStatus::Failed(String::new()).is_active());
    }

    #[test]
    fn shutdown_during_pending_join_is_prompt() {
        let mut serial = SerialPorts::new();
        let started = Instant::now();
        let (modem, _) =
            TcpModem::attach(&ModemConnection::Join("192.0.2.1:9".into()), 1, &mut serial)
                .expect("join setup must run off the caller thread");
        drop(modem);
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn realtime_flush_preserves_fragmented_pcm_alignment() {
        for (buffered, drained) in [(0, 3), (3, 4), (319, 3)] {
            let (socket, mut peer) = socket_pair();
            let queues = Arc::new(Mutex::new(Queues::default()));
            let mut engine = Engine::new(
                Arc::clone(&queues),
                None,
                Some(socket),
                ModemClock::RealTime,
            );
            let stale = wire::encode_chunk(&[1234; CHUNK_SAMPLES]);
            let mut prefix = stale.to_vec();
            prefix.extend_from_slice(&stale);
            if buffered > 0 {
                peer.write_all(&prefix[..buffered]).unwrap();
                wait_bytes(engine.stream.as_ref().unwrap(), buffered);
                engine.tick();
                assert_eq!(engine.wire.buffered_bytes(), buffered);
            }
            peer.write_all(&prefix[buffered..buffered + drained])
                .unwrap();
            wait_bytes(engine.stream.as_ref().unwrap(), drained);
            queues.lock().unwrap().tx.extend(b"ATA\r");
            engine.tick();
            let boundary = (buffered + drained).div_ceil(wire::CHUNK_BYTES) * wire::CHUNK_BYTES;
            let first = std::array::from_fn(|i| i as i16 * 17 - 900);
            let second = std::array::from_fn(|i| i as i16 * 23 + 700);
            let mut rest = prefix[buffered + drained..boundary].to_vec();
            rest.extend(wire::encode_chunk(&first));
            rest.extend(wire::encode_chunk(&second));
            peer.write_all(&rest).unwrap();
            wait_bytes(engine.stream.as_ref().unwrap(), rest.len());
            engine.tick();
            assert_eq!(engine.wire.take_chunk(), Some(second));
            assert_eq!(engine.wire.buffered_bytes(), 0);
        }
    }

    #[test]
    fn the_speaker_follows_the_m_register_like_mdpc() {
        for (mode, offline, online) in [
            (0, false, false),
            (1, true, false),
            (2, true, true),
            (3, true, false),
        ] {
            assert_eq!(
                speaker_audible(mode, false),
                offline,
                "M{mode} before carrier"
            );
            assert_eq!(speaker_audible(mode, true), online, "M{mode} online");
        }
    }

    fn silent_audio(count: usize) -> Vec<crate::AudioSample> {
        (0..count as u64)
            .map(|cycle| crate::AudioSample {
                cycle,
                speaker: 0.0,
                opl2: 0,
                modem: 0.0,
            })
            .collect()
    }

    #[test]
    fn speaker_audio_is_interpolated_to_the_chip_rate_and_silent_when_dry() {
        let mut serial = SerialPorts::new();
        let mut modem = TcpModem::attach_line(2, &mut serial, ModemClock::EmulatedTime).unwrap();
        let mut audio = silent_audio(1000);
        modem.mix_speaker(&mut audio);
        assert!(
            audio.iter().all(|sample| sample.modem == 0.0),
            "no speaker audio yet"
        );

        let input = [0.0, 0.25, 0.5, 0.75, 1.0, 1.0, 1.0, 1.0];
        modem.queues.lock().unwrap().speaker.extend(input);
        // Eight 8 kHz samples cover about 50 chip-rate samples; play past them.
        let mut audio = silent_audio(200);
        modem.mix_speaker(&mut audio);
        let played: Vec<f32> = audio.iter().map(|sample| sample.modem).collect();
        assert!(played.iter().all(|&value| (0.0..=1.0).contains(&value)));
        assert!(
            played.windows(2).take(30).all(|pair| pair[1] >= pair[0]),
            "the ramp rises smoothly: {played:?}"
        );
        assert!(played.contains(&1.0), "the peak is reached");
        assert!(played[150..].iter().all(|&value| value == 0.0), "dry again");
        assert!(modem.queues.lock().unwrap().speaker.is_empty());
    }

    #[test]
    fn dialling_sounds_on_the_speaker_unless_it_is_muted() {
        let dial = |command: &[u8]| {
            let queues = Arc::new(Mutex::new(Queues::default()));
            let mut engine = Engine::new(Arc::clone(&queues), None, None, ModemClock::RealTime);
            queues.lock().unwrap().tx.extend(command);
            for _ in 0..50 {
                engine.tick();
            }
            let speaker = std::mem::take(&mut queues.lock().unwrap().speaker);
            speaker
        };
        let heard = dial(b"ATDT5551234\r");
        assert!(
            heard.iter().any(|sample| sample.abs() > 0.01),
            "dial tones reach the speaker"
        );
        assert!(heard.len() <= SPEAKER_BACKLOG);
        assert!(
            dial(b"ATM0DT5551234\r").is_empty(),
            "M0 keeps the speaker off"
        );
    }
}
