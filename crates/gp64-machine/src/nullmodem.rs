//! Null-modem cable over TCP: UART bytes go to the peer unchanged.
//! Pumped on the emulation thread. Listening and dialling share
//! [`ModemLine`] with the modem; a connected line raises DSR, CTS and DCD,
//! and an unplugged one drops them and discards what the guest sends.
use crate::modem::{LineCommand, LineStatus, ModemLine, PendingConnection};
use gp64_hw::serial::{SerialPort, SerialPorts};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
        Arc, Mutex,
    },
};

/// Sent bytes held for the socket before the UART is left to back up.
const MAX_PENDING_TX: usize = 256;

pub struct NullModem {
    port: usize,
    line: ModemLine,
    commands: Receiver<LineCommand>,
    status: Arc<Mutex<LineStatus>>,
    shutdown: Arc<AtomicBool>,
    listener: Option<TcpListener>,
    dial: Option<PendingConnection>,
    stream: Option<TcpStream>,
    pending_tx: Vec<u8>,
    pending_rx: VecDeque<u8>,
    last_cycle: u64,
}
impl NullModem {
    /// A cable on the one-based COM port with nothing at the far end yet;
    /// see [`NullModem::line`].
    pub fn attach(com: u8, serial: &mut SerialPorts) -> io::Result<Self> {
        if !(1..=2).contains(&com) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "COM must be 1 or 2",
            ));
        }
        let shutdown = Arc::new(AtomicBool::new(false));
        let (line, commands, status) = ModemLine::new(com, Arc::clone(&shutdown));
        let port = usize::from(com - 1);
        serial.set_present(port, true);
        let mut modem = Self {
            port,
            line,
            commands,
            status,
            shutdown,
            listener: None,
            dial: None,
            stream: None,
            pending_tx: Vec::new(),
            pending_rx: VecDeque::new(),
            last_cycle: 0,
        };
        modem.unplug(&mut serial.ports[port]);
        Ok(modem)
    }
    /// A handle that connects and disconnects this cable.
    pub fn line(&self) -> ModemLine {
        self.line.clone()
    }
    pub(crate) fn start_at(&mut self, cycle: u64) {
        self.last_cycle = cycle;
    }
    pub(crate) fn pump(&mut self, serial: &mut SerialPorts, cycle: u64, rate: u64) {
        if cycle.saturating_sub(self.last_cycle) < (rate / 1000).max(1) {
            return;
        }
        self.last_cycle = cycle;
        self.service(&mut serial.ports[self.port]);
        serial.event_reschedule_pending = true;
    }
    fn set_status(&self, status: LineStatus) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
    }
    /// One pump: line commands, connection setup, then bytes both ways.
    fn service(&mut self, uart: &mut SerialPort) {
        // A guest reset clears the flag; the cable still owns the signals.
        uart.external_tx = true;
        while let Ok(command) = self.commands.try_recv() {
            self.listener = None;
            self.dial = None;
            self.disconnect(uart);
            match command {
                LineCommand::Listen(listener) => self.listener = Some(listener),
                LineCommand::Dial(dial) => self.dial = Some(dial),
                LineCommand::HangUp => self.set_status(LineStatus::Off),
            }
        }
        if let Some(result) = self.dial.as_ref().and_then(PendingConnection::poll) {
            self.dial = None;
            match result {
                Ok(stream) => self.connect(stream, uart),
                Err(error) => {
                    eprintln!("TCP serial connection failed: {error}");
                    self.set_status(LineStatus::Failed(crate::modem::short_reason(&error)));
                }
            }
        }
        if self.stream.is_none() {
            if let Some(Ok((stream, _))) = self.listener.as_ref().map(TcpListener::accept) {
                self.connect(stream, uart);
            }
        }
        if self.stream.is_some() {
            self.pump_bytes(uart);
        } else {
            // Nothing is plugged in: what the guest sends is lost.
            while uart.take_completed_tx_byte().is_some() {}
        }
    }
    fn connect(&mut self, stream: TcpStream, uart: &mut SerialPort) {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        // Small game packets must not wait for Nagle; failure only costs latency.
        let _ = stream.set_nodelay(true);
        let peer = stream
            .peer_addr()
            .map_or_else(|_| "peer".into(), |peer| peer.to_string());
        self.stream = Some(stream);
        self.pending_tx.clear();
        self.pending_rx.clear();
        uart.update_msr_signals(true, true, true, false);
        self.set_status(LineStatus::Connected(peer));
    }
    /// Drops a call; a host keeps listening for the next one.
    fn disconnect(&mut self, uart: &mut SerialPort) {
        if self.stream.take().is_none() {
            return;
        }
        self.unplug(uart);
        let next = self
            .listener
            .as_ref()
            .and_then(|listener| listener.local_addr().ok())
            .map_or(LineStatus::Off, |address| {
                LineStatus::Listening(address.port())
            });
        self.set_status(next);
    }
    fn unplug(&mut self, uart: &mut SerialPort) {
        self.pending_tx.clear();
        self.pending_rx.clear();
        uart.external_tx = true;
        uart.update_msr_signals(false, false, false, false);
    }
    /// UART -> socket, then socket -> UART, never taking more than the
    /// other side can hold.
    fn pump_bytes(&mut self, uart: &mut SerialPort) {
        let Some(stream) = self.stream.as_mut() else {
            return;
        };
        while self.pending_tx.len() < MAX_PENDING_TX {
            let Some(byte) = uart.take_completed_tx_byte() else {
                break;
            };
            self.pending_tx.push(byte);
        }
        let mut lost = false;
        if !self.pending_tx.is_empty() {
            match stream.write(&self.pending_tx) {
                Ok(n) => {
                    // Windows can report a full send buffer as a zero write;
                    // a closed peer shows up as a read EOF instead.
                    self.pending_tx.drain(..n);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => lost = true,
            }
        }
        // Bytes queued before the guest reset its receive FIFO are stale.
        if std::mem::take(&mut uart.rx_fifo_reset) {
            self.pending_rx.clear();
        }
        while uart.line_rx_capacity() > 0 {
            let Some(byte) = self.pending_rx.pop_front() else {
                break;
            };
            uart.enqueue_line_rx(byte);
        }
        let capacity = uart.line_rx_capacity();
        if !lost && capacity > 0 {
            let mut input = [0u8; 64];
            match stream.read(&mut input[..capacity.min(64)]) {
                Ok(0) => lost = true,
                Ok(n) => {
                    for &byte in &input[..n] {
                        if !uart.enqueue_line_rx(byte) {
                            self.pending_rx.push_back(byte);
                        }
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => lost = true,
            }
        }
        if lost {
            self.disconnect(uart);
        }
    }
}
impl Drop for NullModem {
    fn drop(&mut self) {
        // Stops a dial still retrying in the background.
        self.shutdown.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modem::ModemConnection;
    use gp64_hw::serial::msr;
    use std::{
        net::SocketAddr,
        time::{Duration, Instant},
    };

    const COM1: u16 = 0x3F8;
    const RATE: u64 = 1_000_000;

    struct End {
        serial: SerialPorts,
        modem: NullModem,
        cycle: u64,
    }
    impl End {
        fn new() -> Self {
            let mut serial = SerialPorts::new();
            let modem = NullModem::attach(1, &mut serial).unwrap();
            let mut end = Self {
                serial,
                modem,
                cycle: 0,
            };
            // 115200 baud, 8N1, FIFO on, DTR and RTS up.
            end.serial.write(COM1 + 3, 0x80);
            end.serial.write(COM1, 1);
            end.serial.write(COM1 + 1, 0);
            end.serial.write(COM1 + 3, 0x03);
            end.serial.write(COM1 + 2, 0x07);
            end.serial.write(COM1 + 4, 0x03);
            end.serial.ports[0].set_cycles_per_second(RATE);
            end
        }
        /// One millisecond of guest time: the UART shifts, the cable pumps.
        fn step(&mut self) {
            self.cycle += RATE / 1000;
            self.serial.advance_to_cycle(self.cycle);
            self.modem.pump(&mut self.serial, self.cycle, RATE);
        }
        fn status(&self) -> LineStatus {
            self.modem.line().status()
        }
        fn msr(&mut self) -> u8 {
            self.serial.read(COM1 + 6).unwrap()
        }
        fn send(&mut self, bytes: &[u8]) {
            for &byte in bytes {
                while self.serial.read(COM1 + 5).unwrap() & 0x20 == 0 {
                    self.step();
                }
                self.serial.write(COM1, byte);
            }
        }
        fn received(&mut self) -> Vec<u8> {
            let mut bytes = Vec::new();
            while self.serial.read(COM1 + 5).unwrap() & 0x01 != 0 {
                bytes.push(self.serial.read(COM1).unwrap());
            }
            bytes
        }
    }
    fn until(ends: &mut [&mut End], mut done: impl FnMut(&mut [&mut End]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(ends) {
            assert!(Instant::now() < deadline, "timed out");
            for end in ends.iter_mut() {
                end.step();
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    /// A host and a caller, connected through the line handles.
    fn linked() -> (End, End) {
        let mut host = End::new();
        let mut caller = End::new();
        let address: SocketAddr = host
            .modem
            .line()
            .start(&ModemConnection::Host(0))
            .unwrap()
            .unwrap();
        host.step();
        caller
            .modem
            .line()
            .call(&format!("127.0.0.1:{}", address.port()))
            .unwrap();
        until(&mut [&mut host, &mut caller], |ends| {
            ends.iter()
                .all(|end| matches!(end.status(), LineStatus::Connected(_)))
        });
        (host, caller)
    }

    #[test]
    fn unplugged_cable_drops_signals_and_discards_sent_bytes() {
        let mut end = End::new();
        end.step();
        assert_eq!(end.msr() & (msr::DSR | msr::CTS | msr::DCD), 0);
        end.send(b"lost");
        for _ in 0..20 {
            end.step();
        }
        assert!(end.modem.pending_tx.is_empty());
        assert_eq!(end.serial.ports[0].take_completed_tx_byte(), None);
        assert_eq!(end.status(), LineStatus::Off);
    }

    #[test]
    fn linked_cables_carry_bytes_both_ways_and_raise_signals() {
        let (mut host, mut caller) = linked();
        for end in [&mut host, &mut caller] {
            let signals = end.msr() & (msr::DSR | msr::CTS | msr::DCD);
            assert_eq!(signals, msr::DSR | msr::CTS | msr::DCD);
        }
        let payload: Vec<u8> = (0..=255).cycle().take(2000).collect();
        caller.send(&payload);
        host.send(b"reply");
        let mut at_host = Vec::new();
        let mut at_caller = Vec::new();
        until(&mut [&mut host, &mut caller], |ends| {
            at_host.extend(ends[0].received());
            at_caller.extend(ends[1].received());
            at_host.len() >= payload.len() && at_caller.len() >= 5
        });
        assert_eq!(at_host, payload);
        assert_eq!(at_caller, b"reply");
    }

    #[test]
    fn peer_loss_unplugs_and_a_host_listens_again() {
        let (mut host, mut caller) = linked();
        caller.modem.line().hang_up();
        until(&mut [&mut host, &mut caller], |ends| {
            matches!(ends[0].status(), LineStatus::Listening(_))
                && ends[1].status() == LineStatus::Off
        });
        assert_eq!(host.msr() & (msr::DSR | msr::CTS | msr::DCD), 0);
        assert_eq!(caller.msr() & (msr::DSR | msr::CTS | msr::DCD), 0);
    }

    #[test]
    fn a_call_replaces_an_idle_listener_but_not_a_live_call() {
        let (_host, caller) = linked();
        assert!(caller.modem.line().call("127.0.0.1:1").is_err());
        let end = End::new();
        let line = end.modem.line();
        line.start(&ModemConnection::Host(0)).unwrap();
        assert!(line.start(&ModemConnection::Host(0)).is_err());
        line.call("127.0.0.1:1").unwrap();
        assert_eq!(line.status(), LineStatus::Connecting("127.0.0.1:1".into()));
    }

    #[test]
    fn a_failed_dial_reports_failure() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let mut end = End::new();
        end.modem.line().call(&format!("127.0.0.1:{port}")).unwrap();
        until(&mut [&mut end], |ends| {
            matches!(ends[0].status(), LineStatus::Failed(_))
        });
    }

    #[test]
    fn a_receive_fifo_reset_drops_bytes_still_queued() {
        let (mut host, mut caller) = linked();
        host.modem.pending_rx.extend(b"stale");
        host.serial.write(COM1 + 2, 0x03);
        host.step();
        assert!(host.modem.pending_rx.is_empty());
        caller.send(b"x");
        let mut got = Vec::new();
        until(&mut [&mut host, &mut caller], |ends| {
            got.extend(ends[0].received());
            !got.is_empty()
        });
        assert_eq!(got, b"x");
    }
}
