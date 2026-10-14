//! Host folders used by settings persistence and capture output.

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(all(unix, any(test, not(any(target_os = "macos", target_os = "ios")))))]
use std::path::Path;
use std::path::PathBuf;

pub use platform::{config_dir, data_local_dir, home_dir, video_dir};

#[cfg(all(unix, any(test, not(any(target_os = "macos", target_os = "ios")))))]
fn xdg_dir(value: Option<OsString>, home: Option<PathBuf>, suffix: &str) -> Option<PathBuf> {
    value
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.map(|path| path.join(suffix)))
}

// The parser's compatibility rules derive from dirs-sys 0.4.1; see
// ../HOST_DIRS_LICENSE.txt for the upstream MIT notice.
#[cfg(all(unix, any(test, not(any(target_os = "macos", target_os = "ios")))))]
fn video_from_file(home: &Path, contents: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    fn trim_blank(bytes: &[u8]) -> &[u8] {
        let start = bytes
            .iter()
            .position(|byte| !matches!(byte, b' ' | b'\t'))
            .unwrap_or(bytes.len());
        let bytes = &bytes[start..];
        let end = bytes
            .iter()
            .rposition(|byte| !matches!(byte, b' ' | b'\t'))
            .map_or(0, |index| index + 1);
        &bytes[..end]
    }

    for line in contents.split(|byte| *byte == b'\n') {
        let Some(separator) = line.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        if trim_blank(&line[..separator]) != b"XDG_VIDEOS_DIR" {
            continue;
        }
        let value = trim_blank(&line[separator + 1..]);
        let Some(value) = value
            .strip_prefix(b"\"")
            .and_then(|value| value.strip_suffix(b"\""))
        else {
            continue;
        };
        let (value, relative) = if value == b"$HOME/" {
            continue;
        } else if let Some(value) = value.strip_prefix(b"$HOME/") {
            (value, true)
        } else if value.starts_with(b"/") {
            (value, false)
        } else {
            continue;
        };
        // Match xdg-user-dirs-update's escaped byte format, including non-UTF8
        // filenames. No shell evaluation or environment expansion is performed.
        let mut bytes = value.iter().copied();
        let mut unescaped = Vec::with_capacity(value.len());
        while let Some(byte) = bytes.next() {
            if byte == b'\\' {
                if let Some(next) = bytes.next() {
                    unescaped.push(next);
                }
            } else {
                unescaped.push(byte);
            }
        }
        let path = PathBuf::from(OsString::from_vec(unescaped));
        return Some(if relative { home.join(path) } else { path });
    }
    None
}

#[cfg(unix)]
fn unix_home(
    value: Option<OsString>,
    fallback: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(fallback)
}

#[cfg(unix)]
mod platform {
    use super::{unix_home, PathBuf};
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    use super::{video_from_file, xdg_dir};

    pub fn home_dir() -> Option<PathBuf> {
        unix_home(std::env::var_os("HOME"), account_home)
    }

    #[cfg(any(target_os = "android", target_os = "ios", target_os = "emscripten"))]
    fn account_home() -> Option<PathBuf> {
        None
    }

    #[cfg(not(any(target_os = "android", target_os = "ios", target_os = "emscripten")))]
    fn account_home() -> Option<PathBuf> {
        use std::ffi::CStr;
        use std::os::unix::ffi::OsStrExt;
        // Derived from dirs-sys 0.4.1 (../HOST_DIRS_LICENSE.txt). Match its
        // single lookup and OS-recommended buffer size. In
        // particular, a failed lookup returns None rather than an invented path.
        // SAFETY: sysconf/getuid take no pointers. All getpwuid_r output storage
        // is writable and stays alive through copying its NUL-terminated pw_dir.
        unsafe {
            let size = libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX);
            let size = usize::try_from(size).unwrap_or(512);
            let mut buffer = vec![0_u8; size];
            let mut account: libc::passwd = std::mem::zeroed();
            let mut result = std::ptr::null_mut();
            let status = libc::getpwuid_r(
                libc::getuid(),
                &mut account,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            );
            if status != 0 || result.is_null() || account.pw_dir.is_null() {
                return None;
            }
            let bytes = CStr::from_ptr(account.pw_dir).to_bytes();
            (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
        }
    }

