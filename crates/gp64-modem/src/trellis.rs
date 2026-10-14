//! V.32bis Trellis encoder and Viterbi decoder.
//!
//! 8-state convolutional encoder (rate 2/3) adds one coded bit per symbol.
//! The encoder takes 4 data bits per symbol: the 2 MSBs pass through uncoded,
//! the 2 LSBs are convolutionally encoded to 3 bits, producing 5 coded bits
//! total that select one of 32 constellation points.
//!
//! The Viterbi decoder uses hard-decision or soft-decision (Euclidean distance)
//! branch metrics with traceback depth of 15 symbols.

/// Traceback depth: 5x constraint length (constraint length = 3 for 8 states).
const TRACEBACK_DEPTH: usize = 15;

/// Number of encoder states.
const NUM_STATES: usize = 8;

/// 8-state Trellis encoder for V.32bis.
///
/// Systematic rate-2/3 convolutional encoder:
/// - 3-bit state register: `[s0, s1, s2]`
/// - Input: 2 bits `(b0, b1)` (the 2 LSBs of the 4-bit data)
/// - Parity bit: `p = b0 XOR s0 XOR s2`
/// - Coded output: `(b0, b1, p)` — 3 bits
/// - Next state: `[b0, s0, s1]` — shift `b0` in, shift `s2` out
///
/// The 2 MSBs of the 4-bit input pass through uncoded, producing 5 total bits.
pub struct TrellisEncoder {
    state: u8, // 3-bit state (0-7)
}

impl TrellisEncoder {
    pub fn new() -> Self {
        Self { state: 0 }
    }

    /// Encode 4 data bits into 5 coded bits for 32-QAM.
    ///
    /// Input: 4 bits `(b3, b2, b1, b0)` in the low 4 bits of `data_bits`.
    /// Output: 5 bits `(b3, b2, y2, y1, y0)` in the low 5 bits of return value.
    ///
    /// The top 2 bits `(b3, b2)` pass through uncoded.
    /// The bottom 2 bits `(b1, b0)` are convolutionally encoded to 3 bits `(y2, y1, y0)`.
    pub fn encode(&mut self, data_bits: u8) -> u8 {
        let b0 = data_bits & 1;
        let b1 = (data_bits >> 1) & 1;
        let b2 = (data_bits >> 2) & 1;
        let b3 = (data_bits >> 3) & 1;

        let s0 = self.state & 1;
        let s1 = (self.state >> 1) & 1;
        let s2 = (self.state >> 2) & 1;

        // Parity: p = b0 XOR s0 XOR s2
        let p = b0 ^ s0 ^ s2;

        // Coded output bits: y0 = p, y1 = b0, y2 = b1 (systematic)
        let y0 = p;
        let y1 = b0;
        let y2 = b1;

        // Next state: [b0, s0, s1] — shift b0 in, shift s2 out
        self.state = b0 | (s0 << 1) | (s1 << 2);

        // Assemble 5-bit output: (b3, b2, y2, y1, y0)
        y0 | (y1 << 1) | (y2 << 2) | (b2 << 3) | (b3 << 4)
    }

    /// Reset encoder state to zero.
    pub fn reset(&mut self) {
        self.state = 0;
    }

    /// Current state (0-7).
    pub fn state(&self) -> u8 {
        self.state
    }
}

impl Default for TrellisEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute the encoder outputs for a given state and 2-bit input.
///
/// Returns `(coded_3bits, next_state)` where `coded_3bits` = `(y2, y1, y0)`.
fn encoder_transition(state: u8, b0: u8, b1: u8) -> (u8, u8) {
    let s0 = state & 1;
    let s1 = (state >> 1) & 1;
    let s2 = (state >> 2) & 1;

    let p = b0 ^ s0 ^ s2;
    let y0 = p;
    let y1 = b0;
    let y2 = b1;

    let next_state = b0 | (s0 << 1) | (s1 << 2);
    let coded = y0 | (y1 << 1) | (y2 << 2);

    (coded, next_state)
}

/// Viterbi decoder for the 8-state V.32bis Trellis code.
///
/// Uses add-compare-select with either hard-decision (Hamming distance) or
/// soft-decision (Euclidean distance) branch metrics. Traceback depth is 15
/// symbols.
pub struct ViterbiDecoder {
    /// Path metrics for each state.
    path_metrics: [f64; NUM_STATES],
    /// Survivor paths: `survivors[time % capacity][state] = (previous_state, decoded_2bits)`.
    survivors: Vec<[(u8, u8); NUM_STATES]>,
    /// Uncoded bits received at each time step, stored so traceback can
    /// retrieve the uncoded bits corresponding to the oldest symbol.
    uncoded_history: Vec<u8>,
    /// Traceback depth.
    traceback_depth: usize,
    /// Number of symbols fed so far.
    time: usize,
}

