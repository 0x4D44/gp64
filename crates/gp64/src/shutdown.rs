//! Signal handlers only set an atomic flag; normal threads finalise capture.
use std::sync::atomic::{AtomicBool, Ordering};
static REQUESTED: AtomicBool = AtomicBool::new(false);
pub fn requested() -> bool {
    REQUESTED.load(Ordering::Acquire)
}
#[cfg(unix)]
extern "C" fn handle(_: std::ffi::c_int) {
    REQUESTED.store(true, Ordering::Release);
}
#[cfg(unix)]
pub fn install() -> std::io::Result<()> {
    unsafe extern "C" {
        fn signal(signal: std::ffi::c_int, handler: usize) -> usize;
    }
    for number in [2, 15] {
        // SAFETY: these are SIGINT/SIGTERM on our Unix targets. The handler
        // has C ABI, remains valid for process lifetime and only stores an atomic.
        if unsafe { signal(number, handle as *const () as usize) } == usize::MAX {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}
#[cfg(windows)]
unsafe extern "system" fn handle(event: u32) -> i32 {
    if event <= 2 {
        REQUESTED.store(true, Ordering::Release);
        1
    } else {
        0
    }
}
#[cfg(windows)]
pub fn install() -> std::io::Result<()> {
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(handler: unsafe extern "system" fn(u32) -> i32, add: i32) -> i32;
    }
    // SAFETY: the callback has the required ABI and process lifetime.
    if unsafe { SetConsoleCtrlHandler(handle, 1) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
