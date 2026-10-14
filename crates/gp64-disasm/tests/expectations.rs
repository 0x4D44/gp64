//! Hand-written expectations, pinned without consulting iced.
//!
//! The differential sweeps next door are the correctness oracle — they compare
//! hundreds of thousands of encodings against an independent decoder, which is
//! evidence no hand-written list can match. This file exists for the two things
//! they cannot do.
//!
//! First, they are only as trustworthy as the oracle: where iced and this crate
//! could agree on the same mistake, a sweep says nothing. These strings were
//! written from the encoding and the SDM.
//!
//! Second, iced is a dev-dependency that the product deliberately does not
//! link. If it is ever dropped from the workspace the sweeps go with it, and
//! this file is what still fails when someone breaks the decoder.

use gp64_disasm::{disassemble, instruction_length, Bitness, ControlFlow, DecodeError};

#[track_caller]
fn is(bytes: &[u8], bitness: Bitness, expected: &str) {
    let decoded = disassemble(bytes, bitness, 0x1000).expect("should decode");
    assert_eq!(decoded.text, expected, "bytes {bytes:02X?}");
    assert_eq!(
        decoded.len,
        bytes.len(),
        "length disagrees for {bytes:02X?} ({expected})"
    );
}

#[test]
fn system_and_descriptor_instructions() {
    // Group 6, register and memory forms. `D8` is mod=11 reg=3 rm=0.
    is(&[0x0F, 0x00, 0xD8], Bitness::Bits32, "ltr ax");
    is(&[0x0F, 0x00, 0x00], Bitness::Bits32, "sldt [eax]");
    is(&[0x0F, 0x00, 0x20], Bitness::Bits32, "verr [eax]");
    // Group 7. The descriptor-table loads are memory-only.
    is(&[0x0F, 0x01, 0x10], Bitness::Bits32, "lgdt [eax]");
    is(&[0x0F, 0x01, 0x38], Bitness::Bits32, "invlpg [eax]");
    is(&[0x0F, 0x01, 0xF0], Bitness::Bits32, "lmsw ax");
    // Neither takes a size marker: there is only one `lgdt`, so naming its
    // operand size would add nothing.
    is(&[0x0F, 0x06], Bitness::Bits32, "clts");
    is(&[0x0F, 0x09], Bitness::Bits32, "wbinvd");
    is(&[0x0F, 0x0B], Bitness::Bits32, "ud2");
    is(&[0x0F, 0x31], Bitness::Bits32, "rdtsc");
    is(&[0x0F, 0xA2], Bitness::Bits32, "cpuid");
}

#[test]
fn control_and_debug_register_moves_are_always_32_bit() {
    is(&[0x0F, 0x20, 0xC0], Bitness::Bits32, "mov eax,cr0");
    is(&[0x0F, 0x22, 0xD8], Bitness::Bits32, "mov cr3,eax");
    is(&[0x0F, 0x21, 0xC1], Bitness::Bits32, "mov ecx,dr0");
    // Even in 16-bit code, and even behind an operand-size prefix, the general
    // register is the full 32 bits — the prefix selects nothing here.
    is(&[0x0F, 0x20, 0xC0], Bitness::Bits16, "mov eax,cr0");
    is(&[0x66, 0x0F, 0x20, 0xC0], Bitness::Bits16, "mov eax,cr0");
    // The mod field is ignored, so a byte that would otherwise mean "memory
    // with a disp32" still encodes a plain register pair and stays 3 bytes.
    is(&[0x0F, 0x20, 0x05], Bitness::Bits32, "mov ebp,cr0");
}

#[test]
fn extends_carry_a_size_marker_and_their_neighbours_do_not() {
    // MOVZX names both sizes, so the memory operand needs saying: `ecx` does
    // not tell you only two bytes were read.
    is(
        &[0x0F, 0xB7, 0x08],
        Bitness::Bits32,
        "movzx ecx,word ptr [eax]",
    );
    is(
        &[0x0F, 0xBE, 0x00],
        Bitness::Bits32,
        "movsx eax,byte ptr [eax]",
    );
    is(&[0x0F, 0xB6, 0xC1], Bitness::Bits32, "movzx eax,cl");
    // BT reads exactly as wide as the register beside it, so no marker.
    is(&[0x0F, 0xA3, 0x18], Bitness::Bits32, "bt [eax],ebx");
    // ...but the immediate form has no register to say it, so it needs one.
    is(
        &[0x0F, 0xBA, 0x20, 0x41],
        Bitness::Bits32,
        "bt dword ptr [eax],41h",
    );
    is(&[0x0F, 0xBA, 0xE0, 0x41], Bitness::Bits32, "bt eax,41h");
    // LAR reads a word however wide its destination is, and is the only `lar`,
    // so it takes no marker either.
    is(&[0x0F, 0x02, 0x00], Bitness::Bits32, "lar eax,[eax]");
    is(&[0x0F, 0x02, 0xC0], Bitness::Bits32, "lar eax,eax");
}

