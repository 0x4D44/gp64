//! Console I/O, error handling, and address translation helpers for INT 21h

use super::*;

impl Dos {
    /// Write to the pre-opened STDAUX(3) / STDPRN(4) handles, returning how
    /// many bytes the bridge accepted.
    pub(crate) fn write_char_device(&mut self, handle: u16, buf: &[u8]) -> usize {
        let Ok(mut bridge) = self.device_bridge.lock() else {
            return 0;
        };
        if handle == 3 {
            bridge.write_aux(buf)
        } else {
            bridge.write_prn(buf)
        }
    }

    /// Write the single character of `AH=04h` (STDAUX) or `AH=05h` (STDPRN).
    ///
    /// These calls have nowhere to report a short write — there is no count
    /// register — and real DOS simply blocks until the device takes the byte.
    /// We cannot block, so the one thing we must not do is lose it in silence:
    /// a refused byte is counted and logged.
    pub(crate) fn write_char_device_byte(&mut self, handle: u16, byte: u8) {
        if self.write_char_device(handle, &[byte]) == 1 {
            return;
        }
        if let Ok(mut bridge) = self.device_bridge.lock() {
            bridge.note_dropped_char(if handle == 3 { "AUX" } else { "PRN" });
        }
    }

    /// Allocate an SFT entry for the `AUX` or `PRN` character device.
    ///
    /// `Ok(handle)`, or `Err(code)` with the DOS error when the handle table is
    /// full. The device object holds a clone of the shared bridge — for `PRN`
    /// its lifetime *is* the job boundary, since the bridge counts live device
    /// objects.
    pub(crate) fn open_char_device(
        &mut self,
        device: &str,
        share_mode: u8,
        access_mode: u8,
    ) -> Result<u16, u16> {
        let bridge = self.device_bridge.clone();
        // The PRN open is counted only once the device really has a handle. A
        // failed allocation drops the device on the spot, and counting in the
        // constructor made that drop take the open count 1 -> 0 — emitting a job
        // boundary that flushed whatever an unrelated, still-open `PRN` handle
        // had written so far.
        let (file, ticket): (Box<dyn crate::filesystem::VirtualFile>, _) = if device == "AUX" {
            (Box::new(crate::filesystem::AuxDevice::new(bridge)), None)
        } else {
            let (prn, ticket) = crate::filesystem::PrnDevice::new(bridge);
            (Box::new(prn), Some(ticket))
        };
        match self.file_table.allocate_with_sharing(
            file,
            device.to_string(),
            self.current_psp,
            share_mode,
            access_mode & 0x07,
        ) {
            Some(handle) => {
                if let Some(ticket) = ticket {
                    ticket.confirm();
                }
                Ok(handle)
            }
            None => Err(4), // Too many open files
        }
    }

    /// Open any DOS character device by its `parse_device_name` identity, for
    /// the create and extended-open calls that must reach the device rather
    /// than the host filesystem.
    ///
    /// `AH=3Ch` is the route that matters: every DOS C runtime opens `PRN` for
    /// output with `fopen("PRN", "w")`, which is a create, and so does `>PRN`
    /// redirection. A create that resolved a path first would drop a host file
    /// literally named `PRN` into the guest's directory instead of printing.
    ///
    /// `Ok(handle)`, or `Err(code)` carrying the DOS error. `CON` yields the
    /// same `0x00FE` marker `AH=3Dh` returns, which `resolve_alias_handle` folds
    /// onto stdin/stdout; it has no SFT entry.
    pub(crate) fn open_named_device(
        &mut self,
        device: &str,
        share_mode: u8,
        access_mode: u8,
    ) -> Result<u16, u16> {
        match device {
            "NUL" => {
                let nul: Box<dyn crate::filesystem::VirtualFile> =
                    Box::new(crate::filesystem::NulDevice);
                self.file_table
                    .allocate_with_sharing(
                        nul,
                        "NUL".to_string(),
                        self.current_psp,
                        share_mode,
                        access_mode & 0x07,
                    )
                    .ok_or(4) // Too many open files
            }
            "CON" => Ok(0x00FE),
            "AUX" | "PRN" => self.open_char_device(device, share_mode, access_mode),
            // `parse_device_name` yields nothing else; an unknown name is
            // reported absent rather than silently routed to a device.
            _ => Err(2),
        }
    }

    /// Apply an [`open_named_device`](Self::open_named_device) result to the
    /// caller's registers: AX = handle with CF clear, or AX = error with CF set.
    pub(crate) fn set_device_open_return(&mut self, cpu: &mut Cpu, result: Result<u16, u16>) {
        match result {
            Ok(handle) => {
                self.clear_extended_error();
                cpu.regs.set_ax(handle);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }
            Err(code) => {
                self.set_extended_error(code);
                cpu.regs.set_ax(code);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }
        }
    }

    /// DOS API register width follows the current caller's code segment, not
    /// 16-bit protected-mode helper stubs inside an otherwise 32-bit client.
    #[inline]
    pub(crate) fn dos_api_32bit(&self, cpu: &Cpu) -> bool {
        self.dpmi.in_protected_mode() && !self.simulated_real_mode_hle && cpu.regs.cs_cache.big
    }

