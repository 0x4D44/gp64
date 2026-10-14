//! Process management, exec, and TSR operations for INT 21h

use super::*;
use crate::{dump_env_progname, dump_env_strings, dump_psp_bytes};
use std::io::Read;

/// Does this `VirtualDrive::open_file` error represent an access-denied condition
/// (an existing but unreadable EXEC target — e.g. a directory or a
/// permission-denied file)? Drives surface such errors as anyhow-wrapped
/// `io::Error`s. MDD-BUG-FLUXHOMEARPA-00018.
fn is_access_denied(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
}

/// An EXEC parameter block with its far pointers resolved to linear
/// addresses (RBIL INT 21h/4Bh: environment, command tail, FCB 1, FCB 2).
struct ExecParams {
    env: ExecEnv,
    cmdline: Option<usize>,
    fcb1: Option<usize>,
    fcb2: Option<usize>,
}

/// The environment an EXEC parameter block names.
enum ExecEnv {
    /// A real-mode segment; 0 inherits the caller's.
    Segment(u16),
    /// A DPMI client's block at a linear address, `bytes` long.
    Linear { base: usize, bytes: usize },
}

/// Copy the machine-wide state (control and debug registers, descriptor
/// tables, task register) from `live` into a restored process register set.
/// Returning to a parent restores that process's registers, not the machine:
/// PE stays clear because the parent resumes in real mode.
fn keep_machine_state(regs: &mut gp64_cpu::Registers, live: &gp64_cpu::Registers) {
    regs.cr0 = live.cr0 & !1;
    regs.cr2 = live.cr2;
    regs.cr3 = live.cr3;
    regs.cr4 = live.cr4;
    regs.dr0 = live.dr0;
    regs.dr1 = live.dr1;
    regs.dr2 = live.dr2;
    regs.dr3 = live.dr3;
    regs.dr6 = live.dr6;
    regs.dr7 = live.dr7;
    regs.gdtr = live.gdtr;
    regs.idtr = live.idtr;
    regs.gdt_generation = live.gdt_generation;
    regs.ldt_generation = live.ldt_generation;
    regs.tr = live.tr;
    regs.tss_base = live.tss_base;
    regs.tss_limit = live.tss_limit;
    regs.tss_ss0 = live.tss_ss0;
    regs.tss_esp0 = live.tss_esp0;
    regs.ldtr = live.ldtr;
    regs.ldt_base = live.ldt_base;
    regs.ldt_limit = live.ldt_limit;
}

/// Maximum bytes read from a guest-named executable or overlay before rejecting
/// it as too large. A guest can point EXEC/overlay load at an arbitrarily large
/// (or virtual) file; reading it whole would exhaust host memory before DOS
/// memory limits reject the load (MDD-BUG-FLUXHOMEARPA-00009). 64 MiB is far
/// above any real DOS executable/overlay (including DOS-extender payloads) yet
/// bounds the host allocation.
const MAX_DOS_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Bytes MS-DOS reads before checking an executable's MZ signature. This is
/// the operational header through `e_lfarlc`; shorter nonempty files are COM
/// programs even when their first two bytes happen to be `MZ`.
const DOS_EXEC_HEADER_LEN: usize = 26;

/// A COM image starts at PSP:0100 and must end no later than offset FFFF.
const MAX_COM_IMAGE_BYTES: usize = 0xFF00;

/// Maximum environment bytes inspected when the segment is not the start of a
/// DOS-owned MCB. This keeps malformed or foreign environment pointers finite.
const ENV_FALLBACK_SCAN_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvironmentParseError {
    InvalidSegment,
    MissingTerminator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnvironmentLayout {
    base: usize,
    strings_end: usize,
    scan_limit: usize,
}

fn is_mz_executable(data: &[u8]) -> bool {
    data.len() >= DOS_EXEC_HEADER_LEN && data.starts_with(b"MZ")
}

/// End offset of the MZ-declared load image — the last byte a DOS loader maps.
///
/// `e_cp` (@0x04, 512-byte pages including the last) and `e_cblp` (@0x02, bytes
/// used in the last page) declare how much of the file is image. Anything past
/// that is appended overlay, symbol-table or debug data which DOS never loads;
/// `unpacker::mz_image_end_bytes` documents the same contract on the unpacker
/// side.
///
/// `e_cp == 0` means the fields are unset, so the whole file is taken as image
/// — the historical behaviour, kept so files that omit them still load. The
/// result is clamped to `[header_size, data.len()]`.
///
/// Centralized for MDD-BUG-CRUCIBLE-00594: normal AL=00 EXEC computed this
/// inline (via closed MDD-00052) while load-only and overlay loading each
/// sliced the entire physical tail, so they sized allocations from — and copied
/// into guest memory — bytes a DOS loader must leave unloaded.
///
/// Caller must have validated `data.len() >= DOS_EXEC_HEADER_LEN`.
fn mz_declared_image_end(data: &[u8], header_size: usize) -> usize {
    let bytes_in_last_page = u16::from_le_bytes([data[2], data[3]]) as usize;
    let pages_in_file = u16::from_le_bytes([data[4], data[5]]) as usize;
    let declared = if pages_in_file == 0 {
        data.len()
    } else {
        let final_page_bytes = if bytes_in_last_page == 0 {
            512
        } else {
            bytes_in_last_page
        };
        pages_in_file.saturating_sub(1) * 512 + final_page_bytes
    };
    declared.clamp(header_size, data.len())
}

/// Bytes of memory DOS reserves for an MZ image: `pages*512 - headersize`
/// (the DOS model `dos_execute.cpp:361`, MS-DOS EXEC).
///
/// DOS rounds the image *allocation* up to the 512-byte page boundary — the
/// last-page byte count (`e_cblp`) bounds only how much is *copied*
/// (`mz_declared_image_end`), never how many paragraphs are reserved. Sizing
/// the allocation from the exact declared file length under-allocates by up to
/// 511 bytes, and a program that checks the PSP top-of-memory word against a
/// header-derived requirement fails when it should pass (MDD-BUG-KILN-00639).
///
/// `e_cp == 0` keeps the whole-file legacy fallback, mirroring
/// `mz_declared_image_end`. The result is deliberately NOT clamped to
/// `data.len()`: DOS allocates from the header even when the file's last page
/// is physically short (LZEXE 0.91 tails). A bogus huge page count overflows
/// `exec_total_paragraphs`' u16 range and is rejected before any mutation.
///
/// Caller must have validated `data.len() >= DOS_EXEC_HEADER_LEN`.
fn mz_alloc_image_bytes(data: &[u8], header_size: usize) -> usize {
    let pages_in_file = u16::from_le_bytes([data[4], data[5]]) as usize;
    if pages_in_file == 0 {
        data.len().saturating_sub(header_size)
    } else {
        (pages_in_file * 512).saturating_sub(header_size)
    }
}

/// Convert the complete DOS allocation size to paragraphs without truncation.
///
/// The allocator accepts `u16` paragraphs, so a payload whose PSP, image, and
/// minimum extra memory exceed that range is not representable and must be
/// rejected before any MCB or guest memory mutation.
fn exec_total_paragraphs(image_bytes: usize, extra_paragraphs: u16) -> Option<u16> {
    let image_paragraphs = image_bytes.div_ceil(16);
    let total = 0x10usize
        .checked_add(image_paragraphs)?
        .checked_add(extra_paragraphs as usize)?;
    u16::try_from(total).ok()
}

fn reject_exec_for_memory(cpu: &mut Cpu, largest_available: u16) {
    cpu.regs.set_ax(8);
    cpu.regs.set_bx(largest_available);
    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
}

fn reject_exec_for_invalid_environment(cpu: &mut Cpu) {
    cpu.regs.set_ax(10);
    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
}

/// Read an executable/overlay file while bounding the host allocation to
/// `MAX_DOS_IMAGE_BYTES`. Reads at most `cap + 1` bytes (streaming via
/// `Read::take`) and rejects the file if it exceeds the cap, so a huge
/// guest-named file cannot exhaust host memory before the load is validated
/// (MDD-BUG-FLUXHOMEARPA-00009).
fn read_exec_file_capped<R: Read>(reader: R) -> std::io::Result<Vec<u8>> {
    read_reader_capped(reader, MAX_DOS_IMAGE_BYTES)
}

/// `read_exec_file_capped` with an explicit cap (split out for testing). Reads at
/// most `cap + 1` bytes via `Read::take` and rejects if the reader yields more.
fn read_reader_capped<R: Read>(reader: R, cap: u64) -> std::io::Result<Vec<u8>> {
    let mut data = Vec::new();
    reader.take(cap.saturating_add(1)).read_to_end(&mut data)?;
    if data.len() as u64 > cap {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "executable or overlay exceeds the maximum loadable size",
        ));
    }
    Ok(data)
}

/// Build the terminal EXEC-search error, preserving access-denied so callers can
/// return DOS "access denied" (AX=5) instead of collapsing every failure to
/// "file not found" (AX=2). MDD-BUG-FLUXHOMEARPA-00018.
fn exec_search_error(access_denied: bool) -> std::io::Error {
    if access_denied {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied")
    } else {
        std::io::Error::new(std::io::ErrorKind::NotFound, "file not found")
    }
}

/// Format a 256-byte PSP dump as a 16-line hex string (16 bytes per line)
/// for the `gp64::dos::psp` trace target. Called only when the target
/// is enabled; the byte-walk + formatting cost is paid only at trace time.
fn format_psp_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(16 * 64);
    for (row, chunk) in bytes.chunks(16).enumerate() {
        let _ = write!(
            s,
            "\n  PSP[0x{:02X}..0x{:02X}] =",
            row * 16,
            row * 16 + chunk.len()
        );
        for &b in chunk {
            let _ = write!(s, " {:02X}", b);
        }
    }
    s
}

/// Emit a one-shot PSP + env dump for the `gp64::dos::psp` trace target.
///
/// Called from `exec_load_exe`, `exec_load_com`, and `exec_load_only`
/// immediately after all per-EXEC PSP writes are complete. The call site is
/// gated by `tracing::enabled!` so the 256-byte byte-walk + format cost is
/// zero when the target is not enabled.
fn trace_psp_dump(memory: &Memory, psp_seg: u16, env_seg: u16, kind: &str) {
    if !tracing::enabled!(target: "gp64::dos::psp", tracing::Level::TRACE) {
        return;
    }
    // Emit two separate records (PSP_DUMP + ENV_DUMP) per the HLD example
    // output, so structured fields don't get appended to the tail of the
    // PSP hex line. A grep for `PSP_DUMP at EXEC` still finds both because
    // the records are emitted back-to-back with the same target.
    let psp_bytes = dump_psp_bytes(memory, psp_seg);
    let psp_hex = format_psp_hex(&psp_bytes);
    tracing::trace!(
        target: "gp64::dos::psp",
        kind = kind,
        psp_seg = format_args!("{:04X}", psp_seg),
        "PSP_DUMP at EXEC entry to child{}",
        psp_hex,
    );

    let env_strings = dump_env_strings(memory, env_seg);
    let env_progname = dump_env_progname(memory, env_seg);
    tracing::trace!(
        target: "gp64::dos::psp",
        kind = kind,
        env_seg = format_args!("{:04X}", env_seg),
        env_strings = ?env_strings,
        env_progname = ?env_progname,
        "ENV_DUMP at EXEC entry to child",
    );
}

impl Dos {
    /// Save interrupt vectors 22h, 23h, 24h to PSP
    /// Called after creating child PSP during EXEC
    /// PSP layout:
    /// - Offset 0x0A: INT 22h (Terminate address)
    /// - Offset 0x0E: INT 23h (Ctrl-C handler)
    /// - Offset 0x12: INT 24h (Critical error handler)
    pub(crate) fn save_psp_vectors(&self, memory: &mut Memory, psp_seg: u16) {
        let psp_addr = Memory::seg_offset_to_linear(psp_seg, 0);

        // Read current vectors from IVT
        let int22 = memory.read_u32(0x22 * 4); // Terminate address
        let int23 = memory.read_u32(0x23 * 4); // Ctrl-C handler
        let int24 = memory.read_u32(0x24 * 4); // Critical error handler

        // Save to PSP
        memory.write_u32(psp_addr + PSP_INT22_TERMINATE, int22);
        memory.write_u32(psp_addr + PSP_INT23_CTRLC, int23);
        memory.write_u32(psp_addr + PSP_INT24_CRITICAL, int24);
    }

    /// Restore interrupt vectors 22h, 23h, 24h from PSP
    /// Called before returning to parent process during termination
    pub fn restore_psp_vectors(&self, memory: &mut Memory, psp_seg: u16) {
        let psp_addr = Memory::seg_offset_to_linear(psp_seg, 0);

        // Read vectors from PSP
        let int22 = memory.read_u32(psp_addr + PSP_INT22_TERMINATE);
        let int23 = memory.read_u32(psp_addr + PSP_INT23_CTRLC);
        let int24 = memory.read_u32(psp_addr + PSP_INT24_CRITICAL);

        // Restore to IVT
        memory.write_u32(0x22 * 4, int22);
        memory.write_u32(0x23 * 4, int23);
        memory.write_u32(0x24 * 4, int24);
    }

    /// Parse an environment string area within its allocation.
    ///
    /// DOS normally passes the usable start of an environment MCB. Foreign
    /// segments still occur in practice, so they get a finite fallback window.
    /// Both paths are clamped to usable guest memory before any byte is read.
    fn parse_environment(
        &self,
        memory: &Memory,
        env_seg: u16,
    ) -> Result<EnvironmentLayout, EnvironmentParseError> {
        if env_seg == 0 {
            return Err(EnvironmentParseError::InvalidSegment);
        }

        let base = Memory::seg_offset_to_linear(env_seg, 0);
        let allocation_bytes = self
            .dos_mem
            .block_end_segment(env_seg)
            .map(|end_seg| end_seg.saturating_sub(env_seg) as usize * 16)
            .unwrap_or(ENV_FALLBACK_SCAN_BYTES);
        Self::parse_environment_at(memory, base, allocation_bytes)
    }

    /// [`Self::parse_environment`] for a block at a linear address, `bytes`
    /// long (a DPMI client's environment, bounded by its selector's limit).
    fn parse_environment_at(
        memory: &Memory,
        base: usize,
        bytes: usize,
    ) -> Result<EnvironmentLayout, EnvironmentParseError> {
        let available = memory
            .usable_len()
            .checked_sub(base)
            .ok_or(EnvironmentParseError::InvalidSegment)?;
        if available < 2 {
            return Err(EnvironmentParseError::InvalidSegment);
        }
        let scan_limit = bytes.min(available);

        for offset in 0..scan_limit.saturating_sub(1) {
            if memory.read_u8(base + offset) == 0 && memory.read_u8(base + offset + 1) == 0 {
                return Ok(EnvironmentLayout {
                    base,
                    strings_end: offset + 2,
                    scan_limit,
                });
            }
        }

        Err(EnvironmentParseError::MissingTerminator)
    }

    fn get_path_from_environment(
        &self,
        memory: &Memory,
        env_seg: u16,
    ) -> Result<Option<String>, EnvironmentParseError> {
        if env_seg == 0 {
            return Ok(None);
        }
        let layout = self.parse_environment(memory, env_seg)?;
        let strings_limit = layout.strings_end - 1;
        let mut start = 0;

        while start < strings_limit {
            let mut end = start;
            while end < strings_limit && memory.read_u8(layout.base + end) != 0 {
                end += 1;
            }
            if end == start {
                break;
            }

            if end - start >= 5
                && (0..5).all(|i| {
                    memory
                        .read_u8(layout.base + start + i)
                        .eq_ignore_ascii_case(&b"PATH="[i])
                })
            {
                let value = (start + 5..end)
                    .map(|i| memory.read_u8(layout.base + i) as char)
                    .collect();
                return Ok(Some(value));
            }
            start = end + 1;
        }

        Ok(None)
    }

    /// Read PATH environment variable from environment segment.
    pub fn get_path_from_env(&self, memory: &Memory) -> Option<String> {
        self.get_path_from_environment(memory, self.env_segment)
            .ok()
            .flatten()
    }