impl ViterbiDecoder {
    pub fn new() -> Self {
        let traceback_depth = TRACEBACK_DEPTH;
        // Need traceback_depth + 2 slots: the traceback walks traceback_depth + 1
        // transitions (from the current time back to the oldest symbol).
        let capacity = traceback_depth + 2;
        Self {
            path_metrics: {
                let mut m = [f64::MAX; NUM_STATES];
                m[0] = 0.0; // Start in state 0.
                m
            },
            survivors: vec![[(0u8, 0u8); NUM_STATES]; capacity],
            uncoded_history: vec![0u8; capacity],
            traceback_depth,
            time: 0,
        }
    }

    /// Feed one received symbol (hard-decision decode).
    ///
    /// `coded_bits`: the 5-bit coded symbol index. The top 2 bits are uncoded
    /// and pass through; the bottom 3 bits are decoded via Viterbi.
    ///
    /// Returns decoded 4-bit data after traceback delay, or `None` during the
    /// initial warmup period.
    pub fn decode(&mut self, coded_bits: u8) -> Option<u8> {
        let uncoded = (coded_bits >> 3) & 0x03; // b3, b2
        let received_coded = coded_bits & 0x07; // y2, y1, y0

        self.step_acs_hard(received_coded, uncoded)
    }

    /// Soft-decision decode: takes the received I/Q and finds the best path
    /// considering Euclidean distances to all possible transmitted points.
    ///
    /// Returns decoded 4-bit data after traceback delay, or `None` during warmup.
    pub fn decode_soft(
        &mut self,
        received_i: f64,
        received_q: f64,
        constellation: &super::qam::Constellation,
    ) -> Option<u8> {
        let mut new_metrics = [f64::MAX; NUM_STATES];
        let capacity = self.survivors.len();
        let slot = self.time % capacity;
        let mut new_survivors = [(0u8, 0u8); NUM_STATES];
        let mut best_uncoded = 0u8;
        let mut best_global_metric = f64::MAX;

        for prev_state in 0..NUM_STATES as u8 {
            if self.path_metrics[prev_state as usize] == f64::MAX {
                continue;
            }

            // Try all 4 possible 2-bit coded inputs.
            for coded_input in 0..4u8 {
                let b0 = coded_input & 1;
                let b1 = (coded_input >> 1) & 1;

                let (coded_3, next_state) = encoder_transition(prev_state, b0, b1);

                // Try all 4 possible uncoded bit pairs.
                for uncoded in 0..4u8 {
                    let symbol_index = coded_3 | (uncoded << 3);
                    let (pi, pq) = constellation.point(symbol_index);
                    let di = received_i - pi;
                    let dq = received_q - pq;
                    let dist_sq = di * di + dq * dq;

                    let metric = self.path_metrics[prev_state as usize] + dist_sq;

                    if metric < new_metrics[next_state as usize] {
                        new_metrics[next_state as usize] = metric;
                        new_survivors[next_state as usize] = (prev_state, coded_input);
                    }

                    // Track global best to determine uncoded bits for this time step.
                    if metric < best_global_metric {
                        best_global_metric = metric;
                        best_uncoded = uncoded;
                    }
                }
            }
        }

        self.path_metrics = new_metrics;
        self.survivors[slot] = new_survivors;
        self.uncoded_history[slot] = best_uncoded;
        self.time += 1;

        if self.time > self.traceback_depth {
            Some(self.traceback())
        } else {
            None
        }
    }

