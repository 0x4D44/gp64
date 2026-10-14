//! DOS file system support
//!
//! Implements file handle management and path resolution for DOS programs
//!
//! Uses a System File Table (SFT) architecture where actual file state is stored
//! in SFT entries, and file handles point to these entries. This allows duplicated
//! handles (via INT 21h/45h or 46h) to share file position, matching DOS behavior.

use crate::filesystem::VirtualFile;
use std::collections::BTreeMap;
use std::path::Path;

/// Stable identity of one backend file.
///
/// Host drives use the contained canonical host path so long names, generated
/// short names, case variants, and DOS path variants converge. Other drive
/// backends use a drive-scoped normalized DOS path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity(String);

impl FileIdentity {
    pub(crate) fn for_resolved_path(drive: u8, path: &str, host_path: Option<&Path>) -> Self {
        if let Some(host_path) = host_path {
            #[cfg(windows)]
            let host_key = host_path.to_string_lossy().to_uppercase();
            #[cfg(not(windows))]
            let host_key = host_path.to_string_lossy().into_owned();
            return Self(format!("host\0{host_key}"));
        }

        Self(format!(
            "drive\0{drive}\0{}",
            Self::normalize_dos_path(path)
        ))
    }

    fn from_dos_path(path: &str) -> Self {
        let bytes = path.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            let drive = bytes[0].to_ascii_uppercase().wrapping_sub(b'A');
            if drive < 26 {
                return Self::for_resolved_path(drive, &path[2..], None);
            }
        }

        Self(format!("guest\0{}", Self::normalize_dos_path(path)))
    }

    fn normalize_dos_path(path: &str) -> String {
        let mut components = Vec::new();
        for component in path.replace('/', "\\").split('\\') {
            match component {
                "" | "." => {}
                ".." => {
                    components.pop();
                }
                _ => components.push(component.to_ascii_uppercase()),
            }
        }
        format!(r"\{}", components.join(r"\"))
    }
}

/// File lock region (for INT 21h/5Ch)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLock {
    pub offset: u32,
    pub length: u32,
}

impl FileLock {
    /// Check if this lock overlaps with another region
    pub fn overlaps(&self, other_offset: u32, other_length: u32) -> bool {
        self.overlaps_u64(u64::from(other_offset), u64::from(other_length))
    }

    fn overlaps_u64(&self, other_offset: u64, other_length: u64) -> bool {
        // Zero-length regions don't overlap with anything
        if self.length == 0 || other_length == 0 {
            return false;
        }

        let self_offset = u64::from(self.offset);
        let self_end = self_offset.saturating_add(u64::from(self.length));
        let other_end = other_offset.saturating_add(other_length);

        // Regions overlap if: start1 < end2 AND start2 < end1
        self_offset < other_end && other_offset < self_end
    }
}

/// Maximum number of active regions owned by one DOS open identity.
///
/// The emulator exposes 256 SFT-reachable handle slots.  Capping one SFT at
/// that many regions is generous enough for normal DOS sharing workloads while
/// keeping guest-controlled lock state bounded.  Duplicated handles point at
/// the same SFT and therefore share this limit.
const MAX_FILE_LOCKS_PER_SFT: usize = 256;

/// Check a lock registry without scanning every region.
///
/// Existing locks are non-overlapping and keyed by their start offset, so only
/// the predecessor and successor of the requested range can overlap it.
fn lock_map_overlaps(locks: &BTreeMap<u32, FileLock>, offset: u64, length: u64) -> bool {
    if length == 0 {
        return false;
    }

    let predecessor = if offset > u64::from(u32::MAX) {
        locks.iter().next_back()
    } else {
        locks.range(..=offset as u32).next_back()
    };
    if predecessor.is_some_and(|(_, lock)| lock.overlaps_u64(offset, length)) {
        return true;
    }

    (offset <= u64::from(u32::MAX))
        && locks
            .range(offset as u32..)
            .next()
            .is_some_and(|(_, lock)| lock.overlaps_u64(offset, length))
}

/// Sharing mode constants (bits 4-6 of open mode)
pub const SHARE_COMPAT: u8 = 0x00; // Compatibility mode
pub const SHARE_DENY_ALL: u8 = 0x10; // Deny all (exclusive)
pub const SHARE_DENY_WRITE: u8 = 0x20; // Deny write
pub const SHARE_DENY_READ: u8 = 0x30; // Deny read
pub const SHARE_DENY_NONE: u8 = 0x40; // Deny none

/// Access mode constants (bits 0-2 of open mode)
pub const ACCESS_READ: u8 = 0x00;
pub const ACCESS_WRITE: u8 = 0x01;
pub const ACCESS_READWRITE: u8 = 0x02;

/// DOS sharing check for two opens of the same file. Compatibility mode
/// permits another compatibility open; against explicit
/// sharing modes it behaves as deny-write, with write access also reading.
pub fn sharing_modes_conflict(
    existing_share: u8,
    existing_access: u8,
    new_share: u8,
    new_access: u8,
) -> bool {
    let existing_share = existing_share & 0x70;
    let new_share = new_share & 0x70;
    if existing_share == SHARE_COMPAT && new_share == SHARE_COMPAT {
        return false;
    }
    let effective_access = |share: u8, access: u8| {
        if share == SHARE_COMPAT && access == ACCESS_WRITE {
            ACCESS_READWRITE
        } else {
            access
        }
    };
    let existing_access = effective_access(existing_share, existing_access);
    let new_access = effective_access(new_share, new_access);
    let existing_share = if existing_share == SHARE_COMPAT {
        SHARE_DENY_WRITE
    } else {
        existing_share
    };
    let new_share = if new_share == SHARE_COMPAT {
        SHARE_DENY_WRITE
    } else {
        new_share
    };
    let reads = |access| access == ACCESS_READ || access == ACCESS_READWRITE;
    let writes = |access| access == ACCESS_WRITE || access == ACCESS_READWRITE;
    existing_share == SHARE_DENY_ALL
        || new_share == SHARE_DENY_ALL
        || (existing_share == SHARE_DENY_READ && reads(new_access))
        || (existing_share == SHARE_DENY_WRITE && writes(new_access))
        || (new_share == SHARE_DENY_READ && reads(existing_access))
        || (new_share == SHARE_DENY_WRITE && writes(existing_access))
}

/// What an open SFT entry is backed by, and therefore how a snapshot restore
/// has to reconnect it.
///
/// Most handles are files on a mounted drive and are reopened by path. The
/// synthetic ones are not: their bytes never existed on any drive, and their
/// stored `path` is a bare name (`GLIDE2X.OVL`, `ACPIANO.PAT`) that carries no
/// drive letter to reopen through. Recording the source at open time is what
/// lets restore rebuild them; sniffing the path afterwards would guess.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SftBacking {
    /// An ordinary file on a mounted drive, reopened by its DOS path.
    #[default]
    Drive,
    /// The synthetic `GLIDE2X.OVL` image served from `glide_ovl_data`.
    GlideOvl,
    /// A registered in-memory virtual file, by its `virtual_files` key.
    VirtualFile(String),
}

/// The live sources a snapshot restore rebuilds synthetic handles from.
///
/// Borrowed from `DosState` at restore time rather than copied into the
/// snapshot: the Glide overlay and the GUS assets are properties of this run's
/// configuration, and `virtual_files` is restored from the snapshot just before
/// this is built, so the bytes are the ones the handle was serving.
pub struct SyntheticSources<'a> {
    /// `glide_ovl_data`, when a Glide overlay is configured.
    pub glide_ovl: Option<&'a std::sync::Arc<[u8]>>,
    /// The registered in-memory virtual files, keyed as `virtual_file_key`
    /// spells them.
    pub virtual_files: &'a BTreeMap<String, std::sync::Arc<[u8]>>,
}

impl SyntheticSources<'_> {
    /// Re-reads the bytes behind a synthetic backing, or `None` if that source
    /// is no longer available in this run.
    fn rebuild(&self, backing: &SftBacking) -> Option<crate::filesystem::SyntheticPayload> {
        // Shared, never copied: a reconnected handle reads the SAME allocation
        // static asset cache (MDD-BUG-CRUCIBLE-00380).
        match backing {
            SftBacking::Drive => None,
            SftBacking::GlideOvl => self.glide_ovl.cloned().map(Into::into),
            SftBacking::VirtualFile(key) => self.virtual_files.get(key).cloned().map(Into::into),
        }
    }
}

/// System File Table entry - holds the actual file state
/// Multiple handles can point to the same SFT entry (via handle duplication)
pub struct SftEntry {
    pub file: Box<dyn VirtualFile>,
    pub path: String, // Stored as DOS path string
    identity: FileIdentity,
    pub position: u64,
    pub ref_count: u16, // Number of handles pointing to this entry
    /// Active file locks, indexed by start offset for bounded logarithmic lookups.
    pub locks: BTreeMap<u32, FileLock>,
    pub share_mode: u8,  // Sharing mode (bits 4-6 of open mode)
    pub access_mode: u8, // Access mode (bits 0-2 of open mode)
    pub owner_psp: u16,  // PSP of process that opened the file
    /// Position after the last read operation (for HDD seek sound detection)
    pub last_read_end: u64,
    /// Sequential read byte accumulator — emits 1 click per 32KB of sequential reads
    pub seq_read_bytes: u32,
    /// How a snapshot restore must reconnect this entry.
    pub backing: SftBacking,
}

impl SftEntry {
    pub(crate) fn allows_read(&self) -> bool {
        matches!(self.access_mode & 0x07, ACCESS_READ | ACCESS_READWRITE)
    }

    pub(crate) fn allows_write(&self) -> bool {
        matches!(self.access_mode & 0x07, ACCESS_WRITE | ACCESS_READWRITE)
    }
}

/// DOS file handle entry - points to an SFT entry
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct FileHandle {
    pub sft_index: usize,
    pub owner_psp: u16, // PSP segment of owning process
}

/// File handle table (handles 0-4 are reserved for stdin/stdout/stderr/stdaux/stdprn)
pub struct FileTable {
    handles: [Option<FileHandle>; 256],
    /// Device aliases share the DOS handle namespace with SFT-backed files.
    pub(crate) std_handle_aliases: [Option<u16>; 256],
    sft: Vec<Option<SftEntry>>,
    next_handle: u16,
    /// Per-process file-handle limit (the DOS Job File Table size). DOS defaults
    /// to 20 entries; handles 0-4 are the standard devices, so the guest can open
    /// `handle_limit - 5` files. INT 21h AH=67h (SetHandleCount) grows this and
    /// never shrinks it below 20. Dynamic allocation (AH=3Ch/3Dh/45h) only hands
    /// out handle numbers below this limit — matching real DOS, which returns
    /// "too many open files" once the JFT is full.
    handle_limit: u16,
}

impl FileTable {
    pub fn new() -> Self {
        let mut sft = Vec::with_capacity(256);
        sft.resize_with(256, || None);

        Self {
            handles: [None; 256],
            std_handle_aliases: [None; 256],
            sft,
            next_handle: 5,   // Skip 0-4 (standard handles)
            handle_limit: 20, // DOS default JFT size (FILES=20 → 15 user files)
        }
    }

    /// Grow the per-process file-handle limit (INT 21h AH=67h / SetHandleCount).
    ///
    /// DOS never shrinks the limit and never drops below the default of 20, so the
    /// requested `count` is clamped to `[20, 255]` and only ever raises the limit.
    /// After this, the guest can open `handle_limit - 5` files (handles 0-4 are the
    /// standard devices). Callers reject `count > 255` before invoking this.
    pub fn set_handle_count(&mut self, count: u16) {
        let target = count.clamp(20, 255);
        self.handle_limit = self.handle_limit.max(target);
    }

    /// The current per-process file-handle limit (JFT size).
    pub fn handle_limit(&self) -> u16 {
        self.handle_limit
    }

    /// Allocate a new SFT entry and return its index
    fn allocate_sft_entry(
        &mut self,
        file: Box<dyn VirtualFile>,
        path: String,
        identity: FileIdentity,
        share_mode: u8,
        access_mode: u8,
        owner_psp: u16,
    ) -> Option<usize> {
        for i in 0..self.sft.len() {
            if self.sft[i].is_none() {
                self.sft[i] = Some(SftEntry {
                    file,
                    path,
                    identity,
                    position: 0,
                    ref_count: 1,
                    locks: BTreeMap::new(),
                    share_mode,
                    access_mode,
                    owner_psp,
                    last_read_end: 0,
                    seq_read_bytes: 0,
                    backing: SftBacking::Drive,
                });
                return Some(i);
            }
        }
        None
    }

    /// Whether an ordinary open could be handed a handle right now.
    ///
    /// MDD-BUG-KIL-01019: a create or truncate must not touch the file unless
    /// the handle it would return can actually be produced. Discovering a full
    /// table AFTER the open is too late — `allocate_with_sharing_identity` can
    /// undo its own SFT entry, and dropping the backend closes the host file,
    /// but neither restores the bytes a truncate discarded nor removes a file
    /// the create brought into existence. So the capacity question has to be
    /// asked first, and this is the question it asks: is there a free SFT entry
    /// AND a free JFT slot below the handle limit, which is exactly the pair
    /// `allocate_with_sharing_identity` needs.
    ///
    /// A `false` here means the caller must fail with "too many open files"
    /// (error 4) without opening anything.
    pub(crate) fn has_open_capacity(&self) -> bool {
        if !self.sft.iter().any(Option::is_none) {
            return false;
        }
        self.next_free_handle().is_some()
    }

    /// Lowest available dynamic JFT slot, shared by file and device allocations.
    pub(crate) fn next_free_handle(&self) -> Option<u16> {
        let limit = self.handle_limit.min(self.handles.len() as u16);
        (self.next_handle..limit).find(|&handle| self.is_handle_free(handle))
    }

    /// Check if opening a file with given sharing/access modes would cause a violation
    /// Returns true if there's a sharing violation, false if OK to open
    pub fn check_sharing_violation(
        &self,
        path: &str,
        share_mode: u8,
        access_mode: u8,
        owner_psp: u16,
    ) -> bool {
        self.check_sharing_violation_for_identity(
            &FileIdentity::from_dos_path(path),
            share_mode,
            access_mode,
            owner_psp,
        )
    }

    pub(crate) fn check_sharing_violation_for_identity(
        &self,
        identity: &FileIdentity,
        share_mode: u8,
        access_mode: u8,
        owner_psp: u16,
    ) -> bool {
        for sft_entry in self.sft.iter().filter_map(|e| e.as_ref()) {
            if &sft_entry.identity != identity {
                continue;
            }

            let existing_share = sft_entry.share_mode & 0x70;
            let existing_access = sft_entry.access_mode & 0x07;
            let new_share = share_mode & 0x70;
            let new_access = access_mode & 0x07;

            // DOS compatibility mode grants reopens from the same PSP.
            if existing_share == SHARE_COMPAT || new_share == SHARE_COMPAT {
                if sft_entry.owner_psp != owner_psp {
                    return true;
                }
                continue;
            }
            if existing_share == SHARE_DENY_ALL || new_share == SHARE_DENY_ALL {
                return true;
            }
            let reads = |access| access == ACCESS_READ || access == ACCESS_READWRITE;
            let writes = |access| access == ACCESS_WRITE || access == ACCESS_READWRITE;
            if (existing_share == SHARE_DENY_READ && reads(new_access))
                || (existing_share == SHARE_DENY_WRITE && writes(new_access))
                || (new_share == SHARE_DENY_READ && reads(existing_access))
                || (new_share == SHARE_DENY_WRITE && writes(existing_access))
            {
                return true;
            }
        }

        false // No sharing violation
    }

