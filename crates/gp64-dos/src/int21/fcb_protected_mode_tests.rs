//! MDD-BUG-KIL-00771: FCB services resolve every guest pointer through the
//! descriptor cache.
//!
//! Under a protected-mode guest, `DS`, `ES` and friends are selectors, so
//! `segment << 4` names an unrelated linear address. AH=24h read the FCB's
//! current block and record that way (writing a random record derived from low
//! memory into the real FCB), and AH=29h both read the filename from and wrote
//! the parsed FCB to the wrong place.
//!
//! Each fixture gives the selector a base that is deliberately NOT
//! `selector << 4`, so a real-mode computation lands somewhere the test can
//! see is wrong rather than somewhere that happens to work.

use super::*;
use crate::tests::create_test_io;
use gp64_cpu::SegmentCache;

/// A selector value, and the unrelated linear base its descriptor carries.
const SELECTOR: u16 = 0x0207;
const BASE: u32 = 0x0004_0000;
/// Where `segment << 4` would land instead.
const REAL_MODE_BASE: usize = (SELECTOR as usize) << 4;

struct Fixture {
    dos: Dos,
    cpu: gp64_cpu::Cpu,
    memory: Memory,
    vga: crate::Vga,
    keyboard: crate::Keyboard,
    pic: crate::Pic,
}

impl Fixture {
    /// A protected-mode guest whose DS and ES select one flat, non-real-mode
    /// base.
    fn protected_mode() -> Self {
        let (vga, keyboard) = create_test_io();
        let mut cpu = gp64_cpu::Cpu::new();
        cpu.regs.cr0 |= 1;
        let cache = SegmentCache {
            base: BASE,
            ..SegmentCache::real_mode(0)
        };
        cpu.regs.ds = SELECTOR;
        cpu.regs.es = SELECTOR;
        cpu.regs.ds_cache = cache;
        cpu.regs.es_cache = cache;
        assert!(
            !cpu.is_real_mode(),
            "the fixture must exercise the protected-mode path"
        );
        Fixture {
            dos: Dos::new(),
            cpu,
            memory: Memory::new(),
            vga,
            keyboard,
            pic: crate::Pic::new(),
        }
    }

    fn call(&mut self, ah: u8) {
        self.cpu.regs.set_ah(ah);
        self.dos
            .int21(
                &mut self.cpu,
                &mut self.memory,
                &mut self.vga,
                &mut self.keyboard,
                &mut self.pic,
            )
            .expect("int21 dispatch");
    }
}

/// AH=24h: set random record from current block and record.
#[test]
fn set_random_record_reads_the_fcb_through_its_selector() {
    let mut fixture = Fixture::protected_mode();
    let fcb = 0x0100u16;
    let fcb_linear = BASE as usize + fcb as usize;

    // Current block 3, current record 7 -> random record 3 * 128 + 7.
    fixture.memory.write_u16(fcb_linear + 0x0C, 3);
    fixture.memory.write_u8(fcb_linear + 0x20, 7);
    // Decoys at the real-mode address, which a `ds << 4` would read instead.
    fixture
        .memory
        .write_u16(REAL_MODE_BASE + fcb as usize + 0x0C, 9);
    fixture
        .memory
        .write_u8(REAL_MODE_BASE + fcb as usize + 0x20, 11);

    fixture.cpu.regs.set_dx(fcb);
    fixture.call(0x24);

    assert_eq!(
        fixture.memory.read_u32(fcb_linear + 0x21),
        3 * 128 + 7,
        "the random record must come from the FCB the selector names"
    );
}

/// AH=29h: parse a filename into an FCB.
#[test]
fn parse_filename_reads_and_writes_through_selectors() {
    let mut fixture = Fixture::protected_mode();
    let name = 0x0200u16;
    let fcb = 0x0300u16;
    let name_linear = BASE as usize + name as usize;
    let fcb_linear = BASE as usize + fcb as usize;

    for (index, byte) in b"REPORT.TXT\0".iter().enumerate() {
        fixture.memory.write_u8(name_linear + index, *byte);
    }
    // A decoy name at the real-mode address the old arithmetic would read.
    for (index, byte) in b"DECOY.BIN\0".iter().enumerate() {
        fixture
            .memory
            .write_u8(REAL_MODE_BASE + name as usize + index, *byte);
    }

    fixture.cpu.regs.set_si(name);
    fixture.cpu.regs.set_di(fcb);
    fixture.cpu.regs.set_al(0);
    fixture.call(0x29);

    let parsed: Vec<u8> = (1..12)
        .map(|offset| fixture.memory.read_u8(fcb_linear + offset))
        .collect();
    assert_eq!(
        parsed, b"REPORT  TXT",
        "the parsed FCB must be written through the ES selector, from the DS one"
    );
    assert_eq!(
        fixture.memory.read_u8(REAL_MODE_BASE + fcb as usize + 1),
        0,
        "nothing is written at the real-mode address"
    );
}
