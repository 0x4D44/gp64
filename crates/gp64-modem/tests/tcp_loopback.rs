//! TCP loopback integration test.
//!
//! Connects two ModemDsp instances via real loopback TCP sockets,
//! exercising the wire framing, chunk reassembly, and non-blocking I/O.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use gp64_modem::wire::{self, WireBuffer, CHUNK_BYTES, CHUNK_SAMPLES};
use gp64_modem::{ModemConfig, ModemDsp, ModemPhase};

/// Create a pair of connected TCP streams over the loopback interface.
/// Both ends are set to non-blocking with TCP_NODELAY.
fn loopback_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = TcpStream::connect(addr).unwrap();
    let (server, _) = listener.accept().unwrap();
    client.set_nonblocking(true).unwrap();
    server.set_nonblocking(true).unwrap();
    client.set_nodelay(true).unwrap();
    server.set_nodelay(true).unwrap();
    (client, server)
}

// ---------------------------------------------------------------------------
// Wire framing tests
// ---------------------------------------------------------------------------

/// Send a single PCM chunk over TCP, receive and decode it on the other end.
/// Verifies that encode/decode survives a real TCP round-trip.
#[test]
fn test_tcp_wire_framing_roundtrip() {
    let (mut sender, mut receiver) = loopback_pair();

    // Switch receiver to blocking with a timeout so we can do a simple read.
    receiver.set_nonblocking(false).unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();

    // Build a distinctive sample pattern.
    let mut samples = [0i16; CHUNK_SAMPLES];
    for (i, sample) in samples.iter_mut().enumerate() {
        *sample = (i as i16) * 100;
    }

    let encoded = wire::encode_chunk(&samples);
    sender.write_all(&encoded).unwrap();

    // Small delay for loopback delivery.
    std::thread::sleep(Duration::from_millis(50));

    let mut buf = [0u8; 4096];
    let n = receiver.read(&mut buf).unwrap();

    let mut wire_buf = WireBuffer::new();
    wire_buf.push(&buf[..n]);
    let decoded = wire_buf.take_chunk().expect("should get one chunk");

    assert_eq!(decoded, samples);
}

/// Send multiple chunks in one TCP write (simulating a burst), then verify
/// that WireBuffer correctly reassembles all of them on the receive side.
#[test]
fn test_tcp_wire_multi_chunk_reassembly() {
    let (mut sender, mut receiver) = loopback_pair();

    receiver.set_nonblocking(false).unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();

    // Build 3 distinct chunks.
    let mut chunks = Vec::new();
    let mut burst = Vec::new();
    for chunk_idx in 0..3u16 {
        let mut samples = [0i16; CHUNK_SAMPLES];
        for (i, sample) in samples.iter_mut().enumerate() {
            *sample = (chunk_idx as i16 + 1) * (i as i16);
        }
        let encoded = wire::encode_chunk(&samples);
        burst.extend_from_slice(&encoded);
        chunks.push(samples);
    }

    // Write all 3 chunks in a single TCP write.
    sender.write_all(&burst).unwrap();

    std::thread::sleep(Duration::from_millis(50));

    // Read everything available — TCP may deliver it in one read or several.
    let mut wire_buf = WireBuffer::new();
    let mut buf = [0u8; 4096];
    loop {
        match receiver.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => wire_buf.push(&buf[..n]),
            Err(_) => break,
        }
        if wire_buf.buffered_bytes() >= CHUNK_BYTES * 3 {
            break;
        }
    }

    assert_eq!(wire_buf.chunks_available(), 3);
    for expected in &chunks {
        let decoded = wire_buf.take_chunk().expect("should have a chunk");
        assert_eq!(&decoded, expected);
    }
}

/// Verify that partial TCP reads are correctly reassembled.
/// Sends a chunk in two halves with a delay between them.
#[test]
fn test_tcp_wire_partial_read_reassembly() {
    let (mut sender, mut receiver) = loopback_pair();

    receiver.set_nonblocking(false).unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();

    let mut samples = [0i16; CHUNK_SAMPLES];
    for (i, sample) in samples.iter_mut().enumerate() {
        *sample = -(i as i16) * 50;
    }

    let encoded = wire::encode_chunk(&samples);
    let half = encoded.len() / 2;

    // Write the first half.
    sender.write_all(&encoded[..half]).unwrap();
    std::thread::sleep(Duration::from_millis(30));

    // Read what's available — should be partial.
    let mut wire_buf = WireBuffer::new();
    let mut buf = [0u8; 4096];
    if let Ok(n) = receiver.read(&mut buf) {
        wire_buf.push(&buf[..n]);
    }
    assert!(
        wire_buf.take_chunk().is_none(),
        "Should not have a complete chunk from half the data"
    );

    // Write the second half.
    sender.write_all(&encoded[half..]).unwrap();
    std::thread::sleep(Duration::from_millis(30));

    if let Ok(n) = receiver.read(&mut buf) {
        wire_buf.push(&buf[..n]);
    }
    let decoded = wire_buf
        .take_chunk()
        .expect("should reassemble after second half");
    assert_eq!(decoded, samples);
}

// ---------------------------------------------------------------------------
// Full modem connection test
// ---------------------------------------------------------------------------

