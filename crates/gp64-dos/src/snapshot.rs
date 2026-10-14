//! DOS state snapshot for save/restore.
//!
//! The `Dos` struct contains trait objects (`Box<dyn VirtualDrive>`,
//! `Box<dyn VirtualFile>`) that prevent it from deriving `Clone`. This module
//! provides `DosSnapshot` which captures only the serializable/cloneable
//! fields needed to restore a functioning DOS environment.

use crate::country::DosCountryInfo;
use crate::dpmi::Dpmi;
use crate::files::{FileHandle, FileLock};
use crate::memory::DosMem;
use crate::network::NetworkHandler;
use crate::FindSearchState;
use crate::LfnFindSearchState;
use crate::PendingCriticalError;
use crate::ProcessState;
use gp64_memory::xms::Xms;
use gp64_types::{FixedString, FixedVec};

/// Metadata for a single SFT (System File Table) entry, without the
/// `Box<dyn VirtualFile>` trait object. Captures enough information to
/// reconnect file handles on restore.
#[derive(Clone, Debug)]
pub struct SftMetadata {
    pub path: FixedString<260>,
    pub position: u64,
    pub ref_count: u16,
    pub locks: Vec<FileLock>,
    pub share_mode: u8,
    pub access_mode: u8,
    pub owner_psp: u16,
    pub last_read_end: u64,
    pub seq_read_bytes: u32,
    /// What the entry was backed by, so a synthetic handle can be rebuilt from
    /// its source instead of reopened through a drive it never lived on.
    pub backing: crate::files::SftBacking,
}

/// Snapshot of the DOS file table: handles array, SFT metadata, next_handle,
/// and the AH=67h-expanded handle limit.
#[derive(Clone, Debug)]
pub struct FileTableSnapshot {
    pub handles: [Option<FileHandle>; 256],
    pub sft: Vec<Option<SftMetadata>>,
    pub next_handle: u16,
    /// Per-process handle limit (JFT size). Grown by INT 21h AH=67h
    /// (SetHandleCount); must survive save/restore or a guest that raised it
    /// reverts to the default and fails to open handles that were valid before
    /// the snapshot (MDD-BUG-CRUCIBLE-00059).
    pub handle_limit: u16,
}

/// Snapshot of PathResolver state.
#[derive(Clone, Debug)]
pub struct PathResolverSnapshot {
    pub current_drive: u8,
    pub current_dirs: Vec<(u8, FixedString<260>)>,
}

/// One runtime SUBST mapping: its drive, the host directory it maps, and the
/// DOS pathname an LFN caller supplied for true-name results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubstMapping {
    pub drive: u8,
    pub host_root: std::path::PathBuf,
    pub target: Option<String>,
}

/// Complete DOS state snapshot (everything except trait objects and
/// non-essential runtime buffers).
#[derive(Clone)]
pub struct DosSnapshot {
    // Scalar fields
    pub version_major: u8,
    pub version_minor: u8,
    pub env_segment: u16,
    pub dta_segment: u16,
    pub dta_offset: u16,
    pub dta_linear: usize,
    pub dta_segment_limit: u32,
    pub current_psp: u16,
    pub temp_file_counter: u32,
    pub last_exit_code: u8,
    pub last_termination_type: u8,
    pub dpmi_program_exit_code: Option<u8>,
    /// One-shot EXEC image-load event, consumed by the root epoch detector.
    ///
    /// Snapshotted so a rewind cannot re-emit an event from an abandoned
    /// timeline, nor lose one that live execution consumed after the snapshot
    /// was taken (MDD-BUG-CRUCIBLE-00650).
    pub last_exec_image_cs: Option<u16>,
    pub memory_strategy: u8,
    pub umb_linked: bool,
    pub ctrl_break_check: bool,
    pub pending_extended_scancode: Option<u8>,
    pub extended_error: (u16, u8, u8, u8),
    pub verify_flag: bool,
    pub hdd_seek_clicks: u8,
    pub hdd_sound_enabled: bool,
    pub emulated_date: (u16, u8, u8, u8),

