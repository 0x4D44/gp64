//! Exhaustive text adjudication of the disassembler against `iced-x86`.
//!
//! The comparison is plain string equality on the formatted instruction, over
//! the whole one-byte opcode space against every ModRM byte, under both
//! bitnesses and several prefix combinations. That is deliberately unforgiving:
//! a disassembler is judged entirely by what it prints, so anything short of
//! character equality would be grading our own homework.
//!
//! Coverage is measured and asserted, not assumed. `Stats` counts the cases that
//! actually produced a comparison, separately from those skipped, so a sweep
//! that silently stopped decoding anything cannot pass.

use gp64_disasm::{disassemble, Bitness, DecodeError};
use iced_x86::{Decoder, DecoderError, DecoderOptions, EncodingKind, Formatter, IntelFormatter};

const RIP: u64 = 0x0040_1000;

/// What iced prints for the instruction at the start of `bytes`, or `None` if it
/// does not decode one this project's CPUs could execute.
fn iced_text_at(bytes: &[u8], bitness: Bitness, rip: u64) -> Option<String> {
    let bits = match bitness {
        Bitness::Bits16 => 16,
        Bitness::Bits32 => 32,
    };
    let mut decoder = Decoder::with_ip(bits, bytes, rip, DecoderOptions::NONE);
    let instr = decoder.decode();
    if decoder.last_error() != DecoderError::None {
        return None;
    }
    if !matches!(instr.encoding(), EncodingKind::Legacy) {
        return None;
    }
    let mut formatter = IntelFormatter::new();
    let mut text = String::new();
    formatter.format(&instr, &mut text);
    Some(text)
}

fn iced_text(bytes: &[u8], bitness: Bitness) -> Option<String> {
    iced_text_at(bytes, bitness, RIP)
}

#[derive(Default)]
struct Stats {
    compared: usize,
    /// Opcodes we do not decode yet — the two-byte map, x87 and the groups that
    /// land in later stages. Counted so the shortfall is visible rather than
    /// looking like success.
    not_ours: usize,
}

const PREFIXES: &[&[u8]] = &[
    &[],
    &[0x66],
    &[0x67],
    &[0x66, 0x67],
    &[0x2E],
    &[0xF0],
    &[0xF3],
];

/// A tail of `inc ecx`, so an over-read lands on something legal.
const TAIL: [u8; 15] = [0x41; 15];

fn build(prefix: &[u8], op: u8, modrm: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(32);
    v.extend_from_slice(prefix);
    v.push(op);
    v.push(modrm);
    v.extend_from_slice(&TAIL);
    v
}

/// `C6 /7` and `C7 /7` are `XABORT` and `XBEGIN` to a decoder that models TSX.
/// Nothing this project emulates has it — the CPU ceiling is Pentium 4 — so
/// there is no encoding here for us to be right or wrong about.
fn is_tsx(bytes: &[u8]) -> bool {
    let after = bytes
        .iter()
        .position(|b| gp64_disasm::legacy_prefix_group(*b).is_none())
        .unwrap_or(bytes.len());
    matches!(bytes.get(after), Some(0xC6 | 0xC7))
        && bytes.get(after + 1).is_some_and(|m| (m >> 3) & 7 != 0)
}

/// The `0F 0D` prefetch group, where we and iced disagree about NAMES.
///
/// AMD documents two mnemonics here — PREFETCH and PREFETCHW — and reserves
/// `/2`-`/7` as encodings that behave as PREFETCH. iced instead prints
/// `prefetch_exclusive`, `prefetch_reserved` and `prefetchwt1`, which are not
/// mnemonics any assembler accepts and, in the last case, name a Knights
/// Landing instruction. A dump naming something you cannot look up is worse
/// than one naming the documented instruction, so AMD's names win and this
/// group is skipped.
///
/// The rest of 3DNow! — FEMMS and the whole `0F 0F` suffix space — is decoded
/// and compared normally.
fn is_3dnow_prefetch_naming(bytes: &[u8]) -> bool {
    let after = bytes
        .iter()
        .position(|b| gp64_disasm::legacy_prefix_group(*b).is_none())
        .unwrap_or(bytes.len());
    let (Some(&0x0F), Some(&0x0D), Some(&modrm)) =
        (bytes.get(after), bytes.get(after + 1), bytes.get(after + 2))
    else {
        return false;
    };
    // Only the memory forms differ; the register forms are reserved NOPs and
    // agree.
    modrm >> 6 != 3
}

