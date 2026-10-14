//! Directory operations and find support for INT 21h

use super::*;

// DOS file attribute bits (INT 21h/4300h, Find First/Next attribute filter)
const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_LABEL: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;

// DTA (Disk Transfer Area) field offsets for Find First/Next results
const DTA_ATTRIBUTE: usize = 0x15;
const DTA_TIME: usize = 0x16;
const DTA_DATE: usize = 0x18;
const DTA_SIZE: usize = 0x1A;
const DTA_FILENAME: usize = 0x1E;

pub(crate) const LFN_FIND_DATA_SIZE: usize = 318;
const LFN_FIND_LONG_NAME: usize = 44;
const LFN_FIND_SHORT_NAME: usize = 304;

/// One directory entry in the DOS namespace: the canonical 8.3 name the guest
/// sees and matches its patterns against, plus the host name the drive
/// actually stores the file under.
///
/// The two differ whenever a host-backed drive holds a name DOS cannot
/// express — `Long Filename.txt` is visible to the guest only as
/// `LONGFI~1.TXT`. Any DOS-visible directory operation must therefore *match*
/// on `dos_name` and *act* on `host_name`; matching the host name lets a guest
/// enumerate a file it can then neither delete nor rename.
/// MDD-BUG-CRUCIBLE-00446.
pub(crate) struct DosDirEntry {
    pub(crate) dos_name: String,
    pub(crate) host_name: String,
    pub(crate) attrs: crate::filesystem::FileAttributes,
}

/// Enumerate `dir_part` on `drive` as DOS sees it.
///
/// This is the single definition of "what the guest can see in a directory",
/// shared by Find First, FCB wildcard delete, and FCB wildcard rename, so the
/// three cannot drift apart on which name a pattern is tested against. Short
/// names come from NTFS on a host-backed Windows drive and otherwise from the
/// emulator's own assignment, exactly as `find_first` has always sourced them.
///
/// An unreadable directory yields no entries — every caller already treats a
/// failed `read_dir` as "nothing matched".
pub(crate) fn enumerate_dos_visible(
    drive: &mut dyn crate::filesystem::VirtualDrive,
    dir_part: &str,
) -> Vec<DosDirEntry> {
    let Ok(entries) = drive.read_dir(dir_part) else {
        return Vec::new();
    };
    let names: Vec<String> = entries
        .iter()
        .filter(|(_, attrs)| !attrs.volume_label)
        .map(|(name, _)| name.clone())
        .collect();
    let host_dir = drive.resolve_host_path(dir_part);
    let assignments: std::collections::HashMap<_, _> =
        crate::sfn::assign_short_names_in_dir(host_dir.as_deref(), &names)
            .into_iter()
            .collect();

    // A DOS subdirectory begins with its `.` and `..` entries, and MS-DOS
    // lists them first. No backend yields them (host `read_dir` never does,
    // and the FAT and ISO readers skip them), so they are synthesised here,
    // once, carrying the directory's own attributes (MDD-BUG-CRU-01407). The
    // root has neither. FCB delete and rename skip directories, so they can
    // never act on these names.
    let mut visible = Vec::new();
    if !dir_part.trim_matches('\\').is_empty() {
        let own = drive.get_attributes(dir_part).unwrap_or_default();
        let attrs = crate::filesystem::FileAttributes {
            directory: true,
            size: 0,
            ..own
        };
        for dot in [".", ".."] {
            visible.push(DosDirEntry {
                dos_name: dot.to_string(),
                host_name: dot.to_string(),
                attrs,
            });
        }
    }
    visible.extend(entries.into_iter().map(|(host_name, attrs)| {
        DosDirEntry {
            // Volume labels are raw 11-byte directory names, not 8.3 file
            // names. In particular old DOS can expose a VFAT label-bit entry
            // with embedded NULs; short-name assignment must not rewrite it.
            dos_name: if attrs.volume_label {
                host_name.clone()
            } else {
                assignments
                    .get(&host_name)
                    .cloned()
                    .unwrap_or_else(|| host_name.clone())
            },
            host_name,
            attrs,
        }
    }));
    visible
}