#[test]
fn conditionals_shifts_and_the_opcode_register_forms() {
    is(&[0x0F, 0x45, 0xC3], Bitness::Bits32, "cmovne eax,ebx");
    is(&[0x0F, 0x94, 0xC0], Bitness::Bits32, "sete al");
    // SETcc has no other operand size, so its memory form takes no marker.
    is(&[0x0F, 0x94, 0x00], Bitness::Bits32, "sete [eax]");
    is(
        &[0x0F, 0xA4, 0xC1, 0x41],
        Bitness::Bits32,
        "shld ecx,eax,41h",
    );
    is(&[0x0F, 0xA5, 0xC1], Bitness::Bits32, "shld ecx,eax,cl");
    // BSWAP takes its register from the opcode and has no ModRM byte at all.
    is(&[0x0F, 0xC8], Bitness::Bits32, "bswap eax");
    is(&[0x0F, 0xCF], Bitness::Bits32, "bswap edi");
    is(&[0x0F, 0xB2, 0x00], Bitness::Bits32, "lss eax,[eax]");
    is(&[0x0F, 0xC1, 0x18], Bitness::Bits32, "xadd [eax],ebx");
}

#[test]
fn the_nop_block_distinguishes_the_documented_form_from_the_reserved_ones() {
    // `0F 1F /0` is the multi-byte NOP Intel documents, written with one
    // operand...
    is(&[0x0F, 0x1F, 0x00], Bitness::Bits32, "nop [eax]");
    // ...while every other encoding in the block is a reserved NOP, and names
    // both ModRM fields because that is what the bytes actually say.
    is(&[0x0F, 0x1F, 0x20], Bitness::Bits32, "nop [eax],esp");
    is(&[0x0F, 0x19, 0x20], Bitness::Bits32, "nop [eax],esp");
    // Group 16's memory forms are the prefetch hints; its register forms are
    // reserved NOPs, since there is nothing to prefetch in a register.
    is(&[0x0F, 0x18, 0x08], Bitness::Bits32, "prefetcht0 [eax]");
    is(&[0x0F, 0x18, 0xC8], Bitness::Bits32, "nop eax,ecx");
}

#[test]
fn group_15_splits_on_the_mod_field() {
    is(&[0x0F, 0xAE, 0x00], Bitness::Bits32, "fxsave [eax]");
    is(&[0x0F, 0xAE, 0x10], Bitness::Bits32, "ldmxcsr [eax]");
    is(&[0x0F, 0xAE, 0x38], Bitness::Bits32, "clflush [eax]");
    // The fences are register-form-only and name no operand at all.
    is(&[0x0F, 0xAE, 0xE8], Bitness::Bits32, "lfence");
    is(&[0x0F, 0xAE, 0xF0], Bitness::Bits32, "mfence");
    is(&[0x0F, 0xAE, 0xF8], Bitness::Bits32, "sfence");
    // CMPXCHG8B is likewise memory-only.
    is(&[0x0F, 0xC7, 0x08], Bitness::Bits32, "cmpxchg8b [eax]");
}

#[test]
fn encodings_that_are_not_instructions_on_our_cpus_are_refused() {
    for bytes in [
        // Group 6 `/6` and `/7` are reserved.
        &[0x0F, 0x00, 0x30][..],
        // The register form of `lgdt` is XGETBV, which arrived with XSAVE.
        &[0x0F, 0x01, 0xD0][..],
        // Group 15 `/0` has no register form.
        &[0x0F, 0xAE, 0xC0][..],
        // Group 9 is CMPXCHG8B and nothing else for us.
        &[0x0F, 0xC7, 0x00][..],
    ] {
        assert!(
            disassemble(bytes, Bitness::Bits32, 0x1000).is_err(),
            "{bytes:02X?} should not decode"
        );
    }
}

#[test]
fn fe_reserved_suboperations_are_refused_but_still_sized() {
    fn memory_form(bitness: Bitness, reg: u8) -> Vec<u8> {
        match bitness {
            Bitness::Bits16 => vec![0xFE, 0x46 | (reg << 3), 0x7F],
            Bitness::Bits32 => vec![0xFE, 0x84 | (reg << 3), 0x8D, 0x78, 0x56, 0x34, 0x12],
        }
    }

    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        for (reg, mnemonic) in [(0, "inc"), (1, "dec")] {
            let register = [0xFE, 0xC0 | (reg << 3)];
            assert_eq!(
                disassemble(&register, bitness, 0).unwrap().mnemonic,
                mnemonic,
                "bytes {register:02X?} under {bitness:?}"
            );

            let memory = memory_form(bitness, reg);
            assert_eq!(
                disassemble(&memory, bitness, 0).unwrap().mnemonic,
                mnemonic,
                "bytes {memory:02X?} under {bitness:?}"
            );
        }

        for reg in 2..=7 {
            let register = [0xFE, 0xC0 | (reg << 3)];
            let memory = memory_form(bitness, reg);
            let memory_len = if bitness == Bitness::Bits16 { 3 } else { 7 };

            for (bytes, expected_len) in [(&register[..], 2), (&memory[..], memory_len)] {
                assert!(
                    matches!(disassemble(bytes, bitness, 0), Err(DecodeError::Invalid)),
                    "bytes {bytes:02X?} under {bitness:?} should be rejected"
                );
                assert_eq!(
                    instruction_length(bytes, bitness),
                    Ok(expected_len),
                    "bytes {bytes:02X?} under {bitness:?} should still be sized"
                );
            }
        }
    }
}