    #[inline]
    pub(crate) fn int21_effective_protected_mode(&self, cpu: &Cpu) -> bool {
        cpu.is_protected_mode() && !cpu.regs.is_v86_mode() && !self.simulated_real_mode_hle
    }

    /// A PM INT 21h call from a live DPMI client, whose PSP and DOS-data
    /// pointers the host translates to selectors.
    pub(crate) fn dpmi_client_int21(&self, cpu: &Cpu) -> bool {
        self.int21_effective_protected_mode(cpu) && self.dpmi.in_protected_mode()
    }

    /// INT 21h 51h/62h: the current PSP as the caller names it — a selector
    /// for a DPMI client, the segment otherwise.
    pub(crate) fn current_psp_for_caller(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> u16 {
        let segment = self.current_psp;
        if !self.dpmi_client_int21(cpu) {
            return segment;
        }
        self.dpmi
            .data_selector_for_segment(cpu, memory, segment)
            .unwrap_or(segment)
    }

    /// Set extended error information for INT 21h/59h
    /// Maps DOS error codes to appropriate error class, action, and locus
    pub fn set_extended_error(&mut self, error_code: u16) {
        // Error classes: 01=out of resource, 02=temp situation, 03=auth problem,
        // 04=internal error, 05=hw failure, 06=system failure, 07=app error,
        // 08=not found, 09=bad format, 0A=locked, 0B=media error, 0C=already exists
        // Suggested actions: 01=retry, 02=delay retry, 03=user input, 04=abort w/cleanup,
        // 05=immediate abort, 06=ignore, 07=retry after user intervention
        // Locus: 01=unknown, 02=block device, 03=network, 04=serial, 05=memory
        let (class, action, locus) = match error_code {
            0 => (0, 0, 0),           // No error
            1 => (0x07, 0x04, 0x01),  // Invalid function - app error, abort, unknown
            2 => (0x08, 0x03, 0x02),  // File not found - not found, user input, disk
            3 => (0x08, 0x03, 0x02),  // Path not found - not found, user input, disk
            4 => (0x01, 0x04, 0x01),  // Too many open files - out of resource, abort
            5 => (0x03, 0x04, 0x02),  // Access denied - auth problem, abort, disk
            6 => (0x07, 0x04, 0x01),  // Invalid handle - app error, abort
            7 => (0x04, 0x05, 0x05),  // MCB destroyed - internal error, abort, memory
            8 => (0x01, 0x04, 0x05),  // Insufficient memory - out of resource, abort, memory
            9 => (0x07, 0x04, 0x05),  // Invalid memory block - app error, abort, memory
            10 => (0x07, 0x04, 0x05), // Invalid environment - app error, abort, memory
            11 => (0x09, 0x04, 0x02), // Invalid format - bad format, abort, disk
            12 => (0x07, 0x04, 0x02), // Invalid access code - app error, abort, disk
            13 => (0x07, 0x04, 0x01), // Invalid data - app error, abort
            15 => (0x07, 0x03, 0x02), // Invalid drive - app error, user input, disk
            16 => (0x07, 0x04, 0x02), // Cannot remove current dir - app error, abort, disk
            17 => (0x02, 0x01, 0x02), // Not same device - temp situation, retry, disk
            // MS-DOS v4.0/src/DOS/MS_TABLE.ASM ErrTab entry for
            // error_no_more_files is NotFnd/User/Disk, just like errors 2/3.
            18 => (0x08, 0x03, 0x02), // No more files - not found, user input, disk
            // MS-DOS v4.0/src/DOS/MS_TABLE.ASM ERR_TABLE_24 (13h-1Fh).
            // FFh loci are call-specific in DOS. Our modeled critical I/O is
            // block-device I/O, so retain Disk for those entries.
            19 => (0x0B, 0x07, 0x02), // Write protect - media, intervention, disk
            20 => (0x04, 0x05, 0x01), // Unknown unit - internal, panic, unknown
            21 => (0x05, 0x07, 0x02), // Not ready - hardware, intervention, disk
            22 => (0x04, 0x05, 0x01), // Unknown command - internal, panic, unknown
            23 => (0x0B, 0x04, 0x02), // CRC - media, abort, disk
            24 => (0x04, 0x05, 0x01), // Bad length - internal, panic, unknown
            25 => (0x05, 0x01, 0x02), // Seek - hardware, retry, disk
            26 => (0x0B, 0x07, 0x02), // Unknown media - media, intervention, disk
            27 => (0x0B, 0x04, 0x02), // Sector not found - media, abort, disk
            28 => (0x02, 0x07, 0x04), // Out of paper - temporary, intervention, serial
            29 => (0x05, 0x04, 0x02), // Write fault - hardware, abort, disk
            30 => (0x05, 0x04, 0x02), // Read fault - hardware, abort, disk
            31 => (0x0D, 0x04, 0x02), // General failure - unknown, abort, disk
            32 => (0x0A, 0x02, 0x02), // Sharing violation - locked, delay retry, disk
            33 => (0x0A, 0x02, 0x02), // Lock violation - locked, delay retry, disk
            // Error codes 34-49 (sharing/network errors)
            34 => (0x0A, 0x04, 0x02), // Wrong disk change - locked, abort, disk
            35 => (0x01, 0x04, 0x02), // FCB unavailable - out of resource, abort, disk
            36 => (0x0A, 0x02, 0x02), // Sharing buffer overflow - locked, delay retry, disk
            37..=49 => (0x0D, 0x04, 0x03), // Reserved network errors - unknown, abort, network
            // Error codes 50-79 (network errors)
            50 => (0x02, 0x04, 0x03), // Network not supported - temp, abort, network
            51 => (0x02, 0x01, 0x03), // Remote not listening - temp, retry, network
            52 => (0x02, 0x01, 0x03), // Duplicate name on network - temp, retry, network
            53 => (0x08, 0x01, 0x03), // Network name not found - not found, retry, network
            54 => (0x02, 0x04, 0x03), // Network busy - temp, abort, network
            55 => (0x08, 0x04, 0x03), // Device no longer exists - not found, abort, network
            56 => (0x01, 0x04, 0x03), // NetBIOS cmd limit exceeded - out of resource, abort, network
            57 => (0x05, 0x04, 0x03), // Network adapter hw error - hw failure, abort, network
            58 => (0x02, 0x01, 0x03), // Network incorrect response - temp, retry, network
            59 => (0x02, 0x01, 0x03), // Unexpected network error - temp, retry, network
            60 => (0x02, 0x01, 0x03), // Incompatible remote adapter - temp, retry, network
            61 => (0x01, 0x04, 0x03), // Print queue full - out of resource, abort, network
            62 => (0x01, 0x04, 0x03), // Queue not full - out of resource, abort, network
            63 => (0x01, 0x04, 0x03), // Not enough space for print - out of resource, abort, network
            64 => (0x08, 0x04, 0x03), // Network name deleted - not found, abort, network
            65 => (0x03, 0x04, 0x03), // Access denied - auth, abort, network
            66 => (0x07, 0x04, 0x03), // Network device type incorrect - app error, abort, network
            67 => (0x08, 0x04, 0x03), // Network name not found - not found, abort, network
            68 => (0x01, 0x04, 0x03), // Network name limit exceeded - out of resource, abort, network
            69 => (0x01, 0x04, 0x03), // NetBIOS session limit exceeded - out of resource, abort, network
            70 => (0x02, 0x04, 0x03), // Temporarily paused - temp, abort, network
            71 => (0x02, 0x04, 0x03), // Network request not accepted - temp, abort, network
            72 => (0x02, 0x04, 0x03), // Redir paused - temp, abort, network
            73..=79 => (0x0D, 0x04, 0x03), // Reserved network - unknown, abort, network
            // Error codes 80-89
            80 => (0x0C, 0x04, 0x02), // File already exists - already exists, abort, disk
            81 => (0x0D, 0x04, 0x02), // Reserved (81) - unknown, abort, disk
            82 => (0x01, 0x04, 0x02), // Cannot make directory - out of resource, abort, disk
            83 => (0x0B, 0x05, 0x02), // Fail on INT 24 - media error, abort, disk
            84 => (0x01, 0x04, 0x03), // Too many redirections - out of resource, abort, network
            85 => (0x0C, 0x04, 0x03), // Duplicate redirection - already exists, abort, network
            86 => (0x07, 0x04, 0x03), // Invalid password - app error, abort, network
            87 => (0x07, 0x04, 0x01), // Invalid parameter - app error, abort, unknown
            88 => (0x05, 0x04, 0x03), // Network device fault - hw failure, abort, network
            89 => (0x02, 0x04, 0x03), // Function not supported by network - temp, abort, network
            _ => (0x0D, 0x04, 0x01),  // Unknown error
        };
        self.extended_error = (error_code, class, action, locus);
    }

    /// Clear extended error (call after successful operations)
    pub(crate) fn clear_extended_error(&mut self) {
        self.extended_error = (0, 0, 0, 0);
    }

    /// Helper to set error return: sets AX, carry flag, and extended error info
    pub(crate) fn set_error_return(&mut self, cpu: &mut Cpu, error_code: u16) {
        cpu.regs.set_ax(error_code);
        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
        self.set_extended_error(error_code);
    }

    /// Whether the caller's IRET frame sits at `SS*16 + SP`, where these
    /// helpers address it: true for a real-mode or V86 caller. A
    /// protected-mode caller's SS is a selector, so that address is unrelated
    /// memory; its frame (16- or 32-bit) belongs to the HLE return, which
    /// merges the handler's flags itself.
    fn caller_frame_is_real_mode(cpu: &Cpu) -> bool {
        !cpu.is_protected_mode() || cpu.regs.is_v86_mode()
    }

    /// Sync carry flag to the IRET frame on the stack.
    ///
    /// When `skip_iret_sync` is set (DPMI HLE path), the stack holds
    /// protected-mode data, not an IRET frame — skip all writes. A
    /// protected-mode caller's frame is never written here either: writing
    /// `SS*16 + SP + 4` would change bit 0 of unrelated real-mode memory.
    pub fn sync_iret_carry(cpu: &Cpu, memory: &mut Memory, skip: bool) {
        if skip || !Self::caller_frame_is_real_mode(cpu) {
            return;
        }
        let flags_addr = cpu.regs.sp().wrapping_add(4);
        let mut flags_on_stack = memory.read_u16_seg(cpu.regs.ss, flags_addr);
        if cpu.regs.flags.contains(gp64_cpu::Flags::CARRY) {
            flags_on_stack |= 1;
        } else {
            flags_on_stack &= !1;
        }
        memory.write_u16_seg(cpu.regs.ss, flags_addr, flags_on_stack);
    }

    /// Rewind INT 21h and ensure IF is set so IRQ1 can deliver keyboard input.
    ///
    /// When `skip` is true (DPMI HLE path), the stack holds protected-mode
    /// data, not an IRET frame — skip all stack writes. The DPMI dispatcher
    /// handles input-wait loopback itself.
    pub(crate) fn prep_input_wait(cpu: &mut Cpu, memory: &mut Memory, skip: bool) {
        if skip || !Self::caller_frame_is_real_mode(cpu) {
            // Still set IF so IRQ1 can deliver keys.
            cpu.interrupts_enabled = true;
            cpu.regs.flags.insert(gp64_cpu::Flags::INTERRUPT);
            return;
        }
        let ret_ip = memory.read_u16_seg(cpu.regs.ss, cpu.regs.sp());
        memory.write_u16_seg(cpu.regs.ss, cpu.regs.sp(), ret_ip.wrapping_sub(2));

        cpu.interrupts_enabled = true;
        cpu.regs.flags.insert(gp64_cpu::Flags::INTERRUPT);

        let flags_addr = cpu.regs.sp().wrapping_add(4);
        let flags = memory.read_u16_seg(cpu.regs.ss, flags_addr);
        memory.write_u16_seg(cpu.regs.ss, flags_addr, flags | 0x0200);
    }

    pub(crate) fn wait_for_input(
        cpu: &mut Cpu,
        memory: &mut Memory,
        skip: bool,
    ) -> Int21Disposition {
        Self::prep_input_wait(cpu, memory, skip);
        // Model blocking DOS console input by halting until an interrupt arrives.
        // Without this, programs that poll AH=07/08 via re-executed INT 21h
        // spin aggressively and become host-CPU bound, making menus feel sticky.
        cpu.halted = true;
        Self::iret_with_carry(cpu, memory, skip)
    }

    pub(crate) fn iret_with_carry(cpu: &Cpu, memory: &mut Memory, skip: bool) -> Int21Disposition {
        Self::sync_iret_carry(cpu, memory, skip);
        Int21Disposition::Iret
    }

    /// Common keyboard input logic for INT 21h AH=01/06/07/08.
    ///
    /// Drains the keyboard controller, checks for a pending extended scancode,
    /// dequeues from the BDA keyboard buffer, handles extended keys, and
    /// optionally checks Ctrl-C and echoes the character.
    ///
    /// On `KeyboardReadResult::Ready`, AL is already set with the character.
    /// On `KeyboardReadResult::CtrlC`, "^C" has been echoed and INT 23h triggered.
    /// On `KeyboardReadResult::NoKey`, nothing has been modified.
    pub(crate) fn read_keyboard_char(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        _keyboard: &mut Keyboard,
        _pic: &mut Pic,
        vga: &mut Vga,
        echo: bool,
        check_ctrl_c: bool,
    ) -> KeyboardReadResult {
        // Check for pending extended scan code first
        if let Some(scancode) = self.pending_extended_scancode.take() {
            cpu.regs.set_al(scancode);
            // Don't echo extended key scan codes
            KeyboardReadResult::Ready
        } else if let Some((sc, ascii)) = bda::kbd_dequeue(memory) {
            // Check for Ctrl-C (ASCII 3) if requested.
            // Console input functions (AH=01/08) always check; the break flag
            // only affects non-console I/O.
            if check_ctrl_c && ascii == 0x03 {
                // Echo "^C" to console per DOS behavior
                vga.teletype(b'^', 0, memory);
                vga.teletype(b'C', 0, memory);
                Self::trigger_ctrl_c(cpu, memory);
                return KeyboardReadResult::CtrlC;
            }
            if ascii == 0 || ascii == 0xE0 {
                // Extended key: return prefix now, save scan code for next call
                self.pending_extended_scancode = Some(sc);
                cpu.regs.set_al(ascii);
                // Don't echo extended prefix
            } else {
                cpu.regs.set_al(ascii);
                if echo {
                    vga.teletype(ascii, 0, memory);
                }
            }
            KeyboardReadResult::Ready
        } else {
            KeyboardReadResult::NoKey
        }
    }

    /// Return the DS:DX offset appropriate for the current CPU mode.
    /// 32-bit DPMI clients use EDX; real mode and 16-bit DPMI use DX.
    #[inline]
    pub(crate) fn dos_offset_dx(&self, cpu: &Cpu) -> u32 {
        if self.dos_api_32bit(cpu) {
            cpu.regs.edx
        } else {
            cpu.regs.dx() as u32
        }
    }

    /// Return the ES:DI or DS:DI offset appropriate for the current CPU mode.
    /// 32-bit DPMI clients use EDI; real mode and 16-bit DPMI use DI.
    #[inline]
    pub(crate) fn dos_offset_di(&self, cpu: &Cpu) -> u32 {
        if self.dos_api_32bit(cpu) {
            cpu.regs.edi
        } else {
            cpu.regs.di() as u32
        }
    }

    /// Return the DS:SI offset appropriate for the current CPU mode.
    /// 32-bit DPMI clients use ESI; real mode and 16-bit DPMI use SI.
    #[inline]
    pub(crate) fn dos_offset_si(&self, cpu: &Cpu) -> u32 {
        if self.dos_api_32bit(cpu) {
            cpu.regs.esi
        } else {
            cpu.regs.si() as u32
        }
    }

    /// Return the byte count from CX/ECX appropriate for the current CPU mode.
    /// 32-bit DPMI clients use ECX; real mode and 16-bit DPMI use CX.
    #[inline]
    pub(crate) fn dos_count_cx(&self, cpu: &Cpu) -> u32 {
        if self.dos_api_32bit(cpu) {
            cpu.regs.ecx
        } else {
            cpu.regs.cx() as u32
        }
    }

    /// Set a byte-count result in AX (real/16-bit) or EAX (32-bit DPMI).
    /// Used by INT 21h/3Fh (read) and 40h (write) to return bytes transferred.
    #[inline]
    pub(crate) fn dos_set_result_count(&self, cpu: &mut Cpu, count: usize) {
        if self.dos_api_32bit(cpu) {
            cpu.regs.eax = count as u32;
        } else {
            cpu.regs.set_ax(count as u16);
        }
    }

    /// Translate a guest file-transfer buffer into physical page runs.
    ///
    /// MDD-BUG-KIL-01018: INT 21h AH=3Fh/40h used to resolve DS:EDX with
    /// `dos_translate_ds_dx` — which is segment base plus offset and applies NO
    /// paging — and then move bytes with raw `Memory::read_u8`/`write_u8` at
    /// that LINEAR address. For a 32-bit DPMI client with paging enabled that
    /// is simply the wrong memory: under a non-identity mapping the transfer
    /// silently read or overwrote whatever happened to live at the linear
    /// address, and it bypassed the page-present, page-access and
    /// segment-limit checks entirely.
    ///
    /// Translation is done for the WHOLE span before any byte moves, so a
    /// faulting buffer transfers nothing rather than leaving the guest with a
    /// half-written buffer and an advanced file position. The fault itself
    /// propagates as `CpuError::PageFault` through `int21`'s `Result`, which is
    /// the emulator's defined fault path.
    ///
    /// Each run is `(physical base, length)` and never crosses a page boundary,
    /// because a page boundary is exactly where the physical mapping may jump.
    pub(crate) fn dos_guest_buffer_runs(
        &self,
        cpu: &Cpu,
        memory: &mut Memory,
        segment: u16,
        offset: u32,
        len: usize,
        access: gp64_cpu::PageAccess,
    ) -> Result<Vec<(usize, usize)>> {
        let mut runs = Vec::new();
        let mut done = 0_usize;
        while done < len {
            let chunk_offset = offset.wrapping_add(done as u32);
            // `translate_address_full_checked` enforces the segment limit
            // (including expand-down) and then the page walk with `access`.
            let physical =
                cpu.translate_address_full_checked(memory, segment, chunk_offset, access)?;
            // The run may extend only to the end of this page: the next page can
            // map anywhere, or not be present at all.
            let linear = cpu.translate_address(segment, chunk_offset);
            let page_remaining = 0x1000 - (linear & 0xfff);
            let run = page_remaining.min(len - done);
            runs.push((physical, run));
            done += run;
        }
        Ok(runs)
    }

    /// Translate DS:DX to a linear address, using the correct offset width.
    #[inline]
    pub(crate) fn dos_translate_ds_dx(&self, cpu: &Cpu) -> usize {
        cpu.translate_address(cpu.regs.ds, self.dos_offset_dx(cpu))
    }

    /// Translate DS:SI to a linear address, using the correct offset width.
    #[inline]
    pub(crate) fn dos_translate_ds_si(&self, cpu: &Cpu) -> usize {
        cpu.translate_address(cpu.regs.ds, self.dos_offset_si(cpu))
    }

    /// Translate ES:DI to a linear address, using the correct offset width.
    #[inline]
    pub(crate) fn dos_translate_es_di(&self, cpu: &Cpu) -> usize {
        cpu.translate_address(cpu.regs.es, self.dos_offset_di(cpu))
    }

    /// Which "not found" a missing DOS path is: error 2 (file not found) when
    /// the directory holding it exists, error 3 (path not found) when a
    /// directory component does not. Period installers branch on the
    /// difference to decide whether they must MKDIR first, so collapsing both
    /// onto 2 tells them the wrong thing.
    ///
    /// The question deliberately goes through the `Drive` trait rather than
    /// the host filesystem: one drive implementation is backed by a FAT disk
    /// image and has no host path to inspect at all.
    pub(crate) fn missing_dos_path_error(&mut self, drive: u8, path: &str) -> u16 {
        let Some(parent) = crate::files::PathResolver::dos_parent_dir(path) else {
            // No directory component below the root, which always exists.
            return 2;
        };
        let parent = parent.to_string();
        let parent_is_dir = self
            .drives
            .get_mut(&drive)
            .and_then(|d| d.get_attributes(&parent).ok())
            .is_some_and(|attrs| attrs.directory);
        if parent_is_dir {
            2
        } else {
            3
        }
    }

    /// Classify an I/O error as either a normal DOS error (returns Iret with error set)
    /// or a critical error (returns CriticalError for INT 24h dispatch).
    ///
    /// INT 24h is only invoked for hardware-level I/O errors (codes 19-31).
    /// Non-critical errors (file not found, access denied, etc.) return immediately.
    pub(crate) fn classify_io_error(
        &mut self,
        cpu: &mut Cpu,
        memory: &Memory,
        io_err: &std::io::Error,
        ah_function: u8,
        handle: Option<u16>,
        is_write: bool,
        // MDD-BUG-CRUCIBLE-00381: taken BY VALUE, not pre-boxed. The box is
        // still wanted inside `Int21Disposition::CriticalError` - `Registers`
        // is 35+ fields including six `SegmentCache` structs, so inlining it
        // would bloat every disposition value - but this function only ever
        // runs on the error path, so that is where the allocation belongs.
        entry_snapshot: gp64_cpu::Registers,
    ) -> Int21Disposition {
        use std::io::ErrorKind;

        let (dos_error, is_critical) = match io_err.kind() {
            ErrorKind::PermissionDenied => (5u16, false),
            ErrorKind::NotFound => (2, false),
            ErrorKind::AlreadyExists => (80, false),
            ErrorKind::WriteZero => (29, true),
            ErrorKind::UnexpectedEof => (30, true),
            _ => (5, false), // Unknown errors → access denied, NOT critical
        };

        if !is_critical {
            self.set_error_return(cpu, dos_error);
            return Int21Disposition::Iret;
        }

        // Guard: recursive critical error. If already inside an INT 24h handler
        // (critical error flag set), force Fail instead of re-invoking.
        // Real DOS does the same to prevent infinite nesting.
        let crit_flag_addr = Memory::seg_offset_to_linear(INDOS_SEGMENT, CRITICAL_ERROR_OFFSET);
        let crit_flag = memory.read_u8(crit_flag_addr);
        if crit_flag != 0 {
            self.set_error_return(cpu, dos_error);
            return Int21Disposition::Iret;
        }

        // Derive drive letter from SFT entry's path
        let drive = self.drive_from_handle(handle);

        Int21Disposition::CriticalError {
            ah_function,
            drive,
            error_code: dos_error as u8,
            is_write,
            entry_snapshot: Box::new(entry_snapshot),
        }
    }

    /// A path call (`ah_function`) on `drive`, which is in
    /// [`Dos::not_ready_drives`]: a fitted drive with no media. DOS sets
    /// extended error 15h (drive not ready) and raises a critical error for
    /// the session's handler; the registers are those the call came in with
    /// (extended error 15h, class/action 0507h). Inside a
    /// critical-error handler already, it fails at once with 15h, as
    /// [`Self::classify_io_error`] does.
    pub(crate) fn not_ready_drive_error(
        &mut self,
        cpu: &mut Cpu,
        memory: &Memory,
        ah_function: u8,
        drive: u8,
    ) -> Int21Disposition {
        const DRIVE_NOT_READY: u16 = 0x15;
        let crit_flag_addr = Memory::seg_offset_to_linear(INDOS_SEGMENT, CRITICAL_ERROR_OFFSET);
        if memory.read_u8(crit_flag_addr) != 0 {
            self.set_error_return(cpu, DRIVE_NOT_READY);
            return Int21Disposition::Iret;
        }
        self.set_extended_error(DRIVE_NOT_READY);
        Int21Disposition::CriticalError {
            ah_function,
            drive,
            error_code: DRIVE_NOT_READY as u8,
            is_write: false,
            entry_snapshot: Box::new(cpu.regs.clone()),
        }
    }

    /// Extract the drive number from the file handle's SFT path.
    /// Returns 0=A, 1=B, 2=C, etc. Defaults to 2 (C:) if no handle or no drive letter.
    pub(crate) fn drive_from_handle(&self, handle: Option<u16>) -> u8 {
        if let Some(h) = handle {
            if let Some(sft) = self.file_table.get_sft(h) {
                let bytes = sft.path.as_bytes();
                if bytes.len() >= 2 && bytes[1] == b':' {
                    let ch = bytes[0].to_ascii_uppercase();
                    if ch.is_ascii_uppercase() {
                        return ch - b'A';
                    }
                }
            }
        }
        2 // Default: C:
    }
}

#[cfg(test)]
mod guest_buffer_span_tests {
    use super::*;
    use gp64_cpu::PageAccess;

