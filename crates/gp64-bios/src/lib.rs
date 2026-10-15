//! BIOS services emulation

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod int10;
mod int15;
mod int16;
mod int1a;
mod misc;

pub use int15::Int15Disp;
pub use int16::Int16Disp;
pub use misc::Int14Disp;

use gp64_memory::Memory;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

const ROM_FUNCTIONALITY_TABLE: [u8; 16] = [
    0xFF, 0xFF, 0x0F, 0x00, 0x00, 0x00, 0x00, 0x07, 0x04, 0x02, 0xFF, 0x0E, 0x00, 0x00, 0x00, 0x00,
];

pub(crate) const ROM_VIDEO_PARAM_TABLE: [u8; 88] = [
    // modes 0/1
    0x38, 0x28, 0x2D, 0x0A, 0x1F, 0x06, 0x19, 0x1C, 0x02, 0x07, 0x06, 0x07, 0x00, 0x00, 0x00, 0x00,
    // modes 2/3
    0x71, 0x50, 0x5A, 0x0A, 0x1F, 0x06, 0x19, 0x1C, 0x02, 0x07, 0x06, 0x07, 0x00, 0x00, 0x00, 0x00,
    // modes 4/5
    0x38, 0x28, 0x2D, 0x0A, 0x7F, 0x06, 0x64, 0x70, 0x02, 0x01, 0x06, 0x07, 0x00, 0x00, 0x00, 0x00,
    // modes 6/7
    0x61, 0x50, 0x52, 0x0F, 0x19, 0x06, 0x19, 0x19, 0x02, 0x0D, 0x0B, 0x0C, 0x00, 0x00, 0x00, 0x00,
    // buffer length (words)
    0x00, 0x08, 0x00, 0x10, 0x00, 0x40, 0x00, 0x40, // columns
    40, 40, 80, 80, 40, 40, 80, 80, // CGA mode register
    0x2C, 0x28, 0x2D, 0x29, 0x2A, 0x2E, 0x1E, 0x29,
];

/// E820 memory map entry type
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum E820Type {
    AddressRangeMemory = 1,
    AddressRangeReserved = 2,
    AddressRangeACPI = 3,
    AddressRangeNVS = 4,
    AddressRangeUnusable = 5,
}

/// E820 memory map entry (20-byte standard format)
#[derive(Clone, Copy, Debug)]
pub struct E820Entry {
    pub base: u64,
    pub length: u64,
    pub entry_type: u32,
}

impl E820Entry {
    pub const SIZE: usize = 20;
    pub const SIZE_EXTENDED: usize = 24;

    pub fn new(base: u64, length: u64, entry_type: E820Type) -> Self {
        Self {
            base,
            length,
            entry_type: entry_type as u32,
        }
    }

    pub fn write_to_memory(&self, memory: &mut Memory, es: u16, di: u16) {
        let addr = ((es as u32) << 4) + di as u32;
        memory.write_u64(addr as usize, self.base);
        memory.write_u64((addr + 8) as usize, self.length);
        memory.write_u32((addr + 16) as usize, self.entry_type);
    }
}

const ROM_VIDEO_DCC_TABLE: [u8; 36] = [
    0x10, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x02, 0x02, 0x01, 0x00, 0x04, 0x04, 0x01,
    0x00, 0x05, 0x02, 0x05, 0x00, 0x06, 0x01, 0x06, 0x05, 0x06, 0x00, 0x08, 0x01, 0x08, 0x00, 0x07,
    0x02, 0x07, 0x06, 0x07,
];

/// System configuration table for INT 15h AH=C0
/// This table describes the system hardware configuration
/// Format: 2-byte length, then configuration data
#[rustfmt::skip]
const ROM_SYSTEM_CONFIG_TABLE: [u8; 16] = [
    0x08, 0x00,     // Length of following data (8 bytes)
    ROM_MODEL_BYTE,
    0x01,           // Submodel byte
    0x00,           // BIOS revision
    0x70,           // Feature byte 1: bits 6/5/4 set; bit 2 clear (no HLE EBDA)
    0x00,           // Feature byte 2
    0x00,           // Feature byte 3
    0x00,           // Feature byte 4
    0x00,           // Feature byte 5
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Reserved
];

/// Default 1.44 MB diskette parameter table (Disk Base Table), 11 bytes, pointed
/// at by INT 1Eh and returned by INT 13h AH=08h in ES:DI for floppies
/// (MDD-BUG-ANVIL-00015). Standard AT values: step-rate/head-unload,
/// head-load/DMA, motor-off delay, bytes-per-sector code (2 = 512),
/// sectors-per-track (18), R/W gap, DTL, format gap, format fill byte,
/// head-settle time, motor-start time.
const ROM_DISKETTE_PARAM_TABLE: [u8; 11] = [
    0xAF, 0x02, 0x25, 0x02, 0x12, 0x1B, 0xFF, 0x6C, 0xF6, 0x0F, 0x08,
];

/// BIOS date string location in ROM (F000:FFF5)
/// Standard 8-character date format: "MM/DD/YY"
const ROM_BIOS_DATE_OFFSET: u16 = 0xFFF5;

/// IBM PC model byte location in ROM (F000:FFFE).
const ROM_MODEL_BYTE_OFFSET: u16 = 0xFFFE;

/// AT-compatible machine model byte.
const ROM_MODEL_BYTE: u8 = 0xFC;

/// Disk Address Packet for Extended INT 13h (LBA access)
/// Memory layout:
///   +00h: 1 byte - packet size (10h minimum)
///   +01h: 1 byte - reserved (0)
///   +02h: 2 bytes - sector count
///   +04h: 2 bytes - buffer offset
///   +06h: 2 bytes - buffer segment
///   +08h: 8 bytes - starting LBA
pub struct DiskAddressPacket {
    pub size: u8,
    pub sector_count: u16,
    pub buffer_offset: u16,
    pub buffer_segment: u16,
    pub lba: u64,
}

impl DiskAddressPacket {
    pub fn read_from_memory(memory: &Memory, addr: usize) -> Self {
        Self {
            size: memory.read_u8(addr),
            // +01 is reserved
            sector_count: memory.read_u16(addr + 2),
            buffer_offset: memory.read_u16(addr + 4),
            buffer_segment: memory.read_u16(addr + 6),
            lba: memory.read_u64(addr + 8),
        }
    }
}

#[derive(Clone)]
pub struct Bios {
    /// Timer tick count (incremented 18.2 times per second)
    pub tick_count: u32,
    /// INT 15h AH=83h event wait state: linear address of flag byte
    pub(crate) event_wait_flag_addr: usize,
    /// Remaining microseconds for the event wait interval
    pub(crate) event_wait_remaining_us: u64,
    /// Whether an event wait interval is currently active
    pub(crate) event_wait_active: bool,
    /// Whether to advertise an EMM at the INT 67h vector (`EMMXXXX0` at
    /// F000:000A). Cleared by `--no-ems` (MDD-BUG-KILN-00647).
    ///
    /// Held on the BIOS rather than applied once to memory because `init_ivt`
    /// rewrites the ROM data area every time it runs: a caller that retracted
    /// the signature and then (re-)initialised the IVT had its retraction
    /// silently undone. Keeping the decision here makes the write correct at
    /// every init, in any order.
    ems_signature_present: bool,
    /// Poll progress of an in-flight INT 14h timeout wait, if any
    /// (MDD-BUG-KILN-00278). `None` whenever no wait is suspended.
    pub(crate) serial_wait: Option<crate::misc::SerialWait>,
    /// Per-instance keyboard trace budget; diagnostic state is not shared by
    /// independent BIOS instances or snapshots.
    kbd_trace_budget: KbdTraceBudget,
}

