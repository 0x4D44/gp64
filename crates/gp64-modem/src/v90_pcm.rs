//! V.90 PCM downstream path: G.711 mu-law codec, PCM constellation, encoder/decoder.
//!
//! V.90 modems use PCM (Pulse Code Modulation) for the downstream (server → client)
//! path. The digital end transmits data as mu-law encoded PCM samples at 8 kHz,
//! which the PSTN codec converts to analog. The analog end recovers data by
//! quantizing the received analog signal back to mu-law codes.
//!
//! This module implements:
//! - **G.711 mu-law codec**: Standard ITU-T companding with 256 quantization levels
//! - **PcmConstellation**: Maps between data bits and mu-law amplitude levels
//! - **PcmEncoder**: Serializes data bytes into PCM samples (digital end TX)
//! - **PcmDecoder**: Recovers data bytes from PCM samples (analog end RX)
//!
//! Trellis-coded modulation: 1 data bit is convolutionally encoded into
//! 2 coded bits (sign, MSB) via the (7,5) code, while the remaining 6 bits
//! pass through uncoded. This provides error correction with free distance 5.

use crate::scrambler::{Descrambler, Scrambler};
use crate::v90_trellis::{PcmTrellisDecoder, PcmTrellisEncoder};
use std::collections::VecDeque;

// ---------------------------------------------------------------------------
// G.711 mu-law decode table (const, computed at compile time)
// ---------------------------------------------------------------------------

/// Build the G.711 mu-law decode table at compile time.
///
/// Internal (non-inverted) representation:
/// - Bit 7: sign (1 = positive, 0 = negative)
/// - Bits 6-4: segment (0-7)
/// - Bits 3-0: step within segment (0-15)
///
/// Decode expression: `magnitude = (2 * step + 33) << segment - 33`
const fn build_mu_law_decode_table() -> [i16; 256] {
    let mut table = [0i16; 256];
    let mut code: u16 = 0;
    while code < 256 {
        let sign = (code >> 7) & 1;
        let segment = (code >> 4) & 0x07;
        let step = code & 0x0F;

        // Standard G.711 mu-law decode expression.
        let magnitude = ((2 * step as i32 + 33) << segment) - 33;

        table[code as usize] = if sign == 1 {
            magnitude as i16
        } else {
            -(magnitude as i16)
        };
        code += 1;
    }
    table
}

/// Pre-computed G.711 mu-law decode table (256 entries, indexed by code).
static MU_LAW_DECODE_TABLE: [i16; 256] = build_mu_law_decode_table();

// ---------------------------------------------------------------------------
// G.711 mu-law codec functions
// ---------------------------------------------------------------------------

/// Convert a mu-law code (0-255) to a linear PCM amplitude.
///
/// Uses the standard G.711 decode table. The internal representation has
/// bit 7 as the sign bit (1 = positive).
pub fn mu_law_decode(code: u8) -> i16 {
    MU_LAW_DECODE_TABLE[code as usize]
}

/// Convert a linear PCM amplitude to the nearest mu-law code (0-255).
///
/// Performs a linear search through the decode table to find the code whose
/// decoded value is closest to the input sample. This is O(256) per call
/// but entirely adequate for the 8 kHz symbol rate.
pub fn mu_law_encode(sample: i16) -> u8 {
    let target = sample as i32;
    let mut best_code = 0u8;
    let mut best_dist = i32::MAX;

    for code in 0u16..256 {
        let decoded = MU_LAW_DECODE_TABLE[code as usize] as i32;
        let dist = (target - decoded).abs();
        if dist < best_dist {
            best_dist = dist;
            best_code = code as u8;
        }
    }
    best_code
}

// ---------------------------------------------------------------------------
// PcmConstellation
// ---------------------------------------------------------------------------