    /// Try to read a file, searching current directory first, then PATH directories
    pub(crate) fn try_read_file_by_dos_name(
        &mut self,
        cpu: &Cpu,
        memory: &Memory,
        ds: u16,
        dx: u32,
    ) -> std::io::Result<(PathBuf, Vec<u8>)> {
        let dos_name = self.read_asciiz(cpu, memory, ds, dx);
        trace_dos_path("EXEC", ds, dx, &dos_name);

        // Generate candidate filenames (add .COM/.EXE if no extension). Extension
        // probing depends on the FINAL path component only, not dots in a parent
        // directory: `C:\DIR.V1\PROG` has an extensionless basename and must still
        // probe synthetic-program / synthetic-program. MDD-BUG-CRUCIBLE-00061.
        let basename = match dos_name.rfind(['\\', '/']) {
            Some(pos) => &dos_name[pos + 1..],
            None => dos_name.as_str(),
        };
        let mut base_candidates: Vec<String> = Vec::new();
        if basename.contains('.') {
            base_candidates.push(dos_name.clone());
        } else {
            base_candidates.push(format!("{dos_name}.COM"));
            base_candidates.push(format!("{dos_name}.EXE"));
            base_candidates.push(dos_name.clone());
        }

        // Check if name contains path separator or drive letter (absolute or relative path)
        let has_path = dos_name.contains('\\')
            || dos_name.contains('/')
            || (dos_name.len() >= 2 && dos_name.as_bytes()[1] == b':');

        // Track whether any candidate existed but could not be opened/read due to
        // access denial (a directory or a permission-denied file), so the terminal
        // error is "access denied" (DOS 5) instead of "file not found" (DOS 2).
        // MDD-BUG-FLUXHOMEARPA-00018.
        let mut access_denied = false;

        // First, try current directory (or specified path)
        for cand in &base_candidates {
            let resolved = self.path_resolver.resolve(cand);
            tracing::debug!(
                candidate = %cand,
                drive = resolved.drive,
                path = %resolved.path,
                drive_exists = self.drives.contains_key(&resolved.drive),
                "EXEC file search"
            );
            if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                match drive.open_file(&resolved.path, false, false, false) {
                    Ok(file) => match read_exec_file_capped(file) {
                        Ok(data) => {
                            let fake_path = PathBuf::from(format!(
                                "{}:{}",
                                (resolved.drive + b'A') as char,
                                resolved.path
                            ));
                            return Ok((fake_path, data));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                            access_denied = true;
                        }
                        Err(_) => {}
                    },
                    Err(e) if is_access_denied(&e) => access_denied = true,
                    Err(_) => {}
                }
            }
        }

        // If name already has a path, don't search PATH
        if has_path {
            return Err(exec_search_error(access_denied));
        }

