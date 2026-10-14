use anyhow::{anyhow, Result};
use std::fs::OpenOptions;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Attributes for a file or directory
#[derive(Debug, Clone, Copy)]
pub struct FileAttributes {
    pub read_only: bool,
    pub hidden: bool,
    pub system: bool,
    pub volume_label: bool,
    pub directory: bool,
    pub archive: bool,
    pub size: u64,
    pub modified: SystemTime,
}

impl Default for FileAttributes {
    fn default() -> Self {
        Self {
            read_only: false,
            hidden: false,
            system: false,
            volume_label: false,
            directory: false,
            archive: false,
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
        }
    }
}

/// File metadata exposed by the DOS 7 long-filename APIs.
///
/// `FileAttributes` deliberately remains the compact directory-listing shape
/// used throughout the emulator.  The LFN calls need the three independent
/// Windows timestamp fields as well, so they use this richer view instead of
/// making every filesystem backend and every existing attribute literal carry
/// those fields.
#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub attributes: FileAttributes,
    pub creation_time: SystemTime,
    pub last_access_time: SystemTime,
    pub last_write_time: SystemTime,
}

/// Abstract interface for an open file
pub trait VirtualFile: Read + Write + Seek + Send {
    fn set_len(&mut self, size: u64) -> Result<()>;
    fn sync_all(&mut self) -> Result<()>;

    /// Identity of the opened host file, independent of path and drive aliases.
    /// Virtual backends without host handles keep their existing path semantics.
    fn host_identity(&self) -> Result<Option<HostFileIdentity>> {
        Ok(None)
    }

    /// Give the handle up WITHOUT a guest close: anything it still buffers
    /// belongs to a timeline being thrown away and must not reach the media
    /// when it drops. Snapshot restore calls this on every live handle before
    /// replacing the file table (MDD-BUG-KIL-00781). Handles that buffer
    /// nothing -- host files write through -- need not override it.
    fn abandon(&mut self) {}
}

/// Stable host identity while the file handle remains open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostFileIdentity {
    device: u64,
    file: u64,
}

/// DOS allocation units returned by INT 21h AH=36h.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskGeometry {
    pub sectors_per_cluster: u16,
    pub bytes_per_sector: u16,
    pub total_clusters: u16,
    pub free_clusters: u16,
}

/// Abstract interface for a storage drive (C:, D:, etc.)
pub trait VirtualDrive: Send {
    /// Open a file
    fn open_file(
        &mut self,
        path: &str,
        write: bool,
        create: bool,
        truncate: bool,
    ) -> Result<Box<dyn VirtualFile>>;

    /// Atomically create a writable file, failing with `AlreadyExists` on collision.
    /// Backends must implement exclusive creation rather than checking then opening.
    fn create_new_file(&mut self, _path: &str) -> Result<Box<dyn VirtualFile>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "exclusive file creation is not supported by this drive",
        )
        .into())
    }

    /// Create a directory
    fn create_dir(&mut self, path: &str) -> Result<()>;

    /// Remove a file
    fn remove_file(&mut self, path: &str) -> Result<()>;

    /// Remove a directory
    fn remove_dir(&mut self, path: &str) -> Result<()>;

    /// Rename a file or directory
    fn rename(&mut self, from: &str, to: &str) -> Result<()>;

    /// Get file/directory attributes
    fn get_attributes(&mut self, path: &str) -> Result<FileAttributes>;

    /// Set file/directory attributes
    fn set_attributes(&mut self, path: &str, attrs: FileAttributes) -> Result<()>;

    /// Set file modification time
    fn set_file_time(&mut self, path: &str, modified: SystemTime) -> Result<()>;

    /// Get the complete metadata needed by DOS 7 LFN calls.
    ///
    /// Backends which only expose DOS directory attributes still provide a
    /// useful implementation: DOS treats the available modification time as
    /// the other timestamps when the filesystem has no separate fields.
    fn get_file_metadata(&mut self, path: &str) -> Result<FileMetadata> {
        let attributes = self.get_attributes(path)?;
        let modified = attributes.modified;
        Ok(FileMetadata {
            attributes,
            creation_time: modified,
            last_access_time: modified,
            last_write_time: modified,
        })
    }

    /// Set the last-access timestamp.  The fallback preserves useful DOS
    /// behaviour on simple backends by using their ordinary file-time hook.
    fn set_file_access_time(&mut self, path: &str, accessed: SystemTime) -> Result<()> {
        self.set_file_time(path, accessed)
    }

    /// Set the creation timestamp.  Filesystems without a birth-time API use
    /// their modification-time hook as the closest representable operation.
    fn set_file_creation_time(&mut self, path: &str, created: SystemTime) -> Result<()> {
        self.set_file_time(path, created)
    }

    /// Flush filesystem-side buffers for a drive reset (AX=710Dh).
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    /// Read directory contents
    /// Returns list of (filename, attributes)
    fn read_dir(&mut self, path: &str) -> Result<Vec<(String, FileAttributes)>>;

    /// Get total and free disk space in bytes
    fn get_disk_space(&self) -> Result<(u64, u64)>;

    /// Allocation geometry, when a drive has a particular DOS layout.
    /// The default preserves the synthetic 32 KiB clusters used by HLE drives.
    fn get_disk_geometry(&self) -> Result<DiskGeometry> {
        let (total, free) = self.get_disk_space()?;
        Ok(DiskGeometry {
            sectors_per_cluster: 64,
            bytes_per_sector: 512,
            total_clusters: (total / 32768).min(0xFFFF) as u16,
            free_clusters: (free / 32768).min(0xFFFF) as u16,
        })
    }

    /// Resolve a DOS path to a host path if supported by the drive.
    fn resolve_host_path(&self, _path: &str) -> Option<PathBuf> {
        None
    }
}

/// Implementation for Host OS filesystem
pub struct HostDrive {
    root_path: PathBuf,
    /// Writable overlay directory. When the root filesystem is read-only or
    /// doesn't support file creation (e.g. disk image mounts via WinFSP/mddskimg),
    /// files that need to be created or modified are redirected here. The overlay
    write_overlay: Option<PathBuf>,
    /// Reverse mapping from 8.3 short names to long names, per directory.
    /// Only used on non-Windows platforms (Windows NTFS handles 8.3 natively).
    #[cfg(not(windows))]
    sfn_map: std::collections::BTreeMap<PathBuf, std::collections::BTreeMap<String, String>>,
}

#[cfg(windows)]
fn path_starts_with_case_insensitive(path: &std::path::Path, root: &std::path::Path) -> bool {
    let path_lower = path.to_string_lossy().to_lowercase();
    let root_lower = root.to_string_lossy().to_lowercase();

    if !path_lower.starts_with(&root_lower) {
        return false;
    }
    if path_lower.len() == root_lower.len() {
        return true;
    }

    let root_ends_with_sep = matches!(root_lower.as_bytes().last(), Some(b'\\') | Some(b'/'));
    if root_ends_with_sep {
        return true;
    }

    matches!(
        path_lower.as_bytes().get(root_lower.len()),
        Some(b'\\') | Some(b'/')
    )
}

impl HostDrive {
    pub fn new(root_path: PathBuf) -> Self {
        Self {
            root_path,
            write_overlay: None,
            #[cfg(not(windows))]
            sfn_map: std::collections::BTreeMap::new(),
        }
    }

    /// Truncate a filename to DOS 8.3 format.
    /// "Brazilian.PES" → "BRAZILIA.PES", "LONGFILENAME.TEXT" → "LONGFILE.TEX"
    fn truncate_filename_8_3(name: &str) -> String {
        if let Some(dot_pos) = name.rfind('.') {
            let base = &name[..dot_pos];
            let ext = &name[dot_pos + 1..];
            let trunc_base: String = base.chars().take(8).collect();
            let trunc_ext: String = ext.chars().take(3).collect();
            if trunc_ext.is_empty() {
                trunc_base
            } else {
                format!("{}.{}", trunc_base, trunc_ext)
            }
        } else {
            name.chars().take(8).collect()
        }
    }

    /// Set a writable overlay directory. When file creation fails on the
    /// root filesystem (e.g. read-only disk image mounts), the overlay
    /// directory is used instead.
    pub fn set_write_overlay(&mut self, overlay_path: PathBuf) {
        self.write_overlay = Some(overlay_path);
    }

    /// A name that separates users sharing one temp directory.
    ///
    /// Convenience only: it keeps two accounts on a shared `/tmp` out of each
    /// other's way. It is NOT the security boundary - [`adopt_private_dir`]
    /// is, and it holds whatever this returns.
    fn overlay_user_tag() -> String {
        let raw = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let tag: String = raw
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(32)
            .collect();
        if tag.is_empty() {
            "default".to_string()
        } else {
            tag
        }
    }

