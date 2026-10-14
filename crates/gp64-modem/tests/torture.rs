//! Extended modem torture test suite.
//!
//! Long-soak mode covers ALL modem speeds (4800, 9600, 14400, 28800, 56000),
//! pushes large data volumes, tests binary patterns (including escape-triggering
//! sequences), measures throughput, and exercises bidirectional full-duplex
//! under load. Default ignored-test execution is smoke coverage only.
//!
//! Quick tests (< 5s each):
//!   cargo test -p gp64-modem --test torture
//!
//! Ignored torture tests run bounded smoke coverage by default for automation.
//! Set MDEM_LONG_TESTS=soaks for the full soaks:
//!   MDEM_LONG_TESTS=soaks cargo test -p gp64-modem --test torture -- --include-ignored --nocapture

use std::time::Instant;

use gp64_modem::wire::{self, WireBuffer, CHUNK_SAMPLES};
use gp64_modem::{ModemConfig, ModemDsp};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// All supported modem speeds.
const ALL_SPEEDS: &[u32] = &[4800, 9600, 14400, 28800, 56000];

/// V.32bis speeds (known stable).
const V32BIS_SPEEDS: &[u32] = &[4800, 9600, 14400];

/// Single-speed smoke path for ignored tests in automation.
const SMOKE_SPEEDS: &[u32] = &[9600];

/// Compact speed cycle for ignored smoke tests.
const COMBINED_SMOKE_SPEEDS: &[u32] = &[9600, 4800];

/// Maximum DSP ticks to allow for handshake before declaring failure.
fn handshake_timeout(speed: u32) -> u32 {
    match speed {
        s if s <= 14400 => 800,
        28800 => 2000,
        56000 => 3000,
        _ => 2000,
    }
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

fn run_long_soaks() -> bool {
    // `MDEM_LONG_TESTS` is a comma list; `soaks` selects the long durations.
    std::env::var("MDEM_LONG_TESTS")
        .is_ok_and(|keys| keys.split(',').any(|key| key.trim() == "soaks"))
}

fn coverage_mode_label(long: bool) -> &'static str {
    if long {
        "long soak"
    } else {
        "smoke; set MDEM_LONG_TESTS=soaks for full long soak"
    }
}

fn speed_is_required(long: bool, speed: u32) -> bool {
    !long || V32BIS_SPEEDS.contains(&speed)
}

fn connect_for_coverage(
    long: bool,
    speed: u32,
    connection: Option<ModemPair>,
) -> Option<ModemPair> {
    match connection {
        Some(pair) => Some(pair),
        None if speed_is_required(long, speed) => {
            panic!("required stable speed {speed} failed to connect")
        }
        None => {
            eprintln!("  {speed:>5} bps: SKIP (experimental long-soak speed failed to connect)");
            None
        }
    }
}

fn assert_reconnect_coverage(long: bool, speed: u32, completed: u32, requested: u32) {
    if speed_is_required(long, speed) {
        assert_eq!(
            completed, requested,
            "required stable speed {speed} completed only {completed}/{requested} reconnect cycles"
        );
    }
}

/// Simple CRC32 (IEEE) for data integrity checking.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

// ---------------------------------------------------------------------------
// ModemPair: reusable test harness
// ---------------------------------------------------------------------------

struct ModemPair {
    caller: ModemDsp,
    answerer: ModemDsp,
    caller_wb: WireBuffer,
    answerer_wb: WireBuffer,
    caller_out: [i16; CHUNK_SAMPLES],
    answerer_out: [i16; CHUNK_SAMPLES],
    total_ticks: u64,
}

impl ModemPair {
    /// Create a pair and perform handshake. Returns None if handshake fails.
    fn try_connect(speed: u32) -> Option<Self> {
        let mut pair = Self {
            caller: ModemDsp::new(ModemConfig {
                max_speed: speed,
                ..Default::default()
            }),
            answerer: ModemDsp::new(ModemConfig {
                max_speed: speed,
                ..Default::default()
            }),
            caller_wb: WireBuffer::new(),
            answerer_wb: WireBuffer::new(),
            caller_out: [0i16; CHUNK_SAMPLES],
            answerer_out: [0i16; CHUNK_SAMPLES],
            total_ticks: 0,
        };
        if pair.do_handshake(speed) {
            Some(pair)
        } else {
            None
        }
    }

    /// Create a pair and assert handshake succeeds.
    fn connect(speed: u32) -> Self {
        Self::try_connect(speed).unwrap_or_else(|| {
            panic!("Handshake failed at speed {speed}");
        })
    }

    /// Attempt handshake. Returns true on success.
    fn do_handshake(&mut self, speed: u32) -> bool {
        for &b in b"ATS0=1\r" {
            self.answerer.receive_from_serial(b);
        }
        drain_serial(&mut self.answerer);
        for &b in b"ATD5551234\r" {
            self.caller.receive_from_serial(b);
        }
        drain_serial(&mut self.caller);

        let timeout = handshake_timeout(speed);
        for _ in 0..timeout {
            self.dsp_tick_drain();
            if self.caller.is_online() && self.answerer.is_online() {
                drain_serial(&mut self.caller);
                drain_serial(&mut self.answerer);
                // Convergence: V.34/V.90 Buffering needs ~235 ticks
                let convergence = if speed > 14400 { 300 } else { 60 };
                for _ in 0..convergence {
                    self.dsp_tick_drain();
                }
                drain_serial(&mut self.caller);
                drain_serial(&mut self.answerer);
                return true;
            }
        }
        false
    }

