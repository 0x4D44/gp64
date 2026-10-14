//! PC hardware for gp64.
pub mod bda;
pub mod cmos;
pub mod io;
pub mod joystick;
pub mod keyboard;
pub mod mouse;
pub mod pic;
pub mod serial;
pub mod sound;
pub mod sound_blaster;
pub mod system;
pub mod timer;
pub mod vga;
pub use cmos::{Cmos, CmosCheckpointState};
pub use io::IoBus;
pub use keyboard::{
    Keyboard, SharedKeyboardState, SHIFT_FLAG_ALT, SHIFT_FLAG_CAPS_LOCK, SHIFT_FLAG_CTRL,
    SHIFT_FLAG_NUM_LOCK, SHIFT_FLAG_SCROLL_LOCK, SHIFT_FLAG_SHIFT,
};
pub use mouse::{BusMouse, Ps2Mouse, SharedMouseState};
pub use pic::{Pic, Pic8259};
pub use sound::PcSpeaker;
pub use sound_blaster::SoundBlaster;
pub use system::{GuestResetSource, SystemControl};
pub use timer::Pit;
pub use vga::Vga;
gp64_types::define_trace_checker!(pub kbd_flow_trace_enabled, "gp64::kbd::flow", tracing::Level::TRACE);