/// Full modem connection test over TCP loopback.
///
/// Creates two ModemDsp instances (caller and answerer), connects them via a
/// single bidirectional TCP stream, and runs the DSP loop until both reach
/// the Online phase. This exercises the entire negotiation pipeline
/// (CNG/ANS tones, V.8 negotiation, training) over real TCP I/O.
///
/// Note: Full data round-trip through the DSP pipeline (scramble -> trellis
/// -> QAM -> channel -> demod -> decode -> descramble) requires more
/// convergence time than practical for a unit test. Both modems reaching
/// Online is the primary success criterion.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "host TCP-loopback timing on macOS is too variable for the real-time handshake tick budget — bursty non-blocking-read delivery intermittently starves the modem negotiation state machine (flaky, ~1 in 3). The handshake logic is covered deterministically by the in-memory crossover in stress.rs; this test validates the real-TCP transport integration, which holds on Linux/Windows."
)]
fn test_tcp_loopback_full_connection() {
    let (mut caller_tcp, mut answerer_tcp) = loopback_pair();

    let mut caller = ModemDsp::new(ModemConfig {
        max_speed: 9600,
        ..Default::default()
    });
    let mut answerer = ModemDsp::new(ModemConfig {
        max_speed: 9600,
        ..Default::default()
    });

    let mut caller_rx_buf = WireBuffer::new();
    let mut answerer_rx_buf = WireBuffer::new();

    // Issue AT commands: caller dials, answerer auto-answers on ring 1.
    for &b in b"ATD5551234\r" {
        caller.receive_from_serial(b);
    }
    for &b in b"ATS0=1\r" {
        answerer.receive_from_serial(b);
    }

    // Drain any initial result codes before entering the DSP loop.
    drain_serial(&mut caller);
    drain_serial(&mut answerer);

    let mut caller_out = [0i16; CHUNK_SAMPLES];
    let mut answerer_out = [0i16; CHUNK_SAMPLES];

    let timeout = 900; // 900 ticks * 10 ms = 9 seconds (ANSam is 3.3s)
    let mut connected = false;
    let mut connect_tick = 0;

    // Track phase transitions for diagnostic purposes.
    let mut caller_phases = vec![caller.phase()];
    let mut answerer_phases = vec![answerer.phase()];

    for tick in 0..timeout {
        // --- Serial drain (simulating 10 x 1 ms cadence) ---
        for _ in 0..10 {
            drain_serial(&mut caller);
            drain_serial(&mut answerer);
        }

        // --- Read incoming TCP into wire buffers ---
        read_tcp_into(&mut caller_tcp, &mut caller_rx_buf);
        read_tcp_into(&mut answerer_tcp, &mut answerer_rx_buf);

        // --- Build line-in: either a real chunk or silence ---
        let caller_line_in = caller_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
        let answerer_line_in = answerer_rx_buf.take_chunk().unwrap_or(wire::SILENCE);

        // --- DSP tick (10 ms) ---
        caller.tick_dsp(&caller_line_in, &mut caller_out);
        answerer.tick_dsp(&answerer_line_in, &mut answerer_out);

        // --- Write output chunks to TCP ---
        let caller_encoded = wire::encode_chunk(&caller_out);
        let answerer_encoded = wire::encode_chunk(&answerer_out);
        let _ = caller_tcp.write_all(&caller_encoded);
        let _ = answerer_tcp.write_all(&answerer_encoded);

        // Record phase transitions.
        let cp = caller.phase();
        let ap = answerer.phase();
        if cp != *caller_phases.last().unwrap() {
            caller_phases.push(cp);
        }
        if ap != *answerer_phases.last().unwrap() {
            answerer_phases.push(ap);
        }

        if caller.is_online() && answerer.is_online() {
            connected = true;
            connect_tick = tick;
            break;
        }
    }

    assert!(
        connected,
        "Modems should connect over TCP loopback within 9 seconds.\n\
         Caller phases: {:?}\n\
         Answerer phases: {:?}",
        caller_phases, answerer_phases
    );

    // Verify both modems went through the expected negotiation phases.
    assert!(
        caller_phases.contains(&ModemPhase::CallSetup),
        "Caller should have entered CallSetup"
    );
    assert!(
        caller_phases.contains(&ModemPhase::V8Negotiation),
        "Caller should have entered V8Negotiation"
    );
    assert!(
        caller_phases.contains(&ModemPhase::Training),
        "Caller should have entered Training"
    );
    assert!(
        caller_phases.contains(&ModemPhase::Online),
        "Caller should have reached Online"
    );

    assert!(
        answerer_phases.contains(&ModemPhase::CallSetup),
        "S0=1 answerer should enter CallSetup on the first ring"
    );
    assert!(
        !answerer_phases.contains(&ModemPhase::Ringing),
        "S0=1 answerer should answer within the first ring-detection tick"
    );
    assert!(
        answerer_phases.contains(&ModemPhase::Online),
        "Answerer should have reached Online"
    );

    // Sanity check: connection happened within a reasonable time.
    // Full ANSam is 3.3s, plus V.8 and V.32bis training.
    assert!(
        connect_tick < 800,
        "Connection took {} ticks ({:.1}s) — expected < 8 seconds",
        connect_tick,
        connect_tick as f64 * 0.01
    );
}