    /// Create `path` as a directory we own, or adopt an existing one only if
    /// it is genuinely ours.
    ///
    /// `create_dir`, never `create_dir_all`: the latter succeeds THROUGH an
    /// existing symlink-to-directory, which is the whole defect. When the
    /// name is already taken we inspect it with `symlink_metadata` - the
    /// plain `metadata` follows the link and would report the attacker's
    /// target as a perfectly good directory.
    ///
    /// On Unix we also require that it is ours and not group- or
    /// world-writable. A 0700 directory belonging to another account is
    /// refused rather than adopted: we could not write into it anyway, and
    /// failing here says so instead of leaving every guest save to fail with
    /// "path escapes write overlay" (MDD-BUG-CRU-01410).
    fn adopt_private_dir(path: &Path) -> Option<()> {
        match std::fs::create_dir(path) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
                }
                Some(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(path).ok()?;
                if !metadata.is_dir() {
                    tracing::warn!(
                        target: "gp64::dos::files",
                        path = %path.display(),
                        "refusing a write-overlay component that is not a directory we created"
                    );
                    return None;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    // SAFETY: `geteuid` reads this process's own effective
                    // uid and cannot fail.
                    let us = unsafe { libc::geteuid() };
                    if metadata.uid() != us || metadata.mode() & 0o077 != 0 {
                        tracing::warn!(
                            target: "gp64::dos::files",
                            path = %path.display(),
                            owner = metadata.uid(),
                            mode = format_args!("{:o}", metadata.mode() & 0o7777),
                            "refusing a write-overlay component owned by another user or open to one"
                        );
                        return None;
                    }
                }
                Some(())
            }
            Err(_) => None,
        }
    }

    /// Lazily initialize the write overlay in the system temp directory.
    /// Called automatically when file creation fails on the root filesystem.
    ///
    /// `None` when no overlay can be created SAFELY. The caller then refuses
    /// the write, which is the right answer: the overlay exists to hold guest
    /// bytes the mount root would not take, and writing them somewhere a
    /// local attacker chose is worse than not writing them at all.
    fn ensure_write_overlay(&mut self) -> Option<&PathBuf> {
        if self.write_overlay.is_none() {
            // Canonicalize the OS-supplied temp directory ONCE, here, where
            // it is trustworthy - this is what resolves macOS's
            // `/var` -> `/private/var`. Every component below it is one we
            // create ourselves, so the result is already canonical and the
            // containment reference is the path we INTENDED rather than
            // whatever a pre-planted link happened to resolve to.
            let base = std::env::temp_dir().canonicalize().ok()?;
            let mut overlay = base.join(format!("gp64_overlay-{}", Self::overlay_user_tag()));
            Self::adopt_private_dir(&overlay)?;
            // Use a hash of the root path to avoid collisions between sessions
            let hash = {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                self.root_path.hash(&mut hasher);
                hasher.finish()
            };
            overlay.push(format!("{:016x}", hash));
            Self::adopt_private_dir(&overlay)?;
            tracing::info!(
                target: "gp64::dos::files",
                root = %self.root_path.display(),
                overlay = %overlay.display(),
                "Write overlay initialized for read-only filesystem"
            );
            self.write_overlay = Some(overlay);
        }
        self.write_overlay.as_ref()
    }

    /// True when any component carries a `:` — the NTFS stream separator.
    ///
    /// The drive designator is stripped long before a path reaches a backend,
    /// so a surviving colon can only be a stream reference (or, on a
    /// non-Windows host, a name DOS could never have produced). Shared by the
    /// primary and overlay resolvers so the two cannot diverge
    /// (MDD-BUG-CRUCIBLE-00644).
    fn has_stream_separator(path: &str) -> bool {
        path.split(['\\', '/'])
            .any(|component| component.contains(':'))
    }

    fn resolve_overlay_path(overlay: &Path, path: &str) -> Option<PathBuf> {
        let mut relative = PathBuf::new();
        let path = path.trim_start_matches('\\').trim_start_matches('/');
        for component in path.split(['\\', '/']) {
            if component.is_empty() || component == "." {
                continue;
            }
            if component == ".." || component.contains(':') {
                return None;
            }
            relative.push(component);
        }

        // Verify, never create. `ensure_write_overlay` already made this
        // directory safely; re-running `create_dir_all` here would walk
        // through a symlink planted at the root's name between calls, which
        // is the same hole one TOCTOU window later (MDD-BUG-CRU-01410).
        if !std::fs::symlink_metadata(overlay).is_ok_and(|md| md.is_dir()) {
            return None;
        }
        let canonical_overlay = overlay.canonicalize().ok()?;
        let full = overlay.join(relative);

        if std::fs::symlink_metadata(&full).is_ok() {
            let canonical = full.canonicalize().ok()?;
            #[cfg(windows)]
            let within_overlay = path_starts_with_case_insensitive(&canonical, &canonical_overlay);
            #[cfg(not(windows))]
            let within_overlay = canonical.starts_with(&canonical_overlay);
            return within_overlay.then_some(canonical);
        }

        let parent = full.parent()?;
        let mut existing_ancestor = parent;
        while !existing_ancestor.exists() {
            existing_ancestor = existing_ancestor.parent()?;
        }
        let canonical_ancestor = existing_ancestor.canonicalize().ok()?;
        #[cfg(windows)]
        let ancestor_within_overlay =
            path_starts_with_case_insensitive(&canonical_ancestor, &canonical_overlay);
        #[cfg(not(windows))]
        let ancestor_within_overlay = canonical_ancestor.starts_with(&canonical_overlay);
        if !ancestor_within_overlay {
            return None;
        }

        std::fs::create_dir_all(parent).ok()?;
        let canonical_parent = parent.canonicalize().ok()?;
        #[cfg(windows)]
        let parent_within_overlay =
            path_starts_with_case_insensitive(&canonical_parent, &canonical_overlay);
        #[cfg(not(windows))]
        let parent_within_overlay = canonical_parent.starts_with(&canonical_overlay);
        if !parent_within_overlay {
            return None;
        }

        Some(canonical_parent.join(full.file_name()?))
    }

    /// Resolve a DOS path to the overlay directory, creating parent dirs as needed.
    fn resolve_overlay(&self, path: &str) -> Option<PathBuf> {
        let overlay = self.write_overlay.as_ref()?;
        Self::resolve_overlay_path(overlay, path)
    }

    /// Resolve a DOS path to the overlay directory, initializing overlay if needed.
    fn resolve_overlay_lazy(&mut self, path: &str) -> Option<PathBuf> {
        let overlay = self.ensure_write_overlay()?.clone();
        Self::resolve_overlay_path(&overlay, path)
    }

    /// Resolve a DOS path to the host path that actually backs it.
    ///
    /// The overlay is an UPPER LAYER: when an entry exists there it shadows the
    /// root, because it is the more recent version (or the only one — the
    /// overlay exists precisely because the root refused the create). Every
    /// read and every mutation must resolve through here, or the two layers
    /// disagree about what the drive contains.
    ///
    /// MDD-BUG-CRUCIBLE-00646: this used to be open-coded at three call sites
    /// and absent from the rest, so a file the guest had successfully saved was
    /// invisible to a read-only reopen, to `rename`, to attribute and timestamp
    /// updates, and to directory enumeration. Routing every site through one
    /// resolver is what stops the layers drifting apart again.
    fn resolve_effective(&self, path: &str) -> Result<PathBuf> {
        if let Some(overlay_path) = self.overlay_path_if_present(path) {
            return Ok(overlay_path);
        }
        self.resolve(path)
    }

    /// Whether `path`'s parent directory exists in the overlay but not in the
    /// root. A new entry there can only be created in the overlay: the root
    /// would report the path missing (MDD-BUG-CRU-01399).
    fn parent_only_in_overlay(&self, path: &str) -> bool {
        let parent = path.rfind(['\\', '/']).map_or("", |at| &path[..at]);
        !parent.is_empty()
            && self.overlay_path_if_present(parent).is_some()
            && !self
                .resolve(parent)
                .is_ok_and(|root_parent| root_parent.exists())
    }

    /// The overlay host path for `path`, but only when something is there.
    fn overlay_path_if_present(&self, path: &str) -> Option<PathBuf> {
        self.write_overlay.as_ref()?;
        let overlay_path = self.resolve_overlay(path)?;
        overlay_path.exists().then_some(overlay_path)
    }

    /// The body of `create_dir` past the CD-ROM refusal, with the root create
    /// passed in so a test can stand in for a read-only filesystem mount,
    /// which no temp directory can produce (MDD-BUG-CRU-01655).
    fn create_dir_with_root_attempt(
        &mut self,
        path: &str,
        root_create: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<()> {
        if !self.parent_only_in_overlay(path) {
            match root_create(&self.resolve(path)?) {
                Ok(()) => return Ok(()),
                Err(e) if !Self::is_filesystem_level_error(&e) => return Err(e.into()),
                Err(_) => {}
            }
        }
        let overlay_path = self
            .resolve_overlay_lazy(path)
            .ok_or_else(|| anyhow!("path escapes write overlay: {path}"))?;
        std::fs::create_dir(overlay_path).map_err(|e| e.into())
    }

    /// Keep overlay routing identical to ordinary creation, while refusing
    /// occupied names in either layer and creating the destination exclusively.
    fn create_new_file_with_root_attempt(
        &mut self,
        path: &str,
        root_create: impl FnOnce(&Path) -> std::io::Result<std::fs::File>,
    ) -> Result<Box<dyn VirtualFile>> {
        let collision = || std::io::Error::from(std::io::ErrorKind::AlreadyExists);
        if let Some(overlay) = &self.write_overlay {
            // Use the host resolver for DOS case and short-name aliases too.
            let overlay_path = HostDrive::new(overlay.clone()).resolve(path)?;
            if std::fs::symlink_metadata(overlay_path).is_ok() {
                return Err(collision().into());
            }
        }
        if !self.parent_only_in_overlay(path) {
            let full_path = self.resolve(path)?;
            if std::fs::symlink_metadata(&full_path).is_ok() {
                return Err(collision().into());
            }
            match root_create(&full_path) {
                Ok(file) => return Ok(Box::new(file)),
                Err(error) if !Self::is_filesystem_level_error(&error) => return Err(error.into()),
                Err(_) => {}
            }
        }
        let overlay_path = self
            .resolve_overlay_lazy(path)
            .ok_or_else(|| anyhow!("path escapes write overlay: {path}"))?;
        let file = Self::open_host_file_options(&overlay_path, true, false, false, true)?;
        Ok(Box::new(file))
    }

    /// Check if an I/O error indicates a filesystem-level issue (the filesystem
    /// itself doesn't support file creation) rather than a file-level permission
    /// error. Filesystem-level errors trigger the write overlay fallback.
    fn is_filesystem_level_error(e: &std::io::Error) -> bool {
        // On Windows, ERROR_INVALID_PARAMETER (87) is returned by filesystems
        // that don't support file creation (e.g., WinFSP/mddskimg disk images).
        // ERROR_WRITE_PROTECT (19) indicates a write-protected disk.
        // These are distinct from ERROR_ACCESS_DENIED (5) which is file-level.
        #[cfg(windows)]
        {
            if let Some(code) = e.raw_os_error() {
                return matches!(code, 19 | 87);
            }
        }
        // On Unix, EROFS (read-only filesystem) indicates a filesystem-level issue.
        #[cfg(not(windows))]
        {
            if e.kind() == std::io::ErrorKind::ReadOnlyFilesystem {
                return true;
            }
            if let Some(code) = e.raw_os_error() {
                return code == 30; // EROFS
            }
        }
        false
    }

    fn is_symlink_or_reparse_point(metadata: &std::fs::Metadata) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;

            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        }
        #[cfg(not(windows))]
        {
            metadata.file_type().is_symlink()
        }
    }

    /// Open the resolved leaf without following a link installed after path
    /// validation. Existing in-root links have already been canonicalized by
    /// `resolve_checked`, so a reparse point here is either dangling or raced.
    fn open_host_file(
        path: &Path,
        write: bool,
        create: bool,
        truncate: bool,
    ) -> std::io::Result<std::fs::File> {
        Self::open_host_file_options(path, write, create, truncate, false)
    }

    fn open_host_file_options(
        path: &Path,
        write: bool,
        create: bool,
        truncate: bool,
        create_new: bool,
    ) -> std::io::Result<std::fs::File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(write)
            .create(create)
            .truncate(truncate)
            .create_new(create_new);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;

            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }

        let file = options.open(path)?;

        #[cfg(windows)]
        if Self::is_symlink_or_reparse_point(&file.metadata()?) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "refusing to open a filesystem reparse point",
            ));
        }

        Ok(file)
    }

    fn resolve_generated_sfn_path(&self, dos_path: &str, canonical_root: &Path) -> Option<PathBuf> {
        let normalized = dos_path.replace('\\', "/");
        let parts: Vec<&str> = normalized
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();

        let mut current = self.root_path.clone();
        // Set when the final component does not exist: real DOS resolves the
        // directory chain and then creates the missing leaf in it, so a
        // missing last component is the create case, not a failure.
        let mut created_leaf = false;
        for (idx, part) in parts.iter().enumerate() {
            let part = *part;
            if part == ".." {
                current = current.parent()?.to_path_buf();
                continue;
            }

            let exact = current.join(part);
            if exact.exists() {
                current = exact;
                continue;
            }

            if let Some(case_matched) = Self::resolve_case_insensitive_component(&current, part) {
                current = case_matched;
                continue;
            }

            if part.contains('~') {
                if let Some(sfn_matched) = Self::resolve_generated_sfn_component(&current, part) {
                    current = sfn_matched;
                    continue;
                }
            }

            if idx + 1 == parts.len() {
                current = current.join(part);
                created_leaf = true;
                break;
            }
            return None;
        }

        if created_leaf {
            // Verify the resolved PARENT directory is inside the root; the
            // leaf itself does not exist yet.
            let leaf = current.file_name()?.to_os_string();
            let canonical_parent = current.parent()?.canonicalize().ok()?;
            #[cfg(windows)]
            let within_root = path_starts_with_case_insensitive(&canonical_parent, canonical_root);
            #[cfg(not(windows))]
            let within_root = canonical_parent.starts_with(canonical_root);
            return within_root.then(|| canonical_parent.join(leaf));
        }

        if !current.exists() {
            return None;
        }

        let canonical = current.canonicalize().unwrap_or_else(|_| current.clone());
        #[cfg(windows)]
        let within_root = path_starts_with_case_insensitive(&canonical, canonical_root);
        #[cfg(not(windows))]
        let within_root = canonical.starts_with(canonical_root);

        within_root.then_some(canonical)
    }

    fn resolve_case_insensitive_component(parent: &Path, requested: &str) -> Option<PathBuf> {
        for entry in std::fs::read_dir(parent).ok()? {
            let Ok(entry) = entry else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.eq_ignore_ascii_case(requested) {
                return Some(entry.path());
            }
        }

        None
    }

    fn resolve_generated_sfn_component(parent: &Path, requested: &str) -> Option<PathBuf> {
        use crate::sfn;

        let requested = requested.to_uppercase();
        let mut entries = Vec::new();

        for entry in std::fs::read_dir(parent).ok()? {
            let Ok(entry) = entry else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            entries.push((name, entry.path()));
        }

        let names: Vec<String> = entries.iter().map(|(name, _)| name.clone()).collect();
        let assignments: std::collections::HashMap<_, _> =
            sfn::assign_short_names_in_dir(Some(parent), &names)
                .into_iter()
                .collect();

        for (name, path) in entries {
            if assignments
                .get(&name)
                .is_some_and(|short| short.eq_ignore_ascii_case(&requested))
            {
                return Some(path);
            }
        }

        None
    }

    /// Resolve a DOS path to a host path, with directory traversal protection.
    /// Returns None if the path would escape the root directory.
    fn resolve_checked(&self, path: &str) -> Option<PathBuf> {
        // Strip leading slashes/backslashes
        let p = path.trim_start_matches('\\').trim_start_matches('/');

        // MDD-BUG-CRUCIBLE-00644: a colon may only ever be the leading drive
        // designator, which `PathResolver::split_drive_and_path` has already
        // consumed — so no component reaching a backend may contain one. On
        // NTFS, `synthetic-program:Zone.Identifier` names an ALTERNATE DATA STREAM, not
        // a DOS file, so passing it through let a guest read host metadata and
        // create, truncate, delete or rename streams. Rejecting here covers
        // every HostDrive operation at once, because they all resolve through
        // this one gate. `resolve_overlay_path` already applies the identical
        // rule; this closes the matching hole on the primary path.
        if Self::has_stream_separator(p) {
            tracing::warn!(
                target: "gp64::dos::files",
                ?path,
                "Rejected path with a stream separator (NTFS alternate data stream)"
            );
            return None;
        }

        // Convert DOS path separators to native
        let normalized = p.replace('\\', std::path::MAIN_SEPARATOR_STR);

        // Join with root path
        let joined = self.root_path.join(&normalized);

        // Get canonical root for comparison.  canonicalize() fails on WinFSP-mounted
        // drives (mddskimg) so fall back to the raw root path.  Directory-traversal
        // protection still works because DOS paths are pre-normalised (no ".."
        // components reach here) and we use starts_with on the joined result.
        let canonical_root = self
            .root_path
            .canonicalize()
            .unwrap_or_else(|_| self.root_path.clone());

        // Try the exact path first (fast path). `symlink_metadata` observes a
        // dangling final link that `Path::exists` would hide.
        if let Ok(metadata) = std::fs::symlink_metadata(&joined) {
            let canonical = match joined.canonicalize() {
                Ok(canonical) => canonical,
                Err(error) if Self::is_symlink_or_reparse_point(&metadata) => {
                    tracing::warn!(
                        target: "gp64::dos::files",
                        ?path,
                        ?error,
                        "Directory traversal blocked: unresolved symlink or reparse point"
                    );
                    return None;
                }
                Err(_) => joined.clone(),
            };
            #[cfg(windows)]
            let within_root = path_starts_with_case_insensitive(&canonical, &canonical_root);
            #[cfg(not(windows))]
            let within_root = canonical.starts_with(&canonical_root);
            if within_root {
                return Some(canonical);
            }
            tracing::warn!(
                target: "gp64::dos::files",
                ?path,
                ?canonical_root,
                "Directory traversal blocked: path escapes root"
            );
            return None;
        }

        // On real DOS, filenames are silently truncated to 8.3 format before
        // pass long names (e.g. "Brazilian.PES") expecting DOS to truncate to
        // "BRAZILIA.PES". Try the truncated name as a fallback.
        if let Some(file_name) = std::path::Path::new(&normalized).file_name() {
            let name_str = file_name.to_string_lossy();
            let truncated = Self::truncate_filename_8_3(&name_str);
            if truncated != name_str.as_ref() {
                let parent = std::path::Path::new(&normalized)
                    .parent()
                    .map(|p| self.root_path.join(p))
                    .unwrap_or_else(|| self.root_path.clone());
                let truncated_path = parent.join(&truncated);
                if truncated_path.exists() {
                    let canonical = truncated_path
                        .canonicalize()
                        .unwrap_or_else(|_| truncated_path.clone());
                    #[cfg(windows)]
                    let within_root =
                        path_starts_with_case_insensitive(&canonical, &canonical_root);
                    #[cfg(not(windows))]
                    let within_root = canonical.starts_with(&canonical_root);
                    if within_root {
                        return Some(canonical);
                    }
                }
            }
        }

        if let Some(sfn_resolved) = self.resolve_generated_sfn_path(p, &canonical_root) {
            return Some(sfn_resolved);
        }

        // On case-sensitive filesystems (Linux), try case-insensitive matching.
        #[cfg(not(windows))]
        if let Some(case_matched) = self.resolve_case_insensitive(p) {
            if case_matched.exists() {
                if let Ok(canonical) = case_matched.canonicalize() {
                    if canonical.starts_with(&canonical_root) {
                        return Some(canonical);
                    }
                }
            } else if let Some(parent) = case_matched.parent() {
                // File doesn't exist yet (creation case). If the parent directory
                // exists with correct casing, use this path so the file is created
                // in the right place (e.g. /tmp/ not /TMP/).
                if parent.exists() {
                    if let Ok(canonical_parent) = parent.canonicalize() {
                        if canonical_parent.starts_with(&canonical_root) {
                            let candidate = canonical_parent.join(case_matched.file_name()?);
                            return Some(candidate);
                        }
                    }
                }
            }
        }

        // On non-Windows: if the filename contains '~', check the SFN reverse map.
        // This handles the case where a DOS program opens a file by its generated
        // 8.3 short name (e.g. LONGFI~1.TXT) but the host filesystem only has
        // the long name (e.g. LongFileName.txt).
        #[cfg(not(windows))]
        if let Some(file_component) = std::path::Path::new(p).file_name() {
            let file_str = file_component.to_string_lossy();
            if file_str.contains('~') {
                let parent_dos = std::path::Path::new(p)
                    .parent()
                    .unwrap_or(std::path::Path::new(""));
                let parent_host = self
                    .root_path
                    .join(parent_dos.to_string_lossy().replace('\\', "/"));
                let file_upper = file_str.to_uppercase();

                // Check the sfn_map for this directory
                if let Some(dir_map) = self.sfn_map.get(&parent_host) {
                    if let Some(long_name) = dir_map.get(&file_upper) {
                        let resolved = parent_host.join(long_name);
                        if resolved.exists() {
                            if let Ok(canonical) = resolved.canonicalize() {
                                if canonical.starts_with(&canonical_root) {
                                    return Some(canonical);
                                }
                            }
                        }
                    }
                }

                // Also try with canonical parent path
                if let Ok(canonical_parent) = parent_host.canonicalize() {
                    if let Some(dir_map) = self.sfn_map.get(&canonical_parent) {
                        if let Some(long_name) = dir_map.get(&file_upper) {
                            let resolved = canonical_parent.join(long_name);
                            if resolved.exists() {
                                if let Ok(canonical) = resolved.canonicalize() {
                                    if canonical.starts_with(&canonical_root) {
                                        return Some(canonical);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fall back: walk up to nearest existing ancestor for ".." resolution
        {
            let mut current = joined.as_path();
            let mut remainder: Vec<std::ffi::OsString> = Vec::new();

            let ancestor_metadata = loop {
                match std::fs::symlink_metadata(current) {
                    Ok(metadata) => break metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let name = current.file_name()?.to_os_string();
                        remainder.push(name);
                        current = current.parent()?;
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "gp64::dos::files",
                            ?path,
                            ?error,
                            "Directory traversal blocked: path ancestor cannot be inspected"
                        );
                        return None;
                    }
                }
            };

            let resolved_parent = match current.canonicalize() {
                Ok(parent) => parent,
                Err(error) if Self::is_symlink_or_reparse_point(&ancestor_metadata) => {
                    tracing::warn!(
                        target: "gp64::dos::files",
                        ?path,
                        ?error,
                        "Directory traversal blocked: unresolved symlink or reparse point"
                    );
                    return None;
                }
                Err(_) => current.to_path_buf(),
            };
            #[cfg(windows)]
            let parent_ok = path_starts_with_case_insensitive(&resolved_parent, &canonical_root);
            #[cfg(not(windows))]
            let parent_ok = resolved_parent.starts_with(&canonical_root);
            if !parent_ok {
                tracing::warn!(
                    target: "gp64::dos::files",
                    ?path,
                    ?canonical_root,
                    "Directory traversal blocked: path escapes root"
                );
                return None;
            }

            let mut candidate = resolved_parent;
            for component in remainder.iter().rev() {
                candidate = candidate.join(component);
            }

            // Ensure the resolved path is under the root directory
            #[cfg(windows)]
            let within_root = path_starts_with_case_insensitive(&candidate, &canonical_root);
            #[cfg(not(windows))]
            let within_root = candidate.starts_with(&canonical_root);
            if within_root {
                return Some(candidate);
            }
        }

        tracing::warn!(
            target: "gp64::dos::files",
            ?path,
            ?canonical_root,
            "Directory traversal blocked: path escapes root"
        );
        None
    }

    /// On case-sensitive filesystems, resolve a DOS path by matching each component
    /// case-insensitively. Returns the host path with correct casing if found.
    #[cfg(not(windows))]
    fn resolve_case_insensitive(&self, dos_path: &str) -> Option<PathBuf> {
        let parts: Vec<String> = dos_path
            .replace('\\', "/")
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .map(|c| c.to_string())
            .collect();

        let mut current = self.root_path.clone();

        for (idx, part) in parts.iter().enumerate() {
            if part == ".." {
                current = current.parent()?.to_path_buf();
                // Verify we haven't traversed above the root directory
                if !current.starts_with(&self.root_path) {
                    return None;
                }
                continue;
            }

            // Try exact match first (fast)
            let exact = current.join(part);
            if exact.exists() {
                current = exact;
                continue;
            }

            // Case-insensitive scan of the directory
            let part_upper = part.to_uppercase();
            let mut found = false;
            if let Ok(entries) = std::fs::read_dir(&current) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.to_uppercase() == part_upper {
                        current = entry.path();
                        found = true;
                        break;
                    }
                }
            }

            if !found {
                // Component not found even with case-insensitive matching.
                // Return the path with the remaining components appended
                // (for create operations where the file doesn't exist yet).
                for remaining in &parts[idx..] {
                    current = current.join(remaining);
                }
                return Some(current);
            }
        }

        Some(current)
    }

    fn resolve(&self, path: &str) -> Result<PathBuf> {
        self.resolve_checked(path)
            .ok_or_else(|| anyhow!("path escapes root: {path}"))
    }
}

/// The DOS hidden and system bits a host file carries (MDD-BUG-CRU-01397).
///
/// Windows stores both natively. Other hosts have no place for them, so a
/// host file there is never hidden or system.
fn host_hidden_system(metadata: &std::fs::Metadata) -> (bool, bool) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let attributes = metadata.file_attributes();
        (attributes & 0x02 != 0, attributes & 0x04 != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        (false, false)
    }
}

/// Windows carries the DOS archive flag natively; other hosts retain the policy
/// that non-directory entries are archived and directories are not.
fn host_archive(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        windows_archive(metadata.file_attributes())
    }
    #[cfg(not(windows))]
    {
        !metadata.is_dir()
    }
}

#[cfg(any(windows, test))]
fn windows_archive(attributes: u32) -> bool {
    attributes & 0x20 != 0
}

/// DOS directory entries have no file payload, even when the host filesystem
/// reports an implementation-specific directory metadata length.
fn host_dos_file_size(metadata: &std::fs::Metadata) -> u64 {
    if metadata.is_dir() {
        0
    } else {
        metadata.len()
    }
}

fn file_times(accessed: Option<SystemTime>, modified: Option<SystemTime>) -> std::fs::FileTimes {
    let mut times = std::fs::FileTimes::new();
    if let Some(accessed) = accessed {
        times = times.set_accessed(accessed);
    }
    if let Some(modified) = modified {
        times = times.set_modified(modified);
    }
    times
}

#[cfg(windows)]
fn set_host_file_times(
    path: &Path,
    accessed: Option<SystemTime>,
    modified: Option<SystemTime>,
) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;

    // Match filetime's Windows opener and retain directory support.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .set_times(file_times(accessed, modified))
}

