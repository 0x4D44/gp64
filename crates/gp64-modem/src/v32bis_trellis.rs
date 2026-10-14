//! V.32bis trellis encoder and Viterbi decoder for 14400 bps mode.
//!
//! This module implements the V.32bis rate-2/3 convolutional code specifically
//! for 128-QAM (14400 bps). The encoder maps 6 data bits per symbol interval
//! into 7 coded bits:
//!
//! - 2 LSBs are convolutionally encoded to 3 bits (8-state, same as 32-QAM trellis)
//! - 4 MSBs pass through uncoded
//! - Total: 7 bits = one 128-QAM constellation point
//!
//! This is kept **separate** from the V.34 trellis code (v34_trellis.rs) as
//! required by the HLD. The 8-state convolutional code is the same topology
//! as the 32-QAM trellis in trellis.rs, but the bit packing differs:
//! - 32-QAM: 4 data bits -> 5 coded bits (2 uncoded + 3 coded)
//! - 128-QAM: 6 data bits -> 7 coded bits (4 uncoded + 3 coded)

/// Traceback depth: 5x constraint length (constraint length = 3 for 8 states).
const TRACEBACK_DEPTH: usize = 15;

/// Number of encoder states.
const NUM_STATES: usize = 8;

/// 8-state trellis encoder for V.32bis 14400 bps (128-QAM).
///
/// Systematic rate-2/3 convolutional encoder:
/// - 3-bit state register: `[s0, s1, s2]`
/// - Input: 2 LSBs `(b0, b1)` of the 6-bit data
/// - Parity bit: `p = b0 XOR s0 XOR s2`
/// - Coded output: `(b0, b1, p)` — 3 bits
/// - Next state: `[b0, s0, s1]`
///
/// The 4 MSBs of the 6-bit input pass through uncoded, producing 7 total bits.
pub struct V32bisTrellisEncoder {
    state: u8, // 3-bit state (0-7)
}

impl V32bisTrellisEncoder {
    pub fn new() -> Self {
        Self { state: 0 }
    }

