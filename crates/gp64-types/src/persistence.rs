//! Persistence module for saving/loading window settings.
//!
//! On Windows, saves settings under HKCU\Software\gp64.
//! On other platforms, saves window position and size to the platform-specific local data
//! directory (e.g., `~/.local/share/gp64/window.json` on Linux).

use serde::{Deserialize, Serialize};

#[cfg(not(target_os = "windows"))]
use std::fs;
#[cfg(not(target_os = "windows"))]
use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
use std::ffi::{c_void, OsStr};
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;
#[cfg(target_os = "windows")]
use std::ptr;

/// Window position and size settings
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WindowSettings {
    /// X position of window
    pub x: i32,
    /// Y position of window
    pub y: i32,
    /// Width of window
    pub width: i32,
    /// Height of window
    pub height: i32,
    /// Whether the window was maximized. The x/y/width/height above always
    /// describe the NORMAL (restored) placement, matching Windows'
    /// WINDOWPLACEMENT convention. Defaults false so settings files written
    /// before this field existed keep their meaning.
    #[serde(default)]
    pub maximized: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            x: -1,
            y: -1,
            width: 960, // Default 4:3 window at reasonable size
            height: 720,
            maximized: false,
        }
    }
}

/// Stable storage key for per-application window settings.
///
/// The identifier can be an absolute path, title, or other caller-defined
/// string. The returned key is short, ASCII-only, and safe for both registry
/// subkeys and filesystem names.
pub fn window_settings_storage_key(identifier: &str) -> String {
    use std::fmt::Write as _;

    let normalized = identifier.trim();
    let hash = blake3::hash(normalized.as_bytes());
    let mut key = String::with_capacity(16);
    for byte in &hash.as_bytes()[..8] {
        write!(&mut key, "{byte:02x}").expect("writing to String should not fail");
    }
    key
}

// =============================================================================
// Windows registry persistence
// =============================================================================

#[cfg(target_os = "windows")]
const REGISTRY_BASE_KEY: &str = "Software\\gp64";
#[cfg(target_os = "windows")]
const REGISTRY_MAIN_SUBKEY: &str = "Window";
#[cfg(target_os = "windows")]
const REGISTRY_MONITOR_SUBKEY: &str = "MonitorWindow";
#[cfg(target_os = "windows")]
const REGISTRY_NAMED_WINDOW_SUBKEY: &str = "NamedWindows";
#[cfg(target_os = "windows")]
const REGISTRY_VALUE_SETTINGS: &str = "Settings";
#[cfg(target_os = "windows")]
const REGISTRY_VALUE_X: &str = "X";
#[cfg(target_os = "windows")]
const REGISTRY_VALUE_Y: &str = "Y";
#[cfg(target_os = "windows")]
const REGISTRY_VALUE_WIDTH: &str = "Width";
#[cfg(target_os = "windows")]
const REGISTRY_VALUE_HEIGHT: &str = "Height";

#[cfg(target_os = "windows")]
#[allow(clippy::upper_case_acronyms)]
type HKEY = *mut c_void;

#[cfg(target_os = "windows")]
const HKEY_CURRENT_USER: HKEY = 0x8000_0001_usize as HKEY;
#[cfg(target_os = "windows")]
const KEY_WRITE: u32 = 0x20006;
#[cfg(target_os = "windows")]
const REG_BINARY: u32 = 3;
#[cfg(all(test, target_os = "windows"))]
const REG_DWORD: u32 = 4;
#[cfg(target_os = "windows")]
const RRF_RT_REG_BINARY: u32 = 0x0000_0008;
#[cfg(target_os = "windows")]
const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
#[cfg(target_os = "windows")]
const ERROR_SUCCESS: u32 = 0;
#[cfg(target_os = "windows")]
const WINDOW_SETTINGS_BINARY_VERSION: u32 = 2;
#[cfg(target_os = "windows")]
const WINDOW_SETTINGS_BINARY_LEN: usize = 24;
/// Version-1 records carried no `maximized` slot; still accepted on read.
#[cfg(target_os = "windows")]
const WINDOW_SETTINGS_BINARY_V1_LEN: usize = 20;

#[cfg(target_os = "windows")]
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
    fn RegSetValueExW(
        hkey: HKEY,
        value_name: *const u16,
        reserved: u32,
        value_type: u32,
        data: *const u8,
        data_size: u32,
    ) -> u32;
    fn RegGetValueW(
        hkey: HKEY,
        sub_key: *const u16,
        value: *const u16,
        flags: u32,
        value_type: *mut u32,
        data: *mut c_void,
        data_size: *mut u32,
    ) -> u32;
    fn RegCloseKey(hkey: HKEY) -> u32;
}

/// `base` is [`REGISTRY_BASE_KEY`] in production; tests pass a private key
/// so they never touch the user's real window settings.
#[cfg(target_os = "windows")]
fn registry_subkey_path(base: &str, subkey: &str) -> String {
    if subkey.is_empty() {
        base.to_string()
    } else {
        format!("{base}\\{subkey}")
    }
}

#[cfg(target_os = "windows")]
fn to_wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(target_os = "windows")]
fn open_or_create_key(path: &str) -> Result<HKEY, String> {
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
    if status != ERROR_SUCCESS {
        return Err(format!("RegCreateKeyExW failed: {status}"));
    }
    Ok(key)
}