// ---------------------------------------------------------------------------
// V.8 negotiation at 4800 bps with serial_net.rs-style tick cadence
// ---------------------------------------------------------------------------

/// Reproduces the V.8 negotiation failure observed over TCP with 4800 bps
/// modems. This test replicates the exact tick cadence from serial_net.rs:
/// TCP reads happen on *every* 1 ms tick, but DSP processing only runs every
/// 10th tick (the `dsp_tick_counter` pattern). This is different from
/// `test_tcp_loopback_full_connection` which runs DSP on every tick.
///
/// If this test fails, the bug is in the modem DSP or V.8 negotiation logic
/// when driven at the serial_net.rs cadence. If it passes, the issue is in
/// serial_net.rs specifically (e.g. stream lifecycle, UART interaction).
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "host TCP-loopback timing on macOS is too variable for the real-time handshake tick budget — bursty non-blocking-read delivery intermittently starves the V.8 negotiation (flaky). Deterministic handshake coverage is in the in-memory stress.rs harness; this validates the real-TCP transport, which holds on Linux/Windows."
)]
fn test_v32bis_v8_negotiation_tcp_loopback() {
    let (mut caller_tcp, mut answerer_tcp) = loopback_pair();

    let mut caller = ModemDsp::new(ModemConfig {
        max_speed: 4800,
        ..Default::default()
    });
    let mut answerer = ModemDsp::new(ModemConfig {
        max_speed: 4800,
        ..Default::default()
    });

    let mut caller_rx_buf = WireBuffer::new();
    let mut answerer_rx_buf = WireBuffer::new();

    // Issue AT commands: answerer sets auto-answer on ring 1, caller dials.
    for &b in b"ATS0=1\r" {
        answerer.receive_from_serial(b);
    }
    for &b in b"ATD5551234\r" {
        caller.receive_from_serial(b);
    }

    // Drain initial result codes (OK from ATS0=1, etc.).
    drain_serial(&mut caller);
    drain_serial(&mut answerer);

    let mut caller_out = [0i16; CHUNK_SAMPLES];
    let mut answerer_out = [0i16; CHUNK_SAMPLES];

    // serial_net.rs calls tick_dsp_modem_data_pump() every ~1 ms.
    // DSP only fires every 10th call (10 ms = one PCM chunk period).
    // 9000 iterations = 9 seconds of wall time.
    let max_iterations = 9000;
    let mut connected = false;
    let mut connect_iter = 0;
    let mut caller_dsp_counter: u8 = 0;
    let mut answerer_dsp_counter: u8 = 0;

    // Track phase transitions for diagnostics.
    let mut caller_phases = vec![caller.phase()];
    let mut answerer_phases = vec![answerer.phase()];

    for iter in 0..max_iterations {
        // --- (a) Serial drain (every 1 ms tick, matching serial_net.rs) ---
        drain_serial(&mut caller);
        drain_serial(&mut answerer);

        // --- (b) Read TCP into wire buffers (every tick) ---
        read_tcp_into(&mut caller_tcp, &mut caller_rx_buf);
        read_tcp_into(&mut answerer_tcp, &mut answerer_rx_buf);

        // --- (c) DSP tick: only every 10th iteration ---
        caller_dsp_counter += 1;
        if caller_dsp_counter >= 10 {
            caller_dsp_counter = 0;

            let caller_line_in = caller_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            caller.tick_dsp(&caller_line_in, &mut caller_out);

            // Drain result codes immediately after DSP tick (matches
            // serial_net.rs step d2: drain after tick_dsp).
            drain_serial(&mut caller);

            // Write output PCM chunk to TCP.
            let encoded = wire::encode_chunk(&caller_out);
            let _ = caller_tcp.write_all(&encoded);
        }

        answerer_dsp_counter += 1;
        if answerer_dsp_counter >= 10 {
            answerer_dsp_counter = 0;

            let answerer_line_in = answerer_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            answerer.tick_dsp(&answerer_line_in, &mut answerer_out);

            drain_serial(&mut answerer);

            let encoded = wire::encode_chunk(&answerer_out);
            let _ = answerer_tcp.write_all(&encoded);
        }

        // --- Record phase transitions ---
        let cp = caller.phase();
        let ap = answerer.phase();
        if cp != *caller_phases.last().unwrap() {
            caller_phases.push(cp);
        }
        if ap != *answerer_phases.last().unwrap() {
            answerer_phases.push(ap);
        }

        if caller.is_online() && answerer.is_online() {
            connected = true;
            connect_iter = iter;
            break;
        }
    }

    // --- Assertions ---
    assert!(
        connected,
        "V.8 negotiation at 4800 bps should complete over TCP loopback \
         within 9 seconds (serial_net.rs cadence).\n\
         Caller phases:   {:?}\n\
         Answerer phases: {:?}",
        caller_phases, answerer_phases
    );

    // Verify the caller went through the expected negotiation sequence.
    assert!(
        caller_phases.contains(&ModemPhase::CallSetup),
        "Caller should have entered CallSetup. Phases: {:?}",
        caller_phases
    );
    assert!(
        caller_phases.contains(&ModemPhase::V8Negotiation),
        "Caller should have entered V8Negotiation. Phases: {:?}",
        caller_phases
    );
    assert!(
        caller_phases.contains(&ModemPhase::Training),
        "Caller should have entered Training. Phases: {:?}",
        caller_phases
    );
    assert!(
        caller_phases.contains(&ModemPhase::Online),
        "Caller should have reached Online. Phases: {:?}",
        caller_phases
    );

    // S0=1 answers in the same DSP tick that detects the first ring, so phase
    // sampling observes CallSetup directly rather than an intermediate Ringing.
    assert!(
        answerer_phases.contains(&ModemPhase::CallSetup),
        "S0=1 answerer should enter CallSetup on the first ring. Phases: {:?}",
        answerer_phases
    );
    assert!(
        !answerer_phases.contains(&ModemPhase::Ringing),
        "S0=1 answerer should not remain in Ringing. Phases: {:?}",
        answerer_phases
    );
    assert!(
        answerer_phases.contains(&ModemPhase::Online),
        "Answerer should have reached Online. Phases: {:?}",
        answerer_phases
    );

    // With a full 3.3-second ANSam, negotiation should still complete within ~8 seconds.
    let elapsed_ms = connect_iter;
    assert!(
        elapsed_ms < 8000,
        "Connection took {} ms — expected < 8000 ms.\n\
         Caller phases:   {:?}\n\
         Answerer phases: {:?}",
        elapsed_ms,
        caller_phases,
        answerer_phases
    );
}