#[test]
fn x87_names_the_implicit_top_of_stack_on_the_side_the_data_moves() {
    // A load reads memory into the stack, so `st` leads...
    is(&[0xD8, 0x00], Bitness::Bits32, "fadd st,dword ptr [eax]");
    is(&[0xDC, 0x00], Bitness::Bits32, "fadd st,qword ptr [eax]");
    is(&[0xD9, 0x00], Bitness::Bits32, "fld st,dword ptr [eax]");
    is(&[0xDB, 0x28], Bitness::Bits32, "fld st,tbyte ptr [eax]");
    is(&[0xDF, 0x28], Bitness::Bits32, "fild st,qword ptr [eax]");
    is(&[0xDE, 0x00], Bitness::Bits32, "fiadd st,word ptr [eax]");
    // ...and a store goes the other way.
    is(&[0xD9, 0x18], Bitness::Bits32, "fstp dword ptr [eax],st");
    is(&[0xDD, 0x18], Bitness::Bits32, "fstp qword ptr [eax],st");
    is(&[0xDF, 0x30], Bitness::Bits32, "fbstp tbyte ptr [eax],st");
    // The control word touches no stack register, and takes no size marker.
    is(&[0xD9, 0x28], Bitness::Bits32, "fldcw [eax]");
    is(&[0xD9, 0x38], Bitness::Bits32, "fnstcw [eax]");
    is(&[0xDD, 0x38], Bitness::Bits32, "fnstsw [eax]");
}

#[test]
fn the_dc_and_de_blocks_reverse_subtract_and_divide() {
    // This is the trap: the same low bits mean opposite things depending on
    // which escape they follow. `E0` is the plain form under D8 and the
    // reversed form under DC.
    is(&[0xD8, 0xE2], Bitness::Bits32, "fsub st,st(2)");
    is(&[0xDC, 0xE2], Bitness::Bits32, "fsubr st(2),st");
    is(&[0xD8, 0xEA], Bitness::Bits32, "fsubr st,st(2)");
    is(&[0xDC, 0xEA], Bitness::Bits32, "fsub st(2),st");
    is(&[0xD8, 0xF2], Bitness::Bits32, "fdiv st,st(2)");
    is(&[0xDC, 0xF2], Bitness::Bits32, "fdivr st(2),st");
    // The popping forms in DE reverse the same way.
    is(&[0xDE, 0xE2], Bitness::Bits32, "fsubrp st(2),st");
    is(&[0xDE, 0xEA], Bitness::Bits32, "fsubp st(2),st");
}

#[test]
fn x87_writes_the_canonical_form_when_the_slot_is_st1() {
    // Bare `FXCH` means `FXCH ST(1)`, so that encoding is written bare.
    is(&[0xD9, 0xC9], Bitness::Bits32, "fxch");
    is(&[0xDE, 0xC1], Bitness::Bits32, "faddp");
    is(&[0xD8, 0xD1], Bitness::Bits32, "fcom");
    is(&[0xDD, 0xE1], Bitness::Bits32, "fucom");
    // Any other slot is spelled out.
    is(&[0xD9, 0xCA], Bitness::Bits32, "fxch st,st(2)");
    is(&[0xDE, 0xC2], Bitness::Bits32, "faddp st(2),st");
    // FADD has no bare form, so even its st(1) encoding keeps both operands.
    is(&[0xD8, 0xC1], Bitness::Bits32, "fadd st,st(1)");
}

#[test]
fn x87_singletons_and_the_status_word() {
    is(&[0xD9, 0xD0], Bitness::Bits32, "fnop");
    is(&[0xD9, 0xE0], Bitness::Bits32, "fchs");
    is(&[0xD9, 0xE8], Bitness::Bits32, "fld1");
    is(&[0xD9, 0xEE], Bitness::Bits32, "fldz");
    is(&[0xD9, 0xFA], Bitness::Bits32, "fsqrt");
    is(&[0xDE, 0xD9], Bitness::Bits32, "fcompp");
    is(&[0xDA, 0xE9], Bitness::Bits32, "fucompp");
    // FNSTSW writes `ax`, never `eax` — the only general register in the block,
    // and its width does not follow the operand size.
    is(&[0xDF, 0xE0], Bitness::Bits32, "fnstsw ax");
    is(&[0xDF, 0xE0], Bitness::Bits16, "fnstsw ax");
}

