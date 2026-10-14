//! LAPM control-plane helper.
//!
//! This module keeps byte-oriented LAPM/HDLC handling out of the modem state
//! machine. It preserves seam bytes, frames control traffic, exposes a small
//! transmit queue, and provides a minimal data path plus retry timer so a real
//! handoff loop can own negotiation without growing state-machine logic.

use std::collections::VecDeque;

use crate::crc::crc_ccitt;

const HDLC_FLAG: u8 = 0x7E;
const HDLC_ESCAPE: u8 = 0x7D;
const HDLC_ESCAPE_XOR: u8 = 0x20;
/// LAPM address octet for a COMMAND frame.
const HDLC_ADDRESS: u8 = 0x03;
/// LAPM address octet for a RESPONSE frame.
///
/// Every frame used to go out as `HDLC_ADDRESS`, so a received XID could not be
/// told apart from one we had sent: each peer answered the other's answer and
/// the exchange never settled (`MDD-BUG-CRUCIBLE-00518`). The address octet is
/// where HDLC carries that distinction, so that is where it is modelled.
const HDLC_ADDRESS_RESPONSE: u8 = 0x01;
const HDLC_POLL_FINAL_MASK: u8 = 0x10;

const CONTROL_INFORMATION: u8 = 0x00;
const CONTROL_RR: u8 = 0x01;
const CONTROL_XID: u8 = 0xAF;
const CONTROL_SABME: u8 = 0x6F;
const CONTROL_UA: u8 = 0x63;
const CONTROL_DM: u8 = 0x0F;
const CONTROL_DISC: u8 = 0x43;

const ACK_RETRY_TICKS: u32 = 20;
const ACK_RETRY_LIMIT: u8 = 2;
// One complete control retry budget, independent of received traffic.
const NEGOTIATION_TIMEOUT_TICKS: u32 = ACK_RETRY_TICKS * (ACK_RETRY_LIMIT as u32 + 1);
const DEFAULT_MAX_INFORMATION_BYTES: usize = 128;
const DEFAULT_WINDOW_SIZE: u8 = 1;
const LAPM_FRAME_OVERHEAD_BYTES: usize = 4; // address + control + FCS

/// Minimal negotiated LAPM session parameters.
///
/// The module enforces only a small bounded subset:
/// - maximum information-frame payload length
/// - bounded send window size
///
/// Phase-1 interop keeps the runtime defaults conservative (`k=1`,
/// `N401=128`). Wider windows remain an internal helper capability that the
/// unit tests can exercise without claiming a broader wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LapmSessionParameters {
    pub max_information_bytes: usize,
    pub window_size: u8,
}

impl LapmSessionParameters {
    fn default_local() -> Self {
        Self {
            max_information_bytes: DEFAULT_MAX_INFORMATION_BYTES,
            window_size: DEFAULT_WINDOW_SIZE,
        }
    }
}

/// Recognized LAPM control frame kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LapmControlKind {
    Xid,
    Sabme,
    Ua,
    Dm,
    Disc,
    Unknown(u8),
}

impl LapmControlKind {
    fn from_control_byte(control: u8) -> Self {
        match control & !HDLC_POLL_FINAL_MASK {
            CONTROL_XID => Self::Xid,
            CONTROL_SABME => Self::Sabme,
            CONTROL_UA => Self::Ua,
            CONTROL_DM => Self::Dm,
            CONTROL_DISC => Self::Disc,
            other => Self::Unknown(other),
        }
    }

    fn control_byte(self) -> u8 {
        match self {
            Self::Xid => CONTROL_XID,
            Self::Sabme => CONTROL_SABME,
            Self::Ua => CONTROL_UA,
            Self::Dm => CONTROL_DM,
            Self::Disc => CONTROL_DISC,
            Self::Unknown(control) => control,
        }
    }
}

/// Recognized LAPM supervisory frame kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LapmSupervisoryKind {
    Rr,
    Rnr,
    Rej,
    Srej,
    Unknown(u8),
}

impl LapmSupervisoryKind {
    fn from_control_byte(control: u8) -> Self {
        match (control & !HDLC_POLL_FINAL_MASK) & 0x0C {
            0x00 => Self::Rr,
            0x04 => Self::Rnr,
            0x08 => Self::Rej,
            0x0C => Self::Srej,
            other => Self::Unknown(other),
        }
    }

    fn control_byte(self) -> u8 {
        match self {
            Self::Rr => CONTROL_RR,
            Self::Rnr => CONTROL_RR | 0x04,
            Self::Rej => CONTROL_RR | 0x08,
            Self::Srej => CONTROL_RR | 0x0C,
            Self::Unknown(control) => control,
        }
    }
}

/// LAPM frame kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LapmFrameKind {
    Information,
    Supervisory(LapmSupervisoryKind),
    Control(LapmControlKind),
}

impl LapmFrameKind {
    fn from_control_byte(control: u8) -> Self {
        let raw = control & !HDLC_POLL_FINAL_MASK;
        if raw & 0x01 == 0 {
            Self::Information
        } else if raw & 0x03 == 0x01 {
            Self::Supervisory(LapmSupervisoryKind::from_control_byte(control))
        } else {
            Self::Control(LapmControlKind::from_control_byte(control))
        }
    }

    fn control_byte(self) -> u8 {
        match self {
            Self::Information => CONTROL_INFORMATION,
            Self::Supervisory(kind) => kind.control_byte(),
            Self::Control(kind) => kind.control_byte(),
        }
    }
}

/// Parsed LAPM frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LapmFrame {
    pub kind: LapmFrameKind,
    pub control: u8,
    pub payload: Vec<u8>,
    pub fcs_ok: bool,
    /// True when the peer sent this as a response rather than a command,
    /// decoded from the address octet.
    pub is_response: bool,
}

/// Simple negotiation / data status for the LAPM helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LapmStatus {
    Negotiating,
    AwaitingAck,
    Established,
    SessionRejected,
    RemoteRejected(LapmControlKind),
    RemoteTerminated(LapmControlKind),
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueuedInfoFrame {
    sequence: u8,
    payload: Vec<u8>,
}
struct XidAckWait {
    frame: Vec<u8>,
    elapsed_ticks: u32,
    retries_left: u8,
}

/// Byte-oriented LAPM control handler.
///
/// The type preserves the raw seam bytes it was initialized with, accepts more
/// bytes via [`feed`], queues framed control/data traffic via helper methods,
/// and uses [`tick`] to manage retransmit timers for the first usable handoff
/// loop.
pub struct V42Lapm {
    startup_bytes: Vec<u8>,
    session_parameters: LapmSessionParameters,
    status: LapmStatus,
    ever_established: bool,
    negotiation_elapsed_ticks: u32,
    rx_in_frame: bool,
    rx_escape_next: bool,
    rx_frame: Vec<u8>,
    rx_payloads: Vec<Vec<u8>>,
    tx_queue: Vec<u8>,
    pending_info_payloads: VecDeque<Vec<u8>>,
    awaiting_ack: bool,
    retry_elapsed_ticks: u32,
    retries_left: u8,
    pending_retransmit: Option<Vec<u8>>,
    xid_ack_wait: Option<XidAckWait>,
    tx_inflight_info_frames: VecDeque<QueuedInfoFrame>,
    tx_sequence: u8,
    rx_sequence: u8,
    remote_ready: bool,
}

impl V42Lapm {
    /// Create a LAPM control helper from V.42 seam bytes.
    pub fn new(startup_bytes: Vec<u8>) -> Self {
        Self {
            startup_bytes,
            session_parameters: LapmSessionParameters::default_local(),
            status: LapmStatus::Negotiating,
            ever_established: false,
            negotiation_elapsed_ticks: 0,
            rx_in_frame: false,
            rx_escape_next: false,
            rx_frame: Vec::new(),
            rx_payloads: Vec::new(),
            tx_queue: Vec::new(),
            pending_info_payloads: VecDeque::new(),
            tx_inflight_info_frames: VecDeque::new(),
            tx_sequence: 0,
            rx_sequence: 0,
            remote_ready: true,
            awaiting_ack: false,
            retry_elapsed_ticks: 0,
            retries_left: 0,
            pending_retransmit: None,
            xid_ack_wait: None,
        }
    }

    /// Preserve the original bytes from the V.42 seam.
    pub fn startup_bytes(&self) -> &[u8] {
        &self.startup_bytes
    }

    /// Current negotiation / data status.
    pub fn status(&self) -> LapmStatus {
        self.status
    }

    /// Whether this session has completed SABME/UA establishment.
    pub fn ever_established(&self) -> bool {
        self.ever_established
    }

