//! Session-owned host section for the flat-memory shared arena (HLD §5.2, U3).
//!
//! A section is mapped at the same physical offset in every process's RAM
//! reservation. Host commitment is retained until the last section owner is
//! dropped. Guest decommit/reuse and its zero-fill rule belong to U4.

use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::ram_backing::PRIVATE_WINDOW_BYTES;

/// Guest physical start of the shared data pages.
pub const ARENA_PHYS_START: usize = PRIVATE_WINDOW_BYTES;
/// Guest physical start of the 256 shared page tables.
pub const ARENA_TABLES_PHYS_START: usize = ARENA_PHYS_START + 0x4000_0000;
/// First guest physical byte after the section.
pub const ARENA_PHYS_END: usize = ARENA_TABLES_PHYS_START + 0x10_0000;
/// Guest physical span of the section.
pub const ARENA_PHYS_RANGE: Range<usize> = ARENA_PHYS_START..ARENA_PHYS_END;
/// Size of the one host section.
pub const ARENA_STORE_BYTES: usize = ARENA_PHYS_END - ARENA_PHYS_START;

/// 4 KiB guest pages in the section.
const ARENA_PAGES: usize = ARENA_STORE_BYTES / 4096;

/// The unique right to admit arena references through one mounted `Memory`.
/// Cross-process marshalling in U10 must use the admitted process's memory;
/// it must not reach a parked process's host mapping directly.
#[derive(Debug)]
pub struct ArenaAdmission {
    pub(crate) store: Arc<ArenaStore>,
}

impl ArenaAdmission {
    /// Verify that a mount is moving this session's token.
    pub fn belongs_to(&self, store: &Arc<ArenaStore>) -> bool {
        Arc::ptr_eq(&self.store, store)
    }
}

/// One pagefile-style section and its host-commit record for a flat-memory session.
pub struct ArenaStore {
    #[cfg(windows)]
    handle: isize,
    #[cfg(unix)]
    fd: i32,
    /// One bit per 4 KiB guest page. A set bit stays set until teardown, so
    /// readers need no lock: a bit read as set is committed for good. Commits
    /// happen on the emulator thread (HLD I3); `Relaxed` suffices because a
    /// commit and every access it enables are ordered by that one thread (or
    /// by the mount hand-off, which synchronises through the session mutex).
    committed: Box<[AtomicU64]>,
}

