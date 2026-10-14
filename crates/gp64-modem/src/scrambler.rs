//! V.32bis self-synchronizing scrambler and descrambler.
//!
//! Uses the ITU-T V.32bis generating polynomial: 1 + x^-18 + x^-23.
//! The scrambler XORs each data bit with the scrambled output delayed
//! by 18 and 23 bits. Self-synchronizing: the descrambler converges
//! after at most 23 bits regardless of initial state.

/// V.32bis scrambler.
#[derive(Clone)]
pub struct Scrambler {
    /// Shift register (at least 23 bits of history).
    register: u32,
}

impl Default for Scrambler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scrambler {
    pub fn new() -> Self {
        Self { register: 0 }
    }

    /// Scramble a single bit. Returns the scrambled bit.
    pub fn scramble_bit(&mut self, bit: bool) -> bool {
        let bit_18 = (self.register >> 17) & 1 != 0;
        let bit_23 = (self.register >> 22) & 1 != 0;
        let output = bit ^ bit_18 ^ bit_23;
        self.register = (self.register << 1) | (output as u32);
        output
    }

    /// Scramble a byte (LSB first). Returns the scrambled byte.
    pub fn scramble_byte(&mut self, byte: u8) -> u8 {
        let mut result = 0u8;
        for i in 0..8 {
            let input_bit = (byte >> i) & 1 != 0;
            let output_bit = self.scramble_bit(input_bit);
            if output_bit {
                result |= 1 << i;
            }
        }
        result
    }

    /// Scramble a block of bytes.
    pub fn scramble(&mut self, input: &[u8], output: &mut Vec<u8>) {
        output.reserve(input.len());
        for &byte in input {
            output.push(self.scramble_byte(byte));
        }
    }

    /// Reset the shift register to zero.
    pub fn reset(&mut self) {
        self.register = 0;
    }

    /// Seed the shift register to an arbitrary value.
    ///
    /// Enables deterministic scrambler initialisation when a known non-zero
    /// starting state is required (e.g. the V.34 Phase 4 E delimiter, where
    /// both sides must use an identical seed to produce a matchable pattern).
    /// A zero seed combined with zero inputs is a fixed point of the LFSR and
    /// produces all-zero output — use a non-zero value to avoid that.
    pub fn seed(&mut self, value: u32) {
        self.register = value;
    }
}

/// V.32bis descrambler.
pub struct Descrambler {
    /// Shift register (at least 23 bits of history).
    register: u32,
}

impl Default for Descrambler {
    fn default() -> Self {
        Self::new()
    }
}

impl Descrambler {
    pub fn new() -> Self {
        Self { register: 0 }
    }

    /// Descramble a single bit. Returns the original data bit.
    pub fn descramble_bit(&mut self, bit: bool) -> bool {
        let bit_18 = (self.register >> 17) & 1 != 0;
        let bit_23 = (self.register >> 22) & 1 != 0;
        let output = bit ^ bit_18 ^ bit_23;
        // Key: feed the RECEIVED (scrambled) bit into the register, not the output.
        self.register = (self.register << 1) | (bit as u32);
        output
    }

    /// Descramble a byte (LSB first). Returns the original byte.
    pub fn descramble_byte(&mut self, byte: u8) -> u8 {
        let mut result = 0u8;
        for i in 0..8 {
            let input_bit = (byte >> i) & 1 != 0;
            let output_bit = self.descramble_bit(input_bit);
            if output_bit {
                result |= 1 << i;
            }
        }
        result
    }

    /// Descramble a block of bytes.
    pub fn descramble(&mut self, input: &[u8], output: &mut Vec<u8>) {
        output.reserve(input.len());
        for &byte in input {
            output.push(self.descramble_byte(byte));
        }
    }

    /// Reset the shift register to zero.
    pub fn reset(&mut self) {
        self.register = 0;
    }