    /// Under VSHARE (MDD-BUG-FLU-03201): whether an open of `identity` with
    /// `share_mode`/`access_mode` meets a holder, by the VSHARE conflict matrix
    /// ([`sharing_modes_conflict`]) whatever the owning process. `held` are
    /// files held open outside any handle, each read-only, deny-write.
    pub(crate) fn vshare_open_conflict(
        &self,
        identity: &FileIdentity,
        share_mode: u8,
        access_mode: u8,
        held: &[FileIdentity],
    ) -> bool {
        self.vshare_holders(identity, held).any(|(share, access)| {
            sharing_modes_conflict(share, access, share_mode, access_mode & 0x07)
        })
    }

    /// Under VSHARE: a delete is refused while `identity` is open in a
    /// sharing mode (including deny-none) or held. Compatibility-mode
    /// holders do not refuse deletion under the implemented policy.
    pub(crate) fn vshare_delete_refused(
        &self,
        identity: &FileIdentity,
        held: &[FileIdentity],
    ) -> bool {
        self.vshare_holders(identity, held)
            .any(|(share, _)| share & 0x70 != SHARE_COMPAT)
    }

    /// The (share, access) modes `identity` is open with: every SFT entry on
    /// it, and each hold as read-only, deny-write.
    fn vshare_holders<'a>(
        &'a self,
        identity: &'a FileIdentity,
        held: &'a [FileIdentity],
    ) -> impl Iterator<Item = (u8, u8)> + 'a {
        let entries = self
            .sft
            .iter()
            .flatten()
            .filter(move |entry| &entry.identity == identity)
            .map(|entry| (entry.share_mode, entry.access_mode & 0x07));
        let holds = held
            .iter()
            .filter(move |holder| *holder == identity)
            .map(|_| (SHARE_DENY_WRITE, ACCESS_READ));
        entries.chain(holds)
    }

    pub fn allocate(
        &mut self,
        file: Box<dyn VirtualFile>,
        path: String,
        owner_psp: u16,
    ) -> Option<u16> {
        // Use default sharing mode (compatibility) and read access for backwards compatibility
        self.allocate_with_sharing(file, path, owner_psp, SHARE_COMPAT, ACCESS_READ)
    }

    /// Allocate a file handle with explicit sharing and access modes
    /// Allocates a handle for a file the emulator synthesized, recording where
    /// its bytes came from so a snapshot restore can rebuild it.
    ///
    /// Synthetic handles store a bare name as their path, so the drive-based
    /// reopen has nothing to work with — see [`SftBacking`].
    pub fn allocate_synthetic_with_sharing(
        &mut self,
        file: Box<dyn VirtualFile>,
        path: String,
        owner_psp: u16,
        share_mode: u8,
        access_mode: u8,
        backing: SftBacking,
    ) -> Option<u16> {
        let handle = self.allocate_with_sharing(file, path, owner_psp, share_mode, access_mode)?;
        if let Some(entry) = self
            .handles
            .get(handle as usize)
            .and_then(|h| h.as_ref())
            .and_then(|h| self.sft.get_mut(h.sft_index))
            .and_then(Option::as_mut)
        {
            entry.backing = backing;
        }
        Some(handle)
    }

    pub fn allocate_with_sharing(
        &mut self,
        file: Box<dyn VirtualFile>,
        path: String,
        owner_psp: u16,
        share_mode: u8,
        access_mode: u8,
    ) -> Option<u16> {
        let identity = FileIdentity::from_dos_path(&path);
        self.allocate_with_sharing_identity(
            file,
            path,
            identity,
            owner_psp,
            share_mode,
            access_mode,
        )
    }

    /// Allocate an SFT entry using a backend-resolved file identity.
    pub(crate) fn allocate_with_sharing_identity(
        &mut self,
        file: Box<dyn VirtualFile>,
        path: String,
        identity: FileIdentity,
        owner_psp: u16,
        share_mode: u8,
        access_mode: u8,
    ) -> Option<u16> {
        // First allocate an SFT entry
        let path_for_trace = path.clone();
        let sft_index =
            self.allocate_sft_entry(file, path, identity, share_mode, access_mode, owner_psp)?;

        // File opens and device aliases consume the same JFT slots.
        if let Some(handle) = self.next_free_handle() {
            self.handles[handle as usize] = Some(FileHandle {
                sft_index,
                owner_psp,
            });
            tracing::debug!(
                "DOS file open: handle={} path=\"{}\" psp={:04X} share={:02X} access={:02X}",
                handle,
                path_for_trace,
                owner_psp,
                share_mode,
                access_mode
            );
            return Some(handle);
        }

        // Failed to allocate handle, clean up SFT entry
        tracing::debug!(
            "DOS file open failed: no handles path=\"{}\"",
            path_for_trace
        );
        self.sft[sft_index] = None;
        None
    }

    /// Allocate a file handle at a specific handle number.
    /// Used to bridge 16-bit file handles into the DOS file table so both
    /// namespaces share the same handle number (matching the handle-bridge contract).
    /// Returns `true` if the handle was installed, `false` if the slot was already
    /// occupied (or out of range, or the SFT was full) — in which case nothing
    /// changed. Callers that track bridged handles must consult the return value
    /// so they never record (and later free) a slot they did not actually fill.
    #[must_use]
    pub fn allocate_at_handle(
        &mut self,
        handle: u16,
        file: Box<dyn VirtualFile>,
        path: String,
        owner_psp: u16,
    ) -> bool {
        self.allocate_at_handle_with_sharing(
            handle,
            file,
            path,
            None,
            owner_psp,
            SHARE_COMPAT,
            ACCESS_READWRITE,
        )
    }

    /// Mirror a 16-bit HFILE with its actual sharing modes and resolved host
    /// identity, so subsequent DOS opens see the same file and restrictions.
    #[must_use]
    pub fn allocate_at_handle_with_sharing(
        &mut self,
        handle: u16,
        file: Box<dyn VirtualFile>,
        path: String,
        host_path: Option<&Path>,
        owner_psp: u16,
        share_mode: u8,
        access_mode: u8,
    ) -> bool {
        let idx = handle as usize;
        if !self.is_handle_free(handle) {
            return false;
        }
        let identity = host_path.map_or_else(
            || FileIdentity::from_dos_path(&path),
            |host| FileIdentity::for_resolved_path(2, &path, Some(host)),
        );
        if let Some(sft_index) = self.allocate_sft_entry(
            file,
            path.clone(),
            identity,
            share_mode,
            access_mode,
            owner_psp,
        ) {
            self.handles[idx] = Some(FileHandle {
                sft_index,
                owner_psp,
            });
            tracing::debug!(
                "DOS file bridge: handle={} path=\"{}\" psp={:04X}",
                handle,
                path,
                owner_psp
            );
            true
        } else {
            false
        }
    }

    /// Returns `true` if the given handle slot is in range and currently unused.
    /// Used by the 16-bit→DOS handle bridge to detect a namespace collision before
    /// mirroring a 16-bit `OpenFile` handle into the DOS table.
    pub fn is_handle_free(&self, handle: u16) -> bool {
        let idx = handle as usize;
        idx < self.handles.len()
            && self.handles[idx].is_none()
            && self.std_handle_aliases[idx].is_none()
    }

    /// Get the SFT entry for a handle
    pub fn get_sft(&self, handle: u16) -> Option<&SftEntry> {
        let fh = self.handles.get(handle as usize)?.as_ref()?;
        self.sft.get(fh.sft_index)?.as_ref()
    }

    /// Get mutable SFT entry for a handle
    pub fn get_sft_mut(&mut self, handle: u16) -> Option<&mut SftEntry> {
        let sft_index = self.handles.get(handle as usize)?.as_ref()?.sft_index;
        self.sft.get_mut(sft_index)?.as_mut()
    }

    /// Get the owner PSP for a handle
    pub fn get_owner(&self, handle: u16) -> Option<u16> {
        Some(self.handles.get(handle as usize)?.as_ref()?.owner_psp)
    }

    /// Close a handle, draining emulator-side write buffers before the last
    /// SFT reference is removed. A flush failure leaves the handle and dirty
    /// file intact so DOS can report the error and retry.
    ///
    /// Deliberately `flush()`, NOT `sync_all()` (MDD-BUG-FLUX-00157): real DOS
    /// close drains DOS-internal buffers only — it never issued a host
    /// storage barrier, and `File::sync_all` is `fcntl(F_FULLFSYNC)` on
    /// macOS, a multi-millisecond stall per guest close. `flush()` drains
    /// every emulator-owned layer (the FAT-image wrapper's caching included);
    /// the host barrier belongs solely to INT 21h AH=68h Commit File.
    pub fn try_free(&mut self, handle: u16) -> anyhow::Result<bool> {
        let Some(fh) = self.handles.get(handle as usize).and_then(|slot| *slot) else {
            tracing::debug!("DOS file close failed: invalid handle={}", handle);
            return Ok(false);
        };

        if let Some(Some(entry)) = self.sft.get_mut(fh.sft_index) {
            tracing::debug!(
                "DOS file close: handle={} path=\"{}\" refs={}",
                handle,
                entry.path,
                entry.ref_count
            );
            if entry.ref_count <= 1 && entry.allows_write() {
                entry.file.flush()?;
            }
        }

        self.handles[handle as usize] = None;
        if let Some(sft_entry) = self.sft.get_mut(fh.sft_index) {
            if let Some(entry) = sft_entry {
                entry.ref_count = entry.ref_count.saturating_sub(1);
                if entry.ref_count == 0 {
                    *sft_entry = None;
                }
            }
        }
        Ok(true)
    }

    /// Best-effort close for teardown paths that cannot return a DOS error.
    /// Failed synchronization is logged and the handle remains live.
    pub fn free(&mut self, handle: u16) -> bool {
        match self.try_free(handle) {
            Ok(closed) => closed,
            Err(error) => {
                tracing::warn!("DOS file close sync failed for handle {handle}: {error}");
                false
            }
        }
    }

    /// Duplicate a file handle (AH=45h) - new handle shares file position with original
    pub fn duplicate(&mut self, src_handle: u16, owner_psp: u16) -> Option<u16> {
        // Get source handle's SFT index
        let sft_index = self.handles.get(src_handle as usize)?.as_ref()?.sft_index;

        let handle = self.next_free_handle()?;
        let sft_entry = self.sft.get_mut(sft_index)?.as_mut()?;
        sft_entry.ref_count += 1;
        self.handles[handle as usize] = Some(FileHandle {
            sft_index,
            owner_psp,
        });
        tracing::debug!(
            "DOS file dup: src={} -> dst={} psp={:04X}",
            src_handle,
            handle,
            owner_psp
        );
        Some(handle)
    }

    /// Force duplicate handle (AH=46h) - redirect dst to same file as src
    pub fn force_duplicate(&mut self, src_handle: u16, dst_handle: u16, owner_psp: u16) -> bool {
        let src_sft_index = match self.handles.get(src_handle as usize) {
            Some(Some(fh)) => fh.sft_index,
            _ => return false,
        };
        if self
            .sft
            .get(src_sft_index)
            .and_then(Option::as_ref)
            .is_none()
        {
            return false;
        }

        let dst_index = dst_handle as usize;
        if dst_index >= self.handles.len() {
            return false;
        }

        // AH=46h with the same source and destination is a successful no-op.
        // In particular, do not close and recreate the only reference.
        if src_handle == dst_handle {
            return true;
        }

        // `free` routes through the fallible close path and logs a flush error.
        // On failure, it leaves the old destination binding intact.
        if self.handles[dst_index].is_some() && !self.free(dst_handle) {
            return false;
        }

        self.sft[src_sft_index]
            .as_mut()
            .expect("validated source SFT remains live after closing another handle")
            .ref_count += 1;
        self.handles[dst_index] = Some(FileHandle {
            sft_index: src_sft_index,
            owner_psp,
        });
        self.std_handle_aliases[dst_index] = None;
        true
    }

    /// Insert a file handle at a specific slot (for AH=46h Force Duplicate Handle with new file)
    pub fn insert_at(
        &mut self,
        handle: u16,
        file: Box<dyn VirtualFile>,
        path: String,
        owner_psp: u16,
    ) -> bool {
        // First allocate an SFT entry with default sharing mode (compatibility) and read access
        let sft_index = match self.allocate_sft_entry(
            file,
            path.clone(),
            FileIdentity::from_dos_path(&path),
            SHARE_COMPAT,
            ACCESS_READ,
            owner_psp,
        ) {
            Some(idx) => idx,
            None => return false,
        };

        if let Some(slot) = self.handles.get_mut(handle as usize) {
            // Free existing handle if any
            if let Some(old_fh) = slot.take() {
                if let Some(Some(entry)) = self.sft.get_mut(old_fh.sft_index) {
                    entry.ref_count = entry.ref_count.saturating_sub(1);
                    if entry.ref_count == 0 {
                        self.sft[old_fh.sft_index] = None;
                    }
                }
            }

            *slot = Some(FileHandle {
                sft_index,
                owner_psp,
            });
            self.std_handle_aliases[handle as usize] = None;
            true
        } else {
            // Clean up SFT entry
            self.sft[sft_index] = None;
            false
        }
    }

    /// Close all files owned by a specific PSP (called during process termination)
    pub fn close_process_files(&mut self, psp: u16) {
        for handle in 5..self.handles.len() {
            if let Some(fh) = &self.handles[handle] {
                if fh.owner_psp == psp {
                    // Use free() to properly decrement SFT ref count
                    self.free(handle as u16);
                }
            }
        }
    }

    /// Check if a handle is valid (for compatibility checks)
    ///
    /// MDD-BUG-CRUCIBLE-00445: validity needs BOTH a populated handle slot and
    /// a live SFT entry behind it. Checking only the slot let a handle whose
    /// file failed to reopen after a snapshot restore report valid to close,
    /// IOCTL and handle allocation while every actual I/O path — which goes
    /// through `get_sft` — correctly saw it as invalid.
    pub fn is_valid(&self, handle: u16) -> bool {
        self.get_sft(handle).is_some()
    }

    fn handle_sft_index(&self, handle: u16) -> Result<usize, u16> {
        self.handles
            .get(handle as usize)
            .and_then(|slot| slot.as_ref())
            .map(|fh| fh.sft_index)
            .ok_or(6)
    }

    fn region_has_lock(
        &self,
        identity: &FileIdentity,
        excluded_sft: Option<usize>,
        offset: u64,
        length: u64,
    ) -> bool {
        self.sft.iter().enumerate().any(|(index, entry)| {
            if excluded_sft == Some(index) {
                return false;
            }
            let Some(entry) = entry.as_ref() else {
                return false;
            };
            &entry.identity == identity && lock_map_overlaps(&entry.locks, offset, length)
        })
    }

    fn io_identity(&self, handle: u16) -> Result<(usize, FileIdentity), u16> {
        let sft_index = self.handle_sft_index(handle)?;
        let entry = self
            .sft
            .get(sft_index)
            .and_then(Option::as_ref)
            .ok_or(6u16)?;
        Ok((sft_index, entry.identity.clone()))
    }

    fn has_foreign_file_lock(&self, identity: &FileIdentity, excluded_sft: usize) -> bool {
        self.sft.iter().enumerate().any(|(index, entry)| {
            index != excluded_sft
                && entry
                    .as_ref()
                    .is_some_and(|entry| &entry.identity == identity && !entry.locks.is_empty())
        })
    }

    fn current_io_position(&mut self, sft_index: usize) -> Result<u64, u16> {
        let entry = self
            .sft
            .get_mut(sft_index)
            .and_then(Option::as_mut)
            .ok_or(6u16)?;
        let position = entry.file.stream_position().map_err(|_| 5u16)?;
        Ok(position)
    }

    fn end_position_preserving_current(
        &mut self,
        sft_index: usize,
        position: u64,
    ) -> Result<u64, u16> {
        use std::io::{Seek, SeekFrom};

        let entry = self
            .sft
            .get_mut(sft_index)
            .and_then(Option::as_mut)
            .ok_or(6u16)?;
        let end = entry.file.seek(SeekFrom::End(0)).map_err(|_| 5u16)?;
        entry
            .file
            .seek(SeekFrom::Start(position))
            .map_err(|_| 5u16)?;
        Ok(end)
    }

    /// Check whether an independently opened SFT locks bytes an AH=3Fh read
    /// would actually transfer. Locks owned by this same open identity do not
    /// conflict, so duplicated handles retain their shared-owner semantics.
    pub fn read_region_lock_violation(
        &mut self,
        handle: u16,
        requested_length: u64,
    ) -> Result<bool, u16> {
        if requested_length == 0 {
            return Ok(false);
        }

        let (sft_index, identity) = self.io_identity(handle)?;
        if !self.has_foreign_file_lock(&identity, sft_index) {
            return Ok(false);
        }
        let position = self.current_io_position(sft_index)?;
        if !self.region_has_lock(&identity, Some(sft_index), position, requested_length) {
            return Ok(false);
        }

        let end = self.end_position_preserving_current(sft_index, position)?;
        let actual_length = end.saturating_sub(position).min(requested_length);
        Ok(self.region_has_lock(&identity, Some(sft_index), position, actual_length))
    }

    /// Check whether an independently opened SFT locks an AH=40h write span.
    pub fn write_region_lock_violation(&mut self, handle: u16, length: u64) -> Result<bool, u16> {
        if length == 0 {
            return Ok(false);
        }

        let (sft_index, identity) = self.io_identity(handle)?;
        if !self.has_foreign_file_lock(&identity, sft_index) {
            return Ok(false);
        }
        let position = self.current_io_position(sft_index)?;
        Ok(self.region_has_lock(&identity, Some(sft_index), position, length))
    }

    /// Check the exact byte span changed by AH=40h/CX=0. This covers both
    /// shortening a file and extending it when the current position is past EOF.
    pub fn truncate_region_lock_violation(&mut self, handle: u16) -> Result<bool, u16> {
        let (sft_index, identity) = self.io_identity(handle)?;
        if !self.has_foreign_file_lock(&identity, sft_index) {
            return Ok(false);
        }
        let position = self.current_io_position(sft_index)?;
        let end = self.end_position_preserving_current(sft_index, position)?;
        let changed_start = position.min(end);
        let changed_length = position.abs_diff(end);
        Ok(self.region_has_lock(&identity, Some(sft_index), changed_start, changed_length))
    }

    /// Lock a region of a file (INT 21h/5Ch AL=00h)
    /// Returns Ok(()) on success, Err(error_code) on failure
    pub fn lock_region(&mut self, handle: u16, offset: u32, length: u32) -> Result<(), u16> {
        // Zero-length locks always succeed (no-op)
        if length == 0 {
            return Ok(());
        }

        // Get SFT index for this handle
        let sft_index = self.handle_sft_index(handle)?;
        let identity = match self.sft.get(sft_index) {
            Some(Some(entry)) => entry.identity.clone(),
            _ => return Err(6),
        };

        // Locks are owned by an SFT/open identity, but protect every live SFT
        // for the same backend file.
        if self.region_has_lock(&identity, None, u64::from(offset), u64::from(length)) {
            return Err(33); // Error 33: Lock violation
        }

        // Add the new lock
        let entry = self
            .sft
            .get_mut(sft_index)
            .and_then(Option::as_mut)
            .ok_or(6u16)?;
        if entry.locks.len() >= MAX_FILE_LOCKS_PER_SFT {
            return Err(33); // Error 33: lock table exhausted
        }
        let _ = entry.locks.insert(offset, FileLock { offset, length });
        Ok(())
    }

    /// Unlock a region of a file (INT 21h/5Ch AL=01h)
    /// Returns Ok(()) on success, Err(error_code) on failure
    pub fn unlock_region(&mut self, handle: u16, offset: u32, length: u32) -> Result<(), u16> {
        // Zero-length unlocks always succeed (no-op)
        if length == 0 {
            return Ok(());
        }

        // Get SFT index for this handle
        let sft_index = self.handle_sft_index(handle)?;

        // Find and remove the exact matching lock
        if let Some(Some(entry)) = self.sft.get_mut(sft_index) {
            if entry
                .locks
                .get(&offset)
                .is_some_and(|lock| lock.length == length)
            {
                entry.locks.remove(&offset);
                Ok(())
            } else {
                Err(33) // Error 33: Lock violation (lock not found)
            }
        } else {
            Err(6) // Invalid handle
        }
    }

    /// Increment the reference count for an SFT entry.
    /// Called when JFT entries are copied during EXEC (handle inheritance).
    /// No-op if sft_index is out of range or the slot is empty (defensive).
    pub fn increment_sft_ref_count(&mut self, sft_index: usize) {
        if let Some(Some(entry)) = self.sft.get_mut(sft_index) {
            entry.ref_count += 1;
        }
    }

    /// Get the reference count for an SFT entry by index.
    /// Returns None if the index is out of range or the slot is empty.
    pub fn get_sft_ref_count(&self, sft_index: usize) -> Option<u16> {
        self.sft.get(sft_index)?.as_ref().map(|e| e.ref_count)
    }

    /// Get the SFT index for a handle (for lock operations that need to share locks across duplicate handles)
    pub fn get_sft_index(&self, handle: u16) -> Option<usize> {
        self.handles
            .get(handle as usize)?
            .as_ref()
            .map(|fh| fh.sft_index)
    }

    /// Capture a snapshot of the file table metadata. The actual
    /// `Box<dyn VirtualFile>` trait objects are NOT included -- only the
    /// handle-to-SFT mapping and per-entry metadata (path, position, locks,
    /// etc.) needed to reconnect files on restore. Device aliases remain in
    /// the enclosing DOS snapshot so its existing schema stays unchanged.
    pub fn snapshot(&self) -> crate::snapshot::FileTableSnapshot {
        let sft = self
            .sft
            .iter()
            .map(|entry| {
                entry.as_ref().map(|e| crate::snapshot::SftMetadata {
                    path: gp64_types::FixedString::from(e.path.as_str()),
                    position: e.position,
                    ref_count: e.ref_count,
                    locks: e.locks.values().cloned().collect(),
                    share_mode: e.share_mode,
                    access_mode: e.access_mode,
                    owner_psp: e.owner_psp,
                    last_read_end: e.last_read_end,
                    seq_read_bytes: e.seq_read_bytes,
                    backing: e.backing.clone(),
                })
            })
            .collect();
        crate::snapshot::FileTableSnapshot {
            handles: self.handles,
            sft,
            next_handle: self.next_handle,
            handle_limit: self.handle_limit,
        }
    }

    /// Restore file table metadata from a snapshot. The handle array,
    /// `next_handle` counter, and the AH=67h `handle_limit` are restored here.
    /// SFT entries are NOT restored (they contain `Box<dyn VirtualFile>` which
    /// requires file reconnection as a separate post-restore step).
    pub fn restore(&mut self, snap: &crate::snapshot::FileTableSnapshot) {
        self.handles = snap.handles;
        self.next_handle = snap.next_handle;
        self.handle_limit = snap.handle_limit;
    }

    /// Reopen files from snapshot metadata after a restore.
    ///
    /// For each active SFT entry in the snapshot, attempts to reopen the file
    /// using the mounted drives and seek to the saved position. Files that
    /// cannot be reopened are left as `None` in the SFT — subsequent DOS
    /// access to those handles will get an "invalid handle" error, which is
    /// the correct best-effort behavior.
    pub fn reopen_files_from_snapshot(
        &mut self,
        snap: &crate::snapshot::FileTableSnapshot,
        drives: &mut BTreeMap<u8, Box<dyn crate::filesystem::VirtualDrive>>,
        synthetic: &SyntheticSources<'_>,
    ) {
        use std::io::{Seek, SeekFrom};

        // Clear any stale SFT entries from before restore. They belong to the
        // timeline being abandoned: dropping one is NOT a guest close, so it
        // must not commit what it buffered since the snapshot
        // (MDD-BUG-KIL-00781).
        for entry in self.sft.iter_mut() {
            if let Some(live) = entry.as_mut() {
                live.file.abandon();
            }
            *entry = None;
        }

        // Ensure SFT vec is large enough
        while self.sft.len() < snap.sft.len() {
            self.sft.push(None);
        }

        for (i, meta_opt) in snap.sft.iter().enumerate() {
            let meta = match meta_opt {
                Some(m) => m,
                None => continue,
            };

            let path_str = meta.path.as_str();

            // A synthetic handle was never on a drive: its bytes came from the
            // Glide overlay, the GUS asset loader or the registered virtual
            // file map, and its stored path is a bare name with no drive to
            // reopen through. Rebuild it from the recorded source instead.
            if meta.backing != SftBacking::Drive {
                match synthetic.rebuild(&meta.backing) {
                    Some(bytes) => {
                        let identity = FileIdentity::from_dos_path(path_str);
                        let mut file: Box<dyn VirtualFile> =
                            Box::new(crate::filesystem::SyntheticFile::new(bytes));
                        let eof = file.seek(SeekFrom::End(0)).unwrap_or(0);
                        let target = meta.position.min(eof);
                        let position = file.seek(SeekFrom::Start(target)).unwrap_or(0);
                        self.sft[i] = Some(SftEntry {
                            file,
                            path: path_str.to_string(),
                            identity,
                            position,
                            ref_count: meta.ref_count,
                            locks: BTreeMap::new(),
                            share_mode: meta.share_mode,
                            access_mode: meta.access_mode,
                            owner_psp: meta.owner_psp,
                            last_read_end: meta.last_read_end,
                            seq_read_bytes: meta.seq_read_bytes,
                            backing: meta.backing.clone(),
                        });
                        tracing::debug!(
                            sft = i,
                            path = path_str,
                            position,
                            "snapshot restore: synthetic file rebuilt"
                        );
                    }
                    None => {
                        tracing::warn!(
                            sft = i,
                            path = path_str,
                            backing = ?meta.backing,
                            "snapshot restore: synthetic source is gone, skipping"
                        );
                    }
                }
                continue;
            }

            // Parse drive letter from the full path (e.g. "C:\GAME\DATA.DAT")
            let (drive_idx, file_path) = match parse_drive_and_path(path_str) {
                Some(pair) => pair,
                None => {
                    tracing::warn!(
                        sft = i,
                        path = path_str,
                        "snapshot restore: cannot parse drive from path, skipping"
                    );
                    continue;
                }
            };

            let drive = match drives.get_mut(&drive_idx) {
                Some(d) => d,
                None => {
                    tracing::warn!(
                        sft = i,
                        path = path_str,
                        drive = drive_idx,
                        "snapshot restore: drive not mounted, skipping"
                    );
                    continue;
                }
            };

            // Determine write mode from saved access_mode
            let write = meta.access_mode & 0x07 != ACCESS_READ;
            let host_path = drive.resolve_host_path(file_path);
            let identity =
                FileIdentity::for_resolved_path(drive_idx, file_path, host_path.as_deref());

            let mut file = match drive.open_file(file_path, write, false, false) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(
                        sft = i,
                        path = path_str,
                        error = %e,
                        "snapshot restore: cannot reopen file, skipping"
                    );
                    continue;
                }
            };

            // Seek to the saved position, CLAMPED to the reopened file's
            // current length.
            //
            // MDD-BUG-CRUCIBLE-00647: this used to seek straight to
            // `meta.position` and fall back to EOF only if that seek FAILED.
            // Seeking past EOF is legal for both `std::fs::File` and the
            // Cursor-backed virtual files, so the seek always succeeded and
            // the short-file fallback was unreachable. If the backing file
            // shrank between capture and restore, the SFT stayed past EOF:
            // reads returned EOF from an impossible offset, a later write
            // would punch a sparse gap, and relative seeks and locks all
            // started from the wrong place. Measuring the end first makes the
            // clamp explicit instead of relying on an error that never comes.
            let eof = match file.seek(SeekFrom::End(0)) {
                Ok(end) => end,
                Err(e) => {
                    tracing::warn!(
                        sft = i,
                        path = path_str,
                        error = %e,
                        "snapshot restore: cannot measure file length, skipping"
                    );
                    continue;
                }
            };
            let target = meta.position.min(eof);
            if target != meta.position {
                tracing::warn!(
                    sft = i,
                    path = path_str,
                    saved_pos = meta.position,
                    actual_pos = target,
                    "snapshot restore: file shorter than saved position, clamped to EOF"
                );
            }
            let position = match file.seek(SeekFrom::Start(target)) {
                Ok(pos) => pos,
                Err(e) => {
                    tracing::warn!(
                        sft = i,
                        path = path_str,
                        error = %e,
                        "snapshot restore: seek failed, skipping"
                    );
                    continue;
                }
            };

            let mut locks = BTreeMap::new();
            for lock in &meta.locks {
                if locks.len() >= MAX_FILE_LOCKS_PER_SFT
                    || lock_map_overlaps(&locks, u64::from(lock.offset), u64::from(lock.length))
                {
                    continue;
                }
                let _ = locks.insert(lock.offset, lock.clone());
            }

            self.sft[i] = Some(SftEntry {
                file,
                path: path_str.to_string(),
                identity,
                position,
                ref_count: meta.ref_count,
                locks,
                share_mode: meta.share_mode,
                access_mode: meta.access_mode,
                owner_psp: meta.owner_psp,
                last_read_end: meta.last_read_end,
                seq_read_bytes: meta.seq_read_bytes,
                backing: SftBacking::Drive,
            });

            tracing::debug!(
                sft = i,
                path = path_str,
                position,
                "snapshot restore: file reopened"
            );
        }

        self.drop_handles_without_sft();
    }

    /// Drop every handle whose SFT entry is missing.
    ///
    /// MDD-BUG-CRUCIBLE-00445: `restore` reinstates the whole handle array,
    /// but reopening is best-effort — an unparseable path, an unmounted
    /// drive, a file that will not open, an unmeasurable length or a failed
    /// seek all `continue` and leave that SFT slot `None`. The handles
    /// pointing at those slots survived, so a handle could be simultaneously
    /// valid to close, IOCTL and handle-allocation logic and invalid to every
    /// real I/O path. Reopening documents those handles as invalid, so make
    /// them actually invalid.
    fn drop_handles_without_sft(&mut self) {
        for handle in 0..self.handles.len() {
            let Some(fh) = self.handles[handle].as_ref() else {
                continue;
            };
            if self
                .sft
                .get(fh.sft_index)
                .and_then(|entry| entry.as_ref())
                .is_some()
            {
                continue;
            }
            tracing::warn!(
                handle,
                sft = fh.sft_index,
                "snapshot restore: file could not be reopened, dropping its handle"
            );
            self.handles[handle] = None;
        }
    }
}