    /// Whether the link is established and still carrying data.
    ///
    /// `AwaitingAck` is an ordinary data-transfer status: `queue_i_frame` — the
    /// only site that sets it — enters it when an I-frame is outstanding on an
    /// already-established link. Treating only the exact `Established` status
    /// as "up" made the modem report Training/offline for the duration of every
    /// ACK wait on a perfectly normal call, contradicting DCD and letting the
    /// speaker unmute mid-transfer (MDD-BUG-CRUCIBLE-00407).
    ///
    /// Gated on [`Self::ever_established`] as well, so a status that never
    /// completed SABME/UA cannot report a live link, and terminal statuses
    /// (rejected, remotely terminated, timed out) stay offline.
    pub fn is_data_link_live(&self) -> bool {
        self.ever_established
            && matches!(
                self.status,
                LapmStatus::Established | LapmStatus::AwaitingAck
            )
    }

    /// Current negotiated session parameters.
    pub fn session_parameters(&self) -> LapmSessionParameters {
        self.session_parameters
    }

    /// Number of bytes currently queued for transmit.
    pub fn pending_tx_len(&self) -> usize {
        self.tx_queue.len()
            + self
                .pending_info_payloads
                .iter()
                .map(|payload| payload.len())
                .sum::<usize>()
    }

    /// Whether a new LAPM I-frame can be queued.
    pub fn can_queue_i_frame(&self) -> bool {
        self.link_established()
            && self.remote_ready
            && self.tx_inflight_info_frames.len() < self.window_capacity()
    }

