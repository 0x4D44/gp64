//! The window's place and size kept between runs, through the per-user
//! store in [`gp64_types::persistence`] (the registry on Windows, a file
//! elsewhere).
//!
//! The position is in the desktop's own units: physical pixels on Windows
//! and X11, points on macOS, so a window comes back where it was on any
//! monitor. The size is logical, so a window keeps its apparent size when
//! it reopens on a monitor of a different scale. Both describe the normal
//! (restored) window; `maximized` says whether to maximize it after.
use gp64_types::WindowSettings;
use winit::{
    dpi::{LogicalSize, PhysicalPosition, Position},
    monitor::MonitorHandle,
    window::{Window, WindowAttributes},
};

/// The saved size is used only within these logical bounds.
const SIZE_RANGE: std::ops::RangeInclusive<i32> = 64..=16384;
/// A restored window must have this point, relative to its top-left
/// corner, on some monitor, so its title bar can still be grabbed.
const GRAB_POINT: (i32, i32) = (32, 8);

/// A monitor's area in desktop units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Area {
    fn contains(&self, x: i32, y: i32) -> bool {
        let (x, y) = (i64::from(x), i64::from(y));
        let (left, top) = (i64::from(self.x), i64::from(self.y));
        x >= left
            && y >= top
            && x < left + i64::from(self.width)
            && y < top + i64::from(self.height)
    }
}

/// What to apply to a new window from the saved settings.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    pub maximized: bool,
}

/// The saved position is used only where its title bar lands on one of
/// `monitors`, so a window saved on a monitor since unplugged reopens where
/// the system puts it. Each part is checked alone: a bad size does not
/// cost a good position.
pub fn plan(saved: Option<&WindowSettings>, monitors: &[Area]) -> Plan {
    let Some(saved) = saved else {
        return Plan::default();
    };
    let grab = (
        saved.x.saturating_add(GRAB_POINT.0),
        saved.y.saturating_add(GRAB_POINT.1),
    );
    Plan {
        position: monitors
            .iter()
            .any(|monitor| monitor.contains(grab.0, grab.1))
            .then_some((saved.x, saved.y)),
        size: (SIZE_RANGE.contains(&saved.width) && SIZE_RANGE.contains(&saved.height))
            .then_some((saved.width, saved.height)),
        maximized: saved.maximized,
    }
}

/// Follows the window's normal placement while it runs.
pub struct Tracker {
    /// What the store held at startup, so an unchanged window writes nothing.
    loaded: Option<WindowSettings>,
    current: Option<WindowSettings>,
    /// The window moved or resized since the last [`Tracker::sample`].
    stale: bool,
}

impl Tracker {
    /// Loads the saved placement and applies it to `attributes`.
    pub fn restore(
        attributes: WindowAttributes,
        monitors: impl Iterator<Item = MonitorHandle>,
        default_size: LogicalSize<f64>,
    ) -> (Self, WindowAttributes) {
        let loaded = gp64_types::load_window_settings();
        let monitors: Vec<Area> = monitors.map(|monitor| desktop_area(&monitor)).collect();
        let plan = plan(loaded.as_ref(), &monitors);
        let size = plan.size.map_or(default_size, |(width, height)| {
            LogicalSize::new(f64::from(width), f64::from(height))
        });
        let mut attributes = attributes
            .with_inner_size(size)
            .with_maximized(plan.maximized);
        if let Some((x, y)) = plan.position {
            attributes = attributes.with_position(desktop_position(x, y));
        }
        let tracker = Self {
            current: loaded.clone(),
            loaded,
            // Record where the window actually opened, even if never moved.
            stale: true,
        };
        (tracker, attributes)
    }

    /// Notes a move or resize; [`Tracker::sample`] reads it later.
    pub fn changed(&mut self) {
        self.stale = true;
    }

    /// Reads the window's placement after a change. Called once events are
    /// drained, so a maximize or minimize in progress has already updated
    /// the window's state and its geometry is not taken as the normal one.
    pub fn sample(&mut self, window: &Window) {
        if !std::mem::take(&mut self.stale)
            || window.fullscreen().is_some()
            || window.is_minimized() == Some(true)
        {
            return;
        }
        let mut settings = self.current.clone().unwrap_or_default();
        settings.maximized = window.is_maximized();
        if !settings.maximized {
            // Positions are unavailable on Wayland; keep the last known one.
            if let Ok(position) = window.outer_position() {
                (settings.x, settings.y) = from_window_position(window, position);
            }
            let size = window.inner_size().to_logical::<f64>(window.scale_factor());
            settings.width = size.width.round() as i32;
            settings.height = size.height.round() as i32;
        }
        self.current = Some(settings);
    }

    /// Stores the placement if it changed; a failure costs only persistence.
    pub fn save(&self) {
        if let Some(current) = self
            .current
            .as_ref()
            .filter(|current| self.loaded.as_ref() != Some(*current))
        {
            if let Err(error) = gp64_types::save_window_settings(current) {
                eprintln!("gp64: cannot save window placement: {error}");
            }
        }
    }
}

