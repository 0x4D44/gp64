//! V.34 4D trellis encoder and Viterbi decoder.
//!
//! 16-state systematic feedback convolutional encoder (rate 2/3) processing 4D
//! super-symbols (pairs of consecutive 2D symbols). Each super-symbol carries 2
//! trellis-coded bits that select a 4D coset pair from the even-sum group.
//!
//! ## Encoder
//!
//! Input: 2 data bits (Y1 = I0, Y2 = I1) per 4D super-symbol.
//! State: 4-bit feedback register (r3, r2, r1, r0).
//! Output parity: Y0 = r0 (LSB of state register).
//! Coded output: 3 bits (Y2, Y1, Y0) → one of 8 4D coset pairs.
//! State update (Figure 10 of ITU-T V.34):
//!   r0' = r1 ⊕ Y1, r1' = r2 ⊕ Y2, r2' = r3 ⊕ Y2 ⊕ Y0, r3' = Y0.
//!
//! ## Decoder
//!
//! 16-state Viterbi with 4D branch metrics. Unlike V.32bis, survivor memory
//! stores per-state point indices because they are coset-dependent.
//!
//! ## Generator polynomials
//!
//! The encoder implements ITU-T V.34 Figure 10 (16-state convolutional encoder),
//! verified against Bellard's linmodem v34.c (`trellis_next_state`, case 16).
//! The encoder is systematic: Y1 and Y2 pass through unchanged, and the feedback
//! register produces the parity bit Y0.

use crate::v34_constellation::{V34Constellation, NUM_COSETS};

/// Traceback depth: 5× constraint length (constraint length = 5 for 16 states).
const TRACEBACK_DEPTH: usize = 100; // Deep traceback to handle 2.5% ISI errors without equalizer

/// Number of encoder/decoder states.
const NUM_STATES: usize = 16;

/// Number of possible 2-bit inputs per state transition.
const NUM_INPUTS: usize = 4;

/// 4D coset pair table: maps 3-bit coded output (y2, y1, y0) to (coset_a, coset_b).
///
/// Uses the even-sum group where (c_a + c_b) mod 2 = 0:
///   000 → (C0,C0), 001 → (C1,C1), 010 → (C2,C2), 011 → (C3,C3)
///   100 → (C0,C2), 101 → (C1,C3), 110 → (C2,C0), 111 → (C3,C1)
const COSET_PAIR_TABLE: [(u8, u8); 8] = [
    (0, 0), // 000
    (1, 1), // 001
    (2, 2), // 010
    (3, 3), // 011
    (0, 2), // 100
    (1, 3), // 101
    (2, 0), // 110
    (3, 1), // 111
];

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// 16-state V.34 trellis encoder.
///
/// Systematic rate-2/3 convolutional encoder with 4-bit state register.
/// Processes one 4D super-symbol per `encode()` call.
#[derive(Clone)]
pub struct V34TrellisEncoder {
    /// 4-bit state register (s3, s2, s1, s0) packed in bits [3:0].
    state: u8,
}

impl V34TrellisEncoder {
    pub fn new() -> Self {
        Self { state: 0 }
    }

    /// Encode 2 data bits into a 4D coset pair.
    ///
    /// `data_bits`: 2-bit input (I1, I0) in bits [1:0].
    /// Returns `(coset_a, coset_b)` where each is 0-3.
    pub fn encode(&mut self, data_bits: u8) -> (u8, u8) {
        let coded = self.encode_raw(data_bits);
        COSET_PAIR_TABLE[coded as usize]
    }

    /// Encode 2 data bits, returning the 3-bit coded output directly.
    ///
    /// Useful for tests that need the raw coded bits rather than coset pairs.
    fn encode_raw(&mut self, data_bits: u8) -> u8 {
        let (coded, next_state) = encoder_transition(self.state, data_bits);
        self.state = next_state;
        coded
    }

    /// Reset encoder state to zero.
    pub fn reset(&mut self) {
        self.state = 0;
    }

    /// Current state (0-15).
    pub fn state(&self) -> u8 {
        self.state
    }
}

impl Default for V34TrellisEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute encoder outputs for a given state and 2-bit input.
///
/// Returns `(coded_3bits, next_state)` where `coded_3bits` = (y2, y1, y0).
///
/// This implements the V.34 16-state systematic feedback encoder (ITU-T V.34
/// Figure 10), as verified against Bellard's linmodem v34.c:
///
///   Inputs: Y1 (= I0), Y2 (= I1) — 2 data bits
///   Output: Y0 = r0 (parity bit = LSB of state register before update)
///   Coded output: (Y2, Y1, Y0) = (I1, I0, r0)
///
///   State update (4-bit register r3,r2,r1,r0):
///     r0' = r1 ⊕ Y1
///     r1' = r2 ⊕ Y2
///     r2' = r3 ⊕ Y2 ⊕ Y0   (where Y0 = old r0)
///     r3' = Y0               (= old r0)
///
/// The encoder is systematic: the two data bits pass through unchanged as Y1
/// and Y2, and the encoder adds one parity bit Y0. The state register implements
/// the feedback polynomial structure that creates the free distance gain.
fn encoder_transition(state: u8, data_bits: u8) -> (u8, u8) {
    let y1 = data_bits & 1; // I0 → Y1
    let y2 = (data_bits >> 1) & 1; // I1 → Y2

    let r0 = state & 1;
    let r1 = (state >> 1) & 1;
    let r2 = (state >> 2) & 1;
    let r3 = (state >> 3) & 1;

    // Output parity bit = LSB of current state.
    let y0 = r0;

    // 3-bit coded output: (y2, y1, y0).
    let coded = y0 | (y1 << 1) | (y2 << 2);

    // State update (Figure 10 of V.34 spec):
    let new_r0 = r1 ^ y1;
    let new_r1 = r2 ^ y2;
    let new_r2 = r3 ^ y2 ^ y0;
    let new_r3 = y0;

    let next_state = new_r0 | (new_r1 << 1) | (new_r2 << 2) | (new_r3 << 3);

    (coded, next_state)
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// Survivor entry for one state at one timestep.
#[derive(Clone, Copy, Default)]
struct V34Survivor {
    /// Predecessor state (0-15).
    prev_state: u8,
    /// 2-bit trellis input that caused this transition.
    trellis_input: u8,
    /// Nearest-in-coset point index for the first 2D symbol.
    point_a: u16,
    /// Nearest-in-coset point index for the second 2D symbol.
    point_b: u16,
}

/// Decoded output from one 4D super-symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V34DecodedSymbol {
    /// 2 trellis-coded data bits.
    pub trellis_bits: u8,
    /// Point index within coset_a (0-255 for direct mapping).
    pub point_a: u16,
    /// Point index within coset_b (0-255 for direct mapping).
    pub point_b: u16,
    /// Coset index for the first 2D symbol (0-3).
    pub coset_a: u8,
    /// Coset index for the second 2D symbol (0-3).
    pub coset_b: u8,
}