    // Cloneable sub-structures
    pub dpmi: Dpmi,
    pub dos_mem: DosMem,
    pub xms: Xms,
    pub file_table: FileTableSnapshot,
    pub path_resolver: PathResolverSnapshot,
    pub(crate) process_stack: FixedVec<ProcessState, 16>,
    pub(crate) resident_returns: FixedVec<(u16, ProcessState), 8>,
    pub(crate) find_searches: FixedVec<(u32, Box<FindSearchState>), 32>,
    pub next_search_id: u32,
    pub(crate) lfn_find_searches: FixedVec<(u16, Box<LfnFindSearchState>), 32>,
    pub next_lfn_search_handle: u16,
    pub fcb_search_id: u32,
    /// The result layout of the active FCB search, which `AH=12h` cannot
    /// re-derive: it travels with `fcb_search_id` so a rewound search keeps
    /// publishing the record its caller asked for (MDD-BUG-CRU-01405).
    pub(crate) fcb_search_extended: bool,
    pub(crate) fcb_search_attr: u8,
    pub pending_critical_error: Option<PendingCriticalError>,
    pub std_handle_aliases: [Option<u16>; 256],
    pub network: NetworkHandler,
    pub country_info: DosCountryInfo,
    // MDD-BUG-FLUXHOMEARPA-00015: must be snapshotted/restored like other state.
    // The payloads are immutable, so the snapshot shares the live `Arc`s rather
    // checkpoint and restore both run on the emulator thread
    // (MDD-BUG-KIL-00780). The snapshot is in-memory only; nothing serializes it.
    pub virtual_files: std::collections::BTreeMap<String, std::sync::Arc<[u8]>>,
    // Cooked (line-mode) stdin state used by INT 21h/AH=3Fh.
    // MDD-BUG-CRUCIBLE-00376: guest-visible, so it must travel with the
    // snapshot. Omitting it let a rewound guest read bytes typed in the
    // abandoned timeline, lose a snapshot-time prefix, or replay the wrong
    // remainder of a partially drained line.
    pub stdin_line_acc: Vec<u8>,
    pub stdin_line_buf: Vec<u8>,
    // Runtime SUBST drives. Unlike startup mounts they change while the guest
    // runs, so a restore must return them to the captured set: otherwise a
    // mapping created after the capture survives and one deleted after it
    // stays gone (MDD-BUG-KIL-00775).
    pub subst_mappings: Vec<SubstMapping>,
}

impl crate::Dos {
    /// Capture a snapshot of the DOS state. Skips trait objects (drives,
    /// open file handles, stdout_capture, glide_ovl_data).
    pub fn snapshot(&self) -> DosSnapshot {
        DosSnapshot {
            version_major: self.version_major,
            version_minor: self.version_minor,
            env_segment: self.env_segment,
            dta_segment: self.dta_segment,
            dta_offset: self.dta_offset,
            dta_linear: self.dta_linear,
            dta_segment_limit: self.dta_segment_limit,
            current_psp: self.current_psp,
            temp_file_counter: self.temp_file_counter,
            last_exit_code: self.last_exit_code,
            last_termination_type: self.last_termination_type,
            dpmi_program_exit_code: self.dpmi_program_exit_code,
            last_exec_image_cs: self.last_exec_image_cs,
            memory_strategy: self.memory_strategy,
            umb_linked: self.umb_linked,
            ctrl_break_check: self.ctrl_break_check,
            pending_extended_scancode: self.pending_extended_scancode,
            stdin_line_acc: self.stdin_line_acc.clone(),
            stdin_line_buf: self.stdin_line_buf.clone(),
            extended_error: self.extended_error,
            verify_flag: self.verify_flag,
            hdd_seek_clicks: self.hdd_seek_clicks,
            hdd_sound_enabled: self.hdd_sound_enabled,
            emulated_date: self.emulated_date,

            dpmi: (*self.dpmi).clone(),
            dos_mem: self.dos_mem.clone(),
            xms: self.xms.clone(),
            file_table: self.file_table.snapshot(),
            path_resolver: self.path_resolver.snapshot(),
            process_stack: self.process_stack.clone(),
            resident_returns: self.resident_returns.clone(),
            find_searches: self.find_searches.clone(),
            next_search_id: self.next_search_id,
            lfn_find_searches: self.lfn_find_searches.clone(),
            next_lfn_search_handle: self.next_lfn_search_handle,
            fcb_search_id: self.fcb_search_id,
            fcb_search_extended: self.fcb_search_extended,
            fcb_search_attr: self.fcb_search_attr,
            pending_critical_error: self.pending_critical_error.clone(),
            std_handle_aliases: self.file_table.std_handle_aliases,
            network: self.network.clone(),
            country_info: self.country_info.clone(),
            virtual_files: self.virtual_files.clone(),
            subst_mappings: self
                .subst_drives
                .iter()
                .filter_map(|&drive| {
                    Some(SubstMapping {
                        drive,
                        host_root: self.subst_roots.get(&drive)?.clone(),
                        target: self.subst_targets.get(&drive).cloned(),
                    })
                })
                .collect(),
        }
    }