/// PCM constellation mapping between data bits and mu-law amplitude levels.
///
/// In V.90, the digital end selects mu-law codes to encode data. The full
/// constellation uses all 128 levels per sign polarity (7 bits per symbol).
/// Reduced constellations use fewer levels for lower data rates or better
/// noise margins.
///
/// A reduced constellation of `2^m` magnitudes per polarity keeps the `2^m`
/// LARGEST ones — mu-law magnitude codes `128 - 2^m ..= 127`. The small
/// magnitudes are the finely spaced ones near zero, so they are the first a
/// noisy line makes indistinguishable. Magnitude increases monotonically with
/// the code's low seven bits, so the usable set is one contiguous range.
#[derive(Clone)]
pub struct PcmConstellation {
    /// Linear PCM amplitudes for all 256 mu-law codes, indexed by code.
    levels: [i16; 256],
    /// Number of usable levels per sign polarity.
    usable_levels_per_sign: u8,
    /// Bits per symbol (floor of log2 of usable levels per sign).
    magnitude_bits: u8,
}

impl PcmConstellation {
    /// Create a full constellation with all 128 levels per polarity usable.
    ///
    /// This gives 7 data bits per symbol (128 levels = 2^7).
    pub fn new_full() -> Self {
        let mut levels = [0i16; 256];
        for code in 0..256u16 {
            levels[code as usize] = mu_law_decode(code as u8);
        }
        Self {
            levels,
            usable_levels_per_sign: 128,
            magnitude_bits: 7,
        }
    }

    /// Create a constellation with a specified number of usable levels per polarity.
    ///
    /// The bits per symbol is `floor(log2(usable_per_sign))`. For example,
    /// 64 levels gives 6 bits per symbol.
    pub fn new_with_levels(usable_per_sign: u8) -> Self {
        let mut levels = [0i16; 256];
        for code in 0..256u16 {
            levels[code as usize] = mu_law_decode(code as u8);
        }
        // floor(log2(n)) for n >= 1
        let bits = if usable_per_sign == 0 {
            0
        } else {
            (usable_per_sign as f64).log2().floor() as u8
        };
        Self {
            levels,
            usable_levels_per_sign: usable_per_sign,
            magnitude_bits: bits,
        }
    }

    /// Get the number of usable levels per sign polarity.
    pub fn usable_levels_per_sign(&self) -> u8 {
        self.usable_levels_per_sign
    }

    /// The smallest mu-law magnitude code (low seven bits) in the usable set.
    fn lowest_magnitude(&self) -> u8 {
        // `magnitude_bits` is floor(log2) of a u8, so at most 7.
        128 - (1u8 << self.magnitude_bits)
    }

    /// The mu-law code carrying `sign` and the `index`-th usable magnitude
    /// (`index < 2^bits_per_symbol`).
    pub fn code_for(&self, sign: u8, index: u8) -> u8 {
        debug_assert!(u16::from(index) < (1u16 << self.magnitude_bits));
        ((sign & 1) << 7) | (self.lowest_magnitude() + index)
    }

    /// The usable-magnitude index a code from [`Self::nearest_code`] carries.
    pub fn magnitude_index(&self, code: u8) -> u8 {
        (code & 0x7F) - self.lowest_magnitude()
    }

    /// Find the nearest USABLE mu-law code for a given linear PCM amplitude.
    ///
    /// The usable magnitudes are the top contiguous range of the polarity, so
    /// the nearest one to an amplitude below that range is its lowest member.
    pub fn nearest_code(&self, sample: i16) -> u8 {
        let code = mu_law_encode(sample);
        let lowest = self.lowest_magnitude();
        if code & 0x7F < lowest {
            (code & 0x80) | lowest
        } else {
            code
        }
    }

    /// Get the number of data bits per symbol.
    pub fn bits_per_symbol(&self) -> u8 {
        self.magnitude_bits
    }

    /// Get the linear amplitude for a mu-law code.
    pub fn level(&self, code: u8) -> i16 {
        self.levels[code as usize]
    }
}

// ---------------------------------------------------------------------------
// PcmEncoder (Digital End TX)
// ---------------------------------------------------------------------------

/// PCM encoder for the V.90 downstream (digital end transmit) path.
///
/// Accepts serial data bytes, scrambles them, and maps groups of
/// `bits_per_symbol` bits (7 for the full constellation) to mu-law PCM
/// samples at 8 kHz. Uses trellis-coded modulation: 1 bit is convolutionally
/// encoded into (sign, magnitude MSB), and the rest pass uncoded.
pub struct PcmEncoder {
    scrambler: Scrambler,
    constellation: PcmConstellation,
    trellis: PcmTrellisEncoder,
    /// Buffered scrambled bytes waiting to be consumed as symbols.
    byte_buf: VecDeque<u8>,
    /// Bit accumulator for partial-byte leftovers (max 31 bits).
    bit_accum: u32,
    bit_count: u8,
}