/// Encodings iced decodes as an instruction that arrived AFTER the newest CPU
/// this project models (a Pentium 4).
///
/// These are not disagreements about the encoding — they are disagreements
/// about which CPU is being disassembled. iced decodes for the latest silicon;
/// we decode for ours, so where iced sees `xsave` we see a reserved encoding.
/// Comparing them would pin iced's CPU generation into our table, which is the
/// opposite of what this suite is for.
fn is_post_pentium4(bytes: &[u8]) -> bool {
    let after = bytes
        .iter()
        .position(|b| gp64_disasm::legacy_prefix_group(*b).is_none())
        .unwrap_or(bytes.len());
    if bytes.get(after) != Some(&0x0F) {
        return false;
    }
    let (Some(&op2), Some(&modrm)) = (bytes.get(after + 1), bytes.get(after + 2)) else {
        return false;
    };
    let reg = (modrm >> 3) & 7;
    let is_reg_form = modrm >> 6 == 3;
    let saw_f3 = bytes[..after].contains(&0xF3);
    let saw_66 = bytes[..after].contains(&0x66);

    match op2 {
        // `F3 0F 09` is WBNOINVD (Ice Lake); bare, it is the WBINVD we decode.
        0x09 if saw_f3 => true,
        // `F3` turns the bit scans into TZCNT and LZCNT (BMI1 / LZCNT). Our
        // CPUs ignore the prefix and execute BSF/BSR.
        0xBC | 0xBD if saw_f3 => true,
        // Group 7's register forms: ENCLV, MONITOR/MWAIT, XGETBV/XSETBV, the
        // AMD SVM block and SERIALIZE. Every one of them post-dates us.
        0x01 => is_reg_form && matches!(reg, 0..=3 | 5),
        // Group 16 `/6` and `/7` against memory: PREFETCHIT0/PREFETCHIT1, which
        // are Granite Rapids. On our CPUs they are reserved NOPs.
        0x18 => !is_reg_form && matches!(reg, 6 | 7),
        // The MPX bound-register block. Reserved NOPs for us.
        0x1A | 0x1B => true,
        // CLDEMOTE (Tremont) took `/0` against memory. Reserved NOP for us.
        0x1C => !is_reg_form && reg == 0,
        // CET took the `F3`-prefixed forms: ENDBR32/ENDBR64 and RDSSPD.
        // Unprefixed, this is still an ordinary reserved NOP and is compared.
        0x1E => saw_f3,
        // Group 15 `/4`-`/6` against memory: XSAVE, XRSTOR, XSAVEOPT (Nehalem
        // and later). Under `66`, `/7` becomes CLFLUSHOPT (Skylake) rather than
        // the plain CLFLUSH we decode.
        // Under `66` or `F3` the whole group is reassigned on later CPUs —
        // CLFLUSHOPT, TPAUSE, UMONITOR/UMWAIT, INCSSP, the FSGSBASE block. Our
        // CPUs ignore those prefixes here, so nothing in the group is comparable
        // once one is present.
        0xAE if saw_66 || saw_f3 => true,
        0xAE => !is_reg_form && matches!(reg, 4..=6),
        // Group 9 beyond CMPXCHG8B: the XSAVE-supervisor block, RDRAND, RDSEED
        // and the VMX pointer ops.
        0xC7 => matches!(reg, 3..=7),
        _ => false,
    }
}

/// The one x87 encoding where we and iced disagree about the NAME.
///
/// `D9 D8`-`D9 DF` is an undocumented second encoding of FSTP. We call it
/// `fstp`, because `gp64-cpu`'s `x87_decode.rs` — the decoder the shipped
/// interpreter actually executes on — calls it `Fstp`, and the disassembler
/// agreeing with the executor beside it matters more than agreeing with an
/// outside tool. iced calls it `fstpnce`. Both agree on what it does.
///
/// Every other undocumented alias (FFREEP, the FXCH/FCOM/FCOMP duplicates,
/// FNENI/FNDISI/FNSETPM) now decodes and is compared normally.
fn is_x87_naming_divergence(bytes: &[u8]) -> bool {
    let after = bytes
        .iter()
        .position(|b| gp64_disasm::legacy_prefix_group(*b).is_none())
        .unwrap_or(bytes.len());
    let (Some(&op), Some(&modrm)) = (bytes.get(after), bytes.get(after + 1)) else {
        return false;
    };
    op == 0xD9 && (0xD8..=0xDF).contains(&modrm)
}