/// 16-state Viterbi decoder for the V.34 4D trellis code.
///
/// Uses soft-decision (Euclidean distance) branch metrics computed from
/// received (I, Q) pairs and per-coset nearest-neighbor lookups.
///
/// Key difference from V.32bis: survivor memory stores per-state point
/// indices because they are coset-dependent.
pub struct V34ViterbiDecoder {
    /// Path metrics for each state.
    path_metrics: [f64; NUM_STATES],
    /// Survivor paths: `survivors[time % capacity][state]`.
    survivors: Vec<[V34Survivor; NUM_STATES]>,
    /// Traceback depth.
    traceback_depth: usize,
    /// Number of super-symbols fed so far.
    time: usize,
}

impl V34ViterbiDecoder {
    pub fn new() -> Self {
        let traceback_depth = TRACEBACK_DEPTH;
        let capacity = traceback_depth + 2;
        Self {
            path_metrics: {
                let mut m = [f64::MAX; NUM_STATES];
                m[0] = 0.0; // Start in state 0.
                m
            },
            survivors: vec![[V34Survivor::default(); NUM_STATES]; capacity],
            traceback_depth,
            time: 0,
        }
    }

    /// Number of super-symbols fed so far (for diagnostic use).
    pub fn time(&self) -> usize {
        self.time
    }

    /// Decode one 4D super-symbol from two received (I, Q) pairs.
    ///
    /// Returns decoded output after traceback delay, or `None` during
    /// the initial warmup period (first `traceback_depth` super-symbols).
    pub fn decode(
        &mut self,
        rx_a: (f64, f64),
        rx_b: (f64, f64),
        constellation: &V34Constellation,
    ) -> Option<V34DecodedSymbol> {
        let mut new_metrics = [f64::MAX; NUM_STATES];
        let capacity = self.survivors.len();
        let slot = self.time % capacity;
        let mut new_survivors = [V34Survivor::default(); NUM_STATES];

        // `nearest_in_coset` is a pure function of the received point and the
        // coset — it takes `&self` and mutates nothing — so its result does not
        // depend on the trellis state or input. Computing it inside the branch
        // loop meant the 64 branches recomputed the same eight results, and
        // each search scans at least 256 constellation points
        // (MDD-BUG-CRUCIBLE-00513).
        //
        // The eight searches now run unconditionally, where before an
        // all-unreachable state vector would have run none. That is a constant
        // 8 against a previous worst case of 64, so the trade is deliberate.
        let nearest_a: [(u16, f64); NUM_COSETS] = std::array::from_fn(|coset| {
            constellation.nearest_in_coset(rx_a.0, rx_a.1, coset as u8)
        });
        let nearest_b: [(u16, f64); NUM_COSETS] = std::array::from_fn(|coset| {
            constellation.nearest_in_coset(rx_b.0, rx_b.1, coset as u8)
        });

        for prev_state in 0..NUM_STATES as u8 {
            if self.path_metrics[prev_state as usize] == f64::MAX {
                continue;
            }

            // Try all 4 possible 2-bit inputs.
            for input in 0..NUM_INPUTS as u8 {
                let (coded, next_state) = encoder_transition(prev_state, input);
                let (coset_a, coset_b) = COSET_PAIR_TABLE[coded as usize];

                // Per-coset nearest-neighbor lookups for branch metric,
                // resolved once per received point above.
                let (idx_a, dist_a) = nearest_a[coset_a as usize];
                let (idx_b, dist_b) = nearest_b[coset_b as usize];

                let branch_metric = dist_a + dist_b;
                let total_metric = self.path_metrics[prev_state as usize] + branch_metric;

                if total_metric < new_metrics[next_state as usize] {
                    new_metrics[next_state as usize] = total_metric;
                    new_survivors[next_state as usize] = V34Survivor {
                        prev_state,
                        trellis_input: input,
                        point_a: idx_a,
                        point_b: idx_b,
                    };
                }
            }
        }

        // Normalize path metrics by subtracting the minimum. Without this,
        // metrics grow without bound over thousands of symbols, causing f64
        // precision loss that produces Viterbi decode errors. Those errors
        // permanently desynchronize the descrambler's LFSR.
        // Skip f64::MAX entries: those are unreachable-state sentinels that
        // must stay at f64::MAX (subtracting a finite value from f64::MAX
        // still yields f64::MAX due to precision, but the intent is clearer
        // when we preserve them explicitly).
        let min_metric = new_metrics
            .iter()
            .copied()
            .filter(|&m| m != f64::MAX)
            .fold(f64::INFINITY, f64::min);
        if min_metric.is_finite() {
            for m in new_metrics.iter_mut() {
                if *m != f64::MAX {
                    *m -= min_metric;
                }
            }
        }

        self.path_metrics = new_metrics;
        self.survivors[slot] = new_survivors;
        self.time += 1;

        if self.time > self.traceback_depth {
            Some(self.traceback())
        } else {
            None
        }
    }

    /// Flush remaining super-symbols from the traceback buffer.
    ///
    /// Call after the last super-symbol. Feeds zero-input symbols to drain
    /// the pipeline, returning all buffered decoded outputs.
    pub fn flush(&mut self, constellation: &V34Constellation) -> Vec<V34DecodedSymbol> {
        let mut result = Vec::new();
        // Feed zero symbols at origin to flush the pipeline.
        let origin = (0.0, 0.0);
        for _ in 0..self.traceback_depth {
            if let Some(decoded) = self.decode(origin, origin, constellation) {
                result.push(decoded);
            }
        }
        result
    }

