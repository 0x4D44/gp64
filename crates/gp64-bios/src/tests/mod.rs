mod bios_memory_and_startup;
mod coverage_bios_services;
mod coverage_video_and_disk;
mod int10_palette;
mod int10_text_and_pages;
mod memory_map_and_video;
mod miscellaneous_services;
mod serial_and_disk;
mod system_and_keyboard;
mod tracing_and_rom;

use super::*;
use gp64_cpu::Cpu;
use gp64_hw::bda;
use gp64_hw::cmos::Cmos;
use gp64_hw::keyboard::Keyboard;
use gp64_hw::vga::Vga;
use gp64_memory::Memory;
use gp64_types::bda::{BDA_MIDNIGHT_FLAG_ADDR, BDA_TICK_COUNT_ADDR};
use std::time::Duration;

// ------------------------------------------------------------------
// MDD-BUG-CRUCIBLE-00603 — AH=04h verify and AH=05h format must agree
// ------------------------------------------------------------------

// -------------------------------------------------------------
// Gremlin Step 2: tape coverage-gap poisoning (HLD §"RTC/port/
// boot-environment determinism" — "It never reads host hardware
// in tape mode.")
//
// When a tape is active but does not have an entry for the
// requested source at the requested PIT tick, the INT 1Ah
// handler MUST substitute poisoned values rather than fall
// through to the host-derived CMOS / BDA values.
// -------------------------------------------------------------

// Additional BIOS tests for coverage

fn text_cell_addr(row: usize, col: usize) -> usize {
    0xB8000 + ((row * 80 + col) * 2)
}

fn read_text_cell(memory: &Memory, row: usize, col: usize) -> (u8, u8) {
    let addr = text_cell_addr(row, col);
    (memory.read_u8(addr), memory.read_u8(addr + 1))
}

fn set_text_mode_03(bios: &mut Bios, cpu: &mut Cpu, memory: &mut Memory, vga: &mut Vga) {
    memory.write_u8(0x44B, 0xAA);
    cpu.regs.set_ax(0x0003);
    bios.int10(cpu, memory, vga).unwrap();
    assert_eq!(memory.read_u16(0x44A), 80, "mode 03 columns word in BDA");
    assert_eq!(memory.read_u8(0x484), 24, "mode 03 rows-minus-one in BDA");
}

// ========================================================================
// days_to_ymd helper function tests
// ========================================================================

// ========================================================================
// BIOS ROM system info tests (BIOS date, system configuration)
// ========================================================================

// =========================================================================
// Keyboard Buffer Edge Case Tests
// =========================================================================

// ============================================================================
// Integration Tests - Multi-Interrupt Scenarios
// ============================================================================
// These tests verify that multiple BIOS interrupts work correctly together,
// testing state consistency across interrupt calls.

// ========== INVARIANT TESTS ==========
// These tests verify state consistency across BIOS subsystems

// ===== INT 14h (serial) tests =====

/// Drive one INT 14h call to completion the way the dispatcher does.
///
/// MDD-BUG-KILN-00278 chunked the timeout waits, so a wait that runs to its
/// BDA limit now spans many dispatches: the handler yields with
/// [`Int14Disp::Retry`] and the dispatcher rewinds CS:IP onto the `INT 14h`
/// so the guest re-enters. Tests that assert a TIMEOUT result must model
/// that re-entry. Returns the number of dispatches the wait took, which is
/// the observable the yield test asserts on.
fn int14_to_completion(
    bios: &mut Bios,
    cpu: &mut Cpu,
    memory: &mut Memory,
    serial: &mut gp64_hw::serial::SerialPorts,
    tick: &mut dyn FnMut(&mut gp64_hw::serial::SerialPorts, usize),
) -> usize {
    for dispatches in 1..=1_000_000usize {
        match bios.int14(cpu, memory, serial, tick).unwrap() {
            Int14Disp::Iret => return dispatches,
            Int14Disp::Retry => {}
        }
    }
    panic!("INT 14h wait never completed");
}

// ===== INT 14h UART timing advancement tests (Stage 2C) =====

// ===== INT 17h (printer) tests =====

// ===== copy_memory_block tests =====

// ===== INT 15h AH=87h (Move Extended Memory Block) tests =====

// ===== INT 15h AH=83h (Event Wait) additional tests =====

// ===== INT 15h AH=C1h (EBDA) test =====

// ===== INT 15h AX=E881h (32-bit Extended Memory Size) tests =====

// ===== INT 15h AH=E8h default (unsupported sub-function) test =====

// ===== E820 edge case tests =====

// ===== tick_event_wait tests =====

// ===== generate_e820_map tests =====

// ===== parse_descriptor_base test =====

// ===== load_font_to_plane2 test =====

// ===== E820Entry tests =====

// ===== Coverage: INT 16h keyboard (lines 2833-2959) =====

// ===== Coverage: INT 10h palette (lines 898-999) =====

// ===== Coverage: INT 10h set border / set all palette (lines 985-999) =====

// ===== Coverage: INT 10h select display page for graphics (line 835) =====

// ===== Coverage: grayscale_triplet clamping (line 501) =====