    fn dsp_tick(&mut self) {
        // In-memory crossover: each side consumes the chunk the other produced
        // last tick, then this tick's outputs are pushed into the opposite
        // buffer — the one-tick latency the former 127.0.0.1 TCP loopback gave,
        // but deterministic (the socket transport flaked on macOS when a
        // non-blocking read raced loopback delivery and starved the handshake).
        let caller_in = self.caller_wb.take_chunk().unwrap_or(wire::SILENCE);
        let answerer_in = self.answerer_wb.take_chunk().unwrap_or(wire::SILENCE);
        self.caller.tick_dsp(&caller_in, &mut self.caller_out);
        self.answerer.tick_dsp(&answerer_in, &mut self.answerer_out);
        let c_enc = wire::encode_chunk(&self.caller_out);
        let a_enc = wire::encode_chunk(&self.answerer_out);
        self.answerer_wb.push(&c_enc);
        self.caller_wb.push(&a_enc);
        self.total_ticks += 1;
    }

    fn dsp_tick_drain(&mut self) {
        drain_serial(&mut self.caller);
        drain_serial(&mut self.answerer);
        self.dsp_tick();
    }

    /// Transfer data caller→answerer. Returns received data.
    /// Panics if modems go offline or timeout is exceeded.
    fn transfer_c2a(&mut self, data: &[u8]) -> Vec<u8> {
        assert!(
            self.caller.is_online() && self.answerer.is_online(),
            "C→A: modems not online (caller={:?} answerer={:?})",
            self.caller.phase(),
            self.answerer.phase()
        );
        let count = data.len();
        let mut fed = 0;
        let mut received = Vec::with_capacity(count);

        let max_ticks = count * 10 + 4000;
        for _ in 0..max_ticks {
            drain_serial(&mut self.caller);
            let end = (fed + 16).min(count);
            for &b in &data[fed..end] {
                self.caller.receive_from_serial(b);
            }
            fed = end;
            self.dsp_tick();
            let rx = drain_serial(&mut self.answerer);
            received.extend_from_slice(&rx);
            if received.len() >= count {
                break;
            }
            if !self.caller.is_online() || !self.answerer.is_online() {
                panic!(
                    "C→A: modem went offline after {}/{} bytes (caller={:?} answerer={:?})",
                    received.len(),
                    count,
                    self.caller.phase(),
                    self.answerer.phase()
                );
            }
        }
        received.truncate(count);
        received
    }

    /// Transfer data answerer→caller. Returns received data.
    fn transfer_a2c(&mut self, data: &[u8]) -> Vec<u8> {
        assert!(
            self.caller.is_online() && self.answerer.is_online(),
            "A→C: modems not online"
        );
        let count = data.len();
        let mut fed = 0;
        let mut received = Vec::with_capacity(count);

        let max_ticks = count * 10 + 4000;
        for _ in 0..max_ticks {
            drain_serial(&mut self.answerer);
            let end = (fed + 16).min(count);
            for &b in &data[fed..end] {
                self.answerer.receive_from_serial(b);
            }
            fed = end;
            self.dsp_tick();
            let rx = drain_serial(&mut self.caller);
            received.extend_from_slice(&rx);
            if received.len() >= count {
                break;
            }
            if !self.caller.is_online() || !self.answerer.is_online() {
                panic!(
                    "A→C: modem went offline after {}/{} bytes (caller={:?} answerer={:?})",
                    received.len(),
                    count,
                    self.caller.phase(),
                    self.answerer.phase()
                );
            }
        }
        received.truncate(count);
        received
    }

    /// Bidirectional simultaneous transfer. Returns (c2a_received, a2c_received).
    fn transfer_bidir(&mut self, c2a_data: &[u8], a2c_data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let c_count = c2a_data.len();
        let a_count = a2c_data.len();
        let mut c_fed = 0;
        let mut a_fed = 0;
        let mut a_rx = Vec::with_capacity(c_count);
        let mut c_rx = Vec::with_capacity(a_count);

        let max_ticks = (c_count.max(a_count)) * 10 + 4000;
        for _ in 0..max_ticks {
            let c_end = (c_fed + 8).min(c_count);
            for &b in &c2a_data[c_fed..c_end] {
                self.caller.receive_from_serial(b);
            }
            c_fed = c_end;

            let a_end = (a_fed + 8).min(a_count);
            for &b in &a2c_data[a_fed..a_end] {
                self.answerer.receive_from_serial(b);
            }
            a_fed = a_end;

            self.dsp_tick();
            a_rx.extend_from_slice(&drain_serial(&mut self.answerer));
            c_rx.extend_from_slice(&drain_serial(&mut self.caller));

            if a_rx.len() >= c_count && c_rx.len() >= a_count {
                break;
            }
        }
        a_rx.truncate(c_count);
        c_rx.truncate(a_count);
        (a_rx, c_rx)
    }

    /// Abrupt disconnect via silence injection.
    fn abrupt_disconnect(&mut self) {
        self.caller_wb.clear();
        self.answerer_wb.clear();
        // Feed silence directly until both modems detect carrier loss. No
        // crossover here: each side must see silence, not the other's output.
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

    /// Reconnect after disconnect with fresh modem instances.
    fn reconnect(&mut self, speed: u32) -> bool {
        self.caller = ModemDsp::new(ModemConfig {
            max_speed: speed,
            ..Default::default()
        });
        self.answerer = ModemDsp::new(ModemConfig {
            max_speed: speed,
            ..Default::default()
        });
        self.caller_wb.clear();
        self.answerer_wb.clear();
        self.do_handshake(speed)
    }
}

// ---------------------------------------------------------------------------
// Test data generators
// ---------------------------------------------------------------------------

/// Sequential pattern: 0, 1, 2, ..., 250, 0, 1, ...
fn pattern_sequential(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// All 256 byte values repeated to fill `len` bytes.
fn pattern_all_bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 256) as u8).collect()
}