// ---------------------------------------------------------------------------
// Non-blocking I/O test
// ---------------------------------------------------------------------------

/// Verify that non-blocking TCP reads integrate correctly with the DSP loop.
/// A modem running with no peer data should not panic or produce errors.
#[test]
fn test_tcp_nonblocking_no_data() {
    let (mut stream, _peer) = loopback_pair();

    let mut wire_buf = WireBuffer::new();
    let mut modem = ModemDsp::new(ModemConfig {
        max_speed: 9600,
        ..Default::default()
    });

    // Run 10 DSP ticks with no incoming data — WouldBlock on every read.
    for _ in 0..10 {
        read_tcp_into(&mut stream, &mut wire_buf);
        let line_in = wire_buf.take_chunk().unwrap_or(wire::SILENCE);
        let mut line_out = [0i16; CHUNK_SAMPLES];
        modem.tick_dsp(&line_in, &mut line_out);
    }

    // Modem should still be idle and functional.
    assert_eq!(modem.phase(), ModemPhase::Idle);
}

// ---------------------------------------------------------------------------
// Asymmetric start tests — timing mismatch between two emulator processes
// ---------------------------------------------------------------------------

/// Simulates Process A (answerer) running for 500 DSP ticks before Process B
/// (caller) connects. During the head-start, the answerer writes silence/CNG
/// response PCM to TCP but nobody reads from the other end. When the caller
/// finally starts, it reads a huge backlog of old PCM from the answerer while
/// the answerer processes the caller's CNG in real-time.
///
/// This reproduces the timing asymmetry seen in two-process TCP negotiation:
/// the answerer has been running for ~5 seconds before the caller even dials.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "host TCP-loopback timing on macOS is too variable for the real-time handshake tick budget — bursty non-blocking-read delivery intermittently starves the asymmetric-start V.8 negotiation (flaky). Deterministic handshake coverage is in the in-memory stress.rs harness; this validates the real-TCP transport, which holds on Linux/Windows."
)]
fn test_v32bis_v8_asymmetric_start_answerer_ahead() {
    let (mut caller_tcp, mut answerer_tcp) = loopback_pair();

    let mut answerer = ModemDsp::new(ModemConfig {
        max_speed: 4800,
        ..Default::default()
    });
    let mut answerer_rx_buf = WireBuffer::new();

    // Answerer auto-answers on ring 1.
    for &b in b"ATS0=1\r" {
        answerer.receive_from_serial(b);
    }
    drain_serial(&mut answerer);

    let mut answerer_out = [0i16; CHUNK_SAMPLES];

    // --- Phase 1: Answerer runs alone for 500 DSP ticks (5 seconds) ---
    // The answerer writes PCM to TCP, but nobody reads from the caller end.
    // This builds up a large backlog in the TCP buffer.
    let head_start_ticks = 500;
    let mut answerer_dsp_counter: u8 = 0;

    for _ in 0..(head_start_ticks * 10) {
        drain_serial(&mut answerer);

        // Read TCP into answerer's buffer (will be empty — caller hasn't started).
        read_tcp_into(&mut answerer_tcp, &mut answerer_rx_buf);

        answerer_dsp_counter += 1;
        if answerer_dsp_counter >= 10 {
            answerer_dsp_counter = 0;

            let answerer_line_in = answerer_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            answerer.tick_dsp(&answerer_line_in, &mut answerer_out);
            drain_serial(&mut answerer);

            let encoded = wire::encode_chunk(&answerer_out);
            let _ = answerer_tcp.write_all(&encoded);
        }
    }

    eprintln!("After head start: answerer phase = {:?}", answerer.phase());

    // --- Phase 2: Caller starts, both tick together ---
    let mut caller = ModemDsp::new(ModemConfig {
        max_speed: 4800,
        ..Default::default()
    });
    let mut caller_rx_buf = WireBuffer::new();

    for &b in b"ATD5551234\r" {
        caller.receive_from_serial(b);
    }
    drain_serial(&mut caller);

    let mut caller_out = [0i16; CHUNK_SAMPLES];
    let mut caller_dsp_counter: u8 = 0;

    let max_iterations = 9000;
    let mut connected = false;
    let mut connect_iter = 0;

    let mut caller_phases = vec![caller.phase()];
    let mut answerer_phases = vec![answerer.phase()];

    for iter in 0..max_iterations {
        drain_serial(&mut caller);
        drain_serial(&mut answerer);

        // Read TCP — caller will get a huge backlog on the first reads.
        read_tcp_into(&mut caller_tcp, &mut caller_rx_buf);
        read_tcp_into(&mut answerer_tcp, &mut answerer_rx_buf);

        caller_dsp_counter += 1;
        if caller_dsp_counter >= 10 {
            caller_dsp_counter = 0;

            // Flush stale PCM if the modem entered call setup.
            if caller.take_flush_wire_request() {
                flush_stale_pcm(&mut caller_tcp, &mut caller_rx_buf);
            }
            let caller_line_in = caller_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            caller.tick_dsp(&caller_line_in, &mut caller_out);
            drain_serial(&mut caller);

            let encoded = wire::encode_chunk(&caller_out);
            let _ = caller_tcp.write_all(&encoded);
        }

        answerer_dsp_counter += 1;
        if answerer_dsp_counter >= 10 {
            answerer_dsp_counter = 0;

            // Flush stale PCM if the modem entered call setup.
            if answerer.take_flush_wire_request() {
                flush_stale_pcm(&mut answerer_tcp, &mut answerer_rx_buf);
            }
            let answerer_line_in = answerer_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            answerer.tick_dsp(&answerer_line_in, &mut answerer_out);
            drain_serial(&mut answerer);

            let encoded = wire::encode_chunk(&answerer_out);
            let _ = answerer_tcp.write_all(&encoded);
        }

        let cp = caller.phase();
        let ap = answerer.phase();
        if cp != *caller_phases.last().unwrap() {
            caller_phases.push(cp);
        }
        if ap != *answerer_phases.last().unwrap() {
            answerer_phases.push(ap);
        }

        if caller.is_online() && answerer.is_online() {
            connected = true;
            connect_iter = iter;
            break;
        }
    }

    // Report the backlog size the caller saw.
    eprintln!(
        "Answerer-ahead test: connected={}, iter={}, caller backlog chunks remaining={}",
        connected,
        connect_iter,
        caller_rx_buf.chunks_available()
    );

    assert!(
        connected,
        "V.8 negotiation should succeed even when the answerer has a 5-second \
         head start (asymmetric timing).\n\
         Caller phases:   {:?}\n\
         Answerer phases: {:?}",
        caller_phases, answerer_phases
    );
    assert!(
        connect_iter < 8000,
        "Answerer-ahead connection took {} ms — expected < 8000 ms",
        connect_iter
    );

    assert!(
        caller_phases.contains(&ModemPhase::V8Negotiation),
        "Caller should have entered V8Negotiation. Phases: {:?}",
        caller_phases
    );
    assert!(
        caller_phases.contains(&ModemPhase::Online),
        "Caller should have reached Online. Phases: {:?}",
        caller_phases
    );
    assert!(
        answerer_phases.contains(&ModemPhase::Online),
        "Answerer should have reached Online. Phases: {:?}",
        answerer_phases
    );
}