    /// Flush remaining symbols from the traceback buffer.
    ///
    /// Call after the last symbol has been fed. Returns the decoded 4-bit
    /// values that were still buffered in the traceback pipeline. Zero-input
    /// symbols are assumed for flushing.
    pub fn flush(&mut self) -> Vec<u8> {
        let mut result = Vec::new();

        // Feed traceback_depth zero symbols to flush the pipeline.
        for _ in 0..self.traceback_depth {
            if let Some(decoded) = self.decode(0) {
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
            *entry = [(0, 0); NUM_STATES];
        }
        for entry in self.uncoded_history.iter_mut() {
            *entry = 0;
        }
        self.time = 0;
    }

    /// Perform the add-compare-select step for hard-decision decoding.
    fn step_acs_hard(&mut self, received_coded: u8, uncoded: u8) -> Option<u8> {
        let mut new_metrics = [f64::MAX; NUM_STATES];
        let capacity = self.survivors.len();
        let slot = self.time % capacity;
        let mut new_survivors = [(0u8, 0u8); NUM_STATES];

        for prev_state in 0..NUM_STATES as u8 {
            if self.path_metrics[prev_state as usize] == f64::MAX {
                continue;
            }

            // Try all 4 possible 2-bit inputs.
            for input in 0..4u8 {
                let b0 = input & 1;
                let b1 = (input >> 1) & 1;

                let (coded, next_state) = encoder_transition(prev_state, b0, b1);

                // Hamming distance between received coded bits and expected coded bits.
                let hamming = (received_coded ^ coded).count_ones() as f64;
                let metric = self.path_metrics[prev_state as usize] + hamming;

                if metric < new_metrics[next_state as usize] {
                    new_metrics[next_state as usize] = metric;
                    new_survivors[next_state as usize] = (prev_state, input);
                }
            }
        }

        self.path_metrics = new_metrics;
        self.survivors[slot] = new_survivors;
        self.uncoded_history[slot] = uncoded;
        self.time += 1;

        if self.time > self.traceback_depth {
            Some(self.traceback())
        } else {
            None
        }
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
    /// 4-bit data at the traceback depth point.
    ///
    /// Retrieves both the coded 2-bit input and the uncoded 2-bit pair from
    /// the oldest buffered time step.
    fn traceback(&self) -> u8 {
        let capacity = self.survivors.len();

        // Start traceback from the best state at the most recent time step.
        let mut state = self.best_state();
        let mut decoded_input = 0u8;

        // Walk backwards through the survivor memory. We need traceback_depth + 1
        // iterations to reach back to the oldest symbol: the survivor at slot t
        // records the transition INTO time t (i.e., the input applied at t-1→t),
        // so to recover the input from the very first buffered time step we must
        // read one more slot than the traceback depth.
        for step in 0..=self.traceback_depth {
            let t = (self.time - 1 - step) % capacity;
            let (prev_state, input) = self.survivors[t][state as usize];
            if step == self.traceback_depth {
                decoded_input = input;
            }
            state = prev_state;
        }

        // The uncoded bits come from the oldest time step in the traceback.
        // oldest time index = self.time - 1 - traceback_depth
        let oldest_slot = (self.time - 1 - self.traceback_depth) % capacity;
        let uncoded = self.uncoded_history[oldest_slot];

        // Combine: uncoded bits (b3,b2) in bits [3:2], decoded (b1,b0) in bits [1:0].
        decoded_input | (uncoded << 2)
    }
}

impl Default for ViterbiDecoder {
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

    #[test]
    fn test_trellis_encode_preserves_top_bits() {
        // Top 2 bits of input should appear unchanged in output bits [4:3].
        let mut encoder = TrellisEncoder::new();

        for data in 0..16u8 {
            encoder.reset();
            let coded = encoder.encode(data);
            let input_top = (data >> 2) & 0x03;
            let output_top = (coded >> 3) & 0x03;
            assert_eq!(
                input_top, output_top,
                "Top bits mismatch for input {data:#06b}: coded={coded:#07b}, \
                 expected top bits {input_top:#04b}, got {output_top:#04b}"
            );
        }
    }

    #[test]
    fn test_trellis_encode_deterministic() {
        // Same input + same state should always produce the same output.
        for data in 0..16u8 {
            let mut enc_a = TrellisEncoder::new();
            let mut enc_b = TrellisEncoder::new();

            let out_a = enc_a.encode(data);
            let out_b = enc_b.encode(data);
            assert_eq!(
                out_a, out_b,
                "Determinism failure: input={data}, out_a={out_a:#07b}, out_b={out_b:#07b}"
            );
        }
    }

    #[test]
    fn test_trellis_encode_state_advances() {
        let mut encoder = TrellisEncoder::new();
        assert_eq!(encoder.state(), 0);

        // Encode a non-zero value — state should change.
        encoder.encode(0x01); // b0=1
        assert_ne!(
            encoder.state(),
            0,
            "State should change after encoding a non-zero input"
        );

        // Encode another — state should change again (from non-zero start).
        let _state_after_first = encoder.state();
        encoder.encode(0x03); // b0=1, b1=1
        let state_after_second = encoder.state();
        assert!(
            state_after_second <= 7,
            "State should be 0-7, got {state_after_second}"
        );

        // Verify state changes: encode a sequence and check not all states are the same.
        encoder.reset();
        let mut states = vec![encoder.state()];
        for i in 0..8 {
            encoder.encode(i);
            states.push(encoder.state());
        }
        let unique_count = {
            let mut v = states.clone();
            v.sort();
            v.dedup();
            v.len()
        };
        assert!(
            unique_count > 1,
            "State should change across multiple encodes, but all states were the same: {states:?}"
        );
    }

    #[test]
    fn test_trellis_roundtrip() {
        // Encode N symbols, hard-decision decode, verify all match after warmup.
        let mut encoder = TrellisEncoder::new();
        let mut decoder = ViterbiDecoder::new();

        let data_sequence: Vec<u8> = (0..30).map(|i| (i * 3) % 16).collect();

        let mut coded_symbols = Vec::new();
        for &d in &data_sequence {
            coded_symbols.push(encoder.encode(d));
        }

        let mut decoded = Vec::new();
        for &c in &coded_symbols {
            if let Some(d) = decoder.decode(c) {
                decoded.push(d);
            }
        }

        // Flush remaining.
        decoded.extend(decoder.flush());

        // The decoded output should match the input (after traceback delay).
        // The total decoded length should equal the input length.
        assert_eq!(
            decoded.len(),
            data_sequence.len(),
            "Decoded length {} != input length {}",
            decoded.len(),
            data_sequence.len()
        );

        for (i, (&expected, &got)) in data_sequence.iter().zip(decoded.iter()).enumerate() {
            assert_eq!(
                expected, got,
                "Mismatch at position {i}: expected {expected:#06b}, got {got:#06b}"
            );
        }
    }

    #[test]
    fn test_trellis_roundtrip_long() {
        // Encode 100 random-ish 4-bit values, decode, verify zero errors.
        let mut encoder = TrellisEncoder::new();
        let mut decoder = ViterbiDecoder::new();

        // Pseudo-random sequence (deterministic, no rand dependency).
        let mut seed: u32 = 0xDEAD_BEEF;
        let data_sequence: Vec<u8> = (0..100)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed & 0x0F) as u8
            })
            .collect();

