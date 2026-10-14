//! DOS command interpreter-style standard-handle redirection (`<`, `>`, `>>`).
//!
//! DOS command interpreter redirects a command by saving its own stdin/stdout
//! with a DUP, opening the target, forcing it onto handle 0 or 1 with a
//! force-DUP, and EXECing the child, which inherits the handle table. When
//! the child exits, DOS command interpreter force-DUPs the saved handle back. The file
//! table is this emulator's single JFT, so pointing the shell's handle at the
//! file is exactly what the child inherits.

use super::*;
use crate::shell::{RedirectMode, SavedStdBinding, StdHandleRedirect};

impl Dos {
    /// Point standard handle `std_handle` (0 or 1) at `target`, opened per
    /// `mode` against the current drive and directory, and return what it
    /// replaced. `Err` carries the DOS error; the handle is then unchanged.
    pub fn redirect_std_handle(
        &mut self,
        std_handle: u16,
        target: &str,
        mode: RedirectMode,
    ) -> Result<StdHandleRedirect, u16> {
        // Reserve the old file binding before opening a target that may truncate
        // or create a file. Release that reservation if opening fails.
        let saved = if self.file_table.is_valid(std_handle) {
            match self.file_table.duplicate(std_handle, self.current_psp) {
                Some(dup) => SavedStdBinding::Dup(dup),
                None => return Err(4), // Too many open files
            }
        } else {
            SavedStdBinding::Alias(self.file_table.std_handle_aliases[std_handle as usize])
        };
        let opened = match self.open_redirect_target(target, mode) {
            Ok(opened) => opened,
            Err(error) => {
                if let SavedStdBinding::Dup(dup) = saved {
                    self.file_table.free(dup);
                }
                return Err(error);
            }
        };
        self.bind_std_handle(std_handle, opened);
        Ok(StdHandleRedirect { std_handle, saved })
    }

    /// Undo a [`redirect_std_handle`](Self::redirect_std_handle): close the
    /// redirected file (draining it) and give the handle back its old binding.
    pub fn restore_std_handle(&mut self, redirect: StdHandleRedirect) {
        let std_handle = redirect.std_handle;
        match redirect.saved {
            SavedStdBinding::Dup(dup) => self.bind_std_handle(std_handle, dup),
            SavedStdBinding::Alias(alias) => {
                if self.file_table.is_valid(std_handle) {
                    self.file_table.free(std_handle);
                }
                self.file_table.std_handle_aliases[std_handle as usize] = alias;
            }
        }
    }

    /// Force-DUP `source` onto `std_handle`, then close `source` so only the
    /// standard handle holds it. `0x00FE` (CON) returns the handle to the
    /// console device.
    fn bind_std_handle(&mut self, std_handle: u16, source: u16) {
        if self.file_table.is_valid(std_handle) {
            self.file_table.free(std_handle);
        }
        self.file_table.std_handle_aliases[std_handle as usize] = None;
        if source == 0x00FE {
            return;
        }
        self.file_table
            .force_duplicate(source, std_handle, self.current_psp);
        self.file_table.free(source);
    }

