//! INT 15h - System services

use anyhow::Result;
use gp64_cpu::Cpu;
use gp64_memory::Memory;

use super::{Bios, E820Entry, E820Type};

/// How the INT 15h dispatcher should return to the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Int15Disp {
    /// Normal return — the caller should IRET the HLE frame.
    Iret,
    /// AH=89h switched the CPU into protected mode and set CS:EIP itself,
    /// discarding the INT frame. The caller must NOT IRET (there is no frame
    /// left, and CS/FLAGS are already the post-switch values).
    NoIret,
}

impl Bios {
    /// Generate the E820 memory map based on current system configuration
    /// Returns a fixed-size array and count to avoid heap allocation on every call
    pub(crate) fn generate_e820_map(
        &self,
        memory: &Memory,
    ) -> ([E820Entry; Self::E820_MAX_ENTRIES], usize) {
        let ram_size = memory.ram_size() as u64;
        let mut entries =
            [E820Entry::new(0, 0, E820Type::AddressRangeMemory); Self::E820_MAX_ENTRIES];
        let mut count = 0;

        // Entry 0: Base memory (640 KB). The HLE BIOS does not allocate an EBDA.
        entries[count] = E820Entry::new(0x0000_0000, 0x000A_0000, E820Type::AddressRangeMemory);
        count += 1;

        // Entry 1: Video/ROM area (384 KB at A0000-FFFFF)
        entries[count] = E820Entry::new(0x000A_0000, 0x0006_0000, E820Type::AddressRangeReserved);
        count += 1;

        // Entry 2: Extended memory (if present, from 1MB to top of RAM)
        if ram_size > 0x10_0000 {
            entries[count] = E820Entry::new(
                0x0010_0000,
                ram_size - 0x10_0000,
                E820Type::AddressRangeMemory,
            );
            count += 1;
        }

        (entries, count)
    }

    /// Parse a 24/32-bit base address from an 8086 segment descriptor
    /// Descriptor layout: `limit_lo`(2), `base_lo`(2), `base_mid`(1), access(1), `limit_hi`+flags(1), `base_hi`(1)
    pub(crate) fn parse_descriptor_base(memory: &Memory, desc_addr: usize) -> u32 {
        let base_low = memory.read_u16(desc_addr + 2) as u32;
        let base_mid = memory.read_u8(desc_addr + 4) as u32;
        let base_high = memory.read_u8(desc_addr + 7) as u32;
        base_low | (base_mid << 16) | (base_high << 24)
    }

    /// Copy a memory block, handling overlapping regions correctly
    pub(crate) fn copy_memory_block(memory: &mut Memory, src: usize, dst: usize, len: usize) {
        if src == dst || len == 0 {
            return;
        }

        let overlaps = src < dst && src + len > dst;

        if overlaps {
            // Copy backwards in 4-byte chunks to handle overlapping regions
            let mut offset = len;
            while offset >= 4 {
                offset -= 4;
                let val = memory.read_u32(src + offset);
                memory.write_u32(dst + offset, val);
            }
            while offset > 0 {
                offset -= 1;
                let b = memory.read_u8(src + offset);
                memory.write_u8(dst + offset, b);
            }
        } else {
            // Copy forwards in 4-byte chunks
            let mut offset = 0;
            while offset + 4 <= len {
                let val = memory.read_u32(src + offset);
                memory.write_u32(dst + offset, val);
                offset += 4;
            }
            while offset < len {
                let b = memory.read_u8(src + offset);
                memory.write_u8(dst + offset, b);
                offset += 1;
            }
        }
    }