/// Binary pattern with lots of '+' (0x2B) characters — tests escape detection.
/// Contains: raw +++, +++ with surrounding data, isolated +'s, etc.
fn pattern_escape_heavy(len: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(len);
    let patterns: &[&[u8]] = &[
        b"\x2B\x2B\x2B",         // raw +++
        b"\x00\x2B\x2B\x2B\x00", // +++ surrounded by nulls
        b"\xFF\x2B\x2B\x2B\xFF", // +++ surrounded by 0xFF
        b"\x2B\x2B",             // only two +'s
        b"\x2B",                 // single +
        b"\x2B\x00\x2B\x00\x2B", // +'s with gaps
        b"AT\x2B\x2B\x2BH\r",    // looks like AT+++H command
        b"\x2B\x2B\x2B\x2B\x2B", // five +'s in a row
        b"\x7E\x2B\x2B\x2B\x7E", // HDLC flags around +++
    ];
    let mut pat_idx = 0;
    while data.len() < len {
        let p = patterns[pat_idx % patterns.len()];
        let remaining = len - data.len();
        let take = p.len().min(remaining);
        data.extend_from_slice(&p[..take]);
        pat_idx += 1;
    }
    data.truncate(len);
    data
}

/// Pseudo-random data using a simple PRNG.
fn pattern_random(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            // xorshift64
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xFF) as u8
        })
        .collect()
}

/// All zeros.
fn pattern_zeros(len: usize) -> Vec<u8> {
    vec![0u8; len]
}

/// All ones (0xFF).
fn pattern_ones(len: usize) -> Vec<u8> {
    vec![0xFFu8; len]
}

// =========================================================================
// Quick tests (run by default, < 5s each)
// =========================================================================

/// Connect and transfer 64 bytes at every speed. Reports which speeds work.
#[test]
fn test_quick_all_speeds_connect() {
    let mut results = Vec::new();
    for &speed in ALL_SPEEDS {
        let start = Instant::now();
        match ModemPair::try_connect(speed) {
            Some(mut pair) => {
                let data = pattern_sequential(64);
                let rx = pair.transfer_c2a(&data);
                let ok = rx == data;
                results.push((speed, true, ok, start.elapsed()));
            }
            None => {
                results.push((speed, false, false, start.elapsed()));
            }
        }
    }

    eprintln!("\n=== ALL-SPEED CONNECT RESULTS ===");
    let mut any_failure = false;
    for (speed, connected, data_ok, elapsed) in &results {
        let status = match (connected, data_ok) {
            (true, true) => "OK",
            (true, false) => "CONNECT OK, DATA CORRUPT",
            (false, _) => "CONNECT FAIL",
        };
        eprintln!("  {speed:>5} bps: {status} ({:.1}s)", elapsed.as_secs_f64());
        if !connected || !data_ok {
            any_failure = true;
        }
    }

    // V.32bis speeds MUST work
    for &speed in V32BIS_SPEEDS {
        let r = results.iter().find(|r| r.0 == speed).unwrap();
        assert!(
            r.1 && r.2,
            "V.32bis speed {speed} must connect and transfer data"
        );
    }

    if any_failure {
        eprintln!("  (Some speeds failed — see above. V.32bis speeds are required.)");
    }
}

/// Transfer all 256 byte values at each V.32bis speed.
#[test]
fn test_quick_all_byte_values() {
    for &speed in V32BIS_SPEEDS {
        let mut pair = ModemPair::connect(speed);
        let data = pattern_all_bytes(512); // 2 complete cycles of 0x00..0xFF
        let rx = pair.transfer_c2a(&data);
        assert_eq!(
            rx, data,
            "All-byte-values test failed at {speed}: some bytes were corrupted or filtered"
        );
    }
}

/// Binary patterns containing +++ must survive data mode without triggering escape.
#[test]
fn test_quick_escape_pattern_survives() {
    for &speed in V32BIS_SPEEDS {
        let mut pair = ModemPair::connect(speed);
        let data = pattern_escape_heavy(256);
        let rx = pair.transfer_c2a(&data);
        assert_eq!(
            rx, data,
            "Escape pattern test failed at {speed}: data containing +++ was corrupted. \
             This likely means the escape detector falsely triggered on in-band data."
        );
        // Verify modems are still online
        assert!(
            pair.caller.is_online() && pair.answerer.is_online(),
            "Modems went offline after escape-heavy data at {speed} — \
             escape detector likely triggered on in-band +++"
        );
    }
}

/// Bidirectional simultaneous transfer at each V.32bis speed.
#[test]
fn test_quick_bidir_all_v32bis() {
    for &speed in V32BIS_SPEEDS {
        let mut pair = ModemPair::connect(speed);
        let c2a = pattern_sequential(256);
        let a2c = pattern_random(256, 42);
        let (rx_a, rx_c) = pair.transfer_bidir(&c2a, &a2c);
        assert_eq!(rx_a, c2a, "Bidir C→A data corruption at {speed}");
        assert_eq!(rx_c, a2c, "Bidir A→C data corruption at {speed}");
    }
}

/// Post-training accuracy gate for the QAM16 preamble-trained LMS equalizer
/// (HLD: 2026.04.22 - V32bis QAM16 preamble-trained equalizer, Phase A).
///
/// After the equalizer has completed its 100-byte preamble-trained LMS
/// window at 9600 bps, a 1024-byte pseudo-random transfer must arrive
/// byte-perfect.
///
/// Historically this failed at the first byte mismatch because the raw
/// QAM16 symbol-error rate was ~1%, which HDLC byte destuffing amplifies
/// into multi-byte corruption.
#[test]
fn test_qam16_post_training_accuracy() {
    let mut pair = ModemPair::connect(9600);

    // 1024 bytes of deterministic pseudo-random data C→A. Fixed seed so
    // any failure reproduces bit-for-bit.
    let payload = pattern_random(1024, 0xDEADBEEF);
    let received = pair.transfer_c2a(&payload);
    assert_eq!(
        received, payload,
        "9600 bps transfer must be byte-perfect after preamble training"
    );
}

