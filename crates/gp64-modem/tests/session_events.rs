//! Acceptance session-event integration tests.
//!
//! Runs a loopback modem scenario using the same direct-wire pattern as
//! `pstn_impairment.rs`, captures tracing events, and verifies that the
//! `modem_session_summary` and `modem_session_end` events fire with the
//! expected fields.
//!
//! These tests validate the dry-run path of the modem acceptance tooling
//! without requiring real hardware.
//!
//! Run with:
//!   cargo test -p gp64-modem --test session_events -- --nocapture

use std::sync::{Arc, Mutex};

use gp64_modem::wire::CHUNK_SAMPLES;
use gp64_modem::{ModemConfig, ModemDsp};

// ---------------------------------------------------------------------------
// Tracing capture infrastructure
// ---------------------------------------------------------------------------

/// Writer that appends all tracing output to a shared buffer.
#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Install a tracing subscriber that captures all output for the
/// `gp64::hw::serial` target (where session events are emitted).
/// Returns (guard, captured_output_handle).
fn capture_tracing() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<u8>>>) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = CaptureWriter(buffer.clone());

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("gp64::hw::serial=info")
        .with_writer(writer)
        .with_ansi(false)
        .finish();

    let guard = tracing::subscriber::set_default(subscriber);
    (guard, buffer)
}

fn captured_output(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buffer.lock().unwrap()).to_string()
}

// ---------------------------------------------------------------------------
// Direct-wire loopback harness (no TCP, no impairments)
// ---------------------------------------------------------------------------

/// Drain all bytes from the modem's serial RX buffer.
fn drain_serial(modem: &mut ModemDsp) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(b) = modem.send_to_serial() {
        out.push(b);
    }
    out
}

/// Direct-wire loopback: caller's output goes directly to answerer's input
/// and vice versa, with no channel impairments. This is the simplest
/// possible loopback that exercises the full modem stack.
struct DirectWireHarness {
    caller: ModemDsp,
    answerer: ModemDsp,
    prev_caller_out: [i16; CHUNK_SAMPLES],
    prev_answerer_out: [i16; CHUNK_SAMPLES],
    caller_out: [i16; CHUNK_SAMPLES],
    answerer_out: [i16; CHUNK_SAMPLES],
}

impl DirectWireHarness {
    fn new(speed: u32) -> Self {
        Self {
            caller: ModemDsp::new(ModemConfig {
                max_speed: speed,
                ..Default::default()
            }),
            answerer: ModemDsp::new(ModemConfig {
                max_speed: speed,
                ..Default::default()
            }),
            prev_caller_out: [0i16; CHUNK_SAMPLES],
            prev_answerer_out: [0i16; CHUNK_SAMPLES],
            caller_out: [0i16; CHUNK_SAMPLES],
            answerer_out: [0i16; CHUNK_SAMPLES],
        }
    }

    /// Initiate a call and run DSP ticks until both modems are online.
    /// Returns false if handshake fails within the timeout.
    fn handshake(&mut self, timeout_ticks: usize) -> bool {
        // Set answerer to auto-answer
        for &b in b"ATS0=1\r" {
            self.answerer.receive_from_serial(b);
        }
        drain_serial(&mut self.answerer);

        // Caller dials
        for &b in b"ATD5551234\r" {
            self.caller.receive_from_serial(b);
        }
        drain_serial(&mut self.caller);

        for _ in 0..timeout_ticks {
            self.tick();
            drain_serial(&mut self.caller);
            drain_serial(&mut self.answerer);

            if self.caller.is_online() && self.answerer.is_online() {
                // Post-handshake convergence
                for _ in 0..60 {
                    self.tick();
                    drain_serial(&mut self.caller);
                    drain_serial(&mut self.answerer);
                }
                return true;
            }
        }
        false
    }

    fn tick(&mut self) {
        // Direct wire: caller hears answerer's previous output and vice versa
        self.caller
            .tick_dsp(&self.prev_answerer_out, &mut self.caller_out);
        self.answerer
            .tick_dsp(&self.prev_caller_out, &mut self.answerer_out);

        self.prev_caller_out = self.caller_out;
        self.prev_answerer_out = self.answerer_out;
    }