#[test]
fn the_environment_block_is_the_one_x87_operand_a_prefix_resizes() {
    // At the mode's own default the bare form already says which size it is.
    is(&[0xD9, 0x20], Bitness::Bits32, "fldenv [eax]");
    is(&[0xD9, 0x20], Bitness::Bits16, "fldenv [bx+si]");
    // With the prefix present it is the keyword that records it — so these
    // never also print `data16`/`data32`.
    is(
        &[0x66, 0xD9, 0x20],
        Bitness::Bits16,
        "fldenv fpuenv28 ptr [bx+si]",
    );
    is(
        &[0x66, 0xD9, 0x20],
        Bitness::Bits32,
        "fldenv fpuenv14 ptr [eax]",
    );
    is(
        &[0x66, 0xDD, 0x20],
        Bitness::Bits16,
        "frstor fpustate108 ptr [bx+si]",
    );
    // Every other x87 operand takes its width from the opcode, so a prefix in
    // front of one changes nothing at all.
    is(
        &[0x66, 0xD8, 0x00],
        Bitness::Bits16,
        "fadd st,dword ptr [bx+si]",
    );
}

#[test]
fn undocumented_x87_aliases_are_named_not_refused() {
    // A disassembler says what the bytes are. Whether this CPU would execute
    // them is the `--cpu` identity's question, answered by a #UD — and refusing
    // to name an encoding does not prevent anything, it only loses the
    // information in the dump where it was wanted. We execute all of these:
    // `gp64-cpu`'s `x87_decode.rs` decodes them and `fpu.rs` runs them.
    is(&[0xDF, 0xC0], Bitness::Bits32, "ffreep st(0)");
    is(&[0xDF, 0xC3], Bitness::Bits32, "ffreep st(3)");
    // Second encodings of FSTP, FXCH, FCOM and FCOMP.
    is(&[0xD9, 0xDA], Bitness::Bits32, "fstp st(2),st");
    is(&[0xDD, 0xCA], Bitness::Bits32, "fxch st,st(2)");
    is(&[0xDC, 0xD2], Bitness::Bits32, "fcom st,st(2)");
    is(&[0xDE, 0xD2], Bitness::Bits32, "fcomp st,st(2)");
    is(&[0xDF, 0xCA], Bitness::Bits32, "fxch st,st(2)");
    is(&[0xDF, 0xD2], Bitness::Bits32, "fstp st(2),st");
    // The canonical-st(1) collapse applies to the aliases too.
    is(&[0xDD, 0xC9], Bitness::Bits32, "fxch");
    is(&[0xDC, 0xD1], Bitness::Bits32, "fcom");
    // The 8087/287 configuration no-ops, which the 387 retired and Intel
    // stopped documenting. Period code still contains them.
    is(&[0xDB, 0xE0], Bitness::Bits32, "fneni");
    is(&[0xDB, 0xE1], Bitness::Bits32, "fndisi");
    is(&[0xDB, 0xE4], Bitness::Bits32, "fnsetpm");
    // FNCLEX and FNINIT share that block and are documented.
    is(&[0xDB, 0xE2], Bitness::Bits32, "fnclex");
    is(&[0xDB, 0xE3], Bitness::Bits32, "fninit");
    // Genuinely unassigned encodings are still refused — naming those WOULD be
    // invention, which is the line this is drawn at.
    for bytes in [
        &[0xDB, 0xE5][..],
        &[0xDA, 0xE0][..],
        &[0xDE, 0xD8][..],
        &[0xDD, 0xF0][..],
        &[0xDF, 0xF8][..],
    ] {
        assert!(
            disassemble(bytes, Bitness::Bits32, 0x1000).is_err(),
            "{bytes:02X?} is unassigned and should not be named"
        );
    }
}

