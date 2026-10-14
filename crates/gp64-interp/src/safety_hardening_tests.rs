use super::*;
use gp64_memory::Memory;

fn setup(code: &[u8]) -> (Cpu, Memory) {
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();
    cpu.regs.cs = 0x0000;
    cpu.regs.set_ip(0x0100);
    cpu.regs.eip = 0x0100;
    cpu.regs.set_sp(0x1000);
    cpu.regs.esp = 0x1000;
    cpu.regs.ss = 0x0000;
    for (i, &b) in code.iter().enumerate() {
        mem.ram[0x0100 + i] = b;
    }
    (cpu, mem)
}

// F7: Write to ROM area (0xF0000..0x100000) should be silently ignored
#[test]
fn test_rom_write_ignored() {
    let mut mem = Memory::new();

    // Place a known byte in the ROM area
    let rom_addr = 0xF0000;
    mem.ram[rom_addr] = 0xAA;

    let ram = mem.ram_ptr_mut();
    let ram_len = mem.usable_len();
    let _ = ram_len; // suppress unused warning in release

    // Attempt to write via vga_aware_write_u8 — should be silently ignored
    let returned_ram =
        unsafe { vga_aware_write_u8(ram, rom_addr, 0x55, &mut mem, WriteGuards::default()) };
    assert_eq!(mem.ram[rom_addr], 0xAA, "ROM write should be ignored (u8)");
    // Returned pointer should be the original (no realloc)
    assert_eq!(returned_ram, ram);

    // Also test u16 write to ROM
    mem.ram[rom_addr] = 0xBB;
    mem.ram[rom_addr + 1] = 0xCC;
    let ram = mem.ram_ptr_mut();
    let returned_ram =
        unsafe { vga_aware_write_u16(ram, rom_addr, 0x1234, &mut mem, WriteGuards::default()) };
    assert_eq!(
        mem.ram[rom_addr], 0xBB,
        "ROM write should be ignored (u16 lo)"
    );
    assert_eq!(
        mem.ram[rom_addr + 1],
        0xCC,
        "ROM write should be ignored (u16 hi)"
    );
    assert_eq!(returned_ram, ram);

    // Also test u32 write to ROM
    mem.ram[rom_addr] = 0xDD;
    let ram = mem.ram_ptr_mut();
    let returned_ram =
        unsafe { vga_aware_write_u32(ram, rom_addr, 0xDEADBEEF, &mut mem, WriteGuards::default()) };
    assert_eq!(mem.ram[rom_addr], 0xDD, "ROM write should be ignored (u32)");
    assert_eq!(returned_ram, ram);
}

// Shadow RAM accepts stores in the ROM window when the access policy enables
// writes. Exercise all store widths at a synthetic interior offset.
#[test]
fn test_rom_write_allowed_when_shadow_ram_enabled() {
    let mut mem = Memory::new();
    let rom_addr = 0xF_1234usize;
    mem.ram[rom_addr] = 0xAA;

    let allow_guards = {
        let mut g = WriteGuards::default();
        g.policy.allow_rom_writes = true;
        g
    };

    // u8 — single-byte store lands.
    let ram = mem.ram_ptr_mut();
    let _ = unsafe { vga_aware_write_u8(ram, rom_addr, 0x55, &mut mem, allow_guards) };
    assert_eq!(
        mem.ram[rom_addr], 0x55,
        "F-seg u8 write must land when allow_rom_writes is set"
    );

    // u16 — two-byte store lands.
    let ram = mem.ram_ptr_mut();
    let _ = unsafe { vga_aware_write_u16(ram, rom_addr, 0xBEEF, &mut mem, allow_guards) };
    assert_eq!(
        u16::from_le_bytes([mem.ram[rom_addr], mem.ram[rom_addr + 1]]),
        0xBEEF,
        "F-seg u16 write must land when allow_rom_writes is set"
    );

    // u32 — four-byte store lands.
    let ram = mem.ram_ptr_mut();
    let _ = unsafe { vga_aware_write_u32(ram, rom_addr, 0xDEAD_BEEF, &mut mem, allow_guards) };
    assert_eq!(
        u32::from_le_bytes([
            mem.ram[rom_addr],
            mem.ram[rom_addr + 1],
            mem.ram[rom_addr + 2],
            mem.ram[rom_addr + 3],
        ]),
        0xDEAD_BEEF,
        "F-seg u32 write must land when allow_rom_writes is set"
    );
}

