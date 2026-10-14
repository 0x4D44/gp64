//! Test utilities for unit tests within the crate ecosystem.

/// Guard that restores an environment variable to its previous state on drop.
pub struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    /// Set an environment variable, returning a guard that restores it on drop.
    pub fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prev }
    }

    /// Unset an environment variable, returning a guard that restores it on drop.
    pub fn unset(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = &self.prev {
            std::env::set_var(self.key, prev);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

/// Acquire a lock for tests that modify environment variables.
/// This prevents concurrent tests from interfering with each other.
/// Recovers from poisoned state to allow tests to continue even if a prior test panicked.
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let mutex = LOCK.get_or_init(|| Mutex::new(()));
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Keys `MDEM_LONG_TESTS` accepts: `proptest` runs proptest suites at their
/// full case counts, `soaks` runs the long soak durations. Neither is set by
/// `full_test`; both are opt-in for a deliberate deep run.
const LONG_TESTS_KEYS: [&str; 2] = ["proptest", "soaks"];

/// Whether the comma-separated `MDEM_LONG_TESTS` value `list` names `key`.
/// Panics on an unknown key so a typo cannot silently keep a run short.
pub fn long_tests_list_has(list: &str, key: &str) -> bool {
    let mut found = false;
    for item in list
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        assert!(
            LONG_TESTS_KEYS.contains(&item),
            "MDEM_LONG_TESTS: unknown key {item:?}; expected one of {LONG_TESTS_KEYS:?}"
        );
        found |= item == key;
    }
    found
}

/// Whether `MDEM_LONG_TESTS` enables `key` (`proptest` or `soaks`).
pub fn long_tests(key: &str) -> bool {
    std::env::var("MDEM_LONG_TESTS").is_ok_and(|list| long_tests_list_has(&list, key))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[cfg(any(unix, windows))]
    struct RestoreEnvVar {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    #[cfg(any(unix, windows))]
    impl RestoreEnvVar {
        fn new(key: &'static str) -> Self {
            Self {
                key,
                original: std::env::var_os(key),
            }
        }
    }

    #[cfg(any(unix, windows))]
    impl Drop for RestoreEnvVar {
        fn drop(&mut self) {
            if let Some(original) = &self.original {
                std::env::set_var(self.key, original);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[cfg(unix)]
    fn non_unicode_value() -> std::ffi::OsString {
        use std::os::unix::ffi::OsStringExt;

        std::ffi::OsString::from_vec(vec![0xff])
    }

    #[cfg(windows)]
    fn non_unicode_value() -> std::ffi::OsString {
        use std::os::windows::ffi::OsStringExt;

        std::ffi::OsString::from_wide(&[0xd800])
    }

    #[test]
    fn test_env_var_guard_set_and_restore() {
        let _lock = env_lock();
        let key = "TESTUTIL_ENV_GUARD_SET";
        let original = std::env::var(key).ok();

        {
            let _guard = EnvVarGuard::set(key, "value");
            assert_eq!(std::env::var(key).as_deref(), Ok("value"));
        }

        assert_eq!(std::env::var(key).ok(), original);
    }

    #[test]
    fn test_env_var_guard_unset_and_restore() {
        let _lock = env_lock();
        let key = "TESTUTIL_ENV_GUARD_UNSET";
        std::env::set_var(key, "present");
        let original = std::env::var(key).ok();

        {
            let _guard = EnvVarGuard::unset(key);
            assert!(std::env::var(key).is_err());
        }

        assert_eq!(std::env::var(key).ok(), original);
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn test_env_var_guard_set_restores_non_unicode() {
        let _lock = env_lock();
        let key = "GP64_TEST_ENV_VAR_GUARD_SET_NON_UNICODE_00098";
        let _restore = RestoreEnvVar::new(key);
        let original = non_unicode_value();
        std::env::set_var(key, &original);

        {
            let _guard = EnvVarGuard::set(key, "temporary set value");
            assert_eq!(
                std::env::var_os(key),
                Some(std::ffi::OsString::from("temporary set value"))
            );
        }

        assert_eq!(
            std::env::var_os(key),
            Some(original),
            "EnvVarGuard::set must restore the exact prior OS value"
        );
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn test_env_var_guard_unset_restores_non_unicode() {
        let _lock = env_lock();
        let key = "GP64_TEST_ENV_VAR_GUARD_UNSET_NON_UNICODE_00098";
        let _restore = RestoreEnvVar::new(key);
        let original = non_unicode_value();
        std::env::set_var(key, &original);

        {
            let _guard = EnvVarGuard::unset(key);
            assert!(std::env::var_os(key).is_none());
        }

        assert_eq!(
            std::env::var_os(key),
            Some(original),
            "EnvVarGuard::unset must restore the exact prior OS value"
        );
    }

    #[test]
    fn test_env_lock_reentry() {
        // Force a poisoned lock and verify env_lock recovers via into_inner().
        let _ = std::thread::spawn(|| {
            let _lock = env_lock();
            panic!("intentional poison");
        })
        .join();

        let _lock = env_lock();
    }

    #[test]
    fn long_tests_list_selects_keys_and_rejects_typos() {
        assert!(super::long_tests_list_has("proptest, soaks", "soaks"));
        assert!(super::long_tests_list_has("proptest", "proptest"));
        assert!(!super::long_tests_list_has("proptest", "soaks"));
        assert!(!super::long_tests_list_has("", "proptest"));
        assert!(std::panic::catch_unwind(|| super::long_tests_list_has("soak", "soaks")).is_err());
    }
}
