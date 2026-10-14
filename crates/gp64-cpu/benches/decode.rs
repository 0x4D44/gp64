//! Microbenchmarks for iced-x86 instruction decoding.
//!
//! Measures single-instruction decode latency and stream throughput for
//! blocks of varying size.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use iced_x86::{Decoder, DecoderOptions, Instruction};

// ---------------------------------------------------------------------------
// Single instruction decode
// ---------------------------------------------------------------------------

fn bench_decode_single(c: &mut Criterion) {
    let mut g = c.benchmark_group("decode/single");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    // Representative instruction encodings covering different lengths/complexity
    let instructions: &[(&str, &[u8])] = &[
        ("nop", &[0x90]),                          // 1 byte
        ("mov_reg_reg", &[0x89, 0xD8]),            // 2 bytes
        ("add_imm8", &[0x83, 0xC0, 0x01]),         // 3 bytes: ADD AX, 1
        ("mov_reg_imm16", &[0xB8, 0x34, 0x12]),    // 3 bytes
        ("push_reg", &[0x50]),                     // 1 byte
        ("jmp_short", &[0xEB, 0x10]),              // 2 bytes
        ("call_near", &[0xE8, 0x00, 0x10]),        // 3 bytes
        ("rep_movsb", &[0xF3, 0xA4]),              // 2 bytes (prefix + opcode)
        ("seg_override_mov", &[0x26, 0x8B, 0x07]), // 3 bytes: ES: MOV AX, [BX]
    ];

    for &(name, bytes) in instructions {
        g.bench_with_input(BenchmarkId::from_parameter(name), &bytes, |b, &bytes| {
            b.iter(|| {
                let mut dec = Decoder::with_ip(16, black_box(bytes), 0, DecoderOptions::NONE);
                black_box(dec.decode())
            });
        });
    }

    g.finish();
}

// ---------------------------------------------------------------------------
// Stream decode (block throughput)
// ---------------------------------------------------------------------------

fn bench_decode_stream(c: &mut Criterion) {
    let mut g = c.benchmark_group("decode/stream");
    g.sample_size(200);
    g.warm_up_time(std::time::Duration::from_secs(2));
    g.measurement_time(std::time::Duration::from_secs(5));
    g.noise_threshold(0.03);

    // A realistic mixed block: MOV, ADD, CMP, JNE, PUSH, POP, NOP, XOR, INC
    let block_template: &[u8] = &[
        0x89, 0xD8, // MOV AX, BX
        0x01, 0xC8, // ADD AX, CX
        0x39, 0xD0, // CMP AX, DX
        0x75, 0x02, // JNE +2
        0x90, // NOP
        0x90, // NOP
        0x50, // PUSH AX
        0x58, // POP AX
        0x31, 0xDB, // XOR BX, BX
        0x40, // INC AX
    ]; // 14 bytes, ~10 instructions

    for &reps in &[1usize, 5, 20] {
        let block: Vec<u8> = block_template
            .iter()
            .copied()
            .cycle()
            .take(block_template.len() * reps)
            .collect();
        let block_len = block.len();

        g.throughput(Throughput::Bytes(block_len as u64));
        g.bench_with_input(
            BenchmarkId::new("mixed_block", format!("{reps}x")),
            &block,
            |b, block| {
                b.iter(|| {
                    let mut dec = Decoder::with_ip(16, black_box(block), 0, DecoderOptions::NONE);
                    let mut instr = Instruction::default();
                    let mut count = 0u32;
                    while dec.can_decode() {
                        dec.decode_out(&mut instr);
                        count += 1;
                    }
                    black_box(count)
                });
            },
        );
    }

    g.finish();
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

criterion_group!(benches, bench_decode_single, bench_decode_stream);
criterion_main!(benches);
