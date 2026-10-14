//! INT 10h - Video BIOS services

use anyhow::Result;
use gp64_cpu::Cpu;
use gp64_hw::vga::{font_bank_offset, ScrollRegion, Vga, DEFAULT_TEXT_ATTRIBUTE};
use gp64_memory::Memory;

use super::{Bios, ROM_VIDEO_PARAM_TABLE};

const CGA_MODE_CONTROL_TABLE_OFFSET: usize = 80;

/// Display Combination Code (DCC) table.
///
/// Each entry encodes: `(alternate_display << 8) | active_display`
/// so that `set_bx(entry)` yields BL=active, BH=alternate per the
/// VGA BIOS specification (INT 10h AX=1A00h).
///
/// Display codes: 0x00=none, 0x01=MDA, 0x02=CGA, 0x04=EGA color,
/// 0x05=EGA mono, 0x06=PGA, 0x07=VGA mono, 0x08=VGA color.
const DCC_ENTRIES: [u16; 16] = [
    0x0000, 0x0001, 0x0002, 0x0201, 0x0004, 0x0401, 0x0005, 0x0205, 0x0006, 0x0106, 0x0506, 0x0008,
    0x0108, 0x0007, 0x0207, 0x0607,
];

impl Bios {
    /// stdvga VRAM addressing ratio for a legacy BIOS mode.
    ///
    /// | memory model | ratio |
    /// |---|---|
    /// | text | 2 |
    /// | CGA | 4 / depth (depth 1 -> 4, depth 2 -> 2) |
    /// | planar | 4 |
    /// | packed | 1 |
    ///
    /// A CRTC display-start word is `byte_offset * ratio / 4`. Used by
    /// MDD-BUG-CRUCIBLE-00427: the page-select path previously assumed the
    /// ratio was 2 for every graphics mode.
    fn stdvga_vram_ratio(mode: u8) -> u8 {
        match mode {
            // Text: 40x25 and 80x25, colour and mono.
            0x00 | 0x01 | 0x02 | 0x03 | 0x07 => 2,
            // CGA 320x200 4-colour: depth 2 -> ratio 2.
            0x04 | 0x05 => 2,
            // CGA 640x200 2-colour: depth 1 -> ratio 4.
            0x06 => 4,
            // EGA/VGA planar (incl. the 1bpp mono/planar modes).
            0x0D..=0x12 => 4,
            // Mode 13h, packed 8bpp.
            0x13 => 1,
            // Unknown/SVGA: the ROM's DISPLAY_START_WORD falls back to the
            // word-addressing assumption when it has no mode entry, i.e.
            // byte/2, which is ratio 2.
            _ => 2,
        }
    }

    fn set_cursor_position(vga: &mut Vga, memory: &mut Memory, page: u8, row: u8, col: u8) {
        let page = vga.normalize_cursor_page(page) as u8;
        vga.set_cursor(page, row, col);
        memory.write_u16(0x450 + page as usize * 2, ((row as u16) << 8) | col as u16);
    }

    fn scroll_region_from_bda(cpu: &Cpu, memory: &Memory) -> ScrollRegion {
        let max_right = memory.read_u16(0x44A).wrapping_sub(1).min(u8::MAX as u16) as u8;
        let max_bottom = memory.read_u8(0x484);

        ScrollRegion {
            top: cpu.regs.ch(),
            left: cpu.regs.cl(),
            bottom: cpu.regs.dh().min(max_bottom),
            right: cpu.regs.dl().min(max_right),
        }
    }