#[cfg(not(windows))]
fn set_host_file_times(
    path: &Path,
    accessed: Option<SystemTime>,
    modified: Option<SystemTime>,
) -> std::io::Result<()> {
    // Match filetime's Unix opener: read-only first, then write-only fallback.
    let file = std::fs::File::open(path).or_else(|_| OpenOptions::new().write(true).open(path))?;
    file.set_times(file_times(accessed, modified))
}

/// Applies the DOS read-only, hidden, system and archive bits to a host file
/// (INT 21h AH=43h AL=01h, 7143h BL=01h), or fails without changing anything.
///
/// MDD-BUG-CRU-01397: this used to apply read-only alone and report success,
/// dropping hidden and system, which the INT 21h layer
/// hands down. It also cleared read-only on Unix with `set_readonly(false)`,
/// which is `chmod a+w` and left every such file writable by every user.
#[cfg(windows)]
fn set_host_attributes(path: &Path, attrs: &FileAttributes) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    const READONLY: u32 = 0x01;
    const HIDDEN: u32 = 0x02;
    const SYSTEM: u32 = 0x04;
    const ARCHIVE: u32 = 0x20;
    const NORMAL: u32 = 0x80;
    extern "system" {
        fn SetFileAttributesW(file_name: *const u16, file_attributes: u32) -> i32;
    }

    // Keep every host bit DOS does not own (compressed, not-indexed, ...).
    let current = std::fs::metadata(path)?.file_attributes();
    let mut attributes = current & !(READONLY | HIDDEN | SYSTEM | ARCHIVE | NORMAL);
    for (set, bit) in [
        (attrs.read_only, READONLY),
        (attrs.hidden, HIDDEN),
        (attrs.system, SYSTEM),
        (attrs.archive, ARCHIVE),
    ] {
        if set {
            attributes |= bit;
        }
    }
    // SetFileAttributes takes NORMAL for "no attributes"; directories keep
    // their directory bit, which it ignores.
    if attributes == 0 {
        attributes = NORMAL;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call.
    if unsafe { SetFileAttributesW(wide.as_ptr(), attributes) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_host_attributes(path: &Path, attrs: &FileAttributes) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // The host cannot store hidden or system. Refuse before touching the
    // file rather than report a success the next read would contradict.
    if attrs.hidden || attrs.system {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "this host filesystem cannot store the DOS hidden or system attribute",
        )
        .into());
    }
    let mut permissions = std::fs::metadata(path)?.permissions();
    let mode = permissions.mode();
    // Read-only clears every write bit; clearing it restores the OWNER's write
    // bit only, never group or other (`set_readonly(false)` is `chmod a+w`).
    let mode = if attrs.read_only {
        mode & !0o222
    } else {
        mode | 0o200
    };
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|e| e.into())
}

// Wrapper to implement VirtualFile for std::fs::File
impl VirtualFile for std::fs::File {
    fn host_identity(&self) -> Result<Option<HostFileIdentity>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = self.metadata()?;
            Ok(Some(HostFileIdentity {
                device: metadata.dev(),
                file: metadata.ino(),
            }))
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;

            #[repr(C)]
            #[derive(Default)]
            struct ByHandleFileInformation {
                attributes: u32,
                creation_time: [u32; 2],
                access_time: [u32; 2],
                write_time: [u32; 2],
                volume_serial: u32,
                size_high: u32,
                size_low: u32,
                number_of_links: u32,
                index_high: u32,
                index_low: u32,
            }

            #[link(name = "kernel32")]
            extern "system" {
                fn GetFileInformationByHandle(
                    handle: *mut std::ffi::c_void,
                    information: *mut ByHandleFileInformation,
                ) -> i32;
            }

            let mut information = ByHandleFileInformation::default();
            // SAFETY: the live File owns the handle, and the output buffer has
            // the Win32 BY_HANDLE_FILE_INFORMATION layout and stays writable.
            if unsafe { GetFileInformationByHandle(self.as_raw_handle(), &mut information) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Some(HostFileIdentity {
                device: u64::from(information.volume_serial),
                file: (u64::from(information.index_high) << 32) | u64::from(information.index_low),
            }))
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(anyhow!(
                "host file identity is unavailable on this platform"
            ))
        }
    }

    fn set_len(&mut self, size: u64) -> Result<()> {
        std::fs::File::set_len(self, size).map_err(|e| e.into())
    }
    fn sync_all(&mut self) -> Result<()> {
        std::fs::File::sync_all(self).map_err(|e| e.into())
    }
}

/// Order a host directory listing as the DOS model's directory cache presents a
/// local drive (`drive_cache.cpp` `SortByDirName`): directories before files,
/// then by 8.3 short name compared byte for byte. A short name is the one the
/// guest sees for the entry (`sfn::assign_short_names_in_dir`), so a long host
/// name sorts by its alias. The result is the same on every host filesystem.
fn sort_host_listing(entries: &mut [(String, FileAttributes)], host_dir: &Path) {
    let names: Vec<String> = entries.iter().map(|(name, _)| name.clone()).collect();
    let short_names: std::collections::HashMap<String, String> =
        crate::sfn::assign_short_names_in_dir(Some(host_dir), &names)
            .into_iter()
            .collect();
    let key = |name: &String| {
        short_names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_uppercase())
    };
    entries.sort_by_cached_key(|(name, attrs)| (!attrs.directory, key(name), name.clone()));
}

impl VirtualDrive for HostDrive {
    fn open_file(
        &mut self,
        path: &str,
        write: bool,
        create: bool,
        truncate: bool,
    ) -> Result<Box<dyn VirtualFile>> {
        // An entry already in the overlay shadows the root for EVERY access
        // mode, not just write/create. Gating this on `write || create` meant a
        // file the guest had just saved could not be reopened read-only when no
        // root copy existed (MDD-BUG-CRUCIBLE-00646).
        if let Some(overlay_path) = self.overlay_path_if_present(path) {
            let f = Self::open_host_file(&overlay_path, write, create, truncate)?;
            return Ok(Box::new(f));
        }

        if create && self.parent_only_in_overlay(path) {
            let overlay_path = self
                .resolve_overlay_lazy(path)
                .ok_or_else(|| anyhow!("path escapes write overlay: {path}"))?;
            let f = Self::open_host_file(&overlay_path, write, true, truncate)?;
            return Ok(Box::new(f));
        }

        let full_path = self.resolve(path)?;
        let file = Self::open_host_file(&full_path, write, create, truncate);
        match file {
            Ok(f) => Ok(Box::new(f)),
            Err(e) if create && Self::is_filesystem_level_error(&e) => {
                // File creation failed at the filesystem level (not a file-level
                // permission error). This commonly happens with read-only disk
                // image mounts (mddskimg/WinFSP) that return ERROR_INVALID_PARAMETER
                // (87) or ERROR_WRITE_PROTECT (19) for any create operation.
                // Lazily initialize a writable overlay directory and retry there.
                let overlay_path = self
                    .resolve_overlay_lazy(path)
                    .ok_or_else(|| anyhow!("path escapes write overlay: {path}"))?;
                tracing::debug!(
                    target: "gp64::dos::files",
                    ?path,
                    ?overlay_path,
                    root_error = %e,
                    "open_file: creation failed on root, using write overlay"
                );
                let f = Self::open_host_file(&overlay_path, write, true, truncate)?;
                Ok(Box::new(f))
            }
            Err(e)
                if write
                    && !create
                    && !truncate
                    && e.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                // Host-drive policy: a non-creating, non-truncating write open
                // denied by host permissions may downgrade to a read-only
                // handle. Subsequent writes still fail through that handle.
                tracing::debug!(
                    target: "gp64::dos::files",
                    ?path,
                    "open_file: write denied, falling back to read-only"
                );
                let f = Self::open_host_file(&full_path, false, false, false)?;
                Ok(Box::new(f))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn create_new_file(&mut self, path: &str) -> Result<Box<dyn VirtualFile>> {
        self.create_new_file_with_root_attempt(path, |full_path| {
            Self::open_host_file_options(full_path, true, false, false, true)
        })
    }

    fn create_dir(&mut self, path: &str) -> Result<()> {
        // Directory creation follows file creation onto the write overlay
        // (MDD-BUG-CRU-01399): a parent that exists only in the overlay holds
        // the new directory too, and a root that refuses the create at the
        // filesystem level (a read-only image mount) falls back to it.
        self.create_dir_with_root_attempt(path, |root_path| std::fs::create_dir(root_path))
    }

    fn remove_file(&mut self, path: &str) -> Result<()> {
        std::fs::remove_file(self.resolve_effective(path)?).map_err(|e| e.into())
    }

    fn remove_dir(&mut self, path: &str) -> Result<()> {
        std::fs::remove_dir(self.resolve_effective(path)?).map_err(|e| e.into())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        // A source living in the overlay must stay in the overlay: the root
        // refused to hold it in the first place, so renaming it onto the root
        // would fail (or, worse, half-succeed against a stale root copy).
        // MDD-BUG-CRUCIBLE-00646.
        if self.overlay_path_if_present(from).is_some() {
            let source = self.resolve_effective(from)?;
            let target = self
                .resolve_overlay_lazy(to)
                .ok_or_else(|| anyhow!("path escapes write overlay: {to}"))?;
            return std::fs::rename(source, target).map_err(|e| e.into());
        }
        std::fs::rename(self.resolve(from)?, self.resolve(to)?).map_err(|e| e.into())
    }

    fn get_attributes(&mut self, path: &str) -> Result<FileAttributes> {
        let metadata = std::fs::metadata(self.resolve_effective(path)?)?;
        let size = host_dos_file_size(&metadata);
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let read_only = metadata.permissions().readonly();
        let directory = metadata.is_dir();
        let (hidden, system) = host_hidden_system(&metadata);

        Ok(FileAttributes {
            read_only,
            hidden,
            system,
            volume_label: false,
            directory,
            archive: host_archive(&metadata),
            size,
            modified,
        })
    }

    fn get_file_metadata(&mut self, path: &str) -> Result<FileMetadata> {
        let full_path = self.resolve_effective(path)?;
        let metadata = std::fs::metadata(&full_path)?;
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let (hidden, system) = host_hidden_system(&metadata);
        let attributes = FileAttributes {
            read_only: metadata.permissions().readonly(),
            hidden,
            system,
            volume_label: false,
            directory: metadata.is_dir(),
            archive: host_archive(&metadata),
            size: host_dos_file_size(&metadata),
            modified,
        };

        Ok(FileMetadata {
            attributes,
            creation_time: metadata.created().unwrap_or(modified),
            last_access_time: metadata.accessed().unwrap_or(modified),
            last_write_time: modified,
        })
    }

    fn set_attributes(&mut self, path: &str, attrs: FileAttributes) -> Result<()> {
        let full_path = self.resolve_effective(path)?;
        set_host_attributes(&full_path, &attrs)
    }

    fn set_file_time(&mut self, path: &str, modified: SystemTime) -> Result<()> {
        let full_path = self.resolve_effective(path)?;
        set_host_file_times(&full_path, None, Some(modified))?;
        Ok(())
    }

    fn set_file_access_time(&mut self, path: &str, accessed: SystemTime) -> Result<()> {
        let full_path = self.resolve_effective(path)?;
        let metadata = std::fs::metadata(&full_path)?;
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        set_host_file_times(&full_path, Some(accessed), Some(modified))?;
        Ok(())
    }

    fn read_dir(&mut self, path: &str) -> Result<Vec<(String, FileAttributes)>> {
        let full_path = self.resolve(path)?;
        let mut entries = Vec::new();

        // MDD-BUG-CRUCIBLE-00645: report why enumeration failed instead of
        // returning an empty success. A caller that cannot see the difference
        // between "no entries" and "could not read the directory" has no way to
        // pick the right DOS error code, and no downstream mapping can recover
        // information discarded here.
        //
        // MDD-BUG-CRUCIBLE-00646: a directory can exist ONLY in the overlay —
        // the guest created a file under a path the root filesystem never held.
        // Enumeration must then succeed from the overlay alone rather than
        // reporting the root's NotFound, or a file the guest just saved is
        // invisible to FindFirst.
        let overlay_dir = self.overlay_path_if_present(path);
        let rd = match std::fs::read_dir(&full_path) {
            Ok(rd) => Some(rd),
            Err(e) if overlay_dir.is_some() && e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(rd) = rd {
            for entry in rd {
                let entry = entry?;
                let metadata = entry.metadata()?;
                let name = entry.file_name().to_string_lossy().into_owned();

                let is_dir = metadata.is_dir();
                let (hidden, system) = host_hidden_system(&metadata);
                let attrs = FileAttributes {
                    read_only: metadata.permissions().readonly(),
                    hidden,
                    system,
                    volume_label: false,
                    directory: is_dir,
                    archive: host_archive(&metadata),
                    size: host_dos_file_size(&metadata),
                    modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                };
                entries.push((name, attrs));
            }
        }

        // Merge the overlay's entries over the root's. The overlay is the upper
        // layer, so on a name collision its version wins — DOS names compare
        // case-insensitively, so the dedup must too (MDD-BUG-CRUCIBLE-00646).
        if let Some(overlay_dir) = overlay_dir {
            if let Ok(rd) = std::fs::read_dir(&overlay_dir) {
                for entry in rd.flatten() {
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let is_dir = metadata.is_dir();
                    let (hidden, system) = host_hidden_system(&metadata);
                    let attrs = FileAttributes {
                        read_only: metadata.permissions().readonly(),
                        hidden,
                        system,
                        volume_label: false,
                        directory: is_dir,
                        archive: host_archive(&metadata),
                        size: host_dos_file_size(&metadata),
                        modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    };
                    match entries
                        .iter_mut()
                        .find(|(existing, _)| existing.eq_ignore_ascii_case(&name))
                    {
                        Some(slot) => *slot = (name, attrs),
                        None => entries.push((name, attrs)),
                    }
                }
            }
        }

        // A host directory has no entry order of its own: readdir order is
        // whatever the host filesystem keeps (APFS listed B.TXT before A.TXT,
        // MDD-BUG-FLU-02174). the DOS model, presents a local drive's
        // FindFirst/FindNext in its DIRALPHABETICAL order on every host:
        // directories first, then by 8.3 short name (`DOS_Drive_Cache`,
        // `SortByDirName`, the default `SetDirSort(DIRALPHABETICAL)`).
        sort_host_listing(&mut entries, &full_path);

        // On non-Windows: build reverse mapping from 8.3 short names to long names
        // for any entry that isn't already a valid 8.3 filename.
        #[cfg(not(windows))]
        {
            use crate::sfn;
            use std::collections::BTreeMap;

            let mut has_long_names = false;
            for (name, _) in &entries {
                if !sfn::is_valid_dos_filename(name) {
                    has_long_names = true;
                    break;
                }
            }

            if has_long_names {
                let mut dir_map = BTreeMap::new();
                let names: Vec<String> = entries.iter().map(|(name, _)| name.clone()).collect();

                for (name, short) in sfn::assign_short_names(&names) {
                    // Only store mapping if the short name differs from the original
                    if short != name.to_uppercase() {
                        dir_map.insert(short, name);
                    }
                }

                if !dir_map.is_empty() {
                    self.sfn_map.insert(full_path, dir_map);
                }
            }
        }

        Ok(entries)
    }

    fn get_disk_space(&self) -> Result<(u64, u64)> {
        Ok((1024 * 1024 * 1024, 512 * 1024 * 1024))
    }

    fn resolve_host_path(&self, path: &str) -> Option<PathBuf> {
        self.resolve_checked(path)
    }
}

/// DOS null device.
pub struct NulDevice;

impl std::io::Read for NulDevice {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(0) // EOF
    }
}

impl std::io::Write for NulDevice {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len()) // Accept and discard
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Seek for NulDevice {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Ok(0) // NUL always at position 0
    }
}

impl VirtualFile for NulDevice {
    fn set_len(&mut self, _size: u64) -> Result<()> {
        Ok(()) // No-op for NUL device
    }
    fn sync_all(&mut self) -> Result<()> {
        Ok(()) // No-op for NUL device
    }
}

/// How many bytes each direction of [`DeviceBridge`] will hold.
///
/// A guest can write faster than the emulator drains, so every queue is bounded
/// and the device reports a short write rather than growing without limit.
pub const BRIDGE_CAPACITY: usize = 4096;

/// Shared bridge carrying bytes between the DOS character devices and the
/// emulator's hardware.
///
/// `Dos::int21` receives no serial or parallel handle and cannot be given one —
/// there are over a thousand call sites — so `AUX` and `PRN` exchange bytes
/// through this structure, which the emulator drains once per dispatch. It is
/// shared with the device objects by `Arc<Mutex<_>>`.
/// shares its state, which `VirtualFile`'s `Send` bound permits.
#[derive(Debug, Default)]
pub struct DeviceBridge {
    /// Written to `AUX`, awaiting injection into COM1's transmit register.
    aux_tx: std::collections::VecDeque<u8>,
    /// Received from COM1, staged for a pending `AUX` read.
    aux_rx: std::collections::VecDeque<u8>,
    /// Written to `PRN`, awaiting the printer accumulator.
    prn_out: std::collections::VecDeque<u8>,
    /// Independently open `PRN` device objects. Duplicated handles (AH=45h)
    /// share one SFT and therefore one device, so they count once — which is
    /// what stops closing the first of two dup'd handles cutting a job in half.
    prn_open: u32,
    /// Bumped each time `prn_open` falls back to zero, i.e. a job ended.
    prn_job_ends: u32,
    /// How many bytes an `AUX` read has asked for and not been given.
    ///
    /// This is the *only* thing that authorises the emulator to move a byte out
    /// of COM1's receiver, and it is why the fill is demand-driven rather than
    /// eager. That receiver is shared with INT 14h and with any guest polling
    /// the RBR directly, and `read_rbr` is destructive, so staging bytes nobody
    /// has asked for steals their input. It is a high-water mark, not a running
    /// total: a guest polling for one byte in a loop must not accumulate a
    /// standing request for hundreds.
    aux_rx_demand: usize,
    /// Non-consuming mirror of COM1's receive depth, refreshed by the emulator
    /// on every drain.
    ///
    /// IOCTL 44h/06h answers from this plus what is already staged, so a status
    /// poll tells the truth about pending input without moving a byte out of
    /// the UART — a consuming status would steal the byte from the read it is
    /// gating.
    uart_rx_len: usize,
    /// Characters `AH=04h`/`AH=05h` could not queue because the bridge was
    /// full. Real DOS blocks; the one thing we must not do is lose the byte
    /// silently, so it is counted and logged.
    dropped_chars: u64,
}

impl DeviceBridge {
    /// Queue bytes for COM1, returning how many were accepted. A short return
    /// is a short write all the way out to `AH=40h`: the bytes beyond it stay
    /// with the guest, so a retry sends each byte exactly once.
    pub fn write_aux(&mut self, buf: &[u8]) -> usize {
        let accepted = buf.len().min(BRIDGE_CAPACITY - self.aux_tx.len());
        self.aux_tx.extend(&buf[..accepted]);
        accepted
    }

