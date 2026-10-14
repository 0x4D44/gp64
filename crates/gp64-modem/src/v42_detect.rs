//! V.42/MNP detection phase.
//!
//! After training completes, real modems may spend a short period deciding
//! whether the link will stay transparent or move into an error-corrected
//! framing mode. The originating modem can advertise LAPM using the ODP
//! (Originating Detection Pattern) followed by HDLC-framed XID bytes, and
//! some peers may offer MNP instead.
//!
//! The detector deliberately stays narrow: it identifies which seam should
//! take ownership next and preserves every byte the future LAPM engine will
//! need. It does not implement LAPM or MNP itself.
//!
//! ## Handoff contract
//!
//! Once detection completes the detector enters a pass-through role keyed on
//! the outcome:
//!
//! - **Transparent** – post-detection bytes accumulate in an internal buffer;
//!   drain them with [`V42Detector::take_data`].
//!
//! - **LAPM** – the detector enters [`V42DetectState::LapmDetected`] and bytes
//!   accumulate from the first HDLC flag onwards in an internal buffer. The
//!   LAPM engine claims the buffer by calling [`V42Detector::take_lapm_bytes`],
//!   which atomically drains the buffer **and** advances the state to
//!   `Complete(Lapm)` to signal unambiguous stream ownership. Any bytes that
//!   arrive via [`V42Detector::feed`] afterward continue to buffer and can be
//!   drained by subsequent calls to [`V42Detector::take_lapm_bytes`].
//!   [`V42Detector::status`] provides a non-destructive snapshot of whatever
//!   is currently in the buffer.
//!
//! - **UnsupportedMnp** – post-detection bytes are silently dropped; this
//!   outcome is emitted for diagnostics only. MNP data mode is not supported
//!   in phase 1.

/// HDLC flag byte marking the end of the ODP alternating-bit preamble.
const HDLC_FLAG: u8 = 0x7E;

/// Minimum consecutive alternating-bit bytes (0xAA or 0x55) to recognize ODP.
const ODP_MIN_ALT_BYTES: usize = 4;

/// MNP SYN character.
const MNP_SYN: u8 = 0x16;

/// MNP DLE character.
const MNP_DLE: u8 = 0x10;

/// MNP STX character.
const MNP_STX: u8 = 0x02;

/// Early-exit timeout: no patterns detected for 500ms (50 ticks at 10ms).
const SILENCE_TIMEOUT_TICKS: u32 = 50;

/// Total detection window: ~2.5s (250 ticks at 10ms).
const TOTAL_TIMEOUT_TICKS: u32 = 250;

/// Structured handoff result from the V.42 seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V42NegotiationOutcome {
    /// No framed link-layer was negotiated; enter transparent data mode.
    Transparent { pending_data: Vec<u8> },
    /// LAPM negotiation is in progress. The buffered bytes start at the first
    /// HDLC flag and must be handed to the LAPM engine intact.
    Lapm { startup_bytes: Vec<u8> },
    /// The remote advertised MNP, which phase 1 does not support.
    UnsupportedMnp,
}

/// Current status of the V.42 detection seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V42NegotiationStatus {
    /// No framed handoff has been selected yet.
    Negotiating,
    /// A next owner for the receive stream has been identified.
    Handoff(V42NegotiationOutcome),
}

/// Detection state machine states.
///
/// The LAPM handoff is a two-step protocol:
///
/// 1. `Listening` → `LapmDetected` on ODP recognition. The detector buffers
///    all bytes from the first HDLC flag onwards; callers see
///    `Handoff(Lapm { .. })` from [`V42Detector::status`].
/// 2. `LapmDetected` → `Complete(Lapm)` when the LAPM engine calls
///    [`V42Detector::take_lapm_bytes`]. This drains the startup bytes and
///    signals unambiguous stream ownership to any observer.
///
/// The `V42NegotiationOutcome` variant stored inside `Complete` acts as a
/// **discriminant only** — its data fields (`pending_data`, `startup_bytes`)
/// are unused placeholders. Actual payload bytes live in
/// `V42Detector::data_buffer` (transparent) or `V42Detector::lapm_buffer`
/// (LAPM) and are served from there by [`V42Detector::status`] and the
/// `take_*` methods.
#[derive(Debug, Clone, PartialEq, Eq)]
enum V42DetectState {
    /// Waiting for patterns or timeout.
    Listening { elapsed_ticks: u32 },
    /// ODP detected (alternating-bit preamble + HDLC flag). The detector is
    /// buffering the LAPM stream but the LAPM engine has not yet acknowledged
    /// ownership. Call [`V42Detector::take_lapm_bytes`] to finalize.
    LapmDetected,
    /// Detection completed with an explicit outcome. The stored
    /// `V42NegotiationOutcome` is a discriminant; see type-level doc.
    Complete(V42NegotiationOutcome),
}

