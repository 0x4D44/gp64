//! Per-`Cpu` direct-mapped cache of decoded x87 descriptors.
//!
//! The interpreter's 0xD8-0xDF arm decodes a portable x87 descriptor from raw
//! instruction bytes. Decoding costs far more than the dispatch it feeds, so
//! decoded descriptors are cached direct-mapped by physical position. An entry
//! owns the exact encoded bytes it was decoded from and is revalidated against
//! the live instruction stream on every probe, so the cache stays sound across
//! self-modifying code with zero coupling to the memory write path. Owning the
//! cache per `Cpu` instance (rather than per host thread, as the previous
//! thread-local design did) means emulator instances can never observe each
//! other's slots and the hot path pays no `LocalKey` access.

use crate::fpu::X87Dispatch;
use crate::x87_decode::{decode_x87, CodeBitness, DecodedX87, X87DecodeError, X87Operation};

const X87_DECODE_CACHE_SIZE: usize = 512;

/// One cached descriptor. The encoded bytes are packed as two overlapping
/// little-endian words (`lo` = bytes 0..8, `hi` = bytes 7..15) with masks
/// selecting only the instruction's own `length` bytes, so revalidation is two
/// masked 64-bit compares instead of a byte-loop memcmp.
#[derive(Clone, Copy)]
struct Entry {
    lo: u64,
    hi: u64,
    lo_mask: u64,
    hi_mask: u64,
    bitness: u8,
    instruction: DecodedX87,
    dispatch: X87Dispatch,
}

/// Runtime-only decode cache. Excluded from `CpuSnapshot` and untouched by
/// `Cpu::reset` (entries are byte-revalidated, so stale entries are sound;
/// same convention as `Cpu::instr_cache`).
#[derive(Clone)]
pub struct X87DecodeCache {
    slots: Box<[Option<Entry>]>,
    /// Lifetime hit counter (same convention as `Cpu::cache_hits`).
    pub hits: u64,
    /// Lifetime miss counter.
    pub misses: u64,
}