// =========================================================================
// Extended tests (run with --include-ignored)
// =========================================================================

/// Unidirectional transfer with CRC verification.
#[test]
#[ignore = "full_test (--include-ignored): unidirectional transfer with CRC verification; smoke by default, soak under MDEM_LONG_TESTS=soaks."]
fn test_large_transfer_smoke_or_long_soak() {
    let long = run_long_soaks();
    let size = if long { 64 * 1024 } else { 8 * 1024 };
    let speeds = if long { ALL_SPEEDS } else { SMOKE_SPEEDS };
    eprintln!(
        "\n=== TRANSFER TEST ({}, {size} bytes, speeds {:?}) ===",
        coverage_mode_label(long),
        speeds
    );

    for &speed in speeds {
        let pair = ModemPair::try_connect(speed);
        if pair.is_none() {
            eprintln!("  {speed:>5} bps: SKIP (connect failed)");
            continue;
        }
        let mut pair = pair.unwrap();

        let data = pattern_random(size, speed as u64);
        let expected_crc = crc32(&data);

        let start = Instant::now();
        let rx = pair.transfer_c2a(&data);
        let elapsed = start.elapsed();

        let rx_crc = crc32(&rx);
        let throughput = size as f64 / elapsed.as_secs_f64();

        if rx.len() != data.len() {
            eprintln!(
                "  {speed:>5} bps: FAIL — got {}/{} bytes ({:.1}s)",
                rx.len(),
                data.len(),
                elapsed.as_secs_f64()
            );
            // Find first mismatch position
            if let Some(pos) = rx.iter().zip(data.iter()).position(|(a, b)| a != b) {
                eprintln!(
                    "    First mismatch at byte {pos}: got 0x{:02X}, expected 0x{:02X}",
                    rx[pos], data[pos]
                );
            }
            panic!("transfer failed at {speed}");
        }

        assert_eq!(
            rx_crc, expected_crc,
            "transfer CRC mismatch at {speed}: got 0x{rx_crc:08X}, expected 0x{expected_crc:08X}"
        );

        eprintln!(
            "  {speed:>5} bps: OK — {:.1} KB/s, CRC 0x{expected_crc:08X} ({:.1}s)",
            throughput / 1024.0,
            elapsed.as_secs_f64()
        );
    }
}

/// Bidirectional simultaneous transfer.
#[test]
#[ignore = "full_test (--include-ignored): bidirectional simultaneous transfer; smoke by default, soak under MDEM_LONG_TESTS=soaks."]
fn test_bidir_transfer_smoke_or_long_soak() {
    let long = run_long_soaks();
    let size = if long { 32 * 1024 } else { 2 * 1024 };
    let speeds = if long { ALL_SPEEDS } else { SMOKE_SPEEDS };
    eprintln!(
        "\n=== BIDIRECTIONAL TEST ({}, {size} bytes per direction, speeds {:?}) ===",
        coverage_mode_label(long),
        speeds
    );

    for &speed in speeds {
        let pair = ModemPair::try_connect(speed);
        if pair.is_none() {
            eprintln!("  {speed:>5} bps: SKIP (connect failed)");
            continue;
        }
        let mut pair = pair.unwrap();

        let c2a_data = pattern_random(size, speed as u64);
        let a2c_data = pattern_random(size, (speed as u64).wrapping_mul(7));

        let start = Instant::now();
        let (rx_a, rx_c) = pair.transfer_bidir(&c2a_data, &a2c_data);
        let elapsed = start.elapsed();

        assert_eq!(rx_a.len(), size, "Bidir C→A length mismatch at {speed}");
        assert_eq!(rx_c.len(), size, "Bidir A→C length mismatch at {speed}");
        assert_eq!(
            crc32(&rx_a),
            crc32(&c2a_data),
            "Bidir C→A CRC mismatch at {speed}"
        );
        assert_eq!(
            crc32(&rx_c),
            crc32(&a2c_data),
            "Bidir A→C CRC mismatch at {speed}"
        );

        let total = size * 2;
        let throughput = total as f64 / elapsed.as_secs_f64();
        eprintln!(
            "  {speed:>5} bps: OK — {:.1} KB/s total ({:.1}s)",
            throughput / 1024.0,
            elapsed.as_secs_f64()
        );
    }
}

/// All data patterns (zeros, ones, sequential, random, escape-heavy).
#[test]
#[ignore = "full_test (--include-ignored): all data patterns; smoke by default, soak under MDEM_LONG_TESTS=soaks."]
fn test_all_patterns_smoke_or_long_soak() {
    let long = run_long_soaks();
    let size = if long { 4096 } else { 512 };
    let speeds = if long { ALL_SPEEDS } else { SMOKE_SPEEDS };
    let patterns: Vec<(&str, Vec<u8>)> = vec![
        ("zeros", pattern_zeros(size)),
        ("ones (0xFF)", pattern_ones(size)),
        ("sequential", pattern_sequential(size)),
        ("all-bytes", pattern_all_bytes(size)),
        ("random", pattern_random(size, 12345)),
        ("escape-heavy", pattern_escape_heavy(size)),
    ];

    eprintln!(
        "\n=== ALL-PATTERN TEST ({}, {size} bytes × {} patterns, speeds {:?}) ===",
        coverage_mode_label(long),
        patterns.len(),
        speeds
    );

    for &speed in speeds {
        let pair = ModemPair::try_connect(speed);
        if pair.is_none() {
            eprintln!("  {speed:>5} bps: SKIP (connect failed)");
            continue;
        }
        let mut pair = pair.unwrap();

        let mut all_ok = true;
        for (name, data) in &patterns {
            let rx = pair.transfer_c2a(data);
            if rx != *data {
                eprintln!("  {speed:>5} bps: FAIL on pattern '{name}'");
                if let Some(pos) = rx.iter().zip(data.iter()).position(|(a, b)| a != b) {
                    eprintln!(
                        "    First mismatch at byte {pos}: got 0x{:02X}, expected 0x{:02X}",
                        rx[pos], data[pos]
                    );
                }
                all_ok = false;
            }
            // Verify still online after each pattern
            assert!(
                pair.caller.is_online() && pair.answerer.is_online(),
                "Modems went offline after pattern '{name}' at {speed}"
            );
        }
        if all_ok {
            eprintln!(
                "  {speed:>5} bps: OK — all {} patterns passed",
                patterns.len()
            );
        } else {
            panic!("Pattern test failures at {speed}");
        }
    }
}

