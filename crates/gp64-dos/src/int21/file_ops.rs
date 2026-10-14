//! File operations, FCB support, and handle management for INT 21h

use super::*;

impl Dos {
    pub fn resolve_alias_handle(&self, handle: u16) -> u16 {
        let mut current = handle;
        for _ in 0..8 {
            let Some(&next) = self
                .file_table
                .std_handle_aliases
                .get(current as usize)
                .and_then(|o| o.as_ref())
            else {
                break;
            };
            if next == current {
                break;
            }
            current = next;
        }
        current
    }

    pub(crate) fn allocate_alias_handle(&self) -> Option<u16> {
        self.file_table.next_free_handle()
    }

    pub(crate) fn read_asciiz(
        &self,
        cpu: &Cpu,
        memory: &Memory,
        segment: u16,
        offset: u32,
    ) -> String {
        // Use segment descriptor-aware translation for protected mode compatibility.
        // Limit to MAX_DOS_PATH to prevent unbounded memory reads.
        let base_addr = cpu.translate_address(segment, offset);
        (0..MAX_DOS_PATH)
            .map(|i| memory.read_u8(base_addr + i))
            .take_while(|&ch| ch != 0)
            .map(|ch| ch as char)
            .collect()
    }

    // ============================================================================
    // FCB (File Control Block) Support
    // ============================================================================
    //
    // FCB structure (37 bytes for standard FCB):
    // Offset  Size  Description
    // 0x00    1     Drive number (0=default, 1=A, 2=B, ...)
    // 0x01    8     Filename (space-padded)
    // 0x09    3     Extension (space-padded)
    // 0x0C    2     Current block number
    // 0x0E    2     Record size (default 128)
    // 0x10    4     File size (set by open)
    // 0x14    2     File date
    // 0x16    2     File time
    // 0x18    8     Reserved (we store handle in first 2 bytes)
    // 0x20    1     Current record within block
    // 0x21    4     Random record number
    //
    // Extended FCB has 7-byte prefix:
    // -7      1     0xFF (extended FCB marker)
    // -6      5     Reserved
    // -1      1     Attribute byte
    // 0+      37    Standard FCB

    /// Bytes an extended FCB prepends to the standard FCB body: the `0xFF`
    /// marker, five reserved bytes, then the search attribute.
    pub(crate) const FCB_EXTENDED_PREFIX: usize = 7;
    /// Offset of the extended FCB's search-attribute byte, relative to the
    /// start of the extended FCB.
    pub(crate) const FCB_EXTENDED_ATTR: usize = 6;

    /// FCB offsets
    pub(crate) const FCB_DRIVE: usize = 0;
    pub(crate) const FCB_FILENAME: usize = 1;
    pub(crate) const FCB_EXTENSION: usize = 9;
    pub(crate) const FCB_CURRENT_BLOCK: usize = 12;
    pub(crate) const FCB_RECORD_SIZE: usize = 14;
    pub(crate) const FCB_FILE_SIZE: usize = 16;
    pub(crate) const FCB_FILE_DATE: usize = 20;
    pub(crate) const FCB_FILE_TIME: usize = 22;
    pub(crate) const FCB_RESERVED: usize = 24; // We store file handle here
    pub(crate) const FCB_CURRENT_RECORD: usize = 32; // Sequential I/O record counter
    pub(crate) const FCB_RANDOM_RECORD: usize = 33;

    /// Check if FCB is extended (starts with 0xFF)
    pub(crate) fn fcb_is_extended(memory: &Memory, fcb: usize) -> bool {
        memory.read_u8(fcb) == 0xFF
    }

    // ---- FCB search result layout (AH=11h / AH=12h) ------------------------
    //
    // MDD-BUG-CRUCIBLE-00447: both handlers used to serialize this inline, and
    // both got it wrong the same way — attribute at DTA+21, size at DTA+28, no
    // time or date, and no extended-FCB prefix at all.
    //
    // The MS-DOS 3.3 Programmer's Reference (Function 11h) defines a NORMAL
    // result as one drive byte followed by a 32-byte directory entry, and an
    // EXTENDED result as a seven-byte prefix followed by that same normal
    // result. Within the directory entry the FAT layout puts the attribute at
    // +11, time at +22, date at +24, starting cluster at +26 and size at +28 —
    // so relative to the DTA of a normal result those land at +12, +23, +25,
    // +27 and +29.
    //
    // Reference: https://www.pcjs.org/documents/books/mspl13/msdos/dosref33/