        // Search PATH directories
        let path_val = self
            .get_path_from_environment(memory, self.env_segment)
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid or unterminated DOS environment",
                )
            })?;
        if let Some(path_val) = path_val {
            for dir in path_val.split(';') {
                let dir = dir.trim();
                if dir.is_empty() {
                    continue;
                }

                for cand in &base_candidates {
                    // Build full path: dir + \ + filename
                    let mut full_path = dir.to_string();
                    if !full_path.ends_with('\\') && !full_path.ends_with('/') {
                        full_path.push('\\');
                    }
                    full_path.push_str(cand);

                    let resolved = self.path_resolver.resolve(&full_path);
                    if let Some(drive) = self.drives.get_mut(&resolved.drive) {
                        match drive.open_file(&resolved.path, false, false, false) {
                            Ok(file) => match read_exec_file_capped(file) {
                                Ok(data) => {
                                    let fake_path = PathBuf::from(format!(
                                        "{}:{}",
                                        (resolved.drive + b'A') as char,
                                        resolved.path
                                    ));
                                    return Ok((fake_path, data));
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                                    access_denied = true;
                                }
                                Err(_) => {}
                            },
                            Err(e) if is_access_denied(&e) => access_denied = true,
                            Err(_) => {}
                        }
                    }
                }
            }
        }

        Err(exec_search_error(access_denied))
    }

    pub fn exec_read_param_block_cmdtail(
        &self,
        memory: &Memory,
        es: u16,
        bx: u16,
    ) -> (u16, Option<(u16, u16)>) {
        if es == 0 && bx == 0 {
            return (0, None);
        }
        let param_block_addr = Memory::seg_offset_to_linear(es, bx);
        let env_seg = memory.read_u16(param_block_addr);
        let cmdline_off = memory.read_u16(param_block_addr + 2);
        let cmdline_seg = memory.read_u16(param_block_addr + 4);
        let cmdline_ptr = if cmdline_seg == 0 && cmdline_off == 0 {
            None
        } else {
            Some((cmdline_seg, cmdline_off))
        };
        (env_seg, cmdline_ptr)
    }

    /// Copy the command-tail buffer from the parent's EXEC parameter block to
    /// the child's PSP at offset 0x80 (`PSP_COMMAND_LINE_LEN`).
    ///
    /// Parameter block format (RBIL INT 21h/AH=4Bh):
    ///   `src+0`        BYTE    cmdtail length (0..126)
    ///   `src+1..N`     BYTE    cmdtail bytes (typically starts with a leading space)
    ///   `src+1+N`      BYTE    `0x0D` (CR terminator)
    ///
    /// Destination format (PSP cmdtail per RBIL):
    ///   `PSP:0x80`     BYTE    length (0..126)
    ///   `PSP:0x81..N`  BYTE    cmdtail bytes
    ///   `PSP:0x81+N`   BYTE    `0x0D` (CR terminator)
    ///
    /// If `cmdline_ptr` is `None` (the parent did not supply a cmdtail buffer
    /// via the EXEC parameter block — for example, the parameter block's
    /// cmdtail far pointer was 0:0 by convention, or the parameter block
    /// itself was absent for an internally-driven EXEC), the child PSP
    /// receives an empty cmdtail: `[len=0][0x0D]`.
    pub fn exec_copy_cmdtail_to_psp(
        &self,
        memory: &mut Memory,
        psp_seg: u16,
        cmdline_ptr: Option<(u16, u16)>,
    ) {
        let src = cmdline_ptr.map(|(seg, off)| Memory::seg_offset_to_linear(seg, off));
        Self::copy_cmdtail_to_psp(memory, psp_seg, src);
    }

    /// [`Self::exec_copy_cmdtail_to_psp`] from a linear source: a DPMI
    /// client's EXEC hands its buffers by selector, anywhere in memory.
    fn copy_cmdtail_to_psp(memory: &mut Memory, psp_seg: u16, src: Option<usize>) {
        let dst = Memory::seg_offset_to_linear(psp_seg, PSP_COMMAND_LINE_LEN);
        if let Some(src) = src {
            let len = memory.read_u8(src).min(126);
            memory.write_u8(dst, len);
            // Copy len bytes + CR
            for i in 0..=len as usize {
                let b = memory.read_u8(src + 1 + i);
                memory.write_u8(dst + 1 + i, b);
            }
        } else {
            memory.write_u8(dst, 0);
            memory.write_u8(dst + 1, 0x0D);
        }
    }

    /// Read the FCB-1 and FCB-2 far pointers from the EXEC parameter block.
    ///
    /// Returns `(fcb1_ptr, fcb2_ptr)` where each is `Option<(seg, off)>` and
    /// `None` indicates the parent did not supply a usable FCB (the
    /// segment:offset pair was 0:0). Per RBIL INT 21h/AH=4Bh AL=00, the
    /// parameter block layout is:
    ///
    /// ```text
    /// 00h   WORD    env_seg
    /// 02h   DWORD   cmdtail far ptr (offset:segment, little-endian)
    /// 06h   DWORD   FCB-1 far ptr (offset:segment, little-endian)
    /// 0Ah   DWORD   FCB-2 far ptr (offset:segment, little-endian)
    /// ```
    ///
    /// Both FCB pointers must be either real far pointers (parent supplied a
    /// parsed FCB) or 0:0 (parent could not parse a filename argument). The
    /// `0:0` sentinel is treated as `None`; the caller leaves the child
    /// PSP's 16-byte FCB slot as the zero it was given by `create_psp`'s
    /// zero-fill. This matches the DOS model `DOS_Execute` reading from
    /// `block.exec.fcb1` / `block.exec.fcb2`.
    #[allow(clippy::type_complexity)] // pair-of-far-pointers is the spec shape
    pub fn exec_read_param_block_fcbs(
        &self,
        memory: &Memory,
        es: u16,
        bx: u16,
    ) -> (Option<(u16, u16)>, Option<(u16, u16)>) {
        if es == 0 && bx == 0 {
            return (None, None);
        }
        let pb = Memory::seg_offset_to_linear(es, bx);
        let fcb1_off = memory.read_u16(pb + 0x06);
        let fcb1_seg = memory.read_u16(pb + 0x08);
        let fcb2_off = memory.read_u16(pb + 0x0A);
        let fcb2_seg = memory.read_u16(pb + 0x0C);
        let fcb1 = if fcb1_seg == 0 && fcb1_off == 0 {
            None
        } else {
            Some((fcb1_seg, fcb1_off))
        };
        let fcb2 = if fcb2_seg == 0 && fcb2_off == 0 {
            None
        } else {
            Some((fcb2_seg, fcb2_off))
        };
        (fcb1, fcb2)
    }

    /// Copy 16 bytes (one default-FCB-format unopened FCB) from each parent
    /// FCB buffer into the child PSP at offsets 0x5C (FCB 1) and 0x6C
    /// (FCB 2) per RBIL "PSP - Program Segment Prefix".
    ///
    /// If a parent's FCB pointer is `None` (parameter block contained 0:0
    /// for that FCB), the corresponding child PSP slot is left as the zero
    /// it was given by `create_psp`'s 256-byte zero-fill. The end-state is
    /// always "PSP:0x5C..0x6B and 0x6C..0x7B are deterministic — either
    /// zero or a copy of the parent's parsed FCB" — never the BSS-init
    /// 0xCF residue that previously leaked through.
    pub fn exec_copy_fcbs_to_psp(
        &self,
        memory: &mut Memory,
        psp_seg: u16,
        fcb1_ptr: Option<(u16, u16)>,
        fcb2_ptr: Option<(u16, u16)>,
    ) {
        let linear =
            |ptr: Option<(u16, u16)>| ptr.map(|(seg, off)| Memory::seg_offset_to_linear(seg, off));
        Self::copy_fcbs_to_psp(memory, psp_seg, linear(fcb1_ptr), linear(fcb2_ptr));
    }

    /// [`Self::exec_copy_fcbs_to_psp`] from linear sources.
    fn copy_fcbs_to_psp(
        memory: &mut Memory,
        psp_seg: u16,
        fcb1: Option<usize>,
        fcb2: Option<usize>,
    ) {
        fn copy_fcb(memory: &mut Memory, psp_seg: u16, psp_off: u16, src: Option<usize>) {
            let Some(src) = src else {
                // Leave the 16 bytes at psp_off as-is (zeroed by create_psp).
                return;
            };
            let dst = Memory::seg_offset_to_linear(psp_seg, psp_off);
            for i in 0..16 {
                let b = memory.read_u8(src + i);
                memory.write_u8(dst + i, b);
            }
        }
        copy_fcb(memory, psp_seg, 0x5C, fcb1);
        copy_fcb(memory, psp_seg, 0x6C, fcb2);
    }

    pub fn exec_load_com(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        image: &[u8],
        env_seg: u16,
        cmdline_ptr: Option<usize>,
        fcb1_ptr: Option<usize>,
        fcb2_ptr: Option<usize>,
        parent_psp: u16,
    ) -> Result<()> {
        let largest = self.dos_mem.largest_free();
        let required = exec_total_paragraphs(image.len(), 0);
        if image.len() > MAX_COM_IMAGE_BYTES || required.is_none_or(|total| largest < total) {
            reject_exec_for_memory(cpu, largest);
            return Ok(());
        }

        let psp_seg = match self.dos_mem.alloc(memory, self.current_psp, largest) {
            Ok(seg) => seg,
            Err(DosMemError::InsufficientMemory { largest_available }) => {
                cpu.regs.set_ax(8);
                cpu.regs.set_bx(largest_available);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                return Ok(());
            }
            Err(_) => {
                cpu.regs.set_ax(8);
                cpu.regs.set_bx(self.dos_mem.largest_free());
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                return Ok(());
            }
        };

        let mem_end = self
            .dos_mem
            .block_end_segment(psp_seg)
            .unwrap_or(psp_seg.wrapping_add(largest));

        self.set_current_psp(psp_seg);
        let _ = self.dos_mem.set_owner(memory, psp_seg, psp_seg);

        self.create_psp(memory, psp_seg, mem_end, env_seg, Some(parent_psp), &[]);
        self.save_psp_vectors(memory, psp_seg);
        Self::copy_cmdtail_to_psp(memory, psp_seg, cmdline_ptr);
        Self::copy_fcbs_to_psp(memory, psp_seg, fcb1_ptr, fcb2_ptr);
        let psp_base = Memory::seg_offset_to_linear(psp_seg, 0);
        memory.write_u16(psp_base + PSP_PARENT_PSP, parent_psp);

        // Load COM at PSP:0100
        let load_addr = Memory::seg_offset_to_linear(psp_seg, PSP_LOAD_ADDRESS);
        for (i, &b) in image.iter().enumerate() {
            memory.write_u8(load_addr + i, b);
        }

        cpu.setup_for_com(psp_seg);
        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
        // Gremlin Step 1 hook: signal the child's image CS to the root
        // crate so the epoch detector can arm on EXEC'd programs (e.g.
        // autoexec.bat invoking the target via DOS command interpreter /C). The
        // root crate drains this in `step_inner`.
        self.last_exec_image_cs = Some(psp_seg);

        // Part 0 trace site (gp64::dos::psp). Dumps the child PSP and
        // env block after all per-EXEC PSP writes are complete. Gated by
        // tracing::enabled! so cost is zero unless target is enabled.
        trace_psp_dump(memory, psp_seg, env_seg, "exec_load_com");

        Ok(())
    }

    pub(crate) fn exec_apply_relocations(
        memory: &mut Memory,
        data: &[u8],
        reloc_offset: usize,
        reloc_count: u16,
        load_segment: u16,
    ) {
        let mut reloc_pos = reloc_offset;
        for _ in 0..reloc_count {
            if reloc_pos + 4 > data.len() {
                break;
            }
            let offset = u16::from_le_bytes([data[reloc_pos], data[reloc_pos + 1]]);
            let segment = u16::from_le_bytes([data[reloc_pos + 2], data[reloc_pos + 3]]);
            reloc_pos += 4;

            let addr = Memory::seg_offset_to_linear(load_segment.wrapping_add(segment), offset);
            let old_val = memory.read_u16(addr);
            memory.write_u16(addr, old_val.wrapping_add(load_segment));
        }
    }

    pub fn exec_load_exe(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
        exe_data: &[u8],
        env_seg: u16,
        cmdline_ptr: Option<usize>,
        fcb1_ptr: Option<usize>,
        fcb2_ptr: Option<usize>,
        parent_psp: u16,
    ) -> Result<()> {
        if !is_mz_executable(exe_data) {
            cpu.regs.set_ax(11); // bad format
            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            return Ok(());
        }

        // Detect and (optionally) unpack packed executables (EXEPACK supported).
        let packer_type = unpacker::detect_packer(exe_data);
        let data = match packer_type {
            unpacker::PackerType::EXEPACK => {
                unpacker::unpack_exepack(exe_data).unwrap_or_else(|_| exe_data.to_vec())
            }
            _ => exe_data.to_vec(),
        };

        // e_cblp (@0x02) / e_cp (@0x04) are read by `mz_declared_image_end`.
        let header_paragraphs = u16::from_le_bytes([data[8], data[9]]);
        let header_size = (header_paragraphs as usize) * 16;
        let reloc_count = u16::from_le_bytes([data[6], data[7]]);
        let reloc_offset = u16::from_le_bytes([data[24], data[25]]) as usize;
        let init_ss = u16::from_le_bytes([data[14], data[15]]);
        let init_sp = u16::from_le_bytes([data[16], data[17]]);
        let init_ip = u16::from_le_bytes([data[20], data[21]]);
        let init_cs = u16::from_le_bytes([data[22], data[23]]);
        let min_extra = u16::from_le_bytes([data[10], data[11]]);
        let max_extra = u16::from_le_bytes([data[12], data[13]]);

        if header_size > data.len() {
            cpu.regs.set_ax(11);
            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            return Ok(());
        }

        let image_end = mz_declared_image_end(&data, header_size);
        let image = &data[header_size..image_end];
        // Allocate from the header's full page count, copy only the declared
        // image (MDD-BUG-KILN-00639) — `alloc_bytes >= image.len()` always.
        let alloc_bytes = mz_alloc_image_bytes(&data, header_size);
        let largest = self.dos_mem.largest_free();
        let Some(min_total_paras) = exec_total_paragraphs(alloc_bytes, min_extra) else {
            reject_exec_for_memory(cpu, largest);
            return Ok(());
        };
        // max_extra=0xFFFF means "all available memory". Cap an otherwise
        // unrepresentable desired total after proving the minimum is legal.
        let desired_total_paras = exec_total_paragraphs(alloc_bytes, max_extra).unwrap_or(u16::MAX);
        if largest < min_total_paras {
            reject_exec_for_memory(cpu, largest);
            return Ok(());
        }

        let alloc_paras = desired_total_paras.min(largest);
        let psp_seg = match self.dos_mem.alloc(memory, self.current_psp, alloc_paras) {
            Ok(seg) => seg,
            Err(DosMemError::InsufficientMemory { largest_available }) => {
                cpu.regs.set_ax(8);
                cpu.regs.set_bx(largest_available);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                return Ok(());
            }
            Err(_) => {
                cpu.regs.set_ax(8);
                cpu.regs.set_bx(self.dos_mem.largest_free());
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                return Ok(());
            }
        };

        let mem_end = self
            .dos_mem
            .block_end_segment(psp_seg)
            .unwrap_or(psp_seg.wrapping_add(alloc_paras));
        let load_segment = psp_seg.wrapping_add(0x10);

        self.set_current_psp(psp_seg);
        let _ = self.dos_mem.set_owner(memory, psp_seg, psp_seg);

        self.create_psp(memory, psp_seg, mem_end, env_seg, Some(parent_psp), &[]);
        self.save_psp_vectors(memory, psp_seg);
        Self::copy_cmdtail_to_psp(memory, psp_seg, cmdline_ptr);
        Self::copy_fcbs_to_psp(memory, psp_seg, fcb1_ptr, fcb2_ptr);
        let psp_base = Memory::seg_offset_to_linear(psp_seg, 0);
        memory.write_u16(psp_base + PSP_PARENT_PSP, parent_psp);

        // Load image at load_segment:0000
        debug_assert!(image.len() <= (alloc_paras as usize - 0x10) * 16);
        let load_addr = Memory::seg_offset_to_linear(load_segment, 0);
        for (i, &b) in image.iter().enumerate() {
            memory.write_u8(load_addr + i, b);
        }

        // Apply relocations
        Self::exec_apply_relocations(memory, &data, reloc_offset, reloc_count, load_segment);

        let cs = load_segment.wrapping_add(init_cs);
        let ss = load_segment.wrapping_add(init_ss);
        cpu.setup_for_exe(cs, init_ip, ss, init_sp, psp_seg);
        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
        // Gremlin Step 1 hook: see `exec_load_com` above for rationale.
        // Use the *resolved* image CS (load_segment + init_cs from the
        // MZ header), not the PSP segment — matches `loading.rs`.
        self.last_exec_image_cs = Some(cs);

        // Part 0 trace site (gp64::dos::psp). Dumps the child PSP and
        // env block after all per-EXEC PSP writes are complete. Gated by
        // tracing::enabled! so cost is zero unless target is enabled.
        trace_psp_dump(memory, psp_seg, env_seg, "exec_load_exe");

        Ok(())
    }

    /// Read the program name from an environment segment (after the \0\0 + word count).
    pub(crate) fn read_env_program_name(&self, memory: &Memory, env_seg: u16) -> String {
        let Ok(layout) = self.parse_environment(memory, env_seg) else {
            return String::new();
        };
        let base = layout.base;
        let mut i = layout.strings_end;
        if i + 2 > layout.scan_limit {
            return String::new();
        }
        // Read word count (should be 1)
        let _count = memory.read_u16(base + i);
        i += 2;
        // Read ASCIIZ program name
        let mut name = Vec::new();
        while i < layout.scan_limit && name.len() <= 128 {
            let b = memory.read_u8(base + i);
            if b == 0 {
                break;
            }
            name.push(b);
            i += 1;
        }
        String::from_utf8_lossy(&name).to_string()
    }

    /// Clone an environment segment for EXEC, appending the child's program name.
    ///
    /// Real DOS creates a copy of the parent's environment for each child process
    /// and appends the child's full pathname after the double-null terminator
    /// (DOS 3.0+ convention: \0\0 then WORD count=1, then ASCIIZ program name).
    /// Segment zero represents a genuine empty environment; allocation failure is
    /// returned separately so it cannot alias the parent's block.
    pub(crate) fn clone_env_for_exec(
        &mut self,
        memory: &mut Memory,
        source_env_seg: u16,
        program_name: &str,
    ) -> Result<u16, DosMemError> {
        if source_env_seg == 0 {
            return Ok(0);
        }

        let layout = self
            .parse_environment(memory, source_env_seg)
            .map_err(|_| DosMemError::InvalidBlock)?;
        self.clone_env_layout(memory, &layout, program_name)
    }

    /// Clone the parsed environment `layout` for EXEC (see
    /// [`Self::clone_env_for_exec`]); the source may be anywhere in memory.
    fn clone_env_layout(
        &mut self,
        memory: &mut Memory,
        layout: &EnvironmentLayout,
        program_name: &str,
    ) -> Result<u16, DosMemError> {
        let env_base = layout.base;
        let end_offset = layout.strings_end;

        // Calculate needed size: env strings (incl double null) + word(1) + program name + null
        let name_bytes = program_name.as_bytes();
        let total_bytes = end_offset
            .checked_add(2)
            .and_then(|size| size.checked_add(name_bytes.len()))
            .and_then(|size| size.checked_add(1))
            .ok_or(DosMemError::InvalidBlock)?;
        let total_paras =
            u16::try_from(total_bytes.div_ceil(16)).map_err(|_| DosMemError::InvalidBlock)?;

        // Allocate new environment block owned by the parent PSP. We cannot
        // use owner=0 (free) because `iter_free_conventional_blocks()` would
        // include this block in the post-EXEC BSS-init fill pass, corrupting
        // the env strings + progname-extension bytes we are about to write.
        // The parent PSP is the correct interim owner — the env block is
        // logically the parent's allocation until EXEC succeeds, at which
        // point `set_owner` transfers it to the child PSP so AH=4Ch frees it
        // with the child. If EXEC fails, the existing `dos_mem.free(child_env_seg)`
        // path in `exec_program` (also rooted at parent ownership) handles
        // rollback cleanly.
        let new_seg = self.dos_mem.alloc(memory, self.current_psp, total_paras)?;

        let new_base = Memory::seg_offset_to_linear(new_seg, 0);

        // Copy environment strings including double null
        for i in 0..end_offset {
            memory.write_u8(new_base + i, memory.read_u8(env_base + i));
        }

        // Write word count (1) and program name (DOS 3.0+ convention)
        memory.write_u16(new_base + end_offset, 1);
        let mut off = end_offset + 2;
        for &b in name_bytes {
            memory.write_u8(new_base + off, b);
            off += 1;
        }
        memory.write_u8(new_base + off, 0);

        Ok(new_seg)
    }

    /// A DPMI client's EXEC parameter block. ES:(E)BX and every far pointer
    /// in the block are selector-based, as a translating host accepts them.
    /// `None` when a selector is invalid.
    fn exec_pm_param_block(&self, cpu: &Cpu, memory: &Memory) -> Option<ExecParams> {
        let offset = if self.dos_api_32bit(cpu) {
            cpu.regs.ebx as usize
        } else {
            cpu.regs.bx() as usize
        };
        let block = cpu.read_descriptor(memory, cpu.regs.es)?.base as usize + offset;
        let far_ptr = |at: usize| -> Option<Option<usize>> {
            let off = memory.read_u16(block + at);
            let sel = memory.read_u16(block + at + 2);
            if sel == 0 && off == 0 {
                return Some(None);
            }
            let base = cpu.read_descriptor(memory, sel)?.base as usize;
            Some(Some(base + off as usize))
        };
        let env = memory.read_u16(block);
        let env = if env == 0 {
            ExecEnv::Segment(0)
        } else {
            let desc = cpu.read_descriptor(memory, env)?;
            ExecEnv::Linear {
                base: desc.base as usize,
                bytes: desc.effective_limit() as usize + 1,
            }
        };
        Some(ExecParams {
            env,
            cmdline: far_ptr(0x02)?,
            fcb1: far_ptr(0x06)?,
            fcb2: far_ptr(0x0A)?,
        })
    }

    /// Run a PM parent's EXEC child in real mode, as a DPMI host reflects
    /// INT 21h 4Bh: the client's session stays and its PM context is parked
    /// in the parent's `ProcessState`.
    fn park_pm_parent_for_child(&mut self, cpu: &mut Cpu) {
        self.dpmi.set_client_in_protected_mode(false);
        cpu.dpmi_active = false;
        cpu.allow_pm_real_mode_ss = false;
        cpu.regs.cr0 &= !1;
        cpu.regs.cpl = 0;
        cpu.regs.fs = 0;
        cpu.regs.gs = 0;
        cpu.regs.fs_cache = gp64_cpu::SegmentCache::real_mode(0);
        cpu.regs.gs_cache = gp64_cpu::SegmentCache::real_mode(0);
        cpu.cs_bits = 16;
    }

    /// Resume a PM parent whose EXEC child has ended (or failed to start):
    /// its registers from the INT 21h entry, over the live machine state.
    /// The caller completes the parent's INT 21h with an IRET.
    pub(crate) fn resume_pm_parent(&mut self, cpu: &mut Cpu, parent: &ProcessState) {
        let live = cpu.regs.clone();
        cpu.regs = parent.registers.clone();
        keep_machine_state(&mut cpu.regs, &live);
        cpu.regs.cr0 |= 1;
        self.dpmi.set_client_in_protected_mode(true);
        cpu.dpmi_active = true;
        cpu.allow_pm_real_mode_ss = true;
        cpu.cs_bits = if cpu.regs.cs_cache.big { 32 } else { 16 };
        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
    }

    pub fn exec_program(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        let parent_psp = self.current_psp;
        // A DPMI client's INT 21h 4Bh: the host reflects it, running the child
        // in real mode and resuming the client in PM when the child exits.
        let pm_parent = self.dpmi_client_int21(cpu);
        let ss = cpu.regs.ss;
        let sp = cpu.regs.sp();
        let exec_ret_ip = memory.read_u16_seg(ss, sp);
        let exec_ret_cs = memory.read_u16_seg(ss, sp.wrapping_add(2));
        let exec_ret_flags = memory.read_u16_seg(ss, sp.wrapping_add(4));
        if exec_trace_enabled() {
            tracing::debug!(
                ss = format_args!("{:04X}", ss),
                sp = format_args!("{:04X}", sp),
                ret_cs = format_args!("{:04X}", exec_ret_cs),
                ret_ip = format_args!("{:04X}", exec_ret_ip),
                flags = format_args!("{:04X}", exec_ret_flags),
                "DOS EXEC save frame"
            );
        }

        // Save parent process state before switching.
        let parent_state = ProcessState {
            psp_segment: parent_psp,
            registers: cpu.regs.clone(),
            parent_psp,
            exec_ret_ip,
            exec_ret_cs,
            exec_ret_flags,
            pm_parent,
        };

        let (host_path, data) =
            match self.try_read_file_by_dos_name(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu))
            {
                Ok(v) => v,
                Err(e) => {
                    // Preserve DOS access-denied (5) vs file-not-found (2) instead of
                    // collapsing every EXEC-search failure to not-found.
                    // MDD-BUG-FLUXHOMEARPA-00018.
                    match e.kind() {
                        std::io::ErrorKind::PermissionDenied => {
                            cpu.regs.set_ax(5); // access denied
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                        std::io::ErrorKind::InvalidData => {
                            reject_exec_for_invalid_environment(cpu);
                        }
                        _ => {
                            cpu.regs.set_ax(2); // file not found
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                    return Ok(());
                }
            };

        // The parent-process stack is fixed-capacity (FixedVec<ProcessState, 16>). A
        // deep nested-EXEC chain that reaches that depth must fail the EXEC with DOS
        // "insufficient memory" (AX=8) rather than panic in FixedVec::push. Checked
        // here — before the HDD-sound and clone_env_for_exec side effects — so a
        // refused EXEC commits no partial state (no leaked child environment block).
        // MDD-BUG-CRUCIBLE-00058.
        if self.process_stack.is_full() {
            cpu.regs.set_ax(8); // insufficient memory
            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            return Ok(());
        }

        // Get the child program's DOS path for the environment program name.
        // host_path is in "C:\path\synthetic-program" format from try_read_file_by_dos_name.
        let dos_path = host_path.to_string_lossy().to_uppercase();

        // HDD seek sound: emit clicks proportional to file size
        if should_emit_hdd_sound(&dos_path) {
            let clicks = (3 + data.len() / 32768).min(15) as u8;
            self.emit_hdd_clicks(clicks);
        }

        let params = if pm_parent {
            let Some(params) = self.exec_pm_param_block(cpu, memory) else {
                reject_exec_for_invalid_environment(cpu);
                return Ok(());
            };
            params
        } else {
            let (env_seg, cmdline) =
                self.exec_read_param_block_cmdtail(memory, cpu.regs.es, cpu.regs.bx());
            let (fcb1, fcb2) = self.exec_read_param_block_fcbs(memory, cpu.regs.es, cpu.regs.bx());
            let linear = |ptr: Option<(u16, u16)>| {
                ptr.map(|(seg, off)| Memory::seg_offset_to_linear(seg, off))
            };
            ExecParams {
                env: ExecEnv::Segment(env_seg),
                cmdline: linear(cmdline),
                fcb1: linear(fcb1),
                fcb2: linear(fcb2),
            }
        };
        let env_seg_raw = match params.env {
            ExecEnv::Segment(seg) => seg,
            ExecEnv::Linear { .. } => 0,
        };
        let (cmdline_ptr, fcb1_ptr, fcb2_ptr) = (params.cmdline, params.fcb1, params.fcb2);

        {
            let tail_str = if let Some(cmdline_addr) = cmdline_ptr {
                let len = memory.read_u8(cmdline_addr) as usize;
                let mut tail_bytes = vec![0u8; len];
                for i in 0..len {
                    tail_bytes[i] = memory.read_u8(cmdline_addr + 1 + i);
                }
                String::from_utf8_lossy(&tail_bytes).to_string()
            } else {
                String::from("(none)")
            };
            // Read parent's env program name for comparison
            let parent_env = self.env_segment;
            let parent_pname = self.read_env_program_name(memory, parent_env);
            tracing::info!(
                "DOS EXEC: program=\"{}\" cmdtail=\"{}\" env_seg_raw={:#06X} parent_env_progname=\"{}\"",
                dos_path,
                tail_str.trim(),
                env_seg_raw,
                parent_pname
            );
        }

        let parent_env_seg = if env_seg_raw == 0 {
            self.caller_env_segment(memory)
        } else {
            env_seg_raw
        };

        // Real DOS creates a copy of the environment for the child process and appends
        // the child's program pathname (DOS 3.0+). This is critical for DOS extenders
        let cloned = match params.env {
            ExecEnv::Linear { base, bytes } => Self::parse_environment_at(memory, base, bytes)
                .map_err(|_| DosMemError::InvalidBlock)
                .and_then(|layout| self.clone_env_layout(memory, &layout, &dos_path)),
            ExecEnv::Segment(_) => self.clone_env_for_exec(memory, parent_env_seg, &dos_path),
        };
        let child_env_seg = match cloned {
            Ok(segment) => segment,
            Err(DosMemError::InvalidBlock) => {
                reject_exec_for_invalid_environment(cpu);
                return Ok(());
            }
            Err(DosMemError::InsufficientMemory { largest_available }) => {
                reject_exec_for_memory(cpu, largest_available);
                return Ok(());
            }
        };
        let env_seg = child_env_seg;

        // Push after we know we can at least parse the file.
        self.process_stack.push(parent_state.clone());

        // DOS BSS-init pass (HLD 2026-05-17 §10 step 8). Pre-fill every
        // free conventional MCB so wild jumps inside the child program
        // land on a recoverable byte (IRET, by default). Mirrors the
        // top-level load-path fill in `Emulator::load_with_drive_update`,
        // keeping observable behaviour consistent whether a program was
        // launched directly or via INT 21h/AH=4Bh. The child's env
        // block was already carved out of the free chain just above,
        // so `clone_env_for_exec`'s output is excluded from this fill.
        let pattern = self.bss_init_fill_byte;
        let ranges: Vec<(u16, u16)> = self.dos_mem.iter_free_conventional_blocks().collect();
        for (usable_seg, end_seg_exclusive) in ranges {
            let start = (usable_seg as usize) << 4;
            let end = (end_seg_exclusive as usize) << 4;
            memory.fill_ram(start, end, pattern);
        }

        if pm_parent {
            self.park_pm_parent_for_child(cpu);
        }

        if is_mz_executable(&data) {
            self.exec_load_exe(
                cpu,
                memory,
                &data,
                env_seg,
                cmdline_ptr,
                fcb1_ptr,
                fcb2_ptr,
                parent_psp,
            )?;
        } else {
            self.exec_load_com(
                cpu,
                memory,
                &data,
                env_seg,
                cmdline_ptr,
                fcb1_ptr,
                fcb2_ptr,
                parent_psp,
            )?;
        }

        if cpu.regs.flags.contains(gp64_cpu::Flags::CARRY) {
            // EXEC failed - free the cloned env block and roll back parent state.
            // Note: the BSS-init fill above has already overwritten free-RAM
            // contents at this point; this is intentional (real DOS does not
            // preserve free-RAM contents across a failed INT 21h/4Bh either).
            if child_env_seg != 0 {
                let _ = self.dos_mem.free(memory, child_env_seg);
            }
            let exec_error_ax = cpu.regs.ax();
            let _ = self.process_stack.pop();
            self.set_current_psp(parent_state.psp_segment);
            if pm_parent {
                self.resume_pm_parent(cpu, &parent_state);
            } else {
                cpu.regs = parent_state.registers;
            }
            cpu.regs.set_ax(exec_error_ax);
            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
        } else {
            // EXEC succeeded - fix up env block ownership to child PSP so it
            // gets freed when the child terminates (AH=4Ch).
            if child_env_seg != 0 {
                let _ = self
                    .dos_mem
                    .set_owner(memory, child_env_seg, self.current_psp);
            }
            // DOS points INT 22h and the child's PSP:0Ah at the caller's
            // return address; termination returns through PSP:0Ah, so a
            // A PM parent resumes through the DPMI host instead.
            if !pm_parent {
                let ret = (u32::from(parent_state.exec_ret_cs) << 16)
                    | u32::from(parent_state.exec_ret_ip);
                let child = Memory::seg_offset_to_linear(self.current_psp, 0);
                memory.write_u32(child + PSP_INT22_TERMINATE, ret);
                memory.write_u32(0x22 * 4, ret);
            }
        }

        Ok(())
    }

    /// Where a terminating real-mode child returns: its PSP:0Ah, which EXEC
    /// set to the caller's return address and the program may have changed.
    pub(crate) fn psp_terminate_address(memory: &Memory, psp: u16) -> (u16, u16) {
        let at = Memory::seg_offset_to_linear(psp, 0) + PSP_INT22_TERMINATE;
        (memory.read_u16(at + 2), memory.read_u16(at))
    }

    /// Load but don't execute (INT 21h AH=4Bh AL=01h)
    /// DS:DX = ASCIIZ program name
    /// ES:BX = pointer to ExecParamBlock (0x16 bytes for SS:SP and CS:IP results)
    ///
    /// This function loads the program into memory and fills in SS:SP and CS:IP
    /// in the parameter block, but does NOT transfer control to the loaded program.
    /// The environment a parameter-block environment of 0 inherits: the
    /// CALLER's, at its own PSP:2Ch.
    ///
    /// MDD-BUG-CRUCIBLE-00596: not `self.env_segment`, which is the
    /// root/master block set once at startup and never updated by
    /// `set_current_psp`. Reading the master meant a child that replaced or
    /// edited its own environment and then EXECed a grandchild silently handed
    /// down the ROOT values instead of its own. Both EXEC AL=00 and AL=01 use
    /// this (MDD-BUG-CRU-01396).
    ///
    /// PSP:2Ch of 0 is legitimate (a process with no environment), so it falls
    /// back to the master block rather than cloning segment 0.
    fn caller_env_segment(&self, memory: &Memory) -> u16 {
        let psp_base = Memory::seg_offset_to_linear(self.current_psp, 0);
        let psp_env = memory.read_u16(psp_base + PSP_ENVIRONMENT_SEG);
        // While a DPMI client lives, the host keeps its environment
        // selector in its PSP; DOS needs the segment behind it.
        let psp_env = if psp_env != 0 && psp_env == self.dpmi.env_selector() {
            self.dpmi
                .selector_to_conventional_segment(psp_env)
                .unwrap_or(self.env_segment)
        } else {
            psp_env
        };
        if psp_env != 0 {
            psp_env
        } else {
            self.env_segment
        }
    }

    pub fn exec_load_only(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        let (host_path, data) =
            match self.try_read_file_by_dos_name(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu))
            {
                Ok(v) => v,
                Err(e) => {
                    // Preserve DOS access-denied (5) vs file-not-found (2) instead of
                    // collapsing every EXEC-search failure to not-found.
                    // MDD-BUG-FLUXHOMEARPA-00018.
                    match e.kind() {
                        std::io::ErrorKind::PermissionDenied => {
                            cpu.regs.set_ax(5); // access denied
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                        std::io::ErrorKind::InvalidData => {
                            reject_exec_for_invalid_environment(cpu);
                        }
                        _ => {
                            cpu.regs.set_ax(2); // file not found
                            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                        }
                    }
                    return Ok(());
                }
            };
        // HDD seek sound: emit clicks proportional to file size
        {
            let dos_path = host_path.to_string_lossy().to_uppercase();
            if should_emit_hdd_sound(&dos_path) {
                let clicks = (3 + data.len() / 32768).min(15) as u8;
                self.emit_hdd_clicks(clicks);
            }
        }

        let param_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        let (env_seg_raw, cmdline_ptr) =
            self.exec_read_param_block_cmdtail(memory, cpu.regs.es, cpu.regs.bx());
        let (fcb1_ptr, fcb2_ptr) =
            self.exec_read_param_block_fcbs(memory, cpu.regs.es, cpu.regs.bx());
        // Environment 0 inherits the caller's, as the executing EXEC does
        // (MDD-BUG-CRU-01396).
        let env_seg = if env_seg_raw == 0 {
            self.caller_env_segment(memory)
        } else {
            env_seg_raw
        };
        if env_seg != 0 && self.parse_environment(memory, env_seg).is_err() {
            reject_exec_for_invalid_environment(cpu);
            return Ok(());
        }

        let parent_psp = self.current_psp;

        let (ss_val, sp_val, cs_val, ip_val): (u16, u16, u16, u16);

        if is_mz_executable(&data) {
            // EXE file - parse MZ header
            let header_paragraphs = u16::from_le_bytes([data[8], data[9]]) as usize;
            let reloc_count = u16::from_le_bytes([data[6], data[7]]);
            let reloc_offset = u16::from_le_bytes([data[24], data[25]]) as usize;
            let init_ss = u16::from_le_bytes([data[14], data[15]]);
            let init_sp = u16::from_le_bytes([data[16], data[17]]);
            let init_ip = u16::from_le_bytes([data[20], data[21]]);
            let init_cs = u16::from_le_bytes([data[22], data[23]]);
            let min_extra = u16::from_le_bytes([data[10], data[11]]);

            let header_size = header_paragraphs * 16;
            if header_size > data.len() {
                cpu.regs.set_ax(11); // bad format
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                return Ok(());
            }
            // MDD-BUG-CRUCIBLE-00594: stop at the DECLARED image end, not at
            // physical EOF. Taking the whole tail sized the allocation from
            // appended overlay/debug bytes (a false AX=8 on a tight machine)
            // and copied them into guest memory.
            let image = &data[header_size..mz_declared_image_end(&data, header_size)];

            // Allocate memory: PSP (16 paragraphs) + image + min_extra.
            // Sized from the header's full page count, not the copied length
            // (MDD-BUG-KILN-00639).
            let alloc_bytes = mz_alloc_image_bytes(&data, header_size);
            let largest = self.dos_mem.largest_free();
            let Some(paragraphs_needed) = exec_total_paragraphs(alloc_bytes, min_extra) else {
                reject_exec_for_memory(cpu, largest);
                return Ok(());
            };
            let psp_seg = match self
                .dos_mem
                .alloc(memory, self.current_psp, paragraphs_needed)
            {
                Ok(seg) => seg,
                Err(_) => {
                    cpu.regs.set_ax(8); // Insufficient memory
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
            };

            let mem_end = self
                .dos_mem
                .block_end_segment(psp_seg)
                .unwrap_or(psp_seg.wrapping_add(paragraphs_needed));
            let load_seg = psp_seg.wrapping_add(0x10);

            // Create PSP
            self.create_psp(memory, psp_seg, mem_end, env_seg, Some(parent_psp), &[]);
            self.exec_copy_cmdtail_to_psp(memory, psp_seg, cmdline_ptr);
            self.exec_copy_fcbs_to_psp(memory, psp_seg, fcb1_ptr, fcb2_ptr);
            let psp_base = Memory::seg_offset_to_linear(psp_seg, 0);
            memory.write_u16(psp_base + PSP_PARENT_PSP, parent_psp);
            self.save_psp_vectors(memory, psp_seg);
            self.set_current_psp(psp_seg);

            // Load code
            debug_assert!(image.len() <= (paragraphs_needed as usize - 0x10) * 16);
            let load_addr = Memory::seg_offset_to_linear(load_seg, 0);
            for (i, &byte) in image.iter().enumerate() {
                memory.write_u8(load_addr + i, byte);
            }

            // Apply relocations
            Self::exec_apply_relocations(memory, &data, reloc_offset, reloc_count, load_seg);

            ss_val = load_seg.wrapping_add(init_ss);
            sp_val = init_sp;
            cs_val = load_seg.wrapping_add(init_cs);
            ip_val = init_ip;
        } else {
            // COM file
            let code_size = data.len();
            let largest = self.dos_mem.largest_free();
            if code_size > MAX_COM_IMAGE_BYTES {
                reject_exec_for_memory(cpu, largest);
                return Ok(());
            }
            let Some(paragraphs_needed) = exec_total_paragraphs(code_size, 0) else {
                reject_exec_for_memory(cpu, largest);
                return Ok(());
            };
            let psp_seg = match self
                .dos_mem
                .alloc(memory, self.current_psp, paragraphs_needed)
            {
                Ok(seg) => seg,
                Err(_) => {
                    cpu.regs.set_ax(8);
                    cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                    return Ok(());
                }
            };

            let mem_end = self
                .dos_mem
                .block_end_segment(psp_seg)
                .unwrap_or(psp_seg.wrapping_add(paragraphs_needed));

            // Create PSP
            self.create_psp(memory, psp_seg, mem_end, env_seg, Some(parent_psp), &[]);
            self.exec_copy_cmdtail_to_psp(memory, psp_seg, cmdline_ptr);
            self.exec_copy_fcbs_to_psp(memory, psp_seg, fcb1_ptr, fcb2_ptr);
            let psp_base = Memory::seg_offset_to_linear(psp_seg, 0);
            memory.write_u16(psp_base + PSP_PARENT_PSP, parent_psp);
            self.save_psp_vectors(memory, psp_seg);
            self.set_current_psp(psp_seg);

            // Load at PSP:0100
            debug_assert!(
                PSP_LOAD_ADDRESS as usize + data.len() <= paragraphs_needed as usize * 16
            );
            let load_addr = Memory::seg_offset_to_linear(psp_seg, PSP_LOAD_ADDRESS);
            for (i, &byte) in data.iter().enumerate() {
                memory.write_u8(load_addr + i, byte);
            }

            ss_val = psp_seg;
            sp_val = 0xFFFE;
            cs_val = psp_seg;
            ip_val = PSP_LOAD_ADDRESS;
        }

        // Write results to parameter block
        // +0Eh: SS:SP (DWORD - offset first, then segment)
        // +12h: CS:IP (DWORD - offset first, then segment)
        memory.write_u16(param_addr + 0x0E, sp_val);
        memory.write_u16(param_addr + 0x10, ss_val);
        memory.write_u16(param_addr + 0x12, ip_val);
        memory.write_u16(param_addr + 0x14, cs_val);

        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);

        // Part 0 trace site (gp64::dos::psp). Dumps the child PSP and
        // env block after all per-EXEC PSP writes are complete. The child
        // PSP segment is also stored in CS (for COM) or load_seg-0x10 (for
        // EXE); read it back from `self.current_psp` which was set above.
        //
        // Note: unlike the `exec_program` trace sites, `exec_load_only`
        // does NOT run the BSS-init pass that fills free conventional
        // memory with `bss_init_fill_byte` (0xCF). So the env block at
        // env_seg reflects whatever the caller had there, not a post-
        // BSS-init view. The two trace points are therefore not semantically
        // identical — readers comparing them must account for this.
        let psp_seg_for_trace = self.current_psp();
        trace_psp_dump(memory, psp_seg_for_trace, env_seg, "exec_load_only");

        Ok(())
    }

    /// Load overlay (INT 21h AH=4Bh AL=03h)
    /// DS:DX = ASCIIZ program name
    /// ES:BX = pointer to OverlayParamBlock
    ///
    /// OverlayParamBlock:
    ///   +00h: WORD - Load segment
    ///   +02h: WORD - Relocation factor
    pub(crate) fn load_overlay(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Result<()> {
        let filename = self.read_asciiz(cpu, memory, cpu.regs.ds, self.dos_offset_dx(cpu));
        let param_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());

        let load_segment = memory.read_u16(param_addr);
        let reloc_factor = memory.read_u16(param_addr + 2);

        // Resolve path to host filesystem
        let resolved = self.path_resolver.resolve(&filename);
        let is_hdd_overlay = resolved.drive >= 2;

        tracing::debug!(
            target: "gp64::dos::path",
            op = "OVERLAY",
            name = %filename,
            resolved = %format!("{}:{}", (resolved.drive + b'A') as char, resolved.path),
            load_seg = format_args!("{:04X}", load_segment),
            reloc = format_args!("{:04X}", reloc_factor),
            "DOS load overlay"
        );

        // Get drive
        let Some(drive) = self.drives.get_mut(&resolved.drive) else {
            cpu.regs.set_ax(3); // Path not found
            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            return Ok(());
        };

        // Open file using VirtualDrive trait
        // Signature: open_file(path, write, create, truncate)
        let file = match drive.open_file(&resolved.path, false, false, false) {
            Ok(f) => f,
            Err(e) => {
                // Preserve access-denied (DOS 5) instead of collapsing every open
                // failure to path-not-found (3) / file-not-found (2), matching the
                // regular EXEC path (try_read_file_by_dos_name). MDD-BUG-CRUCIBLE-00062.
                let err_code = if is_access_denied(&e) {
                    5
                } else if filename.contains('\\') || filename.contains('/') {
                    3
                } else {
                    2
                };
                cpu.regs.set_ax(err_code);
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                return Ok(());
            }
        };

        // Read the overlay, bounding the host allocation so a huge guest-named
        // overlay file cannot exhaust host memory (MDD-BUG-FLUXHOMEARPA-00009).
        let data = match read_exec_file_capped(file) {
            Ok(data) => data,
            Err(_) => {
                cpu.regs.set_ax(5); // Access denied / not loadable
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                return Ok(());
            }
        };

        // HDD seek sound: emit clicks proportional to overlay file size
        if is_hdd_overlay {
            let clicks = (3 + data.len() / 32768).min(15) as u8;
            self.emit_hdd_clicks(clicks);
        }

        // Classify the format before checking memory bounds. MZ headers are not
        // loaded, while raw overlays occupy their complete file span.
        let mz_code_start = if data.len() >= 28 && data[0] == b'M' && data[1] == b'Z' {
            let header_paragraphs = u16::from_le_bytes([data[8], data[9]]) as usize;
            let code_start = header_paragraphs * 16;
            if code_start > data.len() {
                // MDD-BUG-CRUCIBLE-00597: a guest-supplied malformed MZ is a
                // DOS "bad format" result, not a Rust error. Bailing here
                // escaped the INT 21h AH=4Bh AL=03h service entirely instead
                // of returning AX with carry set.
                cpu.regs.set_ax(11); // Invalid format
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                return Ok(());
            }
            Some(code_start)
        } else {
            None
        };

        // Conventional memory ends at 0xA0000.
        let load_addr = Memory::seg_offset_to_linear(load_segment, 0);
        // MDD-BUG-CRUCIBLE-00594: size the bounds check from the DECLARED load
        // module. Using the physical tail could return a false AX=8 for an
        // overlay that fits once its appended debug data is excluded.
        // (The separate question of the unloaded MZ header inflating this check
        // is MDD-BUG-CRUCIBLE-00142 and is deliberately untouched here.)
        let load_size = mz_code_start.map_or(data.len(), |code_start| {
            mz_declared_image_end(&data, code_start) - code_start
        });
        if load_addr
            .checked_add(load_size)
            .is_none_or(|load_end| load_end > 0xA0000)
        {
            cpu.regs.set_ax(8); // Insufficient memory
            cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
            return Ok(());
        }

        if mz_code_start.is_some() {
            // MDD-BUG-CRUCIBLE-00597: `load_exe_overlay`'s remaining `bail!`s
            // re-check the header span and the load span, both of which are
            // already validated above, so reaching one means the guest found a
            // shape those checks disagree about. Report the DOS bad-format
            // result rather than unwinding out of an INT 21h service — no
            // guest-supplied file may escape as a Rust error. Genuine
            // emulator-internal failures (file I/O) are handled earlier.
            if let Err(error) = self.load_exe_overlay(memory, &data, load_segment, reloc_factor) {
                tracing::warn!(
                    target: "gp64::dos",
                    %error,
                    "EXEC AL=03: malformed MZ overlay rejected as DOS bad format"
                );
                cpu.regs.set_ax(11); // Invalid format
                cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);
                return Ok(());
            }
        } else {
            // Raw binary overlay - just copy to memory
            for (i, &byte) in data.iter().enumerate() {
                memory.write_u8(load_addr + i, byte);
            }
        }

        cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);
        Ok(())
    }

    pub(crate) fn load_exe_overlay(
        &self,
        memory: &mut Memory,
        data: &[u8],
        load_segment: u16,
        reloc_factor: u16,
    ) -> Result<()> {
        // Parse MZ header
        let header_paragraphs = u16::from_le_bytes([data[8], data[9]]);
        let reloc_count = u16::from_le_bytes([data[6], data[7]]);
        let reloc_offset = u16::from_le_bytes([data[24], data[25]]) as usize;

        let code_start = (header_paragraphs as usize) * 16;
        if code_start > data.len() {
            bail!("Invalid EXE header for overlay");
        }

        // MDD-BUG-CRUCIBLE-00594: the copied range stops at the declared image
        // end; appended overlay/debug bytes are not part of the load module.
        let code_data = &data[code_start..mz_declared_image_end(data, code_start)];
        let load_addr = Memory::seg_offset_to_linear(load_segment, 0);

        // Bounds check
        if load_addr + code_data.len() > 0xA0000 {
            bail!("Overlay too large");
        }

        // Load code at load_segment
        for (i, &byte) in code_data.iter().enumerate() {
            memory.write_u8(load_addr + i, byte);
        }

        // Apply relocations using reloc_factor
        let mut reloc_pos = reloc_offset;
        for _ in 0..reloc_count {
            if reloc_pos + 4 > data.len() {
                break;
            }

            let offset = u16::from_le_bytes([data[reloc_pos], data[reloc_pos + 1]]);
            let segment = u16::from_le_bytes([data[reloc_pos + 2], data[reloc_pos + 3]]);
            reloc_pos += 4;

            // Calculate address in loaded image
            let addr = Memory::seg_offset_to_linear(load_segment.wrapping_add(segment), offset);

            if addr + 2 <= 0xA0000 {
                let old_val = memory.read_u16(addr);
                let new_val = old_val.wrapping_add(reloc_factor);
                memory.write_u16(addr, new_val);
            }
        }

        Ok(())
    }

    /// Terminate and Stay Resident (INT 21h AH=31h)
    /// AL = return code
    /// DX = paragraphs to keep (minimum 0x10 for PSP)
    pub fn terminate_stay_resident(
        &mut self,
        cpu: &mut Cpu,
        memory: &mut Memory,
    ) -> Int21Disposition {
        let return_code = cpu.regs.al();
        let paragraphs = cpu.regs.dx().max(0x10); // Minimum 256 bytes for PSP

        let psp_segment = self.current_psp;

        // Free the environment block only when it belongs to this process.
        let psp_base = Memory::seg_offset_to_linear(psp_segment, 0);
        let env_seg = memory.read_u16(psp_base + PSP_ENVIRONMENT_SEG);
        let owns_environment = env_seg != 0
            && self
                .dos_mem
                .block_info_for_segment(env_seg)
                .is_some_and(|block| block.owner_psp == psp_segment);
        if owns_environment {
            let _ = self.dos_mem.free(memory, env_seg);
        }

        // Every termination restores INT 22h/23h/24h from the PSP, a TSR exit
        // included: a handler hooked in the transient tail would otherwise be
        // left pointing into the memory the resize below releases
        // (MDD-BUG-CRU-01408). The saved vectors live at PSP 0Ah-15h, inside
        // the paragraphs that stay resident.
        self.restore_psp_vectors(memory, psp_segment);

        // Resize the memory block to requested size
        if self
            .dos_mem
            .resize(memory, psp_segment, paragraphs)
            .is_err()
        {
            // If resize fails, just keep current size
        }

        // Mark memory as system-owned (prevents deallocation)
        let _ = self
            .dos_mem
            .set_owner(memory, psp_segment, MCB_OWNER_SYSTEM);

        // Store return code and termination type for INT 21h/4Dh
        self.last_exit_code = return_code;
        self.last_termination_type = 0x03; // TSR termination

        // Return to parent process if we have one
        if let Some(parent_state) = self.process_stack.pop() {
            self.set_current_psp(parent_state.psp_segment);
            self.remember_resident_return(psp_segment, &parent_state);

            // A PM parent's child went resident: the DPMI session is the
            // parent's too, so it stays, and the parent resumes in PM.
            if parent_state.pm_parent {
                self.resume_pm_parent(cpu, &parent_state);
                return Int21Disposition::Iret;
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

            // Restore the parent's registers, but not the machine state the
            // EXEC-time snapshot happens to carry: a resident DPMI client
            // out of PM) keeps its GDT/LDT/IDT/TSS, and its parent re-enters
            // protected mode through them (MDD-00017).
            let (ret_cs, ret_ip) = Self::psp_terminate_address(memory, psp_segment);
            let live = cpu.regs.clone();
            cpu.regs = parent_state.registers.clone();
            keep_machine_state(&mut cpu.regs, &live);
            cpu.regs.set_ax(return_code as u16);
            cpu.regs.cs = ret_cs;
            cpu.regs.cs_cache = gp64_cpu::SegmentCache::real_mode(ret_cs);
            cpu.regs.set_ip(ret_ip);
            cpu.regs.set_sp(cpu.regs.sp().wrapping_add(6)); // Pop INT frame
            cpu.regs.flags.remove(gp64_cpu::Flags::CARRY);

            return Int21Disposition::NoIret;
        }

        // No parent on the process stack means the shell launched us directly —
        // e.g. line 1 of a .BAT. Plain `Iret` here returns into the terminated
        // program's own context and never restores the FFFF:0000 idle sentinel
        // that `Emulator::step` uses to resume the batch interpreter, so a
        // "TSR on line 1, program on line 2" batch stalled forever on line 1
        // (MDD-BUG-KILN-00070).
        //
        // Hand control back to the shell the way the normal-exit path does:
        // restore the parent (shell) PSP and park the CPU on the sentinel.
        // Unlike a normal exit this deliberately does NOT close the process's
        // files or free its memory — INT 21h/31h keeps both, which is the whole
        // point of staying resident. The resident block is already re-owned to
        // MCB_OWNER_SYSTEM above, so it survives regardless.
        let psp_base = Memory::seg_offset_to_linear(psp_segment, 0);
        let parent_psp = memory.read_u16(psp_base + PSP_PARENT_PSP);
        if parent_psp != 0 && parent_psp != psp_segment {
            self.set_current_psp(parent_psp);
        }
        cpu.regs.cs = 0xFFFF;
        cpu.regs.cs_cache = gp64_cpu::SegmentCache::real_mode(0xFFFF);
        cpu.regs.set_ip(0x0000);
        Int21Disposition::NoIret
    }

    /// Keep a resident's EXEC return (see [`Dos::resident_returns`]). A full
    /// table drops its oldest entry: that resident can still stay resident,
    /// it just cannot terminate itself back to its parent.
    fn remember_resident_return(&mut self, resident_psp: u16, parent: &ProcessState) {
        self.resident_returns
            .retain(|(psp, _)| *psp != resident_psp);
        if self.resident_returns.is_full() {
            self.resident_returns.remove(0);
        }
        self.resident_returns.push((resident_psp, parent.clone()));
    }

    /// Take the EXEC return of `psp` if it is a resident terminating itself.
    pub(crate) fn take_resident_return(&mut self, psp: u16) -> Option<ProcessState> {
        let index = self
            .resident_returns
            .iter()
            .position(|(resident, _)| *resident == psp)?;
        Some(self.resident_returns.remove(index).1)
    }

    /// Handle INT 27h - Terminate and Stay Resident (legacy)
    /// DX = last address + 1 relative to PSP (max 64KB)
    pub fn int27_tsr(&mut self, cpu: &mut Cpu, memory: &mut Memory) -> Int21Disposition {
        let bytes_to_keep = cpu.regs.dx() as usize;
        let paragraphs = bytes_to_keep.div_ceil(16) as u16;

        // Convert to INT 21h/31h format and call that
        cpu.regs.set_al(0); // Return code 0
        cpu.regs.set_dx(paragraphs);

        // Reuse TSR logic
        self.terminate_stay_resident(cpu, memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANARY_OWNER: u16 = 0xCAFE;
    const CANARY_BYTES: [u8; 16] = [0xA5; 16];

    fn oversized_mz_image() -> Vec<u8> {
        let header_paragraphs = 2u16;
        let header_size = header_paragraphs as usize * 16;
        let mut data = vec![0; header_size + 0x10_0000];
        data[..2].copy_from_slice(b"MZ");
        data[8..10].copy_from_slice(&header_paragraphs.to_le_bytes());
        data[24..26].copy_from_slice(&28u16.to_le_bytes());
        data[header_size..].fill(0xCC);
        data
    }

    fn mz_overlay_image(code_len: usize, fill: u8) -> Vec<u8> {
        let header_paragraphs = 2u16;
        let header_size = header_paragraphs as usize * 16;
        let mut data = vec![0; header_size + code_len];
        data[..2].copy_from_slice(b"MZ");
        data[8..10].copy_from_slice(&header_paragraphs.to_le_bytes());
        data[24..26].copy_from_slice(&28u16.to_le_bytes());
        data[header_size..].fill(fill);
        data
    }

    /// An MZ whose `e_cp`/`e_cblp` declare exactly `code_len` bytes of load
    /// module, followed by `trailing_len` bytes of appended overlay/debug data
    /// that a DOS loader must neither size from nor copy
    /// (MDD-BUG-CRUCIBLE-00594).
    ///
    /// `mz_overlay_image` above leaves the page fields ZERO, which means
    /// "unset" and legitimately falls back to the whole file — which is why it
    /// cannot witness this defect.
    fn mz_image_with_trailing(
        code_len: usize,
        fill: u8,
        trailing_len: usize,
        trailing_fill: u8,
    ) -> Vec<u8> {
        let header_paragraphs = 2u16;
        let header_size = header_paragraphs as usize * 16;
        let declared = header_size + code_len;
        let mut data = vec![0u8; declared + trailing_len];
        data[..2].copy_from_slice(b"MZ");
        let pages = declared.div_ceil(512) as u16;
        let last_page = (declared % 512) as u16;
        data[2..4].copy_from_slice(&last_page.to_le_bytes());
        data[4..6].copy_from_slice(&pages.to_le_bytes());
        data[8..10].copy_from_slice(&header_paragraphs.to_le_bytes());
        data[24..26].copy_from_slice(&28u16.to_le_bytes());
        data[header_size..declared].fill(fill);
        data[declared..].fill(trailing_fill);
        data
    }

    // ------------------------------------------------------------------
    // MDD-BUG-CRUCIBLE-00597 — a guest-supplied malformed MZ overlay is a DOS
    // result (AX + CF), never a Rust error escaping the INT 21h service.
    // ------------------------------------------------------------------

    /// Drive one AH=4Bh AL=03h overlay load of `bytes` and return the CPU.
    fn run_overlay_load(bytes: Vec<u8>, load_segment: u16) -> (Result<()>, Cpu) {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("BAD.EXE"), bytes).unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        configure_load_only(&mut dos, &mut cpu, &mut memory, temp_dir.path(), "BAD.EXE");
        let param_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        memory.write_u16(param_addr, load_segment);
        memory.write_u16(param_addr + 2, 0);

        let outcome = dos.load_overlay(&mut cpu, &mut memory);
        (outcome, cpu)
    }

    #[test]
    fn load_overlay_reports_a_malformed_mz_header_as_dos_bad_format() {
        // e_cparhdr claims a header far larger than the file, so the load
        // module would start past EOF.
        let mut data = vec![0u8; 0x80];
        data[..2].copy_from_slice(b"MZ");
        data[8..10].copy_from_slice(&0xFFFFu16.to_le_bytes()); // e_cparhdr
        data[24..26].copy_from_slice(&28u16.to_le_bytes());

        let (outcome, cpu) = run_overlay_load(data, 0x4000);

        assert!(
            outcome.is_ok(),
            "a guest-supplied malformed MZ must not escape INT 21h as a Rust error: {outcome:?}"
        );
        assert!(
            cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "a rejected overlay must set carry"
        );
        assert_eq!(cpu.regs.ax(), 11, "DOS error 11 = invalid format");
    }

    #[test]
    fn load_overlay_reports_an_oversized_load_span_as_insufficient_memory() {
        // A declared load module that cannot fit below 0xA0000 from the
        // requested segment is an allocation failure, not a format error.
        let data = mz_image_with_trailing(0x400, 0xA5, 0, 0);

        let (outcome, cpu) = run_overlay_load(data, 0x9FFF);

        assert!(
            outcome.is_ok(),
            "an oversized overlay must not escape INT 21h as a Rust error: {outcome:?}"
        );
        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 8, "DOS error 8 = insufficient memory");
    }

    #[test]
    fn load_overlay_accepts_a_well_formed_mz() {
        // The rejections above must not be catching ordinary overlays.
        let data = mz_image_with_trailing(0x40, 0xA5, 0, 0);

        let (outcome, cpu) = run_overlay_load(data, 0x4000);

        assert!(outcome.is_ok());
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "a valid overlay must succeed"
        );
    }

    #[test]
    fn load_overlay_copies_only_the_declared_load_module() {
        const LOAD_SEGMENT: u16 = 0x4000;
        const LOAD_ADDRESS: usize = 0x40000;
        const CODE_LEN: usize = 0x40;
        const TRAILING_LEN: usize = 0x80;
        const CANARY: u8 = 0xC3;

        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join("OVL.EXE"),
            mz_image_with_trailing(CODE_LEN, 0xA5, TRAILING_LEN, 0x5A),
        )
        .unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        configure_load_only(&mut dos, &mut cpu, &mut memory, temp_dir.path(), "OVL.EXE");
        let param_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        memory.write_u16(param_addr, LOAD_SEGMENT);
        memory.write_u16(param_addr + 2, 0);

        // Canary the region the trailing bytes would land in.
        for offset in 0..TRAILING_LEN {
            memory.write_u8(LOAD_ADDRESS + CODE_LEN + offset, CANARY);
        }

        dos.load_overlay(&mut cpu, &mut memory).unwrap();

        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(
            memory.read_u8(LOAD_ADDRESS),
            0xA5,
            "the declared load module must be copied"
        );
        assert_eq!(memory.read_u8(LOAD_ADDRESS + CODE_LEN - 1), 0xA5);
        for offset in 0..TRAILING_LEN {
            assert_eq!(
                memory.read_u8(LOAD_ADDRESS + CODE_LEN + offset),
                CANARY,
                "trailing byte {offset} was mapped into guest memory"
            );
        }
    }

    #[test]
    fn exec_load_only_maps_only_the_declared_load_module() {
        const CODE_LEN: usize = 0x40;
        const TRAILING_LEN: usize = 0x200;

        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join("LO.EXE"),
            mz_image_with_trailing(CODE_LEN, 0xA5, TRAILING_LEN, 0x5A),
        )
        .unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        configure_load_only(&mut dos, &mut cpu, &mut memory, temp_dir.path(), "LO.EXE");

        dos.exec_load_only(&mut cpu, &mut memory).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

        // The parameter block reports the loaded CS:IP; the image sits at CS:0.
        let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
        let cs = memory.read_u16(param_addr + 0x14);
        let load_addr = Memory::seg_offset_to_linear(cs, 0);

        assert_eq!(
            memory.read_u8(load_addr),
            0xA5,
            "the declared load module must be copied"
        );
        assert_eq!(memory.read_u8(load_addr + CODE_LEN - 1), 0xA5);

        // The trailing block is 0x200 identical bytes, so if the loader mapped
        // it there would be a long run of them straight after the image. A
        // single incidental 0x5A elsewhere in the child's allocation is not
        // evidence either way, so look for the RUN rather than any one byte.
        let after: Vec<u8> = (0..TRAILING_LEN)
            .map(|offset| memory.read_u8(load_addr + CODE_LEN + offset))
            .collect();
        let longest_run = after
            .split(|&byte| byte != 0x5A)
            .map(<[u8]>::len)
            .max()
            .unwrap_or(0);
        assert!(
            longest_run < 16,
            "the loader mapped trailing data: {longest_run} consecutive 0x5A bytes \
             follow the declared load module"
        );
    }

    #[test]
    fn mz_declared_image_end_stops_at_the_page_fields() {
        // Declared: 2-paragraph header + 0x40 code = 0x60 bytes, plus 0x100
        // bytes of trailing data that is NOT image.
        let data = mz_image_with_trailing(0x40, 0xA5, 0x100, 0x5A);
        assert_eq!(data.len(), 0x160);
        assert_eq!(mz_declared_image_end(&data, 0x20), 0x60);

        // e_cp == 0 means "unset": fall back to the whole file, which is what
        // keeps files that omit the fields loadable.
        let legacy = mz_overlay_image(0x40, 0xA5);
        assert_eq!(mz_declared_image_end(&legacy, 0x20), legacy.len());
    }

    #[test]
    fn mz_alloc_image_bytes_rounds_to_the_page_boundary() {
        // Synthetic header: 3 pages, 128 bytes on the last page, 2 header paragraphs.
        // DOS reserves pages*512 - headersize = 1,504 bytes; the last-page
        // count bounds the copy, never the allocation (MDD-BUG-KILN-00639).
        let mut data = vec![0u8; 64];
        data[..2].copy_from_slice(b"MZ");
        data[2..4].copy_from_slice(&128u16.to_le_bytes());
        data[4..6].copy_from_slice(&3u16.to_le_bytes());
        assert_eq!(mz_alloc_image_bytes(&data, 32), 3 * 512 - 32);

        // e_cp == 0: whole-file fallback, mirroring mz_declared_image_end.
        let legacy = mz_overlay_image(0x40, 0xA5);
        assert_eq!(mz_alloc_image_bytes(&legacy, 0x20), legacy.len() - 0x20);
    }

    #[test]
    fn exec_load_exe_allocates_the_full_final_page() {
        // MDD-BUG-KILN-00639: a declared image of 0x60 code bytes ends 0x80
        // bytes into the single 512-byte page; DOS still reserves the whole
        // page: 512 - 32 (header) = 480 bytes = 30 paragraphs, plus 0x10 for
        // the PSP. The buggy copy-length expression reserved only 6 paragraphs.
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);

        dos.exec_load_exe(
            &mut cpu,
            &mut memory,
            &mz_image_with_trailing(0x60, 0xA5, 0, 0),
            0,
            None,
            None,
            None,
            0x0100,
        )
        .unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

        let psp_seg = dos.current_psp();
        let mem_end = dos.dos_mem.block_end_segment(psp_seg).unwrap();
        assert_eq!(
            mem_end.wrapping_sub(psp_seg),
            0x10 + 30,
            "PSP + page-rounded image allocation"
        );
        // The PSP top-of-memory word is what guest memory checks read.
        let psp_base = Memory::seg_offset_to_linear(psp_seg, 0);
        assert_eq!(
            memory.read_u16(psp_base + 2),
            mem_end,
            "PSP top-of-memory word must reflect the page-rounded allocation"
        );
    }

    fn arrange_exec_hole_and_canary(
        dos: &mut Dos,
        memory: &mut Memory,
        hole_paragraphs: u16,
    ) -> u16 {
        let hole = dos
            .dos_mem
            .alloc(memory, dos.current_psp, hole_paragraphs)
            .unwrap();
        let canary = dos.dos_mem.alloc(memory, CANARY_OWNER, 1).unwrap();
        let canary_addr = Memory::seg_offset_to_linear(canary, 0);
        for (index, byte) in CANARY_BYTES.iter().copied().enumerate() {
            memory.write_u8(canary_addr + index, byte);
        }
        dos.dos_mem.free(memory, hole).unwrap();
        canary
    }

    fn arrange_bounded_exec_hole_and_canary(
        dos: &mut Dos,
        memory: &mut Memory,
        hole_paragraphs: u16,
    ) -> u16 {
        let hole = dos
            .dos_mem
            .alloc(memory, dos.current_psp, hole_paragraphs)
            .unwrap();
        let canary = dos.dos_mem.alloc(memory, CANARY_OWNER, 1).unwrap();
        let tail_paragraphs = dos.dos_mem.largest_free();
        let _tail = dos.dos_mem.alloc(memory, 0xBEEF, tail_paragraphs).unwrap();
        let canary_addr = Memory::seg_offset_to_linear(canary, 0);
        for (index, byte) in CANARY_BYTES.iter().copied().enumerate() {
            memory.write_u8(canary_addr + index, byte);
        }
        dos.dos_mem.free(memory, hole).unwrap();
        assert_eq!(dos.dos_mem.largest_free(), hole_paragraphs);
        canary
    }

    fn assert_canary(memory: &Memory, segment: u16) {
        let canary_addr = Memory::seg_offset_to_linear(segment, 0);
        for (index, expected) in CANARY_BYTES.iter().copied().enumerate() {
            assert_eq!(
                memory.read_u8(canary_addr + index),
                expected,
                "adjacent allocation changed at byte {index}"
            );
        }
    }

    fn configure_load_only(
        dos: &mut Dos,
        cpu: &mut Cpu,
        memory: &mut Memory,
        root: &std::path::Path,
        name: &str,
    ) {
        dos.drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(root.to_path_buf())),
        );
        let filename_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
        let guest_path = format!("C:\\{name}\0");
        for (index, byte) in guest_path.bytes().enumerate() {
            memory.write_u8(filename_addr + index, byte);
        }
        cpu.regs.ds = 0x2000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.es = 0x3000;
        cpu.regs.set_bx(0x0200);
    }

    #[test]
    fn exec_load_exe_rejects_image_whose_paragraph_count_exceeds_u16() {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        let canary = arrange_exec_hole_and_canary(&mut dos, &mut memory, 0x10);

        dos.exec_load_exe(
            &mut cpu,
            &mut memory,
            &oversized_mz_image(),
            0,
            None,
            None,
            None,
            0x0100,
        )
        .unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 8);
        assert_eq!(dos.current_psp(), 0x0100);
        assert_canary(&memory, canary);
    }

    #[test]
    fn exec_load_com_rejects_image_larger_than_fragmented_hole() {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        let canary = arrange_bounded_exec_hole_and_canary(&mut dos, &mut memory, 0x20);

        dos.exec_load_com(
            &mut cpu,
            &mut memory,
            &vec![0xCC; 0x0200],
            0,
            None,
            None,
            None,
            0x0100,
        )
        .unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 8);
        assert_eq!(cpu.regs.bx(), 0x20);
        assert_eq!(dos.current_psp(), 0x0100);
        assert_eq!(dos.dos_mem.largest_free(), 0x20);
        assert_canary(&memory, canary);
    }

    #[test]
    fn exec_load_com_rejects_payload_larger_than_segment() {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        let largest = dos.dos_mem.largest_free();

        dos.exec_load_com(
            &mut cpu,
            &mut memory,
            &vec![0xCC; MAX_COM_IMAGE_BYTES + 1],
            0,
            None,
            None,
            None,
            0x0100,
        )
        .unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 8);
        assert_eq!(cpu.regs.bx(), largest);
        assert_eq!(dos.current_psp(), 0x0100);
        assert_eq!(dos.dos_mem.largest_free(), largest);
    }

    #[test]
    fn exec_load_com_accepts_maximum_payload() {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);

        dos.exec_load_com(
            &mut cpu,
            &mut memory,
            &vec![0x7B; MAX_COM_IMAGE_BYTES],
            0,
            None,
            None,
            None,
            0x0100,
        )
        .unwrap();

        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(memory.read_u8_seg(cpu.regs.cs, 0x0100), 0x7B);
        assert_eq!(memory.read_u8_seg(cpu.regs.cs, 0xFFFF), 0x7B);
    }

    /// MDD-BUG-CRUCIBLE-00596 — parameter-block environment 0 must clone the
    /// CALLER's environment (its PSP:2Ch), not the root/master block.
    ///
    /// The setup is the nested-EXEC shape from the ledger: a child whose own
    /// environment differs from the master EXECs a grandchild with env 0. The
    /// grandchild must receive the CHILD's values.
    #[test]
    fn exec_with_environment_zero_clones_the_callers_psp_environment() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("TINY.COM"), [0x90]).unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        dos.drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(
                temp_dir.path().to_path_buf(),
            )),
        );

        // The ROOT/master environment: what the defect would clone.
        let master_psp = 0x0100;
        let master_env = dos.dos_mem.alloc(&mut memory, master_psp, 0x10).unwrap();
        dos.env_segment = master_env;
        let master_base = Memory::seg_offset_to_linear(master_env, 0);
        for (index, &byte) in b"FROM=MASTER\0\0".iter().enumerate() {
            memory.write_u8(master_base + index, byte);
        }

        // The CHILD, mid-flight, with its OWN environment at its PSP:2Ch.
        let child_psp = 0x0800;
        let child_env = dos.dos_mem.alloc(&mut memory, child_psp, 0x10).unwrap();
        let child_base = Memory::seg_offset_to_linear(child_env, 0);
        for (index, &byte) in b"FROM=CHILD\0\0".iter().enumerate() {
            memory.write_u8(child_base + index, byte);
        }
        dos.set_current_psp(child_psp);
        memory.write_u16_seg(child_psp, PSP_ENVIRONMENT_SEG as u16, child_env);

        assert_ne!(
            child_env, master_env,
            "the two environments must be distinct for this test to mean anything"
        );

        // The child EXECs a grandchild with parameter-block environment 0.
        let filename_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
        for (index, &byte) in b"TINY.COM\0".iter().enumerate() {
            memory.write_u8(filename_addr + index, byte);
        }
        cpu.regs.ds = 0x2000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.es = 0;
        cpu.regs.set_bx(0); // parameter block absent => environment 0
        cpu.regs.ss = 0x3000;
        cpu.regs.set_sp(0x0200);
        memory.write_u16_seg(0x3000, 0x0200, 0x1111);
        memory.write_u16_seg(0x3000, 0x0202, 0x2222);
        memory.write_u16_seg(0x3000, 0x0204, 0x0202);

        dos.exec_program(&mut cpu, &mut memory).unwrap();
        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "the EXEC itself must succeed"
        );

        // The grandchild's environment is whatever its PSP:2Ch now points at.
        let grandchild_psp = dos.current_psp();
        let grandchild_env =
            memory.read_u16(Memory::seg_offset_to_linear(grandchild_psp, 0) + PSP_ENVIRONMENT_SEG);
        let grandchild_base = Memory::seg_offset_to_linear(grandchild_env, 0);
        let cloned: Vec<u8> = (0..11)
            .map(|offset| memory.read_u8(grandchild_base + offset))
            .collect();

        assert_eq!(
            &cloned[..10],
            b"FROM=CHILD",
            "the grandchild inherited the master environment instead of its caller's"
        );
        // And the program-name trailer still gets appended to the clone.
        assert!(
            dos.read_env_program_name(&memory, grandchild_env)
                .to_ascii_uppercase()
                .contains("TINY.COM"),
            "the child's program-name trailer is missing from the cloned environment"
        );
    }

    /// MDD-BUG-CRU-01396: EXEC AL=01 with parameter-block environment 0 gives
    /// the child its CALLER's environment (PSP:2Ch), as AL=00 does, not the
    /// root/master block.
    #[test]
    fn exec_load_only_with_environment_zero_clones_the_callers_environment() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("TINY.COM"), [0x90]).unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        dos.drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(
                temp_dir.path().to_path_buf(),
            )),
        );

        let master_env = dos.dos_mem.alloc(&mut memory, 0x0100, 0x10).unwrap();
        dos.env_segment = master_env;
        let master_base = Memory::seg_offset_to_linear(master_env, 0);
        for (index, &byte) in b"FROM=MASTER\0\0".iter().enumerate() {
            memory.write_u8(master_base + index, byte);
        }
        let caller_psp = 0x0800;
        let caller_env = dos.dos_mem.alloc(&mut memory, caller_psp, 0x10).unwrap();
        let caller_base = Memory::seg_offset_to_linear(caller_env, 0);
        for (index, &byte) in b"FROM=CHILD\0\0".iter().enumerate() {
            memory.write_u8(caller_base + index, byte);
        }
        dos.set_current_psp(caller_psp);
        memory.write_u16_seg(caller_psp, PSP_ENVIRONMENT_SEG as u16, caller_env);

        let filename_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
        for (index, &byte) in b"TINY.COM\0".iter().enumerate() {
            memory.write_u8(filename_addr + index, byte);
        }
        // A zeroed AL=01 parameter block: environment 0, no tail or FCBs.
        for offset in 0..0x16 {
            memory.write_u8(Memory::seg_offset_to_linear(0x4000, 0) + offset, 0);
        }
        cpu.regs.ds = 0x2000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.es = 0x4000;
        cpu.regs.set_bx(0);

        dos.exec_load_only(&mut cpu, &mut memory).unwrap();
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));

        let child_env = memory
            .read_u16(Memory::seg_offset_to_linear(dos.current_psp(), 0) + PSP_ENVIRONMENT_SEG);
        let child_base = Memory::seg_offset_to_linear(child_env, 0);
        let cloned: Vec<u8> = (0..10)
            .map(|offset| memory.read_u8(child_base + offset))
            .collect();
        assert_eq!(
            &cloned, b"FROM=CHILD",
            "AL=01 handed down the master environment"
        );
    }

    /// A 16-bit DPMI client selector covering `base..base+0xFFFF`.
    fn pm_data_selector(dos: &mut Dos, cpu: &mut Cpu, memory: &mut Memory, base: u32) -> u16 {
        cpu.regs.set_ax(0x0000);
        cpu.regs.set_cx(1);
        dos.dpmi.int31(cpu, memory).unwrap();
        let selector = cpu.regs.ax();
        cpu.regs.set_ax(0x0007);
        cpu.regs.set_bx(selector);
        cpu.regs.set_cx((base >> 16) as u16);
        cpu.regs.set_dx(base as u16);
        dos.dpmi.int31(cpu, memory).unwrap();
        cpu.regs.set_ax(0x0008);
        cpu.regs.set_bx(selector);
        cpu.regs.set_cx(0);
        cpu.regs.set_dx(0xFFFF);
        dos.dpmi.int31(cpu, memory).unwrap();
        selector
    }

    /// A protected-mode client supplies selectors, possibly into extended memory.
    /// The host runs the child in real mode on the same DPMI session and
    /// resumes the client in PM when the child exits.
    #[test]
    fn exec_from_dpmi_client_runs_child_in_real_mode_and_resumes_the_client() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("TINY.COM"), [0x90]).unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        dos.drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(
                temp_dir.path().to_path_buf(),
            )),
        );
        let parent_psp = dos.dos_mem.alloc(&mut memory, 0x0100, 0x10).unwrap();
        dos.set_current_psp(parent_psp);

        cpu.regs.set_ax(0); // 16-bit client
        dos.dpmi
            .enter_protected_mode(&mut cpu, &mut memory)
            .unwrap();
        dos.dpmi.set_cpu_mode(0);

        // Filename, parameter block, command tail and environment all live
        // above 1 MB, named by selectors.
        let base = 0x0015_0000u32;
        let data = pm_data_selector(&mut dos, &mut cpu, &mut memory, base);
        let env = pm_data_selector(&mut dos, &mut cpu, &mut memory, base + 0x1000);
        let at = |off: u32| (base + off) as usize;
        for (i, &b) in b"TINY.COM\0".iter().enumerate() {
            memory.write_u8(at(0x100) + i, b);
        }
        memory.write_u16(at(0x200), env);
        memory.write_u16(at(0x202), 0x0300);
        memory.write_u16(at(0x204), data);
        for (i, &b) in b"\x03 AB\r".iter().enumerate() {
            memory.write_u8(at(0x300) + i, b);
        }
        for (i, &b) in b"X=PM\0\0".iter().enumerate() {
            memory.write_u8(at(0x1000) + i, b);
        }
        cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::DS, data);
        cpu.load_data_segment(&memory, gp64_cpu::DataSegReg::ES, data);
        cpu.regs.set_dx(0x0100);
        cpu.regs.set_bx(0x0200);
        cpu.regs.flags.insert(gp64_cpu::Flags::CARRY);

        dos.exec_program(&mut cpu, &mut memory).unwrap();

        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "EXEC failed"
        );
        assert_eq!(cpu.regs.cr0 & 1, 0, "the child runs in real mode");
        assert!(!dos.dpmi.in_protected_mode());
        let child_psp = dos.current_psp();
        let psp = Memory::seg_offset_to_linear(child_psp, 0);
        let tail: Vec<u8> = (0..5).map(|i| memory.read_u8(psp + 0x80 + i)).collect();
        assert_eq!(tail, b"\x03 AB\r");
        let child_env = memory.read_u16(psp + PSP_ENVIRONMENT_SEG);
        let env_base = Memory::seg_offset_to_linear(child_env, 0);
        let env_bytes: Vec<u8> = (0..4).map(|i| memory.read_u8(env_base + i)).collect();
        assert_eq!(env_bytes, b"X=PM");

        let disposition = dos.terminate_process(&mut cpu, &mut memory, 0, 0).unwrap();

        assert!(matches!(disposition, Int21Disposition::Iret));
        assert_eq!(cpu.regs.cr0 & 1, 1, "the client resumes in protected mode");
        assert!(dos.dpmi.in_protected_mode());
        assert!(
            dos.dpmi.was_initialized(),
            "the DPMI session survives the child"
        );
        assert_eq!(cpu.regs.ds, data);
        assert_eq!(dos.current_psp(), parent_psp);
        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(
            dos.dpmi_program_exit_code, None,
            "a child's exit is not the client's"
        );
    }

    #[test]
    fn exec_program_rejects_failed_environment_clone_without_aliasing_parent() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("TINY.COM"), [0x90]).unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        dos.drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(
                temp_dir.path().to_path_buf(),
            )),
        );

        let parent_psp = 0x0100;
        dos.set_current_psp(parent_psp);
        let parent_env = dos.dos_mem.alloc(&mut memory, parent_psp, 0x30).unwrap();
        dos.env_segment = parent_env;
        let env_base = Memory::seg_offset_to_linear(parent_env, 0);
        for offset in 0..0x02F0 {
            memory.write_u8(env_base + offset, b'A');
        }
        memory.write_u8(env_base + 0x02F0, 0);
        memory.write_u8(env_base + 0x02F1, 0);

        let child_hole = dos.dos_mem.alloc(&mut memory, 0xCAFE, 0x20).unwrap();
        let tail_paragraphs = dos.dos_mem.largest_free();
        let _tail = dos
            .dos_mem
            .alloc(&mut memory, 0xBEEF, tail_paragraphs)
            .unwrap();
        dos.dos_mem.free(&mut memory, child_hole).unwrap();
        assert_eq!(dos.dos_mem.largest_free(), 0x20);

        let filename_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
        for (index, &byte) in b"TINY.COM\0".iter().enumerate() {
            memory.write_u8(filename_addr + index, byte);
        }
        cpu.regs.ds = 0x2000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.es = 0;
        cpu.regs.set_bx(0);
        cpu.regs.ss = 0x3000;
        cpu.regs.set_sp(0x0200);
        memory.write_u16_seg(0x3000, 0x0200, 0x1111);
        memory.write_u16_seg(0x3000, 0x0202, 0x2222);
        memory.write_u16_seg(0x3000, 0x0204, 0x0202);

        dos.exec_program(&mut cpu, &mut memory).unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 8);
        assert_eq!(dos.current_psp(), parent_psp);
        assert!(dos.process_stack.is_empty());
        assert_eq!(dos.dos_mem.largest_free(), 0x20);
        let env_info = dos
            .dos_mem
            .block_info_for_segment(parent_env)
            .expect("parent environment must remain allocated");
        assert_eq!(env_info.owner_psp, parent_psp);
        for offset in 0..0x02F0 {
            assert_eq!(memory.read_u8(env_base + offset), b'A');
        }
        assert_eq!(memory.read_u8(env_base + 0x02F0), 0);
        assert_eq!(memory.read_u8(env_base + 0x02F1), 0);
    }

    #[test]
    fn clone_env_rejects_non_null_whole_mcb_without_mutation() {
        let mut dos = Dos::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        let source = dos.dos_mem.alloc(&mut memory, 0x0100, 0x10).unwrap();
        let source_base = Memory::seg_offset_to_linear(source, 0);
        for offset in 0..0x100 {
            memory.write_u8(source_base + offset, b'A');
        }
        let blocks_before = dos.dos_mem.mcb_summary();
        let source_before: Vec<_> = (0..0x100)
            .map(|offset| memory.read_u8(source_base + offset))
            .collect();

        let result = dos.clone_env_for_exec(&mut memory, source, "CHILD.EXE");

        assert_eq!(result, Err(DosMemError::InvalidBlock));
        assert_eq!(dos.dos_mem.mcb_summary(), blocks_before);
        assert_eq!(
            (0..0x100)
                .map(|offset| memory.read_u8(source_base + offset))
                .collect::<Vec<_>>(),
            source_before
        );
    }

    #[test]
    fn clone_env_rejects_unresolved_non_null_fallback_without_mutation() {
        let mut dos = Dos::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        let source = 0x6000;
        let source_base = Memory::seg_offset_to_linear(source, 0);
        for offset in 0..4096 {
            memory.write_u8(source_base + offset, b'B');
        }
        let blocks_before = dos.dos_mem.mcb_summary();
        let source_before: Vec<_> = (0..4096)
            .map(|offset| memory.read_u8(source_base + offset))
            .collect();

        let result = dos.clone_env_for_exec(&mut memory, source, "CHILD.EXE");

        assert_eq!(result, Err(DosMemError::InvalidBlock));
        assert_eq!(dos.dos_mem.mcb_summary(), blocks_before);
        assert_eq!(
            (0..4096)
                .map(|offset| memory.read_u8(source_base + offset))
                .collect::<Vec<_>>(),
            source_before
        );
    }

    #[test]
    fn clone_env_accepts_double_null_at_mcb_boundary() {
        let mut dos = Dos::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        let source = dos.dos_mem.alloc(&mut memory, 0x0100, 0x10).unwrap();
        let source_base = Memory::seg_offset_to_linear(source, 0);
        for offset in 0..0x00FE {
            memory.write_u8(source_base + offset, b'C');
        }
        memory.write_u8(source_base + 0x00FE, 0);
        memory.write_u8(source_base + 0x00FF, 0);

        let clone = dos
            .clone_env_for_exec(&mut memory, source, "CHILD.EXE")
            .unwrap();
        let clone_base = Memory::seg_offset_to_linear(clone, 0);

        assert_eq!(memory.read_u8(clone_base + 0x00FD), b'C');
        assert_eq!(memory.read_u8(clone_base + 0x00FE), 0);
        assert_eq!(memory.read_u8(clone_base + 0x00FF), 0);
        assert_eq!(memory.read_u16(clone_base + 0x0100), 1);
        assert_eq!(
            (0..=9)
                .map(|offset| memory.read_u8(clone_base + 0x0102 + offset))
                .collect::<Vec<_>>(),
            b"CHILD.EXE\0"
        );
    }

    #[test]
    fn environment_parser_accepts_fallback_maximum() {
        let dos = Dos::new();
        let mut memory = Memory::new();
        let source = 0x6000;
        let source_base = Memory::seg_offset_to_linear(source, 0);
        for offset in 0..ENV_FALLBACK_SCAN_BYTES - 2 {
            memory.write_u8(source_base + offset, b'D');
        }
        memory.write_u8(source_base + ENV_FALLBACK_SCAN_BYTES - 2, 0);
        memory.write_u8(source_base + ENV_FALLBACK_SCAN_BYTES - 1, 0);

        let layout = dos.parse_environment(&memory, source).unwrap();

        assert_eq!(layout.strings_end, ENV_FALLBACK_SCAN_BYTES);
        assert_eq!(layout.scan_limit, ENV_FALLBACK_SCAN_BYTES);
    }

    #[test]
    fn exec_program_reports_invalid_environment_without_allocating() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("TINY.COM"), [0x90]).unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        dos.drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(
                temp_dir.path().to_path_buf(),
            )),
        );

        let parent_psp = 0x0100;
        dos.set_current_psp(parent_psp);
        let parent_env = dos.dos_mem.alloc(&mut memory, parent_psp, 0x10).unwrap();
        dos.env_segment = parent_env;
        let env_base = Memory::seg_offset_to_linear(parent_env, 0);
        for offset in 0..0x0100 {
            memory.write_u8(env_base + offset, b'E');
        }
        let blocks_before = dos.dos_mem.mcb_summary();

        let filename_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
        for (index, &byte) in b"TINY.COM\0".iter().enumerate() {
            memory.write_u8(filename_addr + index, byte);
        }
        cpu.regs.ds = 0x2000;
        cpu.regs.set_dx(0x0100);
        cpu.regs.es = 0;
        cpu.regs.set_bx(0);
        cpu.regs.ss = 0x3000;
        cpu.regs.set_sp(0x0200);
        memory.write_u16_seg(0x3000, 0x0200, 0x1111);
        memory.write_u16_seg(0x3000, 0x0202, 0x2222);
        memory.write_u16_seg(0x3000, 0x0204, 0x0202);

        dos.exec_program(&mut cpu, &mut memory).unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 10);
        assert_eq!(dos.current_psp(), parent_psp);
        assert!(dos.process_stack.is_empty());
        assert_eq!(dos.dos_mem.mcb_summary(), blocks_before);
        assert!((0..0x0100).all(|offset| memory.read_u8(env_base + offset) == b'E'));
    }

    #[test]
    fn exec_load_only_reports_invalid_environment_without_allocating() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("TINY.COM"), [0x90]).unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        let parent_psp = 0x0100;
        dos.set_current_psp(parent_psp);
        let parent_env = dos.dos_mem.alloc(&mut memory, parent_psp, 0x10).unwrap();
        let env_base = Memory::seg_offset_to_linear(parent_env, 0);
        for offset in 0..0x0100 {
            memory.write_u8(env_base + offset, b'F');
        }
        let blocks_before = dos.dos_mem.mcb_summary();

        configure_load_only(&mut dos, &mut cpu, &mut memory, temp_dir.path(), "TINY.COM");
        let param_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        memory.write_u16(param_addr, parent_env);

        dos.exec_load_only(&mut cpu, &mut memory).unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 10);
        assert_eq!(dos.current_psp(), parent_psp);
        assert_eq!(dos.dos_mem.mcb_summary(), blocks_before);
        assert!((0..0x0100).all(|offset| memory.read_u8(env_base + offset) == b'F'));
    }

    #[test]
    fn path_search_reports_unterminated_environment() {
        let mut dos = Dos::new();
        let cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        let parent_env = dos.dos_mem.alloc(&mut memory, 0x0100, 0x10).unwrap();
        dos.env_segment = parent_env;
        let env_base = Memory::seg_offset_to_linear(parent_env, 0);
        for offset in 0..0x0100 {
            memory.write_u8(env_base + offset, b'G');
        }

        let filename_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
        for (index, &byte) in b"MISSING\0".iter().enumerate() {
            memory.write_u8(filename_addr + index, byte);
        }
        let err = dos
            .try_read_file_by_dos_name(&cpu, &memory, 0x2000, 0x0100)
            .expect_err("unterminated environment must stop PATH search");

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn terminate_stay_resident_preserves_foreign_environment() {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);

        let child_psp = dos.dos_mem.alloc(&mut memory, 0x0100, 0x20).unwrap();
        dos.dos_mem
            .set_owner(&mut memory, child_psp, child_psp)
            .unwrap();
        let parent_env = dos.dos_mem.alloc(&mut memory, 0x0100, 0x10).unwrap();
        memory.write_u16_seg(child_psp, PSP_ENVIRONMENT_SEG as u16, parent_env);
        dos.set_current_psp(child_psp);
        cpu.regs.set_dx(0x10);

        dos.terminate_stay_resident(&mut cpu, &mut memory);

        let env_info = dos
            .dos_mem
            .block_info_for_segment(parent_env)
            .expect("foreign environment must remain allocated");
        assert_eq!(env_info.owner_psp, 0x0100);
    }

    /// A resident child keeps the DPMI session live for its parent to re-enter.
    /// Returning to the parent restores the parent's registers, not the
    /// EXEC-time descriptor tables.
    #[test]
    fn terminate_stay_resident_keeps_the_live_descriptor_tables() {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);

        let parent_psp = dos.dos_mem.alloc(&mut memory, 0x0100, 0x20).unwrap();
        let child_psp = dos.dos_mem.alloc(&mut memory, parent_psp, 0x40).unwrap();
        let mut parent_regs = cpu.regs.clone();
        parent_regs.ss = 0x1234;
        parent_regs.set_sp(0x0100);
        dos.process_stack.push(ProcessState {
            psp_segment: parent_psp,
            registers: parent_regs,
            parent_psp: 0,
            exec_ret_ip: 0x0200,
            exec_ret_cs: 0x2345,
            exec_ret_flags: 0x0202,
            pm_parent: false,
        });
        dos.set_current_psp(child_psp);
        // EXEC points the child's PSP:0Ah at the return address.
        memory.write_u32(child_psp as usize * 16 + PSP_INT22_TERMINATE, 0x2345_0200);

        cpu.regs.gdtr.base = 0x0010_0000;
        cpu.regs.gdtr.limit = 0x0FFF;
        cpu.regs.idtr.base = 0x0010_9000;
        cpu.regs.ldtr = 0x0020;
        cpu.regs.ldt_base = 0x0010_1000;
        cpu.regs.ldt_limit = 0x7FFF;
        cpu.regs.tr = 0x0028;
        cpu.regs.tss_base = 0x0010_A000;
        cpu.regs.set_dx(0x10);

        dos.terminate_stay_resident(&mut cpu, &mut memory);

        assert_eq!(cpu.regs.cs, 0x2345);
        assert_eq!(cpu.regs.ss, 0x1234);
        assert_eq!(cpu.regs.gdtr.base, 0x0010_0000);
        assert_eq!(cpu.regs.idtr.base, 0x0010_9000);
        assert_eq!(cpu.regs.ldtr, 0x0020);
        assert_eq!(cpu.regs.ldt_base, 0x0010_1000);
        assert_eq!(cpu.regs.ldt_limit, 0x7FFF);
        assert_eq!(cpu.regs.tr, 0x0028);
        assert_eq!(cpu.regs.tss_base, 0x0010_A000);
        assert_eq!(cpu.regs.cr0 & 1, 0, "the parent resumes in real mode");
    }

    /// A resident child later terminates itself with 4C00 from its own PSP.
    /// DOS returns through PSP:0Ah to its parent and releases the resident block.
    #[test]
    fn resident_terminating_itself_returns_through_its_psp_and_frees_it() {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);

        let parent_psp = dos.dos_mem.alloc(&mut memory, 0x0100, 0x20).unwrap();
        let child_psp = dos.dos_mem.alloc(&mut memory, parent_psp, 0x40).unwrap();
        let mut parent_regs = cpu.regs.clone();
        parent_regs.ss = 0x1234;
        parent_regs.set_sp(0x0100);
        dos.process_stack.push(ProcessState {
            psp_segment: parent_psp,
            registers: parent_regs,
            parent_psp: 0,
            exec_ret_ip: 0x0300,
            exec_ret_cs: 0x2345,
            exec_ret_flags: 0x0202,
            pm_parent: false,
        });
        dos.set_current_psp(child_psp);
        let child_0a = child_psp as usize * 16 + PSP_INT22_TERMINATE;
        memory.write_u32(child_0a, 0x2345_0300);

        cpu.regs.set_dx(0x10);
        dos.terminate_stay_resident(&mut cpu, &mut memory);
        assert_eq!((cpu.regs.cs, cpu.regs.ip()), (0x2345, 0x0300));
        assert_eq!(dos.current_psp(), parent_psp);

        // The resident redirects its own termination, then ends itself.
        memory.write_u32(child_0a, 0x2345_0400);
        dos.set_current_psp(child_psp);
        let disposition = dos.terminate_process(&mut cpu, &mut memory, 0, 0).unwrap();

        assert!(matches!(disposition, Int21Disposition::NoIret));
        assert_eq!((cpu.regs.cs, cpu.regs.ip()), (0x2345, 0x0400));
        assert_eq!(cpu.regs.ss, 0x1234);
        assert_eq!(dos.current_psp(), parent_psp);
        assert!(
            dos.dos_mem
                .block_info_for_segment(child_psp)
                .is_none_or(|block| block.owner_psp == 0),
            "the resident block is released"
        );
        assert!(dos.resident_returns.is_empty());
    }

    /// MDD-BUG-CRU-01408: a TSR exit must restore INT 22h/23h/24h from the
    /// PSP, as every termination does, or a handler hooked in the released
    /// transient tail stays live in the IVT.
    #[test]
    fn terminate_stay_resident_restores_psp_vectors() {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);

        let child_psp = dos.dos_mem.alloc(&mut memory, 0x0100, 0x400).unwrap();
        dos.dos_mem
            .set_owner(&mut memory, child_psp, child_psp)
            .unwrap();
        let psp = Memory::seg_offset_to_linear(child_psp, 0);
        let saved = [0x1111_2222_u32, 0x3333_4444, 0x5555_6666];
        for (offset, vector) in [PSP_INT22_TERMINATE, PSP_INT23_CTRLC, PSP_INT24_CRITICAL]
            .into_iter()
            .zip(saved)
        {
            memory.write_u32(psp + offset, vector);
        }
        // The TSR hooks all three into its transient tail, then keeps 0x100
        // paragraphs; the hooked handlers lie beyond them.
        let tail = (u32::from(child_psp) << 16) | 0x4000;
        for vector in 0x22..=0x24_usize {
            memory.write_u32(vector * 4, tail);
        }
        dos.set_current_psp(child_psp);
        cpu.regs.set_dx(0x100);

        dos.terminate_stay_resident(&mut cpu, &mut memory);

        for (vector, expected) in (0x22..=0x24_usize).zip(saved) {
            assert_eq!(
                memory.read_u32(vector * 4),
                expected,
                "INT {vector:02X}h must be restored from the PSP"
            );
        }
    }

    #[test]
    fn terminate_stay_resident_frees_owned_environment() {
        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);

        let child_psp = dos.dos_mem.alloc(&mut memory, 0x0100, 0x20).unwrap();
        dos.dos_mem
            .set_owner(&mut memory, child_psp, child_psp)
            .unwrap();
        let child_env = dos.dos_mem.alloc(&mut memory, child_psp, 0x10).unwrap();
        memory.write_u16_seg(child_psp, PSP_ENVIRONMENT_SEG as u16, child_env);
        dos.set_current_psp(child_psp);
        cpu.regs.set_dx(0x10);

        dos.terminate_stay_resident(&mut cpu, &mut memory);

        assert!(
            dos.dos_mem
                .block_info_for_segment(child_env)
                .is_none_or(|info| info.is_free),
            "child-owned environment must be released"
        );
    }

    #[test]
    fn exec_load_only_rejects_mz_image_whose_paragraph_count_exceeds_u16() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("HUGE.EXE"), oversized_mz_image()).unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        let canary = arrange_exec_hole_and_canary(&mut dos, &mut memory, 0x10);
        configure_load_only(&mut dos, &mut cpu, &mut memory, temp_dir.path(), "HUGE.EXE");

        dos.exec_load_only(&mut cpu, &mut memory).unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 8);
        assert_eq!(dos.current_psp(), 0x0100);
        assert_canary(&memory, canary);
    }

    /// The AL=01 twin of `exec_load_exe_allocates_the_full_final_page`
    /// (MDD-BUG-ANVIL-00100).
    ///
    /// KILN-00639's page-rounded allocation sizing has three production sites.
    /// Two were pinned; this one — DOS EXEC's AL=01 "load, do not execute"
    /// overlay path — was not, so reverting it alone to the copied-length
    /// expression left all 3326 `gp64-dos` tests green.
    ///
    /// The existing AL=01 test does reach this site, but only on the REJECTION
    /// path (a paragraph count that overflows `u16`). Nothing asserted the
    /// allocation SIZE on a normal load, which is the property KILN-00639 is
    /// about — a test can execute a line and still pin nothing it does.
    ///
    /// Same arithmetic as the AL=00 twin, deliberately: a declared image of
    /// 0x60 code bytes ends 0x80 bytes into a single 512-byte page, and DOS
    /// reserves the whole page — 512 - 32 (header) = 480 bytes = 30
    /// paragraphs, plus 0x10 for the PSP. The copy-length expression reserves 6.
    #[test]
    fn exec_load_only_allocates_the_full_final_page() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join("PART.EXE"),
            mz_image_with_trailing(0x60, 0xA5, 0, 0),
        )
        .unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        configure_load_only(&mut dos, &mut cpu, &mut memory, temp_dir.path(), "PART.EXE");

        dos.exec_load_only(&mut cpu, &mut memory).unwrap();

        assert!(
            !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
            "the load must succeed, or the size assertions below are vacuous"
        );
        let psp_seg = dos.current_psp();
        assert_ne!(
            psp_seg, 0x0100,
            "precondition: AL=01 must have allocated a NEW PSP — 0x0100 is the \
             caller's, which is what the rejection path leaves behind"
        );
        // Precondition: we really went down the load-only path, which is the
        // only one that fills the EXEC parameter block's CS:IP for the caller.
        let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
        assert_eq!(
            memory.read_u16(param_addr + 0x14),
            psp_seg.wrapping_add(0x10),
            "precondition: the param block's CS must point at the load segment"
        );

        let mem_end = dos.dos_mem.block_end_segment(psp_seg).unwrap();
        assert_eq!(
            mem_end.wrapping_sub(psp_seg),
            0x10 + 30,
            "AL=01 must reserve PSP + the page-rounded image, not the copied length"
        );
        // The PSP top-of-memory word is what guest memory checks read.
        let psp_base = Memory::seg_offset_to_linear(psp_seg, 0);
        assert_eq!(
            memory.read_u16(psp_base + 2),
            mem_end,
            "PSP top-of-memory word must reflect the page-rounded allocation"
        );
    }

    #[test]
    fn exec_load_only_rejects_com_payload_larger_than_segment() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("HUGE.COM"), vec![0xCC; 0xFF01]).unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        let canary = arrange_exec_hole_and_canary(&mut dos, &mut memory, 0x1001);
        configure_load_only(&mut dos, &mut cpu, &mut memory, temp_dir.path(), "HUGE.COM");

        dos.exec_load_only(&mut cpu, &mut memory).unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 8);
        assert_eq!(dos.current_psp(), 0x0100);
        assert_canary(&memory, canary);
    }

    #[test]
    fn exec_load_only_accepts_maximum_com_payload() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join("MAX.COM"),
            vec![0x7B; MAX_COM_IMAGE_BYTES],
        )
        .unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        dos.dos_mem.init(&mut memory);
        configure_load_only(&mut dos, &mut cpu, &mut memory, temp_dir.path(), "MAX.COM");

        dos.exec_load_only(&mut cpu, &mut memory).unwrap();

        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
        let cs = memory.read_u16(param_addr + 0x14);
        assert_eq!(memory.read_u8_seg(cs, 0x0100), 0x7B);
        assert_eq!(memory.read_u8_seg(cs, 0xFFFF), 0x7B);
    }

    #[test]
    fn load_overlay_bounds_mz_by_load_module_instead_of_file_size() {
        const LOAD_SEGMENT: u16 = 0x9FF0;
        const LOAD_ADDRESS: usize = 0x9FF00;
        const AVAILABLE_BYTES: usize = 0x100;

        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join("FIT.EXE"),
            mz_overlay_image(AVAILABLE_BYTES, 0xA5),
        )
        .unwrap();
        std::fs::write(
            temp_dir.path().join("TOOBIG.EXE"),
            mz_overlay_image(AVAILABLE_BYTES + 1, 0x5A),
        )
        .unwrap();

        let mut dos = Dos::new();
        let mut cpu = Cpu::new();
        let mut memory = Memory::new();
        configure_load_only(&mut dos, &mut cpu, &mut memory, temp_dir.path(), "FIT.EXE");
        let param_addr = Memory::seg_offset_to_linear(cpu.regs.es, cpu.regs.bx());
        memory.write_u16(param_addr, LOAD_SEGMENT);
        memory.write_u16(param_addr + 2, 0);

        dos.load_overlay(&mut cpu, &mut memory).unwrap();

        assert!(!cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(memory.read_u8(LOAD_ADDRESS), 0xA5);
        assert_eq!(memory.read_u8(0x9FFFF), 0xA5);

        configure_load_only(
            &mut dos,
            &mut cpu,
            &mut memory,
            temp_dir.path(),
            "TOOBIG.EXE",
        );
        memory.write_u16(param_addr, LOAD_SEGMENT);
        memory.write_u16(param_addr + 2, 0);
        memory.write_u8(0xA0000, 0xC3);

        dos.load_overlay(&mut cpu, &mut memory).unwrap();

        assert!(cpu.regs.flags.contains(gp64_cpu::Flags::CARRY));
        assert_eq!(cpu.regs.ax(), 8);
        assert_eq!(memory.read_u8(0xA0000), 0xC3);
    }

    #[test]
    fn exec_total_paragraphs_preserves_the_u16_boundary() {
        let maximum_image_bytes = (u16::MAX as usize - 0x10) * 16;
        assert_eq!(
            exec_total_paragraphs(maximum_image_bytes, 0),
            Some(u16::MAX)
        );
        assert_eq!(exec_total_paragraphs(maximum_image_bytes + 1, 0), None);
    }

    #[test]
    fn mz_classification_requires_complete_dos_header() {
        let mut data = vec![0u8; DOS_EXEC_HEADER_LEN - 1];
        data[..2].copy_from_slice(b"MZ");
        assert!(!is_mz_executable(&data));

        data.push(0);
        assert!(is_mz_executable(&data));

        data[1] = b'X';
        assert!(!is_mz_executable(&data));
    }

    #[test]
    fn exec_load_only_short_mz_is_loaded_as_com() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let file_path = temp_dir.path().join("SHORT.EXE");

        // MS-DOS reads its complete 26-byte executable header before checking
        // the MZ signature. Every shorter nonempty file follows the COM path.
        for len in 2..26 {
            let mut data = vec![0u8; len];
            data[..2].copy_from_slice(b"MZ");
            std::fs::write(&file_path, &data).unwrap();

            let mut dos = Dos::new();
            let mut cpu = Cpu::new();
            let mut memory = Memory::new();
            dos.dos_mem.init(&mut memory);
            dos.drives.insert(
                2,
                Box::new(crate::filesystem::HostDrive::new(
                    temp_dir.path().to_path_buf(),
                )),
            );

            let filename_addr = Memory::seg_offset_to_linear(0x2000, 0x0100);
            for (index, &byte) in b"C:\\SHORT.EXE\0".iter().enumerate() {
                memory.write_u8(filename_addr + index, byte);
            }
            cpu.regs.ds = 0x2000;
            cpu.regs.set_dx(0x0100);
            cpu.regs.es = 0x3000;
            cpu.regs.set_bx(0x0200);

            dos.exec_load_only(&mut cpu, &mut memory).unwrap();

            assert!(
                !cpu.regs.flags.contains(gp64_cpu::Flags::CARRY),
                "{len}-byte input was rejected"
            );
            let param_addr = Memory::seg_offset_to_linear(0x3000, 0x0200);
            let sp = memory.read_u16(param_addr + 0x0E);
            let ss = memory.read_u16(param_addr + 0x10);
            let ip = memory.read_u16(param_addr + 0x12);
            let cs = memory.read_u16(param_addr + 0x14);
            assert_eq!(sp, 0xFFFE, "{len}-byte input did not use COM SP");
            assert_eq!(ss, cs, "{len}-byte input did not use COM SS:CS");
            assert_eq!(ip, PSP_LOAD_ADDRESS, "{len}-byte input did not use COM IP");

            let load_addr = Memory::seg_offset_to_linear(cs, PSP_LOAD_ADDRESS);
            for (index, &expected) in data.iter().enumerate() {
                assert_eq!(
                    memory.read_u8(load_addr + index),
                    expected,
                    "{len}-byte input differed at byte {index}"
                );
            }
        }
    }

    #[test]
    fn read_reader_capped_bounds_and_rejects_oversized() {
        // A reader below the cap loads fully.
        assert_eq!(
            read_reader_capped(&[1u8, 2, 3][..], 8).unwrap(),
            vec![1, 2, 3]
        );

        // Exactly at the cap is accepted.
        let at_cap = [0u8; 8];
        assert_eq!(read_reader_capped(&at_cap[..], 8).unwrap().len(), 8);

        // An oversized (here effectively infinite) reader is rejected without
        // reading past cap+1 bytes — MDD-BUG-FLUXHOMEARPA-00009. `std::io::repeat`
        // never ends, so this both proves the bound (no hang / unbounded alloc)
        // and the rejection.
        let err = read_reader_capped(std::io::repeat(0u8), 8).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