impl PcmEncoder {
    pub fn new(constellation: PcmConstellation) -> Self {
        Self {
            scrambler: Scrambler::new(),
            constellation,
            trellis: PcmTrellisEncoder::new(),
            byte_buf: VecDeque::new(),
            bit_accum: 0,
            bit_count: 0,
        }
    }

    /// Feed a serial data byte. Scrambles and buffers for later consumption.
    pub fn write_byte(&mut self, byte: u8) {
        let scrambled = self.scrambler.scramble_byte(byte);
        self.byte_buf.push_back(scrambled);
    }

    /// Returns true if enough bits are available for at least one symbol.
    pub fn has_data(&self) -> bool {
        let bits = self.constellation.bits_per_symbol();
        bits > 0 && (self.bit_count >= bits || !self.byte_buf.is_empty())
    }

    /// Ensure the bit accumulator has at least `needed` bits by draining
    /// from the byte buffer.
    fn fill_bits(&mut self, needed: u8) {
        while self.bit_count < needed {
            if let Some(byte) = self.byte_buf.pop_front() {
                self.bit_accum |= (byte as u32) << self.bit_count;
                self.bit_count += 8;
            } else {
                break;
            }
        }
    }

    /// Generate the next PCM sample (one symbol at 8 kHz).
    ///
    /// Pulls `m = bits_per_symbol` bits from the accumulator (7 for the full
    /// constellation): 1 bit is trellis-encoded into (sign, magnitude MSB),
    /// and `m - 1` bits pass through uncoded. The magnitude index is
    /// `msb << (m - 1) | uncoded`, so only the constellation's usable codes
    /// are ever sent.
    /// Returns an idle tone if insufficient data is available.
    pub fn next_sample(&mut self) -> i16 {
        let m = self.constellation.bits_per_symbol();
        self.fill_bits(m);

        if m == 0 || self.bit_count < m {
            // Not enough data — send idle (a mid-range positive usable code;
            // 0xC0 for the full constellation).
            let mid = if m == 0 { 0 } else { 1 << (m - 1) };
            return self
                .constellation
                .level(self.constellation.code_for(1, mid));
        }

        let bits = (self.bit_accum & ((1u32 << m) - 1)) as u8;
        self.bit_accum >>= m;
        self.bit_count -= m;

        // Split: bit 0 is trellis data bit, bits 1..m are uncoded.
        let trellis_bit = bits & 1;
        let uncoded = bits >> 1;

        // Trellis encode: 1 data bit -> (sign, msb).
        let (sign, msb) = self.trellis.encode(trellis_bit);

        let index = (msb << (m - 1)) | uncoded;
        self.constellation
            .level(self.constellation.code_for(sign, index))
    }

    /// Access the constellation (for inspection/testing).
    pub fn constellation(&self) -> &PcmConstellation {
        &self.constellation
    }
}

// ---------------------------------------------------------------------------
// PcmDecoder (Analog End RX)
// ---------------------------------------------------------------------------

/// PCM decoder for the V.90 downstream (analog end receive) path.
///
/// Accepts received PCM samples, quantizes them to mu-law codes, extracts
/// trellis-coded bits (sign, MSB) and uncoded lower 6 bits, runs Viterbi
/// decoding to recover the original data bit, and descrambles.
pub struct PcmDecoder {
    descrambler: Descrambler,
    constellation: PcmConstellation,
    trellis: PcmTrellisDecoder,
    bit_accum: u32,
    bit_count: u8,
    byte_queue: VecDeque<u8>,
}

impl PcmDecoder {
    pub fn new(constellation: PcmConstellation) -> Self {
        Self {
            descrambler: Descrambler::new(),
            constellation,
            trellis: PcmTrellisDecoder::new(),
            bit_accum: 0,
            bit_count: 0,
            byte_queue: VecDeque::new(),
        }
    }