    /// Encode 6 data bits into 7 coded bits for 128-QAM.
    ///
    /// Input: 6 bits `(b5, b4, b3, b2, b1, b0)` in the low 6 bits of `data_bits`.
    /// Output: 7 bits `(b5, b4, b3, b2, y2, y1, y0)` in the low 7 bits of return value.
    ///
    /// The top 4 bits `(b5, b4, b3, b2)` pass through uncoded.
    /// The bottom 2 bits `(b1, b0)` are convolutionally encoded to 3 bits `(y2, y1, y0)`.
    pub fn encode(&mut self, data_bits: u8) -> u8 {
        let b0 = data_bits & 1;
        let b1 = (data_bits >> 1) & 1;
        let uncoded = (data_bits >> 2) & 0x0F; // b5, b4, b3, b2

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

        // Assemble 7-bit output: (b5, b4, b3, b2, y2, y1, y0)
        y0 | (y1 << 1) | (y2 << 2) | (uncoded << 3)
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

impl Default for V32bisTrellisEncoder {
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

/// Viterbi decoder for the 8-state V.32bis trellis code at 14400 bps (128-QAM).
///
/// Uses add-compare-select with hard-decision or soft-decision branch metrics.
/// Traceback depth is 15 symbols. The decoder handles the 128-QAM bit layout:
/// 3 coded LSBs + 4 uncoded MSBs = 7 total bits per symbol.
pub struct V32bisViterbiDecoder {
    /// Path metrics for each state.
    path_metrics: [f64; NUM_STATES],
    /// Survivor paths: `survivors[time % capacity][state] = (previous_state, decoded_2bits)`.
    survivors: Vec<[(u8, u8); NUM_STATES]>,
    /// Uncoded bits per state at each time step.  In soft-decision mode each
    /// state may have arrived via a different uncoded value, so we must store
    /// the winning uncoded value for every state independently.
    uncoded_history: Vec<[u8; NUM_STATES]>,
    /// Traceback depth.
    traceback_depth: usize,
    /// Number of symbols fed so far.
    time: usize,
}

impl V32bisViterbiDecoder {
    pub fn new() -> Self {
        let traceback_depth = TRACEBACK_DEPTH;
        let capacity = traceback_depth + 2;
        Self {
            path_metrics: {
                let mut m = [f64::MAX; NUM_STATES];
                m[0] = 0.0;
                m
            },
            survivors: vec![[(0u8, 0u8); NUM_STATES]; capacity],
            uncoded_history: vec![[0u8; NUM_STATES]; capacity],
            traceback_depth,
            time: 0,
        }
    }

    /// Feed one received symbol (hard-decision decode).
    ///
    /// `coded_bits`: the 7-bit coded symbol index. The top 4 bits are uncoded
    /// and pass through; the bottom 3 bits are decoded via Viterbi.
    ///
    /// Returns decoded 6-bit data after traceback delay, or `None` during the
    /// initial warmup period.
    pub fn decode(&mut self, coded_bits: u8) -> Option<u8> {
        let uncoded = (coded_bits >> 3) & 0x0F; // b5, b4, b3, b2
        let received_coded = coded_bits & 0x07; // y2, y1, y0

        self.step_acs_hard(received_coded, uncoded)
    }

    /// Soft-decision decode: takes the received I/Q and finds the best path
    /// considering Euclidean distances to all possible transmitted points.
    ///
    /// Returns decoded 6-bit data after traceback delay, or `None` during warmup.
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
        let mut new_uncoded = [0u8; NUM_STATES];

        for prev_state in 0..NUM_STATES as u8 {
            if self.path_metrics[prev_state as usize] == f64::MAX {
                continue;
            }

            // Try all 4 possible 2-bit coded inputs.
            for coded_input in 0..4u8 {
                let b0 = coded_input & 1;
                let b1 = (coded_input >> 1) & 1;

                let (coded_3, next_state) = encoder_transition(prev_state, b0, b1);

                // Try all 16 possible uncoded 4-bit values.
                for uncoded in 0..16u8 {
                    let symbol_index = coded_3 | (uncoded << 3);
                    if (symbol_index as usize) >= constellation.num_points() {
                        continue;
                    }
                    let (pi, pq) = constellation.point(symbol_index);
                    let di = received_i - pi;
                    let dq = received_q - pq;
                    let dist_sq = di * di + dq * dq;

                    let metric = self.path_metrics[prev_state as usize] + dist_sq;

                    if metric < new_metrics[next_state as usize] {
                        new_metrics[next_state as usize] = metric;
                        new_survivors[next_state as usize] = (prev_state, coded_input);
                        new_uncoded[next_state as usize] = uncoded;
                    }
                }
            }
        }

        self.path_metrics = new_metrics;
        self.survivors[slot] = new_survivors;
        self.uncoded_history[slot] = new_uncoded;
        self.time += 1;

        if self.time > self.traceback_depth {
            Some(self.traceback())
        } else {
            None
        }
    }

    /// Flush remaining symbols from the traceback buffer.
    pub fn flush(&mut self) -> Vec<u8> {
        let mut result = Vec::new();
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
            *entry = [0u8; NUM_STATES];
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

            for input in 0..4u8 {
                let b0 = input & 1;
                let b1 = (input >> 1) & 1;

                let (coded, next_state) = encoder_transition(prev_state, b0, b1);

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
        // Hard-decision: the uncoded value is unambiguous (directly from the
        // received symbol), so every state gets the same value.
        self.uncoded_history[slot] = [uncoded; NUM_STATES];
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
    /// 6-bit data at the traceback depth point.
    fn traceback(&self) -> u8 {
        let capacity = self.survivors.len();

        let mut state = self.best_state();
        let mut decoded_input = 0u8;
        // The state at the oldest time slot (after the full traceback walk) is
        // the one whose uncoded value we need.
        let mut oldest_state = state;

        for step in 0..=self.traceback_depth {
            let t = (self.time - 1 - step) % capacity;
            let (prev_state, input) = self.survivors[t][state as usize];
            if step == self.traceback_depth {
                decoded_input = input;
                // `state` at this point is the state that was active at the
                // oldest time slot — its uncoded value is the correct one.
                oldest_state = state;
            }
            state = prev_state;
        }

        // Read the uncoded value for the state that the surviving path
        // actually passed through at the oldest time step.
        let oldest_slot = (self.time - 1 - self.traceback_depth) % capacity;
        let uncoded = self.uncoded_history[oldest_slot][oldest_state as usize];

        // Combine: uncoded 4 bits (b5,b4,b3,b2) in bits [5:2], decoded (b1,b0) in bits [1:0].
        decoded_input | (uncoded << 2)
    }
}

impl Default for V32bisViterbiDecoder {
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
    fn test_v32bis_128qam_trellis_encode_preserves_top_bits() {
        let mut encoder = V32bisTrellisEncoder::new();

        for data in 0..64u8 {
            encoder.reset();
            let coded = encoder.encode(data);
            let input_top = (data >> 2) & 0x0F;
            let output_top = (coded >> 3) & 0x0F;
            assert_eq!(
                input_top, output_top,
                "Top bits mismatch for input {data:#08b}: coded={coded:#09b}, \
                 expected top bits {input_top:#06b}, got {output_top:#06b}"
            );
        }
    }

    #[test]
    fn test_v32bis_128qam_trellis_encode_deterministic() {
        for data in 0..64u8 {
            let mut enc_a = V32bisTrellisEncoder::new();
            let mut enc_b = V32bisTrellisEncoder::new();

            let out_a = enc_a.encode(data);
            let out_b = enc_b.encode(data);
            assert_eq!(
                out_a, out_b,
                "Determinism failure: input={data}, out_a={out_a:#09b}, out_b={out_b:#09b}"
            );
        }
    }

    #[test]
    fn test_v32bis_128qam_trellis_roundtrip() {
        let mut encoder = V32bisTrellisEncoder::new();
        let mut decoder = V32bisViterbiDecoder::new();

        // Use 6-bit values (0..63)
        let data_sequence: Vec<u8> = (0u16..40).map(|i| ((i * 7) % 64) as u8).collect();

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

        for (i, (&expected, &got)) in data_sequence.iter().zip(decoded.iter()).enumerate() {
            assert_eq!(
                expected, got,
                "Mismatch at position {i}: expected {expected:#08b}, got {got:#08b}"
            );
        }
    }

    #[test]
    fn test_v32bis_128qam_trellis_roundtrip_long() {
        let mut encoder = V32bisTrellisEncoder::new();
        let mut decoder = V32bisViterbiDecoder::new();

        // Pseudo-random 6-bit values.
        let mut seed: u32 = 0xCAFE_BABE;
        let data_sequence: Vec<u8> = (0..100)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed & 0x3F) as u8
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

        assert_eq!(decoded.len(), data_sequence.len());

        let mut errors = 0;
        for (i, (&expected, &got)) in data_sequence.iter().zip(decoded.iter()).enumerate() {
            if expected != got {
                errors += 1;
                eprintln!("  Error at position {i}: expected {expected:#08b}, got {got:#08b}");
            }
        }
        assert_eq!(errors, 0, "Expected 0 errors, got {errors}/100");
    }

    #[test]
    fn test_v32bis_128qam_trellis_state_advances() {
        let mut encoder = V32bisTrellisEncoder::new();
        assert_eq!(encoder.state(), 0);

        encoder.encode(0x01); // b0=1
        assert_ne!(encoder.state(), 0);

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
            "State should change across multiple encodes"
        );
    }

    #[test]
    fn test_v32bis_128qam_trellis_flush() {
        let mut encoder = V32bisTrellisEncoder::new();
        let mut decoder = V32bisViterbiDecoder::new();

        let data_sequence: Vec<u8> = (0..10).map(|i| i % 64).collect();

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

        // With 10 symbols and traceback depth 15, nothing should be output yet.
        assert!(decoded_before_flush.is_empty());

        let flushed = decoder.flush();
        let total_decoded = decoded_before_flush.len() + flushed.len();
        assert_eq!(total_decoded, data_sequence.len());

        let all_decoded: Vec<u8> = decoded_before_flush
            .iter()
            .chain(flushed.iter())
            .copied()
            .collect();
        for (i, (&expected, &got)) in data_sequence.iter().zip(all_decoded.iter()).enumerate() {
            assert_eq!(
                expected, got,
                "Mismatch at position {i}: expected {expected:#08b}, got {got:#08b}"
            );
        }
    }

    #[test]
    fn test_v32bis_128qam_soft_decision_roundtrip() {
        use crate::qam::Constellation;

        let constellation = Constellation::Qam128;
        let mut decoder = V32bisViterbiDecoder::new();

        // Use a pseudo-random sequence that exercises diverse uncoded bit
        // patterns.  The old bug (global best_uncoded) only manifests when
        // different states have different best uncoded values — which happens
        // whenever the data sequence varies the upper 4 bits.
        //
        // We send N_DATA real symbols followed by TRACEBACK_DEPTH "tail"
        // symbols (value 0) that flush the pipeline.  We only verify the
        // N_DATA real symbols — the tail symbols exist solely to push the
        // last real data out of the traceback buffer.
        const N_DATA: usize = 60;

        let mut seed: u32 = 0xDEAD_BEEF;
        let data_sequence: Vec<u8> = (0..N_DATA)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed & 0x3F) as u8
            })
            .collect();