#[cfg(all(test, target_os = "windows"))]
fn set_reg_dword(key: HKEY, value_name: &str, value: u32) -> Result<(), String> {
    let wide_value = to_wide(value_name);
    let data = value.to_le_bytes();
    let status =
        unsafe { RegSetValueExW(key, wide_value.as_ptr(), 0, REG_DWORD, data.as_ptr(), 4) };
    if status != ERROR_SUCCESS {
        return Err(format!("RegSetValueExW failed: {status}"));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_reg_binary(key: HKEY, value_name: &str, data: &[u8]) -> Result<(), String> {
    let wide_value = to_wide(value_name);
    let data_size =
        u32::try_from(data.len()).map_err(|_| "registry value is too large".to_string())?;
    let status = unsafe {
        RegSetValueExW(
            key,
            wide_value.as_ptr(),
            0,
            REG_BINARY,
            data.as_ptr(),
            data_size,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!("RegSetValueExW failed: {status}"));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn read_reg_dword(subkey: &str, value_name: &str) -> Option<u32> {
    let wide_subkey = to_wide(subkey);
    let wide_value = to_wide(value_name);
    let mut data: u32 = 0;
    let mut data_size: u32 = std::mem::size_of::<u32>() as u32;
    let mut value_type: u32 = 0;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide_subkey.as_ptr(),
            wide_value.as_ptr(),
            RRF_RT_REG_DWORD,
            &raw mut value_type,
            (&raw mut data).cast::<c_void>(),
            &raw mut data_size,
        )
    };
    if status == ERROR_SUCCESS {
        Some(data)
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn encode_window_settings(settings: &WindowSettings) -> [u8; WINDOW_SETTINGS_BINARY_LEN] {
    let mut data = [0; WINDOW_SETTINGS_BINARY_LEN];
    data[..std::mem::size_of::<u32>()]
        .copy_from_slice(&WINDOW_SETTINGS_BINARY_VERSION.to_le_bytes());
    for (slot, value) in [
        settings.x,
        settings.y,
        settings.width,
        settings.height,
        i32::from(settings.maximized),
    ]
    .into_iter()
    .enumerate()
    {
        let start = (slot + 1) * std::mem::size_of::<i32>();
        data[start..start + std::mem::size_of::<i32>()].copy_from_slice(&value.to_le_bytes());
    }
    data
}

#[cfg(target_os = "windows")]
fn decode_window_settings(data: &[u8]) -> Option<WindowSettings> {
    if data.len() < WINDOW_SETTINGS_BINARY_V1_LEN {
        return None;
    }
    let read_i32 = |slot: usize| {
        let start = slot * std::mem::size_of::<i32>();
        i32::from_le_bytes(
            data[start..start + std::mem::size_of::<i32>()]
                .try_into()
                .unwrap(),
        )
    };
    let version = u32::from_le_bytes(data[..std::mem::size_of::<u32>()].try_into().unwrap());
    let maximized = match (version, data.len()) {
        // v1: no maximized slot.
        (1, WINDOW_SETTINGS_BINARY_V1_LEN) => false,
        (2, WINDOW_SETTINGS_BINARY_LEN) => read_i32(5) != 0,
        _ => return None,
    };
    Some(WindowSettings {
        x: read_i32(1),
        y: read_i32(2),
        width: read_i32(3),
        height: read_i32(4),
        maximized,
    })
}

#[cfg(target_os = "windows")]
fn read_window_settings_binary(subkey: &str) -> Option<WindowSettings> {
    let wide_subkey = to_wide(subkey);
    let wide_value = to_wide(REGISTRY_VALUE_SETTINGS);
    let mut data = [0; WINDOW_SETTINGS_BINARY_LEN];
    let mut data_size = WINDOW_SETTINGS_BINARY_LEN as u32;
    let mut value_type = 0;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide_subkey.as_ptr(),
            wide_value.as_ptr(),
            RRF_RT_REG_BINARY,
            &raw mut value_type,
            data.as_mut_ptr().cast::<c_void>(),
            &raw mut data_size,
        )
    };
    // Accept both record lengths: v1 (20 bytes) and v2 (24 bytes).
    let read_len = data_size as usize;
    if status != ERROR_SUCCESS || read_len > data.len() {
        return None;
    }
    decode_window_settings(&data[..read_len])
}

#[cfg(target_os = "windows")]
fn save_window_settings_to_registry(
    base: &str,
    subkey: &str,
    settings: &WindowSettings,
) -> Result<(), String> {
    let path = registry_subkey_path(base, subkey);
    let key = open_or_create_key(&path)?;
    let data = encode_window_settings(settings);
    let result = set_reg_binary(key, REGISTRY_VALUE_SETTINGS, &data);
    unsafe {
        RegCloseKey(key);
    }
    result
}

#[cfg(target_os = "windows")]
fn load_window_settings_from_registry(base: &str, subkey: &str) -> Option<WindowSettings> {
    let path = registry_subkey_path(base, subkey);
    if let Some(settings) = read_window_settings_binary(&path) {
        return Some(settings);
    }

    // Values written by gp64 versions before the atomic Settings record.
    let x = read_reg_dword(&path, REGISTRY_VALUE_X)? as i32;
    let y = read_reg_dword(&path, REGISTRY_VALUE_Y)? as i32;
    let width = read_reg_dword(&path, REGISTRY_VALUE_WIDTH)? as i32;
    let height = read_reg_dword(&path, REGISTRY_VALUE_HEIGHT)? as i32;
    Some(WindowSettings {
        x,
        y,
        width,
        height,
        maximized: false,
    })
}

#[cfg(target_os = "windows")]
pub fn save_window_settings(settings: &WindowSettings) -> Result<(), String> {
    save_window_settings_to_registry(REGISTRY_BASE_KEY, REGISTRY_MAIN_SUBKEY, settings)
}

#[cfg(target_os = "windows")]
pub fn load_window_settings() -> Option<WindowSettings> {
    load_window_settings_from_registry(REGISTRY_BASE_KEY, REGISTRY_MAIN_SUBKEY)
}

#[cfg(target_os = "windows")]
pub fn save_monitor_window_settings(settings: &WindowSettings) -> Result<(), String> {
    save_window_settings_to_registry(REGISTRY_BASE_KEY, REGISTRY_MONITOR_SUBKEY, settings)
}

#[cfg(target_os = "windows")]
pub fn load_monitor_window_settings() -> Option<WindowSettings> {
    load_window_settings_from_registry(REGISTRY_BASE_KEY, REGISTRY_MONITOR_SUBKEY)
}

#[cfg(target_os = "windows")]
fn named_window_settings_subkey(identifier: &str) -> String {
    format!(
        "{}\\{}",
        REGISTRY_NAMED_WINDOW_SUBKEY,
        window_settings_storage_key(identifier)
    )
}

/// Save window settings under a caller-defined identifier.
#[cfg(target_os = "windows")]
pub fn save_named_window_settings(
    identifier: &str,
    settings: &WindowSettings,
) -> Result<(), String> {
    let subkey = named_window_settings_subkey(identifier);
    save_window_settings_to_registry(REGISTRY_BASE_KEY, &subkey, settings)
}

/// Load window settings previously saved under a caller-defined identifier.
#[cfg(target_os = "windows")]
pub fn load_named_window_settings(identifier: &str) -> Option<WindowSettings> {
    let subkey = named_window_settings_subkey(identifier);
    load_window_settings_from_registry(REGISTRY_BASE_KEY, &subkey)
}

// =============================================================================
// Filesystem persistence (non-Windows)
// =============================================================================

#[cfg(not(target_os = "windows"))]
/// Application data directory name
const APP_DIR_NAME: &str = "gp64";

#[cfg(not(target_os = "windows"))]
/// Window settings file name
const WINDOW_SETTINGS_FILE_NAME: &str = "window.json";

#[cfg(not(target_os = "windows"))]
/// Monitor settings file name
const MONITOR_WINDOW_SETTINGS_FILE_NAME: &str = "monitor_window.json";

#[cfg(not(target_os = "windows"))]
/// Directory for per-application window settings
const NAMED_WINDOW_SETTINGS_DIR_NAME: &str = "named_windows";

/// Get the data directory path for gp64
#[cfg(not(target_os = "windows"))]
fn get_data_dir() -> Option<PathBuf> {
    crate::host_dirs::data_local_dir().map(|p| p.join(APP_DIR_NAME))
}

/// The window settings file inside `data_dir`.
#[cfg(not(target_os = "windows"))]
fn window_settings_path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(WINDOW_SETTINGS_FILE_NAME)
}

/// The monitor window settings file inside `data_dir`.
#[cfg(not(target_os = "windows"))]
fn monitor_window_settings_path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(MONITOR_WINDOW_SETTINGS_FILE_NAME)
}