    /// Feed one received PCM sample.
    ///
    /// Extracts the mu-law code, splits into trellis-coded bits (sign, MSB)
    /// and uncoded lower 6 bits, runs Viterbi decoding, and reassembles
    /// data bytes.
    pub fn feed_sample(&mut self, sample: i16) {
        let m = self.constellation.bits_per_symbol();
        if m == 0 {
            // A constellation with no usable magnitude carries no data.
            return;
        }
        let code = self.constellation.nearest_code(sample);

        // Extract trellis-coded bits and uncoded bits from the usable-
        // magnitude index, the encoder's split in reverse.
        let sign = (code >> 7) & 1;
        let index = self.constellation.magnitude_index(code);
        let msb = index >> (m - 1);
        let uncoded = index & ((1 << (m - 1)) - 1);

        // Trellis decode: (sign, msb) -> data_bit, paired with the uncoded bits.
        if let Some((data_bit, decoded_uncoded)) = self.trellis.decode(sign, msb, uncoded) {
            // Reassemble m data bits: trellis_bit(bit 0) | uncoded(bits 1..m).
            let bits = ((decoded_uncoded as u32) << 1) | (data_bit as u32);

            self.bit_accum |= bits << self.bit_count;
            self.bit_count += m;

            // Assemble full bytes and descramble.
            while self.bit_count >= 8 {
                let scrambled_byte = (self.bit_accum & 0xFF) as u8;
                self.bit_accum >>= 8;
                self.bit_count -= 8;
                let byte = self.descrambler.descramble_byte(scrambled_byte);
                self.byte_queue.push_back(byte);
            }
        }
        // None means Viterbi hasn't converged yet — no output.
    }

