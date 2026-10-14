//! V.90 PCM trellis encoder and Viterbi decoder.
//!
//! 4-state convolutional encoder (rate 1/2) maps 1 data bit to 2 coded bits
//! using the optimal (7,5) generator polynomials (octal notation), achieving
//! free distance 5. The coded bits control the sign and MSB of each mu-law
//! code, while the remaining 6 bits pass through uncoded.
//!
//! The Viterbi decoder uses hard-decision (Hamming distance) branch metrics
//! with traceback depth 15.

/// Traceback depth: ~5x constraint length (constraint length = 3 for 4 states).
pub const TRACEBACK_DEPTH: usize = 15;

/// Number of encoder/decoder states.
const NUM_STATES: usize = 4;

// ---------------------------------------------------------------------------
// Transition table
// ---------------------------------------------------------------------------

/// Pre-computed transition table for the (7,5) convolutional code.
///
/// Each entry: `(prev_state, data_bit, next_state, expected_y0, expected_y1)`.
///
/// Generator polynomials (octal 7,5 = binary 111, 101):
///   y0 = d ^ s0 ^ s1    (generator 7)
///   y1 = d ^ s1          (generator 5)
///   next_state = (s0 << 1) | d
const TRANSITIONS: [(u8, u8, u8, u8, u8); 8] = [
    // (prev_state, data_bit, next_state, y0, y1)
    (0, 0, 0, 0, 0), // state 0 (s1=0,s0=0), d=0
    (0, 1, 1, 1, 1), // state 0 (s1=0,s0=0), d=1
    (1, 0, 2, 1, 0), // state 1 (s1=0,s0=1), d=0
    (1, 1, 3, 0, 1), // state 1 (s1=0,s0=1), d=1
    (2, 0, 0, 1, 1), // state 2 (s1=1,s0=0), d=0
    (2, 1, 1, 0, 0), // state 2 (s1=1,s0=0), d=1
    (3, 0, 2, 0, 1), // state 3 (s1=1,s0=1), d=0
    (3, 1, 3, 1, 0), // state 3 (s1=1,s0=1), d=1
];

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// 4-state convolutional encoder for V.90 PCM trellis coding.
///
/// Encodes 1 data bit into 2 coded bits (sign, MSB) using the optimal
/// (7,5) generator polynomials (octal notation), achieving free distance 5.
///
/// State: 2 bits (s1, s0).
/// Generator polynomials (7,5 octal = 111, 101 binary):
///   y0 = d ^ s0 ^ s1     (generator 7: uses d, s0, s1)
///   y1 = d ^ s1           (generator 5: uses d, s1)
///   next_state = (s0 << 1) | d   (shift register: new bit enters as s0)
pub struct PcmTrellisEncoder {
    /// 2-bit state: (s1, s0) packed in bits [1:0].
    state: u8,
}

impl PcmTrellisEncoder {
    pub fn new() -> Self {
        Self { state: 0 }
    }

    /// Encode one data bit into 2 coded bits.
    ///
    /// Returns `(y0, y1)` where y0 maps to sign and y1 maps to MSB.
    pub fn encode(&mut self, data_bit: u8) -> (u8, u8) {
        let d = data_bit & 1;
        let s0 = self.state & 1;
        let s1 = (self.state >> 1) & 1;

        let y0 = d ^ s0 ^ s1; // generator 7 (octal)
        let y1 = d ^ s1; // generator 5 (octal)

        // Next state: s0' = d, s1' = s0
        self.state = (s0 << 1) | d;

        (y0, y1)
    }

    /// Reset encoder state to zero.
    pub fn reset(&mut self) {
        self.state = 0;
    }

    /// Current state (0-3).
    pub fn state(&self) -> u8 {
        self.state
    }
}

impl Default for PcmTrellisEncoder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// 4-state Viterbi decoder for V.90 PCM trellis coding.
///
/// Uses hard-decision branch metrics (Hamming distance) with traceback
/// depth 15. The decoder processes 2 coded bits per symbol and returns
/// 1 decoded data bit along with the 6 uncoded bits that were received
/// at the same time step.
pub struct PcmTrellisDecoder {
    /// Path metrics for each state.
    path_metrics: [f64; NUM_STATES],
    /// Circular buffer of survivor information.
    /// `survivors[cursor][state] = (previous_state, data_bit)`.
    survivors: [[(u8, u8); NUM_STATES]; TRACEBACK_DEPTH],
    /// Circular buffer of lower 6 uncoded bits per symbol.
    /// These are state-independent (same regardless of trellis path).
    lower_bits: [u8; TRACEBACK_DEPTH],
    /// Current write position in the circular buffers.
    cursor: usize,
    /// Number of symbols fed so far.
    symbols_received: usize,
}