/// macOS positions windows in points; elsewhere in physical pixels.
#[cfg(target_os = "macos")]
fn desktop_area(monitor: &MonitorHandle) -> Area {
    let scale = monitor.scale_factor();
    let position = monitor.position().to_logical::<f64>(scale);
    let size = monitor.size().to_logical::<f64>(scale);
    Area {
        x: position.x.round() as i32,
        y: position.y.round() as i32,
        width: size.width.round() as i32,
        height: size.height.round() as i32,
    }
}
#[cfg(not(target_os = "macos"))]
fn desktop_area(monitor: &MonitorHandle) -> Area {
    let (position, size) = (monitor.position(), monitor.size());
    Area {
        x: position.x,
        y: position.y,
        width: i32::try_from(size.width).unwrap_or(i32::MAX),
        height: i32::try_from(size.height).unwrap_or(i32::MAX),
    }
}
#[cfg(target_os = "macos")]
fn desktop_position(x: i32, y: i32) -> Position {
    winit::dpi::LogicalPosition::new(f64::from(x), f64::from(y)).into()
}
#[cfg(not(target_os = "macos"))]
fn desktop_position(x: i32, y: i32) -> Position {
    PhysicalPosition::new(x, y).into()
}
#[cfg(target_os = "macos")]
fn from_window_position(window: &Window, position: PhysicalPosition<i32>) -> (i32, i32) {
    let position = position.to_logical::<f64>(window.scale_factor());
    (position.x.round() as i32, position.y.round() as i32)
}
#[cfg(not(target_os = "macos"))]
fn from_window_position(_window: &Window, position: PhysicalPosition<i32>) -> (i32, i32) {
    (position.x, position.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Area = Area {
        x: -1920,
        y: 0,
        width: 1920,
        height: 1080,
    };
    const MAIN: Area = Area {
        x: 0,
        y: 0,
        width: 2560,
        height: 1440,
    };

    fn saved(x: i32, y: i32, width: i32, height: i32, maximized: bool) -> WindowSettings {
        WindowSettings {
            x,
            y,
            width,
            height,
            maximized,
        }
    }

    #[test]
    fn nothing_saved_leaves_placement_to_the_system() {
        assert_eq!(plan(None, &[MAIN]), Plan::default());
    }

    #[test]
    fn a_window_on_any_monitor_returns_there() {
        // The grab point lands on, or just inside, each monitor edge.
        for (x, y) in [
            (100, 200),
            (-1800, 40),
            (-1920 - 32, -8),
            (-7, 0),
            (2560 - 33, 1440 - 9),
        ] {
            assert_eq!(
                plan(Some(&saved(x, y, 960, 720, false)), &[LEFT, MAIN]),
                Plan {
                    position: Some((x, y)),
                    size: Some((960, 720)),
                    maximized: false,
                },
                "{x},{y}"
            );
        }
    }

    #[test]
    fn a_window_whose_title_bar_is_off_every_monitor_is_placed_by_the_system() {
        // The left monitor is gone; the grab point is just past each edge.
        for (x, y) in [
            (-1800, 40),
            (2560 - 32, 100),
            (100, 1440 - 8),
            (-40, 0),
            (0, -9),
        ] {
            let plan = plan(Some(&saved(x, y, 960, 720, true)), &[MAIN]);
            assert_eq!(plan.position, None, "{x},{y}");
            assert_eq!(plan.size, Some((960, 720)));
            assert!(plan.maximized);
        }
        assert_eq!(
            plan(Some(&saved(100, 100, 960, 720, false)), &[]).position,
            None
        );
    }

    #[test]
    fn an_unusable_size_falls_back_alone() {
        for (width, height) in [(0, 720), (960, 63), (16385, 720), (-1, -1)] {
            let plan = plan(Some(&saved(100, 100, width, height, false)), &[MAIN]);
            assert_eq!(plan.size, None, "{width}x{height}");
            assert_eq!(plan.position, Some((100, 100)));
        }
        assert_eq!(
            plan(Some(&saved(100, 100, 64, 16384, false)), &[MAIN]).size,
            Some((64, 16384))
        );
    }

    #[test]
    fn extreme_saved_positions_do_not_overflow() {
        // Wrapping past i32::MAX would land the grab point on this monitor.
        let far = Area {
            x: i32::MIN,
            y: i32::MIN,
            width: 100,
            height: 100,
        };
        let plan = plan(Some(&saved(i32::MAX, i32::MAX, 960, 720, false)), &[far]);
        assert_eq!(plan.position, None);
        let huge = Area {
            x: i32::MAX - 10,
            y: 0,
            width: i32::MAX,
            height: 100,
        };
        assert!(huge.contains(i32::MAX, 50));
    }
}