impl Dos {
    fn lfn_find_error_from_read_dir(error: &anyhow::Error) -> u16 {
        match error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind)
        {
            Some(std::io::ErrorKind::PermissionDenied) => 5,
            _ => 3,
        }
    }

    /// DOS 7 LFN searches use CX as two attribute masks: CL contains
    /// attributes which may be present and CH contains attributes which must
    /// be present. This is the documented `(MustMatchAttrs, SearchAttrs)`
    /// predicate; read-only and archive are ignored by the search mask.
    pub(crate) fn matches_lfn_attr_filter(
        attrs: &crate::filesystem::FileAttributes,
        filter: u16,
    ) -> bool {
        let bits = Self::attrs_to_dos_byte(attrs);
        let search = filter as u8;
        let required = (filter >> 8) as u8;

        // Bits 0 and 5 (read-only and archive) are deliberately excluded from
        // the allowable-attributes test. DOS always includes ordinary files,
        // while bits 1..4 select which special entries may also be returned.
        (required & !bits & 0x3F) == 0 && ((!search & bits) & 0x1E) == 0
    }

    fn allocate_lfn_search_handle(&mut self) -> Option<u16> {
        let mut candidate = self.next_lfn_search_handle.max(1);
        for _ in 0..u16::MAX {
            if !self
                .lfn_find_searches
                .iter()
                .any(|(handle, _)| *handle == candidate)
            {
                self.next_lfn_search_handle = candidate.wrapping_add(1).max(1);
                return Some(candidate);
            }
            candidate = candidate.wrapping_add(1).max(1);
        }
        None
    }

    /// Start an explicit-handle DOS 7 long-filename search and return its
    /// first result. Unlike AH=4Eh state, this remains allocated until 71A1h.
    pub(crate) fn lfn_find_first(
        &mut self,
        pattern: &str,
        attr_filter: u16,
    ) -> std::result::Result<(u16, LfnFindSearchEntry), u16> {
        use gp64_types::FixedString;

        let resolved = self.path_resolver.resolve_lfn(pattern);
        let split = resolved.path.rfind('\\').unwrap_or(0);
        let file_pattern = &resolved.path[split + 1..];
        let dir_path = resolved.path[..split].to_string();

        let Some(drive) = self.drives.get_mut(&resolved.drive) else {
            return Err(15);
        };
        let directory_entries = drive
            .read_dir(&dir_path)
            .map_err(|error| Self::lfn_find_error_from_read_dir(&error))?;
        let names: Vec<String> = directory_entries
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let host_dir = drive.resolve_host_path(&dir_path);
        let aliases: std::collections::HashMap<_, _> =
            crate::sfn::assign_short_names_in_dir(host_dir.as_deref(), &names)
                .into_iter()
                .collect();

        let mut matching_entries = LfnFindSearchEntries::new_boxed();
        for (long_name, attrs) in directory_entries {
            let short_name = aliases
                .get(&long_name)
                .cloned()
                .unwrap_or_else(|| long_name.clone());
            let name_matches = Self::matches_lfn_pattern(file_pattern, &long_name)
                || Self::matches_lfn_pattern(file_pattern, &short_name);
            if !name_matches || !Self::matches_lfn_attr_filter(&attrs, attr_filter) {
                continue;
            }
            if matching_entries.is_full() {
                tracing::debug!(
                    target: "gp64::dos::find",
                    "lfn_find_first: match set capped at {} entries",
                    matching_entries.capacity()
                );
                break;
            }
            let entry_path = if dir_path == r"\" {
                format!(r"\{long_name}")
            } else {
                format!(r"{dir_path}\{long_name}")
            };
            let metadata =
                drive
                    .get_file_metadata(&entry_path)
                    .unwrap_or(crate::filesystem::FileMetadata {
                        attributes: attrs,
                        creation_time: attrs.modified,
                        last_access_time: attrs.modified,
                        last_write_time: attrs.modified,
                    });
            matching_entries.push(LfnFindSearchEntry {
                long_name: FixedString::from(long_name.as_str()),
                short_name: FixedString::from(short_name.as_str()),
                attrs: metadata.attributes,
                creation_time: metadata.creation_time,
                last_access_time: metadata.last_access_time,
                last_write_time: metadata.last_write_time,
            });
        }

        let Some(first) = matching_entries.first().copied() else {
            return Err(2);
        };
        if self.lfn_find_searches.is_full() {
            return Err(4);
        }
        let handle = self.allocate_lfn_search_handle().ok_or(4u16)?;
        let state = LfnFindSearchState {
            pattern: FixedString::from(file_pattern),
            drive: resolved.drive,
            dir_path: FixedString::from(dir_path.as_str()),
            attr_filter,
            entries: matching_entries,
            index: 1,
            owner_psp: self.current_psp,
        };
        self.lfn_find_searches.push((handle, Box::new(state)));
        Ok((handle, first))
    }

    pub(crate) fn lfn_find_next(
        &mut self,
        handle: u16,
    ) -> std::result::Result<LfnFindSearchEntry, u16> {
        let Some((_, state)) = self
            .lfn_find_searches
            .iter_mut()
            .find(|(candidate, _)| *candidate == handle)
        else {
            return Err(6);
        };
        let Some(entry) = state.entries.get(state.index).copied() else {
            return Err(18);
        };
        state.index += 1;
        Ok(entry)
    }

    pub(crate) fn lfn_find_close(&mut self, handle: u16) -> bool {
        let Some(index) = self
            .lfn_find_searches
            .iter()
            .position(|(candidate, _)| *candidate == handle)
        else {
            return false;
        };
        self.lfn_find_searches.remove(index);
        true
    }

    pub(crate) fn close_lfn_searches_for_psp(&mut self, psp: u16) {
        self.lfn_find_searches
            .retain(|(_, state)| state.owner_psp != psp);
    }

    pub(crate) fn systemtime_to_filetime(time: std::time::SystemTime) -> u64 {
        const FILETIME_UNIX_EPOCH: i128 = 116_444_736_000_000_000;
        let ticks = match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => {
                FILETIME_UNIX_EPOCH
                    + i128::from(duration.as_secs()) * 10_000_000
                    + i128::from(duration.subsec_nanos() / 100)
            }
            Err(error) => {
                let duration = error.duration();
                FILETIME_UNIX_EPOCH
                    - i128::from(duration.as_secs()) * 10_000_000
                    - i128::from(duration.subsec_nanos() / 100)
            }
        };
        ticks.clamp(0, i128::from(u64::MAX)) as u64
    }

    fn encode_lfn_name(name: &str, capacity: usize) -> (Vec<u8>, bool) {
        // The LFN API returns the OEM/ANSI representation. This emulator's DOS
        // text contract is byte-preserving for U+0000..=U+00FF; Windows 95
        // reports an underscore and sets the conversion bit for other Unicode
        // scalars.
        let mut substituted = false;
        let mut bytes = Vec::with_capacity(name.len());
        for character in name.chars() {
            match u8::try_from(u32::from(character)) {
                Ok(byte) => bytes.push(byte),
                Err(_) => {
                    bytes.push(b'_');
                    substituted = true;
                }
            }
        }
        bytes.truncate(capacity.saturating_sub(1));
        (bytes, substituted)
    }

    /// Write the packed 318-byte DOS 7 find-data record. SI=0 selects a
    /// 64-bit FILETIME; SI=1 places packed DOS time/date in the low dword of
    /// each returned FILETIME. The available filesystem timestamp populates
    /// creation, access, and modification time; reserved fields are zero.
    pub(crate) fn write_lfn_find_data(
        &self,
        memory: &mut Memory,
        address: usize,
        entry: &LfnFindSearchEntry,
        time_format: u16,
    ) -> u16 {
        for offset in 0..LFN_FIND_DATA_SIZE {
            memory.write_u8(address + offset, 0);
        }

        memory.write_u32(address, u32::from(Self::attrs_to_dos_byte(&entry.attrs)));
        if time_format == 1 {
            let timestamps = [
                entry.creation_time,
                entry.last_access_time,
                entry.last_write_time,
            ];
            for (offset, timestamp) in [4usize, 12, 20].into_iter().zip(timestamps) {
                let (date, time) = Self::systemtime_to_dos_datetime(timestamp);
                let dos_datetime = (u32::from(date) << 16) | u32::from(time);
                memory.write_u32(address + offset, dos_datetime);
            }
        } else {
            for (offset, timestamp) in [4usize, 12, 20].into_iter().zip([
                entry.creation_time,
                entry.last_access_time,
                entry.last_write_time,
            ]) {
                let filetime = Self::systemtime_to_filetime(timestamp);
                memory.write_u32(address + offset, filetime as u32);
                memory.write_u32(address + offset + 4, (filetime >> 32) as u32);
            }
        }
        memory.write_u32(address + 28, (entry.attrs.size >> 32) as u32);
        memory.write_u32(address + 32, entry.attrs.size as u32);

        let (long_name, long_substituted) = Self::encode_lfn_name(entry.long_name.as_str(), 260);
        for (offset, byte) in long_name.into_iter().enumerate() {
            memory.write_u8(address + LFN_FIND_LONG_NAME + offset, byte);
        }

        let publish_short_name = entry.short_name.as_str() != entry.long_name.as_str();
        let (short_name, short_substituted) = if publish_short_name {
            Self::encode_lfn_name(entry.short_name.as_str(), 14)
        } else {
            (Vec::new(), false)
        };
        for (offset, byte) in short_name.into_iter().enumerate() {
            memory.write_u8(address + LFN_FIND_SHORT_NAME + offset, byte);
        }

        u16::from(long_substituted) | (u16::from(short_substituted) << 1)
    }

    /// Classify a failed FindFirst: an existing directory with no matching
    /// entries returns 18 (no more files), a missing directory returns 3.
    ///
    /// MDD-BUG-FLU-02715: Microsoft MS-DOS v2.0/source/SYSCALL.txt's
    /// FindFirst "Error Returns" explicitly assigns error_no_more_files to
    /// a search with no matches. the DOS model src/dos/drive_local.cpp agrees:
    /// localDrive::FindFirst delegates to FindNext after opening the search
    /// directory; FindNext reports DOSERR_NO_MORE_FILES on exhaustion.
    /// The earlier 2/18 FindFirst/FindNext split inferred from lists of legal
    /// error codes contradicted these oracles and made VB3 Dir raise error 53.
    ///
    /// The directory split mirrors `find_first`'s own, so the two always agree
    /// on which component is the directory.
    pub(crate) fn find_first_failure_error(&mut self, pattern: &str) -> u16 {
        let resolved = self.path_resolver.resolve_find_pattern(pattern);

        // MDD-BUG-CRUCIBLE-00645: an unmounted drive is INVALID DRIVE (15), not
        // "path not found". Checked before anything else, because every probe
        // below needs a drive to probe.
        let Some(drive) = self.drives.get_mut(&resolved.drive) else {
            return 15;
        };

        let pos = resolved.path.rfind('\\').unwrap_or(0);
        let dir_part = resolved.path[..pos].to_string();
        if dir_part.is_empty() {
            // The search is in the root, which always exists.
            return 18;
        }

        // MDD-BUG-CRUCIBLE-00645: ask the drive to enumerate. The distinction
        // between "the directory is there but nothing matched" and "the
        // directory could not be read" only exists in this error, which
        // `HostDrive::read_dir` used to discard.
        match drive.read_dir(&dir_part) {
            Ok(_) => 18,
            Err(err) => {
                let kind = err
                    .downcast_ref::<std::io::Error>()
                    .map(std::io::Error::kind);
                match kind {
                    Some(std::io::ErrorKind::PermissionDenied) => 5,
                    // NotFound, NotADirectory and anything else that means the
                    // search directory is not a readable directory.
                    _ => 3,
                }
            }
        }
    }

    /// Initialize find first search
    /// Returns search ID on success (stored in DTA for Find Next)
    pub(crate) fn find_first(&mut self, pattern: &str, attr_filter: u8) -> Result<u32> {
        use gp64_types::FixedString;

        let resolved = self.path_resolver.resolve_find_pattern(pattern);

        // Extract file pattern from path
        let pos = resolved.path.rfind('\\').unwrap_or(0);
        let file_pat = &resolved.path[pos + 1..];

        // Extract directory part to query drive
        // An exclusive volume-label search queries the drive, even for a
        // FCB with C:????????.??? while C:\PARITY is current). the DOS model's
        // localDrive::FindFirst returns GetLabel() for sAttr == DOS_ATTR_VOLUME
        // independently of _dir and the search mask. Other searches retain both.
        let dir_part = if attr_filter == ATTR_VOLUME_LABEL {
            String::new()
        } else {
            resolved.path[..pos].to_string()
        };

        // Collect matching entries into a temporary Vec, then convert
        let mut matching_entries_tmp = Vec::new();

        if let Some(drive) = self.drives.get_mut(&resolved.drive) {
            // Short names come from NTFS on Windows (when this is a host-backed
            // drive) so FindFirst agrees with GetCurrentDirectory and with host
            // path resolution; otherwise the emulator's own assignment. See
            // HLD 2026.06.14 (8.3 short-name divergence).
            for entry in enumerate_dos_visible(drive.as_mut(), &dir_part) {
                if (attr_filter == ATTR_VOLUME_LABEL
                    || Self::matches_pattern_static(file_pat, &entry.dos_name))
                    && Self::matches_attr_filter(&entry.attrs, attr_filter)
                {
                    matching_entries_tmp.push((entry.dos_name, entry.attrs));
                }
            }
        }

        // A volume-label-exclusive search that matched nothing reports nothing.
        //
        // This used to synthesize a constant `GP64` label for any drive that
        // existed, so an unlabelled host-backed drive answered with a name no
        // real disk carries. A host directory has no volume-label directory
        // entry, so DOS returns "no more files" for a label-only search.
        //
        // A drive that carries a REAL label still reports it: such a label comes
        // back from `enumerate_dos_visible` above and never reaches this point.
        // MDD-BUG-ANVIL-00137.
        if matching_entries_tmp.is_empty() {
            bail!("No matching files");
        }

        // Convert to fixed-capacity entries. The search buffer is bounded, so cap
        // the match set at capacity instead of panicking FixedVec::push on a
        // directory with more matches than fit. MDD-BUG-FLUXHOMEARPA-00008.
        let mut matching_entries = crate::FindSearchEntries::new_boxed();
        for (name, attrs) in matching_entries_tmp {
            if matching_entries.is_full() {
                tracing::debug!(
                    target: "gp64::dos::find",
                    "find_first: match set capped at {} entries",
                    matching_entries.capacity()
                );
                break;
            }
            matching_entries.push((FixedString::from(name.as_str()), attrs));
        }

        // Create search state and assign ID
        let search_id = self.next_search_id;
        self.next_search_id = self.next_search_id.wrapping_add(1);
        if self.next_search_id == 0 {
            self.next_search_id = 1; // Keep 0 as "invalid"
        }

        let state = Box::new(FindSearchState {
            pattern: FixedString::from(file_pat),
            drive: resolved.drive,
            dir_path: FixedString::from(dir_part.as_str()),
            attr_filter,
            entries: matching_entries,
            index: 0,
        });

        // Evict oldest search if at capacity (programs may not exhaust all searches)
        if self.find_searches.is_full() {
            tracing::debug!(
                target: "gp64::dos::find",
                "find_first: evicting oldest search (capacity {} full)",
                self.find_searches.capacity()
            );
            self.find_searches.remove(0);
        }
        self.find_searches.push((search_id, state));

        Ok(search_id)
    }

    /// Get entry from a search by ID, advancing the index
    pub(crate) fn find_get_entry(&mut self, search_id: u32) -> Option<FindEntry> {
        let state = self
            .find_searches
            .iter()
            .find(|(k, _)| *k == search_id)
            .map(|(_, v)| v)?;

        if state.index >= state.entries.len() {
            // Search exhausted - remove it to prevent memory leak
            self.find_searches.retain(|(k, _)| *k != search_id);
            return None;
        }

        let (name, attrs) = &state.entries[state.index];
        let (date, time) = Self::systemtime_to_dos_datetime(attrs.modified);

        let entry = FindEntry {
            name: name.to_string(),
            size: attrs.size as u32,
            attr: Self::attrs_to_dos_byte(attrs),
            time,
            date,
        };

        Some(entry)
    }

    /// Advance to next entry in a search
    pub(crate) fn find_advance(&mut self, search_id: u32) {
        if let Some((_, state)) = self.find_searches.iter_mut().find(|(k, _)| *k == search_id) {
            state.index += 1;
        }
    }

    /// Match a DOS 7 long-name filespec against either the primary long name
    /// or its generated 8.3 alias. LFN searches use the flexible Windows 95
    /// wildcard form over the complete name; unlike the old AH=4Eh matcher,
    /// a pattern such as `*mid*` may span the base name and extension.
    pub(crate) fn matches_lfn_pattern(pattern: &str, filename: &str) -> bool {
        let pattern_upper = pattern.to_uppercase();
        let filename_upper = filename.to_uppercase();
        pattern_upper == "*"
            || pattern_upper == "*.*"
            || Self::wildcard_match(&pattern_upper, &filename_upper)
    }

    /// Static pattern matching (doesn't need self)
    /// Implements DOS wildcard semantics:
    /// - ? matches exactly one character
    /// - * matches zero or more characters (fills remaining in 8.3 context)
    /// - Pattern without dot only matches files without extension (unless pattern ends with *)
    /// - Pattern ending with . matches files without extension
    pub(crate) fn matches_pattern_static(pattern: &str, filename: &str) -> bool {
        let pattern_upper = pattern.to_uppercase();
        let name_upper = filename.to_uppercase();

        // Handle *.* pattern (matches everything)
        if pattern_upper == "*.*" {
            return true;
        }

        // Handle * pattern alone (matches everything, including files with extensions)
        if pattern_upper == "*" {
            return true;
        }

        // Check if pattern has a dot
        let pattern_has_dot = pattern_upper.contains('.');

        // Split pattern and filename into base and extension
        let (pat_base, pat_ext) = if let Some(dot_pos) = pattern_upper.rfind('.') {
            (&pattern_upper[..dot_pos], &pattern_upper[dot_pos + 1..])
        } else {
            (pattern_upper.as_str(), "")
        };

        let (name_base, name_ext) = if let Some(dot_pos) = name_upper.rfind('.') {
            (&name_upper[..dot_pos], &name_upper[dot_pos + 1..])
        } else {
            (name_upper.as_str(), "")
        };

        // Match base part
        if !Self::wildcard_match(pat_base, name_base) {
            return false;
        }

        // Extension matching depends on pattern format
        if pattern_has_dot {
            // Pattern has a dot - match extension (empty pattern ext matches empty file ext)
            if !Self::wildcard_match(pat_ext, name_ext) {
                return false;
            }
        } else {
            // Pattern has no dot
            // If pattern ends with *, it matches any extension
            // Otherwise, file must have no extension
            if !pat_base.ends_with('*') && !name_ext.is_empty() {
                return false;
            }
        }

        true
    }

    /// Wildcard matching helper for * and ? patterns
    pub(crate) fn wildcard_match(pattern: &str, text: &str) -> bool {
        #[cfg(test)]
        let matched = Self::wildcard_match_impl(pattern, text, None);
        #[cfg(not(test))]
        let matched = Self::wildcard_match_impl(pattern, text);
        matched
    }

    #[cfg(test)]
    pub(crate) fn wildcard_match_counted(pattern: &str, text: &str) -> (bool, usize) {
        let mut steps = 0;
        let matched = Self::wildcard_match_impl(pattern, text, Some(&mut steps));
        (matched, steps)
    }

    /// Match Unicode scalar values without recursion or suffix allocation.
    ///
    /// The most recent `*` is the only backtracking point needed. A mismatch
    /// advances that star by one text character and retries the following
    /// pattern suffix. Consecutive stars are collapsed while scanning.
    fn wildcard_match_impl(
        pattern: &str,
        text: &str,
        #[cfg(test)] mut steps: Option<&mut usize>,
    ) -> bool {
        let mut pattern_index = 0;
        let mut text_index = 0;
        let mut star_pattern_index = None;
        let mut star_text_index = 0;

        while text_index < text.len() {
            #[cfg(test)]
            if let Some(counter) = steps.as_deref_mut() {
                *counter += 1;
            }

            let Some((text_char, next_text_index)) = Self::wildcard_next_char(text, text_index)
            else {
                return false;
            };
            if let Some((pattern_char, next_pattern_index)) =
                Self::wildcard_next_char(pattern, pattern_index)
            {
                if pattern_char == '?' || pattern_char == text_char {
                    pattern_index = next_pattern_index;
                    text_index = next_text_index;
                    continue;
                }

                if pattern_char == '*' {
                    pattern_index = next_pattern_index;
                    while let Some(('*', next_star_index)) =
                        Self::wildcard_next_char(pattern, pattern_index)
                    {
                        #[cfg(test)]
                        if let Some(counter) = steps.as_deref_mut() {
                            *counter += 1;
                        }
                        pattern_index = next_star_index;
                    }
                    if pattern_index == pattern.len() {
                        return true;
                    }
                    star_pattern_index = Some(pattern_index);
                    star_text_index = text_index;
                    continue;
                }
            }

            if let Some(retry_pattern_index) = star_pattern_index {
                let Some((_, next_star_text_index)) =
                    Self::wildcard_next_char(text, star_text_index)
                else {
                    return false;
                };
                star_text_index = next_star_text_index;
                text_index = next_star_text_index;
                pattern_index = retry_pattern_index;
                continue;
            }

            return false;
        }

        while let Some((pattern_char, next_pattern_index)) =
            Self::wildcard_next_char(pattern, pattern_index)
        {
            if pattern_char != '*' {
                return false;
            }
            #[cfg(test)]
            if let Some(counter) = steps.as_deref_mut() {
                *counter += 1;
            }
            pattern_index = next_pattern_index;
        }
        true
    }

    #[inline]
    fn wildcard_next_char(value: &str, index: usize) -> Option<(char, usize)> {
        debug_assert!(value.is_char_boundary(index));
        let character = value.get(index..)?.chars().next()?;
        Some((character, index + character.len_utf8()))
    }

    /// Check if file attributes match the search filter
    /// DOS Find First/Next attribute filter rules:
    /// - Normal files (attr=0) are always returned
    /// - Hidden (0x02), System (0x04), Directory (0x10) only returned if requested
    /// - Volume label (0x08) only returned if ONLY volume label bit is set
    pub(crate) fn matches_attr_filter(
        attrs: &crate::filesystem::FileAttributes,
        filter: u8,
    ) -> bool {
        // DOS attribute filter behavior:
        // - Volume label (0x08) is EXCLUSIVE when it's the ONLY attribute bit:
        //   filter=0x08 means ONLY volume labels are returned
        // - If volume bit is combined with other bits (e.g., 0xFF), treat as inclusive
        // - Hidden (0x02), System (0x04), Directory (0x10) are INCLUSIVE: when set,
        //   those files are returned IN ADDITION to normal files

        // Check if volume label bit is set alone (exclusive mode)
        // Volume is exclusive when set without other inclusive attributes
        let volume_exclusive = (filter & ATTR_VOLUME_LABEL) != 0
            && (filter & (ATTR_HIDDEN | ATTR_SYSTEM | ATTR_DIRECTORY)) == 0;

        if volume_exclusive {
            // Only return volume labels when volume bit is set exclusively
            return attrs.volume_label;
        }

        // Volume labels only returned if volume bit is set in filter
        if attrs.volume_label && (filter & ATTR_VOLUME_LABEL) == 0 {
            return false;
        }

        // Hidden files only returned if hidden bit set in filter
        if attrs.hidden && (filter & ATTR_HIDDEN) == 0 {
            return false;
        }
        // System files only returned if system bit set in filter
        if attrs.system && (filter & ATTR_SYSTEM) == 0 {
            return false;
        }
        // Directories only returned if directory bit set in filter
        if attrs.directory && (filter & ATTR_DIRECTORY) == 0 {
            return false;
        }
        true
    }

    /// Convert FileAttributes to DOS attribute byte
    pub(crate) fn attrs_to_dos_byte(attrs: &crate::filesystem::FileAttributes) -> u8 {
        let mut result = 0u8;
        if attrs.read_only {
            result |= ATTR_READ_ONLY;
        }
        if attrs.hidden {
            result |= ATTR_HIDDEN;
        }
        if attrs.system {
            result |= ATTR_SYSTEM;
        }
        if attrs.volume_label {
            result |= ATTR_VOLUME_LABEL;
        }
        if attrs.directory {
            result |= ATTR_DIRECTORY;
        }
        if attrs.archive {
            result |= ATTR_ARCHIVE;
        }
        result
    }

    /// Write DTA entry for find first/next
    /// The search_id is stored in the first 4 bytes of the reserved area for Find Next
    pub(crate) fn write_dta_entry(&self, memory: &mut Memory, search_id: u32, entry: &FindEntry) {
        // DTA entry is 43 bytes (0x2B): ensure we don't overflow segment boundary
        const DTA_ENTRY_SIZE: u32 = 0x2B;
        if u32::from(self.dta_offset) > self.dta_segment_limit - DTA_ENTRY_SIZE + 1 {
            // DTA too close to segment boundary - truncate writes to segment limit
            #[cfg(debug_assertions)]
            tracing::warn!(
                segment = format_args!("{:04X}", self.dta_segment),
                offset = format_args!("{:04X}", self.dta_offset),
                "DOS DTA entry would overflow segment"
            );
            return; // Skip write to prevent overflow
        }

        let dta_addr = self.dta_linear;

        // DTA structure for find first/next (see DTA_* constants):
        // Offset         Size  Description
        // 0x00            4    Search ID (for Find Next to resume)
        // 0x04           17    Reserved (zeroed)
        // DTA_ATTRIBUTE   1    File attributes
        // DTA_TIME        2    File time
        // DTA_DATE        2    File date
        // DTA_SIZE        4    File size
        // DTA_FILENAME   13    Filename (8.3 format, null-terminated)

        // Store search ID at offset 0 for Find Next
        memory.write_u32(dta_addr, search_id);

        // Zero the rest of the reserved area (offset 4-20)
        for i in 4..21 {
            memory.write_u8(dta_addr + i, 0);
        }

        // Attribute
        memory.write_u8(dta_addr + DTA_ATTRIBUTE, entry.attr);

        // Time (packed: hours:5, minutes:6, seconds/2:5)
        memory.write_u16(dta_addr + DTA_TIME, entry.time);

        // Date (packed: year-1980:7, month:4, day:5)
        memory.write_u16(dta_addr + DTA_DATE, entry.date);

        // File size (32-bit)
        memory.write_u32(dta_addr + DTA_SIZE, entry.size);

        // Filename (8.3 format, uppercase, null-terminated)
        // Names are already converted to 8.3 format during find_first()
        let dos_name = entry.name.to_uppercase();
        for (i, byte) in dos_name.bytes().take(12).enumerate() {
            memory.write_u8(dta_addr + DTA_FILENAME + i, byte);
        }
        memory.write_u8(dta_addr + DTA_FILENAME + dos_name.len().min(12), 0);
    }
}
