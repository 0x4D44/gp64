//! The x87 escape space, checked against the decoder that actually executes it.
//!
//! `iced` is an outside opinion. `gp64-cpu`'s `x87_decode.rs` is not: it is
//! the portable decoder the shipped interpreter runs on, so whatever it says an
//! encoding is, that is what the emulator will do with it. If this crate names
//! an encoding differently, a fault dump reports an instruction the guest did
//! not execute — which is the one failure a disassembler must not have, and one
//! no amount of agreement with a third-party tool would catch.
//!
//! The two are written independently: `x87_decode.rs` predates this crate and
//! was built for execution, with operand kinds describing what the FPU reads
//! rather than what Intel syntax prints. Only the instruction identity and the
//! encoded length are compared, because those are the only things both are
//! claiming.

use gp64_cpu::x87_decode::{decode_x87, CodeBitness, X87Operation};
use gp64_disasm::{disassemble, instruction_length, Bitness};

/// Every `X87Operation` variant lowercases exactly onto the mnemonic this crate
/// prints — `Ffreep` to `ffreep`, `F2xm1` to `f2xm1`. That is not a coincidence
/// worth relying on silently, so it is asserted below for every encoding rather
/// than assumed once.
fn executor_mnemonic(op: X87Operation) -> String {
    format!("{op:?}").to_lowercase()
}

#[test]
fn x87_matches_the_executing_decoder() {
    let mut compared = 0usize;
    let mut both_refused = 0usize;

    for (ours, theirs) in [
        (Bitness::Bits16, CodeBitness::Bits16),
        (Bitness::Bits32, CodeBitness::Bits32),
    ] {
        for opcode in 0xD8u8..=0xDF {
            for modrm in 0u16..=0xFF {
                // A displacement-bearing ModRM needs bytes to consume; 0x41 is
                // arbitrary and non-zero so a missing displacement shows up as
                // a length disagreement rather than reading as zero.
                let bytes = [opcode, modrm as u8, 0x41, 0x41, 0x41, 0x41, 0x41, 0x41];

                let executor = decode_x87(&bytes, theirs);
                let disassembler = disassemble(&bytes, ours, 0x1000);

                // The executor reports an unassigned encoding as a
                // successful decode of `Invalid` rather than as an error, so
                // that is the shape to compare against, not `Err`.
                let executor = match executor {
                    Ok(exec) if exec.operation != X87Operation::Invalid => Some(exec),
                    Ok(_) => None,
                    // Truncated or over-long input is a property of the buffer,
                    // not a verdict on the encoding.
                    Err(_) => continue,
                };

                match (executor, disassembler) {
                    (Some(exec), Ok(disasm)) => {
                        compared += 1;
                        assert_eq!(
                            disasm.mnemonic,
                            executor_mnemonic(exec.operation),
                            "{bytes:02X?} under {ours:?}: the dump would name an                              instruction the interpreter does not run"
                        );
                        assert_eq!(
                            disasm.len, exec.length as usize,
                            "{bytes:02X?} under {ours:?}: length disagrees, so the                              instruction AFTER this one would be misread too"
                        );
                    }
                    (None, Err(_)) => both_refused += 1,
                    (Some(exec), Err(e)) => panic!(
                        "{bytes:02X?} under {ours:?}: the interpreter runs this as                          {:?} and the disassembler refuses it ({e:?})",
                        exec.operation
                    ),
                    (None, Ok(disasm)) => panic!(
                        "{bytes:02X?} under {ours:?}: the disassembler names this                          {:?} and the interpreter treats it as invalid",
                        disasm.text
                    ),
                }
            }
        }
    }

    println!("x87 vs executor: {compared} agreed, {both_refused} refused by both");
    assert!(
        compared > 3_500,
        "only {compared} encodings compared — the sweep is not reaching the escapes"
    );
    // Both refusing everything would satisfy the equality checks vacuously.
    assert!(
        both_refused < compared,
        "{both_refused} refused against {compared} compared — this reads like both \
         decoders failing rather than agreeing"
    );
}

/// The length half, swept wider than the escape space: `instruction_length` is
/// what the interpreter uses to step over an instruction it does not itself
/// decode, so a disagreement there desynchronises the whole instruction stream
/// rather than mislabelling one line.
#[test]
fn x87_lengths_match_the_executing_decoder_under_prefixes() {
    let mut compared = 0usize;
    for (ours, theirs) in [
        (Bitness::Bits16, CodeBitness::Bits16),
        (Bitness::Bits32, CodeBitness::Bits32),
    ] {
        for prefix in [
            &[][..],
            &[0x66][..],
            &[0x67][..],
            &[0x2E][..],
            &[0x66, 0x67][..],
        ] {
            for opcode in 0xD8u8..=0xDF {
                for modrm in 0u16..=0xFF {
                    let mut bytes = prefix.to_vec();
                    bytes.push(opcode);
                    bytes.push(modrm as u8);
                    bytes.extend_from_slice(&[0x41; 8]);

                    let (Ok(exec), Ok(len)) =
                        (decode_x87(&bytes, theirs), instruction_length(&bytes, ours))
                    else {
                        continue;
                    };
                    if exec.operation == X87Operation::Invalid {
                        continue;
                    }
                    compared += 1;
                    assert_eq!(
                        len,
                        exec.length as usize,
                        "{:02X?} under {ours:?}: stepping would desynchronise",
                        &bytes[..bytes.len().min(8)]
                    );
                }
            }
        }
    }
    println!("x87 lengths vs executor: {compared} compared");
    assert!(
        compared > 15_000,
        "only {compared} length comparisons — the prefix sweep is not reaching them"
    );
}