#[test]
fn mmx_decodes_across_all_three_generations_that_added_to_it() {
    // The original Pentium MMX set.
    is(&[0x0F, 0x6F, 0xC1], Bitness::Bits32, "movq mm0,mm1");
    is(&[0x0F, 0x6F, 0x08], Bitness::Bits32, "movq mm1,[eax]");
    is(&[0x0F, 0xFD, 0xC1], Bitness::Bits32, "paddw mm0,mm1");
    is(&[0x0F, 0xEF, 0xC9], Bitness::Bits32, "pxor mm1,mm1");
    is(&[0x0F, 0x77], Bitness::Bits32, "emms");
    // MOVD is the one MMX opcode whose memory operand is not 64 bits, and its
    // other half is a general register.
    is(&[0x0F, 0x6E, 0xC0], Bitness::Bits32, "movd mm0,eax");
    is(&[0x0F, 0x7E, 0xC8], Bitness::Bits32, "movd eax,mm1");
    // Neither takes a size marker: `mm0` and `eax` already say the width.
    is(&[0x0F, 0x6E, 0x00], Bitness::Bits32, "movd mm0,[eax]");
    // The shift groups have no memory form at all, so a memory ModRM is not an
    // instruction rather than a shift against memory.
    is(&[0x0F, 0x71, 0xD1, 0x03], Bitness::Bits32, "psrlw mm1,3");
    is(&[0x0F, 0x73, 0xF2, 0x20], Bitness::Bits32, "psllq mm2,20h");
    assert!(disassemble(&[0x0F, 0x71, 0x10, 0x03], Bitness::Bits32, 0).is_err());
    // The MMX extensions that came with SSE on the Pentium III...
    is(&[0x0F, 0xE0, 0xC1], Bitness::Bits32, "pavgb mm0,mm1");
    is(&[0x0F, 0xEE, 0xC1], Bitness::Bits32, "pmaxsw mm0,mm1");
    is(&[0x0F, 0xD7, 0xC1], Bitness::Bits32, "pmovmskb eax,mm1");
    is(
        &[0x0F, 0x70, 0xC1, 0x1B],
        Bitness::Bits32,
        "pshufw mm0,mm1,1Bh",
    );
    // ...and the three 64-bit forms SSE2 added on the Pentium 4.
    is(&[0x0F, 0xD4, 0xC1], Bitness::Bits32, "paddq mm0,mm1");
    is(&[0x0F, 0xF4, 0xC1], Bitness::Bits32, "pmuludq mm0,mm1");
}

#[test]
fn a_mandatory_prefix_makes_it_a_different_instruction_not_a_wider_one() {
    // One opcode, four instructions. None of these is another at a different
    // width — they move different amounts between different register files.
    is(&[0x0F, 0x6F, 0xC1], Bitness::Bits32, "movq mm0,mm1");
    is(
        &[0x66, 0x0F, 0x6F, 0xC1],
        Bitness::Bits32,
        "movdqa xmm0,xmm1",
    );
    is(
        &[0xF3, 0x0F, 0x6F, 0xC1],
        Bitness::Bits32,
        "movdqu xmm0,xmm1",
    );
    assert!(disassemble(&[0xF2, 0x0F, 0x6F, 0xC1], Bitness::Bits32, 0).is_err());
    // The arithmetic block is the same story across all four.
    is(&[0x0F, 0x58, 0xC1], Bitness::Bits32, "addps xmm0,xmm1");
    is(
        &[0x66, 0x0F, 0x58, 0xC1],
        Bitness::Bits32,
        "addpd xmm0,xmm1",
    );
    is(
        &[0xF3, 0x0F, 0x58, 0xC1],
        Bitness::Bits32,
        "addss xmm0,xmm1",
    );
    is(
        &[0xF2, 0x0F, 0x58, 0xC1],
        Bitness::Bits32,
        "addsd xmm0,xmm1",
    );
    // An opcode with no form under some prefix must REFUSE rather than fall
    // back to the bare one. `F2 0F 54` is not ANDPS with a stray prefix.
    is(&[0x0F, 0x54, 0xC1], Bitness::Bits32, "andps xmm0,xmm1");
    is(
        &[0x66, 0x0F, 0x54, 0xC1],
        Bitness::Bits32,
        "andpd xmm0,xmm1",
    );
    for prefix in [0xF3u8, 0xF2] {
        assert!(
            disassemble(&[prefix, 0x0F, 0x54, 0xC1], Bitness::Bits32, 0).is_err(),
            "there is no scalar ANDPS"
        );
    }
    // The prefix is not an operand-size override here, so it must not produce a
    // `data16`/`data32` marker on the bare form either.
    is(&[0x0F, 0xFD, 0xC1], Bitness::Bits16, "paddw mm0,mm1");
    is(
        &[0x66, 0x0F, 0xFD, 0xC1],
        Bitness::Bits16,
        "paddw xmm0,xmm1",
    );
}

#[test]
fn scalar_sse2_movsd_is_not_a_word_sized_string_instruction() {
    for (bytes, bitness, expected) in [
        (
            &[0xF2, 0x0F, 0x10, 0xC1][..],
            Bitness::Bits16,
            "movsd xmm0,xmm1",
        ),
        (
            &[0xF2, 0x0F, 0x11, 0xC1][..],
            Bitness::Bits16,
            "movsd xmm1,xmm0",
        ),
        (
            &[0xF2, 0x0F, 0x10, 0x00][..],
            Bitness::Bits16,
            "movsd xmm0,[bx+si]",
        ),
        (
            &[0xF2, 0x0F, 0x11, 0x00][..],
            Bitness::Bits16,
            "movsd [bx+si],xmm0",
        ),
        (
            &[0x66, 0xF2, 0x0F, 0x10, 0xC1][..],
            Bitness::Bits32,
            "movsd xmm0,xmm1",
        ),
        (
            &[0x66, 0xF2, 0x0F, 0x11, 0xC1][..],
            Bitness::Bits32,
            "movsd xmm1,xmm0",
        ),
        (
            &[0x66, 0xF2, 0x0F, 0x10, 0x00][..],
            Bitness::Bits32,
            "movsd xmm0,[eax]",
        ),
        (
            &[0x66, 0xF2, 0x0F, 0x11, 0x00][..],
            Bitness::Bits32,
            "movsd [eax],xmm0",
        ),
    ] {
        let decoded = disassemble(bytes, bitness, 0x1000).expect("should decode");
        assert_eq!(decoded.mnemonic, "movsd", "bytes {bytes:02X?}");
        assert_eq!(decoded.text, expected, "bytes {bytes:02X?}");
    }

    is(&[0xA5], Bitness::Bits16, "movsw [di],[si]");
    is(&[0xA5], Bitness::Bits32, "movsd [edi],[esi]");
    is(&[0x66, 0xA5], Bitness::Bits32, "movsw [edi],[esi]");
}