    /// Drain any payload bytes received from valid information frames.
    pub fn take_rx_payloads(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.rx_payloads)
    }

    /// Advance the timer by one handoff tick.
    ///
    /// If a frame is awaiting acknowledgement, this will retransmit the last
    /// frame after a short timeout and eventually mark the helper as timed out.
    /// A busy peer is probed with RR polls instead of information frames.
    pub fn tick(&mut self) {
        // A queued response has no ACK timer. Bound the whole establishment
        // phase even when only XID commands or incomplete frames arrive.
        if !self.ever_established && !self.terminal() {
            self.negotiation_elapsed_ticks = self.negotiation_elapsed_ticks.saturating_add(1);
            if self.negotiation_elapsed_ticks >= NEGOTIATION_TIMEOUT_TICKS {
                self.stop_outstanding_transmit();
                self.status = LapmStatus::TimedOut;
                return;
            }
        }

        // Parameter negotiation has its own acknowledgement timer: neither an
        // XID response nor a payload acknowledgement completes the other wait.
        if let Some(wait) = &mut self.xid_ack_wait {
            wait.elapsed_ticks = wait.elapsed_ticks.saturating_add(1);
            if wait.elapsed_ticks >= ACK_RETRY_TICKS {
                if wait.retries_left == 0 {
                    self.stop_outstanding_transmit();
                    self.status = LapmStatus::TimedOut;
                    return;
                }
                wait.elapsed_ticks = 0;
                wait.retries_left -= 1;
                self.tx_queue.extend_from_slice(&wait.frame);
            }
        }

        if !self.awaiting_ack {
            return;
        }

        self.retry_elapsed_ticks = self.retry_elapsed_ticks.saturating_add(1);
        if self.retry_elapsed_ticks < ACK_RETRY_TICKS {
            return;
        }

        self.retry_elapsed_ticks = 0;
        if self.retries_left == 0 {
            self.stop_outstanding_transmit();
            self.status = LapmStatus::TimedOut;
            return;
        }

        self.retries_left -= 1;
        // V.42 section 8.4.6 forbids I-frames while the peer receiver is busy.
        if !self.remote_ready {
            self.queue_remote_ready_poll();
            return;
        }

        if !self.tx_inflight_info_frames.is_empty() {
            self.retransmit_information_window();
            return;
        }

        if let Some(frame) = &self.pending_retransmit {
            self.tx_queue.extend_from_slice(frame);
        }
    }

    /// Feed additional LAPM / HDLC bytes into the parser.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<LapmFrame> {
        let mut frames = Vec::new();
        for &byte in bytes {
            self.ingest_byte(byte, &mut frames);
        }
        frames
    }

    /// Drain queued transmit bytes.
    pub fn take_tx_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx_queue)
    }

    /// Recover every DTE payload byte still owned by LAPM.
    ///
    /// A terminal LAPM status may fall back to a transparent carrier. In that
    /// case the caller must reclaim sent-but-unacknowledged frames followed by
    /// payload chunks still waiting for window capacity. Acknowledged frames
    /// have already left `tx_inflight_info_frames` and are never returned.
    pub fn take_unacknowledged_payloads(&mut self) -> Vec<u8> {
        let payload_len = self
            .tx_inflight_info_frames
            .iter()
            .map(|frame| frame.payload.len())
            .sum::<usize>()
            + self
                .pending_info_payloads
                .iter()
                .map(|payload| payload.len())
                .sum::<usize>();
        let mut payloads = Vec::with_capacity(payload_len);
        for frame in self.tx_inflight_info_frames.drain(..) {
            payloads.extend(frame.payload);
        }
        for payload in self.pending_info_payloads.drain(..) {
            payloads.extend(payload);
        }
        self.stop_outstanding_transmit();
        payloads
    }

    /// Queue a LAPM information frame.
    pub fn queue_i_frame(&mut self, payload: &[u8]) {
        let max_len = self.session_parameters.max_information_bytes.max(1);
        for chunk in payload.chunks(max_len) {
            self.pending_info_payloads.push_back(chunk.to_vec());
        }

        self.queue_next_info_frame();
        if !self.remote_ready && !self.pending_info_payloads.is_empty() && !self.awaiting_ack {
            self.awaiting_ack = true;
            self.retry_elapsed_ticks = 0;
            self.retries_left = ACK_RETRY_LIMIT;
            self.status = LapmStatus::AwaitingAck;
        }
    }

    /// Queue an XID control frame.
    pub fn queue_xid(&mut self, payload: &[u8]) {
        let frame = self.build_frame(LapmFrameKind::Control(LapmControlKind::Xid), payload);
        self.tx_queue.extend_from_slice(&frame);
        self.xid_ack_wait = Some(XidAckWait {
            frame,
            elapsed_ticks: 0,
            retries_left: ACK_RETRY_LIMIT,
        });
        if !self.link_established() {
            self.status = LapmStatus::Negotiating;
        }
    }

    /// Queue an XID RESPONSE — the answer to a peer's XID command.
    ///
    /// Unlike [`Self::queue_xid`] this awaits no acknowledgement and arms no
    /// retransmission: a response terminates a transaction rather than opening
    /// one.
    fn queue_xid_response(&mut self, payload: &[u8]) {
        let frame = self.build_frame_with_address(
            LapmFrameKind::Control(LapmControlKind::Xid),
            payload,
            HDLC_ADDRESS_RESPONSE,
        );
        self.tx_queue.extend_from_slice(&frame);
    }

    /// Queue a SABME control frame.
    pub fn queue_sabme(&mut self) {
        self.xid_ack_wait = None;
        self.queue_frame(
            LapmFrameKind::Control(LapmControlKind::Sabme),
            &[],
            true,
            Some(LapmStatus::Negotiating),
        );
    }

    /// Queue a UA control frame.
    pub fn queue_ua(&mut self) {
        self.queue_frame(
            LapmFrameKind::Control(LapmControlKind::Ua),
            &[],
            false,
            Some(LapmStatus::Established),
        );
    }

    /// Queue a RR acknowledgement frame.
    pub fn queue_rr(&mut self) {
        self.queue_frame(
            LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr),
            &[],
            false,
            Some(LapmStatus::Established),
        );
    }

    /// Queue a DM control frame.
    pub fn queue_dm(&mut self) {
        self.queue_frame(
            LapmFrameKind::Control(LapmControlKind::Dm),
            &[],
            false,
            Some(LapmStatus::RemoteRejected(LapmControlKind::Dm)),
        );
    }

    /// Queue a DISC control frame.
    pub fn queue_disc(&mut self) {
        self.queue_frame(
            LapmFrameKind::Control(LapmControlKind::Disc),
            &[],
            false,
            Some(LapmStatus::RemoteTerminated(LapmControlKind::Disc)),
        );
    }

    fn ingest_byte(&mut self, byte: u8, frames: &mut Vec<LapmFrame>) {
        if byte == HDLC_FLAG {
            if self.rx_in_frame {
                self.finish_frame(frames);
            }
            self.rx_in_frame = true;
            self.rx_escape_next = false;
            return;
        }

        if !self.rx_in_frame {
            return;
        }

        if byte == HDLC_ESCAPE {
            self.rx_escape_next = true;
            return;
        }

        let decoded = if self.rx_escape_next {
            self.rx_escape_next = false;
            byte ^ HDLC_ESCAPE_XOR
        } else {
            byte
        };

        let max_frame_bytes = self
            .session_parameters
            .max_information_bytes
            .saturating_add(LAPM_FRAME_OVERHEAD_BYTES);
        if self.rx_frame.len() >= max_frame_bytes {
            let oversized_information_frame = self.link_established()
                && matches!(
                    self.rx_frame
                        .get(1)
                        .copied()
                        .map(LapmFrameKind::from_control_byte),
                    Some(LapmFrameKind::Information)
                );
            self.rx_frame.clear();
            self.rx_in_frame = false;
            self.rx_escape_next = false;
            if oversized_information_frame {
                self.stop_outstanding_transmit();
                self.status = LapmStatus::SessionRejected;
            }
            return;
        }

        self.rx_frame.push(decoded);
    }

    fn finish_frame(&mut self, frames: &mut Vec<LapmFrame>) {
        if self.rx_frame.len() < 4 {
            self.rx_frame.clear();
            return;
        }

        let frame = std::mem::take(&mut self.rx_frame);
        let body_len = frame.len() - 2;
        let (body, fcs_bytes) = frame.split_at(body_len);
        let address = body[0];
        let control = body[1];
        let payload = body[2..].to_vec();
        let fcs = u16::from_le_bytes([fcs_bytes[0], fcs_bytes[1]]);
        let fcs_ok = crc_ccitt(body) == fcs;
        let kind = LapmFrameKind::from_control_byte(control);

        let parsed = LapmFrame {
            kind,
            control,
            payload,
            fcs_ok,
            is_response: address == HDLC_ADDRESS_RESPONSE,
        };

        self.update_status(&parsed);
        frames.push(parsed);
    }

    fn update_status(&mut self, frame: &LapmFrame) {
        if !frame.fcs_ok || self.terminal() {
            return;
        }

        match frame.kind {
            LapmFrameKind::Information => self.handle_information_frame(frame),
            LapmFrameKind::Supervisory(kind) => self.handle_supervisory_frame(kind, frame),
            LapmFrameKind::Control(kind) => match kind {
                LapmControlKind::Xid => {
                    if frame.is_response {
                        // Only an outstanding XID command is answered here.
                        // Unsolicited responses leave establishment and data
                        // retries alone, and responses never provoke a reply.
                        self.xid_ack_wait = None;
                    } else {
                        // A peer's XID command gets exactly one response, and a
                        // response never awaits an acknowledgement of its own.
                        self.queue_xid_response(&[]);
                    }
                }
                LapmControlKind::Sabme => {
                    self.xid_ack_wait = None;
                    self.tx_inflight_info_frames.clear();
                    self.tx_sequence = 0;
                    self.rx_sequence = 0;
                    self.remote_ready = true;
                    self.complete_ack_wait();
                    self.queue_ua();
                    self.queue_next_info_frame();
                    self.status = LapmStatus::Established;
                }
                LapmControlKind::Ua => {
                    self.ever_established = true;
                    self.complete_ack_wait();
                    self.remote_ready = true;
                    self.status = LapmStatus::Established;
                    self.queue_next_info_frame();
                }
                LapmControlKind::Dm => {
                    self.stop_outstanding_transmit();
                    self.status = LapmStatus::RemoteRejected(LapmControlKind::Dm);
                }
                LapmControlKind::Disc => {
                    self.stop_outstanding_transmit();
                    self.queue_frame(
                        LapmFrameKind::Control(LapmControlKind::Ua),
                        &[],
                        false,
                        None,
                    );
                    self.status = LapmStatus::RemoteTerminated(LapmControlKind::Disc);
                }
                LapmControlKind::Unknown(_) => {}
            },
        }
    }

    fn complete_ack_wait(&mut self) {
        self.retry_elapsed_ticks = 0;
        self.retries_left = 0;
        self.pending_retransmit = None;
        self.awaiting_ack = !self.tx_inflight_info_frames.is_empty();
    }

    fn stop_outstanding_transmit(&mut self) {
        self.tx_queue.clear();
        self.awaiting_ack = false;
        self.retry_elapsed_ticks = 0;
        self.retries_left = 0;
        self.pending_retransmit = None;
        self.xid_ack_wait = None;
        self.remote_ready = false;
    }

    fn queue_next_info_frame(&mut self) {
        if !self.remote_ready {
            return;
        }

        while self.can_queue_i_frame() {
            let Some(payload) = self.pending_info_payloads.pop_front() else {
                break;
            };

            self.queue_frame(
                LapmFrameKind::Information,
                &payload,
                true,
                Some(LapmStatus::AwaitingAck),
            );
        }
    }

    fn link_established(&self) -> bool {
        matches!(
            self.status,
            LapmStatus::Established | LapmStatus::AwaitingAck
        )
    }

    fn terminal(&self) -> bool {
        matches!(
            self.status,
            LapmStatus::SessionRejected
                | LapmStatus::RemoteRejected(_)
                | LapmStatus::RemoteTerminated(_)
                | LapmStatus::TimedOut
        )
    }

    fn queue_frame(
        &mut self,
        kind: LapmFrameKind,
        payload: &[u8],
        awaiting_ack: bool,
        status: Option<LapmStatus>,
    ) {
        let frame = self.build_frame(kind, payload);
        self.tx_queue.extend_from_slice(&frame);

        if awaiting_ack {
            self.awaiting_ack = true;
            if matches!(kind, LapmFrameKind::Information) {
                let sequence = self.tx_sequence;
                let was_empty = self.tx_inflight_info_frames.is_empty();
                self.tx_inflight_info_frames.push_back(QueuedInfoFrame {
                    sequence,
                    payload: payload.to_vec(),
                });
                self.tx_sequence = Self::next_sequence(sequence);
                if was_empty {
                    self.retry_elapsed_ticks = 0;
                    self.retries_left = ACK_RETRY_LIMIT;
                }
            } else {
                self.retry_elapsed_ticks = 0;
                self.retries_left = ACK_RETRY_LIMIT;
                self.pending_retransmit = Some(frame);
            }
        }

        if let Some(status) = status {
            if matches!(status, LapmStatus::Established) {
                self.ever_established = true;
            }
            self.status = status;
        }
    }

    fn build_frame(&self, kind: LapmFrameKind, payload: &[u8]) -> Vec<u8> {
        self.build_frame_with_address(kind, payload, HDLC_ADDRESS)
    }

    fn build_frame_with_address(
        &self,
        kind: LapmFrameKind,
        payload: &[u8],
        address: u8,
    ) -> Vec<u8> {
        let mut body = Vec::with_capacity(2 + payload.len() + 2);
        body.push(address);
        let control = match kind {
            LapmFrameKind::Information => {
                Self::information_control_byte(self.tx_sequence, self.rx_sequence)
            }
            LapmFrameKind::Supervisory(kind) => {
                Self::supervisory_control_byte(kind, self.rx_sequence)
            }
            LapmFrameKind::Control(kind) => LapmFrameKind::Control(kind).control_byte(),
        };
        body.push(control);
        body.extend_from_slice(payload);

        let fcs = crc_ccitt(&body).to_le_bytes();

        let mut tx = Vec::with_capacity(body.len() + 4);
        tx.push(HDLC_FLAG);
        self.push_escaped_bytes(&body, &mut tx);
        self.push_escaped_bytes(&fcs, &mut tx);
        tx.push(HDLC_FLAG);
        tx
    }

    fn push_escaped_bytes(&self, bytes: &[u8], tx: &mut Vec<u8>) {
        for &byte in bytes {
            if byte == HDLC_FLAG || byte == HDLC_ESCAPE {
                tx.push(HDLC_ESCAPE);
                tx.push(byte ^ HDLC_ESCAPE_XOR);
            } else {
                tx.push(byte);
            }
        }
    }
    fn window_capacity(&self) -> usize {
        self.session_parameters.window_size.clamp(1, 7) as usize
    }

    fn next_sequence(sequence: u8) -> u8 {
        (sequence + 1) & 0x07
    }

    fn previous_sequence(sequence: u8) -> u8 {
        (sequence + 7) & 0x07
    }

    fn sequence_distance(from: u8, to: u8) -> usize {
        ((to.wrapping_sub(from)) & 0x07) as usize
    }

    fn information_control_byte(sequence: u8, acknowledgement: u8) -> u8 {
        LapmFrameKind::Information.control_byte()
            | ((sequence & 0x07) << 1)
            | ((acknowledgement & 0x07) << 5)
    }

    fn supervisory_control_byte(kind: LapmSupervisoryKind, acknowledgement: u8) -> u8 {
        LapmFrameKind::Supervisory(kind).control_byte() | ((acknowledgement & 0x07) << 5)
    }

    fn information_sequence(control: u8) -> u8 {
        (control >> 1) & 0x07
    }

    fn information_ack(control: u8) -> u8 {
        (control >> 5) & 0x07
    }

    fn supervisory_ack(control: u8) -> u8 {
        (control >> 5) & 0x07
    }

    fn build_information_frame_bytes(&self, sequence: u8, payload: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(2 + payload.len() + 2);
        body.push(HDLC_ADDRESS);
        body.push(Self::information_control_byte(sequence, self.rx_sequence));
        body.extend_from_slice(payload);

        let fcs = crc_ccitt(&body).to_le_bytes();

        let mut tx = Vec::with_capacity(body.len() + 4);
        tx.push(HDLC_FLAG);
        self.push_escaped_bytes(&body, &mut tx);
        self.push_escaped_bytes(&fcs, &mut tx);
        tx.push(HDLC_FLAG);
        tx
    }

    fn acknowledge_information_frames(&mut self, acknowledgement: u8) {
        let Some(front) = self.tx_inflight_info_frames.front() else {
            return;
        };

        let distance = Self::sequence_distance(front.sequence, acknowledgement);
        if distance == 0 || distance > self.tx_inflight_info_frames.len() {
            return;
        }

        for _ in 0..distance {
            self.tx_inflight_info_frames.pop_front();
        }

        self.retry_elapsed_ticks = 0;
        self.retries_left = ACK_RETRY_LIMIT;
        if self.tx_inflight_info_frames.is_empty() {
            self.complete_ack_wait();
        } else {
            self.awaiting_ack = true;
        }
    }

    fn retransmit_information_window(&mut self) {
        let frames: Vec<_> = self.tx_inflight_info_frames.iter().cloned().collect();
        for frame in frames {
            let bytes = self.build_information_frame_bytes(frame.sequence, &frame.payload);
            self.tx_queue.extend_from_slice(&bytes);
        }
    }

    fn queue_remote_ready_poll(&mut self) {
        let control = Self::supervisory_control_byte(LapmSupervisoryKind::Rr, self.rx_sequence)
            | HDLC_POLL_FINAL_MASK;
        let bytes = self.build_supervisory_frame_bytes(control);
        self.tx_queue.extend_from_slice(&bytes);
    }

    fn build_supervisory_frame_bytes(&self, control: u8) -> Vec<u8> {
        let body = [HDLC_ADDRESS, control];
        let fcs = crc_ccitt(&body).to_le_bytes();

        let mut tx = Vec::with_capacity(body.len() + 4);
        tx.push(HDLC_FLAG);
        self.push_escaped_bytes(&body, &mut tx);
        self.push_escaped_bytes(&fcs, &mut tx);
        tx.push(HDLC_FLAG);
        tx
    }

    fn handle_information_frame(&mut self, frame: &LapmFrame) {
        // V.42 section 8.8 requires a disconnected station to discard
        // information frames until SABME/UA has established the data link.
        if !self.link_established() {
            return;
        }

        let sequence = Self::information_sequence(frame.control);
        let acknowledgement = Self::information_ack(frame.control);
        self.acknowledge_information_frames(acknowledgement);

        if frame.payload.len() > self.session_parameters.max_information_bytes {
            self.stop_outstanding_transmit();
            self.status = LapmStatus::SessionRejected;
            return;
        }

        if sequence == self.rx_sequence {
            self.rx_payloads.push(frame.payload.clone());
            self.rx_sequence = Self::next_sequence(self.rx_sequence);
            self.queue_frame(
                LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr),
                &[],
                false,
                Some(LapmStatus::Established),
            );
            self.queue_next_info_frame();
        } else if sequence == Self::previous_sequence(self.rx_sequence) {
            self.queue_frame(
                LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr),
                &[],
                false,
                Some(LapmStatus::Established),
            );
        } else {
            self.queue_frame(
                LapmFrameKind::Supervisory(LapmSupervisoryKind::Rej),
                &[],
                false,
                Some(LapmStatus::Established),
            );
        }
    }

    fn handle_supervisory_frame(&mut self, kind: LapmSupervisoryKind, frame: &LapmFrame) {
        if !self.link_established() {
            return;
        }

        let remote_was_busy = !self.remote_ready;
        let acknowledgement = Self::supervisory_ack(frame.control);
        self.acknowledge_information_frames(acknowledgement);

        match kind {
            LapmSupervisoryKind::Rr => {
                self.remote_ready = true;
                if remote_was_busy && !self.tx_inflight_info_frames.is_empty() {
                    self.retry_elapsed_ticks = 0;
                    self.retries_left = ACK_RETRY_LIMIT;
                    self.awaiting_ack = true;
                    self.retransmit_information_window();
                }
                self.queue_next_info_frame();
            }
            LapmSupervisoryKind::Rnr => {
                self.remote_ready = false;
                if !self.tx_inflight_info_frames.is_empty()
                    || !self.pending_info_payloads.is_empty()
                {
                    self.awaiting_ack = true;
                    self.retry_elapsed_ticks = 0;
                    self.retries_left = ACK_RETRY_LIMIT;
                }
            }
            LapmSupervisoryKind::Rej => {
                self.remote_ready = true;
                self.retransmit_information_window();
                self.queue_next_info_frame();
            }
            LapmSupervisoryKind::Srej => {
                self.remote_ready = true;
                self.retransmit_information_window();
            }
            LapmSupervisoryKind::Unknown(_) => {}
        }

        if !matches!(
            self.status,
            LapmStatus::RemoteRejected(_)
                | LapmStatus::RemoteTerminated(_)
                | LapmStatus::TimedOut
                | LapmStatus::SessionRejected
        ) {
            self.status = if self.awaiting_ack {
                LapmStatus::AwaitingAck
            } else {
                LapmStatus::Established
            };
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn established_lapm() -> V42Lapm {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.status = LapmStatus::Established;
        lapm.ever_established = true;
        lapm
    }

    fn decode_tx_frames(lapm: &mut V42Lapm) -> Vec<LapmFrame> {
        let mut peer = V42Lapm::new(Vec::new());
        peer.feed(&lapm.take_tx_bytes())
    }

    fn busy_lapm_with_pending_payloads() -> V42Lapm {
        let mut lapm = established_lapm();
        lapm.session_parameters = LapmSessionParameters {
            max_information_bytes: 4,
            window_size: 1,
        };

        let incoming = V42Lapm::information_control_byte(0, 0);
        assert_eq!(
            lapm.feed(&build_raw_frame(incoming, b"rx"))[0].kind,
            LapmFrameKind::Information
        );
        assert_eq!(lapm.take_rx_payloads(), vec![b"rx".to_vec()]);
        assert_eq!(
            decode_tx_frames(&mut lapm)
                .iter()
                .map(|frame| frame.kind)
                .collect::<Vec<_>>(),
            vec![LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr)]
        );

        lapm.queue_i_frame(b"old");
        let prior = decode_tx_frames(&mut lapm);
        assert_eq!(prior.len(), 1);
        assert_eq!(prior[0].kind, LapmFrameKind::Information);
        assert_eq!(prior[0].payload, b"old");

        lapm.queue_i_frame(b"held");
        assert_eq!(lapm.pending_tx_len(), 4);

        let rnr = V42Lapm::supervisory_control_byte(LapmSupervisoryKind::Rnr, 1);
        assert_eq!(
            lapm.feed(&build_raw_frame(rnr, &[]))[0].kind,
            LapmFrameKind::Supervisory(LapmSupervisoryKind::Rnr)
        );
        assert!(!lapm.can_queue_i_frame());
        assert_eq!(lapm.pending_tx_len(), 4);

        lapm.queue_i_frame(b"new");
        assert_eq!(lapm.pending_tx_len(), 7);
        assert!(lapm.take_tx_bytes().is_empty());
        lapm
    }

    fn assert_information_keeps_busy_state(
        sequence: u8,
        response: LapmSupervisoryKind,
        received_payloads: Vec<Vec<u8>>,
    ) {
        for resume_kind in [LapmSupervisoryKind::Rr, LapmSupervisoryKind::Rej] {
            let mut lapm = busy_lapm_with_pending_payloads();
            let control = V42Lapm::information_control_byte(sequence, 1);
            let frames = lapm.feed(&build_raw_frame(control, b"data"));

            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].kind, LapmFrameKind::Information);
            assert_eq!(lapm.take_rx_payloads(), received_payloads.clone());
            assert_eq!(
                decode_tx_frames(&mut lapm)
                    .iter()
                    .map(|frame| frame.kind)
                    .collect::<Vec<_>>(),
                vec![LapmFrameKind::Supervisory(response)],
                "incoming information must retain its normal RR/REJ response"
            );

            lapm.queue_i_frame(b"probe");
            assert!(
                decode_tx_frames(&mut lapm)
                    .iter()
                    .all(|frame| frame.kind != LapmFrameKind::Information),
                "a busy peer must not receive an I-frame after reverse-direction data"
            );
            assert!(!lapm.can_queue_i_frame());
            assert_eq!(lapm.pending_tx_len(), 12);

            assert_supervisory_resumes_pending_payloads(resume_kind, &mut lapm);
        }
    }

    #[test]
    fn negotiation_deadline_survives_xid_and_partial_traffic() {
        let mut lapm = V42Lapm::new(Vec::new());
        let xid = build_raw_frame(CONTROL_XID, &[]);
        for tick in 1..=60 {
            // A peer command queues a response but never opens an ACK wait.
            let frames = lapm.feed(&xid);
            assert!(frames.iter().any(|frame| frame.fcs_ok
                && frame.kind == LapmFrameKind::Control(LapmControlKind::Xid)
                && !frame.is_response));
            lapm.feed(&[HDLC_FLAG, HDLC_ADDRESS, HDLC_ESCAPE]);
            assert!(
                lapm.pending_tx_len() > 0,
                "a valid command queued its response"
            );
            if tick < 60 {
                lapm.take_tx_bytes();
            }
            lapm.tick();
            if tick < 60 {
                assert_eq!(lapm.status(), LapmStatus::Negotiating);
            }
        }
        assert_eq!(
            lapm.status(),
            LapmStatus::TimedOut,
            "valid XID commands and partial frames must not extend negotiation"
        );
        assert_eq!(lapm.pending_tx_len(), 0);
        lapm.feed(&build_raw_frame(CONTROL_SABME, &[]));
        lapm.feed(&build_raw_frame(CONTROL_UA, &[]));
        lapm.tick();
        assert_eq!(
            lapm.status(),
            LapmStatus::TimedOut,
            "terminal negotiation cannot be revived by later establishment frames"
        );
        assert_eq!(lapm.pending_tx_len(), 0);
    }

    #[test]
    fn test_startup_bytes_are_preserved() {
        let startup = vec![HDLC_FLAG, HDLC_ADDRESS, CONTROL_SABME, HDLC_FLAG];
        let lapm = V42Lapm::new(startup.clone());

        assert_eq!(lapm.startup_bytes(), startup.as_slice());
        assert_eq!(lapm.status(), LapmStatus::Negotiating);
    }

    #[test]
    fn test_queue_sabme_round_trip() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.queue_sabme();

        let tx = lapm.take_tx_bytes();
        let mut parser = V42Lapm::new(Vec::new());
        let frames = parser.feed(&tx);

        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].kind,
            LapmFrameKind::Control(LapmControlKind::Sabme)
        );
        assert_eq!(frames[0].control, CONTROL_SABME);
        assert!(frames[0].fcs_ok);
        assert_eq!(parser.status(), LapmStatus::Established);
        assert!(
            !parser.take_tx_bytes().is_empty(),
            "receiver should emit UA"
        );
    }

    #[test]
    fn test_information_waits_for_ua_and_sabme_retry_stays_first() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.queue_sabme();
        let sabme = lapm.take_tx_bytes();

        lapm.queue_i_frame(b"hello");

        assert_eq!(lapm.status(), LapmStatus::Negotiating);
        assert!(
            lapm.take_tx_bytes().is_empty(),
            "application data must remain pending until UA establishes the link"
        );

        for _ in 0..ACK_RETRY_TICKS {
            lapm.tick();
        }
        assert_eq!(
            lapm.take_tx_bytes(),
            sabme,
            "SABME must remain the outstanding retry before establishment"
        );

        let frames = lapm.feed(&build_raw_frame(CONTROL_UA, &[]));
        assert_eq!(frames[0].kind, LapmFrameKind::Control(LapmControlKind::Ua));
        assert_eq!(lapm.status(), LapmStatus::AwaitingAck);

        let mut parser = V42Lapm::new(Vec::new());
        let frames = parser.feed(&lapm.take_tx_bytes());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].kind, LapmFrameKind::Information);
        assert_eq!(frames[0].payload, b"hello");
    }

    #[test]
    fn test_information_before_sabme_is_discarded() {
        let mut lapm = V42Lapm::new(Vec::new());

        let frames = lapm.feed(&build_raw_frame(0x00, b"premature"));

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].kind, LapmFrameKind::Information);
        assert!(lapm.take_rx_payloads().is_empty());
        assert!(
            lapm.take_tx_bytes().is_empty(),
            "a disconnected station must not acknowledge an information frame"
        );
        assert_eq!(lapm.status(), LapmStatus::Negotiating);
    }

    #[test]
    fn test_disc_terminal_status_absorbs_later_frames_in_batch() {
        let mut lapm = established_lapm();
        let mut batch = build_raw_frame(CONTROL_DISC, &[]);
        batch.extend(build_raw_frame(CONTROL_UA, &[]));
        batch.extend(build_raw_frame(CONTROL_SABME, &[]));
        batch.extend(build_raw_frame(0x00, b"post-disc"));

        let frames = lapm.feed(&batch);

        assert_eq!(frames.len(), 4);
        assert_eq!(
            lapm.status(),
            LapmStatus::RemoteTerminated(LapmControlKind::Disc)
        );
        assert!(lapm.take_rx_payloads().is_empty());

        let mut response_parser = V42Lapm::new(Vec::new());
        let responses = response_parser.feed(&lapm.take_tx_bytes());
        assert_eq!(
            responses.iter().map(|frame| frame.kind).collect::<Vec<_>>(),
            vec![LapmFrameKind::Control(LapmControlKind::Ua)]
        );
    }

    #[test]
    fn test_pending_information_follows_ua_for_remote_sabme() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.queue_i_frame(b"pending");
        assert!(lapm.take_tx_bytes().is_empty());

        let _ = lapm.feed(&build_raw_frame(CONTROL_SABME, &[]));

        let mut peer = V42Lapm::new(Vec::new());
        let frames = peer.feed(&lapm.take_tx_bytes());
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].kind, LapmFrameKind::Control(LapmControlKind::Ua));
        assert_eq!(frames[1].kind, LapmFrameKind::Information);
        assert_eq!(frames[1].payload, b"pending");
    }

    #[test]
    fn test_xid_payload_round_trip_with_escaping() {
        let mut lapm = V42Lapm::new(Vec::new());
        let payload = vec![0x01, HDLC_FLAG, HDLC_ESCAPE, 0x02];
        lapm.queue_xid(&payload);

        let tx = lapm.take_tx_bytes();
        let mut parser = V42Lapm::new(Vec::new());
        let frames = parser.feed(&tx);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].kind, LapmFrameKind::Control(LapmControlKind::Xid));
        assert_eq!(frames[0].payload, payload);
        assert!(frames[0].fcs_ok);
        assert_eq!(parser.status(), LapmStatus::Negotiating);
        let response = parser.take_tx_bytes();
        assert!(!response.is_empty(), "receiver should emit XID response");
    }

    #[test]
    fn test_xid_does_not_break_established_session() {
        let mut sender = V42Lapm::new(Vec::new());
        sender.queue_xid(&[0x01]);
        let tx = sender.take_tx_bytes();

        let mut parser = V42Lapm::new(Vec::new());
        parser.status = LapmStatus::Established;
        let frames = parser.feed(&tx);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].kind, LapmFrameKind::Control(LapmControlKind::Xid));
        assert_eq!(parser.status(), LapmStatus::Established);
        assert!(
            !parser.take_tx_bytes().is_empty(),
            "receiver should acknowledge XID"
        );
    }

    #[test]
    fn test_session_parameters_drive_fragmentation_without_xid_negotiation() {
        let mut sender = established_lapm();
        sender.session_parameters = LapmSessionParameters {
            max_information_bytes: 3,
            window_size: 1,
        };

        sender.queue_i_frame(b"hello");
        let first_tx = sender.take_tx_bytes();
        let mut peer = established_lapm();
        let first_frames = peer.feed(&first_tx);

        assert_eq!(first_frames.len(), 1);
        assert_eq!(first_frames[0].kind, LapmFrameKind::Information);
        assert_eq!(first_frames[0].payload, b"hel");

        let ack_tx = peer.take_tx_bytes();
        let next_frames = sender.feed(&ack_tx);
        assert_eq!(next_frames.len(), 1);
        assert_eq!(
            next_frames[0].kind,
            LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr)
        );

        let second_tx = sender.take_tx_bytes();
        let mut second_peer = established_lapm();
        let second_frames = second_peer.feed(&second_tx);
        assert_eq!(second_frames.len(), 1);
        assert_eq!(second_frames[0].kind, LapmFrameKind::Information);
        assert_eq!(second_frames[0].payload, b"lo");
    }

    #[test]
    fn test_information_frame_over_limit_is_rejected() {
        let mut lapm = established_lapm();
        lapm.session_parameters = LapmSessionParameters {
            max_information_bytes: 3,
            window_size: 1,
        };

        let frames = lapm.feed(&build_raw_frame(0x00, b"toolong"));

        assert!(
            frames.is_empty(),
            "oversized information frame must be discarded"
        );
        assert_eq!(lapm.status(), LapmStatus::SessionRejected);
    }

    #[test]
    fn test_information_frame_at_limit_is_accepted() {
        let mut lapm = established_lapm();
        lapm.session_parameters = LapmSessionParameters {
            max_information_bytes: 3,
            window_size: 1,
        };

        let frames = lapm.feed(&build_raw_frame(0x00, b"abc"));

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].kind, LapmFrameKind::Information);
        assert_eq!(frames[0].payload, b"abc");
        assert!(frames[0].fcs_ok);
        assert_eq!(lapm.status(), LapmStatus::Established);
    }

    #[test]
    fn test_unterminated_frame_over_limit_is_discarded_and_parser_recovers() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.session_parameters = LapmSessionParameters {
            max_information_bytes: 3,
            window_size: 1,
        };
        let max_decoded_frame_len =
            lapm.session_parameters.max_information_bytes + LAPM_FRAME_OVERHEAD_BYTES;

        assert!(lapm.feed(&[HDLC_FLAG]).is_empty());
        let overlong_body = vec![0x11; max_decoded_frame_len + 1];
        assert!(lapm
            .feed(&overlong_body[..max_decoded_frame_len])
            .is_empty());
        assert!(lapm
            .feed(&overlong_body[max_decoded_frame_len..])
            .is_empty());

        assert!(
            lapm.rx_frame.is_empty(),
            "oversized partial frame must release its buffered bytes"
        );
        assert!(
            !lapm.rx_in_frame,
            "parser must discard input until the next HDLC flag"
        );
        assert!(lapm.feed(&[0x22; 32]).is_empty());
        assert!(
            lapm.rx_frame.is_empty(),
            "bytes before the recovery flag must remain discarded"
        );

        let frames = lapm.feed(&build_raw_frame(CONTROL_SABME, &[]));
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].kind,
            LapmFrameKind::Control(LapmControlKind::Sabme)
        );
        assert!(frames[0].fcs_ok);
    }

    #[test]
    fn test_information_frame_round_trip_and_auto_ack() {
        let mut sender = established_lapm();
        let payload = vec![0x10, HDLC_FLAG, 0x20, HDLC_ESCAPE, 0x30];
        sender.queue_i_frame(&payload);
        assert_eq!(sender.status(), LapmStatus::AwaitingAck);

        let tx = sender.take_tx_bytes();
        let mut receiver = established_lapm();
        let frames = receiver.feed(&tx);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].kind, LapmFrameKind::Information);
        assert_eq!(frames[0].payload, payload);
        assert!(frames[0].fcs_ok);
        assert_eq!(receiver.take_rx_payloads(), vec![payload.clone()]);

        let ack_tx = receiver.take_tx_bytes();
        let ack_frames = sender.feed(&ack_tx);
        assert_eq!(ack_frames.len(), 1);
        assert_eq!(
            ack_frames[0].kind,
            LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr)
        );
        assert_eq!(sender.status(), LapmStatus::Established);

        receiver.queue_dm();
        let _ = sender.feed(&receiver.take_tx_bytes());
        assert!(
            sender.take_unacknowledged_payloads().is_empty(),
            "acknowledged payload must never be replayed during fallback"
        );
    }

    #[test]
    fn test_information_frame_with_sequence_bits_is_recognized() {
        let mut lapm = established_lapm();
        let frames = lapm.feed(&build_raw_frame(0x12, &[]));

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].kind, LapmFrameKind::Information);
        assert!(frames[0].fcs_ok);
        assert_eq!(lapm.status(), LapmStatus::Established);
    }

    #[test]
    fn test_supervisory_frame_with_ack_bits_is_recognized() {
        let mut lapm = established_lapm();
        let frames = lapm.feed(&build_raw_frame(0xA1, &[]));

        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].kind,
            LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr)
        );
        assert!(frames[0].fcs_ok);
        assert_eq!(lapm.status(), LapmStatus::Established);
    }

    #[test]
    fn test_remote_sabme_clears_stale_ack_wait_state() {
        let mut lapm = established_lapm();
        lapm.queue_i_frame(b"pending");
        let _ = lapm.take_tx_bytes();
        assert_eq!(lapm.status(), LapmStatus::AwaitingAck);

        let frames = lapm.feed(&build_raw_frame(CONTROL_SABME | HDLC_POLL_FINAL_MASK, &[]));

        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].kind,
            LapmFrameKind::Control(LapmControlKind::Sabme)
        );
        assert_eq!(lapm.status(), LapmStatus::Established);
        assert!(
            !lapm.take_tx_bytes().is_empty(),
            "remote SABME should trigger UA"
        );
        assert!(lapm.can_queue_i_frame());
    }

    #[test]
    fn test_tick_retransmits_unacked_information_then_times_out() {
        let mut lapm = established_lapm();
        lapm.queue_i_frame(b"hello");
        let initial = lapm.take_tx_bytes();
        assert!(!initial.is_empty());

        for _ in 0..ACK_RETRY_TICKS {
            lapm.tick();
        }
        let retransmit = lapm.take_tx_bytes();
        assert_eq!(retransmit, initial);
        assert_eq!(lapm.status(), LapmStatus::AwaitingAck);

        for _ in 0..(ACK_RETRY_TICKS * u32::from(ACK_RETRY_LIMIT + 1)) {
            lapm.tick();
        }

        assert_eq!(lapm.status(), LapmStatus::TimedOut);
    }

    #[test]
    fn test_remote_rnr_polls_without_information_and_resumes_on_rr() {
        let mut lapm = established_lapm();
        lapm.queue_i_frame(b"hello");
        let _ = lapm.take_tx_bytes();

        let rnr = V42Lapm::supervisory_control_byte(LapmSupervisoryKind::Rnr, 0);
        let _ = lapm.feed(&build_raw_frame(rnr, &[]));
        assert!(!lapm.can_queue_i_frame());

        for _ in 0..ACK_RETRY_TICKS {
            lapm.tick();
        }

        let mut parser = V42Lapm::new(Vec::new());
        let polls = parser.feed(&lapm.take_tx_bytes());
        assert_eq!(polls.len(), 1);
        assert_eq!(
            polls[0].kind,
            LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr),
            "a busy peer must be polled instead of receiving retransmitted information"
        );
        assert_ne!(polls[0].control & HDLC_POLL_FINAL_MASK, 0);

        let rr =
            V42Lapm::supervisory_control_byte(LapmSupervisoryKind::Rr, 0) | HDLC_POLL_FINAL_MASK;
        let _ = lapm.feed(&build_raw_frame(rr, &[]));

        let resumed = parser.feed(&lapm.take_tx_bytes());
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].kind, LapmFrameKind::Information);
        assert_eq!(resumed[0].payload, b"hello");
        assert_eq!(lapm.status(), LapmStatus::AwaitingAck);
    }

    #[test]
    fn expected_information_does_not_clear_remote_rnr_busy_state() {
        assert_information_keeps_busy_state(1, LapmSupervisoryKind::Rr, vec![b"data".to_vec()]);
    }

    #[test]
    fn duplicate_information_does_not_clear_remote_rnr_busy_state() {
        assert_information_keeps_busy_state(0, LapmSupervisoryKind::Rr, Vec::new());
    }

    #[test]
    fn out_of_order_information_does_not_clear_remote_rnr_busy_state() {
        assert_information_keeps_busy_state(3, LapmSupervisoryKind::Rej, Vec::new());
    }

    fn assert_supervisory_resumes_pending_payloads(kind: LapmSupervisoryKind, lapm: &mut V42Lapm) {
        let resume = V42Lapm::supervisory_control_byte(kind, 1);
        assert_eq!(
            lapm.feed(&build_raw_frame(resume, &[]))[0].kind,
            LapmFrameKind::Supervisory(kind)
        );

        for (payload, sequence, acknowledgement) in [
            (b"held".as_slice(), 1, None),
            (b"new".as_slice(), 2, Some(2)),
            (b"prob".as_slice(), 3, Some(3)),
            (b"e".as_slice(), 4, Some(4)),
        ] {
            if let Some(acknowledgement) = acknowledgement {
                let ack =
                    V42Lapm::supervisory_control_byte(LapmSupervisoryKind::Rr, acknowledgement);
                assert_eq!(
                    lapm.feed(&build_raw_frame(ack, &[]))[0].kind,
                    LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr)
                );
            }

            let sent = decode_tx_frames(lapm);
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0].kind, LapmFrameKind::Information);
            assert_eq!(sent[0].payload, payload);
            assert_eq!(V42Lapm::information_sequence(sent[0].control), sequence);
        }

        let final_ack = V42Lapm::supervisory_control_byte(LapmSupervisoryKind::Rr, 5);
        assert_eq!(
            lapm.feed(&build_raw_frame(final_ack, &[]))[0].kind,
            LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr)
        );
        assert!(decode_tx_frames(lapm).is_empty());
        assert!(lapm.can_queue_i_frame());
        assert_eq!(lapm.pending_tx_len(), 0);
    }

    #[test]
    fn test_remote_rnr_that_never_recovers_times_out_after_bounded_polls() {
        let mut lapm = established_lapm();
        lapm.queue_i_frame(b"hello");
        let _ = lapm.take_tx_bytes();

        let rnr = V42Lapm::supervisory_control_byte(LapmSupervisoryKind::Rnr, 0);
        let _ = lapm.feed(&build_raw_frame(rnr, &[]));

        let mut parser = V42Lapm::new(Vec::new());
        let mut poll_count = 0;
        for _ in 0..(ACK_RETRY_TICKS * u32::from(ACK_RETRY_LIMIT + 1)) {
            lapm.tick();
            for frame in parser.feed(&lapm.take_tx_bytes()) {
                assert_eq!(
                    frame.kind,
                    LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr)
                );
                assert_ne!(frame.control & HDLC_POLL_FINAL_MASK, 0);
                poll_count += 1;
            }
        }

        assert_eq!(poll_count, usize::from(ACK_RETRY_LIMIT));
        assert_eq!(lapm.status(), LapmStatus::TimedOut);
    }

    /// MDD-BUG-CRUCIBLE-00407: an established call enters `AwaitingAck` for the
    /// duration of every outstanding I-frame. Treating only the exact
    /// `Established` status as "up" made the modem report Training/offline
    /// during ordinary data transfer, contradicting DCD and unmuting the
    /// speaker mid-call.
    #[test]
    fn established_link_stays_live_while_an_i_frame_awaits_ack_crucible407() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.ever_established = true;
        lapm.status = LapmStatus::Established;
        assert!(lapm.is_data_link_live(), "an established link is live");

        // Drive it through the real path rather than poking the status: queue
        // a payload larger than one frame so an ACK is genuinely outstanding.
        lapm.session_parameters = LapmSessionParameters {
            max_information_bytes: 4,
            window_size: 2,
        };
        lapm.queue_i_frame(b"abcdefghijkl");
        assert_eq!(
            lapm.status(),
            LapmStatus::AwaitingAck,
            "queueing an I-frame must enter the ACK wait"
        );
        assert!(
            lapm.is_data_link_live(),
            "an ACK wait on an established link is still data mode"
        );
    }

    /// The other direction: terminal statuses, and a link that never completed
    /// establishment, must both stay offline (MDD-BUG-CRUCIBLE-00407).
    #[test]
    fn non_established_and_terminal_lapm_states_are_not_live_crucible407() {
        for terminal in [
            LapmStatus::SessionRejected,
            LapmStatus::TimedOut,
            LapmStatus::RemoteTerminated(LapmControlKind::Disc),
            LapmStatus::RemoteRejected(LapmControlKind::Dm),
        ] {
            let mut lapm = V42Lapm::new(Vec::new());
            lapm.ever_established = true;
            lapm.status = terminal;
            assert!(
                !lapm.is_data_link_live(),
                "{terminal:?} must not report a live data link"
            );
        }

        // Never established: neither Negotiating nor a stray ACK wait counts.
        for status in [LapmStatus::Negotiating, LapmStatus::AwaitingAck] {
            let mut lapm = V42Lapm::new(Vec::new());
            lapm.ever_established = false;
            lapm.status = status;
            assert!(
                !lapm.is_data_link_live(),
                "{status:?} without establishment must not report a live data link"
            );
        }
    }

    #[test]
    fn test_timeout_recovery_preserves_inflight_then_window_pending_payloads() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.status = LapmStatus::Established;
        lapm.session_parameters = LapmSessionParameters {
            max_information_bytes: 4,
            window_size: 2,
        };
        let payload = b"abcdefghijkl";
        lapm.queue_i_frame(payload);
        let _ = lapm.take_tx_bytes();

        assert_eq!(lapm.tx_inflight_info_frames.len(), 2);
        assert_eq!(lapm.pending_info_payloads.len(), 1);
        for _ in 0..(ACK_RETRY_TICKS * u32::from(ACK_RETRY_LIMIT + 1)) {
            lapm.tick();
        }

        assert_eq!(lapm.status(), LapmStatus::TimedOut);
        assert_eq!(lapm.take_unacknowledged_payloads(), payload);
        assert!(lapm.take_unacknowledged_payloads().is_empty());
    }

    #[test]
    fn test_remote_rejection_preserves_unacknowledged_payload_for_fallback() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.status = LapmStatus::Established;
        lapm.queue_i_frame(b"accepted");
        let _ = lapm.take_tx_bytes();

        let mut peer = V42Lapm::new(Vec::new());
        peer.queue_dm();
        let _ = lapm.feed(&peer.take_tx_bytes());

        assert_eq!(
            lapm.status(),
            LapmStatus::RemoteRejected(LapmControlKind::Dm)
        );
        assert_eq!(lapm.take_unacknowledged_payloads(), b"accepted");
    }

    #[test]
    fn test_information_frames_advance_sequence_with_window_capacity() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.status = LapmStatus::Established;
        lapm.session_parameters = LapmSessionParameters {
            max_information_bytes: 4,
            window_size: 2,
        };

        lapm.queue_i_frame(b"abcdefgh");

        assert_eq!(lapm.tx_inflight_info_frames.len(), 2);
        assert_eq!(lapm.tx_inflight_info_frames[0].sequence, 0);
        assert_eq!(lapm.tx_inflight_info_frames[1].sequence, 1);
        assert_eq!(lapm.tx_sequence, 2);
        assert!(!lapm.can_queue_i_frame());

        let rr = LapmFrame {
            kind: LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr),
            control: LapmFrameKind::Supervisory(LapmSupervisoryKind::Rr).control_byte() | (2 << 5),
            payload: Vec::new(),
            fcs_ok: true,
            is_response: false,
        };
        lapm.handle_supervisory_frame(LapmSupervisoryKind::Rr, &rr);

        assert!(lapm.tx_inflight_info_frames.is_empty());
        assert!(lapm.can_queue_i_frame());

        lapm.queue_i_frame(b"wxyz");
        assert_eq!(lapm.tx_inflight_info_frames.len(), 1);
        assert_eq!(lapm.tx_inflight_info_frames[0].sequence, 2);
    }

    /// MDD-BUG-CRUCIBLE-00518: an XID exchange between two peers must reach a
    /// quiescent state.
    ///
    /// Every received XID used to queue another XID that itself awaited an
    /// acknowledgement, and frames carried no command/response distinction — so
    /// A answered B, B answered A's answer, and it never stopped. The existing
    /// tests only checked the first response and never fed it back, which is
    /// precisely why the loop survived.
    ///
    /// This is the two-helper oracle the entry asked for: pump bytes between
    /// two peers until nobody has anything left to say, with a hard cap so a
    /// regression FAILS rather than hanging the suite.
    #[test]
    fn xid_exchange_between_two_peers_reaches_quiescence() {
        let mut a = V42Lapm::new(Vec::new());
        let mut b = V42Lapm::new(Vec::new());

        a.queue_xid(&[]);

        let mut a_commands = 0usize;
        let mut a_responses = 0usize;
        let mut b_commands = 0usize;
        let mut b_responses = 0usize;

        let mut in_flight = a.take_tx_bytes();
        assert!(!in_flight.is_empty(), "queue_xid produced nothing to send");

        let mut rounds = 0usize;
        // A settled exchange needs one hop each way. The cap is generous but
        // finite: the pre-fix behaviour runs until it hits it.
        while !in_flight.is_empty() && rounds < 20 {
            rounds += 1;
            // Odd rounds carry A->B, even rounds carry B->A.
            let (receiver, cmds, resps) = if rounds % 2 == 1 {
                (&mut b, &mut b_commands, &mut b_responses)
            } else {
                (&mut a, &mut a_commands, &mut a_responses)
            };
            for frame in receiver.feed(&in_flight) {
                if frame.kind == LapmFrameKind::Control(LapmControlKind::Xid) {
                    if frame.is_response {
                        *resps += 1;
                    } else {
                        *cmds += 1;
                    }
                }
            }
            in_flight = receiver.take_tx_bytes();
        }

        assert!(
            in_flight.is_empty(),
            "the XID exchange never went quiet: still {} bytes pending after {rounds} rounds",
            in_flight.len()
        );
        // Exactly one command and one response, in the right directions.
        assert_eq!(b_commands, 1, "B should have seen exactly one XID command");
        assert_eq!(b_responses, 0, "B should not have seen an XID response");
        assert_eq!(
            a_responses, 1,
            "A should have seen exactly one XID response"
        );
        assert_eq!(a_commands, 0, "A should not have been sent an XID command");
    }

    /// A response must also CLEAR the initiator's wait, not merely fail to
    /// provoke another frame — otherwise the retransmit timer keeps firing.
    #[test]
    fn xid_response_completes_the_initiators_ack_wait() {
        let mut a = V42Lapm::new(Vec::new());
        let mut b = V42Lapm::new(Vec::new());

        a.queue_xid(&[]);
        let command = a.take_tx_bytes();
        b.feed(&command);
        let response = b.take_tx_bytes();
        assert!(!response.is_empty(), "B did not answer the XID command");

        a.feed(&response);
        assert!(
            !a.awaiting_ack,
            "the XID response left the initiator still waiting for an ack"
        );
        assert!(
            a.take_tx_bytes().is_empty(),
            "the initiator answered a response, which is the loop this guards"
        );
    }

    #[test]
    fn test_information_receive_sequence_advances_expected_number() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.status = LapmStatus::Established;
        lapm.session_parameters = LapmSessionParameters {
            max_information_bytes: 8,
            window_size: 1,
        };

        let first = LapmFrame {
            kind: LapmFrameKind::Information,
            control: LapmFrameKind::Information.control_byte(),
            payload: b"abc".to_vec(),
            fcs_ok: true,
            is_response: false,
        };
        lapm.update_status(&first);
        assert_eq!(lapm.rx_sequence, 1);
        assert_eq!(lapm.take_rx_payloads(), vec![b"abc".to_vec()]);

        let second = LapmFrame {
            kind: LapmFrameKind::Information,
            control: LapmFrameKind::Information.control_byte() | (1 << 1) | (1 << 5),
            payload: b"def".to_vec(),
            fcs_ok: true,
            is_response: false,
        };
        lapm.update_status(&second);
        assert_eq!(lapm.rx_sequence, 2);
        assert_eq!(lapm.take_rx_payloads(), vec![b"def".to_vec()]);
    }

    #[test]
    fn test_status_transitions_on_remote_controls() {
        let mut lapm = V42Lapm::new(Vec::new());

        let mut ua = V42Lapm::new(Vec::new());
        ua.queue_ua();
        let ua_tx = ua.take_tx_bytes();
        let frames = lapm.feed(&ua_tx);
        assert_eq!(frames[0].kind, LapmFrameKind::Control(LapmControlKind::Ua));
        assert_eq!(lapm.status(), LapmStatus::Established);

        let mut dm = V42Lapm::new(Vec::new());
        dm.queue_dm();
        let dm_tx = dm.take_tx_bytes();
        let frames = lapm.feed(&dm_tx);
        assert_eq!(frames[0].kind, LapmFrameKind::Control(LapmControlKind::Dm));
        assert_eq!(
            lapm.status(),
            LapmStatus::RemoteRejected(LapmControlKind::Dm)
        );

        let mut disc = V42Lapm::new(Vec::new());
        disc.queue_disc();
        let disc_tx = disc.take_tx_bytes();
        let mut terminated_lapm = established_lapm();
        let frames = terminated_lapm.feed(&disc_tx);
        assert_eq!(
            frames[0].kind,
            LapmFrameKind::Control(LapmControlKind::Disc)
        );
        assert_eq!(
            terminated_lapm.status(),
            LapmStatus::RemoteTerminated(LapmControlKind::Disc)
        );

        let mut response_parser = V42Lapm::new(Vec::new());
        let response_frames = response_parser.feed(&terminated_lapm.take_tx_bytes());
        assert_eq!(response_frames.len(), 1);
        assert_eq!(
            response_frames[0].kind,
            LapmFrameKind::Control(LapmControlKind::Ua)
        );
        assert_eq!(
            terminated_lapm.status(),
            LapmStatus::RemoteTerminated(LapmControlKind::Disc)
        );
    }

    #[test]
    fn xid_unrelated_response_preserves_sabme_retry_deadline() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.queue_sabme();
        let original = lapm.take_tx_bytes();
        tick_times(&mut lapm, ACK_RETRY_TICKS - 1);
        assert_valid_xid_response(&mut lapm);
        lapm.tick();
        assert_eq!(
            lapm.take_tx_bytes(),
            original,
            "unrelated XID canceled SABME retry"
        );
        tick_times(&mut lapm, ACK_RETRY_TICKS);
        assert_eq!(lapm.take_tx_bytes(), original);
        tick_times(&mut lapm, ACK_RETRY_TICKS - 1);
        assert_eq!(lapm.status(), LapmStatus::Negotiating);
        lapm.tick();
        assert_eq!(lapm.status(), LapmStatus::TimedOut);
    }

    #[test]
    fn xid_unrelated_response_preserves_information_retry_budget() {
        let mut lapm = established_lapm();
        lapm.queue_i_frame(b"synthetic payload");
        let original = lapm.take_tx_bytes();
        tick_times(&mut lapm, ACK_RETRY_TICKS - 1);
        assert_valid_xid_response(&mut lapm);
        lapm.tick();
        assert_eq!(
            lapm.take_tx_bytes(),
            original,
            "unrelated XID changed payload retry deadline"
        );
        tick_times(&mut lapm, ACK_RETRY_TICKS);
        assert_eq!(
            lapm.take_tx_bytes(),
            original,
            "unrelated XID consumed payload retry budget"
        );
        tick_times(&mut lapm, ACK_RETRY_TICKS - 1);
        assert_eq!(lapm.status(), LapmStatus::AwaitingAck);
        lapm.tick();
        assert_eq!(lapm.status(), LapmStatus::TimedOut);
    }

    #[test]
    fn xid_matching_response_stops_control_retry_timer() {
        let mut lapm = V42Lapm::new(Vec::new());
        lapm.queue_xid(b"synthetic parameters");
        let original = lapm.take_tx_bytes();
        tick_times(&mut lapm, ACK_RETRY_TICKS);
        assert_eq!(lapm.take_tx_bytes(), original);
        assert_valid_xid_response(&mut lapm);
        tick_times(&mut lapm, ACK_RETRY_TICKS);
        assert!(
            lapm.take_tx_bytes().is_empty(),
            "completed XID was retransmitted at its retry deadline"
        );
        assert_eq!(lapm.status(), LapmStatus::Negotiating);
        assert!(
            lapm.xid_ack_wait.is_none(),
            "matching response completed XID only"
        );
        tick_times(&mut lapm, ACK_RETRY_TICKS * u32::from(ACK_RETRY_LIMIT + 2));
        assert!(
            lapm.take_tx_bytes().is_empty(),
            "completed XID was retransmitted"
        );
        assert_eq!(
            lapm.status(),
            LapmStatus::TimedOut,
            "completed XID does not complete link establishment"
        );
    }

    #[test]
    fn xid_matching_response_preserves_overlapping_information_timer() {
        let mut lapm = established_lapm();
        lapm.queue_i_frame(b"synthetic overlap");
        let original = lapm.take_tx_bytes();
        tick_times(&mut lapm, ACK_RETRY_TICKS);
        assert_eq!(lapm.take_tx_bytes(), original);
        tick_times(&mut lapm, ACK_RETRY_TICKS - 1);
        lapm.queue_xid(b"synthetic parameters");
        assert!(!lapm.take_tx_bytes().is_empty());
        assert_valid_xid_response(&mut lapm);
        lapm.tick();
        assert_eq!(
            lapm.take_tx_bytes(),
            original,
            "matching XID changed payload retry deadline"
        );
        tick_times(&mut lapm, ACK_RETRY_TICKS - 1);
        assert_eq!(lapm.status(), LapmStatus::AwaitingAck);
        lapm.tick();
        assert_eq!(
            lapm.status(),
            LapmStatus::TimedOut,
            "matching XID replenished payload retry budget"
        );
    }

    #[test]
    fn xid_wait_survives_information_acknowledgement() {
        let mut lapm = established_lapm();
        lapm.queue_i_frame(b"synthetic acknowledged");
        let _ = lapm.take_tx_bytes();
        lapm.queue_xid(b"synthetic parameters");
        let xid = lapm.take_tx_bytes();
        tick_times(&mut lapm, ACK_RETRY_TICKS - 1);
        let frames = lapm.feed(&build_raw_frame(CONTROL_RR | (1 << 5), &[]));
        assert_eq!(frames.len(), 1);
        assert!(frames[0].fcs_ok);
        lapm.tick();
        assert_eq!(
            lapm.take_tx_bytes(),
            xid,
            "payload ACK canceled independent XID wait"
        );
        assert_valid_xid_response(&mut lapm);
        tick_times(&mut lapm, ACK_RETRY_TICKS * u32::from(ACK_RETRY_LIMIT + 2));
        assert!(lapm.take_tx_bytes().is_empty());
        assert_eq!(lapm.status(), LapmStatus::Established);
    }

    fn tick_times(lapm: &mut V42Lapm, ticks: u32) {
        for _ in 0..ticks {
            lapm.tick();
        }
    }

    fn assert_valid_xid_response(lapm: &mut V42Lapm) {
        let response = build_raw_frame_with_address(HDLC_ADDRESS_RESPONSE, CONTROL_XID, &[]);
        let frames = lapm.feed(&response);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].fcs_ok);
        assert!(frames[0].is_response);
        assert_eq!(frames[0].kind, LapmFrameKind::Control(LapmControlKind::Xid));
        assert!(
            lapm.take_tx_bytes().is_empty(),
            "a response provoked XID ping-pong"
        );
    }

    fn build_raw_frame(control: u8, payload: &[u8]) -> Vec<u8> {
        build_raw_frame_with_address(HDLC_ADDRESS, control, payload)
    }

    fn build_raw_frame_with_address(address: u8, control: u8, payload: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(2 + payload.len());
        body.push(address);
        body.push(control);
        body.extend_from_slice(payload);
        let fcs = crc_ccitt(&body).to_le_bytes();

        let mut frame = Vec::with_capacity(body.len() + 6);
        frame.push(HDLC_FLAG);
        push_raw_bytes(&mut frame, &body);
        push_raw_bytes(&mut frame, &fcs);
        frame.push(HDLC_FLAG);
        frame
    }

    fn push_raw_bytes(frame: &mut Vec<u8>, bytes: &[u8]) {
        for &byte in bytes {
            if matches!(byte, HDLC_FLAG | HDLC_ESCAPE) {
                frame.push(HDLC_ESCAPE);
                frame.push(byte ^ HDLC_ESCAPE_XOR);
            } else {
                frame.push(byte);
            }
        }
    }
}