/// V.42/MNP detection phase handler.
///
/// After V.34/V.90 training completes, feed received bytes via [`feed()`] and
/// advance time via [`tick()`]. When detection is complete, any subsequent
/// bytes fed are buffered and retrievable via [`take_data()`] (transparent
/// mode) or [`take_lapm_bytes()`] (LAPM mode). See the module-level doc for
/// the full handoff contract.
pub struct V42Detector {
    state: V42DetectState,
    /// Count of consecutive alternating-bit bytes (0xAA or 0x55) seen.
    alt_run: usize,
    /// Whether any bytes have been received at all (for early-exit logic).
    any_bytes_received: bool,
    /// Buffered user data arriving after transparent selection.
    data_buffer: Vec<u8>,
    /// Buffered LAPM bytes beginning at the first HDLC flag.
    ///
    /// This is the single source of truth for the LAPM startup byte stream.
    /// [`status()`] serves a snapshot clone; [`take_lapm_bytes()`] drains it.
    /// Both operate on this same buffer — draining via `take_lapm_bytes`
    /// clears what a subsequent `status` call will see.
    lapm_buffer: Vec<u8>,
    /// Small buffer for matching the MNP header sequence.
    mnp_header_buf: Vec<u8>,
}

impl Default for V42Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl V42Detector {
    pub fn new() -> Self {
        Self {
            state: V42DetectState::Listening { elapsed_ticks: 0 },
            alt_run: 0,
            any_bytes_received: false,
            data_buffer: Vec::new(),
            lapm_buffer: Vec::new(),
            mnp_header_buf: Vec::new(),
        }
    }

    /// Feed received bytes through the seam and report the current handoff status.
    pub fn feed(&mut self, data: &[u8]) -> V42NegotiationStatus {
        if data.is_empty() {
            return self.status();
        }

        for &byte in data {
            match &self.state {
                V42DetectState::Complete(V42NegotiationOutcome::Transparent { .. }) => {
                    self.data_buffer.push(byte);
                }
                V42DetectState::Complete(V42NegotiationOutcome::Lapm { .. })
                | V42DetectState::LapmDetected => {
                    self.lapm_buffer.push(byte);
                }
                V42DetectState::Complete(V42NegotiationOutcome::UnsupportedMnp) => {}
                V42DetectState::Listening { .. } => {
                    self.any_bytes_received = true;
                    self.process_listening(byte);
                }
            }
        }

        self.status()
    }

    /// Process a byte in the Listening state.
    fn process_listening(&mut self, byte: u8) {
        if self.process_mnp_header(byte) {
            self.state = V42DetectState::Complete(V42NegotiationOutcome::UnsupportedMnp);
            return;
        }

        if is_alternating_bits(byte) {
            self.alt_run += 1;
        } else if byte == HDLC_FLAG && self.alt_run >= ODP_MIN_ALT_BYTES {
            // ODP detected: alternating-bit preamble followed by HDLC flag.
            self.lapm_buffer.clear();
            self.lapm_buffer.push(HDLC_FLAG);
            self.state = V42DetectState::LapmDetected;
            self.alt_run = 0;
        } else {
            // Not part of ODP; reset alternating-bit run.
            self.alt_run = 0;
        }
    }

