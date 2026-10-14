//! INT 16h/INT 09h - Keyboard BIOS services

use anyhow::Result;
use gp64_cpu::Cpu;
use gp64_hw::bda;
use gp64_hw::keyboard::Keyboard;
use gp64_memory::Memory;

use super::Bios;

/// Disposition returned by INT 16h HLE handler.
pub enum Int16Disp {
    /// Normal return — caller should call hle_iret()
    Iret,
    /// Redirect execution to an assembler stub at F000:offset.
    /// The IRET frame stays on the stack; the stub handles the rest.
    Stub(u16),
}

impl Bios {
    /// What INT 16h AH=01h/11h answers, without dequeuing.
    ///
    /// The real-mode path reaches this answer by redirecting to the assembler
    /// peek stub at F000:F880 / F000:F8B0, whose STI opens an IRQ window the
    /// pure-HLE path lacks. A PROTECTED-mode guest cannot execute an F000 stub
    /// at all, so its dispatch arm has to compute the same answer — and used to
    /// hardcode "no key" instead, which hung any
    /// `poll: mov ah,1 / int 16h / jz poll` loop for ever, because a peek does
    /// not dequeue and so nothing could ever change the answer
    /// (MDD-BUG-TEM-00072).
    ///
    /// `Some((scancode, ascii))` means ZF=0 with that pair in AX. `None` means
    /// ZF=1. AH=01h reports an E0 prefix as 0 exactly as AH=00h does; AH=11h
    /// is the extended call and keeps the raw byte.
    pub fn int16_peek_answer(memory: &Memory, ah: u8) -> Option<(u8, u8)> {
        let (scancode, ascii) = bda::kbd_peek(memory)?;
        let ascii = if ah == 0x01 && ascii == 0xE0 {
            0
        } else {
            ascii
        };
        Some((scancode, ascii))
    }