#[test]
fn sse_splits_on_the_mod_field_into_different_instructions() {
    // Not a memory form and a register form of one instruction — two different
    // instructions moving different halves.
    is(&[0x0F, 0x12, 0xC1], Bitness::Bits32, "movhlps xmm0,xmm1");
    is(&[0x0F, 0x12, 0x08], Bitness::Bits32, "movlps xmm1,[eax]");
    is(&[0x0F, 0x16, 0xC1], Bitness::Bits32, "movlhps xmm0,xmm1");
    is(&[0x0F, 0x16, 0x08], Bitness::Bits32, "movhps xmm1,[eax]");
    // The stores have no register form at all.
    is(&[0x0F, 0x13, 0x08], Bitness::Bits32, "movlps [eax],xmm1");
    assert!(disassemble(&[0x0F, 0x13, 0xC1], Bitness::Bits32, 0).is_err());
    // ...and MOVMSKPS is the mirror: register-only.
    is(&[0x0F, 0x50, 0xC1], Bitness::Bits32, "movmskps eax,xmm1");
    assert!(disassemble(&[0x0F, 0x50, 0x08], Bitness::Bits32, 0).is_err());
}

#[test]
fn cmpps_spells_its_predicate_into_the_mnemonic() {
    // Intel syntax folds the first eight immediates into the name and stops
    // printing them as an operand.
    is(
        &[0x0F, 0xC2, 0xC1, 0x00],
        Bitness::Bits32,
        "cmpeqps xmm0,xmm1",
    );
    is(
        &[0x0F, 0xC2, 0xC1, 0x03],
        Bitness::Bits32,
        "cmpunordps xmm0,xmm1",
    );
    is(
        &[0x66, 0x0F, 0xC2, 0xC1, 0x01],
        Bitness::Bits32,
        "cmpltpd xmm0,xmm1",
    );
    is(
        &[0xF3, 0x0F, 0xC2, 0xC1, 0x02],
        Bitness::Bits32,
        "cmpless xmm0,xmm1",
    );
    is(
        &[0xF2, 0x0F, 0xC2, 0xC1, 0x07],
        Bitness::Bits32,
        "cmpordsd xmm0,xmm1",
    );
    // From eight up there is no spelling, so the immediate stays an operand.
    is(
        &[0x0F, 0xC2, 0xC1, 0x08],
        Bitness::Bits32,
        "cmpps xmm0,xmm1,8",
    );
    // The instruction is four bytes either way — folding a byte into the name
    // must not lose it from the length.
    assert_eq!(
        disassemble(&[0x0F, 0xC2, 0xC1, 0x00], Bitness::Bits32, 0)
            .unwrap()
            .len,
        4
    );
}