// Regression guard: with the default guards (allow_rom_writes=false), the
// F7 behaviour must be unchanged. This is the counterpart to
// `test_rom_write_allowed_when_shadow_ram_enabled` and mirrors what
// `test_rom_write_ignored` covers at u8/u16/u32 granularity for the
// default case; keep both to guard against accidental flag inversions.
#[test]
fn test_rom_write_ignored_by_default() {
    let mut mem = Memory::new();
    let rom_addr = 0xF_4DF4usize;
    mem.ram[rom_addr] = 0xAA;

    let default_guards = WriteGuards::default();
    assert!(
        !default_guards.policy.allow_rom_writes,
        "default guards must keep F7 ROM-drop behaviour"
    );

    let ram = mem.ram_ptr_mut();
    let _ = unsafe { vga_aware_write_u32(ram, rom_addr, 0xDEAD_BEEF, &mut mem, default_guards) };
    assert_eq!(
        mem.ram[rom_addr], 0xAA,
        "F-seg u32 write MUST be silently dropped when allow_rom_writes=false"
    );
}

// ROM/HMA boundary straddle: write_u16 at 0xFFFFF PARTIALLY commits —
// the ROM byte is ignored, the HMA byte lands.
#[test]
fn test_rom_write_u16_boundary_straddle() {
    let mut mem = Memory::new();

    // Place known bytes at the ROM/HMA boundary
    let rom_byte = 0xFFFFF; // last byte of ROM area
    let hma_byte = 0x100000; // first byte of HMA
    mem.ram[rom_byte] = 0xAA;
    mem.ram[hma_byte] = 0xBB;

    let ram = mem.ram_ptr_mut();

    // write_u16 at 0xFFFFF straddles ROM (0xFFFFF) and HMA (0x100000).
    // The straddle is an unaligned access that the CPU splits into two
    // independently-decoded bus cycles, so only the ROM cycle is ignored.
    // This used to drop the whole word; `Memory::write_u16`'s own test
    // (`test_write_u16_partial_low_byte_only_at_rom_boundary`) already
    // pinned partial commit, so the interpreter was the outlier.
    let returned_ram =
        unsafe { vga_aware_write_u16(ram, rom_byte, 0x1234, &mut mem, WriteGuards::default()) };
    assert_eq!(mem.ram[rom_byte], 0xAA, "ROM byte must still be ignored");
    assert_eq!(
        mem.ram[hma_byte], 0x12,
        "the non-ROM (HMA) byte of the same access must commit"
    );
    assert_eq!(returned_ram, ram, "RAM pointer should not change");
}

// The u32 form of the same straddle. The Memory-layer twin is
// `rom_region_cross_boundary_write_u32`; using the same shape here keeps
// the interp/Memory agreement legible.
#[test]
fn test_rom_write_u32_boundary_straddle() {
    let mut mem = Memory::new();
    let rom_byte = 0xFFFFF; // last byte of the F-seg BIOS window
    mem.ram[rom_byte] = 0xAA;
    mem.ram[0x100000] = 0x00;
    mem.ram[0x100001] = 0x00;
    mem.ram[0x100002] = 0x00;

    let ram = mem.ram_ptr_mut();
    let _ = unsafe {
        vga_aware_write_u32(ram, rom_byte, 0x1122_3344, &mut mem, WriteGuards::default())
    };
    assert_eq!(mem.ram[rom_byte], 0xAA, "ROM byte must still be ignored");
    assert_eq!(mem.ram[0x100000], 0x33, "HMA byte 0 must commit");
    assert_eq!(mem.ram[0x100001], 0x22, "HMA byte 1 must commit");
    assert_eq!(mem.ram[0x100002], 0x11, "HMA byte 2 must commit");
}