    /// INT 16h - Keyboard BIOS
    pub fn int16(
        &self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        keyboard: &mut Keyboard,
    ) -> Result<Int16Disp> {
        let ah = cpu.regs.ah();

        if self.kbd_trace_allow() {
            let (start, end, head, tail) = bda::kbd_debug_pointers(memory);
            tracing::debug!(
                target: "gp64::kbd::int",
                ah = format_args!("{:02X}", ah),
                if_enabled = cpu.interrupts_enabled,
                cs = format_args!("{:04X}", cpu.regs.cs),
                ip = format_args!("{:04X}", cpu.regs.ip()),
                bda_start = format_args!("{:04X}", start),
                bda_end = format_args!("{:04X}", end),
                bda_head = format_args!("{:04X}", head),
                bda_tail = format_args!("{:04X}", tail),
                shift = format_args!("{:02X}", bda::kbd_get_shift_flags(memory)),
                ext_shift = format_args!("{:02X}", bda::kbd_get_extended_shift_flags(memory)),
                "INT16_ENTER"
            );
        }

        match ah {
            // Read keyboard
            0x00 | 0x10 => {
                if let Some((scancode, ascii)) = bda::kbd_peek(memory) {
                    // Key available — dequeue in HLE
                    bda::kbd_dequeue(memory);
                    if self.kbd_trace_allow() {
                        tracing::debug!(
                            target: "gp64::kbd::int",
                            scancode = format_args!("{:02X}", scancode),
                            ascii = format_args!("{:02X}", ascii),
                            "INT16_READ"
                        );
                    }
                    if gp64_hw::kbd_flow_trace_enabled() {
                        tracing::info!(
                            target: "gp64::kbd::flow",
                            scancode = format_args!("{:02X}", scancode),
                            ascii = format_args!("{:02X}", ascii),
                            ah = format_args!("{:02X}", ah),
                            "INT16_READ"
                        );
                    }
                    let mut out_ascii = ascii;
                    if ah == 0x00 && out_ascii == 0xE0 {
                        out_ascii = 0;
                    }
                    cpu.regs.set_ah(scancode);
                    cpu.regs.set_al(out_ascii);
                    return Ok(Int16Disp::Iret);
                } else {
                    // No key — redirect to assembler wait loop
                    if self.kbd_trace_allow() {
                        tracing::trace!(
                            target: "gp64::kbd::int",
                            "INT16_EMPTY_STUB_REDIRECT"
                        );
                    }
                    // The stub reads the ring control words RAW, so hand it the
                    // same validated state `kbd_peek` just answered from
                    // (MDD-BUG-CRUCIBLE-00429).
                    bda::kbd_normalize_ring(memory);
                    return Ok(Int16Disp::Stub(if ah == 0x00 {
                        Bios::INT16_WAIT_00
                    } else {
                        Bios::INT16_WAIT_10
                    }));
                }
            }

            // Check keyboard status — always redirect to assembler peek stub.
            // The stub's STI creates an IRQ window that the pure-HLE path lacks,
            // and uses RETF 2 to preserve the ZF result for the caller.
            0x01 | 0x11 => {
                // Same contract as the AH=00h/10h redirect above: the peek stub
                // reads head/tail/start/end raw, so validate them first or it
                // answers from state the Rust helpers would have refused
                // (MDD-BUG-CRUCIBLE-00429).
                bda::kbd_normalize_ring(memory);
                return Ok(Int16Disp::Stub(if ah == 0x01 {
                    Bios::INT16_PEEK_01
                } else {
                    Bios::INT16_PEEK_11
                }));
            }

            // Get shift flags
            0x02 | 0x12 => {
                cpu.regs.set_al(bda::kbd_get_shift_flags(memory));
                if ah == 0x12 {
                    cpu.regs.set_ah(bda::kbd_get_enhanced_shift_flags(memory));
                }
            }

            // Set typematic rate/delay (subfunctions in AL)
            0x03 => {
                match cpu.regs.al() {
                    // Reset to default (500ms delay, ~10.9 Hz, IBM AT power-on defaults)
                    0x00 => keyboard.set_typematic(1, 0x0B),
                    // Disable typematic repeat
                    0x04 => keyboard.disable_typematic_repeat(),
                    // Set repeat rate and delay (AT/PS/2)
                    // BH = delay (0-3: 0=250ms, 1=500ms, 2=750ms, 3=1000ms)
                    // BL = repeat rate (0-31: 0=30cps fastest, 0x1F=2cps slowest)
                    0x05 => keyboard.set_typematic(cpu.regs.bh(), cpu.regs.bl()),
                    // Get current rate and delay (PS/2)
                    0x06 => {
                        cpu.regs.set_bh(keyboard.get_typematic_delay());
                        cpu.regs.set_bl(keyboard.get_typematic_rate());
                    }
                    // AL=01h-03h are PCjr-only, not applicable to AT-class emulation
                    _ => {}
                }
            }

            // Keyboard intercept (store keycode)
            0x05 => {
                // Store keycode in buffer
                let scancode = cpu.regs.ch();
                let ascii = cpu.regs.cl();
                if bda::kbd_enqueue(memory, scancode, ascii) {
                    cpu.regs.set_al(0);
                } else {
                    cpu.regs.set_al(1); // Buffer full
                }
            }

            // Write to keyboard buffer (enhanced function)
            0x13 => {
                let scancode = cpu.regs.ch();
                let ascii = cpu.regs.cl();
                if bda::kbd_enqueue(memory, scancode, ascii) {
                    cpu.regs.set_al(0x00);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.set_al(0x01); // Buffer full
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Vendor-specific and extended BIOS calls (AH >= 0x80).
            //
            // This range contains several categories of function:
            //   - TSR installation probes (0x80, 0x87, 0x99, etc.): programs
            //     checking if a resident utility is hooked into INT 16h.
            //   - keyboard driver capabilities (0x92, 0xA2): keyboard driver queries.
            //   - Compaq 386+ BIOS functions (0xF0-0xF3): CPU speed control.
            //     AH=F0: set speed, AH=F1: read speed, AH=F2: keyboard type,
            //     Compaq-style CPU speed reporting.
            //
            // On real non-Compaq BIOSes, unknown AH values fall through to
            // IRET with all registers and flags unchanged. We return with AH
            // unchanged and set CF=1 as a "not supported" signal for callers
            // that check carry.
            0x80..=0xFF => {
                tracing::debug!(
                    ah = format_args!("{:02X}", ah),
                    "INT 16h vendor/extended (not supported)"
                );
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }

            _ => {
                tracing::warn!(ah = format_args!("{:02X}", ah), "Unhandled INT 16h");
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }
        }

        Ok(Int16Disp::Iret)
    }
}