    /// Take bytes staged from COM1 for an `AUX` read.
    pub fn read_aux(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.aux_rx.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.aux_rx.pop_front().unwrap_or(0);
        }
        n
    }

    /// Queue bytes for the printer, returning how many were accepted.
    pub fn write_prn(&mut self, buf: &[u8]) -> usize {
        let accepted = buf.len().min(BRIDGE_CAPACITY - self.prn_out.len());
        self.prn_out.extend(&buf[..accepted]);
        accepted
    }

    /// Record what an `AUX` read asked for and what it got.
    ///
    /// The shortfall becomes the demand a later drain fills. Every route into
    /// AUX input must call this — the SFT-backed `AuxDevice`, the pre-opened
    /// STDAUX handle 3, and `AH=03h` — because demand is the only thing that
    /// authorises the emulator to take a byte out of COM1, and nothing else
    /// (an open, a device object, a status poll) does.
    pub fn note_aux_read(&mut self, wanted: usize, got: usize) {
        let shortfall = wanted.saturating_sub(got).min(BRIDGE_CAPACITY);
        self.aux_rx_demand = self.aux_rx_demand.max(shortfall);
    }

    /// Stage a byte that arrived on COM1 for a pending `AUX` read.
    ///
    /// Returns false — storing nothing — when the staging queue is full or
    /// nothing has asked for the byte, so the caller leaves it in the UART
    /// rather than dropping it or stealing it from INT 14h.
    pub fn push_aux_rx(&mut self, byte: u8) -> bool {
        if self.aux_rx.len() >= BRIDGE_CAPACITY || self.aux_rx_demand == 0 {
            return false;
        }
        self.aux_rx.push_back(byte);
        self.aux_rx_demand -= 1;
        true
    }

    /// How many staged `AUX` bytes are waiting.
    pub fn aux_rx_len(&self) -> usize {
        self.aux_rx.len()
    }

    /// How many bytes the emulator may move out of COM1's receiver right now:
    /// what a pending read still wants, capped by the room left to stage it.
    ///
    /// The emulator must know this *before* it pops a byte out of the UART.
    /// `push_aux_rx` refuses without storing, and there is no way to put a byte
    /// back, so a blind push would lose it.
    pub fn aux_rx_demand(&self) -> usize {
        self.aux_rx_demand.min(BRIDGE_CAPACITY - self.aux_rx.len())
    }

    /// Publish COM1's receive depth, read non-destructively by the emulator.
    pub fn set_uart_rx_len(&mut self, len: usize) {
        self.uart_rx_len = len;
    }

    /// Whether a guest asking `AUX` for input would find any — either already
    /// staged here, or still sitting in COM1's receiver.
    ///
    /// This is what IOCTL 44h/06h reports. It deliberately consumes nothing and
    /// registers no demand: a *status* call must never move a byte, or it steals
    /// it from the read it was gating and from a direct RBR read.
    pub fn aux_input_ready(&self) -> bool {
        !self.aux_rx.is_empty() || self.uart_rx_len > 0
    }

    /// Record a character that could not be queued because the bridge was full.
    pub fn note_dropped_char(&mut self, device: &'static str) {
        self.dropped_chars += 1;
        tracing::warn!(
            target: "gp64::dos",
            device,
            dropped_total = self.dropped_chars,
            "character device queue full; a single-character write was lost \
             (real DOS would block until the device caught up)"
        );
    }

    /// How many single-character writes have been lost to a full queue.
    pub fn dropped_chars(&self) -> u64 {
        self.dropped_chars
    }

    /// Discard every byte in flight, in both directions, and any un-reported
    /// job boundary.
    ///
    /// Called on snapshot restore. Neither queue is snapshotted, so carrying
    /// pre-restore bytes into a restored guest would emit output that guest
    /// never wrote. The pending read demand and the UART mirror go with them:
    /// both describe bytes that no longer exist. The open count is deliberately
    /// left alone — it tracks live Rust device objects, which decrement
    /// themselves when dropped.
    pub fn clear_pending_bytes(&mut self) {
        self.aux_tx.clear();
        self.aux_rx.clear();
        self.prn_out.clear();
        self.prn_job_ends = 0;
        self.aux_rx_demand = 0;
        self.uart_rx_len = 0;
    }

    /// Drain everything queued for COM1.
    pub fn take_aux_tx(&mut self) -> Vec<u8> {
        self.aux_tx.drain(..).collect()
    }

    /// Put back bytes COM1 could not accept this tick, preserving order.
    pub fn unsend_aux_tx(&mut self, bytes: &[u8]) {
        for &b in bytes.iter().rev() {
            self.aux_tx.push_front(b);
        }
    }

    /// Drain everything queued for the printer.
    pub fn take_prn_out(&mut self) -> Vec<u8> {
        self.prn_out.drain(..).collect()
    }

    /// Take the count of jobs that ended since the last call.
    pub fn take_prn_job_ends(&mut self) -> u32 {
        std::mem::take(&mut self.prn_job_ends)
    }

    fn prn_opened(&mut self) {
        self.prn_open += 1;
    }

    fn prn_closed(&mut self) {
        self.prn_open = self.prn_open.saturating_sub(1);
        if self.prn_open == 0 {
            self.prn_job_ends += 1;
        }
    }
}

/// Shared handle to the device bridge.
pub type SharedDeviceBridge = std::sync::Arc<std::sync::Mutex<DeviceBridge>>;

/// `AUX` — the DOS character device for COM1.
///
/// Deliberately holds no open count. Input staging is driven purely by read
/// *demand* (`DeviceBridge::note_aux_read`), which is what makes the pre-opened
/// STDAUX handle 3 and `AH=03h` work identically to an `AH=3Dh` open — none of
/// those routes constructs one of these.
pub struct AuxDevice {
    bridge: SharedDeviceBridge,
}

impl AuxDevice {
    pub fn new(bridge: SharedDeviceBridge) -> Self {
        Self { bridge }
    }
}

impl std::io::Read for AuxDevice {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // Real DOS blocks until a byte arrives; we return what is staged and
        // record the shortfall so the emulator fetches exactly that much on its
        // next drain. Declared deviation — blocking needs the deferred-re-entry
        // protocol the keyboard uses.
        Ok(self
            .bridge
            .lock()
            .map(|mut b| {
                let got = b.read_aux(buf);
                b.note_aux_read(buf.len(), got);
                got
            })
            .unwrap_or(0))
    }
}

impl std::io::Write for AuxDevice {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(self
            .bridge
            .lock()
            .map(|mut b| b.write_aux(buf))
            .unwrap_or(0))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Seek for AuxDevice {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Ok(0) // Character devices have no position.
    }
}

impl VirtualFile for AuxDevice {
    fn set_len(&mut self, _size: u64) -> Result<()> {
        Ok(())
    }
    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

/// `PRN` — the DOS character device for LPT1.
///
/// Construction and drop are the job boundary: the bridge counts live device
/// objects, and a job ends when the last one goes away. Dup'd handles share one
/// SFT and therefore one device, so they close once; two independent opens are
/// two devices, so the job ends only when both are done — and the emulator
/// never has to inspect the file table to find out.
/// A `PrnDevice` counts only once its [`PrnOpenTicket`] is confirmed — see
/// there for why.
pub struct PrnDevice {
    bridge: SharedDeviceBridge,
    counted: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// The right to count one open `PRN` instance, handed back by
/// [`PrnDevice::new`] and redeemed only once the device is really in the file
/// table.
///
/// The open count is the job boundary, so an open that *fails* must leave it
/// untouched. Counting inside `new` did not: a device built for a file table
/// with no free slot is dropped on the spot, and that drop took the count 1 → 0
/// and flushed a job somebody else was still writing. The ticket exists because
/// `allocate_with_sharing` consumes the boxed device, so there is no way to
/// reach back into it after the allocation succeeds — but the ticket, holding a
/// clone of the same flag, is still in the caller's hand.
///
/// Dropping a ticket without confirming it is the normal failure path and does
/// nothing at all.
#[must_use = "an unconfirmed PRN open is never counted, so its close emits no job boundary"]
pub struct PrnOpenTicket {
    bridge: SharedDeviceBridge,
    counted: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PrnOpenTicket {
    /// Count this open. Call once the device has a handle.
    pub fn confirm(self) {
        self.counted
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut bridge) = self.bridge.lock() {
            bridge.prn_opened();
        }
    }
}

impl PrnDevice {
    /// Build a `PRN` device and the ticket that counts it. The device is inert
    /// until [`PrnOpenTicket::confirm`] runs.
    pub fn new(bridge: SharedDeviceBridge) -> (Self, PrnOpenTicket) {
        let counted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ticket = PrnOpenTicket {
            bridge: bridge.clone(),
            counted: counted.clone(),
        };
        (Self { bridge, counted }, ticket)
    }
}

impl Drop for PrnDevice {
    fn drop(&mut self) {
        if !self.counted.load(std::sync::atomic::Ordering::Relaxed) {
            // Never counted, so there is nothing to give back and — crucially —
            // no job boundary to emit.
            return;
        }
        if let Ok(mut b) = self.bridge.lock() {
            b.prn_closed();
        }
    }
}

impl std::io::Read for PrnDevice {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(0) // A printer has nothing to say.
    }
}

impl std::io::Write for PrnDevice {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(self
            .bridge
            .lock()
            .map(|mut b| b.write_prn(buf))
            .unwrap_or(0))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Seek for PrnDevice {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Ok(0)
    }
}

impl VirtualFile for PrnDevice {
    fn set_len(&mut self, _size: u64) -> Result<()> {
        Ok(())
    }
    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Immutable bytes behind a [`SyntheticFile`], shared by every handle that
/// opens the same synthetic path.
///
/// A synthetic file is read-only, so the payload never needs to be copied per
/// open — only the cursor is per-handle. Copying it per open multiplied host
/// tens of MiB (MDD-BUG-CRUCIBLE-00380).
#[derive(Clone, Debug)]
pub enum SyntheticPayload {
    /// Reference-counted bytes; cloning is a refcount bump, not a copy.
    Shared(std::sync::Arc<[u8]>),
    Static(&'static [u8]),
}

impl SyntheticPayload {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            SyntheticPayload::Shared(bytes) => bytes,
            SyntheticPayload::Static(bytes) => bytes,
        }
    }

    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
}

impl From<std::sync::Arc<[u8]>> for SyntheticPayload {
    fn from(bytes: std::sync::Arc<[u8]>) -> Self {
        SyntheticPayload::Shared(bytes)
    }
}

impl From<Vec<u8>> for SyntheticPayload {
    fn from(bytes: Vec<u8>) -> Self {
        SyntheticPayload::Shared(bytes.into())
    }
}

impl From<&'static [u8]> for SyntheticPayload {
    fn from(bytes: &'static [u8]) -> Self {
        SyntheticPayload::Static(bytes)
    }
}

/// Virtual file backed by in-memory bytes, used for serving synthetic binaries.
/// Each open handle has an independent cursor.
///
/// The bytes are shared; the cursor is not. Two handles onto the same path read
/// independently from one allocation.
pub struct SyntheticFile {
    payload: SyntheticPayload,
    pos: u64,
}

impl SyntheticFile {
    pub fn new(bytes: impl Into<SyntheticPayload>) -> Self {
        Self {
            payload: bytes.into(),
            pos: 0,
        }
    }

    /// The shared payload, for callers that want to prove two handles read the
    /// same allocation rather than two copies of it.
    pub fn payload(&self) -> &SyntheticPayload {
        &self.payload
    }
}

impl std::io::Read for SyntheticFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let bytes = self.payload.as_slice();
        // Seeking past the end is legal and reads nothing, matching the
        // `io::Cursor` semantics this used to inherit.
        let start = std::cmp::min(self.pos, bytes.len() as u64) as usize;
        let n = std::cmp::min(buf.len(), bytes.len() - start);
        buf[..n].copy_from_slice(&bytes[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl std::io::Write for SyntheticFile {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "synthetic file is read-only",
        ))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Seek for SyntheticFile {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        // Same rules as `io::Cursor`: past-the-end is allowed, negative or
        // overflowing is an error.
        let (base, offset) = match pos {
            std::io::SeekFrom::Start(n) => {
                self.pos = n;
                return Ok(n);
            }
            std::io::SeekFrom::End(n) => (self.payload.len() as u64, n),
            std::io::SeekFrom::Current(n) => (self.pos, n),
        };
        match base.checked_add_signed(offset) {
            Some(n) => {
                self.pos = n;
                Ok(n)
            }
            None => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid seek to a negative or overflowing position",
            )),
        }
    }
}

impl VirtualFile for SyntheticFile {
    fn set_len(&mut self, _size: u64) -> Result<()> {
        anyhow::bail!("synthetic file is read-only")
    }
    fn sync_all(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Check if a filename is a DOS device name (NUL, CON, AUX, PRN, etc.)
/// Returns the canonical device name if it is, or None if it's a regular file
pub fn parse_device_name(filename: &str) -> Option<&'static str> {
    // Extract base name (after path separators, before extension)
    let base_name = filename.rsplit(['\\', '/']).next().unwrap_or(filename);

    // Strip a leading drive prefix like "C:" on a drive-relative path with no
    // separator (e.g. "C:nul" → "nul"). DOS treats the reserved device names as
    // valid on any drive and in any directory.
    let base_name = {
        let b = base_name.as_bytes();
        if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
            &base_name[2..]
        } else {
            base_name
        }
    };

    // Remove extension if present
    let name_only = base_name.split('.').next().unwrap_or(base_name);
    // Strip trailing colon (CON:, NUL:, COM1:, etc.)
    let name_only = name_only.trim_end_matches(':');
    if name_only.is_empty() {
        return None;
    }

    let name_upper = name_only.to_uppercase();

    // Check for reserved device names.
    //
    // COM2..COM4 and LPT2/LPT3 are deliberately NOT devices. Only COM1 and LPT1
    // exist in this machine, and folding the others onto them would open `COM3`
    // successfully and silently route it to COM1 — worse than not finding it.
    // Real DOS resolves device names through its driver chain, so a port with
    // no driver is simply an ordinary filename, which is what returning None
    // makes it.
    match name_upper.as_str() {
        "NUL" => Some("NUL"),
        "CON" => Some("CON"),
        "AUX" | "COM1" => Some("AUX"),
        "PRN" | "LPT1" => Some("PRN"),
        _ => None,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::io::SeekFrom;
    use std::path::Path;
    use tempfile::tempdir;

    #[cfg(unix)]
    fn create_symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        match std::os::windows::fs::symlink_dir(target, link) {
            Ok(()) => Ok(()),
            Err(err) => {
                let status = std::process::Command::new("cmd")
                    .args(["/C", "mklink", "/J"])
                    .arg(link)
                    .arg(target)
                    .status();
                match status {
                    Ok(status) if status.success() => Ok(()),
                    Ok(_) => Err(err),
                    Err(cmd_err) => Err(cmd_err),
                }
            }
        }
    }

    #[cfg(unix)]
    fn create_symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[test]
    fn windows_archive_reads_native_flag_independently_of_directory() {
        // Native FILE_ATTRIBUTE_NORMAL, READONLY, ARCHIVE and DIRECTORY.
        for (attributes, expected) in [
            (0x80, false),
            (0x01, false),
            (0x20, true),
            (0x21, true),
            (0x10, false),
            (0x30, true),
        ] {
            assert_eq!(
                windows_archive(attributes),
                expected,
                "native attributes {attributes:#x}"
            );
        }
    }

    // ------------------------------------------------------------------
    // MDD-BUG-CRUCIBLE-00644 — a DOS guest must not be able to name an NTFS
    // alternate data stream. The drive designator is consumed by the path
    // resolver, so any colon still present at the backend is a stream
    // reference.
    // ------------------------------------------------------------------