    /// Returns true when the byte stream matches an MNP framing header.
    fn process_mnp_header(&mut self, byte: u8) -> bool {
        self.mnp_header_buf.push(byte);
        if self.mnp_header_buf.len() > 2 {
            self.mnp_header_buf.remove(0);
        }
        self.mnp_header_buf == [MNP_SYN, MNP_SYN] || self.mnp_header_buf == [MNP_DLE, MNP_STX]
    }

    /// Advance time by one tick (10ms) and report the current handoff status.
    pub fn tick(&mut self) -> V42NegotiationStatus {
        if let V42DetectState::Listening { elapsed_ticks } = &mut self.state {
            *elapsed_ticks += 1;
            let elapsed = *elapsed_ticks;

            // Early exit: no usable framing patterns showed up quickly enough,
            // so transparent mode wins.
            if elapsed >= TOTAL_TIMEOUT_TICKS
                || (elapsed >= SILENCE_TIMEOUT_TICKS
                    && (!self.any_bytes_received || self.alt_run == 0))
            {
                self.state = V42DetectState::Complete(V42NegotiationOutcome::Transparent {
                    pending_data: self.take_data(),
                });
            }
        }

        self.status()
    }

    /// Current seam status.
    pub fn status(&self) -> V42NegotiationStatus {
        match &self.state {
            V42DetectState::Listening { .. } => V42NegotiationStatus::Negotiating,
            V42DetectState::LapmDetected => {
                V42NegotiationStatus::Handoff(V42NegotiationOutcome::Lapm {
                    startup_bytes: self.lapm_buffer.clone(),
                })
            }
            V42DetectState::Complete(outcome) => V42NegotiationStatus::Handoff(match outcome {
                V42NegotiationOutcome::Transparent { .. } => V42NegotiationOutcome::Transparent {
                    pending_data: self.data_buffer.clone(),
                },
                V42NegotiationOutcome::Lapm { .. } => V42NegotiationOutcome::Lapm {
                    startup_bytes: self.lapm_buffer.clone(),
                },
                V42NegotiationOutcome::UnsupportedMnp => V42NegotiationOutcome::UnsupportedMnp,
            }),
        }
    }

    /// Take any buffered data bytes that arrived after detection completed.
    /// These are real user data, not detection patterns.
    pub fn take_data(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.data_buffer)
    }

    /// Take the buffered LAPM bytes beginning at the first HDLC flag.
    ///
    /// When called from [`LapmDetected`][V42DetectState::LapmDetected] this
    /// finalizes the detection seam: the state advances to `Complete(Lapm)` so
    /// that the LAPM engine has unambiguous ownership of the stream. Any bytes
    /// subsequently fed via [`feed()`] continue to accumulate in `lapm_buffer`
    /// and are retrievable by subsequent calls to this method.
    ///
    /// Note: [`tick()`] is a no-op after `LapmDetected` or `Complete(Lapm)`;
    /// it only drives timeouts while the detector is in `Listening` state.
    pub fn take_lapm_bytes(&mut self) -> Vec<u8> {
        if self.state == V42DetectState::LapmDetected {
            self.state = V42DetectState::Complete(V42NegotiationOutcome::Lapm {
                startup_bytes: vec![],
            });
        }
        std::mem::take(&mut self.lapm_buffer)
    }
}