    /// INT 10h - Video BIOS
    pub fn int10(&mut self, cpu: &mut Cpu, memory: &mut Memory, vga: &mut Vga) -> Result<()> {
        let ah = cpu.regs.ah();

        // EXP-N007b (unit E): HLE INT 10h mutates raster-live VGA state
        // (palette, DAC, fonts, mode) without traversing the port decoder,
        // so the deferred-write latch (vga::latch_or_apply_raster_write)
        // cannot see it. Apply the same render-before-mutate contract here:
        // catch the raster up to the event grid BEFORE any raster-mutating
        // service runs. Over-triggering is a cheap no-op (the catch-up
        // returns immediately in vblank or when the current grid line is
        // already rendered).
        if matches!(ah, 0x00 | 0x0B | 0x10 | 0x11 | 0x12 | 0x1C | 0x4F) {
            vga.render_catch_up_to_grid(cpu.cycles, memory);
        }

        tracing::debug!(
            ah = format_args!("{:02X}", ah),
            al = format_args!("{:02X}", cpu.regs.al()),
            bx = format_args!("{:04X}", cpu.regs.bx()),
            cx = format_args!("{:04X}", cpu.regs.cx()),
            dx = format_args!("{:04X}", cpu.regs.dx()),
            "INT 10h"
        );
        if matches!(ah, 0x00 | 0x0F) {
            tracing::debug!(
                ah = format_args!("{:02X}", ah),
                al = format_args!("{:02X}", cpu.regs.al()),
                cs = format_args!("{:04X}", cpu.regs.cs),
                ip = format_args!("{:04X}", cpu.regs.ip()),
                "INT 10h mode"
            );
        }

        match ah {
            // Set video mode
            0x00 => {
                let raw_mode = cpu.regs.al();
                let mode = raw_mode & 0x7F;
                // Debug: trace INT 10h mode sets
                {
                    // Read caller's CS:IP from stack (pushed by INT instruction)
                    let sp = cpu.regs.sp();
                    let ss = cpu.regs.ss;
                    let ret_ip = memory.read_u16((((ss as u32) << 4) + sp as u32) as usize);
                    let ret_cs = memory.read_u16((((ss as u32) << 4) + sp as u32 + 2) as usize);
                    tracing::debug!(
                        al = format_args!("{:02X}", raw_mode),
                        mode = format_args!("{:02X}", mode),
                        caller = format_args!("{:04X}:{:04X}", ret_cs, ret_ip),
                        "INT 10h AH=00 set video mode"
                    );
                }
                // A mode number this adapter has no entry for is REFUSED, and
                // nothing at all changes — not the registers, not the active
                // SVGA mode, not BDA 0x449. That is what a real VGA BIOS does
                // and what the DOS model does (`INT10_SetVideoMode` returns false
                // from the failed mode-list lookup before it programs anything).
                //
                // Recording an unsupported number in BDA 0x449 is the part that
                // actively misleads a guest: the classic SVGA probe is
                // `AX=0x005F / INT 10h` then `AH=0x0F / INT 10h` and compare AL,
                // and echoing the request back claims we support every vendor
                // mode number in existence (MDD-BUG-KILN-00641).
                if !Vga::is_supported_bios_mode(mode) {
                    tracing::debug!(
                        mode = format_args!("{:02X}", mode),
                        "INT 10h AH=00: no such video mode — refused, video state unchanged"
                    );
                    return Ok(());
                }
                self.set_standard_video_mode(memory, vga, raw_mode);
            }

            // Set cursor shape (with CGA cursor emulation)
            0x01 => {
                let start = cpu.regs.ch();
                let end = cpu.regs.cl();
                // Store original (unemulated) values in BDA 0x460
                memory.write_u16(0x460, ((start as u16) << 8) | end as u16);

                // CGA cursor emulation: when the character cell is taller than
                // 8 scanlines and the requested end scanline is in the CGA
                // range, scale the low 5 CRTC scanline bits so the cursor
                // appears at the equivalent position in the larger cell.  Real
                // VGA BIOS does this unless bit 0 of BDA 0x487 is set
                // (emulation disabled) or CH bit 5 requests the raw/off path.
                let char_height = memory.read_u8(0x485);
                let emulation_disabled = (memory.read_u8(0x487) & 0x01) != 0;
                let start_scan = start & 0x1F;
                let end_scan = end & 0x1F;
                let (hw_start, hw_end) = if !emulation_disabled
                    && char_height > 8
                    && (start & 0x20) == 0
                    && end_scan < 8
                {
                    let h = char_height as u16;
                    let e = (((end_scan as u16 + 1) * h) / 8 - 1) as u8;
                    let s = if end_scan as u16 == start_scan as u16 + 1 {
                        e.wrapping_sub(1)
                    } else {
                        (((start_scan as u16 + 1) * h) / 8 - 1) as u8
                    };
                    (s, e)
                } else {
                    (start, end)
                };
                vga.set_cursor_shape(hw_start, hw_end);
            }

            // Set cursor position
            0x02 => {
                let page = cpu.regs.bh();
                let row = cpu.regs.dh();
                let col = cpu.regs.dl();
                Self::set_cursor_position(vga, memory, page, row, col);
            }

            // Get cursor position
            0x03 => {
                let page = cpu.regs.bh();
                let (row, col) = vga.get_cursor(page);
                cpu.regs.set_dh(row);
                cpu.regs.set_dl(col);
                let cursor_shape = memory.read_u16(0x460);
                cpu.regs.set_ch((cursor_shape >> 8) as u8);
                cpu.regs.set_cl((cursor_shape & 0xFF) as u8);
            }

            // Read light pen (obsolete)
            0x04 => {
                cpu.regs.set_ax(0);
                cpu.regs.set_bx(0);
                cpu.regs.set_cx(0);
                cpu.regs.set_dx(0);
            }

            // Select display page
            0x05 => {
                let page = cpu.regs.al();
                vga.set_active_page(page);

                // Keep BDA in sync for programs that use it for direct screen writes.
                memory.write_u8(0x462, page);
                let page_size_bytes = memory.read_u16(0x44C);
                // MDD-BUG-CRUCIBLE-00427. BDA 0x44E is the page's BYTE offset
                // which stores `page * [0x044C]` with no division). We used to
                // halve it and publish a word offset, then reuse that same
                // halved value as the CRTC start for EVERY graphics mode —
                // correct only where the mode's addressing ratio happens to
                // be 2. The CRTC start is now derived separately, from the
                // mode's ratio, exactly as `DISPLAY_START_WORD` does.
                let start_bytes_full = page as usize * page_size_bytes as usize;
                // 16-bit BDA field: the ROM stores AX from a `mul word`, so a
                // wider product truncates rather than saturating.
                memory.write_u16(0x44E, start_bytes_full as u16);

                let (mode, _, _) = vga.get_mode();
                let is_text_mode = matches!(mode, 0x00 | 0x01 | 0x02 | 0x03 | 0x07);
                if !is_text_mode {
                    // start_word = byte_offset * vram_ratio / 4 — the ROM's
                    // DISPLAY_START_WORD, computed on the untruncated product.
                    let start_words = start_bytes_full * Self::stdvga_vram_ratio(mode) as usize / 4;
                    vga.set_crtc_start_words(start_words);
                }
            }

            // Scroll up
            0x06 => {
                let lines = cpu.regs.al();
                let attr = cpu.regs.bh();
                let region = Self::scroll_region_from_bda(cpu, memory);
                vga.scroll_up(lines, attr, region, memory);
            }

            // Scroll down
            0x07 => {
                let lines = cpu.regs.al();
                let attr = cpu.regs.bh();
                let region = Self::scroll_region_from_bda(cpu, memory);
                vga.scroll_down(lines, attr, region, memory);
            }

            // Read character and attribute
            0x08 => {
                let (ch, attr) = vga.read_char_attr(cpu.regs.bh(), memory);
                cpu.regs.set_al(ch);
                cpu.regs.set_ah(attr);
            }

            // Write character and attribute
            0x09 => {
                let ch = cpu.regs.al();
                let attr = cpu.regs.bl();
                let count = cpu.regs.cx();
                vga.write_char_attr(cpu.regs.bh(), ch, attr, count, memory);
            }

            // Write character only
            0x0A => {
                let ch = cpu.regs.al();
                let count = cpu.regs.cx();
                vga.write_char(cpu.regs.bh(), ch, count, memory);
            }

            // Set palette register (CGA/EGA/VGA)
            0x0B => {
                let bh = cpu.regs.bh();
                let bl = cpu.regs.bl();
                if bh == 0x00 {
                    // INT 10h AH=0Bh programs the attribute controller
                    // in every mode.  Backgrounds 8-15 use the bright bank in
                    // AC0, while BL bit 4 controls intensity on AC1-AC3.
                    let mut attr = vga.get_attribute_regs();
                    let mut background = bl & 0x0F;
                    if background & 0x08 != 0 {
                        background += 0x08;
                    }
                    attr[0] = background;
                    for value in &mut attr[1..=3] {
                        *value = (*value & !0x10) | (bl & 0x10);
                    }
                    vga.set_attribute_regs(attr);
                } else if bh == 0x01 {
                    // BH=01 selects the CGA palette by changing bit 0 on
                    // attribute registers 1-3.  BH>=02 is a no-op.
                    let mut attr = vga.get_attribute_regs();
                    for value in &mut attr[1..=3] {
                        *value = (*value & !0x01) | (bl & 0x01);
                    }
                    vga.set_attribute_regs(attr);
                }
            }

            // Write pixel
            0x0C => {
                let color = cpu.regs.al();
                let page = cpu.regs.bh();
                let x = cpu.regs.cx();
                let y = cpu.regs.dx();

                // Debug: trace pixel writes
                let trace = tracing::enabled!(target: "gp64::bios::pixel", tracing::Level::TRACE);
                if trace {
                    static PX_COUNT: std::sync::atomic::AtomicUsize =
                        std::sync::atomic::AtomicUsize::new(0);
                    let count = PX_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if count < 100 {
                        tracing::trace!(
                            count,
                            x,
                            y,
                            color = format_args!("{:02X}", color),
                            "INT 10h write pixel"
                        );
                    }
                }

                vga.write_pixel(page, x, y, color, memory);
            }

            // Read pixel
            0x0D => {
                let page = cpu.regs.bh();
                let x = cpu.regs.cx();
                let y = cpu.regs.dx();
                cpu.regs.set_al(vga.read_pixel(page, x, y, memory));
            }

            // Teletype output
            0x0E => {
                let ch = cpu.regs.al();
                // MDD-BUG-CRUCIBLE-00598: normalize the page ONCE and use the
                // same value for the VGA operation and the BDA slot address.
                // `Vga::set_cursor`/`teletype` fold an out-of-range page onto
                // page 0, so using the raw `BH` for the BDA address wrote
                // outside the eight-word cursor table at 0x450: BH=8 lands on
                // 0x460 (the cursor SHAPE word), BH=9 on 0x462 (active page
                // plus the low byte of the CRTC base), and higher values
                // further still — VGA page 0 moved while unrelated BDA state
                // was corrupted.
                let page = vga.normalize_cursor_page(cpu.regs.bh()) as u8;
                vga.teletype_with_color(ch, page, cpu.regs.bl(), memory);

                // Update BDA cursor position for the page
                let (row, col) = vga.get_cursor(page);
                let bda_cursor_addr = 0x450 + (page as usize * 2);
                memory.write_u8(bda_cursor_addr, col);
                memory.write_u8(bda_cursor_addr + 1, row);
            }

            // Get video mode
            0x0F => {
                let (_mode, cols, page) = vga.get_mode();
                // MDD-BUG-CRUCIBLE-00424: recompose, don't echo.
                // INT 10h AH=0Fh returns `BDA[0x449] | (BDA[0x487] & 0x80)`;
                // 0x449 is masked and the no-clear flag comes back from the
                // video-control byte. The old code echoed 0x449 whole, which
                // happened to return the caller's value only because 0x449 was
                // itself storing the unmasked byte.
                let bda_mode = memory.read_u8(0x449) | (memory.read_u8(0x487) & 0x80);
                cpu.regs.set_al(bda_mode);
                cpu.regs.set_ah(cols);
                cpu.regs.set_bh(page);
            }

            // Set palette registers (VGA)
            0x10 => {
                let al = cpu.regs.al();
                match al {
                    0x00 => {
                        // Set individual palette register
                        let reg = cpu.regs.bl();
                        let val = cpu.regs.bh();
                        vga.set_palette_reg(reg, val);
                    }
                    0x01 => {
                        // Set border color
                        let val = cpu.regs.bh();
                        let mut attr = vga.get_attribute_regs();
                        debug_assert!(attr.len() > 0x11, "VGA attr regs must have >0x11 entries");
                        attr[0x11] = val;
                        vga.set_attribute_regs(attr);
                    }
                    0x02 => {
                        // Set all palette registers and border
                        let table_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.dx());
                        for i in 0..16 {
                            let val = memory.read_u8(table_addr + i);
                            vga.set_palette_reg(i as u8, val);
                        }
                        let border = memory.read_u8(table_addr + 16);
                        let mut attr = vga.get_attribute_regs();
                        debug_assert!(attr.len() > 0x11, "VGA attr regs must have >0x11 entries");
                        attr[0x11] = border;
                        vga.set_attribute_regs(attr);
                    }
                    0x03 => {
                        let state = cpu.regs.bl();
                        Self::toggle_blinking(memory, vga, state);
                    }
                    0x07 => {
                        let reg = cpu.regs.bl() as usize;
                        let regs = vga.get_palette_regs();
                        let val = regs.get(reg).copied().unwrap_or(0);
                        cpu.regs.set_bh(val);
                    }
                    0x08 => {
                        let attr = vga.get_attribute_regs();
                        let val = attr.get(0x11).copied().unwrap_or(0);
                        cpu.regs.set_bh(val);
                    }
                    0x09 => {
                        let table_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.dx());
                        let regs = vga.get_palette_regs();
                        #[allow(clippy::needless_range_loop)]
                        for i in 0..16 {
                            memory.write_u8(table_addr + i, regs[i]);
                        }
                        let attr = vga.get_attribute_regs();
                        let border = attr.get(0x11).copied().unwrap_or(0);
                        memory.write_u8(table_addr + 16, border);
                    }
                    0x10 => {
                        // Set individual DAC register - BL contains register number (0-255)
                        let reg = cpu.regs.bl() as u16;
                        let mut r = cpu.regs.dh() & 0x3F;
                        let mut g = cpu.regs.ch() & 0x3F;
                        let mut b = cpu.regs.cl() & 0x3F;
                        if Self::grayscale_enabled(memory) {
                            (r, g, b) = Self::grayscale_triplet(r, g, b);
                        }
                        vga.set_dac_reg(reg, r, g, b);
                    }
                    0x12 => {
                        // Set block of DAC registers
                        let start = cpu.regs.bx();
                        let count = cpu.regs.cx();
                        let addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.dx());
                        {
                            let mut sample = [0u8; 9];
                            let sample_len = (count as usize).saturating_mul(3).min(sample.len());
                            for (i, sample_byte) in sample.iter_mut().enumerate().take(sample_len) {
                                *sample_byte = memory.read_u8(addr + i);
                            }
                            tracing::trace!(
                                start = format_args!("{:04X}", start),
                                count,
                                es = format_args!("{:04X}", cpu.regs.es),
                                dx = format_args!("{:04X}", cpu.regs.dx()),
                                sample = format_args!("{:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                                    sample[0], sample[1], sample[2], sample[3], sample[4],
                                    sample[5], sample[6], sample[7], sample[8]),
                                "DAC set block"
                            );
                        }
                        for i in 0..count {
                            let mut r = memory.read_u8(addr + (i as usize) * 3) & 0x3F;
                            let mut g = memory.read_u8(addr + (i as usize) * 3 + 1) & 0x3F;
                            let mut b = memory.read_u8(addr + (i as usize) * 3 + 2) & 0x3F;
                            if Self::grayscale_enabled(memory) {
                                (r, g, b) = Self::grayscale_triplet(r, g, b);
                            }
                            vga.set_dac_reg(start.wrapping_add(i), r, g, b);
                        }
                    }
                    0x13 => {
                        let function = cpu.regs.bl();
                        let mode = cpu.regs.bh();
                        Self::select_dac_page(vga, function, mode);
                    }
                    0x15 => {
                        // Read DAC register - BL contains register number (0-255)
                        let reg = cpu.regs.bl() as usize;
                        let entries = vga.get_dac_entries();
                        let (r, g, b) = entries[reg];
                        cpu.regs.set_dh(r >> 2);
                        cpu.regs.set_ch(g >> 2);
                        cpu.regs.set_cl(b >> 2);
                    }
                    0x17 => {
                        let start = cpu.regs.bx() as usize;
                        let count = cpu.regs.cx();
                        let addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.dx());
                        let entries = vga.get_dac_entries();
                        for i in 0..count as usize {
                            let idx = start + i;
                            let (r, g, b) = entries.get(idx).copied().unwrap_or((0, 0, 0));
                            let base = addr + i * 3;
                            memory.write_u8(base, r >> 2);
                            memory.write_u8(base + 1, g >> 2);
                            memory.write_u8(base + 2, b >> 2);
                        }
                    }
                    0x18 => {
                        vga.set_pel_mask(cpu.regs.bl());
                    }
                    0x19 => {
                        cpu.regs.set_bl(vga.get_pel_mask());
                        cpu.regs.set_bh(0);
                    }
                    0x1A => {
                        let (mode, page) = Self::get_dac_page(vga);
                        cpu.regs.set_bh(mode);
                        cpu.regs.set_bl(page);
                    }
                    0x1B => {
                        let start = cpu.regs.bx();
                        let count = cpu.regs.cx();
                        Self::perform_grayscale_summing(vga, start, count);
                    }
                    _ => {}
                }
            }

            // Character generator
            0x11 => {
                let al = cpu.regs.al();
                match al {
                    // Load user-specified patterns to plane 2
                    0x00 | 0x10 => {
                        let char_height = cpu.regs.bh() as usize;
                        let count = cpu.regs.cx();
                        let start = cpu.regs.dx();
                        let block = cpu.regs.bl();
                        let addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bp());
                        Self::load_user_font_to_plane2(
                            memory,
                            addr,
                            count,
                            char_height,
                            block,
                            start,
                        );
                        Self::update_text_metrics_for_font(memory, vga, char_height, al == 0x10);
                        vga.custom_font_loaded = true;
                        tracing::debug!(
                            al = format_args!("{:02X}", al),
                            char_height,
                            "INT 10h AH=11h: Loaded user font"
                        );
                    }
                    // Load 8x8 font to plane 2
                    0x02 | 0x12 => {
                        let block = cpu.regs.bl();
                        // Load ROM 8x8 double-dot font
                        Self::load_font_to_plane2_block(
                            memory,
                            &gp64_hw::vga::CP437_FONT_8X8,
                            8,
                            block,
                            0,
                            256,
                        );
                        Self::update_text_metrics_for_font(memory, vga, 8, al == 0x12);
                        vga.custom_font_loaded = true;
                        tracing::debug!(
                            al = format_args!("{:02X}", al),
                            "INT 10h AH=11h: Loaded 8x8 font to plane 2"
                        );
                    }
                    // Load 8x14 font to plane 2
                    0x01 | 0x11 => {
                        let block = cpu.regs.bl();
                        Self::load_font_to_plane2_block(
                            memory,
                            Self::font_8x14(),
                            14,
                            block,
                            0,
                            256,
                        );
                        Self::update_text_metrics_for_font(memory, vga, 14, al == 0x11);
                        vga.custom_font_loaded = true;
                        tracing::debug!(
                            al = format_args!("{:02X}", al),
                            "INT 10h AH=11h: Loaded 8x14 font to plane 2"
                        );
                    }
                    // Set block specifier
                    0x03 => {
                        vga.set_font_block_spec(cpu.regs.bl());
                    }
                    // Load 8x16 font to plane 2
                    0x04 | 0x14 => {
                        let block = cpu.regs.bl();
                        Self::load_font_to_plane2_block(
                            memory,
                            &gp64_hw::vga::CP437_FONT_8X16,
                            16,
                            block,
                            0,
                            256,
                        );
                        Self::update_text_metrics_for_font(memory, vga, 16, al == 0x14);
                        vga.custom_font_loaded = true;
                        tracing::debug!(
                            al = format_args!("{:02X}", al),
                            "INT 10h AH=11h: Loaded 8x16 font to plane 2"
                        );
                    }
                    // AL=20h-24h select the GRAPHICS-mode character tables.
                    // They only repoint the INT 1Fh / INT 43h vectors (and,
                    // for 21h-24h, the BDA height and row count); nothing is
                    // loaded into plane 2, which in a planar graphics mode is
                    // the red plane of the picture (MDD-BUG-KIL-00852). This
                    // S11_V20..S11_VROM.
                    0x20 => {
                        // Upper-half (80h-FFh) 8x8 table: IVT 1Fh only.
                        Self::set_font_vector(memory, 0x1F, cpu.regs.es, cpu.regs.bp());
                    }
                    0x21 => {
                        Self::set_font_vector(memory, 0x43, cpu.regs.es, cpu.regs.bp());
                        // CL only, zero-extended: the ROM stores a word from
                        // the low byte, so CX=012Ch leaves 2Ch in 0x485.
                        Self::set_graphics_font_metrics(
                            memory,
                            u16::from(cpu.regs.cl()),
                            cpu.regs.bl(),
                            cpu.regs.dl(),
                        );
                        if !vga.is_text_mode() {
                            vga.sync_text_layout(memory);
                        }
                    }
                    0x22..=0x24 => {
                        let (offset, height) = match al {
                            0x22 => (Self::ROM_FONT_8X14_OFFSET, 14),
                            0x23 => (Self::ROM_FONT_8X8_OFFSET, 8),
                            _ => (Self::ROM_FONT_8X16_OFFSET, 16),
                        };
                        Self::set_font_vector(memory, 0x43, 0xF000, offset);
                        Self::set_graphics_font_metrics(
                            memory,
                            height,
                            cpu.regs.bl(),
                            cpu.regs.dl(),
                        );
                        if !vga.is_text_mode() {
                            vga.sync_text_layout(memory);
                        }
                    }
                    // Get font info
                    0x30 => {
                        let bh = cpu.regs.bh();
                        let (segment, offset, bytes_per_char) = match bh {
                            0 => {
                                let ivt = 0x1F * 4;
                                (memory.read_u16(ivt + 2), memory.read_u16(ivt), 8)
                            }
                            1 => {
                                let ivt = 0x43 * 4;
                                (memory.read_u16(ivt + 2), memory.read_u16(ivt), 8)
                            }
                            2 | 5 => (0xF000, Self::ROM_FONT_8X14_OFFSET, 14),
                            4 => (0xF000, Self::ROM_FONT_8X8_OFFSET + 1024, 8),
                            6 | 7 => (0xF000, Self::ROM_FONT_8X16_OFFSET, 16),
                            // 3 and all other values use 8x8 font
                            _ => (0xF000, Self::ROM_FONT_8X8_OFFSET, 8),
                        };
                        // Through `load_data_segment`, so the real-mode ES
                        // cache follows the selector — a bare store leaves the
                        // guest's previous segment authoritative for `es:[bp]`
                        // (MDD-BUG-TEM-00067).
                        cpu.load_data_segment(memory, gp64_cpu::DataSegReg::ES, segment);
                        cpu.regs.set_bp(offset);
                        cpu.regs.set_cx(bytes_per_char);
                        cpu.regs.set_dl(memory.read_u8(0x484));
                    }
                    _ => {
                        tracing::debug!(
                            al = format_args!("{:02X}", al),
                            "INT 10h AH=11h unhandled subfunction"
                        );
                    }
                }
            }

            // Alternate function select
            0x12 => {
                let bl = cpu.regs.bl();
                match bl {
                    0x10 => {
                        // Get EGA info
                        let crtc = memory.read_u16(0x463);
                        cpu.regs.set_bh(u8::from(crtc == 0x3B4)); // Mono vs color
                        cpu.regs.set_bl(3); // 256K memory
                        let switches = memory.read_u8(0x488);
                        cpu.regs.set_cl(switches & 0x0F);
                        cpu.regs.set_ch(switches >> 4);
                    }
                    0x20 | 0x32 => {
                        cpu.regs.set_al(0x12);
                    }
                    0x30 => {
                        let al = cpu.regs.al();
                        if al > 2 {
                            cpu.regs.set_al(0);
                        } else {
                            let mut flags = memory.read_u8(0x489);
                            let mut switches = memory.read_u8(0x488) & 0xF0;
                            match al {
                                0 => {
                                    flags = (flags & 0xEF) | 0x80;
                                    switches |= 0x08;
                                }
                                1 => {
                                    flags &= 0x6F;
                                    switches |= 0x09;
                                }
                                2 => {
                                    flags = (flags & 0x6F) | 0x10;
                                    switches |= 0x09;
                                }
                                _ => {}
                            }
                            memory.write_u8(0x489, flags);
                            memory.write_u8(0x488, switches);
                            cpu.regs.set_al(0x12);
                        }
                    }
                    0x31 => {
                        let al = cpu.regs.al() & 1;
                        let mut flags = memory.read_u8(0x489) & 0xF7;
                        if al != 0 {
                            flags |= 0x08;
                        }
                        memory.write_u8(0x489, flags);
                        cpu.regs.set_al(0x12);
                    }
                    0x33 => {
                        let al = cpu.regs.al() & 1;
                        let mut flags = memory.read_u8(0x489) & 0xFD;
                        if al == 0 {
                            flags |= 0x02;
                        }
                        memory.write_u8(0x489, flags);
                        cpu.regs.set_al(0x12);
                    }
                    0x34 => {
                        let al = cpu.regs.al() & 1;
                        let mut ctl = memory.read_u8(0x487) & 0xFE;
                        ctl |= al;
                        memory.write_u8(0x487, ctl);
                        cpu.regs.set_al(0x12);
                    }
                    0x36 => {
                        let al = cpu.regs.al() & 1;
                        let mut regs = vga.get_sequencer_regs();
                        if al == 0 {
                            regs[1] &= !0x20;
                        } else {
                            regs[1] |= 0x20;
                        }
                        vga.set_sequencer_regs(regs);
                        cpu.regs.set_al(0x12);
                    }
                    _ => {}
                }
            }

            // Write string
            0x13 => {
                let mode = cpu.regs.al();
                let page = cpu.regs.bh();
                let attr = cpu.regs.bl();
                let count = cpu.regs.cx();
                let mut cur_row = cpu.regs.dh();
                let mut cur_col = cpu.regs.dl();
                let addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bp());
                let original_cursor = vga.get_cursor(page);

                let (_, columns, _) = vga.get_mode();
                let rows = memory.read_u8(0x484).saturating_add(1); // Rows from BDA (saturating to prevent wrap when 0xFF)

                for i in 0..count as usize {
                    let ch = memory.read_u8(addr + i * if mode & 2 != 0 { 2 } else { 1 });
                    let char_attr = if mode & 2 != 0 {
                        memory.read_u8(addr + i * 2 + 1)
                    } else {
                        attr
                    };
                    // Set cursor to the current write position. AH=13h uses
                    // the same control-character semantics as teletype output:
                    // BEL is ignored, BS moves left, LF moves down (including
                    // scroll), and CR returns to column zero.
                    vga.set_cursor(page, cur_row, cur_col);
                    if matches!(ch, 0x07 | 0x08 | 0x0A | 0x0D) {
                        vga.teletype(ch, page, memory);
                        (cur_row, cur_col) = vga.get_cursor(page);
                        continue;
                    }

                    // Printable characters use write_char_for_string so both
                    // foreground and background pixels are drawn in CGA modes,
                    // matching real VGA BIOS AH=13h behavior.
                    vga.write_char_for_string(page, ch, char_attr, memory);

                    // Advance write position with wrap handling
                    cur_col = cur_col.wrapping_add(1);
                    if cur_col >= columns || cur_col == 0 {
                        cur_col = 0;
                        cur_row = cur_row.saturating_add(1);
                        if cur_row >= rows {
                            // Scroll screen up
                            vga.scroll_up_page(
                                page,
                                1,
                                DEFAULT_TEXT_ATTRIBUTE,
                                gp64_hw::vga::ScrollRegion {
                                    top: 0,
                                    left: 0,
                                    bottom: rows - 1,
                                    right: columns - 1,
                                },
                                memory,
                            );
                            cur_row = rows - 1;
                        }
                    }
                }

                // Drawing temporarily moves VGA's cursor. Commit the final
                // position only when mode bit 0 is set; otherwise restore it.
                if mode & 1 != 0 {
                    Self::set_cursor_position(vga, memory, page, cur_row, cur_col);
                } else if count != 0 {
                    Self::set_cursor_position(
                        vga,
                        memory,
                        page,
                        original_cursor.0,
                        original_cursor.1,
                    );
                }
            }

            // VGA specific functions
            0x1A => {
                let al = cpu.regs.al();
                match al {
                    0x00 => {
                        let dcc = Self::current_dcc_code(memory);
                        cpu.regs.set_al(0x1A);
                        cpu.regs.set_bx(dcc);
                    }
                    0x01 => {
                        let dcc = cpu.regs.bx();
                        if Self::set_dcc_index(memory, dcc) {
                            cpu.regs.set_al(0x1A);
                        } else {
                            cpu.regs.set_al(0);
                        }
                    }
                    _ => {}
                }
            }

            // Get functionality/state info
            0x1B => {
                return self.int10_functionality_state(cpu, memory, vga);
            }

            // Video state save/restore
            0x1C => {
                return self.int10_video_state(cpu, memory, vga);
            }

            // EGA Register Interface Library
            0xF0 => {
                let val = Self::ega_ril_read_register(vga, cpu.regs.bl(), cpu.regs.dx());
                cpu.regs.set_bl(val);
            }
            0xF1 => {
                let reg = cpu.regs.bl();
                let value = cpu.regs.bh();
                Self::ega_ril_write_register(vga, reg, value, cpu.regs.dx());
                cpu.regs.set_bl(value);
            }
            0xF2 => {
                let start = cpu.regs.ch();
                let count = cpu.regs.cl();
                Self::ega_ril_read_range(cpu, memory, vga, start, count, cpu.regs.dx());
            }
            0xF3 => {
                let start = cpu.regs.ch();
                let count = cpu.regs.cl();
                Self::ega_ril_write_range(cpu, memory, vga, start, count, cpu.regs.dx());
            }
            0xF4 => {
                let count = cpu.regs.cx();
                Self::ega_ril_read_register_set(cpu, memory, vga, count);
            }
            0xF5 => {
                let count = cpu.regs.cx();
                Self::ega_ril_write_register_set(cpu, memory, vga, count);
            }
            0xF6 => {
                Self::ega_ril_revert_defaults(vga, cpu.regs.dx());
            }
            0xF7 => {
                Self::ega_ril_define_defaults(cpu, memory, vga);
            }
            0xFA => {
                if cpu.regs.bx() == 0 {
                    // See MDD-BUG-TEM-00067: the ES cache must follow the
                    // selector or `es:[bx]` resolves against the old segment.
                    cpu.load_data_segment(
                        memory,
                        gp64_cpu::DataSegReg::ES,
                        Self::VIDEO_ROM_SEGMENT,
                    );
                    cpu.regs.set_bx(Self::VIDEO_RIL_VERSION_OFFSET);
                } else {
                    cpu.regs.set_bx(0);
                }
            }

            // Hercules driver - Hercules detection
            0xEF => {
                // Return "not Hercules" - DL=FF, DH=FF
                cpu.regs.set_dx(0xFFFF);
            }

            // TopView/DESQview - Get shadow buffer
            0xFE => {
                // Return ES:DI unchanged - indicates no shadow buffer available
                // Nothing to do here
            }

            _ => {
                tracing::warn!(ah = format_args!("{:02X}", ah), "Unhandled INT 10h");
            }
        }

        Ok(())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn int10_functionality_state(
        &self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        vga: &mut Vga,
    ) -> Result<()> {
        if cpu.regs.bx() != 0 {
            cpu.regs.set_al(0);
            return Ok(());
        }

        let buffer_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.di());
        for i in 0..64usize {
            memory.write_u8(buffer_addr + i, 0);
        }

        memory.write_u16(buffer_addr, Self::ROM_FUNCTIONALITY_TABLE_OFFSET);
        memory.write_u16(buffer_addr + 2, 0xF000);

        memory.write_u8(buffer_addr + 0x04, memory.read_u8(0x449));
        memory.write_u16(buffer_addr + 0x05, memory.read_u16(0x44A));
        memory.write_u16(buffer_addr + 0x07, memory.read_u16(0x44C));
        memory.write_u16(buffer_addr + 0x09, memory.read_u16(0x44E));
        for page in 0..8usize {
            let pos = memory.read_u16(0x450 + page * 2);
            memory.write_u16(buffer_addr + 0x0B + page * 2, pos);
        }
        memory.write_u16(buffer_addr + 0x1B, memory.read_u16(0x460));
        memory.write_u8(buffer_addr + 0x1D, memory.read_u8(0x462));
        memory.write_u16(buffer_addr + 0x1E, memory.read_u16(0x463));
        memory.write_u8(buffer_addr + 0x20, vga.get_cga_mode_control());
        memory.write_u8(buffer_addr + 0x21, vga.get_cga_color_select());

        memory.write_u8(buffer_addr + 0x22, memory.read_u8(0x484));
        memory.write_u16(buffer_addr + 0x23, memory.read_u8(0x485) as u16);

        let dcc = Self::current_dcc_code(memory);
        // DCC entry is (alternate << 8) | active; state buffer stores active at +0x25
        memory.write_u8(buffer_addr + 0x25, (dcc & 0xFF) as u8); // active
        memory.write_u8(buffer_addr + 0x26, (dcc >> 8) as u8); // alternate

        let mode = memory.read_u8(0x449) & 0x7F;
        let num_colors = match mode {
            0x04 | 0x05 => 4,
            0x06 | 0x0F | 0x11 => 2,
            0x07 => 1,
            0x13 => 256,
            // All other modes (text, EGA/VGA 16-color) default to 16
            _ => 16,
        };
        memory.write_u16(buffer_addr + 0x27, num_colors);

        let page_size = memory.read_u16(0x44C);
        let pages = 0x8000u16.checked_div(page_size).unwrap_or(1).max(1);
        memory.write_u8(buffer_addr + 0x29, pages as u8);

        let scanlines = vga.dimensions().1 as u16;
        let scan_code = match scanlines {
            350 => 1,
            400 => 2,
            480 => 3,
            // 200 and other values default to 0
            _ => 0,
        };
        memory.write_u8(buffer_addr + 0x2A, scan_code);

        let font_spec = vga.get_font_block_spec();
        let (primary_block, secondary_block) = Self::font_blocks_from_spec(font_spec);
        memory.write_u8(buffer_addr + 0x2B, primary_block);
        memory.write_u8(buffer_addr + 0x2C, secondary_block);

        let mut misc_flags = 0u8;
        if Self::grayscale_enabled(memory) {
            misc_flags |= 0x02;
        }
        if memory.read_u16(0x463) == 0x3B4 {
            misc_flags |= 0x04;
        }
        if (memory.read_u8(0x489) & 0x08) != 0 {
            misc_flags |= 0x08;
        }
        if (memory.read_u8(0x487) & 0x01) != 0 {
            misc_flags |= 0x10;
        }
        let attr_regs = vga.get_attribute_regs();
        debug_assert!(
            attr_regs.len() > 0x10,
            "VGA attr regs must have >0x10 entries"
        );
        if (attr_regs[0x10] & 0x08) != 0 {
            misc_flags |= 0x20;
        }
        memory.write_u8(buffer_addr + 0x2D, misc_flags);
        memory.write_u8(buffer_addr + 0x2E, 0x00);
        memory.write_u8(buffer_addr + 0x31, 0x03);
        memory.write_u8(buffer_addr + 0x32, 0x00);
        memory.write_u8(buffer_addr + 0x33, 0x04);

        cpu.regs.set_al(0x1B);
        Ok(())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn int10_video_state(&self, cpu: &mut Cpu, memory: &mut Memory, vga: &mut Vga) -> Result<()> {
        let subfunction = cpu.regs.al();
        let state = cpu.regs.cx();

        match subfunction {
            0 => {
                let blocks = Self::video_state_blocks(state);
                if blocks == 0 {
                    cpu.regs.set_al(0);
                } else {
                    cpu.regs.set_al(0x1C);
                    cpu.regs.set_bx(blocks);
                }
            }
            1 if Self::video_state_save(state, cpu, memory, vga) => {
                cpu.regs.set_al(0x1C);
            }
            2 if Self::video_state_restore(state, cpu, memory, vga) => {
                cpu.regs.set_al(0x1C);
            }
            _ => {
                cpu.regs.set_al(0);
            }
        }

        Ok(())
    }

    /// Bytes each save-state component occupies.
    ///
    /// These are the SINGLE source of truth for the layout: the AH=1Ch size
    /// query, the VBE 4F04 size query, and both save and restore paths all
    /// derive their offsets from them, so a component can no longer be
    /// advertised at one size and written at another
    /// (`MDD-BUG-CRUCIBLE-00172`). `video_state_component_lengths_match_what_is_written`
    /// pins each constant against the address the corresponding writer
    /// actually reaches.
    ///
    pub(crate) const VIDEO_STATE_HW_LEN: usize = 0x46;
    pub(crate) const VIDEO_STATE_BDA_LEN: usize = 0x3A;
    pub(crate) const VIDEO_STATE_DAC_LEN: usize = 0x304;

    /// The AH=1Ch buffer opens with a pointer table locating each block.
    /// VBE 4F04 has no such header — its components are simply sequential —
    /// which is why the two share block bodies but not offsets.
    pub(crate) const VIDEO_STATE_HEADER_LEN: usize = 0x20;

    #[allow(clippy::verbose_bit_mask)]
    pub(crate) fn video_state_blocks(state: u16) -> u16 {
        if (state & 0x07) == 0 {
            return 0;
        }
        let size = Self::VIDEO_STATE_HEADER_LEN + Self::video_state_payload_len(state);
        (size.div_ceil(64)) as u16
    }

    /// Total bytes the selected AH=1Ch/4F04 components occupy, header excluded.
    pub(crate) fn video_state_payload_len(state: u16) -> usize {
        let mut size = 0;
        if (state & 0x01) != 0 {
            size += Self::VIDEO_STATE_HW_LEN;
        }
        if (state & 0x02) != 0 {
            size += Self::VIDEO_STATE_BDA_LEN;
        }
        if (state & 0x04) != 0 {
            size += Self::VIDEO_STATE_DAC_LEN;
        }
        size
    }

    #[allow(clippy::verbose_bit_mask)]
    pub(crate) fn video_state_save(
        state: u16,
        cpu: &Cpu,
        memory: &mut Memory,
        vga: &mut Vga,
    ) -> bool {
        if (state & 0x07) == 0 {
            return false;
        }

        let buffer_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        for i in 0..0x20usize {
            memory.write_u8(buffer_addr + i, 0);
        }

        let mut base = buffer_addr + 0x20;

        if (state & 0x01) != 0 {
            memory.write_u16(buffer_addr, (base - buffer_addr) as u16);
            Self::save_hw_block(memory, vga, base);
            base += Self::VIDEO_STATE_HW_LEN;
        }

        if (state & 0x02) != 0 {
            memory.write_u16(buffer_addr + 2, (base - buffer_addr) as u16);
            Self::save_bda_block(memory, base);
            base += Self::VIDEO_STATE_BDA_LEN;
        }

        if (state & 0x04) != 0 {
            memory.write_u16(buffer_addr + 4, (base - buffer_addr) as u16);
            Self::save_dac_block(memory, vga, base);
        }

        true
    }

    /// Video hardware state — the CRTC, sequencer, graphics and attribute
    /// register files plus the latches. Exactly [`Self::VIDEO_STATE_HW_LEN`]
    /// bytes from `base`.
    fn save_hw_block(memory: &mut Memory, vga: &Vga, base: usize) {
        {
            memory.write_u8(base, vga.get_sequencer_index());
            memory.write_u8(base + 0x01, vga.get_crtc_index());
            memory.write_u8(base + 0x02, vga.get_graphics_index());
            memory.write_u8(base + 0x03, vga.get_attribute_index());
            memory.write_u8(base + 0x04, vga.get_feature_control());

            let seq_regs = vga.get_sequencer_regs();
            #[allow(clippy::needless_range_loop)]
            for ct in 1..5usize {
                memory.write_u8(base + 0x04 + ct, seq_regs[ct]);
            }

            memory.write_u8(base + 0x09, vga.get_misc_output());

            let crtc_regs = vga.get_crtc_regs();
            #[allow(clippy::needless_range_loop)]
            for ct in 0..0x19usize {
                memory.write_u8(base + 0x0A + ct, crtc_regs[ct]);
            }

            let palette_regs = vga.get_palette_regs();
            #[allow(clippy::needless_range_loop)]
            for ct in 0..0x10usize {
                memory.write_u8(base + 0x23 + ct, palette_regs[ct]);
            }

            let attr_regs = vga.get_attribute_regs();
            #[allow(clippy::needless_range_loop)]
            for ct in 0..4usize {
                memory.write_u8(base + 0x33 + ct, attr_regs[0x10 + ct]);
            }

            let gfx_regs = vga.get_graphics_regs();
            #[allow(clippy::needless_range_loop)]
            for ct in 0..9usize {
                memory.write_u8(base + 0x37 + ct, gfx_regs[ct]);
            }

            memory.write_u16(base + 0x40, memory.read_u16(0x463));
            // VGA latches - save from memory's latch state
            let latches = memory.get_latches();
            #[allow(clippy::needless_range_loop)]
            for ct in 0..4usize {
                memory.write_u8(base + 0x42 + ct, latches[ct]);
            }
        }
    }

    /// BIOS data-area video state. Exactly [`Self::VIDEO_STATE_BDA_LEN`] bytes
    /// from `base`.
    fn save_bda_block(memory: &mut Memory, base: usize) {
        {
            memory.write_u8(base, memory.read_u8(0x410) & 0x30);
            for ct in 0..0x1Eusize {
                memory.write_u8(base + 0x01 + ct, memory.read_u8(0x449 + ct));
            }
            for ct in 0..0x07usize {
                memory.write_u8(base + 0x1F + ct, memory.read_u8(0x484 + ct));
            }
            memory.write_u32(base + 0x26, memory.read_u32(0x48A));
            memory.write_u32(base + 0x2A, memory.read_u32(0x14));
            memory.write_u32(base + 0x2E, memory.read_u32(0x74));
            memory.write_u32(base + 0x32, memory.read_u32(0x7C));
            memory.write_u32(base + 0x36, memory.read_u32(0x10C));
        }
    }

    /// DAC state — read/write index, PEL mask, all 256 palette triplets and the
    /// colour-select register. Exactly [`Self::VIDEO_STATE_DAC_LEN`] bytes from
    /// `base`.
    fn save_dac_block(memory: &mut Memory, vga: &Vga, base: usize) {
        {
            let dac_state = vga.get_dac_state() & 1;
            let dac_index = if dac_state == 0 {
                vga.get_dac_write_index()
            } else {
                vga.get_dac_read_index()
            };
            memory.write_u8(base, dac_state);
            memory.write_u8(base + 0x001, dac_index);
            memory.write_u8(base + 0x002, vga.get_pel_mask());

            let mut cursor = base + 0x003;
            for &(r, g, b) in vga.get_dac_entries() {
                memory.write_u8(cursor, (r >> 2) & 0x3F);
                memory.write_u8(cursor + 1, (g >> 2) & 0x3F);
                memory.write_u8(cursor + 2, (b >> 2) & 0x3F);
                cursor += 3;
            }

            let attr_regs = vga.get_attribute_regs();
            memory.write_u8(base + 0x303, attr_regs[0x14]);
        }
    }

    #[allow(clippy::verbose_bit_mask)]
    pub(crate) fn video_state_restore(
        state: u16,
        cpu: &Cpu,
        memory: &mut Memory,
        vga: &mut Vga,
    ) -> bool {
        if (state & 0x07) == 0 {
            return false;
        }

        let buffer_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());

        if (state & 0x01) != 0 {
            let base = buffer_addr + memory.read_u16(buffer_addr) as usize;
            Self::restore_hw_block(memory, vga, base);
        }

        if (state & 0x02) != 0 {
            let base = buffer_addr + memory.read_u16(buffer_addr + 2) as usize;
            Self::restore_bda_block(memory, vga, base);
        }

        if (state & 0x04) != 0 {
            let base = buffer_addr + memory.read_u16(buffer_addr + 4) as usize;
            Self::restore_dac_block(memory, vga, base);
        }

        true
    }

    /// Inverse of [`Self::save_hw_block`].
    fn restore_hw_block(memory: &mut Memory, vga: &mut Vga, base: usize) {
        {
            let mut seq_regs = vga.get_sequencer_regs();
            #[allow(clippy::needless_range_loop)]
            for ct in 1..5usize {
                seq_regs[ct] = memory.read_u8(base + 0x04 + ct);
            }
            vga.set_sequencer_regs(seq_regs);

            vga.set_misc_output(memory.read_u8(base + 0x09));

            let mut crtc_regs = vga.get_crtc_regs();
            #[allow(clippy::needless_range_loop)]
            for ct in 0..0x19usize {
                crtc_regs[ct] = memory.read_u8(base + 0x0A + ct);
            }
            vga.set_crtc_regs(crtc_regs);

            let mut attr_regs = vga.get_attribute_regs();
            #[allow(clippy::needless_range_loop)]
            for ct in 0..0x10usize {
                attr_regs[ct] = memory.read_u8(base + 0x23 + ct);
            }
            #[allow(clippy::needless_range_loop)]
            for ct in 0..4usize {
                attr_regs[0x10 + ct] = memory.read_u8(base + 0x33 + ct);
            }
            vga.set_attribute_regs(attr_regs);

            let mut gfx_regs = vga.get_graphics_regs();
            #[allow(clippy::needless_range_loop)]
            for ct in 0..9usize {
                gfx_regs[ct] = memory.read_u8(base + 0x37 + ct);
            }
            vga.set_graphics_regs(gfx_regs);

            memory.write_u16(0x463, memory.read_u16(base + 0x40));

            let mut latches = [0u8; 4];
            #[allow(clippy::needless_range_loop)]
            for ct in 0..4usize {
                latches[ct] = memory.read_u8(base + 0x42 + ct);
            }
            memory.set_latches(latches);

            vga.set_sequencer_index(memory.read_u8(base));
            vga.set_crtc_index(memory.read_u8(base + 0x01));
            vga.set_graphics_index(memory.read_u8(base + 0x02));
            vga.set_attribute_index(memory.read_u8(base + 0x03));
            vga.reset_attribute_flip_flop();
            vga.set_feature_control(memory.read_u8(base + 0x04));
        }
    }

    /// Inverse of [`Self::save_bda_block`].
    fn restore_bda_block(memory: &mut Memory, vga: &mut Vga, base: usize) {
        {
            let equip = memory.read_u8(0x410) & 0xCF;
            memory.write_u8(0x410, equip | (memory.read_u8(base) & 0x30));
            for ct in 0..0x1Eusize {
                memory.write_u8(0x449 + ct, memory.read_u8(base + 0x01 + ct));
            }
            for ct in 0..0x07usize {
                memory.write_u8(0x484 + ct, memory.read_u8(base + 0x1F + ct));
            }
            memory.write_u32(0x48A, memory.read_u32(base + 0x26));
            memory.write_u32(0x14, memory.read_u32(base + 0x2A));
            memory.write_u32(0x74, memory.read_u32(base + 0x2E));
            memory.write_u32(0x7C, memory.read_u32(base + 0x32));
            memory.write_u32(0x10C, memory.read_u32(base + 0x36));

            vga.sync_bda_state(memory);
            memory.mark_vga_pipeline_dirty();
            memory.apply_vga_pipeline(&vga.build_pipeline_state());
        }
    }

    /// Inverse of [`Self::save_dac_block`].
    fn restore_dac_block(memory: &mut Memory, vga: &mut Vga, base: usize) {
        {
            vga.set_pel_mask(memory.read_u8(base + 0x002));

            let mut cursor = base + 0x003;
            for idx in 0..256u16 {
                let r = memory.read_u8(cursor);
                let g = memory.read_u8(cursor + 1);
                let b = memory.read_u8(cursor + 2);
                vga.set_dac_reg(idx, r, g, b);
                cursor += 3;
            }

            let mut attr_regs = vga.get_attribute_regs();
            attr_regs[0x14] = memory.read_u8(base + 0x303);
            vga.set_attribute_regs(attr_regs);

            let dac_state = memory.read_u8(base) & 1;
            let dac_index = memory.read_u8(base + 0x001);
            vga.set_dac_state(dac_state);
            if dac_state == 0 {
                vga.set_dac_write_index(dac_index);
            } else {
                vga.set_dac_read_index(dac_index);
            }
            vga.set_dac_component(0);
        }
    }

    pub(crate) fn toggle_blinking(memory: &mut Memory, vga: &mut Vga, state: u8) {
        if state > 1 {
            return;
        }
        let mut attr = vga.get_attribute_regs();
        debug_assert!(attr.len() > 0x10, "VGA attr regs must have >0x10 entries");
        if state == 0 {
            attr[0x10] &= !0x08;
        } else {
            attr[0x10] |= 0x08;
        }
        vga.set_attribute_regs(attr);

        let mut msr = memory.read_u8(0x465) & 0xDF;
        if state != 0 {
            msr |= 0x20;
        }
        memory.write_u8(0x465, msr);
    }

    pub(crate) fn select_dac_page(vga: &mut Vga, function: u8, mode: u8) {
        let mut attr = vga.get_attribute_regs();
        debug_assert!(attr.len() > 0x14, "VGA attr regs must have >0x14 entries");
        let mut reg10 = attr[0x10];
        if function == 0 {
            if mode != 0 {
                reg10 |= 0x80;
            } else {
                reg10 &= 0x7F;
            }
            attr[0x10] = reg10;
        } else {
            let mut page = mode;
            if (reg10 & 0x80) == 0 {
                page = page.wrapping_shl(2);
            }
            attr[0x14] = page & 0x0F;
        }
        vga.set_attribute_regs(attr);
    }

    pub(crate) fn get_dac_page(vga: &Vga) -> (u8, u8) {
        let attr = vga.get_attribute_regs();
        debug_assert!(attr.len() > 0x14, "VGA attr regs must have >0x14 entries");
        let reg10 = attr[0x10];
        let mode = u8::from((reg10 & 0x80) != 0);
        let mut page = attr[0x14];
        if mode != 0 {
            page &= 0x0F;
        } else {
            page = (page & 0x0C) >> 2;
        }
        (mode, page)
    }

    pub(crate) fn perform_grayscale_summing(vga: &mut Vga, start: u16, count: u16) {
        let count = count.min(0x100);
        for i in 0..count {
            let idx = start.wrapping_add(i) as usize;
            let (r, g, b) = vga.get_dac_entries().get(idx).copied().unwrap_or((0, 0, 0));
            let (nr, ng, nb) =
                Self::grayscale_triplet((r >> 2) & 0x3F, (g >> 2) & 0x3F, (b >> 2) & 0x3F);
            vga.set_dac_reg(idx as u16, nr, ng, nb);
        }
    }

    pub(crate) fn ega_ril_group(dx: u16) -> Option<(u8, u16)> {
        match dx {
            0x00 => Some((0, 25)), // CRTC
            0x08 => Some((1, 5)),  // Sequencer
            0x10 => Some((2, 9)),  // Graphics controller
            0x18 => Some((3, 20)), // Attribute controller
            0x20 => Some((4, 0)),  // Misc output
            0x28 => Some((5, 0)),  // Feature control
            0x30 => Some((6, 0)),  // Graphics 1 position (3CC)
            0x38 => Some((7, 0)),  // Graphics 2 position (3CA)
            _ => None,
        }
    }

    pub(crate) fn ega_ril_read_register(vga: &Vga, reg: u8, dx: u16) -> u8 {
        let Some((group, regs)) = Self::ega_ril_group(dx) else {
            return 0;
        };
        if regs == 0 {
            return match group {
                4 | 6 => vga.get_misc_output(),
                5 | 7 => vga.get_feature_control(),
                _ => 0,
            };
        }
        if reg as u16 >= regs {
            return 0;
        }
        match group {
            0 => vga.get_crtc_regs()[reg as usize],
            1 => vga.get_sequencer_regs()[reg as usize],
            2 => vga.get_graphics_regs()[reg as usize],
            3 => vga.get_attribute_regs()[reg as usize],
            _ => 0,
        }
    }

    pub(crate) fn ega_ril_write_register(vga: &mut Vga, reg: u8, value: u8, dx: u16) {
        let Some((group, regs)) = Self::ega_ril_group(dx) else {
            return;
        };
        if regs == 0 {
            match group {
                4 | 6 => vga.set_misc_output(value),
                5 | 7 => vga.set_feature_control(value),
                _ => {}
            }
            return;
        }
        if reg as u16 >= regs {
            return;
        }
        match group {
            0 => {
                let mut regs = vga.get_crtc_regs();
                regs[reg as usize] = value;
                vga.set_crtc_regs(regs);
            }
            1 => {
                let mut regs = vga.get_sequencer_regs();
                regs[reg as usize] = value;
                vga.set_sequencer_regs(regs);
            }
            2 => {
                let mut regs = vga.get_graphics_regs();
                regs[reg as usize] = value;
                vga.set_graphics_regs(regs);
            }
            3 => {
                let mut regs = vga.get_attribute_regs();
                regs[reg as usize] = value;
                vga.set_attribute_regs(regs);
            }
            _ => {}
        }
    }

    pub(crate) fn ega_ril_read_range(
        cpu: &Cpu,
        memory: &mut Memory,
        vga: &Vga,
        start: u8,
        count: u8,
        dx: u16,
    ) {
        let Some((_group, regs)) = Self::ega_ril_group(dx) else {
            return;
        };
        if regs == 0 || start as u16 >= regs {
            return;
        }
        let count = (count as u16).min(regs - start as u16) as u8;
        let base_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        for i in 0..count {
            let val = Self::ega_ril_read_register(vga, start.wrapping_add(i), dx);
            memory.write_u8(base_addr + usize::from(i), val);
        }
    }

    pub(crate) fn ega_ril_write_range(
        cpu: &Cpu,
        memory: &Memory,
        vga: &mut Vga,
        start: u8,
        count: u8,
        dx: u16,
    ) {
        let Some((_group, regs)) = Self::ega_ril_group(dx) else {
            return;
        };
        if regs == 0 || start as u16 >= regs {
            return;
        }
        let count = (count as u16).min(regs - start as u16) as u8;
        let base_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        for i in 0..count {
            let val = memory.read_u8(base_addr + usize::from(i));
            Self::ega_ril_write_register(vga, start.wrapping_add(i), val, dx);
        }
    }

    pub(crate) fn ega_ril_read_register_set(cpu: &Cpu, memory: &mut Memory, vga: &Vga, count: u16) {
        let mut addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        for _ in 0..count {
            let dx = memory.read_u16(addr);
            let reg = memory.read_u8(addr + 2);
            let val = Self::ega_ril_read_register(vga, reg, dx);
            memory.write_u8(addr + 3, val);
            addr += 4;
        }
    }

    pub(crate) fn ega_ril_write_register_set(
        cpu: &Cpu,
        memory: &Memory,
        vga: &mut Vga,
        count: u16,
    ) {
        let mut addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        for _ in 0..count {
            let dx = memory.read_u16(addr);
            let reg = memory.read_u8(addr + 2);
            let val = memory.read_u8(addr + 3);
            Self::ega_ril_write_register(vga, reg, val, dx);
            addr += 4;
        }
    }

    pub(crate) fn ega_ril_define_defaults(cpu: &Cpu, memory: &Memory, vga: &mut Vga) {
        let Some((group, regs)) = Self::ega_ril_group(cpu.regs.dx()) else {
            return;
        };
        let count = if regs == 0 { 1 } else { regs as usize };
        let base = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        let mut values = Vec::with_capacity(count);
        for idx in 0..count {
            values.push(memory.read_u8(base + idx));
        }
        vga.set_ril_defaults(group, values);
    }

    pub(crate) fn ega_ril_revert_defaults(vga: &mut Vga, dx: u16) {
        let Some((group, regs)) = Self::ega_ril_group(dx) else {
            return;
        };
        let Some(values) = vga.get_ril_defaults(group).map(<[u8]>::to_vec) else {
            return;
        };
        let count = if regs == 0 { 1 } else { regs as usize };
        for (idx, &value) in values.iter().take(count).enumerate() {
            Self::ega_ril_write_register(vga, idx as u8, value, dx);
        }
    }

    /// Set a standard (non-VBE) video mode: the whole AH=00 body — teardown,
    /// `set_mode_bios`, the pipeline sync, the video-RAM clear and every BDA
    /// field a real VGA BIOS publishes.
    ///
    /// `raw_mode` is the AL byte as the caller passed it: bit 7 is the
    /// no-clear flag and lands in BDA 0x487, while 0x449 gets the masked
    /// number.
    ///
    fn set_standard_video_mode(&mut self, memory: &mut Memory, vga: &mut Vga, raw_mode: u8) {
        let clear_vram = (raw_mode & 0x80) == 0;
        let mode = raw_mode & 0x7F;
        vga.set_mode_bios(mode);
        memory.mark_vga_pipeline_dirty();
        memory.apply_vga_pipeline(&vga.build_pipeline_state());

        // Real BIOS clears video memory on mode set. Many DOS programs rely on this
        // behavior even when re-setting the same mode purely to clear/reinitialize.
        if clear_vram {
            match mode {
                // Text modes
                0x00 | 0x01 | 0x02 | 0x03 | 0x07 => {
                    let (base, window_end) = if mode == 0x07 {
                        (0xB0000usize, 0xB8000usize)
                    } else {
                        (0xB8000usize, 0xC0000usize)
                    };
                    // The ROM CLEAR_TEXT_VRAM routine fills the full 32 KiB
                    // regen plane with the default blank cell, not just the
                    // visible 25-row page. Guests can select pages 1-7 and
                    // write characters without changing their attributes.
                    for address in (base..window_end).step_by(2) {
                        memory.write_u8(address, b' ');
                        memory.write_u8(address + 1, 0x07);
                    }
                    // Reload the standard 8×16 ROM font into plane 2 at
                    // block 0.  Real VGA BIOS does this on every cold
                    // text-mode set so plane 2 always reflects the
                    // active font.  Without it, a graphics-mode round
                    // trip leaves plane 2 holding pixel residue (Mode
                    // 13h chain-4 writes scatter across all four
                    // planes), which the text renderer then reads as
                    // garbled glyphs whenever the one-way
                    // `custom_font_loaded` latch is set.
                    Self::load_font_to_plane2_block(
                        memory,
                        &gp64_hw::vga::CP437_FONT_8X16,
                        16,
                        0,
                        0,
                        256,
                    );
                    vga.custom_font_loaded = true;
                }
                // CGA graphics modes
                0x04..=0x06 => {
                    // The CGA clear reaches across the full 32 KiB adapter
                    // window, including the bank beyond the visible 16 KiB.
                    for addr in 0xB8000usize..0xC0000usize {
                        memory.write_u8(addr, 0);
                    }
                }
                // EGA/VGA planar modes (16-color and mono) - clear all 4 planes
                0x0D..=0x12 => {
                    // Clear all 4 planes
                    for plane in 0..4 {
                        let p = memory.plane_mut(plane);
                        p.fill(0);
                    }
                    // Note: Real VGA BIOS does NOT load fonts to plane 2
                    // for graphics modes. Font data in plane 2 would show
                    // as red artifacts since plane 2 maps to the red color
                    // bit. Instead, graphics-mode character rendering uses
                    // the ROM font via INT 1Fh/43h vectors and our
                    // write_pixel_planar() function.
                }
                // Mode 13h: clear both planar and linear views.
                // The renderer reads from ram[A0000..] while Mode X
                // and chain4 toggling also use planes_flat[].  Clearing
                // both directly is faster than 64000 Chain4 write_u8
                // calls and avoids any pipeline-state sensitivity
                // (e.g. stale plane_mask from a prior text mode causing
                // the Chain4 writeback to copy garbage from unwritten
                // planes).
                0x13 => {
                    // 64000 pixels / 4 planes = 16000 bytes per plane
                    for plane in 0..4 {
                        memory.plane_mut(plane)[..16000].fill(0);
                    }
                    memory.video_memory_mut()[..64000].fill(0);
                }
                _ => {}
            }
        }

        // Enable/disable grayscale summing based on mode type.
        // Real VGA BIOS enables grayscale summing (BDA 0x489 bit 1)
        // for monochrome modes. This converts subsequent DAC writes
        // via INT 10h AH=10h to grayscale, ensuring programs that
        // load color DAC tables get correct monochrome output.
        {
            let mut flags = memory.read_u8(0x489);
            if matches!(mode, 0x07 | 0x0F | 0x11) {
                flags |= 0x02; // Enable grayscale summing
            } else {
                flags &= !0x02; // Disable grayscale summing
            }
            memory.write_u8(0x489, flags);
        }

        // Update BIOS Data Area with basic video mode information.
        // BDA at 0x400-0x4FF:
        // 0x449: current video mode
        // 0x44A: columns (word)
        // 0x484: rows-1
        // 0x44C: page size in bytes (word)
        // 0x44E: active page start address in BYTES (word) — BIOS convention
        // 0x450: cursor positions for pages 0-7 (8 words: row in high byte, col in low byte)
        // 0x462: active display page
        // 0x463: CRT controller base port (word) (0x3D4 color, 0x3B4 mono)
        let (_m, cols, _page) = vga.get_mode();
        // MDD-BUG-CRUCIBLE-00424. BDA 0x449 holds the MASKED mode and
        // the caller's no-clear flag lives in bit 7 of BDA 0x487 — the
        // `AL & 0x7F` and `0x60 | (raw AL & 0x80)`). We stored the raw
        // byte in 0x449 and never touched 0x487, so every direct BDA
        // reader and the video-state save/restore saw a mode number
        // with a spurious bit 7, and 0x487 lied about the flag. AH=0Fh
        // only looked right because it echoed 0x449 back.
        memory.write_u8(0x449, mode);
        let ctl = (memory.read_u8(0x487) & 0x7F) | (raw_mode & 0x80);
        memory.write_u8(0x487, ctl);
        memory.write_u16(0x44A, cols as u16);
        // vga.rows isn't exposed; infer rows from common modes we support.
        let rows_minus_1 = match mode {
            0x11 | 0x12 => 29, // 480/16 = 30 rows with 8x16 font
            // Most modes use a 25-line text grid for BIOS cursor/teletype.
            _ => 24,
        };
        memory.write_u8(0x484, rows_minus_1);
        let char_height = match mode {
            // VGA text modes: 16 scanlines per character cell
            0x00 | 0x01 | 0x02 | 0x03 | 0x07 | 0x11 | 0x12 => 16,
            0x0F | 0x10 => 14,
            // CGA graphics, EGA graphics, Mode 13h: 8 scanlines
            _ => 8,
        };
        memory.write_u16(0x485, u16::from(char_height));
        if !vga.is_text_mode() {
            let offset = match char_height {
                14 => Self::ROM_FONT_8X14_OFFSET,
                16 => Self::ROM_FONT_8X16_OFFSET,
                _ => Self::ROM_FONT_8X8_OFFSET,
            };
            Self::set_font_vector(memory, 0x43, 0xF000, offset);
        }

        // Bytes per page are aligned by BIOS for compatibility with page flipping.
        let page_size_bytes = match mode {
            0x00 | 0x01 => 0x0800u16,
            0x02 | 0x03 | 0x07 => 0x1000u16,
            0x04..=0x06 => 0x4000u16,
            // MDD-BUG-CRUCIBLE-00426. BDA 0x44C is the BIOS regen-buffer
            // ALLOCATION, not the visible frame's byte count. We stored
            // the visible count (rows * bytes-per-row), which is smaller
            // than the allocation for every planar mode, so page N began
            // before its BIOS-defined boundary and overlapped page N-1.
            0x0D => 0x2000u16,        // 320x200 16-colour planar (visible 0x1F40)
            0x0E => 0x4000u16,        // 640x200 16-colour planar (visible 0x3E80)
            0x0F | 0x10 => 0x8000u16, // 640x350 (visible 0x6D60)
            0x11 | 0x12 => 0xA000u16, // 640x480 (visible 0x9600)
            0x13 => 0xFA00u16,        // 320*200 bytes, typical VGA linear page
            _ => {
                let rows = (rows_minus_1 as u16).wrapping_add(1);
                (cols as u16).saturating_mul(rows).saturating_mul(2)
            }
        };
        memory.write_u16(0x44C, page_size_bytes);
        memory.write_u16(0x44E, 0); // page 0 start = byte offset 0
        for page in 0..8u16 {
            memory.write_u16(0x450 + (page as usize) * 2, 0);
        }
        memory.write_u8(0x462, 0);
        // MDD-BUG-CRUCIBLE-00425: mode 07h is the ONE monochrome mode,
        // and its VGA path selects the mono I/O group (MISC bit 0 = 0),
        // so BDA 0x463 must name the mono CRTC base. Publishing the
        // colour base misreports mono-vs-colour to every downstream
        // reader — INT 10h AH=12h BL=10h (BH), the AH=1Bh functionality
        // state (misc flags bit 2), and any guest that drives the CRTC
        // through the BDA-selected port.
        //
        // Mode 0Fh is greyscale, but the configured VGA timing table uses
        // the colour I/O group (MISC bit 0 = 1), so it retains that CRTC base.
        let crtc_base: u16 = if mode == 0x07 { 0x3B4 } else { 0x3D4 };
        memory.write_u16(0x463, crtc_base);

        if mode <= 0x07 {
            // cga_mode_control_from_table always succeeds for modes 0-7
            // (table has 88 entries, CGA mode control starts at offset 80)
            let cga_mode = Self::cga_mode_control_from_table(mode)
                .expect("CGA mode control table must contain entries for modes 0-7");
            vga.set_cga_mode_control(cga_mode);
            memory.write_u8(0x465, cga_mode);
            memory.write_u8(0x466, vga.get_cga_color_select());
        }

        // Set cursor shape for text modes based on character height.
        // Real VGA BIOS sets a 2-scanline underscore cursor at the bottom
        // of the character cell.
        if matches!(mode, 0x00..=0x03 | 0x07) {
            let cursor_end = char_height - 1;
            let cursor_start = cursor_end - 1;
            vga.set_cursor_shape(cursor_start, cursor_end);
            memory.write_u16(0x460, ((cursor_start as u16) << 8) | cursor_end as u16);
        }
    }

    pub(crate) fn current_dcc_code(memory: &Memory) -> u16 {
        let index = memory.read_u8(0x48A) as usize;
        DCC_ENTRIES.get(index).copied().unwrap_or(0xFFFF)
    }

    pub(crate) fn set_dcc_index(memory: &mut Memory, code: u16) -> bool {
        let mut target = None;
        for (idx, &entry) in DCC_ENTRIES.iter().enumerate() {
            if entry == code || entry.swap_bytes() == code {
                target = Some(idx as u8);
                break;
            }
        }
        if let Some(index) = target {
            memory.write_u8(0x48A, index);
            true
        } else {
            false
        }
    }

    pub(crate) fn grayscale_enabled(memory: &Memory) -> bool {
        (memory.read_u8(0x489) & 0x06) != 0
    }

    pub(crate) fn grayscale_triplet(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
        let r = r as u16;
        let g = g as u16;
        let b = b as u16;
        let i = ((77 * r + 151 * g + 28 * b) + 0x80) >> 8;
        let ic = if i > 0x3F { 0x3F } else { i as u8 };
        (ic, ic, ic)
    }

    pub(crate) fn cga_mode_control_from_table(mode: u8) -> Option<u8> {
        let mode = mode & 0x7F;
        if mode > 0x07 {
            return None;
        }
        let idx = mode as usize;
        ROM_VIDEO_PARAM_TABLE
            .get(CGA_MODE_CONTROL_TABLE_OFFSET + idx)
            .copied()
    }

    pub(crate) fn load_font_to_plane2_block(
        memory: &mut Memory,
        font: &[u8],
        char_height: usize,
        block: u8,
        start_char: u16,
        count: u16,
    ) {
        const BYTES_PER_CHAR_SLOT: usize = 32;
        let plane2 = memory.plane_mut(2);
        let start = (start_char as usize).min(256);
        let count = count.min((256 - start) as u16) as usize;
        let block_base = font_bank_offset(block);
        let height = char_height.min(BYTES_PER_CHAR_SLOT);

        for i in 0..count {
            let char_code = start + i;
            let font_offset = char_code * char_height;
            let plane2_offset = block_base + char_code * BYTES_PER_CHAR_SLOT;
            for row in 0..BYTES_PER_CHAR_SLOT {
                let byte = if row < height && font_offset + row < font.len() {
                    font[font_offset + row]
                } else {
                    0
                };
                // Eight banks of 256 32-byte slots fit in the 64KB plane.
                plane2[plane2_offset + row] = byte;
            }
        }
    }

    /// Points a font interrupt vector (1Fh or 43h) at `segment:offset`.
    fn set_font_vector(memory: &mut Memory, vector: usize, segment: u16, offset: u16) {
        memory.write_u16(vector * 4, offset);
        memory.write_u16(vector * 4 + 2, segment);
    }

    /// Records a graphics font's character height (BDA 0x485, a word) and
    /// the row count selected by BL (BDA 0x484 holds rows - 1): BL=0 takes
    /// DL rows, 1 gives 14, 3 gives 43, and 2 or any other value 25.
    fn set_graphics_font_metrics(memory: &mut Memory, height: u16, bl: u8, dl: u8) {
        let rows = match bl {
            0x00 => dl,
            0x01 => 14,
            0x03 => 43,
            _ => 25,
        };
        memory.write_u16(0x485, height);
        memory.write_u8(0x484, rows.wrapping_sub(1));
    }

    pub(crate) fn load_user_font_to_plane2(
        memory: &mut Memory,
        src_addr: usize,
        count: u16,
        char_height: usize,
        block: u8,
        start_char: u16,
    ) {
        const BYTES_PER_CHAR_SLOT: usize = 32;
        let start = (start_char as usize).min(256);
        let count = count.min((256 - start) as u16) as usize;
        let block_base = font_bank_offset(block);
        let height = char_height.min(BYTES_PER_CHAR_SLOT);

        for i in 0..count {
            let char_code = start + i;
            let plane2_offset = block_base + char_code * BYTES_PER_CHAR_SLOT;
            let src_base = src_addr + i * char_height;
            let mut glyph = [0u8; BYTES_PER_CHAR_SLOT];
            #[allow(clippy::needless_range_loop)]
            for row in 0..BYTES_PER_CHAR_SLOT {
                let byte = if row < height {
                    memory.read_u8(src_base + row)
                } else {
                    0
                };
                glyph[row] = byte;
            }
            let plane2 = memory.plane_mut(2);
            plane2[plane2_offset..(BYTES_PER_CHAR_SLOT + plane2_offset)]
                .copy_from_slice(&glyph[..BYTES_PER_CHAR_SLOT]);
        }
    }

    pub(crate) fn update_text_metrics_for_font(
        memory: &mut Memory,
        vga: &mut Vga,
        char_height: usize,
        adjust_layout: bool,
    ) {
        if char_height == 0 {
            return;
        }
        let height_u8 = char_height.min(255) as u8;
        memory.write_u8(0x485, height_u8);
        if !adjust_layout {
            return;
        }

        let mode = memory.read_u8(0x449) & 0x7F;
        if matches!(mode, 0x00 | 0x01 | 0x02 | 0x03 | 0x07) {
            // Font slots and CRTC maximum scan line both support at most 32 rows.
            let char_height = char_height.min(32);
            let fb_height = vga.dimensions().1;
            // Keep the BDA count representable by the VGA's cached u8 row count.
            let rows = (fb_height / char_height).clamp(1, 255);
            let columns = usize::from(memory.read_u16(0x44A));
            let page_bytes = (columns * rows * 2).div_ceil(0x800) * 0x800;
            memory.write_u16(0x485, char_height as u16);
            memory.write_u8(0x484, (rows - 1) as u8);
            memory.write_u16(0x44C, page_bytes as u16);
            let cursor_end = (char_height - 1) as u8;
            let cursor_start = cursor_end.saturating_sub(1);
            memory.write_u16(0x460, u16::from_be_bytes([cursor_start, cursor_end]));

            let mut crtc = vga.get_crtc_regs();
            crtc[0x09] = (crtc[0x09] & 0xE0) | cursor_end;
            vga.set_crtc_regs(crtc);
            vga.sync_text_layout(memory);
            let page = vga.normalize_cursor_page(memory.read_u8(0x462)) as u8;
            memory.write_u8(0x462, page);
            memory.write_u16(0x44E, (usize::from(page) * page_bytes) as u16);
            vga.set_active_page(page);
            memory.mark_vga_pipeline_dirty();
            memory.apply_vga_pipeline(&vga.build_pipeline_state());
        }
    }

    pub(crate) fn font_blocks_from_spec(spec: u8) -> (u8, u8) {
        let mut primary = spec & 0x03;
        let mut secondary = (spec >> 2) & 0x03;
        if (spec & 0x10) != 0 {
            primary |= 0x04;
        }
        if (spec & 0x20) != 0 {
            secondary |= 0x04;
        }
        (primary, secondary)
    }
}