    /// Transfer data caller->answerer. Returns received bytes.
    fn transfer_c2a(&mut self, payload: &[u8]) -> Vec<u8> {
        let mut received = Vec::new();
        let mut fed = 0;
        let max_ticks = payload.len() * 24 + 1000;

        for _ in 0..max_ticks {
            drain_serial(&mut self.caller);
            drain_serial(&mut self.answerer);

            let end = (fed + 16).min(payload.len());
            for &b in &payload[fed..end] {
                self.caller.receive_from_serial(b);
            }
            fed = end;

            self.tick();
            received.extend(drain_serial(&mut self.answerer));

            if received.len() >= payload.len() {
                break;
            }
        }
        received
    }

    /// Transfer data answerer->caller. Returns received bytes.
    fn transfer_a2c(&mut self, payload: &[u8]) -> Vec<u8> {
        let mut received = Vec::new();
        let mut fed = 0;
        let max_ticks = payload.len() * 24 + 1000;

        for _ in 0..max_ticks {
            drain_serial(&mut self.caller);
            drain_serial(&mut self.answerer);

            let end = (fed + 16).min(payload.len());
            for &b in &payload[fed..end] {
                self.answerer.receive_from_serial(b);
            }
            fed = end;

            self.tick();
            received.extend(drain_serial(&mut self.caller));

            if received.len() >= payload.len() {
                break;
            }
        }
        received
    }