impl Default for X87DecodeCache {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn packed_words(bytes: &[u8; 15]) -> (u64, u64) {
    let lo = u64::from_le_bytes(bytes[0..8].try_into().expect("8-byte slice"));
    let hi = u64::from_le_bytes(bytes[7..15].try_into().expect("8-byte slice"));
    (lo, hi)
}

/// Masks selecting bytes `0..len` of the packed word pair. `lo` covers bytes
/// `0..8`; `hi` covers bytes `7..15` (byte 7 is redundantly checked by both
/// when `len > 7`, which is harmless).
#[inline]
fn length_masks(len: usize) -> (u64, u64) {
    debug_assert!((1..=15).contains(&len));
    let lo_bytes = len.min(8);
    let hi_bytes = len.saturating_sub(7).min(8);
    let mask = |n: usize| {
        if n >= 8 {
            u64::MAX
        } else {
            (1u64 << (8 * n)) - 1
        }
    };
    (mask(lo_bytes), mask(hi_bytes))
}

/// Decode one x87 instruction and classify its dispatch without touching any
/// cache. Used directly for temporary fetch buffers, which have no stable
/// physical-site hint.
#[inline]
pub fn decode_uncached(
    bytes: &[u8],
    bitness: u8,
) -> Result<(DecodedX87, X87Dispatch), X87DecodeError> {
    let code_bitness = if bitness == 32 {
        CodeBitness::Bits32
    } else {
        CodeBitness::Bits16
    };
    let instruction = decode_x87(bytes, code_bitness)?;
    let dispatch = X87Dispatch::classify(&instruction);
    Ok((instruction, dispatch))
}

impl X87DecodeCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: std::iter::repeat_with(|| None)
                .take(X87_DECODE_CACHE_SIZE)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            hits: 0,
            misses: 0,
        }
    }

    /// Probe with the 15 instruction-stream bytes at physical position `pos`,
    /// decoding and installing on miss. Returns the descriptor and its cached
    /// dispatch classification by value.
    ///
    /// Only descriptors with a valid operation are installed; invalid decodes
    /// and decode errors always re-decode (matching the previous design, so a
    /// site that becomes valid via self-modification is picked up).
    #[inline]
    pub fn lookup(
        &mut self,
        pos: usize,
        bytes: &[u8; 15],
        bitness: u8,
    ) -> Result<(DecodedX87, X87Dispatch), X87DecodeError> {
        // Mix the page number into the low address bits so hot x87 sites at
        // the same page offset don't fight for one direct-mapped slot.
        let index = (pos ^ (pos >> 11)) & (X87_DECODE_CACHE_SIZE - 1);
        let (lo, hi) = packed_words(bytes);
        if let Some(entry) = &self.slots[index] {
            if entry.bitness == bitness
                && (entry.lo ^ lo) & entry.lo_mask == 0
                && (entry.hi ^ hi) & entry.hi_mask == 0
            {
                self.hits += 1;
                return Ok((entry.instruction, entry.dispatch));
            }
        }
        self.misses += 1;
        let (instruction, dispatch) = decode_uncached(bytes, bitness)?;
        if instruction.operation != X87Operation::Invalid {
            let (lo_mask, hi_mask) = length_masks(usize::from(instruction.length));
            self.slots[index] = Some(Entry {
                lo,
                hi,
                lo_mask,
                hi_mask,
                bitness,
                instruction,
                dispatch,
            });
        }
        Ok((instruction, dispatch))
    }

    #[cfg(test)]
    fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x87_decode::{X87AddressRegister, X87Operand};

    fn bytes15(prefix: &[u8]) -> [u8; 15] {
        let mut padded = [0u8; 15];
        padded[..prefix.len()].copy_from_slice(prefix);
        padded
    }

    fn lookup_at(
        cache: &mut X87DecodeCache,
        backing: &[u8],
        pos: usize,
        bitness: u8,
    ) -> Result<DecodedX87, X87DecodeError> {
        let window: &[u8; 15] = backing[pos..pos + 15].try_into().expect("15-byte window");
        cache.lookup(pos, window, bitness).map(|(instr, _)| instr)
    }

    #[test]
    fn cache_hits_identical_instruction_bytes() {
        let mut cache = X87DecodeCache::new();
        let bytes = bytes15(&[0xD9, 0xE8]); // FLD1

        let first = lookup_at(&mut cache, &bytes, 0, 32);
        let second = lookup_at(&mut cache, &bytes, 0, 32);

        assert_eq!(first.unwrap().operation, X87Operation::Fld1);
        assert_eq!(second, first);
        assert_eq!(cache.stats(), (1, 1));
    }

    #[test]
    fn cache_redecodes_self_modified_bytes() {
        let mut cache = X87DecodeCache::new();
        let mut bytes = bytes15(&[0xD9, 0xE8]); // FLD1
        let before = lookup_at(&mut cache, &bytes, 0, 32);

        bytes[1] = 0xEE; // FLDZ at the same physical address
        let after = lookup_at(&mut cache, &bytes, 0, 32);

        assert_eq!(before.unwrap().operation, X87Operation::Fld1);
        assert_eq!(after.unwrap().operation, X87Operation::Fldz);
        assert_eq!(cache.stats(), (0, 2));
    }

    #[test]
    fn cache_invalidates_semantic_operation_with_bytes() {
        let mut cache = X87DecodeCache::new();
        let mut bytes = bytes15(&[0xD8, 0x00]); // FADD m32 [EAX]
        let before = lookup_at(&mut cache, &bytes, 0, 32);
        assert_eq!(before.unwrap().operation, X87Operation::Fadd);

        bytes[1] = 0x08; // FMUL m32 [EAX] at the same physical address
        let after = lookup_at(&mut cache, &bytes, 0, 32);
        assert_eq!(after.unwrap().operation, X87Operation::Fmul);
        assert_eq!(cache.stats(), (0, 2));
    }

    #[test]
    fn cache_redecodes_length_changing_self_modification() {
        let mut cache = X87DecodeCache::new();
        let mut bytes = bytes15(&[0xD9, 0xE8]); // FLD1, 2 bytes
        let before = lookup_at(&mut cache, &bytes, 0, 32);

        bytes[..6].copy_from_slice(&[0xD9, 0x05, 0x78, 0x56, 0x34, 0x12]); // FLD m32, 6 bytes
        let after = lookup_at(&mut cache, &bytes, 0, 32);

        assert_eq!(before.unwrap().length, 2);
        let after = after.unwrap();
        assert_eq!(after.length, 6);
        let X87Operand::Memory(memory) = after.operand1 else {
            panic!("expected memory operand");
        };
        assert_eq!(memory.displacement_raw, 0x1234_5678);
        assert_eq!(cache.stats(), (0, 2));
    }

    #[test]
    fn cache_keys_code_bitness() {
        let mut cache = X87DecodeCache::new();
        let bytes = bytes15(&[0xD9, 0x00]);

        let decoded16 = lookup_at(&mut cache, &bytes, 0, 16);
        let decoded32 = lookup_at(&mut cache, &bytes, 0, 32);

        let X87Operand::Memory(memory16) = decoded16.unwrap().operand1 else {
            panic!("expected 16-bit memory operand");
        };
        let X87Operand::Memory(memory32) = decoded32.unwrap().operand1 else {
            panic!("expected 32-bit memory operand");
        };
        assert_eq!(memory16.base, Some(X87AddressRegister::Bx));
        assert_eq!(memory16.index, Some(X87AddressRegister::Si));
        assert_eq!(memory32.base, Some(X87AddressRegister::Eax));
        assert_eq!(cache.stats(), (0, 2));
    }

    #[test]
    fn cache_reuses_address_independent_descriptor_on_collision() {
        let mut cache = X87DecodeCache::new();
        // These positions collide in the direct-mapped cache. Identical bytes
        // share one address-independent descriptor; runtime state is not baked
        // in.
        let mut bytes = vec![0u8; 544];
        bytes[..2].copy_from_slice(&[0xD9, 0xE8]);
        bytes[512..514].copy_from_slice(&[0xD9, 0xE8]);

        let first = lookup_at(&mut cache, &bytes, 0, 32);
        let second = lookup_at(&mut cache, &bytes, 512, 32);

        assert_eq!(first, second);
        assert_eq!(first.unwrap().length, 2);
        assert_eq!(cache.stats(), (1, 1));
    }

    #[test]
    fn cache_matches_fresh_decoder_for_all_escape_modrm_pairs() {
        for bitness in [16u8, 32] {
            for escape in 0xD8u8..=0xDF {
                for modrm in 0u8..=u8::MAX {
                    let mut cache = X87DecodeCache::new();
                    let mut bytes = [0u8; 15];
                    bytes[0] = escape;
                    bytes[1] = modrm;
                    for (index, byte) in bytes[2..].iter_mut().enumerate() {
                        *byte = (index as u8).wrapping_mul(29).wrapping_add(modrm);
                    }

                    let fresh = decode_uncached(&bytes, bitness).map(|(instr, _)| instr);
                    let first = lookup_at(&mut cache, &bytes, 0, bitness);
                    let cached = lookup_at(&mut cache, &bytes, 0, bitness);

                    assert_eq!(
                        first, fresh,
                        "first decode {bitness}-bit {escape:02X} {modrm:02X}"
                    );
                    assert_eq!(
                        cached, fresh,
                        "cached decode {bitness}-bit {escape:02X} {modrm:02X}"
                    );
                    assert_eq!(
                        cache.stats(),
                        if fresh.unwrap().operation == X87Operation::Invalid {
                            (0, 2)
                        } else {
                            (1, 1)
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn cache_matches_prefixed_and_maximum_length_forms() {
        for (bitness, bytes) in [
            (32u8, &[0x26, 0x67, 0xD9, 0x00][..]),
            (16u8, &[0x64, 0x67, 0xD9, 0x00][..]),
            (
                32u8,
                &[
                    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
                    0xD9, 0xE8,
                ][..],
            ),
        ] {
            let mut cache = X87DecodeCache::new();
            let padded = bytes15(bytes);
            let fresh = decode_uncached(&padded, bitness).map(|(instr, _)| instr);
            let first = lookup_at(&mut cache, &padded, 0, bitness);
            let cached = lookup_at(&mut cache, &padded, 0, bitness);

            assert_eq!(first, fresh);
            assert_eq!(cached, fresh);
            assert_eq!(cache.stats(), (1, 1));
        }

        let mut cache = X87DecodeCache::new();
        let invalid = [0x66u8; 15];
        let first = lookup_at(&mut cache, &invalid, 0, 32);
        let second = lookup_at(&mut cache, &invalid, 0, 32);
        assert_eq!(first, Err(X87DecodeError::InstructionTooLong));
        assert_eq!(second, first);
        assert_eq!(cache.stats(), (0, 2));
    }

    #[test]
    fn masked_compare_covers_every_instruction_length() {
        // A one-byte trailing difference beyond the instruction's own length
        // must still hit; a difference inside it must miss. Exercise the
        // lo/hi mask split across the 7/8-byte boundary.
        for len in 2..=15usize {
            let (lo_mask, hi_mask) = length_masks(len);
            for changed in 0..15usize {
                let base = [0xA5u8; 15];
                let mut altered = base;
                altered[changed] ^= 0xFF;
                let (b_lo, b_hi) = packed_words(&base);
                let (a_lo, a_hi) = packed_words(&altered);
                let matches = (b_lo ^ a_lo) & lo_mask == 0 && (b_hi ^ a_hi) & hi_mask == 0;
                assert_eq!(matches, changed >= len, "len {len} changed byte {changed}");
            }
        }
    }
}
