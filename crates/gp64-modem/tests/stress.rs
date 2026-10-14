//! Modem DSP stress / torture tests.
//!
//! Hammers the modem with hundreds of handshakes, megabytes of data transfer,
//! unexpected disconnects, speed changes, and adversarial conditions.
//!
//! The torture test runs for ~30 minutes and exercises every code path we can
//! reach from the DSP layer.
//!
//! Quick tests (< 2s each):
//!   cargo test -p gp64-modem --test stress
//!
//! Full torture (30 min):
//!   cargo test -p gp64-modem --test stress -- --include-ignored --nocapture

use std::time::{Duration, Instant};

use gp64_modem::wire::{self, WireBuffer, CHUNK_SAMPLES};
use gp64_modem::{ModemConfig, ModemDsp, ModemPhase};

fn run_long_soaks() -> bool {
    // `MDEM_LONG_TESTS` is a comma list; `soaks` selects the long durations.
    std::env::var("MDEM_LONG_TESTS")
        .is_ok_and(|keys| keys.split(',').any(|key| key.trim() == "soaks"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn drain_serial(modem: &mut ModemDsp) -> Vec<u8> {
    let mut out = Vec::new();
    while modem.has_data_for_serial() {
        if let Some(b) = modem.send_to_serial() {
            out.push(b);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// ModemPair: reusable harness
// ---------------------------------------------------------------------------

struct ModemPair {
    caller: ModemDsp,
    answerer: ModemDsp,
    caller_wb: WireBuffer,
    answerer_wb: WireBuffer,
    caller_out: [i16; CHUNK_SAMPLES],
    answerer_out: [i16; CHUNK_SAMPLES],
    /// Total bytes successfully transferred caller→answerer
    bytes_c2a: u64,
    /// Total bytes successfully transferred answerer→caller
    bytes_a2c: u64,
    /// Total handshakes completed
    handshakes: u32,
}

impl ModemPair {
    /// Create a connected modem pair. Performs handshake + convergence.
    fn connect(max_speed: u32) -> Self {
        let mut pair = Self {
            caller: ModemDsp::new(ModemConfig {
                max_speed,
                ..Default::default()
            }),
            answerer: ModemDsp::new(ModemConfig {
                max_speed,
                ..Default::default()
            }),
            caller_wb: WireBuffer::new(),
            answerer_wb: WireBuffer::new(),
            caller_out: [0i16; CHUNK_SAMPLES],
            answerer_out: [0i16; CHUNK_SAMPLES],
            bytes_c2a: 0,
            bytes_a2c: 0,
            handshakes: 0,
        };
        pair.do_handshake(max_speed);
        pair
    }

    /// Issue AT commands and run handshake. Panics on timeout.
    fn do_handshake(&mut self, max_speed: u32) {
        for &b in b"ATS0=1\r" {
            self.answerer.receive_from_serial(b);
        }
        drain_serial(&mut self.answerer);
        for &b in b"ATD5551234\r" {
            self.caller.receive_from_serial(b);
        }
        drain_serial(&mut self.caller);

        // V.32bis needs ~300 ticks, V.34 needs ~600-1500 ticks
        let timeout = if max_speed > 14400 { 2000 } else { 800 };
        let mut connected = false;
        for _ in 0..timeout {
            self.dsp_tick_drain();
            if self.caller.is_online() && self.answerer.is_online() {
                connected = true;
                break;
            }
        }
        assert!(
            connected,
            "Handshake failed at speed {max_speed} (timeout={timeout}): \
             caller={:?} answerer={:?}",
            self.caller.phase(),
            self.answerer.phase()
        );

        drain_serial(&mut self.caller);
        drain_serial(&mut self.answerer);

        // Convergence: V.34/V.90 Buffering needs ~235 ticks
        let convergence = if max_speed > 14400 { 300 } else { 60 };
        for _ in 0..convergence {
            self.dsp_tick_drain();
        }
        drain_serial(&mut self.caller);
        drain_serial(&mut self.answerer);

        self.handshakes += 1;
    }

    /// One DSP tick cycle (10ms simulated).
    /// Does NOT drain serial output — callers collect that themselves.
    ///
    /// The two modems are connected by an in-memory crossover: each side's
    /// take_chunk() consumes the chunk the *other* side produced on the previous
    /// tick (pushed at the end below), then this tick's outputs are pushed into
    /// the opposite input buffer. That is exactly the one-tick latency the old
    /// 127.0.0.1 TCP loopback provided, but deterministic — the previous socket
    /// transport flaked on macOS when a non-blocking read raced the loopback
    /// delivery and starved the handshake of samples inside the tick budget.
    fn dsp_tick(&mut self) {
        let caller_in = self.caller_wb.take_chunk().unwrap_or(wire::SILENCE);
        let answerer_in = self.answerer_wb.take_chunk().unwrap_or(wire::SILENCE);
        self.caller.tick_dsp(&caller_in, &mut self.caller_out);
        self.answerer.tick_dsp(&answerer_in, &mut self.answerer_out);
        let c_enc = wire::encode_chunk(&self.caller_out);
        let a_enc = wire::encode_chunk(&self.answerer_out);
        // Crossover: caller's output is the answerer's next input, and vice versa.
        self.answerer_wb.push(&c_enc);
        self.caller_wb.push(&a_enc);
    }

    /// DSP tick that also drains (and discards) serial output from both modems.
    /// Used during handshake/convergence where we don't care about serial data.
    fn dsp_tick_drain(&mut self) {
        drain_serial(&mut self.caller);
        drain_serial(&mut self.answerer);
        self.dsp_tick();
    }

    /// Transfer `count` bytes caller→answerer with sequential pattern.
    /// Feeds in batches, verifies every byte. Returns bytes transferred.
    fn transfer_c2a(&mut self, count: usize) -> usize {
        if !self.caller.is_online() || !self.answerer.is_online() {
            panic!(
                "C→A: modems not online at entry (caller={:?} answerer={:?}, \
                 handshakes={}, bytes_c2a={}, bytes_a2c={})",
                self.caller.phase(),
                self.answerer.phase(),
                self.handshakes,
                self.bytes_c2a,
                self.bytes_a2c
            );
        }
        // Drain receiver (answerer) to clear stale bytes from full-duplex idle.
        drain_serial(&mut self.answerer);
        let tx_data: Vec<u8> = (0..count).map(|i| (i % 251) as u8).collect();
        let mut fed = 0;
        let mut received = Vec::new();

        for _ in 0..(count * 10 + 2000) {
            // Drain both sides' serial output (caller may have stale result codes)
            drain_serial(&mut self.caller);
            let end = (fed + 16).min(count);
            for &b in &tx_data[fed..end] {
                self.caller.receive_from_serial(b);
            }
            fed = end;
            self.dsp_tick();
            let rx = drain_serial(&mut self.answerer);
            received.extend_from_slice(&rx);
            if received.len() >= count {
                break;
            }
        }

        // Full-duplex modems may deliver a few extra pipeline bytes; truncate
        // like the torture test does, then verify the payload matches.
        received.truncate(count);
        assert_eq!(
            received.len(),
            count,
            "C→A: expected {count} bytes, got {}",
            received.len()
        );
        assert_eq!(received, tx_data, "C→A: data corruption");
        self.bytes_c2a += count as u64;
        count
    }

    /// Transfer `count` bytes answerer→caller with sequential pattern.
    fn transfer_a2c(&mut self, count: usize) -> usize {
        assert!(
            self.caller.is_online() && self.answerer.is_online(),
            "A→C: modems not online (caller={:?} answerer={:?})",
            self.caller.phase(),
            self.answerer.phase()
        );
        // Drain receiver (caller) to clear stale bytes from full-duplex idle.
        drain_serial(&mut self.caller);
        let tx_data: Vec<u8> = (0..count).map(|i| ((i * 7 + 3) % 251) as u8).collect();
        let mut fed = 0;
        let mut received = Vec::new();

        for _ in 0..(count * 10 + 2000) {
            drain_serial(&mut self.answerer);
            let end = (fed + 16).min(count);
            for &b in &tx_data[fed..end] {
                self.answerer.receive_from_serial(b);
            }
            fed = end;
            self.dsp_tick();
            let rx = drain_serial(&mut self.caller);
            received.extend_from_slice(&rx);
            if received.len() >= count {
                break;
            }
        }

        // Full-duplex modems may deliver a few extra pipeline bytes; truncate
        // like the torture test does, then verify the payload matches.
        received.truncate(count);
        assert_eq!(
            received.len(),
            count,
            "A→C: expected {count} bytes, got {}",
            received.len()
        );
        assert_eq!(received, tx_data, "A→C: data corruption");
        self.bytes_a2c += count as u64;
        count
    }

    /// Bidirectional transfer. Feeds both sides simultaneously.
    fn transfer_bidir(&mut self, count: usize) -> usize {
        let c_data: Vec<u8> = (0..count).map(|i| (i % 251) as u8).collect();
        let a_data: Vec<u8> = (0..count).map(|i| ((i * 13 + 7) % 251) as u8).collect();
        let mut c_fed = 0;
        let mut a_fed = 0;
        let mut c_rx = Vec::new();
        let mut a_rx = Vec::new();

        for _ in 0..(count * 10 + 2000) {
            let c_end = (c_fed + 8).min(count);
            for &b in &c_data[c_fed..c_end] {
                self.caller.receive_from_serial(b);
            }
            c_fed = c_end;

            let a_end = (a_fed + 8).min(count);
            for &b in &a_data[a_fed..a_end] {
                self.answerer.receive_from_serial(b);
            }
            a_fed = a_end;

            self.dsp_tick();
            a_rx.extend_from_slice(&drain_serial(&mut self.answerer));
            c_rx.extend_from_slice(&drain_serial(&mut self.caller));

            if a_rx.len() >= count && c_rx.len() >= count {
                break;
            }
        }

        assert_eq!(
            a_rx.len(),
            count,
            "bidir C→A: expected {count}, got {}",
            a_rx.len()
        );
        assert_eq!(
            c_rx.len(),
            count,
            "bidir A→C: expected {count}, got {}",
            c_rx.len()
        );
        assert_eq!(a_rx, c_data, "bidir C→A: data corruption");
        assert_eq!(c_rx, a_data, "bidir A→C: data corruption");
        self.bytes_c2a += count as u64;
        self.bytes_a2c += count as u64;
        count * 2
    }

    /// Graceful disconnect via +++ / ATH. Returns true if successful.
    fn hangup_caller(&mut self) -> bool {
        // Guard time silence
        for _ in 0..50 {
            self.dsp_tick_drain();
        }
        for &b in b"+++" {
            self.caller.receive_from_serial(b);
        }
        for _ in 0..150 {
            self.dsp_tick_drain();
        }
        drain_serial(&mut self.caller); // drain "OK" from escape
        for &b in b"ATH\r" {
            self.caller.receive_from_serial(b);
        }
        for _ in 0..100 {
            self.dsp_tick_drain();
            if self.caller.phase() == ModemPhase::Idle {
                return true;
            }
        }
        false
    }

    /// Abrupt disconnect: feed silence to simulate carrier loss.
    fn abrupt_disconnect(&mut self) {
        self.caller_wb.clear();
        self.answerer_wb.clear();

        // Feed silence directly until both modems detect carrier loss (S10
        // timeout). No crossover here: each side must see silence, not the
        // other's output.
        for _ in 0..200 {
            let silence = wire::SILENCE;
            self.caller.tick_dsp(&silence, &mut self.caller_out);
            self.answerer.tick_dsp(&silence, &mut self.answerer_out);
            if !self.caller.is_online() && !self.answerer.is_online() {
                break;
            }
        }
        drain_serial(&mut self.caller);
        drain_serial(&mut self.answerer);
    }

    /// Reconnect after disconnect.
    fn reconnect(&mut self, max_speed: u32) {
        eprintln!(
            "    reconnect({max_speed}): pre-state caller={:?} answerer={:?}",
            self.caller.phase(),
            self.answerer.phase()
        );
        self.caller = ModemDsp::new(ModemConfig {
            max_speed,
            ..Default::default()
        });
        self.answerer = ModemDsp::new(ModemConfig {
            max_speed,
            ..Default::default()
        });
        self.caller_wb.clear();
        self.answerer_wb.clear();
        self.do_handshake(max_speed);
    }
}

// ---------------------------------------------------------------------------
// Quick sanity tests (run by default, < 2s each)
// ---------------------------------------------------------------------------

/// Quick smoke: connect + 64 bytes at each speed, both directions.
#[test]
fn test_quick_all_speeds() {
    for &speed in &[4800, 9600, 14400, 28800] {
        let mut pair = ModemPair::connect(speed);
        pair.transfer_c2a(64);
        pair.transfer_a2c(64);
    }
}

/// Quick smoke: bidirectional 256 bytes.
#[test]
fn test_quick_bidir() {
    let mut pair = ModemPair::connect(9600);
    pair.transfer_bidir(256);
}

// ---------------------------------------------------------------------------
// Torture test: long soak when explicitly enabled, compact smoke otherwise.
// ---------------------------------------------------------------------------

/// Torture coverage for handshakes, data transfer, abrupt disconnects,
/// speed changes, and adversarial conditions.
///
/// By default this ignored test runs a compact smoke path for automation.
/// Set MDEM_LONG_TESTS=soaks for the 30-minute soak.
#[test]
#[ignore = "full_test (--include-ignored): runs a compact smoke path by default; the 30-minute soak needs MDEM_LONG_TESTS=soaks."]
fn test_torture_30min() {
    if !run_long_soaks() {
        let mut pair = ModemPair::connect(9600);
        pair.transfer_c2a(64);
        pair.transfer_a2c(64);
        pair.abrupt_disconnect();
        pair.reconnect(4800);
        pair.transfer_bidir(64);
        eprintln!("=== MODEM TORTURE SMOKE COMPLETE ===");
        return;
    }

    let start = Instant::now();
    let duration = Duration::from_mins(30); // 30 minutes
    let speeds = [4800, 9600, 14400, 28800];

    let mut total_bytes: u64 = 0;
    let mut total_handshakes: u32 = 0;
    let mut total_abrupt_disconnects: u32 = 0;
    let mut total_graceful_disconnects: u32 = 0;
    let mut round = 0u32;

    eprintln!("=== MODEM TORTURE TEST (30 min) ===");
    eprintln!("Speeds: {:?}", speeds);
    let mut last_report = Instant::now();

    // Create ONE pair and reuse it — avoids exhausting OS socket buffers.
    let initial_speed = speeds[0];
    let mut pair = ModemPair::connect(initial_speed);
    total_handshakes += 1;
    eprintln!(
        "  after connect: caller={:?} answerer={:?}",
        pair.caller.phase(),
        pair.answerer.phase()
    );
    // Warm up the TCP pipeline with a few extra ticks
    for _ in 0..20 {
        pair.dsp_tick_drain();
    }
    eprintln!(
        "  after warmup: caller={:?} answerer={:?}",
        pair.caller.phase(),
        pair.answerer.phase()
    );

    while start.elapsed() < duration {
        round += 1;
        let speed = speeds[round as usize % speeds.len()];

        // Ensure connected at the current speed (handles post-disconnect state)
        if !pair.caller.is_online() || !pair.answerer.is_online() {
            pair.reconnect(speed);
            total_handshakes += 1;
        }

        // ── Phase 2: Small transfer (verifies link works) ─────
        pair.transfer_c2a(64);
        pair.transfer_a2c(64);
        total_bytes += 128;

        // ── Phase 3: Bulk transfer (4KB each direction) ───────
        pair.transfer_c2a(4096);
        pair.transfer_a2c(4096);
        total_bytes += 8192;

        // ── Phase 4: Bidirectional simultaneous ───────────────
        let bidir_bytes = pair.transfer_bidir(2048);
        total_bytes += bidir_bytes as u64;

        // ── Phase 5: Graceful disconnect + reconnect ──────────
        let hung_up = pair.hangup_caller();
        if hung_up {
            total_graceful_disconnects += 1;
            pair.reconnect(speed);
            total_handshakes += 1;

            // Verify link works after reconnect
            pair.transfer_c2a(128);
            pair.transfer_a2c(128);
            total_bytes += 256;
        }

        // ── Phase 6: Abrupt disconnect + reconnect ────────────
        pair.abrupt_disconnect();
        total_abrupt_disconnects += 1;

        // Reconnect at a DIFFERENT speed (tests speed change)
        let new_speed = speeds[(round as usize + 1) % speeds.len()];
        pair.reconnect(new_speed);
        total_handshakes += 1;

        // Verify link works after abrupt disconnect + speed change
        pair.transfer_c2a(256);
        pair.transfer_a2c(256);
        total_bytes += 512;

        // ── Phase 7: Large bidirectional soak ─────────────────
        let soak_bytes = pair.transfer_bidir(8192);
        total_bytes += soak_bytes as u64;

        // ── Phase 8: Another abrupt disconnect ────────────────
        pair.abrupt_disconnect();
        total_abrupt_disconnects += 1;

        // ── Progress report every 30 seconds ──────────────────
        if last_report.elapsed() >= Duration::from_secs(30) {
            let elapsed = start.elapsed().as_secs();
            let mb = total_bytes as f64 / (1024.0 * 1024.0);
            eprintln!(
                "[{:3}m {:02}s] round={round} handshakes={total_handshakes} \
                 graceful={total_graceful_disconnects} abrupt={total_abrupt_disconnects} \
                 transferred={mb:.2} MB",
                elapsed / 60,
                elapsed % 60,
            );
            last_report = Instant::now();
        }

        if start.elapsed() >= duration {
            break;
        }
    }

    let elapsed = start.elapsed().as_secs();
    let mb = total_bytes as f64 / (1024.0 * 1024.0);
    eprintln!();
    eprintln!("=== TORTURE TEST COMPLETE ===");
    eprintln!("Duration:     {}m {}s", elapsed / 60, elapsed % 60);
    eprintln!("Rounds:       {round}");
    eprintln!("Handshakes:   {total_handshakes}");
    eprintln!("Graceful DC:  {total_graceful_disconnects}");
    eprintln!("Abrupt DC:    {total_abrupt_disconnects}");
    eprintln!("Data:         {mb:.2} MB");
    eprintln!("Errors:       0 (test would have panicked)");
}

// ---------------------------------------------------------------------------
// Focused stress tests. Default automation runs smoke sizes; set
// MDEM_LONG_TESTS=soaks for the full soak sizes.
// ---------------------------------------------------------------------------

/// Rapid connect/disconnect cycling: 100 cycles at each speed in long-soak
/// mode, or a 9600-bps two-cycle smoke by default.
#[test]
#[ignore = "full_test (--include-ignored): connect/disconnect cycling; a two-cycle smoke by default, 100 cycles per speed under MDEM_LONG_TESTS=soaks."]
fn test_stress_connect_disconnect_100x() {
    let long = run_long_soaks();
    let speeds: &[u32] = if long {
        &[4800, 9600, 14400, 28800]
    } else {
        &[9600]
    };
    let cycles_per_speed = if long { 100 } else { 2 };

    for &speed in speeds {
        let start = Instant::now();
        let mut pair = ModemPair::connect(speed);
        let mut cycles = 0u32;

        for _ in 0..cycles_per_speed {
            // Quick data exchange
            pair.transfer_c2a(32);

            // Abrupt disconnect + reconnect (faster than graceful)
            pair.abrupt_disconnect();
            pair.reconnect(speed);
            cycles += 1;
        }

        eprintln!(
            "  speed {speed}: {cycles} cycles in {:.1}s",
            start.elapsed().as_secs_f64()
        );
    }
}

/// Bulk transfer soak: 1 MB at each speed in long-soak mode, or an 8 KB
/// 9600-bps smoke by default.
#[test]
#[ignore = "full_test (--include-ignored): bulk transfer; an 8 KB smoke by default, 1 MB per speed under MDEM_LONG_TESTS=soaks."]
fn test_stress_1mb_transfer() {
    let long = run_long_soaks();
    let total = if long { 1024 * 1024 } else { 8 * 1024 };
    let chunk = 4096;
    let speeds: &[u32] = if long {
        &[4800, 9600, 14400, 28800]
    } else {
        &[9600]
    };

    for &speed in speeds {
        let start = Instant::now();
        let mut pair = ModemPair::connect(speed);
        let mut transferred = 0u64;

        while transferred < total as u64 {
            pair.transfer_c2a(chunk);
            transferred += chunk as u64;

            if transferred.is_multiple_of(128 * 1024) {
                eprintln!(
                    "  speed {speed}: {} KB / {} KB",
                    transferred / 1024,
                    total / 1024
                );
            }
        }

        eprintln!(
            "  speed {speed}: {} KB in {:.1}s",
            total / 1024,
            start.elapsed().as_secs_f64()
        );
    }
}

/// Abrupt disconnect during data transfer (mid-stream kill).
#[test]
#[ignore = "full_test (--include-ignored): mid-stream disconnect; smoke by default, full soak under MDEM_LONG_TESTS=soaks."]
fn test_stress_midstream_disconnect() {
    let long = run_long_soaks();
    let speeds: &[u32] = if long {
        &[4800, 9600, 14400, 28800]
    } else {
        &[9600]
    };
    let attempts = if long { 20 } else { 2 };

    for &speed in speeds {
        for attempt in 0..attempts {
            let mut pair = ModemPair::connect(speed);

            // Start feeding data but disconnect before it's all received
            let data: Vec<u8> = (0..512).map(|i| (i % 251) as u8).collect();
            for &b in &data[..256] {
                pair.caller.receive_from_serial(b);
            }
            // Run a few ticks so some data is in-flight
            for _ in 0..5 {
                pair.dsp_tick();
            }

            // YANK the connection mid-transfer
            pair.abrupt_disconnect();

            // Reconnect and verify clean state
            pair.reconnect(speed);

            // Full data exchange must work after mid-stream kill
            pair.transfer_c2a(128);
            pair.transfer_a2c(128);

            if attempt == attempts - 1 {
                eprintln!("  speed {speed}: {attempts} mid-stream disconnects OK");
            }
        }
    }
}

/// Speed cycling: connect at one speed, disconnect, reconnect at a different speed.
/// Repeat through all speed permutations.
#[test]
#[ignore = "full_test (--include-ignored): speed cycling; smoke by default, full permutation soak under MDEM_LONG_TESTS=soaks."]
fn test_stress_speed_cycling() {
    let speeds = [4800, 9600, 14400, 28800];
    let rounds = if run_long_soaks() { 30 } else { 4 };

    let mut pair = ModemPair {
        caller: ModemDsp::new(ModemConfig {
            max_speed: speeds[0],
            ..Default::default()
        }),
        answerer: ModemDsp::new(ModemConfig {
            max_speed: speeds[0],
            ..Default::default()
        }),
        caller_wb: WireBuffer::new(),
        answerer_wb: WireBuffer::new(),
        caller_out: [0i16; CHUNK_SAMPLES],
        answerer_out: [0i16; CHUNK_SAMPLES],
        bytes_c2a: 0,
        bytes_a2c: 0,
        handshakes: 0,
    };
    pair.do_handshake(speeds[0]);

    for round in 0..rounds {
        let speed = speeds[round % speeds.len()];
        pair.transfer_c2a(256);
        pair.transfer_a2c(256);

        pair.abrupt_disconnect();
        pair.reconnect(speed);
    }
    eprintln!(
        "  {rounds} speed-change cycles OK ({} handshakes, {:.1} KB transferred)",
        pair.handshakes,
        (pair.bytes_c2a + pair.bytes_a2c) as f64 / 1024.0
    );
}
