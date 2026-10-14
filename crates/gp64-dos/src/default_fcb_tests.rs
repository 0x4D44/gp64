use super::*;

const SEG: u16 = 0x1000;

fn read_fcb(memory: &Memory, off: usize) -> [u8; 12] {
    let base = Memory::seg_offset_to_linear(SEG, 0);
    let mut out = [0u8; 12];
    for (i, b) in out.iter_mut().enumerate() {
        *b = memory.read_u8(base + off + i);
    }
    out
}

fn parse(args: &[&str]) -> (Memory, Dos) {
    let mut dos = Dos::new();
    let mut memory = Memory::new();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    dos.create_psp(&mut memory, SEG, 0x9000, 0x0500, None, &args);
    dos.parse_default_fcbs_from_args(&mut memory, SEG, &args);
    (memory, dos)
}

/// A synthetic 8.3 argument gives FCB1 an uppercase, space-padded name;
/// the absent second argument leaves FCB2 blank rather than zero-filled.
#[test]
fn first_argument_parses_into_fcb1_and_fcb2_stays_blank() {
    let (memory, _dos) = parse(&["sample.dat"]);

    let fcb1 = read_fcb(&memory, 0x5C);
    assert_eq!(fcb1[0], 0, "default drive");
    assert_eq!(&fcb1[1..12], b"SAMPLE  DAT");

    let fcb2 = read_fcb(&memory, 0x6C);
    assert_eq!(fcb2[0], 0);
    assert_eq!(&fcb2[1..12], b"           ", "blank, not zeros");
}

#[test]
fn drive_prefixes_and_wildcards_follow_ah29_semantics() {
    let (memory, _dos) = parse(&["a:file.txt", "B:*.PAS"]);

    let fcb1 = read_fcb(&memory, 0x5C);
    assert_eq!(fcb1[0], 1, "A: is drive 1 in FCB form");
    assert_eq!(&fcb1[1..12], b"FILE    TXT");

    let fcb2 = read_fcb(&memory, 0x6C);
    assert_eq!(fcb2[0], 2, "B: is drive 2");
    assert_eq!(&fcb2[1..12], b"????????PAS", "* expands to ? for the field");
}

/// The command interpreter parses even an empty tail: both FCBs read as
/// default drive + blank name, exactly as real DOS leaves them.
#[test]
fn no_arguments_gives_blank_fcbs() {
    let (memory, _dos) = parse(&[]);
    for off in [0x5C, 0x6C] {
        let fcb = read_fcb(&memory, off);
        assert_eq!(fcb[0], 0);
        assert_eq!(&fcb[1..12], b"           ");
    }
}

/// A switch argument (`/vgamode`) has no valid FCB name characters, and
/// over-long components truncate to the 8.3 fields.
#[test]
fn invalid_chars_terminate_and_long_names_truncate() {
    let (memory, _dos) = parse(&["/vgamode", "verylongname.text"]);

    let fcb1 = read_fcb(&memory, 0x5C);
    assert_eq!(fcb1[0], 0);
    assert_eq!(&fcb1[1..12], b"           ");

    let fcb2 = read_fcb(&memory, 0x6C);
    assert_eq!(&fcb2[1..12], b"VERYLONGTEX");
}
