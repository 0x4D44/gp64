//! PC wiring and named device sources for the shared 8259A chip model.

pub use gp64_pic::{Pic8259, PicChipSnapshot, PicSnapshot};
/// PC/AT interrupt controller pair, including BIOS power-on defaults.
pub type Pic = gp64_pic::Pic<PcPicWiring>;

#[derive(Clone, Debug)]
pub struct PcPicWiring;
impl gp64_pic::PicWiring for PcPicWiring {
    const VECTOR_OFFSETS: [u8; 2] = [0x08, 0x70];
    // Unmask the BIOS timer and keyboard, preserving the PC power-on state.
    const INITIAL_MASKS: [u8; 2] = [0xfc, 0xff];
    const ELCR_WRITABLE_MASKS: [u8; 2] = [0xf8, 0xde];
    const COMMAND_PORTS: [u16; 2] = [0x20, 0xa0];
    const DATA_PORTS: [u16; 2] = [0x21, 0xa1];
    const ELCR_PORTS: [u16; 2] = [0x4d0, 0x4d1];
    // The emulated BIOS has no slave spurious handler.
    const DROP_SPURIOUS_CASCADE: bool = true;
}

/// Independently driven device levels that may share one PIC input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PicIrqSource {
    Opl,
    Keyboard,
    AuxMouse,
    Serial,
    SoundBlaster,
}

impl PicIrqSource {
    const fn id(self) -> u8 {
        match self {
            Self::Opl => 3,
            Self::Keyboard => 7,
            Self::AuxMouse => 8,
            Self::Serial => 9,
            Self::SoundBlaster => 10,
        }
    }
}

impl From<PicIrqSource> for gp64_pic::IrqSource {
    #[inline]
    fn from(source: PicIrqSource) -> Self {
        Self::new(source.id())
    }
}