    /// Seed the shift register to an arbitrary value.
    ///
    /// Mirrors `Scrambler::seed`. Used to bring the descrambler into a known
    /// synchronised state at data-mode entry, matching the peer's TX scrambler
    /// seed so both sides start the byte stream in identical register states.
    ///
    /// V.34 §11.4 Phase 4 sync — see wrk_docs/2026.04.25 PLAN, Step 5.
    pub fn seed(&mut self, value: u32) {
        self.register = value;
    }

    /// Current register state (for debug/diagnostics).
    pub fn register_state(&self) -> u32 {
        self.register
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scramble_descramble_byte() {
        let mut scrambler = Scrambler::new();
        let mut descrambler = Descrambler::new();

        let original = 0xA5u8;
        let scrambled = scrambler.scramble_byte(original);
        let recovered = descrambler.descramble_byte(scrambled);
        assert_eq!(recovered, original);
    }

    #[test]
    fn test_scramble_descramble_block() {
        let mut scrambler = Scrambler::new();
        let mut descrambler = Descrambler::new();

        let input = b"Hello World";
        let mut scrambled = Vec::new();
        scrambler.scramble(input, &mut scrambled);

        let mut recovered = Vec::new();
        descrambler.descramble(&scrambled, &mut recovered);

        assert_eq!(&recovered, input);
    }

    #[test]
    fn test_scramble_changes_data() {
        let mut scrambler = Scrambler::new();

        let input = b"Hello World";
        let mut scrambled = Vec::new();
        scrambler.scramble(input, &mut scrambled);

        // The scrambled output must differ from the input.
        assert_ne!(&scrambled[..], &input[..]);
    }

    #[test]
    fn test_self_synchronizing() {
        // Scrambler starts with register = 0, descrambler with a different state.
        // After 23 bits (3 bytes) the descrambler must synchronize.
        let mut scrambler = Scrambler::new();
        let mut descrambler = Descrambler::new();
        descrambler.register = 0x7FFFFF; // All 23 low bits set — maximally wrong.

        // Feed 4 bytes (32 bits) through — first 23 bits may be garbage.
        let preamble = [0xAA, 0x55, 0xAA, 0x55];
        let mut scrambled_preamble = Vec::new();
        scrambler.scramble(&preamble, &mut scrambled_preamble);

        let mut descrambled_preamble = Vec::new();
        descrambler.descramble(&scrambled_preamble, &mut descrambled_preamble);
        // After the preamble, registers are synchronized.

        // Now all subsequent data must round-trip perfectly.
        let payload = b"After sync this must match exactly!";
        let mut scrambled_payload = Vec::new();
        scrambler.scramble(payload, &mut scrambled_payload);

        let mut recovered_payload = Vec::new();
        descrambler.descramble(&scrambled_payload, &mut recovered_payload);

        assert_eq!(&recovered_payload[..], &payload[..]);
    }

    #[test]
    fn test_scramble_random_data() {
        let mut scrambler = Scrambler::new();
        let mut descrambler = Descrambler::new();

        // Simple PRNG (xorshift32) for deterministic "random" data.
        let mut state: u32 = 0xDEAD_BEEF;
        let mut input = Vec::with_capacity(1000);
        for _ in 0..1000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            input.push(state as u8);
        }

        let mut scrambled = Vec::new();
        scrambler.scramble(&input, &mut scrambled);

        let mut recovered = Vec::new();
        descrambler.descramble(&scrambled, &mut recovered);

        assert_eq!(recovered, input);
    }

    #[test]
    fn test_scramble_all_zeros() {
        let mut scrambler = Scrambler::new();

        // Seed the register so the scrambler has something to work with.
        // With register=0 and input=0, output stays 0 (fixed point).
        // Real modems send a training sequence first; here we just verify
        // that a seeded scrambler produces non-zero output for zero input.
        scrambler.register = 1;

        let input = vec![0u8; 32];
        let mut scrambled = Vec::new();
        scrambler.scramble(&input, &mut scrambled);

        let any_nonzero = scrambled.iter().any(|&b| b != 0);
        assert!(
            any_nonzero,
            "scrambler with non-zero seed must produce non-zero output for all-zero input"
        );
    }