impl PcmTrellisDecoder {
    pub fn new() -> Self {
        let mut path_metrics = [f64::INFINITY; NUM_STATES];
        path_metrics[0] = 0.0; // Start in state 0.
        Self {
            path_metrics,
            survivors: [[(0, 0); NUM_STATES]; TRACEBACK_DEPTH],
            lower_bits: [0; TRACEBACK_DEPTH],
            cursor: 0,
            symbols_received: 0,
        }
    }

    /// Decode one received symbol.
    ///
    /// Takes the 2 coded bits `(y0, y1)` extracted from the received mu-law
    /// code and the 6 uncoded lower bits.
    ///
    /// Returns `Some((data_bit, lower_6_bits))` after the traceback converges
    /// (after `TRACEBACK_DEPTH` symbols), or `None` during warmup.
    pub fn decode(&mut self, y0: u8, y1: u8, lower_6: u8) -> Option<(u8, u8)> {
        // Store lower bits at current cursor position.
        self.lower_bits[self.cursor] = lower_6;

        // Viterbi ACS: compute new path metrics for all states.
        let mut new_metrics = [f64::INFINITY; NUM_STATES];
        let mut new_survivors = [(0u8, 0u8); NUM_STATES];

        for &(prev_state, data_bit, next_state, exp_y0, exp_y1) in &TRANSITIONS {
            let prev_metric = self.path_metrics[prev_state as usize];
            if prev_metric == f64::INFINITY {
                continue;
            }

            let branch_metric = ((y0 ^ exp_y0) + (y1 ^ exp_y1)) as f64;
            let total = prev_metric + branch_metric;

            if total < new_metrics[next_state as usize] {
                new_metrics[next_state as usize] = total;
                new_survivors[next_state as usize] = (prev_state, data_bit);
            }
        }

        self.path_metrics = new_metrics;
        self.survivors[self.cursor] = new_survivors;

        // Advance cursor.
        self.cursor = (self.cursor + 1) % TRACEBACK_DEPTH;
        self.symbols_received += 1;

        // Wait until traceback has enough depth.
        if self.symbols_received < TRACEBACK_DEPTH {
            return None;
        }

        // Traceback from the state with the best (lowest) metric.
        let mut state = 0u8;
        let mut best = f64::INFINITY;
        for s in 0..NUM_STATES {
            if self.path_metrics[s] < best {
                best = self.path_metrics[s];
                state = s as u8;
            }
        }

        // Walk backwards through the survivor memory.
        let mut pos = if self.cursor == 0 {
            TRACEBACK_DEPTH - 1
        } else {
            self.cursor - 1
        };
        let mut decoded_bit = 0u8;

        for step in 0..TRACEBACK_DEPTH {
            let (prev_state, data_bit) = self.survivors[pos][state as usize];
            if step == TRACEBACK_DEPTH - 1 {
                decoded_bit = data_bit;
            }
            state = prev_state;
            pos = if pos == 0 {
                TRACEBACK_DEPTH - 1
            } else {
                pos - 1
            };
        }

        // Retrieve lower_6 bits from TRACEBACK_DEPTH symbols ago.
        // After advancing cursor, it points to the next write position,
        // which is the oldest entry (the one from TRACEBACK_DEPTH ago).
        let lower_6_out = self.lower_bits[self.cursor];

        Some((decoded_bit, lower_6_out))
    }

    /// Reset decoder state.
    pub fn reset(&mut self) {
        self.path_metrics = [f64::INFINITY; NUM_STATES];
        self.path_metrics[0] = 0.0;
        self.survivors = [[(0, 0); NUM_STATES]; TRACEBACK_DEPTH];
        self.lower_bits = [0; TRACEBACK_DEPTH];
        self.cursor = 0;
        self.symbols_received = 0;
    }
}