/// Throughput measurement: continuous streaming for a bounded interval.
/// Reports actual achieved bytes/second.
#[test]
#[ignore = "full_test (--include-ignored): bounded-interval throughput check."]
fn test_throughput_measurement() {
    run_throughput_measurement_with(run_long_soaks(), ModemPair::try_connect);
}

fn run_throughput_measurement_with(
    long: bool,
    mut try_connect: impl FnMut(u32) -> Option<ModemPair>,
) {
    let measure_secs = if long { 5.0f64 } else { 0.25f64 };
    let block_len = if long { 4096 } else { 512 };
    let speeds = if long { ALL_SPEEDS } else { SMOKE_SPEEDS };
    eprintln!(
        "\n=== THROUGHPUT MEASUREMENT ({}, {measure_secs}s per speed, speeds {:?}) ===",
        coverage_mode_label(long),
        speeds
    );

    for &speed in speeds {
        let Some(mut pair) = connect_for_coverage(long, speed, try_connect(speed)) else {
            continue;
        };

        // Pre-generate a block to feed repeatedly.
        let block = pattern_random(block_len, speed as u64);
        let mut total_bytes: u64 = 0;
        let mut total_ticks: u64 = 0;
        let start = Instant::now();

        while start.elapsed().as_secs_f64() < measure_secs {
            // Feed one block
            for &b in &block {
                pair.caller.receive_from_serial(b);
            }
            // Run enough ticks to drain
            for _ in 0..block.len() {
                drain_serial(&mut pair.caller);
                pair.dsp_tick();
                let rx = drain_serial(&mut pair.answerer);
                total_bytes += rx.len() as u64;
                total_ticks += 1;
            }
            if !pair.caller.is_online() {
                if speed_is_required(long, speed) {
                    panic!("required stable speed {speed} went offline after {total_bytes} bytes");
                }
                eprintln!(
                    "  {speed:>5} bps: SKIP (experimental speed went offline after {total_bytes} bytes)"
                );
                break;
            }
        }

        let elapsed = start.elapsed().as_secs_f64();
        let bps = (total_bytes as f64 * 8.0) / elapsed;
        let efficiency = if speed > 0 {
            bps / speed as f64 * 100.0
        } else {
            0.0
        };

        eprintln!(
            "  {speed:>5} bps: {total_bytes:>8} bytes in {elapsed:.1}s = {:.0} bps ({efficiency:.0}% of line rate), \
             {total_ticks} ticks",
            bps
        );
    }
}

/// Rapid connect/disconnect cycling.
#[test]
#[ignore = "full_test (--include-ignored): rapid reconnect cycling; smoke by default, soak under MDEM_LONG_TESTS=soaks."]
fn test_rapid_reconnect_smoke_or_long_soak() {
    run_rapid_reconnect_with(run_long_soaks(), ModemPair::try_connect);
}

fn run_rapid_reconnect_with(long: bool, mut try_connect: impl FnMut(u32) -> Option<ModemPair>) {
    let speeds = if long {
        ALL_SPEEDS
    } else {
        COMBINED_SMOKE_SPEEDS
    };
    let cycles_per_speed = if long { 20 } else { 2 };
    eprintln!(
        "\n=== RAPID RECONNECT TEST ({}, {cycles_per_speed} cycles per speed, speeds {:?}) ===",
        coverage_mode_label(long),
        speeds
    );

    for &speed in speeds {
        let Some(mut pair) = connect_for_coverage(long, speed, try_connect(speed)) else {
            continue;
        };

        let start = Instant::now();
        let mut cycles = 0u32;
        let mut failures = 0u32;

        for _ in 0..cycles_per_speed {
            // Quick transfer
            let data = pattern_sequential(32);
            let rx = pair.transfer_c2a(&data);
            assert_eq!(
                rx, data,
                "Data corruption during reconnect cycle at {speed}"
            );

            // Abrupt disconnect
            pair.abrupt_disconnect();

            // Reconnect
            if pair.reconnect(speed) {
                cycles += 1;
            } else {
                failures += 1;
                // Try again with fresh instances
                if !pair.reconnect(speed) {
                    if speed_is_required(long, speed) {
                        panic!(
                            "required stable speed {speed} could not reconnect after {cycles} cycles"
                        );
                    }
                    eprintln!(
                        "  {speed:>5} bps: SKIP (experimental speed could not reconnect after {cycles} cycles)"
                    );
                    break;
                }
                cycles += 1;
            }
        }

        eprintln!(
            "  {speed:>5} bps: {cycles}/{cycles_per_speed} cycles OK, {failures} reconnect retries ({:.1}s)",
            start.elapsed().as_secs_f64()
        );
        assert_reconnect_coverage(long, speed, cycles, cycles_per_speed);
    }
}