// ===== Coverage: set_dcc_index swapped bytes (line 479) =====

// ===== Coverage: toggle_blinking (lines 1902-1916) =====

// ===== Coverage: select_dac_page / get_dac_page (lines 1918-1955) =====

// ===== Coverage: EGA RIL functions (lines 2006-2137) =====

// ===== Coverage: INT 15h wait (line 3601) =====

// ===== Coverage: INT 15h extended memory move (lines 3641, 3685-3723) =====

// ===== Coverage: INT 15h E801h small memory (lines 3685-3723) =====

// ===== Coverage: video state (lines 1603, 1624, 1759) =====

// ===== Coverage: INT 10h 12h subfunctions (lines 1329, 1338) =====

// ===== Coverage: INT 10h AH=11h font subfunctions (line 1217) =====

// ===== Coverage: INT 10h write string wrap (line 1392) =====

// ===== Coverage: tick_event_wait (lines 4397) =====

// ===== Coverage: update_text_metrics_for_font (lines 4557, 4595) =====

// ===== Coverage: INT 10h functionality state misc flags (line 1603) =====

// ===== Coverage: font_blocks_from_spec =====

// ===== Coverage: cga_mode_control_from_table (line 765) =====

// ===== Coverage: INT 10h load_font_to_plane2_block edge (line 4501, 4506) =====

// ======================================================================
// Batch 2: Target remaining uncovered implementation branches
// ======================================================================

// --- CGA mode control from table returning None (line 765) ---
// This happens when set_mode_bios is called with mode > 7
// But the branch on line 765 is mode <= 7 && cga_mode_control returns None
// Actually cga_mode_control_from_table always succeeds for mode 0-7 since
// the table has those entries. The None branch would need the table lookup
// to fail. Let's skip this one.

// ======================================================================
// Batch 3: Target remaining uncovered implementation branches
// ======================================================================

// --- generate_e820_map without extended memory (line 229: ram_size <= 1MB) ---
// Cannot test directly since Memory::new() always gives > 1MB.
// But we CAN test the branch indirectly via generate_e820_map which
// checks `ram_size > 0x10_0000`. Default memory IS > 1MB so this branch
// is already covered as "true". The "false" path (<=1MB) would need
// custom memory which isn't supported.

// Skip - structurally unreachable with current mode table.

// ======================================================================
// Batch 4: Target specific uncovered branch sides
// ======================================================================

// --- INT 10h set_mode: cga_mode_control returns None for mode 8+ (line 765) ---
// Mode 8 is not in standard table, so cga_mode_control_from_table returns None
// But mode 8+ may not even enter the `if mode <= 0x07` block.
// Line 765 uncovered means `cga_mode_control_from_table(mode)` returns None
// for a mode <= 0x07. Let's check which modes return None.
// Actually, modes 0-7 all have entries, so None means the mode was masked via &0x7F
// and produced a value 0-7 that's not in the table. All modes 0-7 are in the table.
// This branch is likely structurally unreachable.

// --- INT 16h: AH=05h store key successfully (line 2959 true path) ---
// This is already covered by test_int16_store_keycode. The uncovered
// branch 2959 (branch 1) means the `false` (buffer full) was untested.
// But we have test_int16_store_keycode_buffer_full_b3 for that.
// It might be a compound condition issue. Let me verify what's at 2959:
// `if bda::kbd_enqueue(memory, scancode, ascii)` - true vs false.
// True is covered by test_int16_store_keycode, false by buffer_full.
// Both should be covered. Maybe it wasn't re-measured. Skip.

// ===== Coverage: E801h / E881h with max RAM (lines 3685, 3716) =====
// Note: "max RAM" here means the DEFAULT_RAM_SIZE a fresh Memory exposes.

// ===== Coverage: INT 16h AH=11h peek with E0 (line 2928 false branch) =====

// ===== Coverage: INT 10h set video mode with mode > 7 (line 765 CGA branch) =====

// ===== Coverage: E820 with max RAM (line 229) =====

// ===== Coverage: E820 traversal with carry/type checks (lines 5837, 5847) =====

// ===== Coverage: EGA RIL with invalid group (line 2006) =====

// ===== Coverage: INT 10h AH=10h subfunctions for attribute registers =====

// ===== Coverage: toggle_blinking (line 1902) =====

// ===== Coverage: select_dac_page (line 1920) and get_dac_page (line 1943) =====

// ===== Coverage: save/restore functionality state attr check (line 1603) =====

// ===== Coverage: load_user_font_to_plane2 bounds check (line 4544) =====

// ===== Coverage: memory reporting invariant with max RAM =====

// ===== Coverage: E820 generate with max RAM (line 229 true branch) =====

// ===== Coverage: E881h with max RAM =====

// ===== Coverage batch 5: target remaining uncovered source branches =====

// ===== Coverage: copy_memory_block paths (lines 259-291) =====

// ===== Coverage: set_dcc_index swap_bytes path (line 479) =====

// ===== Coverage: grayscale_triplet clamp (line 501) =====

// ===== Coverage: cga_mode_control_from_table mode > 7 (line 507) =====

// ===== Coverage: INT 10h set video mode - text mode 07 clear (line 666) =====