        // Build the full transmit sequence: real data + tail zeros.
        let mut tx_data: Vec<u8> = data_sequence.clone();
        tx_data.extend(std::iter::repeat_n(0u8, TRACEBACK_DEPTH));

        // Encode all of it.
        let coded_symbols: Vec<u8> = {
            let mut enc = V32bisTrellisEncoder::new();
            tx_data.iter().map(|&d| enc.encode(d)).collect()
        };

        // Soft-decode using noiseless I/Q.
        let mut decoded = Vec::new();
        for &sym in &coded_symbols {
            let (pi, pq) = constellation.point(sym);
            if let Some(d) = decoder.decode_soft(pi, pq, &constellation) {
                decoded.push(d);
            }
        }

        assert!(
            decoded.len() >= N_DATA,
            "Too few decoded symbols: {} < {N_DATA}",
            decoded.len(),
        );

        for (i, (&expected, &got)) in data_sequence.iter().zip(decoded.iter()).enumerate() {
            assert_eq!(
                expected, got,
                "Soft-decision mismatch at position {i}: expected {expected:#08b} ({expected}), \
                 got {got:#08b} ({got})"
            );
        }
    }

    #[test]
    fn test_v32bis_128qam_soft_decision_with_noise() {
        use crate::qam::Constellation;

        let constellation = Constellation::Qam128;
        let mut decoder = V32bisViterbiDecoder::new();

        const N_DATA: usize = 80;

        // Pseudo-random data with diverse uncoded bits.
        let mut seed: u32 = 0x1234_5678;
        let data_sequence: Vec<u8> = (0..N_DATA)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed & 0x3F) as u8
            })
            .collect();

        // Append tail zeros to flush the traceback pipeline.
        let mut tx_data: Vec<u8> = data_sequence.clone();
        tx_data.extend(std::iter::repeat_n(0u8, TRACEBACK_DEPTH));

        let coded_symbols: Vec<u8> = {
            let mut enc = V32bisTrellisEncoder::new();
            tx_data.iter().map(|&d| enc.encode(d)).collect()
        };

        // Add small noise — the Viterbi decoder should correct it.
        let mut noise_seed: u32 = 0xAAAA_BBBB;
        let noisy_iq: Vec<(f64, f64)> = coded_symbols
            .iter()
            .map(|&sym| {
                let (pi, pq) = constellation.point(sym);
                // Simple deterministic pseudo-noise, small enough for Viterbi
                // to correct.
                noise_seed ^= noise_seed << 13;
                noise_seed ^= noise_seed >> 17;
                noise_seed ^= noise_seed << 5;
                let ni = ((noise_seed & 0xFFFF) as f64 / 65536.0 - 0.5) * 0.1;
                noise_seed ^= noise_seed << 13;
                noise_seed ^= noise_seed >> 17;
                noise_seed ^= noise_seed << 5;
                let nq = ((noise_seed & 0xFFFF) as f64 / 65536.0 - 0.5) * 0.1;
                (pi + ni, pq + nq)
            })
            .collect();

        let mut decoded = Vec::new();
        for &(ri, rq) in &noisy_iq {
            if let Some(d) = decoder.decode_soft(ri, rq, &constellation) {
                decoded.push(d);
            }
        }

        assert!(
            decoded.len() >= N_DATA,
            "Too few decoded symbols: {} < {N_DATA}",
            decoded.len(),
        );

        let mut errors = 0;
        for (i, (&expected, &got)) in data_sequence.iter().zip(decoded.iter()).enumerate() {
            if expected != got {
                errors += 1;
                eprintln!("  Noisy soft error at {i}: expected {expected:#08b}, got {got:#08b}");
            }
        }
        // With tiny noise and Viterbi coding gain, expect zero errors.
        assert_eq!(
            errors,
            0,
            "Noisy soft-decision: {errors}/{} errors",
            data_sequence.len()
        );
    }

    #[test]
    fn test_v32bis_128qam_output_range() {
        // Verify that encoded outputs are all valid 7-bit values (< 128).
        let mut encoder = V32bisTrellisEncoder::new();
        for data in 0..64u8 {
            encoder.reset();
            let coded = encoder.encode(data);
            assert!(coded < 128, "Coded output {coded} >= 128 for input {data}");
        }
    }
}
