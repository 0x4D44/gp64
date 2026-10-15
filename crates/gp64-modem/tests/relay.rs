//! Data relay handover on a direct back-to-back line: bytes modulated
//! before the drain, a stuffed byte split across the drain's first tick,
//! and bytes relayed afterwards all reach the peer once and in order.
//!
//! Run with:
//!   cargo test -p gp64-modem --test relay

use gp64_modem::wire::CHUNK_SAMPLES;
use gp64_modem::{ModemConfig, ModemDsp};

fn drain_serial(modem: &mut ModemDsp) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(b) = modem.send_to_serial() {
        out.push(b);
    }
    out
}

fn send_at(modem: &mut ModemDsp, cmd: &str) {
    for &byte in cmd.as_bytes() {
        modem.receive_from_serial(byte);
    }
    modem.receive_from_serial(b'\r');
}

/// Cross-feeds audio between two modems with one tick of propagation delay.
struct BackToBack {
    caller_out: [i16; CHUNK_SAMPLES],
    answerer_out: [i16; CHUNK_SAMPLES],
}

impl BackToBack {
    fn tick(&mut self, caller: &mut ModemDsp, answerer: &mut ModemDsp) {
        let mut caller_out = [0i16; CHUNK_SAMPLES];
        let mut answerer_out = [0i16; CHUNK_SAMPLES];
        caller.tick_dsp(&self.answerer_out, &mut caller_out);
        answerer.tick_dsp(&self.caller_out, &mut answerer_out);
        self.caller_out = caller_out;
        self.answerer_out = answerer_out;
    }
}

/// A 9600 bps call, online, converged and ready to relay.
fn connected() -> (ModemDsp, ModemDsp, BackToBack) {
    let config = || ModemConfig {
        max_speed: 9600,
        ..Default::default()
    };
    let (mut caller, mut answerer) = (ModemDsp::new(config()), ModemDsp::new(config()));
    let mut line = BackToBack {
        caller_out: [0; CHUNK_SAMPLES],
        answerer_out: [0; CHUNK_SAMPLES],
    };
    send_at(&mut answerer, "ATS0=1");
    send_at(&mut caller, "ATD5551234");
    for _ in 0..1500 {
        line.tick(&mut caller, &mut answerer);
        if caller.is_online() && answerer.is_online() {
            break;
        }
    }
    for _ in 0..150 {
        line.tick(&mut caller, &mut answerer);
    }
    assert!(caller.relay_ready() && answerer.relay_ready());
    drain_serial(&mut caller);
    drain_serial(&mut answerer);
    (caller, answerer, line)
}

/// Sends `payload` caller to answerer: one modulated tick, a drain started
/// right after it, then the rest relayed. Returns what the answerer's DTE
/// received.
fn hand_over(payload: &[u8]) -> Vec<u8> {
    let (mut caller, mut answerer, mut line) = connected();
    for &byte in payload {
        caller.receive_from_serial(byte);
    }
    line.tick(&mut caller, &mut answerer);
    caller.begin_relay_drain();
    let mut received = Vec::new();
    for _ in 0..10 {
        line.tick(&mut caller, &mut answerer);
        received.extend(drain_serial(&mut answerer));
    }
    caller.enter_relay_tx();
    answerer.enter_relay_rx();
    for _ in 0..300 {
        line.tick(&mut caller, &mut answerer);
        let relayed = caller.take_relay_tx();
        answerer.relay_receive(&relayed);
        received.extend(drain_serial(&mut answerer));
    }
    assert!(
        caller.is_online() && answerer.is_online(),
        "relay lost the call"
    );
    received
}

#[test]
fn bytes_on_both_sides_of_the_handover_arrive_once_in_order() {
    let payload: Vec<u8> = (0..400u32).map(|i| (i * 7 % 256) as u8).collect();
    assert_eq!(hand_over(&payload), payload);
}

#[test]
fn a_stuffed_byte_split_at_the_drain_keeps_its_second_half_on_the_line() {
    // Twelve bytes fit a 9600 bps tick; an escaped flag in the twelfth slot
    // sends its escape now and leaves its second half for the next tick,
    // the first tick of the drain.
    for special in [0x7E, 0x7D] {
        let mut payload: Vec<u8> = (1..=11).collect();
        payload.push(special);
        payload.extend(20..60);
        assert_eq!(hand_over(&payload), payload, "split {special:#04X}");
    }
}

#[test]
fn a_relayed_line_is_not_a_lost_carrier() {
    let (mut caller, mut answerer, mut line) = connected();
    for modem in [&mut caller, &mut answerer] {
        modem.begin_relay_drain();
    }
    for _ in 0..10 {
        line.tick(&mut caller, &mut answerer);
    }
    for modem in [&mut caller, &mut answerer] {
        modem.enter_relay_tx();
        modem.enter_relay_rx();
    }
    // Both lines are now silent, far past the 1.4 s carrier-loss timeout.
    for _ in 0..500 {
        line.tick(&mut caller, &mut answerer);
    }
    assert!(caller.is_online() && answerer.is_online());
}