    const PAGE_DIRECTORY: usize = 0x0010_0000;
    const PAGE_TABLE: usize = 0x0010_1000;
    /// Where the guest *thinks* its buffer is.
    const LINEAR_BUFFER: u32 = 0x0020_0000;
    /// Where it actually lives — deliberately NOT the linear address, which is
    /// the whole point: MDD-BUG-KIL-01018 wrote to the linear one.
    const PHYSICAL_PAGE_0: usize = 0x0030_0000;
    const PHYSICAL_PAGE_1: usize = 0x0035_0000;

    const PAGE_PRESENT: u32 = 1;
    const PAGE_WRITABLE: u32 = 2;
    const PAGE_USER: u32 = 4;

    /// A 32-bit protected-mode guest with paging on and a NON-IDENTITY mapping:
    /// the two pages at `LINEAR_BUFFER` map to two widely separated, and
    /// deliberately out-of-order, physical pages.
    fn paged_guest() -> (Cpu, Memory) {
        let mut memory = Memory::new();
        let mut cpu = Cpu::new();

        cpu.regs.cr0 = 0x8000_0001; // PE | PG
        cpu.regs.cr3 = PAGE_DIRECTORY as u32;
        // A DPMI client runs at CPL 3, and the privilege level is load-bearing:
        // at CPL 0 with CR0.WP clear a supervisor write to a read-only page is
        // architecturally PERMITTED, so a ring-0 fixture would quietly fail to
        // exercise the page-access check at all.
        cpu.regs.cpl = 3;

        // One page directory entry covering the buffer's 4 MiB region.
        let directory_index = (LINEAR_BUFFER >> 22) as usize;
        memory.write_u32(
            PAGE_DIRECTORY + directory_index * 4,
            PAGE_TABLE as u32 | PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER,
        );
        let table_index = ((LINEAR_BUFFER >> 12) & 0x3ff) as usize;
        memory.write_u32(
            PAGE_TABLE + table_index * 4,
            PHYSICAL_PAGE_0 as u32 | PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER,
        );
        memory.write_u32(
            PAGE_TABLE + (table_index + 1) * 4,
            PHYSICAL_PAGE_1 as u32 | PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER,
        );

        // A flat 32-bit DS covering the whole address space.
        cpu.regs.ds = 0x10;
        cpu.regs.ds_cache.base = 0;
        cpu.regs.ds_cache.limit = 0x000f_ffff;
        cpu.regs.ds_cache.cached_effective_limit = 0xffff_ffff;
        cpu.regs.ds_cache.granularity = true;
        cpu.regs.ds_cache.big = true;
        cpu.regs.ds_cache.present = true;
        cpu.regs.ds_cache.is_writable = true;
        cpu.regs.ds_cache.access = 0x92;
        (cpu, memory)
    }