/// The settings file for one caller-defined identifier inside `data_dir`.
#[cfg(not(target_os = "windows"))]
fn named_window_settings_path_in(data_dir: &Path, identifier: &str) -> PathBuf {
    data_dir
        .join(NAMED_WINDOW_SETTINGS_DIR_NAME)
        .join(format!("{}.json", window_settings_storage_key(identifier)))
}

/// Get the window settings file path
#[cfg(not(target_os = "windows"))]
fn get_window_settings_path() -> Option<PathBuf> {
    get_data_dir().map(|dir| window_settings_path_in(&dir))
}

/// Get the monitor settings file path
#[cfg(not(target_os = "windows"))]
fn get_monitor_window_settings_path() -> Option<PathBuf> {
    get_data_dir().map(|dir| monitor_window_settings_path_in(&dir))
}

#[cfg(not(target_os = "windows"))]
fn get_named_window_settings_path(identifier: &str) -> Option<PathBuf> {
    get_data_dir().map(|dir| named_window_settings_path_in(&dir, identifier))
}

#[cfg(not(target_os = "windows"))]
fn window_settings_backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

#[cfg(not(target_os = "windows"))]
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

#[cfg(not(target_os = "windows"))]
fn window_settings_temp_path(path: &Path) -> PathBuf {
    // MDD-BUG-CRUCIBLE-00524: unique per call. A single shared `*.json.tmp`
    // let two savers of the same target interleave: A writes its JSON, B
    // overwrites the SHARED temp with its own, A rotates the primary and
    // renames - so A returns Ok having committed B's settings - and B then
    // fails its rename because the path is already gone. A successful return
    // did not mean the caller's settings were the ones stored.
    //
    // The pid keeps separate processes apart; the counter keeps threads and
    // repeated saves within one process apart. `rename` onto the final path
    // stays atomic, so the commit step is unchanged - only the staging slot
    // becomes private.
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let unique = TEMP_SEQ.fetch_add(1, AtomicOrdering::Relaxed);
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("window.json"));
    name.push(format!(".{}.{}.tmp", std::process::id(), unique));
    path.with_file_name(name)
}

