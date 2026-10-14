//! INT 21h and INT 2Fh dispatch
//!
//! This module contains the AH dispatch table for INT 21h DOS services
//! and the INT 2Fh multiplex interrupt handler.

mod dir_ops;
mod file_ops;
mod io_ops;
mod lfn_ops;
mod memory_ops;
mod misc_ops;
mod process_ops;
mod redirect;

use super::*;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod fcb_namespace_guard_tests;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod fcb_namespace_tests;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod fcb_protected_mode_tests;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod fcb_search_layout_tests;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod guest_span_tests;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod lfn_find_tests;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod readonly_delete_tests;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod sft_position_tests;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod special_open_guard_tests;
#[cfg(all(test, windows))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod windows_search_attrs_tests;

/// Decode the INT 21h/AH=3Dh access code (AL bits 0-2) into `(read, write)`.
///
/// Returns `None` for the reserved codes 3-7, which real DOS rejects with
/// error 12 ("invalid access code").
///
/// MDD-BUG-CRUCIBLE-00375: this exists so the check can run at the TOP of the
/// open handler — before any device, GLIDE2X.OVL, registered-virtual-file or
/// the same predicate. One definition means the two sites cannot drift apart.
pub(crate) fn dos_access_read_write(access_mode: u8) -> Option<(bool, bool)> {
    match access_mode & 0x07 {
        0 => Some((true, false)), // read-only
        1 => Some((false, true)), // write-only
        2 => Some((true, true)),  // read/write
        _ => None,                // 3-7 reserved
    }
}

/// Check whether a file path should trigger HDD seek sounds.
/// Returns false for device names (no drive letter) and floppy drives (A: / B:).
pub(crate) fn should_emit_hdd_sound(path: &str) -> bool {
    let bytes = path.as_bytes();
    // Must have drive letter format (X:\...)
    if bytes.len() < 2 || bytes[1] != b':' {
        return false;
    }
    // Exclude floppy drives
    let drive = bytes[0].to_ascii_uppercase();
    drive != b'A' && drive != b'B'
}

/// Map a non-sequential seek distance to a click count (1-5).
fn seek_distance_to_clicks(distance: u64) -> u8 {
    if distance < 4096 {
        1
    } else if distance < 128 * 1024 {
        2
    } else if distance < 512 * 1024 {
        3
    } else if distance < 2 * 1024 * 1024 {
        4
    } else {
        5
    }
}

const DOS_FILE_TRANSFER_CHUNK: usize = 64 * 1024;
/// DOS extended error 20h: sharing violation.
const SHARING_VIOLATION: u16 = 0x20;
const DOS_FILE_TRANSFER_PREVIEW_LIMIT: usize = 4096;

fn push_transfer_preview(preview: &mut Vec<u8>, limit: usize, bytes: &[u8]) {
    if preview.len() >= limit {
        return;
    }
    let take = (limit - preview.len()).min(bytes.len());
    preview.extend_from_slice(&bytes[..take]);
}

fn dos_read_transfer_preview_limit() -> usize {
    if dos_resource_trace_active() {
        dos_resource_trace_sample_bytes().min(DOS_FILE_TRANSFER_PREVIEW_LIMIT)
    } else {
        0
    }
}

fn dos_write_transfer_preview_limit() -> usize {
    let mut limit = 0usize;
    if dos_write_trace_enabled() {
        limit = limit.max(dos_write_trace_max());
    }
    if dos_print_trace_enabled() {
        limit = limit.max(dos_print_trace_max());
    }
    limit.min(DOS_FILE_TRANSFER_PREVIEW_LIMIT)
}

fn trace_dos_ah40_print_preview(handle: u16, len: usize, preview: &[u8]) {
    if !dos_print_trace_enabled() || !matches!(handle, 1 | 2) {
        return;
    }
    let max = dos_print_trace_max();
    let (s, sample_truncated) = format_dos_bytes(preview, max);
    let truncated = sample_truncated || preview.len() < len;
    tracing::debug!(
        handle,
        len,
        text = %s,
        "DOS AH=40 write"
    );
    if truncated {
        tracing::trace!(max, "DOS AH=40 output truncated");
    }
}

fn int21_disposition_name(disposition: Option<&Int21Disposition>) -> &'static str {
    match disposition {
        None => "dispatch",
        Some(Int21Disposition::Iret) => "iret",
        Some(Int21Disposition::NoIret) => "no_iret",
        Some(Int21Disposition::CtrlC) => "ctrl_c",
        Some(Int21Disposition::CriticalError { .. }) => "critical_error",
        Some(Int21Disposition::KbdWaitStub) => "kbd_wait_stub",
    }
}

fn trace_indos_transition(
    phase: &'static str,
    ah: u8,
    caller_cs: u16,
    caller_ip: u16,
    before: u8,
    after: u8,
    disposition: Option<&Int21Disposition>,
) {
    if !dos_indos_trace_enabled() {
        return;
    }

    tracing::debug!(
        target: "gp64::dos::indos",
        phase,
        caller_cs = format_args!("{:04X}", caller_cs),
        caller_ip = format_args!("{:04X}", caller_ip),
        ah = format_args!("{:02X}", ah),
        before,
        after,
        disposition = int21_disposition_name(disposition),
        "DOS InDOS transition"
    );
}

#[derive(Clone)]
struct DosBootTraceSnapshot {
    caller_cs: u16,
    caller_ip: u16,
    ah: u8,
    al: u8,
    ax: u16,
    bx: u16,
    cx: u16,
    dx: u16,
    ds: u16,
    es: u16,
    ss: u16,
    sp: u16,
    path: Option<String>,
    handle_path: Option<String>,
}

fn dos_boot_trace_enabled() -> bool {
    // KILN-00086: the DOS boot-trace env var was retired; trace permanently off.
    false
}

fn dos_boot_trace_selected(ah: u8) -> bool {
    matches!(ah, 0x09 | 0x3D | 0x3E | 0x3F | 0x42 | 0x4B)
}

/// Pack `FileAttributes` into the DOS attribute word returned in CX.
///
/// Shared by the short-name `AH=43h AL=00h` and the LFN `AX=7143h BL=00h`
/// paths so the two can never disagree about a bit (MDD-BUG-CRUCIBLE-00593:
/// the LFN copy silently omitted the volume-label bit).
fn dos_attr_bits(attrs: &crate::filesystem::FileAttributes) -> u16 {
    let mut bits = 0u16;
    if attrs.read_only {
        bits |= 0x01;
    }
    if attrs.hidden {
        bits |= 0x02;
    }
    if attrs.system {
        bits |= 0x04;
    }
    if attrs.volume_label {
        bits |= 0x08;
    }
    if attrs.directory {
        bits |= 0x10;
    }
    if attrs.archive {
        bits |= 0x20;
    }
    bits
}

/// Unpack the DOS attribute word in CX into `FileAttributes`.
///
/// The inverse of [`dos_attr_bits`], shared by the short-name `AH=43h AL=01h`
/// and LFN `AX=7143h BL=01h` set paths.
fn dos_attrs_from_bits(bits: u16) -> crate::filesystem::FileAttributes {
    crate::filesystem::FileAttributes {
        read_only: (bits & 0x01) != 0,
        hidden: (bits & 0x02) != 0,
        system: (bits & 0x04) != 0,
        volume_label: (bits & 0x08) != 0,
        directory: (bits & 0x10) != 0,
        archive: (bits & 0x20) != 0,
        ..Default::default()
    }
}

/// Map a drive `set_attributes` failure to a DOS error code.
///
/// A host `NotFound` is DOS error 2 (file not found); anything else — a
/// read-only drive, a permission failure — is error 5 (access denied). An
/// `anyhow` error carrying no `io::Error` cannot be told apart, so it keeps
/// the short-name path's historical default of 2.
fn dos_set_attr_error_code(e: &anyhow::Error) -> u16 {
    match e.downcast_ref::<std::io::Error>() {
        Some(io_err) if io_err.kind() == std::io::ErrorKind::NotFound => 2,
        Some(_) => 5,
        None => 2,
    }
}

/// Copy `bytes` into a pre-translated guest span at `span_offset`.
///
/// MDD-BUG-KIL-01018: the runs come from `dos_guest_buffer_runs`, which has
/// already applied the segment limit and the page walk, so this only follows
/// them. Moving bytes at the raw LINEAR address is what the bug was.
fn write_guest_span(
    memory: &mut Memory,
    runs: &[(usize, usize)],
    span_offset: usize,
    bytes: &[u8],
) {
    let mut skip = span_offset;
    let mut written = 0_usize;
    for &(physical, run) in runs {
        if skip >= run {
            skip -= run;
            continue;
        }
        let take = (run - skip).min(bytes.len() - written);
        // One guest-semantic span per run: plain RAM is bulk-copied and
        // anything mapped or device-backed keeps the per-byte path
        // (MDD-BUG-KIL-00779).
        memory.write_bytes_guest(physical + skip, &bytes[written..written + take]);
        written += take;
        skip = 0;
        if written == bytes.len() {
            return;
        }
    }
}

/// Read `out.len()` bytes from a pre-translated guest span at `span_offset`.
fn read_guest_span(memory: &Memory, runs: &[(usize, usize)], span_offset: usize, out: &mut [u8]) {
    let mut skip = span_offset;
    let mut read = 0_usize;
    for &(physical, run) in runs {
        if skip >= run {
            skip -= run;
            continue;
        }
        let take = (run - skip).min(out.len() - read);
        memory.read_guest_run(physical + skip, &mut out[read..read + take]);
        read += take;
        skip = 0;
        if read == out.len() {
            return;
        }
    }
}

impl Dos {
    /// Apply the attributes an AH=3Ch/5Bh create asked for in `CX`.
    ///
    /// The create itself only makes the file; the read-only, hidden and
    /// system bits belong to the directory entry, so they are set after it
    /// exists. The handle just returned stays writable, as in MS-DOS. Only a
    /// create that asks for one of those bits touches the entry, so an
    /// ordinary create is unchanged (MDD-BUG-CRU-01398).
    fn apply_create_attributes(&mut self, drive: u8, path: &str, cx: u16) {
        if cx & 0x07 == 0 {
            return;
        }
        if let Some(drive) = self.drives.get_mut(&drive) {
            if let Err(error) = drive.set_attributes(path, dos_attrs_from_bits(cx & 0x27)) {
                tracing::debug!(
                    target: "gp64::dos::file",
                    path,
                    %error,
                    "create attributes could not be applied"
                );
            }
        }
    }
}

impl Dos {
    /// Validate a CHDIR target and, only if it is good, make it current.
    ///
    /// Shared by short-name `AH=3Bh` and LFN `AX=713Bh`
    /// (MDD-BUG-CRUCIBLE-00592: the LFN arm checked only that the drive was
    /// mounted, so it reported success for a path that did not exist or that
    /// named a regular file, and poisoned every later relative resolution).
    ///
    /// The target must exist AND carry the directory attribute. `spelling` is
    /// the guest's own text rather than the resolved path, because the
    /// resolver stores it using the route's case policy — that is what keeps
    /// the LFN long spelling on the LFN route while preserving the ordinary
    /// short-name route's upper-case behaviour.
    ///
    /// Returns `true` when the directory changed; on `false` the current
    /// directory is untouched and the caller reports DOS error 3.
    fn try_change_directory(
        &mut self,
        drive: u8,
        resolved_path: &str,
        spelling: &str,
        preserve_lfn_case: bool,
    ) -> bool {
        let is_directory = match self.drives.get_mut(&drive) {
            Some(mounted) => matches!(
                mounted.get_attributes(resolved_path),
                Ok(attributes) if attributes.directory
            ),
            None => false,
        };
        if is_directory {
            if preserve_lfn_case {
                self.path_resolver.set_current_dir_lfn(spelling);
            } else {
                self.path_resolver.set_current_dir(spelling);
            }
        }
        is_directory
    }