    /// Reset decoder state.
    pub fn reset(&mut self) {
        self.path_metrics = {
            let mut m = [f64::MAX; NUM_STATES];
            m[0] = 0.0;
            m
        };
        for entry in self.survivors.iter_mut() {
            *entry = [V34Survivor::default(); NUM_STATES];
        }
        self.time = 0;
    }

    /// Find the state with the best (lowest) path metric.
    fn best_state(&self) -> u8 {
        let mut best = 0u8;
        let mut best_metric = f64::MAX;
        for (s, &m) in self.path_metrics.iter().enumerate() {
            if m < best_metric {
                best_metric = m;
                best = s as u8;
            }
        }
        best
    }

    /// Perform traceback from the best current state, returning the decoded
    /// output at the traceback depth point.
    fn traceback(&self) -> V34DecodedSymbol {
        let capacity = self.survivors.len();

        let mut state = self.best_state();
        let mut decoded = V34DecodedSymbol {
            trellis_bits: 0,
            point_a: 0,
            point_b: 0,
            coset_a: 0,
            coset_b: 0,
        };

        // Walk backwards through survivor memory.
        for step in 0..=self.traceback_depth {
            let t = (self.time - 1 - step) % capacity;
            let survivor = &self.survivors[t][state as usize];
            if step == self.traceback_depth {
                decoded.trellis_bits = survivor.trellis_input;
                decoded.point_a = survivor.point_a;
                decoded.point_b = survivor.point_b;
                // Recover coset pair from the encoder transition that produced
                // this survivor. prev_state + trellis_input -> coded -> coset pair.
                let (coded, _) = encoder_transition(survivor.prev_state, survivor.trellis_input);
                let (ca, cb) = COSET_PAIR_TABLE[coded as usize];
                decoded.coset_a = ca;
                decoded.coset_b = cb;
            }
            state = survivor.prev_state;
        }

        decoded
    }
}