    /// Size of the extended-FCB search prefix: `0xFF`, five reserved zero
    /// bytes, then the search attribute.
    pub(crate) const FCB_SEARCH_EXTENDED_PREFIX: usize = 7;
    /// Drive byte, relative to the start of the (normal) result.
    pub(crate) const FCB_SEARCH_DRIVE: usize = 0;
    /// 8-character name, relative to the start of the result.
    pub(crate) const FCB_SEARCH_NAME: usize = 1;
    /// 3-character extension.
    pub(crate) const FCB_SEARCH_EXT: usize = 9;
    /// Attribute byte (directory entry +11).
    pub(crate) const FCB_SEARCH_ATTR: usize = 12;
    /// Packed DOS time (directory entry +22).
    pub(crate) const FCB_SEARCH_TIME: usize = 23;
    /// Packed DOS date (directory entry +24).
    pub(crate) const FCB_SEARCH_DATE: usize = 25;
    /// Starting cluster (directory entry +26).
    pub(crate) const FCB_SEARCH_CLUSTER: usize = 27;
    /// File size in bytes (directory entry +28).
    pub(crate) const FCB_SEARCH_SIZE: usize = 29;
    /// Total bytes of a normal result: drive byte + 32-byte directory entry.
    pub(crate) const FCB_SEARCH_NORMAL_LEN: usize = 33;

    /// Serialize one FCB search result into the DTA, per the MS-DOS 3.3
    /// reference layout above.
    ///
    /// `extended` selects the seven-byte prefix form, which the guest asks for
    /// by passing an extended FCB (first byte `0xFF`). `search_attr` is echoed
    /// into that prefix and ignored for a normal result.
    ///
    /// Returns the number of bytes written, so a caller — or a test — can say
    /// exactly how much of the DTA the call owns.
    pub(crate) fn fcb_write_search_result(
        memory: &mut Memory,
        dta_addr: usize,
        extended: bool,
        search_attr: u8,
        drive: u8,
        name: &str,
        entry: &crate::FindEntry,
    ) -> usize {
        let base = if extended {
            // 0xFF marker, five reserved bytes that DOS zeroes, then the
            // attribute the search was performed with.
            memory.write_u8(dta_addr, 0xFF);
            for i in 1..6 {
                memory.write_u8(dta_addr + i, 0);
            }
            memory.write_u8(dta_addr + 6, search_attr);
            dta_addr + Self::FCB_SEARCH_EXTENDED_PREFIX
        } else {
            dta_addr
        };

        let (name_part, ext_part) = match name.rfind('.') {
            Some(dot) => (&name[..dot], &name[dot + 1..]),
            None => (name, ""),
        };

        memory.write_u8(base + Self::FCB_SEARCH_DRIVE, drive);
        if entry.attr & 0x08 != 0 {
            // A volume label uses all 11 bytes verbatim, with no 8.3 split.
            for i in 0..11 {
                memory.write_u8(
                    base + Self::FCB_SEARCH_NAME + i,
                    name.as_bytes().get(i).copied().unwrap_or(b' '),
                );
            }
        } else {
            for i in 0..8 {
                let ch = name_part.as_bytes().get(i).copied().unwrap_or(b' ');
                memory.write_u8(base + Self::FCB_SEARCH_NAME + i, ch.to_ascii_uppercase());
            }
            for i in 0..3 {
                let ch = ext_part.as_bytes().get(i).copied().unwrap_or(b' ');
                memory.write_u8(base + Self::FCB_SEARCH_EXT + i, ch.to_ascii_uppercase());
            }
        }
        // The 8.3 name ends at index 11, so the attribute at +12 follows it
        // immediately — there is no gap to clear before it.
        memory.write_u8(base + Self::FCB_SEARCH_ATTR, entry.attr);
        // Directory-entry bytes +12..+21 are reserved. DOS leaves them zero
        // rather than exposing whatever the guest last had in the DTA — and
        // one of them is where the OLD code wrongly wrote the attribute.
        for i in (Self::FCB_SEARCH_ATTR + 1)..Self::FCB_SEARCH_TIME {
            memory.write_u8(base + i, 0);
        }
        memory.write_u16(base + Self::FCB_SEARCH_TIME, entry.time);
        memory.write_u16(base + Self::FCB_SEARCH_DATE, entry.date);
        // Starting cluster is meaningless for a host-backed file; DOS still
        // writes the field, so publish a defined zero rather than stale bytes.
        memory.write_u16(base + Self::FCB_SEARCH_CLUSTER, 0);
        memory.write_u32(base + Self::FCB_SEARCH_SIZE, entry.size);

        if extended {
            Self::FCB_SEARCH_EXTENDED_PREFIX + Self::FCB_SEARCH_NORMAL_LEN
        } else {
            Self::FCB_SEARCH_NORMAL_LEN
        }
    }

    /// Linear address of the actual FCB body, skipping the extended-FCB
    /// header when one is present.
    pub(crate) fn fcb_base_addr(memory: &Memory, fcb: usize) -> usize {
        if Self::fcb_is_extended(memory, fcb) {
            fcb + Self::FCB_EXTENDED_PREFIX
        } else {
            fcb
        }
    }

