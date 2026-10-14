use super::*;
use gp64_memory::Memory;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn first_nonzero_row_char() -> u8 {
    for ch in 0u8..=u8::MAX {
        if CP437_FONT_8X8[ch as usize * 8] != 0 {
            return ch;
        }
    }
    0
}

fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

struct EnvGuard {
    key: &'static str,
    previous_env: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let lock = env_lock();
        let previous_env = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self {
            key,
            previous_env,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous_env {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

enum OverrideKind {
    RenderMode,
    CgaHeaderBytes,
}

struct OverrideGuard {
    kind: OverrideKind,
    previous: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl OverrideGuard {
    fn render_mode(value: Option<&str>) -> Self {
        let lock = env_lock();
        let previous = force_render_mode_override();
        set_force_render_mode_override(value.map(str::to_owned));
        Self {
            kind: OverrideKind::RenderMode,
            previous,
            _lock: lock,
        }
    }

    fn cga_header_bytes(value: Option<&str>) -> Self {
        let lock = env_lock();
        let previous = cga_header_bytes_override();
        set_cga_header_bytes_override(value.map(str::to_owned));
        Self {
            kind: OverrideKind::CgaHeaderBytes,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for OverrideGuard {
    fn drop(&mut self) {
        match self.kind {
            OverrideKind::RenderMode => set_force_render_mode_override(self.previous.take()),
            OverrideKind::CgaHeaderBytes => set_cga_header_bytes_override(self.previous.take()),
        }
    }
}

mod core_modes_and_text;
mod crtc_and_scanlines;
mod pixel_modes_and_palette;
mod pixel_writes_and_teletype;
mod register_ports_and_attributes;
mod rendering_and_crtc;
mod scanline_modes;
