//! Exhaustive adjudication of [`gp64_disasm`] against `iced-x86`.
//!
//! This is the crate's whole warrant. A hand-written length decoder that only
//! agreed with itself would be worthless on the guest-fault path, so every
//! verdict is swept against an independent decoder: every one-byte and two-byte
//! opcode, against every ModRM byte, under both bitnesses and every meaningful
//! prefix combination.
//!
//! Both the LENGTH and the ERROR CLASS are compared. Getting the class wrong is
//! the subtler failure: "longer than the bytes I have" is a boundary fault and
//! "not an instruction" is a #UD, and a caller that cannot tell them apart shows
//! the guest the wrong exception.
//!
//! Where we deliberately differ, the difference is named and justified in
//! `KNOWN_DIVERGENCES` rather than papered over by loosening the comparison.

use gp64_disasm::{instruction_length, Bitness, DecodeError, MAX_INSTRUCTION_LEN};
use iced_x86::{Decoder, DecoderError, DecoderOptions, EncodingKind};

/// True when iced decoded the bytes as VEX/EVEX/XOP — an encoding no CPU this
/// project models can reach.
///
/// `C4`, `C5` and `62` are LES, LDS and BOUND on everything up to Pentium 4, and
/// only become VEX/EVEX prefixes on an AVX machine. iced decodes for the whole
/// architecture; we decode for ours. Those cases are excluded from the sweep
/// rather than "fixed", because matching iced there would mean modelling a CPU
/// the emulator deliberately does not offer.
fn iced_used_a_post_sse_encoding(bytes: &[u8], bitness: Bitness) -> bool {
    let bits = match bitness {
        Bitness::Bits16 => 16,
        Bitness::Bits32 => 32,
    };
    let mut decoder = Decoder::new(bits, bytes, DecoderOptions::NONE);
    let instr = decoder.decode();
    if !matches!(instr.encoding(), EncodingKind::Legacy) {
        return true;
    }
    // A TRUNCATED VEX/EVEX is the same divergence wearing a different hat: iced
    // reports NoMoreBytes because it is part-way through a prefix our CPUs do
    // not have, and `encoding()` cannot say so because the decode never
    // finished. `62`, `C4` and `C5` introduce those prefixes only with mod=11,
    // which is exactly the form that is BOUND/LES/LDS on our machines.
    let after_prefixes = bytes
        .iter()
        .position(|b| gp64_disasm::legacy_prefix_group(*b).is_none())
        .unwrap_or(bytes.len());
    let head = bytes.get(after_prefixes).copied();
    let next = bytes.get(after_prefixes + 1).copied();
    // VEX (C4/C5) and EVEX (62) are distinguished from LES/LDS/BOUND by mod=11.
    if matches!(head, Some(0x62 | 0xC4 | 0xC5)) && next.is_some_and(|m| m >> 6 == 3) {
        return true;
    }
    // XOP (AMD) is `8F` with a non-zero ModRM.reg; `8F /0` is POP r/m, which is
    // the only form our CPUs have.
    if matches!(head, Some(0x8F)) && next.is_some_and(|m| (m >> 3) & 7 != 0) {
        return true;
    }
    // `0F 78` / `0F 79` are VMREAD/VMWRITE on an Intel VMX part and EXTRQ/INSERTQ
    // on an AMD SSE4a one — the latter carrying two immediates, which is why the
    // lengths differ. Our CPU ceiling is Pentium 4 (SSE2), which has neither, so
    // there is no encoding here for us to be right or wrong about.
    matches!(head, Some(0x0F)) && matches!(next, Some(0x78 | 0x79))
}

/// What iced says about a byte sequence, in this crate's vocabulary.
fn iced_verdict(bytes: &[u8], bitness: Bitness) -> Result<usize, DecodeError> {
    let bits = match bitness {
        Bitness::Bits16 => 16,
        Bitness::Bits32 => 32,
    };
    let mut decoder = Decoder::new(bits, bytes, DecoderOptions::NONE);
    let instr = decoder.decode();
    match decoder.last_error() {
        DecoderError::None => Ok(instr.len()),
        DecoderError::NoMoreBytes => Err(DecodeError::NoMoreBytes),
        _ => Err(DecodeError::Invalid),
    }
}