    /// Read FCB drive number (0=default, 1=A, 2=B, etc.)
    ///
    /// The byte is guest-supplied and unbounded; build a path from it only
    /// through [`Self::fcb_drive_letter`].
    pub(crate) fn fcb_read_drive(&self, memory: &Memory, fcb: usize) -> u8 {
        let addr = Self::fcb_base_addr(memory, fcb);
        let drive = memory.read_u8(addr + Self::FCB_DRIVE);
        if drive == 0 {
            // 0 means default drive
            self.path_resolver.get_current_drive() + 1
        } else {
            drive
        }
    }

    /// Drive letter an FCB names, or `None` when its drive number is beyond
    /// `Z:` and so names no drive at all (MDD-BUG-CRU-01392).
    ///
    /// DOS drive numbers run 1 (`A:`) to 26 (`Z:`). A larger byte is common
    /// in an FCB built in an uninitialised buffer, and adding it to `b'A'`
    /// overflows for `0xC0..=0xFF`.
    pub(crate) fn fcb_drive_letter(&self, memory: &Memory, fcb: usize) -> Option<char> {
        let drive = self.fcb_read_drive(memory, fcb);
        (1..=26)
            .contains(&drive)
            .then(|| char::from(b'A' + drive - 1))
    }

    /// Read FCB filename as a string (8.3 format)
    pub(crate) fn fcb_read_filename(&self, memory: &Memory, fcb: usize) -> String {
        let addr = Self::fcb_base_addr(memory, fcb);

        // Read filename (8 bytes)
        let mut name = String::new();
        for i in 0..8 {
            let ch = memory.read_u8(addr + Self::FCB_FILENAME + i);
            if ch != b' ' && ch != 0 {
                name.push((ch as char).to_ascii_uppercase());
            }
        }

        // Read extension (3 bytes)
        let mut ext = String::new();
        for i in 0..3 {
            let ch = memory.read_u8(addr + Self::FCB_EXTENSION + i);
            if ch != b' ' && ch != 0 {
                ext.push((ch as char).to_ascii_uppercase());
            }
        }

        if ext.is_empty() {
            name
        } else {
            format!("{name}.{ext}")
        }
    }

    /// Convert raw FCB name/ext bytes to a filename string
    /// Preserves ? and * wildcards, trims trailing spaces
    pub(crate) fn fcb_bytes_to_filename(name_bytes: &[u8; 8], ext_bytes: &[u8; 3]) -> String {
        let mut name = String::new();
        for &ch in name_bytes.iter() {
            if ch != b' ' && ch != 0 {
                name.push((ch as char).to_ascii_uppercase());
            }
        }

        let mut ext = String::new();
        for &ch in ext_bytes.iter() {
            if ch != b' ' && ch != 0 {
                ext.push((ch as char).to_ascii_uppercase());
            }
        }

        if ext.is_empty() {
            name
        } else {
            format!("{name}.{ext}")
        }
    }

    /// Apply FCB rename pattern to a filename
    /// In the new name pattern, ? means "keep the original character"
    pub(crate) fn apply_rename_pattern(
        original: &str,
        new_name_bytes: &[u8; 8],
        new_ext_bytes: &[u8; 3],
    ) -> String {
        // Parse original into 8.3 format with padding
        let original_upper = original.to_uppercase();
        let (orig_name, orig_ext) = if let Some(dot) = original_upper.rfind('.') {
            (&original_upper[..dot], &original_upper[dot + 1..])
        } else {
            (original_upper.as_str(), "")
        };

        // Pad original name and extension
        let mut orig_name_chars: Vec<char> = orig_name.chars().collect();
        orig_name_chars.resize(8, ' ');
        let mut orig_ext_chars: Vec<char> = orig_ext.chars().collect();
        orig_ext_chars.resize(3, ' ');

        // Build new name - ? means keep original character
        let mut result_name = String::new();
        for (i, &ch) in new_name_bytes.iter().enumerate() {
            if ch == b'?' {
                // Keep original character
                if orig_name_chars[i] != ' ' {
                    result_name.push(orig_name_chars[i]);
                }
            } else if ch != b' ' && ch != 0 {
                result_name.push((ch as char).to_ascii_uppercase());
            }
        }

        // Build new extension - ? means keep original character
        let mut result_ext = String::new();
        for (i, &ch) in new_ext_bytes.iter().enumerate() {
            if ch == b'?' {
                // Keep original character
                if orig_ext_chars[i] != ' ' {
                    result_ext.push(orig_ext_chars[i]);
                }
            } else if ch != b' ' && ch != 0 {
                result_ext.push((ch as char).to_ascii_uppercase());
            }
        }

        if result_ext.is_empty() {
            result_name
        } else {
            format!("{result_name}.{result_ext}")
        }
    }