    /// Restore DOS state from a snapshot. Startup drive mounts and open file
    /// handles are NOT restored (they must be reconnected separately by the
    /// caller); runtime SUBST drives are.
    pub fn restore(&mut self, snap: &DosSnapshot) {
        self.restore_subst_mappings(&snap.subst_mappings);
        self.version_major = snap.version_major;
        self.version_minor = snap.version_minor;
        self.env_segment = snap.env_segment;
        self.dta_segment = snap.dta_segment;
        self.dta_offset = snap.dta_offset;
        self.dta_linear = snap.dta_linear;
        self.dta_segment_limit = snap.dta_segment_limit;
        self.current_psp = snap.current_psp;
        self.temp_file_counter = snap.temp_file_counter;
        self.last_exit_code = snap.last_exit_code;
        self.last_termination_type = snap.last_termination_type;
        self.dpmi_program_exit_code = snap.dpmi_program_exit_code;
        self.last_exec_image_cs = snap.last_exec_image_cs;
        self.memory_strategy = snap.memory_strategy;
        self.umb_linked = snap.umb_linked;
        self.ctrl_break_check = snap.ctrl_break_check;
        self.pending_extended_scancode = snap.pending_extended_scancode;
        self.stdin_line_acc = snap.stdin_line_acc.clone();
        self.stdin_line_buf = snap.stdin_line_buf.clone();
        self.extended_error = snap.extended_error;
        self.verify_flag = snap.verify_flag;
        self.hdd_seek_clicks = snap.hdd_seek_clicks;
        self.hdd_sound_enabled = snap.hdd_sound_enabled;
        self.emulated_date = snap.emulated_date;

        *self.dpmi = snap.dpmi.clone();
        self.dos_mem = snap.dos_mem.clone();
        self.xms = snap.xms.clone();
        self.file_table.restore(&snap.file_table);
        self.path_resolver.restore(&snap.path_resolver);
        self.process_stack = snap.process_stack.clone();
        self.resident_returns = snap.resident_returns.clone();
        self.find_searches = snap.find_searches.clone();
        self.next_search_id = snap.next_search_id;
        self.lfn_find_searches = snap.lfn_find_searches.clone();
        self.next_lfn_search_handle = snap.next_lfn_search_handle;
        self.fcb_search_id = snap.fcb_search_id;
        self.fcb_search_extended = snap.fcb_search_extended;
        self.fcb_search_attr = snap.fcb_search_attr;
        self.pending_critical_error = snap.pending_critical_error.clone();
        self.file_table.std_handle_aliases = snap.std_handle_aliases;
        self.network = snap.network.clone();
        self.country_info = snap.country_info.clone();
        self.virtual_files = snap.virtual_files.clone();
    }

    /// Returns the runtime SUBST drives to the captured set.
    ///
    /// Every live SUBST drive is dropped and the captured ones recreated, so a
    /// mapping made after the capture disappears and one deleted after it
    /// comes back. Startup mounts are never touched: a letter they own is not
    /// taken over, which `subst_create` also refuses.
    fn restore_subst_mappings(&mut self, mappings: &[SubstMapping]) {
        for drive in std::mem::take(&mut self.subst_drives) {
            self.drives.remove(&drive);
        }
        self.subst_targets.clear();
        self.subst_roots.clear();
        for mapping in mappings {
            if self.drives.contains_key(&mapping.drive) {
                continue;
            }
            self.drives.insert(
                mapping.drive,
                Box::new(crate::HostDrive::new(mapping.host_root.clone())),
            );
            self.subst_drives.insert(mapping.drive);
            self.subst_roots
                .insert(mapping.drive, mapping.host_root.clone());
            if let Some(target) = &mapping.target {
                self.subst_targets.insert(mapping.drive, target.clone());
            }
        }
    }