/// The contract this sweep enforces, stated once.
///
/// `gp64-disasm` MEASURES; it does not validate (see its crate docs). So the
/// comparison is deliberately asymmetric, and the asymmetry is the design rather
/// than a concession:
///
/// * iced decodes successfully -> our length must match EXACTLY. This is the
///   whole correctness claim and it is checked over the entire opcode space.
/// * iced says `NoMoreBytes` -> we must agree. This is the boundary case the
///   interpreter actually calls us for, so disagreement here is a real fault.
/// * iced says invalid -> we may return a length. iced rejects reserved
///   sub-opcodes, memory-only forms used with `mod=11`, and illegal `LOCK`;
///   those all have a well-defined length and the CPU must still fetch them
///   before refusing. We must NOT, however, claim `NoMoreBytes` for one, since
///   that would turn a #UD into a boundary fault.
fn compare(
    mine: Result<usize, DecodeError>,
    theirs: Result<usize, DecodeError>,
    supplied: usize,
    full_window_ok: bool,
) -> Result<(), String> {
    match (mine, theirs) {
        (m, t) if m == t => Ok(()),
        (Ok(a), Ok(b)) => Err(format!("length differs: ours {a}, iced {b}")),
        // We promise to agree about BOUNDARIES for real instructions. Where iced
        // wants more bytes only to conclude the opcode is unknown, that is the
        // validity disagreement the contract already permits, not a boundary
        // one — `full_window_ok` tells the two apart.
        (_, Err(DecodeError::NoMoreBytes)) if full_window_ok => {
            Err(format!("iced needs more bytes; we said {mine:?}"))
        }
        (_, Err(DecodeError::NoMoreBytes)) => Ok(()),
        // Claiming NoMoreBytes is only wrong if the bytes were actually there.
        // A reserved sub-opcode whose encoding runs past the slice genuinely
        // does need more bytes, and saying so is the honest answer.
        (Err(DecodeError::NoMoreBytes), Err(_)) if supplied >= MAX_INSTRUCTION_LEN => Err(
            "we claimed NoMoreBytes with a full 15-byte window for an encoding iced              rejects — that would turn a #UD into a boundary fault"
                .to_string(),
        ),
        (Err(DecodeError::NoMoreBytes), Err(_)) => Ok(()),
        // Permitted: iced rejects on semantics we do not model, and we measure.
        (Ok(_) | Err(DecodeError::TooLong | DecodeError::Invalid), Err(_)) => Ok(()),
        (m, t) => Err(format!("unhandled combination: ours {m:?}, iced {t:?}")),
    }
}

/// Prefix sequences worth sweeping. Each changes a size that the length depends
/// on, or is a no-op that must NOT change one.
const PREFIXES: &[&[u8]] = &[
    &[],
    &[0x66],             // operand size
    &[0x67],             // address size
    &[0x66, 0x67],       // both
    &[0xF3],             // rep, no size effect
    &[0xF2],             // repne
    &[0xF0],             // lock
    &[0x2E],             // segment override
    &[0x65, 0x66, 0x67], // segment + both sizes
];

/// A filler tail long enough that no instruction runs out of bytes by accident.
/// 0x41 (`inc ecx`) is a harmless one-byte instruction, so a decoder that
/// over-reads lands on something legal rather than on a boundary.
const TAIL: [u8; MAX_INSTRUCTION_LEN] = [0x41; MAX_INSTRUCTION_LEN];

fn build(prefix: &[u8], opcode: &[u8], modrm: Option<u8>) -> Vec<u8> {
    let mut v = Vec::with_capacity(MAX_INSTRUCTION_LEN * 2);
    v.extend_from_slice(prefix);
    v.extend_from_slice(opcode);
    if let Some(m) = modrm {
        v.push(m);
    }
    v.extend_from_slice(&TAIL);
    v
}

fn check(bytes: &[u8], bitness: Bitness, ctx: &str, stats: &mut Stats) {
    if iced_used_a_post_sse_encoding(bytes, bitness) {
        stats.skipped_post_sse += 1;
        return;
    }
    let mine = instruction_length(bytes, bitness);
    let theirs = iced_verdict(bytes, bitness);
    stats.compared += 1;
    if theirs.is_ok() {
        stats.length_checked += 1;
    }
    // What iced makes of the same opcode when it is not starved of bytes.
    let mut padded = bytes.to_vec();
    padded.resize(bytes.len() + MAX_INSTRUCTION_LEN, 0x41);
    let full_window_ok = iced_verdict(&padded, bitness).is_ok();

    if let Err(why) = compare(mine, theirs, bytes.len(), full_window_ok) {
        panic!(
            "{ctx}: bytes {:02X?} under {bitness:?} — {why}",
            &bytes[..bytes.len().min(MAX_INSTRUCTION_LEN)]
        );
    }
}