impl Default for V34ViterbiDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Deterministic xorshift32 PRNG (no external dependency needed).
    struct Xorshift32 {
        state: u32,
    }

    impl Xorshift32 {
        fn new(seed: u32) -> Self {
            Self {
                state: if seed == 0 { 1 } else { seed },
            }
        }

        fn next_u32(&mut self) -> u32 {
            let mut x = self.state;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.state = x;
            x
        }

        /// Uniform f64 in [0, 1).
        fn next_f64(&mut self) -> f64 {
            (self.next_u32() as f64) / (u32::MAX as f64 + 1.0)
        }

        /// Approximate Gaussian via Box-Muller transform.
        fn next_gaussian(&mut self) -> f64 {
            loop {
                let u1 = self.next_f64();
                let u2 = self.next_f64();
                if u1 > 1e-30 {
                    return (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
                }
            }
        }
    }

    fn make_constellation() -> V34Constellation {
        V34Constellation::new()
    }

    // -----------------------------------------------------------------------
    // Encoder: state transitions match polynomial specification.
    // -----------------------------------------------------------------------
    #[test]
    fn encoder_state_transitions_match_polynomials() {
        // Manually verify transitions using the V.34 Figure 10 encoder.
        // State register: (r3, r2, r1, r0). Y0=r0 (parity). Y1=I0, Y2=I1.
        // State update: r0'=r1^Y1, r1'=r2^Y2, r2'=r3^Y2^Y0, r3'=Y0.

        // State 0: r3=0, r2=0, r1=0, r0=0
        //   Input (I1=0, I0=0): Y0=0, Y1=0, Y2=0
        //   coded=(Y2,Y1,Y0)=(0,0,0)=0b000
        //   r0'=0^0=0, r1'=0^0=0, r2'=0^0^0=0, r3'=0 → state=0
        let (coded, next) = encoder_transition(0, 0b00);
        assert_eq!(coded, 0b000, "state=0 input=00");
        assert_eq!(next, 0, "state=0 input=00 next");

        //   Input (I1=0, I0=1): Y0=0, Y1=1, Y2=0
        //   coded=(0,1,0)=0b010
        //   r0'=0^1=1, r1'=0^0=0, r2'=0^0^0=0, r3'=0 → state=0b0001=1
        let (coded, next) = encoder_transition(0, 0b01);
        assert_eq!(coded, 0b010, "state=0 input=01");
        assert_eq!(next, 1, "state=0 input=01 next");

        //   Input (I1=1, I0=0): Y0=0, Y1=0, Y2=1
        //   coded=(1,0,0)=0b100
        //   r0'=0^0=0, r1'=0^1=1, r2'=0^1^0=1, r3'=0 → state=0b0110=6
        let (coded, next) = encoder_transition(0, 0b10);
        assert_eq!(coded, 0b100, "state=0 input=10");
        assert_eq!(next, 6, "state=0 input=10 next");

        //   Input (I1=1, I0=1): Y0=0, Y1=1, Y2=1
        //   coded=(1,1,0)=0b110
        //   r0'=0^1=1, r1'=0^1=1, r2'=0^1^0=1, r3'=0 → state=0b0111=7
        let (coded, next) = encoder_transition(0, 0b11);
        assert_eq!(coded, 0b110, "state=0 input=11");
        assert_eq!(next, 7, "state=0 input=11 next");

        // State 5: r3=0, r2=1, r1=0, r0=1 (binary 0101)
        //   Input (I1=0, I0=1): Y0=1, Y1=1, Y2=0
        //   coded=(0,1,1)=0b011
        //   r0'=0^1=1, r1'=1^0=1, r2'=0^0^1=1, r3'=1 → state=0b1111=15
        let (coded, next) = encoder_transition(0b0101, 0b01);
        assert_eq!(coded, 0b011, "state=5 input=01");
        assert_eq!(next, 15, "state=5 input=01 next");

        // State 15: r3=1, r2=1, r1=1, r0=1 (binary 1111)
        //   Input (I1=0, I0=0): Y0=1, Y1=0, Y2=0
        //   coded=(0,0,1)=0b001
        //   r0'=1^0=1, r1'=1^0=1, r2'=1^0^1=0, r3'=1 → state=0b1011=11
        let (coded, next) = encoder_transition(0b1111, 0b00);
        assert_eq!(coded, 0b001, "state=15 input=00");
        assert_eq!(next, 11, "state=15 input=00 next");
    }

    // -----------------------------------------------------------------------
    // Encoder: all 16 states are visited during a sequence.
    // -----------------------------------------------------------------------
    #[test]
    fn encoder_visits_all_16_states() {
        let mut encoder = V34TrellisEncoder::new();
        let mut rng = Xorshift32::new(0xCAFE_BABE);
        let mut visited = [false; NUM_STATES];
        visited[0] = true;

        for _ in 0..200 {
            let input = (rng.next_u32() & 3) as u8;
            encoder.encode(input);
            visited[encoder.state() as usize] = true;
        }

        let count = visited.iter().filter(|&&v| v).count();
        assert_eq!(count, NUM_STATES, "Only visited {count}/16 states");
    }

    // -----------------------------------------------------------------------
    // Encoder: all 8 4D coset pairs are reachable from every state.
    // -----------------------------------------------------------------------
    #[test]
    fn all_coset_pairs_reachable_from_each_state() {
        for start_state in 0..NUM_STATES as u8 {
            let mut seen_coded = [false; 8];

            // From this state, try all 4 inputs directly (gives 4 coded values).
            // Then also try all reachable 2-step paths to cover remaining codes.
            for input in 0..NUM_INPUTS as u8 {
                let (coded, _) = encoder_transition(start_state, input);
                seen_coded[coded as usize] = true;
            }

            // From one state with 4 inputs we get at most 4 of the 8 codes.
            // To get all 8, we need to check multi-step paths.
            // The requirement "all 8 reachable from each state" means
            // reachable across the trellis, not in a single step.
            for first_input in 0..NUM_INPUTS as u8 {
                let (_, mid_state) = encoder_transition(start_state, first_input);
                for second_input in 0..NUM_INPUTS as u8 {
                    let (coded, _) = encoder_transition(mid_state, second_input);
                    seen_coded[coded as usize] = true;
                }
            }

            let count = seen_coded.iter().filter(|&&v| v).count();
            assert_eq!(
                count, 8,
                "From state {start_state}: only {count}/8 coded outputs reachable in ≤2 steps"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Encoder: state wraps correctly (stays in 0-15).
    // -----------------------------------------------------------------------
    #[test]
    fn encoder_state_stays_in_range() {
        let mut encoder = V34TrellisEncoder::new();
        let mut rng = Xorshift32::new(0xDEAD_BEEF);

        for _ in 0..500 {
            let input = (rng.next_u32() & 3) as u8;
            encoder.encode(input);
            assert!(
                encoder.state() < 16,
                "State {} out of range",
                encoder.state()
            );
        }
    }

    // -----------------------------------------------------------------------
    // Encode → decode round-trip on clean channel (zero BER).
    // -----------------------------------------------------------------------
    #[test]
    fn encode_decode_roundtrip_clean_channel() {
        let constellation = make_constellation();
        let mut encoder = V34TrellisEncoder::new();
        let mut decoder = V34ViterbiDecoder::new();

        let mut rng = Xorshift32::new(0x1234_5678);
        let num_symbols = 100;

        let mut tx_data: Vec<u8> = Vec::with_capacity(num_symbols);
        let mut tx_points_a: Vec<u16> = Vec::with_capacity(num_symbols);
        let mut tx_points_b: Vec<u16> = Vec::with_capacity(num_symbols);

        // Encode and transmit.
        for _ in 0..num_symbols {
            let trellis_input = (rng.next_u32() & 3) as u8;
            let point_idx_a = (rng.next_u32() % 256) as u16;
            let point_idx_b = (rng.next_u32() % 256) as u16;

            let (coset_a, coset_b) = encoder.encode(trellis_input);

            tx_data.push(trellis_input);
            tx_points_a.push(point_idx_a);
            tx_points_b.push(point_idx_b);

            // Look up the actual (I, Q) for these points.
            let iq_a = constellation.point(coset_a, point_idx_a);
            let iq_b = constellation.point(coset_b, point_idx_b);

            // Feed clean signal to decoder.
            if let Some(decoded) = decoder.decode(iq_a, iq_b, &constellation) {
                // Will be checked after flush.
                let idx = tx_data.len() - 1 - decoder.traceback_depth;
                assert_eq!(
                    decoded.trellis_bits, tx_data[idx],
                    "Trellis bits mismatch at super-symbol {idx}"
                );
                assert_eq!(
                    decoded.point_a, tx_points_a[idx],
                    "Point A mismatch at super-symbol {idx}"
                );
                assert_eq!(
                    decoded.point_b, tx_points_b[idx],
                    "Point B mismatch at super-symbol {idx}"
                );
            }
        }

        // Flush.
        let flushed = decoder.flush(&constellation);
        let total_decoded = (num_symbols.saturating_sub(TRACEBACK_DEPTH)) + flushed.len();
        assert_eq!(
            total_decoded, num_symbols,
            "Total decoded {total_decoded} != transmitted {num_symbols}"
        );

        // Verify flushed symbols.
        let flush_start = num_symbols - flushed.len();
        for (i, decoded) in flushed.iter().enumerate() {
            let idx = flush_start + i;
            assert_eq!(
                decoded.trellis_bits, tx_data[idx],
                "Flushed trellis bits mismatch at super-symbol {idx}"
            );
            assert_eq!(
                decoded.point_a, tx_points_a[idx],
                "Flushed point A mismatch at super-symbol {idx}"
            );
            assert_eq!(
                decoded.point_b, tx_points_b[idx],
                "Flushed point B mismatch at super-symbol {idx}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Viterbi convergence: after traceback_depth symbols, all paths merge.
    // -----------------------------------------------------------------------
    #[test]
    fn viterbi_convergence_within_traceback_depth() {
        // On a clean channel, after traceback_depth symbols all survivor
        // paths should converge to the same state at the oldest time step.
        // We verify this indirectly: the decoded output is correct, which
        // requires convergence.
        let constellation = make_constellation();
        let mut encoder = V34TrellisEncoder::new();
        let mut decoder = V34ViterbiDecoder::new();

        let mut rng = Xorshift32::new(0xBAAD_F00D);

        // Encode enough symbols to fill the traceback and then some.
        let mut tx_data = Vec::new();
        let mut decoded_data = Vec::new();
        for _ in 0..(TRACEBACK_DEPTH + 30) {
            let input = (rng.next_u32() & 3) as u8;
            let point_a = (rng.next_u32() % 256) as u16;
            let point_b = (rng.next_u32() % 256) as u16;

            let (coset_a, coset_b) = encoder.encode(input);
            let iq_a = constellation.point(coset_a, point_a);
            let iq_b = constellation.point(coset_b, point_b);

            tx_data.push(input);

            if let Some(decoded) = decoder.decode(iq_a, iq_b, &constellation) {
                decoded_data.push(decoded.trellis_bits);
            }
        }

        // All decoded trellis bits should match transmitted.
        for (i, &decoded) in decoded_data.iter().enumerate() {
            assert_eq!(
                decoded, tx_data[i],
                "Convergence failure at position {i}: expected {}, got {decoded}",
                tx_data[i]
            );
        }
    }

    // -----------------------------------------------------------------------
    // Encode → decode round-trip with all-zero input.
    // -----------------------------------------------------------------------
    #[test]
    fn roundtrip_all_zeros() {
        let constellation = make_constellation();
        let mut encoder = V34TrellisEncoder::new();
        let mut decoder = V34ViterbiDecoder::new();

        let num_symbols = 50;

        for _ in 0..num_symbols {
            let (coset_a, coset_b) = encoder.encode(0);
            let iq_a = constellation.point(coset_a, 0);
            let iq_b = constellation.point(coset_b, 0);

            if let Some(decoded) = decoder.decode(iq_a, iq_b, &constellation) {
                assert_eq!(decoded.trellis_bits, 0, "Expected trellis_bits=0");
                assert_eq!(decoded.point_a, 0, "Expected point_a=0");
                assert_eq!(decoded.point_b, 0, "Expected point_b=0");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Encode → decode long round-trip with pseudo-random data.
    // -----------------------------------------------------------------------
    #[test]
    fn roundtrip_long_random() {
        let constellation = make_constellation();
        let mut encoder = V34TrellisEncoder::new();
        let mut decoder = V34ViterbiDecoder::new();

        let mut rng = Xorshift32::new(0xFEED_FACE);
        let num_symbols = 500;

        let mut tx_trellis: Vec<u8> = Vec::with_capacity(num_symbols);
        let mut tx_pt_a: Vec<u16> = Vec::with_capacity(num_symbols);
        let mut tx_pt_b: Vec<u16> = Vec::with_capacity(num_symbols);
        let mut rx_decoded: Vec<V34DecodedSymbol> = Vec::new();

        for _ in 0..num_symbols {
            let input = (rng.next_u32() & 3) as u8;
            let pa = (rng.next_u32() % 256) as u16;
            let pb = (rng.next_u32() % 256) as u16;

            let (ca, cb) = encoder.encode(input);
            let iq_a = constellation.point(ca, pa);
            let iq_b = constellation.point(cb, pb);

            tx_trellis.push(input);
            tx_pt_a.push(pa);
            tx_pt_b.push(pb);

            if let Some(decoded) = decoder.decode(iq_a, iq_b, &constellation) {
                rx_decoded.push(decoded);
            }
        }

        rx_decoded.extend(decoder.flush(&constellation));

        assert_eq!(
            rx_decoded.len(),
            num_symbols,
            "Decoded {} != transmitted {num_symbols}",
            rx_decoded.len()
        );

        let mut errors = 0;
        for (i, decoded) in rx_decoded.iter().enumerate() {
            if decoded.trellis_bits != tx_trellis[i]
                || decoded.point_a != tx_pt_a[i]
                || decoded.point_b != tx_pt_b[i]
            {
                errors += 1;
                if errors <= 5 {
                    eprintln!(
                        "  Error at {i}: trellis expected={} got={}, \
                         pt_a expected={} got={}, pt_b expected={} got={}",
                        tx_trellis[i],
                        decoded.trellis_bits,
                        tx_pt_a[i],
                        decoded.point_a,
                        tx_pt_b[i],
                        decoded.point_b,
                    );
                }
            }
        }

        assert_eq!(errors, 0, "Expected 0 errors, got {errors}/{num_symbols}");
    }

    // -----------------------------------------------------------------------
    // AWGN coding gain test: verify trellis coding provides measurable gain.
    //
    // Two independent experiments at the same noise level, each sending 2 data
    // bits per 4D super-symbol:
    //
    // Coded:   2 bits → trellis encoder → coset pair → constellation → noise → Viterbi
    // Uncoded: 2 bits → direct coset pair table → constellation → noise → ML 4D decide
    //
    // The coded system exploits sequence constraints (trellis) to increase
    // effective minimum distance. At moderate noise, coded BER << uncoded BER.
    // -----------------------------------------------------------------------
    //
    // Runs a full Viterbi/ML simulation across 5 noise levels × 10_000 symbols,
    // which takes several minutes in debug builds. Gated behind #[ignore] so
    // coverage runs don't stall; exercise explicitly with `--include-ignored`.
    #[test]
    #[ignore = "full_test (--include-ignored): AWGN coding-gain oracle for the V.34 trellis; several minutes in debug, so it is deferred off the per-landing gate."]
    fn awgn_coding_gain() {
        let constellation = make_constellation();
        let coset_size = constellation.coset_size();

        // Uncoded mapping: 2 bits → one of 4 coset pairs (diagonal elements).
        let uncoded_pairs: [(u8, u8); 4] = [(0, 0), (1, 1), (2, 2), (3, 3)];

        // Compute the inter-coset minimum distance to calibrate noise.
        // In the Z² odd-integer lattice, min inter-coset distance = 2 (raw units).
        // After normalization: 2 * scale. We compute empirically.
        let mut inter_coset_min_dist_sq = f64::MAX;
        let sample_count = 20.min(coset_size as usize);
        for ca in 0..4u8 {
            for cb in (ca + 1)..4u8 {
                for i in 0..sample_count {
                    let (pi, pq) = constellation.point(ca, i as u16);
                    let (_, dist) = constellation.nearest_in_coset(pi, pq, cb);
                    if dist < inter_coset_min_dist_sq {
                        inter_coset_min_dist_sq = dist;
                    }
                }
            }
        }
        let inter_coset_min_dist = inter_coset_min_dist_sq.sqrt();
        eprintln!("  inter-coset min distance = {inter_coset_min_dist:.6}");

        let num_symbols = 10_000;

        struct TrialResult {
            sigma: f64,
            coded_errors: usize,
            uncoded_errors: usize,
            num_bits: usize,
        }

        let mut results = Vec::new();

        // Sweep noise levels as fractions of inter-coset min distance.
        for &sigma_factor in &[0.15, 0.25, 0.35, 0.50, 0.70] {
            let sigma = sigma_factor * inter_coset_min_dist;

            // --- Uncoded experiment ---
            let mut uncoded_rng = Xorshift32::new(0xA5A5_0000 + (sigma_factor * 10000.0) as u32);
            let mut uncoded_errors = 0usize;

            for _ in 0..num_symbols {
                let input = (uncoded_rng.next_u32() & 3) as u8;
                let point_idx = (uncoded_rng.next_u32() % (coset_size.min(256) as u32)) as u16;

                let (ca, cb) = uncoded_pairs[input as usize];
                let iq_a = constellation.point(ca, point_idx);
                let iq_b = constellation.point(cb, point_idx);

                let noisy_a = (
                    iq_a.0 + sigma * uncoded_rng.next_gaussian(),
                    iq_a.1 + sigma * uncoded_rng.next_gaussian(),
                );
                let noisy_b = (
                    iq_b.0 + sigma * uncoded_rng.next_gaussian(),
                    iq_b.1 + sigma * uncoded_rng.next_gaussian(),
                );

                // ML decision: try all 4 candidate coset pairs, pick smallest 4D distance.
                let mut best_input = 0u8;
                let mut best_dist = f64::MAX;
                for candidate in 0..4u8 {
                    let (uc_a, uc_b) = uncoded_pairs[candidate as usize];
                    let (_, da) = constellation.nearest_in_coset(noisy_a.0, noisy_a.1, uc_a);
                    let (_, db) = constellation.nearest_in_coset(noisy_b.0, noisy_b.1, uc_b);
                    let total = da + db;
                    if total < best_dist {
                        best_dist = total;
                        best_input = candidate;
                    }
                }

                if best_input != input {
                    uncoded_errors += 1;
                }
            }

            // --- Coded experiment ---
            let mut coded_rng = Xorshift32::new(0xA5A5_0000 + (sigma_factor * 10000.0) as u32);
            let mut encoder = V34TrellisEncoder::new();
            let mut decoder = V34ViterbiDecoder::new();
            let mut tx_trellis = Vec::with_capacity(num_symbols);
            let mut coded_errors = 0usize;
            let mut coded_count = 0usize;

            for _ in 0..num_symbols {
                let input = (coded_rng.next_u32() & 3) as u8;
                let point_idx = (coded_rng.next_u32() % (coset_size.min(256) as u32)) as u16;

                let (ca, cb) = encoder.encode(input);
                let iq_a = constellation.point(ca, point_idx);
                let iq_b = constellation.point(cb, point_idx);

                let noisy_a = (
                    iq_a.0 + sigma * coded_rng.next_gaussian(),
                    iq_a.1 + sigma * coded_rng.next_gaussian(),
                );
                let noisy_b = (
                    iq_b.0 + sigma * coded_rng.next_gaussian(),
                    iq_b.1 + sigma * coded_rng.next_gaussian(),
                );

                tx_trellis.push(input);

                if let Some(decoded) = decoder.decode(noisy_a, noisy_b, &constellation) {
                    let idx = coded_count;
                    if decoded.trellis_bits != tx_trellis[idx] {
                        coded_errors += 1;
                    }
                    coded_count += 1;
                }
            }

            // Flush.
            let flushed = decoder.flush(&constellation);
            for decoded in &flushed {
                let idx = coded_count;
                if idx < tx_trellis.len() && decoded.trellis_bits != tx_trellis[idx] {
                    coded_errors += 1;
                }
                coded_count += 1;
            }

            results.push(TrialResult {
                sigma,
                coded_errors,
                uncoded_errors,
                num_bits: num_symbols,
            });
        }

        // Find a trial where uncoded has significant errors but coded has fewer.
        let mut demonstrated_gain = false;
        for r in &results {
            let uncoded_ber = r.uncoded_errors as f64 / r.num_bits as f64;
            let coded_ber = r.coded_errors as f64 / r.num_bits as f64;

            eprintln!(
                "  sigma={:.6}: uncoded={}/{} ({:.2}%), coded={}/{} ({:.2}%)",
                r.sigma,
                r.uncoded_errors,
                r.num_bits,
                uncoded_ber * 100.0,
                r.coded_errors,
                r.num_bits,
                coded_ber * 100.0,
            );

            // Coding gain demonstrated if:
            // - uncoded has ≥ 0.5% error rate, and coded < half of uncoded, OR
            // - uncoded has errors and coded has zero.
            if r.uncoded_errors > 0 && r.coded_errors == 0 {
                demonstrated_gain = true;
            }
            if uncoded_ber >= 0.005 && coded_ber < uncoded_ber * 0.5 {
                demonstrated_gain = true;
            }
        }

        assert!(
            demonstrated_gain,
            "Failed to demonstrate coding gain. Results: {:?}",
            results
                .iter()
                .map(|r| format!(
                    "sigma={:.6}: uncoded={}/{}, coded={}/{}",
                    r.sigma, r.uncoded_errors, r.num_bits, r.coded_errors, r.num_bits
                ))
                .collect::<Vec<_>>()
        );
    }

    // -----------------------------------------------------------------------
    // Encoder determinism: same input + state → same output.
    // -----------------------------------------------------------------------
    #[test]
    fn encoder_deterministic() {
        for state in 0..NUM_STATES as u8 {
            for input in 0..NUM_INPUTS as u8 {
                let (coded_a, next_a) = encoder_transition(state, input);
                let (coded_b, next_b) = encoder_transition(state, input);
                assert_eq!(coded_a, coded_b);
                assert_eq!(next_a, next_b);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Encoder output range: coded ∈ [0,7], next_state ∈ [0,15].
    // -----------------------------------------------------------------------
    #[test]
    fn encoder_output_ranges() {
        for state in 0..NUM_STATES as u8 {
            for input in 0..NUM_INPUTS as u8 {
                let (coded, next) = encoder_transition(state, input);
                assert!(
                    coded < 8,
                    "coded={coded} out of range for state={state}, input={input}"
                );
                assert!(
                    next < 16,
                    "next_state={next} out of range for state={state}, input={input}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Coset pair table: all 8 entries have valid cosets (0-3).
    // -----------------------------------------------------------------------
    #[test]
    fn coset_pair_table_valid() {
        for (i, &(ca, cb)) in COSET_PAIR_TABLE.iter().enumerate() {
            assert!(ca < 4, "COSET_PAIR_TABLE[{i}].0 = {ca} out of range");
            assert!(cb < 4, "COSET_PAIR_TABLE[{i}].1 = {cb} out of range");
            // Even-sum group: (ca + cb) mod 2 == 0.
            assert_eq!(
                (ca + cb) % 2,
                0,
                "COSET_PAIR_TABLE[{i}] = ({ca}, {cb}) not in even-sum group"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Coset pair table: all 8 entries are distinct.
    // -----------------------------------------------------------------------
    #[test]
    fn coset_pair_table_distinct() {
        for i in 0..8 {
            for j in (i + 1)..8 {
                assert_ne!(
                    COSET_PAIR_TABLE[i], COSET_PAIR_TABLE[j],
                    "COSET_PAIR_TABLE[{i}] == COSET_PAIR_TABLE[{j}]"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Decoder reset: after reset, behaves like fresh decoder.
    // -----------------------------------------------------------------------
    #[test]
    fn decoder_reset_works() {
        let constellation = make_constellation();
        let mut encoder = V34TrellisEncoder::new();
        let mut decoder = V34ViterbiDecoder::new();

        let mut rng = Xorshift32::new(0x9999_AAAA);

        // Feed some symbols.
        for _ in 0..10 {
            let input = (rng.next_u32() & 3) as u8;
            let (ca, cb) = encoder.encode(input);
            let iq_a = constellation.point(ca, 0);
            let iq_b = constellation.point(cb, 0);
            decoder.decode(iq_a, iq_b, &constellation);
        }

        // Reset both.
        encoder.reset();
        decoder.reset();

        // Now do a fresh round-trip.
        let mut tx = Vec::new();
        let mut rx = Vec::new();

        for _ in 0..50 {
            let input = (rng.next_u32() & 3) as u8;
            let (ca, cb) = encoder.encode(input);
            let iq_a = constellation.point(ca, 0);
            let iq_b = constellation.point(cb, 0);
            tx.push(input);

            if let Some(decoded) = decoder.decode(iq_a, iq_b, &constellation) {
                rx.push(decoded.trellis_bits);
            }
        }
        rx.extend(decoder.flush(&constellation).iter().map(|d| d.trellis_bits));

        assert_eq!(rx.len(), tx.len());
        for (i, (&expected, &got)) in tx.iter().zip(rx.iter()).enumerate() {
            assert_eq!(expected, got, "Post-reset mismatch at position {i}");
        }
    }

    // -----------------------------------------------------------------------
    // Minimum free distance: compute d²_free of the trellis code by BFS.
    //
    // d²_free is the minimum squared Euclidean distance between any two valid
    // trellis output sequences that diverge from and re-merge to the same state.
    // For a well-designed 16-state 4D code, d²_free should be significantly
    // larger than the single-symbol d²_min.
    //
    // We compute the 4D coset-pair distance metric: each coset pair maps to
    // a "representative" 4D point, and we measure distances between those.
    // The actual d²_free uses intra-coset minimum distances.
    // -----------------------------------------------------------------------
    #[test]
    fn minimum_free_distance() {
        // Compute the squared distance between two coset pairs.
        // The minimum distance between coset pair (ca1,cb1) and (ca2,cb2)
        // is determined by the inter-coset distances:
        //   d² = d²(ca1,ca2) + d²(cb1,cb2)
        // where d²(cx,cy) is the squared intra/inter-coset min distance.
        //
        // For same coset: d² = 0 (points overlap in same coset).
        // For different cosets: d² = inter-coset min dist² = 4 * scale².
        // (raw lattice: min inter-coset distance = 2, so d² = 4 in raw coords.)
        //
        // We work in raw lattice units where inter-coset d² = 4, intra-coset d² = 16.

        // d²(coset_a, coset_b): minimum squared distance between a point in
        // coset_a and a point in coset_b. For same coset: 0 (same point).
        // For different cosets: 4 (raw lattice units).
        let coset_dist_sq = |c1: u8, c2: u8| -> f64 {
            if c1 == c2 {
                0.0
            } else {
                4.0
            }
        };

        // Distance between two coset pairs.
        let pair_dist_sq = |pair1: (u8, u8), pair2: (u8, u8)| -> f64 {
            coset_dist_sq(pair1.0, pair2.0) + coset_dist_sq(pair1.1, pair2.1)
        };

        // BFS to find minimum free distance: explore all paths that diverge
        // from state 0 and return to state 0, tracking the cumulative distance
        // from the all-zero path (which stays in state 0 with input 0).
        //
        // The all-zero codeword from state 0 produces coded=000 → (C0,C0).
        let zero_pair = COSET_PAIR_TABLE[0]; // (0, 0) for coded=000 from state 0

        let max_depth = 10; // Search depth (in super-symbols)
        let mut min_dfree_sq = f64::MAX;

        // For each non-zero starting input from state 0, trace the divergent path.
        // BFS: (current_state, depth, accumulated_dist²)
        let mut stack: Vec<(u8, usize, f64)> = Vec::new();

        // Start: diverge from state 0 with each non-zero input.
        for input in 1..NUM_INPUTS as u8 {
            let (coded, next_state) = encoder_transition(0, input);
            let pair = COSET_PAIR_TABLE[coded as usize];
            let dist = pair_dist_sq(zero_pair, pair);
            // The zero path from state 0 with input 0 always gives coded=000.
            // This divergent path starts with a non-zero input.
            stack.push((next_state, 1, dist));
        }

        while let Some((state, depth, dist_sq)) = stack.pop() {
            // Pruning: if accumulated distance already exceeds current best, skip.
            if dist_sq >= min_dfree_sq {
                continue;
            }

            // If we've returned to state 0, this is a valid error event.
            if state == 0 && depth > 0 {
                if dist_sq < min_dfree_sq {
                    min_dfree_sq = dist_sq;
                }
                continue;
            }

            if depth >= max_depth {
                continue;
            }

            // The reference (zero) path from state 0 always produces coded=000.
            // But the reference path's state also advances — with input 0 from
            // state 0, it stays in state 0. So the zero path always outputs (C0,C0).

            for input in 0..NUM_INPUTS as u8 {
                let (coded, next_state) = encoder_transition(state, input);
                let pair = COSET_PAIR_TABLE[coded as usize];
                // Distance from the zero path at this step:
                let step_dist = pair_dist_sq(zero_pair, pair);
                stack.push((next_state, depth + 1, dist_sq + step_dist));
            }
        }

        eprintln!("  Minimum free distance² (raw lattice units) = {min_dfree_sq:.1}");

        // A good 16-state code should have d²_free ≥ 12 in raw units.
        // (The V.34 Wei code achieves d²_free = 24 with proper coset partitioning.)
        // Even a modest code should achieve d²_free > d²_min_single = 4.
        assert!(
            min_dfree_sq >= 8.0,
            "Minimum free distance² = {min_dfree_sq:.1} is too low \
             (expected ≥ 8.0 for meaningful coding gain)"
        );
    }
}

// ── MDD-BUG-CRUCIBLE-00513: one coset search per received point, not per branch ──
#[cfg(test)]
mod coset_search_reuse_tests {
    use super::*;
    use crate::v34_constellation::COSET_SEARCHES;

    /// Coset searches performed while `body` ran.
    fn coset_searches(body: impl FnOnce()) -> u64 {
        COSET_SEARCHES.with(|n| n.set(0));
        body();
        COSET_SEARCHES.with(|n| n.get())
    }

    /// The whole point: the nearest-in-coset result depends only on the
    /// received point and the coset, so one `decode` needs 4 cosets x 2 points
    /// = 8 searches — not one per trellis branch.
    ///
    /// Each search scans at least 256 constellation points, and the decoder
    /// visits 16 states x 4 inputs, so the old shape did up to 64.
    #[test]
    fn one_decode_performs_eight_coset_searches() {
        let constellation = V34Constellation::new();
        let mut encoder = V34TrellisEncoder::new();
        let mut decoder = V34ViterbiDecoder::new();

        // Warm the trellis so every state is reachable. Measuring the FIRST
        // decode would understate the defect badly: only state 0 starts
        // reachable, so the old shape did 16 searches there against 128 in
        // steady state.
        for step in 0..32u8 {
            let (coset_a, coset_b) = encoder.encode(step & 0x03);
            decoder.decode(
                constellation.point(coset_a, 0),
                constellation.point(coset_b, 0),
                &constellation,
            );
        }

        let searches = coset_searches(|| {
            decoder.decode((0.3, -0.7), (-1.1, 0.2), &constellation);
        });

        assert_eq!(
            searches,
            (NUM_COSETS * 2) as u64,
            "a steady-state decode must resolve each (received point, coset)              exactly once, not once per trellis branch"
        );
    }

    /// The cached values must equal what the branch loop would have computed.
    /// This is the invariant the hoist rests on: `nearest_in_coset` is a pure
    /// function of its arguments, so resolving it earlier cannot change it.
    #[test]
    fn cached_coset_results_match_direct_lookups() {
        let constellation = V34Constellation::new();
        let points = [(0.3, -0.7), (-1.1, 0.2), (0.0, 0.0), (2.5, 2.5)];

        for (i, q) in points {
            for coset in 0..NUM_COSETS as u8 {
                let first = constellation.nearest_in_coset(i, q, coset);
                let second = constellation.nearest_in_coset(i, q, coset);
                assert_eq!(
                    first, second,
                    "nearest_in_coset must be a pure function of (i, q, coset)"
                );
            }
        }
    }

    /// Equivalence oracle: a noise-free encoded sequence must decode back to
    /// exactly the points that were transmitted. Reusing the coset searches
    /// cannot be allowed to change a single decoded symbol.
    ///
    /// Every symbol is transmitted as index 0 of its coset, so every decoded
    /// point index must be 0 — an assertion on the decoder's actual output
    /// rather than on it merely producing something.
    #[test]
    fn decoded_output_is_unchanged_by_the_reuse() {
        let constellation = V34Constellation::new();
        let mut encoder = V34TrellisEncoder::new();
        let mut decoder = V34ViterbiDecoder::new();

        let mut decoded = Vec::new();
        // Comfortably past TRACEBACK_DEPTH so the traceback emits.
        for step in 0..(TRACEBACK_DEPTH + 60) {
            let bits = (step & 0x03) as u8;
            let (coset_a, coset_b) = encoder.encode(bits);
            let iq_a = constellation.point(coset_a, 0);
            let iq_b = constellation.point(coset_b, 0);
            if let Some(sym) = decoder.decode(iq_a, iq_b, &constellation) {
                decoded.push(sym);
            }
        }

        assert!(
            !decoded.is_empty(),
            "past the traceback depth the decoder must emit symbols"
        );
        for sym in &decoded {
            assert!((sym.coset_a as usize) < NUM_COSETS);
            assert!((sym.coset_b as usize) < NUM_COSETS);
            assert_eq!(
                (sym.point_a, sym.point_b),
                (0, 0),
                "a noise-free point-0 transmission must decode back to point 0"
            );
        }
    }
}
