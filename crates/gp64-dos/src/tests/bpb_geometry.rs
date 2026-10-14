use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Clone, Copy)]
struct Bpb {
    sector_bytes: u16,
    cluster_sectors: u8,
    reserved: u16,
    fats: u8,
    root_entries: u16,
    total: u16,
    fat_sectors: u16,
}

const VALID: Bpb = Bpb {
    sector_bytes: 1024,
    cluster_sectors: 4,
    reserved: 1,
    fats: 2,
    root_entries: 33,
    total: 4096,
    fat_sectors: 8,
};

fn call_bpb(bpb: Bpb, alias: bool) -> (gp64_cpu::Cpu, Memory, usize, Vec<u8>) {
    let mut dos = Dos::new();
    let mut cpu = gp64_cpu::Cpu::new();
    let mut memory = Memory::new();
    let (mut vga, mut keyboard) = create_test_io();
    cpu.regs.ds = 0x2400;
    cpu.regs.ds_cache.base = u32::from(cpu.regs.ds) << 4;
    cpu.set_es_real_mode(if alias { 0x2400 } else { 0x3500 });
    cpu.regs.set_si(0x100);
    cpu.regs.set_bp(0x100);
    cpu.regs.set_sp(0x800);
    let input = Memory::seg_offset_to_linear(cpu.regs.ds, 0x100);
    let output = Memory::seg_offset_to_linear(cpu.regs.es, 0x100);
    for offset in 0..30 {
        memory.write_u8(output - 1 + offset, 0xA5);
    }
    memory.write_u16(input, bpb.sector_bytes);
    memory.write_u8(input + 2, bpb.cluster_sectors);
    memory.write_u16(input + 3, bpb.reserved);
    memory.write_u8(input + 5, bpb.fats);
    memory.write_u16(input + 6, bpb.root_entries);
    memory.write_u16(input + 8, bpb.total);
    memory.write_u8(input + 10, 0xF8);
    memory.write_u16(input + 11, bpb.fat_sectors);
    let before = (0..30).map(|i| memory.read_u8(output - 1 + i)).collect();
    cpu.regs.set_ax(0x5300);
    let result = catch_unwind(AssertUnwindSafe(|| {
        dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut Pic::new(),
        )
    }));
    assert!(
        result.is_ok(),
        "GP64-BUG-FLU-00018: malformed BPB must not panic the host"
    );
    assert!(matches!(result.unwrap().unwrap(), Int21Disposition::Iret));
    assert_eq!(
        memory.read_u8(Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET)),
        0
    );
    (cpu, memory, output, before)
}

fn rejects(bpb: Bpb) {
    for alias in [false, true] {
        let (cpu, memory, output, before) = call_bpb(bpb, alias);
        assert!(
            cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "GP64-BUG-FLU-00018: malformed BPB must return carry"
        );
        assert_eq!(cpu.regs.ax(), 13, "malformed geometry returns invalid data");
        assert_eq!(
            (0..30)
                .map(|i| memory.read_u8(output - 1 + i))
                .collect::<Vec<_>>(),
            before,
            "rejected BPB must leave the entire DPB and guards untouched"
        );
    }
}

#[test]
fn bpb_geometry_zero_sector_bytes() {
    rejects(Bpb {
        sector_bytes: 0,
        ..VALID
    });
}
#[test]
fn bpb_geometry_zero_cluster_sectors() {
    rejects(Bpb {
        cluster_sectors: 0,
        ..VALID
    });
}
#[test]
fn bpb_geometry_non_power_of_two_cluster() {
    rejects(Bpb {
        cluster_sectors: 3,
        ..VALID
    });
}
#[test]
fn bpb_geometry_total_below_metadata() {
    rejects(Bpb { total: 18, ..VALID });
}
#[test]
fn bpb_geometry_no_data_sectors() {
    rejects(Bpb { total: 19, ..VALID });
}
#[test]
fn bpb_geometry_no_full_cluster() {
    rejects(Bpb { total: 22, ..VALID });
}
#[test]
fn bpb_geometry_cluster_number_overflow() {
    rejects(Bpb {
        cluster_sectors: 1,
        reserved: 0,
        fats: 0,
        root_entries: 0,
        total: u16::MAX,
        ..VALID
    });
}
#[test]
fn bpb_geometry_metadata_overflow() {
    rejects(Bpb {
        reserved: u16::MAX,
        fats: u8::MAX,
        fat_sectors: u16::MAX,
        ..VALID
    });
}

#[test]
fn bpb_geometry_valid_non512_and_aliased_input() {
    // Independent integer geometry: ceil(33*32/1024)=2, root=1+2*8=17,
    // data=19, clusters=floor((4096-19)/4)=1019, highest cluster=1020.
    for alias in [false, true] {
        let (cpu, memory, output, _) = call_bpb(VALID, alias);
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 0x5300);
        let expected = [
            0, 0, 0, 4, 3, 2, 1, 0, 2, 33, 0, 19, 0, 252, 3, 8, 8, 0, 17, 0, 0, 0, 0xF8, 0, 255,
            255, 255, 255,
        ];
        assert_eq!(
            (0..28)
                .map(|i| memory.read_u8(output + i))
                .collect::<Vec<_>>(),
            expected,
            "valid BPB must preserve exact DPB geometry and layout"
        );
        assert_eq!(memory.read_u8(output - 1), 0xA5);
        assert_eq!(memory.read_u8(output + 28), 0xA5);
    }
}