#[cfg(not(target_os = "windows"))]
struct TempPathCleanup {
    path: PathBuf,
    armed: bool,
}

#[cfg(not(target_os = "windows"))]
impl TempPathCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(not(target_os = "windows"))]
impl Drop for TempPathCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn read_window_settings(path: &Path) -> Option<WindowSettings> {
    let content = fs::read_to_string(path).ok()?;
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    serde_json::from_str(content).ok()
}

#[cfg(not(target_os = "windows"))]
fn save_window_settings_to_path(settings: &WindowSettings, path: &Path) -> Result<(), String> {
    // Ensure directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create data directory: {e}"))?;
    }

    let json =
        serde_json::to_string_pretty(settings).map_err(|e| format!("Serialization failed: {e}"))?;

    let mut tmp_cleanup = TempPathCleanup::new(window_settings_temp_path(path));
    let backup_path = window_settings_backup_path(path);

    fs::write(tmp_cleanup.path(), json)
        .map_err(|e| format!("Failed to write window settings: {e}"))?;

    if path.exists() {
        let _ = fs::remove_file(&backup_path);
        if fs::rename(path, &backup_path).is_err() {
            fs::copy(path, &backup_path)
                .map_err(|e| format!("Failed to backup window settings: {e}"))?;
            fs::remove_file(path)
                .map_err(|e| format!("Failed to remove old window settings: {e}"))?;
        }
    }

    // Now that the temp path is per-call, a failure would strand a file nobody
    // ever reuses - so remove it rather than leaking one per failed save.
    fs::rename(tmp_cleanup.path(), path)
        .map_err(|e| format!("Failed to finalize window settings: {e}"))?;
    tmp_cleanup.disarm();

    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn load_window_settings_from_path(path: &Path) -> Option<WindowSettings> {
    // Try the primary file if it is present and parses.
    if path.exists() {
        if let Some(settings) = read_window_settings(path) {
            return Some(settings);
        }
    }

    // Fall back to the backup whenever the primary is missing OR corrupt. The
    // save protocol moves the primary to `*.json.bak` before renaming the temp
    // file into place, so a crash in that window leaves only the backup — the
    // primary-missing case must recover from it too (MDD-BUG-CRUCIBLE-00041).
    let backup_path = window_settings_backup_path(path);
    if backup_path.exists() {
        if let Some(settings) = read_window_settings(&backup_path) {
            return Some(settings);
        }
    }

    None
}

/// Save window settings to disk
#[cfg(not(target_os = "windows"))]
pub fn save_window_settings(settings: &WindowSettings) -> Result<(), String> {
    let path = get_window_settings_path().ok_or("Could not determine data directory")?;
    save_window_settings_to_path(settings, &path)
}

/// Load window settings from disk
#[cfg(not(target_os = "windows"))]
pub fn load_window_settings() -> Option<WindowSettings> {
    let path = get_window_settings_path()?;
    load_window_settings_from_path(&path)
}

/// Save monitor window settings to disk
#[cfg(not(target_os = "windows"))]
pub fn save_monitor_window_settings(settings: &WindowSettings) -> Result<(), String> {
    let path = get_monitor_window_settings_path().ok_or("Could not determine data directory")?;
    save_window_settings_to_path(settings, &path)
}

/// Load monitor window settings from disk
#[cfg(not(target_os = "windows"))]
pub fn load_monitor_window_settings() -> Option<WindowSettings> {
    let path = get_monitor_window_settings_path()?;
    load_window_settings_from_path(&path)
}

/// Save window settings under a caller-defined identifier.
#[cfg(not(target_os = "windows"))]
pub fn save_named_window_settings(
    identifier: &str,
    settings: &WindowSettings,
) -> Result<(), String> {
    let path =
        get_named_window_settings_path(identifier).ok_or("Could not determine data directory")?;
    save_window_settings_to_path(settings, &path)
}

/// Load window settings previously saved under a caller-defined identifier.
#[cfg(not(target_os = "windows"))]
pub fn load_named_window_settings(identifier: &str) -> Option<WindowSettings> {
    let path = get_named_window_settings_path(identifier)?;
    load_window_settings_from_path(&path)
}

/// Check if a window position is valid (basic bounds check)
/// Returns true if the position seems reasonable (not too far off-screen)
pub fn is_position_valid(x: i32, y: i32, width: i32, height: i32) -> bool {
    is_position_valid_with_min_size(x, y, width, height, 320, 200)
}