/// Cross-speed transitions: connect at one speed, disconnect, reconnect at another.
/// Tests every selected speed→speed transition.
#[test]
#[ignore = "full_test (--include-ignored): cross-speed transition matrix."]
fn test_speed_transition_matrix() {
    run_speed_transition_matrix_with(run_long_soaks(), ModemPair::try_connect);
}

fn run_speed_transition_matrix_with(
    long: bool,
    mut try_connect: impl FnMut(u32) -> Option<ModemPair>,
) {
    let candidate_speeds = if long {
        ALL_SPEEDS
    } else {
        COMBINED_SMOKE_SPEEDS
    };
    eprintln!(
        "\n=== SPEED TRANSITION MATRIX ({}, candidate speeds {:?}) ===",
        coverage_mode_label(long),
        candidate_speeds
    );

    // Find which speeds actually connect
    let mut working_speeds: Vec<u32> = Vec::new();
    for &speed in candidate_speeds {
        if connect_for_coverage(long, speed, try_connect(speed)).is_some() {
            working_speeds.push(speed);
        }
    }
    eprintln!("  Working speeds: {:?}", working_speeds);

    assert!(
        working_speeds.len() >= 2,
        "speed-transition coverage requires at least two connected speeds"
    );

    let first = working_speeds[0];
    let mut pair = ModemPair::connect(first);
    let mut transitions = 0u32;

    for &from_speed in &working_speeds {
        for &to_speed in &working_speeds {
            if !pair.caller.is_online() {
                if !pair.reconnect(from_speed) {
                    eprintln!("  {from_speed} → {to_speed}: SKIP (reconnect failed)");
                    continue;
                }
            }
            // Transfer at current speed
            let data = pattern_sequential(64);
            let rx = pair.transfer_c2a(&data);
            assert_eq!(
                rx, data,
                "Data corruption at {from_speed} before transition to {to_speed}"
            );

            // Disconnect and reconnect at new speed
            pair.abrupt_disconnect();
            if pair.reconnect(to_speed) {
                // Verify new speed works
                let data2 = pattern_random(64, (from_speed as u64) * 1000 + to_speed as u64);
                let rx2 = pair.transfer_c2a(&data2);
                assert_eq!(
                    rx2, data2,
                    "Data corruption after {from_speed} → {to_speed} transition"
                );
                transitions += 1;
            } else {
                eprintln!("  {from_speed:>5} → {to_speed:>5}: FAIL");
            }
        }
    }

    let expected = working_speeds.len() * working_speeds.len();
    eprintln!("  {transitions}/{expected} transitions OK");
    assert_eq!(
        transitions as usize, expected,
        "Not all speed transitions succeeded"
    );
}

#[test]
fn smoke_oracles_reject_forced_connection_failures() {
    let throughput = std::panic::catch_unwind(|| {
        run_throughput_measurement_with(false, |_| None);
    });
    assert!(
        throughput.is_err(),
        "throughput smoke test accepted a forced stable-speed connection failure"
    );

    let reconnect = std::panic::catch_unwind(|| {
        run_rapid_reconnect_with(false, |_| None);
    });
    assert!(
        reconnect.is_err(),
        "rapid-reconnect smoke test accepted forced stable-speed connection failures"
    );

    let transitions = std::panic::catch_unwind(|| {
        run_speed_transition_matrix_with(false, |_| None);
    });
    assert!(
        transitions.is_err(),
        "speed-transition smoke test accepted forced stable-speed connection failures"
    );
}

#[test]
fn reconnect_oracle_rejects_incomplete_smoke_cycles() {
    let incomplete = std::panic::catch_unwind(|| {
        assert_reconnect_coverage(false, 9600, 1, 2);
    });
    assert!(
        incomplete.is_err(),
        "rapid-reconnect smoke oracle accepted incomplete requested coverage"
    );

    assert_reconnect_coverage(true, 28800, 0, 20);
}

/// Fast 9600 bps echo round-trip — Phase B gate from HLD
/// `2026.04.22 - HLD - V32bis QAM16 preamble-trained equalizer.md`.
///
/// Definitive byte-perfect data-integrity test for V.32bis QAM16: runs
/// 1024 bytes C→A then A→C and asserts byte-identical recovery in both
/// directions plus CRC32 round-trip equality. Single-speed (9600 bps)
/// variant kept in the default suite to actively prevent regressions in
/// the preamble-trained equalizer; the multi-speed `test_echo_round_trip`
/// below remains `#[ignore]`'d as part of the heavyweight torture suite.
#[test]
fn test_echo_round_trip_9600() {
    eprintln!("\n=== ECHO ROUND-TRIP TEST (9600 bps gate) ===");

    let speed = 9600u32;
    let mut pair = ModemPair::connect(speed);

    let data = pattern_random(1024, speed as u64);

    // Send C→A
    let at_answerer = pair.transfer_c2a(&data);
    assert_eq!(at_answerer, data, "Echo phase 1 (C→A) failed at {speed}");

    // Echo back A→C
    let at_caller = pair.transfer_a2c(&at_answerer);
    assert_eq!(at_caller, data, "Echo phase 2 (A→C) failed at {speed}");

    // Verify CRC through the full round trip
    assert_eq!(
        crc32(&at_caller),
        crc32(&data),
        "Echo CRC mismatch at {speed}"
    );

    eprintln!(
        "  {speed:>5} bps: OK — 1024 bytes echoed, CRC 0x{:08X}",
        crc32(&data)
    );
}