// ===== Coverage: INT 10h set video mode - CGA graphics mode clear (lines 681-685) =====

// ===== Coverage: INT 10h set video mode - EGA/VGA planar mode clear (lines 687-702) =====

// ===== Coverage: INT 10h AH=05 select display page non-text (line 835) =====

// ===== Coverage: INT 10h AH=0Bh palette set (lines 894-905) =====

// ===== Coverage: grayscale_enabled + DAC set (lines 1036, 1067) =====

// ===== Coverage: INT 10h AH=13h write string with wrapping (lines 1380-1414) =====

// ===== Coverage: INT 10h AH=1Ah DCC set (line 1430) =====

// ===== Coverage: INT 10h AH=1Bh with BX!=0 (line 1521) =====

// ===== Coverage: INT 10h AH=FAh EGA RIL version (line 1486) =====

// ===== Coverage: INT 10h AH=12h BL=30h scan line settings (lines 1301-1324) =====

// ===== Coverage: INT 10h AH=12h BL=31h default palette loading (line 1329) =====

// ===== Coverage: INT 10h AH=12h BL=33h grayscale summing (line 1338) =====

// ===== Coverage: INT 10h AH=12h BL=34h cursor emulation (line 1354) =====

// ===== Coverage: INT 10h AH=12h BL=36h screen refresh (line 1354) =====

// ===== Coverage: INT 10h AH=11h font sub-functions (lines 1209-1250) =====

fn vector(memory: &Memory, number: usize) -> (u16, u16) {
    (memory.read_u16(number * 4 + 2), memory.read_u16(number * 4))
}

// ===== Coverage: INT 10h AH=11h AL=30h - Get font info (line 1256) =====

// ===== Coverage: INT 15h APM subfunctions (lines 4177-4216) =====

// ===== Coverage: EGA RIL single-register read/write (lines 1982-2043) =====

// ===== Coverage: INT 10h AH=10h additional sub-functions =====

// ===== Coverage: INT 10h AH=10h AL=07h/08h/09h - Read palette (lines 1008-1029) =====

// ===== Coverage: INT 10h AH=10h AL=03h - Toggle blinking via INT (line 1005) =====

// ===== Coverage: INT 10h AH=0Ch - Write pixel (line 937) =====

// ===== Coverage: INT 10h AH=1Ch - Video state save/restore =====

// ===== Coverage: INT 10h Hercules and TopView (lines 1495-1504) =====

// ===== Coverage: INT 10h AH=12h BL=20h/32h print screen status (line 1296) =====

// ===== Coverage: INT 10h AH=12h BL=10h EGA info (line 1287) =====

// ===== Coverage: INT 10h set video mode - EGA mode 0x0D/0x0E font variants =====

// ===== Coverage: INT 10h AH=1Ch video state save/restore subfunctions =====

// ===== Coverage: INT 10h AH=0Dh - Read pixel =====

// ===== Coverage: INT 10h AH=0Eh - Teletype output =====

// ===== Coverage: INT 10h AH=0Fh - Get video mode =====

// ===== Coverage: INT 15h AH=53h APM connect (0x01-0x04) =====

// ===== Coverage: INT 10h miscellaneous subfunctions for completeness =====

// ===== Branch coverage: grayscale DAC (lines 1036, 1067, 501) =====

// ===== Branch coverage: INT 10h AH=11h AL=21h with CX=0 (line 1217) =====

// ===== Branch coverage: INT 10h AH=12h BL=30h AL>2 (line 1301) =====

// ===== Branch coverage: INT 10h AH=12h BL=31h AL=1 (line 1329) =====

// ===== Branch coverage: INT 10h AH=12h BL=33h AL=1 (line 1338 false) =====

// ===== Branch coverage: INT 10h AH=12h BL=36h AL=0 (line 1354 true) =====

// ===== Branch coverage: INT 10h AH=13h write string with attrs and wrapping (lines 1380-1414) =====

// ===== Branch coverage: days_to_ymd negative z (line 4595 false) =====

// ===== Branch coverage: INT 15h E881h with default RAM (lines 13162, 13167) =====

// ===== Branch coverage: Memory size with 16MB RAM =====

// ===== Branch coverage: cga_mode_control_from_table returns None (line 765 false) =====

// ===== Branch coverage: kbd_trace_allow() False branch (line 2833 false) =====

// ===== Branch coverage: days_to_ymd z < 0 branch (line 4595 false) =====

// ===== Branch coverage: INT 10h AH=1Bh attr condition (line 1603 false) =====

// ---- E820Type coverage ----

// ---- E820Entry coverage ----

// ---- Bios fields coverage ----

// ---- to_bcd helper ----

// ---- days_to_ymd edge cases ----

// ---- Parse descriptor base with high byte ----

// ---- E820 generate with no extended memory ----

// ---- Copy memory block: non-aligned length forward ----

// ===================== INT 1Ah coverage tests =====================

// ===================== INT 15h coverage tests =====================

// ===================== INT 16h coverage tests =====================

// ===================== misc.rs (INT 14h, INT 17h) coverage tests =====================

// ===================== INT 10h coverage tests =====================