/// Reverse asymmetry: the caller runs for 500 DSP ticks sending CNG tones
/// before the answerer starts. This tests whether a backlog of CNG tones
/// confuses the answerer's tone detection or V.8 state machine.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "host TCP-loopback timing on macOS is too variable for the real-time handshake tick budget — bursty non-blocking-read delivery intermittently starves the asymmetric-start V.8 negotiation (flaky; structural twin of the answerer-ahead case). Deterministic handshake coverage is in the in-memory stress.rs harness; this validates the real-TCP transport, which holds on Linux/Windows."
)]
fn test_v32bis_v8_asymmetric_start_caller_ahead() {
    let (mut caller_tcp, mut answerer_tcp) = loopback_pair();

    let mut caller = ModemDsp::new(ModemConfig {
        max_speed: 4800,
        ..Default::default()
    });
    let mut caller_rx_buf = WireBuffer::new();

    // Caller dials immediately.
    for &b in b"ATD5551234\r" {
        caller.receive_from_serial(b);
    }
    drain_serial(&mut caller);

    let mut caller_out = [0i16; CHUNK_SAMPLES];

    // --- Phase 1: Caller runs alone for 500 DSP ticks (5 seconds) ---
    // The caller writes CNG tones to TCP, building a backlog for the answerer.
    let head_start_ticks = 500;
    let mut caller_dsp_counter: u8 = 0;

    for _ in 0..(head_start_ticks * 10) {
        drain_serial(&mut caller);

        read_tcp_into(&mut caller_tcp, &mut caller_rx_buf);

        caller_dsp_counter += 1;
        if caller_dsp_counter >= 10 {
            caller_dsp_counter = 0;

            let caller_line_in = caller_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            caller.tick_dsp(&caller_line_in, &mut caller_out);
            drain_serial(&mut caller);

            let encoded = wire::encode_chunk(&caller_out);
            let _ = caller_tcp.write_all(&encoded);
        }
    }

    eprintln!("After head start: caller phase = {:?}", caller.phase());

    // --- Phase 2: Answerer starts, both tick together ---
    let mut answerer = ModemDsp::new(ModemConfig {
        max_speed: 4800,
        ..Default::default()
    });
    let mut answerer_rx_buf = WireBuffer::new();

    for &b in b"ATS0=1\r" {
        answerer.receive_from_serial(b);
    }
    drain_serial(&mut answerer);

    let mut answerer_out = [0i16; CHUNK_SAMPLES];
    let mut answerer_dsp_counter: u8 = 0;

    let max_iterations = 9000;
    let mut connected = false;
    let mut connect_iter = 0;

    let mut caller_phases = vec![caller.phase()];
    let mut answerer_phases = vec![answerer.phase()];

    for iter in 0..max_iterations {
        drain_serial(&mut caller);
        drain_serial(&mut answerer);

        // Read TCP — answerer will get a huge backlog of CNG tones.
        read_tcp_into(&mut caller_tcp, &mut caller_rx_buf);
        read_tcp_into(&mut answerer_tcp, &mut answerer_rx_buf);

        caller_dsp_counter += 1;
        if caller_dsp_counter >= 10 {
            caller_dsp_counter = 0;

            // Flush stale PCM if the modem entered call setup.
            if caller.take_flush_wire_request() {
                flush_stale_pcm(&mut caller_tcp, &mut caller_rx_buf);
            }
            let caller_line_in = caller_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            caller.tick_dsp(&caller_line_in, &mut caller_out);
            drain_serial(&mut caller);

            let encoded = wire::encode_chunk(&caller_out);
            let _ = caller_tcp.write_all(&encoded);
        }

        answerer_dsp_counter += 1;
        if answerer_dsp_counter >= 10 {
            answerer_dsp_counter = 0;

            // Flush stale PCM if the modem entered call setup.
            if answerer.take_flush_wire_request() {
                flush_stale_pcm(&mut answerer_tcp, &mut answerer_rx_buf);
            }
            let answerer_line_in = answerer_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            answerer.tick_dsp(&answerer_line_in, &mut answerer_out);
            drain_serial(&mut answerer);

            let encoded = wire::encode_chunk(&answerer_out);
            let _ = answerer_tcp.write_all(&encoded);
        }

        let cp = caller.phase();
        let ap = answerer.phase();
        if cp != *caller_phases.last().unwrap() {
            caller_phases.push(cp);
        }
        if ap != *answerer_phases.last().unwrap() {
            answerer_phases.push(ap);
        }

        if caller.is_online() && answerer.is_online() {
            connected = true;
            connect_iter = iter;
            break;
        }
    }

    eprintln!(
        "Caller-ahead test: connected={}, iter={}, answerer backlog chunks remaining={}",
        connected,
        connect_iter,
        answerer_rx_buf.chunks_available()
    );

    assert!(
        connected,
        "V.8 negotiation should succeed even when the caller has a 5-second \
         head start (asymmetric timing).\n\
         Caller phases:   {:?}\n\
         Answerer phases: {:?}",
        caller_phases, answerer_phases
    );
    assert!(
        connect_iter < 8000,
        "Caller-ahead connection took {} ms — expected < 8000 ms",
        connect_iter
    );

    assert!(
        answerer_phases.contains(&ModemPhase::V8Negotiation),
        "Answerer should have entered V8Negotiation. Phases: {:?}",
        answerer_phases
    );
    assert!(
        answerer_phases.contains(&ModemPhase::Online),
        "Answerer should have reached Online. Phases: {:?}",
        answerer_phases
    );
    assert!(
        caller_phases.contains(&ModemPhase::Online),
        "Caller should have reached Online. Phases: {:?}",
        caller_phases
    );
}