        let mut coded_symbols = Vec::new();
        for &d in &data_sequence {
            coded_symbols.push(encoder.encode(d));
        }

        let mut decoded = Vec::new();
        for &c in &coded_symbols {
            if let Some(d) = decoder.decode(c) {
                decoded.push(d);
            }
        }
        decoded.extend(decoder.flush());

        assert_eq!(
            decoded.len(),
            data_sequence.len(),
            "Decoded length {} != input length {}",
            decoded.len(),
            data_sequence.len()
        );

        let mut errors = 0;
        for (i, (&expected, &got)) in data_sequence.iter().zip(decoded.iter()).enumerate() {
            if expected != got {
                errors += 1;
                eprintln!("  Error at position {i}: expected {expected:#06b}, got {got:#06b}");
            }
        }

        assert_eq!(errors, 0, "Expected 0 errors, got {errors}/100");
    }

    #[test]
    fn test_trellis_flush() {
        // After encoding, flush returns remaining buffered symbols.
        let mut encoder = TrellisEncoder::new();
        let mut decoder = ViterbiDecoder::new();

        let data_sequence: Vec<u8> = (0..10).map(|i| i % 16).collect();

        let mut coded_symbols = Vec::new();
        for &d in &data_sequence {
            coded_symbols.push(encoder.encode(d));
        }

        let mut decoded_before_flush = Vec::new();
        for &c in &coded_symbols {
            if let Some(d) = decoder.decode(c) {
                decoded_before_flush.push(d);
            }
        }

        // With 10 input symbols and traceback depth 15, nothing should have
        // been output yet before flush.
        assert!(
            decoded_before_flush.is_empty(),
            "Expected no output before flush for 10 symbols with traceback depth 15, \
             got {} symbols",
            decoded_before_flush.len()
        );

        let flushed = decoder.flush();

        // Total decoded should equal input length.
        let total_decoded = decoded_before_flush.len() + flushed.len();
        assert_eq!(
            total_decoded,
            data_sequence.len(),
            "Total decoded {total_decoded} != input length {}",
            data_sequence.len()
        );

        // Verify correctness.
        let all_decoded: Vec<u8> = decoded_before_flush
            .iter()
            .chain(flushed.iter())
            .copied()
            .collect();
        for (i, (&expected, &got)) in data_sequence.iter().zip(all_decoded.iter()).enumerate() {
            assert_eq!(
                expected, got,
                "Mismatch at position {i}: expected {expected:#06b}, got {got:#06b}"
            );
        }
    }

    #[test]
    fn test_viterbi_all_zeros() {
        // All-zero input should encode and decode correctly.
        let mut encoder = TrellisEncoder::new();
        let mut decoder = ViterbiDecoder::new();

        let data_sequence: Vec<u8> = vec![0u8; 30];

        let mut coded_symbols = Vec::new();
        for &d in &data_sequence {
            coded_symbols.push(encoder.encode(d));
        }

        // All-zero input from state 0 should produce all-zero coded output.
        for (i, &c) in coded_symbols.iter().enumerate() {
            assert_eq!(
                c, 0,
                "All-zero input should produce all-zero coded output, \
                 but symbol {i} = {c:#07b}"
            );
        }

        let mut decoded = Vec::new();
        for &c in &coded_symbols {
            if let Some(d) = decoder.decode(c) {
                decoded.push(d);
            }
        }
        decoded.extend(decoder.flush());

        assert_eq!(decoded.len(), data_sequence.len());

        for (i, &got) in decoded.iter().enumerate() {
            assert_eq!(got, 0, "Expected 0 at position {i}, got {got:#06b}");
        }
    }
}