use gp64_types::bda::BIOS_TICKS_PER_DAY;

#[derive(Default)]
struct KbdTraceBudget(AtomicUsize);

impl Clone for KbdTraceBudget {
    fn clone(&self) -> Self {
        Self(AtomicUsize::new(self.0.load(Ordering::Relaxed)))
    }
}

impl Bios {
    /// Permit one of the bounded keyboard trace messages.
    ///
    /// The tracing check comes first so normal INT 16h traffic does not touch
    /// the atomic budget. Once the cap is reached, `fetch_update` returns
    /// without another write.
    #[inline]
    pub(crate) fn kbd_trace_allow(&self) -> bool {
        const MAX: usize = 5000;
        if !tracing::enabled!(target: "gp64::kbd::int", tracing::Level::DEBUG) {
            return false;
        }
        self.kbd_trace_budget
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < MAX).then_some(count + 1)
            })
            .is_ok()
    }

    pub fn new() -> Self {
        Self {
            tick_count: 0,
            event_wait_flag_addr: 0,
            event_wait_remaining_us: 0,
            event_wait_active: false,
            ems_signature_present: false,
            serial_wait: None,
            kbd_trace_budget: KbdTraceBudget::default(),
        }
    }

    /// Choose whether `init_ivt` advertises an EMM at the INT 67h vector.
    ///
    /// Set this BEFORE or AFTER `init_ivt` — it is consulted at every init, so
    /// the two are order-independent. That is the point: retracting the
    /// signature directly in memory and then re-running `init_ivt` used to
    /// restore it (MDD-BUG-KILN-00647).
    pub fn set_ems_signature_present(&mut self, _present: bool) {
        self.ems_signature_present = false;
    }

    /// Maximum number of E820 entries
    const E820_MAX_ENTRIES: usize = 5;

    // BIOS ROM addresses for fonts (F000:xxxx)
    // 8x8 font at F000:E000 (2048 bytes for 256 chars)
    const ROM_FONT_8X8_OFFSET: u16 = 0xE000;
    // 8x14 font at F000:D000 (3584 bytes for 256 chars)
    const ROM_FONT_8X14_OFFSET: u16 = 0xD000;
    // 8x16 font at F000:E800 (4096 bytes for 256 chars)
    const ROM_FONT_8X16_OFFSET: u16 = 0xE800;
    // Legacy 8x8 font location used by some programs reading ROM directly.
    // BIOS places the lower 128 chars at F000:FA6E (0x20 starts at F000:FB6E).
    const ROM_FONT_8X8_LEGACY_OFFSET: u16 = 0xFA6E;
    const ROM_FUNCTIONALITY_TABLE_OFFSET: u16 = 0xDE00;
    const ROM_VIDEO_DCC_TABLE_OFFSET: u16 = 0xDE20;
    const ROM_VIDEO_SAVE_POINTER_TABLE_OFFSET: u16 = 0xDE50;
    const ROM_VIDEO_SAVE_POINTERS_OFFSET: u16 = 0xDE70;
    /// Video parameter table offset in the free F000 ROM gap.
    const ROM_VIDEO_PARAM_TABLE_OFFSET: u16 = 0x1200;
    /// Diskette parameter table (Disk Base Table) offset in ROM. Placed in the
    /// large free zero-filled ROM gap below the 8x14 font (0xD000), clear of every
    /// stub/table/font the BIOS installs. Pointed at by INT 1Eh and returned in
    /// ES:DI by INT 13h AH=08h for floppies (MDD-BUG-ANVIL-00015).
    pub(crate) const ROM_DISKETTE_PARAM_TABLE_OFFSET: u16 = 0x1000;
    const VIDEO_ROM_SEGMENT: u16 = 0xC000;
    const VIDEO_RIL_VERSION_OFFSET: u16 = 0x0030;
    /// System configuration table offset in the free F000 ROM gap.
    const ROM_SYSTEM_CONFIG_TABLE_OFFSET: u16 = 0x1100;

    /// Base offset for IVT entry points in ROM
    /// Entry points are at `F000:(IVT_ENTRY_BASE + vector*4)`
    /// This avoids conflict with EMS signature at F000:000A
    pub const IVT_ENTRY_BASE: u16 = 0x0100;

    /// Assembler keyboard stub offsets in F000 ROM.
    ///
    /// Reserved range `F000:F800..F000:F900` — do not place HLE entry points
    /// here. The range itself is defined by
    /// [`gp64_types::bios_layout::STUB_CODE_RANGE_START`] and
    /// [`gp64_types::bios_layout::STUB_CODE_RANGE_END`], which the
    /// fast-interpreter outer gate and the HLE dispatcher pass-through both
    /// read so the two sites cannot drift.
    pub const INT16_WAIT_00: u16 = gp64_types::bios_layout::STUB_CODE_RANGE_START;
    pub const INT16_WAIT_10: u16 = gp64_types::bios_layout::STUB_CODE_RANGE_START + 0x40;
    pub const INT16_PEEK_01: u16 = gp64_types::bios_layout::STUB_CODE_RANGE_START + 0x80;
    pub const INT16_PEEK_11: u16 = gp64_types::bios_layout::STUB_CODE_RANGE_START + 0xB0;
    /// MDD-BUG-KILN-00276: moved from +0xD0 to +0xE0. The AH=11h peek stub
    /// filled its 0x20-byte slot exactly, leaving no room for the
    /// SP-independent ZF-clear, so the slot below it had to grow. This stub is
    /// 25 bytes and the range ends at +0x100, so 0x20 still fits it.
    pub const INT21_KBD_WAIT: u16 = gp64_types::bios_layout::STUB_CODE_RANGE_START + 0xE0;
    /// IRET immediately after the wait stub's nested INT 21h (24 code bytes).
    /// This instruction continuation identifies resumption of the retained DOS entry.
    pub const INT21_KBD_WAIT_RETURN: u16 = Self::INT21_KBD_WAIT + 24;

    /// Regions used by standard VGA BIOS metadata.
    pub const C0000_RESERVED: &'static [(&'static str, u16, u16)] = &[
        ("option-ROM header", 0x0000, 0x20),
        ("RIL version table", Self::VIDEO_RIL_VERSION_OFFSET, 0x02),
    ];

    /// Mouse driver stub location in conventional memory.
    /// A small JMP FAR trampoline lives here so that programs probing the
    /// INT 33h vector see a non-BIOS handler segment and conclude a mouse
    /// driver is installed.
    const MOUSE_STUB_SEG: u16 = 0x0050;
    const MOUSE_STUB_OFF: u16 = 0x00F0;

    /// Keyboard IRQ1 handler stub location in conventional memory.
    /// A real x86 stub that reads port 60h, calls INT 15h AH=4Fh, checks CF,
    /// and either sends EOI (key consumed) or traps to HLE for BDA processing.
    /// Same segment as the mouse stub — below MCB start, safe from allocation.
    pub const KBD_IRQ1_STUB_SEG: u16 = 0x0050;
    pub const KBD_IRQ1_STUB_OFF: u16 = 0x0080;

    /// HLE trap target for the keyboard IRQ1 stub's "process normally" path.
    /// When INT 15h returns CF=1 (process scancode), the stub does JMP FAR F000:FF10
    /// which triggers the HLE sentinel. The dispatch handler reads AL (scancode),
    /// writes to BDA, sends EOI, restores registers, and IRETs.
    pub const KBD_IRQ1_HLE_TRAP_OFF: u16 = 0xFF10;

    /// BIOS INT 8 ISR entry point — matches the DOS model's `BIOS_DEFAULT_IRQ0_LOCATION`
    /// at `RealMake(0xf000, 0xfea5)` (the canonical IBM PC/AT IRQ0 location).
    ///
    /// `init_ivt` writes `IVT[0x08] = (offset=BIOS_INT8_ISR_OFF, segment=0xF000)`
    /// and installs a 16-byte real-bytes ISR at F000:FEA5..F000:FEB4 that does
    /// STI / BIOS-tick HLE trap / PUSH DS/AX/DX / INT 1Ch / CLI / EOI / POP /
    /// IRET — same shape as the DOS model's CB_IRQ0 sequence.  See
    /// `wrk_docs/2026.05.21 - HLD - HLE BIOS INT 8 chain fix HLD-A v3.md`.
    pub const BIOS_INT8_ISR_OFF: u16 = 0xFEA5;

    /// HLE trap byte offset inside the BIOS INT 8 ISR. Sits at the second byte
    /// of the ISR (= `BIOS_INT8_ISR_OFF + 1`) and is recognised by the HLE
    /// dispatcher to drive the BIOS-tick / floppy motor / PIT housekeeping.
    /// The byte at this address is `0xF1` (ICEBP / INT1) — a 1-byte
    /// placeholder; the dispatcher intercepts before it executes.
    pub const BIOS_INT8_TICK_TRAP_OFF: u16 = 0xFEA6;

    /// BIOS equipment word returned by INT 11h and stored in BDA at 0x0410.
    ///   Bit 0:  Floppy drive installed
    ///   Bit 1:  Math coprocessor (x87 FPU) installed
    ///   Bit 2:  PS/2 pointing device installed
    ///   Bit 5:  Initial video mode — 80×25 colour
    ///   Bits 9-11: Number of RS-232 serial ports (2 = COM1 + COM2)
    pub const EQUIPMENT_WORD: u16 = 0x0427; // floppy + FPU + PS/2 mouse + 80x25 color + 2 serial ports

    /// Floppy drives an equipment word reports: bit 0 says any are fitted
    /// and bits 7:6 hold the count minus one. DOS names them from A:.
    pub const fn floppy_drives_fitted(equipment: u16) -> u8 {
        if equipment & 1 == 0 {
            0
        } else {
            ((equipment >> 6) & 3) as u8 + 1
        }
    }
    /// Conventional memory size in KiB, mirrored by INT 12h and BDA 0040:0013.
    pub const CONVENTIONAL_MEMORY_KB: u16 = 640;

    /// Initialize interrupt vector table and BIOS ROM font data.
    ///
    /// When `skip_video_init` is true, IVT[0x10] and BDA video fields are
    /// left uninitialised so that a real VGA BIOS ROM can set them during
    /// POST-lite.  All non-video fields and all other IVT entries are still
    /// written normally.
    pub fn init_ivt(&self, memory: &mut Memory, skip_video_init: bool) {
        // Set up IVT entries to point to entry points in BIOS ROM.
        // Important vectors (0x00-0x3F) get unique addresses for proper emulation.
        // Less-used vectors are grouped to share addresses, which is more realistic
        // (real BIOS implementations point unused vectors to common stubs).
        // scans the IVT looking for duplicate entries to detect DPMI presence.
        //
        // Groups:
        // - 0x00-0x3F: Unique (CPU exceptions, BIOS, DOS)
        // - 0x40-0x5F: Share one stub (0x500)
        // - 0x60-0x7F: Share one stub (0x580)
        // - 0x80-0x9F: Share one stub (0x600)
        // - 0xA0-0xBF: Share one stub (0x680)
        // - 0xC0-0xFF: Unique (for any future use)

        // First, write IRET stubs at the group entry points
        let stub_offsets: &[u16] = &[0x0500, 0x0580, 0x0600, 0x0680];
        for &offset in stub_offsets {
            let linear_addr = Memory::seg_offset_to_linear(0xF000, offset);
            memory.write_rom(linear_addr, 0xCF); // IRET
        }

        for vector in 0..256 {
            let ivt_addr = vector * 4;
            #[allow(clippy::match_same_arms)] // 0x67/0x7A must be separate from 0x60..=0x7F
            let entry_point = match vector {
                // Important vectors get unique addresses
                0x00..=0x3F | 0x67 | 0x7A | 0xC0..=0xFF => {
                    Self::IVT_ENTRY_BASE + (vector as u16) * 4
                }
                // Group vectors to share stub addresses
                0x40..=0x5F => 0x0500,
                // Note: 0x67 and 0x7A are handled above, so 0x60..=0x7F excludes them via match order
                0x60..=0x7F => 0x0580,
                0x80..=0x9F => 0x0600,
                0xA0..=0xBF => 0x0680,
                _ => unreachable!(),
            };

            // When skip_video_init is set, leave IVT[0x10] for the VGA BIOS
            // ROM to install.  The IRET stub byte is still written so the
            // address is valid if anything reads it.
            //
            // Vector 0x08 (IRQ0 timer) is special-cased: it points at
            // `BIOS_INT8_ISR_OFF` (F000:FEA5) — the canonical IBM PC/AT IRQ0
            // location matching the DOS model — so that guest code chaining via
            // `[saved_prev_int8]` reaches a real-bytes BIOS ISR instead of a
            // single-byte IRET stub.  The legacy F000:0120 IRET byte is still
            // written below (the `0x00..=0x3F` arm below catches it) so any
            // code that incidentally reads that address sees a benign IRET.
            if !(skip_video_init && vector == 0x10) {
                let ivt_entry_offset = if vector == 0x08 {
                    Self::BIOS_INT8_ISR_OFF
                } else {
                    entry_point
                };
                memory.write_u16(ivt_addr, ivt_entry_offset); // Offset
                memory.write_u16(ivt_addr + 2, 0xF000); // Segment
            }

            // Write IRET at unique entry points (grouped ones already have IRET)
            if matches!(vector, 0x00..=0x3F | 0x67 | 0x7A | 0xC0..=0xFF) {
                let linear_addr = Memory::seg_offset_to_linear(0xF000, entry_point);
                memory.write_rom(linear_addr, 0xCF);
            }
        }

        // INT 33h (mouse driver): install a stub in conventional memory so
        // and recognise a mouse driver as installed.  If the vector pointed at
        // F000:xxxx, diagnostic tools would conclude "no driver loaded" since
        // the BIOS ROM segment means no TSR hooked the vector.
        //
        // The stub contains JMP FAR F000:<mouse_hle_entry>.  When executed,
        // the JMP transfers to the BIOS ROM address where the HLE dispatcher
        // intercepts and runs the Rust mouse handler.  JMP FAR is transparent
        // to the stack, so hle_iret() correctly pops the caller's INT frame.
        let mouse_hle_entry = Self::IVT_ENTRY_BASE + 0x33 * 4;
        let stub_addr = Memory::seg_offset_to_linear(Self::MOUSE_STUB_SEG, Self::MOUSE_STUB_OFF);
        memory.write_u8(stub_addr, 0xEA); // JMP FAR imm16:imm16
        memory.write_u16(stub_addr + 1, mouse_hle_entry); // offset in F000
        memory.write_u16(stub_addr + 3, 0xF000); // segment F000

        // Point INT 33h IVT at the conventional-memory stub
        let ivt_33 = 0x33 * 4;
        memory.write_u16(ivt_33, Self::MOUSE_STUB_OFF);
        memory.write_u16(ivt_33 + 2, Self::MOUSE_STUB_SEG);

        // Initialize protected mode HLE stubs at F000:FE00-FFFF.
        // DPMI sets up IDT entries pointing to offsets 0xFE00 + (vector * 2) in segment 0xF000.
        // Each stub is 2 bytes: IRET (0xCF) + NOP (0x90) padding.
        // Without this, the memory contains 0xCC (INT 3) causing infinite exception loops.
        // init_rom_system_info later overwrites FFF5..FFFE with the ROM identity
        // (date, model byte); the dispatcher traps these stubs by address.
        for vector in 0u16..256 {
            let offset = 0xFE00 + (vector * 2);
            let linear_addr = Memory::seg_offset_to_linear(0xF000, offset);
            memory.write_rom(linear_addr, 0xCF); // IRET
            memory.write_rom(linear_addr + 1, 0x90); // NOP (pad to 2 bytes)
        }

        // BIOS INT 8 ISR — real-bytes 16-byte handler at F000:FEA5..FEB4
        // matching the DOS model's CB_IRQ0 sequence at BIOS_DEFAULT_IRQ0_LOCATION.
        //
        //   F000:FEA5  FB              STI
        //   F000:FEA6  F1              ICEBP / INT1   ; BIOS-tick HLE trap
        //   F000:FEA7  1E              PUSH DS
        //   F000:FEA8  50              PUSH AX
        //   F000:FEA9  52              PUSH DX
        //   F000:FEAA  CD 1C           INT 1Ch
        //   F000:FEAC  FA              CLI
        //   F000:FEAD  B0 20           MOV AL, 20h
        //   F000:FEAF  E6 20           OUT 20h, AL    ; PIC EOI to master 8259
        //   F000:FEB1  5A              POP DX
        //   F000:FEB2  58              POP AX
        //   F000:FEB3  1F              POP DS
        //   F000:FEB4  CF              IRET
        //
        // Ordering matters: the PM IDT stub loop above writes 0xCF 0x90 IRET+NOP
        // pairs across F000:FE00..FFFF, which overlaps our ISR span. The ISR
        // overwrite must come *after* the PM stub loop so its bytes survive.
        // Regression test: test_init_ivt_pm_stub_loop_runs_before_isr_overwrite.
        // See wrk_docs/2026.05.21 - HLD - HLE BIOS INT 8 chain fix HLD-A v3.md
        // §3.1 / §4.1 for rationale.
        {
            const BIOS_INT8_ISR_BYTES: [u8; 16] = [
                0xFB, // STI
                0xF1, // ICEBP / INT1  — BIOS-tick HLE trap (BIOS_INT8_TICK_TRAP_OFF)
                0x1E, // PUSH DS
                0x50, // PUSH AX
                0x52, // PUSH DX
                0xCD, 0x1C, // INT 1Ch
                0xFA, // CLI
                0xB0, 0x20, // MOV AL, 20h
                0xE6, 0x20, // OUT 20h, AL  — PIC EOI to master 8259
                0x5A, // POP DX
                0x58, // POP AX
                0x1F, // POP DS
                0xCF, // IRET
            ];
            let isr_base = Memory::seg_offset_to_linear(0xF000, Self::BIOS_INT8_ISR_OFF);
            for (i, &byte) in BIOS_INT8_ISR_BYTES.iter().enumerate() {
                memory.write_rom(isr_base + i, byte);
            }
        }

        // Keyboard IRQ1 handler: install a real x86 stub in conventional memory
        // that reads port 60h, calls INT 15h AH=4Fh (keyboard intercept), and
        // either sends EOI (key consumed, CF=0) or traps to F000:FF10 for HLE
        // BDA processing (key not consumed, CF=1).
        //
        // This replaces the direct HLE handler at F000:0124 as the IVT[09h]
        // target. The stub makes the INT 15h call synchronously as real x86 code,
        // so resident keyboard intercept handlers execute synchronously.
        //
        // Must be written AFTER the DPMI stub loop above (which writes to FF10).
        {
            let stub_addr =
                Memory::seg_offset_to_linear(Self::KBD_IRQ1_STUB_SEG, Self::KBD_IRQ1_STUB_OFF);
            #[rustfmt::skip]
            let stub_bytes: &[u8] = &[
                0x60,                   // pusha          — save all GP registers
                0x1E,                   // push ds        — save DS
                0x06,                   // push es        — save ES
                0xE4, 0x60,             // in al, 60h     — read scancode from keyboard controller
                0xB4, 0x4F,             // mov ah, 4Fh    — INT 15h keyboard intercept function
                0xF9,                   // stc            — CF=1 default ("process normally")
                0xCD, 0x15,             // int 15h        — call interceptor (or default IRET)
                0x73, 0x05,             // jnc consumed   — CF=0 → key was consumed, skip to EOI
                // CF=1 → process normally: trap to HLE for BDA write
                0xEA,                   // jmp far F000:FF10
                (Self::KBD_IRQ1_HLE_TRAP_OFF & 0xFF) as u8,
                (Self::KBD_IRQ1_HLE_TRAP_OFF >> 8) as u8,
                0x00, 0xF0,             //   segment F000
                // consumed: send EOI, restore registers, IRET
                0xFA,                   // cli
                0xB0, 0x20,             // mov al, 20h
                0xE6, 0x20,             // out 20h, al    — EOI to PIC
                0x07,                   // pop es
                0x1F,                   // pop ds
                0x61,                   // popa
                0xCF,                   // iret
            ];
            for (i, &byte) in stub_bytes.iter().enumerate() {
                memory.write_u8(stub_addr + i, byte);
            }

            // Point IVT[09h] to our x86 stub in conventional memory instead of
            // the HLE sentinel at F000:0124. The stub reads port 60h, calls
            // INT 15h AH=4Fh synchronously, and traps to F000:FF10 for BDA
            // processing.
            let ivt_09 = 0x09 * 4;
            memory.write_u16(ivt_09, Self::KBD_IRQ1_STUB_OFF); // offset
            memory.write_u16(ivt_09 + 2, Self::KBD_IRQ1_STUB_SEG); // segment

            // Write an IRET at the HLE trap target. This follows the pattern of
            // other HLE entry points — the byte exists in case anything reads the
            // address, but the HLE sentinel intercepts before it executes.
            let trap_addr = Memory::seg_offset_to_linear(0xF000, Self::KBD_IRQ1_HLE_TRAP_OFF);
            memory.write_rom(trap_addr, 0xCF); // IRET
        }

        // Copy font data to BIOS ROM area
        // This allows programs to read fonts directly from ROM via INT 10h/11h/30h pointers
        Self::init_rom_fonts(memory);
        Self::init_rom_video_tables(memory);

        // INT 1Dh points to the Video Parameter Table in ROM.
        let ivt_1d_addr = 0x1D * 4;
        memory.write_u16(ivt_1d_addr, Self::ROM_VIDEO_PARAM_TABLE_OFFSET);
        memory.write_u16(ivt_1d_addr + 2, 0xF000);

        // INT 1Eh points to the Diskette Parameter Table (Disk Base Table) in
        // ROM. INT 13h AH=08h returns ES:DI here for floppies (MDD-BUG-ANVIL-00015).
        let dbt_addr = Memory::seg_offset_to_linear(0xF000, Self::ROM_DISKETTE_PARAM_TABLE_OFFSET);
        for (i, &byte) in ROM_DISKETTE_PARAM_TABLE.iter().enumerate() {
            memory.write_rom(dbt_addr + i, byte);
        }
        let ivt_1e_addr = 0x1E * 4;
        memory.write_u16(ivt_1e_addr, Self::ROM_DISKETTE_PARAM_TABLE_OFFSET);
        memory.write_u16(ivt_1e_addr + 2, 0xF000);

        // BIOS Data Area: COM port base addresses at 0040:0000.
        // Serial port discovery reads these BIOS-maintained base addresses.
        memory.write_u16(0x400, 0x03F8); // COM1 base I/O address
        memory.write_u16(0x402, 0x02F8); // COM2 base I/O address

        // BDA COM port timeout values at 0040:007Ch (one byte per port).
        // Standard BIOS POST default is 1. the DOS model also uses 1.
        // The timeout loop structure is: outer loop counts down the BDA value,
        // inner loop does 0xFFFF iterations per outer tick.
        memory.write_u8(0x47C, 1); // COM1 timeout
        memory.write_u8(0x47D, 1); // COM2 timeout
        memory.write_u8(0x47E, 1); // COM3 timeout
        memory.write_u8(0x47F, 1); // COM4 timeout

        // BIOS Data Area: equipment word at 0040:0010.
        // Must match INT 11h return value so programs reading the BDA directly
        memory.write_u16(0x410, Self::EQUIPMENT_WORD);

        // BIOS Data Area: no EBDA segment at 0040:000E.
        memory.write_u16(0x40E, 0);

        // Conventional memory size in KiB at 0040:0013. Must match INT 12h
        // for programs that read the BDA directly.
        memory.write_u16(0x413, Self::CONVENTIONAL_MEMORY_KB);

        // Hard disk count at 0040:0075. Real POST sets this by probing the IDE
        // controller; we set it unconditionally to match CMOS 0x12 = 0xF0 (one drive).
        memory.write_u8(0x475, 1);

        // BIOS Data Area defaults for mode 3 (80x25 text, VGA color).
        // A real BIOS POST sets mode 3 which initializes all these fields.
        // BDA 0040:0063 identifies the active CRTC port for direct I/O
        // independently of INT 10h service calls.
        //
        // When skip_video_init is true, the VGA BIOS ROM's POST routine
        // will set these fields itself, so we leave them uninitialised.
        if !skip_video_init {
            memory.write_u8(0x449, 0x03); // current video mode = 3
            memory.write_u16(0x44A, 80); // columns = 80
            memory.write_u16(0x44C, 0x1000); // page size = 4096 bytes
            memory.write_u16(0x44E, 0); // start address (word offset) = 0
            for page in 0..8 {
                memory.write_u16(0x450 + page * 2, 0); // cursor positions
            }
            memory.write_u16(0x460, 0x0E0F); // cursor shape: scanlines 14-15 for 16px font
            memory.write_u8(0x462, 0); // active display page = 0
            memory.write_u16(0x463, 0x3D4); // CRT controller base port (VGA color)
            memory.write_u8(0x465, 0x09); // current MSR
            memory.write_u8(0x466, 0x00); // CGA color select mirror
            memory.write_u8(0x484, 24); // rows - 1 = 24 (25 rows)
            memory.write_u8(0x485, 16); // character height
            memory.write_u8(0x487, 0x60); // video control
            memory.write_u8(0x488, 0xF9); // switches
            memory.write_u8(0x489, 0x51); // VGA flags
            memory.write_u8(0x48A, 11); // display combination code index: VGA color (0x08)
        }

        // Keyboard buffer boundaries (required by assembler keyboard stubs at F800-F8FF).
        // Without these, the stubs' CMP BX,[0082h] reads zeroes and corrupts the IVT.
        memory.write_u16(0x480, 0x001E); // 0040:0080 = buffer start offset
        memory.write_u16(0x482, 0x003E); // 0040:0082 = buffer end offset

        // Video save pointer table at 0040:00A8.
        if !skip_video_init {
            memory.write_u16(0x4A8, Self::ROM_VIDEO_SAVE_POINTERS_OFFSET);
            memory.write_u16(0x4AA, 0xF000);
        }

        // VGA BIOS ROM header at C000:0000.
        // On real hardware the VGA card's option ROM starts with a 55 AA
        // signature followed by a size byte (in 512-byte units). Programs
        // check this signature to detect EGA/VGA.
        let vga_rom_base = Memory::seg_offset_to_linear(Self::VIDEO_ROM_SEGMENT, 0);
        memory.write_u8(vga_rom_base, 0x55);
        memory.write_u8(vga_rom_base + 1, 0xAA);
        memory.write_u8(vga_rom_base + 2, 0x40); // 32 KB ROM (0x40 * 512)

        // RIL version table for INT 10h AH=FA.
        let ril_addr =
            Memory::seg_offset_to_linear(Self::VIDEO_ROM_SEGMENT, Self::VIDEO_RIL_VERSION_OFFSET);
        memory.write_u8(ril_addr, 0x01);
        memory.write_u8(ril_addr + 1, 0x00);

        // Install assembler keyboard stubs at F000:F800-F8FF.
        // These are real x86 code that executes in the interpreter, providing
        // IRQ delivery windows that HLE handlers cannot.
        Self::install_keyboard_stubs(memory);

        // Initialize BIOS date and system configuration table
        self.init_rom_system_info(memory);
    }

    /// Initialize BIOS date string, model byte and system configuration table in ROM
    fn init_rom_system_info(&self, memory: &mut Memory) {
        // Seed a synthetic BIOS identity using the standard date layout.
        let identity_addr = Memory::seg_offset_to_linear(0xF000, ROM_BIOS_DATE_OFFSET);
        for (i, &byte) in b"10/10/26".iter().enumerate() {
            memory.write_rom(identity_addr + i, byte);
        }
        memory.write_rom(
            Memory::seg_offset_to_linear(0xF000, ROM_MODEL_BYTE_OFFSET),
            ROM_MODEL_BYTE,
        );

        // Write the system configuration table into the reserved F000 gap.
        let config_addr =
            Memory::seg_offset_to_linear(0xF000, Self::ROM_SYSTEM_CONFIG_TABLE_OFFSET);
        for (i, &byte) in ROM_SYSTEM_CONFIG_TABLE.iter().enumerate() {
            memory.write_rom(config_addr + i, byte);
        }

        // Consult the BIOS's own flag, so a `--no-ems` retraction survives any
        // later re-init of the IVT (MDD-BUG-KILN-00647).
        Self::set_ems_driver_signature(memory, self.ems_signature_present);
    }

    /// Offset of the EMM device-header name within the F000 segment.
    ///
    /// Sits below [`Self::IVT_ENTRY_BASE`] (0x0100), so it never collides with
    /// an IVT stub entry point.
    pub const EMS_SIGNATURE_OFFSET: u16 = 0x000A;

    /// Clear the EMS driver name because expanded memory is unavailable.
    pub fn set_ems_driver_signature(memory: &mut Memory, _present: bool) {
        let addr = Memory::seg_offset_to_linear(0xF000, Self::EMS_SIGNATURE_OFFSET);
        // Zeros, not spaces: an all-zero name matches no driver, and a program
        // scanning device headers sees an empty field rather than a plausible
        // one.
        let bytes = &[0u8; 8];
        for (i, &byte) in bytes.iter().enumerate() {
            memory.write_rom(addr + i, byte);
        }
    }

    /// Install assembler keyboard stubs at F000:F800-F8FF.
    ///
    /// These are real x86 machine code that executes in the interpreter,
    /// providing IRQ delivery windows that zero-cycle HLE handlers cannot.
    /// The HLE dispatch early-out at F800-F8FF lets the interpreter run them.
    ///
    /// Stubs:
    /// - F800: INT 16h AH=00h blocking wait (dequeues from BDA, E0→00 fixup)
    /// - F840: INT 16h AH=10h blocking wait (dequeues from BDA, no fixup)
    /// - F880: INT 16h AH=01h non-blocking peek (RETF 2, E0→00 fixup)
    /// - F8B0: INT 16h AH=11h non-blocking peek (RETF 2, no fixup)
    /// - F8E0: INT 21h keyboard wait (loops until BDA has data, re-enters INT 21h)
    fn install_keyboard_stubs(memory: &mut Memory) {
        // Helper: write a byte slice to ROM at F000:offset.
        fn write_stub(memory: &mut Memory, offset: u16, bytes: &[u8]) {
            let base = Memory::seg_offset_to_linear(0xF000, offset);
            for (i, &b) in bytes.iter().enumerate() {
                memory.write_rom(base + i, b);
            }
        }

        // INT 16h AH=00h: Blocking wait loop with E0→00 fixup.
        // Modeled on IBM AT BIOS. STI+NOP opens IRQ window, CLI protects
        // the atomic check-and-dequeue from BDA.
        //
        //   .loop:
        //       STI / NOP / CLI
        //       PUSH DS / PUSH BX
        //       MOV BX, 0040h / MOV DS, BX
        //       MOV BX, [001Ah]        ; head
        //       CMP BX, [001Ch]        ; tail
        //       JNE .dequeue
        //       POP BX / POP DS
        //       JMP .loop
        //   .dequeue:
        //       MOV AX, [BX]           ; key word
        //       ADD BX, 2
        //       CMP BX, [0082h]        ; buffer end
        //       JB .no_wrap
        //       MOV BX, [0080h]        ; wrap to start
        //   .no_wrap:
        //       MOV [001Ah], BX        ; update head
        //       POP BX / POP DS
        //       CMP AL, 0E0h / JNE .ret / MOV AL, 0
        //   .ret:
        //       IRET
        #[rustfmt::skip]
        let int16_wait_00: &[u8] = &[
            // .loop: (offset 0)
            0xFB,                   // STI
            0x90,                   // NOP (STI shadow)
            0xFA,                   // CLI
            0x1E,                   // PUSH DS
            0x53,                   // PUSH BX
            0xBB, 0x40, 0x00,       // MOV BX, 0040h
            0x8E, 0xDB,             // MOV DS, BX
            0x8B, 0x1E, 0x1A, 0x00, // MOV BX, [001Ah]   (head)
            0x3B, 0x1E, 0x1C, 0x00, // CMP BX, [001Ch]   (tail)
            0x75, 0x04,             // JNE .dequeue       (+4 -> offset 24)
            // empty: pop and loop back
            0x5B,                   // POP BX
            0x1F,                   // POP DS
            0xEB, 0xE8,             // JMP .loop          (-24 -> offset 0)
            // .dequeue: (offset 24)
            0x8B, 0x07,             // MOV AX, [BX]
            0x83, 0xC3, 0x02,       // ADD BX, 2
            0x3B, 0x1E, 0x82, 0x00, // CMP BX, [0082h]   (buffer end)
            0x72, 0x04,             // JB .no_wrap        (+4 -> offset 39)
            0x8B, 0x1E, 0x80, 0x00, // MOV BX, [0080h]   (wrap to start)
            // .no_wrap: (offset 39)
            0x89, 0x1E, 0x1A, 0x00, // MOV [001Ah], BX   (update head)
            0x5B,                   // POP BX
            0x1F,                   // POP DS
            0x3C, 0xE0,             // CMP AL, 0E0h
            0x75, 0x02,             // JNE .ret           (+2 -> offset 51)
            0xB0, 0x00,             // MOV AL, 0
            // .ret: (offset 51)
            0xCF,                   // IRET
        ];
        write_stub(memory, Self::INT16_WAIT_00, int16_wait_00);

        // INT 16h AH=10h: Blocking wait loop, NO E0→00 fixup.
        // Identical to AH=00h except the dequeue path goes straight to IRET.
        #[rustfmt::skip]
        let int16_wait_10: &[u8] = &[
            // .loop: (offset 0)
            0xFB,                   // STI
            0x90,                   // NOP
            0xFA,                   // CLI
            0x1E,                   // PUSH DS
            0x53,                   // PUSH BX
            0xBB, 0x40, 0x00,       // MOV BX, 0040h
            0x8E, 0xDB,             // MOV DS, BX
            0x8B, 0x1E, 0x1A, 0x00, // MOV BX, [001Ah]
            0x3B, 0x1E, 0x1C, 0x00, // CMP BX, [001Ch]
            0x75, 0x04,             // JNE .dequeue       (+4 -> offset 24)
            0x5B,                   // POP BX
            0x1F,                   // POP DS
            0xEB, 0xE8,             // JMP .loop          (-24 -> offset 0)
            // .dequeue: (offset 24)
            0x8B, 0x07,             // MOV AX, [BX]
            0x83, 0xC3, 0x02,       // ADD BX, 2
            0x3B, 0x1E, 0x82, 0x00, // CMP BX, [0082h]
            0x72, 0x04,             // JB .no_wrap        (+4 -> offset 39)
            0x8B, 0x1E, 0x80, 0x00, // MOV BX, [0080h]
            // .no_wrap: (offset 39)
            0x89, 0x1E, 0x1A, 0x00, // MOV [001Ah], BX
            0x5B,                   // POP BX
            0x1F,                   // POP DS
            0xCF,                   // IRET (no E0 fixup)
        ];
        write_stub(memory, Self::INT16_WAIT_10, int16_wait_10);

        // INT 16h AH=01h: Non-blocking peek with E0→00 fixup.
        // Uses RETF 2 to discard saved FLAGS, preserving ZF from CMP/TEST.
        //
        //       STI
        //       PUSH DS / PUSH BX
        //       MOV BX, 0040h / MOV DS, BX
        //       MOV BX, [001Ah] / CMP BX, [001Ch]
        //       JE .empty
        //       MOV AX, [BX]          ; peek (don't dequeue)
        //       POP BX / POP DS
        //       CMP AL, 0E0h / JNE .zf0 / MOV AL, 0
        //   .zf0:
        //       OR BX, 1               ; ZF=0 unconditionally (see below)
        //       POP BX / POP DS        ; POP does not affect flags
        //       RETF 2
        //   .empty:
        //       POP BX / POP DS
        //       RETF 2                 ; ZF=1 from CMP head==tail
        //
        // MDD-BUG-KILN-00276: this used to clear ZF with `TEST SP, SP` on the
        // assumption that SP is never zero. It can be: a caller with SP=0006h
        // executing INT 16h has its IP/CS/FLAGS frame consume exactly those six
        // bytes, and the stub's balanced pushes and pops restore SP=0 right
        // before the test. ZF then came back SET with a key in AX, so the
        // caller saw "no key" — and since a peek does not dequeue, every retry
        // repeated the false answer.
        //
        // `OR BX, 1` is SP-independent and its result always has bit 0 set, so
        // ZF=0 holds for any input rather than for likely inputs. BX is ours
        // (we pushed it), and the pops that restore it do not affect flags, so
        // the fixup now happens BEFORE them and the caller's BX and DS are
        // still restored intact.
        #[rustfmt::skip]
        let int16_peek_01: &[u8] = &[
            0xFB,                   // STI
            0x1E,                   // PUSH DS
            0x53,                   // PUSH BX
            0xBB, 0x40, 0x00,       // MOV BX, 0040h
            0x8E, 0xDB,             // MOV DS, BX
            0x8B, 0x1E, 0x1A, 0x00, // MOV BX, [001Ah]
            0x3B, 0x1E, 0x1C, 0x00, // CMP BX, [001Ch]
            0x74, 0x10,             // JE .empty          (+16 -> offset 34)
            // key available:
            0x8B, 0x07,             // MOV AX, [BX]
            0x3C, 0xE0,             // CMP AL, 0E0h
            0x75, 0x02,             // JNE .zf0           (+2 -> offset 26)
            0xB0, 0x00,             // MOV AL, 0
            // .zf0: (offset 26)
            0x83, 0xCB, 0x01,       // OR BX, 1           (ZF=0, BX is ours)
            0x5B,                   // POP BX
            0x1F,                   // POP DS
            0xCA, 0x02, 0x00,       // RETF 2
            // .empty: (offset 34)
            0x5B,                   // POP BX
            0x1F,                   // POP DS
            0xCA, 0x02, 0x00,       // RETF 2             (ZF=1)
        ];
        write_stub(memory, Self::INT16_PEEK_01, int16_peek_01);

        // INT 16h AH=11h: Non-blocking peek, NO E0→00 fixup.
        // Same as AH=01h, including the SP-independent ZF clear
        // (MDD-BUG-KILN-00276), but with no E0 fixup on the key path.
        #[rustfmt::skip]
        let int16_peek_11: &[u8] = &[
            0xFB,                   // STI
            0x1E,                   // PUSH DS
            0x53,                   // PUSH BX
            0xBB, 0x40, 0x00,       // MOV BX, 0040h
            0x8E, 0xDB,             // MOV DS, BX
            0x8B, 0x1E, 0x1A, 0x00, // MOV BX, [001Ah]
            0x3B, 0x1E, 0x1C, 0x00, // CMP BX, [001Ch]
            0x74, 0x0A,             // JE .empty          (+10 -> offset 28)
            // key available:
            0x8B, 0x07,             // MOV AX, [BX]
            0x83, 0xCB, 0x01,       // OR BX, 1           (ZF=0, BX is ours)
            0x5B,                   // POP BX
            0x1F,                   // POP DS
            0xCA, 0x02, 0x00,       // RETF 2
            // .empty: (offset 28)
            0x5B,                   // POP BX
            0x1F,                   // POP DS
            0xCA, 0x02, 0x00,       // RETF 2             (ZF=1)
        ];
        write_stub(memory, Self::INT16_PEEK_11, int16_peek_11);

        // INT 21h keyboard wait: loops until BDA has data, then re-enters INT 21h.
        // AH is preserved from the original call, so the HLE handler runs the
        // correct function (01h/07h/08h/0Ah) and finds the key in BDA.
        //
        //   .loop:
        //       STI / NOP / CLI
        //       PUSH DS / PUSH AX
        //       MOV AX, 0040h / MOV DS, AX
        //       MOV AX, [001Ah] / CMP AX, [001Ch]
        //       POP AX / POP DS
        //       JE .loop
        //       STI
        //       INT 21h
        //       IRET
        #[rustfmt::skip]
        let int21_kbd_wait: &[u8] = &[
            // .loop: (offset 0)
            0xFB,                   // STI
            0x90,                   // NOP (STI shadow)
            0xFA,                   // CLI
            0x1E,                   // PUSH DS
            0x50,                   // PUSH AX
            0xB8, 0x40, 0x00,       // MOV AX, 0040h
            0x8E, 0xD8,             // MOV DS, AX
            0xA1, 0x1A, 0x00,       // MOV AX, [001Ah]
            0x3B, 0x06, 0x1C, 0x00, // CMP AX, [001Ch]
            0x58,                   // POP AX
            0x1F,                   // POP DS
            0x74, 0xEB,             // JE .loop           (-21 -> offset 0)
            0xFB,                   // STI
            0xCD, 0x21,             // INT 21h
            0xCF,                   // IRET
        ];
        write_stub(memory, Self::INT21_KBD_WAIT, int21_kbd_wait);
    }

    fn write_rom_u16(memory: &mut Memory, addr: usize, value: u16) {
        let bytes = value.to_le_bytes();
        memory.write_rom(addr, bytes[0]);
        memory.write_rom(addr + 1, bytes[1]);
    }

    fn write_rom_u32(memory: &mut Memory, addr: usize, value: u32) {
        let bytes = value.to_le_bytes();
        for (i, byte) in bytes.iter().enumerate() {
            memory.write_rom(addr + i, *byte);
        }
    }

    fn write_rom_farptr(memory: &mut Memory, addr: usize, segment: u16, offset: u16) {
        Self::write_rom_u16(memory, addr, offset);
        Self::write_rom_u16(memory, addr + 2, segment);
    }

    fn font_8x14() -> &'static [u8] {
        static FONT: OnceLock<Vec<u8>> = OnceLock::new();
        FONT.get_or_init(|| {
            let mut font = vec![0u8; 256 * 14];
            for ch in 0..256 {
                let src = &gp64_hw::vga::CP437_FONT_8X16[ch * 16..ch * 16 + 16];
                let dst = &mut font[ch * 14..ch * 14 + 14];
                dst.copy_from_slice(&src[1..15]);
            }
            font
        })
    }

    fn init_rom_video_tables(memory: &mut Memory) {
        let base_func = Memory::seg_offset_to_linear(0xF000, Self::ROM_FUNCTIONALITY_TABLE_OFFSET);
        for (i, &byte) in ROM_FUNCTIONALITY_TABLE.iter().enumerate() {
            memory.write_rom(base_func + i, byte);
        }

        let base_vpt = Memory::seg_offset_to_linear(0xF000, Self::ROM_VIDEO_PARAM_TABLE_OFFSET);
        for (i, &byte) in ROM_VIDEO_PARAM_TABLE.iter().enumerate() {
            memory.write_rom(base_vpt + i, byte);
        }

        let base_dcc = Memory::seg_offset_to_linear(0xF000, Self::ROM_VIDEO_DCC_TABLE_OFFSET);
        for (i, &byte) in ROM_VIDEO_DCC_TABLE.iter().enumerate() {
            memory.write_rom(base_dcc + i, byte);
        }

        let base_save_table =
            Memory::seg_offset_to_linear(0xF000, Self::ROM_VIDEO_SAVE_POINTER_TABLE_OFFSET);
        Self::write_rom_u16(memory, base_save_table, 0x001A);
        let mut cursor = base_save_table + 2;
        Self::write_rom_farptr(memory, cursor, 0xF000, Self::ROM_VIDEO_DCC_TABLE_OFFSET);
        cursor += 4;
        for _ in 0..5 {
            Self::write_rom_u32(memory, cursor, 0);
            cursor += 4;
        }

        let base_pointers =
            Memory::seg_offset_to_linear(0xF000, Self::ROM_VIDEO_SAVE_POINTERS_OFFSET);
        let mut cursor = base_pointers;
        Self::write_rom_farptr(memory, cursor, 0xF000, Self::ROM_VIDEO_PARAM_TABLE_OFFSET);
        cursor += 4;
        for _ in 0..3 {
            Self::write_rom_u32(memory, cursor, 0);
            cursor += 4;
        }
        Self::write_rom_farptr(
            memory,
            cursor,
            0xF000,
            Self::ROM_VIDEO_SAVE_POINTER_TABLE_OFFSET,
        );
        cursor += 4;
        for _ in 0..2 {
            Self::write_rom_u32(memory, cursor, 0);
            cursor += 4;
        }
    }

    /// Initialize font data in BIOS ROM area
    fn init_rom_fonts(memory: &mut Memory) {
        use gp64_hw::vga::{CP437_FONT_8X16, CP437_FONT_8X8};

        // Write 8x8 font to ROM at F000:E000
        let base_8x8 = Memory::seg_offset_to_linear(0xF000, Self::ROM_FONT_8X8_OFFSET);
        for (i, &byte) in CP437_FONT_8X8.iter().enumerate() {
            memory.write_rom(base_8x8 + i, byte);
        }

        // Write 8x16 font to ROM at F000:E800
        let base_8x16 = Memory::seg_offset_to_linear(0xF000, Self::ROM_FONT_8X16_OFFSET);
        for (i, &byte) in CP437_FONT_8X16.iter().enumerate() {
            memory.write_rom(base_8x16 + i, byte);
        }

        // Write 8x14 font to ROM at F000:D000
        let base_8x14 = Memory::seg_offset_to_linear(0xF000, Self::ROM_FONT_8X14_OFFSET);
        for (i, &byte) in Self::font_8x14().iter().enumerate() {
            memory.write_rom(base_8x14 + i, byte);
        }

        // Mirror lower 128 chars of 8x8 font to the legacy BIOS location.
        let base_8x8_legacy =
            Memory::seg_offset_to_linear(0xF000, Self::ROM_FONT_8X8_LEGACY_OFFSET);
        for (i, &byte) in CP437_FONT_8X8[..1024].iter().enumerate() {
            memory.write_rom(base_8x8_legacy + i, byte);
        }

        // Set up INT 1Fh to point to upper 128 chars of 8x8 font (chars 128-255)
        // The font for chars 128-255 starts at offset 128*8 = 1024
        let ivt_1f_addr = 0x1F * 4;
        memory.write_u16(ivt_1f_addr, Self::ROM_FONT_8X8_OFFSET + 1024);
        memory.write_u16(ivt_1f_addr + 2, 0xF000);

        // Set up INT 43h to point to 8x8 font (full 256 chars)
        let ivt_43_addr = 0x43 * 4;
        memory.write_u16(ivt_43_addr, Self::ROM_FONT_8X8_OFFSET);
        memory.write_u16(ivt_43_addr + 2, 0xF000);
    }

    /// Increment tick count (called by timer interrupt)
    pub fn tick(&mut self) -> bool {
        self.tick_count = self.tick_count.wrapping_add(1);
        if self.tick_count >= BIOS_TICKS_PER_DAY {
            self.tick_count = 0;
            return true;
        }
        false
    }

    /// An INT 15h AH=83h flag write still needs instruction-boundary polling.
    pub fn event_wait_active(&self) -> bool {
        self.event_wait_active
    }

    /// Tick the INT 15h AH=83h event wait timer.
    ///
    /// Call from the emulator loop with elapsed microseconds. When the
    /// interval expires, bit 7 of the flag byte (at the address given
    /// by the original INT 15h call) is set in memory.
    pub fn tick_event_wait(&mut self, elapsed_us: u64, memory: &mut Memory) {
        if !self.event_wait_active {
            return;
        }
        if elapsed_us >= self.event_wait_remaining_us {
            // Interval expired - set bit 7 of the flag byte
            let current = memory.read_u8(self.event_wait_flag_addr);
            memory.write_u8(self.event_wait_flag_addr, current | 0x80);
            self.event_wait_active = false;
        } else {
            self.event_wait_remaining_us -= elapsed_us;
        }
    }
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day)
#[cfg(test)]
fn days_to_ymd(days: u64) -> (u32, u8, u8) {
    // Algorithm based on Howard Hinnant's date algorithms
    // https://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719_468; // Shift epoch from 1970-01-01 to 0000-03-01
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year of era [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month offset [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u8, d as u8)
}

#[cfg(test)]
fn to_bcd(val: u8) -> u8 {
    ((val / 10) << 4) | (val % 10)
}

impl Default for Bios {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
