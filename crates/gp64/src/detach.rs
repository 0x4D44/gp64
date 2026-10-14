//! On Windows a console launch hands the prompt back: gp64 checks its command
//! line, then reruns itself without a console and exits. The detached copy has
//! nowhere to print, so a failure is shown in a message box instead.
/// Marks the detached copy so it neither detaches again nor prints.
#[cfg(windows)]
const MARKER: &str = "GP64_DETACHED";
/// Whether this process is the detached copy.
#[cfg(windows)]
pub fn is_detached() -> bool {
    std::env::var_os(MARKER).is_some()
}
#[cfg(not(windows))]
pub fn is_detached() -> bool {
    false
}
/// Starts the detached copy with this run's arguments. False means this
/// process should run gp64 itself.
#[cfg(windows)]
pub fn relaunch() -> std::io::Result<bool> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> isize;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    if is_detached() {
        return Ok(false);
    }
    // The copy inherits every inheritable handle. A caller reading our output
    // through a pipe would otherwise wait until the game closes.
    for which in [-10i32, -11, -12] {
        // SAFETY: STD_INPUT/OUTPUT/ERROR_HANDLE; a null or invalid handle
        // only makes SetHandleInformation fail, which is harmless here.
        unsafe {
            SetHandleInformation(GetStdHandle(which as u32), HANDLE_FLAG_INHERIT, 0);
        }
    }
    Command::new(std::env::current_exe()?)
        .args(std::env::args_os().skip(1))
        .env(MARKER, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()?;
    Ok(true)
}
#[cfg(not(windows))]
pub fn relaunch() -> std::io::Result<bool> {
    Ok(false)
}
/// Shows a failure the detached copy cannot print.
#[cfg(windows)]
pub fn show_error(message: &str) {
    #[link(name = "User32")]
    unsafe extern "system" {
        fn MessageBoxW(window: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    const MB_ICONERROR: u32 = 0x0000_0010;
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, caption) = (wide(message.trim_end()), wide("gp64"));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call,
    // and a null owner window is allowed.
    unsafe {
        MessageBoxW(0, text.as_ptr(), caption.as_ptr(), MB_ICONERROR);
    }
}
#[cfg(not(windows))]
pub fn show_error(_: &str) {}
