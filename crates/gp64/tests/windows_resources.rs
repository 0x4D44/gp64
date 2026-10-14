//! The Windows executable carries the icon and version resources build.rs
//! embeds; Explorer, the taskbar and the window icon all read them.
#![cfg(all(windows, target_env = "msvc"))]

use std::os::windows::ffi::OsStrExt;

const RT_ICON: usize = 3;
const RT_GROUP_ICON: usize = 14;
const RT_VERSION: usize = 16;
const LOAD_LIBRARY_AS_DATAFILE: u32 = 0x0000_0002;

#[link(name = "Kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(name: *const u16, file: isize, flags: u32) -> isize;
    fn FreeLibrary(module: isize) -> i32;
    fn FindResourceW(module: isize, name: usize, kind: usize) -> isize;
    fn SizeofResource(module: isize, resource: isize) -> u32;
    fn LoadResource(module: isize, resource: isize) -> isize;
    fn LockResource(data: isize) -> *const u8;
}

/// Reads one resource with ordinal type and name, or None when absent.
fn resource(module: isize, kind: usize, name: usize) -> Option<Vec<u8>> {
    // SAFETY: the module is a live data-file mapping; the resource pointer
    // stays valid until FreeLibrary and spans SizeofResource bytes.
    unsafe {
        let found = FindResourceW(module, name, kind);
        if found == 0 {
            return None;
        }
        let size = SizeofResource(module, found) as usize;
        let data = LockResource(LoadResource(module, found));
        Some(std::slice::from_raw_parts(data, size).to_vec())
    }
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
fn executable_has_icon_and_version_resources() {
    let path: Vec<u16> = std::path::Path::new(env!("CARGO_BIN_EXE_gp64"))
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect();
    // SAFETY: a NUL-terminated path; the mapping is released below.
    let module = unsafe { LoadLibraryExW(path.as_ptr(), 0, LOAD_LIBRARY_AS_DATAFILE) };
    assert_ne!(module, 0, "load gp64.exe as data");

    let group = resource(module, RT_GROUP_ICON, 1).expect("icon group 1");
    let count = u16::from_le_bytes([group[4], group[5]]) as usize;
    let sizes: Vec<u8> = (0..count).map(|i| group[6 + i * 14]).collect();
    // A 256-pixel icon records its size as 0.
    assert_eq!(sizes, [16, 32, 48, 64, 0]);
    for i in 0..count {
        let id = u16::from_le_bytes([group[6 + i * 14 + 12], group[6 + i * 14 + 13]]);
        assert!(resource(module, RT_ICON, id.into()).is_some(), "icon {id}");
    }

    let version = resource(module, RT_VERSION, 1).expect("version information");
    let has = |text: &str| version.windows(text.len() * 2).any(|w| w == utf16(text));
    assert!(has("VS_VERSION_INFO"));
    assert!(has("FileDescription"));
    assert!(has(env!("CARGO_PKG_VERSION")));
    assert!(
        version
            .windows(4)
            .any(|w| w == 0xFEEF_04BDu32.to_le_bytes()),
        "fixed file information"
    );

    // SAFETY: the module came from LoadLibraryExW above.
    unsafe { FreeLibrary(module) };
}