#[test]
fn the_sse_conversions_cross_register_files() {
    // Packed integer conversions use MMX; scalar ones use a general register.
    is(&[0x0F, 0x2A, 0xC1], Bitness::Bits32, "cvtpi2ps xmm0,mm1");
    is(
        &[0xF3, 0x0F, 0x2A, 0xC1],
        Bitness::Bits32,
        "cvtsi2ss xmm0,ecx",
    );
    is(&[0x0F, 0x2C, 0xC1], Bitness::Bits32, "cvttps2pi mm0,xmm1");
    is(
        &[0xF3, 0x0F, 0x2C, 0xC1],
        Bitness::Bits32,
        "cvttss2si eax,xmm1",
    );
    is(
        &[0xF2, 0x0F, 0x5A, 0xC1],
        Bitness::Bits32,
        "cvtsd2ss xmm0,xmm1",
    );
    // CVTSI2SS and CVTSI2SD are the only two here that carry a size marker:
    // their source is an integer and their destination an XMM register, so the
    // register does not say how much memory is read.
    is(
        &[0xF3, 0x0F, 0x2A, 0x08],
        Bitness::Bits32,
        "cvtsi2ss xmm1,dword ptr [eax]",
    );
    is(&[0x0F, 0x2C, 0x08], Bitness::Bits32, "cvttps2pi mm1,[eax]");
    // MOVD moves between an MMX or XMM register and a general one, and both
    // widths are fixed, so neither takes a marker.
    is(&[0x66, 0x0F, 0x6E, 0xC1], Bitness::Bits32, "movd xmm0,ecx");
    is(&[0x66, 0x0F, 0x7E, 0xC1], Bitness::Bits32, "movd ecx,xmm0");
    // `F3 0F 7E` is the one place an F3 names a move rather than a scalar op.
    is(&[0xF3, 0x0F, 0x7E, 0xC1], Bitness::Bits32, "movq xmm0,xmm1");
    is(
        &[0xF2, 0x0F, 0xD6, 0xC1],
        Bitness::Bits32,
        "movdq2q mm0,xmm1",
    );
    is(
        &[0xF3, 0x0F, 0xD6, 0xC1],
        Bitness::Bits32,
        "movq2dq xmm0,mm1",
    );
    // `0F E6` is three different conversions and nothing at all bare.
    is(
        &[0x66, 0x0F, 0xE6, 0xC1],
        Bitness::Bits32,
        "cvttpd2dq xmm0,xmm1",
    );
    is(
        &[0xF3, 0x0F, 0xE6, 0xC1],
        Bitness::Bits32,
        "cvtdq2pd xmm0,xmm1",
    );
    is(
        &[0xF2, 0x0F, 0xE6, 0xC1],
        Bitness::Bits32,
        "cvtpd2dq xmm0,xmm1",
    );
    assert!(disassemble(&[0x0F, 0xE6, 0xC1], Bitness::Bits32, 0).is_err());
    // MOVNTI stores a general register and is memory-only.
    is(&[0x0F, 0xC3, 0x08], Bitness::Bits32, "movnti [eax],ecx");
    assert!(disassemble(&[0x0F, 0xC3, 0xC1], Bitness::Bits32, 0).is_err());
}

#[test]
fn sse2_shifts_add_two_forms_mmx_cannot_have() {
    // The same three opcodes as MMX, with the same operations...
    is(
        &[0x66, 0x0F, 0x71, 0xD1, 0x03],
        Bitness::Bits32,
        "psrlw xmm1,3",
    );
    is(&[0x0F, 0x71, 0xD1, 0x03], Bitness::Bits32, "psrlw mm1,3");
    // ...plus two whole-register byte shifts, which have no MMX form because
    // there is no 64-bit register to shift across.
    is(
        &[0x66, 0x0F, 0x73, 0xD9, 0x04],
        Bitness::Bits32,
        "psrldq xmm1,4",
    );
    is(
        &[0x66, 0x0F, 0x73, 0xF9, 0x04],
        Bitness::Bits32,
        "pslldq xmm1,4",
    );
    assert!(disassemble(&[0x0F, 0x73, 0xD9, 0x04], Bitness::Bits32, 0).is_err());
}

#[test]
fn three_d_now_is_named_because_a_period_guest_can_contain_it() {
    // squarely inside the era the emulator targets. Whether the selected --cpu
    // would execute it is a separate question, answered by a #UD.
    is(&[0x0F, 0x0E], Bitness::Bits32, "femms");
    is(&[0x0F, 0x0D, 0x00], Bitness::Bits32, "prefetch [eax]");
    is(&[0x0F, 0x0D, 0x08], Bitness::Bits32, "prefetchw [eax]");
    // `0F 0F` is unique in the whole map: the operation is named by a byte
    // AFTER the ModRM and its displacement.
    is(&[0x0F, 0x0F, 0xC1, 0xB4], Bitness::Bits32, "pfmul mm0,mm1");
    is(&[0x0F, 0x0F, 0xC1, 0x9E], Bitness::Bits32, "pfadd mm0,mm1");
    is(
        &[0x0F, 0x0F, 0xC1, 0xBF],
        Bitness::Bits32,
        "pavgusb mm0,mm1",
    );
    // The Athlon's 3DNow! extensions.
    is(&[0x0F, 0x0F, 0xC1, 0xBB], Bitness::Bits32, "pswapd mm0,mm1");
    is(&[0x0F, 0x0F, 0xC1, 0x8A], Bitness::Bits32, "pfnacc mm0,mm1");
    // The suffix sits past the displacement, so a memory form has to walk it
    // before the instruction is even known.
    is(
        &[0x0F, 0x0F, 0x80, 0x78, 0x56, 0x34, 0x12, 0xB4],
        Bitness::Bits32,
        "pfmul mm0,[eax+12345678h]",
    );
    // An unassigned suffix is not an instruction.
    assert!(disassemble(&[0x0F, 0x0F, 0xC1, 0x00], Bitness::Bits32, 0).is_err());
}

#[test]
fn the_deliberately_undefined_encodings_are_named() {
    // UD2 is the documented one, but all three of these are guaranteed to
    // fault, and a dump of the program that just executed one is exactly where
    // saying so beats printing raw bytes.
    is(&[0x0F, 0x0B], Bitness::Bits32, "ud2");
    is(&[0x0F, 0xB9, 0xC1], Bitness::Bits32, "ud1 eax,ecx");
    is(&[0x0F, 0xFF, 0xC1], Bitness::Bits32, "ud0 eax,ecx");
}

