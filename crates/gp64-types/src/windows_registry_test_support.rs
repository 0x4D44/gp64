use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

#[allow(clippy::upper_case_acronyms)]
type HKEY = *mut c_void;

const HKEY_CURRENT_USER: HKEY = 0x8000_0001_usize as HKEY;
const KEY_WRITE: u32 = 0x20006;
const ERROR_ACCESS_DENIED: u32 = 5;
const REGISTRY_WRITE_SKIP_REASON: &str =
    "skipping Windows registry persistence test: HKCU registry writes are denied in this runtime (Win32 error 5)";

static REGISTRY_COUNTER: AtomicUsize = AtomicUsize::new(0);
static SKIP_REASON: OnceLock<Option<&'static str>> = OnceLock::new();

#[link(name = "advapi32")]
extern "system" {
    fn RegCreateKeyExW(
        hkey: HKEY,
        sub_key: *const u16,
        reserved: u32,
        class: *mut u16,
        options: u32,
        sam_desired: u32,
        security_attributes: *const c_void,
        result: *mut HKEY,
        disposition: *mut u32,
    ) -> u32;
    fn RegCloseKey(hkey: HKEY) -> u32;
    fn RegDeleteTreeW(hkey: HKEY, sub_key: *const u16) -> u32;
}

pub fn skip_registry_write_tests() -> bool {
    if let Some(reason) = *SKIP_REASON.get_or_init(probe_registry_write_skip_reason) {
        eprintln!("{reason}");
        return true;
    }
    false
}

fn probe_registry_write_skip_reason() -> Option<&'static str> {
    let id = REGISTRY_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = format!(
        "Software\\gp64-test-capability-{}-{}",
        std::process::id(),
        id
    );
    let status = create_probe_key(&path);
    if status == 0 {
        delete_registry_tree(&path);
    }
    registry_write_skip_reason_for_status(status)
}

fn create_probe_key(path: &str) -> u32 {
    let wide_path = to_wide(path);
    let mut key: HKEY = ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide_path.as_ptr(),
            0,
            ptr::null_mut(),
            0,
            KEY_WRITE,
            ptr::null(),
            &raw mut key,
            ptr::null_mut(),
        )
    };
    if status == 0 && !key.is_null() {
        unsafe {
            RegCloseKey(key);
        }
    }
    status
}

fn delete_registry_tree(path: &str) {
    let wide_path = to_wide(path);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, wide_path.as_ptr());
    }
}

fn to_wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn registry_write_skip_reason_for_status(status: u32) -> Option<&'static str> {
    if status == ERROR_ACCESS_DENIED {
        return Some(REGISTRY_WRITE_SKIP_REASON);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_write_skip_reason_matches_access_denied() {
        assert_eq!(
            registry_write_skip_reason_for_status(ERROR_ACCESS_DENIED),
            Some(REGISTRY_WRITE_SKIP_REASON)
        );
    }

    #[test]
    fn test_registry_write_skip_reason_ignores_other_errors() {
        assert_eq!(registry_write_skip_reason_for_status(2), None);
        assert_eq!(registry_write_skip_reason_for_status(0), None);
    }
}
