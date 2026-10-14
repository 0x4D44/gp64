//! MDD-BUG-KIL-00779: AH=3Fh/40h move their data through the guest-semantic
//! span operations, one call per translated run, not one per byte.

use gp64_memory::Memory;
use gp64_types::span_write_census::width_bucket;

use super::{read_guest_span, write_guest_span};

fn bulk_and_per_byte(memory: &Memory, len: usize) -> (u64, u64) {
    let (bulk, per_byte) = memory.span_write_census();
    let bucket = width_bucket(len);
    (bulk[bucket], per_byte[bucket])
}

/// A read into plain RAM lands as one bulk span per run: the census counts
/// the spans, and the per-byte dispatch it replaced left no trace there at all.
#[test]
fn a_transfer_into_plain_ram_is_one_bulk_span_per_run() {
    let mut memory = Memory::new();
    let bytes: Vec<u8> = (1..=20).collect();
    // The span starts 4 bytes into the first 16-byte run, so 12 bytes land in
    // it and the remaining 8 in the second.
    let runs = [(0x2000, 16), (0x5000, 16)];
    let (bulk_12, per_byte_12) = bulk_and_per_byte(&memory, 12);
    let (bulk_8, per_byte_8) = bulk_and_per_byte(&memory, 8);

    write_guest_span(&mut memory, &runs, 4, &bytes);

    assert_eq!(
        bulk_and_per_byte(&memory, 12),
        (bulk_12 + 1, per_byte_12),
        "the first run is one bulk span"
    );
    assert_eq!(
        bulk_and_per_byte(&memory, 8),
        (bulk_8 + 1, per_byte_8),
        "and so is the second"
    );
    let landed: Vec<u8> = (0..12)
        .map(|i| memory.read_u8(0x2004 + i))
        .chain((0..8).map(|i| memory.read_u8(0x5000 + i)))
        .collect();
    assert_eq!(landed, bytes);
    assert_eq!(memory.read_u8(0x2003), 0, "nothing before the span offset");
    assert_eq!(memory.read_u8(0x5008), 0, "nothing past the transfer");
}

/// A run that reaches into the VGA window is not plain RAM, so it keeps the
/// per-byte guest path and lands exactly as a `write_u8` loop does.
#[test]
fn a_transfer_across_the_vga_window_matches_the_per_byte_path() {
    let bytes: Vec<u8> = (0x40..0x50).collect();
    let runs = [(0x9_FFF8, 16)];
    let mut spanned = Memory::new();
    write_guest_span(&mut spanned, &runs, 0, &bytes);
    let mut per_byte = Memory::new();
    for (i, byte) in bytes.iter().copied().enumerate() {
        per_byte.write_u8(0x9_FFF8 + i, byte);
    }
    let read = |memory: &Memory| {
        (0..16)
            .map(|i| memory.read_u8(0x9_FFF8 + i))
            .collect::<Vec<_>>()
    };
    assert_eq!(read(&spanned), read(&per_byte));
    assert_eq!(
        bulk_and_per_byte(&spanned, 16).1,
        1,
        "the span took the per-byte path"
    );
}

/// A write's host buffer is filled from each run as the guest would read it,
/// across the run boundary and the VGA window alike.
#[test]
fn a_write_reads_its_guest_runs_as_the_per_byte_path_does() {
    let mut memory = Memory::new();
    for i in 0..16 {
        memory.write_u8(0x3000 + i, 0x10 + i as u8);
        memory.write_u8(0x9_FFF8 + i, 0x80 + i as u8);
    }
    let runs = [(0x3000, 16), (0x9_FFF8, 16)];
    let mut out = [0u8; 20];
    read_guest_span(&memory, &runs, 6, &mut out);
    let expected: Vec<u8> = (6..16)
        .map(|i| memory.read_u8(0x3000 + i))
        .chain((0..10).map(|i| memory.read_u8(0x9_FFF8 + i)))
        .collect();
    assert_eq!(out.to_vec(), expected);
}