fn check(bytes: &[u8], bitness: Bitness, stats: &mut Stats) {
    if is_tsx(bytes)
        || is_3dnow_prefetch_naming(bytes)
        || is_post_pentium4(bytes)
        || is_x87_naming_divergence(bytes)
    {
        return;
    }
    let Some(theirs) = iced_text(bytes, bitness) else {
        return;
    };
    // `xacquire`/`xrelease` are the TSX HLE hints — an F2/F3 prefix on a
    // lock-eligible memory operation. Same exclusion as XABORT/XBEGIN above: on
    // a Pentium 4 those prefixes are simply ignored bytes.
    if theirs.starts_with("xacquire ") || theirs.starts_with("xrelease ") {
        return;
    }
    match disassemble(bytes, bitness, RIP) {
        Ok(ours) => {
            stats.compared += 1;
            assert_eq!(
                ours.text,
                theirs,
                "bytes {:02X?} under {bitness:?}",
                &bytes[..bytes.len().min(12)]
            );
        }
        Err(_) => stats.not_ours += 1,
    }
}

fn assert_branch_text(bytes: &[u8], bitness: Bitness, rip: u64, expected: &str) {
    let ours = disassemble(bytes, bitness, rip).expect("gp64 should decode the branch");
    assert_eq!(
        ours.text, expected,
        "gp64 output for {bytes:02X?} at {rip:X}"
    );
}

fn assert_branch_text_matches_iced(bytes: &[u8], bitness: Bitness, rip: u64, expected: &str) {
    let iced = iced_text_at(bytes, bitness, rip).expect("iced should decode the branch");
    assert_eq!(
        iced, expected,
        "IntelFormatter output for {bytes:02X?} at {rip:X}"
    );
    assert_branch_text(bytes, bitness, rip, expected);
}

#[test]
fn branch_targets_keep_their_width_and_add_a_zero_before_hex_letters() {
    for (target, expected) in [
        (0xA123_u16, "0A123h"),
        (0xB123, "0B123h"),
        (0xC123, "0C123h"),
        (0xD123, "0D123h"),
        (0xE123, "0E123h"),
        (0xF123, "0F123h"),
    ] {
        assert_branch_text_matches_iced(
            &[0xEB, 0],
            Bitness::Bits16,
            u64::from(target.wrapping_sub(2)),
            &format!("jmp short {expected}"),
        );
        assert_branch_text_matches_iced(
            &[0xE9, 0, 0],
            Bitness::Bits16,
            u64::from(target.wrapping_sub(3)),
            &format!("jmp {expected}"),
        );
    }

    for (target, expected) in [
        (0xA000_0123_u32, "0A0000123h"),
        (0xB000_0123, "0B0000123h"),
        (0xC000_0123, "0C0000123h"),
        (0xD000_0123, "0D0000123h"),
        (0xE000_0123, "0E0000123h"),
        (0xF000_0123, "0F0000123h"),
    ] {
        assert_branch_text_matches_iced(
            &[0xEB, 0],
            Bitness::Bits32,
            u64::from(target.wrapping_sub(2)),
            &format!("jmp short {expected}"),
        );
        assert_branch_text_matches_iced(
            &[0xE9, 0, 0, 0, 0],
            Bitness::Bits32,
            u64::from(target.wrapping_sub(5)),
            &format!("jmp {expected}"),
        );
    }

    for (bytes, bitness, rip, expected) in [
        (&[0xEB, 0][..], Bitness::Bits16, 0x1233, "jmp short 1235h"),
        (&[0xE9, 0, 0], Bitness::Bits16, 0x1232, "jmp 1235h"),
        (
            &[0xEB, 0],
            Bitness::Bits32,
            0x0123_0000,
            "jmp short 01230002h",
        ),
        (
            &[0xE9, 0, 0, 0, 0],
            Bitness::Bits32,
            0x0123_0000,
            "jmp 01230005h",
        ),
    ] {
        assert_branch_text_matches_iced(bytes, bitness, rip, expected);
    }

    // iced prints a wrapped branch target of zero as bare `0`; this crate's
    // target spelling keeps the existing four/eight-digit code-width padding.
    for (bytes, bitness, rip, expected) in [
        (&[0xEB, 0][..], Bitness::Bits16, 0xFFFE, "jmp short 0000h"),
        (
            &[0xEB, 0],
            Bitness::Bits32,
            0xFFFF_FFFE,
            "jmp short 00000000h",
        ),
        (&[0xE9, 0, 0], Bitness::Bits16, 0xFFFD, "jmp 0000h"),
        (
            &[0xE9, 0, 0, 0, 0],
            Bitness::Bits32,
            0xFFFF_FFFB,
            "jmp 00000000h",
        ),
    ] {
        assert_branch_text(bytes, bitness, rip, expected);
    }

    for (bytes, bitness, rip, expected) in [
        (
            &[0xEB, 0xFC][..],
            Bitness::Bits16,
            0xABC8,
            "jmp short 0ABC6h",
        ),
        (
            &[0xEB, 0xFA],
            Bitness::Bits32,
            0xA000_1004,
            "jmp short 0A0001000h",
        ),
        (&[0xE9, 0xFC, 0xFF], Bitness::Bits16, 0xABC8, "jmp 0ABC7h"),
        (
            &[0xE9, 0xFA, 0xFF, 0xFF, 0xFF],
            Bitness::Bits32,
            0xA000_1004,
            "jmp 0A0001003h",
        ),
        (&[0xEB, 0xFC], Bitness::Bits16, 0, "jmp short 0FFFEh"),
        (&[0xE9, 0xFB, 0xFF], Bitness::Bits16, 0, "jmp 0FFFEh"),
        (&[0xEB, 0xFC], Bitness::Bits32, 0, "jmp short 0FFFFFFFEh"),
        (
            &[0xE9, 0xF9, 0xFF, 0xFF, 0xFF],
            Bitness::Bits32,
            0,
            "jmp 0FFFFFFFEh",
        ),
    ] {
        assert_branch_text_matches_iced(bytes, bitness, rip, expected);
    }
}