    /// Reopen files from a snapshot after restore.
    ///
    /// Startup mounts persist across save/restore and `restore` has already
    /// returned the SUBST drives to the captured set, so this reopens files
    /// through the drives now mounted.
    pub fn reopen_files_from_snapshot(&mut self, snap: &DosSnapshot) {
        let synthetic = crate::files::SyntheticSources {
            glide_ovl: self.glide_ovl_data.as_ref(),
            virtual_files: &self.virtual_files,
        };
        self.file_table
            .reopen_files_from_snapshot(&snap.file_table, &mut self.drives, &synthetic);
    }
}

#[cfg(test)]
mod tests {
    use crate::Dos;

    /// MDD-BUG-KIL-00775: runtime SUBST drives return to the captured set. A
    /// mapping created after the capture must vanish, and one deleted after it
    /// must come back with its root and LFN target.
    #[test]
    fn restore_returns_runtime_subst_drives_to_the_captured_set() {
        const D: u8 = 3;
        const E: u8 = 4;
        let root_d = std::env::temp_dir().join("gp64-subst-d");
        let root_e = std::env::temp_dir().join("gp64-subst-e");

        let mut dos = Dos::new();
        dos.subst_create_with_target(D, root_d.clone(), Some("C:\\GAME".to_string()))
            .expect("D: is free");
        let snapshot = dos.snapshot();

        dos.subst_create_with_target(E, root_e, None)
            .expect("E: is free");
        dos.subst_delete(D)
            .expect("D: is a SUBST drive and not current");
        assert!(!dos.drives.contains_key(&D) && dos.drives.contains_key(&E));

        dos.restore(&snapshot);

        assert!(dos.drives.contains_key(&D), "the deleted mapping is back");
        assert!(dos.subst_drives.contains(&D));
        assert_eq!(dos.subst_roots.get(&D), Some(&root_d));
        assert_eq!(
            dos.subst_targets.get(&D).map(String::as_str),
            Some("C:\\GAME")
        );
        assert!(
            !dos.drives.contains_key(&E),
            "the post-capture mapping is gone"
        );
        assert!(!dos.subst_drives.contains(&E));
        assert!(
            dos.drives.contains_key(&2),
            "the startup C: mount is untouched"
        );
    }

    #[test]
    fn restore_clears_dpmi_exit_latch_absent_from_snapshot() {
        let mut dos = Dos::new();
        let snapshot = dos.snapshot();

        dos.dpmi_program_exit_code = Some(42);
        dos.restore(&snapshot);

        assert_eq!(dos.dpmi_program_exit_code, None);
    }

    #[test]
    fn restore_reinstates_dpmi_exit_latch_consumed_after_snapshot() {
        let mut dos = Dos::new();
        dos.dpmi_program_exit_code = Some(42);
        let snapshot = dos.snapshot();

        assert_eq!(dos.dpmi_program_exit_code.take(), Some(42));
        dos.restore(&snapshot);

        assert_eq!(dos.dpmi_program_exit_code, Some(42));
    }

    // ---- MDD-BUG-CRUCIBLE-00376: cooked stdin must travel with the snapshot ----

    /// Empty-to-mutated: a rewind must discard input typed after the snapshot,
    /// including a half-typed line still accumulating. Without this the guest
    /// read bytes from the abandoned timeline.
    #[test]
    fn restore_discards_cooked_stdin_typed_after_the_snapshot_crucible376() {
        let mut dos = Dos::new();
        let snapshot = dos.snapshot();

        dos.stdin_line_acc = b"half-typed".to_vec();
        dos.stdin_line_buf = b"READY
"
        .to_vec();
        dos.restore(&snapshot);

        assert!(
            dos.stdin_line_acc.is_empty(),
            "partial accumulation from the abandoned timeline survived: {:?}",
            dos.stdin_line_acc
        );
        assert!(
            dos.stdin_line_buf.is_empty(),
            "completed line from the abandoned timeline survived: {:?}",
            dos.stdin_line_buf
        );
    }