    /// Take the next decoded byte, if available.
    pub fn take_byte(&mut self) -> Option<u8> {
        self.byte_queue.pop_front()
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
    fn test_mu_law_decode_reference_values() {
        // G.711 reference values (internal non-inverted representation).
        // Code 0xFF: positive, segment 7, step 15 -> +8031
        assert_eq!(mu_law_decode(0xFF), 8031);
        // Code 0x80: positive, segment 0, step 0 -> +0 (smallest positive)
        assert_eq!(mu_law_decode(0x80), 0);
        // Code 0x7F: negative, segment 7, step 15 -> -8031
        assert_eq!(mu_law_decode(0x7F), -8031);
        // Code 0x00: negative, segment 0, step 0 -> 0 (smallest negative)
        assert_eq!(mu_law_decode(0x00), 0);

        // Segment boundaries.
        // Positive segment 1, step 0: code 0x90
        // magnitude = (2*0 + 33) << 1 - 33 = 66 - 33 = 33
        assert_eq!(mu_law_decode(0x90), 33);
    }

    #[test]
    fn test_mu_law_roundtrip_all_codes() {
        // Every decode -> encode -> decode should return the same value.
        for code in 0u16..256 {
            let decoded = mu_law_decode(code as u8);
            let re_encoded = mu_law_encode(decoded);
            let re_decoded = mu_law_decode(re_encoded);
            assert_eq!(
                decoded, re_decoded,
                "Roundtrip failed for code {code}: decoded={decoded}, \
                 re_encoded={re_encoded}, re_decoded={re_decoded}"
            );
        }
    }

    #[test]
    fn test_mu_law_encode_nearest() {
        // Values between mu-law levels should encode to one of the two
        // adjacent codes.
        let code_a = 0xC0u8;
        let code_b = 0xC1u8;
        let val_a = mu_law_decode(code_a);
        let val_b = mu_law_decode(code_b);
        let mid = (val_a as i32 + val_b as i32) / 2;
        let encoded = mu_law_encode(mid as i16);
        assert!(
            encoded == code_a || encoded == code_b,
            "Midpoint {mid} between codes {code_a:02X}={val_a} and \
             {code_b:02X}={val_b} encoded to {encoded:02X}={}",
            mu_law_decode(encoded)
        );
    }

    #[test]
    fn test_pcm_constellation_full() {
        let c = PcmConstellation::new_full();
        assert_eq!(c.bits_per_symbol(), 7);
        // Level 0xFF should be the largest positive.
        assert_eq!(c.level(0xFF), 8031);
        // Level 0x80 should be smallest positive.
        assert_eq!(c.level(0x80), 0);
    }

    #[test]
    fn test_pcm_constellation_nearest_code() {
        let c = PcmConstellation::new_full();
        // Exact values should map back to a code with the same decoded value.
        for code in 0..=255u8 {
            let level = c.level(code);
            let nearest = c.nearest_code(level);
            assert_eq!(
                mu_law_decode(nearest),
                level,
                "nearest_code({level}) = {nearest:02X}, decoded = {}",
                mu_law_decode(nearest)
            );
        }
    }

    #[test]
    fn test_pcm_constellation_reduced_levels() {
        let c = PcmConstellation::new_with_levels(64);
        assert_eq!(c.bits_per_symbol(), 6);
    }

    #[test]
    fn test_pcm_encoder_decoder_roundtrip() {
        // Send 1024+ bytes through encoder -> decoder and verify all arrive intact.
        // The Viterbi decoder introduces a delay of TRACEBACK_DEPTH (15) symbols,
        // which means ~15 extra padding bytes are needed to flush all real data
        // through the pipeline.
        use crate::v90_trellis::TRACEBACK_DEPTH;

        let constellation = PcmConstellation::new_full();
        let mut encoder = PcmEncoder::new(constellation.clone());
        let mut decoder = PcmDecoder::new(constellation);

        // Generate test data with all 256 byte values represented.
        let mut test_data: Vec<u8> = Vec::new();
        for i in 0..=255u8 {
            test_data.push(i);
        }
        // Pad to 1100+ bytes with a simple PRNG.
        let mut rng_state: u32 = 0xDEAD_BEEF;
        while test_data.len() < 1100 {
            rng_state = rng_state.wrapping_mul(1_103_515_245).wrapping_add(12345);
            test_data.push((rng_state >> 16) as u8);
        }

        // Feed all data to encoder.
        for &byte in &test_data {
            encoder.write_byte(byte);
        }

        // Feed padding bytes to flush the Viterbi pipeline.
        // TRACEBACK_DEPTH symbols * 7 bits / 8 bits = ~14 bytes, add margin.
        let padding_bytes = (TRACEBACK_DEPTH * 7).div_ceil(8) + 5;
        for _ in 0..padding_bytes {
            encoder.write_byte(0x00);
        }

        // Generate PCM samples and feed to decoder.
        // Total bytes (data + padding) * 8 bits / 7 bits per symbol, plus margin.
        let total_bytes = test_data.len() + padding_bytes;
        let num_symbols = (total_bytes * 8).div_ceil(7) + 10;
        let mut decoded: Vec<u8> = Vec::new();

        for _ in 0..num_symbols {
            let sample = encoder.next_sample();
            decoder.feed_sample(sample);
            while let Some(byte) = decoder.take_byte() {
                decoded.push(byte);
            }
        }

        // Decoded output should contain all original data (may have trailing
        // bytes from padding).
        assert!(
            decoded.len() >= test_data.len(),
            "Expected at least {} decoded bytes, got {}",
            test_data.len(),
            decoded.len()
        );
        assert_eq!(
            &decoded[..test_data.len()],
            &test_data[..],
            "Decoded data doesn't match input"
        );
    }

    #[test]
    fn test_pcm_encoder_has_data() {
        let constellation = PcmConstellation::new_full();
        let mut encoder = PcmEncoder::new(constellation);
        assert!(!encoder.has_data());
        encoder.write_byte(0x42);
        assert!(encoder.has_data()); // 8 bits > 7 needed
    }

    #[test]
    fn test_mu_law_symmetry() {
        // Positive and negative codes with same magnitude bits should have
        // equal-and-opposite decoded values (except for the zero/zero case).
        for step in 0..16u8 {
            for seg in 0..8u8 {
                let pos_code = 0x80 | (seg << 4) | step;
                let neg_code = (seg << 4) | step;
                let pos_val = mu_law_decode(pos_code);
                let neg_val = mu_law_decode(neg_code);
                assert_eq!(
                    pos_val, -neg_val,
                    "Positive code 0x{pos_code:02X}={pos_val} should be \
                     opposite of negative code 0x{neg_code:02X}={neg_val}"
                );
            }
        }
    }

    #[test]
    fn test_mu_law_monotonic_positive() {
        // In the positive half (codes 0x80-0xFF), higher codes should decode
        // to larger magnitudes (within each segment).
        for seg in 0..8u8 {
            let base = 0x80 | (seg << 4);
            for step in 1..16u8 {
                let prev = mu_law_decode(base | (step - 1));
                let curr = mu_law_decode(base | step);
                assert!(
                    curr >= prev,
                    "Positive codes should be monotonically increasing: \
                     code 0x{:02X}={prev} -> 0x{:02X}={curr}",
                    base | (step - 1),
                    base | step
                );
            }
        }
    }

    #[test]
    fn test_mu_law_extreme_values() {
        // Encoding extreme i16 values should not panic
        let _ = mu_law_encode(i16::MAX);
        let _ = mu_law_encode(i16::MIN);
        let _ = mu_law_encode(0);
    }

    #[test]
    fn test_pcm_constellation_with_zero_levels() {
        let c = PcmConstellation::new_with_levels(0);
        assert_eq!(c.bits_per_symbol(), 0);
    }

    #[test]
    fn test_pcm_constellation_32_levels() {
        let c = PcmConstellation::new_with_levels(32);
        assert_eq!(c.bits_per_symbol(), 5); // floor(log2(32)) = 5
        assert_eq!(c.usable_levels_per_sign(), 32);
    }

    /// Scrambled data through a reduced constellation: every code sent is one
    /// of its usable codes, and the decoder recovers the data.
    fn assert_reduced_roundtrip(levels: u8, bits: u8) {
        use crate::v90_trellis::TRACEBACK_DEPTH;
        let constellation = PcmConstellation::new_with_levels(levels);
        assert_eq!(constellation.bits_per_symbol(), bits);
        let mut encoder = PcmEncoder::new(constellation.clone());
        let mut decoder = PcmDecoder::new(constellation.clone());
        let mut rng_state: u32 = 0x1234_5678;
        let data: Vec<u8> = (0..600)
            .map(|_| {
                rng_state = rng_state.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (rng_state >> 16) as u8
            })
            .collect();
        for &byte in &data {
            encoder.write_byte(byte);
        }
        let padding = (TRACEBACK_DEPTH * usize::from(bits)).div_ceil(8) + 5;
        for _ in 0..padding {
            encoder.write_byte(0);
        }
        let lowest = 128 - (1u16 << bits);
        let mut magnitudes = std::collections::BTreeSet::new();
        let mut decoded = Vec::new();
        while encoder.has_data() {
            let sample = encoder.next_sample();
            let code = mu_law_encode(sample);
            assert!(
                u16::from(code & 0x7F) >= lowest,
                "{levels}-level constellation sent code {code:02X}, below its usable range"
            );
            assert_eq!(constellation.nearest_code(sample), code);
            magnitudes.insert(code & 0x7F);
            decoder.feed_sample(sample);
            while let Some(byte) = decoder.take_byte() {
                decoded.push(byte);
            }
        }
        assert_eq!(
            magnitudes.len(),
            1usize << bits,
            "scrambled data reaches every usable magnitude and no other"
        );
        assert!(
            decoded.len() >= data.len(),
            "decoded {} of {}",
            decoded.len(),
            data.len()
        );
        assert_eq!(
            &decoded[..data.len()],
            &data[..],
            "{levels}-level round trip"
        );
    }

    #[test]
    fn a_32_level_constellation_sends_only_its_codes_and_round_trips() {
        assert_reduced_roundtrip(32, 5);
    }

    #[test]
    fn a_64_level_constellation_sends_only_its_codes_and_round_trips() {
        assert_reduced_roundtrip(64, 6);
    }

    #[test]
    fn each_symbol_consumes_the_constellations_bits_per_symbol() {
        for (levels, bits) in [(128u8, 7usize), (64, 6), (32, 5)] {
            let mut encoder = PcmEncoder::new(PcmConstellation::new_with_levels(levels));
            for byte in 0..60u8 {
                encoder.write_byte(byte);
            }
            let mut symbols = 0;
            while encoder.has_data() {
                encoder.next_sample();
                symbols += 1;
            }
            assert_eq!(
                symbols,
                60 * 8 / bits,
                "{levels} levels carry {bits} bits a symbol"
            );
        }
    }

    #[test]
    fn test_pcm_encoder_next_sample_without_data() {
        let constellation = PcmConstellation::new_full();
        let mut encoder = PcmEncoder::new(constellation);
        // Without any data, next_sample should still produce a sample (idle
        // fill) and not panic. The return type already proves the value is a
        // valid i16, so we just discard it.
        let _sample = encoder.next_sample();
    }
}