    fn clear_page_table_entry(memory: &mut Memory, page: u32, value: u32) {
        let table_index = ((LINEAR_BUFFER >> 12) & 0x3ff) as usize + page as usize;
        memory.write_u32(PAGE_TABLE + table_index * 4, value);
    }

    /// `int21` returns `anyhow::Result`, so a `CpuError` arrives boxed.
    fn is_page_fault(error: &anyhow::Error) -> bool {
        matches!(
            error.downcast_ref::<gp64_cpu::CpuError>(),
            Some(gp64_cpu::CpuError::PageFault(_))
        )
    }

    /// MDD-BUG-KIL-01018: the transfer must land on the PHYSICAL page, not the
    /// linear address.
    ///
    /// Red proof: resolve the buffer with `dos_translate_ds_dx` and write with
    /// raw `Memory::write_u8` — the pre-fix shape — and the bytes appear at
    /// `LINEAR_BUFFER` while `PHYSICAL_PAGE_0` stays zero, failing both halves.
    #[test]
    fn a_non_identity_mapping_transfers_through_the_physical_page() {
        let dos = Dos::new();
        let (cpu, mut memory) = paged_guest();

        let runs = dos
            .dos_guest_buffer_runs(
                &cpu,
                &mut memory,
                cpu.regs.ds,
                LINEAR_BUFFER,
                4,
                PageAccess::Write,
            )
            .expect("a present, writable page translates");
        assert_eq!(runs, vec![(PHYSICAL_PAGE_0, 4)]);

        for (index, &(physical, _)) in runs.iter().enumerate() {
            memory.write_u8(physical, 0xAB + index as u8);
        }
        assert_eq!(memory.read_u8(PHYSICAL_PAGE_0), 0xAB);
        assert_eq!(
            memory.read_u8(LINEAR_BUFFER as usize),
            0,
            "the linear address must be untouched — writing there was the bug"
        );
    }