/// Parse a DOS full path (e.g. "C:\GAME\DATA.DAT") into a drive index and
/// the path portion after the colon. Returns `None` if the path doesn't
/// match the expected `X:...` format.
fn parse_drive_and_path(full_path: &str) -> Option<(u8, &str)> {
    let bytes = full_path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let letter = bytes[0].to_ascii_uppercase();
        if letter.is_ascii_uppercase() {
            let drive_idx = letter - b'A';
            let path = &full_path[2..];
            return Some((drive_idx, path));
        }
    }
    None
}

impl Default for FileTable {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPath {
    pub drive: u8,
    pub path: String,
}

/// DOS path resolver
pub struct PathResolver {
    /// Current drive (0=A, 1=B, 2=C, etc.)
    current_drive: u8,
    /// Current working directory per drive (DOS paths, always start with \)
    current_dirs: BTreeMap<u8, String>,
    /// Bumped on every current-drive/-directory change, so layered kernels
    /// (the 16-bit KERNEL keeps a host-path mirror of the DOS cwd) can detect
    /// staleness cheaply instead of re-deriving the cwd on every call.
    cwd_generation: u64,
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PathResolver {
    pub fn new() -> Self {
        let mut current_dirs = BTreeMap::new();
        // Default C:\
        current_dirs.insert(2, String::from(r"\"));

        Self {
            current_drive: 2, // Default to C:
            current_dirs,
            cwd_generation: 0,
        }
    }

    /// Capture a snapshot of the path resolver state.
    pub fn snapshot(&self) -> crate::snapshot::PathResolverSnapshot {
        let current_dirs = self
            .current_dirs
            .iter()
            .map(|(&drive, dir)| (drive, gp64_types::FixedString::from(dir.as_str())))
            .collect();
        crate::snapshot::PathResolverSnapshot {
            current_drive: self.current_drive,
            current_dirs,
        }
    }

    /// Restore path resolver state from a snapshot.
    pub fn restore(&mut self, snap: &crate::snapshot::PathResolverSnapshot) {
        self.current_drive = snap.current_drive;
        self.current_dirs.clear();
        for &(drive, ref dir) in &snap.current_dirs {
            self.current_dirs.insert(drive, dir.as_str().to_string());
        }
        self.cwd_generation = self.cwd_generation.wrapping_add(1);
    }

    pub fn get_current_drive(&self) -> u8 {
        self.current_drive
    }

    /// Monotonic counter of current-drive/-directory changes; equality means
    /// the cwd has not moved since the caller last sampled it.
    pub fn cwd_generation(&self) -> u64 {
        self.cwd_generation
    }

    pub fn set_current_drive(&mut self, drive: u8) {
        self.current_drive = drive;
        // Ensure entry exists
        self.current_dirs
            .entry(drive)
            .or_insert_with(|| String::from(r"\"));
        self.cwd_generation = self.cwd_generation.wrapping_add(1);
    }

    fn normalize_dos_dir(dos_dir: &str) -> String {
        Self::normalize_dos_dir_impl(dos_dir, true, false)
    }

    /// The directory component of a normalized DOS path, or `None` when there
    /// is no directory that could be missing — a bare name, or a child of the
    /// root, since the root always exists.
    ///
    /// Used to tell DOS error 2 (file not found) from error 3 (path not
    /// found); see `Dos::missing_dos_path_error`.
    pub(crate) fn dos_parent_dir(path: &str) -> Option<&str> {
        let path = path.trim_end_matches('\\');
        let separator = path.rfind('\\')?;
        if separator == 0 {
            None // a child of the root
        } else {
            Some(&path[..separator])
        }
    }

    /// Like `normalize_dos_dir` but preserves original case.
    /// Used for LFN (Long File Name) operations where case must be retained.
    fn normalize_dos_dir_preserve_case(dos_dir: &str) -> String {
        Self::normalize_dos_dir_impl(dos_dir, false, false)
    }

    fn normalize_dos_find_pattern(dos_dir: &str) -> String {
        Self::normalize_dos_dir_impl(dos_dir, true, true)
    }

    fn normalize_dos_dir_impl(
        dos_dir: &str,
        uppercase: bool,
        preserve_final_empty_ext_dot: bool,
    ) -> String {
        let mut s = dos_dir.replace('/', r"\");
        if uppercase {
            s = s.to_uppercase();
        }
        if s.is_empty() {
            s.push('\\');
        }
        if !s.starts_with('\\') {
            s.insert(0, '\\');
        }
        // Remove trailing backslash unless root.
        while s.len() > 1 && s.ends_with('\\') {
            s.pop();
        }

        // Collapse "." and "..", and (SFN only) de-pad FCB-style 8.3 names.
        let mut parts: Vec<String> = Vec::new();
        let raw_parts: Vec<&str> = s.split('\\').collect();
        for (idx, &part) in raw_parts.iter().enumerate() {
            // A find pattern's final `.` or `..` names that entry of the
            // directory, which DOS lists, rather than stepping through it
            // (MDD-BUG-CRU-01407).
            if preserve_final_empty_ext_dot
                && idx + 1 == raw_parts.len()
                && (part == "." || part == "..")
            {
                parts.push(part.to_string());
                continue;
            }
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." {
                parts.pop();
                continue;
            }
            // On the standard (uppercase/SFN) path, apply real-DOS 8.3 field
            // parsing: a trailing-space-padded FCB name like "FONDOS  .GRF" or
            // "SUB     .D01" refers to the de-padded file ("FONDOS.GRF",
            // "SUB.D01"). LFN resolution (uppercase=false) preserves spaces.
            let norm = if uppercase {
                Self::fcb_depad_component(
                    part,
                    preserve_final_empty_ext_dot && idx + 1 == raw_parts.len(),
                )
            } else {
                part.to_string()
            };
            if norm.is_empty() {
                continue;
            }
            parts.push(norm);
        }

        let mut res = String::from(r"\");
        res.push_str(&parts.join(r"\"));
        res
    }

    /// De-pad a single SFN path component per real-DOS 8.3 parsing: the name and
    /// extension fields are padded with trailing spaces in FCB form, and DOS
    /// opens the de-padded 8.3 name. Trims trailing spaces from the basename
    /// (before the first '.') and from the extension. Used only on the SFN
    /// (uppercased) resolution path — never for LFN, where spaces are valid.
    fn fcb_depad_component(part: &str, preserve_empty_ext_dot: bool) -> String {
        match part.split_once('.') {
            Some((base, ext)) => {
                let base = base.trim_end_matches(' ');
                let ext = ext.trim_end_matches(' ');
                if ext.is_empty() {
                    if preserve_empty_ext_dot {
                        format!("{base}.")
                    } else {
                        base.to_string()
                    }
                } else {
                    format!("{base}.{ext}")
                }
            }
            None => part.trim_end_matches(' ').to_string(),
        }
    }

    pub fn split_drive_and_path(dos_path: &str) -> (Option<u8>, &str) {
        let p = dos_path.trim();
        let bytes = p.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            let drive_char = (bytes[0] as char).to_ascii_uppercase();
            if drive_char.is_ascii_uppercase() {
                let drive = drive_char as u8 - b'A';
                return (Some(drive), &p[2..]);
            }
        }
        (None, p)
    }

    /// Resolve a DOS path to a (Drive, InternalPath) pair.
    /// Standard DOS resolution: uppercases the path (8.3 convention).
    pub fn resolve(&mut self, dos_path: &str) -> ResolvedPath {
        self.resolve_impl(dos_path, true, false)
    }

    /// Resolve a DOS search pattern, preserving an explicit blank extension on
    /// the final component so wildcard matching can distinguish `*.` from `*`.
    pub(crate) fn resolve_find_pattern(&mut self, dos_path: &str) -> ResolvedPath {
        self.resolve_impl(dos_path, true, true)
    }

    /// Resolve a DOS path for LFN (Long File Name) operations.
    /// Preserves the original case of path components.
    pub fn resolve_lfn(&mut self, dos_path: &str) -> ResolvedPath {
        self.resolve_impl(dos_path, false, false)
    }

    fn resolve_impl(
        &mut self,
        dos_path: &str,
        uppercase: bool,
        preserve_final_empty_ext_dot: bool,
    ) -> ResolvedPath {
        let (drive_opt, p) = Self::split_drive_and_path(dos_path);
        let drive = drive_opt.unwrap_or(self.current_drive);

        // Ensure current dir entry exists for this drive if we might need it
        let current_dir = self
            .current_dirs
            .entry(drive)
            .or_insert_with(|| String::from(r"\"))
            .clone();

        let p = p.replace('/', r"\");

        let full_path = if p.starts_with('\\') {
            // Absolute path on drive
            p
        } else {
            // Relative path
            let mut combined = current_dir;
            if !combined.ends_with('\\') {
                combined.push('\\');
            }
            combined.push_str(&p);
            combined
        };

        let normalized = if preserve_final_empty_ext_dot {
            Self::normalize_dos_find_pattern(&full_path)
        } else if uppercase {
            Self::normalize_dos_dir(&full_path)
        } else {
            Self::normalize_dos_dir_preserve_case(&full_path)
        };

        ResolvedPath {
            drive,
            path: normalized,
        }
    }

    pub fn set_current_dir(&mut self, dos_path: &str) {
        let resolved = self.resolve(dos_path);
        self.current_dirs.insert(resolved.drive, resolved.path);
        self.cwd_generation = self.cwd_generation.wrapping_add(1);
    }

    /// Set a current directory from an LFN call while retaining the spelling
    /// supplied by the guest. The ordinary DOS route intentionally folds its
    /// path to upper-case for 8.3 compatibility; the LFN route must preserve
    /// mixed-case and long components so AX=7147h can return them.
    pub fn set_current_dir_lfn(&mut self, dos_path: &str) {
        let resolved = self.resolve_lfn(dos_path);
        self.current_dirs.insert(resolved.drive, resolved.path);
        self.cwd_generation = self.cwd_generation.wrapping_add(1);
    }

    pub fn get_current_dir(&self, drive: u8) -> String {
        self.current_dirs
            .get(&drive)
            .cloned()
            .unwrap_or_else(|| String::from(r"\"))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #[test]
    fn vshare_open_matrix_matches_contract() {
        use super::*;
        // VSHARE contract: first mode is share/access; each string
        // answers the 20 second modes in share order 00,40,30,20,10 and
        // access order read,write,readwrite,reserved. Reserved always fails.
        let shares = [0x00, 0x40, 0x30, 0x20, 0x10];
        let expected = [
            "11101000000010000000",
            "11101000000000000000",
            "11101000000000000000",
            "11101110000011100000",
            "00001110111000000000",
            "00001110000000000000",
            "00000100000001000000",
            "00000100010000000000",
            "00000100000000000000",
            "10001000000010000000",
            "00001000100000000000",
            "00001000000000000000",
            "00000000000000000000",
            "00000000000000000000",
            "00000000000000000000",
        ];
        for (first_index, oracle) in expected.iter().enumerate() {
            let first_share = shares[first_index / 3];
            let first_access = (first_index % 3) as u8;
            for (second_index, &answer) in oracle.as_bytes().iter().enumerate() {
                let second_share = shares[second_index / 4];
                let second_access = (second_index % 4) as u8;
                let allowed = second_access != 3
                    && !sharing_modes_conflict(
                        first_share,
                        first_access,
                        second_share,
                        second_access,
                    );
                assert_eq!(allowed, answer == b'1', "first {first_share:02X}/{first_access}, second {second_share:02X}/{second_access}");
            }
        }
    }
    use super::*;
    use anyhow::Result;
    use std::io::{Cursor, Read, Seek, SeekFrom, Write};

    /// Only a real directory component counts. A child of the root has none
    /// that could be missing, because the root always exists — which is what
    /// keeps `\FOO.TXT` reporting DOS error 2 rather than 3.
    #[test]
    fn dos_parent_dir_reports_only_a_real_directory_component() {
        assert_eq!(
            PathResolver::dos_parent_dir(r"\SAVE\CMRALLY.CFG"),
            Some(r"\SAVE")
        );
        assert_eq!(PathResolver::dos_parent_dir(r"\A\B\C.TXT"), Some(r"\A\B"));

        assert_eq!(PathResolver::dos_parent_dir(r"\FOO.TXT"), None);
        assert_eq!(PathResolver::dos_parent_dir("FOO.TXT"), None);
        assert_eq!(PathResolver::dos_parent_dir(r"\"), None);
        assert_eq!(PathResolver::dos_parent_dir(""), None);
        // A trailing separator must not invent a component.
        assert_eq!(PathResolver::dos_parent_dir(r"\SAVE\"), None);
        assert_eq!(PathResolver::dos_parent_dir(r"\A\B\"), Some(r"\A"));
    }

    #[derive(Default)]
    struct MemFile {
        cursor: Cursor<Vec<u8>>,
    }

    impl Read for MemFile {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.cursor.read(buf)
        }
    }

    impl Write for MemFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.cursor.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.cursor.flush()
        }
    }

    impl Seek for MemFile {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.cursor.seek(pos)
        }
    }

    impl VirtualFile for MemFile {
        fn set_len(&mut self, size: u64) -> Result<()> {
            self.cursor.get_mut().resize(size as usize, 0);
            Ok(())
        }

        fn sync_all(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct ProbeFileState {
        flushes: std::sync::atomic::AtomicUsize,
        drops: std::sync::atomic::AtomicUsize,
        fail_flush: std::sync::atomic::AtomicBool,
    }

    struct ProbeFile {
        cursor: Cursor<Vec<u8>>,
        state: std::sync::Arc<ProbeFileState>,
    }

    impl Drop for ProbeFile {
        fn drop(&mut self) {
            self.state
                .drops
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    impl Read for ProbeFile {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.cursor.read(buf)
        }
    }

    impl Write for ProbeFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.cursor.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.state
                .flushes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self
                .state
                .fail_flush
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(std::io::Error::other("injected flush failure"));
            }
            self.cursor.flush()
        }
    }

    impl Seek for ProbeFile {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.cursor.seek(pos)
        }
    }

    impl VirtualFile for ProbeFile {
        fn set_len(&mut self, size: u64) -> Result<()> {
            self.cursor.get_mut().resize(size as usize, 0);
            Ok(())
        }

        fn sync_all(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn make_probe_file() -> (Box<dyn VirtualFile>, std::sync::Arc<ProbeFileState>) {
        let state = std::sync::Arc::new(ProbeFileState::default());
        (
            Box::new(ProbeFile {
                cursor: Cursor::default(),
                state: std::sync::Arc::clone(&state),
            }),
            state,
        )
    }

    fn make_file() -> Box<dyn VirtualFile> {
        Box::new(MemFile::default())
    }

    /// MDD-BUG-FLUX-00157: INT 21h close must drain emulator-side buffers
    /// (`flush`) and must NEVER issue a host storage barrier (`sync_all` is
    /// `fcntl(F_FULLFSYNC)` on macOS — a multi-millisecond stall per guest
    /// close that real DOS never had). The barrier belongs to AH=68h Commit
    /// File alone. Red without the `try_free` fix: the old close called
    /// `sync_all` once and `flush` never.
    #[test]
    fn close_drains_buffers_without_a_host_storage_barrier() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        struct BarrierProbeFile {
            syncs: Arc<AtomicU32>,
            flushes: Arc<AtomicU32>,
        }
        impl Read for BarrierProbeFile {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Ok(0)
            }
        }
        impl Write for BarrierProbeFile {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }
        impl Seek for BarrierProbeFile {
            fn seek(&mut self, _pos: SeekFrom) -> std::io::Result<u64> {
                Ok(0)
            }
        }
        impl VirtualFile for BarrierProbeFile {
            fn set_len(&mut self, _size: u64) -> Result<()> {
                Ok(())
            }
            fn sync_all(&mut self) -> Result<()> {
                self.syncs.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }

        let syncs = Arc::new(AtomicU32::new(0));
        let flushes = Arc::new(AtomicU32::new(0));
        let mut table = FileTable::new();
        let handle = table
            .allocate_with_sharing(
                Box::new(BarrierProbeFile {
                    syncs: Arc::clone(&syncs),
                    flushes: Arc::clone(&flushes),
                }),
                String::from(r"C:\LOG.TXT"),
                0x1000,
                SHARE_COMPAT,
                ACCESS_READWRITE,
            )
            .unwrap();

        assert!(table.try_free(handle).unwrap());
        assert_eq!(
            syncs.load(Ordering::Relaxed),
            0,
            "close must never issue a host storage barrier"
        );
        assert_eq!(
            flushes.load(Ordering::Relaxed),
            1,
            "close must drain emulator-side buffers exactly once"
        );
    }

    #[test]
    fn test_snapshot_preserves_expanded_handle_limit() {
        // MDD-BUG-CRUCIBLE-00059: an AH=67h SetHandleCount expansion must survive
        // a file-table snapshot/restore round trip, or a guest that raised its
        // handle limit reverts to the default after a save/restore and fails to
        // open handles that were valid before the snapshot.
        let mut table = FileTable::new();
        assert_eq!(table.handle_limit(), 20, "default JFT size");

        table.set_handle_count(100); // INT 21h AH=67h
        assert_eq!(table.handle_limit(), 100);

        let snap = table.snapshot();

        // Restore into a fresh table (default limit) and confirm the raised limit
        // is carried across.
        let mut restored = FileTable::new();
        restored.restore(&snap);
        assert_eq!(
            restored.handle_limit(),
            100,
            "restore must carry the AH=67h-expanded handle limit"
        );
    }

    #[test]
    fn test_sft_access_mode_predicates() {
        let mut table = FileTable::new();
        let cases = [
            (ACCESS_READ, true, false),
            (ACCESS_WRITE, false, true),
            (ACCESS_READWRITE, true, true),
            (0x03, false, false),
        ];

        for (access, can_read, can_write) in cases {
            let handle = table
                .allocate_with_sharing(
                    make_file(),
                    format!(r"C:\ACCESS{access}.DAT"),
                    0x1000,
                    SHARE_COMPAT,
                    access,
                )
                .unwrap();
            let sft = table.get_sft(handle).unwrap();
            assert_eq!(sft.access_mode & 0x07, access);
            assert_eq!(sft.allows_read(), can_read, "access={access:02X}");
            assert_eq!(sft.allows_write(), can_write, "access={access:02X}");
        }
    }

    #[test]
    fn test_file_table_allocate_and_free() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\FILE.TXT"), 0x1000)
            .unwrap();

        assert_eq!(handle, 5);
        let sft = table.get_sft(handle).unwrap();
        assert_eq!(sft.path, r"C:\FILE.TXT");
        assert_eq!(sft.ref_count, 1);
        assert!(table.is_valid(handle));

        assert!(table.free(handle));
        assert!(!table.is_valid(handle));
    }

    #[test]
    fn test_file_table_is_valid_out_of_range() {
        let table = FileTable::new();
        assert!(!table.is_valid(999));
    }

    #[test]
    fn test_file_table_get_owner_and_mutate_sft() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\OWNER.DAT"), 0x1234)
            .unwrap();

        assert_eq!(table.get_owner(handle), Some(0x1234));

        {
            let sft = table.get_sft_mut(handle).unwrap();
            sft.position = 42;
        }
        assert_eq!(table.get_sft(handle).unwrap().position, 42);
    }

    #[test]
    fn test_file_lock_overlap_checks() {
        let lock = FileLock {
            offset: 10,
            length: 0,
        };
        assert!(!lock.overlaps(0, 5));

        let lock = FileLock {
            offset: 10,
            length: 5,
        };
        assert!(!lock.overlaps(0, 0));
        assert!(!lock.overlaps(0, 5));
        assert!(lock.overlaps(12, 2));
        assert!(!lock.overlaps(20, 3));
    }

    #[test]
    fn test_file_table_get_owner_invalid_returns_none() {
        let table = FileTable::new();
        assert!(table.get_owner(5).is_none());
        assert!(table.get_owner(999).is_none());
    }

    #[test]
    fn test_file_table_get_sft_mut_invalid_returns_none() {
        let mut table = FileTable::new();
        assert!(table.get_sft_mut(5).is_none());
    }

    #[test]
    fn test_file_table_allocation_fails_when_handles_full() {
        let mut table = FileTable::new();
        // Grow the JFT to its maximum, then fill it completely so the next
        // allocation has no free handle (real DOS: "too many open files").
        table.set_handle_count(255);
        let mut handles = Vec::new();

        while let Some(handle) = table.allocate(make_file(), String::from(r"C:\FULL.TXT"), 0x2000) {
            handles.push(handle);
        }
        assert!(!handles.is_empty());

        let result = table.allocate(make_file(), String::from(r"C:\NOPE.TXT"), 0x2000);
        assert!(result.is_none());

        let sft_entries = table.sft.iter().filter(|entry| entry.is_some()).count();
        assert_eq!(sft_entries, handles.len());

        for handle in handles {
            assert!(table.free(handle));
        }
    }

    #[test]
    fn test_file_table_duplicate_shares_sft() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\SHARED.DAT"), 0x2000)
            .unwrap();

        let duplicate = table.duplicate(handle, 0x2000).unwrap();
        assert_ne!(handle, duplicate);

        let sft_index = table.handles[handle as usize].as_ref().unwrap().sft_index;
        let dup_index = table.handles[duplicate as usize]
            .as_ref()
            .unwrap()
            .sft_index;
        assert_eq!(sft_index, dup_index);

        let sft = table.sft[sft_index].as_ref().unwrap();
        assert_eq!(sft.ref_count, 2);

        table.free(duplicate);
        let sft = table.sft[sft_index].as_ref().unwrap();
        assert_eq!(sft.ref_count, 1);
    }

    #[test]
    fn test_file_table_force_duplicate_overwrites_dest() {
        let mut table = FileTable::new();
        let src = table
            .allocate(make_file(), String::from(r"C:\SRC.BIN"), 0x3000)
            .unwrap();
        let dst = table
            .allocate(make_file(), String::from(r"C:\DST.BIN"), 0x3000)
            .unwrap();

        let dst_sft_index = table.handles[dst as usize].as_ref().unwrap().sft_index;

        assert!(table.force_duplicate(src, dst, 0x3000));

        let src_index = table.handles[src as usize].as_ref().unwrap().sft_index;
        let new_dst_index = table.handles[dst as usize].as_ref().unwrap().sft_index;
        assert_eq!(src_index, new_dst_index);
        assert!(table.sft[dst_sft_index].is_none());
    }

    #[test]
    fn test_file_table_force_duplicate_invalid_destination_restores_refcount() {
        let mut table = FileTable::new();
        let src = table
            .allocate(make_file(), String::from(r"C:\SRC.BIN"), 0x3000)
            .unwrap();
        let src_index = table.handles[src as usize].as_ref().unwrap().sft_index;
        let before = table.sft[src_index].as_ref().unwrap().ref_count;

        assert!(!table.force_duplicate(src, 999, 0x3000));
        let after = table.sft[src_index].as_ref().unwrap().ref_count;
        assert_eq!(before, after);
    }

    #[test]
    fn test_file_table_force_duplicate_invalid_source_returns_false() {
        let mut table = FileTable::new();
        assert!(!table.force_duplicate(999, 6, 0x3000));
    }

    #[test]
    fn test_file_table_force_duplicate_missing_sft_returns_false() {
        let mut table = FileTable::new();
        let src = table
            .allocate(make_file(), String::from(r"C:\SRC.BIN"), 0x3000)
            .unwrap();
        let src_index = table.handles[src as usize].as_ref().unwrap().sft_index;
        table.sft[src_index] = None;

        assert!(!table.force_duplicate(src, 6, 0x3000));
    }

    #[test]
    fn test_file_table_insert_at_overwrites_and_cleans_old() {
        let mut table = FileTable::new();
        assert!(table.insert_at(6, make_file(), String::from(r"C:\ONE.TXT"), 0x4000));

        let first_index = table.handles[6].as_ref().unwrap().sft_index;
        assert!(table.sft[first_index].is_some());

        assert!(table.insert_at(6, make_file(), String::from(r"C:\TWO.TXT"), 0x4000));
        assert!(table.sft[first_index].is_none());
    }

    #[test]
    fn test_file_table_insert_at_invalid_slot_cleans_sft() {
        let mut table = FileTable::new();
        assert!(!table.insert_at(999, make_file(), String::from(r"C:\BAD.TXT"), 0x4000));
        let sft_entries = table.sft.iter().filter(|entry| entry.is_some()).count();
        assert_eq!(sft_entries, 0);
    }

    #[test]
    fn test_file_table_insert_at_out_of_range_sft_index() {
        let mut table = FileTable::new();
        table.handles[6] = Some(FileHandle {
            sft_index: 999,
            owner_psp: 0x4000,
        });

        assert!(table.insert_at(6, make_file(), String::from(r"C:\NEW.TXT"), 0x4000));
    }

    #[test]
    fn test_file_table_close_process_files() {
        let mut table = FileTable::new();
        let handle_a = table
            .allocate(make_file(), String::from(r"C:\A.TXT"), 0x1111)
            .unwrap();
        let handle_b = table
            .allocate(make_file(), String::from(r"C:\B.TXT"), 0x2222)
            .unwrap();
        let handle_c = table
            .allocate(make_file(), String::from(r"C:\C.TXT"), 0x1111)
            .unwrap();

        table.close_process_files(0x1111);

        assert!(!table.is_valid(handle_a));
        assert!(table.is_valid(handle_b));
        assert!(!table.is_valid(handle_c));
    }

    #[test]
    fn test_path_resolver_normalize() {
        assert_eq!(PathResolver::normalize_dos_dir(r"foo\bar\"), r"\FOO\BAR");
        assert_eq!(
            PathResolver::normalize_dos_dir(r"\foo\.\bar\..\baz"),
            r"\FOO\BAZ"
        );
    }

    #[test]
    fn test_path_resolver_normalize_fcb_depad_spaces() {
        // Real DOS de-pads FCB-style trailing-space-padded 8.3 names. Dinamic's
        // El Capitan Trueno opens "FONDOS  .GRF"; Das Boot opens "SUB     .D01".
        assert_eq!(
            PathResolver::normalize_dos_dir(r"\game\FONDOS  .GRF"),
            r"\GAME\FONDOS.GRF"
        );
        assert_eq!(PathResolver::normalize_dos_dir("SUB     .D01"), r"\SUB.D01");
        // Trailing spaces with no extension are trimmed too.
        assert_eq!(PathResolver::normalize_dos_dir("CHARSET     "), r"\CHARSET");
        // Embedded (non-trailing) spaces are preserved (we only de-pad trailing).
        assert_eq!(
            PathResolver::normalize_dos_dir("SAVE 01.DAT"),
            r"\SAVE 01.DAT"
        );
        // LFN resolution must PRESERVE spaces (uppercase=false, no de-pad).
        assert_eq!(
            PathResolver::normalize_dos_dir_preserve_case(r"\My Game\file  .dat"),
            r"\My Game\file  .dat"
        );
    }

    #[test]
    fn test_path_resolver_find_pattern_preserves_blank_extension() {
        let mut resolver = PathResolver::new();
        resolver.set_current_dir(r"C:\GAMES");

        let resolved = resolver.resolve_find_pattern("*.");
        assert_eq!(resolved.path, r"\GAMES\*.");

        let resolved = resolver.resolve_find_pattern(r"C:\DATA\README.   ");
        assert_eq!(resolved.path, r"\DATA\README.");

        assert_eq!(PathResolver::normalize_dos_dir("*."), r"\*");
    }

    #[test]
    fn test_path_resolver_split_drive_and_path() {
        let (drive, path) = PathResolver::split_drive_and_path(r"C:\DOS\COMMAND.COM");
        assert_eq!(drive, Some(2));
        assert_eq!(path, r"\DOS\COMMAND.COM");

        let (drive, path) = PathResolver::split_drive_and_path(r"c:TOOLS\EDIT.EXE");
        assert_eq!(drive, Some(2));
        assert_eq!(path, r"TOOLS\EDIT.EXE");

        let (drive, path) = PathResolver::split_drive_and_path("foo");
        assert_eq!(drive, None);
        assert_eq!(path, "foo");
    }

    #[test]
    fn test_path_resolver_resolve_and_dirs() {
        let mut resolver = PathResolver::new();
        resolver.set_current_dir(r"C:\GAMES");

        let resolved = resolver.resolve("TEST.EXE");
        assert_eq!(resolved.drive, 2);
        assert_eq!(resolved.path, r"\GAMES\TEST.EXE");

        let resolved = resolver.resolve(r"\DOS\COMMAND.COM");
        assert_eq!(resolved.path, r"\DOS\COMMAND.COM");

        let resolved = resolver.resolve(r"D:TOOLS\EDIT.EXE");
        assert_eq!(resolved.drive, 3);
        assert_eq!(resolved.path, r"\TOOLS\EDIT.EXE");

        resolver.set_current_drive(4);
        assert_eq!(resolver.get_current_dir(4), r"\");
    }

    #[test]
    fn test_path_resolver_get_current_dir_unknown_drive() {
        let resolver = PathResolver::new();
        assert_eq!(resolver.get_current_dir(5), r"\");
    }

    #[test]
    fn test_path_resolver_resolve_empty_relative() {
        let mut resolver = PathResolver::new();
        resolver.set_current_dir(r"C:\GAMES");

        let resolved = resolver.resolve("");
        assert_eq!(resolved.path, r"\GAMES");
    }

    #[test]
    fn test_file_table_free_invalid_handles() {
        let mut table = FileTable::new();
        assert!(!table.free(0));
        assert!(!table.free(999));
    }

    #[test]
    fn test_file_table_free_missing_sft_entry() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\TEMP.DAT"), 0x2000)
            .unwrap();
        let sft_index = table.handles[handle as usize].as_ref().unwrap().sft_index;
        table.sft[sft_index] = None;

        assert!(table.free(handle));
    }

    #[test]
    fn test_file_table_free_out_of_range_sft_index() {
        let mut table = FileTable::new();
        table.handles[6] = Some(FileHandle {
            sft_index: 999,
            owner_psp: 0x2000,
        });

        assert!(table.free(6));
    }

    #[test]
    fn test_file_table_duplicate_invalid_handle_returns_none() {
        let mut table = FileTable::new();
        assert!(table.duplicate(0, 0x2000).is_none());
        assert!(table.duplicate(999, 0x2000).is_none());
    }

    #[test]
    fn test_file_table_duplicate_when_full_does_not_increment_refcount() {
        let mut table = FileTable::new();
        // Grow the JFT to its maximum so we can fill every remaining slot.
        table.set_handle_count(255);
        let first = table
            .allocate(make_file(), String::from(r"C:\BASE.DAT"), 0x2000)
            .unwrap();

        // Fill every remaining handle so a duplicate has nowhere to land.
        while table
            .allocate(make_file(), String::from(r"C:\FILL.DAT"), 0x2000)
            .is_some()
        {}

        let sft_index = table.handles[first as usize].as_ref().unwrap().sft_index;
        let before = table.sft[sft_index].as_ref().unwrap().ref_count;
        let duplicate = table.duplicate(first, 0x2000);
        assert!(duplicate.is_none());
        let after = table.sft[sft_index].as_ref().unwrap().ref_count;
        assert_eq!(before, after);
    }

    #[test]
    fn test_file_table_get_sft_invalid_returns_none() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\TEMP.DAT"), 0x2000)
            .unwrap();
        table.free(handle);

        assert!(table.get_sft(handle).is_none());
        assert!(table.get_sft(0).is_none());
    }

    #[test]
    fn test_file_table_lock_unlock_region_paths() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\LOCK.DAT"), 0x2000)
            .unwrap();

        assert_eq!(table.lock_region(handle, 0, 10), Ok(()));
        assert_eq!(table.lock_region(handle, 20, 5), Ok(()));
        assert_eq!(table.lock_region(handle, 5, 2), Err(33));
        assert_eq!(table.unlock_region(handle, 1, 2), Err(33));
        assert_eq!(table.unlock_region(handle, 0, 10), Ok(()));
    }

    #[test]
    fn test_file_table_region_locks_conflict_across_independent_sfts() {
        let mut table = FileTable::new();
        let first = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\LOCK.DAT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_READWRITE,
            )
            .unwrap();
        let second = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"c:\lock.dat"),
                0x2000,
                SHARE_DENY_NONE,
                ACCESS_READWRITE,
            )
            .unwrap();

        assert_eq!(table.lock_region(first, 10, 5), Ok(()));
        assert_eq!(table.lock_region(second, 12, 1), Err(33));
        assert_eq!(table.lock_region(second, 15, 1), Ok(()));
    }

    #[test]
    fn test_file_table_io_lock_checks_use_actual_span_and_open_identity() {
        let mut table = FileTable::new();
        let make_data_file = || {
            Box::new(MemFile {
                cursor: Cursor::new(b"ABCDE".to_vec()),
            }) as Box<dyn VirtualFile>
        };
        let locker = table
            .allocate_with_sharing(
                make_data_file(),
                String::from(r"C:\IOLOCK.DAT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_READWRITE,
            )
            .unwrap();
        let duplicate = table.duplicate(locker, 0x1000).unwrap();
        let other = table
            .allocate_with_sharing(
                make_data_file(),
                String::from(r"C:\IOLOCK.DAT"),
                0x2000,
                SHARE_DENY_NONE,
                ACCESS_READWRITE,
            )
            .unwrap();
        assert_eq!(table.lock_region(locker, 2, 2), Ok(()));
        assert_eq!(table.lock_region(locker, 10, 2), Ok(()));

        table
            .get_sft_mut(other)
            .unwrap()
            .file
            .seek(SeekFrom::Start(2))
            .unwrap();
        assert_eq!(table.read_region_lock_violation(other, 1), Ok(true));
        assert_eq!(table.write_region_lock_violation(other, 1), Ok(true));

        table
            .get_sft_mut(other)
            .unwrap()
            .file
            .seek(SeekFrom::Start(4))
            .unwrap();
        assert_eq!(
            table.read_region_lock_violation(other, 10),
            Ok(false),
            "a requested read must not conflict beyond its actual EOF span"
        );

        table
            .get_sft_mut(other)
            .unwrap()
            .file
            .seek(SeekFrom::Start(9))
            .unwrap();
        assert_eq!(table.write_region_lock_violation(other, 2), Ok(true));

        table
            .get_sft_mut(other)
            .unwrap()
            .file
            .seek(SeekFrom::Start(1))
            .unwrap();
        assert_eq!(table.truncate_region_lock_violation(other), Ok(true));

        table
            .get_sft_mut(duplicate)
            .unwrap()
            .file
            .seek(SeekFrom::Start(2))
            .unwrap();
        assert_eq!(
            table.read_region_lock_violation(duplicate, 1),
            Ok(false),
            "a duplicated handle shares the lock owner's open identity"
        );
        assert_eq!(
            table.write_region_lock_violation(duplicate, 1),
            Ok(false),
            "a duplicated handle shares the lock owner's open identity"
        );
    }

    #[test]
    fn test_file_table_lock_unlock_zero_length_noop() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\ZERO.DAT"), 0x2000)
            .unwrap();

        assert_eq!(table.lock_region(handle, 0, 0), Ok(()));
        assert_eq!(table.unlock_region(handle, 0, 0), Ok(()));
    }

    #[test]
    fn test_file_table_lock_region_invalid_handle_errors() {
        let mut table = FileTable::new();
        assert_eq!(table.lock_region(6, 0, 1), Err(6));
    }

    #[test]
    fn test_file_table_lock_unlock_missing_sft_errors() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\MISSING.DAT"), 0x2000)
            .unwrap();
        let sft_index = table.handles[handle as usize].as_ref().unwrap().sft_index;
        table.sft[sft_index] = None;

        assert_eq!(table.lock_region(handle, 0, 1), Err(6));
        assert_eq!(table.unlock_region(handle, 0, 1), Err(6));
    }

    #[test]
    fn test_file_table_close_process_files_shared_sft() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\SHARE.DAT"), 0x1111)
            .unwrap();
        let dup = table.duplicate(handle, 0x2222).unwrap();

        let sft_index = table.handles[handle as usize].as_ref().unwrap().sft_index;
        assert_eq!(table.sft[sft_index].as_ref().unwrap().ref_count, 2);

        table.close_process_files(0x1111);

        assert!(!table.is_valid(handle));
        assert!(table.is_valid(dup));
        assert_eq!(table.sft[sft_index].as_ref().unwrap().ref_count, 1);
    }

    #[test]
    fn test_file_table_insert_at_preserves_shared_sft() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\ONE.DAT"), 0x1111)
            .unwrap();
        let dup = table.duplicate(handle, 0x2222).unwrap();

        let shared_index = table.handles[handle as usize].as_ref().unwrap().sft_index;
        assert_eq!(table.sft[shared_index].as_ref().unwrap().ref_count, 2);

        assert!(table.insert_at(handle, make_file(), String::from(r"C:\TWO.DAT"), 0x1111));

        let new_index = table.handles[handle as usize].as_ref().unwrap().sft_index;
        assert_ne!(new_index, shared_index);
        assert!(table.is_valid(dup));
        assert!(table.sft[shared_index].is_some());
        assert_eq!(table.sft[shared_index].as_ref().unwrap().ref_count, 1);
    }

    #[test]
    fn test_file_table_force_duplicate_to_std_handle() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\STD.DAT"), 0x1111)
            .unwrap();

        assert!(table.force_duplicate(handle, 0, 0x1111));

        let src_index = table.handles[handle as usize].as_ref().unwrap().sft_index;
        let dst_index = table.handles[0].as_ref().unwrap().sft_index;
        assert_eq!(src_index, dst_index);
        assert_eq!(table.sft[src_index].as_ref().unwrap().ref_count, 2);
    }

    #[test]
    fn force_duplicate_replacing_stdout_releases_sft_and_sharing_identity() {
        use std::sync::atomic::Ordering;

        let mut table = FileTable::new();
        let mut previous: Option<(String, std::sync::Arc<ProbeFileState>)> = None;

        for index in 0..300 {
            let path = format!(r"C:\OUT{index:03}.LOG");
            let (file, state) = make_probe_file();
            let src = table
                .allocate_with_sharing(file, path.clone(), 0x1000, SHARE_DENY_ALL, ACCESS_READWRITE)
                .expect("file opens must continue beyond the SFT capacity");

            assert!(table.force_duplicate(src, 1, 0x1000));
            assert!(table.try_free(src).unwrap());

            if let Some((old_path, old_state)) = previous.take() {
                assert_eq!(
                    old_state.drops.load(Ordering::Relaxed),
                    1,
                    "replacing stdout must drop its previous backend"
                );

                let reopened = table
                    .allocate_with_sharing(
                        make_file(),
                        old_path,
                        0x2000,
                        SHARE_DENY_ALL,
                        ACCESS_READWRITE,
                    )
                    .expect("replacing stdout must release the old sharing identity");
                assert!(table.try_free(reopened).unwrap());
            }

            previous = Some((path, state));
        }

        assert!(table.try_free(1).unwrap());
        assert_eq!(
            previous.unwrap().1.drops.load(Ordering::Relaxed),
            1,
            "closing the final stdout binding must drop its backend"
        );
    }

    #[test]
    fn force_duplicate_same_handle_is_a_noop() {
        use std::sync::atomic::Ordering;

        let mut table = FileTable::new();
        let (file, state) = make_probe_file();
        let handle = table
            .allocate_with_sharing(
                file,
                String::from(r"C:\SAME.DAT"),
                0x1000,
                SHARE_COMPAT,
                ACCESS_READWRITE,
            )
            .unwrap();
        let sft_index = table.handles[handle as usize].unwrap().sft_index;

        assert!(table.force_duplicate(handle, handle, 0x2222));
        assert_eq!(table.get_owner(handle), Some(0x1000));
        assert_eq!(table.sft[sft_index].as_ref().unwrap().ref_count, 1);
        assert_eq!(state.flushes.load(Ordering::Relaxed), 0);
        assert_eq!(state.drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn force_duplicate_same_standard_handle_is_a_noop() {
        use std::sync::atomic::Ordering;

        let mut table = FileTable::new();
        let (file, state) = make_probe_file();
        let src = table
            .allocate_with_sharing(
                file,
                String::from(r"C:\SAME-STD.DAT"),
                0x1000,
                SHARE_COMPAT,
                ACCESS_READWRITE,
            )
            .unwrap();
        assert!(table.force_duplicate(src, 0, 0x1111));
        let sft_index = table.handles[0].unwrap().sft_index;
        assert_eq!(table.sft[sft_index].as_ref().unwrap().ref_count, 2);

        assert!(table.force_duplicate(0, 0, 0x2222));
        assert_eq!(table.get_owner(0), Some(0x1111));
        assert_eq!(table.sft[sft_index].as_ref().unwrap().ref_count, 2);
        assert_eq!(state.flushes.load(Ordering::Relaxed), 0);
        assert_eq!(state.drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn force_duplicate_shared_sft_standard_destination_keeps_reference_count() {
        use std::sync::atomic::Ordering;

        let mut table = FileTable::new();
        let (file, state) = make_probe_file();
        let src = table
            .allocate_with_sharing(
                file,
                String::from(r"C:\SHARED-STD.DAT"),
                0x1000,
                SHARE_COMPAT,
                ACCESS_READWRITE,
            )
            .unwrap();
        assert!(table.force_duplicate(src, 0, 0x1000));
        let second_source = table.duplicate(src, 0x1000).unwrap();
        let sft_index = table.handles[src as usize].unwrap().sft_index;
        assert_eq!(table.sft[sft_index].as_ref().unwrap().ref_count, 3);

        assert!(table.force_duplicate(second_source, 0, 0x2000));
        assert_eq!(table.sft[sft_index].as_ref().unwrap().ref_count, 3);
        assert_eq!(state.flushes.load(Ordering::Relaxed), 0);
        assert_eq!(state.drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn force_duplicate_standard_destination_flush_failure_is_atomic_and_retryable() {
        use std::sync::atomic::Ordering;

        let mut table = FileTable::new();
        let (old_file, old_state) = make_probe_file();
        let old_source = table
            .allocate_with_sharing(
                old_file,
                String::from(r"C:\OLD-STD.DAT"),
                0x1000,
                SHARE_COMPAT,
                ACCESS_READWRITE,
            )
            .unwrap();
        table
            .get_sft_mut(old_source)
            .unwrap()
            .file
            .write_all(b"dirty bytes")
            .unwrap();
        assert!(table.force_duplicate(old_source, 1, 0x1000));
        assert!(table.try_free(old_source).unwrap());
        let old_sft = table.handles[1].unwrap().sft_index;

        let (new_file, new_state) = make_probe_file();
        let src = table
            .allocate_with_sharing(
                new_file,
                String::from(r"C:\NEW-STD.DAT"),
                0x1000,
                SHARE_COMPAT,
                ACCESS_READWRITE,
            )
            .unwrap();
        let new_sft = table.handles[src as usize].unwrap().sft_index;

        old_state.fail_flush.store(true, Ordering::Relaxed);
        assert!(!table.force_duplicate(src, 1, 0x1000));
        assert_eq!(table.handles[1].unwrap().sft_index, old_sft);
        assert_eq!(table.handles[src as usize].unwrap().sft_index, new_sft);
        assert_eq!(table.sft[old_sft].as_ref().unwrap().ref_count, 1);
        assert_eq!(table.sft[new_sft].as_ref().unwrap().ref_count, 1);
        assert!(table.sft[old_sft].is_some());
        assert_eq!(old_state.flushes.load(Ordering::Relaxed), 1);
        assert_eq!(old_state.drops.load(Ordering::Relaxed), 0);
        assert_eq!(new_state.drops.load(Ordering::Relaxed), 0);

        let mut bytes = [0; 11];
        let old_file = &mut table.get_sft_mut(1).unwrap().file;
        old_file.seek(SeekFrom::Start(0)).unwrap();
        old_file.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"dirty bytes");

        old_state.fail_flush.store(false, Ordering::Relaxed);
        assert!(table.force_duplicate(src, 1, 0x1000));
        assert!(table.sft[old_sft].is_none());
        assert_eq!(old_state.flushes.load(Ordering::Relaxed), 2);
        assert_eq!(old_state.drops.load(Ordering::Relaxed), 1);
        assert_eq!(table.sft[new_sft].as_ref().unwrap().ref_count, 2);
    }

    #[test]
    fn force_duplicate_missing_source_sft_preserves_occupied_destination() {
        use std::sync::atomic::Ordering;

        let mut table = FileTable::new();
        let src = table
            .allocate(make_file(), String::from(r"C:\MISSING-SOURCE.DAT"), 0x1000)
            .unwrap();
        let (dst_file, dst_state) = make_probe_file();
        let dst = table
            .allocate_with_sharing(
                dst_file,
                String::from(r"C:\PRESERVED-DEST.DAT"),
                0x1000,
                SHARE_COMPAT,
                ACCESS_READWRITE,
            )
            .unwrap();
        let dst_sft = table.handles[dst as usize].unwrap().sft_index;
        let src_sft = table.handles[src as usize].unwrap().sft_index;
        table.sft[src_sft] = None;

        assert!(!table.force_duplicate(src, dst, 0x2000));
        assert_eq!(
            table.handles[dst as usize].map(|handle| handle.sft_index),
            Some(dst_sft),
            "invalid source SFT must not close the occupied destination"
        );
        assert_eq!(table.sft[dst_sft].as_ref().unwrap().ref_count, 1);
        assert_eq!(dst_state.flushes.load(Ordering::Relaxed), 0);
        assert_eq!(dst_state.drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_path_resolver_normalize_dotdot_root() {
        assert_eq!(PathResolver::normalize_dos_dir(r"\..\"), r"\");
    }

    #[test]
    fn test_path_resolver_normalize_empty_path() {
        assert_eq!(PathResolver::normalize_dos_dir(""), r"\");
    }

    #[test]
    fn test_path_resolver_split_drive_invalid_prefix() {
        let (drive, path) = PathResolver::split_drive_and_path("1:\\FOO");
        assert_eq!(drive, None);
        assert_eq!(path, "1:\\FOO");
    }

    #[test]
    fn test_path_resolver_resolve_parent_relative() {
        let mut resolver = PathResolver::new();
        resolver.set_current_dir(r"C:\GAMES\BIN");

        let resolved = resolver.resolve("..\\PLAY.EXE");
        assert_eq!(resolved.path, r"\GAMES\PLAY.EXE");
    }

    #[test]
    fn test_path_resolver_resolve_drive_only_uses_current_dir() {
        let mut resolver = PathResolver::new();
        resolver.set_current_dir(r"D:\TOOLS");

        let resolved = resolver.resolve("D:");
        assert_eq!(resolved.drive, 3);
        assert_eq!(resolved.path, r"\TOOLS");
    }

    #[test]
    fn test_path_resolver_resolve_uses_current_drive() {
        let mut resolver = PathResolver::new();
        resolver.set_current_drive(1);

        let resolved = resolver.resolve("FILE.TXT");
        assert_eq!(resolved.drive, 1);
        assert_eq!(resolved.path, r"\FILE.TXT");
    }

    #[test]
    fn test_path_resolver_set_current_dir_does_not_change_drive() {
        let mut resolver = PathResolver::new();
        resolver.set_current_drive(2);
        resolver.set_current_dir(r"D:\BIN");

        assert_eq!(resolver.get_current_drive(), 2);
        assert_eq!(resolver.get_current_dir(3), r"\BIN");
    }

    #[test]
    fn test_path_resolver_normalize_dotdot_beyond_root() {
        assert_eq!(PathResolver::normalize_dos_dir(r"\..\..\FOO"), r"\FOO");
    }

    #[test]
    fn test_path_resolver_resolve_forward_slashes() {
        let mut resolver = PathResolver::new();
        resolver.set_current_dir(r"C:\GAMES");

        let resolved = resolver.resolve("TOOLS/SETUP.EXE");
        assert_eq!(resolved.path, r"\GAMES\TOOLS\SETUP.EXE");
    }

    #[test]
    fn test_file_table_duplicate_missing_sft_returns_none() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\TEMP.DAT"), 0x2000)
            .unwrap();
        let sft_index = table.handles[handle as usize].as_ref().unwrap().sft_index;
        table.sft[sft_index] = None;

        assert!(table.duplicate(handle, 0x2000).is_none());
    }

    #[test]
    fn test_file_table_insert_at_missing_sft_entry_replaces_handle() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\OLD.DAT"), 0x2000)
            .unwrap();
        let sft_index = table.handles[handle as usize].as_ref().unwrap().sft_index;
        table.sft[sft_index] = None;

        assert!(table.insert_at(handle, make_file(), String::from(r"C:\NEW.DAT"), 0x2000));
        assert!(table.handles[handle as usize].is_some());
    }

    // ========== Sharing Mode Tests ==========

    #[test]
    fn test_sharing_deny_all_blocks_second_open() {
        let mut table = FileTable::new();

        // Open file with DENY_ALL
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_ALL,
                ACCESS_READ,
            )
            .unwrap();

        // Second open should be blocked
        assert!(table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_NONE,
            ACCESS_READ,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_deny_write_allows_read() {
        let mut table = FileTable::new();

        // Open file with DENY_WRITE
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_WRITE,
                ACCESS_READ,
            )
            .unwrap();

        // Read-only open should succeed
        assert!(!table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_NONE,
            ACCESS_READ,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_deny_write_blocks_write() {
        let mut table = FileTable::new();

        // Open file with DENY_WRITE
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_WRITE,
                ACCESS_READ,
            )
            .unwrap();

        // Write access should be blocked
        assert!(table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_NONE,
            ACCESS_WRITE,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_deny_read_blocks_read() {
        let mut table = FileTable::new();

        // Open file with DENY_READ
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_READ,
                ACCESS_WRITE,
            )
            .unwrap();

        // Read access should be blocked
        assert!(table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_NONE,
            ACCESS_READ,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_deny_read_allows_write() {
        let mut table = FileTable::new();

        // Open file with DENY_READ
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_READ,
                ACCESS_WRITE,
            )
            .unwrap();

        // Write access should succeed
        assert!(!table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_NONE,
            ACCESS_WRITE,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_deny_none_allows_all() {
        let mut table = FileTable::new();

        // Open file with DENY_NONE
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_READWRITE,
            )
            .unwrap();

        // Any access should succeed
        assert!(!table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_NONE,
            ACCESS_READ,
            0x2000
        ));
        assert!(!table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_NONE,
            ACCESS_WRITE,
            0x2000
        ));
        assert!(!table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_NONE,
            ACCESS_READWRITE,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_compat_same_psp_allowed() {
        let mut table = FileTable::new();

        // Open file with COMPAT mode from PSP 0x1000
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_COMPAT,
                ACCESS_READ,
            )
            .unwrap();

        // Same PSP should succeed
        assert!(!table.check_sharing_violation(r"C:\FILE.TXT", SHARE_COMPAT, ACCESS_READ, 0x1000));
    }

    #[test]
    fn test_sharing_compat_different_psp_blocked() {
        let mut table = FileTable::new();

        // Open file with COMPAT mode from PSP 0x1000
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_COMPAT,
                ACCESS_READ,
            )
            .unwrap();

        // Different PSP should be blocked
        assert!(table.check_sharing_violation(r"C:\FILE.TXT", SHARE_COMPAT, ACCESS_READ, 0x2000));
    }

    #[test]
    fn test_sharing_case_insensitive_path() {
        let mut table = FileTable::new();

        // Open with uppercase
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_ALL,
                ACCESS_READ,
            )
            .unwrap();

        // Check with lowercase - should match
        assert!(table.check_sharing_violation(
            r"c:\file.txt",
            SHARE_DENY_NONE,
            ACCESS_READ,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_different_file_not_blocked() {
        let mut table = FileTable::new();

        // Open one file with DENY_ALL
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE1.TXT"),
                0x1000,
                SHARE_DENY_ALL,
                ACCESS_READ,
            )
            .unwrap();

        // Different file should not be blocked
        assert!(!table.check_sharing_violation(
            r"C:\FILE2.TXT",
            SHARE_DENY_NONE,
            ACCESS_READ,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_new_deny_all_blocks_existing() {
        let mut table = FileTable::new();

        // Open file with DENY_NONE
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_READ,
            )
            .unwrap();

        // New open with DENY_ALL should be blocked (existing file is open)
        assert!(table.check_sharing_violation(r"C:\FILE.TXT", SHARE_DENY_ALL, ACCESS_READ, 0x2000));
    }

    #[test]
    fn test_sharing_new_deny_write_blocks_existing_writer() {
        let mut table = FileTable::new();

        // Open file for writing
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_WRITE,
            )
            .unwrap();

        // New open with DENY_WRITE should be blocked (existing file has write access)
        assert!(table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_WRITE,
            ACCESS_READ,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_new_compat_allows_same_psp_with_non_compat_existing() {
        let mut table = FileTable::new();

        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_READ,
            )
            .unwrap();

        assert!(!table.check_sharing_violation(r"C:\FILE.TXT", SHARE_COMPAT, ACCESS_READ, 0x1000));
    }

    #[test]
    fn test_sharing_new_deny_read_blocks_existing_reader() {
        let mut table = FileTable::new();

        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_READ,
            )
            .unwrap();

        assert!(table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_READ,
            ACCESS_READ,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_new_deny_read_allows_existing_writer() {
        let mut table = FileTable::new();

        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_WRITE,
            )
            .unwrap();

        assert!(!table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_READ,
            ACCESS_READ,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_new_deny_write_allows_read_only_existing() {
        let mut table = FileTable::new();

        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_READ,
            )
            .unwrap();

        assert!(!table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_WRITE,
            ACCESS_READ,
            0x2000
        ));
    }

    // ========== get_sft_index tests ==========

    #[test]
    fn test_get_sft_index_valid_handle() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\IDX.DAT"), 0x1000)
            .unwrap();
        let sft_index = table.get_sft_index(handle);
        assert!(sft_index.is_some());
        let expected = table.handles[handle as usize].as_ref().unwrap().sft_index;
        assert_eq!(sft_index.unwrap(), expected);
    }

    #[test]
    fn test_get_sft_index_invalid_handle() {
        let table = FileTable::new();
        assert!(table.get_sft_index(5).is_none());
        assert!(table.get_sft_index(999).is_none());
    }

    #[test]
    fn test_get_sft_index_freed_handle() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\FREE.DAT"), 0x1000)
            .unwrap();
        table.free(handle);
        assert!(table.get_sft_index(handle).is_none());
    }

    // ========== check_sharing_violation READWRITE branches ==========

    #[test]
    fn test_sharing_deny_write_blocks_readwrite() {
        let mut table = FileTable::new();
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_WRITE,
                ACCESS_READ,
            )
            .unwrap();
        assert!(table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_NONE,
            ACCESS_READWRITE,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_deny_read_blocks_readwrite() {
        let mut table = FileTable::new();
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_READ,
                ACCESS_WRITE,
            )
            .unwrap();
        assert!(table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_NONE,
            ACCESS_READWRITE,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_new_deny_read_blocks_existing_readwrite() {
        let mut table = FileTable::new();
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_READWRITE,
            )
            .unwrap();
        assert!(table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_READ,
            ACCESS_WRITE,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_new_deny_write_blocks_existing_readwrite() {
        let mut table = FileTable::new();
        let _h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\FILE.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_READWRITE,
            )
            .unwrap();
        assert!(table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_WRITE,
            ACCESS_READ,
            0x2000
        ));
    }

    #[test]
    fn test_sharing_no_files_open_no_violation() {
        let table = FileTable::new();
        assert!(!table.check_sharing_violation(
            r"C:\FILE.TXT",
            SHARE_DENY_ALL,
            ACCESS_READ,
            0x1000
        ));
    }

    // ========== LFN path resolution ==========

    #[test]
    fn test_path_resolver_resolve_lfn_preserves_case() {
        let mut resolver = PathResolver::new();
        resolver.set_current_dir(r"C:\GAMES");
        let resolved = resolver.resolve_lfn("MyGame.exe");
        assert_eq!(resolved.drive, 2);
        assert_eq!(resolved.path, r"\GAMES\MyGame.exe");
    }

    #[test]
    fn test_normalize_dos_dir_preserve_case() {
        assert_eq!(
            PathResolver::normalize_dos_dir_preserve_case(r"Foo\Bar\"),
            r"\Foo\Bar"
        );
    }

    // ========== FileTable default and ResolvedPath ==========

    #[test]
    fn test_file_table_default() {
        let table = FileTable::default();
        assert!(!table.is_valid(5));
    }

    #[test]
    fn test_path_resolver_default() {
        let resolver = PathResolver::default();
        assert_eq!(resolver.get_current_drive(), 2);
    }

    #[test]
    fn test_resolved_path_clone_and_eq() {
        let rp = ResolvedPath {
            drive: 2,
            path: String::from(r"\TEST"),
        };
        let rp2 = rp.clone();
        assert_eq!(rp, rp2);
    }

    // ========== unlock_region invalid handle ==========

    #[test]
    fn test_unlock_region_invalid_handle() {
        let mut table = FileTable::new();
        assert_eq!(table.unlock_region(6, 0, 1), Err(6));
    }

    // ========== File lock edge cases ==========

    #[test]
    fn test_file_lock_overlap_saturating_add() {
        let lock = FileLock {
            offset: u32::MAX - 5,
            length: 100,
        };
        assert!(lock.overlaps(u32::MAX - 3, 10));
    }

    #[test]
    fn test_file_lock_adjacent_no_overlap() {
        let lock = FileLock {
            offset: 10,
            length: 5,
        };
        assert!(!lock.overlaps(15, 5));
    }

    // ========== split_drive_and_path edge cases ==========

    #[test]
    fn test_split_drive_and_path_single_char() {
        let (drive, path) = PathResolver::split_drive_and_path("X");
        assert_eq!(drive, None);
        assert_eq!(path, "X");
    }

    #[test]
    fn test_split_drive_and_path_empty() {
        let (drive, path) = PathResolver::split_drive_and_path("");
        assert_eq!(drive, None);
        assert_eq!(path, "");
    }

    #[test]
    fn test_split_drive_and_path_lowercase_drive() {
        let (drive, path) = PathResolver::split_drive_and_path("d:\\test");
        assert_eq!(drive, Some(3));
        assert_eq!(path, "\\test");
    }

    // ========== SFT reference count increment ==========

    #[test]
    fn test_increment_sft_ref_count_basic() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\REF.DAT"), 0x1000)
            .unwrap();
        let sft_index = table.get_sft_index(handle).unwrap();

        assert_eq!(table.sft[sft_index].as_ref().unwrap().ref_count, 1);
        table.increment_sft_ref_count(sft_index);
        assert_eq!(table.sft[sft_index].as_ref().unwrap().ref_count, 2);
    }

    #[test]
    fn test_increment_sft_ref_count_out_of_range_is_noop() {
        let mut table = FileTable::new();
        // Should not panic
        table.increment_sft_ref_count(999);
    }

    #[test]
    fn test_increment_sft_ref_count_empty_slot_is_noop() {
        let mut table = FileTable::new();
        // SFT slot 0 is None by default
        table.increment_sft_ref_count(0);
        assert!(table.sft[0].is_none());
    }

    // ── Coverage: SFT exhaustion (allocate_sft_entry returns None) ──

    #[test]
    fn test_allocate_sft_entry_returns_none_when_full() {
        let mut table = FileTable::new();
        // Fill all 256 SFT entries
        for _ in 0..256 {
            let result = table.allocate_sft_entry(
                make_file(),
                "test".to_string(),
                FileIdentity::from_dos_path("test"),
                SHARE_COMPAT,
                ACCESS_READ,
                0x100,
            );
            assert!(result.is_some());
        }
        // 257th allocation should fail
        let result = table.allocate_sft_entry(
            make_file(),
            "test".to_string(),
            FileIdentity::from_dos_path("test"),
            SHARE_COMPAT,
            ACCESS_READ,
            0x100,
        );
        assert!(result.is_none());
    }

    // ── Coverage: allocate_at_handle ──

    #[test]
    fn test_allocate_at_handle_success() {
        let mut table = FileTable::new();
        // Handle 10 should be free
        assert!(table.get_sft(10).is_none());
        assert!(table.is_handle_free(10));
        assert!(table.allocate_at_handle(10, make_file(), "bridge.txt".to_string(), 0x200));
        assert!(table.get_sft(10).is_some());
        assert!(!table.is_handle_free(10));
    }

    #[test]
    fn handle_bridge_preserves_resolved_identity_and_share_mode() {
        let mut table = FileTable::new();
        let resolved = Path::new("resolved/bridge.txt");
        assert!(table.allocate_at_handle_with_sharing(
            10,
            make_file(),
            r"C:\BRIDGE.TXT".to_string(),
            Some(resolved),
            0x200,
            SHARE_DENY_READ,
            ACCESS_READ,
        ));
        let identity = FileIdentity::for_resolved_path(2, r"\BRIDGE.TXT", Some(resolved));
        assert!(table.check_sharing_violation_for_identity(
            &identity,
            SHARE_DENY_NONE,
            ACCESS_READ,
            0x300,
        ));
        assert!(!table.check_sharing_violation_for_identity(
            &identity,
            SHARE_DENY_NONE,
            ACCESS_WRITE,
            0x300,
        ));
    }

    #[test]
    fn test_allocate_at_handle_occupied_is_noop() {
        let mut table = FileTable::new();
        // First allocate at handle 10
        assert!(table.allocate_at_handle(10, make_file(), "first.txt".to_string(), 0x200));
        assert!(table.get_sft(10).is_some());
        let first_path = table.get_sft(10).unwrap().path.clone();
        // Second allocation at same handle must report failure and leave the
        // existing entry untouched (relocate-on-collision depends on this).
        assert!(!table.allocate_at_handle(10, make_file(), "second.txt".to_string(), 0x200));
        // Still accessible and still the FIRST file (no eviction).
        assert!(table.get_sft(10).is_some());
        assert_eq!(table.get_sft(10).unwrap().path, first_path);
    }

    #[test]
    fn test_allocate_at_handle_out_of_range() {
        let mut table = FileTable::new();
        // Handle beyond 255 should be a no-op (no panic) and report failure.
        assert!(!table.allocate_at_handle(300, make_file(), "oob.txt".to_string(), 0x200));
        // Out-of-range handles are never reported free.
        assert!(!table.is_handle_free(300));
    }

    // ── Coverage: insert_at when SFT is exhausted ──

    #[test]
    fn test_insert_at_sft_full() {
        let mut table = FileTable::new();
        // Fill all SFT entries
        for _ in 0..256 {
            table.allocate_sft_entry(
                make_file(),
                "fill".to_string(),
                FileIdentity::from_dos_path("fill"),
                SHARE_COMPAT,
                ACCESS_READ,
                0x100,
            );
        }
        // insert_at should fail because SFT is full
        let result = table.insert_at(5, make_file(), "new.txt".to_string(), 0x200);
        assert!(!result);
    }

    // ========== Coverage: parse_drive_and_path ==========

    #[test]
    fn test_parse_drive_and_path_valid() {
        let (drive, path) = parse_drive_and_path(r"C:\GAME\DATA.DAT").unwrap();
        assert_eq!(drive, 2);
        assert_eq!(path, r"\GAME\DATA.DAT");
    }

    #[test]
    fn test_parse_drive_and_path_lowercase() {
        let (drive, path) = parse_drive_and_path(r"d:\tools\edit.exe").unwrap();
        assert_eq!(drive, 3);
        assert_eq!(path, r"\tools\edit.exe");
    }

    #[test]
    fn test_parse_drive_and_path_no_drive() {
        assert!(parse_drive_and_path(r"\FILE.TXT").is_none());
        assert!(parse_drive_and_path("FILE.TXT").is_none());
    }

    #[test]
    fn test_parse_drive_and_path_empty() {
        assert!(parse_drive_and_path("").is_none());
    }

    #[test]
    fn test_parse_drive_and_path_single_char() {
        assert!(parse_drive_and_path("C").is_none());
    }

    #[test]
    fn test_parse_drive_and_path_digit_before_colon() {
        // "1:" is not a valid drive letter
        assert!(parse_drive_and_path("1:\\FOO").is_none());
    }

    #[test]
    fn test_parse_drive_and_path_drive_only() {
        let (drive, path) = parse_drive_and_path("A:").unwrap();
        assert_eq!(drive, 0);
        assert_eq!(path, "");
    }

    // ========== Coverage: get_sft_ref_count ==========

    #[test]
    fn test_get_sft_ref_count_valid() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\REF.DAT"), 0x1000)
            .unwrap();
        let sft_index = table.get_sft_index(handle).unwrap();
        assert_eq!(table.get_sft_ref_count(sft_index), Some(1));
    }

    #[test]
    fn test_get_sft_ref_count_after_dup() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\REF.DAT"), 0x1000)
            .unwrap();
        let _dup = table.duplicate(handle, 0x1000).unwrap();
        let sft_index = table.get_sft_index(handle).unwrap();
        assert_eq!(table.get_sft_ref_count(sft_index), Some(2));
    }

    #[test]
    fn test_get_sft_ref_count_out_of_range() {
        let table = FileTable::new();
        assert!(table.get_sft_ref_count(999).is_none());
    }

    #[test]
    fn test_get_sft_ref_count_empty_slot() {
        let table = FileTable::new();
        assert!(table.get_sft_ref_count(0).is_none());
    }

    // ========== Coverage: lock overlap edge cases ==========

    #[test]
    fn test_file_lock_overlap_at_exact_boundary() {
        let lock = FileLock {
            offset: 10,
            length: 5,
        };
        // [10..15) vs [15..20) -> no overlap
        assert!(!lock.overlaps(15, 5));
        // [10..15) vs [14..19) -> overlap at byte 14
        assert!(lock.overlaps(14, 5));
    }

    #[test]
    fn test_file_lock_overlap_contained() {
        let lock = FileLock {
            offset: 0,
            length: 100,
        };
        // Inner region fully contained
        assert!(lock.overlaps(10, 5));
    }

    #[test]
    fn test_file_lock_overlap_zero_offset() {
        let lock = FileLock {
            offset: 0,
            length: 10,
        };
        assert!(lock.overlaps(0, 1));
        assert!(lock.overlaps(9, 1));
        assert!(!lock.overlaps(10, 1));
    }

    // ========== Coverage: multiple locks on same file ==========

    #[test]
    fn test_multiple_non_overlapping_locks() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\MULTI.DAT"), 0x2000)
            .unwrap();

        assert_eq!(table.lock_region(handle, 0, 10), Ok(()));
        assert_eq!(table.lock_region(handle, 10, 10), Ok(()));
        assert_eq!(table.lock_region(handle, 20, 10), Ok(()));

        // Unlock middle region
        assert_eq!(table.unlock_region(handle, 10, 10), Ok(()));

        // Lock the gap again
        assert_eq!(table.lock_region(handle, 10, 10), Ok(()));

        // Try overlapping all three
        assert_eq!(table.lock_region(handle, 5, 20), Err(33));
    }

    #[test]
    fn test_file_lock_capacity_rejects_without_growth() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\LOCK-CAP.DAT"), 0x2000)
            .unwrap();

        for index in 0..MAX_FILE_LOCKS_PER_SFT {
            assert_eq!(table.lock_region(handle, (index * 2) as u32, 1), Ok(()));
        }
        assert_eq!(
            table.get_sft(handle).unwrap().locks.len(),
            MAX_FILE_LOCKS_PER_SFT
        );

        assert_eq!(
            table.lock_region(handle, (MAX_FILE_LOCKS_PER_SFT * 2) as u32, 1),
            Err(33),
            "a full DOS lock registry must report a guest-visible lock violation"
        );
        assert_eq!(
            table.get_sft(handle).unwrap().locks.len(),
            MAX_FILE_LOCKS_PER_SFT,
            "a rejected lock must not grow the registry"
        );
    }

    #[test]
    fn test_file_lock_capacity_unlock_releases_one_slot() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\LOCK-REUSE.DAT"), 0x2000)
            .unwrap();

        for index in 0..MAX_FILE_LOCKS_PER_SFT {
            assert_eq!(table.lock_region(handle, (index * 2) as u32, 1), Ok(()));
        }
        assert_eq!(table.unlock_region(handle, 20, 1), Ok(()));
        assert_eq!(
            table.get_sft(handle).unwrap().locks.len(),
            MAX_FILE_LOCKS_PER_SFT - 1
        );
        assert_eq!(
            table.lock_region(handle, (MAX_FILE_LOCKS_PER_SFT * 2) as u32, 1),
            Ok(())
        );
        assert_eq!(
            table.get_sft(handle).unwrap().locks.len(),
            MAX_FILE_LOCKS_PER_SFT
        );
    }

    #[test]
    fn test_duplicated_handles_share_file_lock_capacity() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\LOCK-DUP.DAT"), 0x2000)
            .unwrap();
        let duplicate = table.duplicate(handle, 0x3000).unwrap();

        for index in 0..MAX_FILE_LOCKS_PER_SFT {
            assert_eq!(table.lock_region(handle, (index * 2) as u32, 1), Ok(()));
        }
        assert_eq!(
            table.lock_region(duplicate, (MAX_FILE_LOCKS_PER_SFT * 2) as u32, 1),
            Err(33),
            "duplicated handles must consume the same SFT lock capacity"
        );
        assert_eq!(table.unlock_region(duplicate, 20, 1), Ok(()));
        assert_eq!(
            table.lock_region(duplicate, (MAX_FILE_LOCKS_PER_SFT * 2) as u32, 1),
            Ok(())
        );
    }

    // ========== Coverage: allocate_with_sharing edge cases ==========

    #[test]
    fn test_allocate_with_sharing_returns_sequential_handles() {
        let mut table = FileTable::new();
        let h1 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\A.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_READ,
            )
            .unwrap();
        let h2 = table
            .allocate_with_sharing(
                make_file(),
                String::from(r"C:\B.TXT"),
                0x1000,
                SHARE_DENY_NONE,
                ACCESS_WRITE,
            )
            .unwrap();
        assert_eq!(h1, 5);
        assert_eq!(h2, 6);
    }

    // ========== Coverage: close_process_files with no matching PSP ==========

    #[test]
    fn test_close_process_files_no_matches() {
        let mut table = FileTable::new();
        let handle = table
            .allocate(make_file(), String::from(r"C:\KEEP.DAT"), 0x1111)
            .unwrap();

        // Close files for a different PSP
        table.close_process_files(0x9999);

        // Original file should still be valid
        assert!(table.is_valid(handle));
    }

    // ========== Coverage: force_duplicate to handle < 5 (std handle) ==========

    #[test]
    fn test_force_duplicate_to_handle_1_stdout() {
        let mut table = FileTable::new();
        let src = table
            .allocate(make_file(), String::from(r"C:\OUT.LOG"), 0x1000)
            .unwrap();

        // Redirect stdout (handle 1) to our file
        assert!(table.force_duplicate(src, 1, 0x1000));

        let src_sft = table.handles[src as usize].as_ref().unwrap().sft_index;
        let dst_sft = table.handles[1].as_ref().unwrap().sft_index;
        assert_eq!(src_sft, dst_sft);
        // handle < 5: free() should NOT have been called
        assert_eq!(table.sft[src_sft].as_ref().unwrap().ref_count, 2);
    }

    // ========== Coverage: PathResolver normalize with mixed separators ==========

    #[test]
    fn test_path_resolver_normalize_forward_slash_and_backslash() {
        assert_eq!(
            PathResolver::normalize_dos_dir("foo/bar\\baz/"),
            r"\FOO\BAR\BAZ"
        );
    }

    #[test]
    fn test_path_resolver_normalize_consecutive_separators() {
        assert_eq!(
            PathResolver::normalize_dos_dir(r"\\foo\\bar\\"),
            r"\FOO\BAR"
        );
    }

    #[test]
    fn test_path_resolver_normalize_only_dots() {
        assert_eq!(PathResolver::normalize_dos_dir("."), r"\");
        assert_eq!(PathResolver::normalize_dos_dir(".."), r"\");
        assert_eq!(PathResolver::normalize_dos_dir(r".\.\"), r"\");
    }

    #[test]
    fn test_path_resolver_resolve_lfn_absolute_preserves_case() {
        let mut resolver = PathResolver::new();
        let resolved = resolver.resolve_lfn(r"C:\MyApp\Config.ini");
        assert_eq!(resolved.drive, 2);
        assert_eq!(resolved.path, r"\MyApp\Config.ini");
    }

    #[test]
    fn test_path_resolver_resolve_lfn_relative_preserves_case() {
        let mut resolver = PathResolver::new();
        resolver.set_current_dir(r"C:\Base");
        let resolved = resolver.resolve_lfn("SubDir/File.Txt");
        assert_eq!(resolved.drive, 2);
        assert_eq!(resolved.path, r"\BASE\SubDir\File.Txt");
    }

    // ------------------------------------------------------------------
    // MDD-BUG-CRUCIBLE-00647 — a snapshot restored against a SHRUNKEN
    // backing file must not leave the SFT positioned past EOF.
    // ------------------------------------------------------------------

    /// Capture a handle at offset `saved_pos`, shrink the backing file to
    /// `new_len`, restore, and return `(sft_position, stream_position)`.
    fn reopen_after_shrink(original: &[u8], saved_pos: u64, new_len: u64) -> (u64, u64) {
        let dir = tempfile::TempDir::new().unwrap();
        let host_file = dir.path().join("DATA.BIN");
        std::fs::write(&host_file, original).unwrap();

        let mut drives: BTreeMap<u8, Box<dyn crate::filesystem::VirtualDrive>> = BTreeMap::new();
        drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(dir.path().to_path_buf())),
        );

        let mut table = FileTable::new();
        let file = drives
            .get_mut(&2)
            .unwrap()
            .open_file(r"\DATA.BIN", false, false, false)
            .expect("open");
        let handle = table
            .allocate(file, r"C:\DATA.BIN".to_string(), 0x0100)
            .expect("allocate");
        table.get_sft_mut(handle).unwrap().position = saved_pos;

        let snap = table.snapshot();

        // The backing file shrinks between capture and restore.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&host_file)
            .unwrap()
            .set_len(new_len)
            .unwrap();

        let mut restored = FileTable::new();
        restored.restore(&snap);
        let empty_virtual_files = BTreeMap::new();
        let synthetic = SyntheticSources {
            glide_ovl: None,
            virtual_files: &empty_virtual_files,
        };
        restored.reopen_files_from_snapshot(&snap, &mut drives, &synthetic);

        let entry = restored
            .get_sft_mut(handle)
            .expect("the handle must survive the restore");
        let stream_pos = entry.file.stream_position().expect("stream position");
        (entry.position, stream_pos)
    }

    // ------------------------------------------------------------------
    // MDD-BUG-CRUCIBLE-00445 — a handle whose file could not be reopened
    // must not survive the restore as a ghost.
    // ------------------------------------------------------------------

    /// Capture one open handle, break the environment so reopening it fails,
    /// restore, and hand back the restored table and that handle.
    ///
    /// `spoil` runs between capture and restore with the media directory and
    /// the drive map, so each caller can pick a different failure route into
    /// the same `continue` in `reopen_files_from_snapshot`.
    fn reopen_after_failure(
        spoil: impl FnOnce(
            &std::path::Path,
            &mut BTreeMap<u8, Box<dyn crate::filesystem::VirtualDrive>>,
        ),
    ) -> (FileTable, u16) {
        let dir = tempfile::TempDir::new().unwrap();
        let host_file = dir.path().join("DATA.BIN");
        std::fs::write(&host_file, b"0123456789ABCDEF").unwrap();

        let mut drives: BTreeMap<u8, Box<dyn crate::filesystem::VirtualDrive>> = BTreeMap::new();
        drives.insert(
            2,
            Box::new(crate::filesystem::HostDrive::new(dir.path().to_path_buf())),
        );

        let mut table = FileTable::new();
        let file = drives
            .get_mut(&2)
            .unwrap()
            .open_file(r"\DATA.BIN", false, false, false)
            .expect("open");
        let handle = table
            .allocate(file, r"C:\DATA.BIN".to_string(), 0x0100)
            .expect("allocate");
        let snap = table.snapshot();

        spoil(dir.path(), &mut drives);

        let mut restored = FileTable::new();
        restored.restore(&snap);
        let empty_virtual_files = BTreeMap::new();
        let synthetic = SyntheticSources {
            glide_ovl: None,
            virtual_files: &empty_virtual_files,
        };
        restored.reopen_files_from_snapshot(&snap, &mut drives, &synthetic);
        (restored, handle)
    }

    /// The whole point: every consumer must agree the handle is gone.
    ///
    /// Before the fix `get_sft` said invalid while `is_valid` said valid, so
    /// close and IOCTL treated the handle as live and closing it reported
    /// success for a file that was never reopened.
    fn assert_handle_is_not_a_ghost(table: &mut FileTable, handle: u16) {
        assert!(
            table.get_sft(handle).is_none(),
            "a file that failed to reopen must have no SFT entry"
        );
        assert!(
            !table.is_valid(handle),
            "the handle must not report valid while its SFT entry is missing"
        );
        assert!(
            !table.free(handle),
            "closing a handle that was never reopened must not report success"
        );
        assert!(
            table.duplicate(handle, 0x0100).is_none(),
            "a handle with no SFT entry must not be duplicable"
        );
        assert!(
            table.is_handle_free(handle),
            "the slot must be available for allocation again"
        );
    }

    #[test]
    fn snapshot_reopen_drops_the_handle_when_the_file_is_gone() {
        let (mut table, handle) = reopen_after_failure(|dir, _drives| {
            std::fs::remove_file(dir.join("DATA.BIN")).unwrap();
        });
        assert_handle_is_not_a_ghost(&mut table, handle);
    }

    #[test]
    fn snapshot_reopen_drops_the_handle_when_the_drive_is_unmounted() {
        let (mut table, handle) = reopen_after_failure(|_dir, drives| {
            drives.remove(&2);
        });
        assert_handle_is_not_a_ghost(&mut table, handle);
    }

    #[test]
    fn snapshot_reopen_keeps_a_handle_whose_file_still_opens() {
        // The control: an untouched file must still restore normally, so the
        // sweep above cannot be passing by dropping everything.
        let (table, handle) = reopen_after_failure(|_dir, _drives| {});
        assert!(
            table.get_sft(handle).is_some(),
            "an intact file must still reopen"
        );
        assert!(table.is_valid(handle), "and its handle must stay valid");
    }

    #[test]
    fn snapshot_reopen_clamps_a_position_past_a_shrunken_files_eof() {
        // Captured at offset 16; the file is only 4 bytes by restore time.
        let (sft_pos, stream_pos) = reopen_after_shrink(b"0123456789ABCDEF", 16, 4);

        assert_eq!(
            sft_pos, 4,
            "the SFT position must be clamped to the new EOF, not left past it"
        );
        assert_eq!(
            stream_pos, 4,
            "the reopened stream must sit at the new EOF too"
        );
    }

    #[test]
    fn snapshot_reopen_preserves_a_position_that_still_fits() {
        // The ordinary case: the file did not shrink, so nothing moves.
        let (sft_pos, stream_pos) = reopen_after_shrink(b"0123456789ABCDEF", 8, 16);

        assert_eq!(sft_pos, 8, "an in-range position must be preserved exactly");
        assert_eq!(stream_pos, 8);
    }
}