#[test]
fn control_flow_classifies_what_nests_and_what_does_not() {
    #[track_caller]
    fn flow(bytes: &[u8], bitness: Bitness) -> ControlFlow {
        disassemble(bytes, bitness, 0x1000)
            .expect("should decode")
            .flow
    }

    // Calls and returns nest, in both their near and far forms.
    assert_eq!(
        flow(&[0xE8, 0, 0, 0, 0], Bitness::Bits32),
        ControlFlow::Call
    );
    assert_eq!(
        flow(&[0x9A, 0, 0, 0, 0, 0, 0], Bitness::Bits32),
        ControlFlow::Call
    );
    assert_eq!(flow(&[0xFF, 0xD0], Bitness::Bits32), ControlFlow::Call);
    assert_eq!(flow(&[0xC3], Bitness::Bits32), ControlFlow::Return);
    assert_eq!(flow(&[0xCB], Bitness::Bits32), ControlFlow::Return);
    assert_eq!(
        flow(&[0xCA, 0x04, 0x00], Bitness::Bits32),
        ControlFlow::Return
    );
    assert_eq!(flow(&[0xFF, 0x18], Bitness::Bits32), ControlFlow::Call);
    assert_eq!(
        flow(&[0xC2, 0x04, 0x00], Bitness::Bits32),
        ControlFlow::Return
    );

    // Interrupts nest separately, because they are unwound by IRET.
    assert_eq!(flow(&[0xCD, 0x21], Bitness::Bits32), ControlFlow::Interrupt);
    assert_eq!(flow(&[0xCC], Bitness::Bits32), ControlFlow::Interrupt);
    assert_eq!(flow(&[0xCE], Bitness::Bits16), ControlFlow::Interrupt);
    assert_eq!(flow(&[0xF1], Bitness::Bits32), ControlFlow::Interrupt);
    // IRET and IRETD are the same instruction at two operand sizes, and the
    // classification must not depend on which spelling the size produced.
    assert_eq!(flow(&[0xCF], Bitness::Bits32), ControlFlow::InterruptReturn);
    assert_eq!(flow(&[0xCF], Bitness::Bits16), ControlFlow::InterruptReturn);

    // A jump does not nest, so nothing tracking depth should see it.
    assert_eq!(flow(&[0xEB, 0x00], Bitness::Bits32), ControlFlow::Other);
    assert_eq!(
        flow(&[0xE9, 0, 0, 0, 0], Bitness::Bits32),
        ControlFlow::Other
    );
    assert_eq!(flow(&[0x74, 0x00], Bitness::Bits32), ControlFlow::Other);
    assert_eq!(flow(&[0x90], Bitness::Bits32), ControlFlow::Other);
}

/// The classification is derived from the mnemonic, so a mnemonic spelled in a
/// way the classifier does not recognise would silently fall through to
/// `Other` — a call that stops counting rather than an error. This sweeps the
/// whole one-byte space and fails if any instruction whose printed text names a
/// call, return or interrupt was left unclassified.
#[test]
fn no_control_flow_instruction_is_left_unclassified() {
    let mut checked = 0usize;
    for bitness in [Bitness::Bits16, Bitness::Bits32] {
        for op in 0u16..=0xFF {
            for modrm in 0u16..=0xFF {
                let bytes = [op as u8, modrm as u8, 0, 0, 0, 0, 0, 0, 0, 0];
                let Ok(d) = disassemble(&bytes, bitness, 0x1000) else {
                    continue;
                };
                // Step past any prefix words to reach the mnemonic itself.
                let word = d
                    .text
                    .split(' ')
                    .find(|w| {
                        !matches!(
                            *w,
                            "lock"
                                | "rep"
                                | "repe"
                                | "repne"
                                | "data16"
                                | "data32"
                                | "addr16"
                                | "addr32"
                                | "hint-taken"
                                | "hint-not-taken"
                        )
                    })
                    .unwrap_or("");
                let names_control_flow = matches!(
                    word,
                    "call" | "ret" | "int" | "int1" | "int3" | "into" | "iret" | "iretd"
                );
                if names_control_flow {
                    checked += 1;
                    assert_ne!(
                        d.flow,
                        ControlFlow::Other,
                        "{:02X?} prints {:?} but classifies as Other",
                        &bytes[..d.len],
                        d.text
                    );
                }
                // ...and the converse: nothing else should claim to nest.
                if !names_control_flow {
                    assert_eq!(
                        d.flow,
                        ControlFlow::Other,
                        "{:02X?} prints {:?} but claims to affect call depth",
                        &bytes[..d.len],
                        d.text
                    );
                }
            }
        }
    }
    assert!(
        checked > 2_000,
        "only {checked} control-flow instructions seen — the sweep is not reaching them"
    );
}
