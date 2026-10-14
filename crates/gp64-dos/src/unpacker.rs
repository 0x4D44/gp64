//! DOS executable unpacker support
//!
//! Handles packed executables (LZEXE, PKLITE, EXEPACK)

use anyhow::{bail, Result};

/// Detect the type of packer used (if any)
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(clippy::upper_case_acronyms)]
pub enum PackerType {
    None,
    LZEXE,
    PKLITE,
    EXEPACK,
    Unknown,
}

const EXEPACK_ERROR_STR: &[u8] = b"Packed file is corrupt";
const MAX_EXEPACK_OUTPUT_SIZE: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
struct ExepackHeader {
    header_offset: usize,
    header_size: usize,
    real_ip: u16,
    real_cs: u16,
    #[allow(dead_code)]
    mem_start: u16,
    exepack_size: u16,
    real_sp: u16,
    real_ss: u16,
    dest_len: u16,
    skip_len: u16, // 1 when header_size == 16
}

fn read_u16_at(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

/// Size in bytes of the MZ "image" (header + load module) as declared by the
/// MZ header page-count fields: `e_cblp` (bytes used in the last page, @0x02)
/// and `e_cp` (number of 512-byte pages including the last, @0x04).
///
/// Anything in the file *past* this offset is appended overlay / symbol-table /
/// debug data that the loader never maps into memory. A genuine EXEPACK header
/// always lives *inside* the load module, so the EXEPACK detector must not scan
/// trailing data for it (a stray "RB" byte pair in a symbol name like
/// "COLOBJRBANK" would otherwise synthesize a fake header and corrupt an
/// uncompressed MZ — see an executable with appended symbols).
///
/// Returns the offset clamped to the actual file length.
fn mz_image_end_bytes(data: &[u8]) -> usize {
    let e_cblp = read_u16_at(data, 0x02).unwrap_or(0) as usize;
    let e_cp = read_u16_at(data, 0x04).unwrap_or(0) as usize;
    let total = if e_cblp == 0 {
        e_cp * 512
    } else {
        e_cp.saturating_sub(1) * 512 + e_cblp
    };
    total.min(data.len())
}

fn write_u16_at(data: &mut [u8], offset: usize, value: u16) {
    let [lo, hi] = value.to_le_bytes();
    data[offset] = lo;
    data[offset + 1] = hi;
}

fn parse_exepack_header_at(
    data: &[u8],
    header_offset: usize,
    header_size: usize,
) -> Option<ExepackHeader> {
    if header_offset + header_size > data.len() {
        return None;
    }

    let sig = match header_size {
        16 => read_u16_at(data, header_offset + 0x0E)?,
        18 => read_u16_at(data, header_offset + 0x10)?,
        _ => return None,
    };
    if sig != 0x4252 {
        return None;
    }

    let real_ip = read_u16_at(data, header_offset)?;
    let real_cs = read_u16_at(data, header_offset + 0x02)?;
    let mem_start = read_u16_at(data, header_offset + 0x04)?;
    let exepack_size = read_u16_at(data, header_offset + 0x06)?;
    let real_sp = read_u16_at(data, header_offset + 0x08)?;
    let real_ss = read_u16_at(data, header_offset + 0x0A)?;
    let dest_len = read_u16_at(data, header_offset + 0x0C)?;
    let skip_len = if header_size == 18 {
        read_u16_at(data, header_offset + 0x0E)?
    } else {
        1
    };

    Some(ExepackHeader {
        header_offset,
        header_size,
        real_ip,
        real_cs,
        mem_start,
        exepack_size,
        real_sp,
        real_ss,
        dest_len,
        skip_len,
    })
}

fn validate_exepack_header(header: &ExepackHeader, data_len: usize, allow_zero_dest: bool) -> bool {
    // No-RB variants (header_size=14) skip error string and size checks
    if header.header_size != 14 {
        if (header.exepack_size as usize) < header.header_size + EXEPACK_ERROR_STR.len() {
            return false;
        }
        if header.header_offset + header.exepack_size as usize > data_len {
            return false;
        }
    }
    if header.header_size == 18 && header.skip_len == 0 {
        return false;
    }
    if header.dest_len == 0 && !allow_zero_dest {
        return false;
    }

    if header.dest_len != 0 {
        // CS must be within the program image, but SS can extend beyond it
        // (the stack segment may be allocated past the code+data area).
        if header.real_cs >= header.dest_len {
            return false;
        }
        let entry = (header.real_cs as usize) * 16 + header.real_ip as usize;
        if entry >= (header.dest_len as usize) * 16 {
            return false;
        }
    }

    true
}

fn find_last_subslice(data: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || data.len() < needle.len() {
        return None;
    }
    data.windows(needle.len()).rposition(|w| w == needle)
}

fn find_exepack_header(data: &[u8], trace: bool) -> Option<ExepackHeader> {
    if data.len() < 0x1C {
        return None;
    }

    // The one declared-image boundary, used by BOTH searches below and threaded
    // on into relocation parsing and reconstruction (MDD-BUG-CRUCIBLE-00595).
    // When the MZ page fields are unset/garbage (image_end < 0x20, e.g. some
    // synthetic test fixtures) fall back to the full file so we don't lose
    // detection on files that omit them.
    let image_end = mz_image_end_bytes(data);
    let fallback_cap = if image_end >= 0x20 {
        image_end
    } else {
        data.len()
    };

    let mut search_ranges = Vec::new();
    // MDD-BUG-CRUCIBLE-00595: the error-string search used to scan the WHOLE
    // physical file while only the loose "RB" fallback below was clamped, so a
    // header-shaped sequence in appended overlay/debug data could drive a false
    // EXEPACK classification. A genuine EXEPACK header lives inside the load
    // module, so both searches now stop at the declared image end.
    if let Some(err_pos) = find_last_subslice(&data[..fallback_cap], EXEPACK_ERROR_STR) {
        let start = err_pos.saturating_sub(512);
        search_ranges.push((start, err_pos, true));
        if trace {
            tracing::debug!(
                position = format_args!("0x{:X}", err_pos),
                "EXEPACK: error string found"
            );
        }
    }

    // Clamp the loose fallback "RB"-pair scan to the same boundary so a stray
    // "RB" inside an appended symbol/debug table can't fabricate a header for
    // an uncompressed MZ (an executable with appended symbols).
    let fallback_end = data
        .len()
        .saturating_sub(1)
        .min(fallback_cap.saturating_sub(1));
    let fallback_start = fallback_end.saturating_sub(0x2000);
    search_ranges.push((fallback_start, fallback_end, false));

    for (start, end, allow_zero_dest) in search_ranges {
        if start >= end {
            continue;
        }
        for rb_offset in (start..end).rev() {
            if data[rb_offset] != b'R' || data[rb_offset + 1] != b'B' {
                continue;
            }

            for (header_size, sig_off) in [(18usize, 0x10usize), (16usize, 0x0Eusize)] {
                if rb_offset < sig_off {
                    continue;
                }
                let header_offset = rb_offset - sig_off;
                let Some(header) = parse_exepack_header_at(data, header_offset, header_size) else {
                    continue;
                };
                if validate_exepack_header(&header, fallback_cap, allow_zero_dest) {
                    return Some(header);
                }
            }
        }
    }

    // Fallback: detect "no-RB" EXEPACK variant by stub signature at init_CS:init_IP.
    // This variant has a 14-byte header (real_ip, real_cs, skip, ???, dest_len, ???,
    // packed_len) followed directly by the stub code starting with 06 0E 1F 8B 0E 0C 00
    // (PUSH ES; PUSH CS; POP DS; MOV CX,[000C]; ...).
    //
    // Bounded by the same declared image as the searches above
    // (MDD-BUG-KIL-00776): initial CS:IP is guest-controlled, so an
    // uncompressed MZ could point it into appended overlay data holding a
    // header-shaped run and the stub signature.
    if data.len() >= 0x1C {
        let header_paras = read_u16_at(data, 0x08)?;
        let header_bytes = (header_paras as usize) * 16;
        let init_ip = read_u16_at(data, 0x14)?;
        let init_cs = read_u16_at(data, 0x16)?;
        let stub_file_off = header_bytes + (init_cs as usize) * 16 + init_ip as usize;
        let header_file_off = header_bytes + (init_cs as usize) * 16;
        let stub_sig: [u8; 7] = [0x06, 0x0E, 0x1F, 0x8B, 0x0E, 0x0C, 0x00];
        if stub_file_off + stub_sig.len() <= fallback_cap
            && header_file_off + 14 <= fallback_cap
            && init_ip == 0x000E
            && data[stub_file_off..stub_file_off + stub_sig.len()] == stub_sig
        {
            // Parse the 14-byte header: real_ip(2) real_cs(2) skip(2) ???(2) dest_len(2) ???(2) packed_paras(2)
            let real_ip = read_u16_at(data, header_file_off)?;
            let real_cs = read_u16_at(data, header_file_off + 2)?;
            let dest_len = read_u16_at(data, header_file_off + 8)?;
            // SS:SP from MZ header (this variant stores them there, not in the EXEPACK header)
            let real_ss = read_u16_at(data, 0x0E)?;
            let real_sp = read_u16_at(data, 0x10)?;
            // exepack_size: stub + header = file size - packed_start - packed_data_size
            let packed_data_len = header_file_off - header_bytes;
            let remaining = fallback_cap - header_file_off;
            if trace {
                tracing::debug!(
                    cs_ip = format_args!("{:04X}:{:04X}", real_cs, real_ip),
                    ss_sp = format_args!("{:04X}:{:04X}", real_ss, real_sp),
                    dest_len_paras = dest_len,
                    packed_len = packed_data_len,
                    "EXEPACK: detected no-RB variant via stub signature"
                );
            }
            return Some(ExepackHeader {
                header_offset: header_file_off,
                header_size: 14,
                real_ip,
                real_cs,
                mem_start: 0,
                exepack_size: remaining as u16,
                real_sp,
                real_ss,
                dest_len,
                skip_len: 1,
            });
        }
    }

    None
}

/// Detect packer type from EXE data
pub fn detect_packer(data: &[u8]) -> PackerType {
    if data.len() < 0x1C {
        return PackerType::None;
    }

    // Check for MZ signature
    if data[0] != b'M' || data[1] != b'Z' {
        return PackerType::None;
    }

    // Check for LZEXE signature (various versions)
    if data.len() >= 0x200 {
        // LZEXE 0.91 places "LZ91" at offset 0x1C (first byte after the
        // standard 28-byte MZ header). Also check 0x1E for older tools that
        // may place it 2 bytes later.
        if &data[0x1C..0x1C + 4] == b"LZ91" || &data[0x1E..0x1E + 4] == b"LZ91" {
            return PackerType::LZEXE;
        }

        let stub_start = &data[0x60..0x80];
        if stub_start[0] == 0x50
            && stub_start[1] == 0xB8
            && data
                .get(0xE0..0x120)
                .is_some_and(|chunk| chunk.windows(3).any(|w| w[0] == 0xD1 && w[1] == 0xED))
        {
            return PackerType::LZEXE;
        }
    }

    // Check for PKLITE signature
    if data.len() >= 0x300 {
        for i in 0x1C..0x200 {
            if &data[i..i + 6] == b"PKLITE" {
                return PackerType::PKLITE;
            }
        }
    }

    // Check for EXEPACK header
    if find_exepack_header(data, false).is_some() {
        return PackerType::EXEPACK;
    }

    PackerType::None
}

fn find_error_string(data: &[u8], start: usize, end: usize) -> Option<usize> {
    let end = end.min(data.len());
    if start >= end || end - start < EXEPACK_ERROR_STR.len() {
        return None;
    }
    data[start..end]
        .windows(EXEPACK_ERROR_STR.len())
        .position(|w| w == EXEPACK_ERROR_STR)
        .map(|pos| start + pos)
}

/// Parse the packed relocation table.
///
/// `image_end` bounds every read to the MZ-declared image
/// (MDD-BUG-CRUCIBLE-00595). Without it the guest-controlled per-section counts
/// were followed to physical EOF, so a table that "ends" only by spilling into
/// appended overlay data consumed those bytes as relocations — and the rebuilt
/// file then redeclared them as loadable image.
fn unpack_exepack_relocations(
    data: &[u8],
    reloc_offset: usize,
    image_end: usize,
) -> Result<(Vec<(u16, u16)>, usize)> {
    let limit = image_end.min(data.len());
    let mut pos = reloc_offset;
    let mut relocs = Vec::new();

    // Read a u16 only if it lies wholly inside the declared image.
    let read_bounded = |pos: usize| -> Option<u16> {
        if pos.checked_add(2)? > limit {
            return None;
        }
        read_u16_at(data, pos)
    };

    for section in 0u16..16 {
        let count = read_bounded(pos).ok_or_else(|| {
            anyhow::anyhow!(
                "EXEPACK: relocation table truncated at section {section} \
                 (crosses the declared image end at 0x{limit:X})"
            )
        })?;
        pos += 2;

        for _ in 0..count {
            let offset = read_bounded(pos).ok_or_else(|| {
                anyhow::anyhow!(
                    "EXEPACK: relocation table truncated in section {section} \
                     (crosses the declared image end at 0x{limit:X})"
                )
            })?;
            pos += 2;
            relocs.push((offset, section * 0x1000));
        }
    }

    // The rebuilt MZ stores the entry count in a u16 at 0x06, so a table that
    // cannot be represented there must be rejected rather than silently
    // truncated.
    if relocs.len() > u16::MAX as usize {
        bail!(
            "EXEPACK: {} relocation entries exceed the MZ e_crlc u16 field",
            relocs.len()
        );
    }

    Ok((relocs, pos))
}

/// Decompress EXEPACK data
fn decompress_exepack(packed: &[u8], dest_size: usize) -> Result<Vec<u8>> {
    if dest_size > MAX_EXEPACK_OUTPUT_SIZE {
        bail!("EXEPACK: output too large ({dest_size} bytes)");
    }
    let mut unpacked = vec![0u8; dest_size];
    let mut src_pos = packed.len();
    let mut dst_pos = dest_size;

    // Skip trailing 0xFF padding to find first command.
    while src_pos > 0 && packed[src_pos - 1] == 0xFF {
        src_pos -= 1;
    }

    loop {
        if src_pos == 0 {
            return Ok(unpacked);
        }

        // Read command byte
        src_pos -= 1;
        let cmd = packed[src_pos];

        // Check for end marker (low bit set = last block)
        let is_last = (cmd & 0x01) != 0;
        let cmd_type = cmd & 0xFE;

        match cmd_type {
            0xB0 => {
                // Run-length fill
                if src_pos < 3 {
                    bail!("EXEPACK: Truncated B0 command");
                }
                src_pos -= 1;
                let length_hi = packed[src_pos] as usize;
                src_pos -= 1;
                let length_lo = packed[src_pos] as usize;
                let length = (length_hi << 8) | length_lo;
                src_pos -= 1;
                let fill_byte = packed[src_pos];

                if length == 0 {
                    continue;
                }

                if length > dst_pos {
                    let actual_len = dst_pos;
                    for byte in unpacked.iter_mut().take(actual_len) {
                        *byte = fill_byte;
                    }
                    dst_pos = 0;
                } else {
                    dst_pos -= length;
                    for i in 0..length {
                        unpacked[dst_pos + i] = fill_byte;
                    }
                }
            }
            0xB2 => {
                // Literal copy
                if src_pos < 2 {
                    bail!("EXEPACK: Truncated B2 command");
                }
                src_pos -= 1;
                let length_hi = packed[src_pos] as usize;
                src_pos -= 1;
                let length_lo = packed[src_pos] as usize;
                let length = (length_hi << 8) | length_lo;

                if length == 0 {
                    continue;
                }

                if length > src_pos {
                    bail!("EXEPACK: Copy length {length} exceeds source position {src_pos}");
                }

                if length > dst_pos {
                    let overflow = length - dst_pos;
                    let actual_len = dst_pos;

                    src_pos -= length;

                    if actual_len > 0 {
                        unpacked[0..actual_len]
                            .copy_from_slice(&packed[src_pos + overflow..src_pos + length]);
                    }
                    dst_pos = 0;
                } else {
                    src_pos -= length;
                    dst_pos -= length;
                    unpacked[dst_pos..dst_pos + length]
                        .copy_from_slice(&packed[src_pos..src_pos + length]);
                }
            }
            _ => bail!("EXEPACK: Unknown command byte 0x{cmd:02X}"),
        }

        if is_last {
            break;
        }
    }

    // Copy the uncompressed prefix: EXEPACK doesn't RLE-compress the entire
    // image. The packed data is [uncompressed_prefix][RLE_compressed_data].
    // The EXEPACK stub copies everything to the top of the allocation then
    // decompresses backward; the prefix naturally lands at position 0. In our
    // static decompressor we need to copy it explicitly.
    if dst_pos > 0 && src_pos > 0 {
        let copy_len = dst_pos.min(src_pos);
        unpacked[..copy_len].copy_from_slice(&packed[..copy_len]);
    }

    Ok(unpacked)
}

/// Unpack EXEPACK compressed executable
pub fn unpack_exepack(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 0x1C {
        bail!("File too small to be EXEPACK packed");
    }

    let trace = tracing::enabled!(target: "gp64::unpacker::exepack", tracing::Level::DEBUG);
    if trace {
        tracing::debug!("EXEPACK: Attempting to unpack executable");
    }

    // Parse original MZ header
    let header_paragraphs = u16::from_le_bytes([data[8], data[9]]);
    let header_size = (header_paragraphs as usize) * 16;

    if trace {
        tracing::debug!(
            size = header_size,
            paragraphs = header_paragraphs,
            "EXEPACK: Header size"
        );
    }

    // Locate and parse EXEPACK header
    let header = match find_exepack_header(data, trace) {
        Some(h) => h,
        None => {
            if trace {
                tracing::warn!(
                    "EXEPACK: Could not locate header - returning as-is for stub execution"
                );
            }
            return Ok(data.to_vec());
        }
    };

    if trace {
        tracing::debug!(
            offset = format_args!("0x{:X}", header.header_offset),
            size = header.header_size,
            "EXEPACK: Header location"
        );
        tracing::debug!(
            cs_ip = format_args!("{:04X}:{:04X}", header.real_cs, header.real_ip),
            "EXEPACK: Real entry point"
        );
        tracing::debug!(
            ss_sp = format_args!("{:04X}:{:04X}", header.real_ss, header.real_sp),
            "EXEPACK: Real stack"
        );
        tracing::debug!(
            paragraphs = header.dest_len,
            bytes = (header.dest_len as usize) * 16,
            "EXEPACK: Dest length"
        );
        tracing::debug!(size = header.exepack_size, "EXEPACK: Block size");
        if header.header_size == 18 {
            tracing::debug!(paragraphs = header.skip_len, "EXEPACK: Skip length");
        }
    }

    if header.dest_len == 0 {
        if trace {
            tracing::warn!("EXEPACK: dest_len is 0, falling back to stub execution");
        }
        return Ok(data.to_vec());
    }

    // Packed data starts right after the MZ header and ends before the EXEPACK header.
    let packed_start = header_size;
    let mut packed_end = header.header_offset;

    // 18-byte headers include skip_len paragraphs between packed data and header (+1).
    if header.header_size == 18 && header.skip_len > 1 {
        let skip_bytes = (header.skip_len as usize - 1) * 16;
        packed_end = packed_end.saturating_sub(skip_bytes);
    }

    if packed_end <= packed_start {
        bail!("EXEPACK: Invalid packed data boundaries");
    }

    let packed_data = &data[packed_start..packed_end];
    let dest_size = (header.dest_len as usize) * 16;

    if trace {
        tracing::debug!(
            bytes = packed_data.len(),
            start = format_args!("0x{:X}", packed_start),
            end = format_args!("0x{:X}", packed_end),
            "EXEPACK: Packed data range"
        );
    }

    // Use static decompression by default. While the original EXEPACK stub can
    // run in the emulator, many EXEPACK variants lack the 0xFF padding that the
    // stub expects, causing them to fail with "Packed file is corrupt".
    //
    // Static decompression is more reliable across EXEPACK variants.
    // KILN-00086: the force-stub env override was retired; static decompression is now
    // the only path.

    if trace {
        tracing::debug!("EXEPACK: Attempting static decompression");
    }
    let unpacked_image = decompress_exepack(packed_data, dest_size)?;
    if trace {
        tracing::debug!(bytes = unpacked_image.len(), "EXEPACK: Decompressed");
    }

    // Quick sanity check: entry point bytes should not be all 0x00/0xFF.
    let entry_off = (header.real_cs as usize) * 16 + (header.real_ip as usize);
    if entry_off + 8 <= unpacked_image.len() {
        let window = &unpacked_image[entry_off..entry_off + 8];
        if window.iter().all(|&b| b == 0xFF) {
            if trace {
                tracing::warn!("EXEPACK: Static decompression sanity check failed at entry point (all 0xFF), falling back to stub execution");
            }
            return Ok(data.to_vec());
        }
    }

    // The one declared-image boundary for this file. Everything the unpacker
    // parses lives inside it; anything past it is appended overlay/debug data
    // that must be preserved byte-for-byte but never parsed and never counted
    // as loadable image (MDD-BUG-CRUCIBLE-00595). Files with unset/garbage MZ
    // page fields fall back to the full length, matching the detector.
    let declared_image_end = {
        let end = mz_image_end_bytes(data);
        if end >= 0x20 {
            end
        } else {
            data.len()
        }
    };

    // Locate the packed relocation table via the error string.
    // No-RB variants (header_size=14) may not have an error string or packed reloc table.
    let (relocs, reloc_end) = if header.header_size == 14 {
        // No-RB variant: no packed relocation table. Use MZ header's relocation count.
        let mz_relocs = read_u16_at(data, 0x06).unwrap_or(0);
        if mz_relocs == 0 {
            (Vec::new(), header.header_offset)
        } else {
            // Try to find relocations if the error string exists
            let exepack_block_end = header
                .header_offset
                .saturating_add(header.exepack_size as usize)
                .min(data.len());
            match find_error_string(data, header.header_offset, exepack_block_end) {
                Some(err_pos) => {
                    let reloc_offset = err_pos + EXEPACK_ERROR_STR.len();
                    unpack_exepack_relocations(data, reloc_offset, declared_image_end)?
                }
                None => (Vec::new(), header.header_offset),
            }
        }
    } else {
        let exepack_block_end = header
            .header_offset
            .saturating_add(header.exepack_size as usize)
            .min(data.len());
        let err_pos = find_error_string(data, header.header_offset, exepack_block_end)
            .ok_or_else(|| anyhow::anyhow!("EXEPACK: error string not found"))?;
        let reloc_offset = err_pos + EXEPACK_ERROR_STR.len();
        unpack_exepack_relocations(data, reloc_offset, declared_image_end)?
    };

    if trace {
        tracing::debug!(
            entries = relocs.len(),
            table_end = format_args!("0x{:X}", reloc_end),
            "EXEPACK: Relocations"
        );
    }

    let trailing_data = if reloc_end < data.len() {
        data[reloc_end..].to_vec()
    } else {
        Vec::new()
    };

    // Reconstruct executable header + relocation table.
    let reloc_table_size = relocs.len() * 4;
    let header_bytes = 0x1C + reloc_table_size;
    let header_bytes_aligned = header_bytes.div_ceil(16) * 16;
    let header_paragraphs_out = (header_bytes_aligned / 16) as u16;

    let mut new_exe = vec![0u8; header_bytes_aligned];

    // MZ signature
    new_exe[0] = b'M';
    new_exe[1] = b'Z';

    // Header size and relocation metadata
    write_u16_at(&mut new_exe, 0x06, relocs.len() as u16); // e_crlc
    write_u16_at(&mut new_exe, 0x08, header_paragraphs_out); // e_cparhdr
    write_u16_at(&mut new_exe, 0x18, 0x001C); // e_lfarlc
    write_u16_at(&mut new_exe, 0x1A, 0x0000); // e_ovno

    // Adjust min_alloc: the packed header's min_alloc includes decompression buffer
    // space (dest_len - packed_image_paras) that the EXEPACK stub needs at runtime.
    // After static unpacking, the decompressed image is already in the file, so
    // subtract the decompression overhead to get the program's actual BSS needs.
    let packed_min_alloc = read_u16_at(data, 0x0A).unwrap_or(0);
    let max_alloc = read_u16_at(data, 0x0C).unwrap_or(0xFFFF);
    // The declared image, not the file: appended overlay or debug data is
    // not loaded, and a file over 1 MB wrapped the old `as u16`
    // (MDD-BUG-CRU-01409).
    let packed_image_paras =
        u16::try_from(declared_image_end.saturating_sub(header_size).div_ceil(16))
            .unwrap_or(u16::MAX);
    let decompression_overhead = header.dest_len.saturating_sub(packed_image_paras);
    let min_alloc = packed_min_alloc.saturating_sub(decompression_overhead);
    write_u16_at(&mut new_exe, 0x0A, min_alloc);
    write_u16_at(&mut new_exe, 0x0C, max_alloc);

    // SS:SP and CS:IP from EXEPACK header.
    write_u16_at(&mut new_exe, 0x0E, header.real_ss);
    write_u16_at(&mut new_exe, 0x10, header.real_sp);
    write_u16_at(&mut new_exe, 0x14, header.real_ip);
    write_u16_at(&mut new_exe, 0x16, header.real_cs);

    // Relocation table
    let mut rel_pos = 0x1C;
    for (offset, segment) in &relocs {
        write_u16_at(&mut new_exe, rel_pos, *offset);
        write_u16_at(&mut new_exe, rel_pos + 2, *segment);
        rel_pos += 4;
    }

    // Append the unpacked image, then any trailing data.
    new_exe.extend_from_slice(&unpacked_image);

    // MDD-BUG-CRUCIBLE-00595: the MZ page fields describe the LOADABLE IMAGE,
    // and must be computed BEFORE the trailing bytes are appended. Measuring
    // the finished file instead re-declared preserved overlay/debug data as
    // loadable image — the exact boundary violation this unpacker exists to
    // respect. Trailing bytes are still preserved physically; they are simply
    // not counted.
    let image_size = new_exe.len();
    if !trailing_data.is_empty() {
        new_exe.extend_from_slice(&trailing_data);
    }

    // Update the declared image size in the header.
    let blocks = image_size.div_ceil(512);
    let last_block = image_size % 512;

    // e_cp is a u16: an image needing more pages than that cannot be described
    // by an MZ header, so reject it rather than wrap.
    if blocks > u16::MAX as usize {
        bail!("EXEPACK: unpacked image needs {blocks} pages, more than MZ e_cp can express");
    }
    if header_paragraphs_out as usize * 16 != header_bytes_aligned {
        bail!("EXEPACK: rebuilt header is too large for the MZ e_cparhdr u16 field");
    }

    write_u16_at(&mut new_exe, 0x02, last_block as u16);
    write_u16_at(&mut new_exe, 0x04, blocks as u16);

    if trace {
        tracing::debug!(
            bytes = new_exe.len(),
            declared_image = image_size,
            trailing = trailing_data.len(),
            "EXEPACK: Successfully unpacked executable"
        );
    }
    Ok(new_exe)
}

/// Extract the original entry point (CS:IP) from an EXEPACK executable, if present.
///
/// Values are the *relative* CS:IP from the packed file (i.e. before the loader adds `load_segment`).
/// This is useful for heuristics like disabling JIT until control transfers to the unpacked program.
pub fn exepack_real_entry(data: &[u8]) -> Option<(u16, u16)> {
    let header = find_exepack_header(data, false)?;
    if header.dest_len == 0 {
        return None;
    }
    Some((header.real_cs, header.real_ip))
}

/// Unpack an LZEXE compressed executable
pub fn unpack_lzexe(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 0x1C {
        bail!("File too small to be LZEXE packed");
    }

    tracing::info!("LZEXE packed executable detected - emulator will execute unpacker stub");

    bail!("LZEXE unpacking not yet implemented - stub execution required")
}

/// Unpack an executable if it's packed
pub fn unpack_if_needed(data: &[u8]) -> Result<Vec<u8>> {
    let packer = detect_packer(data);

    match packer {
        PackerType::None => {
            // Not packed, return as-is
            Ok(data.to_vec())
        }
        PackerType::LZEXE => {
            tracing::warn!(
                "LZEXE packed executable detected - emulator will attempt to execute unpacker stub"
            );
            // Return as-is and let the CPU execute the stub
            Ok(data.to_vec())
        }
        PackerType::PKLITE => {
            tracing::warn!("PKLITE packed executable detected - emulator will attempt to execute unpacker stub");
            Ok(data.to_vec())
        }
        PackerType::EXEPACK => {
            // Try to unpack EXEPACK
            unpack_exepack(data)
        }
        PackerType::Unknown => {
            tracing::warn!(
                "Unknown packer detected - emulator may not run this executable correctly"
            );
            Ok(data.to_vec())
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn sample_header() -> ExepackHeader {
        ExepackHeader {
            header_offset: 0,
            header_size: 16,
            real_ip: 0,
            real_cs: 0,
            mem_start: 0,
            exepack_size: (16 + EXEPACK_ERROR_STR.len()) as u16,
            real_sp: 0,
            real_ss: 0,
            dest_len: 0x10,
            skip_len: 1,
        }
    }

    fn trace_subscriber(filter: &str) -> tracing::subscriber::DefaultGuard {
        use tracing_subscriber::EnvFilter;
        let sub = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new(filter))
            .with_writer(std::io::sink)
            .finish();
        tracing::subscriber::set_default(sub)
    }

    fn make_mz_image(size: usize, header_paragraphs: u16) -> Vec<u8> {
        let mut data = vec![0u8; size];
        data[0] = b'M';
        data[1] = b'Z';
        write_u16_at(&mut data, 0x08, header_paragraphs);
        data
    }

    fn write_exepack_header(
        data: &mut [u8],
        header_offset: usize,
        header_size: usize,
        exepack_size: u16,
        dest_len: u16,
        real_cs: u16,
        real_ip: u16,
        real_ss: u16,
        real_sp: u16,
        skip_len: u16,
    ) {
        write_u16_at(data, header_offset, real_ip);
        write_u16_at(data, header_offset + 0x02, real_cs);
        write_u16_at(data, header_offset + 0x04, 0x0000);
        write_u16_at(data, header_offset + 0x06, exepack_size);
        write_u16_at(data, header_offset + 0x08, real_sp);
        write_u16_at(data, header_offset + 0x0A, real_ss);
        write_u16_at(data, header_offset + 0x0C, dest_len);
        match header_size {
            16 => write_u16_at(data, header_offset + 0x0E, 0x4252),
            18 => {
                write_u16_at(data, header_offset + 0x0E, skip_len);
                write_u16_at(data, header_offset + 0x10, 0x4252);
            }
            _ => panic!("unsupported EXEPACK header size for test"),
        }
    }

    fn assert_err_contains<T: std::fmt::Debug>(result: Result<T>, expected: &str) {
        let err = result.expect_err("expected error");
        let msg = err.to_string();
        assert!(
            msg.contains(expected),
            "expected error containing '{expected}', got '{msg}'"
        );
    }

    fn make_static_exepack_fixture(
        fill_byte: u8,
        dest_len_paragraphs: u16,
        real_ip: u16,
        trailing: &[u8],
    ) -> Vec<u8> {
        let header_offset = 0x14usize;
        let header_size = 16usize;
        let dest_size = (dest_len_paragraphs as usize) * 16;
        let reloc_table_bytes = 16 * 2;
        let exepack_size = (header_size + EXEPACK_ERROR_STR.len() + reloc_table_bytes) as u16;
        let err_pos = header_offset + header_size;
        let reloc_offset = err_pos + EXEPACK_ERROR_STR.len();
        let reloc_end = reloc_offset + reloc_table_bytes;

        let mut data = make_mz_image(reloc_end, 1);
        write_exepack_header(
            &mut data,
            header_offset,
            header_size,
            exepack_size,
            dest_len_paragraphs,
            0,
            real_ip,
            0,
            0,
            1,
        );

        let cmd = [
            fill_byte,
            (dest_size & 0xFF) as u8,
            ((dest_size >> 8) & 0xFF) as u8,
            0xB1,
        ];
        data[0x10..0x14].copy_from_slice(&cmd);
        data[err_pos..err_pos + EXEPACK_ERROR_STR.len()].copy_from_slice(EXEPACK_ERROR_STR);

        if !trailing.is_empty() {
            data.extend_from_slice(trailing);
        }

        data
    }

    #[test]
    fn test_detect_packer_rejects_short_and_non_mz() {
        assert_eq!(detect_packer(&[]), PackerType::None);

        let mut data = vec![0u8; 0x1C];
        data[0] = b'Z';
        data[1] = b'M';
        assert_eq!(detect_packer(&data), PackerType::None);
    }

    #[test]
    fn test_detect_packer_pklite_signature() {
        let mut data = vec![0u8; 0x300];
        data[0] = b'M';
        data[1] = b'Z';
        data[0x1C..0x22].copy_from_slice(b"PKLITE");
        assert_eq!(detect_packer(&data), PackerType::PKLITE);
    }

    #[test]
    fn test_detect_packer_lz91_signature() {
        let mut data = vec![0u8; 0x200];
        data[0] = b'M';
        data[1] = b'Z';
        data[0x1E..0x22].copy_from_slice(b"LZ91");
        assert_eq!(detect_packer(&data), PackerType::LZEXE);
    }

    #[test]
    fn test_detect_packer_mz_without_signatures_returns_none() {
        let mut data = vec![0u8; 0x300];
        data[0] = b'M';
        data[1] = b'Z';
        assert_eq!(detect_packer(&data), PackerType::None);
    }

    #[test]
    fn test_detect_packer_mz_bad_second_signature_byte() {
        let mut data = vec![0u8; 0x300];
        data[0] = b'M';
        data[1] = b'X';
        assert_eq!(detect_packer(&data), PackerType::None);
    }

    #[test]
    fn test_detect_packer_between_1c_and_200_bytes() {
        let mut data = vec![0u8; 0x100];
        data[0] = b'M';
        data[1] = b'Z';
        assert_eq!(detect_packer(&data), PackerType::None);
    }

    #[test]
    fn test_detect_packer_lzexe_stub_prefix_without_pattern_returns_none() {
        let mut data = vec![0u8; 0x200];
        data[0] = b'M';
        data[1] = b'Z';
        data[0x60] = 0x50;
        data[0x61] = 0x00;
        assert_eq!(detect_packer(&data), PackerType::None);
    }

    #[test]
    fn test_detect_packer_lzexe_stub_without_shift_pattern_returns_none() {
        let mut data = vec![0u8; 0x200];
        data[0] = b'M';
        data[1] = b'Z';
        data[0x60] = 0x50;
        data[0x61] = 0xB8;
        assert_eq!(detect_packer(&data), PackerType::None);
    }

    #[test]
    fn test_find_exepack_header_accepts_valid_header() {
        let mut data = vec![0u8; 0x200];
        let header_offset = 0x80;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len()) as u16;

        write_u16_at(&mut data, header_offset, 0x0000);
        write_u16_at(&mut data, header_offset + 0x02, 0x0001);
        write_u16_at(&mut data, header_offset + 0x04, 0x0000);
        write_u16_at(&mut data, header_offset + 0x06, exepack_size);
        write_u16_at(&mut data, header_offset + 0x08, 0x0000);
        write_u16_at(&mut data, header_offset + 0x0A, 0x0001);
        write_u16_at(&mut data, header_offset + 0x0C, 0x0010);
        write_u16_at(&mut data, header_offset + 0x0E, 0x4252); // "RB"

        let header = find_exepack_header(&data, false).expect("header should parse");
        assert_eq!(header.header_offset, header_offset);
        assert_eq!(header.header_size, 16);
        assert_eq!(header.real_cs, 0x0001);
        assert_eq!(header.dest_len, 0x0010);
    }

    #[test]
    fn test_detect_packer_exepack() {
        let mut data = vec![0u8; 0x200];
        data[0] = b'M';
        data[1] = b'Z';

        let header_offset = 0x60;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len()) as u16;

        write_u16_at(&mut data, header_offset, 0x0000);
        write_u16_at(&mut data, header_offset + 0x02, 0x0001);
        write_u16_at(&mut data, header_offset + 0x04, 0x0000);
        write_u16_at(&mut data, header_offset + 0x06, exepack_size);
        write_u16_at(&mut data, header_offset + 0x08, 0x0000);
        write_u16_at(&mut data, header_offset + 0x0A, 0x0001);
        write_u16_at(&mut data, header_offset + 0x0C, 0x0010);
        write_u16_at(&mut data, header_offset + 0x0E, 0x4252); // "RB"

        assert_eq!(detect_packer(&data), PackerType::EXEPACK);
    }

    #[test]
    fn test_detect_packer_rejects_rb_in_trailing_symbols() {
        // Regression: an uncompressed MZ whose appended symbol/debug table happens
        // to contain an "RB" byte pair must NOT be mis-detected as EXEPACK.
        // (an executable with appended symbols: the symbol name "COLOBJRBANK"
        // supplied an "RB" that synthesized a fake header and corrupted the load.)
        // The fallback "RB" scan must stay within the MZ-declared load module.
        let mut data = vec![0u8; 0x1000];
        data[0] = b'M';
        data[1] = b'Z';
        // Declare a small load image: e_cblp=0 (full last page), e_cp=1 => 512 bytes.
        write_u16_at(&mut data, 0x02, 0x0000);
        write_u16_at(&mut data, 0x04, 0x0001);
        assert_eq!(mz_image_end_bytes(&data), 0x200);

        // Plant a header that WOULD validate, but in trailing data past image end
        // (offset 0x800, well beyond the 0x200 load module).
        let header_offset = 0x800;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len()) as u16;
        write_u16_at(&mut data, header_offset, 0x0000); // real_ip
        write_u16_at(&mut data, header_offset + 0x02, 0x0001); // real_cs
        write_u16_at(&mut data, header_offset + 0x04, 0x0000);
        write_u16_at(&mut data, header_offset + 0x06, exepack_size);
        write_u16_at(&mut data, header_offset + 0x08, 0x0000);
        write_u16_at(&mut data, header_offset + 0x0A, 0x0001);
        write_u16_at(&mut data, header_offset + 0x0C, 0x0010); // dest_len
        write_u16_at(&mut data, header_offset + 0x0E, 0x4252); // "RB"

        // Pre-fix this returned EXEPACK (false positive); post-fix it must not.
        assert!(find_exepack_header(&data, false).is_none());
        assert_ne!(detect_packer(&data), PackerType::EXEPACK);
    }

    #[test]
    fn test_detect_packer_rejects_exepack_block_past_declared_image() {
        // pair inside the MZ load image whose preceding bytes resemble an
        // EXEPACK header. The fabricated block extends beyond the declared MZ
        // image into the appended executable, so it cannot be genuine.
        let mut data = vec![0u8; 0x1000];
        data[0] = b'M';
        data[1] = b'Z';
        // Declare a 512-byte MZ image followed by an appended payload.
        write_u16_at(&mut data, 0x02, 0x0000);
        write_u16_at(&mut data, 0x04, 0x0001);
        assert_eq!(mz_image_end_bytes(&data), 0x200);

        // Keep the signature inside the image, but make the claimed EXEPACK
        // block cross its 0x200 boundary while remaining inside the file.
        let header_offset = 0x1E0;
        write_exepack_header(&mut data, header_offset, 18, 0x40, 0x10, 0, 0, 0, 0, 1);

        assert!(find_exepack_header(&data, false).is_none());
        assert_ne!(detect_packer(&data), PackerType::EXEPACK);
    }

    // ------------------------------------------------------------------
    // MDD-BUG-CRUCIBLE-00595 — nothing past the MZ-declared image may be
    // parsed as EXEPACK structure or counted as loadable image.
    // ------------------------------------------------------------------

    #[test]
    fn exepack_error_string_in_trailing_data_does_not_classify() {
        // The sibling test above covers the loose "RB" fallback scan, which was
        // already clamped. THIS covers the error-string search, which was not:
        // it scanned the whole physical file, so a header-shaped run in
        // appended overlay/debug data could drive a false classification.
        let mut data = vec![0u8; 0x1000];
        data[0] = b'M';
        data[1] = b'Z';
        // Declare a 512-byte load module; everything past 0x200 is overlay.
        write_u16_at(&mut data, 0x02, 0x0000);
        write_u16_at(&mut data, 0x04, 0x0001);
        assert_eq!(mz_image_end_bytes(&data), 0x200);

        // Plant the EXEPACK error string in the overlay, with a header that
        // would validate, positioned so the pre-fix 512-byte look-back window
        // lands on it.
        let header_offset = 0x800;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len()) as u16;
        write_u16_at(&mut data, header_offset, 0x0000);
        write_u16_at(&mut data, header_offset + 0x02, 0x0001);
        write_u16_at(&mut data, header_offset + 0x04, 0x0000);
        write_u16_at(&mut data, header_offset + 0x06, exepack_size);
        write_u16_at(&mut data, header_offset + 0x08, 0x0000);
        write_u16_at(&mut data, header_offset + 0x0A, 0x0001);
        write_u16_at(&mut data, header_offset + 0x0C, 0x0010);
        write_u16_at(&mut data, header_offset + 0x0E, 0x4252); // "RB"
        let err_at = header_offset + 0x10;
        data[err_at..err_at + EXEPACK_ERROR_STR.len()].copy_from_slice(EXEPACK_ERROR_STR);

        assert!(
            find_exepack_header(&data, false).is_none(),
            "an error string in trailing data must not synthesize a header"
        );
        assert_ne!(detect_packer(&data), PackerType::EXEPACK);
        // And the file must survive untouched.
        assert_eq!(unpack_exepack(&data).expect("stub fallback"), data);
    }

    #[test]
    fn exepack_no_rb_signature_in_trailing_data_does_not_classify() {
        // MDD-BUG-KIL-00776: the no-RB branch validated its header and stub
        // signature against the whole physical file, so initial CS:IP aimed
        // into appended overlay data could classify an uncompressed MZ.
        const STUB_SIG: [u8; 7] = [0x06, 0x0E, 0x1F, 0x8B, 0x0E, 0x0C, 0x00];
        let build = |init_cs: u16| {
            let mut data = vec![0u8; 0x1000];
            data[0] = b'M';
            data[1] = b'Z';
            // A 512-byte load module with a two-paragraph MZ header.
            write_u16_at(&mut data, 0x02, 0x0000);
            write_u16_at(&mut data, 0x04, 0x0001);
            write_u16_at(&mut data, 0x08, 0x0002);
            write_u16_at(&mut data, 0x14, 0x000E);
            write_u16_at(&mut data, 0x16, init_cs);
            let header = 0x20 + usize::from(init_cs) * 16;
            write_u16_at(&mut data, header + 0x08, 0x0020); // dest_len
            data[header + 0x0E..header + 0x0E + STUB_SIG.len()].copy_from_slice(&STUB_SIG);
            data
        };

        // Header at 0x420, stub at 0x42E: past the 0x200 image end.
        let overlay = build(0x40);
        assert_eq!(mz_image_end_bytes(&overlay), 0x200);
        assert!(
            find_exepack_header(&overlay, false).is_none(),
            "a no-RB sequence in trailing data must not synthesize a header"
        );
        assert_ne!(detect_packer(&overlay), PackerType::EXEPACK);
        assert_eq!(unpack_exepack(&overlay).expect("stub fallback"), overlay);

        // Control: the same sequence inside the image is still detected.
        let inside = build(0x10);
        let header = find_exepack_header(&inside, false).expect("no-RB header inside the image");
        assert_eq!(header.header_offset, 0x120);
        assert_eq!(usize::from(header.exepack_size), 0x200 - 0x120);
    }

    #[test]
    fn exepack_relocations_may_not_spill_past_the_declared_image() {
        // A section count that can only be satisfied by reading past the
        // declared image end must be rejected, not silently satisfied from
        // appended overlay bytes.
        let mut data = vec![0u8; 0x40];
        // Section 0 claims 8 entries, but the declared image ends after 6 bytes,
        // so only two of them are inside it.
        write_u16_at(&mut data, 0, 8);
        for entry in 0..8 {
            write_u16_at(&mut data, 2 + entry * 2, 0x1000 + entry as u16);
        }

        let declared_image_end = 6;
        let err = unpack_exepack_relocations(&data, 0, declared_image_end)
            .expect_err("the table crosses the declared image end");
        assert!(
            err.to_string().contains("declared image end"),
            "unexpected error: {err}"
        );

        // With the whole buffer declared as image, the same table parses.
        assert!(unpack_exepack_relocations(&data, 0, data.len()).is_ok());
    }

    #[test]
    fn exepack_rejects_a_relocation_count_the_mz_header_cannot_express() {
        // e_crlc is a u16, so more than 65,535 entries cannot be written back.
        // 16 sections x 4096 entries = 65,536 — one past the limit.
        const PER_SECTION: usize = 4096;
        let mut data = vec![0u8; 16 * (2 + PER_SECTION * 2)];
        let mut pos = 0;
        for _ in 0..16 {
            write_u16_at(&mut data, pos, PER_SECTION as u16);
            pos += 2 + PER_SECTION * 2;
        }

        let err = unpack_exepack_relocations(&data, 0, data.len())
            .expect_err("65,536 entries must not be silently truncated to 0");
        assert!(
            err.to_string().contains("e_crlc"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_decompress_exepack_fill() {
        let packed = vec![0xAA, 0x04, 0x00, 0xB1];
        let unpacked = decompress_exepack(&packed, 4).expect("fill should succeed");
        assert_eq!(unpacked, vec![0xAA; 4]);
    }

    #[test]
    fn test_decompress_exepack_copy() {
        let packed = vec![0x10, 0x20, 0x30, 0x40, 0x04, 0x00, 0xB3];
        let unpacked = decompress_exepack(&packed, 4).expect("copy should succeed");
        assert_eq!(unpacked, vec![0x10, 0x20, 0x30, 0x40]);
    }

    #[test]
    fn test_decompress_exepack_unknown_command() {
        let packed = vec![0x90];
        let result = decompress_exepack(&packed, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_unpack_exepack_too_small() {
        let data = vec![0u8; 0x1B];
        let result = unpack_exepack(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_exepack_header_rejects_invalid_size() {
        let data = vec![0u8; 0x20];
        assert!(parse_exepack_header_at(&data, 0, 17).is_none());
    }

    #[test]
    fn test_parse_exepack_header_rejects_signature() {
        let data = vec![0u8; 0x20];
        assert!(parse_exepack_header_at(&data, 0, 16).is_none());
    }

    #[test]
    fn test_parse_exepack_header_accepts_18_byte_header() {
        let mut data = vec![0u8; 0x80];
        write_exepack_header(
            &mut data,
            0x20,
            18,
            (18 + EXEPACK_ERROR_STR.len()) as u16,
            0x0030,
            0x0002,
            0x0003,
            0x0004,
            0x0005,
            2,
        );

        let header = parse_exepack_header_at(&data, 0x20, 18).expect("header should parse");
        assert_eq!(header.header_size, 18);
        assert_eq!(header.skip_len, 2);
        assert_eq!(header.real_cs, 0x0002);
        assert_eq!(header.real_ip, 0x0003);
    }

    #[test]
    fn test_parse_exepack_header_rejects_truncated_data() {
        let data = vec![0u8; 0x2F];
        assert!(parse_exepack_header_at(&data, 0x20, 16).is_none());
    }

    #[test]
    fn test_parse_exepack_header_rejects_18_byte_signature() {
        let mut data = vec![0u8; 0x40];
        write_u16_at(&mut data, 0x0E, 1);
        assert!(parse_exepack_header_at(&data, 0, 18).is_none());
    }

    #[test]
    fn test_validate_exepack_header_dest_len_zero_allowed() {
        let mut header = sample_header();
        header.dest_len = 0;
        assert!(!validate_exepack_header(&header, 0x200, false));
        assert!(validate_exepack_header(&header, 0x200, true));
    }

    #[test]
    fn test_validate_exepack_header_skip_len_zero_invalid() {
        let mut header = sample_header();
        header.header_size = 18;
        header.exepack_size = (18 + EXEPACK_ERROR_STR.len()) as u16;
        header.skip_len = 0;
        assert!(!validate_exepack_header(&header, 0x200, true));
    }

    #[test]
    fn test_validate_exepack_header_skip_len_nonzero_valid_for_18_byte_header() {
        let mut header = sample_header();
        header.header_size = 18;
        header.exepack_size = (18 + EXEPACK_ERROR_STR.len()) as u16;
        header.skip_len = 1;
        assert!(validate_exepack_header(&header, 0x200, true));
    }

    #[test]
    fn test_validate_exepack_header_entry_out_of_range() {
        let mut header = sample_header();
        header.dest_len = 1;
        header.real_cs = 2;
        assert!(!validate_exepack_header(&header, 0x200, true));
    }

    #[test]
    fn test_validate_exepack_header_rejects_small_exepack_block() {
        let mut header = sample_header();
        header.exepack_size = (header.header_size + EXEPACK_ERROR_STR.len() - 1) as u16;
        assert!(!validate_exepack_header(&header, 0x200, true));
    }

    #[test]
    fn test_validate_exepack_header_rejects_block_beyond_data_len() {
        let mut header = sample_header();
        header.header_offset = 0x1A0;
        header.exepack_size = 0x80;
        assert!(!validate_exepack_header(&header, 0x200, true));
    }

    #[test]
    fn test_validate_exepack_header_allows_real_ss_beyond_dest_len() {
        // SS can extend beyond dest_len (stack segment past code+data area)
        let mut header = sample_header();
        header.dest_len = 2;
        header.real_ss = 3;
        assert!(validate_exepack_header(&header, 0x200, true));
    }

    #[test]
    fn test_validate_exepack_header_rejects_entry_offset_out_of_range() {
        let mut header = sample_header();
        header.dest_len = 1;
        header.real_cs = 0;
        header.real_ip = 0x20;
        assert!(!validate_exepack_header(&header, 0x200, true));
    }

    #[test]
    fn test_find_last_subslice_empty() {
        assert_eq!(find_last_subslice(&[1, 2, 3], &[]), None);
    }

    #[test]
    fn test_find_last_subslice_haystack_shorter_than_needle() {
        assert_eq!(find_last_subslice(&[1, 2], &[1, 2, 3]), None);
    }

    #[test]
    fn test_find_exepack_header_rejects_short_input() {
        assert!(find_exepack_header(&[0u8; 0x10], false).is_none());
    }

    #[test]
    fn test_find_exepack_header_allows_zero_dest_when_error_string_present() {
        let mut data = vec![0u8; 0x200];
        let header_offset = 0x80;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len() + 0x10) as u16;
        write_exepack_header(&mut data, header_offset, 16, exepack_size, 0, 0, 0, 0, 0, 1);
        let err_pos = header_offset + 0x10;
        data[err_pos..err_pos + EXEPACK_ERROR_STR.len()].copy_from_slice(EXEPACK_ERROR_STR);

        let header = find_exepack_header(&data, false).expect("header should parse");
        assert_eq!(header.dest_len, 0);
    }

    #[test]
    fn test_find_exepack_header_rejects_zero_dest_without_error_string() {
        let mut data = vec![0u8; 0x200];
        let header_offset = 0x80;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len() + 0x10) as u16;
        write_exepack_header(&mut data, header_offset, 16, exepack_size, 0, 0, 0, 0, 0, 1);

        assert!(find_exepack_header(&data, false).is_none());
    }

    #[test]
    fn test_find_exepack_header_trace_path_start_ge_end() {
        let mut data = vec![0u8; 0x1C];
        data[..EXEPACK_ERROR_STR.len()].copy_from_slice(EXEPACK_ERROR_STR);
        assert!(find_exepack_header(&data, true).is_none());
    }

    #[test]
    fn test_find_exepack_header_rb_with_wrong_second_byte() {
        let mut data = vec![0u8; 0x80];
        data[0x30] = b'R';
        data[0x31] = b'X';
        assert!(find_exepack_header(&data, false).is_none());
    }

    #[test]
    fn test_find_exepack_header_rb_too_early_for_signature_offset() {
        let mut data = vec![0u8; 0x80];
        data[4] = b'R';
        data[5] = b'B';
        assert!(find_exepack_header(&data, false).is_none());
    }

    #[test]
    fn test_find_error_string_missing() {
        let data = vec![0u8; 64];
        assert!(find_error_string(&data, 0, 10).is_none());
        assert!(find_error_string(&data, 10, 5).is_none());
    }

    #[test]
    fn test_find_error_string_respects_window_bounds() {
        let mut data = vec![0u8; 128];
        data[0x20..0x20 + EXEPACK_ERROR_STR.len()].copy_from_slice(EXEPACK_ERROR_STR);
        data[0x50..0x50 + EXEPACK_ERROR_STR.len()].copy_from_slice(EXEPACK_ERROR_STR);

        assert_eq!(find_error_string(&data, 0x10, 0x40), Some(0x20));
        assert_eq!(find_error_string(&data, 0x40, 0x1000), Some(0x50));
    }

    #[test]
    fn test_unpack_exepack_relocations_truncated_at_section_count() {
        assert_err_contains(
            unpack_exepack_relocations(&[], 0, 0),
            "relocation table truncated at section 0",
        );
    }

    #[test]
    fn test_unpack_exepack_relocations_truncated_in_section_entries() {
        let data = vec![1, 0];
        assert_err_contains(
            unpack_exepack_relocations(&data, 0, data.len()),
            "relocation table truncated in section 0",
        );
    }

    #[test]
    fn test_unpack_exepack_relocations_parses_all_sections() {
        // 16 sections, sequential format: for each section, read u16 count,
        // then count * u16 offsets. Layout:
        //   pos=0: section 0 count = 1
        //   pos=2: section 0 entry = 0x1234
        //   pos=4: section 1 count = 0
        //   pos=6: section 2 count = 0
        //   pos=8: section 3 count = 1
        //   pos=10: section 3 entry = 0x5678
        //   pos=12: section 4 count = 0
        //   ...
        //   pos=12 + 12*2 = 36: end
        let mut data = vec![0u8; 38];
        write_u16_at(&mut data, 0, 1); // section 0: 1 entry
        write_u16_at(&mut data, 2, 0x1234); // section 0 entry
                                            // section 1: 0 entries (pos 4)
                                            // section 2: 0 entries (pos 6)
        write_u16_at(&mut data, 8, 1); // section 3: 1 entry
        write_u16_at(&mut data, 10, 0x5678); // section 3 entry
                                             // sections 4-15: 0 entries (pos 12..36)

        let (relocs, end) =
            unpack_exepack_relocations(&data, 0, data.len()).expect("reloc parse should pass");
        assert_eq!(relocs.len(), 2);
        assert_eq!(relocs[0], (0x1234, 0x0000)); // section 0 → segment 0*0x1000
        assert_eq!(relocs[1], (0x5678, 0x3000)); // section 3 → segment 3*0x1000
        assert_eq!(end, 36); // 16 count words (some with entries)
    }

    #[test]
    fn test_decompress_exepack_output_too_large() {
        let packed = vec![0u8; 4];
        let result = decompress_exepack(&packed, MAX_EXEPACK_OUTPUT_SIZE + 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_decompress_exepack_copy_overflows_dest() {
        let packed = vec![0x10, 0x20, 0x30, 0x40, 0x04, 0x00, 0xB3];
        let unpacked = decompress_exepack(&packed, 2).expect("copy should succeed");
        assert_eq!(unpacked, vec![0x30, 0x40]);
    }

    #[test]
    fn test_decompress_exepack_truncated_b0_command() {
        let packed = vec![0xB1];
        let result = decompress_exepack(&packed, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_decompress_exepack_truncated_b2_command() {
        assert_err_contains(decompress_exepack(&[0xB3], 1), "Truncated B2 command");
    }

    #[test]
    fn test_decompress_exepack_copy_length_exceeds_source_position() {
        assert_err_contains(
            decompress_exepack(&[0xAA, 0x02, 0x00, 0xB3], 2),
            "Copy length 2 exceeds source position 1",
        );
    }

    #[test]
    fn test_decompress_exepack_empty_input_returns_zeroes() {
        let unpacked = decompress_exepack(&[], 3).expect("empty stream should decode");
        assert_eq!(unpacked, vec![0u8; 3]);
    }

    #[test]
    fn test_decompress_exepack_padding_only_returns_zeroes() {
        let unpacked = decompress_exepack(&[0xFF, 0xFF], 2).expect("padding should be skipped");
        assert_eq!(unpacked, vec![0u8; 2]);
    }

    #[test]
    fn test_decompress_exepack_b0_zero_length_command() {
        let unpacked =
            decompress_exepack(&[0xAA, 0x00, 0x00, 0xB0], 2).expect("decode should pass");
        assert_eq!(unpacked, vec![0u8; 2]);
    }

    #[test]
    fn test_decompress_exepack_b2_zero_length_command() {
        let unpacked = decompress_exepack(&[0x00, 0x00, 0xB2], 2).expect("decode should pass");
        assert_eq!(unpacked, vec![0u8; 2]);
    }

    #[test]
    fn test_decompress_exepack_fill_overflows_destination() {
        let unpacked =
            decompress_exepack(&[0xCC, 0x04, 0x00, 0xB1], 2).expect("fill should succeed");
        assert_eq!(unpacked, vec![0xCC; 2]);
    }

    #[test]
    fn test_decompress_exepack_copy_overflow_with_zero_destination() {
        let unpacked =
            decompress_exepack(&[0x10, 0x20, 0x02, 0x00, 0xB3], 0).expect("copy should succeed");
        assert!(unpacked.is_empty());
    }

    #[test]
    fn test_decompress_exepack_handles_non_last_then_last_command() {
        let packed = [0x00, 0x00, 0x00, 0xB1, 0xAA, 0x00, 0x00, 0xB0];
        let unpacked = decompress_exepack(&packed, 1).expect("decode should pass");
        assert_eq!(unpacked, vec![0]);
    }

    #[test]
    fn test_decompress_exepack_executes_non_last_command_path() {
        let packed = [0x11, 0x01, 0x00, 0xB1, 0x22, 0x01, 0x00, 0xB0];
        let unpacked = decompress_exepack(&packed, 2).expect("decode should pass");
        assert_eq!(unpacked, vec![0x11, 0x22]);
    }

    #[test]
    fn test_detect_packer_lzexe_stub_signature() {
        let mut data = vec![0u8; 0x200];
        data[0] = b'M';
        data[1] = b'Z';
        data[0x60] = 0x50;
        data[0x61] = 0xB8;
        data[0xE0] = 0xD1;
        data[0xE1] = 0xED;
        assert_eq!(detect_packer(&data), PackerType::LZEXE);
    }

    #[test]
    fn test_unpack_exepack_missing_header_returns_stub() {
        // No EXEPACK header found → returns data as-is (no env var needed)
        let mut data = vec![0u8; 0x200];
        data[0] = b'M';
        data[1] = b'Z';
        let out = unpack_exepack(&data).expect("stub fallback");
        assert_eq!(out, data);
    }

    #[test]
    fn test_unpack_exepack_missing_header_with_trace_returns_stub() {
        let _trace = trace_subscriber("gp64::unpacker::exepack=debug");
        // No EXEPACK header found → returns data as-is (no env var needed)
        let mut data = vec![0u8; 0x200];
        data[0] = b'M';
        data[1] = b'Z';
        let out = unpack_exepack(&data).expect("stub fallback");
        assert_eq!(out, data);
    }

    #[test]
    fn test_unpack_exepack_dest_len_zero_returns_stub() {
        // dest_len == 0 → returns data as-is (no env var needed)
        let mut data = vec![0u8; 0x200];
        data[0] = b'M';
        data[1] = b'Z';

        let header_offset = 0x80;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len()) as u16;

        write_u16_at(&mut data, header_offset, 0x0000);
        write_u16_at(&mut data, header_offset + 0x02, 0x0000);
        write_u16_at(&mut data, header_offset + 0x04, 0x0000);
        write_u16_at(&mut data, header_offset + 0x06, exepack_size);
        write_u16_at(&mut data, header_offset + 0x08, 0x0000);
        write_u16_at(&mut data, header_offset + 0x0A, 0x0000);
        write_u16_at(&mut data, header_offset + 0x0C, 0x0000);
        write_u16_at(&mut data, header_offset + 0x0E, 0x4252); // "RB"

        let err_pos = header_offset + 0x20;
        data[err_pos..err_pos + EXEPACK_ERROR_STR.len()].copy_from_slice(EXEPACK_ERROR_STR);

        let out = unpack_exepack(&data).expect("stub fallback");
        assert_eq!(out, data);
    }

    #[test]
    fn test_unpack_exepack_dest_len_zero_with_trace_returns_stub() {
        let _trace = trace_subscriber("gp64::unpacker::exepack=debug");
        // dest_len == 0 → returns data as-is (no env var needed)
        let mut data = vec![0u8; 0x200];
        data[0] = b'M';
        data[1] = b'Z';

        let header_offset = 0x80;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len()) as u16;
        write_exepack_header(&mut data, header_offset, 16, exepack_size, 0, 0, 0, 0, 0, 1);
        let err_pos = header_offset + 0x20;
        data[err_pos..err_pos + EXEPACK_ERROR_STR.len()].copy_from_slice(EXEPACK_ERROR_STR);

        let out = unpack_exepack(&data).expect("stub fallback");
        assert_eq!(out, data);
    }

    // KILN-00086: test_unpack_exepack_18_byte_header_with_skip_len_{trims,one_keeps}_packed_end
    // removed — they forced the retired force-stub passthrough (asserting the input
    // is returned unchanged), a capability that no longer exists (static decompression is the
    // only path).

    #[test]
    fn test_unpack_exepack_rejects_invalid_packed_bounds() {
        let mut data = make_mz_image(0x200, 8);
        let header_offset = 0x70;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len()) as u16;
        write_exepack_header(&mut data, header_offset, 16, exepack_size, 1, 0, 0, 0, 0, 1);

        assert_err_contains(
            unpack_exepack(&data),
            "EXEPACK: Invalid packed data boundaries",
        );
    }

    #[test]
    fn test_unpack_exepack_static_missing_error_string_reports_error() {
        // Static decompression is now the default — no env var needed
        let mut data = make_mz_image(0x200, 1);
        let header_offset = 0x40;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len() + 0x10) as u16;
        write_exepack_header(
            &mut data,
            header_offset,
            16,
            exepack_size,
            1,
            0,
            12,
            0,
            0,
            1,
        );

        let payload = [0x10, 0x20, 0x30, 0x40, 0x04, 0x00, 0xB3];
        let payload_start = header_offset - payload.len();
        data[payload_start..header_offset].copy_from_slice(&payload);

        assert_err_contains(unpack_exepack(&data), "EXEPACK: error string not found");
    }

    #[test]
    fn test_unpack_exepack_static_zero_entry_window_succeeds() {
        // All-0x00 at entry point is no longer rejected (only all-0xFF is)
        let _trace = trace_subscriber("gp64::unpacker::exepack=debug");
        let data = make_static_exepack_fixture(0x00, 2, 0, &[]);
        let out = unpack_exepack(&data).expect("static unpack should succeed");
        assert_eq!(&out[0..2], b"MZ");
        // Output should be different from input (unpacked MZ header rebuilt)
        assert_ne!(out, data);
    }

    #[test]
    fn test_unpack_exepack_static_sanity_fallback_for_ff_entry_window() {
        // All-0xFF at entry point is rejected → falls back to stub execution
        let data = make_static_exepack_fixture(0xFF, 2, 0, &[]);
        let out = unpack_exepack(&data).expect("sanity fallback");
        assert_eq!(out, data);
    }

    #[test]
    fn test_unpack_exepack_static_success_without_trailing_data() {
        // Static decompression is now the default
        let data = make_static_exepack_fixture(0x7F, 2, 0, &[]);
        let out = unpack_exepack(&data).expect("static unpack should succeed");
        assert_eq!(&out[0..2], b"MZ");
    }

    #[test]
    fn test_unpack_exepack_static_success_with_trailing_data_and_trace() {
        let _trace = trace_subscriber("gp64::unpacker::exepack=debug");
        // Static decompression is now the default
        let trailing = [0xDE, 0xAD, 0xBE, 0xEF];
        let data = make_static_exepack_fixture(0x7F, 2, 0, &trailing);
        let out = unpack_exepack(&data).expect("static unpack should succeed");
        assert!(out.ends_with(&trailing));
    }

    #[test]
    fn test_unpack_exepack_static_success_skips_entry_sanity_when_window_out_of_range() {
        // Static decompression is now the default
        let data = make_static_exepack_fixture(0x7F, 1, 12, &[]);
        let out = unpack_exepack(&data).expect("static unpack should succeed");
        assert_eq!(&out[0..2], b"MZ");
    }

    // KILN-00086: test_unpack_exepack_stub_override_{returns_data_as_is,trace_off_returns_
    // data_as_is} removed — they asserted the retired force-stub passthrough
    // (input returned unchanged even with a valid header); that override no longer exists.

    // -----------------------------------------------------------------------
    // Tests for exepack_real_entry()
    // -----------------------------------------------------------------------

    #[test]
    fn test_exepack_real_entry_no_signature() {
        // Plain MZ without EXEPACK header → None
        let data = make_mz_image(0x200, 1);
        assert_eq!(exepack_real_entry(&data), None);
    }

    #[test]
    fn test_exepack_real_entry_valid() {
        // Build data with a valid EXEPACK header containing known CS:IP
        let mut data = vec![0u8; 0x200];
        let header_offset = 0x80;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len()) as u16;
        write_exepack_header(
            &mut data,
            header_offset,
            16,
            exepack_size,
            0x0020, // dest_len
            0x0005, // real_cs
            0x0010, // real_ip
            0x0010, // real_ss
            0x0100, // real_sp
            1,      // skip_len
        );
        let result = exepack_real_entry(&data);
        assert_eq!(result, Some((0x0005, 0x0010)));
    }

    #[test]
    fn test_exepack_real_entry_zero_dest_len() {
        // EXEPACK header with dest_len == 0 → None
        // (need error string so find_exepack_header finds it with allow_zero_dest)
        let mut data = vec![0u8; 0x200];
        let header_offset = 0x80;
        let exepack_size = (16 + EXEPACK_ERROR_STR.len() + 0x10) as u16;
        write_exepack_header(
            &mut data,
            header_offset,
            16,
            exepack_size,
            0, // dest_len = 0
            0,
            0, // real_cs, real_ip
            0,
            0, // real_ss, real_sp
            1, // skip_len
        );
        // Place error string so header is discovered with allow_zero_dest
        let err_pos = header_offset + 0x10;
        data[err_pos..err_pos + EXEPACK_ERROR_STR.len()].copy_from_slice(EXEPACK_ERROR_STR);

        assert_eq!(exepack_real_entry(&data), None);
    }

    // -----------------------------------------------------------------------
    // Tests for unpack_lzexe()
    // -----------------------------------------------------------------------

    #[test]
    fn test_unpack_lzexe_too_small() {
        let data = vec![0u8; 0x1B]; // < 0x1C
        assert_err_contains(unpack_lzexe(&data), "File too small");
    }

    #[test]
    fn test_unpack_lzexe_returns_error() {
        // Valid-size MZ but LZEXE static unpacking is not implemented
        let data = make_mz_image(0x200, 1);
        let result = unpack_lzexe(&data);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LZEXE"),
            "error should mention LZEXE, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Tests for unpack_if_needed()
    // -----------------------------------------------------------------------

    #[test]
    fn test_unpack_if_needed_none() {
        // Plain MZ with no packer signature → returns data as-is
        let data = make_mz_image(0x300, 1);
        let out = unpack_if_needed(&data).expect("should return as-is");
        assert_eq!(out, data);
    }

    #[test]
    fn test_unpack_if_needed_lzexe() {
        // LZEXE detected → returns data as-is (stub execution)
        let mut data = make_mz_image(0x200, 1);
        data[0x1E..0x22].copy_from_slice(b"LZ91");
        let out = unpack_if_needed(&data).expect("should return as-is for LZEXE");
        assert_eq!(out, data);
    }

    #[test]
    fn test_unpack_if_needed_pklite() {
        // PKLITE detected → returns data as-is (stub execution)
        let mut data = make_mz_image(0x300, 1);
        data[0x1C..0x22].copy_from_slice(b"PKLITE");
        let out = unpack_if_needed(&data).expect("should return as-is for PKLITE");
        assert_eq!(out, data);
    }

    #[test]
    fn test_unpack_if_needed_unknown() {
        // Non-MZ data → PackerType::None → returns as-is
        let data = vec![0u8; 0x10];
        let out = unpack_if_needed(&data).expect("should return as-is for unknown");
        assert_eq!(out, data);
    }

    // -----------------------------------------------------------------------
    // Tests for uncompressed prefix copy, min_alloc adjustment, STUB env var
    // -----------------------------------------------------------------------

    #[test]
    fn test_decompress_exepack_uncompressed_prefix_copy() {
        // The decompressor should copy the uncompressed prefix (data before
        // the first RLE command's reach) into the output at position 0.
        // Build packed data: [prefix_byte, prefix_byte, fill_byte, count_lo, count_hi, 0xB1]
        // This fills 2 bytes with fill_byte (last command), leaving positions 0..2
        // to be filled by the uncompressed prefix copy.
        let packed = [0xAA, 0xBB, 0xCC, 0x02, 0x00, 0xB1];
        let unpacked = decompress_exepack(&packed, 4).expect("prefix copy should work");
        // Positions 2..4 are filled with 0xCC by the B0 command
        assert_eq!(unpacked[2], 0xCC);
        assert_eq!(unpacked[3], 0xCC);
        // Positions 0..2 are copied from the uncompressed prefix
        assert_eq!(unpacked[0], 0xAA);
        assert_eq!(unpacked[1], 0xBB);
    }

    #[test]
    fn test_unpack_exepack_min_alloc_adjustment() {
        // After static unpacking, min_alloc should be reduced by the
        // decompression overhead (dest_len - packed_image_paragraphs).
        let data = make_static_exepack_fixture(0x7F, 2, 0, &[]);

        // Set a known min_alloc in the input
        let mut data = data;
        let packed_min_alloc: u16 = 0x0100;
        write_u16_at(&mut data, 0x0A, packed_min_alloc);

        let out = unpack_exepack(&data).expect("static unpack should succeed");
        assert_eq!(&out[0..2], b"MZ");

        // Read the output min_alloc
        let out_min_alloc = read_u16_at(&out, 0x0A).unwrap();
        // It should be less than or equal to the packed min_alloc
        // (reduced by the decompression overhead)
        assert!(
            out_min_alloc <= packed_min_alloc,
            "output min_alloc ({out_min_alloc:#06X}) should be <= packed min_alloc ({packed_min_alloc:#06X})"
        );
    }

    /// MDD-BUG-CRU-01409: the rebuilt min_alloc comes from the declared image,
    /// so appended data - here past 1 MB, which also wrapped the old u16 cast -
    /// leaves it exactly as it is without that data.
    #[test]
    fn unpack_exepack_min_alloc_ignores_appended_data() {
        let mut data = make_static_exepack_fixture(0x7F, 0x40, 0, &[]);
        write_u16_at(&mut data, 0x0A, 0x0100);
        // Declare exactly the image built so far, so anything appended below
        // lies past the declared image end.
        let len = data.len();
        write_u16_at(&mut data, 0x02, (len % 512) as u16);
        write_u16_at(&mut data, 0x04, len.div_ceil(512) as u16);
        let baseline = unpack_exepack(&data).expect("static unpack");
        let baseline_min_alloc = read_u16_at(&baseline, 0x0A).unwrap();

        data.extend(std::iter::repeat_n(0x5A, 0x11_0000));
        let out = unpack_exepack(&data).expect("static unpack with an overlay");

        assert_eq!(read_u16_at(&out, 0x0A).unwrap(), baseline_min_alloc);
    }

    // KILN-00086: test_unpack_exepack_stub_env_var_forces_passthrough removed — it asserted
    // the retired force-stub passthrough; that override no longer exists.

    #[test]
    fn test_unpack_exepack_without_stub_env_var_decompresses() {
        // Static decompression is the only path (the force-stub override was retired).
        let data = make_static_exepack_fixture(0x7F, 2, 0, &[]);
        let out = unpack_exepack(&data).expect("static decompression");
        assert_ne!(out, data, "output should be decompressed");
        assert_eq!(&out[0..2], b"MZ");
    }

    #[test]
    fn test_unpack_exepack_relocations_section_multiplier() {
        // Verify that section segments use the 0x1000 multiplier:
        // section N → segment = N * 0x1000
        // Single entry in section 15 → segment = 15 * 0x1000 = 0xF000
        let mut data = vec![0u8; 64];
        // Sections 0-14: count = 0
        // Section 15 starts at offset 15 * 2 = 30
        write_u16_at(&mut data, 30, 1); // section 15: 1 entry
        write_u16_at(&mut data, 32, 0x0042); // offset within section

        let (relocs, _) = unpack_exepack_relocations(&data, 0, data.len()).expect("reloc parse");
        assert_eq!(relocs.len(), 1);
        assert_eq!(relocs[0], (0x0042, 0xF000)); // 15 * 0x1000 = 0xF000
    }
}
