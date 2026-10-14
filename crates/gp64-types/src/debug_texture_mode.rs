/// Triangle-texture diagnostics exposed by the F12 graphics menu.
///
/// The discriminants are part of the cross-thread wire contract: keep
/// `Off = 0`, `Flat = 1`, and `Edges = 2` stable because the frontend stores
/// this mode in an [`AtomicU8`](std::sync::atomic::AtomicU8).
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DebugTextureMode {
    /// Normal textured rendering with no diagnostic overlay.
    #[default]
    Off = 0,
    /// Flat white triangles with magenta borders.
    Flat = 1,
    /// Normal textured triangles with magenta triangle edges.
    Edges = 2,
}

impl DebugTextureMode {
    /// Stable encoding for cross-thread atomic storage.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Flat => 1,
            Self::Edges => 2,
        }
    }

    /// Decode cross-thread atomic storage, treating invalid values as `Off`.
    pub const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Flat,
            2 => Self::Edges,
            _ => Self::Off,
        }
    }

    /// Exact value shown by the F12 graphics menu.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Flat => "FLAT",
            Self::Edges => "EDGES",
        }
    }

    /// Move one position in the menu's forward or reverse cycle.
    pub const fn cycle(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::Off, true) => Self::Flat,
            (Self::Flat, true) => Self::Edges,
            (Self::Edges, true) => Self::Off,
            (Self::Off, false) => Self::Edges,
            (Self::Flat, false) => Self::Off,
            (Self::Edges, false) => Self::Flat,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DebugTextureMode;

    #[test]
    fn encoding_and_default_are_stable() {
        assert_eq!(DebugTextureMode::default(), DebugTextureMode::Off);
        assert_eq!(DebugTextureMode::Off.as_u8(), 0);
        assert_eq!(DebugTextureMode::Flat.as_u8(), 1);
        assert_eq!(DebugTextureMode::Edges.as_u8(), 2);
    }

    #[test]
    fn invalid_encoding_decodes_to_off() {
        assert_eq!(DebugTextureMode::from_u8(3), DebugTextureMode::Off);
        assert_eq!(DebugTextureMode::from_u8(u8::MAX), DebugTextureMode::Off);
    }

    #[test]
    fn cycle_moves_forward_and_backward() {
        assert_eq!(DebugTextureMode::Off.cycle(true), DebugTextureMode::Flat);
        assert_eq!(DebugTextureMode::Flat.cycle(true), DebugTextureMode::Edges);
        assert_eq!(DebugTextureMode::Edges.cycle(true), DebugTextureMode::Off);
        assert_eq!(DebugTextureMode::Off.cycle(false), DebugTextureMode::Edges);
        assert_eq!(DebugTextureMode::Edges.cycle(false), DebugTextureMode::Flat);
        assert_eq!(DebugTextureMode::Flat.cycle(false), DebugTextureMode::Off);
    }

    #[test]
    fn labels_are_exact_and_distinct() {
        assert_eq!(DebugTextureMode::Off.label(), "OFF");
        assert_eq!(DebugTextureMode::Flat.label(), "FLAT");
        assert_eq!(DebugTextureMode::Edges.label(), "EDGES");
        assert_ne!(
            DebugTextureMode::Off.label(),
            DebugTextureMode::Flat.label()
        );
        assert_ne!(
            DebugTextureMode::Flat.label(),
            DebugTextureMode::Edges.label()
        );
    }
}