/// Check window geometry with a caller-selected minimum client size.
///
/// The real-boot window can legitimately persist below the normal logical-size
/// floor when a native physical framebuffer is displayed on a high-DPI monitor.
pub fn is_position_valid_with_min_size(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    min_width: i32,
    min_height: i32,
) -> bool {
    // Allow some negative values for multi-monitor setups, but not too extreme
    x >= -2000
        && y >= -2000
        && x <= 20000
        && y <= 20000
        && width >= min_width
        && height >= min_height
        && width <= 8000
        && height <= 8000
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Lightweight temp dir helper to avoid extra test-only dependencies.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> std::io::Result<Self> {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "gp64-types-persistence-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self { path })
        }

        fn new_at(parent: &Path) -> std::io::Result<Self> {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            fs::create_dir_all(parent)?;
            loop {
                let id = COUNTER.fetch_add(1, Ordering::Relaxed);
                let path = parent.join(format!(
                    "gp64-types-persistence-{}-{id}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Ok(Self { path }),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn test_window_settings_default() {
        let settings = WindowSettings::default();
        assert_eq!(settings.width, 960);
        assert_eq!(settings.height, 720); // 4:3 aspect ratio
    }

    #[test]
    fn test_window_settings_serialization() {
        let settings = WindowSettings {
            x: 100,
            y: 200,
            width: 800,
            height: 600,
            maximized: true,
        };

        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("100"));
        assert!(json.contains("200"));

        let restored: WindowSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.x, 100);
        assert_eq!(restored.y, 200);
    }

    #[test]
    fn test_window_settings_storage_key_is_stable_and_safe() {
        let key = window_settings_storage_key(r"C:\Apps\FIRST.EXE");
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(key, window_settings_storage_key(r"C:\Apps\FIRST.EXE"));
        assert_ne!(key, window_settings_storage_key(r"C:\Apps\SECOND.EXE"));
    }

    #[test]
    fn test_is_position_valid() {
        assert!(is_position_valid(100, 100, 800, 600));
        assert!(is_position_valid(-100, -100, 800, 600)); // Multi-monitor
        assert!(!is_position_valid(-3000, 100, 800, 600)); // Too far left
        assert!(!is_position_valid(25000, 100, 800, 600)); // Too far right
        assert!(!is_position_valid(100, 25000, 800, 600)); // Too far down
        assert!(!is_position_valid(100, 100, 100, 100)); // Too small
    }

    #[test]
    fn test_save_and_load_window_settings_round_trip() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        let settings = WindowSettings {
            x: 12,
            y: 34,
            width: 1024,
            height: 768,
            maximized: false,
        };
        save_window_settings_to_path(&settings, &window_settings_path_in(dir)).unwrap();

        let loaded = load_window_settings_from_path(&window_settings_path_in(dir)).unwrap();
        assert_eq!(loaded.x, settings.x);
        assert_eq!(loaded.y, settings.y);
        assert_eq!(loaded.width, settings.width);
        assert_eq!(loaded.height, settings.height);
    }

    #[test]
    fn test_save_and_load_named_window_settings_round_trip() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        let first_settings = WindowSettings {
            x: 111,
            y: 222,
            width: 333,
            height: 444,
            maximized: true,
        };
        let second_settings = WindowSettings {
            x: 555,
            y: 666,
            width: 777,
            height: 888,
            maximized: false,
        };

        save_window_settings_to_path(
            &first_settings,
            &named_window_settings_path_in(dir, r"C:\Apps\FIRST.EXE"),
        )
        .unwrap();
        save_window_settings_to_path(
            &second_settings,
            &named_window_settings_path_in(dir, r"C:\Apps\SECOND.EXE"),
        )
        .unwrap();

        assert_eq!(
            load_window_settings_from_path(&named_window_settings_path_in(
                dir,
                r"C:\Apps\FIRST.EXE"
            )),
            Some(first_settings)
        );
        assert_eq!(
            load_window_settings_from_path(&named_window_settings_path_in(
                dir,
                r"C:\Apps\SECOND.EXE"
            )),
            Some(second_settings)
        );
        assert!(
            load_window_settings_from_path(&named_window_settings_path_in(
                dir,
                r"C:\Apps\THIRD.EXE"
            ))
            .is_none()
        );
    }

    #[test]
    fn test_save_window_settings_creates_backup() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        let first = WindowSettings {
            x: 10,
            y: 20,
            width: 640,
            height: 480,
            maximized: false,
        };
        save_window_settings_to_path(&first, &window_settings_path_in(dir)).unwrap();

        let second = WindowSettings {
            x: 30,
            y: 40,
            width: 800,
            height: 600,
            maximized: true,
        };
        save_window_settings_to_path(&second, &window_settings_path_in(dir)).unwrap();

        let path = window_settings_path_in(dir);
        let backup_path = window_settings_backup_path(&path);
        let backup_json = fs::read_to_string(&backup_path).unwrap();
        let backup_settings: WindowSettings = serde_json::from_str(&backup_json).unwrap();

        assert_eq!(backup_settings.x, first.x);
        assert_eq!(backup_settings.y, first.y);
        assert_eq!(backup_settings.width, first.width);
        assert_eq!(backup_settings.height, first.height);
    }

    #[test]
    fn test_window_settings_backup_failure_cleans_staging() {
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
        let temp_dir = TempDir::new_at(&scratch).unwrap();
        let dir = temp_dir.path();
        let path = dir.join("window.json");
        let backup_path = window_settings_backup_path(&path);
        let primary_contents = b"synthetic primary settings";
        let obstruction_contents = b"synthetic backup obstruction";
        fs::write(&path, primary_contents).unwrap();
        fs::create_dir(&backup_path).unwrap();
        let obstruction = backup_path.join("keep.txt");
        fs::write(&obstruction, obstruction_contents).unwrap();

        let settings = WindowSettings {
            x: 12,
            y: 34,
            width: 1024,
            height: 768,
            maximized: false,
        };
        for _ in 0..2 {
            let error = save_window_settings_to_path(&settings, &path).unwrap_err();
            assert!(
                error.starts_with("Failed to backup window settings:"),
                "unexpected save error: {error}"
            );
            assert!(path.is_file());
            assert_eq!(fs::read(&path).unwrap(), primary_contents);
            assert!(backup_path.is_dir());
            assert_eq!(fs::read(&obstruction).unwrap(), obstruction_contents);
        }

        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec![
                std::ffi::OsString::from("window.json"),
                std::ffi::OsString::from("window.json.bak"),
            ],
            "failed saves should clean their unique staging files"
        );
    }

    #[test]
    fn test_load_window_settings_strips_bom() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        let settings = WindowSettings {
            x: 1,
            y: 2,
            width: 800,
            height: 600,
            maximized: false,
        };
        let json = serde_json::to_string_pretty(&settings).unwrap();
        let path = window_settings_path_in(dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("\u{feff}{json}")).unwrap();

        let loaded = load_window_settings_from_path(&window_settings_path_in(dir)).unwrap();
        assert_eq!(loaded.x, settings.x);
        assert_eq!(loaded.y, settings.y);
        assert_eq!(loaded.width, settings.width);
        assert_eq!(loaded.height, settings.height);
    }

    #[test]
    fn test_load_window_settings_invalid_json_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        let path = window_settings_path_in(dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{not-json").unwrap();

        assert!(load_window_settings_from_path(&window_settings_path_in(dir)).is_none());
    }

    #[test]
    fn test_load_window_settings_falls_back_to_backup() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        let settings = WindowSettings {
            x: 5,
            y: 6,
            width: 700,
            height: 500,
            maximized: false,
        };

        let path = window_settings_path_in(dir);
        let backup_path = window_settings_backup_path(&path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &backup_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();
        fs::write(&path, "{not-json").unwrap();

        let loaded = load_window_settings_from_path(&window_settings_path_in(dir)).unwrap();
        assert_eq!(loaded.x, settings.x);
        assert_eq!(loaded.y, settings.y);
        assert_eq!(loaded.width, settings.width);
        assert_eq!(loaded.height, settings.height);
    }

    #[test]
    fn test_load_window_settings_recovers_from_backup_when_primary_missing() {
        // MDD-BUG-CRUCIBLE-00041: the save protocol can crash after moving the
        // primary to *.json.bak but before renaming the temp file into place,
        // leaving the primary absent and only the backup present. The loader
        // must recover from the backup in that window, not return None.
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        let settings = WindowSettings {
            x: 11,
            y: 22,
            width: 640,
            height: 480,
            maximized: false,
        };

        let path = window_settings_path_in(dir);
        let backup_path = window_settings_backup_path(&path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &backup_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();
        // Primary intentionally NOT written — only the backup exists.
        assert!(!path.exists());

        let loaded = load_window_settings_from_path(&window_settings_path_in(dir))
            .expect("should recover geometry from backup");
        assert_eq!(loaded.x, settings.x);
        assert_eq!(loaded.y, settings.y);
        assert_eq!(loaded.width, settings.width);
        assert_eq!(loaded.height, settings.height);
    }

    #[test]
    fn test_load_window_settings_invalid_backup_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        let path = window_settings_path_in(dir);
        let backup_path = window_settings_backup_path(&path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{not-json").unwrap();
        fs::write(&backup_path, "{also-not-json").unwrap();

        assert!(load_window_settings_from_path(&window_settings_path_in(dir)).is_none());
    }

    #[test]
    fn test_load_window_settings_missing_file_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        assert!(load_window_settings_from_path(&window_settings_path_in(dir)).is_none());
    }

    #[test]
    fn test_save_window_settings_fails_when_data_dir_is_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("not_a_dir");
        fs::write(&file_path, "x").unwrap();

        let settings = WindowSettings {
            x: 1,
            y: 2,
            width: 640,
            height: 480,
            maximized: false,
        };
        let result = save_window_settings_to_path(&settings, &window_settings_path_in(&file_path));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Failed to create data directory"));
    }

    #[test]
    #[cfg(not(coverage))]
    fn test_window_settings_path_helpers() {
        let path = Path::new("/tmp/window.json");
        let backup = window_settings_backup_path(path);
        let temp = window_settings_temp_path(path);

        assert_eq!(
            backup.file_name().and_then(|name| name.to_str()),
            Some("window.json.bak")
        );
        // MDD-BUG-CRUCIBLE-00524: this used to assert the exact shared name
        // `window.json.tmp`, pinning the very collision that let two savers
        // clobber each other's staging file. The invariant that actually
        // matters is the shape plus UNIQUENESS, so assert both.
        let temp_name = temp.file_name().and_then(|name| name.to_str()).unwrap();
        assert!(
            temp_name.starts_with("window.json.") && temp_name.ends_with(".tmp"),
            "temp name must stay recognisable and land beside the target, got {temp_name}"
        );
        assert_eq!(
            temp.parent(),
            path.parent(),
            "temp must be in the same directory"
        );
        assert_ne!(
            window_settings_temp_path(path),
            temp,
            "two saves of the same target must not share a staging file"
        );
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    mod windows_registry_test_support {
        include!("windows_registry_test_support.rs");
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn RegDeleteTreeW(hkey: HKEY, sub_key: *const u16) -> u32;
    }

    fn delete_registry_tree(path: &str) {
        let wide = to_wide(path);
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, wide.as_ptr());
        }
    }

    fn with_test_registry_base<F: FnOnce(&str)>(f: F) {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = format!("Software\\gp64_test\\{}_{}", std::process::id(), id);
        f(&base);
        delete_registry_tree(&base);
    }

    #[test]
    fn test_registry_subkey_path_empty_uses_base() {
        assert_eq!(
            registry_subkey_path(REGISTRY_BASE_KEY, ""),
            REGISTRY_BASE_KEY
        );
    }

    #[test]
    fn test_registry_subkey_path_appends_subkey() {
        assert_eq!(
            registry_subkey_path(REGISTRY_BASE_KEY, "Window"),
            format!("{REGISTRY_BASE_KEY}\\Window")
        );
    }

    #[test]
    fn test_open_or_create_key_rejects_long_path() {
        if windows_registry_test_support::skip_registry_write_tests() {
            return;
        }
        with_test_registry_base(|base| {
            let long_segment = "A".repeat(512);
            let path = format!("{}\\{}", base, long_segment);
            let result = open_or_create_key(&path);
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(err.contains("RegCreateKeyExW failed"));
        });
    }

    #[test]
    fn test_set_reg_dword_invalid_handle_reports_error() {
        if windows_registry_test_support::skip_registry_write_tests() {
            return;
        }
        // A closed handle's value can be reused by a key another test opens
        // in parallel, which made this test flaky; null is never a key.
        let result = set_reg_dword(ptr::null_mut(), REGISTRY_VALUE_X, 1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("RegSetValueExW failed"));
    }

    #[test]
    fn test_save_window_settings_to_registry_open_failure() {
        if windows_registry_test_support::skip_registry_write_tests() {
            return;
        }
        with_test_registry_base(|base| {
            let long_subkey = "A".repeat(512);
            let settings = WindowSettings {
                x: 1,
                y: 2,
                width: 640,
                height: 480,
                maximized: false,
            };

            let result = save_window_settings_to_registry(base, &long_subkey, &settings);
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(err.contains("RegCreateKeyExW failed"));
        });
    }

    #[test]
    fn test_window_settings_default() {
        let settings = WindowSettings::default();
        assert_eq!(settings.width, 960);
        assert_eq!(settings.height, 720);
    }

    #[test]
    fn test_window_settings_storage_key_is_stable_and_safe() {
        let key = window_settings_storage_key(r"C:\Apps\FIRST.EXE");
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(key, window_settings_storage_key(r"C:\Apps\FIRST.EXE"));
        assert_ne!(key, window_settings_storage_key(r"C:\Apps\SECOND.EXE"));
    }

    #[test]
    fn test_window_settings_binary_format_is_versioned() {
        let settings = WindowSettings {
            x: -200,
            y: 150,
            width: 800,
            height: 600,
            maximized: true,
        };
        let data = encode_window_settings(&settings);
        assert_eq!(
            u32::from_le_bytes(data[..4].try_into().unwrap()),
            WINDOW_SETTINGS_BINARY_VERSION
        );
        assert_eq!(decode_window_settings(&data), Some(settings));

        let mut unknown_version = data;
        unknown_version[..4].copy_from_slice(&(WINDOW_SETTINGS_BINARY_VERSION + 1).to_le_bytes());
        assert!(decode_window_settings(&unknown_version).is_none());
        assert!(decode_window_settings(&data[..data.len() - 1]).is_none());
    }

    #[test]
    fn test_window_settings_binary_v1_migrates_maximized_to_false() {
        let mut data = [0u8; WINDOW_SETTINGS_BINARY_V1_LEN];
        for (slot, value) in [
            (0, 1u32),
            (1, -320i32 as u32),
            (2, 240i32 as u32),
            (3, 1280i32 as u32),
            (4, 720i32 as u32),
        ] {
            let start = slot * std::mem::size_of::<u32>();
            data[start..start + std::mem::size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
        }

        assert_eq!(
            decode_window_settings(&data),
            Some(WindowSettings {
                x: -320,
                y: 240,
                width: 1280,
                height: 720,
                maximized: false,
            })
        );
    }

    #[test]
    fn test_is_position_valid() {
        assert!(is_position_valid(100, 100, 800, 600));
        assert!(!is_position_valid(100, 100, 100, 100));
        assert!(!is_position_valid(-3000, 0, 800, 600));
        assert!(!is_position_valid(0, -3000, 800, 600));
        assert!(!is_position_valid(25001, 0, 800, 600));
        assert!(!is_position_valid(0, 25001, 800, 600));
        assert!(!is_position_valid(0, 0, 100, 600));
        assert!(!is_position_valid(0, 0, 800, 100));
        assert!(!is_position_valid(0, 0, 9000, 600));
        assert!(!is_position_valid(0, 0, 800, 9000));
    }

    #[test]
    fn test_save_and_load_window_settings_round_trip() {
        if windows_registry_test_support::skip_registry_write_tests() {
            return;
        }
        with_test_registry_base(|base| {
            let settings = WindowSettings {
                x: 12,
                y: 34,
                width: 1024,
                height: 768,
                maximized: false,
            };
            save_window_settings_to_registry(base, REGISTRY_MAIN_SUBKEY, &settings).unwrap();

            let loaded = load_window_settings_from_registry(base, REGISTRY_MAIN_SUBKEY).unwrap();
            assert_eq!(loaded, settings);
        });
    }

    #[test]
    fn test_saved_window_settings_ignore_partial_legacy_generation() {
        if windows_registry_test_support::skip_registry_write_tests() {
            return;
        }
        with_test_registry_base(|base| {
            let old = WindowSettings {
                x: 10,
                y: 20,
                width: 640,
                height: 480,
                maximized: false,
            };
            let new = WindowSettings {
                x: 30,
                y: 40,
                width: 800,
                height: 600,
                maximized: true,
            };
            save_window_settings_to_registry(base, REGISTRY_MAIN_SUBKEY, &old).unwrap();

            // Model a crashed or interleaved legacy save: only the position
            // reached the registry, while the size still belongs to `old`.
            let path = registry_subkey_path(base, REGISTRY_MAIN_SUBKEY);
            let key = open_or_create_key(&path).unwrap();
            set_reg_dword(key, REGISTRY_VALUE_X, new.x as u32).unwrap();
            set_reg_dword(key, REGISTRY_VALUE_Y, new.y as u32).unwrap();
            unsafe {
                RegCloseKey(key);
            }

            let loaded = load_window_settings_from_registry(base, REGISTRY_MAIN_SUBKEY).unwrap();
            assert!(
                loaded == old || loaded == new,
                "load returned a mixed save generation: {loaded:?}"
            );
        });
    }

    #[test]
    fn test_save_and_load_named_window_settings_round_trip() {
        if windows_registry_test_support::skip_registry_write_tests() {
            return;
        }
        with_test_registry_base(|base| {
            let first_settings = WindowSettings {
                x: 111,
                y: 222,
                width: 333,
                height: 444,
                maximized: true,
            };
            let second_settings = WindowSettings {
                x: 555,
                y: 666,
                width: 777,
                height: 888,
                maximized: false,
            };

            save_window_settings_to_registry(
                base,
                &named_window_settings_subkey(r"C:\Apps\FIRST.EXE"),
                &first_settings,
            )
            .unwrap();
            save_window_settings_to_registry(
                base,
                &named_window_settings_subkey(r"C:\Apps\SECOND.EXE"),
                &second_settings,
            )
            .unwrap();

            assert_eq!(
                load_window_settings_from_registry(
                    base,
                    &named_window_settings_subkey(r"C:\Apps\FIRST.EXE")
                ),
                Some(first_settings)
            );
            assert_eq!(
                load_window_settings_from_registry(
                    base,
                    &named_window_settings_subkey(r"C:\Apps\SECOND.EXE")
                ),
                Some(second_settings)
            );
            assert!(load_window_settings_from_registry(
                base,
                &named_window_settings_subkey(r"C:\Apps\THIRD.EXE")
            )
            .is_none());
        });
    }

    #[test]
    fn test_save_and_load_monitor_window_settings_round_trip() {
        if windows_registry_test_support::skip_registry_write_tests() {
            return;
        }
        with_test_registry_base(|base| {
            let settings = WindowSettings {
                x: -200,
                y: 150,
                width: 800,
                height: 600,
                maximized: false,
            };
            save_window_settings_to_registry(base, REGISTRY_MONITOR_SUBKEY, &settings).unwrap();

            let loaded = load_window_settings_from_registry(base, REGISTRY_MONITOR_SUBKEY).unwrap();
            assert_eq!(loaded, settings);
        });
    }

    #[test]
    fn test_load_window_settings_missing_returns_none() {
        with_test_registry_base(|base| {
            assert!(load_window_settings_from_registry(base, REGISTRY_MAIN_SUBKEY).is_none());
            assert!(load_window_settings_from_registry(base, REGISTRY_MONITOR_SUBKEY).is_none());
        });
    }

    #[test]
    fn test_load_window_settings_partial_values_returns_none_until_complete() {
        if windows_registry_test_support::skip_registry_write_tests() {
            return;
        }
        with_test_registry_base(|base| {
            let path = registry_subkey_path(base, REGISTRY_MAIN_SUBKEY);
            let key = open_or_create_key(&path).unwrap();

            set_reg_dword(key, REGISTRY_VALUE_X, 12).unwrap();
            assert!(load_window_settings_from_registry(base, REGISTRY_MAIN_SUBKEY).is_none());

            set_reg_dword(key, REGISTRY_VALUE_Y, 34).unwrap();
            assert!(load_window_settings_from_registry(base, REGISTRY_MAIN_SUBKEY).is_none());

            set_reg_dword(key, REGISTRY_VALUE_WIDTH, 640).unwrap();
            assert!(load_window_settings_from_registry(base, REGISTRY_MAIN_SUBKEY).is_none());

            set_reg_dword(key, REGISTRY_VALUE_HEIGHT, 480).unwrap();
            assert_eq!(
                load_window_settings_from_registry(base, REGISTRY_MAIN_SUBKEY),
                Some(WindowSettings {
                    x: 12,
                    y: 34,
                    width: 640,
                    height: 480,
                    maximized: false,
                })
            );

            unsafe {
                RegCloseKey(key);
            }
        });
    }
}