// ---------------------------------------------------------------------------
// Online data exchange test — verifies bytes flow after CONNECT
// ---------------------------------------------------------------------------

/// After a full V.32bis handshake, verify that data bytes actually flow
/// between the two modems. This catches the convergence deadlock where
/// `rx_converge_count` starts at 0 and never reaches 255 because silence
/// on the wire means the 0x7E flag is never decoded.
///
/// Uses the serial_net.rs cadence (DSP every 10th tick) to match real usage.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "host TCP-loopback timing on macOS is too variable for the real-time handshake tick budget — the handshake must complete before the data-exchange phase, and bursty non-blocking-read delivery intermittently starves it (flaky). Deterministic handshake + data coverage is in the in-memory stress.rs harness; this validates the real-TCP serial_net cadence, which holds on Linux/Windows."
)]
fn test_v32bis_online_data_exchange_with_serial_net_cadence() {
    let (mut caller_tcp, mut answerer_tcp) = loopback_pair();

    let mut caller = ModemDsp::new(ModemConfig {
        max_speed: 4800,
        ..Default::default()
    });
    let mut answerer = ModemDsp::new(ModemConfig {
        max_speed: 4800,
        ..Default::default()
    });

    let mut caller_rx_buf = WireBuffer::new();
    let mut answerer_rx_buf = WireBuffer::new();

    // Issue AT commands: answerer sets auto-answer on ring 1, caller dials.
    for &b in b"ATS0=1\r" {
        answerer.receive_from_serial(b);
    }
    for &b in b"ATD5551234\r" {
        caller.receive_from_serial(b);
    }

    // Drain initial result codes (OK from ATS0=1, etc.).
    drain_serial(&mut caller);
    drain_serial(&mut answerer);

    let mut caller_out = [0i16; CHUNK_SAMPLES];
    let mut answerer_out = [0i16; CHUNK_SAMPLES];

    // --- Phase 1: Complete the V.32bis handshake ---
    let max_iterations = 9000;
    let mut connected = false;
    let mut caller_dsp_counter: u8 = 0;
    let mut answerer_dsp_counter: u8 = 0;

    for _iter in 0..max_iterations {
        drain_serial(&mut caller);
        drain_serial(&mut answerer);

        read_tcp_into(&mut caller_tcp, &mut caller_rx_buf);
        read_tcp_into(&mut answerer_tcp, &mut answerer_rx_buf);

        caller_dsp_counter += 1;
        if caller_dsp_counter >= 10 {
            caller_dsp_counter = 0;
            let caller_line_in = caller_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            caller.tick_dsp(&caller_line_in, &mut caller_out);
            drain_serial(&mut caller);
            let encoded = wire::encode_chunk(&caller_out);
            let _ = caller_tcp.write_all(&encoded);
        }

        answerer_dsp_counter += 1;
        if answerer_dsp_counter >= 10 {
            answerer_dsp_counter = 0;
            let answerer_line_in = answerer_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            answerer.tick_dsp(&answerer_line_in, &mut answerer_out);
            drain_serial(&mut answerer);
            let encoded = wire::encode_chunk(&answerer_out);
            let _ = answerer_tcp.write_all(&encoded);
        }

        if caller.is_online() && answerer.is_online() {
            connected = true;
            break;
        }
    }

    assert!(
        connected,
        "Handshake must complete before testing data exchange"
    );

    // Drain CONNECT result codes so they don't pollute data checks.
    drain_serial(&mut caller);
    drain_serial(&mut answerer);

    // --- Phase 2: RX convergence ---
    // Both modems start with rx_converge_count=0 and need to synchronize
    // before data flows. The convergence timeout fires after >50 DSP ticks.
    // At 10-tick cadence, that's ~510+ loop iterations. Run 600 to be safe.
    let convergence_ticks = 600;
    for _iter in 0..convergence_ticks {
        read_tcp_into(&mut caller_tcp, &mut caller_rx_buf);
        read_tcp_into(&mut answerer_tcp, &mut answerer_rx_buf);

        caller_dsp_counter += 1;
        if caller_dsp_counter >= 10 {
            caller_dsp_counter = 0;
            let caller_line_in = caller_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            caller.tick_dsp(&caller_line_in, &mut caller_out);
            let encoded = wire::encode_chunk(&caller_out);
            let _ = caller_tcp.write_all(&encoded);
        }

        answerer_dsp_counter += 1;
        if answerer_dsp_counter >= 10 {
            answerer_dsp_counter = 0;
            let answerer_line_in = answerer_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            answerer.tick_dsp(&answerer_line_in, &mut answerer_out);
            let encoded = wire::encode_chunk(&answerer_out);
            let _ = answerer_tcp.write_all(&encoded);
        }

        // Drain any serial output during convergence (should be empty).
        drain_serial(&mut caller);
        drain_serial(&mut answerer);
    }

    // --- Phase 3: Send a byte from caller to answerer ---
    // Feed the test byte into the caller's serial TX buffer AFTER
    // convergence, so it isn't discarded during the convergence window.
    let test_byte: u8 = 0x42; // 'B'
    caller.receive_from_serial(test_byte);

    // Run DSP ticks to propagate the byte through the pipe.
    let data_ticks = 500;
    let mut received_bytes: Vec<u8> = Vec::new();

    for _iter in 0..data_ticks {
        read_tcp_into(&mut caller_tcp, &mut caller_rx_buf);
        read_tcp_into(&mut answerer_tcp, &mut answerer_rx_buf);

        caller_dsp_counter += 1;
        if caller_dsp_counter >= 10 {
            caller_dsp_counter = 0;
            let caller_line_in = caller_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            caller.tick_dsp(&caller_line_in, &mut caller_out);
            let encoded = wire::encode_chunk(&caller_out);
            let _ = caller_tcp.write_all(&encoded);
        }

        answerer_dsp_counter += 1;
        if answerer_dsp_counter >= 10 {
            answerer_dsp_counter = 0;
            let answerer_line_in = answerer_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            answerer.tick_dsp(&answerer_line_in, &mut answerer_out);
            let encoded = wire::encode_chunk(&answerer_out);
            let _ = answerer_tcp.write_all(&encoded);
        }

        // Check if the answerer has data for the DTE.
        while answerer.has_data_for_serial() {
            if let Some(b) = answerer.send_to_serial() {
                received_bytes.push(b);
            }
        }
    }

    assert!(
        received_bytes.contains(&test_byte),
        "Answerer should have received byte 0x{:02X} from caller, \
         but got {:?} ({} bytes total)",
        test_byte,
        received_bytes
            .iter()
            .map(|b| format!("0x{:02X}", b))
            .collect::<Vec<_>>(),
        received_bytes.len()
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Drain all pending serial output from a modem (discard the bytes).
fn drain_serial(modem: &mut ModemDsp) {
    while modem.has_data_for_serial() {
        modem.send_to_serial();
    }
}

/// Non-blocking read from a TCP stream into a WireBuffer.
fn read_tcp_into(stream: &mut TcpStream, wb: &mut WireBuffer) {
    let mut buf = [0u8; 4096];
    match stream.read(&mut buf) {
        Ok(n) if n > 0 => wb.push(&buf[..n]),
        _ => {} // WouldBlock or zero bytes — nothing to do
    }
}

/// Flush stale PCM: drain all pending TCP data and clear the wire buffer.
/// Called when the modem enters call setup to discard the backlog that
/// accumulates when one side starts before the other.
fn flush_stale_pcm(stream: &mut TcpStream, wb: &mut WireBuffer) {
    // Drain the OS TCP receive buffer — read and discard until WouldBlock.
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(n) if n > 0 => {} // discard
            _ => break,
        }
    }
    wb.clear();
}