    /// A protected-mode caller's SS is a selector: `SS*16 + SP + 4` is
    /// unrelated real-mode memory, never its FLAGS. Carry synchronization
    /// must leave a synthetic sentinel at that aliased address unchanged.
    #[test]
    fn int21_leaves_real_mode_memory_alone_for_a_protected_mode_caller() {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        cpu.regs.cr0 |= 1;
        cpu.regs.ss = 0x0010;
        cpu.regs.set_sp(0x0200);
        let aliased_flags = (0x0010 << 4) + 0x0200 + 4;
        memory.write_u16(aliased_flags, 0xA5A5);

        cpu.regs.set_ah(0x30);
        let (mut vga, mut keyboard) = crate::tests::create_test_io();
        dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut gp64_hw::pic::Pic::new(),
        )
        .unwrap();

        assert_eq!(memory.read_u16(aliased_flags), 0xA5A5);
    }

    /// MDD-BUG-CRU-01406: a device read (here STDIN's completed line) lands on
    /// the PHYSICAL page too, as a file read does, not at the linear address.
    #[test]
    fn a_stdin_read_lands_through_the_physical_page() {
        let mut dos = Dos::new();
        dos.skip_iret_sync = true;
        let (mut cpu, mut memory) = paged_guest();
        cpu.regs.ds_cache.base = LINEAR_BUFFER;
        dos.stdin_line_buf = b"HELLO".to_vec();

        cpu.regs.set_ah(0x3F);
        cpu.regs.set_bx(0);
        cpu.regs.set_cx(5);
        cpu.regs.set_dx(0);
        let (mut vga, mut keyboard) = crate::tests::create_test_io();
        dos.int21(
            &mut cpu,
            &mut memory,
            &mut vga,
            &mut keyboard,
            &mut gp64_hw::pic::Pic::new(),
        )
        .unwrap();

        assert_eq!(cpu.regs.ax(), 5);
        let landed: Vec<u8> = (0..5)
            .map(|i| memory.read_u8(PHYSICAL_PAGE_0 + i))
            .collect();
        assert_eq!(&landed, b"HELLO");
        assert_eq!(
            memory.read_u8(LINEAR_BUFFER as usize),
            0,
            "the linear address must be untouched"
        );
    }