/// Returns `true` if the byte has alternating bits (0xAA or 0x55).
fn is_alternating_bits(byte: u8) -> bool {
    byte == 0xAA || byte == 0x55
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_odp_detection() {
        let mut det = V42Detector::new();

        // ODP: alternating-bit preamble (>= 4 bytes of 0xAA) + HDLC flag.
        let mut odp = vec![0xAA; 6];
        odp.push(HDLC_FLAG);
        // Followed by some LAPM XID frame bytes (arbitrary — we don't parse).
        odp.extend_from_slice(&[0x01, 0x02, 0x03]);

        assert_eq!(
            det.feed(&odp),
            V42NegotiationStatus::Handoff(V42NegotiationOutcome::Lapm {
                startup_bytes: vec![HDLC_FLAG, 0x01, 0x02, 0x03],
            })
        );
        assert!(matches!(det.state, V42DetectState::LapmDetected));
    }

    #[test]
    fn test_mnp_detection() {
        let mut det = V42Detector::new();

        // Feed MNP SYN+DLE+STX header. Should be absorbed silently.
        let mnp = [MNP_SYN, MNP_SYN, MNP_DLE, MNP_STX, 0x04, 0x05];
        assert_eq!(
            det.feed(&mnp),
            V42NegotiationStatus::Handoff(V42NegotiationOutcome::UnsupportedMnp)
        );
    }

    #[test]
    fn test_timeout_no_patterns() {
        let mut det = V42Detector::new();

        // Feed no data at all, tick 50 times (500ms).
        for i in 0..SILENCE_TIMEOUT_TICKS {
            let status = det.tick();
            if i < SILENCE_TIMEOUT_TICKS - 1 {
                assert_eq!(status, V42NegotiationStatus::Negotiating);
            } else {
                assert_eq!(
                    status,
                    V42NegotiationStatus::Handoff(V42NegotiationOutcome::Transparent {
                        pending_data: Vec::new(),
                    })
                );
            }
        }
    }

    #[test]
    fn test_passthrough_after_transparent_selection() {
        let mut det = V42Detector::new();

        // Force completion via silence timeout.
        for _ in 0..SILENCE_TIMEOUT_TICKS {
            det.tick();
        }

        // Now feed user data — should be buffered.
        let user_data = b"Hello, modem!";
        det.feed(user_data);

        let buffered = det.take_data();
        assert_eq!(buffered, user_data);

        // Second take should be empty.
        assert!(det.take_data().is_empty());
    }

    #[test]
    fn test_noise_not_detected() {
        let mut det = V42Detector::new();

        // Feed random-ish bytes that don't match ODP or MNP patterns.
        let noise: Vec<u8> = (0..20).map(|i| (i * 37 + 13) as u8).collect();
        det.feed(&noise);

        // Should still be Listening (noise doesn't match ODP).
        assert!(matches!(det.state, V42DetectState::Listening { .. }));

        // Early exit after silence timeout since no valid pattern was found.
        for _ in 0..SILENCE_TIMEOUT_TICKS {
            det.tick();
        }
        assert!(matches!(
            det.status(),
            V42NegotiationStatus::Handoff(V42NegotiationOutcome::Transparent { .. })
        ));
    }

    #[test]
    fn test_interleaved_data_and_odp() {
        let mut det = V42Detector::new();

        // Feed some non-ODP bytes first (part of detection exchange, discarded).
        det.feed(&[0x12, 0x34, 0x56]);

        // Then feed ODP.
        let mut odp = vec![0xAA; 5];
        odp.push(HDLC_FLAG);
        odp.extend_from_slice(&[0xFF, 0xFE]); // LAPM XID data
        det.feed(&odp);

        // Should have detected ODP despite prior non-ODP bytes.
        assert!(matches!(det.state, V42DetectState::LapmDetected));

        // Pre-ODP bytes are discarded (they were part of the detection
        // exchange). No user data should be buffered.
        assert!(det.take_data().is_empty());
        assert_eq!(det.take_lapm_bytes(), vec![HDLC_FLAG, 0xFF, 0xFE]);
    }

    // --- LAPM byte-preservation contract tests ---
    //
    // These tests directly verify the acceptance criterion: the seam must not
    // silently consume bytes later needed for LAPM handoff.

    #[test]
    fn test_lapm_bytes_accumulated_across_multiple_feeds() {
        // LAPM XID bytes arriving in separate feed() calls must all be
        // preserved in lapm_buffer — none silently dropped between calls.
        let mut det = V42Detector::new();

        // Establish ODP and enter LapmDetected.
        let mut odp: Vec<u8> = vec![0xAA; 4]; // exactly the minimum preamble
        odp.push(HDLC_FLAG);
        det.feed(&odp);
        assert!(matches!(det.state, V42DetectState::LapmDetected));

        // Deliver LAPM XID bytes in three separate calls.
        det.feed(&[0x01, 0x02]);
        det.feed(&[0x03, 0x04]);
        det.feed(&[0x05]);

        // Every byte from the HDLC flag onward must be present, in order.
        let lapm = det.take_lapm_bytes();
        assert_eq!(lapm, vec![HDLC_FLAG, 0x01, 0x02, 0x03, 0x04, 0x05]);
    }

    #[test]
    fn test_status_non_destructive_for_lapm() {
        // Calling status() must not remove bytes from the LAPM buffer.
        // The LAPM engine relies on take_lapm_bytes() being the sole drain.
        let mut det = V42Detector::new();

        let mut odp: Vec<u8> = vec![0xAA; 4];
        odp.push(HDLC_FLAG);
        odp.extend_from_slice(&[0xAB, 0xCD]);
        det.feed(&odp);

        // Poll status() three times — must be idempotent.
        let s1 = det.status();
        let s2 = det.status();
        let s3 = det.status();
        assert_eq!(s1, s2);
        assert_eq!(s2, s3);

        // Taking bytes after repeated polling must still yield all of them.
        let lapm = det.take_lapm_bytes();
        assert_eq!(lapm, vec![HDLC_FLAG, 0xAB, 0xCD]);
    }

    #[test]
    fn test_odp_preamble_excluded_from_lapm_buffer() {
        // The alternating-bit ODP preamble (0xAA/0x55) bytes must NOT appear
        // in the LAPM startup bytes. The handoff boundary is the HDLC flag.
        let mut det = V42Detector::new();

        let mut stream: Vec<u8> = vec![0x55; 6]; // 0x55 is a valid alt-bit byte
        stream.push(HDLC_FLAG);
        stream.extend_from_slice(&[0x11, 0x22]);
        det.feed(&stream);

        let lapm = det.take_lapm_bytes();
        // Preamble bytes must be absent; buffer must begin at the HDLC flag.
        assert!(
            !lapm.contains(&0x55),
            "ODP preamble byte 0x55 leaked into LAPM buffer: {lapm:?}"
        );
        assert_eq!(
            lapm,
            vec![HDLC_FLAG, 0x11, 0x22],
            "LAPM buffer has wrong content: {lapm:?}"
        );
    }

    #[test]
    fn test_odp_requires_minimum_alt_bytes() {
        // Fewer than ODP_MIN_ALT_BYTES alternating bytes before an HDLC flag
        // must not trigger LAPM detection — avoids false handoffs on noise.
        let mut det = V42Detector::new();

        // Only 3 alternating bytes (one below the 4-byte minimum).
        det.feed(&[0xAA, 0xAA, 0xAA, HDLC_FLAG]);

        // Must remain Listening; no LAPM bytes buffered.
        assert!(
            matches!(det.state, V42DetectState::Listening { .. }),
            "Expected Listening, got {:?}",
            det.state
        );
        assert!(
            det.take_lapm_bytes().is_empty(),
            "LAPM buffer should be empty after below-minimum ODP preamble"
        );
    }

    // ── Additional seam / boundary tests ─────────────────────────────────────

    #[test]
    fn test_odp_exact_minimum_success() {
        // Exactly ODP_MIN_ALT_BYTES (4) alternating bytes then HDLC flag must
        // succeed — off-by-one on the lower bound would lose valid connections.
        let mut det = V42Detector::new();
        let mut odp = vec![0xAA; ODP_MIN_ALT_BYTES];
        odp.push(HDLC_FLAG);
        assert!(
            matches!(
                det.feed(&odp),
                V42NegotiationStatus::Handoff(V42NegotiationOutcome::Lapm { .. })
            ),
            "exactly ODP_MIN_ALT_BYTES must trigger LAPM"
        );
    }

    #[test]
    fn test_odp_mixed_alt_bytes() {
        // Both 0xAA and 0x55 are valid alternating-bit patterns; a run that
        // alternates between them must still extend the counter.
        let mut det = V42Detector::new();
        // 4 mixed alt bytes is the exact minimum.
        let odp = [0xAA, 0x55, 0xAA, 0x55, HDLC_FLAG];
        assert!(
            matches!(
                det.feed(&odp),
                V42NegotiationStatus::Handoff(V42NegotiationOutcome::Lapm { .. })
            ),
            "mixed 0xAA/0x55 preamble must detect ODP"
        );
    }

    #[test]
    fn test_take_lapm_bytes_finalizes_handoff() {
        // take_lapm_bytes() must advance the state from LapmDetected to
        // Complete(Lapm) so the LAPM engine has unambiguous stream ownership.
        let mut det = V42Detector::new();
        let mut odp = vec![0xAA; ODP_MIN_ALT_BYTES];
        odp.push(HDLC_FLAG);
        odp.extend_from_slice(&[0xAB, 0xCD]);
        det.feed(&odp);

        assert!(matches!(det.state, V42DetectState::LapmDetected));

        let taken = det.take_lapm_bytes();
        assert_eq!(taken, vec![HDLC_FLAG, 0xAB, 0xCD]);

        assert!(
            matches!(
                det.state,
                V42DetectState::Complete(V42NegotiationOutcome::Lapm { .. })
            ),
            "state must be Complete(Lapm) after take_lapm_bytes(), got {:?}",
            det.state
        );
    }

    #[test]
    fn test_bytes_after_lapm_handoff_not_lost() {
        // Bytes arriving after the initial take_lapm_bytes() must accumulate
        // and be retrievable — they must not be silently dropped.
        let mut det = V42Detector::new();
        let mut odp = vec![0xAA; ODP_MIN_ALT_BYTES];
        odp.push(HDLC_FLAG);
        det.feed(&odp);
        let first = det.take_lapm_bytes(); // finalize; drains HDLC_FLAG
        assert_eq!(first, vec![HDLC_FLAG]);

        // State is now Complete(Lapm).  Post-handoff bytes must still buffer.
        det.feed(&[0x01, 0x02, 0x03]);
        let second = det.take_lapm_bytes();
        assert_eq!(second, vec![0x01, 0x02, 0x03]);
        assert!(
            det.take_lapm_bytes().is_empty(),
            "buffer must be empty after second take"
        );
    }

    #[test]
    fn test_tick_does_not_override_lapm_detection() {
        // Once LAPM is detected, ticking past both timeout thresholds must not
        // revert the outcome to Transparent.
        let mut det = V42Detector::new();
        let mut odp = vec![0xAA; ODP_MIN_ALT_BYTES];
        odp.push(HDLC_FLAG);
        det.feed(&odp);

        for _ in 0..TOTAL_TIMEOUT_TICKS + 10 {
            assert!(
                matches!(
                    det.tick(),
                    V42NegotiationStatus::Handoff(V42NegotiationOutcome::Lapm { .. })
                ),
                "tick must not revert confirmed LAPM detection to Transparent"
            );
        }
    }

    #[test]
    fn test_total_timeout_with_partial_odp() {
        // An alt_run in progress (HDLC flag never arrives) blocks the 500 ms
        // early-exit but must fall through to Transparent at the 2.5 s total
        // timeout.
        let mut det = V42Detector::new();
        // Raise alt_run past the threshold but never send the HDLC flag.
        det.feed(&[0xAA; ODP_MIN_ALT_BYTES + 1]);
        // Silence-timeout (tick 50) must NOT fire because alt_run > 0.
        // Total timeout (tick 250) must produce Transparent.
        for i in 0..TOTAL_TIMEOUT_TICKS {
            let s = det.tick();
            if i < TOTAL_TIMEOUT_TICKS - 1 {
                assert_eq!(s, V42NegotiationStatus::Negotiating, "tick {i}");
            } else {
                assert!(
                    matches!(
                        s,
                        V42NegotiationStatus::Handoff(V42NegotiationOutcome::Transparent { .. })
                    ),
                    "expected Transparent at total timeout (tick {i})"
                );
            }
        }
    }

    #[test]
    fn test_mnp_dle_stx() {
        // DLE+STX is the second MNP framing marker; must also trigger
        // UnsupportedMnp (not just SYN+SYN).
        let mut det = V42Detector::new();
        let seq = [MNP_DLE, MNP_STX, 0x04, 0x01];
        assert_eq!(
            det.feed(&seq),
            V42NegotiationStatus::Handoff(V42NegotiationOutcome::UnsupportedMnp)
        );
    }
}
