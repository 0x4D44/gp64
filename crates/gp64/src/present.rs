//! UI-thread presentation of the frontend's frozen guest frame and host overlays.
use crate::{
    overlay,
    scaler::{Blitter, Scaler},
};
use anyhow::Result;
use std::{num::NonZeroU32, sync::Arc};
use winit::{dpi::PhysicalSize, window::Window};

/// Borrowed UI-owned pixels in 0x00RRGGBB format; no emulation lock crosses here.
#[derive(Clone, Copy)]
pub struct Frame<'a> {
    pub pixels: &'a [u32],
    pub width: usize,
    pub height: usize,
}

/// Presentation backend owned by the frontend, with all drawing behind it.
pub enum Presenter {
    Software(Software),
}
impl Presenter {
    pub fn new(window: Arc<Window>) -> Result<Self> {
        Ok(Self::Software(Software::new(window)?))
    }

    pub fn present(
        &mut self,
        frame: Frame<'_>,
        size: PhysicalSize<u32>,
        scaler: Scaler,
        overlays: overlay::Layers<'_>,
    ) -> Result<()> {
        match self {
            Self::Software(software) => software.present(frame, size, scaler, overlays),
        }
    }
}

/// The existing softbuffer path, including its cached CPU scaler state.
pub struct Software {
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    blitter: Blitter,
}
impl Software {
    fn new(window: Arc<Window>) -> Result<Self> {
        let context = softbuffer::Context::new(window.clone())
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let surface = softbuffer::Surface::new(&context, window)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Self {
            surface,
            blitter: Blitter::default(),
        })
    }

    fn present(
        &mut self,
        frame: Frame<'_>,
        size: PhysicalSize<u32>,
        scaler: Scaler,
        overlays: overlay::Layers<'_>,
    ) -> Result<()> {
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return Ok(());
        };
        self.surface
            .resize(width, height)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Self::render(
            &mut self.blitter,
            frame,
            &mut buffer,
            size,
            scaler,
            overlays,
        );
        buffer
            .present()
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    /// The exact buffer drawing path, also usable without a native window in tests.
    fn render(
        blitter: &mut Blitter,
        frame: Frame<'_>,
        buffer: &mut [u32],
        size: PhysicalSize<u32>,
        scaler: Scaler,
        overlays: overlay::Layers<'_>,
    ) {
        let (width, height) = (size.width as usize, size.height as usize);
        // Softbuffer may return an age-zero buffer with unspecified contents.
        // Blitter clears it once, including when no guest frame exists yet.
        blitter.blit(
            frame.pixels,
            frame.width,
            frame.height,
            buffer,
            width,
            height,
            scaler,
        );
        // Overlays touch only the window buffer, never guest pixels or recordings.
        overlays.render(buffer, width, height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_pixels_match_inline_draw_for_all_scalers_and_overlays() {
        let source: Vec<u32> = (0..320 * 200).map(|i| (i * 757) & 0xffffff).collect();
        let original = source.clone();
        for scaler in Scaler::ALL {
            let mut blitter = Blitter::default();
            let mut old_blitter = Blitter::default();
            let mut actual = Vec::new();
            let mut expected = Vec::new();
            for (width, height) in [(997, 731), (701, 999), (80, 60), (1, 1), (997, 731)] {
                let size = PhysicalSize::new(width as u32, height as u32);
                actual.resize(width * height, 0xabcdef);
                expected.resize(width * height, 0x123456);
                for open in [true, false] {
                    let mut menu = overlay::Menu::default();
                    if open {
                        menu.toggle();
                    }
                    for (recording, pulse_ms) in
                        [(Some(61_500), 0), (Some(3_723_000), 500), (None, 1000)]
                    {
                        let values = overlay::Values {
                            scaler,
                            fullscreen: false,
                            mute: true,
                            mute_locked: true,
                            recording,
                            serial: overlay::Link::default(),
                            modem: overlay::Link::default(),
                        };
                        for frame in [
                            Frame {
                                pixels: &source,
                                width: 320,
                                height: 200,
                            },
                            Frame {
                                pixels: &[],
                                width: 0,
                                height: 0,
                            },
                        ] {
                            Software::render(
                                &mut blitter,
                                frame,
                                &mut actual,
                                size,
                                scaler,
                                overlay::Layers {
                                    menu: &menu,
                                    values: &values,
                                    pulse_ms: &|| pulse_ms,
                                },
                            );
                            // Independent orchestration of the old inline App::draw.
                            old_blitter.blit(
                                frame.pixels,
                                frame.width,
                                frame.height,
                                &mut expected,
                                width,
                                height,
                                scaler,
                            );
                            menu.render(&mut expected, width, height, &values);
                            overlay::render_recording(
                                &mut expected,
                                width,
                                height,
                                recording,
                                pulse_ms,
                            );
                            assert!(
                                actual == expected,
                                "{scaler:?} {width}x{height}, menu={open}, recording={recording:?}, pulse={pulse_ms}, source={}x{}",
                                frame.width,
                                frame.height,
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(
            source, original,
            "presentation must not change guest pixels"
        );
    }
}