    /// Store file handle in FCB reserved area
    pub(crate) fn fcb_set_handle(&self, memory: &mut Memory, fcb: usize, handle: u16) {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.write_u16(addr + Self::FCB_RESERVED, handle);
    }

    /// Read file handle from FCB reserved area
    pub(crate) fn fcb_get_handle(&self, memory: &Memory, fcb: usize) -> u16 {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.read_u16(addr + Self::FCB_RESERVED)
    }

    /// Get file size from FCB
    pub(crate) fn fcb_get_file_size(&self, memory: &Memory, fcb: usize) -> u32 {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.read_u32(addr + Self::FCB_FILE_SIZE)
    }

    /// Set FCB file size
    pub(crate) fn fcb_set_file_size(&self, memory: &mut Memory, fcb: usize, size: u32) {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.write_u32(addr + Self::FCB_FILE_SIZE, size);
    }

    /// Get FCB record size
    pub(crate) fn fcb_get_record_size(&self, memory: &Memory, fcb: usize) -> u16 {
        let addr = Self::fcb_base_addr(memory, fcb);
        let size = memory.read_u16(addr + Self::FCB_RECORD_SIZE);
        if size == 0 {
            128
        } else {
            size
        }
    }

    /// Set FCB record size
    pub(crate) fn fcb_set_record_size(&self, memory: &mut Memory, fcb: usize, size: u16) {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.write_u16(addr + Self::FCB_RECORD_SIZE, size);
    }

    /// Get FCB random record number (4 bytes at offset 0x21)
    pub(crate) fn fcb_get_random_record(&self, memory: &Memory, fcb: usize) -> u32 {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.read_u32(addr + Self::FCB_RANDOM_RECORD)
    }

    /// Set FCB random record number (4 bytes at offset 0x21)
    pub(crate) fn fcb_set_random_record(&self, memory: &mut Memory, fcb: usize, record: u32) {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.write_u32(addr + Self::FCB_RANDOM_RECORD, record);
    }

    /// Set FCB file date/time
    pub(crate) fn fcb_set_datetime(&self, memory: &mut Memory, fcb: usize, date: u16, time: u16) {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.write_u16(addr + Self::FCB_FILE_DATE, date);
        memory.write_u16(addr + Self::FCB_FILE_TIME, time);
    }

    /// Get FCB current block
    pub(crate) fn fcb_get_current_block(&self, memory: &Memory, fcb: usize) -> u16 {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.read_u16(addr + Self::FCB_CURRENT_BLOCK)
    }

    /// Set FCB current block
    pub(crate) fn fcb_set_current_block(&self, memory: &mut Memory, fcb: usize, block: u16) {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.write_u16(addr + Self::FCB_CURRENT_BLOCK, block);
    }

    /// Get FCB current record
    pub(crate) fn fcb_get_current_record(&self, memory: &Memory, fcb: usize) -> u8 {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.read_u8(addr + Self::FCB_CURRENT_RECORD)
    }

    /// Set FCB current record
    pub(crate) fn fcb_set_current_record(&self, memory: &mut Memory, fcb: usize, record: u8) {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.write_u8(addr + Self::FCB_CURRENT_RECORD, record);
    }

    /// Return the byte offset named by the current block and record fields.
    /// DOS FCB blocks contain 128 records; widen each field before computing
    /// the record number so the largest block and record size fit in `u64`.
    pub(crate) fn fcb_current_byte_offset(&self, memory: &Memory, fcb: usize) -> u64 {
        let block = u64::from(self.fcb_get_current_block(memory, fcb));
        let record = u64::from(self.fcb_get_current_record(memory, fcb));
        let record_size = u64::from(self.fcb_get_record_size(memory, fcb));
        (block * 128 + record) * record_size
    }

    /// Advance FCB position after sequential I/O
    /// DOS FCB sequential position: record 0-127 in current block, then block increments
    pub(crate) fn fcb_advance_position(&self, memory: &mut Memory, fcb: usize) {
        let current_record = self.fcb_get_current_record(memory, fcb);
        let current_block = self.fcb_get_current_block(memory, fcb);

        if current_record >= 127 {
            // Record wraps from 127 to 0, block increments
            self.fcb_set_current_record(memory, fcb, 0);
            self.fcb_set_current_block(memory, fcb, current_block.wrapping_add(1));
        } else {
            self.fcb_set_current_record(memory, fcb, current_record + 1);
        }
    }

    /// Update drive byte in FCB (set to actual drive number)
    pub(crate) fn fcb_set_drive(&self, memory: &mut Memory, fcb: usize, drive: u8) {
        let addr = Self::fcb_base_addr(memory, fcb);
        memory.write_u8(addr + Self::FCB_DRIVE, drive);
    }
}