    /// Open a redirection target the way DOS command interpreter does: a device name
    /// reaches the device, `<` opens an existing file for reading, `>`
    /// creates (truncates) and `>>` opens-or-creates and seeks to the end.
    fn open_redirect_target(&mut self, target: &str, mode: RedirectMode) -> Result<u16, u16> {
        let access = if mode == RedirectMode::Input {
            crate::files::ACCESS_READ
        } else {
            crate::files::ACCESS_READWRITE
        };
        if let Some(device) = crate::filesystem::parse_device_name(target) {
            return self.open_named_device(device, crate::files::SHARE_COMPAT, access);
        }

        let resolved = self.path_resolver.resolve(target);
        if !self.drives.contains_key(&resolved.drive) {
            return Err(15); // Invalid drive
        }
        let identity = self.resolved_file_identity(resolved.drive, &resolved.path);
        if self.file_table.check_sharing_violation_for_identity(
            &identity,
            crate::files::SHARE_COMPAT,
            access,
            self.current_psp,
        ) {
            return Err(32); // Sharing violation
        }
        if !self.file_table.has_open_capacity() {
            return Err(4); // Too many open files
        }

        let (write, create, truncate) = match mode {
            RedirectMode::Input => (false, false, false),
            RedirectMode::Truncate => (true, true, true),
            RedirectMode::Append => (true, true, false),
        };
        let mut file = self
            .drives
            .get_mut(&resolved.drive)
            .expect("drive presence checked above")
            .open_file(&resolved.path, write, create, truncate)
            .map_err(|_| {
                if mode == RedirectMode::Input {
                    2u16
                } else {
                    5u16
                }
            })?;
        let position = if mode == RedirectMode::Append {
            file.seek(SeekFrom::End(0)).map_err(|_| 5u16)?
        } else {
            0
        };
        let full_path = format!("{}:{}", (resolved.drive + b'A') as char, resolved.path);
        let handle = self
            .file_table
            .allocate_with_sharing_identity(
                file,
                full_path,
                identity,
                self.current_psp,
                crate::files::SHARE_COMPAT,
                access,
            )
            .ok_or(4u16)?;
        if let Some(entry) = self.file_table.get_sft_mut(handle) {
            entry.position = position;
        }
        Ok(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    const STDOUT: u16 = 1;
    const STDOUT_BYTES: &[u8] = b"saved stdout bytes";
    const FIRST_FILE_HANDLE: u16 = 5;
    const DEFAULT_HANDLE_LIMIT: u16 = 20;
    const DYNAMIC_HANDLE_COUNT: usize = (DEFAULT_HANDLE_LIMIT - FIRST_FILE_HANDLE) as usize;
    type HandleState = (Option<usize>, Option<(String, u64, u16)>, Option<u16>);

    struct Fixture {
        dos: Dos,
        root: TempDir,
        stdout_sft: usize,
    }

    impl Fixture {
        fn new(free_slots: usize) -> Self {
            assert!(free_slots <= DYNAMIC_HANDLE_COUNT);
            let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
            std::fs::create_dir_all(&scratch).expect("create test scratch directory");
            let root = tempfile::Builder::new()
                .prefix(&format!("redirect-{}-", std::process::id()))
                .tempdir_in(&scratch)
                .expect("create unique redirect fixture directory");
            std::fs::write(root.path().join("STDOUT.TXT"), STDOUT_BYTES)
                .expect("write synthetic stdout file");
            std::fs::write(root.path().join("FILL.DAT"), b"fill")
                .expect("write synthetic handle-filler file");

            let mut dos = Dos::new();
            dos.drives.insert(
                2,
                Box::new(crate::filesystem::HostDrive::new(root.path().to_path_buf())),
            );
            let stdout_handle = dos
                .open_redirect_target("STDOUT.TXT", RedirectMode::Append)
                .expect("open synthetic stdout file");
            let stdout_sft = dos
                .file_table
                .get_sft_index(stdout_handle)
                .expect("stdout file has an SFT entry");
            dos.bind_std_handle(STDOUT, stdout_handle);

            for _ in 0..(DYNAMIC_HANDLE_COUNT - free_slots) {
                dos.open_redirect_target("FILL.DAT", RedirectMode::Input)
                    .expect("open synthetic handle-filler file");
            }

            let fixture = Self {
                dos,
                root,
                stdout_sft,
            };
            assert_eq!(
                fixture.dos.file_table.get_sft_index(STDOUT),
                Some(stdout_sft)
            );
            assert_eq!(
                fixture.dos.file_table.get_sft_ref_count(stdout_sft),
                Some(1)
            );
            let expected_free =
                (free_slots > 0).then_some(DEFAULT_HANDLE_LIMIT - free_slots as u16);
            assert_eq!(
                fixture.dos.file_table.next_free_handle(),
                expected_free,
                "fixture must leave exactly {free_slots} dynamic JFT slots free"
            );
            fixture
        }

        fn target_path(&self, name: &str) -> std::path::PathBuf {
            self.root.path().join(name)
        }

        fn handle_state(&self) -> Vec<HandleState> {
            (0..self.dos.file_table.handle_limit())
                .map(|handle| {
                    let entry = self.dos.file_table.get_sft(handle);
                    (
                        self.dos.file_table.get_sft_index(handle),
                        entry.map(|entry| (entry.path.clone(), entry.position, entry.ref_count)),
                        self.dos.file_table.std_handle_aliases[handle as usize],
                    )
                })
                .collect()
        }

        fn assert_capacity_unchanged(&self, free_slots: usize) {
            let expected = DEFAULT_HANDLE_LIMIT - free_slots as u16;
            assert_eq!(self.dos.file_table.handle_limit(), DEFAULT_HANDLE_LIMIT);
            assert_eq!(self.dos.file_table.next_free_handle(), Some(expected));
            assert!(self.dos.file_table.has_open_capacity());
        }

        fn assert_last_slot_failure(&mut self, target: &str, original: Option<&[u8]>) {
            let path = self.target_path(target);
            let before = self.handle_state();
            assert_eq!(
                self.dos
                    .redirect_std_handle(STDOUT, target, RedirectMode::Truncate)
                    .unwrap_err(),
                4
            );
            match original {
                Some(bytes) => assert_eq!(std::fs::read(path).unwrap(), bytes),
                None => assert!(!path.exists(), "failed redirection created its target"),
            }
            assert_eq!(
                std::fs::read(self.target_path("STDOUT.TXT")).unwrap(),
                STDOUT_BYTES
            );
            assert_eq!(
                self.handle_state(),
                before,
                "failed redirect changed JFT/SFT state"
            );
            assert_eq!(
                self.dos.file_table.get_sft_index(STDOUT),
                Some(self.stdout_sft)
            );
            assert_eq!(
                self.dos.file_table.get_sft_ref_count(self.stdout_sft),
                Some(1)
            );
            self.assert_capacity_unchanged(1);
        }

        fn assert_failed_open_rollback(
            fixture: &mut Fixture,
            std_handle: u16,
            target: &str,
            mode: RedirectMode,
            expected_error: u16,
            free_slots: usize,
        ) {
            let before = fixture.handle_state();
            assert_eq!(
                fixture
                    .dos
                    .redirect_std_handle(std_handle, target, mode)
                    .unwrap_err(),
                expected_error
            );
            assert_eq!(
                fixture.handle_state(),
                before,
                "failed open changed JFT/SFT state"
            );
            assert_eq!(
                fixture.dos.file_table.get_sft_ref_count(fixture.stdout_sft),
                Some(1)
            );
            fixture.assert_capacity_unchanged(free_slots);
        }
    }

    #[test]
    fn flu00016_failed_truncate_with_one_free_slot_preserves_target_and_stdout() {
        let mut fixture = Fixture::new(1);
        let sentinel = b"target bytes must survive";
        std::fs::write(fixture.target_path("EXISTING.TXT"), sentinel).unwrap();
        fixture.assert_last_slot_failure("EXISTING.TXT", Some(sentinel));
    }

    #[test]
    fn flu00016_failed_create_with_one_free_slot_does_not_create_target() {
        let mut fixture = Fixture::new(1);
        fixture.assert_last_slot_failure("ABSENT.TXT", None);
    }

    #[test]
    fn flu00016_failed_open_paths_release_saved_stdout_duplicate() {
        let mut missing = Fixture::new(2);
        Fixture::assert_failed_open_rollback(
            &mut missing,
            STDOUT,
            "MISSING.TXT",
            RedirectMode::Input,
            2,
            2,
        );

        let mut invalid_drive = Fixture::new(2);
        Fixture::assert_failed_open_rollback(
            &mut invalid_drive,
            STDOUT,
            r"Z:\MISSING.TXT",
            RedirectMode::Input,
            15,
            2,
        );

        let mut sharing = Fixture::new(2);
        std::fs::write(sharing.target_path("BLOCKED.TXT"), b"sharing sentinel").unwrap();
        sharing
            .dos
            .open_redirect_target("BLOCKED.TXT", RedirectMode::Append)
            .expect("open target under the original PSP");
        sharing.dos.current_psp = 0x2000;
        Fixture::assert_failed_open_rollback(
            &mut sharing,
            STDOUT,
            "BLOCKED.TXT",
            RedirectMode::Append,
            32,
            1,
        );
        assert_eq!(
            std::fs::read(sharing.target_path("BLOCKED.TXT")).unwrap(),
            b"sharing sentinel"
        );
    }

    #[test]
    fn flu00016_successful_file_modes_and_device_aliases_restore() {
        let mut fixture = Fixture::new(3);
        std::fs::write(fixture.target_path("OUTPUT.TXT"), b"old output").unwrap();
        let output_redirect = fixture
            .dos
            .redirect_std_handle(STDOUT, "OUTPUT.TXT", RedirectMode::Truncate)
            .expect("truncate redirection should succeed with two slots");
        assert!(fixture
            .dos
            .file_table
            .get_sft(STDOUT)
            .unwrap()
            .path
            .ends_with("OUTPUT.TXT"));
        assert_eq!(
            std::fs::read(fixture.target_path("OUTPUT.TXT")).unwrap(),
            b""
        );
        fixture.dos.restore_std_handle(output_redirect);
        assert_eq!(
            fixture.dos.file_table.get_sft_index(STDOUT),
            Some(fixture.stdout_sft)
        );
        assert_eq!(
            fixture.dos.file_table.get_sft_ref_count(fixture.stdout_sft),
            Some(1)
        );
        assert_eq!(
            std::fs::read(fixture.target_path("STDOUT.TXT")).unwrap(),
            STDOUT_BYTES
        );

        let input_path = fixture.target_path("INPUT.TXT");
        std::fs::write(&input_path, b"input bytes").unwrap();
        let input_redirect = fixture
            .dos
            .redirect_std_handle(0, "INPUT.TXT", RedirectMode::Input)
            .expect("input redirection should open an existing file");
        let input_sft = fixture.dos.file_table.get_sft(0).unwrap();
        assert!(input_sft.path.ends_with("INPUT.TXT"));
        assert_eq!(input_sft.access_mode & 0x07, crate::files::ACCESS_READ);
        fixture.dos.restore_std_handle(input_redirect);

        let append_path = fixture.target_path("APPEND.TXT");
        std::fs::write(&append_path, b"existing bytes").unwrap();
        let append_redirect = fixture
            .dos
            .redirect_std_handle(STDOUT, "APPEND.TXT", RedirectMode::Append)
            .expect("append redirection should open or create its target");
        let append_sft = fixture.dos.file_table.get_sft(STDOUT).unwrap();
        assert!(append_sft.path.ends_with("APPEND.TXT"));
        assert_eq!(
            append_sft.access_mode & 0x07,
            crate::files::ACCESS_READWRITE
        );
        assert_eq!(append_sft.position, b"existing bytes".len() as u64);
        fixture.dos.restore_std_handle(append_redirect);
        assert_eq!(
            fixture.dos.file_table.get_sft_index(STDOUT),
            Some(fixture.stdout_sft)
        );
        assert_eq!(
            fixture.dos.file_table.get_sft_ref_count(fixture.stdout_sft),
            Some(1)
        );
        fixture.assert_capacity_unchanged(3);

        let mut fixture = Fixture::new(0);
        assert!(fixture.dos.file_table.free(STDOUT));
        fixture.dos.file_table.std_handle_aliases[STDOUT as usize] = Some(0);
        let before = fixture.handle_state();

        let console_redirect = fixture
            .dos
            .redirect_std_handle(STDOUT, "CON", RedirectMode::Truncate)
            .expect("CON redirection should not need an ordinary handle");
        assert!(!fixture.dos.file_table.is_valid(STDOUT));
        assert_eq!(
            fixture.dos.file_table.std_handle_aliases[STDOUT as usize],
            None
        );
        fixture.dos.restore_std_handle(console_redirect);
        assert_eq!(
            fixture.dos.file_table.std_handle_aliases[STDOUT as usize],
            Some(0)
        );
        assert_eq!(
            fixture.handle_state(),
            before,
            "CON restore changed the alias-only binding"
        );

        let mut nul_fixture = Fixture::new(1);
        assert!(nul_fixture.dos.file_table.free(STDOUT));
        nul_fixture.dos.file_table.std_handle_aliases[STDOUT as usize] = Some(0);
        let nul_redirect = nul_fixture
            .dos
            .redirect_std_handle(STDOUT, "NUL", RedirectMode::Truncate)
            .expect("NUL redirection should use a normal file-table handle");
        assert_eq!(
            nul_fixture.dos.file_table.get_sft(STDOUT).unwrap().path,
            "NUL"
        );
        assert_eq!(
            nul_fixture.dos.file_table.std_handle_aliases[STDOUT as usize],
            None
        );
        nul_fixture.dos.restore_std_handle(nul_redirect);
        assert_eq!(
            nul_fixture.dos.file_table.std_handle_aliases[STDOUT as usize],
            Some(0)
        );
        nul_fixture.assert_capacity_unchanged(1);
    }
}