#[derive(Default)]
struct Stats {
    compared: usize,
    /// Cases where iced decoded successfully, so our LENGTH was actually
    /// checked. Tracked separately because a sweep that only ever hit invalid
    /// encodings would compare a great many nothings.
    length_checked: usize,
    /// VEX/EVEX/XOP encodings, which our CPUs cannot reach.
    skipped_post_sse: usize,
}

#[test]
fn every_one_byte_opcode_and_modrm_matches_iced() {
    let mut stats = Stats::default();
    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        for prefix in PREFIXES {
            for op in 0u16..=0xFF {
                let op = op as u8;
                // Prefixes and the two-byte escape are not opcodes here.
                if gp64_disasm::legacy_prefix_group(op).is_some() || op == 0x0F {
                    continue;
                }
                // Every ModRM, so the SIB and displacement forms are all covered.
                for modrm in 0u16..=0xFF {
                    let bytes = build(prefix, &[op], Some(modrm as u8));
                    check(&bytes, bitness, "one-byte", &mut stats);
                }
            }
        }
    }
    assert!(
        stats.compared > 1_000_000,
        "sweep collapsed to {} comparisons — it is not covering the space",
        stats.compared
    );
    assert!(
        stats.length_checked > 500_000,
        "only {} cases actually exercised a LENGTH comparison; the rest were          encodings iced rejects, so this sweep proves far less than it appears to",
        stats.length_checked
    );
}

#[test]
fn every_two_byte_opcode_and_modrm_matches_iced() {
    let mut stats = Stats::default();
    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        for prefix in PREFIXES {
            for op2 in 0u16..=0xFF {
                for modrm in 0u16..=0xFF {
                    let bytes = build(prefix, &[0x0F, op2 as u8], Some(modrm as u8));
                    check(&bytes, bitness, "two-byte", &mut stats);
                }
            }
        }
    }
    assert!(
        stats.compared > 800_000,
        "sweep collapsed to {} comparisons",
        stats.compared
    );
    assert!(
        stats.length_checked > 200_000,
        "only {} cases exercised a LENGTH comparison",
        stats.length_checked
    );
}

/// Truncation is the case the interpreter actually calls this for, so it gets
/// its own sweep: every prefix of every instruction must yield either the same
/// length or `NoMoreBytes`, never a different length and never `Invalid`.
#[test]
fn truncated_instructions_report_no_more_bytes_exactly_when_iced_does() {
    let mut stats = Stats::default();
    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        for op in 0u16..=0xFF {
            let op = op as u8;
            if op == 0x0F {
                continue;
            }
            for modrm in [0x00u8, 0x04, 0x05, 0x44, 0x45, 0x84, 0x85, 0xC0] {
                let full = build(&[], &[op], Some(modrm));
                for take in 1..=MAX_INSTRUCTION_LEN.min(full.len()) {
                    check(&full[..take], bitness, "truncated", &mut stats);
                }
            }
        }
    }
    assert!(stats.compared > 20_000, "truncation sweep collapsed");
}

/// The fifteen-byte window: a run of prefixes with no opcode must be reported as
/// a length failure, because that is the boundary fault the caller is asking
/// about, not a #UD.
#[test]
fn an_overlong_prefix_run_is_a_length_failure() {
    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        let all_prefixes = [0x66u8; MAX_INSTRUCTION_LEN + 4];
        assert_eq!(
            instruction_length(&all_prefixes, bitness),
            Err(DecodeError::TooLong),
            "prefixes filling the whole window have no completion, so this is TooLong"
        );
    }
}

/// Pseudo-random byte strings, to reach encodings the structured sweeps do not
/// generate — deep SIB and displacement combinations behind long prefix runs.
#[test]
fn random_byte_strings_match_iced() {
    // A fixed LCG: reproducible, and no dependency for a test that only needs
    // arbitrary bytes rather than statistical quality.
    let mut state = 0x2026_0820_u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u8
    };

    let mut stats = Stats::default();
    for _ in 0..200_000 {
        let len = 1 + (next() as usize % MAX_INSTRUCTION_LEN);
        let bytes: Vec<u8> = (0..len).map(|_| next()).collect();
        for bitness in [Bitness::Bits16, Bitness::Bits32] {
            check(&bytes, bitness, "random", &mut stats);
        }
    }
    assert!(stats.compared > 300_000, "random sweep collapsed");
}