    /// Populated-to-consumed: a line captured at snapshot time, then partially
    /// drained by AH=3Fh, must come back WHOLE. Without this the guest lost the
    /// snapshot-time prefix and replayed only the remainder.
    #[test]
    fn restore_reinstates_cooked_stdin_drained_after_the_snapshot_crucible376() {
        let mut dos = Dos::new();
        dos.stdin_line_acc = b"partial".to_vec();
        dos.stdin_line_buf = b"DIR *.EXE
"
        .to_vec();
        let snapshot = dos.snapshot();

        // AH=3Fh hands out a prefix and keeps the rest.
        dos.stdin_line_buf.drain(..4);
        dos.stdin_line_acc.clear();
        dos.restore(&snapshot);

        assert_eq!(
            dos.stdin_line_buf,
            b"DIR *.EXE
"
            .to_vec(),
            "the drained prefix was not restored"
        );
        assert_eq!(
            dos.stdin_line_acc,
            b"partial".to_vec(),
            "the cleared accumulator was not restored"
        );
    }

    /// The snapshot must be an independent copy: mutating the live buffers
    /// afterwards must not reach back into it.
    #[test]
    fn cooked_stdin_snapshot_is_an_independent_copy_crucible376() {
        let mut dos = Dos::new();
        dos.stdin_line_buf = b"ORIGINAL".to_vec();
        let snapshot = dos.snapshot();

        dos.stdin_line_buf.clear();
        dos.stdin_line_buf.extend_from_slice(b"OVERWRITTEN");

        assert_eq!(snapshot.stdin_line_buf, b"ORIGINAL".to_vec());
    }

    // MDD-BUG-CRUCIBLE-00650: `last_exec_image_cs` is the same shape of one-shot
    // latch as `dpmi_program_exit_code` above and needs the same two round
    // trips. It was absent from the snapshot entirely.

    /// MDD-BUG-CRU-01405: a restore rewinds the FCB search's result layout
    /// with its id, so `AH=12h` keeps publishing the record the search was
    /// started for, not the abandoned timeline's.
    #[test]
    fn restore_rewinds_the_fcb_search_result_layout() {
        let mut dos = Dos::new();
        dos.fcb_search_extended = true;
        dos.fcb_search_attr = 0x16;
        let snapshot = dos.snapshot();

        dos.fcb_search_extended = false;
        dos.fcb_search_attr = 0;
        dos.restore(&snapshot);

        assert!(
            dos.fcb_search_extended,
            "extended layout lost across restore"
        );
        assert_eq!(dos.fcb_search_attr, 0x16);
    }

    /// Rewinding to before an EXEC must not carry the image-load event back
    /// from the abandoned timeline.
    #[test]
    fn restore_clears_exec_image_latch_absent_from_snapshot() {
        let mut dos = Dos::new();
        let snapshot = dos.snapshot();

        dos.last_exec_image_cs = Some(0x1234);
        dos.restore(&snapshot);

        assert_eq!(
            dos.last_exec_image_cs, None,
            "a rewind re-emitted an EXEC from the abandoned timeline"
        );
    }

    /// Rewinding to a snapshot whose event live execution has since consumed
    /// must bring the event back.
    #[test]
    fn restore_reinstates_exec_image_latch_consumed_after_snapshot() {
        let mut dos = Dos::new();
        dos.last_exec_image_cs = Some(0x1234);
        let snapshot = dos.snapshot();

        assert_eq!(dos.last_exec_image_cs.take(), Some(0x1234));
        dos.restore(&snapshot);

        assert_eq!(
            dos.last_exec_image_cs,
            Some(0x1234),
            "a rewind lost an EXEC the live timeline had consumed"
        );
    }

    /// The restored event must be delivered EXACTLY once, not once per read.
    ///
    /// This models the root consumer rather than driving it: the real one is
    /// `Emulator::…` in `src/emulator/execution.rs`, which needs a whole
    /// emulator to construct. What it does to this field is a single `take()`,
    /// which is what is reproduced here — so this pins the latch semantics the
    /// consumer depends on, not the consumer itself.
    #[test]
    fn a_restored_exec_image_latch_is_consumed_exactly_once() {
        let mut dos = Dos::new();
        dos.last_exec_image_cs = Some(0x0ABC);
        let snapshot = dos.snapshot();

        dos.last_exec_image_cs = None;
        dos.restore(&snapshot);

        assert_eq!(
            dos.last_exec_image_cs.take(),
            Some(0x0ABC),
            "first read must see the restored event"
        );
        assert_eq!(
            dos.last_exec_image_cs.take(),
            None,
            "the event must not survive being consumed"
        );
    }
}