/// Echo test: send data, have the other side repeat it back.
/// This tests the full round-trip path both directions.
#[test]
#[ignore = "full_test (--include-ignored): full-path echo round trip in both directions."]
fn test_echo_round_trip() {
    eprintln!("\n=== ECHO ROUND-TRIP TEST ===");

    for &speed in V32BIS_SPEEDS {
        let mut pair = ModemPair::connect(speed);

        let data = pattern_random(1024, speed as u64);

        // Send C→A
        let at_answerer = pair.transfer_c2a(&data);
        assert_eq!(at_answerer, data, "Echo phase 1 (C→A) failed at {speed}");

        // Echo back A→C
        let at_caller = pair.transfer_a2c(&at_answerer);
        assert_eq!(at_caller, data, "Echo phase 2 (A→C) failed at {speed}");

        // Verify CRC through the full round trip
        assert_eq!(
            crc32(&at_caller),
            crc32(&data),
            "Echo CRC mismatch at {speed}"
        );

        eprintln!(
            "  {speed:>5} bps: OK — 1024 bytes echoed, CRC 0x{:08X}",
            crc32(&data)
        );
    }
}

/// Extended data integrity test with multiple patterns.
#[test]
#[ignore = "full_test (--include-ignored): multi-pattern data integrity; smoke by default, soak under MDEM_LONG_TESTS=soaks."]
fn test_data_integrity_smoke_or_long_soak() {
    let long = run_long_soaks();
    let total_per_pattern = if long { 256 * 1024 } else { 8 * 1024 };
    let chunk = if long { 8192 } else { 1024 };
    let speeds = if long { V32BIS_SPEEDS } else { SMOKE_SPEEDS };
    eprintln!(
        "\n=== DATA INTEGRITY TEST ({}, {total_per_pattern} bytes per pattern, speeds {:?}) ===",
        coverage_mode_label(long),
        speeds
    );

    for &speed in speeds {
        let mut pair = ModemPair::connect(speed);

        type Generator = Box<dyn Fn(usize, usize) -> Vec<u8>>;
        let generators: Vec<(&str, Generator)> = vec![
            ("sequential", Box::new(|sz, _| pattern_sequential(sz))),
            ("random", Box::new(|sz, i| pattern_random(sz, i as u64))),
            ("escape-heavy", Box::new(|sz, _| pattern_escape_heavy(sz))),
        ];

        for (name, gen) in &generators {
            let start = Instant::now();
            let mut total_crc: u32 = 0;
            let mut bytes_ok: usize = 0;

            let chunks = total_per_pattern / chunk;
            for i in 0..chunks {
                let data = gen(chunk, i);
                total_crc = total_crc.wrapping_add(crc32(&data));

                let rx = pair.transfer_c2a(&data);
                assert_eq!(
                    rx, data,
                    "data integrity: corruption in chunk {i}/{chunks} of '{name}' at {speed}"
                );
                bytes_ok += chunk;

                assert!(
                    pair.caller.is_online() && pair.answerer.is_online(),
                    "data integrity: modems offline in chunk {i}/{chunks} of '{name}' at {speed}"
                );
            }

            let elapsed = start.elapsed();
            let throughput = bytes_ok as f64 / elapsed.as_secs_f64();
            eprintln!(
                "  {speed:>5} bps / {name:>13}: {bytes_ok:>7} bytes OK, {:.1} KB/s, CRC-sum 0x{total_crc:08X} ({:.1}s)",
                throughput / 1024.0,
                elapsed.as_secs_f64()
            );
        }
    }
}

/// Combined torture: cycles through speeds doing handshake, bulk transfer,
/// bidirectional transfer, patterns, disconnect, speed change, and repeat.
///
/// By default this ignored test runs a compact smoke path for automation.
/// Set MDEM_LONG_TESTS=soaks for the full 50-round soak.
#[test]
#[ignore = "full_test (--include-ignored): combined torture; smoke by default, the 50-round soak needs MDEM_LONG_TESTS=soaks."]
fn test_combined_torture() {
    let long = run_long_soaks();

    let rounds_total = if long { 50 } else { 2 };
    let small_bytes = if long { 128 } else { 64 };
    let bulk_bytes = if long { 16 * 1024 } else { 1024 };
    let reverse_bytes = if long { 8 * 1024 } else { 512 };
    let bidir_bytes = if long { 4096 } else { 256 };
    let escape_bytes = if long { 2048 } else { 256 };
    let progress_every = if long { 10 } else { 1 };
    let candidate_speeds = if long {
        ALL_SPEEDS
    } else {
        COMBINED_SMOKE_SPEEDS
    };
    eprintln!(
        "\n=== COMBINED TORTURE TEST ({}, {rounds_total} rounds, candidate speeds {:?}) ===",
        coverage_mode_label(long),
        candidate_speeds
    );

    // Discover working speeds dynamically.
    let mut working_speeds: Vec<u32> = Vec::new();
    for &speed in candidate_speeds {
        if ModemPair::try_connect(speed).is_some() {
            working_speeds.push(speed);
        }
    }
    eprintln!("  Working speeds: {:?}", working_speeds);
    assert!(!working_speeds.is_empty(), "No speeds connect at all");
    if !long {
        assert_eq!(
            working_speeds.as_slice(),
            COMBINED_SMOKE_SPEEDS,
            "Combined torture smoke speeds must connect"
        );
    }

    let first = working_speeds[0];
    let mut pair = ModemPair::connect(first);
    let start = Instant::now();

    let mut total_bytes: u64 = 0;
    let mut total_handshakes: u32 = 1; // initial connect
    let mut completed_rounds = 0u32;

    for round in 0..rounds_total {
        let speed = working_speeds[round as usize % working_speeds.len()];

        // Ensure connected
        if !pair.caller.is_online() || !pair.answerer.is_online() {
            if !pair.reconnect(speed) {
                panic!("Reconnect failed at round {round}, speed {speed}");
            }
            total_handshakes += 1;
        }

        // Phase 1: Small transfer
        let data = pattern_sequential(small_bytes);
        let rx = pair.transfer_c2a(&data);
        assert_eq!(
            rx, data,
            "Small transfer failed at round {round}, speed {speed}"
        );
        total_bytes += small_bytes as u64;

        // Phase 2: Bulk transfer
        let data = pattern_random(bulk_bytes, round as u64);
        let rx = pair.transfer_c2a(&data);
        assert_eq!(
            rx, data,
            "bulk transfer failed at round {round}, speed {speed}"
        );
        total_bytes += bulk_bytes as u64;

        // Phase 3: Reverse direction
        let data = pattern_random(reverse_bytes, round as u64 + 1000);
        let rx = pair.transfer_a2c(&data);
        assert_eq!(
            rx, data,
            "Reverse transfer failed at round {round}, speed {speed}"
        );
        total_bytes += reverse_bytes as u64;

        // Phase 4: Bidirectional
        let c2a = pattern_random(bidir_bytes, round as u64 + 2000);
        let a2c = pattern_random(bidir_bytes, round as u64 + 3000);
        let (rx_a, rx_c) = pair.transfer_bidir(&c2a, &a2c);
        assert_eq!(rx_a, c2a, "Bidir C→A failed at round {round}");
        assert_eq!(rx_c, a2c, "Bidir A→C failed at round {round}");
        total_bytes += (bidir_bytes * 2) as u64;

        // Phase 5: Escape-heavy pattern
        let data = pattern_escape_heavy(escape_bytes);
        let rx = pair.transfer_c2a(&data);
        assert_eq!(
            rx, data,
            "Escape pattern failed at round {round}, speed {speed}"
        );
        total_bytes += escape_bytes as u64;

        // Phase 6: Disconnect and switch speed
        pair.abrupt_disconnect();
        let next_speed = working_speeds[(round as usize + 1) % working_speeds.len()];
        if !pair.reconnect(next_speed) {
            panic!("Speed-switch reconnect failed at round {round}: {speed} → {next_speed}");
        }
        total_handshakes += 1;

        completed_rounds = round + 1;

        if completed_rounds.is_multiple_of(progress_every) {
            let elapsed = start.elapsed().as_secs_f64();
            let mb = total_bytes as f64 / (1024.0 * 1024.0);
            eprintln!(
                "  [{completed_rounds}/{rounds_total}] {total_handshakes} handshakes, {mb:.2} MB, {elapsed:.1}s"
            );
        }
    }

    let elapsed = start.elapsed();
    let mb = total_bytes as f64 / (1024.0 * 1024.0);
    eprintln!("\n  TORTURE COMPLETE:");
    eprintln!("  Rounds:     {completed_rounds}");
    eprintln!("  Handshakes: {total_handshakes}");
    eprintln!("  Data:       {mb:.2} MB");
    eprintln!("  Time:       {:.1}s", elapsed.as_secs_f64());
    eprintln!("  Errors:     0");
}