    /// Register files held outside the DOS handle table by guest path.
    /// The alternate sharing policy treats each as read-only, deny-write.
    pub fn set_held_files<'a>(&mut self, guest_paths: impl IntoIterator<Item = &'a str>) {
        let held = guest_paths
            .into_iter()
            .map(|path| {
                let resolved = self.path_resolver.resolve(path);
                self.resolved_file_identity(resolved.drive, &resolved.path)
            })
            .collect();
        self.held_files = held;
    }

    fn resolved_file_identity(&self, drive: u8, path: &str) -> crate::files::FileIdentity {
        let host_path = self
            .drives
            .get(&drive)
            .and_then(|drive| drive.resolve_host_path(path));
        crate::files::FileIdentity::for_resolved_path(drive, path, host_path.as_deref())
    }

    /// Apply DOS sharing semantics before a drive open can create or truncate.
    ///
    /// Every path-based open/create API must call this with the modes that will
    /// be stored in the SFT. Keeping the check here also keeps error 32 and the
    /// extended error synchronized across the legacy and extended APIs.
    fn reject_pre_open_sharing_violation(
        &mut self,
        cpu: &mut Cpu,
        identity: &crate::files::FileIdentity,
        share_mode: u8,
        access_mode: u8,
    ) -> bool {
        if self.vshare {
            return self.vshare_open_refused(cpu, identity, share_mode, access_mode);
        }
        if !self.file_table.check_sharing_violation_for_identity(
            identity,
            share_mode,
            access_mode,
            self.current_psp,
        ) {
            return false;
        }

        self.set_extended_error(32);
        cpu.regs.set_ax(32);
        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
        true
    }

    /// [`Self::reject_pre_open_sharing_violation`] under VSHARE
    /// (MDD-BUG-FLU-03201): a refusal answers AX=5 with extended error
    /// 20h. A refused compatibility-mode open also goes through the
    /// critical-error handler first: [`Self::vshare_open_critical`].
    fn vshare_open_refused(
        &mut self,
        cpu: &mut Cpu,
        identity: &crate::files::FileIdentity,
        share_mode: u8,
        access_mode: u8,
    ) -> bool {
        if !self.file_table.vshare_open_conflict(
            identity,
            share_mode,
            access_mode,
            &self.held_files,
        ) {
            return false;
        }
        self.set_extended_error(SHARING_VIOLATION);
        cpu.regs.set_ax(5);
        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
        true
    }

    /// A refused open under VSHARE: a compatibility-mode one raises the
    /// critical error (retail `fho` BOX mode0-open02; a deny-all open, 12,
    /// raises none), outside a critical-error handler. `entry` is the
    /// registers the call came in with; the caller has already failed the
    /// call, which is what it returns once the handler fails it.
    fn vshare_open_critical(
        &self,
        memory: &Memory,
        entry: &gp64_cpu::Registers,
        ah_function: u8,
        drive: u8,
        share_mode: u8,
    ) -> Option<Int21Disposition> {
        let crit_flag_addr = Memory::seg_offset_to_linear(INDOS_SEGMENT, CRITICAL_ERROR_OFFSET);
        (self.vshare
            && share_mode & 0x70 == crate::files::SHARE_COMPAT
            && memory.read_u8(crit_flag_addr) == 0)
            .then(|| Int21Disposition::CriticalError {
                ah_function,
                drive,
                error_code: SHARING_VIOLATION as u8,
                is_write: false,
                entry_snapshot: Box::new(entry.clone()),
            })
    }

    fn read_dollar_string_for_boot_trace(
        &self,
        cpu: &Cpu,
        memory: &Memory,
        max_len: usize,
    ) -> String {
        let base = self.dos_translate_ds_dx(cpu);
        let mut bytes = Vec::new();
        for i in 0..max_len {
            let b = memory.read_u8(base.wrapping_add(i));
            if b == b'$' || b == 0 {
                break;
            }
            bytes.push(b);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn boot_trace_handle_path(&self, cpu: &Cpu) -> Option<String> {
        let handle = cpu.regs.bx();
        let effective_handle = if handle == 0x00FE {
            0
        } else {
            self.resolve_alias_handle(handle)
        };
        self.file_table
            .get_sft(effective_handle)
            .map(|sft| sft.path.clone())
    }

    fn capture_dos_boot_trace(
        &self,
        cpu: &Cpu,
        memory: &Memory,
        caller_cs: u16,
        caller_ip: u16,
    ) -> Option<DosBootTraceSnapshot> {
        let ah = cpu.regs.ah();
        if !dos_boot_trace_enabled() || !dos_boot_trace_selected(ah) {
            return None;
        }
        let path = match ah {
            0x09 => Some(self.read_dollar_string_for_boot_trace(cpu, memory, 160)),
            0x3D | 0x4B => {
                Some(self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu)))
            }
            _ => None,
        };
        let handle_path = match ah {
            0x3E | 0x3F | 0x42 => self.boot_trace_handle_path(cpu),
            _ => None,
        };
        Some(DosBootTraceSnapshot {
            caller_cs,
            caller_ip,
            ah,
            al: cpu.regs.al(),
            ax: cpu.regs.ax(),
            bx: cpu.regs.bx(),
            cx: cpu.regs.cx(),
            dx: cpu.regs.dx(),
            ds: cpu.regs.ds,
            es: cpu.regs.es,
            ss: cpu.regs.ss,
            sp: cpu.regs.sp(),
            path,
            handle_path,
        })
    }

    fn trace_dos_boot_return(
        snapshot: &DosBootTraceSnapshot,
        cpu: &Cpu,
        disposition: &Int21Disposition,
    ) {
        let cf = cpu.regs.flags.contains(gp64_cpu::Flags::CARRY);
        tracing::info!(
            target: "gp64::diag::dos_boot",
            caller_cs = format_args!("{:04X}", snapshot.caller_cs),
            caller_ip = format_args!("{:04X}", snapshot.caller_ip),
            ah = format_args!("{:02X}", snapshot.ah),
            al = format_args!("{:02X}", snapshot.al),
            in_ax = format_args!("{:04X}", snapshot.ax),
            in_bx = format_args!("{:04X}", snapshot.bx),
            in_cx = format_args!("{:04X}", snapshot.cx),
            in_dx = format_args!("{:04X}", snapshot.dx),
            in_ds = format_args!("{:04X}", snapshot.ds),
            in_es = format_args!("{:04X}", snapshot.es),
            in_ss = format_args!("{:04X}", snapshot.ss),
            in_sp = format_args!("{:04X}", snapshot.sp),
            out_ax = format_args!("{:04X}", cpu.regs.ax()),
            out_bx = format_args!("{:04X}", cpu.regs.bx()),
            out_cx = format_args!("{:04X}", cpu.regs.cx()),
            out_dx = format_args!("{:04X}", cpu.regs.dx()),
            out_ds = format_args!("{:04X}", cpu.regs.ds),
            out_es = format_args!("{:04X}", cpu.regs.es),
            cf,
            disposition = int21_disposition_name(Some(disposition)),
            path = snapshot.path.as_deref().unwrap_or(""),
            handle_path = snapshot.handle_path.as_deref().unwrap_or(""),
            "DOS boot trace"
        );
    }

    /// Handle INT 21h
    pub fn int21(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        vga: &mut Vga,
        keyboard: &mut Keyboard,
        pic: &mut Pic,
    ) -> Result<Int21Disposition> {
        let ah = cpu.regs.ah();
        let caller_cs = memory.read_u16_seg(cpu.regs.ss, cpu.regs.sp().wrapping_add(2));
        let caller_ip = memory
            .read_u16_seg(cpu.regs.ss, cpu.regs.sp())
            .wrapping_sub(2);
        // Increment InDOS flag (TSRs check this to avoid reentrant DOS calls)
        let indos_before_enter =
            memory.read_u8(Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET));
        Self::enter_dos(memory);
        trace_indos_transition(
            "enter",
            ah,
            caller_cs,
            caller_ip,
            indos_before_enter,
            memory.read_u8(Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET)),
            None,
        );

        // Every service return, including recursive redispatch and early
        // returns, crosses this accounting boundary once.
        let disposition = match self.int21_dispatch(cpu, memory, vga, keyboard, pic) {
            Ok(disposition) => disposition,
            Err(error) => {
                Self::leave_dos(memory);
                return Err(error);
            }
        };
        match &disposition {
            Int21Disposition::CriticalError { .. } => {
                // Still inside DOS — leave_dos() deferred until after INT 24h completes.
                // Do NOT call sync_iret_carry() — the INT 24h IRET frame will be
                // pushed on top of the INT 21h frame, so sync_iret_carry would
                // modify the wrong FLAGS word.
                let indos =
                    memory.read_u8(Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET));
                trace_indos_transition(
                    "defer",
                    ah,
                    caller_cs,
                    caller_ip,
                    indos,
                    indos,
                    Some(&disposition),
                );
            }
            Int21Disposition::KbdWaitStub => {
                // The BIOS wait stub retains this entry until its resume boundary
                // releases it immediately before re-entering INT 21h.
                // Do NOT sync carry into the still-active outer frame.
                let indos =
                    memory.read_u8(Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET));
                trace_indos_transition(
                    "defer",
                    ah,
                    caller_cs,
                    caller_ip,
                    indos,
                    indos,
                    Some(&disposition),
                );
            }
            Int21Disposition::Iret => {
                if !self.skip_iret_sync {
                    Self::sync_iret_carry(cpu, memory, false);
                }
                let indos_before_leave =
                    memory.read_u8(Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET));
                Self::leave_dos(memory);
                trace_indos_transition(
                    "leave",
                    ah,
                    caller_cs,
                    caller_ip,
                    indos_before_leave,
                    memory.read_u8(Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET)),
                    Some(&disposition),
                );
            }
            Int21Disposition::NoIret | Int21Disposition::CtrlC => {
                let indos_before_leave =
                    memory.read_u8(Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET));
                Self::leave_dos(memory);
                trace_indos_transition(
                    "leave",
                    ah,
                    caller_cs,
                    caller_ip,
                    indos_before_leave,
                    memory.read_u8(Memory::seg_offset_to_linear(INDOS_SEGMENT, INDOS_OFFSET)),
                    Some(&disposition),
                );
            }
        }

        Ok(disposition)
    }

    fn int21_dispatch(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        vga: &mut Vga,
        keyboard: &mut Keyboard,
        pic: &mut Pic,
    ) -> Result<Int21Disposition> {
        let ah = cpu.regs.ah();
        let trace_enabled = dos_trace_enabled() && should_trace_ah(ah);
        let int_trace_enabled = dos_int_trace_enabled() && should_trace_ah(ah);
        let resource_trace_enabled = dos_resource_trace_active();
        let boot_trace_requested = dos_boot_trace_enabled() && dos_boot_trace_selected(ah);
        let (pre_regs, caller_cs, caller_ip) =
            if trace_enabled || int_trace_enabled || resource_trace_enabled || boot_trace_requested
            {
                // Best-effort callsite: INT pushes IP, CS, FLAGS (IP at SS:SP).
                // Return IP points to the instruction *after* INT; caller IP is usually ret_ip-2.
                let ss = cpu.regs.ss;
                let sp = cpu.regs.sp();
                let ret_ip = memory.read_u16_seg(ss, sp);
                let ret_cs = memory.read_u16_seg(ss, sp.wrapping_add(2));
                (
                    trace_enabled.then(|| cpu.regs.clone()),
                    ret_cs,
                    ret_ip.wrapping_sub(2),
                )
            } else {
                (None, 0, 0)
            };

        // Log DOS calls with full register state (for debugging).
        // If the DOS-trace-AH filter (retired) is set, restrict logging to matching AH values/ranges.
        if trace_enabled {
            tracing::debug!(
                caller_cs = format_args!("{:04X}", caller_cs),
                caller_ip = format_args!("{:04X}", caller_ip),
                ah = format_args!("{:02X}", ah),
                al = format_args!("{:02X}", cpu.regs.al()),
                bx = format_args!("{:04X}", cpu.regs.bx()),
                cx = format_args!("{:04X}", cpu.regs.cx()),
                dx = format_args!("{:04X}", cpu.regs.dx()),
                si = format_args!("{:04X}", cpu.regs.si()),
                di = format_args!("{:04X}", cpu.regs.di()),
                ds = format_args!("{:04X}", cpu.regs.ds),
                es = format_args!("{:04X}", cpu.regs.es),
                ss = format_args!("{:04X}", cpu.regs.ss),
                sp = format_args!("{:04X}", cpu.regs.sp()),
                "INT 21h"
            );
        }
        if int_trace_enabled {
            trace_dos_int(cpu, ah, caller_cs, caller_ip);
        }
        let boot_trace = self.capture_dos_boot_trace(cpu, memory, caller_cs, caller_ip);

        let mut disposition = Int21Disposition::Iret;
        match ah {
            // Terminate program (legacy CP/M-compatible).
            // Equivalent to INT 20h / INT 21h AH=4Ch with exit code 0: terminate the
            // current process, restore the parent PSP + INT 22h/23h/24h vectors, free
            // the process's memory, and return to the parent. The exit code is always 0
            // regardless of the incoming AL. (We track the PSP explicitly in
            // self.current_psp, so the real-DOS CS=PSP requirement is intentionally
            // ignored — the DOS model's DOS_Terminate likewise keys off the tracked PSP.)
            0x00 => {
                tracing::warn!(target: "gp64::exit",
                    "INT 21h/00h (terminate): CS:IP={:04X}:{:04X} PSP={:04X}",
                    cpu.regs.cs, cpu.regs.ip(), self.current_psp);
                // Force AL=0 so the root/batch exit path (handle_step_error reads AL)
                // and AH=4Dh both observe exit code 0, matching real DOS.
                cpu.regs.set_al(0);
                disposition = self.terminate_process(cpu, memory, 0, 0x00)?;
            }

            // Character input with echo
            0x01 => match self.read_keyboard_char(cpu, memory, keyboard, pic, vga, true, true) {
                KeyboardReadResult::Ready => {}
                KeyboardReadResult::CtrlC => {
                    return Ok(Int21Disposition::CtrlC);
                }
                KeyboardReadResult::NoKey => {
                    if self.skip_iret_sync {
                        return Ok(Self::wait_for_input(cpu, memory, true));
                    }
                    return Ok(Int21Disposition::KbdWaitStub);
                }
            },

            // Character output
            0x02 => {
                let ch = cpu.regs.dl();
                if let Some(capture) = self.stdout_capture.as_mut() {
                    capture.push(ch);
                }
                vga.teletype(ch, 0, memory);
            }

            // Read character from STDAUX (COM1).
            //
            // Real DOS blocks until a character arrives; we return what the
            // bridge has staged, or 0. Declared deviation — see the AUX/PRN
            // HLD; blocking needs the keyboard's deferred-re-entry protocol.
            0x03 => {
                let mut byte = [0u8; 1];
                let got = self
                    .device_bridge
                    .lock()
                    .map(|mut b| {
                        // Registering the shortfall is what makes this route
                        // work at all: nothing has been opened, so demand is
                        // the only thing that lets the emulator take a byte
                        // out of COM1 on our behalf.
                        let got = b.read_aux(&mut byte);
                        b.note_aux_read(1, got);
                        got
                    })
                    .unwrap_or(0);
                cpu.regs.set_al(if got == 1 { byte[0] } else { 0 });
            }

            // Write character to STDAUX (COM1).
            0x04 => {
                let dl = cpu.regs.dl();
                self.write_char_device_byte(3, dl);
            }

            // Write character to STDPRN (LPT1).
            0x05 => {
                let dl = cpu.regs.dl();
                self.write_char_device_byte(4, dl);
            }

            // Direct console I/O (no Ctrl-C check, no wait if DL=0xFF)
            0x06 => {
                let dl = cpu.regs.dl();
                if dl == 0xFF {
                    // Input mode: non-blocking, ZF indicates availability
                    match self.read_keyboard_char(cpu, memory, keyboard, pic, vga, false, false) {
                        KeyboardReadResult::Ready => {
                            // Key available: clear ZF
                            let mut clear_zero = |f: &mut u16| *f &= !gp64_cpu::Flags::ZERO.bits();
                            cpu.with_iret_flags_mut(memory, &mut clear_zero)?;
                            cpu.regs.flags.set(gp64_cpu::Flags::ZERO, false);
                        }
                        KeyboardReadResult::CtrlC => {
                            // Cannot happen: check_ctrl_c is false
                            unreachable!();
                        }
                        KeyboardReadResult::NoKey => {
                            // No key available: set ZF, AL=0
                            cpu.regs.set_al(0);
                            let mut set_zero = |f: &mut u16| *f |= gp64_cpu::Flags::ZERO.bits();
                            cpu.with_iret_flags_mut(memory, &mut set_zero)?;
                            cpu.regs.flags.set(gp64_cpu::Flags::ZERO, true);
                        }
                    }
                } else {
                    // Output mode
                    if let Some(capture) = self.stdout_capture.as_mut() {
                        capture.push(dl);
                    }
                    if dos_ah06_trace_enabled() {
                        self.ah06_line_buf.push(dl);
                    }
                    vga.teletype(dl, 0, memory);
                }
            }

            // Direct character input without echo (no Ctrl-C check)
            0x07 => match self.read_keyboard_char(cpu, memory, keyboard, pic, vga, false, false) {
                KeyboardReadResult::Ready => {}
                KeyboardReadResult::CtrlC => unreachable!(),
                KeyboardReadResult::NoKey => {
                    if self.skip_iret_sync {
                        return Ok(Self::wait_for_input(cpu, memory, true));
                    }
                    return Ok(Int21Disposition::KbdWaitStub);
                }
            },

            // Character input without echo (with Ctrl-C check)
            0x08 => match self.read_keyboard_char(cpu, memory, keyboard, pic, vga, false, true) {
                KeyboardReadResult::Ready => {}
                KeyboardReadResult::CtrlC => {
                    return Ok(Int21Disposition::CtrlC);
                }
                KeyboardReadResult::NoKey => {
                    if self.skip_iret_sync {
                        return Ok(Self::wait_for_input(cpu, memory, true));
                    }
                    return Ok(Int21Disposition::KbdWaitStub);
                }
            },

            // Print string (terminated by $)
            0x09 => {
                let mut addr = self.dos_translate_ds_dx(cpu);
                let trace = dos_print_trace_enabled();
                let trace_max = dos_print_trace_max();
                let max_bytes = DOS_AH09_MAX;

                if trace {
                    let mut s = String::new();
                    let mut n = 0usize;
                    while n < trace_max {
                        let ch = memory.read_u8(addr + n);
                        if ch == b'$' {
                            break;
                        }
                        if (0x20..=0x7E).contains(&ch) {
                            s.push(ch as char);
                        } else if ch == b'\r' {
                            s.push_str("\\r");
                        } else if ch == b'\n' {
                            s.push_str("\\n");
                        } else if ch == b'\t' {
                            s.push_str("\\t");
                        } else {
                            use std::fmt::Write;
                            let _ = write!(s, "\\x{ch:02X}");
                        }
                        n += 1;
                    }
                    // Also scan for a terminator within a bounded window to quickly detect runaway prints.
                    let mut term_at: Option<usize> = None;
                    for i in 0..max_bytes.min(4096) {
                        let ch = memory.read_u8(addr + i);
                        if ch == b'$' {
                            term_at = Some(i);
                            break;
                        }
                    }
                    tracing::debug!(term_at = ?term_at, text = %s, "DOS AH=09 print");
                }

                let mut printed = 0usize;
                loop {
                    if printed >= max_bytes {
                        if trace {
                            tracing::debug!(
                                printed,
                                ds = format_args!("{:04X}", cpu.regs.ds),
                                dx = format_args!("{:04X}", cpu.regs.dx()),
                                "DOS AH=09 print aborted"
                            );
                        }
                        break;
                    }
                    let ch = memory.read_u8(addr);
                    if ch == b'$' {
                        break;
                    }
                    if let Some(capture) = self.stdout_capture.as_mut() {
                        capture.push(ch);
                    }
                    vga.teletype(ch, 0, memory);
                    addr += 1;
                    printed += 1;
                }
            }

            // Buffered input
            0x0A => {
                // DS:DX -> input buffer:
                // [0]=max len, [1]=filled count, [2..]=chars, terminated with CR (0x0D)
                let buf_addr = self.dos_translate_ds_dx(cpu);
                let max_len = memory.read_u8(buf_addr) as usize;
                if max_len == 0 {
                    // RBIL: if max_len=0, return immediately without reading
                    memory.write_u8(buf_addr + 1, 0);
                    return Ok(Int21Disposition::Iret);
                }
                let mut len = memory.read_u8(buf_addr + 1) as usize;

                if len > max_len {
                    len = 0;
                    memory.write_u8(buf_addr + 1, 0);
                } else if len > 0 {
                    let mut ok = true;
                    let mut i = 0usize;
                    while i < len {
                        let b = memory.read_u8(buf_addr + 2 + i);
                        if b == 0 || b == 0x0D || b < 0x20 {
                            ok = false;
                            break;
                        }
                        i += 1;
                    }
                    if !ok {
                        len = 0;
                        memory.write_u8(buf_addr + 1, 0);
                    }
                }

                if bda::kbd_peek(memory).is_none() {
                    if self.skip_iret_sync {
                        return Ok(Self::wait_for_input(cpu, memory, true));
                    }
                    return Ok(Int21Disposition::KbdWaitStub);
                }

                let (_sc, ascii) = bda::kbd_dequeue(memory).unwrap();

                // Check for Ctrl-C (ASCII 3) - buffered input always checks
                if ascii == 0x03 {
                    // Echo "^C" to console per DOS behavior
                    vga.teletype(b'^', 0, memory);
                    vga.teletype(b'C', 0, memory);
                    Self::trigger_ctrl_c(cpu, memory);
                    return Ok(Int21Disposition::CtrlC);
                }

                match ascii {
                    // Enter - complete input
                    0x0D => {
                        // Only write CR if there's space in the buffer (max_len > 0)
                        if max_len > 0 {
                            memory.write_u8(buf_addr + 2 + len, 0x0D);
                        }
                        vga.teletype(0x0D, 0, memory);
                        vga.teletype(0x0A, 0, memory);
                        memory.write_u8(buf_addr + 1, len as u8);
                    }
                    // Backspace - delete last character
                    0x08 => {
                        if len > 0 {
                            len -= 1;
                            vga.teletype(0x08, 0, memory);
                            vga.teletype(b' ', 0, memory);
                            vga.teletype(0x08, 0, memory);
                        }
                        memory.write_u8(buf_addr + 1, len as u8);
                        if self.skip_iret_sync {
                            Self::prep_input_wait(cpu, memory, true);
                        } else {
                            return Ok(Int21Disposition::KbdWaitStub);
                        }
                    }
                    // Escape - clear line, print backslash, move to new line
                    0x1B => {
                        vga.teletype(b'\\', 0, memory);
                        vga.teletype(0x0D, 0, memory);
                        vga.teletype(0x0A, 0, memory);
                        memory.write_u8(buf_addr + 1, 0);
                        if self.skip_iret_sync {
                            Self::prep_input_wait(cpu, memory, true);
                        } else {
                            return Ok(Int21Disposition::KbdWaitStub);
                        }
                    }
                    // Tab - expand to spaces (standard 8-column tabs)
                    0x09 => {
                        let spaces_needed = 8 - (len % 8);
                        for _ in 0..spaces_needed {
                            if len < max_len.saturating_sub(1) {
                                memory.write_u8(buf_addr + 2 + len, b' ');
                                len += 1;
                                vga.teletype(b' ', 0, memory);
                            }
                        }
                        memory.write_u8(buf_addr + 1, len as u8);
                        if self.skip_iret_sync {
                            Self::prep_input_wait(cpu, memory, true);
                        } else {
                            return Ok(Int21Disposition::KbdWaitStub);
                        }
                    }
                    // Extended key (ASCII=0/0xE0) - ignore and continue waiting
                    0x00 | 0xE0 => {
                        // Extended keys like F1-F12, arrows - just discard the scan code
                        // and continue waiting for regular input
                        if self.skip_iret_sync {
                            Self::prep_input_wait(cpu, memory, true);
                        } else {
                            return Ok(Int21Disposition::KbdWaitStub);
                        }
                    }
                    // Regular printable characters
                    _ => {
                        if ascii >= 0x20 && len < max_len.saturating_sub(1) {
                            memory.write_u8(buf_addr + 2 + len, ascii);
                            len += 1;
                            vga.teletype(ascii, 0, memory);
                        }
                        memory.write_u8(buf_addr + 1, len as u8);
                        if self.skip_iret_sync {
                            Self::prep_input_wait(cpu, memory, true);
                        } else {
                            return Ok(Int21Disposition::KbdWaitStub);
                        }
                    }
                }
            }

            // Check input status
            0x0B => {
                cpu.regs.set_al(if bda::kbd_peek(memory).is_some() {
                    0xFF
                } else {
                    0x00
                });
            }

            // Clear keyboard buffer and invoke function
            0x0C => {
                let subfunc = cpu.regs.al();
                bda::kbd_clear(memory);
                keyboard.clear();
                self.pending_extended_scancode = None;
                match subfunc {
                    0x01 | 0x06 | 0x07 | 0x08 | 0x0A => {
                        cpu.regs.set_ah(subfunc);
                        return self.int21_dispatch(cpu, memory, vga, keyboard, pic);
                    }
                    _ => {}
                }
            }

            // Reset disk - always succeeds
            0x0D => {
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // FCB: Open file
            0x0F => 'fcb_open: {
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let filename = self.fcb_read_filename(memory, fcb_addr);
                let drive = self.fcb_read_drive(memory, fcb_addr);
                let Some(drive_letter) = self.fcb_drive_letter(memory, fcb_addr) else {
                    self.set_extended_error(15); // Invalid drive
                    cpu.regs.set_al(0xFF);
                    break 'fcb_open;
                };

                // Build full path: drive letter + filename
                let dos_path = format!("{drive_letter}:{filename}");
                let resolved = self.path_resolver.resolve(&dos_path);
                let file_identity = self.resolved_file_identity(resolved.drive, &resolved.path);

                let file_res = if let Some(drv) = self.drives.get_mut(&resolved.drive) {
                    // FCB Open always opens for read/write (no mode flag in FCB)
                    drv.open_file(&resolved.path, true, false, false)
                } else {
                    Err(anyhow::anyhow!("Drive not found"))
                };

                match file_res {
                    Ok(f) => {
                        // Get file size and modification time
                        let (size, date, time) = if let Ok(attr) = {
                            if let Some(drv) = self.drives.get_mut(&resolved.drive) {
                                drv.get_attributes(&resolved.path)
                            } else {
                                Err(anyhow::anyhow!("Drive not found"))
                            }
                        } {
                            let (d, t) = Self::systemtime_to_dos_datetime(attr.modified);
                            (attr.size as u32, d, t)
                        } else {
                            (0, 0, 0)
                        };

                        // Allocate handle
                        let full_path =
                            format!("{}:{}", (resolved.drive + b'A') as char, resolved.path);
                        if let Some(handle) = self.file_table.allocate_with_sharing_identity(
                            f,
                            full_path,
                            file_identity,
                            self.current_psp,
                            crate::files::SHARE_COMPAT,
                            crate::files::ACCESS_READWRITE,
                        ) {
                            // Update FCB fields
                            self.fcb_set_drive(memory, fcb_addr, drive);
                            self.fcb_set_handle(memory, fcb_addr, handle);
                            self.fcb_set_file_size(memory, fcb_addr, size);
                            self.fcb_set_record_size(memory, fcb_addr, 128); // Default record size
                            self.fcb_set_current_block(memory, fcb_addr, 0);
                            // Set date/time from file metadata
                            self.fcb_set_datetime(memory, fcb_addr, date, time);
                            self.clear_extended_error();
                            cpu.regs.set_al(0x00); // Success
                        } else {
                            self.set_extended_error(4); // Too many open files
                            cpu.regs.set_al(0xFF);
                        }
                    }
                    Err(_) => {
                        self.set_extended_error(2); // File not found
                        cpu.regs.set_al(0xFF);
                    }
                }
            }

            // FCB: Close file
            0x10 => {
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let handle = self.fcb_get_handle(memory, fcb_addr);

                if handle < 5 {
                    self.set_extended_error(6); // Invalid handle
                    cpu.regs.set_al(0xFF);
                } else {
                    match self.file_table.try_free(handle) {
                        Ok(true) => {
                            self.clear_extended_error();
                            cpu.regs.set_al(0x00); // Success
                        }
                        Ok(false) => {
                            self.set_extended_error(6); // Invalid handle
                            cpu.regs.set_al(0xFF);
                        }
                        Err(error) => {
                            tracing::warn!(
                                handle,
                                %error,
                                "FCB close failed to synchronize file"
                            );
                            self.set_extended_error(5); // Access denied / I/O failure
                            cpu.regs.set_al(0xFF);
                        }
                    }
                }
            }

            // FCB: Find first matching file
            0x11 => 'fcb_find_first: {
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let filename = self.fcb_read_filename(memory, fcb_addr);
                let drive = self.fcb_read_drive(memory, fcb_addr);
                let Some(drive_letter) = self.fcb_drive_letter(memory, fcb_addr) else {
                    self.set_extended_error(15); // Invalid drive
                    cpu.regs.set_al(0xFF);
                    break 'fcb_find_first;
                };

                // Convert FCB wildcards (?) to standard pattern
                let pattern = format!("{drive_letter}:{filename}");

                // FCB operations match all file types (0xFF)
                // MDD-BUG-CRUCIBLE-00447: an extended FCB asks for the
                // seven-byte-prefixed result form, and the search attribute
                // lives in the caller's own prefix.
                let extended = Self::fcb_is_extended(memory, fcb_addr);
                let search_attr = if extended {
                    memory.read_u8(fcb_addr + Self::FCB_EXTENDED_ATTR)
                } else {
                    0
                };
                self.fcb_search_extended = extended;
                self.fcb_search_attr = search_attr;

                // MDD-BUG-KILN-00233: an EXTENDED FCB carries its own search
                // attribute, and it selects what the search may match — most
                // visibly `0x08` alone, which under DOS means "the volume label
                // and nothing else". Passing a blanket `0xFF` here made a
                // volume-label search match every ordinary file instead, so the
                // caller got the first FILE in the directory serialized into the
                // DTA's 11-byte label field. File Manager renders that as the
                // drive's label: retail shows `C:` for an unlabelled disk, we
                // showed `C: [` plus a filename fragment `]`, which is the
                // "tofu box" this ticket was filed for.
                //
                // A plain (non-extended) FCB has no attribute byte to honour and
                // keeps the historical `0xFF`; narrowing that is a separate
                // question with no witness here.
                let find_attr = if extended { search_attr } else { 0xFF };

                if let Ok(search_id) = self.find_first(&pattern, find_attr) {
                    self.fcb_search_id = search_id;
                    if let Some(entry) = self.find_get_entry(search_id) {
                        Self::fcb_write_search_result(
                            memory,
                            self.dta_linear,
                            extended,
                            search_attr,
                            drive,
                            &entry.name.clone(),
                            &entry,
                        );
                        cpu.regs.set_al(0x00); // Found
                    } else {
                        cpu.regs.set_al(0xFF); // Not found
                    }
                } else {
                    cpu.regs.set_al(0xFF); // Not found
                }
            }

            // FCB: Find next matching file
            0x12 => {
                let search_id = self.fcb_search_id;
                self.find_advance(search_id);
                if let Some(entry) = self.find_get_entry(search_id) {
                    // MDD-BUG-CRUCIBLE-00447: same serializer as AH=11h, and
                    // the same result form the matching Find First chose — the
                    // drive byte sits after the prefix in the extended layout,
                    // so it must be read back from the right place.
                    let extended = self.fcb_search_extended;
                    let drive_off = if extended {
                        Self::FCB_SEARCH_EXTENDED_PREFIX
                    } else {
                        0
                    };
                    let drive = memory.read_u8(self.dta_linear + drive_off);

                    Self::fcb_write_search_result(
                        memory,
                        self.dta_linear,
                        extended,
                        self.fcb_search_attr,
                        drive,
                        &entry.name.clone(),
                        &entry,
                    );
                    cpu.regs.set_al(0x00); // Found
                } else {
                    cpu.regs.set_al(0xFF); // No more files
                }
            }

            // FCB: Delete file (DOS 1.0+)
            0x13 => 'fcb_delete: {
                // DS:DX = FCB
                // Returns: AL = 00h if deleted, FFh if not found
                // Wildcards (?) are supported - deletes all matching files
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));

                // Parse FCB filename and drive
                let filename = self.fcb_read_filename(memory, fcb_addr);
                let Some(drive_letter) = self.fcb_drive_letter(memory, fcb_addr) else {
                    self.set_extended_error(15); // Invalid drive
                    cpu.regs.set_al(0xFF);
                    break 'fcb_delete;
                };
                let dos_path = format!("{drive_letter}:{filename}");
                let resolved = self.path_resolver.resolve(&dos_path);

                // Check if pattern contains wildcards
                let has_wildcards = filename.contains('?') || filename.contains('*');

                if has_wildcards {
                    // Get directory path and pattern
                    let pos = resolved.path.rfind('\\').unwrap_or(0);
                    let dir_part = resolved.path[..pos].to_string();
                    let file_pat = &resolved.path[pos + 1..];

                    let mut deleted_any = false;
                    let mut files_to_delete = Vec::new();

                    if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                        // First collect matching files. MDD-BUG-CRUCIBLE-00446:
                        // the pattern is a DOS pattern, so it is tested against
                        // the canonical 8.3 name the guest can actually see —
                        // but the delete acts on the host name behind it.
                        for entry in dir_ops::enumerate_dos_visible(drive.as_mut(), &dir_part) {
                            // Skip directories
                            if entry.attrs.directory {
                                continue;
                            }
                            if Self::matches_pattern_static(file_pat, &entry.dos_name) {
                                let host_name = &entry.host_name;
                                let full_path = if dir_part.is_empty() {
                                    format!("\\{host_name}")
                                } else {
                                    format!("{dir_part}\\{host_name}")
                                };
                                files_to_delete.push(full_path);
                            }
                        }

                        // Then delete them (separate loop to avoid borrow issues)
                        for file_path in files_to_delete {
                            if drive.remove_file(&file_path).is_ok() {
                                deleted_any = true;
                            }
                        }
                    }

                    if deleted_any {
                        cpu.regs.set_al(0x00);
                    } else {
                        cpu.regs.set_al(0xFF);
                    }
                } else {
                    // Single file deletion (no wildcards)
                    if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                        match drive.remove_file(&resolved.path) {
                            Ok(()) => cpu.regs.set_al(0x00),
                            Err(_) => cpu.regs.set_al(0xFF),
                        }
                    } else {
                        cpu.regs.set_al(0xFF);
                    }
                }
            }

            // FCB: Sequential read
            0x14 => {
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let handle = self.fcb_get_handle(memory, fcb_addr);
                let record_size = self.fcb_get_record_size(memory, fcb_addr) as usize;
                let position = self.fcb_current_byte_offset(memory, fcb_addr);

                if let Some(fh) = self.file_table.get_sft_mut(handle) {
                    match fh.file.seek(SeekFrom::Start(position)) {
                        Ok(position) => {
                            fh.position = position;
                            let dta_addr = self.dta_linear;
                            let mut buffer = vec![0u8; record_size];

                            match fh.file.read(&mut buffer) {
                                Ok(0) => {
                                    self.set_extended_error(38); // End of file
                                    cpu.regs.set_al(0x01); // EOF, no data
                                }
                                Ok(n) => {
                                    // Write to DTA
                                    for (i, &b) in buffer.iter().take(n).enumerate() {
                                        memory.write_u8(dta_addr + i, b);
                                    }
                                    // Pad with zeros if partial read
                                    for i in n..record_size {
                                        memory.write_u8(dta_addr + i, 0);
                                    }
                                    fh.position += n as u64;

                                    // Advance FCB position (current record/block)
                                    self.fcb_advance_position(memory, fcb_addr);

                                    if n < record_size {
                                        self.clear_extended_error();
                                        cpu.regs.set_al(0x03); // Partial record
                                    } else {
                                        self.clear_extended_error();
                                        cpu.regs.set_al(0x00); // Success
                                    }
                                }
                                Err(_) => {
                                    self.set_extended_error(30); // Read fault
                                    cpu.regs.set_al(0x01); // EOF/error
                                }
                            }
                        }
                        Err(_) => {
                            self.set_extended_error(30); // Read fault
                            cpu.regs.set_al(0x01); // Seek error
                        }
                    }
                } else {
                    self.set_extended_error(6); // Invalid handle
                    cpu.regs.set_al(0x01); // Error
                }
            }

            // FCB: Sequential write
            0x15 => {
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let handle = self.fcb_get_handle(memory, fcb_addr);
                let record_size = self.fcb_get_record_size(memory, fcb_addr) as usize;
                let position = self.fcb_current_byte_offset(memory, fcb_addr);
                let current_size = self.fcb_get_file_size(memory, fcb_addr);

                // Write result: (bytes_written, final_position, is_error)
                let write_result: Option<(usize, u64, bool)> =
                    if let Some(fh) = self.file_table.get_sft_mut(handle) {
                        match fh.file.seek(SeekFrom::Start(position)) {
                            Ok(position) => {
                                fh.position = position;
                                let dta_addr = self.dta_linear;
                                let mut buffer = vec![0u8; record_size];

                                // Read from DTA
                                #[allow(clippy::needless_range_loop)]
                                for i in 0..record_size {
                                    buffer[i] = memory.read_u8(dta_addr + i);
                                }

                                match fh.file.write(&buffer) {
                                    Ok(n) => {
                                        fh.position += n as u64;
                                        Some((n, fh.position, false))
                                    }
                                    Err(_) => Some((0, fh.position, true)),
                                }
                            }
                            Err(_) => Some((0, fh.position, true)),
                        }
                    } else {
                        None
                    };

                // Process result and update FCB fields (after mutable borrow is released)
                match write_result {
                    Some((bytes_written, final_position, is_error)) => {
                        if is_error {
                            cpu.regs.set_al(0x01); // Disk full
                        } else {
                            // Update file size if extended
                            if final_position > current_size as u64 {
                                self.fcb_set_file_size(memory, fcb_addr, final_position as u32);
                            }

                            // Advance FCB position (current record/block)
                            self.fcb_advance_position(memory, fcb_addr);

                            if bytes_written < record_size {
                                cpu.regs.set_al(0x01); // Disk full
                            } else {
                                cpu.regs.set_al(0x00); // Success
                            }
                        }
                    }
                    None => {
                        cpu.regs.set_al(0x01); // Error
                    }
                }
            }

            // FCB: Create file
            0x16 => 'fcb_create: {
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let filename = self.fcb_read_filename(memory, fcb_addr);
                let drive = self.fcb_read_drive(memory, fcb_addr);
                let Some(drive_letter) = self.fcb_drive_letter(memory, fcb_addr) else {
                    self.set_extended_error(15); // Invalid drive
                    cpu.regs.set_al(0xFF);
                    break 'fcb_create;
                };

                let dos_path = format!("{drive_letter}:{filename}");
                let resolved = self.path_resolver.resolve(&dos_path);
                let file_identity = self.resolved_file_identity(resolved.drive, &resolved.path);

                // MDD-BUG-KIL-01019: the create truncates, so it must not run unless a
                // handle can be produced for it. The `Err` arm below reports
                // AL=0xFF, which is what a full table already reported — only
                // now the file is left alone.
                let file_res = if !self.file_table.has_open_capacity() {
                    Err(anyhow::anyhow!("no free DOS file handle"))
                } else if let Some(drv) = self.drives.get_mut(&resolved.drive) {
                    drv.open_file(&resolved.path, true, true, true) // write, create, truncate
                } else {
                    Err(anyhow::anyhow!("Drive not found"))
                };

                match file_res {
                    Ok(f) => {
                        let full_path =
                            format!("{}:{}", (resolved.drive + b'A') as char, resolved.path);
                        if let Some(handle) = self.file_table.allocate_with_sharing_identity(
                            f,
                            full_path,
                            file_identity,
                            self.current_psp,
                            crate::files::SHARE_COMPAT,
                            crate::files::ACCESS_READWRITE,
                        ) {
                            self.fcb_set_drive(memory, fcb_addr, drive);
                            self.fcb_set_handle(memory, fcb_addr, handle);
                            self.fcb_set_file_size(memory, fcb_addr, 0);
                            self.fcb_set_record_size(memory, fcb_addr, 128);
                            self.fcb_set_current_block(memory, fcb_addr, 0);
                            // Set date/time from emulated clock
                            let (date, time) = self.emulated_dos_datetime(memory);
                            self.fcb_set_datetime(memory, fcb_addr, date, time);
                            cpu.regs.set_al(0x00);
                        } else {
                            cpu.regs.set_al(0xFF);
                        }
                    }
                    Err(_) => {
                        cpu.regs.set_al(0xFF);
                    }
                }
            }

            // FCB: Rename file (DOS 1.0+)
            0x17 => 'fcb_rename: {
                // DS:DX = Modified FCB with old and new names
                // FCB+0: Drive, FCB+1-8: Old name, FCB+9-11: Old ext
                // FCB+17-24: New name, FCB+25-27: New ext
                // Returns: AL = 00h if renamed, FFh if error
                // Wildcards: ? in old pattern matches any character
                //            ? in new pattern means "keep original character"
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let fcb_base = Self::fcb_base_addr(memory, fcb_addr);
                let Some(drive_letter) = self.fcb_drive_letter(memory, fcb_addr) else {
                    self.set_extended_error(15); // Invalid drive
                    cpu.regs.set_al(0xFF);
                    break 'fcb_rename;
                };

                // Read raw FCB bytes for old name (8) and extension (3)
                let mut old_name_bytes = [b' '; 8];
                let mut old_ext_bytes = [b' '; 3];
                for (i, byte) in old_name_bytes.iter_mut().enumerate() {
                    *byte = memory.read_u8(fcb_base + Self::FCB_FILENAME + i);
                }
                for (i, byte) in old_ext_bytes.iter_mut().enumerate() {
                    *byte = memory.read_u8(fcb_base + Self::FCB_EXTENSION + i);
                }

                // Read raw FCB bytes for new name (8) and extension (3)
                // New name starts at offset 11h (17) from the standard FCB base
                let mut new_name_bytes = [b' '; 8];
                let mut new_ext_bytes = [b' '; 3];
                for (i, byte) in new_name_bytes.iter_mut().enumerate() {
                    *byte = memory.read_u8(fcb_base + 17 + i);
                }
                for (i, byte) in new_ext_bytes.iter_mut().enumerate() {
                    *byte = memory.read_u8(fcb_base + 25 + i);
                }

                // Convert to string for pattern matching
                let old_name_str = Self::fcb_bytes_to_filename(&old_name_bytes, &old_ext_bytes);
                let new_name_pattern = Self::fcb_bytes_to_filename(&new_name_bytes, &new_ext_bytes);

                // Check for wildcards in old name
                let has_wildcards = old_name_str.contains('?') || old_name_str.contains('*');

                let dos_path = format!("{}:{}", drive_letter, old_name_str);
                let resolved = self.path_resolver.resolve(&dos_path);

                if has_wildcards {
                    // Get directory path and pattern
                    let pos = resolved.path.rfind('\\').unwrap_or(0);
                    let dir_part = resolved.path[..pos].to_string();
                    let file_pat = &resolved.path[pos + 1..];

                    let mut renamed_any = false;
                    let mut rename_ops = Vec::new();

                    // Collect matching files
                    if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                        // MDD-BUG-CRUCIBLE-00446: match on the DOS-visible 8.3
                        // name, and derive the destination from it too — the
                        // guest's replacement pattern refers to the characters
                        // it can see, not to the host name. Only the source
                        // path uses the host name.
                        for entry in dir_ops::enumerate_dos_visible(drive.as_mut(), &dir_part) {
                            if entry.attrs.directory {
                                continue;
                            }
                            if Self::matches_pattern_static(file_pat, &entry.dos_name) {
                                // Apply new name pattern
                                let new_filename = Self::apply_rename_pattern(
                                    &entry.dos_name,
                                    &new_name_bytes,
                                    &new_ext_bytes,
                                );
                                let host_name = &entry.host_name;
                                let old_path = if dir_part.is_empty() {
                                    format!("\\{host_name}")
                                } else {
                                    format!("{dir_part}\\{host_name}")
                                };
                                let new_path = if dir_part.is_empty() {
                                    format!("\\{new_filename}")
                                } else {
                                    format!("{dir_part}\\{new_filename}")
                                };
                                rename_ops.push((old_path, new_path));
                            }
                        }
                    }

                    if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                        // Perform renames
                        for (old_path, new_path) in rename_ops {
                            if drive.rename(&old_path, &new_path).is_ok() {
                                renamed_any = true;
                            }
                        }
                    }

                    if renamed_any {
                        cpu.regs.set_al(0x00);
                    } else {
                        cpu.regs.set_al(0xFF);
                    }
                } else {
                    // Single file rename (no wildcards)
                    let new_dos_path = format!("{drive_letter}:{new_name_pattern}");
                    let new_resolved = self.path_resolver.resolve(&new_dos_path);

                    if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                        match drive.rename(&resolved.path, &new_resolved.path) {
                            Ok(()) => cpu.regs.set_al(0x00),
                            Err(_) => cpu.regs.set_al(0xFF),
                        }
                    } else {
                        cpu.regs.set_al(0xFF);
                    }
                }
            }

            // FCB: Random read
            0x21 => {
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let handle = self.fcb_get_handle(memory, fcb_addr);
                let record_size = self.fcb_get_record_size(memory, fcb_addr) as usize;
                let record_num = self.fcb_get_random_record(memory, fcb_addr);
                let position = (record_num as u64) * (record_size as u64);

                if let Some(fh) = self.file_table.get_sft_mut(handle) {
                    // Seek to position
                    if fh.file.seek(SeekFrom::Start(position)).is_err() {
                        cpu.regs.set_al(0x01); // EOF
                        return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                    }
                    fh.position = position;

                    let dta_addr = self.dta_linear;
                    let mut buffer = vec![0u8; record_size];

                    match fh.file.read(&mut buffer) {
                        Ok(0) => {
                            cpu.regs.set_al(0x01); // EOF, no data
                        }
                        Ok(n) => {
                            // Write to DTA
                            for (i, &b) in buffer.iter().take(n).enumerate() {
                                memory.write_u8(dta_addr + i, b);
                            }
                            // Pad with zeros if partial read
                            for i in n..record_size {
                                memory.write_u8(dta_addr + i, 0);
                            }
                            fh.position += n as u64;

                            // NOTE: FCB Random Read (AH=21) does NOT auto-increment the random
                            // record number. The program must explicitly set it before each call.
                            // Only block read/write (AH=27/28) update the record number.

                            if n < record_size {
                                cpu.regs.set_al(0x03); // Partial record at EOF
                            } else {
                                cpu.regs.set_al(0x00); // Success
                            }
                        }
                        Err(_) => {
                            cpu.regs.set_al(0x01); // EOF/error
                        }
                    }
                } else {
                    cpu.regs.set_al(0x01); // Error
                }
                // Set carry flag based on result: clear on success (AL=0x00 or 0x03), set on error
                if cpu.regs.al() == 0x00 || cpu.regs.al() == 0x03 {
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // FCB: Random write
            0x22 => {
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let handle = self.fcb_get_handle(memory, fcb_addr);
                let record_size = self.fcb_get_record_size(memory, fcb_addr) as usize;
                let record_num = self.fcb_get_random_record(memory, fcb_addr);
                let position = (record_num as u64) * (record_size as u64);
                let current_size = self.fcb_get_file_size(memory, fcb_addr);

                // Write result: (bytes_written, final_position, is_error, seek_failed)
                let write_result: Option<(usize, u64, bool, bool)> =
                    if let Some(fh) = self.file_table.get_sft_mut(handle) {
                        // Seek to position
                        if fh.file.seek(SeekFrom::Start(position)).is_err() {
                            Some((0, position, false, true)) // Seek failed
                        } else {
                            fh.position = position;

                            let dta_addr = self.dta_linear;
                            let mut buffer = vec![0u8; record_size];

                            // Read from DTA
                            #[allow(clippy::needless_range_loop)]
                            for i in 0..record_size {
                                buffer[i] = memory.read_u8(dta_addr + i);
                            }

                            match fh.file.write(&buffer) {
                                Ok(n) => {
                                    fh.position += n as u64;
                                    Some((n, fh.position, false, false))
                                }
                                Err(_) => Some((0, fh.position, true, false)),
                            }
                        }
                    } else {
                        None
                    };

                // Process result and update FCB fields (after mutable borrow is released)
                match write_result {
                    Some((_, _, _, true)) => {
                        // Seek failed
                        cpu.regs.set_al(0x01);
                        return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                    }
                    Some((bytes_written, final_position, is_error, false)) => {
                        if is_error {
                            cpu.regs.set_al(0x01); // Disk full
                        } else {
                            // Update file size if extended
                            if final_position > current_size as u64 {
                                self.fcb_set_file_size(memory, fcb_addr, final_position as u32);
                            }

                            // NOTE: FCB Random Write (AH=22) does NOT auto-increment the random
                            // record number. Only block read/write (AH=27/28) update the record number.
                            if bytes_written < record_size {
                                cpu.regs.set_al(0x01); // Disk full
                            } else {
                                cpu.regs.set_al(0x00); // Success
                            }
                        }
                    }
                    None => {
                        cpu.regs.set_al(0x01); // Error
                    }
                }
                // Set carry flag based on result: clear on success (AL=0x00), set on error
                if cpu.regs.al() == 0x00 {
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // FCB: Get file size (DOS 1.0+)
            0x23 => {
                // DS:DX = Unopened FCB
                // Returns: AL = 00h if found, FFh if not found
                // Updates FCB random record field with file size in records
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let filename = self.fcb_read_filename(memory, fcb_addr);
                let resolved = self.path_resolver.resolve(&filename);

                if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                    match drive.get_attributes(&resolved.path) {
                        Ok(attrs) => {
                            let record_size = self.fcb_get_record_size(memory, fcb_addr) as u64;
                            let num_records = attrs.size.div_ceil(record_size);
                            self.fcb_set_random_record(memory, fcb_addr, num_records as u32);
                            cpu.regs.set_al(0x00);
                        }
                        Err(_) => cpu.regs.set_al(0xFF),
                    }
                } else {
                    cpu.regs.set_al(0xFF);
                }
            }

            // FCB: Set random record number (DOS 1.0+)
            0x24 => {
                // DS:DX = Opened FCB
                // Sets random record field from current block and record
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));

                // Read current block (offset 0x0C) and current record (offset
                // 0x20) through the SAME translated address: a second
                // `ds << 4` here read a protected-mode guest's random record
                // out of unrelated low memory and wrote it into the real FCB
                // (MDD-BUG-KIL-00771).
                let current_block = memory.read_u16(fcb_addr + 0x0C) as u32;
                let current_record = memory.read_u8(fcb_addr + 0x20) as u32;

                // Random record = current_block * 128 + current_record
                let random_record = current_block * 128 + current_record;
                self.fcb_set_random_record(memory, fcb_addr, random_record);
                // No return value
            }

            // Create PSP (obsolete - use 0x4B EXEC instead)
            0x26 => {
                // DX = segment for new PSP
                // Copies current PSP to new location, updates parent pointer
                let new_psp_seg = cpu.regs.dx();
                let current_psp = self.current_psp;
                let src_base = Memory::seg_offset_to_linear(current_psp, 0);
                let dst_base = Memory::seg_offset_to_linear(new_psp_seg, 0);

                // Copy entire 256-byte PSP
                for i in 0..256 {
                    let b = memory.read_u8(src_base + i);
                    memory.write_u8(dst_base + i, b);
                }

                // Update parent PSP pointer in new PSP
                memory.write_u16(dst_base + PSP_PARENT_PSP, current_psp);

                // This function has no documented return value
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // FCB: Random block read
            0x27 => {
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let handle = self.fcb_get_handle(memory, fcb_addr);
                let record_size = self.fcb_get_record_size(memory, fcb_addr) as usize;
                let record_num = self.fcb_get_random_record(memory, fcb_addr);
                let record_count = cpu.regs.cx() as usize;
                let position = (record_num as u64) * (record_size as u64);

                if record_count == 0 {
                    // Special case: set file size in FCB, don't read
                    cpu.regs.set_cx(0);
                    cpu.regs.set_al(0x00);
                } else if let Some(fh) = self.file_table.get_sft_mut(handle) {
                    if fh.file.seek(SeekFrom::Start(position)).is_err() {
                        cpu.regs.set_cx(0);
                        cpu.regs.set_al(0x01);
                    } else if let Some(total_bytes) = record_size
                        .checked_mul(record_count)
                        .filter(|&n| n <= 0xFFFF)
                    {
                        // Use checked_mul to prevent integer overflow, limit to 64KB for DOS compatibility
                        fh.position = position;
                        let dta_addr = self.dta_linear;
                        let mut buffer = vec![0u8; total_bytes];

                        match fh.file.read(&mut buffer) {
                            Ok(0) => {
                                cpu.regs.set_cx(0);
                                cpu.regs.set_al(0x01); // EOF
                            }
                            Ok(n) => {
                                for (i, &b) in buffer.iter().take(n).enumerate() {
                                    memory.write_u8(dta_addr + i, b);
                                }
                                for i in n..total_bytes {
                                    memory.write_u8(dta_addr + i, 0);
                                }
                                fh.position += n as u64;

                                let records_read = n.div_ceil(record_size);
                                cpu.regs.set_cx(records_read as u16);

                                // Update random record number to next record
                                self.fcb_set_random_record(
                                    memory,
                                    fcb_addr,
                                    record_num + records_read as u32,
                                );

                                if n < total_bytes {
                                    cpu.regs
                                        .set_al(if n % record_size != 0 { 0x03 } else { 0x01 });
                                } else {
                                    cpu.regs.set_al(0x00);
                                }
                            }
                            Err(_) => {
                                cpu.regs.set_cx(0);
                                cpu.regs.set_al(0x01);
                            }
                        }
                    } else {
                        // Overflow in size calculation or size exceeds 64KB
                        cpu.regs.set_cx(0);
                        cpu.regs.set_al(0x01);
                    }
                } else {
                    cpu.regs.set_cx(0);
                    cpu.regs.set_al(0x01);
                }
            }

            // FCB: Random block write
            0x28 => {
                let ds = cpu.regs.ds;
                let dx = cpu.regs.dx();
                // MDD-BUG-KILN-00233: DS is a SELECTOR under a protected-mode
                // guest, so the FCB pointer must be resolved through the
                // descriptor cache, not by a real-mode `ds << 4`.
                let fcb_addr = cpu.translate_address(ds, u32::from(dx));
                let handle = self.fcb_get_handle(memory, fcb_addr);
                let record_size = self.fcb_get_record_size(memory, fcb_addr) as usize;
                let record_num = self.fcb_get_random_record(memory, fcb_addr);
                let record_count = cpu.regs.cx() as usize;
                let position = (record_num as u64) * (record_size as u64);

                if record_count == 0 {
                    // Special case: truncate file to current random record position
                    let truncated = self
                        .file_table
                        .get_sft_mut(handle)
                        .is_some_and(|fh| fh.file.set_len(position).is_ok());
                    cpu.regs.set_cx(0);
                    if truncated {
                        self.fcb_set_file_size(memory, fcb_addr, position as u32);
                        cpu.regs.set_al(0x00);
                    } else {
                        cpu.regs.set_al(0x01);
                    }
                } else {
                    // Get current file size before mutable borrow
                    let current_size = self.fcb_get_file_size(memory, fcb_addr);

                    // Write result: (bytes_written, final_position, records_written, is_error)
                    let write_result: Option<(usize, u64, usize, bool)> =
                        if let Some(fh) = self.file_table.get_sft_mut(handle) {
                            if fh.file.seek(SeekFrom::Start(position)).is_err() {
                                Some((0, position, 0, true))
                            } else if let Some(total_bytes) = record_size
                                .checked_mul(record_count)
                                .filter(|&n| n <= 0xFFFF)
                            {
                                fh.position = position;
                                let dta_addr = self.dta_linear;
                                let mut buffer = vec![0u8; total_bytes];

                                #[allow(clippy::needless_range_loop)]
                                for i in 0..total_bytes {
                                    buffer[i] = memory.read_u8(dta_addr + i);
                                }

                                match fh.file.write(&buffer) {
                                    Ok(n) => {
                                        fh.position += n as u64;
                                        let records_written = n / record_size;
                                        Some((n, fh.position, records_written, false))
                                    }
                                    Err(_) => Some((0, fh.position, 0, true)),
                                }
                            } else {
                                Some((0, position, 0, true))
                            }
                        } else {
                            None
                        };

                    // Process result and update FCB fields (after mutable borrow is released)
                    match write_result {
                        Some((bytes_written, final_position, records_written, is_error)) => {
                            if is_error {
                                cpu.regs.set_cx(0);
                                cpu.regs.set_al(0x01);
                            } else {
                                cpu.regs.set_cx(records_written as u16);

                                // Update file size if extended
                                if final_position > current_size as u64 {
                                    self.fcb_set_file_size(memory, fcb_addr, final_position as u32);
                                }

                                // Update random record number
                                self.fcb_set_random_record(
                                    memory,
                                    fcb_addr,
                                    record_num + records_written as u32,
                                );

                                let total_bytes = record_size * record_count;
                                cpu.regs.set_al(if bytes_written < total_bytes {
                                    0x01
                                } else {
                                    0x00
                                });
                            }
                        }
                        None => {
                            cpu.regs.set_cx(0);
                            cpu.regs.set_al(0x01);
                        }
                    }
                }
            }

            // Parse filename into FCB
            0x29 => {
                let si = cpu.regs.si();
                let es = cpu.regs.es;
                let di = cpu.regs.di();
                let al = cpu.regs.al();

                // DS:SI and ES:DI are SELECTORS under a protected-mode
                // guest, so both are resolved through the descriptor cache
                // rather than by a real-mode `segment << 4`: the real-mode
                // arithmetic read the filename from, and wrote the parsed FCB
                // to, unrelated low memory (MDD-BUG-KIL-00771).
                let src_addr = cpu.translate_address(cpu.regs.ds, u32::from(si));
                let fcb_addr = cpu.translate_address(es, u32::from(di));

                // Read filename from DS:SI (ASCIIZ or space-terminated)
                let mut filename: Vec<u8> = Vec::new();
                let mut offset = 0usize;

                // Skip leading separators if AL bit 0 is set
                if al & 0x01 != 0 {
                    while offset < 128 {
                        let ch = memory.read_u8(src_addr + offset);
                        if ch == 0 || !matches!(ch, b' ' | b'\t' | b';' | b',' | b'=' | b':') {
                            break;
                        }
                        offset += 1;
                    }
                }
                let skipped = offset;

                // Gather the candidate text (up to NUL or the 128-byte cap);
                // the shared parser decides where the name actually ends.
                while offset < 128 {
                    let ch = memory.read_u8(src_addr + offset);
                    if ch == 0 {
                        break;
                    }
                    filename.push(ch);
                    offset += 1;
                }

                // Drive, name and extension — including `*` expanding to `?`
                // for the rest of its field and the AH=29h invalid-character
                // stop — come from the one parser the crate already has
                // (MDD-BUG-CRU-01391); a second copy here would drift.
                let (fcb, consumed) = Dos::parse_fcb_name(&filename);
                for (i, b) in fcb.iter().enumerate() {
                    memory.write_u8(fcb_addr + i, *b);
                }

                // Update SI to point past parsed filename
                cpu.regs
                    .set_si(si.wrapping_add((skipped + consumed) as u16));

                // Return AL based on wildcards
                let has_wildcard = fcb[1..12].contains(&b'?');
                cpu.regs.set_al(if has_wildcard { 0x01 } else { 0x00 });
            }

            // Select disk
            0x0E => {
                let drive = cpu.regs.dl();
                // Real DOS allows selecting any drive 0-25, validation happens on file access
                if drive < 26 {
                    self.path_resolver.set_current_drive(drive);
                }
                cpu.regs.set_al(26); // Number of logical drives
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get current drive
            0x19 => {
                cpu.regs.set_al(self.path_resolver.get_current_drive());
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Set DTA
            0x1A => {
                self.dta_segment = cpu.regs.ds;
                // Store offset (16-bit for real/16-bit DPMI, truncated for 32-bit DPMI
                // since dta_offset is u16 for Get DTA compatibility - dta_linear has the
                // correct full address)
                self.dta_offset = self.dos_offset_dx(cpu) as u16;
                // Compute linear address now (handles both real and protected mode)
                self.dta_linear = self.dos_translate_ds_dx(cpu);
                self.dta_segment_limit = if self.dos_api_32bit(cpu) {
                    u32::MAX
                } else {
                    0xFFFF
                };
                // Note: DTA should have at least 128 bytes available
                // Warn in debug builds if DTA is set too close to segment boundary
                #[cfg(debug_assertions)]
                if self.dta_offset > 0xFF80 {
                    tracing::warn!(
                        segment = format_args!("{:04X}", self.dta_segment),
                        offset = format_args!("{:04X}", self.dta_offset),
                        "DOS DTA set near segment boundary"
                    );
                }
            }

            // Get default drive allocation data (DOS 1.0+)
            0x1B => {
                // Returns allocation info for default drive
                // AL = sectors per cluster
                // CX = bytes per sector
                // DX = total clusters
                // DS:BX = pointer to media ID byte
                let drive_num = self.path_resolver.get_current_drive();
                let drive_letter = (b'A' + drive_num) as char;
                self.get_drive_allocation_data(cpu, memory, drive_letter);
            }

            // Get drive allocation data (DOS 2.0+)
            0x1C => {
                // DL = drive (0=default, 1=A, 2=B, etc.)
                // Returns same as 0x1B
                let dl = cpu.regs.dl();
                // DL above 26 names no drive; `b'A' + dl - 1` would also
                // overflow for DL >= 0xBF (MDD-BUG-CRU-01392).
                if dl > 26 {
                    cpu.regs.set_al(0xFF);
                } else {
                    let drive_letter = if dl == 0 {
                        let drive_num = self.path_resolver.get_current_drive();
                        (b'A' + drive_num) as char
                    } else {
                        (b'A' + dl - 1) as char
                    };
                    self.get_drive_allocation_data(cpu, memory, drive_letter);
                }
            }

            // Get DTA
            0x2F => {
                // The DTA is a real-mode segment until a PM caller sets its
                // own with 1Ah (the default one EXEC points at PSP:0080). A
                // DPMI client gets a selector either way.
                let dta_is_segment =
                    Memory::seg_offset_to_linear(self.dta_segment, self.dta_offset)
                        == self.dta_linear;
                let (dta_seg, pm) = if self.dpmi_client_int21(cpu) {
                    let segment = self.dta_segment;
                    let selector = if dta_is_segment {
                        self.dpmi.data_selector_for_segment(cpu, memory, segment)
                    } else {
                        Some(segment)
                    };
                    selector.map_or((segment, false), |sel| (sel, true))
                } else {
                    (self.dta_segment, false)
                };
                cpu.return_data_seg_for_hle(Some(memory), gp64_cpu::DataSegReg::ES, dta_seg, pm);
                cpu.regs.set_bx(self.dta_offset);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get date - return cached emulated date (seeded from host at boot)
            0x2A => {
                self.consume_midnight_rollovers(memory);
                let (year, month, day, day_of_week) = self.emulated_date;
                cpu.regs.set_al(day_of_week);
                cpu.regs.set_cx(year);
                cpu.regs.set_dh(month);
                cpu.regs.set_dl(day);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Set date
            0x2B => {
                let year = cpu.regs.cx();
                let month = cpu.regs.dh();
                let day = cpu.regs.dl();
                let valid =
                    if !(1980..=2099).contains(&year) || !(1..=12).contains(&month) || day < 1 {
                        false
                    } else {
                        let days_in_month = match month {
                            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                            4 | 6 | 9 | 11 => 30,
                            2 => {
                                if (year.is_multiple_of(4) && !year.is_multiple_of(100))
                                    || year.is_multiple_of(400)
                                {
                                    29
                                } else {
                                    28
                                }
                            }
                            _ => 0,
                        };
                        day <= days_in_month
                    };
                if valid {
                    let dow = day_of_week(year, month, day);
                    self.emulated_date = (year, month, day, dow);
                    cpu.regs.set_al(0x00);
                } else {
                    cpu.regs.set_al(0xFF);
                }
            }

            // DOS time of day follows the PIT-driven BIOS tick count, so
            // guest-visible time advances with emulated time.
            0x2C => {
                self.consume_midnight_rollovers(memory);
                let ticks = memory.read_u32(BDA_TICK_COUNT_ADDR);
                let (hours, minutes, seconds, hundredths) = bda_ticks_to_time(ticks);
                cpu.regs.set_ch(hours);
                cpu.regs.set_cl(minutes);
                cpu.regs.set_dh(seconds);
                cpu.regs.set_dl(hundredths);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Set time
            0x2D => {
                let hours = cpu.regs.ch();
                let minutes = cpu.regs.cl();
                let seconds = cpu.regs.dh();
                let hundredths = cpu.regs.dl();
                let valid = hours <= 23 && minutes <= 59 && seconds <= 59 && hundredths <= 99;
                if valid {
                    let ticks = time_to_bda_ticks(hours, minutes, seconds, hundredths);
                    memory.write_u32(BDA_TICK_COUNT_ADDR, ticks);
                    cpu.regs.set_al(0x00);
                } else {
                    cpu.regs.set_al(0xFF);
                }
            }

            // Set verify flag (DOS 2.0+)
            0x2E => {
                // AL = 0 to turn off verify, 1 to turn on.
                // Only AL==1 enables; other non-zero values are ignored per the DOS model behavior.
                self.verify_flag = cpu.regs.al() == 1;
                // No return value
            }

            // Get DOS version
            0x30 => {
                cpu.regs.set_al(self.version_major);
                cpu.regs.set_ah(self.version_minor);
                cpu.regs.set_bh(0xFF); // OEM serial number: FFh = Microsoft
                cpu.regs.set_bl(0);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Terminate and Stay Resident (TSR)
            0x31 => {
                disposition = self.terminate_stay_resident(cpu, memory);
            }

            // Get DPB (Drive Parameter Block)
            0x32 => {
                let drive_num = if cpu.regs.dl() == 0 {
                    self.path_resolver.get_current_drive()
                } else {
                    cpu.regs.dl() - 1 // Convert 1-based to 0-based
                };

                // The DPB chain at DPB_SEG contains 26 pre-built stub
                // entries (one per drive letter) set up by
                // `init_kernel_structures`. Point DS:BX into the
                // corresponding entry rather than stomping 0x0050:0000,
                // which used to clobber the InDOS / critical error
                // flags at linear 0x0503/0x0504.
                if (drive_num as u16) < DOS_DRIVES && self.drives.contains_key(&drive_num) {
                    cpu.return_data_seg_for_hle(
                        Some(memory),
                        gp64_cpu::DataSegReg::DS,
                        DPB_SEG,
                        false,
                    );
                    cpu.regs.set_bx((drive_num as u16) * 33);
                    cpu.regs.set_al(0); // Success
                } else {
                    cpu.regs.set_al(0xFF); // Invalid drive
                }
            }

            // Get/Set Ctrl-C check flag
            0x33 => {
                match cpu.regs.al() {
                    0x00 => {
                        // Get Ctrl-Break check state
                        cpu.regs.set_dl(if self.ctrl_break_check { 1 } else { 0 });
                    }
                    0x01 => {
                        // Set Ctrl-Break check state
                        self.ctrl_break_check = cpu.regs.dl() != 0;
                    }
                    0x05 => {
                        // Get boot drive
                        cpu.regs.set_dl(3); // C: drive
                    }
                    0x06 => {
                        // Get true DOS version
                        cpu.regs
                            .set_bx((self.version_minor as u16) << 8 | self.version_major as u16);
                        cpu.regs.set_dx(0);
                    }
                    _ => {}
                }
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get InDOS flag address
            0x34 => {
                self.get_indos_flag_address(cpu);
                // A DPMI host translates the returned pointer: ES must be a
                // selector for the InDOS segment, not the real-mode segment
                // loads ES, MDD-00017).
                if self.dpmi_client_int21(cpu) {
                    let segment = cpu.regs.es;
                    if let Some(selector) = self.dpmi.segment_to_descriptor(cpu, memory, segment) {
                        cpu.return_data_seg_for_hle(
                            Some(memory),
                            gp64_cpu::DataSegReg::ES,
                            selector,
                            true,
                        );
                    }
                }
            }

            // Get interrupt vector
            0x35 => {
                let vector = cpu.regs.al();
                if self.int21_effective_protected_mode(cpu) {
                    // DPMI spec: INT 21h/35h in PM returns the PM interrupt vector.
                    // If a client handler was set via INT 21h/25h, return it.
                    // Otherwise return a generic HLE vector stub. INT 1/3 have
                    // separate prior-vector stubs below because a debugger
                    // saves them before replacing the live vector.
                    if let Some((selector, offset)) = self.dpmi.client_pm_int_handler(vector) {
                        cpu.return_data_seg_for_hle(
                            Some(memory),
                            gp64_cpu::DataSegReg::ES,
                            selector,
                            true,
                        );
                        if self.dos_api_32bit(cpu) {
                            cpu.regs.ebx = offset;
                        } else {
                            cpu.regs.set_bx(offset as u16);
                        }
                    } else if matches!(vector, 1 | 3) {
                        // A saved prior vector must keep its default IRET
                        // action after a replacement handler is installed.
                        cpu.return_data_seg_for_hle(
                            Some(memory),
                            gp64_cpu::DataSegReg::ES,
                            PM_BIOS_CODE_SEL,
                            true,
                        );
                        let stub_offset =
                            crate::dpmi::HLE_PM_OLD_INT_VECTOR_STUB_BASE + u16::from(vector) * 2;
                        if self.dos_api_32bit(cpu) {
                            cpu.regs.ebx = u32::from(stub_offset);
                        } else {
                            cpu.regs.set_bx(stub_offset);
                        }
                    } else {
                        // Return HLE BIOS stub: PM_BIOS_CODE_SEL:(FE00 + vector*2)
                        cpu.return_data_seg_for_hle(
                            Some(memory),
                            gp64_cpu::DataSegReg::ES,
                            PM_BIOS_CODE_SEL,
                            true,
                        );
                        let stub_offset = 0xFE00u32 + (vector as u32) * 2;
                        if self.dos_api_32bit(cpu) {
                            cpu.regs.ebx = stub_offset;
                        } else {
                            cpu.regs.set_bx(stub_offset as u16);
                        }
                    }
                } else {
                    let ivt_addr = (vector as usize) * 4;
                    let offset = memory.read_u16(ivt_addr);
                    let segment = memory.read_u16(ivt_addr + 2);
                    cpu.regs.set_bx(offset);
                    cpu.return_data_seg_for_hle(
                        Some(memory),
                        gp64_cpu::DataSegReg::ES,
                        segment,
                        false,
                    );
                }
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get free disk space
            0x36 => {
                let drive = if cpu.regs.dl() == 0 {
                    self.path_resolver.get_current_drive()
                } else {
                    cpu.regs.dl() - 1
                };

                if let Some(d) = self.drives.get(&drive) {
                    if let Ok(geometry) = d.get_disk_geometry() {
                        cpu.regs.set_ax(geometry.sectors_per_cluster);
                        cpu.regs.set_bx(geometry.free_clusters);
                        cpu.regs.set_cx(geometry.bytes_per_sector);
                        cpu.regs.set_dx(geometry.total_clusters);
                    } else {
                        cpu.regs.set_ax(0xFFFF);
                    }
                } else {
                    cpu.regs.set_ax(0xFFFF);
                }
            }

            // Get/Set switch character (DOS 2.0+, undocumented but used by some programs)
            0x37 => {
                let al = cpu.regs.al();
                match al {
                    0 => {
                        // Get switch character
                        cpu.regs.set_dl(b'/'); // Always '/' for switch char
                        cpu.regs.set_al(0x00); // Success
                    }
                    1 => {
                        // Set switch character - we ignore but report success
                        cpu.regs.set_al(0x00);
                    }
                    2 => {
                        // Get device availability
                        // DL = 0 means /DEV/ required, non-0 means optional
                        cpu.regs.set_dl(0xFF); // /DEV/ prefix optional
                        cpu.regs.set_al(0x00);
                    }
                    3 => {
                        // Set device availability - ignore but report success
                        cpu.regs.set_al(0x00);
                    }
                    _ => {
                        // Invalid subfunction
                        cpu.regs.set_al(0xFF);
                    }
                }
            }

            // Create directory
            0x39 => {
                let dos_path = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                let resolved = self.path_resolver.resolve(&dos_path);

                if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                    match drive.create_dir(&resolved.path) {
                        Ok(()) => {
                            cpu.regs.set_ax(0);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                        Err(e) => {
                            let error_code =
                                if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
                                    if io_err.kind() == std::io::ErrorKind::AlreadyExists {
                                        5
                                    } else {
                                        3
                                    }
                                } else {
                                    3
                                };
                            cpu.regs.set_ax(error_code);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                } else {
                    cpu.regs.set_ax(3);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Remove directory
            0x3A => {
                let dos_path = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                let resolved = self.path_resolver.resolve(&dos_path);

                if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                    match drive.remove_dir(&resolved.path) {
                        Ok(()) => {
                            cpu.regs.set_ax(0);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                        Err(_) => {
                            cpu.regs.set_ax(3);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                } else {
                    cpu.regs.set_ax(3);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Change directory
            0x3B => {
                let path = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                trace_dos_path("CHDIR", cpu.regs.ds, self.dos_offset_dx(cpu), &path);
                let resolved = self.path_resolver.resolve(&path);
                if self.try_change_directory(resolved.drive, &resolved.path, &path, false) {
                    cpu.regs.set_ax(0);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.set_ax(3);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Create file
            0x3C => {
                let filename = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                trace_dos_path("CREATE", cpu.regs.ds, self.dos_offset_dx(cpu), &filename);
                tracing::trace!(
                    target: "gp64::dos::files",
                    function = "3C",
                    path = %filename,
                    attributes = format_args!("{:04X}", cpu.regs.cx()),
                    "DOS create request"
                );

                // A device name is resolved before any path is, because a
                // device is not a file and must never reach a drive. This is
                // the commonest print route of all: `fopen("PRN", "w")` and
                // `>PRN` redirection are both AH=3Ch, and without this the
                // call created a host file literally named `PRN`.
                if let Some(device) = crate::filesystem::parse_device_name(&filename) {
                    let result = self.open_named_device(
                        device,
                        crate::files::SHARE_COMPAT,
                        crate::files::ACCESS_READWRITE,
                    );
                    self.set_device_open_return(cpu, result);
                    return Ok(Int21Disposition::Iret);
                }

                let resolved = self.path_resolver.resolve(&filename);

                // Check if drive exists first: a fitted drive with no media
                // is not ready, any other missing one invalid
                // (MDD-BUG-FLU-03064).
                if !self.drives.contains_key(&resolved.drive)
                    && self.not_ready_drives.contains(&resolved.drive)
                {
                    disposition = self.not_ready_drive_error(cpu, memory, 0x3C, resolved.drive);
                } else if !self.drives.contains_key(&resolved.drive) {
                    self.set_extended_error(15); // Invalid drive
                    cpu.regs.set_ax(15);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else {
                    let is_hdd = resolved.drive >= 2;
                    let full_path =
                        format!("{}:{}", (resolved.drive + b'A') as char, resolved.path);
                    let file_identity = self.resolved_file_identity(resolved.drive, &resolved.path);
                    let entry = self.vshare.then(|| cpu.regs.clone());
                    if self.reject_pre_open_sharing_violation(
                        cpu,
                        &file_identity,
                        crate::files::SHARE_COMPAT,
                        crate::files::ACCESS_READWRITE,
                    ) {
                        if let Some(critical) = entry.as_ref().and_then(|entry| {
                            self.vshare_open_critical(
                                memory,
                                entry,
                                0x3C,
                                resolved.drive,
                                crate::files::SHARE_COMPAT,
                            )
                        }) {
                            return Ok(critical);
                        }
                        return Ok(Int21Disposition::Iret);
                    }

                    // MDD-BUG-KIL-01019: this open creates or truncates, so the handle
                    // has to be available BEFORE it runs. The response is the
                    // one the post-open failure gave; what changes is that an
                    // existing file still has its bytes.
                    if !self.file_table.has_open_capacity() {
                        self.set_extended_error(4);
                        cpu.regs.set_ax(4); // Too many open files
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(Int21Disposition::Iret);
                    }

                    let file_res = self.drives.get_mut(&resolved.drive).unwrap().open_file(
                        &resolved.path,
                        true,
                        true,
                        true,
                    );

                    match file_res {
                        Ok(f) => {
                            if let Some(handle) = self.file_table.allocate_with_sharing_identity(
                                f,
                                full_path,
                                file_identity,
                                self.current_psp,
                                crate::files::SHARE_COMPAT,
                                crate::files::ACCESS_READWRITE,
                            ) {
                                self.clear_extended_error();
                                cpu.regs.set_ax(handle);
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                self.apply_create_attributes(
                                    resolved.drive,
                                    &resolved.path,
                                    cpu.regs.cx(),
                                );
                                if is_hdd {
                                    self.emit_hdd_clicks(2);
                                }
                            } else {
                                self.set_extended_error(4); // Too many open files
                                cpu.regs.set_ax(4);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            }
                        }
                        Err(e) => {
                            // Classify by ErrorKind where the error carries
                            // one: the wording of a host error message is not
                            // ours to depend on and differs across platforms.
                            // A drive that reports an opaque error still falls
                            // back to the textual heuristic, because there is
                            // nothing else to go on.
                            let err_code = match e
                                .downcast_ref::<std::io::Error>()
                                .map(std::io::Error::kind)
                            {
                                Some(std::io::ErrorKind::PermissionDenied) => 5, // Access denied
                                Some(std::io::ErrorKind::NotFound) => {
                                    self.missing_dos_path_error(resolved.drive, &resolved.path)
                                }
                                // A create that failed any other way is
                                // reported as path not found, as before.
                                Some(_) => 3,
                                None => {
                                    let err_str = e.to_string().to_lowercase();
                                    if err_str.contains("permission") || err_str.contains("access")
                                    {
                                        5 // Access denied
                                    } else {
                                        3 // Path not found
                                    }
                                }
                            };
                            self.set_extended_error(err_code);
                            cpu.regs.set_ax(err_code);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                }
            }

            // Open file
            0x3D => {
                let filename = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                trace_dos_path("OPEN", cpu.regs.ds, self.dos_offset_dx(cpu), &filename);
                let access_mode = cpu.regs.al();
                let share_mode = access_mode & 0x70;

                // MDD-BUG-CRUCIBLE-00375: validate the access byte BEFORE any
                // special-name or synthetic-file interception can return.
                //
                // Real DOS decodes AL first — the access code is a property of
                // the request, not of whatever the name turns out to resolve
                // to. This validation used to sit after the device, GLIDE2X.OVL,
                // could open any intercepted name with reserved access bits and
                // get a handle back instead of error 12.
                if dos_access_read_write(access_mode).is_none() {
                    self.set_extended_error(12); // Invalid access code
                    cpu.regs.set_ax(12);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_open(
                        0x3D,
                        &filename,
                        None,
                        None,
                        access_mode,
                        share_mode,
                        None,
                        None,
                        Some(12),
                        caller_cs,
                        caller_ip,
                    );
                    return Ok(Int21Disposition::Iret);
                }

                // Check for special device names (EMS driver detection)
                let upper_name = filename.to_uppercase();
                let base_name = upper_name.rsplit(['\\', '/']).next().unwrap_or(&upper_name);

                if base_name == "EMMXXXX0" && self.ems_present {
                    // EMS driver detection - return special device handle
                    // Programs check for EMS by opening EMMXXXX0 device
                    // Handle 0xFF is used as a special EMS device marker
                    cpu.regs.set_ax(0x00FF);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_open(
                        0x3D,
                        &filename,
                        None,
                        None,
                        access_mode,
                        share_mode,
                        None,
                        Some(0x00FF),
                        None,
                        caller_cs,
                        caller_ip,
                    );
                    return Ok(Int21Disposition::Iret);
                }

                // Check for DOS device names (NUL, CON, AUX, PRN)
                if let Some(device_name) = crate::filesystem::parse_device_name(&filename) {
                    match device_name {
                        "NUL" => {
                            // NUL device - discards writes, returns EOF on reads
                            let nul_dev: Box<dyn crate::filesystem::VirtualFile> =
                                Box::new(crate::filesystem::NulDevice);
                            if let Some(handle) = self.file_table.allocate_with_sharing(
                                nul_dev,
                                "NUL".to_string(),
                                self.current_psp,
                                share_mode,
                                access_mode & 0x07,
                            ) {
                                self.clear_extended_error();
                                cpu.regs.set_ax(handle);
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                trace_dos_resource_open(
                                    0x3D,
                                    &filename,
                                    None,
                                    None,
                                    access_mode,
                                    share_mode,
                                    None,
                                    Some(handle),
                                    None,
                                    caller_cs,
                                    caller_ip,
                                );
                            } else {
                                self.set_extended_error(4);
                                cpu.regs.set_ax(4);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                trace_dos_resource_open(
                                    0x3D,
                                    &filename,
                                    None,
                                    None,
                                    access_mode,
                                    share_mode,
                                    None,
                                    None,
                                    Some(4),
                                    caller_cs,
                                    caller_ip,
                                );
                            }
                            return Ok(Int21Disposition::Iret);
                        }
                        "CON" => {
                            // CON device - use standard handles (0=stdin, 1=stdout)
                            // Return success with a special marker for CON
                            // CON maps to STDIN for reading, STDOUT for writing
                            cpu.regs.set_ax(0x00FE); // Special CON handle marker
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            trace_dos_resource_open(
                                0x3D,
                                &filename,
                                None,
                                None,
                                access_mode,
                                share_mode,
                                None,
                                Some(0x00FE),
                                None,
                                caller_cs,
                                caller_ip,
                            );
                            return Ok(Int21Disposition::Iret);
                        }
                        device @ ("AUX" | "PRN") => {
                            let result = self.open_char_device(device, share_mode, access_mode);
                            self.set_device_open_return(cpu, result);
                            trace_dos_resource_open(
                                0x3D,
                                &filename,
                                None,
                                None,
                                access_mode,
                                share_mode,
                                None,
                                result.ok(),
                                result.err(),
                                caller_cs,
                                caller_ip,
                            );
                            return Ok(Int21Disposition::Iret);
                        }
                        _ => {}
                    }
                }

                // Glide OVL interception: serve synthetic GLIDE2X.OVL
                if base_name == "GLIDE2X.OVL" {
                    if let Some(ovl_bytes) = self.get_glide_ovl_bytes() {
                        let ovl_file: Box<dyn crate::filesystem::VirtualFile> =
                            Box::new(crate::filesystem::SyntheticFile::new(ovl_bytes));
                        if let Some(handle) = self.file_table.allocate_synthetic_with_sharing(
                            ovl_file,
                            "GLIDE2X.OVL".to_string(),
                            self.current_psp,
                            share_mode,
                            access_mode & 0x07,
                            crate::files::SftBacking::GlideOvl,
                        ) {
                            tracing::info!("3dfx: intercepted GLIDE2X.OVL open, handle={}", handle);
                            self.clear_extended_error();
                            cpu.regs.set_ax(handle);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            trace_dos_resource_open(
                                0x3D,
                                &filename,
                                None,
                                None,
                                access_mode,
                                share_mode,
                                None,
                                Some(handle),
                                None,
                                caller_cs,
                                caller_ip,
                            );
                        } else {
                            self.set_extended_error(4);
                            cpu.regs.set_ax(4);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            trace_dos_resource_open(
                                0x3D,
                                &filename,
                                None,
                                None,
                                access_mode,
                                share_mode,
                                None,
                                None,
                                Some(4),
                                caller_cs,
                                caller_ip,
                            );
                        }
                        return Ok(Int21Disposition::Iret);
                    }
                }

                // Real DOS returns "file not found" for empty filenames.
                // LE loading and expect error 2, not error 5 (access denied).
                if filename.is_empty() {
                    self.set_extended_error(2); // File not found
                    cpu.regs.set_ax(2);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_open(
                        0x3D,
                        &filename,
                        None,
                        None,
                        access_mode,
                        share_mode,
                        None,
                        None,
                        Some(2),
                        caller_cs,
                        caller_ip,
                    );
                    return Ok(Int21Disposition::Iret);
                }

                let resolved = self.path_resolver.resolve(&filename);
                let trace_open = dos_trace_enabled() && should_trace_ah(0x3D);
                if trace_open {
                    tracing::debug!(
                        name = %filename,
                        resolved = %format!("{}:{}", (resolved.drive + b'A') as char, resolved.path),
                        "DOS open"
                    );
                }

                // Virtual file interception: serve in-memory bytes registered
                // DOS extenders that re-open argv[0] (e.g. a DOS extender)
                // find the decompressed image at the program's original DOS
                // path instead of a leaked host-temp-file basename.
                if let Some(bytes) = self.get_virtual_file_bytes(resolved.drive, &resolved.path) {
                    let vf: Box<dyn crate::filesystem::VirtualFile> =
                        Box::new(crate::filesystem::SyntheticFile::new(bytes));
                    let vf_key = Self::virtual_file_key(resolved.drive, &resolved.path);
                    if let Some(handle) = self.file_table.allocate_synthetic_with_sharing(
                        vf,
                        filename.clone(),
                        self.current_psp,
                        share_mode,
                        access_mode & 0x07,
                        crate::files::SftBacking::VirtualFile(vf_key),
                    ) {
                        tracing::debug!(
                            target: "gp64::dos::files",
                            path = %resolved.path,
                            handle,
                            "virtual file open intercepted"
                        );
                        self.clear_extended_error();
                        cpu.regs.set_ax(handle);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        trace_dos_resource_open(
                            0x3D,
                            &filename,
                            Some(resolved.drive),
                            Some(&resolved.path),
                            access_mode,
                            share_mode,
                            None,
                            Some(handle),
                            None,
                            caller_cs,
                            caller_ip,
                        );
                        return Ok(Int21Disposition::Iret);
                    } else {
                        self.set_extended_error(4);
                        cpu.regs.set_ax(4);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        trace_dos_resource_open(
                            0x3D,
                            &filename,
                            Some(resolved.drive),
                            Some(&resolved.path),
                            access_mode,
                            share_mode,
                            None,
                            None,
                            Some(4),
                            caller_cs,
                            caller_ip,
                        );
                        return Ok(Int21Disposition::Iret);
                    }
                }

                let sft_access = access_mode & 0x07;
                // Kept as defence in depth: the same predicate already ran at
                // the top of AH=3Dh (MDD-BUG-CRUCIBLE-00375), so this arm is
                // now unreachable for a request that got this far. Both sites
                // consult `dos_access_read_write`, so they cannot drift apart.
                let (_read, write) = match dos_access_read_write(access_mode) {
                    Some(rw) => rw,
                    None => {
                        // Modes 3-7 are reserved/invalid - return "Invalid access code"
                        self.set_extended_error(12); // Invalid access code
                        cpu.regs.set_ax(12);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        trace_dos_resource_open(
                            0x3D,
                            &filename,
                            Some(resolved.drive),
                            Some(&resolved.path),
                            access_mode,
                            share_mode,
                            None,
                            None,
                            Some(12),
                            caller_cs,
                            caller_ip,
                        );
                        return Ok(Int21Disposition::Iret);
                    }
                };

                // Check if drive exists first: a fitted drive with no media
                // is not ready, any other missing one invalid
                // (MDD-BUG-FLU-03064).
                if !self.drives.contains_key(&resolved.drive)
                    && self.not_ready_drives.contains(&resolved.drive)
                {
                    disposition = self.not_ready_drive_error(cpu, memory, 0x3D, resolved.drive);
                } else if !self.drives.contains_key(&resolved.drive) {
                    self.set_extended_error(15); // Invalid drive
                    cpu.regs.set_ax(15);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_open(
                        0x3D,
                        &filename,
                        Some(resolved.drive),
                        Some(&resolved.path),
                        access_mode,
                        share_mode,
                        None,
                        None,
                        Some(15),
                        caller_cs,
                        caller_ip,
                    );
                } else {
                    // Host-mount policy: a read/write open of a read-only
                    // file may acquire read access; later writes still fail.
                    // A write-only open requires write permission.
                    let mut effective_write = write;
                    if write && sft_access == 2 {
                        if let Ok(attrs) = self
                            .drives
                            .get_mut(&resolved.drive)
                            .unwrap()
                            .get_attributes(&resolved.path)
                        {
                            if attrs.read_only {
                                effective_write = false;
                                if trace_open {
                                    tracing::debug!(
                                        "DOS open: downgrading R/W to read-only (host file is read-only)"
                                    );
                                }
                            }
                        }
                    }
                    // Write-only mode (1) on a read-only file returns access denied
                    if write && sft_access == 1 {
                        if let Ok(attrs) = self
                            .drives
                            .get_mut(&resolved.drive)
                            .unwrap()
                            .get_attributes(&resolved.path)
                        {
                            if attrs.read_only {
                                self.set_extended_error(5); // Access denied
                                cpu.regs.set_ax(5);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                if trace_open {
                                    tracing::debug!("DOS open failed: read-only file");
                                }
                                trace_dos_resource_open(
                                    0x3D,
                                    &filename,
                                    Some(resolved.drive),
                                    Some(&resolved.path),
                                    access_mode,
                                    share_mode,
                                    Some(effective_write),
                                    None,
                                    Some(5),
                                    caller_cs,
                                    caller_ip,
                                );
                                return Ok(Int21Disposition::Iret);
                            }
                        }
                    }

                    // Check sharing violation before opening
                    let full_path =
                        format!("{}:{}", (resolved.drive + b'A') as char, resolved.path);
                    let file_identity = self.resolved_file_identity(resolved.drive, &resolved.path);
                    let entry = self.vshare.then(|| cpu.regs.clone());
                    if self.reject_pre_open_sharing_violation(
                        cpu,
                        &file_identity,
                        share_mode,
                        sft_access,
                    ) {
                        if let Some(critical) = entry.as_ref().and_then(|entry| {
                            self.vshare_open_critical(
                                memory,
                                entry,
                                0x3D,
                                resolved.drive,
                                share_mode,
                            )
                        }) {
                            return Ok(critical);
                        }
                        tracing::debug!(
                            target: "gp64::dos::files",
                            path = %filename,
                            requested_mode = format_args!("{:02X}", access_mode),
                            share_mode = format_args!("{:02X}", share_mode),
                            access = sft_access,
                            carry = true,
                            error = 32,
                            handle = -1,
                            "DOS file open response"
                        );
                        trace_dos_resource_open(
                            0x3D,
                            &filename,
                            Some(resolved.drive),
                            Some(&resolved.path),
                            access_mode,
                            share_mode,
                            Some(effective_write),
                            None,
                            Some(32),
                            caller_cs,
                            caller_ip,
                        );
                        return Ok(Int21Disposition::Iret);
                    }

                    let file_res = self.drives.get_mut(&resolved.drive).unwrap().open_file(
                        &resolved.path,
                        effective_write,
                        false,
                        false,
                    );

                    let is_hdd = resolved.drive >= 2;
                    match file_res {
                        Ok(f) => {
                            if let Some(handle) = self.file_table.allocate_with_sharing_identity(
                                f,
                                full_path,
                                file_identity,
                                self.current_psp,
                                share_mode,
                                sft_access,
                            ) {
                                self.clear_extended_error();
                                cpu.regs.set_ax(handle);
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                if is_hdd {
                                    self.emit_hdd_clicks(2);
                                }
                                trace_dos_resource_open(
                                    0x3D,
                                    &filename,
                                    Some(resolved.drive),
                                    Some(&resolved.path),
                                    access_mode,
                                    share_mode,
                                    Some(effective_write),
                                    Some(handle),
                                    None,
                                    caller_cs,
                                    caller_ip,
                                );
                                if trace_open {
                                    tracing::debug!(
                                        handle = format_args!("{:04X}", handle),
                                        "DOS open ok"
                                    );
                                }
                            } else {
                                self.set_extended_error(4); // Too many open files
                                cpu.regs.set_ax(4);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                trace_dos_resource_open(
                                    0x3D,
                                    &filename,
                                    Some(resolved.drive),
                                    Some(&resolved.path),
                                    access_mode,
                                    share_mode,
                                    Some(effective_write),
                                    None,
                                    Some(4),
                                    caller_cs,
                                    caller_ip,
                                );
                                if trace_open {
                                    tracing::debug!("DOS open failed: too many open files");
                                }
                            }
                        }
                        Err(_) => {
                            // Check if the path exists as a directory. On real
                            // DOS, opening a directory with INT 21h/3Dh returns
                            // error 5 (access denied), not error 2 (file not
                            // found). Many programs test directory existence by
                            // trying to open the path and checking for error 5.
                            let is_dir = self
                                .drives
                                .get_mut(&resolved.drive)
                                .and_then(|d| d.get_attributes(&resolved.path).ok())
                                .is_some_and(|a| a.directory);
                            if is_dir {
                                self.set_extended_error(5); // Access denied
                                cpu.regs.set_ax(5);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                trace_dos_resource_open(
                                    0x3D,
                                    &filename,
                                    Some(resolved.drive),
                                    Some(&resolved.path),
                                    access_mode,
                                    share_mode,
                                    Some(effective_write),
                                    None,
                                    Some(5),
                                    caller_cs,
                                    caller_ip,
                                );
                                if trace_open {
                                    tracing::debug!(
                                        "DOS open failed: is a directory (access denied)"
                                    );
                                }
                            } else {
                                // 2 (file not found) or 3 (path not found),
                                // depending on which component is missing.
                                let code =
                                    self.missing_dos_path_error(resolved.drive, &resolved.path);
                                self.set_extended_error(code);
                                cpu.regs.set_ax(code);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                trace_dos_resource_open(
                                    0x3D,
                                    &filename,
                                    Some(resolved.drive),
                                    Some(&resolved.path),
                                    access_mode,
                                    share_mode,
                                    Some(effective_write),
                                    None,
                                    Some(code),
                                    caller_cs,
                                    caller_ip,
                                );
                                if trace_open {
                                    tracing::debug!(code, "DOS open failed: not found");
                                }
                            }
                        }
                    }
                }
                let carry = cpu.regs.flags.contains(gp64_cpu::Flags::CARRY);
                tracing::debug!(
                    target: "gp64::dos::files",
                    path = %filename,
                    requested_mode = format_args!("{:02X}", access_mode),
                    share_mode = format_args!("{:02X}", share_mode),
                    access = access_mode & 0x07,
                    carry,
                    error = if carry { i32::from(cpu.regs.ax()) } else { -1 },
                    handle = if carry { -1 } else { i32::from(cpu.regs.ax()) },
                    "DOS file open response"
                );
            }

            // Close file
            0x3E => {
                let handle = cpu.regs.bx();
                if handle as usize >= self.file_table.std_handle_aliases.len() {
                    // DOS handles are 0..255; anything >= 256 is invalid
                    self.set_extended_error(6);
                    cpu.regs.set_ax(6);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_close(handle, None, Some(6), caller_cs, caller_ip);
                } else if handle == 0x00FF || handle == 0x00FE {
                    // Handle 0xFF = EMS device, 0xFE = CON device
                    self.clear_extended_error();
                    // Real DOS returns AX=0 on successful close
                    cpu.regs.set_ax(0);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_close(handle, None, None, caller_cs, caller_ip);
                } else if self.file_table.is_valid(handle) {
                    let resource_path = if dos_resource_trace_active() {
                        self.file_table.get_sft(handle).map(|sft| sft.path.clone())
                    } else {
                        None
                    };
                    match self.file_table.try_free(handle) {
                        Ok(true) => {
                            if let Some(slot) =
                                self.file_table.std_handle_aliases.get_mut(handle as usize)
                            {
                                *slot = None;
                            }
                            self.clear_extended_error();
                            // Real DOS returns AX=0 on successful close
                            cpu.regs.set_ax(0);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            trace_dos_resource_close(
                                handle,
                                resource_path.as_deref(),
                                None,
                                caller_cs,
                                caller_ip,
                            );
                        }
                        Ok(false) => {
                            self.set_extended_error(6);
                            cpu.regs.set_ax(6);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            trace_dos_resource_close(
                                handle,
                                resource_path.as_deref(),
                                Some(6),
                                caller_cs,
                                caller_ip,
                            );
                        }
                        Err(error) => {
                            tracing::warn!(
                                handle,
                                path = resource_path.as_deref().unwrap_or("<unknown>"),
                                %error,
                                "DOS file close failed to synchronize backing storage"
                            );
                            self.set_extended_error(5);
                            cpu.regs.set_ax(5);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            trace_dos_resource_close(
                                handle,
                                resource_path.as_deref(),
                                Some(5),
                                caller_cs,
                                caller_ip,
                            );
                        }
                    }
                } else if {
                    let had_alias = self
                        .file_table
                        .std_handle_aliases
                        .get(handle as usize)
                        .and_then(|o| o.as_ref())
                        .is_some();
                    if had_alias {
                        self.file_table.std_handle_aliases[handle as usize] = None;
                    }
                    had_alias
                } || handle <= 4
                {
                    self.clear_extended_error();
                    // Real DOS returns AX=0 on successful close
                    cpu.regs.set_ax(0);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_close(handle, None, None, caller_cs, caller_ip);
                } else {
                    self.set_extended_error(6); // Invalid handle
                    cpu.regs.set_ax(6);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_close(handle, None, Some(6), caller_cs, caller_ip);
                }
            }

            // Read from file
            0x3F => {
                // Snapshot CPU state for potential INT 24h Retry. Taken before
                // any function-specific logic modifies registers.
                // MDD-BUG-CRUCIBLE-00381: a stack clone, not a heap one. The
                // snapshot must still be taken HERE - registers are mutated
                // well before the error site, so deferring it would capture
                // post-mutation state and silently break INT 24h Retry. What
                // was removable is the allocation: `classify_io_error` runs
                // only on the error path and boxes it there, so a successful
                // read or write no longer pays a malloc and free.
                let entry_cpu_snapshot = cpu.regs.clone();
                let handle = cpu.regs.bx();
                // Resolve aliases first: a handle dup'd from CON (stdin/stdout/
                // stderr) aliases to the 0x00FE CON marker, which reads as stdin.
                let effective_handle = match self.resolve_alias_handle(handle) {
                    0x00FE => 0,
                    other => other,
                };
                let count = self.dos_count_cx(cpu) as usize;
                // MDD-BUG-KIL-01018: resolve the DESTINATION through the checked
                // segment + paging seam before any byte moves.
                let read_runs = self.dos_guest_buffer_runs(
                    cpu,
                    memory,
                    cpu.regs.ds,
                    self.dos_offset_dx(cpu),
                    count,
                    gp64_cpu::PageAccess::Write,
                )?;
                let use_eax = self.dos_api_32bit(cpu);

                let mut hdd_clicks: u8 = 0;
                let mut read_io_error: Option<std::io::Error> = None;
                let mut read_access_denied = false;
                let can_read_file = effective_handle >= 5
                    && self
                        .file_table
                        .get_sft(effective_handle)
                        .is_some_and(|entry| entry.allows_read());
                let read_lock_error = if can_read_file && count > 0 {
                    match self
                        .file_table
                        .read_region_lock_violation(effective_handle, count as u64)
                    {
                        Ok(true) => Some(33),
                        Ok(false) => None,
                        Err(error) => Some(error),
                    }
                } else {
                    None
                };
                let mut read_preflight_error = None;
                if let Some(file_handle) = self.file_table.get_sft_mut(effective_handle) {
                    let path = file_handle.path.clone();
                    if !file_handle.allows_read() {
                        cpu.regs.set_ax(5);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        trace_dos_read(effective_handle, &path, count, 0, false, None);
                        trace_dos_resource_read(
                            effective_handle,
                            &path,
                            count,
                            0,
                            None,
                            None,
                            Some(5),
                            &[],
                            false,
                            caller_cs,
                            caller_ip,
                        );
                        read_access_denied = true;
                    } else if let Some(error) = read_lock_error {
                        let position = file_handle.file.stream_position().ok();
                        cpu.regs.set_ax(error);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        trace_dos_read(effective_handle, &path, count, 0, false, position);
                        trace_dos_resource_read(
                            effective_handle,
                            &path,
                            count,
                            0,
                            position,
                            position,
                            Some(error),
                            &[],
                            false,
                            caller_cs,
                            caller_ip,
                        );
                        read_preflight_error = Some(error);
                    } else {
                        let mut buffer = vec![0u8; count.min(DOS_FILE_TRANSFER_CHUNK)];
                        let trace_limit = dos_read_transfer_preview_limit();
                        let mut trace_preview = Vec::with_capacity(trace_limit);
                        let mut bytes_read = 0usize;
                        let mut first_error = None;

                        while bytes_read < count {
                            let chunk_len = (count - bytes_read).min(DOS_FILE_TRANSFER_CHUNK);
                            if buffer.len() != chunk_len {
                                buffer.resize(chunk_len, 0);
                            }
                            match file_handle.file.read(&mut buffer[..chunk_len]) {
                                Ok(n) => {
                                    write_guest_span(memory, &read_runs, bytes_read, &buffer[..n]);
                                    push_transfer_preview(
                                        &mut trace_preview,
                                        trace_limit,
                                        &buffer[..n],
                                    );
                                    bytes_read += n;
                                    if n < chunk_len {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    first_error = Some(e);
                                    break;
                                }
                            }
                        }

                        let read_failed_without_bytes = if let Some(e) = first_error {
                            if bytes_read == 0 {
                                let pos = file_handle.file.stream_position().ok();
                                trace_dos_read(effective_handle, &path, count, 0, false, pos);
                                trace_dos_resource_read(
                                    effective_handle,
                                    &path,
                                    count,
                                    0,
                                    pos,
                                    pos,
                                    Some(5),
                                    &[],
                                    false,
                                    caller_cs,
                                    caller_ip,
                                );
                                // Save error for classify_io_error after borrow ends
                                read_io_error = Some(e);
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        if !read_failed_without_bytes {
                            if use_eax {
                                cpu.regs.eax = bytes_read as u32;
                            } else {
                                cpu.regs.set_ax(bytes_read as u16);
                            }
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            let pos = file_handle.file.stream_position().ok();
                            // MDD-BUG-CRUCIBLE-00441: keep the snapshot-visible
                            // position in step with the cursor this read moved,
                            // as the FCB paths (AH=14h/21h/22h) already do. The
                            // value is already in hand for tracing, so this
                            // costs no extra seek.
                            if let Some(p) = pos {
                                file_handle.position = p;
                            }
                            trace_dos_read(effective_handle, &path, count, bytes_read, true, pos);
                            let before_pos = pos.map(|p| p.saturating_sub(bytes_read as u64));
                            trace_dos_resource_read(
                                effective_handle,
                                &path,
                                count,
                                bytes_read,
                                before_pos,
                                pos,
                                None,
                                &trace_preview,
                                true,
                                caller_cs,
                                caller_ip,
                            );

                            // HDD seek sound detection for reads
                            if effective_handle >= 5
                                && bytes_read > 0
                                && should_emit_hdd_sound(&path)
                            {
                                if let Some(position_after_read) = pos {
                                    let position_before_read =
                                        position_after_read.saturating_sub(bytes_read as u64);
                                    if position_before_read == file_handle.last_read_end {
                                        // Sequential read
                                        file_handle.seq_read_bytes = file_handle
                                            .seq_read_bytes
                                            .saturating_add(bytes_read as u32);
                                        while file_handle.seq_read_bytes >= 32768 {
                                            hdd_clicks = hdd_clicks.saturating_add(1);
                                            file_handle.seq_read_bytes -= 32768;
                                        }
                                    } else {
                                        // Non-sequential read (seek detected)
                                        file_handle.seq_read_bytes = 0;
                                        let distance = position_before_read
                                            .abs_diff(file_handle.last_read_end);
                                        hdd_clicks = seek_distance_to_clicks(distance);
                                    }
                                    file_handle.last_read_end = position_after_read;
                                }
                            }
                        }
                    }
                } else if effective_handle == 3 || effective_handle == 4 {
                    // STDAUX(3) / STDPRN(4). These are pre-opened by DOS and
                    // deliberately have no SFT entry (the PSP hardcodes JFT
                    // 3/4 to literal SFT indices that EXEC increments), so
                    // they are served here exactly as AH=40h serves writes.
                    //
                    // Handle 3 returns what the bridge has staged from COM1;
                    // real DOS would block for a byte, which is the declared
                    // deviation in the AUX/PRN HLD. Handle 4 is output-only —
                    // a printer has nothing to say, so a read is a clean
                    // zero-byte EOF rather than an error.
                    let got = if effective_handle == 3 {
                        // Bounded by the bridge's own capacity, never by
                        // `count`: that is ECX for a 32-bit client, so sizing
                        // the buffer from it would let a guest ask us to
                        // allocate gigabytes.
                        let mut staged = vec![0u8; count.min(crate::filesystem::BRIDGE_CAPACITY)];
                        let got = self
                            .device_bridge
                            .lock()
                            .map(|mut bridge| {
                                let got = bridge.read_aux(&mut staged);
                                // The full `count` is what the guest asked for,
                                // so the shortfall against it is the demand the
                                // next drain fills.
                                bridge.note_aux_read(count, got);
                                got
                            })
                            .unwrap_or(0);
                        // Device bytes land through the same page walk as file
                        // bytes (MDD-BUG-CRU-01406).
                        write_guest_span(memory, &read_runs, 0, &staged[..got]);
                        got
                    } else {
                        0
                    };
                    self.dos_set_result_count(cpu, got);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else if effective_handle == 0 {
                    // STDIN - cooked-mode line-buffered input.
                    // On real DOS, handle 0 in cooked mode blocks until CR,
                    // echoes characters, supports backspace, and returns the
                    // complete line including CR+LF.  C runtime scanf/fgets
                    // depends on this behavior.

                    // RBIL INT 21/AH=3Fh: a zero-byte read is a no-op for any
                    // device including STDIN. Return AX=0, CF=0 immediately
                    // without consulting the keyboard buffer (which would
                    // otherwise block on cooked-mode wait_for_input).
                    if count == 0 {
                        self.dos_set_result_count(cpu, 0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                    }

                    // 1. Serve any previously completed line data first
                    if !self.stdin_line_buf.is_empty() {
                        let avail = self.stdin_line_buf.len().min(count);
                        write_guest_span(memory, &read_runs, 0, &self.stdin_line_buf[..avail]);
                        self.stdin_line_buf.drain(..avail);
                        self.dos_set_result_count(cpu, avail);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    } else {
                        // 2. Accumulate a new line character by character
                        if bda::kbd_peek(memory).is_none() {
                            // Cooked console input waits regardless of the video mode.
                            return Ok(Self::wait_for_input(cpu, memory, self.skip_iret_sync));
                        }

                        let (_sc, ascii) = bda::kbd_dequeue(memory).unwrap();

                        // Ctrl-C
                        if ascii == 0x03 {
                            self.stdin_line_acc.clear();
                            vga.teletype(b'^', 0, memory);
                            vga.teletype(b'C', 0, memory);
                            Self::trigger_ctrl_c(cpu, memory);
                            return Ok(Int21Disposition::CtrlC);
                        }

                        match ascii {
                            // Enter - complete the line
                            0x0D => {
                                vga.teletype(0x0D, 0, memory);
                                vga.teletype(0x0A, 0, memory);
                                let mut line = std::mem::take(&mut self.stdin_line_acc);
                                line.push(0x0D);
                                line.push(0x0A);
                                self.stdin_line_buf = line;
                                // Re-enter to serve from the now-filled buffer
                                Self::prep_input_wait(cpu, memory, self.skip_iret_sync);
                            }
                            // Backspace
                            0x08 => {
                                if !self.stdin_line_acc.is_empty() {
                                    self.stdin_line_acc.pop();
                                    vga.teletype(0x08, 0, memory);
                                    vga.teletype(b' ', 0, memory);
                                    vga.teletype(0x08, 0, memory);
                                }
                                Self::prep_input_wait(cpu, memory, self.skip_iret_sync);
                            }
                            // Escape - clear accumulated line
                            0x1B => {
                                self.stdin_line_acc.clear();
                                vga.teletype(b'\\', 0, memory);
                                vga.teletype(0x0D, 0, memory);
                                vga.teletype(0x0A, 0, memory);
                                Self::prep_input_wait(cpu, memory, self.skip_iret_sync);
                            }
                            // Tab
                            0x09 => {
                                let spaces = 8 - (self.stdin_line_acc.len() % 8);
                                for _ in 0..spaces {
                                    if self.stdin_line_acc.len() >= 254 {
                                        break;
                                    }
                                    self.stdin_line_acc.push(b' ');
                                    vga.teletype(b' ', 0, memory);
                                }
                                Self::prep_input_wait(cpu, memory, self.skip_iret_sync);
                            }
                            // Extended keys - ignore
                            0x00 | 0xE0 => {
                                Self::prep_input_wait(cpu, memory, self.skip_iret_sync);
                            }
                            // Regular printable characters
                            _ => {
                                // Cap at 254 chars (255 - 1 for CR), matching DOS CON limit
                                if ascii >= 0x20 && self.stdin_line_acc.len() < 254 {
                                    self.stdin_line_acc.push(ascii);
                                    vga.teletype(ascii, 0, memory);
                                }
                                Self::prep_input_wait(cpu, memory, self.skip_iret_sync);
                            }
                        }
                    }
                } else {
                    cpu.regs.set_ax(6);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_read(
                        effective_handle,
                        "<invalid>",
                        count,
                        0,
                        None,
                        None,
                        Some(6),
                        &[],
                        true,
                        caller_cs,
                        caller_ip,
                    );
                }
                // Classify I/O error outside the file_table borrow scope
                if let Some(error) = read_preflight_error {
                    self.set_extended_error(error);
                } else if read_access_denied {
                    self.set_extended_error(5);
                }
                if let Some(e) = read_io_error {
                    let disp = self.classify_io_error(
                        cpu,
                        memory,
                        &e,
                        0x3F,
                        Some(effective_handle),
                        false,
                        entry_cpu_snapshot,
                    );
                    disposition = disp;
                }
                if hdd_clicks > 0 {
                    self.emit_hdd_clicks(hdd_clicks);
                }
            }

            // Write to file/device
            0x40 => {
                // Snapshot CPU state for potential INT 24h Retry. Taken before
                // any function-specific logic modifies registers.
                // MDD-BUG-CRUCIBLE-00381: a stack clone, not a heap one. The
                // snapshot must still be taken HERE - registers are mutated
                // well before the error site, so deferring it would capture
                // post-mutation state and silently break INT 24h Retry. What
                // was removable is the allocation: `classify_io_error` runs
                // only on the error path and boxes it there, so a successful
                // read or write no longer pays a malloc and free.
                let entry_cpu_snapshot = cpu.regs.clone();
                let handle = cpu.regs.bx();
                // Resolve aliases first: a handle dup'd from CON (stdin/stdout/
                // stderr) aliases to the 0x00FE CON marker, which writes to stdout.
                let effective_handle = match self.resolve_alias_handle(handle) {
                    0x00FE => 1,
                    other => other,
                };
                let count = self.dos_count_cx(cpu) as usize;
                // MDD-BUG-KIL-01018: resolve the SOURCE through the checked
                // segment + paging seam before any byte moves.
                let write_runs = self.dos_guest_buffer_runs(
                    cpu,
                    memory,
                    cpu.regs.ds,
                    self.dos_offset_dx(cpu),
                    count,
                    gp64_cpu::PageAccess::Read,
                )?;
                let use_eax = self.dos_api_32bit(cpu);

                let mut write_io_error: Option<std::io::Error> = None;
                let mut write_access_denied = false;
                let can_write_file = effective_handle >= 5
                    && self
                        .file_table
                        .get_sft(effective_handle)
                        .is_some_and(|entry| entry.allows_write());
                let write_lock_error = if can_write_file {
                    let check = if count == 0 {
                        self.file_table
                            .truncate_region_lock_violation(effective_handle)
                    } else {
                        self.file_table
                            .write_region_lock_violation(effective_handle, count as u64)
                    };
                    match check {
                        Ok(true) => Some(33),
                        Ok(false) => None,
                        Err(error) => Some(error),
                    }
                } else {
                    None
                };
                let mut write_preflight_error = None;
                if let Some(file_handle) = self.file_table.get_sft_mut(effective_handle) {
                    if !file_handle.allows_write() {
                        cpu.regs.set_ax(5);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        write_access_denied = true;
                    } else if let Some(error) = write_lock_error {
                        cpu.regs.set_ax(error);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        write_preflight_error = Some(error);
                    } else if count == 0 {
                        // CX=0 means truncate file at current position
                        use std::io::Seek;
                        // Get current position
                        if let Ok(pos) = file_handle.file.stream_position() {
                            // MDD-BUG-CRUCIBLE-00441: a zero-length write is a
                            // truncate at the cursor; record where that is.
                            file_handle.position = pos;
                            // Truncate file to current position
                            if file_handle.file.set_len(pos).is_ok() {
                                self.dos_set_result_count(cpu, 0);
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            } else {
                                cpu.regs.set_ax(5);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            }
                        } else {
                            cpu.regs.set_ax(5);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    } else {
                        let trace_limit = dos_write_transfer_preview_limit();
                        let mut trace_preview = Vec::with_capacity(trace_limit);
                        let mut buf = vec![0u8; count.min(DOS_FILE_TRANSFER_CHUNK)];
                        let mut bytes_written = 0usize;
                        let mut first_error = None;

                        while bytes_written < count {
                            let chunk_len = (count - bytes_written).min(DOS_FILE_TRANSFER_CHUNK);
                            if buf.len() != chunk_len {
                                buf.resize(chunk_len, 0);
                            }
                            read_guest_span(memory, &write_runs, bytes_written, &mut buf);
                            push_transfer_preview(&mut trace_preview, trace_limit, &buf);

                            match file_handle.file.write(&buf) {
                                Ok(n) => {
                                    // Feed the exact prefix the file accepted while
                                    // it is still in the transfer buffer. Reading the
                                    // guest source again after the write would add
                                    // normal-run overhead, can perturb VGA latches,
                                    // and can observe bytes changed by device output.
                                    if let Some(marker) = self.dos_write_marker.as_mut() {
                                        marker.observe(&buf[..n.min(buf.len())]);
                                    }
                                    // Tee stdout/stderr to host even when SFT entry exists.
                                    if matches!(effective_handle, 1 | 2) {
                                        use std::io::Write;
                                        let written = &buf[..n.min(buf.len())];
                                        if effective_handle == 1 {
                                            let _ = std::io::stdout().write_all(written);
                                        } else {
                                            let _ = std::io::stderr().write_all(written);
                                        }
                                    }
                                    bytes_written += n;
                                    if n < chunk_len {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    first_error = Some(e);
                                    break;
                                }
                            }
                        }

                        trace_dos_write_preview(effective_handle, count, &trace_preview);
                        trace_dos_ah40_print_preview(effective_handle, count, &trace_preview);

                        let write_failed_without_bytes = if let Some(e) = first_error {
                            if bytes_written == 0 {
                                // Save error for classify_io_error after borrow ends
                                write_io_error = Some(e);
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        if !write_failed_without_bytes {
                            if use_eax {
                                cpu.regs.eax = bytes_written as u32;
                            } else {
                                cpu.regs.set_ax(bytes_written as u16);
                            }
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            // Update HDD seek tracking: a write moves the file
                            // position, so record it to avoid phantom seek clicks
                            // on a subsequent read.
                            {
                                use std::io::Seek;
                                if let Ok(pos) = file_handle.file.stream_position() {
                                    // MDD-BUG-CRUCIBLE-00441: a write moves the
                                    // cursor, so the snapshot-visible position
                                    // must follow it for EVERY handle.
                                    file_handle.position = pos;
                                    // HDD seek tracking is a device-sound
                                    // concern and stays limited to real files,
                                    // to avoid phantom clicks on a later read.
                                    if effective_handle >= 5 {
                                        file_handle.last_read_end = pos;
                                        file_handle.seq_read_bytes = 0;
                                    }
                                }
                            }
                        }
                    }
                } else {
                    match effective_handle {
                        1 | 2 => {
                            let trace_limit = dos_write_transfer_preview_limit();
                            let mut trace_preview = Vec::with_capacity(trace_limit);
                            let mut buf = vec![0u8; count.min(DOS_FILE_TRANSFER_CHUNK)];
                            let mut bytes_written = 0usize;
                            while bytes_written < count {
                                let chunk_len =
                                    (count - bytes_written).min(DOS_FILE_TRANSFER_CHUNK);
                                if buf.len() != chunk_len {
                                    buf.resize(chunk_len, 0);
                                }
                                read_guest_span(memory, &write_runs, bytes_written, &mut buf);
                                push_transfer_preview(&mut trace_preview, trace_limit, &buf);

                                // The no-SFT stdout/stderr fallback still reports
                                // every chunk as successfully written. Feed the
                                // exact pre-device bytes so the generic marker
                                // contract covers both file-backed and fallback
                                // AH=40 routes without rereading guest memory.
                                if let Some(marker) = self.dos_write_marker.as_mut() {
                                    marker.observe(&buf);
                                }

                                // Capture stdout/stderr if buffer is active.
                                if let Some(ref mut capture) = self.stdout_capture {
                                    capture.extend_from_slice(&buf);
                                }

                                // Write to VGA teletype for visual display.
                                for &byte in &buf {
                                    vga.teletype(byte, 0, memory);
                                }
                                // Also write to real process stdout/stderr so output
                                // is captured by shell redirection in headless mode.
                                use std::io::Write;
                                if effective_handle == 1 {
                                    let _ = std::io::stdout().write_all(&buf);
                                } else {
                                    let _ = std::io::stderr().write_all(&buf);
                                }
                                bytes_written += chunk_len;
                            }
                            trace_dos_write_preview(effective_handle, count, &trace_preview);
                            trace_dos_ah40_print_preview(effective_handle, count, &trace_preview);
                            if use_eax {
                                cpu.regs.eax = bytes_written as u32;
                            } else {
                                cpu.regs.set_ax(bytes_written as u16);
                            }
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                        // DOS pre-opens STDAUX(3) and STDPRN(4), and a C
                        // runtime's `fprintf(stdprn, …)` writes them without
                        // ever calling AH=3Dh. They are handled here rather
                        // than as SFT entries because the PSP hardcodes JFT
                        // 3/4 to the literal SFT indices 3/4 and EXEC
                        // increments those slots, so installing real entries
                        // there would leak counts across EXEC.
                        3 | 4 => {
                            // Chunked exactly like the `1 | 2` arm above, and
                            // for the same reason: `count` is ECX for a 32-bit
                            // client, so `vec![0u8; count]` is an allocation the
                            // guest chooses — a multi-gigabyte request aborts
                            // the process.
                            let mut buf = vec![0u8; count.min(DOS_FILE_TRANSFER_CHUNK)];
                            let mut bytes_written = 0usize;
                            while bytes_written < count {
                                let chunk_len =
                                    (count - bytes_written).min(DOS_FILE_TRANSFER_CHUNK);
                                if buf.len() != chunk_len {
                                    buf.resize(chunk_len, 0);
                                }
                                read_guest_span(memory, &write_runs, bytes_written, &mut buf);
                                let n = self.write_char_device(effective_handle, &buf);
                                bytes_written += n;
                                // A short return is honest: the bridge is
                                // bounded, and DOS lets a write report fewer
                                // bytes than asked. The bytes beyond it stay
                                // with the guest, so a retry sends each exactly
                                // once.
                                if n < chunk_len {
                                    break;
                                }
                            }
                            // Through the shared helper, so a 32-bit client sees
                            // the count in EAX and no stale bits above AX.
                            self.dos_set_result_count(cpu, bytes_written);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                        0 => {
                            cpu.regs.set_ax(5);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                        _ => {
                            cpu.regs.set_ax(6);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                }
                // Classify I/O error outside the file_table borrow scope
                if let Some(error) = write_preflight_error {
                    self.set_extended_error(error);
                } else if write_access_denied {
                    self.set_extended_error(5);
                }
                if let Some(e) = write_io_error {
                    let disp = self.classify_io_error(
                        cpu,
                        memory,
                        &e,
                        0x40,
                        Some(effective_handle),
                        true,
                        entry_cpu_snapshot,
                    );
                    disposition = disp;
                }
            }

            // Delete file
            0x41 => {
                let filename = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                let resolved = self.path_resolver.resolve(&filename);
                tracing::trace!(target: "gp64::dos::files", filename, drive = resolved.drive, path = %resolved.path, "delete file");

                // VSHARE refuses to delete a file open in a sharing mode or
                // held (MDD-BUG-FLU-03201): the critical error, then AX=2
                // with extended error 20h once the handler fails it.
                let vshare_refused = self.vshare && {
                    let identity = self.resolved_file_identity(resolved.drive, &resolved.path);
                    self.file_table
                        .vshare_delete_refused(&identity, &self.held_files)
                };
                if vshare_refused {
                    self.set_extended_error(SHARING_VIOLATION);
                    let crit_flag_addr =
                        Memory::seg_offset_to_linear(INDOS_SEGMENT, CRITICAL_ERROR_OFFSET);
                    if memory.read_u8(crit_flag_addr) == 0 {
                        return Ok(Int21Disposition::CriticalError {
                            ah_function: 0x41,
                            drive: resolved.drive,
                            error_code: SHARING_VIOLATION as u8,
                            is_write: false,
                            entry_snapshot: Box::new(cpu.regs.clone()),
                        });
                    }
                    cpu.regs.set_ax(2);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                    // DOS denies deletion with error 5 until AH=43h AL=01
                    // clears the read-only attribute.
                    let read_only = drive
                        .get_attributes(&resolved.path)
                        .is_ok_and(|attrs| attrs.read_only);
                    if read_only {
                        self.set_extended_error(5); // Access denied
                        cpu.regs.set_ax(5);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                    }
                    match drive.remove_file(&resolved.path) {
                        Ok(()) => {
                            self.clear_extended_error();
                            // Real DOS returns AX=0 on successful delete
                            cpu.regs.set_ax(0);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                        Err(_) => {
                            // 2 (file not found) or 3 (path not found),
                            // depending on which component is missing.
                            let code = self.missing_dos_path_error(resolved.drive, &resolved.path);
                            self.set_extended_error(code);
                            cpu.regs.set_ax(code);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                } else {
                    self.set_extended_error(3); // Path not found
                    cpu.regs.set_ax(3);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Seek
            0x42 => {
                let handle = self.resolve_alias_handle(cpu.regs.bx());
                let origin = cpu.regs.al();
                // Form offset as unsigned first to avoid sign-extension issues
                let offset_u32 = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
                // Reinterpret as signed for relative seeks
                let offset_i32 = offset_u32 as i32;
                let offset_display = if origin == 0 {
                    offset_u32 as i64
                } else {
                    offset_i32 as i64
                };

                // Perform seek in a limited borrow scope, collecting results
                let seek_outcome: Option<Result<u64, ()>> =
                    if let Some(file_handle) = self.file_table.get_sft_mut(handle) {
                        let path = file_handle.path.clone();
                        let before_pos = if resource_trace_enabled {
                            file_handle.file.stream_position().ok()
                        } else {
                            None
                        };
                        let seek_from = match origin {
                            0 => SeekFrom::Start(offset_u32 as u64),
                            1 => SeekFrom::Current(offset_i32 as i64),
                            2 => SeekFrom::End(offset_i32 as i64),
                            _ => {
                                cpu.regs.set_ax(1);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                trace_dos_seek(handle, &path, origin, offset_display, false, None);
                                trace_dos_resource_seek(
                                    handle,
                                    &path,
                                    origin,
                                    offset_display,
                                    before_pos,
                                    None,
                                    Some(1),
                                    caller_cs,
                                    caller_ip,
                                );
                                return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                            }
                        };

                        match file_handle.file.seek(seek_from) {
                            Ok(new_pos) => {
                                // MDD-BUG-CRUCIBLE-00441: the seek IS the
                                // position change, so record it.
                                file_handle.position = new_pos;
                                cpu.regs.set_dx((new_pos >> 16) as u16);
                                cpu.regs.set_ax((new_pos & 0xFFFF) as u16);
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                // HDD seek sound: distance-based clicks
                                let hdd_clicks = if handle >= 5
                                    && new_pos != file_handle.last_read_end
                                    && should_emit_hdd_sound(&path)
                                {
                                    let distance = new_pos.abs_diff(file_handle.last_read_end);
                                    file_handle.last_read_end = new_pos;
                                    seek_distance_to_clicks(distance)
                                } else {
                                    0
                                };
                                trace_dos_seek(
                                    handle,
                                    &path,
                                    origin,
                                    offset_display,
                                    true,
                                    Some(new_pos),
                                );
                                trace_dos_resource_seek(
                                    handle,
                                    &path,
                                    origin,
                                    offset_display,
                                    before_pos,
                                    Some(new_pos),
                                    None,
                                    caller_cs,
                                    caller_ip,
                                );
                                Some(Ok(hdd_clicks as u64))
                            }
                            Err(_) => {
                                cpu.regs.set_ax(25);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                trace_dos_seek(handle, &path, origin, offset_display, false, None);
                                trace_dos_resource_seek(
                                    handle,
                                    &path,
                                    origin,
                                    offset_display,
                                    before_pos,
                                    None,
                                    Some(25),
                                    caller_cs,
                                    caller_ip,
                                );
                                Some(Err(()))
                            }
                        }
                    } else {
                        None
                    };

                // Handle deferred self-borrows after file_handle scope ends
                match seek_outcome {
                    Some(Ok(clicks)) => {
                        self.clear_extended_error();
                        if clicks > 0 {
                            self.emit_hdd_clicks(clicks as u8);
                        }
                    }
                    Some(Err(())) => {
                        self.set_extended_error(25); // Seek error
                    }
                    None => {
                        self.set_extended_error(6); // Invalid handle
                        cpu.regs.set_ax(6);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        trace_dos_seek(handle, "<invalid>", origin, offset_display, false, None);
                        trace_dos_resource_seek(
                            handle,
                            "<invalid>",
                            origin,
                            offset_display,
                            None,
                            None,
                            Some(6),
                            caller_cs,
                            caller_ip,
                        );
                    }
                }
            }

            // Get/set file attributes
            0x43 => {
                let filename = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                trace_dos_path("ATTR", cpu.regs.ds, self.dos_offset_dx(cpu), &filename);
                let subfunction = cpu.regs.al();

                // Report the DOS archive attribute for a registered overlay.
                if subfunction == 0 {
                    let upper = filename.to_uppercase();
                    let base = upper.rsplit(['\\', '/']).next().unwrap_or(&upper);
                    if base == "GLIDE2X.OVL" && self.glide_ovl_data.is_some() {
                        tracing::info!("3dfx: intercepted GLIDE2X.OVL attribute check");
                        cpu.regs.set_cx(0x20); // archive attribute
                        cpu.regs.set_ah(0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                    }
                }

                // Registered virtual files report DOS archive attributes.
                if subfunction == 0 {
                    let resolved_probe = self.path_resolver.resolve(&filename);
                    if self.has_virtual_file(resolved_probe.drive, &resolved_probe.path) {
                        tracing::debug!(
                            target: "gp64::dos::files",
                            path = %resolved_probe.path,
                            "virtual file attribute check intercepted"
                        );
                        cpu.regs.set_cx(0x20); // archive attribute (read-only would also work)
                        cpu.regs.set_ah(0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                    }
                }

                let resolved = self.path_resolver.resolve(&filename);

                if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                    if subfunction == 0 {
                        match drive.get_attributes(&resolved.path) {
                            Ok(attrs) => {
                                cpu.regs.set_cx(dos_attr_bits(&attrs));
                                // Clear AH on success (the DOS model compatibility)
                                cpu.regs.set_ah(0);
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                            Err(_) => {
                                cpu.regs.set_ax(2);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            }
                        }
                    } else if subfunction == 1 {
                        let attrs = dos_attrs_from_bits(cpu.regs.cx());
                        match drive.set_attributes(&resolved.path, attrs) {
                            Ok(()) => {
                                // Clear AH on success (the DOS model compatibility)
                                cpu.regs.set_ah(0);
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                            Err(e) => {
                                cpu.regs.set_ax(dos_set_attr_error_code(&e));
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            }
                        }
                    } else {
                        cpu.regs.set_ax(1);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                } else {
                    cpu.regs.set_ax(3);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
                let carry = cpu.regs.flags.contains(gp64_cpu::Flags::CARRY);
                tracing::debug!(
                    target: "gp64::dos::files",
                    path = %filename,
                    resolved_path = %resolved.path,
                    drive = resolved.drive,
                    subfunction,
                    attributes = format_args!("{:04X}", cpu.regs.cx()),
                    carry,
                    error = if carry { u32::from(cpu.regs.ax()) } else { 0 },
                    "DOS file attribute response"
                );
            }

            // IOCTL - Enhanced implementation
            0x44 => {
                let subfunc = cpu.regs.al();
                let handle = cpu.regs.bx();
                let handle = if handle == 0x00FE || handle == 0x00FF {
                    handle
                } else {
                    self.resolve_alias_handle(handle)
                };
                let handle_is_file = self.file_table.is_valid(handle);
                match subfunc {
                    // 00h - Get device information
                    0x00 => {
                        let device_info = if handle_is_file {
                            // A character device opened by name (NUL/CON/AUX/PRN) lives
                            // in the file table but must report DEVICE flags, not file
                            // (drive) flags — bit 7 (ISDEV) set, plus the device's bits.
                            let dev = self
                                .file_table
                                .get_sft(handle)
                                .and_then(|sft| crate::filesystem::parse_device_name(&sft.path));
                            if let Some(dev) = dev {
                                match dev {
                                    "NUL" => 0x80C4, // ISDEV | ISNUL | NOTEOF
                                    "CON" => 0x80D3, // ISDEV | ISCIN | ISCOT | SPECIAL | NOTEOF
                                    _ => 0x80C0,     // AUX/PRN: ISDEV | NOTEOF
                                }
                            } else {
                                // File: drive number in bits 0-5, bit 6 = not written
                                let drive = self
                                    .file_table
                                    .get_sft(handle)
                                    .and_then(|sft| {
                                        files::PathResolver::split_drive_and_path(&sft.path).0
                                    })
                                    .unwrap_or_else(|| self.path_resolver.get_current_drive());
                                (drive as u16) | 0x0040
                            }
                        } else {
                            match handle {
                                0x00FF => 0x80C0, // EMS device (EMMXXXX0)
                                0x00FE => 0x80D3, // CON device
                                // Console handles (stdin/stdout/stderr) are the
                                // CON device: ISDEV | ISCIN | ISCOT | SPECIAL | NOTEOF.
                                0..=2 => 0x80D3,
                                // stdaux (COM1) and stdprn (LPT1) are character
                                // devices but NOT the console: ISDEV | NOTEOF only.
                                3..=4 => 0x80C0,
                                _ => {
                                    self.set_error_return(cpu, 6); // Invalid handle
                                    return Ok(disposition);
                                }
                            }
                        };
                        cpu.regs.set_dx(device_info);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 01h - Set device information (character devices only)
                    0x01 => {
                        if handle_is_file {
                            self.set_error_return(cpu, 1); // Invalid function for files
                        } else if handle <= 4 || handle == 0x00FE {
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            self.set_error_return(cpu, 1); // Invalid function for files
                        }
                    }

                    // 02h - Read from character device control channel
                    0x02 => {
                        // Not supported - return 0 bytes read
                        cpu.regs.set_ax(0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 03h - Write to character device control channel
                    0x03 => {
                        // Accept but ignore - pretend we wrote everything
                        let count = cpu.regs.cx();
                        cpu.regs.set_ax(count);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 04h - Read from block device control channel
                    0x04 => {
                        cpu.regs.set_ax(0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 05h - Write to block device control channel
                    0x05 => {
                        let count = cpu.regs.cx();
                        cpu.regs.set_ax(count);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 06h - Check input status
                    0x06 => {
                        // 0xFE = CON device, 0xFF = EMS device (EMMXXXX0)
                        if handle <= 4 || handle == 0x00FE || handle == 0x00FF || handle_is_file {
                            // AUX is the one input device whose readiness we
                            // actually know, so it reports the truth: a guest
                            // polls this before AH=3Fh, and answering "ready"
                            // unconditionally turns the zero-byte read that
                            // follows into a soft EOF. The answer covers both
                            // what the bridge has already staged and what is
                            // still sitting in COM1's receiver, and it consumes
                            // nothing and asks for nothing — a status call that
                            // moved a byte would steal it from the read it is
                            // gating and from a direct RBR read. Every other
                            // handle keeps the unconditional ready answer.
                            let is_aux = handle == 3
                                || self.file_table.get_sft(handle).and_then(|sft| {
                                    crate::filesystem::parse_device_name(&sft.path)
                                }) == Some("AUX");
                            let ready = !is_aux
                                || self
                                    .device_bridge
                                    .lock()
                                    .map(|bridge| bridge.aux_input_ready())
                                    .unwrap_or(false);
                            cpu.regs.set_al(if ready { 0xFF } else { 0x00 });
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            self.set_error_return(cpu, 6); // Invalid handle
                        }
                    }

                    // 07h - Check output status
                    0x07 => {
                        // 0xFE = CON device, 0xFF = EMS device (EMMXXXX0)
                        if handle <= 4 || handle == 0x00FE || handle == 0x00FF || handle_is_file {
                            cpu.regs.set_al(0xFF); // Ready
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            self.set_error_return(cpu, 6); // Invalid handle
                        }
                    }

                    // 08h - Check if block device is removable
                    0x08 => {
                        let drive = if cpu.regs.bl() == 0 {
                            self.path_resolver.get_current_drive()
                        } else {
                            cpu.regs.bl() - 1
                        };
                        // A: and B: are removable (floppy), C:+ are fixed
                        cpu.regs.set_ax(if drive < 2 { 0 } else { 1 });
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 09h - Check if block device is remote
                    0x09 => {
                        // DX bit 12 set = remote device
                        cpu.regs.set_dx(0x0000); // All local
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 0Ah - Check if handle is remote
                    0x0A => {
                        if handle <= 4 || handle_is_file {
                            cpu.regs.set_dx(0x0000); // Local
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ax(6);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }

                    // 0Bh - Set sharing retry count
                    0x0B => {
                        // Accept but ignore (no DOS sharing driver emulation)
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 0Ch - Generic IOCTL for character devices
                    0x0C => {
                        cpu.regs.set_ax(1); // Not supported
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }

                    // 0Dh - Generic IOCTL for block devices
                    0x0D => {
                        let minor = cpu.regs.cl();
                        match minor {
                            0x40 => cpu.regs.flags.remove(gp64_cpu::Flags::CARRY), // Set device params
                            0x41 => cpu.regs.flags.remove(gp64_cpu::Flags::CARRY), // Write track
                            0x42 => cpu.regs.flags.remove(gp64_cpu::Flags::CARRY), // Format track
                            0x46 => cpu.regs.flags.remove(gp64_cpu::Flags::CARRY), // Set media ID
                            0x47 => cpu.regs.flags.remove(gp64_cpu::Flags::CARRY), // Set access flag
                            0x60 => cpu.regs.flags.remove(gp64_cpu::Flags::CARRY), // Get device params
                            0x61 => cpu.regs.flags.remove(gp64_cpu::Flags::CARRY), // Read track
                            0x62 => cpu.regs.flags.remove(gp64_cpu::Flags::CARRY), // Verify track
                            0x66 => cpu.regs.flags.remove(gp64_cpu::Flags::CARRY), // Get media ID
                            0x67 => {
                                cpu.regs.set_al(0); // Access flag = not accessed
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                            0x68 => {
                                cpu.regs.set_al(0x02); // Sense media type: 720K 3.5"
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                            _ => {
                                cpu.regs.set_ax(1);
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            }
                        }
                    }

                    // 0Eh - Get logical drive map
                    0x0E => {
                        cpu.regs.set_al(0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 0Fh - Set logical drive map
                    0x0F => {
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 10h - Query IOCTL device support
                    0x10 => {
                        cpu.regs.set_ax(0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    // 11h - Query IOCTL block support
                    0x11 => {
                        cpu.regs.set_ax(0);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }

                    _ => {
                        cpu.regs.set_ax(1);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Duplicate file handle
            0x45 => {
                let handle = self.resolve_alias_handle(cpu.regs.bx());
                if self.file_table.is_valid(handle) {
                    if let Some(new_handle) = self.file_table.duplicate(handle, self.current_psp) {
                        // Successfully duplicated - new handle shares file position with original
                        cpu.regs.set_ax(new_handle);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    } else {
                        self.set_error_return(cpu, 4); // Too many open files
                    }
                } else if handle <= 4 || handle == 0x00FE {
                    // A dup of CON (stdin/stdout/stderr, or a prior CON dup =
                    // 0x00FE) snapshots to the stable 0x00FE CON marker, NOT to
                    // the source handle number: a live alias to handle 1 would
                    // wrongly follow handle 1 into a later redirect (e.g. dup2 of
                    // a file onto stdout), so the saved dup could not restore CON.
                    // AUX(3)/PRN(4) keep the by-number alias.
                    let target = if handle <= 2 || handle == 0x00FE {
                        0x00FE
                    } else {
                        handle
                    };
                    if let Some(new_handle) = self.allocate_alias_handle() {
                        self.file_table.std_handle_aliases[new_handle as usize] = Some(target);
                        cpu.regs.set_ax(new_handle);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    } else {
                        self.set_error_return(cpu, 4); // Too many open files
                    }
                } else {
                    self.set_error_return(cpu, 6); // Invalid handle
                }
            }

            // Force duplicate handle (redirect BX to CX)
            0x46 => {
                let src_handle = self.resolve_alias_handle(cpu.regs.bx());
                let dst_handle = cpu.regs.cx();

                if dst_handle as usize >= self.file_table.std_handle_aliases.len() {
                    // DOS handles are 0..255; anything >= 256 is invalid
                    self.set_error_return(cpu, 6);
                } else if src_handle == dst_handle {
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else if self.file_table.is_valid(src_handle) {
                    if self
                        .file_table
                        .force_duplicate(src_handle, dst_handle, self.current_psp)
                    {
                        // Successfully redirected dst to src's file (sharing position)
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    } else {
                        self.set_error_return(cpu, 6); // Invalid handle
                    }
                } else if src_handle <= 4 || src_handle == 0x00FE {
                    // src is a standard device (0-4) or the CON marker (0x00FE,
                    // e.g. a saved dup of stdout): point dst at it, dropping any
                    // file redirect currently occupying dst so it reverts to the
                    // device (restores CON after a dup2(file, stdout)/restore).
                    if self.file_table.is_valid(dst_handle) {
                        self.file_table.free(dst_handle);
                    }
                    if let Some(slot) = self
                        .file_table
                        .std_handle_aliases
                        .get_mut(dst_handle as usize)
                    {
                        *slot = Some(src_handle);
                    }
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    self.set_error_return(cpu, 6); // Invalid handle
                }
            }

            // Get current directory
            0x47 => {
                let drive = if cpu.regs.dl() == 0 {
                    self.path_resolver.get_current_drive()
                } else {
                    cpu.regs.dl() - 1
                };

                if !self.drives.contains_key(&drive) {
                    self.set_error_return(cpu, 15); // Invalid drive
                } else {
                    let current_dir = self.path_resolver.get_current_dir(drive);
                    let buffer_addr = self.dos_translate_ds_si(cpu);

                    let path_no_leading = current_dir.trim_start_matches('\\');
                    // DOS buffer is 64 bytes max (63 chars + null terminator)
                    let max_len = 63.min(path_no_leading.len());
                    for (i, &byte) in path_no_leading.as_bytes()[..max_len].iter().enumerate() {
                        memory.write_u8(buffer_addr + i, byte);
                    }
                    memory.write_u8(buffer_addr + max_len, 0);

                    tracing::debug!(
                        target: "gp64::dos",
                        drive = drive,
                        dir = %path_no_leading,
                        len = max_len,
                        "INT 21h/47h: get current directory"
                    );

                    cpu.regs.set_ax(0x0100);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                }
            }

            // Allocate memory
            0x48 => {
                let paragraphs = cpu.regs.bx();
                match self.dos_mem.alloc_with_strategy_and_umb(
                    memory,
                    self.current_psp,
                    paragraphs,
                    self.memory_strategy,
                    self.umb_linked,
                ) {
                    Ok(seg) => {
                        self.clear_extended_error();
                        // DPMI §4.4: In PM, return a selector instead of
                        // a segment so the caller can load it into a
                        // segment register without #NP.
                        if self.dpmi.in_protected_mode() {
                            if let Some(sel) =
                                self.dpmi.dos_alloc_selector(cpu, memory, seg, paragraphs)
                            {
                                cpu.regs.set_ax(sel);
                            } else {
                                // No descriptors available — free the DOS
                                // block and report insufficient memory.
                                let _ = self.dos_mem.free(memory, seg);
                                self.set_extended_error(8);
                                cpu.regs.set_ax(8);
                                cpu.regs.set_bx(self.dos_mem.largest_free());
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                return Ok(Int21Disposition::Iret);
                            }
                        } else {
                            cpu.regs.set_ax(seg);
                        }
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        if dos_mem_trace_enabled() {
                            tracing::debug!(
                                target: "gp64::dos::memory",
                                op = "ALLOC",
                                status = "ok",
                                paragraphs,
                                seg = format_args!("{:04X}", seg),
                                ax = format_args!("{:04X}", cpu.regs.ax()),
                                pm = self.dpmi.in_protected_mode(),
                                psp = format_args!("{:04X}", self.current_psp),
                                "DOS memory"
                            );
                        }
                    }
                    Err(DosMemError::InsufficientMemory { largest_available }) => {
                        self.set_extended_error(8); // Insufficient memory
                        cpu.regs.set_ax(8);
                        cpu.regs.set_bx(largest_available);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        if dos_mem_trace_enabled() {
                            tracing::debug!(
                                target: "gp64::dos::memory",
                                op = "ALLOC",
                                status = "err",
                                err = "INSUFFICIENT",
                                paragraphs,
                                largest_available,
                                "DOS memory"
                            );
                        }
                    }
                    Err(_) => {
                        self.set_extended_error(8); // Insufficient memory
                        cpu.regs.set_ax(8);
                        cpu.regs.set_bx(self.dos_mem.largest_free());
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        if dos_mem_trace_enabled() {
                            tracing::debug!(
                                target: "gp64::dos::memory",
                                op = "ALLOC",
                                status = "err",
                                err = "INVALID",
                                paragraphs,
                                largest = self.dos_mem.largest_free(),
                                "DOS memory"
                            );
                        }
                    }
                }
            }

            // Free memory
            0x49 => {
                // In PM, ES holds a selector — translate to the
                // underlying DOS segment before freeing.
                let segment = if self.dpmi.in_protected_mode() {
                    self.dpmi
                        .dos_selector_to_segment(cpu.regs.es)
                        .unwrap_or(cpu.regs.es)
                } else {
                    cpu.regs.es
                };
                match self.dos_mem.free(memory, segment) {
                    Ok(()) => {
                        // Free the corresponding DPMI selector if in PM
                        if self.dpmi.in_protected_mode()
                            && self.dpmi.dos_selector_to_segment(cpu.regs.es).is_some()
                        {
                            self.dpmi.dos_free_selector(cpu, memory, cpu.regs.es);
                        }
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        if dos_mem_trace_enabled() {
                            tracing::debug!(
                                target: "gp64::dos::memory",
                                op = "FREE",
                                status = "ok",
                                seg = format_args!("{:04X}", segment),
                                "DOS memory"
                            );
                        }
                    }
                    Err(DosMemError::InvalidBlock) => {
                        cpu.regs.set_ax(9);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        if dos_mem_trace_enabled() {
                            if let Some(info) = self.dos_mem.block_info_for_segment(segment) {
                                tracing::debug!(
                                    target: "gp64::dos::memory",
                                    op = "FREE",
                                    status = "err",
                                    err = "INVALID",
                                    seg = format_args!("{:04X}", segment),
                                    psp = format_args!("{:04X}", self.current_psp),
                                    env = format_args!("{:04X}", self.env_segment),
                                    mcb = format_args!("{:04X}", info.mcb_seg),
                                    usable = format_args!("{:04X}", info.usable_seg),
                                    end = format_args!("{:04X}", info.end_seg),
                                    paras = info.size_paras,
                                    owner = format_args!("{:04X}", info.owner_psp),
                                    is_free = info.is_free,
                                    "DOS memory"
                                );
                            } else {
                                tracing::debug!(
                                    target: "gp64::dos::memory",
                                    op = "FREE",
                                    status = "err",
                                    err = "INVALID",
                                    seg = format_args!("{:04X}", segment),
                                    psp = format_args!("{:04X}", self.current_psp),
                                    env = format_args!("{:04X}", self.env_segment),
                                    block = "NONE",
                                    "DOS memory"
                                );
                            }
                        }
                    }
                    Err(_) => {
                        cpu.regs.set_ax(9);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        if dos_mem_trace_enabled() {
                            tracing::debug!(
                                target: "gp64::dos::memory",
                                op = "FREE",
                                status = "err",
                                err = "UNKNOWN",
                                seg = format_args!("{:04X}", segment),
                                "DOS memory"
                            );
                        }
                    }
                }
            }

            // Resize memory block
            0x4A => {
                // In PM, ES holds a selector — translate to segment.
                let segment = if self.dpmi.in_protected_mode() {
                    self.dpmi
                        .dos_selector_to_segment(cpu.regs.es)
                        .unwrap_or(cpu.regs.es)
                } else {
                    cpu.regs.es
                };
                let new_paragraphs = cpu.regs.bx();
                let pm_selector = if self.dpmi.in_protected_mode()
                    && self.dpmi.dos_selector_to_segment(cpu.regs.es).is_some()
                {
                    Some(cpu.regs.es)
                } else {
                    None
                };
                if pm_selector.is_some_and(|selector| {
                    !self
                        .dpmi
                        .dos_can_resize_selector(cpu, selector, new_paragraphs)
                }) {
                    self.set_extended_error(8);
                    cpu.regs.set_ax(8);
                    cpu.regs.set_bx(
                        self.dos_mem
                            .max_resize_for_segment(segment)
                            .unwrap_or(0)
                            .min(
                                pm_selector
                                    .and_then(|selector| {
                                        self.dpmi.dos_max_resize_selector_paragraphs(cpu, selector)
                                    })
                                    .unwrap_or(u16::MAX),
                            ),
                    );
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(Int21Disposition::Iret);
                }
                // Try conventional memory first, fall back to UMB
                let result = match self.dos_mem.resize(memory, segment, new_paragraphs) {
                    Err(DosMemError::InvalidBlock) => {
                        self.dos_mem.resize_umb(memory, segment, new_paragraphs)
                    }
                    other => other,
                };
                match result {
                    Ok(()) => {
                        // Update the DPMI selector limit if in PM
                        if let Some(selector) = pm_selector {
                            self.dpmi
                                .dos_resize_selector(cpu, memory, selector, new_paragraphs);
                        }
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        if dos_mem_trace_enabled() {
                            tracing::debug!(
                                target: "gp64::dos::memory",
                                op = "RESIZE",
                                status = "ok",
                                seg = format_args!("{:04X}", segment),
                                paras = new_paragraphs,
                                "DOS memory"
                            );
                        }
                    }
                    Err(DosMemError::InsufficientMemory { largest_available }) => {
                        cpu.regs.set_ax(8);
                        cpu.regs.set_bx(largest_available);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        if dos_mem_trace_enabled() {
                            tracing::debug!(
                                target: "gp64::dos::memory",
                                op = "RESIZE",
                                status = "err",
                                err = "INSUFFICIENT",
                                seg = format_args!("{:04X}", segment),
                                paras = new_paragraphs,
                                largest = largest_available,
                                "DOS memory"
                            );
                        }
                    }
                    Err(DosMemError::InvalidBlock) => {
                        cpu.regs.set_ax(9);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        if dos_mem_trace_enabled() {
                            tracing::debug!(
                                target: "gp64::dos::memory",
                                op = "RESIZE",
                                status = "err",
                                err = "INVALID",
                                seg = format_args!("{:04X}", segment),
                                paras = new_paragraphs,
                                "DOS memory"
                            );
                        }
                    }
                }
            }

            // Get/Set memory allocation strategy
            0x58 => {
                let subfunc = cpu.regs.al();
                match subfunc {
                    // Get allocation strategy
                    0x00 => {
                        cpu.regs.set_ax(self.memory_strategy as u16);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // Set allocation strategy
                    0x01 => {
                        let strategy = cpu.regs.bl();
                        // Valid strategies: 0-2 (low byte) with optional high bits
                        // 0x00 = First fit (low memory first)
                        // 0x01 = Best fit (smallest block that fits)
                        // 0x02 = Last fit (high memory first)
                        // 0x40 = Best fit (high memory first)
                        // 0x41 = Best fit (high then low)
                        // 0x80 = First fit (high memory first)
                        // 0x81 = Best fit (high then low)
                        // 0x82 = Last fit (high then low)
                        let base_strategy = strategy & 0x3F;
                        if base_strategy <= 2 {
                            self.memory_strategy = strategy;
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ax(1); // Invalid function
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                    // Get UMB link state
                    0x02 => {
                        cpu.regs.set_al(if self.umb_linked { 1 } else { 0 });
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    // Set UMB link state
                    0x03 => {
                        let link = cpu.regs.bx();
                        if link <= 1 {
                            let was_linked = self.umb_linked;
                            self.umb_linked = link == 1;
                            if self.umb_linked != was_linked {
                                tracing::debug!(
                                    target: "gp64::dos::memory",
                                    "UMB link state: {} (UMBs {})",
                                    if self.umb_linked { "linked" } else { "unlinked" },
                                    if self.dos_mem.is_umb_initialized() { "available" } else { "not available" }
                                );
                            }
                            // Note: we do NOT auto-create UMBs here.  On real
                            // hardware, UMBs only exist if a UMB provider
                            // (UMB manager) was loaded during CONFIG.SYS
                            // processing.  Setting the link flag merely tells
                            // DOS to include existing UMBs in the MCB chain —
                            // it never creates new ones.
                            //
                            // Update MCB chain linkage in memory so programs
                            // walking the chain see continuous conventional->UMB
                            self.dos_mem.write_umb_link(memory, self.umb_linked);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ax(1); // Invalid function
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                    _ => {
                        cpu.regs.set_ax(1); // Invalid subfunction
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Execute program
            0x4B => {
                let al = cpu.regs.al();
                match al {
                    0x00 => {
                        self.exec_program(cpu, memory)?;
                        if !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY) {
                            disposition = Int21Disposition::NoIret;
                        }
                    }
                    0x01 => {
                        // Load but don't execute
                        self.exec_load_only(cpu, memory)?;
                    }
                    0x03 => {
                        self.load_overlay(cpu, memory)?;
                    }
                    _ => {
                        cpu.regs.set_ax(1); // Invalid function
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Terminate with return code
            0x4C => {
                let exit_code = cpu.regs.al();

                // Log process state before cleanup for termination diagnostics
                tracing::warn!(target: "gp64::exit",
                    "INT 21h/4Ch: exit_code={} CS:IP={:04X}:{:04X} \
                     AX={:04X} BX={:04X} CX={:04X} DX={:04X} \
                     SI={:04X} DI={:04X} BP={:04X} SP={:04X} \
                     DS={:04X} ES={:04X} SS={:04X} FLAGS={:04X} \
                     PM={} PSP={:04X}",
                    exit_code,
                    cpu.regs.cs, cpu.regs.ip(),
                    cpu.regs.ax(), cpu.regs.bx(), cpu.regs.cx(), cpu.regs.dx(),
                    cpu.regs.si(), cpu.regs.di(), cpu.regs.bp(), cpu.regs.sp(),
                    cpu.regs.ds, cpu.regs.es, cpu.regs.ss,
                    cpu.regs.flags.bits(),
                    self.dpmi.in_protected_mode(),
                    self.current_psp);

                disposition = self.terminate_process(cpu, memory, exit_code, 0x00)?;
            }

            // Get return code
            0x4D => {
                // AL = exit code, AH = termination type
                let status =
                    (self.last_exit_code as u16) | ((self.last_termination_type as u16) << 8);
                cpu.regs.set_ax(status);
                self.last_exit_code = 0;
                self.last_termination_type = 0;
            }

            // Find first file
            0x4E => {
                let pattern = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                let attr_filter = cpu.regs.cx() as u8;
                match self.find_first(&pattern, attr_filter) {
                    Ok(search_id) => {
                        if let Some(entry) = self.find_get_entry(search_id) {
                            trace_dos_find("first", &pattern, attr_filter, Some(&entry), true);
                            self.write_dta_entry(memory, search_id, &entry);
                            self.clear_extended_error();
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            trace_dos_find("first", &pattern, attr_filter, None, false);
                            self.set_extended_error(18); // No more files
                            cpu.regs.set_ax(18);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                    Err(_) => {
                        trace_dos_find("first", &pattern, attr_filter, None, false);
                        // 18 when nothing matched, 3 when the search directory
                        // itself is absent. Keep AH=59h's retained error in sync:
                        // VB3 Dir uses that AX to recognize an empty search.
                        let code = self.find_first_failure_error(&pattern);
                        self.set_extended_error(code);
                        cpu.regs.set_ax(code);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Find next file
            0x4F => {
                // Read search ID from DTA reserved bytes (offset 0x00-0x03)
                let dta_addr = self.dta_linear;
                let search_id = memory.read_u32(dta_addr);
                let (pattern, attr_filter) = self
                    .find_searches
                    .iter()
                    .find(|(k, _)| *k == search_id)
                    .map(|(_, state)| {
                        if state.dir_path.is_empty() {
                            (state.pattern.to_string(), state.attr_filter)
                        } else {
                            (
                                format!("{}\\{}", state.dir_path, state.pattern),
                                state.attr_filter,
                            )
                        }
                    })
                    .unwrap_or_else(|| (String::from("<unknown>"), 0));

                // Advance to next entry
                self.find_advance(search_id);

                if let Some(entry) = self.find_get_entry(search_id) {
                    trace_dos_find("next", &pattern, attr_filter, Some(&entry), true);
                    self.write_dta_entry(memory, search_id, &entry);
                    self.clear_extended_error();
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    trace_dos_find("next", &pattern, attr_filter, None, false);
                    self.set_extended_error(18); // No more files
                    cpu.regs.set_ax(18);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Set interrupt vector
            0x25 => {
                let vector = cpu.regs.al();
                if self.int21_effective_protected_mode(cpu) {
                    // DPMI spec: INT 21h/25h in PM translates to set PM interrupt vector.
                    // DS is a PM selector, DX/EDX is the handler offset.
                    let selector = cpu.regs.ds;
                    let offset = if self.dos_api_32bit(cpu) {
                        cpu.regs.edx
                    } else {
                        cpu.regs.dx() as u32
                    };
                    // Store as a client handler (separate from pm_idt which holds
                    // DOS extender thunks). This ensures we chain to the application's
                    // handler, not the DOS extender's interrupt trampoline.
                    self.dpmi
                        .set_client_pm_int_handler(vector, selector, offset);
                    tracing::debug!(
                        vector = vector,
                        selector = format_args!("{:04X}", selector),
                        offset = format_args!("{:08X}", offset),
                        "INT 21h/25h: Set PM interrupt handler (DPMI translation)"
                    );
                } else {
                    let ivt_addr = (vector as usize) * 4;
                    memory.write_u16(ivt_addr, cpu.regs.dx());
                    memory.write_u16(ivt_addr + 2, cpu.regs.ds);
                }
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Set Current PSP (undocumented, used by TSRs)
            0x50 => {
                // BX = new PSP segment (a PSP selector from a DPMI client)
                let psp = if self.dpmi_client_int21(cpu) {
                    self.dpmi.selector_to_conventional_segment(cpu.regs.bx())
                } else {
                    Some(cpu.regs.bx())
                };
                if let Some(psp) = psp {
                    self.set_current_psp(psp);
                }
                // No return value, no carry flag change
            }

            // Get Current PSP (undocumented, same as 0x62)
            0x51 => {
                let psp = self.current_psp_for_caller(cpu, memory);
                cpu.regs.set_bx(psp);
            }

            // Get List of Lists (undocumented)
            0x52 => {
                // Returns ES:BX pointing to the DOS List of Lists structure.
                // Match the DOS model by refreshing the block-device count at
                // LoL+0x20 each time the LoL is requested.
                let block_devices = (self.drives.len() as u8).min(DOS_DRIVES as u8);
                let lol_base = Memory::seg_offset_to_linear(LOL_SEGMENT, LOL_OFFSET);
                memory.write_u8(lol_base + 0x20, block_devices);

                cpu.return_data_seg_for_hle(
                    Some(memory),
                    gp64_cpu::DataSegReg::ES,
                    LOL_SEGMENT,
                    false,
                );
                cpu.regs.set_bx(LOL_OFFSET);
            }

            // Translate BPB to DPB (DOS 2.0+)
            0x53 => {
                // DS:SI = pointer to BPB (BIOS Parameter Block)
                // ES:BP = pointer to buffer for DPB (Drive Parameter Block)
                //
                // BPB Structure:
                //   00h: WORD  Bytes per sector
                //   02h: BYTE  Sectors per cluster
                //   03h: WORD  Reserved sectors
                //   05h: BYTE  Number of FATs
                //   06h: WORD  Root directory entries
                //   08h: WORD  Total sectors (16-bit)
                //   0Ah: BYTE  Media descriptor
                //   0Bh: WORD  Sectors per FAT
                //
                // DPB Structure (output):
                //   00h: BYTE  Drive number
                //   01h: BYTE  Unit number
                //   02h: WORD  Bytes per sector
                //   04h: BYTE  Sectors per cluster - 1
                //   05h: BYTE  Cluster shift count
                //   06h: WORD  Reserved sectors
                //   08h: BYTE  Number of FATs
                //   09h: WORD  Root directory entries
                //   0Bh: WORD  First data sector
                //   0Dh: WORD  Highest cluster + 1
                //   0Fh: BYTE  Sectors per FAT (DOS 3.x)
                //   10h: WORD  Sectors per FAT (DOS 4.0+)
                //   12h: WORD  First root directory sector
                //   14h: DWORD Device driver header (set to 0)
                //   18h: BYTE  Media descriptor
                //   19h: BYTE  Access flag (0)
                //   1Ah: DWORD Pointer to next DPB (set to FFFFFFFFh)

                let bpb_addr = self.dos_translate_ds_si(cpu);
                let dpb_addr = cpu.translate_address(cpu.regs.es, cpu.regs.bp() as u32);

                // Read BPB
                let bytes_per_sector = memory.read_u16(bpb_addr);
                let sectors_per_cluster = memory.read_u8(bpb_addr + 2);
                let reserved_sectors = memory.read_u16(bpb_addr + 3);
                let num_fats = memory.read_u8(bpb_addr + 5);
                let root_entries = memory.read_u16(bpb_addr + 6);
                let total_sectors = memory.read_u16(bpb_addr + 8);
                let media_descriptor = memory.read_u8(bpb_addr + 10);
                let sectors_per_fat = memory.read_u16(bpb_addr + 11);

                // Define malformed-input handling as DOS invalid data
                // (AX=13, CF set) without touching the DPB.
                // Calculate and validate every output field before any write;
                // the input BPB may overlap the destination DPB.
                let geometry = (|| {
                    if bytes_per_sector == 0 || !sectors_per_cluster.is_power_of_two() {
                        return None;
                    }
                    let root_dir_sectors = u32::from(root_entries)
                        .checked_mul(32)?
                        .div_ceil(u32::from(bytes_per_sector));
                    let first_root_sector = u32::from(reserved_sectors).checked_add(
                        u32::from(num_fats).checked_mul(u32::from(sectors_per_fat))?,
                    )?;
                    let first_data_sector = first_root_sector.checked_add(root_dir_sectors)?;
                    let data_sectors = u32::from(total_sectors).checked_sub(first_data_sector)?;
                    let total_clusters =
                        data_sectors.checked_div(u32::from(sectors_per_cluster))?;
                    if total_clusters == 0 {
                        return None;
                    }
                    Some((
                        u16::try_from(first_root_sector).ok()?,
                        u16::try_from(first_data_sector).ok()?,
                        u16::try_from(total_clusters.checked_add(1)?).ok()?,
                    ))
                })();
                let Some((first_root_sector, first_data_sector, highest_cluster)) = geometry else {
                    self.set_error_return(cpu, 13);
                    return Ok(Int21Disposition::Iret);
                };
                let cluster_shift = sectors_per_cluster.trailing_zeros() as u8;

                // Write DPB
                memory.write_u8(dpb_addr, 0); // 00h: Drive number (0 = default)
                memory.write_u8(dpb_addr + 1, 0); // 01h: Unit number
                memory.write_u16(dpb_addr + 2, bytes_per_sector); // 02h: Bytes per sector
                memory.write_u8(dpb_addr + 4, sectors_per_cluster - 1); // 04h: Sectors/cluster - 1
                memory.write_u8(dpb_addr + 5, cluster_shift); // 05h: Cluster shift
                memory.write_u16(dpb_addr + 6, reserved_sectors); // 06h: Reserved sectors
                memory.write_u8(dpb_addr + 8, num_fats); // 08h: Number of FATs
                memory.write_u16(dpb_addr + 9, root_entries); // 09h: Root entries
                memory.write_u16(dpb_addr + 11, first_data_sector); // 0Bh: First data sector
                memory.write_u16(dpb_addr + 13, highest_cluster); // 0Dh: Highest cluster + 1
                memory.write_u8(dpb_addr + 15, sectors_per_fat as u8); // 0Fh: Sectors per FAT (DOS 3.x)
                memory.write_u16(dpb_addr + 16, sectors_per_fat); // 10h: Sectors per FAT (DOS 4.0+)
                memory.write_u16(dpb_addr + 18, first_root_sector); // 12h: First root sector
                memory.write_u32(dpb_addr + 20, 0); // 14h: Device driver (null)
                memory.write_u8(dpb_addr + 22, media_descriptor); // 16h: Media descriptor
                memory.write_u8(dpb_addr + 23, 0); // 17h: Access flag
                memory.write_u32(dpb_addr + 24, 0xFFFF_FFFF); // 18h: Next DPB (none)
            }

            // Get verify flag (DOS 2.0+)
            0x54 => {
                // Returns AL = verify flag (0 = off, 1 = on)
                cpu.regs.set_al(self.verify_flag as u8);
            }

            // Create child PSP (DOS 3.0+)
            0x55 => {
                // DX = segment for new PSP
                // SI = size of new program in paragraphs
                // Creates a PSP at the specified segment, copying from current PSP
                let new_seg = cpu.regs.dx();
                let _size = cpu.regs.si();

                // Copy current PSP to new segment
                let current_psp_addr = Memory::seg_offset_to_linear(self.current_psp, 0);
                let new_psp_addr = Memory::seg_offset_to_linear(new_seg, 0);

                // Copy the 256-byte PSP
                for i in 0..256 {
                    let byte = memory.read_u8(current_psp_addr + i);
                    memory.write_u8(new_psp_addr + i, byte);
                }

                // Update parent PSP pointer in new PSP
                memory.write_u16(new_psp_addr + PSP_PARENT_PSP, self.current_psp);

                // No return value, just success
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get PSP address
            0x62 => {
                let psp = self.current_psp_for_caller(cpu, memory);
                cpu.regs.set_bx(psp);
            }

            // Get DBCS lead byte table pointer (DOS 3.3+)
            0x63 => {
                // Returns DS:SI pointing to DBCS lead byte table
                // For US/Western systems, return empty table (just two 0 bytes = end marker)
                let table_addr = Memory::seg_offset_to_linear(METADATA_SEG, DBCS_TABLE_OFFSET);
                memory.write_u16(table_addr, 0);
                self.return_metadata_segment(cpu, memory);
                cpu.regs.set_si(DBCS_TABLE_OFFSET);
                cpu.regs.set_al(0x00); // Success
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Get/Set Lookahead Character (undocumented, DOS 3.3+)
            0x64 => {
                // AL = 00h: Get lookahead character
                // Returns: AL = character (0 if none available)
                //
                // This is an undocumented function for keyboard lookahead.
                // We always return 0 (no character available) since we don't
                // buffer keyboard input ahead of time.
                cpu.regs.set_al(0x00); // No character available
            }

            // Get extended country information (DOS 3.3+)
            //
            // CX on entry = caller's buffer size. Real DOS never writes more
            // bytes than CX specifies. Programs commonly pass CX=7 to fetch
            // only the 7-byte header (info ID + size word + country + codepage).
            0x65 => {
                let al = cpu.regs.al();
                let buffer_addr = self.dos_translate_es_di(cpu);
                let buffer_size = cpu.regs.cx() as usize;

                match al {
                    0x01 => {
                        // Get extended country info (up to 41 bytes)
                        let full = self.country_info.extended_to_bytes();
                        let copy_len = buffer_size.min(full.len());
                        for i in 0..copy_len {
                            memory.write_u8(buffer_addr + i, full[i]);
                        }
                        cpu.regs.set_cx(copy_len as u16);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    0x02 | 0x04 | 0x05 | 0x06 | 0x07 => {
                        // Table pointer subfunctions (5 bytes each):
                        //   02 = uppercase, 04 = filename uppercase,
                        //   05 = filename char, 06 = collating, 07 = DBCS lead byte
                        let data: [u8; 5] = [al, 0, 0, 0, 0];
                        let copy_len = buffer_size.min(5);
                        for i in 0..copy_len {
                            memory.write_u8(buffer_addr + i, data[i]);
                        }
                        cpu.regs.set_cx(copy_len as u16);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        // Unknown subfunction
                        cpu.regs.set_ax(1); // Invalid function
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Get/Set global code page (DOS 3.3+)
            0x66 => {
                let al = cpu.regs.al();
                match al {
                    0x01 => {
                        // Get global code page
                        cpu.regs.set_bx(self.country_info.code_page);
                        cpu.regs.set_dx(self.country_info.code_page);
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    0x02 => {
                        // Set global code page
                        // We ignore the set but report success
                        let _requested_page = cpu.regs.bx();
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        cpu.regs.set_ax(1); // Invalid function
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Get Extended Error Information (DOS 3.0+)
            0x59 => {
                // BX = 0000h for DOS 3.0+ info version
                // Returns:
                //   AX = extended error code (same as would be in AX after error)
                //   BH = error class
                //   BL = suggested action
                //   CH = locus (where error occurred)
                //   CL, DI, SI, BP, DS, ES may be destroyed
                let (error_code, error_class, suggested_action, locus) = self.extended_error;
                tracing::debug!(
                    target: "gp64::dos::files",
                    error = error_code,
                    class = error_class,
                    action = suggested_action,
                    locus,
                    "DOS extended error response"
                );
                cpu.regs.set_ax(error_code);
                cpu.regs.set_bh(error_class);
                cpu.regs.set_bl(suggested_action);
                cpu.regs.set_ch(locus);
                cpu.regs.set_cl(0); // Reserved
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            // Create temp file
            0x5A => {
                // DS:DX points to directory path ending with backslash (e.g., "C:\TEMP\")
                // CX = file attributes
                // On success: AX = handle, DS:DX buffer contains full path with generated filename
                // On error: CF=1, AX = error code

                let buffer_addr = self.dos_translate_ds_dx(cpu);

                // Read the directory path (find the null terminator)
                let mut path_len = 0usize;
                while memory.read_u8(buffer_addr + path_len) != 0 && path_len < 128 {
                    path_len += 1;
                }

                // Read the path bytes
                let mut path_bytes = Vec::with_capacity(path_len);
                for i in 0..path_len {
                    path_bytes.push(memory.read_u8(buffer_addr + i));
                }
                let dir_path = String::from_utf8_lossy(&path_bytes).to_string();

                // MS-DOS 5.0 inserts a missing trailing backslash before the
                // generated name to form a path usable by later DOS calls.
                let separator = if !dir_path.is_empty()
                    && !dir_path.ends_with('\\')
                    && !dir_path.ends_with('/')
                {
                    "\\"
                } else {
                    ""
                };
                // All generated names have the same length; reject overflow
                // before probing the namespace or touching a drive.
                let full_path_len = dir_path.len() + separator.len() + "~DOS0000.TMP".len();
                // Check buffer overflow (DOS path max is 128 bytes)
                if full_path_len >= 128 {
                    cpu.regs.set_ax(3); // Path not found
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                }

                // Allocation only consumes SFT/JFT slots, so this preflight
                // guarantees a handle before exclusive creation has a side effect.
                if !self.file_table.has_open_capacity() {
                    cpu.regs.set_ax(4); // Too many open files
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                }
                let mut created = None;
                for _ in 0..=u16::MAX {
                    self.temp_file_counter = self.temp_file_counter.wrapping_add(1);
                    let temp_name = format!("~DOS{:04X}.TMP", self.temp_file_counter & 0xFFFF);
                    let full_path = format!("{dir_path}{separator}{temp_name}");
                    let resolved = self.path_resolver.resolve(&full_path);
                    if self.has_virtual_file(resolved.drive, &resolved.path) {
                        continue;
                    }
                    let file_res = if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                        drive.create_new_file(&resolved.path)
                    } else {
                        Err(anyhow::anyhow!("Drive not found"))
                    };
                    match file_res {
                        Ok(file) => {
                            created = Some((file, full_path, resolved));
                            break;
                        }
                        Err(error)
                            if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                                error.kind() == std::io::ErrorKind::AlreadyExists
                            }) =>
                        {
                            continue
                        }
                        Err(_) => {
                            cpu.regs.set_ax(5); // Access denied
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                        }
                    }
                }
                let Some((file, full_path, resolved)) = created else {
                    cpu.regs.set_ax(80); // File exists: no name available
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                };
                let file_identity = self.resolved_file_identity(resolved.drive, &resolved.path);
                let full_dos_path =
                    format!("{}:{}", (resolved.drive + b'A') as char, resolved.path);
                if let Some(handle) = self.file_table.allocate_with_sharing_identity(
                    file,
                    full_dos_path,
                    file_identity,
                    self.current_psp,
                    crate::files::SHARE_COMPAT,
                    crate::files::ACCESS_READWRITE,
                ) {
                    // Write the full path (with generated filename) back to the buffer
                    for (i, &b) in full_path.as_bytes().iter().enumerate() {
                        memory.write_u8(buffer_addr + i, b);
                    }
                    memory.write_u8(buffer_addr + full_path.len(), 0); // Null terminate

                    cpu.regs.set_ax(handle);
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    cpu.regs.set_ax(4); // Too many open files
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Create new file (fail if exists)
            0x5B => {
                // DS:DX = ASCIIZ filename
                // CX = file attributes
                // Returns: AX = handle or error, CF on error
                let filename = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));

                // Devices resolve before paths, as in AH=3Ch. A character
                // device has no directory entry, so the "file exists" arm
                // below can never apply to one — it simply opens.
                if let Some(device) = crate::filesystem::parse_device_name(&filename) {
                    let result = self.open_named_device(
                        device,
                        crate::files::SHARE_COMPAT,
                        crate::files::ACCESS_READWRITE,
                    );
                    self.set_device_open_return(cpu, result);
                    return Ok(Int21Disposition::Iret);
                }

                tracing::trace!(
                    target: "gp64::dos::files",
                    function = "5B",
                    path = %filename,
                    attributes = format_args!("{:04X}", cpu.regs.cx()),
                    "DOS create request"
                );
                let resolved = self.path_resolver.resolve(&filename);
                let is_hdd_5b = resolved.drive >= 2;
                let file_identity = self.resolved_file_identity(resolved.drive, &resolved.path);
                // MDD-BUG-KIL-01019: read before the drive is borrowed, and consulted only
                // on the arm that actually creates — so a full handle table
                // still reports "file exists" (80) for a name that does, rather
                // than shadowing it with error 4.
                let has_open_capacity = self.file_table.has_open_capacity();
                let mut created = false;

                if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                    // Check if file already exists by trying to get attributes
                    let file_exists = drive.get_attributes(&resolved.path).is_ok();

                    if file_exists {
                        cpu.regs.set_ax(80); // File exists
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    } else if !has_open_capacity {
                        cpu.regs.set_ax(4); // Too many open files
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    } else {
                        // Create new file (read/write, create, don't truncate)
                        match drive.open_file(&resolved.path, true, true, true) {
                            Ok(f) => {
                                let full_path = format!(
                                    "{}:{}",
                                    (resolved.drive + b'A') as char,
                                    resolved.path
                                );
                                if let Some(handle) =
                                    self.file_table.allocate_with_sharing_identity(
                                        f,
                                        full_path,
                                        file_identity,
                                        self.current_psp,
                                        crate::files::SHARE_COMPAT,
                                        crate::files::ACCESS_READWRITE,
                                    )
                                {
                                    cpu.regs.set_ax(handle);
                                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                    created = true;
                                    if is_hdd_5b {
                                        self.emit_hdd_clicks(2);
                                    }
                                } else {
                                    cpu.regs.set_ax(4); // Too many open files
                                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                }
                            }
                            Err(_) => {
                                cpu.regs.set_ax(5); // Access denied
                                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            }
                        }
                    }
                } else {
                    cpu.regs.set_ax(3); // Path not found
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
                if created {
                    self.apply_create_attributes(resolved.drive, &resolved.path, cpu.regs.cx());
                }
            }

            // Lock/Unlock File Region (DOS 3.0+)
            0x5C => {
                // AL = function: 0 = lock, 1 = unlock
                // BX = file handle
                // CX:DX = offset of region to lock/unlock
                // SI:DI = length of region to lock/unlock
                // Returns: CF set on error, AX = error code
                let handle = self.resolve_alias_handle(cpu.regs.bx());
                let function = cpu.regs.al();
                let offset = ((cpu.regs.cx() as u32) << 16) | (cpu.regs.dx() as u32);
                let length = ((cpu.regs.si() as u32) << 16) | (cpu.regs.di() as u32);

                if handle <= 4 {
                    // Standard handles - locking is a no-op but always succeeds
                    self.clear_extended_error();
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    match function {
                        0 => {
                            // Lock region
                            match self.file_table.lock_region(handle, offset, length) {
                                Ok(()) => {
                                    self.clear_extended_error();
                                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                }
                                Err(err_code) => {
                                    self.set_extended_error(err_code);
                                    cpu.regs.set_ax(err_code);
                                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                }
                            }
                        }
                        1 => {
                            // Unlock region
                            match self.file_table.unlock_region(handle, offset, length) {
                                Ok(()) => {
                                    self.clear_extended_error();
                                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                }
                                Err(err_code) => {
                                    self.set_extended_error(err_code);
                                    cpu.regs.set_ax(err_code);
                                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                }
                            }
                        }
                        _ => {
                            self.set_extended_error(1); // Invalid function
                            cpu.regs.set_ax(1);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                }
                let carry = cpu.regs.flags.contains(gp64_cpu::Flags::CARRY);
                tracing::debug!(
                    target: "gp64::dos::files",
                    handle,
                    function,
                    offset,
                    length,
                    carry,
                    error = if carry { i32::from(cpu.regs.ax()) } else { -1 },
                    "DOS file region lock response"
                );
            }

            // Rename file (DOS 2.0+)
            0x56 => {
                // DS:DX = ASCIIZ old filename
                // ES:DI = ASCIIZ new filename
                // Returns: CF clear on success, CF set on error (AX = error code)
                let old_name = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                let new_name = self.read_asciiz(cpu, memory, cpu.regs.es, self.dos_offset_di(cpu));
                tracing::trace!(
                    target: "gp64::dos::files",
                    old_path = %old_name,
                    new_path = %new_name,
                    "DOS rename request"
                );

                let old_resolved = self.path_resolver.resolve(&old_name);
                let new_resolved = self.path_resolver.resolve(&new_name);

                // Both files must be on the same drive
                if old_resolved.drive != new_resolved.drive {
                    self.set_extended_error(17); // Not same device
                    cpu.regs.set_ax(17);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else if let Some(drive) = self.drives.get_mut(&old_resolved.drive) {
                    match drive.rename(&old_resolved.path, &new_resolved.path) {
                        Ok(()) => {
                            self.clear_extended_error();
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                        Err(_) => {
                            self.set_extended_error(5); // Access denied
                            cpu.regs.set_ax(5);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                } else {
                    self.set_extended_error(3); // Path not found
                    cpu.regs.set_ax(3);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Get/Set file date and time (DOS 2.0+)
            0x57 => {
                // AL = 0: Get file date/time
                // AL = 1: Set file date/time
                // BX = file handle
                // CX = time (for set) / Returns time (for get)
                // DX = date (for set) / Returns date (for get)
                let handle = self.resolve_alias_handle(cpu.regs.bx());
                let subfunc = cpu.regs.al();

                if let Some(file_handle) = self.file_table.get_sft_mut(handle) {
                    // Get the file path so we can query attributes via the drive
                    let file_path = file_handle.path.clone();
                    let _ = file_handle; // Release borrow

                    match subfunc {
                        0 => {
                            // Get file date/time using drive's get_attributes
                            let resolved = self.path_resolver.resolve(&file_path);
                            if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                                match drive.get_attributes(&resolved.path) {
                                    Ok(attrs) => {
                                        let (date, time) =
                                            Self::systemtime_to_dos_datetime(attrs.modified);
                                        cpu.regs.set_cx(time);
                                        cpu.regs.set_dx(date);
                                        self.clear_extended_error();
                                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                    }
                                    Err(_) => {
                                        // Can't get attributes, return epoch
                                        cpu.regs.set_cx(0); // Time: 00:00:00
                                        cpu.regs.set_dx(0x0021); // Date: 1980-01-01
                                        self.clear_extended_error();
                                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                    }
                                }
                            } else {
                                // Drive not found, return epoch
                                cpu.regs.set_cx(0);
                                cpu.regs.set_dx(0x0021);
                                self.clear_extended_error();
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                        }
                        1 => {
                            // Set file date/time
                            let time = cpu.regs.cx();
                            let date = cpu.regs.dx();
                            let modified = Self::dos_datetime_to_systemtime(date, time);

                            let resolved = self.path_resolver.resolve(&file_path);
                            if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                                match drive.set_file_time(&resolved.path, modified) {
                                    Ok(()) => {
                                        self.clear_extended_error();
                                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                    }
                                    Err(_) => {
                                        // Setting time failed, but we'll still report success
                                        // (matches behavior of some DOS versions on read-only files)
                                        self.clear_extended_error();
                                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                    }
                                }
                            } else {
                                // Drive not found, report success anyway
                                self.clear_extended_error();
                                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            }
                        }
                        _ => {
                            self.set_extended_error(1); // Invalid function
                            cpu.regs.set_ax(1);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                } else {
                    // Standard handles - no date/time
                    self.set_extended_error(6); // Invalid handle
                    cpu.regs.set_ax(6);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Server Function Call (DOS 3.1+)
            0x5D => {
                // AL = subfunction:
                //   00h: Server function call (not commonly used)
                //   01h: Commit all files (flush all buffered writes)
                //   06h: Get open file list entry (not commonly used)
                //   0Ah: Set extended error
                //
                // Returns: varies by subfunction

                let subfunction = cpu.regs.al();
                match subfunction {
                    0x01 => {
                        // Commit all files - flush all buffered writes to disk
                        // We don't have buffered writes in our implementation, so just return success
                        self.clear_extended_error();
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    0x0A => {
                        // Set extended error
                        // DS:DX -> error info structure:
                        //   Offset 00h: WORD error code (to be placed in AX)
                        //   Offset 02h: WORD error class (BH) / suggested action (BL)
                        //   Offset 04h: WORD locus (CH)
                        let buf_addr = self.dos_translate_ds_dx(cpu);
                        let error_code = memory.read_u16(buf_addr);

                        // Set the extended error
                        self.set_extended_error(error_code);

                        // No return values, just set the internal error state
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    }
                    _ => {
                        // Unknown/unsupported subfunction
                        self.set_extended_error(1); // Invalid function
                        cpu.regs.set_ax(1);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            // Network - Get machine name / printer setup
            0x5E => {
                self.network.handle_5e(cpu, memory);
            }

            // Network - Assign-list / Redirector
            0x5F => {
                self.network.handle_5f(cpu, memory);
            }

            // Truename - Canonicalize path
            0x60 => {
                // DS:SI = source path (ASCIIZ)
                // ES:DI = destination buffer (128 bytes)
                // Returns: CF clear on success, CF set on error (AX = error code)
                let dest_addr = self.dos_translate_es_di(cpu);
                let input_path =
                    self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_si(cpu));

                // Resolve to canonical form
                let resolved = self.path_resolver.resolve(&input_path);

                // Build canonical DOS path: "D:\PATH\FILE.EXT"
                let canonical = format!(
                    "{}:\\{}",
                    (resolved.drive + b'A') as char,
                    resolved.path.trim_start_matches('\\')
                );

                // Convert to uppercase (DOS convention)
                let canonical_upper = canonical.to_ascii_uppercase();

                // Check buffer size (max 128 bytes including null)
                if canonical_upper.len() >= 128 {
                    cpu.regs.set_ax(3); // Path not found
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else {
                    // Write canonical path to destination
                    for (i, b) in canonical_upper.bytes().enumerate() {
                        memory.write_u8(dest_addr + i, b);
                    }
                    memory.write_u8(dest_addr + canonical_upper.len(), 0); // Null terminate

                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                }
            }

            // Get/Set Country-Dependent Information (DOS 2.0+)
            0x38 => {
                // Dispatch: DX=FFFFh → SET, DX≠FFFFh → GET
                // GET: AL=0 current country; AL=1-FEh specific code; AL=FFh code in BX
                //      DS:DX = pointer to 34-byte buffer
                // SET: DX=FFFFh; AL=1-FEh country code; AL=FFh code in BX
                // Returns: CF clear on success, BX = country code
                //          CF set on error, AX = 2 (country not found)
                let al = cpu.regs.al();
                let dx = cpu.regs.dx();

                if dx == 0xFFFF {
                    // ── SET country ──
                    let requested_code = if al == 0xFF { cpu.regs.bx() } else { al as u16 };

                    if let Some(info) = country::country_from_code(requested_code) {
                        self.country_info = info;
                        cpu.regs.set_bx(requested_code);
                        self.clear_extended_error();
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    } else {
                        cpu.regs.set_ax(2); // "File not found" reused for unknown country
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                } else {
                    // ── GET country info ──
                    let buffer_addr = self.dos_translate_ds_dx(cpu);

                    if al == 0 {
                        // Get current country
                        self.country_info.write_to_buffer(memory, buffer_addr);
                        cpu.regs.set_bx(self.country_info.country_code);
                        self.clear_extended_error();
                        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                    } else {
                        // Get specific country (AL=1-FEh: code in AL; AL=FFh: code in BX)
                        let requested_code = if al == 0xFF { cpu.regs.bx() } else { al as u16 };

                        if let Some(info) = country::country_from_code(requested_code) {
                            info.write_to_buffer(memory, buffer_addr);
                            cpu.regs.set_bx(info.country_code);
                            self.clear_extended_error();
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ax(2);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                }
            }

            // Set Handle Count (DOS 3.3+)
            0x67 => {
                // BX = new number of handles (max 255 for DOS 3.3+, up to 65535 for DOS 4+)
                // Returns: CF clear on success, CF set on error (AX = error code)
                let new_count = cpu.regs.bx();

                // DOS 3.3+ caps the JFT at 255 entries. Grow the per-process
                // handle limit (never shrinking, floor 20); the guest can then
                // open `handle_limit - 5` files. A no-op grow is what DOS does
                // when the requested count is at or below the current size.
                if new_count <= 255 {
                    self.file_table.set_handle_count(new_count);
                    self.clear_extended_error();
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    // Too many handles requested
                    self.set_extended_error(8); // Insufficient memory
                    cpu.regs.set_ax(8);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Commit File (DOS 3.3+)
            0x68 => {
                // BX = file handle
                // Returns: CF clear on success, CF set on error (AX = error code)
                // Flushes all buffers for the file to disk
                let handle = self.resolve_alias_handle(cpu.regs.bx());

                if let Some(file_handle) = self.file_table.get_sft_mut(handle) {
                    // Flush file buffers
                    match file_handle.file.sync_all() {
                        Ok(()) => {
                            self.clear_extended_error();
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                        Err(_) => {
                            self.set_extended_error(5); // Access denied
                            cpu.regs.set_ax(5);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                } else if handle <= 4 {
                    // Standard handles - nothing to flush
                    self.clear_extended_error();
                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                } else {
                    self.set_extended_error(6); // Invalid handle
                    cpu.regs.set_ax(6);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                }
            }

            // Get/Set Disk Serial Number (DOS 4.0+)
            0x69 => {
                // AL = 00h: Get disk serial number
                // AL = 01h: Set disk serial number
                // BL = drive number (0 = default, 1 = A:, 2 = B:, 3 = C:, etc.)
                // DS:DX = pointer to disk info buffer:
                //   Offset 00h: WORD  info level (0)
                //   Offset 02h: DWORD disk serial number
                //   Offset 06h: 11 BYTES volume label (space-padded)
                //   Offset 11h: 8 BYTES file system type ("FAT12   ", "FAT16   ", etc.)
                //
                // Returns:
                //   CF clear on success
                //   CF set on error, AX = error code (15 = invalid drive)

                let al = cpu.regs.al();
                let drive_num = if cpu.regs.bl() == 0 {
                    self.path_resolver.get_current_drive()
                } else {
                    cpu.regs.bl() - 1
                };

                if !self.drives.contains_key(&drive_num) {
                    // Invalid drive
                    self.set_extended_error(15);
                    cpu.regs.set_ax(15);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                } else {
                    let buf_addr = self.dos_translate_ds_dx(cpu);

                    match al {
                        0x00 => {
                            // Get disk serial number
                            memory.write_u16(buf_addr, 0); // Info level
                            memory.write_u32(buf_addr + 2, 0x12345678); // Serial number
                                                                        // Volume label (11 bytes, space-padded)
                            let label = b"NO NAME    ";
                            for (i, &b) in label.iter().enumerate() {
                                memory.write_u8(buf_addr + 6 + i, b);
                            }
                            // File system type (8 bytes)
                            let fs_type = b"FAT16   ";
                            for (i, &b) in fs_type.iter().enumerate() {
                                memory.write_u8(buf_addr + 17 + i, b);
                            }
                            self.clear_extended_error();
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                        0x01 => {
                            // Set disk serial number - just accept but ignore
                            // (we don't actually persist the serial number)
                            self.clear_extended_error();
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        }
                        _ => {
                            self.set_extended_error(1); // Invalid function
                            cpu.regs.set_ax(1);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                }
            }

            // Extended Open/Create (DOS 4.0+)
            0x6C => {
                // BL = open mode (like AL in 3Dh)
                //   bits 0-2: access mode (0=read, 1=write, 2=read/write)
                //   bit 3: reserved (must be 0)
                //   bits 4-6: sharing mode
                //   bit 7: inheritance flag
                // BH = flags
                //   bit 6: auto-commit on each write
                //   bit 5: return error rather than INT 24h
                // CX = create attributes (if file created)
                // DL = action:
                //   low nibble: 0=fail, 1=open, 2=replace if file exists
                //   high nibble: 0=fail, 1=create if file doesn't exist
                // DH = 00h (reserved)
                // DS:SI = ASCIIZ filename
                //
                // Returns:
                //   CF clear on success:
                //     AX = handle
                //     CX = action taken:
                //       1 = file opened
                //       2 = file created/truncated
                //       3 = file opened and truncated
                //   CF set on error: AX = error code

                let open_mode = cpu.regs.bl();
                let _flags = cpu.regs.bh();
                let create_attr = cpu.regs.cx();
                let action = cpu.regs.dl();
                let filename = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_si(cpu));
                trace_dos_path("OPENEX", cpu.regs.ds, self.dos_offset_si(cpu), &filename);
                tracing::trace!(
                    target: "gp64::dos::files",
                    function = "6C",
                    path = %filename,
                    attributes = format_args!("{:04X}", cpu.regs.cx()),
                    open_mode = format_args!("{:04X}", cpu.regs.bx()),
                    action = format_args!("{:04X}", cpu.regs.dx()),
                    "DOS create request"
                );

                let open_if_exists = (action & 0x01) != 0;
                let create_if_not_exists = (action & 0x10) != 0;
                let truncate_if_exists = (action & 0x02) != 0;
                let access_mode = open_mode & 0x07;
                let sft_access_6c = open_mode & 0x07;
                let share_mode_6c = open_mode & 0x70;

                // Devices resolve before paths, as in AH=3Ch. The reserved
                // access modes are rejected first so a device open does not
                // quietly accept a mode a file open refuses. A device has no
                // directory entry, so neither the exists nor the create arm
                // below applies: it is always "opened" (CX=1), never created.
                if let Some(device) = crate::filesystem::parse_device_name(&filename) {
                    if access_mode > 2 {
                        self.set_extended_error(12); // Invalid access code
                        cpu.regs.set_ax(12);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                    }
                    let result = self.open_named_device(device, share_mode_6c, sft_access_6c);
                    self.set_device_open_return(cpu, result);
                    if result.is_ok() {
                        cpu.regs.set_cx(1); // Action taken: opened
                    }
                    return Ok(Int21Disposition::Iret);
                }

                let resolved = self.path_resolver.resolve(&filename);

                // Check if file exists
                let file_exists = if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                    drive.get_attributes(&resolved.path).is_ok()
                } else {
                    // Keep AH=59h consistent with this call's result rather than
                    // leaving a stale extended error, as the LFN twin already
                    // does. MDD-BUG-CRUCIBLE-00063.
                    self.set_extended_error(3); // Path not found (no such drive)
                    cpu.regs.set_ax(3);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_open_ex(
                        0x6C,
                        &filename,
                        Some(resolved.drive),
                        Some(&resolved.path),
                        access_mode,
                        share_mode_6c,
                        None,
                        None,
                        Some(3),
                        action,
                        None,
                        caller_cs,
                        caller_ip,
                    );
                    return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                };

                let (_should_open, should_create, should_truncate) = if file_exists {
                    // MDD-BUG-FLU-02877: replacement (action 2) is an
                    // independent action, not a flag requiring open (action 1).
                    if truncate_if_exists {
                        (true, false, true)
                    } else if open_if_exists {
                        (true, false, false)
                    } else {
                        // File exists but neither open nor replace was requested
                        self.set_extended_error(80); // File exists. CRUCIBLE-00063.
                        cpu.regs.set_ax(80);
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        trace_dos_resource_open_ex(
                            0x6C,
                            &filename,
                            Some(resolved.drive),
                            Some(&resolved.path),
                            access_mode,
                            share_mode_6c,
                            None,
                            None,
                            Some(80),
                            action,
                            None,
                            caller_cs,
                            caller_ip,
                        );
                        return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                    }
                } else if create_if_not_exists {
                    (false, true, false)
                } else {
                    // File doesn't exist and create not requested: 2 when the
                    // file alone is absent, 3 when its directory is. the DOS model
                    // reaches the same split by delegating to DOS_OpenFile,
                    // which tests PathExists before choosing the code.
                    let code = self.missing_dos_path_error(resolved.drive, &resolved.path);
                    self.set_extended_error(code); // CRUCIBLE-00063.
                    cpu.regs.set_ax(code);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_open_ex(
                        0x6C,
                        &filename,
                        Some(resolved.drive),
                        Some(&resolved.path),
                        access_mode,
                        share_mode_6c,
                        None,
                        None,
                        Some(code),
                        action,
                        None,
                        caller_cs,
                        caller_ip,
                    );
                    return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                };

                // Modes 3-7 are reserved/invalid
                if access_mode > 2 {
                    self.set_extended_error(12); // Invalid access code
                    cpu.regs.set_ax(12);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_open_ex(
                        0x6C,
                        &filename,
                        Some(resolved.drive),
                        Some(&resolved.path),
                        access_mode,
                        share_mode_6c,
                        None,
                        None,
                        Some(12),
                        action,
                        None,
                        caller_cs,
                        caller_ip,
                    );
                    return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                }
                let write_access = access_mode >= 1;
                let full_path = format!("{}:{}", (resolved.drive + b'A') as char, resolved.path);
                let file_identity = self.resolved_file_identity(resolved.drive, &resolved.path);
                let entry = self.vshare.then(|| cpu.regs.clone());
                if self.reject_pre_open_sharing_violation(
                    cpu,
                    &file_identity,
                    share_mode_6c,
                    sft_access_6c,
                ) {
                    if let Some(critical) = entry.as_ref().and_then(|entry| {
                        self.vshare_open_critical(
                            memory,
                            entry,
                            0x6C,
                            resolved.drive,
                            share_mode_6c,
                        )
                    }) {
                        return Ok(critical);
                    }
                    trace_dos_resource_open_ex(
                        0x6C,
                        &filename,
                        Some(resolved.drive),
                        Some(&resolved.path),
                        access_mode,
                        share_mode_6c,
                        Some(write_access),
                        None,
                        Some(32),
                        action,
                        None,
                        caller_cs,
                        caller_ip,
                    );
                    return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                }

                // MDD-BUG-KIL-01019: only the create and truncate actions change the
                // file, so only they need the handle established first. A plain
                // open that cannot be given a handle still fails afterwards,
                // having changed nothing.
                if (should_create || should_truncate) && !self.file_table.has_open_capacity() {
                    self.set_extended_error(4);
                    cpu.regs.set_ax(4); // Too many open files
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    trace_dos_resource_open_ex(
                        0x6C,
                        &filename,
                        Some(resolved.drive),
                        Some(&resolved.path),
                        access_mode,
                        share_mode_6c,
                        Some(write_access),
                        None,
                        Some(4),
                        action,
                        None,
                        caller_cs,
                        caller_ip,
                    );
                    return Ok(Self::iret_with_carry(cpu, memory, self.skip_iret_sync));
                }

                let is_hdd_6c = resolved.drive >= 2;
                let drive = self
                    .drives
                    .get_mut(&resolved.drive)
                    .expect("drive missing after file exists check");
                let file_res = if should_create {
                    drive.open_file(&resolved.path, true, true, true)
                } else if should_truncate {
                    drive.open_file(&resolved.path, write_access, false, true)
                } else {
                    drive.open_file(&resolved.path, write_access, false, false)
                };

                match file_res {
                    Ok(f) => {
                        if let Some(handle) = self.file_table.allocate_with_sharing_identity(
                            f,
                            full_path,
                            file_identity,
                            self.current_psp,
                            share_mode_6c,
                            sft_access_6c,
                        ) {
                            cpu.regs.set_ax(handle);
                            if should_create || should_truncate {
                                // CX is an input attribute word and an output action
                                // code. Apply the saved input after opening, so the
                                // creating handle remains writable for read-only files.
                                self.apply_create_attributes(
                                    resolved.drive,
                                    &resolved.path,
                                    create_attr,
                                );
                            }
                            // Set CX to action taken
                            let action_taken = if should_create {
                                2 // File created
                            } else if should_truncate {
                                3 // File opened and truncated
                            } else {
                                1 // File opened
                            };
                            cpu.regs.set_cx(action_taken);
                            self.clear_extended_error();
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                            if is_hdd_6c {
                                self.emit_hdd_clicks(2);
                            }
                            trace_dos_resource_open_ex(
                                0x6C,
                                &filename,
                                Some(resolved.drive),
                                Some(&resolved.path),
                                access_mode,
                                share_mode_6c,
                                Some(write_access),
                                Some(handle),
                                None,
                                action,
                                Some(action_taken),
                                caller_cs,
                                caller_ip,
                            );
                        } else {
                            self.set_extended_error(4);
                            cpu.regs.set_ax(4); // Too many open files
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                            trace_dos_resource_open_ex(
                                0x6C,
                                &filename,
                                Some(resolved.drive),
                                Some(&resolved.path),
                                access_mode,
                                share_mode_6c,
                                Some(write_access),
                                None,
                                Some(4),
                                action,
                                None,
                                caller_cs,
                                caller_ip,
                            );
                        }
                    }
                    Err(_) => {
                        self.set_extended_error(5);
                        cpu.regs.set_ax(5); // Access denied
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        trace_dos_resource_open_ex(
                            0x6C,
                            &filename,
                            Some(resolved.drive),
                            Some(&resolved.path),
                            access_mode,
                            share_mode_6c,
                            Some(write_access),
                            None,
                            Some(5),
                            action,
                            None,
                            caller_cs,
                            caller_ip,
                        );
                    }
                }
            }

            // Long Filename (LFN) functions (DOS 7.0+)
            // INT 21h AH=71h - LFN functions
            // These are DOS long-filename extensions
            0x71 => {
                let al = cpu.regs.al();
                match al {
                    // 710Dh - LFN Reset Drive
                    0x0D => {
                        self.lfn_reset_drive(cpu);
                    }

                    // 7139h - LFN Create Directory
                    0x39 => {
                        let path =
                            self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                        trace_dos_path("LFN_MKDIR", cpu.regs.ds, self.dos_offset_dx(cpu), &path);

                        let resolved = self.path_resolver.resolve_lfn(&path);
                        if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                            match drive.create_dir(&resolved.path) {
                                Ok(()) => {
                                    cpu.regs.set_ax(0);
                                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                }
                                Err(_) => {
                                    cpu.regs.set_ax(5); // Access denied
                                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                }
                            }
                        } else {
                            cpu.regs.set_ax(3); // Path not found
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }

                    // 713Ah - LFN Remove Directory
                    0x3A => {
                        let path =
                            self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                        trace_dos_path("LFN_RMDIR", cpu.regs.ds, self.dos_offset_dx(cpu), &path);

                        let resolved = self.path_resolver.resolve_lfn(&path);
                        if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                            match drive.remove_dir(&resolved.path) {
                                Ok(()) => {
                                    cpu.regs.set_ax(0);
                                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                }
                                Err(_) => {
                                    cpu.regs.set_ax(5); // Access denied
                                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                }
                            }
                        } else {
                            cpu.regs.set_ax(3); // Path not found
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }

                    // 713Bh - LFN Change Directory
                    0x3B => {
                        let path =
                            self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                        trace_dos_path("LFN_CHDIR", cpu.regs.ds, self.dos_offset_dx(cpu), &path);

                        // MDD-BUG-CRUCIBLE-00592: this used to accept any
                        // mounted drive without checking that the target
                        // existed or was a directory. It now shares the
                        // short-name AH=3Bh validation; LFN resolution and
                        // the long spelling are preserved.
                        let resolved = self.path_resolver.resolve_lfn(&path);
                        if self.try_change_directory(resolved.drive, &resolved.path, &path, true) {
                            cpu.regs.set_ax(0);
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ax(3); // Path not found
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }

                    // 7141h - LFN Delete File
                    0x41 => {
                        self.lfn_delete_file(cpu, memory);
                    }

                    // 7143h - LFN Get/Set File Attributes
                    0x43 => {
                        self.lfn_attributes(cpu, memory);
                    }

                    // 7147h - LFN Get Current Directory
                    0x47 => {
                        let dl = cpu.regs.dl();
                        let drive_num = if dl == 0 {
                            self.path_resolver.get_current_drive()
                        } else {
                            dl - 1
                        };

                        if self.drives.contains_key(&drive_num) {
                            let dir = self.path_resolver.get_current_dir(drive_num);
                            // Remove leading backslash if present
                            let dir = dir.strip_prefix('\\').unwrap_or(&dir);
                            // Write to DS:SI buffer
                            let addr = self.dos_translate_ds_si(cpu);
                            for (i, ch) in dir.bytes().enumerate() {
                                memory.write_u8(addr + i, ch);
                            }
                            memory.write_u8(addr + dir.len(), 0); // Null terminate
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            cpu.regs.set_ax(0x0F); // Invalid drive
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }

                    // 7156h - LFN Rename File
                    0x56 => {
                        let src_path =
                            self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                        let dst_path =
                            self.read_asciiz(cpu, memory, cpu.regs.es, self.dos_offset_di(cpu));
                        trace_dos_path(
                            "LFN_RENAME_SRC",
                            cpu.regs.ds,
                            self.dos_offset_dx(cpu),
                            &src_path,
                        );
                        trace_dos_path(
                            "LFN_RENAME_DST",
                            cpu.regs.es,
                            self.dos_offset_di(cpu),
                            &dst_path,
                        );

                        let src_resolved = self.path_resolver.resolve_lfn(&src_path);
                        let dst_resolved = self.path_resolver.resolve_lfn(&dst_path);

                        if src_resolved.drive != dst_resolved.drive {
                            cpu.regs.set_ax(17); // Not same device
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        } else if let Some(drive) = self.drives.get_mut(&src_resolved.drive) {
                            match drive.rename(&src_resolved.path, &dst_resolved.path) {
                                Ok(()) => {
                                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                }
                                Err(_) => {
                                    cpu.regs.set_ax(5); // Access denied
                                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                }
                            }
                        } else {
                            cpu.regs.set_ax(3); // Path not found
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }

                    // 7160h - LFN Canonicalize Path / Get Short Path
                    0x60 => {
                        self.lfn_true_name(cpu, memory);
                    }

                    // 716Ch - LFN Extended Open/Create
                    0x6C => {
                        self.lfn_open(cpu, memory);
                    }

                    // 714Eh - LFN Find First
                    0x4E => {
                        if cpu.regs.si() > 1 {
                            self.set_extended_error(1);
                            cpu.regs.set_ax(1);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        } else {
                            let pattern =
                                self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
                            trace_dos_path(
                                "LFN_FIND_FIRST",
                                cpu.regs.ds,
                                self.dos_offset_dx(cpu),
                                &pattern,
                            );
                            let attr_filter = cpu.regs.cx();
                            match self.lfn_find_first(&pattern, attr_filter) {
                                Ok((handle, entry)) => {
                                    let address = self.dos_translate_es_di(cpu);
                                    let conversion_flags = self.write_lfn_find_data(
                                        memory,
                                        address,
                                        &entry,
                                        cpu.regs.si(),
                                    );
                                    cpu.regs.set_ax(handle);
                                    cpu.regs.set_cx(conversion_flags);
                                    self.clear_extended_error();
                                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                }
                                Err(code) => {
                                    self.set_extended_error(code);
                                    cpu.regs.set_ax(code);
                                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                }
                            }
                        }
                    }

                    // 714Fh - LFN Find Next
                    0x4F => {
                        if cpu.regs.si() > 1 {
                            self.set_extended_error(1);
                            cpu.regs.set_ax(1);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        } else {
                            let handle = cpu.regs.bx();
                            match self.lfn_find_next(handle) {
                                Ok(entry) => {
                                    let address = self.dos_translate_es_di(cpu);
                                    let conversion_flags = self.write_lfn_find_data(
                                        memory,
                                        address,
                                        &entry,
                                        cpu.regs.si(),
                                    );
                                    // The DOS long-filename API returns AH=4Fh and the low byte of
                                    // the file-find handle on a successful next.
                                    cpu.regs.set_ax(0x4F00 | (handle & 0x00FF));
                                    cpu.regs.set_cx(conversion_flags);
                                    self.clear_extended_error();
                                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                }
                                Err(code) => {
                                    self.set_extended_error(code);
                                    cpu.regs.set_ax(code);
                                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                }
                            }
                        }
                    }

                    // 71A2h - Internal FindNext alias. Uses
                    // the same search state and FindData record as 714Fh, but
                    // preserves AX=71A2h on success.
                    0xA2 => {
                        if cpu.regs.si() > 1 {
                            self.set_extended_error(1);
                            cpu.regs.set_ax(1);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        } else {
                            let handle = cpu.regs.bx();
                            match self.lfn_find_next(handle) {
                                Ok(entry) => {
                                    let address = self.dos_translate_es_di(cpu);
                                    let conversion_flags = self.write_lfn_find_data(
                                        memory,
                                        address,
                                        &entry,
                                        cpu.regs.si(),
                                    );
                                    cpu.regs.set_ax(0x71A2);
                                    cpu.regs.set_cx(conversion_flags);
                                    self.clear_extended_error();
                                    cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                                }
                                Err(code) => {
                                    self.set_extended_error(code);
                                    cpu.regs.set_ax(code);
                                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                                }
                            }
                        }
                    }

                    // 71A0h - LFN Get Volume Information
                    0xA0 => {
                        self.lfn_volume_info(cpu, memory);
                    }

                    // 71A1h - LFN Find Close
                    0xA1 => {
                        // BX = search handle
                        if self.lfn_find_close(cpu.regs.bx()) {
                            cpu.regs.set_ax(0);
                            self.clear_extended_error();
                            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
                        } else {
                            self.set_extended_error(6);
                            cpu.regs.set_ax(6);
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }

                    // 71A6h - LFN Get File Info By Handle
                    0xA6 => {
                        self.lfn_file_info(cpu, memory);
                    }

                    // 71A7h - LFN FILETIME/DOS date-time conversion
                    0xA7 => {
                        self.lfn_time_conversion(cpu, memory);
                    }

                    // 71A8h - LFN Generate Short Filename
                    0xA8 => {
                        self.lfn_generate_short_name(cpu, memory);
                    }

                    // 71A9h - LFN Server Create/Open File
                    0xA9 => {
                        self.lfn_open(cpu, memory);
                    }

                    // 71AAh - LFN SUBST operations
                    0xAA => {
                        self.lfn_subst(cpu, memory);
                    }

                    _ => {
                        // Unsupported LFN subfunction
                        tracing::warn!(al = format_args!("{:02X}", al), "Unsupported LFN function");
                        cpu.regs.set_ax(0x7100); // Function not supported
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                }
            }

            0xDC => {
                // INT 21h/DCh — Microsoft internal (Get/Set AZUSA_MEM).
                // Return AX=0 to report this optional service as unavailable.
                tracing::debug!("INT 21h/DCh: AZUSA_MEM stub, returning AX=0");
                cpu.regs.set_ax(0);
                cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            }

            0xFF => {
                // INT 21h/FFh — DOS extender identification check .
                // Extenders call this to detect DOS/32A presence. CF=1 means
                // "not DOS/32A," which is the correct response for a vanilla DOS
                // environment. Logged at debug because this fires on every startup
                tracing::debug!("INT 21h/FFh: DOS extender ID check, returning CF=1 (not DOS/32A)");
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }

            _ => {
                tracing::warn!(ah = format_args!("{:02X}", ah), "Unhandled INT 21h");
                // Return CF=1 (error) for unhandled functions so DOS extenders
                // don't mistake silence for success.
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            }
        }

        if let Some(snapshot) = &boot_trace {
            Self::trace_dos_boot_return(snapshot, cpu, &disposition);
        }

        if trace_enabled {
            let cf = if cpu.regs.flags.contains(gp64_cpu::Flags::CARRY) {
                1
            } else {
                0
            };
            let pre_regs = pre_regs.expect("pre_regs must be Some when trace_enabled");
            tracing::debug!(
                caller_cs = format_args!("{:04X}", caller_cs),
                caller_ip = format_args!("{:04X}", caller_ip),
                ah = format_args!("{:02X}", ah),
                cf,
                ax = format_args!("{:04X}", cpu.regs.ax()),
                bx = format_args!("{:04X}", cpu.regs.bx()),
                cx = format_args!("{:04X}", cpu.regs.cx()),
                dx = format_args!("{:04X}", cpu.regs.dx()),
                ds = format_args!("{:04X}", cpu.regs.ds),
                es = format_args!("{:04X}", cpu.regs.es),
                pre_ax = format_args!("{:04X}", pre_regs.ax()),
                pre_bx = format_args!("{:04X}", pre_regs.bx()),
                pre_cx = format_args!("{:04X}", pre_regs.cx()),
                pre_dx = format_args!("{:04X}", pre_regs.dx()),
                "INT 21h return"
            );
        }

        Ok(disposition)
    }

    /// Handle INT 2Fh - Multiplex Interrupt
    pub fn int2f(&mut self, cpu: &mut Cpu, memory: &mut Memory, win16_active: bool) -> Result<()> {
        let ax = cpu.regs.ax();
        let ah = (ax >> 8) as u8;

        match ah {
            // DOS sharing driver functions (AH=10h)
            0x10 => {
                // Installation check (AL=0x00)
                // AL=0 means not installed, AL=FFh means installed
                // Return 0 (not installed) since we don't fully support sharing
            }

            // Network Redirector (AH=11h)
            0x11 => {
                // Not installed - leave AX unchanged
            }

            // DOS Internal Services (AH=12h)
            0x12 => {
                match ax & 0xFF {
                    // Get length of ASCIIZ string
                    0x21 => {
                        let addr = Memory::seg_offset_to_linear(cpu.regs.ds, cpu.regs.si());
                        let mut len = 0u16;
                        while memory.read_u8(addr + len as usize) != 0 && len < 0x8000 {
                            len += 1;
                        }
                        cpu.regs.set_cx(len);
                    }
                    // Get current PSP
                    0x51 => {
                        cpu.regs.set_bx(self.current_psp);
                    }
                    _ => {}
                }
            }

            // Windows Enhanced Mode API (AH=16h)
            0x16 => {
                match ax & 0xFF {
                    // Installation check
                    0x00 => {
                        if win16_active {
                            // Windows 3.10 Enhanced Mode running
                            cpu.regs.set_al(0x03); // major version
                            cpu.regs.set_ah(0x0A); // minor version
                        } else {
                            // AL=00h: No Windows running
                            cpu.regs.set_al(0x00);
                        }
                    }
                    // Windows enhanced mode init broadcast
                    0x05 => {
                        // Return CX=0 to indicate no failure
                        cpu.regs.set_cx(0);
                    }
                    // Windows enhanced mode exit broadcast
                    0x06 => {
                        // No action needed
                    }
                    // Windows NT version check (AX=160Ah)
                    0x0A => {
                        // If NT is NOT running, leave AX unchanged (stays 160Ah).
                        // Programs detect "no NT" by seeing AX unmodified.
                    }
                    // Windows 3.1 release timeslice
                    0x80 => {
                        // No action needed for non-multitasking
                    }
                    // Get VxD API Entry Point (AL=84h)
                    0x84 => {
                        // Return ES:DI = 0000:0000 — "VxD not present"
                        // No VxD dispatch is currently implemented; well-behaved
                        // apps check for null and fall back gracefully.
                        cpu.return_data_seg_for_hle(
                            Some(memory),
                            gp64_cpu::DataSegReg::ES,
                            0,
                            false,
                        );
                        cpu.regs.set_di(0);
                    }
                    // Get Vendor-Specific API Entry Point (AL=8Ah).
                    // Microsoft "MS-DOS API Extensions for DPMI Hosts" lookup,
                    // running under our DPMI host. Per spec the call is
                    // protected-mode only — leave AX unchanged in real mode
                    // (the spec failure signal: AL stays 0x8A).
                    //
                    // See `wrk_docs/2026.05.06 - HLD - Phar Lap RTM extender
                    0x8A if cpu.is_protected_mode() && !self.simulated_real_mode_hle => {
                        // Lookup recognises vendor "MS-DOS"; on no-match it
                        // leaves AX alone (current default behaviour preserved).
                        let _ = self.dpmi.int2f_msdos_ext_lookup(cpu, memory);
                    }
                    _ => {}
                }
            }

            // XMS API (AH=43h) - handled by XMS module via int2f_check
            0x43 => {
                // Handled by xms.int2f_check() in lib.rs
            }

            // DOSKEY (AH=48h)
            0x48 => {
                match ax & 0xFF {
                    // Installation check
                    0x00 => {
                        // AL=0 = not installed
                        cpu.regs.set_al(0);
                    }
                    // Read command line
                    0x10 => {
                        // Not implemented - set carry to indicate no data
                        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    }
                    _ => {}
                }
            }

            // HMA Management (AH=4Ah)
            0x4A => {
                match ax & 0xFF {
                    // Query free HMA space
                    0x01 => {
                        // BX = free HMA space in bytes
                        // Return 0 = no HMA available (unless XMS provides it)
                        cpu.regs.set_bx(0);
                    }
                    // Allocate HMA space
                    0x02 => {
                        // BX = bytes requested
                        // Return ES:DI = 0:0 (allocation failed)
                        cpu.return_data_seg_for_hle(
                            Some(memory),
                            gp64_cpu::DataSegReg::ES,
                            0,
                            false,
                        );
                        cpu.regs.set_di(0);
                    }
                    _ => {}
                }
            }

            // DISPLAY.SYS (AH=ADh)
            0xAD => {
                // Installation check (AL=0x00)
                // AL=FFh = installed, AL=00h = not installed
                cpu.regs.set_al(0x00); // Not installed
            }

            // APPEND (AH=B7h)
            0xB7 => {
                match ax & 0xFF {
                    // Installation check
                    0x00 => {
                        // AL=00h = not installed
                        // AL=FFh = installed
                        cpu.regs.set_al(0x00); // Not installed
                    }
                    // Get APPEND path
                    0x02 => {
                        // ES:DI = pointer to path
                        // Return null pointer (not installed)
                        cpu.return_data_seg_for_hle(
                            Some(memory),
                            gp64_cpu::DataSegReg::ES,
                            0,
                            false,
                        );
                        cpu.regs.set_di(0);
                    }
                    // Get APPEND state
                    0x06 => {
                        // BX = append state
                        cpu.regs.set_bx(0); // Disabled
                    }
                    _ => {}
                }
            }

            _ => {
                // Unhandled function - leave registers unchanged
                // Most programs check specific return values
            }
        }
        Ok(())
    }

    /// Terminate the current process.
    ///
    /// Performs all process teardown: close files, restore IVT vectors from PSP,
    /// free memory, restore parent process state, and set exit code.
    ///
    /// Used by:
    /// - INT 21h AH=4Ch (normal termination)
    /// - INT 24h Abort response
    ///
    /// Returns `NoIret` if a parent process was restored, or bails if this is
    /// the root process (no parent).
    pub fn terminate_process(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        exit_code: u8,
        termination_type: u8,
    ) -> anyhow::Result<Int21Disposition> {
        tracing::warn!(target: "gp64::exit",
            "terminate_process: exit_code={} type={} PSP={:04X} PM={}",
            exit_code, termination_type, self.current_psp, self.dpmi.in_protected_mode());

        // Store exit code and termination type for INT 21h/4Dh
        self.last_exit_code = exit_code;
        self.last_termination_type = termination_type;

        // Record the DPMI program's exit for the run loops. Only the ROOT
        // process's exit ends the program: a child returning to its parent
        // and a batch line's exit resumes the batch (MDD-BUG-NMI-00210). Use
        // was_initialized() rather than in_protected_mode() because INT 21h
        // may be called via DPMI's simulate-real-mode-interrupt (INT 31h/0300h),
        // which temporarily sets protected_mode = false.
        //
        // resident PSP) returns to the parent it went resident from, as DOS
        // does through the PSP's parent linkage, and gives back its memory.
        let terminating_psp = self.current_psp;
        // Read before the PSP's memory is released below.
        let (ret_cs, ret_ip) = Self::psp_terminate_address(memory, terminating_psp);
        let resident_return = self.take_resident_return(terminating_psp);
        let is_resident = resident_return.is_some();
        if self.process_stack.is_empty() && !is_resident && self.dpmi.was_initialized() {
            self.dpmi_program_exit_code = Some(exit_code);
        }
        // Residents whose parent is this process can no longer return to it.
        self.resident_returns
            .retain(|(_, parent)| parent.psp_segment != terminating_psp);

        if let Some(parent_state) = resident_return.or_else(|| self.process_stack.pop()) {
            // Close all files owned by the terminating process (flushes each on last close)
            self.close_lfn_searches_for_psp(self.current_psp);
            self.file_table.close_process_files(self.current_psp);
            // Flush host stdout/stderr so buffered output reaches the terminal
            {
                use std::io::Write;
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().flush();
            }
            // Restore INT 22h/23h/24h vectors from current PSP before freeing memory
            self.restore_psp_vectors(memory, self.current_psp);
            // Free ALL memory blocks owned by this process, not just the PSP block
            self.dos_mem.free_all_for_psp(memory, self.current_psp);
            // Going resident gave the PSP block to the system; it ends here.
            if is_resident {
                let _ = self.dos_mem.free(memory, terminating_psp);
            }
            self.set_current_psp(parent_state.psp_segment);

            // The parent EXECed from DPMI PM: its session outlives the child,
            // and it resumes in PM, completing its INT 21h 4Bh.
            if parent_state.pm_parent {
                self.resume_pm_parent(cpu, &parent_state);
                return Ok(Int21Disposition::Iret);
            }

            // If we were in DPMI protected mode, clean up and switch back to real mode.
            // The parent process was in real mode when it called EXEC.
            if self.dpmi.in_protected_mode() {
                self.dpmi.reset_with_memory(memory);
                // Switch CPU back to real mode
                cpu.regs.cr0 &= !1; // Clear PE bit
                cpu.regs.cpl = 0; // Real mode has no privilege levels
                cpu.dpmi_active = false;
                // Set segment caches to real mode style for the parent's segments
                cpu.regs.cs_cache = gp64_cpu::SegmentCache::real_mode(parent_state.exec_ret_cs);
                cpu.regs.ds_cache = gp64_cpu::SegmentCache::real_mode(parent_state.registers.ds);
                cpu.regs.es_cache = gp64_cpu::SegmentCache::real_mode(parent_state.registers.es);
                cpu.regs.ss_cache = gp64_cpu::SegmentCache::real_mode(parent_state.registers.ss);
                cpu.regs.fs_cache = gp64_cpu::SegmentCache::real_mode(0);
                cpu.regs.gs_cache = gp64_cpu::SegmentCache::real_mode(0);
                cpu.cs_bits = 16; // Real mode is 16-bit
            }

            // Restore parent's registers (full restore now that we're in real mode)
            cpu.regs = parent_state.registers.clone();
            cpu.regs.set_ax(exit_code as u16);
            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
            cpu.regs.cs = ret_cs;
            cpu.regs.cs_cache = gp64_cpu::SegmentCache::real_mode(ret_cs);
            cpu.regs.set_ip(ret_ip);
            cpu.regs.set_sp(cpu.regs.sp().wrapping_add(6));
            cpu.regs.flags = gp64_cpu::Flags::from_bits_truncate(parent_state.exec_ret_flags);
            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);

            Ok(Int21Disposition::NoIret)
        } else {
            bail!("Program terminated with exit code {exit_code}");
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // -- should_emit_hdd_sound tests --

    #[test]
    fn test_hdd_sound_hard_drive_paths() {
        assert!(should_emit_hdd_sound(r"C:\FILE.TXT"));
        assert!(should_emit_hdd_sound(r"D:\GAME\TEST.EXE"));
        assert!(should_emit_hdd_sound(r"E:\DATA"));
        assert!(should_emit_hdd_sound("c:\\lower.dat")); // lowercase drive
    }

    #[test]
    fn test_hdd_sound_floppy_excluded() {
        assert!(!should_emit_hdd_sound(r"A:\DISK1\FILE.DAT"));
        assert!(!should_emit_hdd_sound(r"B:\BOOT.COM"));
        assert!(!should_emit_hdd_sound(r"a:\lower.dat")); // lowercase floppy
    }

    #[test]
    fn test_hdd_sound_devices_excluded() {
        assert!(!should_emit_hdd_sound("NUL"));
        assert!(!should_emit_hdd_sound("CON"));
        assert!(!should_emit_hdd_sound("AUX"));
        assert!(!should_emit_hdd_sound("PRN"));
        assert!(!should_emit_hdd_sound("")); // empty string
    }

    // -- seek_distance_to_clicks tests --

    #[test]
    fn test_seek_distance_to_clicks_thresholds() {
        // < 4KB → 1
        assert_eq!(seek_distance_to_clicks(0), 1);
        assert_eq!(seek_distance_to_clicks(100), 1);
        assert_eq!(seek_distance_to_clicks(4095), 1);
        // 4KB–128KB → 2
        assert_eq!(seek_distance_to_clicks(4096), 2);
        assert_eq!(seek_distance_to_clicks(128 * 1024 - 1), 2);
        // 128KB–512KB → 3
        assert_eq!(seek_distance_to_clicks(128 * 1024), 3);
        assert_eq!(seek_distance_to_clicks(512 * 1024 - 1), 3);
        // 512KB–2MB → 4
        assert_eq!(seek_distance_to_clicks(512 * 1024), 4);
        assert_eq!(seek_distance_to_clicks(2 * 1024 * 1024 - 1), 4);
        // > 2MB → 5
        assert_eq!(seek_distance_to_clicks(2 * 1024 * 1024), 5);
        assert_eq!(seek_distance_to_clicks(100 * 1024 * 1024), 5);
    }

    // -- emit_hdd_clicks / Dos accumulator tests --

    #[test]
    fn test_emit_hdd_clicks_disabled() {
        let mut dos = Dos::new();
        dos.hdd_sound_enabled = false;
        dos.emit_hdd_clicks(5);
        assert_eq!(dos.hdd_seek_clicks, 0);
    }

    #[test]
    fn test_emit_hdd_clicks_enabled() {
        let mut dos = Dos::new();
        dos.hdd_sound_enabled = true;
        dos.emit_hdd_clicks(3);
        assert_eq!(dos.hdd_seek_clicks, 3);
        dos.emit_hdd_clicks(4);
        assert_eq!(dos.hdd_seek_clicks, 7);
    }

    #[test]
    fn test_emit_hdd_clicks_saturates() {
        let mut dos = Dos::new();
        dos.hdd_sound_enabled = true;
        dos.hdd_seek_clicks = 250;
        dos.emit_hdd_clicks(10);
        assert_eq!(dos.hdd_seek_clicks, 255); // saturated, not wrapped
    }

    // -- Sequential read accumulator tests --

    #[test]
    fn test_sequential_read_accumulator_emits_at_32k() {
        // Simulate what the read handler does for sequential reads
        let mut seq_read_bytes: u32 = 0;
        let mut clicks: u8 = 0;

        // Read 16KB — no click yet
        seq_read_bytes += 16384;
        while seq_read_bytes >= 32768 {
            clicks = clicks.saturating_add(1);
            seq_read_bytes -= 32768;
        }
        assert_eq!(clicks, 0);
        assert_eq!(seq_read_bytes, 16384);

        // Read another 16KB — crosses 32KB boundary, emit 1 click
        seq_read_bytes += 16384;
        while seq_read_bytes >= 32768 {
            clicks = clicks.saturating_add(1);
            seq_read_bytes -= 32768;
        }
        assert_eq!(clicks, 1);
        assert_eq!(seq_read_bytes, 0);
    }

    #[test]
    fn test_sequential_read_large_single_read() {
        // A single 100KB read should emit 3 clicks (3 * 32KB = 96KB)
        let mut seq_read_bytes: u32 = 0;
        let mut clicks: u8 = 0;

        seq_read_bytes += 100 * 1024;
        while seq_read_bytes >= 32768 {
            clicks = clicks.saturating_add(1);
            seq_read_bytes -= 32768;
        }
        assert_eq!(clicks, 3); // 100KB / 32KB = 3.125, so 3 clicks
        assert_eq!(seq_read_bytes, 100 * 1024 - 3 * 32768); // 4096 remainder
    }

    // -- Ah06LineBuffer tests (HLD 2026.05.23) --

    /// `tracing::fmt::MakeWriter` adapter that appends to a shared `Vec<u8>`.
    /// Standard idiom for capturing tracing output in unit tests; see
    /// `tracing-subscriber` docs for `MakeWriter`.
    #[derive(Clone)]
    struct SharedBufWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBufWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Install a tracing subscriber that captures `gp64::dos::stdout`
    /// events into the returned buffer, run `body`, then return the captured
    /// bytes. The subscriber is scoped to the closure via
    /// `tracing::subscriber::with_default`, so concurrent tests don't trip
    /// over each other's global subscriber.
    fn capture_ah06_trace(body: impl FnOnce()) -> String {
        use tracing_subscriber::layer::SubscriberExt;
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer_buf = captured.clone();
        let make_writer = move || SharedBufWriter(writer_buf.clone());
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(make_writer)
                .with_target(true)
                .with_ansi(false)
                .without_time(),
        );
        tracing::subscriber::with_default(subscriber, body);
        let bytes = captured.lock().unwrap().clone();
        String::from_utf8(bytes).expect("tracing output must be UTF-8")
    }

    #[test]
    fn ah06_line_buffer_emits_on_newline() {
        let out = capture_ah06_trace(|| {
            let mut buf = Ah06LineBuffer::default();
            // 'A' (0x41), 'B', '\n' — should emit one line containing "AB".
            buf.push(b'A');
            buf.push(b'B');
            buf.push(b'\n');
        });

        assert!(
            out.contains("gp64::dos::stdout"),
            "wrong target in captured trace: {out}"
        );
        assert!(
            out.contains("DOS AH=06 output"),
            "missing event message: {out}"
        );
        // Apply-feedback: tighten to the field-boundary form so a regression
        // that pushes a spurious extra byte (e.g. "ABC") doesn't pass.
        // `tracing-subscriber`'s `fmt::layer` separates fields with a space.
        assert!(
            out.contains("text=AB ") || out.contains("text=AB\n"),
            "missing `text=AB` at field boundary in: {out}"
        );
        assert!(
            out.contains("newline=true"),
            "expected `newline=true` for `\\n`-terminated line: {out}"
        );
    }

    #[test]
    fn ah06_line_buffer_drops_bare_cr_in_crlf() {
        // DOS conventionally writes "\r\n" — the buffer should drop the bare
        // '\r' and emit exactly one line on the trailing '\n'.
        let out = capture_ah06_trace(|| {
            let mut buf = Ah06LineBuffer::default();
            buf.push(b'A');
            buf.push(b'\r');
            buf.push(b'\n');
        });

        // Exactly one `DOS AH=06 output` event must appear.
        let count = out.matches("DOS AH=06 output").count();
        assert_eq!(
            count, 1,
            "expected exactly one event for \"A\\r\\n\", got {count}: {out}"
        );
        // The bare '\r' must NOT show up in the rendered text — neither as a
        // literal byte nor as the escape `\r` that `format_dos_bytes` emits
        // for embedded CRs.
        assert!(out.contains("text=A"), "missing `text=A` in: {out}");
        assert!(
            !out.contains("text=A\\r"),
            "bare CR leaked into rendered text: {out}"
        );
    }

    #[test]
    fn ah06_line_buffer_caps_runaway_line() {
        // 1000 bytes with no '\n'. The buffer must flush at AH06_MAX_LINE
        // (512), so the first event sees `newline=false` and there are at
        // least two events total in the captured output.
        let out = capture_ah06_trace(|| {
            let mut buf = Ah06LineBuffer::default();
            for _ in 0..1000 {
                buf.push(b'A');
            }
            // After 1000 pushes we expect exactly one flush at 512 bytes,
            // leaving 488 bytes still buffered.
            assert_eq!(
                buf.buf.len(),
                1000 - AH06_MAX_LINE,
                "remainder after cap-flush is wrong"
            );
        });

        // At least one event with `newline=false` (the cap-induced flush).
        assert!(
            out.contains("newline=false"),
            "expected at least one `newline=false` flush: {out}"
        );
        // And it must report `len=512` for the cap-induced flush.
        assert!(
            out.contains("len=512"),
            "expected the cap-induced flush to report len=512: {out}"
        );
    }
}