    /// INT 15h - System services.
    ///
    /// Returns the requested wait duration for AH=86h plus a dispatch verdict.
    /// The emulator owns the scheduler clock, so the BIOS layer must not
    /// advance `cpu.cycles` directly.
    ///
    /// `pic` carries the interrupt controllers for AH=89h (Switch to Protected
    /// Mode), which reprograms their vector bases. A dispatch context that
    /// cannot legally perform the switch (the DPMI HLE-simulation path, whose
    /// caller unconditionally restores PM state afterwards) passes `None`, and
    /// AH=89h keeps its historical "unsupported" answer there.
    pub fn int15(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        pic: Option<&mut gp64_hw::Pic>,
    ) -> Result<(Option<u32>, Int15Disp)> {
        let ah = cpu.regs.ah();
        let mut wait_micros = None;
        let mut disp = Int15Disp::Iret;

        match ah {
            // Keyboard intercept
            0x4F => {
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }

            // Wait
            0x86 => {
                let micros = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
                wait_micros = Some(micros);
                cpu.regs.set_ah(0x00);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Move Extended Memory Block
            0x87 => {
                let word_count = cpu.regs.cx() as usize;
                let gdt_addr = ((cpu.regs.es as usize) << 4) + cpu.regs.si() as usize;

                // Validate word count (max 32K words = 64KB)
                if word_count > 0x8000 {
                    cpu.regs.set_ah(0x02); // Invalid count
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else if word_count == 0 {
                    // Zero count is a no-op success
                    cpu.regs.set_ah(0x00);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    // GDT layout (8 bytes each):
                    // +00h: Dummy descriptor
                    // +08h: Self GDT descriptor
                    // +10h: Source segment descriptor
                    // +18h: Destination segment descriptor
                    // +20h: BIOS CS descriptor
                    // +28h: BIOS SS descriptor
                    let src_base = Self::parse_descriptor_base(memory, gdt_addr + 0x10);
                    let dst_base = Self::parse_descriptor_base(memory, gdt_addr + 0x18);
                    let byte_count = word_count * 2;
                    let ram_size = memory.ram_size();

                    // Validate memory ranges
                    let src_end = src_base.saturating_add(byte_count as u32);
                    let dst_end = dst_base.saturating_add(byte_count as u32);

                    if src_end as usize > ram_size || dst_end as usize > ram_size {
                        cpu.regs.set_ah(0x02); // Memory error
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    } else {
                        // A real AT BIOS enables A20 for the duration of
                        // BLOCK_MOVE and restores the caller's gate afterwards,
                        // because the whole point of this call is to reach
                        // memory the caller cannot address itself. The bases
                        // above were validated RAW against `ram_size`, but the
                        // transfer goes through `Memory::read_u32`/`write_u32`,
                        // which apply `mask_address` - so with the gate closed
                        // the call validated one address and copied another
                        // 1 MiB below it, silently, and reported success.
                        // AH=87h exists precisely for programs with no XMS
                        // driver, which is exactly when A20 is still closed (it
                        // is closed in the default HLE DOS state). Same bracket
                        // the equivalent XMS function 0Bh already uses, in
                        // `gp64-dos/src/xms_ext.rs`.
                        let prev_a20 = memory.a20_enabled();
                        memory.set_a20(true);
                        Self::copy_memory_block(
                            memory,
                            src_base as usize,
                            dst_base as usize,
                            byte_count,
                        );
                        memory.set_a20(prev_a20);

                        cpu.regs.set_ah(0x00);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);

                        tracing::debug!(
                            src_base = format_args!("{:08X}", src_base),
                            dst_base = format_args!("{:08X}", dst_base),
                            byte_count,
                            "INT 15h 87h: Moved memory block"
                        );
                    }
                }
            }

            // Extended memory size (in KB above 1MB)
            // Cap at 15360 KB (15 MB) to match CMOS registers 0x17:0x18.
            // display garbage (e.g. "Available: -1K") when they disagree.
            // The E801h/E820h functions report the full extended memory.
            0x88 => {
                let extended_kb = ((memory.ram_size() as u32).saturating_sub(0x0010_0000) / 1024)
                    .min(0x3C00) as u16;
                cpu.regs.set_ax(extended_kb);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Switch to Protected Mode.
            //
            // Reference: the DOS model `INT15_Handler` case 0x89 (src/ints/bios.cpp),
            // matched operation-for-operation — including the parts that differ
            // The ES:SI build-table contract and its pitfalls are documented in
            // MDD-BUG-KILN-00159..00162; KILN-00638 is the missing-HLE-arm bug.
            0x89 => {
                if let Some(pic) = pic {
                    // The two selector loads below are the only fallible calls
                    // in this crate, and the GDT limit they validate against is
                    // guest-supplied (the build table at ES:SI). They used to
                    // run AFTER the 8259 reprogramming, the A20 force and
                    // CR0.PE, with their errors carried out of `int15` by `?` -
                    // so a guest declaring a GDT too small to hold selector
                    // 0x28 left the machine in protected mode with DS and ES
                    // loaded, no valid SS, the INT frame still on the stack,
                    // and both PICs reprogrammed with every line masked.
                    // Unrecoverable, and the error unwound into the host. Real
                    // hardware raises #GP inside the BIOS; it does not
                    // half-switch (MDD-BUG-TEM-00068).
                    //
                    // So the switch is attempted first and the irreversible
                    // 8259 programming happens only once it has succeeded.
                    // Everything the attempt touches is restorable: `Registers`
                    // is Clone, and A20 is bracketed like any other transient
                    // gate change. Reordering the loads earlier is not on its
                    // own enough - they need CR0.PE set to validate at all,
                    // which is exactly why the rollback has to exist.
                    let saved_regs = cpu.regs.clone();
                    let saved_a20 = memory.a20_enabled();

                    // A20 on: protected mode behind a closed gate is 1 MiB-
                    // aliased protected mode (KILN-00161). It must precede the
                    // build-table read below, because ES:SI can address up to
                    // 0x10FFEF and a closed gate would alias that read too.
                    memory.set_a20(true);

                    // The caller's build table at ES:SI. Entry 1 (+0x08) is the
                    // caller's GDT descriptor and entry 2 (+0x10) its IDT
                    // descriptor — READ them, never write them (KILN-00160). A
                    // descriptor's first five bytes are limit[15:0]+base[23:0],
                    // exactly what LGDT/LIDT want; the sixth is the access
                    // byte, which the oracle's `& 0xFFFFFF` masks off.
                    let table = ((cpu.regs.es as usize) << 4) + cpu.regs.si() as usize;
                    cpu.regs.gdtr.limit = memory.read_u16(table + 0x08);
                    cpu.regs.gdtr.base = memory.read_u32(table + 0x0A) & 0x00FF_FFFF;
                    cpu.regs.idtr.limit = memory.read_u16(table + 0x10);
                    cpu.regs.idtr.base = memory.read_u32(table + 0x12) & 0x00FF_FFFF;

                    // Enter protected mode and load the fixed selector set the
                    // service defines: DS=0x18, ES=0x20, SS=0x28, CS=0x30.
                    cpu.regs.cr0 |= 1;
                    cpu.regs.cpl = 0;
                    cpu.load_data_segment(memory, gp64_cpu::DataSegReg::DS, 0x18);
                    cpu.load_data_segment(memory, gp64_cpu::DataSegReg::ES, 0x20);
                    // `&&` short-circuits, so a bad SS never reaches the CS
                    // load. A failed load leaves its own register untouched;
                    // the rollback below undoes everything else.
                    let switched =
                        cpu.load_ss(memory, 0x28).is_ok() && cpu.load_cs(memory, 0x30).is_ok();

                    if !switched {
                        // Nothing irreversible has happened: the PICs were
                        // never touched. Undo the rest and give the arm's own
                        // failure answer, which callers already handle.
                        cpu.regs = saved_regs;
                        memory.set_a20(saved_a20);
                        cpu.regs.set_ah(0x86);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        tracing::debug!(
                            "INT 15h 89h refused: the caller's GDT does not describe the fixed 18/20/28/30 selector set"
                        );
                        return Ok((wait_micros, disp));
                    }

                    // Committed from here. Reprogram both 8259s to the caller's
                    // vector bases (BH = master, BL = slave). The oracle's exact
                    // byte sequence: ICW1=0x10 (edge, cascade, no ICW4),
                    // ICW2=base, ICW3=0, then OCW1 IMR=0xFF - all lines masked.
                    pic.write_command(0, 0x10);
                    pic.write_data(0, cpu.regs.bh());
                    pic.write_data(0, 0x00);
                    pic.write_data(0, 0xFF);
                    pic.write_command(1, 0x10);
                    pic.write_data(1, cpu.regs.bl());
                    pic.write_data(1, 0x00);
                    pic.write_data(1, 0xFF);

                    // The return offset comes from the INT frame, read through
                    // the NEW SS (the caller builds the SS descriptor so SP
                    // stays valid across the switch); the 6-byte frame is then
                    // discarded — this service never IRETs (KILN-00162).
                    let sp = cpu.regs.sp();
                    let ret_ip =
                        memory.read_u16(cpu.regs.ss_cache.base.wrapping_add(sp as u32) as usize);
                    cpu.regs.set_sp(sp.wrapping_add(6));

                    // The oracle zeroes FLAGS (`CPU_SetFlags(0, FMASK_ALL)`):
                    // interrupts off, IOPL 0. AX=0 signals success.
                    cpu.regs.flags = gp64_cpu::Flags::RESERVED1;
                    cpu.regs.set_ax(0);
                    cpu.regs.eip = u32::from(ret_ip);
                    disp = Int15Disp::NoIret;

                    tracing::debug!(
                        gdtr_base = format_args!("{:08X}", cpu.regs.gdtr.base),
                        gdtr_limit = format_args!("{:04X}", cpu.regs.gdtr.limit),
                        idtr_base = format_args!("{:08X}", cpu.regs.idtr.base),
                        idtr_limit = format_args!("{:04X}", cpu.regs.idtr.limit),
                        eip = format_args!("{:04X}", ret_ip),
                        "INT 15h 89h: switched to protected mode"
                    );
                } else {
                    // DPMI HLE-simulation context: performing a real switch here
                    // would be clobbered by the caller's PM-state restore. Keep
                    // the historical "unsupported" answer.
                    cpu.regs.set_ah(0x86);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Extended memory size (E801 method)
            // Returns extended memory size for systems with >64MB RAM
            0xE8 => {
                let al = cpu.regs.al();
                match al {
                    0x01 => {
                        // AX=E801h - Get Extended Memory Size
                        let ram_bytes = memory.ram_size() as u64;

                        // Memory between 1MB and 16MB, in KB
                        // Max is 15MB = 15360 KB = 0x3C00
                        let mem_1_to_16_kb = if ram_bytes > 0x0010_0000 {
                            ((ram_bytes - 0x0010_0000).min(0x00F0_0000) / 1024) as u16
                        } else {
                            0
                        };

                        // Memory above 16MB, in 64KB blocks
                        let mem_above_16_blocks = if ram_bytes > 0x0100_0000 {
                            ((ram_bytes - 0x0100_0000) / 0x0001_0000) as u16
                        } else {
                            0
                        };

                        cpu.regs.set_ax(mem_1_to_16_kb);
                        cpu.regs.set_bx(mem_above_16_blocks);
                        cpu.regs.set_cx(mem_1_to_16_kb); // Some BIOSes only set CX/DX
                        cpu.regs.set_dx(mem_above_16_blocks);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);

                        tracing::debug!(
                            mem_1_to_16_kb = mem_1_to_16_kb,
                            mem_above_16_blocks = mem_above_16_blocks,
                            "INT 15h E801: Get Extended Memory Size"
                        );
                    }
                    0x81 => {
                        // AX=E881h - Get Extended Memory Size (32-bit)
                        // Same as E801 but returns values in full 32-bit registers
                        let ram_bytes = memory.ram_size() as u64;

                        // Memory between 1MB and 16MB, in KB
                        let mem_1_to_16_kb = if ram_bytes > 0x0010_0000 {
                            ((ram_bytes - 0x0010_0000).min(0x00F0_0000) / 1024) as u32
                        } else {
                            0
                        };

                        // Memory above 16MB, in 64KB blocks
                        let mem_above_16_blocks = if ram_bytes > 0x0100_0000 {
                            ((ram_bytes - 0x0100_0000) / 0x0001_0000) as u32
                        } else {
                            0
                        };

                        cpu.regs.eax = mem_1_to_16_kb;
                        cpu.regs.ebx = mem_above_16_blocks;
                        cpu.regs.ecx = mem_1_to_16_kb;
                        cpu.regs.edx = mem_above_16_blocks;
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);

                        tracing::debug!(
                            mem_1_to_16_kb,
                            mem_above_16_blocks,
                            "INT 15h E881: Get Extended Memory Size (32-bit)"
                        );
                    }
                    0x20 => {
                        // AX=E820h - Get System Memory Map (ACPI)
                        // Input:  EAX=E820h, EBX=continuation (0=first), ECX=buffer size,
                        //         EDX='SMAP' (534D4150h), ES:DI=buffer
                        // Output: EAX='SMAP', EBX=next (0=done), ECX=entry size, CF=0
                        // Entry:  base(8), length(8), type(4) [extended(4) optional]

                        const SMAP: u32 = 0x534D_4150;

                        // Verify signature
                        #[allow(clippy::if_not_else)]
                        if cpu.regs.edx != SMAP {
                            tracing::warn!("INT 15h E820: Invalid signature {:08X}", cpu.regs.edx);
                            cpu.regs.set_ah(0x86);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        } else {
                            let (entries, entry_count) = self.generate_e820_map(memory);
                            let entry_index = cpu.regs.ebx as usize;

                            if entry_index >= entry_count {
                                // Invalid continuation
                                tracing::warn!(
                                    entry_index,
                                    entry_count,
                                    "INT 15h E820: Invalid continuation value"
                                );
                                cpu.regs.set_ah(0x86);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            } else if cpu.regs.ecx < E820Entry::SIZE as u32 {
                                // Buffer too small
                                tracing::warn!(
                                    buffer_size = cpu.regs.ecx,
                                    required = E820Entry::SIZE,
                                    "INT 15h E820: Buffer too small"
                                );
                                cpu.regs.set_ah(0x86);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            } else {
                                let entry = &entries[entry_index];

                                // Write entry to buffer
                                entry.write_to_memory(memory, cpu.regs.es, cpu.regs.di());

                                // Set return values
                                cpu.regs.eax = SMAP;
                                cpu.regs.ecx = E820Entry::SIZE as u32;

                                // Next continuation or 0 if done
                                cpu.regs.ebx = if entry_index + 1 >= entry_count {
                                    0
                                } else {
                                    (entry_index + 1) as u32
                                };

                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);

                                tracing::debug!(
                                    entry_index,
                                    base = format_args!("{:#x}", entry.base),
                                    length = format_args!("{:#x}", entry.length),
                                    entry_type = entry.entry_type,
                                    next_ebx = cpu.regs.ebx,
                                    "INT 15h E820: Get System Memory Map"
                                );
                            }
                        }
                    }
                    _ => {
                        tracing::warn!(
                            al = format_args!("{:02X}", al),
                            "Unhandled INT 15h AH=E8 function"
                        );
                        cpu.regs.set_ah(0x86); // Function not supported
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // A20 gate (stubbed - A20 always enabled)
            0x24 => {
                let al = cpu.regs.al();
                match al {
                    0x00 => {
                        // Disable A20
                        memory.set_a20(false);
                        cpu.regs.set_ah(0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    0x01 => {
                        // Enable A20
                        memory.set_a20(true);
                        cpu.regs.set_ah(0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    0x02 => {
                        // Get A20 status
                        cpu.regs.set_al(u8::from(memory.a20_enabled()));
                        cpu.regs.set_ah(0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        cpu.regs.set_ah(0x86);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Event wait (set alarm)
            0x83 => {
                let al = cpu.regs.al();
                match al {
                    0x00 => {
                        // Set interval: CX:DX = microseconds, ES:BX = flag byte address
                        // When interval expires, bit 7 of flag byte is set.
                        if self.event_wait_active {
                            // Already active - return error (CF=1)
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        } else {
                            let micros = ((cpu.regs.cx() as u64) << 16) | (cpu.regs.dx() as u64);
                            let flag_addr =
                                Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
                            // Clear the flag byte's high bit initially
                            let current = memory.read_u8(flag_addr);
                            memory.write_u8(flag_addr, current & 0x7F);
                            self.event_wait_flag_addr = flag_addr;
                            self.event_wait_remaining_us = micros;
                            self.event_wait_active = true;
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                    }
                    0x01 => {
                        // Cancel wait interval
                        self.event_wait_active = false;
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        cpu.regs.set_ah(0x86);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Get system configuration
            0xC0 => {
                // Return pointer to system configuration table in ES:BX.
                // Through `load_data_segment`, not a bare selector store: in
                // real mode the interpreter resolves an ES-relative address
                // from `es_cache.base`, and only falls back to `selector << 4`
                // while that cache is still in its pristine reset shape. A
                // guest that has executed any MOV ES / LES has a non-zero
                // cache base, so a bare store leaves the OLD segment
                // authoritative and `es:[bx]` reads the wrong memory
                // (MDD-BUG-TEM-00067).
                cpu.load_data_segment(memory, gp64_cpu::DataSegReg::ES, 0xF000);
                cpu.regs.set_bx(Self::ROM_SYSTEM_CONFIG_TABLE_OFFSET);
                cpu.regs.set_ah(0x00); // Success
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get Extended BIOS Data Area (EBDA) segment
            0xC1 => {
                // This HLE machine has no EBDA. Keep this aligned with BDA
                // 0040:000E, INT 12h, E820, and the system config feature byte.
                cpu.regs.set_ah(0x86);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }

            // PS/2 Mouse functions
            0xC2 => {
                let al = cpu.regs.al();
                match al {
                    // C200h - Enable/Disable mouse
                    0x00 => {
                        let enable = cpu.regs.bh();
                        tracing::debug!(enable = enable, "INT 15h C200: Enable/Disable Mouse");
                        // BH=00 disable, BH=01 enable
                        // In our emulation, mouse is always enabled if present
                        cpu.regs.set_ah(0x00); // Success
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // C201h - Reset mouse
                    0x01 => {
                        tracing::debug!("INT 15h C201: Reset Mouse");
                        // Returns: BH = device ID (0=standard PS/2, 3=Intellimouse, 4=5-button)
                        cpu.regs.set_bh(0x00); // Standard PS/2 mouse
                        cpu.regs.set_ah(0x00); // Success
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // C202h - Set sample rate
                    0x02 => {
                        let rate = cpu.regs.bh();
                        tracing::debug!(rate_code = rate, "INT 15h C202: Set Sample Rate");
                        // BH = rate code: 0=10/sec, 1=20, 2=40, 3=60, 4=80, 5=100, 6=200
                        // We accept any rate code 0-6
                        if rate <= 6 {
                            cpu.regs.set_ah(0x00); // Success
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ah(0x02); // Invalid parameter
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }

                    // C203h - Set resolution
                    0x03 => {
                        let resolution = cpu.regs.bh();
                        tracing::debug!(resolution = resolution, "INT 15h C203: Set Resolution");
                        // BH = resolution: 0=1/mm, 1=2/mm, 2=4/mm, 3=8/mm
                        if resolution <= 3 {
                            cpu.regs.set_ah(0x00); // Success
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ah(0x02); // Invalid parameter
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }

                    // C204h - Get device ID
                    0x04 => {
                        tracing::debug!("INT 15h C204: Get Device ID");
                        // Returns: BH = device ID (0=standard, 3=Intellimouse, 4=5-button)
                        cpu.regs.set_bh(0x00); // Standard PS/2 mouse
                        cpu.regs.set_ah(0x00); // Success
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // C205h - Initialize pointing device interface
                    0x05 => {
                        let packet_size = cpu.regs.bh();
                        tracing::debug!(
                            packet_size = packet_size,
                            "INT 15h C205: Initialize Interface"
                        );
                        // BH = data package size (1-8 bytes per packet)
                        if (1..=8).contains(&packet_size) {
                            cpu.regs.set_ah(0x00); // Success
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ah(0x02); // Invalid parameter
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }

                    // C206h - Extended commands
                    0x06 => {
                        let subfunction = cpu.regs.bh();
                        tracing::debug!(
                            subfunction = subfunction,
                            "INT 15h C206: Extended Commands"
                        );
                        match subfunction {
                            0x00 => {
                                // Get device status
                                // Returns: BL = status byte 1, CL = resolution, DL = sample rate
                                cpu.regs.set_bl(0x00); // Status: stream mode, disabled, 1:1 scaling
                                cpu.regs.set_cl(0x02); // Resolution: 4 counts/mm
                                cpu.regs.set_dl(0x64); // Sample rate: 100 samples/sec
                                cpu.regs.set_ah(0x00);
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                            0x01 => {
                                // Set scaling 1:1
                                cpu.regs.set_ah(0x00);
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                            0x02 => {
                                // Set scaling 2:1
                                cpu.regs.set_ah(0x00);
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                            _ => {
                                cpu.regs.set_ah(0x02); // Invalid parameter
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            }
                        }
                    }

                    // C207h - Set device handler address
                    0x07 => {
                        let handler_seg = cpu.regs.es;
                        let handler_off = cpu.regs.bx();
                        tracing::debug!(
                            seg = format_args!("{:04X}", handler_seg),
                            off = format_args!("{:04X}", handler_off),
                            "INT 15h C207: Set Device Handler"
                        );
                        // ES:BX = far pointer to handler routine
                        // Handler callback storage is not modeled yet.
                        cpu.regs.set_ah(0x00); // Success
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    _ => {
                        tracing::warn!(
                            al = format_args!("{:02X}", al),
                            "Unhandled INT 15h/C2 PS/2 Mouse function"
                        );
                        cpu.regs.set_ah(0x01); // Invalid function
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // AH=BFh is a DOS extender kernel handshake, not a BIOS service.
            // A real BIOS reports this probe as unsupported.
            0xBF => {
                tracing::debug!(
                    al = format_args!("{:02X}", cpu.regs.al()),
                    "INT 15h AH=BF (extender kernel API probe) - unsupported, as on real hardware"
                );
                cpu.regs.set_ah(0x86); // Function not supported
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }

            // APM (Advanced Power Management)
            0x53 => {
                let al = cpu.regs.al();
                tracing::trace!(al = format_args!("{:02X}", al), "INT 15h AH=53 APM");
                match al {
                    // 5300h - APM Installation Check
                    0x00 => {
                        // BX = 0x504D ("PM") to indicate APM present
                        // AX = APM version (high=major, low=minor) - return 1.2
                        // CX = flags: bit 0 = 16-bit PM interface supported,
                        //             bit 1 = 32-bit PM interface supported
                        //             (APM 1.2 spec, function 5300h)
                        //
                        // Both are ZERO because we have no protected-mode APM
                        // entry point to offer. This service is reached only by
                        // dispatching a real-mode INT 15h and performing an HLE
                        // IRET; nothing installs a PM callback at the address
                        // 5302h/5303h would hand back, so a guest that connected
                        // would far-call into whatever happens to live there.
                        // Raise these bits only alongside a real bridge.
                        cpu.regs.set_bx(0x504D); // "PM"
                        cpu.regs.set_ax(0x0102); // APM version 1.2
                        cpu.regs.set_cx(0x0000); // No PM interface: real mode only
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // 5301h - APM Real Mode Interface Connect
                    0x01 => {
                        // Connect to APM in real mode
                        // BX should be 0x0000 (system BIOS power device ID)
                        cpu.regs.set_ah(0x00); // Success
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // 5302h - APM 16-bit Protected Mode Interface Connect
                    0x02 => {
                        // Refused, because there is no 16-bit PM entry point to
                        // return. This used to answer success with CS=F000 and a
                        // zero offset, which is not a callable APM bridge; a
                        // guest that connected would execute whatever bytes sat
                        // at F000:0000.
                        //
                        // APM 1.2 spec: 06h = 16-bit protected mode interface
                        // not supported. Matches the cleared bit 0 in 5300h's CX.
                        cpu.regs.set_ah(0x06);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                    // 5303h - APM 32-bit Protected Mode Interface Connect
                    0x03 => {
                        // Refused for the same reason as 5302h.
                        //
                        // APM 1.2 spec: 08h = 32-bit protected mode interface
                        // not supported. Matches the cleared bit 1 in 5300h's CX.
                        cpu.regs.set_ah(0x08);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                    // 5304h - APM Interface Disconnect
                    0x04 => {
                        cpu.regs.set_ah(0x00); // Success
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // 5305h - CPU Idle
                    0x05 => {
                        // Indicate CPU is idle - emulator can use this as hint
                        // In real hardware, this reduces power consumption
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // 5306h - CPU Busy
                    0x06 => {
                        // Indicate CPU is busy again
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // 5307h - Set Power State
                    0x07 => {
                        // BX = device ID, CX = power state
                        // Device 0001h with state 0003h = system suspend
                        // Device 0001h with state 0002h = system standby
                        // For emulation, just acknowledge
                        let device_id = cpu.regs.bx();
                        let power_state = cpu.regs.cx();
                        tracing::debug!(
                            device_id = format_args!("{:04X}", device_id),
                            power_state = format_args!("{:04X}", power_state),
                            "APM Set Power State"
                        );
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // 530Ah - Get Power Status
                    0x0A => {
                        // BX = device ID (0001h = system BIOS power device)
                        // Returns:
                        // BH = AC line status (00=off, 01=on, 02=on backup, FF=unknown)
                        // BL = battery status (00=high, 01=low, 02=critical, 03=charging,
                        //                      04=no battery, FF=unknown)
                        // CH = battery flag (bit 7=no battery, bit 3=charging,
                        //                    bits 0-2: high/low/critical)
                        // CL = remaining battery percentage (0-100, FF=unknown)
                        // DX = remaining battery time (minutes, FFFFh=unknown,
                        //      bit 15=1 means seconds)
                        // SI = number of battery units (APM 1.2+)
                        cpu.regs.set_bh(0x01); // AC power on
                        cpu.regs.set_bl(0xFF); // Battery status unknown (assume no battery)
                        cpu.regs.set_ch(0x80); // No system battery
                        cpu.regs.set_cl(0xFF); // Percentage unknown
                        cpu.regs.set_dx(0xFFFF); // Time unknown
                        cpu.regs.set_si(0x0000); // No batteries
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // 530Bh - Get PM Event
                    0x0B => {
                        // Returns power management event in BX
                        // For emulation, return "no events pending" (AH=80h)
                        cpu.regs.set_ah(0x80); // No events pending
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                    // 530Ch - Get Power State
                    0x0C => {
                        // BX = device ID
                        // Returns: CX = power state
                        cpu.regs.set_cx(0x0000); // Ready (fully operational)
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // 530Eh - APM Driver Version
                    0x0E => {
                        // BX = device ID (should be 0)
                        // CH:CL = APM driver version requested
                        // Returns: AX = APM connection version
                        let requested_major = cpu.regs.ch();
                        let requested_minor = cpu.regs.cl();
                        // Return the minimum of requested and supported (1.2)
                        let major = requested_major.min(1);
                        let minor = match major {
                            0 => requested_minor,
                            1 => requested_minor.min(2),
                            _ => 2,
                        };
                        cpu.regs.set_ah(major);
                        cpu.regs.set_al(minor);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // 5310h - Get Capabilities
                    0x10 => {
                        // Returns: BX = number of battery units
                        // CX = capabilities flags
                        cpu.regs.set_bx(0x0000); // No batteries
                        cpu.regs.set_cx(0x0000); // No special capabilities
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // 5308h/5309h - Enable/Disable Power Management / Restore Defaults
                    // 530Dh - Enable/Disable Device Power Management
                    // 530Fh - Engage/Disengage Power Management
                    // 5311h-5312h - (1.2) Engage/Disengage for specific device
                    // 5313h - Enable/Disable APM for OEM APM signature
                    0x08 | 0x09 | 0x0D | 0x0F | 0x11 | 0x12 | 0x13 => {
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        tracing::warn!(
                            al = format_args!("{:02X}", al),
                            "Unhandled INT 15h AH=53 APM function"
                        );
                        cpu.regs.set_ah(0x0C); // Function not supported
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Get CPU Type and Stepping
            // Used by diagnostic tools (HWINFO, AIDA, etc.)
            // Returns: AH=00, CH=CPU family (3=386, 4=486, 5=Pentium), CL=stepping
            0xC9 => {
                // MDD-BUG-CRUCIBLE-00600: report the CONFIGURED identity. This
                // used to answer CH=3 unconditionally, so a `--cpu 486` or
                // `--cpu pentium` guest saw a BIOS answer contradicting its own
                // instruction set and CPUID.
                //
                // CH is the family digit. Pre-386 parts have no code in this
                // function's vocabulary — it is a 386+-era call — so they get
                // this handler's standard "function not supported" answer
                // (AH=86h + CF), the same shape as the AH=10h TopView arm
                // below. That is a DECLARED choice for our HLE BIOS, not a
                // measured one: no pre-386 BIOS oracle was available to
                // adjudicate it, and a real XT/AT BIOS would simply have no
                // AH=C9h at all, which is what "unsupported" models.
                //
                // CL is the stepping. Zero is deliberate and uniform across
                // families: we model no specific silicon revision, and no
                // known guest branches on it.
                let family = match cpu.cpu_type {
                    gp64_cpu::CpuType::I80386 => Some(0x03),
                    gp64_cpu::CpuType::I80486 => Some(0x04),
                    gp64_cpu::CpuType::Pentium | gp64_cpu::CpuType::PentiumMmx => Some(0x05),
                    // Family 6. DECLARED, not measured — same standing as the
                    // families above (see the comment there): no BIOS oracle
                    // was consulted for what a real P6-era BIOS returns in CH,
                    // so this extends an unverified declaration consistently
                    // rather than adding a verified fact.
                    gp64_cpu::CpuType::PentiumPro
                    | gp64_cpu::CpuType::PentiumII
                    | gp64_cpu::CpuType::PentiumIII
                    | gp64_cpu::CpuType::PentiumIIICoppermine => Some(0x06),
                    gp64_cpu::CpuType::Pentium4 => Some(0x0F),
                    gp64_cpu::CpuType::I8086
                    | gp64_cpu::CpuType::I8088
                    | gp64_cpu::CpuType::I80186
                    | gp64_cpu::CpuType::I80286 => None,
                };

                match family {
                    Some(code) => {
                        cpu.regs.set_ah(0x00); // Success
                        cpu.regs.set_ch(code);
                        cpu.regs.set_cl(0x00); // Stepping: 0
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        tracing::debug!(
                            cpu_type = code,
                            stepping = 0,
                            "INT 15h C9h: Get CPU Type/Stepping"
                        );
                    }
                    None => {
                        cpu.regs.set_ah(0x86); // Function not supported
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        tracing::debug!(
                            cpu_type = ?cpu.cpu_type,
                            "INT 15h C9h: not supported on a pre-386 identity"
                        );
                    }
                }
            }

            // TopView API / BIOS-directed extended memory access
            // AH=10h: Open device (TopView) — not supported, return error
            0x10 => {
                cpu.regs.set_ah(0x86); // Function not supported
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                tracing::debug!(
                    al = format_args!("{:02X}", cpu.regs.al()),
                    "INT 15h AH=10h: TopView API not supported"
                );
            }

            // PS/2: Reset Processor / Joystick support probe
            // Not supported — return AH=86h, CF=1.
            0x0C => {
                cpu.regs.set_ah(0x86); // Function not supported
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                tracing::debug!("INT 15h AH=0Ch: not supported");
            }

            _ => {
                tracing::warn!(ah = format_args!("{:02X}", ah), "Unhandled INT 15h");
                cpu.regs.set_ah(0x86);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }
        }

        Ok((wait_micros, disp))
    }
}