    /// A span crossing a page boundary must split at that boundary, because the
    /// next page maps somewhere else entirely.
    #[test]
    fn a_span_crossing_a_page_boundary_splits_into_its_two_physical_runs() {
        let dos = Dos::new();
        let (cpu, mut memory) = paged_guest();

        // Start 16 bytes before the boundary and run 48 bytes past it.
        let start = LINEAR_BUFFER + 0x1000 - 16;
        let runs = dos
            .dos_guest_buffer_runs(&cpu, &mut memory, cpu.regs.ds, start, 64, PageAccess::Write)
            .expect("both pages translate");
        assert_eq!(
            runs,
            vec![(PHYSICAL_PAGE_0 + 0x1000 - 16, 16), (PHYSICAL_PAGE_1, 48)],
            "the split must fall exactly on the page boundary"
        );
    }

    /// An absent page must fault rather than transfer.
    #[test]
    fn an_absent_page_faults_instead_of_transferring() {
        let dos = Dos::new();
        let (cpu, mut memory) = paged_guest();
        clear_page_table_entry(&mut memory, 1, 0); // second page not present

        let error = dos
            .dos_guest_buffer_runs(
                &cpu,
                &mut memory,
                cpu.regs.ds,
                LINEAR_BUFFER + 0x1000 - 8,
                16,
                PageAccess::Write,
            )
            .expect_err("an absent page must fault");
        assert!(
            is_page_fault(&error),
            "expected a page fault, got {error:?}"
        );
    }