impl ArenaStore {
    /// Create the session section and its one non-cloneable admission token.
    pub fn new() -> io::Result<(Arc<Self>, ArenaAdmission)> {
        #[cfg(windows)]
        let handle = {
            use windows_sys::Win32::System::Memory::{
                CreateFileMappingW, PAGE_READWRITE, SEC_RESERVE,
            };
            // SAFETY: INVALID_HANDLE_VALUE makes a pagefile-backed section;
            // its handle is closed in Drop.
            let raw = unsafe {
                CreateFileMappingW(
                    -1isize as _,
                    std::ptr::null(),
                    PAGE_READWRITE | SEC_RESERVE,
                    (ARENA_STORE_BYTES >> 32) as u32,
                    ARENA_STORE_BYTES as u32,
                    std::ptr::null(),
                )
            };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            raw as isize
        };

        #[cfg(target_os = "linux")]
        let fd = {
            // SAFETY: the name is NUL-terminated and the result is owned.
            let raw = unsafe {
                libc::syscall(
                    libc::SYS_memfd_create,
                    c"mdem-shared-arena".as_ptr(),
                    libc::MFD_CLOEXEC,
                )
            } as i32;
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            raw
        };

        #[cfg(target_os = "macos")]
        let fd = {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let name = std::ffi::CString::new(format!(
                "/mdem-arena-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
            .expect("arena shm name");
            // SAFETY: unique name, immediately unlinked after opening.
            let raw = unsafe {
                libc::shm_open(
                    name.as_ptr(),
                    libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                    0o600,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::shm_unlink(name.as_ptr()) } != 0 {
                let error = io::Error::last_os_error();
                unsafe { libc::close(raw) };
                return Err(error);
            }
            raw
        };

        #[cfg(unix)]
        if unsafe { libc::ftruncate(fd, ARENA_STORE_BYTES as libc::off_t) } != 0 {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(error);
        }

        let store = Arc::new(Self {
            #[cfg(windows)]
            handle,
            #[cfg(unix)]
            fd,
            committed: (0..ARENA_PAGES.div_ceil(64))
                .map(|_| AtomicU64::new(0))
                .collect(),
        });
        let admission = ArenaAdmission {
            store: store.clone(),
        };
        Ok((store, admission))
    }

    #[cfg(windows)]
    pub(crate) fn handle(&self) -> *mut core::ffi::c_void {
        self.handle as _
    }

    #[cfg(unix)]
    pub(crate) fn fd(&self) -> i32 {
        self.fd
    }

    #[inline]
    fn page_committed(&self, page: usize) -> bool {
        page < ARENA_PAGES
            && self.committed[page / 64].load(Ordering::Relaxed) & (1 << (page % 64)) != 0
    }

    /// True only when every page of the physical range was host-committed.
    /// An empty range is backed when it touches a committed page (its start,
    /// or the page it ends), matching slice indexing at an extent's end.
    pub fn is_committed(&self, range: Range<usize>) -> bool {
        if range.start > range.end || range.start < ARENA_PHYS_START || range.end > ARENA_PHYS_END {
            return false;
        }
        if range.is_empty() {
            let offset = range.start - ARENA_PHYS_START;
            let page = offset / 4096;
            return self.page_committed(page)
                || (offset.is_multiple_of(4096) && page > 0 && self.page_committed(page - 1));
        }
        let first = (range.start - ARENA_PHYS_START) / 4096;
        let last = (range.end - 1 - ARENA_PHYS_START) / 4096;
        (first..=last).all(|page| self.page_committed(page))
    }

    /// End of the committed run starting at `offset`, for range-from indexing.
    pub(crate) fn committed_end(&self, offset: usize) -> Option<usize> {
        if offset == ARENA_PHYS_END {
            return Some(offset);
        }
        if !(ARENA_PHYS_START..ARENA_PHYS_END).contains(&offset) {
            return None;
        }
        let mut page = (offset - ARENA_PHYS_START) / 4096;
        if !self.page_committed(page) {
            return None;
        }
        while self.page_committed(page) {
            page += 1;
        }
        Some(ARENA_PHYS_START + page * 4096)
    }

    /// Coalesce host-committed pages for sparse whole-block consumers.
    pub(crate) fn committed_extents(&self) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut start = None;
        for page in 0..=ARENA_PAGES {
            match (start, self.page_committed(page)) {
                (None, true) => start = Some(page),
                (Some(first), false) => {
                    ranges
                        .push((ARENA_PHYS_START + first * 4096)..(ARENA_PHYS_START + page * 4096));
                    start = None;
                }
                _ => {}
            }
        }
        ranges
    }

    /// Commit host pages through an attached view before exposing guest PTEs.
    pub(crate) fn commit(&self, view: *mut u8, range: Range<usize>) -> io::Result<()> {
        if range.start >= range.end || range.start < ARENA_PHYS_START || range.end > ARENA_PHYS_END
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "arena commit range",
            ));
        }
        let page = crate::host_page_size();
        let first = ((range.start - ARENA_PHYS_START) / page) * page;
        let end = (range.end - ARENA_PHYS_START).div_ceil(page) * page;
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Memory::{VirtualAlloc, MEM_COMMIT, PAGE_READWRITE};
            // SAFETY: view is the live section mapping and the aligned range
            // lies inside it. SEC_RESERVE shares commitment across its views.
            let committed = unsafe {
                VirtualAlloc(
                    view.wrapping_add(first).cast_const().cast(),
                    end - first,
                    MEM_COMMIT,
                    PAGE_READWRITE,
                )
            };
            if committed.is_null() {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(unix)]
        let _ = view;
        for p in first / 4096..end / 4096 {
            self.committed[p / 64].fetch_or(1 << (p % 64), Ordering::Relaxed);
        }
        Ok(())
    }
}

impl std::fmt::Debug for ArenaStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The bitmap is 64 Ki words; print its population, not its contents.
        let committed_pages: u32 = self
            .committed
            .iter()
            .map(|word| word.load(Ordering::Relaxed).count_ones())
            .sum();
        f.debug_struct("ArenaStore")
            .field("committed_pages", &committed_pages)
            .finish_non_exhaustive()
    }
}

impl Drop for ArenaStore {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle());
        }
        #[cfg(unix)]
        unsafe {
            libc::close(self.fd);
        }
    }
}