// LOW edge of the fixed BIOS window. The guard checks the full span,
// rather than only the START address
// (`addr >= 0xF0000 && addr < 0x100000`) to a span-overlap check, so a
// wide store starting just BELOW 0xF0000 is now caught. Previously it
// fell through to the raw `write_u16_fast`/`write_u32_fast` path and
// poked a byte INTO the F-segment BIOS image. The Memory-layer twin is
// `test_write_u16_partial_low_byte_only_at_rom_boundary`.
// MDD-BUG-CRUCIBLE-00436 tracks the fixed-BIOS-window form specifically.
#[test]
fn test_rom_write_u16_boundary_straddle_low_edge() {
    let mut mem = Memory::new();
    let ram_byte = 0xEFFFF; // last byte below the BIOS window
    let rom_byte = 0xF0000; // first byte of the BIOS window
    mem.ram[ram_byte] = 0x00;
    mem.ram[rom_byte] = 0xAA;

    let ram = mem.ram_ptr_mut();
    let _ = unsafe { vga_aware_write_u16(ram, ram_byte, 0x1234, &mut mem, WriteGuards::default()) };
    assert_eq!(mem.ram[ram_byte], 0x34, "the byte below the window commits");
    assert_eq!(
        mem.ram[rom_byte], 0xAA,
        "the F-seg byte of the same access must NOT be written"
    );
}

#[test]
fn test_rom_write_u32_all_lower_boundary_straddles() {
    const FIRST_ROM_BYTE: usize = 0xF0000;
    const VALUE: u32 = 0x4433_2211;
    let value_bytes = VALUE.to_le_bytes();

    // A dword can cross the lower edge from exactly three starting
    // addresses. Each byte below the edge must commit, while every byte
    // decoded by the fixed BIOS window must retain its original value.
    for start in FIRST_ROM_BYTE - 3..FIRST_ROM_BYTE {
        let mut mem = Memory::new();
        let before = [0xA0, 0xB0, 0xC0, 0xD0];
        mem.ram[start..start + 4].copy_from_slice(&before);

        let ram = mem.ram_ptr_mut();
        let returned_ram =
            unsafe { vga_aware_write_u32(ram, start, VALUE, &mut mem, WriteGuards::default()) };

        assert_eq!(
            returned_ram, ram,
            "RAM pointer changed for start {start:#x}"
        );
        for index in 0..4 {
            let address = start + index;
            let expected = if address < FIRST_ROM_BYTE {
                value_bytes[index]
            } else {
                before[index]
            };
            assert_eq!(
                mem.ram[address], expected,
                "wrong byte at {address:#x} for dword start {start:#x}"
            );
        }
    }
}

// C5: Write to an address beyond the RAM ceiling (MAX_RAM_SIZE) via
// vga_aware_write — should not panic. usable_len() is only the DEFAULT
// committed size (< MAX_RAM_SIZE); writes below MAX_RAM_SIZE grow RAM on
// demand, so only addresses at/above the ceiling stay unmapped — the write
// is silently dropped and reads return bus float (0xFF).
#[test]
fn test_vga_aware_write_u8_above_ram_len() {
    let mut mem = Memory::new();
    let addr = gp64_memory::MAX_RAM_SIZE + 0x100; // beyond the RAM ceiling — cannot grow
    let ram = mem.ram_ptr_mut();
    // Should not panic — write is silently ignored for addresses beyond MAX_RAM_SIZE
    let _returned_ram =
        unsafe { vga_aware_write_u8(ram, addr, 0xAB, &mut mem, WriteGuards::default()) };
    // Read returns bus float (0xFF) since the address is unmapped
    assert_eq!(mem.read_u8(addr), 0xFF);
}

#[test]
fn test_vga_aware_write_u16_above_ram_len() {
    let mut mem = Memory::new();
    let addr = gp64_memory::MAX_RAM_SIZE + 0x100; // beyond the RAM ceiling — cannot grow
    let ram = mem.ram_ptr_mut();
    let _returned_ram =
        unsafe { vga_aware_write_u16(ram, addr, 0xBEEF, &mut mem, WriteGuards::default()) };
    assert_eq!(mem.read_u16(addr), 0xFFFF);
}

