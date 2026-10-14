//! Byte-validated decode cache for legacy SSE/SSE2 instructions.
//!
//! SSE instructions are sparse in the `0F` map, so the interpreter keeps its
//! compact opcode switch and only reaches this cache for SIMD candidates. A
//! cache entry owns the bytes that produced the decode and validates them on
//! every hit. That keeps self-modifying DOS code correct without coupling the
//! cache to every memory-write path.

use crate::sse_decode::{self, SseDecode, SseInstr};

const SSE_DECODE_CACHE_SIZE: usize = 512;

#[inline]
fn packed_words(bytes: &[u8; 15]) -> (u64, u64) {
    let lo = u64::from_le_bytes(bytes[..8].try_into().expect("eight-byte low word"));
    let hi = u64::from_le_bytes(bytes[7..].try_into().expect("eight-byte high word"));
    (lo, hi)
}

#[inline]
fn length_masks(length: usize) -> (u64, u64) {
    debug_assert!((1..=15).contains(&length));
    let mask = |bytes: usize| match bytes {
        8.. => u64::MAX,
        0 => 0,
        n => (1u64 << (n * 8)) - 1,
    };
    (mask(length.min(8)), mask(length.saturating_sub(7).min(8)))
}

/// What one cached decode of our own decoder produced.
///
/// A valid-but-not-ours encoding is cached too, exactly as the pre-port iced
/// cache cached it: hot non-SSE encodings that reach the SIMD admission guard
/// (`66 0F 1F` NOP padding, `66 0F B6` MOVZX, …) must keep hitting the cache
/// and falling through cheaply rather than re-decoding every execution.
#[derive(Clone, Copy)]
enum CachedDecode {
    Decoded(SseInstr),
    NotOurs { consumed: u8 },
}

#[derive(Clone, Copy)]
struct InstrEntry {
    lo: u64,
    hi: u64,
    lo_mask: u64,
    hi_mask: u64,
    bitness: u8,
    decode: CachedDecode,
}

/// Runtime-only per-CPU decode cache for [`sse_decode`]. Snapshotting this
/// would add no guest state; stale entries are safe because every hit
/// rechecks the encoded bytes.
pub struct SseInstrCache {
    slots: Box<[Option<InstrEntry>]>,
    pub hits: u64,
    pub misses: u64,
}