impl Default for PcmTrellisDecoder {
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
    fn test_encoder_state_transitions() {
        // Verify (7,5) polynomial by hand-computed examples.
        let mut enc = PcmTrellisEncoder::new();

        // State 0, input 0: y0=0^0^0=0, y1=0^0=0, next=(0,0)=0
        assert_eq!(enc.encode(0), (0, 0));
        assert_eq!(enc.state(), 0);

        // State 0, input 1: y0=1^0^0=1, y1=1^0=1, next=(0,1)=1
        let mut enc2 = PcmTrellisEncoder::new();
        assert_eq!(enc2.encode(1), (1, 1));
        assert_eq!(enc2.state(), 1);
    }

    #[test]
    fn test_encoder_all_state_transitions() {
        // Exhaustively verify all 8 transitions against the table.
        for &(prev_state, data_bit, expected_next, expected_y0, expected_y1) in &TRANSITIONS {
            let mut enc = PcmTrellisEncoder::new();
            enc.state = prev_state;
            let (y0, y1) = enc.encode(data_bit);
            assert_eq!(
                (y0, y1, enc.state()),
                (expected_y0, expected_y1, expected_next),
                "Transition mismatch: state={prev_state}, d={data_bit}"
            );
        }
    }

    #[test]
    fn test_encoder_deterministic() {
        // Same input from same state should always produce the same output.
        for d in 0..2u8 {
            let mut enc_a = PcmTrellisEncoder::new();
            let mut enc_b = PcmTrellisEncoder::new();
            assert_eq!(
                enc_a.encode(d),
                enc_b.encode(d),
                "Determinism failure for d={d}"
            );
        }
    }

    #[test]
    fn test_encoder_decoder_roundtrip() {
        let mut enc = PcmTrellisEncoder::new();
        let mut dec = PcmTrellisDecoder::new();

        // Encode a pseudo-random sequence of bits with arbitrary lower_6 data.
        let test_bits: Vec<u8> = (0..100).map(|i| ((i * 7 + 3) % 2) as u8).collect();
        let test_lower: Vec<u8> = (0..100).map(|i| ((i * 31 + 17) % 64) as u8).collect();

        let mut coded: Vec<(u8, u8, u8)> = Vec::new();
        for (i, &bit) in test_bits.iter().enumerate() {
            let (y0, y1) = enc.encode(bit);
            coded.push((y0, y1, test_lower[i]));
        }

        // Decode.
        let mut decoded_bits: Vec<u8> = Vec::new();
        let mut decoded_lower: Vec<u8> = Vec::new();
        for &(y0, y1, lower_6) in &coded {
            if let Some((data_bit, lower_out)) = dec.decode(y0, y1, lower_6) {
                decoded_bits.push(data_bit);
                decoded_lower.push(lower_out);
            }
        }

        // After TRACEBACK_DEPTH warmup, decoded should match.
        let expected_bits = &test_bits[..decoded_bits.len()];
        assert_eq!(decoded_bits, expected_bits, "Trellis data bits mismatch");

        let expected_lower = &test_lower[..decoded_lower.len()];
        assert_eq!(decoded_lower, expected_lower, "Lower 6 bits mismatch");
    }

    #[test]
    fn test_viterbi_convergence() {
        let mut dec = PcmTrellisDecoder::new();

        // Feed TRACEBACK_DEPTH-1 symbols: should get None.
        for i in 0..TRACEBACK_DEPTH - 1 {
            assert!(
                dec.decode(0, 0, 0).is_none(),
                "Should be None at symbol {i}"
            );
        }
        // Symbol TRACEBACK_DEPTH should produce first output.
        assert!(
            dec.decode(0, 0, 0).is_some(),
            "Should produce output at symbol {}",
            TRACEBACK_DEPTH
        );
    }