    pub fn config_dir() -> Option<PathBuf> {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            home_dir().map(|home| home.join("Library/Application Support"))
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            xdg_dir(std::env::var_os("XDG_CONFIG_HOME"), home_dir(), ".config")
        }
    }

    pub fn data_local_dir() -> Option<PathBuf> {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            config_dir()
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            xdg_dir(
                std::env::var_os("XDG_DATA_HOME"),
                home_dir(),
                ".local/share",
            )
        }
    }

    pub fn video_dir() -> Option<PathBuf> {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            home_dir().map(|home| home.join("Movies"))
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            let home = home_dir()?;
            let config = xdg_dir(
                std::env::var_os("XDG_CONFIG_HOME"),
                Some(home.clone()),
                ".config",
            )?;
            let contents = std::fs::read(config.join("user-dirs.dirs")).ok()?;
            video_from_file(&home, &contents)
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::PathBuf;
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;

    // Windows SDK GUID layout and KnownFolders.h identifiers. Keep native shell
    // lookup: environment variables do not reflect redirected known folders.
    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    impl Guid {
        const fn from_u128(value: u128) -> Self {
            Self {
                data1: (value >> 96) as u32,
                data2: (value >> 80) as u16,
                data3: (value >> 64) as u16,
                data4: (value as u64).to_be_bytes(),
            }
        }
    }
    const PROFILE: Guid = Guid::from_u128(0x5e6c858f_0e22_4760_9afe_ea3317b67173);
    const ROAMING_APP_DATA: Guid = Guid::from_u128(0x3eb685db_65f9_4cf6_a03a_e3ef65729f3d);
    const LOCAL_APP_DATA: Guid = Guid::from_u128(0xf1b32785_6fba_4fcf_9d55_7b8e7f157091);
    const VIDEOS: Guid = Guid::from_u128(0x18989b1d_99b5_455b_841c_ab7c74e4ddfc);

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            folder: *const Guid,
            flags: u32,
            token: *mut c_void,
            path: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(pointer: *const c_void);
    }

    struct ShellPath(*mut u16);
    impl Drop for ShellPath {
        fn drop(&mut self) {
            // SAFETY: SHGetKnownFolderPath allocates with the COM allocator,
            // including on failure; CoTaskMemFree also permits a null pointer.
            unsafe {
                CoTaskMemFree(self.0.cast());
            }
        }
    }

    fn known_folder(folder: &Guid) -> Option<PathBuf> {
        let mut path = ShellPath(std::ptr::null_mut());
        // SAFETY: folder has the SDK GUID layout; flags/token request the current
        // user's folder. The writable out-pointer is owned by the RAII guard.
        let result = unsafe { SHGetKnownFolderPath(folder, 0, std::ptr::null_mut(), &mut path.0) };
        if result != 0 || path.0.is_null() {
            return None;
        }
        // SAFETY: successful shell lookup returns a NUL-terminated UTF-16 path.
        // Copy it before the allocation is freed by ShellPath's destructor.
        unsafe {
            let mut length = 0;
            while *path.0.add(length) != 0 {
                length += 1;
            }
            Some(PathBuf::from(OsString::from_wide(
                std::slice::from_raw_parts(path.0, length),
            )))
        }
    }

    pub fn home_dir() -> Option<PathBuf> {
        known_folder(&PROFILE)
    }
    pub fn config_dir() -> Option<PathBuf> {
        known_folder(&ROAMING_APP_DATA)
    }
    pub fn data_local_dir() -> Option<PathBuf> {
        known_folder(&LOCAL_APP_DATA)
    }
    pub fn video_dir() -> Option<PathBuf> {
        known_folder(&VIDEOS)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn xdg_absolute_override_and_relative_empty_fallback() {
        for invalid in [
            None,
            Some(OsString::new()),
            Some("relative/settings".into()),
        ] {
            assert_eq!(
                xdg_dir(invalid, Some("/users/alex".into()), ".config"),
                Some("/users/alex/.config".into())
            );
        }
        assert_eq!(
            xdg_dir(Some("/shared/config".into()), None, ".config"),
            Some("/shared/config".into())
        );
        assert_eq!(
            xdg_dir(None, Some("relative-home".into()), ".local/share"),
            Some("relative-home/.local/share".into())
        );
        assert_eq!(xdg_dir(None, None, ".config"), None);
    }

    #[test]
    fn video_home_absolute_quoting_and_first_valid_assignment() {
        let home = Path::new("/users/alex");
        assert_eq!(
            video_from_file(home, br#" XDG_VIDEOS_DIR = "$HOME/Clips with spaces" "#),
            Some("/users/alex/Clips with spaces".into())
        );
        assert_eq!(
            video_from_file(home, br#"XDG_VIDEOS_DIR="/media/Clips""#),
            Some("/media/Clips".into())
        );
        assert_eq!(video_from_file(home, b"XDG_VIDEOS_DIR=\"relative\"\nXDG_VIDEOS_DIR=\"/first\"\nXDG_VIDEOS_DIR=\"/second\""), Some("/first".into()));
    }

    #[test]
    fn video_rejects_disabled_unquoted_and_other_variables() {
        let home = Path::new("/users/alex");
        for input in [
            b"".as_slice(),
            br#"XDG_VIDEOS_DIR="$HOME/""#,
            br#"XDG_VIDEOS_DIR="$HOME""#,
            br#"XDG_VIDEOS_DIR="${HOME}/Movies""#,
            br#"XDG_VIDEOS_DIR="relative""#,
            br"XDG_VIDEOS_DIR='/movies'",
            br"XDG_VIDEOS_DIR=/movies",
            br#"XDG_MUSIC_DIR="/music""#,
            b"XDG_VIDEOS_DIR=\"/movies\"\r",
        ] {
            assert_eq!(video_from_file(home, input), None, "{input:?}");
        }
        assert_eq!(
            video_from_file(
                home,
                b"XDG_VIDEOS_DIR=\"$HOME/\"\nXDG_VIDEOS_DIR=\"/enabled\""
            ),
            Some("/enabled".into())
        );
    }

    #[test]
    fn malformed_lone_quote_is_ignored_and_later_assignment_survives() {
        let home = Path::new("/users/alex");
        assert_eq!(video_from_file(home, b"XDG_VIDEOS_DIR=\""), None);
        assert_eq!(
            video_from_file(home, b"XDG_VIDEOS_DIR=\"\nXDG_VIDEOS_DIR=\"/valid\""),
            Some("/valid".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn video_preserves_non_utf8_and_upstream_backslash_rules() {
        use std::os::unix::ffi::OsStringExt;
        assert_eq!(
            video_from_file(
                Path::new("/h"),
                b"XDG_VIDEOS_DIR=\"$HOME/a\\ b\\$c\\`d\\\"e\\\\f\\q\xff\""
            ),
            Some(PathBuf::from(OsString::from_vec(
                b"/h/a b$c`d\"e\\fq\xff".to_vec()
            )))
        );
        assert_eq!(
            video_from_file(Path::new("/h"), br#"XDG_VIDEOS_DIR="$HOME//elsewhere""#),
            Some("/elsewhere".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn home_uses_nonempty_env_even_when_relative_and_pwd_for_empty() {
        assert_eq!(
            unix_home(Some("relative-home".into()), || panic!(
                "fallback should not run"
            )),
            Some("relative-home".into())
        );
        for missing in [None, Some(OsString::new())] {
            assert_eq!(
                unix_home(missing, || Some("/account/home".into())),
                Some("/account/home".into())
            );
        }
        assert_eq!(unix_home(None, || None), None);
    }
}