impl Default for SseInstrCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SseInstrCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: std::iter::repeat_with(|| None)
                .take(SSE_DECODE_CACHE_SIZE)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            hits: 0,
            misses: 0,
        }
    }

    /// Decode the live 15-byte fetch window, reusing a byte-identical entry.
    ///
    /// A `Decoded` entry validates the `len` bytes the instruction occupies; a
    /// `NotOurs` entry validates the `consumed` bytes the rejection decision
    /// read, which is what lets `0F AE /7` (not ours) and `0F AE /2` (ours)
    /// occupy the same slot without aliasing — the ModR/M byte is inside the
    /// validated window.
    ///
    /// A full 15-byte window can never truncate, so `Truncated` never reaches
    /// the cache.
    #[inline]
    pub fn lookup(&mut self, position: usize, bytes: &[u8; 15], bitness: u8) -> SseDecode {
        let index = (position ^ (position >> 11)) & (SSE_DECODE_CACHE_SIZE - 1);
        let (lo, hi) = packed_words(bytes);
        if let Some(entry) = self.slots[index] {
            if entry.bitness == bitness
                && (entry.lo ^ lo) & entry.lo_mask == 0
                && (entry.hi ^ hi) & entry.hi_mask == 0
            {
                self.hits += 1;
                return match entry.decode {
                    CachedDecode::Decoded(instr) => SseDecode::Decoded(instr),
                    CachedDecode::NotOurs { consumed } => SseDecode::NotOurs { consumed },
                };
            }
        }

        self.misses += 1;
        let decoded = sse_decode::decode(bytes, bitness);
        let (decode, length) = match decoded {
            SseDecode::Decoded(instr) => (CachedDecode::Decoded(instr), usize::from(instr.len)),
            SseDecode::NotOurs { consumed } => (
                CachedDecode::NotOurs { consumed },
                usize::from(consumed).max(1),
            ),
            SseDecode::Truncated { .. } => return decoded,
        };
        let (lo_mask, hi_mask) = length_masks(length);
        self.slots[index] = Some(InstrEntry {
            lo,
            hi,
            lo_mask,
            hi_mask,
            bitness,
            decode,
        });
        decoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse_decode::SseOp;

    fn padded(bytes: &[u8]) -> [u8; 15] {
        let mut result = [0u8; 15];
        result[..bytes.len()].copy_from_slice(bytes);
        result
    }

    // ------------------------------------------------------------------
    // `SseInstrCache`: the same scheme over our own decoder
    // ------------------------------------------------------------------

    fn decoded_op(decode: SseDecode) -> SseOp {
        match decode {
            SseDecode::Decoded(instr) => instr.op,
            other => panic!("expected Decoded, got {other:?}"),
        }
    }

    #[test]
    fn instr_identical_bytes_hit() {
        let bytes = padded(&[0x0F, 0x58, 0xC1]);
        let mut cache = SseInstrCache::new();
        assert_eq!(decoded_op(cache.lookup(0x1234, &bytes, 32)), SseOp::Addps);
        assert_eq!(decoded_op(cache.lookup(0x1234, &bytes, 32)), SseOp::Addps);
        assert_eq!((cache.hits, cache.misses), (1, 1));
    }

    #[test]
    fn instr_self_modification_redecodes() {
        let mut bytes = padded(&[0x0F, 0x58, 0xC1]);
        let mut cache = SseInstrCache::new();
        assert_eq!(decoded_op(cache.lookup(0x1234, &bytes, 32)), SseOp::Addps);
        bytes[1] = 0x59;
        assert_eq!(decoded_op(cache.lookup(0x1234, &bytes, 32)), SseOp::Mulps);
        assert_eq!((cache.hits, cache.misses), (0, 2));
    }

    #[test]
    fn instr_code_bitness_is_part_of_the_key() {
        // `0F 10 00` is MOVUPS xmm0, [EAX] in 32-bit code and [BX+SI] in
        // 16-bit code: same op, different address expression.
        let bytes = padded(&[0x0F, 0x10, 0x00]);
        let mut cache = SseInstrCache::new();
        let decoded16 = cache.lookup(7, &bytes, 16);
        let decoded32 = cache.lookup(7, &bytes, 32);
        assert_ne!(decoded16, decoded32);
        assert_eq!((cache.hits, cache.misses), (0, 2));
    }

    #[test]
    fn instr_not_ours_sentinel_hits() {
        // `66 0F 1F /0` is multi-byte NOP padding: valid, reaches the SIMD
        // admission guard, and is not ours. It must hit the cache.
        let bytes = padded(&[0x66, 0x0F, 0x1F, 0x00]);
        let mut cache = SseInstrCache::new();
        assert!(matches!(
            cache.lookup(0x40, &bytes, 32),
            SseDecode::NotOurs { .. }
        ));
        assert!(matches!(
            cache.lookup(0x40, &bytes, 32),
            SseDecode::NotOurs { .. }
        ));
        assert_eq!((cache.hits, cache.misses), (1, 1));
    }

    #[test]
    fn instr_not_ours_validates_the_bytes_its_decision_read() {
        // Group 15 splits by ModR/M reg: the memory form of `/7` is CLFLUSH
        // (not ours) while `/2` is LDMXCSR (ours). The rejection read the
        // ModR/M byte, so a cached NotOurs must not satisfy the sibling.
        let mut cache = SseInstrCache::new();
        let clflush = padded(&[0x0F, 0xAE, 0x39]);
        assert!(matches!(
            cache.lookup(0x80, &clflush, 32),
            SseDecode::NotOurs { .. }
        ));
        let ldmxcsr = padded(&[0x0F, 0xAE, 0x11]);
        assert_eq!(decoded_op(cache.lookup(0x80, &ldmxcsr, 32)), SseOp::Ldmxcsr);
        assert_eq!((cache.hits, cache.misses), (0, 2));
    }
}