    #[test]
    fn test_single_error_correction() {
        // Encode, inject one error, verify decoder corrects it.
        let mut enc = PcmTrellisEncoder::new();
        let mut dec = PcmTrellisDecoder::new();

        let test_bits: Vec<u8> = (0..50).map(|i| (i % 2) as u8).collect();
        let mut coded: Vec<(u8, u8, u8)> = Vec::new();

        for &bit in &test_bits {
            let (y0, y1) = enc.encode(bit);
            coded.push((y0, y1, 0));
        }

        // Inject error at position 20 (flip y0).
        coded[20].0 ^= 1;

        let mut decoded: Vec<u8> = Vec::new();
        for &(y0, y1, lower_6) in &coded {
            if let Some((data_bit, _)) = dec.decode(y0, y1, lower_6) {
                decoded.push(data_bit);
            }
        }

        let expected = &test_bits[..decoded.len()];
        assert_eq!(
            decoded, expected,
            "Decoder should correct single-symbol error"
        );
    }

    #[test]
    fn test_all_zeros() {
        // All-zero input from state 0 should produce all-zero coded output
        // and decode back to all zeros.
        let mut enc = PcmTrellisEncoder::new();
        let mut dec = PcmTrellisDecoder::new();

        let mut coded = Vec::new();
        for _ in 0..30 {
            let (y0, y1) = enc.encode(0);
            assert_eq!((y0, y1), (0, 0), "All-zero input should produce (0,0)");
            coded.push((y0, y1, 0u8));
        }

        let mut decoded: Vec<u8> = Vec::new();
        for &(y0, y1, lower_6) in &coded {
            if let Some((data_bit, _)) = dec.decode(y0, y1, lower_6) {
                decoded.push(data_bit);
            }
        }

        for (i, &bit) in decoded.iter().enumerate() {
            assert_eq!(bit, 0, "Expected 0 at position {i}, got {bit}");
        }
    }

    #[test]
    fn test_encoder_reset() {
        let mut enc = PcmTrellisEncoder::new();
        enc.encode(1);
        assert_ne!(enc.state(), 0);
        enc.reset();
        assert_eq!(enc.state(), 0);
    }

    #[test]
    fn test_decoder_reset() {
        let mut dec = PcmTrellisDecoder::new();
        // Feed some symbols.
        for _ in 0..5 {
            dec.decode(1, 0, 42);
        }
        dec.reset();
        assert_eq!(dec.symbols_received, 0);
        assert_eq!(dec.cursor, 0);
    }

    #[test]
    fn test_coding_gain() {
        let mut enc = PcmTrellisEncoder::new();
        let mut dec = PcmTrellisDecoder::new();

        let num_bits = 1000;
        let mut rng: u32 = 0xBEEF_CAFE;
        let error_rate = 0.10; // 10% bit error rate

        // Generate random data bits
        let mut data_bits: Vec<u8> = Vec::new();
        for _ in 0..num_bits {
            rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
            data_bits.push(((rng >> 16) & 1) as u8);
        }

        // Encode
        let mut coded: Vec<(u8, u8)> = Vec::new();
        for &bit in &data_bits {
            coded.push(enc.encode(bit));
        }

        // Inject errors
        let mut noisy = coded.clone();
        let threshold = (error_rate * (u32::MAX as f64)) as u32;

        for pair in noisy.iter_mut() {
            rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
            if rng < threshold {
                pair.0 ^= 1; // flip y0
            }
            rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
            if rng < threshold {
                pair.1 ^= 1; // flip y1
            }
        }

        // Decode with Viterbi
        let mut decoded: Vec<u8> = Vec::new();
        for &(y0, y1) in &noisy {
            if let Some((bit, _)) = dec.decode(y0, y1, 0) {
                decoded.push(bit);
            }
        }

        // Count Viterbi errors
        let viterbi_errors: u32 = decoded
            .iter()
            .zip(data_bits.iter())
            .map(|(d, e)| if d != e { 1 } else { 0 })
            .sum();

        // Memoryless decoder: just take y0 as data bit (simple majority)
        let memoryless: Vec<u8> = noisy.iter().map(|&(y0, _)| y0).collect();
        let memoryless_errors: u32 = memoryless
            .iter()
            .take(decoded.len())
            .zip(data_bits.iter())
            .map(|(d, e)| if d != e { 1 } else { 0 })
            .sum();

        eprintln!(
            "Coding gain test: Viterbi errors={viterbi_errors}, memoryless errors={memoryless_errors}"
        );
        assert!(
            viterbi_errors < memoryless_errors,
            "Viterbi ({viterbi_errors}) should have fewer errors than memoryless ({memoryless_errors})"
        );
    }
}