#[test]
fn one_byte_opcodes_disassemble_exactly_as_iced_prints_them() {
    let mut stats = Stats::default();
    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        for prefix in PREFIXES {
            for op in 0u16..=0xFF {
                let op = op as u8;
                // Prefixes and the two-byte escape are not instructions. The
                // x87 escapes are, but they are selected by the whole ModRM
                // byte rather than by the opcode, so they get their own sweep.
                if matches!(
                    op,
                    0x0F | 0x26 | 0x2E | 0x36 | 0x3E | 0x64..=0x67 | 0xF0 | 0xF2 | 0xF3
                ) || matches!(op, 0xD8..=0xDF)
                {
                    continue;
                }
                for modrm in 0u16..=0xFF {
                    let bytes = build(prefix, op, modrm as u8);
                    check(&bytes, bitness, &mut stats);
                }
            }
        }
    }
    println!(
        "compared {} instructions; {} not yet decoded by us",
        stats.compared, stats.not_ours
    );
    assert!(
        stats.compared > 400_000,
        "only {} comparisons — the sweep is not reaching the opcode space",
        stats.compared
    );
}

/// Memory addressing is where a formatter is most likely to be subtly wrong, so
/// it gets its own sweep at a fixed opcode with a real register operand.
#[test]
fn every_addressing_form_prints_exactly_as_iced_prints_it() {
    let mut stats = Stats::default();
    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        for prefix in PREFIXES {
            for modrm in 0u16..=0xFF {
                for sib in 0u16..=0xFF {
                    // `mov eax, r/m` — a register operand, so no size marker,
                    // leaving the addressing expression as the whole subject.
                    let mut v = Vec::new();
                    v.extend_from_slice(prefix);
                    v.push(0x8B);
                    v.push(modrm as u8);
                    v.push(sib as u8);
                    v.extend_from_slice(&TAIL);
                    check(&v, bitness, &mut stats);
                }
            }
        }
    }
    println!("addressing forms compared: {}", stats.compared);
    assert!(
        stats.compared > 200_000,
        "only {} addressing comparisons",
        stats.compared
    );
}

/// The two-byte map, on the same terms as the one-byte sweep.
///
/// Separate rather than folded in, because it reaches a different table and its
/// coverage floor should fail independently: a two-byte table that stopped
/// decoding must not be masked by a healthy one-byte count.
#[test]
fn two_byte_opcodes_disassemble_exactly_as_iced_prints_them() {
    let mut stats = Stats::default();
    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        for prefix in PREFIXES {
            for op2 in 0u16..=0xFF {
                for modrm in 0u16..=0xFF {
                    let mut v = Vec::with_capacity(32);
                    v.extend_from_slice(prefix);
                    v.push(0x0F);
                    v.push(op2 as u8);
                    v.push(modrm as u8);
                    v.extend_from_slice(&TAIL);
                    check(&v, bitness, &mut stats);
                }
            }
        }
    }
    println!(
        "two-byte: compared {}; {} not decoded by us",
        stats.compared, stats.not_ours
    );
    assert!(
        stats.compared > 595_000,
        "only {} two-byte comparisons — the sweep is not reaching the map",
        stats.compared
    );
}