// ---------------------------------------------------------------------------
// V.90 TCP loopback test
// ---------------------------------------------------------------------------

/// V.90 negotiation over TCP loopback.
/// Verifies both modems reach Online via V.90 using the serial_net.rs-style
/// tick cadence (DSP every 10th iteration).
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "host TCP-loopback timing on macOS is too variable for the real-time handshake tick budget — bursty non-blocking-read delivery intermittently starves the V.90 negotiation (flaky). Deterministic handshake coverage is in the in-memory stress.rs harness; this validates the real-TCP transport, which holds on Linux/Windows."
)]
fn test_v90_tcp_loopback() {
    let (mut caller_tcp, mut answerer_tcp) = loopback_pair();

    let mut caller = ModemDsp::new(ModemConfig {
        max_speed: 56000,
        ..Default::default()
    });
    let mut answerer = ModemDsp::new(ModemConfig {
        max_speed: 56000,
        ..Default::default()
    });

    let mut caller_rx_buf = WireBuffer::new();
    let mut answerer_rx_buf = WireBuffer::new();

    for &b in b"ATS0=1\r" {
        answerer.receive_from_serial(b);
    }
    for &b in b"ATD5551234\r" {
        caller.receive_from_serial(b);
    }

    drain_serial(&mut caller);
    drain_serial(&mut answerer);

    let mut caller_out = [0i16; CHUNK_SAMPLES];
    let mut answerer_out = [0i16; CHUNK_SAMPLES];

    // V.90 training takes longer, especially after a full 3.3-second ANSam.
    let max_iterations = 12000;
    let mut connected = false;
    let mut connect_iter = 0;
    let mut caller_dsp_counter: u8 = 0;
    let mut answerer_dsp_counter: u8 = 0;

    let mut caller_phases = vec![caller.phase()];
    let mut answerer_phases = vec![answerer.phase()];

    for _iter in 0..max_iterations {
        drain_serial(&mut caller);
        drain_serial(&mut answerer);

        read_tcp_into(&mut caller_tcp, &mut caller_rx_buf);
        read_tcp_into(&mut answerer_tcp, &mut answerer_rx_buf);

        caller_dsp_counter += 1;
        if caller_dsp_counter >= 10 {
            caller_dsp_counter = 0;
            let caller_line_in = caller_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            caller.tick_dsp(&caller_line_in, &mut caller_out);
            drain_serial(&mut caller);
            let encoded = wire::encode_chunk(&caller_out);
            let _ = caller_tcp.write_all(&encoded);
        }

        answerer_dsp_counter += 1;
        if answerer_dsp_counter >= 10 {
            answerer_dsp_counter = 0;
            let answerer_line_in = answerer_rx_buf.take_chunk().unwrap_or(wire::SILENCE);
            answerer.tick_dsp(&answerer_line_in, &mut answerer_out);
            drain_serial(&mut answerer);
            let encoded = wire::encode_chunk(&answerer_out);
            let _ = answerer_tcp.write_all(&encoded);
        }

        let cp = caller.phase();
        let ap = answerer.phase();
        if cp != *caller_phases.last().unwrap() {
            caller_phases.push(cp);
        }
        if ap != *answerer_phases.last().unwrap() {
            answerer_phases.push(ap);
        }

        if caller.is_online() && answerer.is_online() {
            connected = true;
            connect_iter = _iter;
            break;
        }
    }

    assert!(
        connected,
        "V.90 negotiation should complete over TCP loopback.\n\
         Caller phases: {:?}\nAnswerer phases: {:?}",
        caller_phases, answerer_phases
    );
    assert!(
        connect_iter < 11000,
        "V.90 loopback took {} ms — expected < 11000 ms",
        connect_iter
    );
}