    /// Hang up the caller modem (send ATH in command mode via escape sequence).
    /// This triggers the session-end event.
    fn hang_up_caller(&mut self) {
        // First, go silent for the guard time (S12=50 = 1 second = 100 ticks)
        for _ in 0..120 {
            self.tick();
            drain_serial(&mut self.caller);
            drain_serial(&mut self.answerer);
        }

        // Send +++ escape sequence
        for &b in b"+++" {
            self.caller.receive_from_serial(b);
        }

        // Wait for post-guard time
        for _ in 0..120 {
            self.tick();
            drain_serial(&mut self.caller);
            drain_serial(&mut self.answerer);
        }

        // Send ATH to hang up
        for &b in b"ATH\r" {
            self.caller.receive_from_serial(b);
        }
        drain_serial(&mut self.caller);

        // Tick a few more times for the answerer to detect carrier loss
        for _ in 0..200 {
            self.tick();
            drain_serial(&mut self.caller);
            drain_serial(&mut self.answerer);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Full loopback scenario at V.32bis 9600 baud: handshake, data exchange,
/// and hangup. Verifies that session-end events fire with the expected fields
/// regardless of whether V.42 negotiation resulted in LAPM or transparent mode.
///
/// The modem_session_end event always fires when a session terminates.
/// The modem_session_summary event only fires when LAPM is established.
#[test]
fn session_end_fires_on_v32bis_loopback() {
    let (_guard, buffer) = capture_tracing();

    let mut harness = DirectWireHarness::new(9600);

    assert!(
        harness.handshake(800),
        "V.32bis 9600 handshake failed (caller={:?}, answerer={:?})",
        harness.caller.phase(),
        harness.answerer.phase()
    );

    // Transfer some data in both directions.
    let payload = b"Hello from caller to answerer!";
    let received = harness.transfer_c2a(payload);
    assert!(
        received.len() >= payload.len(),
        "C->A transfer lost data: got {} of {} bytes",
        received.len(),
        payload.len()
    );

    let payload2 = b"Hello from answerer to caller!";
    let received2 = harness.transfer_a2c(payload2);
    assert!(
        received2.len() >= payload2.len(),
        "A->C transfer lost data: got {} of {} bytes",
        received2.len(),
        payload2.len()
    );

    // Hang up to trigger session-end events.
    harness.hang_up_caller();

    let output = captured_output(&buffer);

    // Session-end must fire for at least the caller.
    assert!(
        output.contains("modem_session_end"),
        "session-end event should fire after hangup.\n\
         Captured output:\n{output}"
    );

    // Session-end should report role, family, and V42 outcome.
    let end_lines: Vec<&str> = output
        .lines()
        .filter(|l| l.contains("modem_session_end"))
        .collect();

    assert!(
        !end_lines.is_empty(),
        "should have at least one session-end line"
    );

    // Check that session fields are present in at least one end event.
    let all_end_text = end_lines.join("\n");

    assert!(
        all_end_text.contains("role=Caller") || all_end_text.contains("role=Answerer"),
        "session-end should include role.\nEnd lines:\n{all_end_text}"
    );
    assert!(
        all_end_text.contains("V32bis"),
        "session-end should report V32bis family.\nEnd lines:\n{all_end_text}"
    );
    // V42 outcome should be present (either Lapm or Transparent).
    assert!(
        all_end_text.contains("Lapm") || all_end_text.contains("Transparent"),
        "session-end should report V42 outcome.\nEnd lines:\n{all_end_text}"
    );

    // If LAPM was established, the session-summary should also have fired.
    if all_end_text.contains("lapm_established=true") {
        assert!(
            output.contains("modem_session_summary"),
            "when LAPM is established, session-summary should fire.\n\
             Captured output:\n{output}"
        );

        // Verify the summary has the right fields.
        let summary_lines: Vec<&str> = output
            .lines()
            .filter(|l| l.contains("modem_session_summary"))
            .collect();
        let summary_text = summary_lines.join("\n");

        assert!(
            summary_text.contains("lapm_established=true"),
            "session-summary should report lapm_established=true.\n\
             Summary lines:\n{summary_text}"
        );
    }
}

/// Verify that session-end events include payload byte counts.
/// After data exchange, at least one session-end should report
/// tx_payload_bytes > 0 and rx_payload_bytes > 0.
#[test]
fn session_end_includes_payload_byte_counts() {
    let (_guard, buffer) = capture_tracing();

    let mut harness = DirectWireHarness::new(9600);

    assert!(harness.handshake(800), "V.32bis 9600 handshake failed");

    // Exchange a meaningful amount of data.
    let payload = vec![0xAAu8; 256];
    let _ = harness.transfer_c2a(&payload);
    let _ = harness.transfer_a2c(&payload);

    // Hang up.
    harness.hang_up_caller();

    let output = captured_output(&buffer);

    // Session-end events should show non-zero payload bytes.
    // The format is: tx_payload_bytes=N rx_payload_bytes=N
    let has_nonzero_tx = output
        .lines()
        .filter(|line| line.contains("modem_session_end"))
        .any(|line| {
            line.split_whitespace()
                .filter_map(|token| token.strip_prefix("tx_payload_bytes="))
                .any(|val| val.parse::<u64>().unwrap_or(0) > 0)
        });

    let has_nonzero_rx = output
        .lines()
        .filter(|line| line.contains("modem_session_end"))
        .any(|line| {
            line.split_whitespace()
                .filter_map(|token| token.strip_prefix("rx_payload_bytes="))
                .any(|val| val.parse::<u64>().unwrap_or(0) > 0)
        });

    assert!(
        has_nonzero_tx,
        "at least one session-end should report tx_payload_bytes > 0.\n\
         Captured output:\n{output}"
    );
    assert!(
        has_nonzero_rx,
        "at least one session-end should report rx_payload_bytes > 0.\n\
         Captured output:\n{output}"
    );
}

/// Verify that an early disconnect (carrier loss without data exchange)
/// produces a session-end event with a disconnect reason.
#[test]
fn session_end_on_hangup_before_data() {
    let (_guard, buffer) = capture_tracing();

    let mut harness = DirectWireHarness::new(9600);

    assert!(harness.handshake(800), "V.32bis 9600 handshake failed");

    // Immediately hang up without transferring any payload data.
    harness.hang_up_caller();

    let output = captured_output(&buffer);

    assert!(
        output.contains("modem_session_end"),
        "session-end should fire even without data exchange.\n\
         Captured output:\n{output}"
    );

    // The caller's session-end should show a disconnect reason.
    let has_hangup_reason = output
        .lines()
        .filter(|line| line.contains("modem_session_end"))
        .any(|line| line.contains("ATH") || line.contains("hangup") || line.contains("carrier"));

    assert!(
        has_hangup_reason,
        "session-end should include a disconnect reason for ATH hangup.\n\
         Captured output:\n{output}"
    );
}

/// Verify that the terminal_stage field appears in session-end events.
/// After a successful connection and hangup, terminal stage should be Online.
#[test]
fn session_end_reports_terminal_stage() {
    let (_guard, buffer) = capture_tracing();

    let mut harness = DirectWireHarness::new(9600);

    assert!(harness.handshake(800), "V.32bis 9600 handshake failed");

    harness.hang_up_caller();

    let output = captured_output(&buffer);

    let end_lines: Vec<&str> = output
        .lines()
        .filter(|l| l.contains("modem_session_end"))
        .collect();

    assert!(
        !end_lines.is_empty(),
        "should have session-end events.\nCaptured output:\n{output}"
    );

    // At least the caller's terminal stage should be Online (it was
    // connected and then hung up).
    let has_online_terminal = end_lines
        .iter()
        .any(|l| l.contains("terminal_stage=Online"));

    assert!(
        has_online_terminal,
        "caller session-end should have terminal_stage=Online.\nEnd lines:\n{}",
        end_lines.join("\n")
    );
}