#[test]
fn test_vga_aware_write_u32_above_ram_len() {
    let mut mem = Memory::new();
    let addr = gp64_memory::MAX_RAM_SIZE + 0x100; // beyond the RAM ceiling — cannot grow
    let ram = mem.ram_ptr_mut();
    let _returned_ram =
        unsafe { vga_aware_write_u32(ram, addr, 0xDEAD_BEEF, &mut mem, WriteGuards::default()) };
    assert_eq!(mem.read_u32(addr), 0xFFFF_FFFF);
}

// C5: Read from address above usable RAM — should return 0xFF (bus float)
#[test]
fn test_vga_aware_read_u8_above_ram_len() {
    let mem = Memory::new();
    let addr = mem.usable_len() + 0x100;
    let ram = mem.ram.host_base().cast_const();
    let val = unsafe { vga_aware_read_u8(ram, addr, &mem) };
    assert_eq!(val, 0xFF, "out-of-range read should return 0xFF");
}

#[test]
fn option_rom_decoder_routes_interpreter_reads_and_blocks_writes() {
    const BASE: usize = 0xC0000;
    let rom = [0x55, 0xAA, 0x12, 0x34, 0x56, 0x78];
    let mut mem = Memory::new();
    mem.set_option_rom_backing(&rom);
    mem.configure_option_rom_decoder(BASE, true);
    let mut ram = mem.ram_ptr_mut();

    assert_eq!(unsafe { vga_aware_read_u8(ram, BASE, &mem) }, 0x55);
    assert_eq!(unsafe { vga_aware_read_u16(ram, BASE, &mem) }, 0xAA55);
    assert_eq!(
        unsafe { vga_aware_read_u32(ram, BASE + 2, &mem) },
        0x7856_3412
    );

    // MDD-BUG-CRUCIBLE-00438: the writes must be NON-ZERO. This test used to
    // write 0x00 / 0x0000 / 0 into RAM that `Memory::new()` had already
    // zero-filled, so a blocked write and a write that landed were
    // indistinguishable - deleting all three interpreter option-ROM write
    // guards left it green.
    ram = unsafe { vga_aware_write_u8(ram, BASE, 0xDE, &mut mem, WriteGuards::default()) };
    ram = unsafe { vga_aware_write_u16(ram, BASE, 0xBEEF, &mut mem, WriteGuards::default()) };
    let _ = unsafe {
        vga_aware_write_u32(ram, BASE + 2, 0xDEAD_C0DE, &mut mem, WriteGuards::default())
    };

    // With the decoder enabled, reads are served from the ROM backing.
    // This checks ROUTING only: `read_bytes` returns the immutable backing
    // on overlap, so it cannot see RAM and proves nothing about the guards.
    let mut observed = [0; 6];
    mem.read_bytes(BASE, &mut observed);
    assert_eq!(observed, rom, "an enabled decoder must serve ROM bytes");

    // Disable the decoder and look at the RAM underneath. THIS is the
    // guard oracle: if a blocked write had reached RAM, the poison above
    // would be sitting here instead of the zero fill.
    mem.configure_option_rom_decoder(BASE, false);
    let mut underneath = [0u8; 6];
    mem.read_bytes(BASE, &mut underneath);
    assert_eq!(
        underneath, [0u8; 6],
        "a write blocked by the option-ROM guards must not reach RAM;              found {underneath:02X?} under the decoder"
    );
    assert_eq!(
        unsafe { vga_aware_read_u16(mem.ram.host_base().cast_const(), BASE, &mem) },
        0,
        "the interpreter's own read path must agree the RAM is untouched"
    );
}

#[test]
fn test_vga_aware_read_u16_above_ram_len() {
    let mem = Memory::new();
    let addr = mem.usable_len() + 0x100;
    let ram = mem.ram.host_base().cast_const();
    let val = unsafe { vga_aware_read_u16(ram, addr, &mem) };
    assert_eq!(val, 0xFFFF, "out-of-range read should return 0xFFFF");
}