#[test]
fn scalar_movsd_loads_and_stores_match_iced_under_f2() {
    let mut stats = Stats::default();
    for (bitness, operand_size_prefix) in [
        (Bitness::Bits16, &[][..]),
        (Bitness::Bits32, &[][..]),
        (Bitness::Bits32, &[0x66][..]),
    ] {
        for op2 in [0x10, 0x11] {
            for modrm in 0u16..=0xFF {
                let mut bytes = operand_size_prefix.to_vec();
                bytes.extend_from_slice(&[0xF2, 0x0F, op2, modrm as u8]);
                bytes.extend_from_slice(&TAIL);
                check(&bytes, bitness, &mut stats);
            }
        }
    }
    assert_eq!(stats.compared, 1536);
    assert_eq!(stats.not_ours, 0);
}

/// The x87 escapes, `D8`-`DF`.
///
/// Swept separately from the rest of the one-byte map because the instruction
/// is chosen by the ModRM byte, not the opcode: all 256 ModRM values are a
/// different instruction rather than a different operand, so this is 2,048
/// distinct encodings rather than eight.
#[test]
fn x87_escapes_disassemble_exactly_as_iced_prints_them() {
    let mut stats = Stats::default();
    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        for prefix in PREFIXES {
            for op in 0xD8u8..=0xDF {
                for modrm in 0u16..=0xFF {
                    let bytes = build(prefix, op, modrm as u8);
                    check(&bytes, bitness, &mut stats);
                }
            }
        }
    }
    println!(
        "x87: compared {}; {} not decoded by us",
        stats.compared, stats.not_ours
    );
    assert!(
        stats.compared > 21_000,
        "only {} x87 comparisons — the sweep is not reaching the escapes",
        stats.compared
    );
}

#[test]
fn emms_requires_its_bare_encoding_and_keeps_real_sse2_integer_forms() {
    let mut discrepancies = Vec::new();

    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        for prefix in [0x66, 0xF2, 0xF3] {
            // A prefix plus 0F 77 is a complete, unsupported form. Bytes after
            // it cannot turn the operandless EMMS slot into an XMM instruction.
            for suffix in [None, Some(0xC0), Some(0x00), Some(0xFF)] {
                let mut bytes = vec![prefix, 0x0F, 0x77];
                if let Some(suffix) = suffix {
                    bytes.push(suffix);
                }

                if !matches!(disassemble(&bytes, bitness, RIP), Err(DecodeError::Invalid)) {
                    discrepancies.push(format!(
                        "{bitness:?} {bytes:02X?}: expected Invalid, got {:?}",
                        disassemble(&bytes, bitness, RIP)
                    ));
                }
                if iced_text(&bytes, bitness).is_some() {
                    discrepancies.push(format!(
                        "iced accepted prefixed EMMS {bitness:?} {bytes:02X?}"
                    ));
                }
            }
        }

        for bytes in [&[0x0F, 0x77][..], &[0x0F, 0x77, 0xC0][..]] {
            match disassemble(bytes, bitness, RIP) {
                Ok(decoded) if decoded.text == "emms" && decoded.len == 2 => {}
                actual => discrepancies.push(format!(
                    "{bitness:?} {bytes:02X?}: expected operandless emms len 2, got {actual:?}"
                )),
            }
            if iced_text(bytes, bitness).as_deref() != Some("emms") {
                discrepancies.push(format!(
                    "iced disagreed on bare EMMS {bitness:?} {bytes:02X?}"
                ));
            }
        }

        for (op2, expected) in [(0x74, "pcmpeqb xmm0,xmm1"), (0xFD, "paddw xmm0,xmm1")] {
            let bytes = [0x66, 0x0F, op2, 0xC1];
            match disassemble(&bytes, bitness, RIP) {
                Ok(decoded) if decoded.text == expected && decoded.len == bytes.len() => {}
                actual => discrepancies.push(format!(
                    "{bitness:?} {bytes:02X?}: expected {expected} len {}, got {actual:?}",
                    bytes.len()
                )),
            }
            if iced_text(&bytes, bitness).as_deref() != Some(expected) {
                discrepancies.push(format!(
                    "iced disagreed on SSE2 {bitness:?} {bytes:02X?}: expected {expected}"
                ));
            }
        }
    }

    assert!(
        discrepancies.is_empty(),
        "EMMS/SSE2 decode mismatches:\n{}",
        discrepancies.join("\n")
    );
}