    #[test]
    fn test_flu00012_host_exclusive_creation_and_overlay() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
        std::fs::create_dir_all(&base).unwrap();
        let root = tempfile::Builder::new()
            .prefix(&format!("flu00012-host-{}-", std::process::id()))
            .tempdir_in(&base)
            .unwrap();
        let overlay = tempfile::Builder::new()
            .prefix("flu00012-upper-")
            .tempdir_in(&base)
            .unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());
        // Insert a competing creator after resolution and collision checks.
        let raced = drive.create_new_file_with_root_attempt("\\RACE.TMP", |path| {
            std::fs::write(path, b"raced sentinel")?;
            HostDrive::open_host_file_options(path, true, false, false, true)
        });
        assert!(
            raced.is_err(),
            "a racing creator must win without being opened"
        );
        assert_eq!(
            raced
                .err()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read(root.path().join("RACE.TMP")).unwrap(),
            b"raced sentinel"
        );

        let fallback_raced = drive.create_new_file_with_root_attempt("\\UPPER.TMP", |_| {
            std::fs::write(overlay.path().join("UPPER.TMP"), b"upper sentinel")?;
            Err(std::io::Error::from_raw_os_error(if cfg!(windows) {
                19
            } else {
                30
            }))
        });
        assert_eq!(
            fallback_raced
                .err()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read(overlay.path().join("UPPER.TMP")).unwrap(),
            b"upper sentinel"
        );

        // A read-only mount failure routes an exclusive create to the overlay.
        let file = drive
            .create_new_file_with_root_attempt("\\FRESH.TMP", |_| {
                Err(std::io::Error::from_raw_os_error(if cfg!(windows) {
                    19
                } else {
                    30
                }))
            })
            .unwrap();
        drop(file);
        assert!(overlay.path().join("FRESH.TMP").is_file());
        assert!(!root.path().join("FRESH.TMP").exists());
        std::fs::write(overlay.path().join("fresh.tmp"), b"lowercase sentinel").unwrap();
        assert!(drive.create_new_file("\\fresh.tmp").is_err());
        assert_eq!(
            std::fs::read(overlay.path().join("fresh.tmp")).unwrap(),
            b"lowercase sentinel"
        );
        assert!(drive.create_new_file("\\FRESH.TMP").is_err());
        std::fs::create_dir(overlay.path().join("ONLY")).unwrap();
        drop(drive.create_new_file("\\ONLY\\CHILD.TMP").unwrap());
        assert!(overlay.path().join("ONLY/CHILD.TMP").is_file());
        assert!(!root.path().join("ONLY").exists());
        assert!(drive.create_new_file("\\..\\ESCAPE.TMP").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join("RACE.TMP"), root.path().join("ALIAS.TMP"))
                .unwrap();
            assert_eq!(
                drive
                    .create_new_file("\\ALIAS.TMP")
                    .err()
                    .unwrap()
                    .downcast_ref::<std::io::Error>()
                    .unwrap()
                    .kind(),
                std::io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                std::fs::read(root.path().join("RACE.TMP")).unwrap(),
                b"raced sentinel"
            );
        }
    }

    /// Every HostDrive mutation and lookup must refuse a stream name.
    #[test]
    fn hostdrive_rejects_alternate_data_stream_names() {
        let root = tempdir().unwrap();
        std::fs::write(root.path().join("HOST.EXE"), b"payload").unwrap();
        std::fs::create_dir(root.path().join("SUB")).unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());

        for name in [
            "HOST.EXE:Zone.Identifier",
            "SUB\\HOST.EXE:Zone.Identifier",
            "HOST.EXE:$DATA",
            ":Zone.Identifier",
        ] {
            assert!(
                drive.open_file(name, false, false, false).is_err(),
                "{name}: ADS read must be refused"
            );
            assert!(
                drive.open_file(name, true, true, false).is_err(),
                "{name}: ADS create must be refused"
            );
            assert!(
                drive.open_file(name, true, false, true).is_err(),
                "{name}: ADS truncate must be refused"
            );
            assert!(
                drive.remove_file(name).is_err(),
                "{name}: ADS delete must be refused"
            );
            assert!(
                drive.rename(name, "COPY.TXT").is_err(),
                "{name}: ADS rename (source) must be refused"
            );
            assert!(
                drive.rename("HOST.EXE", name).is_err(),
                "{name}: ADS rename (destination) must be refused"
            );
            assert!(
                drive.get_attributes(name).is_err(),
                "{name}: ADS stat must be refused"
            );
            assert!(
                drive.resolve_host_path(name).is_none(),
                "{name}: ADS must not resolve to a host path"
            );
        }

        // The carrier file itself is untouched by all of the above.
        assert_eq!(
            std::fs::read(root.path().join("HOST.EXE")).unwrap(),
            b"payload",
            "the ADS attempts must not have disturbed the real file"
        );
    }

    /// The rejection must not catch ordinary DOS names.
    #[test]
    fn hostdrive_still_accepts_ordinary_names_without_colons() {
        let root = tempdir().unwrap();
        std::fs::write(root.path().join("PLAIN.TXT"), b"ok").unwrap();
        std::fs::create_dir(root.path().join("SUB")).unwrap();
        std::fs::write(root.path().join("SUB").join("INNER.TXT"), b"ok").unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());

        assert!(drive.open_file("PLAIN.TXT", false, false, false).is_ok());
        assert!(drive
            .open_file("SUB\\INNER.TXT", false, false, false)
            .is_ok());
        assert!(drive.get_attributes("PLAIN.TXT").is_ok());
        assert!(drive.resolve_host_path("PLAIN.TXT").is_some());
    }

    #[test]
    fn test_hostdrive_blocks_symlink_escape() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();

        let link_path = root.path().join("link");
        create_symlink_dir(outside.path(), &link_path).expect("symlink or junction creation");
        std::fs::create_dir(outside.path().join("escape")).unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());
        let result = drive.create_dir("link\\escape");
        assert!(result.is_err());
    }

    #[test]
    fn test_hostdrive_resolve_host_path_blocks_symlink_escape() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();

        let link_path = root.path().join("link");
        create_symlink_dir(outside.path(), &link_path).expect("symlink or junction creation");
        std::fs::create_dir(outside.path().join("escape")).unwrap();

        let drive = HostDrive::new(root.path().to_path_buf());
        let resolved = drive.resolve_host_path("link\\escape");
        assert!(resolved.is_none());
    }

    #[test]
    fn test_hostdrive_create_rejects_dangling_final_symlink_escape() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("created-outside.txt");
        let link_path = root.path().join("escape.txt");

        if let Err(error) = create_symlink_file(&outside_file, &link_path) {
            #[cfg(windows)]
            if error.kind() == std::io::ErrorKind::PermissionDenied
                || error.raw_os_error() == Some(1314)
            {
                eprintln!("skipping: Windows symlink privilege unavailable: {error}");
                return;
            }
            panic!("file symlink creation failed: {error}");
        }

        assert!(!outside_file.exists(), "test target must begin absent");
        let mut drive = HostDrive::new(root.path().to_path_buf());
        let result = drive.open_file("escape.txt", true, true, false);

        assert!(
            result.is_err(),
            "HostDrive create must reject a dangling final symlink"
        );
        assert!(
            !outside_file.exists(),
            "HostDrive create must not materialize the symlink target outside its root"
        );
        assert!(
            std::fs::symlink_metadata(link_path).is_ok(),
            "the rejected symlink itself must remain intact"
        );
    }

    #[test]
    fn test_hostdrive_open_rejects_leaf_replaced_after_resolution() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("created-after-check.txt");
        let link_path = root.path().join("race.txt");
        let drive = HostDrive::new(root.path().to_path_buf());

        // Resolve while the leaf is absent, then install a link before the
        // actual open. This deterministically exercises the no-follow open
        // rather than the resolver's dangling-link check.
        let resolved = drive.resolve("race.txt").expect("missing create path");
        if let Err(error) = create_symlink_file(&outside_file, &link_path) {
            #[cfg(windows)]
            if error.kind() == std::io::ErrorKind::PermissionDenied
                || error.raw_os_error() == Some(1314)
            {
                eprintln!("skipping: Windows symlink privilege unavailable: {error}");
                return;
            }
            panic!("file symlink creation failed: {error}");
        }

        let result = HostDrive::open_host_file(&resolved, true, true, false);
        assert!(
            result.is_err(),
            "HostDrive open must reject a link installed after resolution"
        );
        assert!(
            !outside_file.exists(),
            "HostDrive open must not materialize a raced link target outside its root"
        );
    }

    #[test]
    fn test_hostdrive_create_through_generated_sfn_dir_component() {
        // Regression: launching from a host directory whose name is not
        // 8.3-clean makes the DOS CWD a generated short name (e.g. TMPNOT~1).
        // Creating a NEW file through that component must resolve the
        // directory chain and join the missing leaf (real DOS resolves the
        // path, then creates the leaf), not fail with path-not-found.
        let root = tempdir().unwrap();
        let long_dir = root.path().join("tmp.notEightThree");
        std::fs::create_dir(&long_dir).unwrap();

        let names = vec!["tmp.notEightThree".to_string()];
        let assignments: std::collections::HashMap<_, _> =
            crate::sfn::assign_short_names(&names).into_iter().collect();
        let short = assignments
            .get("tmp.notEightThree")
            .expect("generated short name")
            .clone();
        assert!(short.contains('~'), "expected a generated SFN, got {short}");

        let mut drive = HostDrive::new(root.path().to_path_buf());
        let dos_path = format!("{short}\\NEW.TXT");

        let resolved = drive
            .resolve_host_path(&dos_path)
            .expect("create-case resolution through SFN dir component");
        assert_eq!(resolved, long_dir.canonicalize().unwrap().join("NEW.TXT"));

        // End to end: the create itself must succeed and land in the real dir.
        let mut f = drive
            .open_file(&dos_path, true, true, true)
            .expect("AH=3Ch-style create through SFN dir component");
        use std::io::Write as _;
        f.write_all(b"ok").unwrap();
        drop(f);
        assert!(long_dir.join("NEW.TXT").exists());
    }

    #[test]
    fn test_parse_device_name_variants() {
        assert_eq!(parse_device_name("NUL"), Some("NUL"));
        assert_eq!(parse_device_name("nul.txt"), Some("NUL"));
        assert_eq!(parse_device_name("C:\\path\\CON"), Some("CON"));
        assert_eq!(parse_device_name("C:\\path\\CON:"), Some("CON"));
        assert_eq!(parse_device_name("aux"), Some("AUX"));
        assert_eq!(parse_device_name("COM1"), Some("AUX"));
        assert_eq!(parse_device_name("COM1:"), Some("AUX"));
        assert_eq!(parse_device_name("COM1.TXT"), Some("AUX"));
        assert_eq!(parse_device_name("lpt1.prn"), Some("PRN"));
        // Drive-relative device names with no separator (DOS "C:nul").
        assert_eq!(parse_device_name("C:nul"), Some("NUL"));
        assert_eq!(parse_device_name("c:CON"), Some("CON"));
        assert_eq!(parse_device_name("regular.txt"), None);
        assert_eq!(parse_device_name("C:regular.txt"), None);
    }

    #[test]
    fn test_parse_device_name_more_variants() {
        assert_eq!(parse_device_name("com1"), Some("AUX"));
        assert_eq!(parse_device_name("C:\\dev\\AUX.txt"), Some("AUX"));
        assert_eq!(parse_device_name("dir\\nul"), Some("NUL"));
        assert_eq!(parse_device_name("LPT1"), Some("PRN"));
        assert_eq!(parse_device_name("LPT4"), None);
        assert_eq!(parse_device_name("COM9"), None);
        assert_eq!(parse_device_name("CON."), Some("CON"));
    }

    /// Only COM1 and LPT1 exist in this machine. The other port names must NOT
    /// fold onto them — that would open `COM3` and silently route it to COM1.
    /// Real DOS resolves device names through its driver chain, so a port with
    /// no driver is an ordinary filename.
    #[test]
    fn test_parse_device_name_does_not_alias_absent_ports() {
        for absent in ["COM2", "com3", "COM4", "LPT2", "lpt3"] {
            assert_eq!(
                parse_device_name(absent),
                None,
                "{absent} must not alias onto port 1"
            );
        }
    }

    #[test]
    fn test_parse_device_name_empty_and_colon() {
        assert_eq!(parse_device_name(""), None);
        assert_eq!(parse_device_name(":"), None);
    }

    #[test]
    fn test_nul_device_io() {
        let mut dev = NulDevice;
        let mut buf = [0u8; 4];
        assert_eq!(dev.read(&mut buf).unwrap(), 0);
        assert_eq!(dev.write(b"test").unwrap(), 4);
        assert_eq!(dev.seek(std::io::SeekFrom::Start(10)).unwrap(), 0);
        dev.set_len(10).unwrap();
        dev.sync_all().unwrap();
    }

    #[test]
    fn test_hostdrive_open_file_roundtrip() {
        let root = tempdir().unwrap();
        let file_path = root.path().join("hello.txt");
        std::fs::write(&file_path, b"hello").unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());
        let mut file = drive.open_file("hello.txt", false, false, false).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "hello");
    }

    #[test]
    fn test_hostdrive_open_file_create_and_truncate() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());

        {
            let mut file = drive.open_file("data.txt", true, true, true).unwrap();
            file.write_all(b"hello").unwrap();
        }
        let meta = std::fs::metadata(root.path().join("data.txt")).unwrap();
        assert_eq!(meta.len(), 5);

        {
            let _file = drive.open_file("data.txt", true, false, true).unwrap();
        }
        let meta = std::fs::metadata(root.path().join("data.txt")).unwrap();
        assert_eq!(meta.len(), 0);
    }

    #[test]
    fn test_hostdrive_open_file_missing_without_create_fails() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());

        let result = drive.open_file("missing.txt", false, false, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_hostdrive_resolve_rejects_parent_escape() {
        let root = tempdir().unwrap();
        let drive = HostDrive::new(root.path().to_path_buf());
        let resolved = drive.resolve_host_path("..\\escape.txt");
        assert!(resolved.is_none());
    }

    #[test]
    fn test_hostdrive_resolve_rejects_parent_escape_on_create_path() {
        // Regression test: when the target file doesn't exist (create path),
        // ".." traversal beyond root must still be blocked. The file is
        // intentionally NOT created so canonicalization cannot be used.
        let root = tempdir().unwrap();
        // Create a subdirectory so "subdir\..\..\escape.txt" is a plausible path
        std::fs::create_dir(root.path().join("subdir")).unwrap();
        let drive = HostDrive::new(root.path().to_path_buf());

        // Single ".." beyond root — should be blocked
        let resolved = drive.resolve_host_path("..\\escape.txt");
        assert!(resolved.is_none(), "single .. escape must be blocked");

        // Double ".." beyond root via subdir — should be blocked
        let resolved = drive.resolve_host_path("subdir\\..\\..\\escape.txt");
        assert!(resolved.is_none(), "subdir/../.. escape must be blocked");

        // Triple ".." — should be blocked
        let resolved = drive.resolve_host_path("subdir\\..\\..\\..\\escape.txt");
        assert!(resolved.is_none(), "deep .. escape must be blocked");

        // Legitimate ".." that stays within root — should succeed
        let resolved = drive.resolve_host_path("subdir\\..\\newfile.txt");
        assert!(
            resolved.is_some(),
            "subdir/../newfile.txt should resolve within root"
        );
        let resolved = resolved.unwrap();
        let canonical_root = root.path().canonicalize().unwrap();
        #[cfg(windows)]
        let within_root = path_starts_with_case_insensitive(&resolved, &canonical_root);
        #[cfg(not(windows))]
        let within_root = resolved.starts_with(&canonical_root);
        assert!(within_root, "resolved path must be within root");
    }

    #[test]
    fn test_hostdrive_resolve_host_path_strips_leading_separators() {
        let root = tempdir().unwrap();
        let dir = root.path().join("dir");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("file.txt");
        std::fs::write(&file_path, b"data").unwrap();

        let drive = HostDrive::new(root.path().to_path_buf());
        let resolved = drive.resolve_host_path("\\dir\\file.txt").unwrap();
        assert_eq!(resolved, file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_hostdrive_resolve_host_path_allows_forward_slashes() {
        let root = tempdir().unwrap();
        let dir = root.path().join("dir").join("sub");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("file.txt");
        std::fs::write(&file_path, b"data").unwrap();

        let drive = HostDrive::new(root.path().to_path_buf());
        let resolved = drive.resolve_host_path("dir/sub/file.txt").unwrap();
        assert_eq!(resolved, file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_hostdrive_resolve_host_path_success() {
        let root = tempdir().unwrap();
        let path = root.path().join("file.txt");
        std::fs::write(&path, b"ok").unwrap();

        let drive = HostDrive::new(root.path().to_path_buf());
        let resolved = drive.resolve_host_path("file.txt").unwrap();
        assert_eq!(resolved, path.canonicalize().unwrap());
    }

    #[test]
    fn test_hostdrive_resolve_generated_sfn_directory_alias_without_prior_read_dir() {
        let root = tempdir().unwrap();
        let long_dir = root
            .path()
            .join("Sample_Project_With_A_Long_Directory_Name");
        std::fs::create_dir(&long_dir).unwrap();
        let exe_path = long_dir.join("tool.exe");
        std::fs::write(&exe_path, b"MZ").unwrap();

        let drive = HostDrive::new(root.path().to_path_buf());
        let resolved = drive
            .resolve_host_path(r"SAMPLE~1\TOOL.EXE")
            .expect("generated directory SFN should resolve");
        assert_eq!(resolved, exe_path.canonicalize().unwrap());
    }

    #[test]
    fn test_hostdrive_generated_sfn_reserves_real_short_name_sibling() {
        let root = tempdir().unwrap();
        let real_short_dir = root.path().join("SAMPLE~1");
        std::fs::create_dir(&real_short_dir).unwrap();
        let real_short_exe = real_short_dir.join("TOOL.EXE");
        std::fs::write(&real_short_exe, b"real").unwrap();

        let long_dir = root
            .path()
            .join("Sample_Project_With_A_Long_Directory_Name");
        std::fs::create_dir(&long_dir).unwrap();
        let long_exe = long_dir.join("TOOL.EXE");
        std::fs::write(&long_exe, b"long").unwrap();

        let drive = HostDrive::new(root.path().to_path_buf());
        let real_resolved = drive
            .resolve_host_path(r"SAMPLE~1\TOOL.EXE")
            .expect("real 8.3 sibling should keep its own name");
        assert_eq!(real_resolved, real_short_exe.canonicalize().unwrap());

        let long_resolved = drive
            .resolve_host_path(r"SAMPLE~2\TOOL.EXE")
            .expect("long sibling should use the next generated alias");
        assert_eq!(long_resolved, long_exe.canonicalize().unwrap());
    }

    #[test]
    fn test_hostdrive_resolve_host_path_nonexistent() {
        let root = tempdir().unwrap();
        let drive = HostDrive::new(root.path().to_path_buf());

        let resolved = drive
            .resolve_host_path("missing\\file.txt")
            .expect("resolved path");
        let expected_suffix = std::path::Path::new("missing").join("file.txt");
        assert!(resolved.ends_with(&expected_suffix));
        let root_path = root
            .path()
            .canonicalize()
            .unwrap_or_else(|_| root.path().to_path_buf());
        let root_str = root_path.to_string_lossy().to_lowercase();
        let resolved_str = resolved.to_string_lossy().to_lowercase();
        assert!(resolved_str.contains(&root_str));
    }

    #[test]
    fn test_hostdrive_resolve_host_path_nonexistent_matches_root_string() {
        let root = tempdir().unwrap();
        let drive = HostDrive::new(root.path().to_path_buf());

        let resolved = drive
            .resolve_host_path("missing\\file.txt")
            .expect("resolved path");
        let root_str = root
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        let resolved_str = resolved.to_string_lossy().to_lowercase();
        assert!(resolved_str.contains(&root_str));
    }

    #[cfg(not(windows))]
    #[test]
    fn test_hostdrive_case_insensitive_file_open() {
        // DOS is case-insensitive: opening "file.txt" should find "FILE.TXT"
        let root = tempdir().unwrap();
        let upper_path = root.path().join("TESTFILE.TXT");
        std::fs::write(&upper_path, b"hello").unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());

        // Opening with exact case should work
        let f = drive.open_file("TESTFILE.TXT", false, false, false);
        assert!(f.is_ok(), "exact case open should succeed");

        // Opening with lowercase should also work (case-insensitive)
        let f = drive.open_file("testfile.txt", false, false, false);
        assert!(
            f.is_ok(),
            "lowercase open should succeed via case-insensitive match"
        );

        // Opening with mixed case should also work
        let f = drive.open_file("TestFile.Txt", false, false, false);
        assert!(
            f.is_ok(),
            "mixed case open should succeed via case-insensitive match"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn test_hostdrive_case_insensitive_subdir() {
        // DOS paths with subdirectories should also resolve case-insensitively
        let root = tempdir().unwrap();
        let sub = root.path().join("MYDIR");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("DATA.DAT"), b"data").unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());

        // Opening with lowercase directory name
        let f = drive.open_file(r"mydir\data.dat", false, false, false);
        assert!(
            f.is_ok(),
            "case-insensitive subdir open should succeed: {:?}",
            f.err()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn test_hostdrive_case_insensitive_nonexistent_returns_error() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());

        // File truly doesn't exist - should still fail
        let f = drive.open_file("NOPE.TXT", false, false, false);
        assert!(
            f.is_err(),
            "non-existent file should fail even with case-insensitive matching"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_path_starts_with_case_insensitive_boundaries() {
        let root = std::path::Path::new(r"C:\Temp\Root");
        let child = std::path::Path::new(r"c:\temp\root\child");
        let sibling = std::path::Path::new(r"C:\Temp\Root2\child");
        let other_drive = std::path::Path::new(r"D:\Other");

        assert!(path_starts_with_case_insensitive(child, root));
        assert!(!path_starts_with_case_insensitive(sibling, root));
        assert!(!path_starts_with_case_insensitive(other_drive, root));
        assert!(path_starts_with_case_insensitive(root, root));
    }

    #[cfg(windows)]
    #[test]
    fn test_path_starts_with_case_insensitive_root_separator() {
        let root = std::path::Path::new(r"C:\");
        let child = std::path::Path::new(r"c:\Windows");

        assert!(path_starts_with_case_insensitive(child, root));
    }

    #[cfg(windows)]
    #[test]
    fn test_create_symlink_dir_existing_link_fails() {
        let root = tempdir().unwrap();
        let target = root.path().join("target");
        std::fs::create_dir(&target).unwrap();

        let link = root.path().join("link");
        std::fs::write(&link, b"data").unwrap();

        let result = create_symlink_dir(&target, &link);
        assert!(result.is_err());
    }

    #[test]
    fn test_hostdrive_resolve_host_path_empty_returns_root() {
        let root = tempdir().unwrap();
        let drive = HostDrive::new(root.path().to_path_buf());

        let resolved = drive.resolve_host_path("").unwrap();
        assert_eq!(resolved, root.path().canonicalize().unwrap());
    }

    #[test]
    fn test_hostdrive_resolve_host_path_dot_path() {
        let root = tempdir().unwrap();
        let file_path = root.path().join("file.txt");
        std::fs::write(&file_path, b"ok").unwrap();

        let drive = HostDrive::new(root.path().to_path_buf());
        let resolved = drive.resolve_host_path(".\\file.txt").unwrap();
        assert_eq!(resolved, file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_hostdrive_resolve_host_path_parent_within_root() {
        let root = tempdir().unwrap();
        let dir = root.path().join("dir");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = root.path().join("file.txt");
        std::fs::write(&file_path, b"ok").unwrap();

        let drive = HostDrive::new(root.path().to_path_buf());
        let resolved = drive.resolve_host_path("dir\\..\\file.txt").unwrap();
        assert_eq!(resolved, file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_hostdrive_resolve_host_path_trailing_separator() {
        let root = tempdir().unwrap();
        let dir = root.path().join("dir");
        std::fs::create_dir_all(&dir).unwrap();

        let drive = HostDrive::new(root.path().to_path_buf());
        let resolved = drive.resolve_host_path("dir\\").unwrap();
        assert_eq!(resolved, dir.canonicalize().unwrap());
    }

    #[test]
    fn test_hostdrive_create_rename_and_remove() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());

        drive.create_dir("dir").unwrap();
        std::fs::write(root.path().join("dir").join("file.txt"), b"data").unwrap();

        drive.rename("dir\\file.txt", "dir\\renamed.txt").unwrap();
        assert!(!root.path().join("dir").join("file.txt").exists());
        assert!(root.path().join("dir").join("renamed.txt").exists());

        drive.remove_file("dir\\renamed.txt").unwrap();
        assert!(!root.path().join("dir").join("renamed.txt").exists());

        drive.remove_dir("dir").unwrap();
        assert!(!root.path().join("dir").exists());
    }

    /// MDD-BUG-CRU-01397: a host drive stores hidden and system where the host
    /// can (Windows) and reads them back; set_attributes used to drop them and
    /// report success.
    #[cfg(windows)]
    #[test]
    fn hostdrive_set_attributes_round_trips_hidden_and_system() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        std::fs::write(root.path().join("SAVE.DAT"), b"abc").unwrap();

        let hidden_system = FileAttributes {
            hidden: true,
            system: true,
            archive: true,
            ..Default::default()
        };
        drive.set_attributes("SAVE.DAT", hidden_system).unwrap();
        let attrs = drive.get_attributes("SAVE.DAT").unwrap();
        assert!(attrs.hidden && attrs.system, "{attrs:?}");
        assert!(!attrs.read_only);
        let meta = drive.get_file_metadata("SAVE.DAT").unwrap();
        assert!(meta.attributes.hidden && meta.attributes.system);

        drive
            .set_attributes(
                "SAVE.DAT",
                FileAttributes {
                    archive: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let attrs = drive.get_attributes("SAVE.DAT").unwrap();
        assert!(!attrs.hidden && !attrs.system, "{attrs:?}");
    }

    /// MDD-BUG-CRU-01397: on Unix, clearing read-only restores only the owner's
    /// write bit (it used to be `chmod a+w`), and a hidden/system request the
    /// host cannot store fails without changing the file.
    #[cfg(unix)]
    #[test]
    fn hostdrive_set_attributes_restores_owner_write_only_and_refuses_hidden() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        let file = root.path().join("SAVE.DAT");
        std::fs::write(&file, b"abc").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let mode = || std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;

        let read_only = FileAttributes {
            read_only: true,
            ..Default::default()
        };
        drive.set_attributes("SAVE.DAT", read_only).unwrap();
        assert_eq!(mode(), 0o440);
        drive
            .set_attributes("SAVE.DAT", FileAttributes::default())
            .unwrap();
        assert_eq!(mode(), 0o640, "only the owner's write bit comes back");

        let hidden = FileAttributes {
            hidden: true,
            read_only: true,
            ..Default::default()
        };
        assert!(drive.set_attributes("SAVE.DAT", hidden).is_err());
        assert_eq!(mode(), 0o640, "a refused request changes nothing");
    }

    #[test]
    fn test_hostdrive_attributes_and_read_dir() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());

        drive.create_dir("dir").unwrap();
        drive.create_dir("dir\\subdir").unwrap();
        std::fs::write(root.path().join("dir").join("file.txt"), b"abc").unwrap();

        let attrs = drive.get_attributes("dir\\file.txt").unwrap();
        assert_eq!(attrs.size, 3);
        assert!(!attrs.directory);

        drive
            .set_attributes(
                "dir\\file.txt",
                FileAttributes {
                    read_only: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let attrs = drive.get_attributes("dir\\file.txt").unwrap();
        assert!(attrs.read_only);

        let entries = drive.read_dir("dir").unwrap();
        assert!(entries.iter().any(|(name, _)| name == "file.txt"));
        assert!(entries
            .iter()
            .any(|(name, attrs)| name == "subdir" && attrs.directory));

        drive
            .set_attributes(
                "dir\\file.txt",
                FileAttributes {
                    read_only: false,
                    ..Default::default()
                },
            )
            .unwrap();
        let attrs = drive.get_attributes("dir\\file.txt").unwrap();
        assert!(!attrs.read_only);
        #[cfg(not(windows))]
        {
            // Other hosts do not store an archive flag: clearing it leaves
            // the non-directory policy intact in every metadata path.
            assert!(attrs.archive);
            assert!(
                drive
                    .get_file_metadata("dir\\file.txt")
                    .unwrap()
                    .attributes
                    .archive
            );
            assert!(entries
                .iter()
                .any(|(name, attrs)| name == "file.txt" && attrs.archive));
            assert!(entries
                .iter()
                .any(|(name, attrs)| name == "subdir" && !attrs.archive));
        }
    }

    #[test]
    fn test_hostdrive_get_attributes_directory() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());

        drive.create_dir("dir").unwrap();
        let attrs = drive.get_attributes("dir").unwrap();
        assert!(attrs.directory);
        assert!(!attrs.archive);
    }

    /// A host filesystem's directory metadata length is not a DOS file size.
    /// In particular, APFS reports a nonzero length for directories, which
    /// leaked into File Manager's total-byte count on macOS.
    #[test]
    fn hostdrive_directory_sizes_are_zero_in_metadata_and_listings() {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        std::fs::create_dir(root.path().join("ROOTDIR")).unwrap();
        std::fs::write(root.path().join("ROOT.TXT"), b"root").unwrap();
        std::fs::create_dir(overlay.path().join("OVRDIR")).unwrap();
        std::fs::write(overlay.path().join("OVR.TXT"), b"upper").unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        for directory in ["ROOTDIR", "OVRDIR"] {
            let attrs = drive.get_attributes(directory).unwrap();
            assert!(attrs.directory);
            assert_eq!(attrs.size, 0, "{directory}");
            assert_eq!(
                drive.get_file_metadata(directory).unwrap().attributes.size,
                0,
                "{directory}"
            );
        }
        assert_eq!(drive.get_attributes("ROOT.TXT").unwrap().size, 4);
        assert_eq!(drive.get_attributes("OVR.TXT").unwrap().size, 5);

        let entries = drive.read_dir("\\").unwrap();
        let listed_size = |name: &str| {
            entries
                .iter()
                .find(|(entry, _)| entry.eq_ignore_ascii_case(name))
                .unwrap_or_else(|| panic!("missing {name} in {entries:?}"))
                .1
                .size
        };
        assert_eq!(listed_size("ROOTDIR"), 0);
        assert_eq!(listed_size("OVRDIR"), 0);
        assert_eq!(listed_size("ROOT.TXT"), 4);
        assert_eq!(listed_size("OVR.TXT"), 5);
    }

    #[test]
    fn test_hostdrive_remove_missing_file_and_dir() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());

        assert!(drive.remove_file("missing.txt").is_err());
        assert!(drive.remove_dir("missing").is_err());
    }

    #[test]
    fn test_hostdrive_rename_missing_source_fails() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());

        let result = drive.rename("missing.txt", "new.txt");
        assert!(result.is_err());
    }

    #[test]
    fn host_listing_sorts_directories_first_then_by_short_name() {
        // MDD-BUG-FLU-02174: the order a host hands back is not DOS's. Feed the
        // sort a listing in reverse and check the DOS model's DIRALPHABETICAL order
        // (`SortByDirName`): directories first, then the 8.3 name byte order,
        // a long name by its alias: "a z.txt" is AZ~1.TXT, after AB.TXT,
        // though its own spelling (a space, 0x20) would sort first.
        let attrs = |directory| FileAttributes {
            directory,
            ..FileAttributes::default()
        };
        let mut entries = vec![
            ("longname file.txt".to_string(), attrs(false)),
            ("a z.txt".to_string(), attrs(false)),
            ("b.txt".to_string(), attrs(false)),
            ("AB.TXT".to_string(), attrs(false)),
            ("A.TXT".to_string(), attrs(false)),
            ("ZDIR".to_string(), attrs(true)),
            ("adir".to_string(), attrs(true)),
        ];
        sort_host_listing(&mut entries, Path::new("/nonexistent-02174"));
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "adir",
                "ZDIR",
                "A.TXT",
                "AB.TXT",
                "a z.txt",
                "b.txt",
                "longname file.txt"
            ]
        );
    }

    #[test]
    fn test_hostdrive_read_dir_missing_reports_not_found() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());

        // MDD-BUG-CRUCIBLE-00645: this used to return Ok(empty), which a caller
        // cannot tell apart from a real directory with no entries. FindFirst
        // needs that distinction to answer 3 (path not found) rather than 2.
        let err = drive
            .read_dir("missing")
            .expect_err("a missing directory must report why");
        assert_eq!(
            err.downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn test_hostdrive_read_dir_on_file_reports_an_error() {
        let root = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        std::fs::write(root.path().join("file.txt"), b"data").unwrap();

        // MDD-BUG-CRUCIBLE-00645: enumerating a plain file is an error, not an
        // empty directory. The exact `ErrorKind` differs by platform
        // (`NotADirectory` on Linux, `Other`/`NotFound` on Windows), so only the
        // failure itself is pinned.
        drive
            .read_dir("file.txt")
            .expect_err("enumerating a file must not look like an empty directory");
    }

    #[test]
    fn test_hostdrive_get_disk_space() {
        let root = tempdir().unwrap();
        let drive = HostDrive::new(root.path().to_path_buf());
        let (total, free) = drive.get_disk_space().unwrap();
        assert!(total >= free);
    }

    // ================================================================
    // path_starts_with_case_insensitive (Windows-only)
    // ================================================================

    #[cfg(windows)]
    #[test]
    fn test_path_starts_with_case_insensitive_root_with_trailing_sep() {
        // Exercises the `root_ends_with_sep` true branch at line 104
        let root = std::path::Path::new(r"C:\Users\");
        let child = std::path::Path::new(r"c:\users\test");
        assert!(path_starts_with_case_insensitive(child, root));
    }

    // ---- SyntheticFile tests ----

    #[test]
    fn test_synthetic_file_read_returns_correct_data() {
        use std::io::Read;
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x42];
        let mut sf = SyntheticFile::new(data.clone());
        let mut buf = Vec::new();
        sf.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, data);
    }

    #[test]
    fn test_synthetic_file_read_empty() {
        use std::io::Read;
        let mut sf = SyntheticFile::new(Vec::new());
        let mut buf = Vec::new();
        let n = sf.read_to_end(&mut buf).unwrap();
        assert_eq!(n, 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_synthetic_file_read_partial() {
        use std::io::Read;
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut sf = SyntheticFile::new(data);
        let mut buf = [0u8; 3];
        let n = sf.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(buf, [1, 2, 3]);
        // Read remaining
        let n = sf.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(buf, [4, 5, 6]);
        let n = sf.read(&mut buf).unwrap();
        assert_eq!(n, 2);
        assert_eq!(buf[..2], [7, 8]);
    }

    #[test]
    fn test_synthetic_file_seek() {
        use std::io::{Read, Seek, SeekFrom};
        let data = vec![10, 20, 30, 40, 50];
        let mut sf = SyntheticFile::new(data);

        // Seek to offset 3
        let pos = sf.seek(SeekFrom::Start(3)).unwrap();
        assert_eq!(pos, 3);
        let mut buf = [0u8; 1];
        sf.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 40);

        // Seek from current (-2)
        let pos = sf.seek(SeekFrom::Current(-2)).unwrap();
        assert_eq!(pos, 2);
        sf.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 30);

        // Seek from end
        let pos = sf.seek(SeekFrom::End(-1)).unwrap();
        assert_eq!(pos, 4);
        sf.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 50);
    }

    #[test]
    fn test_synthetic_file_write_rejected() {
        use std::io::Write;
        let mut sf = SyntheticFile::new(vec![0; 4]);
        let result = sf.write(&[0xAA, 0xBB]);
        assert!(result.is_err(), "write to synthetic file should fail");
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn test_synthetic_file_set_len_rejected() {
        let mut sf = SyntheticFile::new(vec![1, 2, 3]);
        assert!(
            sf.set_len(5).is_err(),
            "set_len on synthetic file should fail"
        );
        assert!(
            sf.set_len(2).is_err(),
            "truncate on synthetic file should fail"
        );
    }

    #[test]
    fn test_synthetic_file_sync_all() {
        let mut sf = SyntheticFile::new(vec![1]);
        assert!(sf.sync_all().is_ok());
    }

    #[test]
    fn test_synthetic_file_position_tracking() {
        use std::io::{Read, Seek};
        let data = vec![0u8; 100];
        let mut sf = SyntheticFile::new(data);

        // Initial position is 0
        let pos = sf.stream_position().unwrap();
        assert_eq!(pos, 0);

        // After reading 10 bytes, position is 10
        let mut buf = [0u8; 10];
        sf.read_exact(&mut buf).unwrap();
        let pos = sf.stream_position().unwrap();
        assert_eq!(pos, 10);
    }

    // ============================================================
    // VirtualFile for std::fs::File tests
    // ============================================================

    #[test]
    fn test_host_file_set_len() {
        let root = tempdir().unwrap();
        let file_path = root.path().join("test.bin");
        std::fs::write(&file_path, b"hello world").unwrap();

        let mut file: Box<dyn VirtualFile> = Box::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&file_path)
                .unwrap(),
        );
        file.set_len(5).unwrap();
        drop(file);

        let contents = std::fs::read(&file_path).unwrap();
        assert_eq!(contents.len(), 5);
    }

    #[test]
    fn test_host_file_sync_all() {
        let root = tempdir().unwrap();
        let file_path = root.path().join("test.bin");
        std::fs::write(&file_path, b"data").unwrap();

        let mut file: Box<dyn VirtualFile> = Box::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&file_path)
                .unwrap(),
        );
        // sync_all should not error
        file.sync_all().unwrap();
    }

    // ============================================================
    // SyntheticFile tests
    // ============================================================

    #[test]
    fn test_synthetic_file_read_and_seek() {
        use std::io::{Read, Seek, SeekFrom};
        let mut sf = SyntheticFile::new(vec![1, 2, 3, 4, 5]);
        let mut buf = vec![0u8; 3];
        let n = sf.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, &[1, 2, 3]);

        // Seek back to start
        sf.seek(SeekFrom::Start(0)).unwrap();
        let mut all = Vec::new();
        sf.read_to_end(&mut all).unwrap();
        assert_eq!(&all, &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_synthetic_file_write_is_error() {
        use std::io::Write;
        let mut sf = SyntheticFile::new(vec![1, 2, 3]);
        let result = sf.write(b"abc");
        assert!(result.is_err()); // SyntheticFile is read-only
    }

    #[test]
    fn test_synthetic_file_set_len_is_error() {
        let mut sf = SyntheticFile::new(vec![0xAA; 10]);
        assert!(sf.set_len(5).is_err()); // Read-only
    }

    #[test]
    fn test_synthetic_file_sync_all_ok() {
        let mut sf = SyntheticFile::new(vec![0xAA; 10]);
        sf.sync_all().unwrap(); // sync_all succeeds
    }

    // ============================================================
    // HostDrive set_attributes / set_file_time / rename tests
    // ============================================================

    #[test]
    fn test_hostdrive_rename() {
        let root = tempdir().unwrap();
        std::fs::write(root.path().join("old.txt"), b"data").unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.rename("old.txt", "new.txt").unwrap();

        assert!(!root.path().join("old.txt").exists());
        assert!(root.path().join("new.txt").exists());
    }

    #[test]
    fn test_hostdrive_get_attributes_directory_info() {
        let root = tempdir().unwrap();
        std::fs::create_dir(root.path().join("subdir")).unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());
        let attrs = drive.get_attributes("subdir").unwrap();
        assert!(attrs.directory);
        assert!(!attrs.archive);
    }

    #[test]
    fn test_hostdrive_get_attributes_file_info() {
        let root = tempdir().unwrap();
        std::fs::write(root.path().join("file.txt"), b"data").unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());
        let attrs = drive.get_attributes("file.txt").unwrap();
        assert!(!attrs.directory);
        assert!(attrs.archive);
        assert_eq!(attrs.size, 4);
    }

    #[test]
    fn test_hostdrive_read_dir_entries() {
        let root = tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), b"aaa").unwrap();
        std::fs::write(root.path().join("b.txt"), b"bbb").unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());
        let entries = drive.read_dir("").unwrap();
        assert!(entries.len() >= 3);

        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
        assert!(names.contains(&"sub"));
    }

    #[test]
    fn test_hostdrive_normal_disk_space() {
        let root = tempdir().unwrap();
        let drive = HostDrive::new(root.path().to_path_buf());
        let (total, free) = drive.get_disk_space().unwrap();
        assert_eq!(total, 1024 * 1024 * 1024);
        assert_eq!(free, 512 * 1024 * 1024);
    }

    #[test]
    fn test_synthetic_file_flush() {
        use std::io::Write;
        let mut sf = SyntheticFile::new(vec![1, 2, 3]);
        sf.flush().unwrap();
    }

    #[test]
    fn test_synthetic_file_write_is_readonly() {
        use std::io::Write;
        let mut sf = SyntheticFile::new(vec![1, 2, 3]);
        let result = sf.write(b"data");
        assert!(result.is_err());
    }

    // ========== Coverage: SyntheticFile ==========

    #[test]
    fn test_synthetic_file_read() {
        let mut f = SyntheticFile::new(vec![1, 2, 3, 4, 5]);
        let mut buf = [0u8; 3];
        f.read_exact(&mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3]);
    }

    #[test]
    fn test_synthetic_file_write_fails() {
        let mut f = SyntheticFile::new(vec![1, 2, 3]);
        assert!(f.write(&[4, 5]).is_err());
    }

    #[test]
    fn test_synthetic_file_flush_succeeds() {
        let mut f = SyntheticFile::new(vec![]);
        f.flush().unwrap();
    }

    #[test]
    fn test_synthetic_file_seek_and_read() {
        let mut f = SyntheticFile::new(vec![10, 20, 30]);
        f.seek(SeekFrom::Start(2)).unwrap();
        let mut buf = [0u8; 1];
        f.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 30);
    }

    #[test]
    fn test_synthetic_file_set_len_fails() {
        let mut f = SyntheticFile::new(vec![]);
        assert!(f.set_len(100).is_err());
    }

    #[test]
    fn test_synthetic_file_sync_all_succeeds() {
        let mut f = SyntheticFile::new(vec![]);
        f.sync_all().unwrap();
    }

    // ========== Coverage: FileAttributes default ==========

    #[test]
    fn test_file_attributes_default_all_fields() {
        let attrs = FileAttributes::default();
        assert!(!attrs.read_only);
        assert!(!attrs.hidden);
        assert!(!attrs.system);
        assert!(!attrs.volume_label);
        assert!(!attrs.directory);
        assert!(!attrs.archive);
        assert_eq!(attrs.size, 0);
        assert_eq!(attrs.modified, SystemTime::UNIX_EPOCH);
    }

    // ========== Coverage: HostDrive set_file_time ==========

    #[test]
    fn test_hostdrive_set_file_time() {
        let root = tempdir().unwrap();
        let file_path = root.path().join("timed.txt");
        std::fs::write(&file_path, b"data").unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());
        let target_time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        drive.set_file_time("timed.txt", target_time).unwrap();

        let attrs = drive.get_attributes("timed.txt").unwrap();
        // Modified time should be approximately what we set (within 2 seconds for FAT resolution)
        let diff = attrs
            .modified
            .duration_since(target_time)
            .unwrap_or_default();
        assert!(diff.as_secs() < 3);
    }

    fn set_test_file_times(path: &Path, accessed: SystemTime, modified: SystemTime) {
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_accessed(accessed)
                    .set_modified(modified),
            )
            .unwrap();
    }

    #[test]
    fn hostdrive_set_file_time_preserves_access_time() {
        let root = tempdir().unwrap();
        let file_path = root.path().join("timed.txt");
        std::fs::write(&file_path, b"data").unwrap();
        let accessed =
            SystemTime::UNIX_EPOCH + std::time::Duration::new(1_600_000_000, 123_456_700);
        let modified =
            SystemTime::UNIX_EPOCH + std::time::Duration::new(1_600_000_010, 765_432_100);
        let target = SystemTime::UNIX_EPOCH + std::time::Duration::new(1_700_000_020, 333_333_300);
        set_test_file_times(&file_path, accessed, modified);

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_file_time("timed.txt", target).unwrap();

        let metadata = std::fs::metadata(file_path).unwrap();
        assert_eq!(metadata.modified().unwrap(), target);
        assert_eq!(metadata.accessed().unwrap(), accessed);
    }

    #[test]
    fn hostdrive_set_file_access_time_preserves_modification_time() {
        let root = tempdir().unwrap();
        let file_path = root.path().join("timed.txt");
        std::fs::write(&file_path, b"data").unwrap();
        let accessed =
            SystemTime::UNIX_EPOCH + std::time::Duration::new(1_600_000_000, 111_111_100);
        let modified =
            SystemTime::UNIX_EPOCH + std::time::Duration::new(1_600_000_010, 765_432_100);
        let target = SystemTime::UNIX_EPOCH + std::time::Duration::new(1_700_000_020, 333_333_300);
        set_test_file_times(&file_path, accessed, modified);

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_file_access_time("timed.txt", target).unwrap();

        let metadata = std::fs::metadata(file_path).unwrap();
        assert_eq!(metadata.accessed().unwrap(), target);
        assert_eq!(metadata.modified().unwrap(), modified);
    }

    #[test]
    fn hostdrive_set_file_time_supports_directories() {
        let root = tempdir().unwrap();
        std::fs::create_dir(root.path().join("subdir")).unwrap();
        let target = SystemTime::UNIX_EPOCH + std::time::Duration::new(1_700_000_020, 500_000_000);

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_file_time("subdir", target).unwrap();

        assert_eq!(
            std::fs::metadata(root.path().join("subdir"))
                .unwrap()
                .modified()
                .unwrap(),
            target
        );
    }

    #[cfg(unix)]
    #[test]
    fn hostdrive_set_file_time_rejects_owned_mode000_files() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().unwrap();
        let file_path = root.path().join("locked.txt");
        std::fs::write(&file_path, b"data").unwrap();
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let target = SystemTime::UNIX_EPOCH + std::time::Duration::new(1_700_000_020, 250_000_000);

        let mut drive = HostDrive::new(root.path().to_path_buf());
        let error = drive.set_file_time("locked.txt", target).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_ne!(
            std::fs::metadata(file_path).unwrap().modified().unwrap(),
            target
        );
    }

    #[cfg(unix)]
    #[test]
    fn hostdrive_set_file_access_time_rejects_owned_mode000_files() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().unwrap();
        let file_path = root.path().join("locked.txt");
        std::fs::write(&file_path, b"data").unwrap();
        let modified =
            SystemTime::UNIX_EPOCH + std::time::Duration::new(1_600_000_010, 765_432_100);
        set_test_file_times(
            &file_path,
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000),
            modified,
        );
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let target = SystemTime::UNIX_EPOCH + std::time::Duration::new(1_700_000_020, 250_000_000);

        let mut drive = HostDrive::new(root.path().to_path_buf());
        let error = drive
            .set_file_access_time("locked.txt", target)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::metadata(file_path).unwrap().modified().unwrap(),
            modified
        );
    }

    #[cfg(unix)]
    #[test]
    fn hostdrive_set_file_time_supports_owned_write_only_files() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().unwrap();
        let file_path = root.path().join("write-only.txt");
        std::fs::write(&file_path, b"data").unwrap();
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o200)).unwrap();
        let target = SystemTime::UNIX_EPOCH + std::time::Duration::new(1_700_000_020, 250_000_000);

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive
            .set_file_time("write-only.txt", target)
            .expect("the write-only fallback should permit updating owned files");

        assert_eq!(
            std::fs::metadata(file_path).unwrap().modified().unwrap(),
            target
        );
    }

    #[test]
    fn hostdrive_set_file_time_preserves_pre_epoch_fractional_time() {
        let root = tempdir().unwrap();
        let file_path = root.path().join("timed.txt");
        std::fs::write(&file_path, b"data").unwrap();
        let target = SystemTime::UNIX_EPOCH - std::time::Duration::new(1, 250_000_000);

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_file_time("timed.txt", target).unwrap();

        assert_eq!(
            std::fs::metadata(file_path).unwrap().modified().unwrap(),
            target
        );
    }

    #[test]
    fn hostdrive_timestamp_updates_follow_in_root_symlinks_and_reject_escapes() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let in_root_target = root.path().join("target.txt");
        let in_root_link = root.path().join("alias.txt");
        let outside_target = outside.path().join("outside.txt");
        let escape_link = root.path().join("escape.txt");
        std::fs::write(&in_root_target, b"inside").unwrap();
        std::fs::write(&outside_target, b"outside").unwrap();

        for (target, link) in [
            (&in_root_target, &in_root_link),
            (&outside_target, &escape_link),
        ] {
            if let Err(error) = create_symlink_file(target, link) {
                #[cfg(windows)]
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(1314)
                {
                    eprintln!("skipping: Windows symlink privilege unavailable: {error}");
                    return;
                }
                panic!("file symlink creation failed: {error}");
            }
        }

        let target_time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_020);
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_file_time("alias.txt", target_time).unwrap();
        assert_eq!(
            std::fs::metadata(in_root_target)
                .unwrap()
                .modified()
                .unwrap(),
            target_time
        );

        let outside_modified = std::fs::metadata(&outside_target)
            .unwrap()
            .modified()
            .unwrap();
        let error = drive
            .set_file_time("escape.txt", target_time)
            .expect_err("a symlink target outside the drive must be rejected");
        assert!(
            error.to_string().contains("path escapes root"),
            "unexpected error: {error}"
        );
        assert_eq!(
            std::fs::metadata(outside_target)
                .unwrap()
                .modified()
                .unwrap(),
            outside_modified
        );
    }

    #[test]
    fn hostdrive_timestamp_updates_propagate_missing_path_errors_without_creation() {
        let root = tempdir().unwrap();
        let missing_path = root.path().join("missing.txt");
        let mut drive = HostDrive::new(root.path().to_path_buf());
        let target = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_020);

        for error in [
            drive.set_file_time("missing.txt", target).unwrap_err(),
            drive
                .set_file_access_time("missing.txt", target)
                .unwrap_err(),
        ] {
            assert_eq!(
                error
                    .downcast_ref::<std::io::Error>()
                    .expect("the host I/O error should remain inspectable")
                    .kind(),
                std::io::ErrorKind::NotFound
            );
            assert!(
                !missing_path.exists(),
                "timestamp updates must not create files"
            );
        }
    }

    // ---- Write overlay tests ----

    #[test]
    fn test_write_overlay_explicit() {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        // Creating a file should work on the root
        let f = drive.open_file(r"\test.txt", true, true, false);
        assert!(f.is_ok());
        assert!(root.path().join("test.txt").exists());

        // A file created in the overlay should be found by open_file
        std::fs::write(overlay.path().join("overlay_file.txt"), "hello").unwrap();
        let f = drive.open_file(r"\overlay_file.txt", true, false, false);
        assert!(f.is_ok());
    }

    // -----------------------------------------------------------------------
    // MDD-BUG-CRUCIBLE-00646: the overlay is a coherent UPPER LAYER
    // -----------------------------------------------------------------------
    //
    // The overlay exists because the root refused a create, so for these files
    // the overlay copy is the ONLY copy. Every one of these operations used to
    // resolve against the root alone, which meant a file the guest had
    // successfully saved was invisible or unusable afterwards.

    /// Root drive plus overlay, with `name` present only in the overlay.
    fn overlay_only_drive(
        name: &str,
        contents: &str,
    ) -> (tempfile::TempDir, tempfile::TempDir, HostDrive) {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());
        std::fs::write(overlay.path().join(name), contents).unwrap();
        (root, overlay, drive)
    }

    #[test]
    fn overlay_file_reopens_read_only() {
        // The headline symptom: saved successfully, then unreadable. The old
        // overlay lookup was gated on `write || create`, so a plain read-only
        // open fell through to the root and failed with NotFound.
        let (_root, _overlay, mut drive) = overlay_only_drive("save.dat", "payload");

        let mut file = drive
            .open_file(r"\save.dat", false, false, false)
            .expect("a file saved into the overlay must be readable again");
        let mut buf = String::new();
        file.read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "payload", "the read must reach the overlay's contents");
    }

    #[test]
    fn overlay_file_appears_in_directory_enumeration() {
        // FindFirst is how DOS software discovers its own save files.
        let (root, _overlay, mut drive) = overlay_only_drive("save.dat", "payload");
        std::fs::write(root.path().join("root.dat"), "root").unwrap();

        let names: Vec<String> = drive
            .read_dir(r"\")
            .unwrap()
            .into_iter()
            .map(|(name, _)| name.to_uppercase())
            .collect();
        assert!(
            names.contains(&"SAVE.DAT".to_string()),
            "the overlay entry must be enumerated, got {names:?}"
        );
        assert!(
            names.contains(&"ROOT.DAT".to_string()),
            "merging must not drop root entries, got {names:?}"
        );
    }

    #[test]
    fn enumeration_reports_a_shadowed_name_once_with_the_overlay_version() {
        // A name in BOTH layers is one file to DOS, and the upper layer is the
        // current one. Reporting it twice would make FindNext hand the guest a
        // stale duplicate.
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());
        std::fs::write(root.path().join("both.dat"), "old").unwrap();
        std::fs::write(overlay.path().join("both.dat"), "much newer").unwrap();

        let entries = drive.read_dir(r"\").unwrap();
        let matching: Vec<_> = entries
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("both.dat"))
            .collect();
        assert_eq!(matching.len(), 1, "one DOS name is one entry: {entries:?}");
        assert_eq!(
            matching[0].1.size,
            "much newer".len() as u64,
            "the overlay version must win, not the stale root copy"
        );
    }

    #[test]
    fn enumeration_succeeds_for_a_directory_that_exists_only_in_the_overlay() {
        // The guest created a file under a path the root never held, so the
        // root's read_dir reports NotFound. Propagating that hides the file.
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());
        std::fs::create_dir_all(overlay.path().join("SAVES")).unwrap();
        std::fs::write(overlay.path().join("SAVES").join("slot1.sav"), "s").unwrap();

        let names: Vec<String> = drive
            .read_dir(r"\SAVES")
            .expect("an overlay-only directory must still enumerate")
            .into_iter()
            .map(|(name, _)| name.to_uppercase())
            .collect();
        assert!(names.contains(&"SLOT1.SAV".to_string()), "got {names:?}");
    }

    /// MDD-BUG-CRU-01399: MKDIR under a directory that exists only in the
    /// overlay creates it there, where file creation already goes, so a guest
    /// that checks MD's result before saving can save.
    #[test]
    fn mkdir_under_an_overlay_only_directory_lands_in_the_overlay() {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());
        std::fs::create_dir_all(overlay.path().join("PROJECT")).unwrap();

        drive
            .create_dir(r"\PROJECT\SAVE")
            .expect("MD under an overlay-only directory must succeed");
        assert!(overlay.path().join("PROJECT").join("SAVE").is_dir());
        assert!(
            !root.path().join("PROJECT").exists(),
            "the root is untouched"
        );
        drive
            .open_file(r"\PROJECT\SAVE\GAME.SAV", true, true, false)
            .expect("a save into the new directory succeeds");
    }

    /// The other CRU-01399 arm: a root that refuses MKDIR at the filesystem
    /// level (a read-only image mount) falls back to the overlay, while a
    /// file-level refusal such as access denied is still an error
    /// (MDD-BUG-CRU-01655).
    #[test]
    fn mkdir_on_a_read_only_root_falls_back_to_the_overlay() {
        #[cfg(windows)]
        let read_only_fs = || std::io::Error::from_raw_os_error(19); // ERROR_WRITE_PROTECT
        #[cfg(not(windows))]
        let read_only_fs = || std::io::Error::from(std::io::ErrorKind::ReadOnlyFilesystem);
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        drive
            .create_dir_with_root_attempt(r"\SAVES", |_| Err(read_only_fs()))
            .expect("MD on a read-only root must fall back to the overlay");
        assert!(overlay.path().join("SAVES").is_dir());
        assert!(!root.path().join("SAVES").exists(), "the root is untouched");

        let refused = drive.create_dir_with_root_attempt(r"\DENIED", |_| {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        assert!(
            refused.is_err(),
            "a file-level refusal is not an overlay fallback"
        );
        assert!(!overlay.path().join("DENIED").exists());
    }

    #[test]
    fn a_missing_directory_still_reports_not_found_with_an_overlay_configured() {
        // The overlay-only path above must not turn every bad path into an
        // empty success — that is the failure MDD-BUG-CRUCIBLE-00645 fixed.
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        assert!(
            drive.read_dir(r"\NOPE").is_err(),
            "a directory in neither layer is still an error"
        );
    }

    #[test]
    fn overlay_file_can_be_renamed_and_stays_in_the_overlay() {
        let (root, overlay, mut drive) = overlay_only_drive("old.dat", "payload");

        drive
            .rename(r"\old.dat", r"\new.dat")
            .expect("renaming an overlay-backed file must succeed");

        assert!(
            overlay.path().join("new.dat").exists(),
            "the renamed file belongs in the overlay: the root refused it once already"
        );
        assert!(!overlay.path().join("old.dat").exists());
        assert!(
            !root.path().join("new.dat").exists(),
            "it must not be moved onto the root"
        );
    }

    #[test]
    fn overlay_file_accepts_attribute_and_timestamp_updates() {
        let (_root, overlay, mut drive) = overlay_only_drive("meta.dat", "payload");

        drive
            .set_file_time(
                r"\meta.dat",
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000),
            )
            .expect("a timestamp update must find the overlay copy");
        let attrs = drive.get_attributes(r"\meta.dat").unwrap();
        assert_eq!(
            attrs.modified,
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000)
        );

        let mut read_only = attrs;
        read_only.read_only = true;
        drive
            .set_attributes(r"\meta.dat", read_only)
            .expect("an attribute update must find the overlay copy");
        assert!(
            std::fs::metadata(overlay.path().join("meta.dat"))
                .unwrap()
                .permissions()
                .readonly(),
            "the overlay copy is what should have been marked read-only"
        );
    }

    #[test]
    fn test_write_overlay_remove_file() {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        // Create a file in the overlay
        std::fs::write(overlay.path().join("to_delete.txt"), "data").unwrap();
        assert!(overlay.path().join("to_delete.txt").exists());

        // remove_file should delete from the overlay
        let r = drive.remove_file(r"\to_delete.txt");
        assert!(r.is_ok());
        assert!(!overlay.path().join("to_delete.txt").exists());
    }

    #[test]
    fn test_write_overlay_get_attributes() {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        // Create a file in the overlay
        std::fs::write(overlay.path().join("attrs.txt"), "test data").unwrap();

        // get_attributes should find the overlay file
        let attrs = drive.get_attributes(r"\attrs.txt");
        assert!(attrs.is_ok());
        let attrs = attrs.unwrap();
        assert!(!attrs.directory);
        assert_eq!(attrs.size, 9);
    }

    #[cfg(windows)]
    #[test]
    fn test_write_overlay_rejects_absolute_open_and_truncate() {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, b"preserve me").unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        let result = drive.open_file(victim.to_str().unwrap(), true, false, true);
        assert!(result.is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"preserve me");
    }

    #[cfg(windows)]
    #[test]
    fn test_write_overlay_rejects_absolute_remove() {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, b"preserve me").unwrap();

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        let result = drive.remove_file(victim.to_str().unwrap());
        assert!(result.is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"preserve me");
    }

    #[cfg(windows)]
    #[test]
    fn test_write_overlay_rejects_absolute_parent_creation() {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_parent = outside.path().join("created").join("parent");
        let guest_path = outside_parent.join("victim.txt");

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        assert!(drive
            .resolve_overlay(guest_path.to_str().unwrap())
            .is_none());
        assert!(!outside_parent.exists());
    }

    #[test]
    fn test_write_overlay_rejects_unsafe_components() {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        for path in [
            r"\..\victim",
            r"\safe\..\victim",
            r"C:\victim",
            r"\safe\C:\victim",
        ] {
            assert!(
                drive.resolve_overlay(path).is_none(),
                "unsafe overlay path was accepted: {path}"
            );
        }
    }

    /// MDD-BUG-CRU-01410: the overlay root is never adopted through a link.
    ///
    /// `create_dir_all` succeeds THROUGH an existing symlink-to-directory, and
    /// the old code then canonicalized its containment reference from whatever
    /// that resolved to - so every `within_overlay` check passed by
    /// construction and a pre-planted `<temp>/gp64_overlay` redirected
    /// every guest write outside the mount.
    ///
    /// `adopt_private_dir` is that boundary. It uses `symlink_metadata`, not
    /// `metadata`: the latter follows the link and reports the attacker's
    /// target as a perfectly good directory.
    #[test]
    fn overlay_root_refuses_a_planted_link_and_adopts_only_its_own_directory() {
        let home = tempdir().unwrap();
        let outside = tempdir().unwrap();

        // A directory we create ourselves is adopted, and adopting it again
        // is idempotent - the overlay is initialized once per session but the
        // root survives across runs.
        let ours = home.path().join("ours");
        assert!(HostDrive::adopt_private_dir(&ours).is_some());
        assert!(ours.is_dir());
        assert!(
            HostDrive::adopt_private_dir(&ours).is_some(),
            "re-adopting our own directory must succeed, or a second run of \
             the emulator can never write"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&ours).unwrap().permissions().mode() & 0o777,
                0o700,
                "the overlay root must not be readable by other accounts"
            );
        }

        // The defect itself: a link standing where our directory should be.
        let planted = home.path().join("planted");
        create_symlink_dir(outside.path(), &planted).expect("symlink or junction creation");
        assert!(
            HostDrive::adopt_private_dir(&planted).is_none(),
            "a link at the overlay root must be refused, not followed"
        );
        assert!(
            !outside.path().join("marker").exists(),
            "nothing may be created through the link"
        );

        // A plain file wearing the name is refused too.
        let file = home.path().join("file");
        std::fs::write(&file, b"not a directory").unwrap();
        assert!(HostDrive::adopt_private_dir(&file).is_none());
    }

    /// The second, non-adversarial half of MDD-BUG-CRU-01410: on a shared
    /// `/tmp` the first user's world-readable directory used to make every
    /// later user's overlay fail with "path escapes write overlay". It is now
    /// refused at the root, where the reason is visible.
    #[cfg(unix)]
    #[test]
    fn overlay_root_refuses_a_directory_open_to_other_accounts() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempdir().unwrap();
        let shared = home.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            HostDrive::adopt_private_dir(&shared).is_none(),
            "a group- or world-writable overlay root must be refused"
        );
    }

    /// `resolve_overlay_path` verifies the root rather than re-creating it.
    ///
    /// The old `create_dir_all(overlay)` here walked through a link planted at
    /// the root's name AFTER initialization - the same hole, one TOCTOU window
    /// later (MDD-BUG-CRU-01410).
    #[test]
    fn overlay_resolution_refuses_a_root_that_became_a_link() {
        let home = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let root = home.path().join("root");
        create_symlink_dir(outside.path(), &root).expect("symlink or junction creation");

        assert!(
            HostDrive::resolve_overlay_path(&root, r"\victim.txt").is_none(),
            "a root that is a link must not resolve, however it got there"
        );
        assert!(!outside.path().join("victim.txt").exists());
    }

    #[test]
    fn test_write_overlay_blocks_parent_symlink_escape() {
        let root = tempdir().unwrap();
        let overlay = tempdir().unwrap();
        let outside = tempdir().unwrap();
        create_symlink_dir(outside.path(), &overlay.path().join("link"))
            .expect("symlink or junction creation");

        let mut drive = HostDrive::new(root.path().to_path_buf());
        drive.set_write_overlay(overlay.path().to_path_buf());

        assert!(drive.resolve_overlay(r"\link\created\victim.txt").is_none());
        assert!(!outside.path().join("created").exists());
    }

    #[test]
    fn test_is_filesystem_level_error() {
        #[cfg(windows)]
        {
            let e = std::io::Error::from_raw_os_error(87);
            assert!(HostDrive::is_filesystem_level_error(&e));
            let e = std::io::Error::from_raw_os_error(19);
            assert!(HostDrive::is_filesystem_level_error(&e));
            let e = std::io::Error::from_raw_os_error(5);
            assert!(!HostDrive::is_filesystem_level_error(&e));
        }
        #[cfg(not(windows))]
        {
            // EROFS (30) should trigger overlay
            let e = std::io::Error::from_raw_os_error(30);
            assert!(HostDrive::is_filesystem_level_error(&e));
            // EACCES (13) should NOT trigger overlay
            let e = std::io::Error::from_raw_os_error(13);
            assert!(!HostDrive::is_filesystem_level_error(&e));
        }
        // ENOENT / file not found should NOT trigger overlay (both platforms)
        let e = std::io::Error::from_raw_os_error(2);
        assert!(!HostDrive::is_filesystem_level_error(&e));
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod synthetic_payload_sharing_tests {
    //! MDD-BUG-CRUCIBLE-00380: a synthetic file is read-only, so every handle
    //! onto the same path shares one allocation and owns only its cursor.
    //! Cloning the payload per open multiplied host memory by the handle count,

    use super::{SyntheticFile, SyntheticPayload};
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::Arc;

    /// Two handles, one allocation, independent positions.
    #[test]
    fn synthetic_handles_share_one_allocation_with_independent_cursors() {
        let payload: Arc<[u8]> = Arc::from(vec![10u8, 20, 30, 40, 50]);
        assert_eq!(Arc::strong_count(&payload), 1);

        let mut first = SyntheticFile::new(payload.clone());
        let mut second = SyntheticFile::new(payload.clone());

        // The refcount is the proof of sharing: three owners of ONE buffer.
        // A per-open copy would leave this at 1 and allocate twice.
        assert_eq!(
            Arc::strong_count(&payload),
            3,
            "each open must share the payload, not copy it"
        );
        for handle in [&first, &second] {
            match handle.payload() {
                SyntheticPayload::Shared(bytes) => assert!(
                    Arc::ptr_eq(bytes, &payload),
                    "the handle must point at the registered allocation"
                ),
                other => panic!("expected a shared payload, got {other:?}"),
            }
        }

        let mut buf = [0u8; 2];
        first
            .read_exact(&mut buf)
            .expect("read from the first handle");
        assert_eq!(buf, [10, 20]);

        // The second handle must be untouched by the first handle's read.
        let mut other = [0u8; 3];
        second.read_exact(&mut other).expect("read from the second");
        assert_eq!(other, [10, 20, 30], "cursors must be per handle");

        // And seeking one must not move the other.
        first
            .seek(SeekFrom::Start(4))
            .expect("seek the first handle");
        let mut tail = [0u8; 1];
        first.read_exact(&mut tail).expect("read the tail");
        assert_eq!(tail, [50]);
        let mut after = [0u8; 1];
        second
            .read_exact(&mut after)
            .expect("read the second again");
        assert_eq!(after, [40], "the second cursor stayed where it was");
    }

    #[test]
    fn synthetic_static_payload_is_borrowed_not_copied() {
        static PATCH: &[u8] = &[1, 2, 3, 4];
        let file = SyntheticFile::new(PATCH);

        match file.payload() {
            SyntheticPayload::Static(bytes) => assert!(
                std::ptr::eq(*bytes, PATCH),
                "a static asset must be borrowed in place"
            ),
            other => panic!("expected a static payload, got {other:?}"),
        }
    }

    /// Seeking past the end reads nothing and a negative seek errors — the
    /// `io::Cursor` contract the hand-rolled cursor replaced.
    #[test]
    fn synthetic_seek_matches_the_cursor_contract() {
        let mut file = SyntheticFile::new(vec![1u8, 2, 3]);

        assert_eq!(file.seek(SeekFrom::Start(99)).expect("past-end seek"), 99);
        let mut buf = [0u8; 4];
        assert_eq!(file.read(&mut buf).expect("read past end"), 0);

        assert_eq!(file.seek(SeekFrom::End(-1)).expect("seek from end"), 2);
        assert_eq!(file.read(&mut buf).expect("read the last byte"), 1);
        assert_eq!(buf[0], 3);

        file.seek(SeekFrom::Start(0)).expect("rewind");
        assert!(
            file.seek(SeekFrom::Current(-1)).is_err(),
            "a negative position must be rejected"
        );
    }

    /// EOF reads, including empty buffers, must leave a past-end cursor alone.
    #[test]
    fn synthetic_file_eof_reads_preserve_cursor_position() {
        for payload in [vec![1u8, 2, 3], Vec::new()] {
            let mut file = SyntheticFile::new(payload.clone());
            let mut oracle = std::io::Cursor::new(payload);

            assert_eq!(
                file.seek(SeekFrom::Start(99)).unwrap(),
                oracle.seek(SeekFrom::Start(99)).unwrap()
            );

            for _ in 0..2 {
                let mut actual = [0xAA; 2];
                let mut expected = [0xAA; 2];
                assert_eq!(
                    file.read(&mut actual).unwrap(),
                    oracle.read(&mut expected).unwrap()
                );
                assert_eq!(actual, expected);
                assert_eq!(
                    file.stream_position().unwrap(),
                    oracle.stream_position().unwrap(),
                    "EOF read must preserve the past-end cursor position"
                );
            }

            let mut actual = [];
            let mut expected = [];
            assert_eq!(
                file.read(&mut actual).unwrap(),
                oracle.read(&mut expected).unwrap()
            );
            assert_eq!(
                file.stream_position().unwrap(),
                oracle.stream_position().unwrap(),
                "zero-buffer EOF read must preserve the past-end cursor position"
            );

            assert_eq!(file.seek(SeekFrom::Start(u64::MAX)).unwrap(), u64::MAX);
            assert_eq!(oracle.seek(SeekFrom::Start(u64::MAX)).unwrap(), u64::MAX);
            let mut actual = [0u8; 1];
            let mut expected = [0u8; 1];
            assert_eq!(
                file.read(&mut actual).unwrap(),
                oracle.read(&mut expected).unwrap()
            );
            assert_eq!(file.stream_position().unwrap(), u64::MAX);
            assert_eq!(oracle.stream_position().unwrap(), u64::MAX);
            assert_eq!(
                file.stream_position().unwrap(),
                u64::MAX,
                "zero-offset position query preserves the maximum cursor"
            );
            assert_eq!(oracle.stream_position().unwrap(), u64::MAX);
            assert!(file.seek(SeekFrom::Current(1)).is_err());
            assert!(oracle.seek(SeekFrom::Current(1)).is_err());
            assert_eq!(file.stream_position().unwrap(), u64::MAX);
            assert_eq!(oracle.stream_position().unwrap(), u64::MAX);

            if !file.payload().is_empty() {
                assert_eq!(file.seek(SeekFrom::Start(0)).unwrap(), 0);
                assert_eq!(oracle.seek(SeekFrom::Start(0)).unwrap(), 0);
                let mut actual = [0u8; 1];
                let mut expected = [0u8; 1];
                assert_eq!(file.read(&mut actual).unwrap(), 1);
                assert_eq!(oracle.read(&mut expected).unwrap(), 1);
                assert_eq!(actual, expected);
                assert_eq!(file.stream_position().unwrap(), 1);
                assert_eq!(oracle.stream_position().unwrap(), 1);
            }
        }
    }
}