    #[test]
    fn test_scramble_all_ones() {
        let mut scrambler = Scrambler::new();

        // With all-ones input and register=0 the first 18 outputs are 1
        // (no delayed feedback yet), then the polynomial kicks in and
        // flips some bits, producing non-0xFF bytes.
        let input = vec![0xFFu8; 32];
        let mut scrambled = Vec::new();
        scrambler.scramble(&input, &mut scrambled);

        let any_not_ff = scrambled.iter().any(|&b| b != 0xFF);
        assert!(
            any_not_ff,
            "scrambler must alter all-ones input once delay taps engage"
        );
    }

    #[test]
    fn test_scramble_bit_roundtrip() {
        let mut scrambler = Scrambler::new();
        let mut descrambler = Descrambler::new();

        // Bit-level roundtrip
        let bits = [true, false, true, true, false, false, true, false];
        for &bit in &bits {
            let scrambled = scrambler.scramble_bit(bit);
            let recovered = descrambler.descramble_bit(scrambled);
            assert_eq!(recovered, bit);
        }
    }

    #[test]
    fn test_scrambler_reset() {
        let mut s1 = Scrambler::new();
        let mut s2 = Scrambler::new();

        // Advance s1
        s1.scramble_byte(0xAA);
        s1.scramble_byte(0x55);

        // Reset should bring it back to initial state
        s1.reset();

        // Both should produce the same output
        assert_eq!(s1.scramble_byte(0xAA), s2.scramble_byte(0xAA));
    }

    #[test]
    fn test_descrambler_reset() {
        let mut d1 = Descrambler::new();
        let mut d2 = Descrambler::new();

        d1.descramble_byte(0xAA);
        d1.descramble_byte(0x55);
        d1.reset();

        assert_eq!(d1.descramble_byte(0xAA), d2.descramble_byte(0xAA));
    }

    #[test]
    fn test_default_trait() {
        let s = Scrambler::default();
        let d = Descrambler::default();
        // Default should produce same output as new()
        let mut s2 = s.clone();
        let mut d2 = Descrambler::new();
        let scrambled = s2.scramble_byte(0x42);
        let recovered = d2.descramble_byte(scrambled);
        let _ = d;
        assert_eq!(recovered, 0x42);
    }

    #[test]
    fn test_descramble_block() {
        let mut scrambler = Scrambler::new();
        let mut descrambler = Descrambler::new();

        let input = vec![0x00, 0x55, 0xAA, 0xFF];
        let mut scrambled = Vec::new();
        scrambler.scramble(&input, &mut scrambled);

        let mut recovered = Vec::new();
        descrambler.descramble(&scrambled, &mut recovered);
        assert_eq!(recovered, input);
    }

    #[test]
    fn test_scrambler_state_dependent() {
        // Same input at different register states should produce different output
        // over a sequence of bytes (individual bytes may coincidentally match).
        let mut s1 = Scrambler::new();
        let mut s2 = Scrambler::new();

        let mut out1 = Vec::new();
        s1.scramble(&[0x42; 4], &mut out1);

        // Feed several bytes first to build up different state
        s2.scramble_byte(0xFF);
        s2.scramble_byte(0xAA);
        s2.scramble_byte(0x55);
        let mut out2 = Vec::new();
        s2.scramble(&[0x42; 4], &mut out2);
        assert_ne!(
            out1, out2,
            "Same input at different states should differ over a sequence"
        );
    }

    #[test]
    fn test_scramble_descramble_long_stream() {
        let mut scrambler = Scrambler::new();
        let mut descrambler = Descrambler::new();

        // Test a long stream (covers many register cycles)
        let input: Vec<u8> = (0..=255).cycle().take(2048).collect();
        let mut scrambled = Vec::new();
        scrambler.scramble(&input, &mut scrambled);

        let mut recovered = Vec::new();
        descrambler.descramble(&scrambled, &mut recovered);
        assert_eq!(recovered, input);
    }
}