#[test]
fn test_vga_aware_read_u32_above_ram_len() {
    let mem = Memory::new();
    let addr = mem.usable_len() + 0x100;
    let ram = mem.ram.host_base().cast_const();
    let val = unsafe { vga_aware_read_u32(ram, addr, &mem) };
    assert_eq!(
        val, 0xFFFF_FFFF,
        "out-of-range read should return 0xFFFFFFFF"
    );
}

// F3: Normal push/pop with valid stack — debug_assert should not fire
#[test]
fn test_push_pop_normal_bounds() {
    // PUSH AX (0x50) then POP AX (0x58)
    let (mut cpu, mut mem) = setup(&[0x50, 0x58]);
    cpu.regs.eax = 0x1234;

    let mut io = ();
    let result = run(&mut cpu, &mut mem, &mut io, 2);
    assert!(
        matches!(result, RunResult::BudgetExhausted { executed: 2 }),
        "PUSH+POP should execute 2 instructions, got {:?}",
        result
    );
    // AX should round-trip through the stack
    assert_eq!(
        cpu.regs.eax & 0xFFFF,
        0x1234,
        "AX should be preserved after PUSH+POP"
    );
    // SP should return to original value
    assert_eq!(
        cpu.regs.sp(),
        0x1000,
        "SP should return to original after PUSH+POP"
    );
}

// M3: Push/pop with stack near/above usable RAM boundary — should not panic
#[test]
fn test_push_pop_near_ram_boundary() {
    use gp64_cpu::SegmentCache;
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();

    // Set up PM with SS base near usable RAM boundary
    cpu.regs.cr0 |= 1; // protected mode
    cpu.regs.ss_cache = SegmentCache {
        base: mem.usable_len() as u32 - 0x100, // base near end of RAM
        limit: 0xFFFF,
        cached_effective_limit: 0xFFFF,
        big: false,
        ..Default::default()
    };
    cpu.regs.set_sp(0x100); // SP at exactly the boundary
    cpu.regs.esp = 0x100;
    cpu.regs.eax = 0x4242;

    let ss_base = cpu.regs.ss_cache.base;
    let a20 = 0xFFFF_FFFFu32; // A20 enabled
    let ram = mem.ram_ptr_mut();
    // Push at boundary: ss_base + sp - 2 = usable_len() - 2 (just within)
    let big_stack = false; // 16-bit stack
    let _ram = push16(
        &mut cpu,
        ram,
        ss_base,
        0xBEEF,
        a20,
        big_stack,
        &mut mem,
        WriteGuards::default(),
    );
    assert_eq!(cpu.regs.sp(), 0x00FE);

    // Pop back — should read the value we just pushed
    let ram = mem.ram.host_base().cast_const();
    let val = pop16(&mut cpu, ram, ss_base, a20, big_stack, &mem);
    assert_eq!(val, 0xBEEF);
    assert_eq!(cpu.regs.sp(), 0x0100);
}

// M3: Push with stack address above usable RAM — should not panic
#[test]
fn test_push_above_ram_boundary_no_panic() {
    use gp64_cpu::SegmentCache;
    let mut cpu = Cpu::new();
    let mut mem = Memory::new();

    // SS base well above usable RAM
    cpu.regs.cr0 |= 1;
    cpu.regs.ss_cache = SegmentCache {
        base: mem.usable_len() as u32 + 0x1000,
        limit: 0xFFFF,
        cached_effective_limit: 0xFFFF,
        big: false,
        ..Default::default()
    };
    cpu.regs.set_sp(0x100);
    cpu.regs.esp = 0x100;

    let ss_base = cpu.regs.ss_cache.base;
    let a20 = 0xFFFF_FFFFu32; // A20 enabled
    let ram = mem.ram_ptr_mut();
    // Push above RAM — vga_aware_write routes through Memory slow path
    let big_stack = false; // 16-bit stack
    let _ram = push16(
        &mut cpu,
        ram,
        ss_base,
        0xCAFE,
        a20,
        big_stack,
        &mut mem,
        WriteGuards::default(),
    );
    // Should not panic; SP should be decremented
    assert_eq!(cpu.regs.sp(), 0x00FE);
}