    /// A read-only page must fault for a WRITE (INT 21h AH=3Fh's destination)
    /// while still translating for a READ (AH=40h's source).
    #[test]
    fn a_read_only_page_faults_for_a_write_but_not_for_a_read() {
        let dos = Dos::new();
        let (cpu, mut memory) = paged_guest();
        clear_page_table_entry(
            &mut memory,
            0,
            PHYSICAL_PAGE_0 as u32 | PAGE_PRESENT | PAGE_USER, // present, NOT writable
        );

        let error = dos
            .dos_guest_buffer_runs(
                &cpu,
                &mut memory,
                cpu.regs.ds,
                LINEAR_BUFFER,
                8,
                PageAccess::Write,
            )
            .expect_err("a read-only destination must fault");
        assert!(
            is_page_fault(&error),
            "expected a page fault, got {error:?}"
        );

        dos.dos_guest_buffer_runs(
            &cpu,
            &mut memory,
            cpu.regs.ds,
            LINEAR_BUFFER,
            8,
            PageAccess::Read,
        )
        .expect("the same page is still readable");
    }

    /// A span running past the segment limit must fault, not wrap or truncate.
    #[test]
    fn a_span_crossing_the_segment_limit_faults() {
        let dos = Dos::new();
        let (mut cpu, mut memory) = paged_guest();
        // Shrink DS so the buffer's second page is out of bounds.
        cpu.regs.ds_cache.granularity = false;
        cpu.regs.ds_cache.limit = LINEAR_BUFFER + 0x1000 - 1;
        cpu.regs.ds_cache.cached_effective_limit = LINEAR_BUFFER + 0x1000 - 1;

        dos.dos_guest_buffer_runs(
            &cpu,
            &mut memory,
            cpu.regs.ds,
            LINEAR_BUFFER,
            8,
            PageAccess::Write,
        )
        .expect("inside the limit still translates");

        let error = dos
            .dos_guest_buffer_runs(
                &cpu,
                &mut memory,
                cpu.regs.ds,
                LINEAR_BUFFER + 0x1000 - 4,
                16,
                PageAccess::Write,
            )
            .expect_err("crossing the segment limit must fault");
        assert!(
            !is_page_fault(&error),
            "a segment-limit crossing is a general-protection fault, not a page fault: {error:?}"
        );
    }
}