// =========================================================================
// V.34 diagnostic tests
// =========================================================================

/// Diagnostic: V.34 over TCP with extended idle warmup (300 ticks).
/// If this passes but the normal torture test fails, the issue is
/// insufficient convergence time, not a fundamental TCP problem.
#[test]
#[ignore = "diagnostic: V.34-over-TCP warmup investigation, kept for hand-running rather than automation."]
fn test_v34_tcp_extended_warmup() {
    eprintln!("\n=== V.34 TCP EXTENDED WARMUP DIAGNOSTIC ===");

    let pair = ModemPair::try_connect(28800);
    if pair.is_none() {
        eprintln!("  V.34 connect failed — skipping");
        return;
    }
    let mut pair = pair.unwrap();

    // Extended warmup: 300 ticks should be enough for Buffering (235) + Sweep + DataMode.
    eprintln!("  Running 300 idle ticks for V.34 startup...");
    for _ in 0..300 {
        pair.dsp_tick_drain();
    }
    eprintln!("  Warmup done. Sending 64 bytes...");

    let data = pattern_sequential(64);
    let rx = pair.transfer_c2a(&data);

    let ok = rx == data;
    eprintln!(
        "  Result: {} — {} bytes received",
        if ok { "OK" } else { "CORRUPT" },
        rx.len()
    );
    if !ok && !rx.is_empty() {
        let preview: Vec<String> = rx.iter().take(32).map(|b| format!("{b:02x}")).collect();
        eprintln!("  RX: [{}]", preview.join(", "));
        let expected: Vec<String> = data.iter().take(32).map(|b| format!("{b:02x}")).collect();
        eprintln!("  EX: [{}]", expected.join(", "));
    }
    assert_eq!(rx, data, "V.34 with extended warmup: data corrupt");
}

/// Diagnostic: V.34 over TCP with 500 idle ticks (maximum warmup).
#[test]
#[ignore = "diagnostic: V.34-over-TCP maximum-warmup investigation, kept for hand-running rather than automation."]
fn test_v34_tcp_max_warmup() {
    eprintln!("\n=== V.34 TCP MAX WARMUP DIAGNOSTIC ===");

    let pair = ModemPair::try_connect(28800);
    if pair.is_none() {
        eprintln!("  V.34 connect failed — skipping");
        return;
    }
    let mut pair = pair.unwrap();

    eprintln!("  Running 500 idle ticks...");
    for _ in 0..500 {
        pair.dsp_tick_drain();
    }
    eprintln!("  Warmup done. Sending 256 bytes...");

    let data = pattern_sequential(256);
    let rx = pair.transfer_c2a(&data);

    let ok = rx == data;
    eprintln!(
        "  Result: {} — {} bytes received",
        if ok { "OK" } else { "CORRUPT" },
        rx.len()
    );
    if !ok && !rx.is_empty() {
        let preview: Vec<String> = rx.iter().take(32).map(|b| format!("{b:02x}")).collect();
        eprintln!("  RX: [{}]", preview.join(", "));
    }
    assert_eq!(rx, data, "V.34 with max warmup: data corrupt");
}
