//! Physical keys to XT set-1 byte packets, including atomic E0 sequences.
use std::collections::HashSet;
use winit::keyboard::{KeyCode, ModifiersState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub bytes: Vec<u8>,
    pub index: usize,
    pub down: bool,
}
fn scan(key: KeyCode) -> Option<(u8, bool)> {
    use KeyCode::{
        AltLeft, AltRight, ArrowDown, ArrowLeft, ArrowRight, ArrowUp, Backquote, Backslash,
        Backspace, BracketLeft, BracketRight, CapsLock, Comma, ContextMenu, ControlLeft,
        ControlRight, Delete, Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7,
        Digit8, Digit9, End, Enter, Equal, Escape, Home, Insert, IntlBackslash, KeyA, KeyB, KeyC,
        KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR,
        KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ, Minus, NumLock, Numpad0, Numpad1, Numpad2,
        Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9, NumpadAdd, NumpadDecimal,
        NumpadDivide, NumpadEnter, NumpadMultiply, NumpadSubtract, PageDown, PageUp, Period, Quote,
        ScrollLock, Semicolon, ShiftLeft, ShiftRight, Slash, Space, SuperLeft, SuperRight, Tab, F1,
        F10, F11, F12, F2, F3, F4, F5, F6, F7, F8, F9,
    };
    Some(match key {
        Escape => (0x01, false),
        Digit1 => (0x02, false),
        Digit2 => (0x03, false),
        Digit3 => (0x04, false),
        Digit4 => (0x05, false),
        Digit5 => (0x06, false),
        Digit6 => (0x07, false),
        Digit7 => (0x08, false),
        Digit8 => (0x09, false),
        Digit9 => (0x0a, false),
        Digit0 => (0x0b, false),
        Minus => (0x0c, false),
        Equal => (0x0d, false),
        Backspace => (0x0e, false),
        Tab => (0x0f, false),
        KeyQ => (0x10, false),
        KeyW => (0x11, false),
        KeyE => (0x12, false),
        KeyR => (0x13, false),
        KeyT => (0x14, false),
        KeyY => (0x15, false),
        KeyU => (0x16, false),
        KeyI => (0x17, false),
        KeyO => (0x18, false),
        KeyP => (0x19, false),
        BracketLeft => (0x1a, false),
        BracketRight => (0x1b, false),
        Enter => (0x1c, false),
        ControlLeft => (0x1d, false),
        KeyA => (0x1e, false),
        KeyS => (0x1f, false),
        KeyD => (0x20, false),
        KeyF => (0x21, false),
        KeyG => (0x22, false),
        KeyH => (0x23, false),
        KeyJ => (0x24, false),
        KeyK => (0x25, false),
        KeyL => (0x26, false),
        Semicolon => (0x27, false),
        Quote => (0x28, false),
        Backquote => (0x29, false),
        ShiftLeft => (0x2a, false),
        Backslash => (0x2b, false),
        KeyZ => (0x2c, false),
        KeyX => (0x2d, false),
        KeyC => (0x2e, false),
        KeyV => (0x2f, false),
        KeyB => (0x30, false),
        KeyN => (0x31, false),
        KeyM => (0x32, false),
        Comma => (0x33, false),
        Period => (0x34, false),
        Slash => (0x35, false),
        ShiftRight => (0x36, false),
        NumpadMultiply => (0x37, false),
        AltLeft => (0x38, false),
        Space => (0x39, false),
        CapsLock => (0x3a, false),
        F1 => (0x3b, false),
        F2 => (0x3c, false),
        F3 => (0x3d, false),
        F4 => (0x3e, false),
        F5 => (0x3f, false),
        F6 => (0x40, false),
        F7 => (0x41, false),
        F8 => (0x42, false),
        F9 => (0x43, false),
        F10 => (0x44, false),
        NumLock => (0x45, false),
        ScrollLock => (0x46, false),
        Numpad7 => (0x47, false),
        Numpad8 => (0x48, false),
        Numpad9 => (0x49, false),
        NumpadSubtract => (0x4a, false),
        Numpad4 => (0x4b, false),
        Numpad5 => (0x4c, false),
        Numpad6 => (0x4d, false),
        NumpadAdd => (0x4e, false),
        Numpad1 => (0x4f, false),
        Numpad2 => (0x50, false),
        Numpad3 => (0x51, false),
        Numpad0 => (0x52, false),
        NumpadDecimal => (0x53, false),
        IntlBackslash => (0x56, false),
        F11 => (0x57, false),
        F12 => (0x58, false),
        NumpadEnter => (0x1c, true),
        ControlRight => (0x1d, true),
        NumpadDivide => (0x35, true),
        AltRight => (0x38, true),
        Home => (0x47, true),
        ArrowUp => (0x48, true),
        PageUp => (0x49, true),
        ArrowLeft => (0x4b, true),
        ArrowRight => (0x4d, true),
        End => (0x4f, true),
        ArrowDown => (0x50, true),
        PageDown => (0x51, true),
        Insert => (0x52, true),
        Delete => (0x53, true),
        SuperLeft => (0x5b, true),
        SuperRight => (0x5c, true),
        ContextMenu => (0x5d, true),
        _ => return None,
    })
}
pub fn packet(key: KeyCode, down: bool) -> Option<Packet> {
    if key == KeyCode::Pause {
        return Some(Packet {
            bytes: if down {
                vec![0xe1, 0x1d, 0x45, 0xe1, 0x9d, 0xc5]
            } else {
                vec![]
            },
            index: 0x45,
            down,
        });
    }
    if key == KeyCode::PrintScreen {
        return Some(Packet {
            bytes: if down {
                vec![0xe0, 0x2a, 0xe0, 0x37]
            } else {
                vec![0xe0, 0xb7, 0xe0, 0xaa]
            },
            index: 0x37,
            down,
        });
    }
    let (code, extended) = scan(key)?;
    let byte = code | if down { 0 } else { 0x80 };
    Some(Packet {
        bytes: if extended {
            vec![0xe0, byte]
        } else {
            vec![byte]
        },
        index: usize::from(code),
        down,
    })
}
#[derive(Debug, PartialEq, Eq)]
pub enum KeyAction {
    Forward,
    Consume,
    Hotkey,
}
#[derive(Default)]
pub struct Hotkeys {
    consumed: HashSet<KeyCode>,
}
impl Hotkeys {
    pub fn clear(&mut self) {
        self.consumed.clear();
    }
    pub fn event(
        &mut self,
        key: KeyCode,
        down: bool,
        repeat: bool,
        modifiers: ModifiersState,
    ) -> KeyAction {
        if (!down && self.consumed.remove(&key)) || (down && self.consumed.contains(&key)) {
            return KeyAction::Consume;
        }
        // F12 belongs to the host menu: the guest never sees it.
        let hotkey = key == KeyCode::F11
            || key == KeyCode::F12
            || (key == KeyCode::Enter && modifiers.alt_key())
            || (key == KeyCode::F10 && modifiers.control_key());
        if down && !repeat && hotkey {
            self.consumed.insert(key);
            KeyAction::Hotkey
        } else {
            KeyAction::Forward
        }
    }
}
#[derive(Default)]
pub struct Keyboard {
    held: HashSet<KeyCode>,
}
impl Keyboard {
    /// Repeated presses emit repeated makes. Unmatched releases are ignored.
    pub fn event(&mut self, key: KeyCode, down: bool) -> Option<Packet> {
        let packet = packet(key, down)?;
        if down {
            self.held.insert(key);
        } else if !self.held.remove(&key) {
            return None;
        }
        Some(packet)
    }
    pub fn release_all(&mut self) -> Vec<Packet> {
        self.held
            .drain()
            .filter_map(|key| packet(key, false))
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_mapped_key_matches_the_xt_layout() {
        use KeyCode::{
            AltLeft, AltRight, ArrowDown, ArrowLeft, ArrowRight, ArrowUp, Backquote, Backslash,
            Backspace, BracketLeft, BracketRight, CapsLock, Comma, ContextMenu, ControlLeft,
            ControlRight, Delete, Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7,
            Digit8, Digit9, End, Enter, Equal, Escape, Home, Insert, IntlBackslash, KeyA, KeyB,
            KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP,
            KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ, Minus, NumLock, Numpad0,
            Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
            NumpadAdd, NumpadDecimal, NumpadDivide, NumpadEnter, NumpadMultiply, NumpadSubtract,
            PageDown, PageUp, Pause, Period, PrintScreen, Quote, ScrollLock, Semicolon, ShiftLeft,
            ShiftRight, Slash, Space, SuperLeft, SuperRight, Tab, F1, F10, F11, F12, F2, F3, F4,
            F5, F6, F7, F8, F9,
        };
        // Ordered physical positions from the XT layout, rather than scanning
        // the implementation's mapping. Later positions are AT extensions.
        let keys = [
            Escape,
            Digit1,
            Digit2,
            Digit3,
            Digit4,
            Digit5,
            Digit6,
            Digit7,
            Digit8,
            Digit9,
            Digit0,
            Minus,
            Equal,
            Backspace,
            Tab,
            KeyQ,
            KeyW,
            KeyE,
            KeyR,
            KeyT,
            KeyY,
            KeyU,
            KeyI,
            KeyO,
            KeyP,
            BracketLeft,
            BracketRight,
            Enter,
            ControlLeft,
            KeyA,
            KeyS,
            KeyD,
            KeyF,
            KeyG,
            KeyH,
            KeyJ,
            KeyK,
            KeyL,
            Semicolon,
            Quote,
            Backquote,
            ShiftLeft,
            Backslash,
            KeyZ,
            KeyX,
            KeyC,
            KeyV,
            KeyB,
            KeyN,
            KeyM,
            Comma,
            Period,
            Slash,
            ShiftRight,
            NumpadMultiply,
            AltLeft,
            Space,
            CapsLock,
            F1,
            F2,
            F3,
            F4,
            F5,
            F6,
            F7,
            F8,
            F9,
            F10,
            NumLock,
            ScrollLock,
            Numpad7,
            Numpad8,
            Numpad9,
            NumpadSubtract,
            Numpad4,
            Numpad5,
            Numpad6,
            NumpadAdd,
            Numpad1,
            Numpad2,
            Numpad3,
            Numpad0,
            NumpadDecimal,
        ];
        for (offset, key) in keys.into_iter().enumerate() {
            let make = (offset + 1) as u8;
            assert_eq!(packet(key, true).unwrap().bytes, [make], "{key:?}");
            assert_eq!(packet(key, false).unwrap().bytes, [make | 0x80], "{key:?}");
        }
        for (key, code) in [(IntlBackslash, 0x56), (F11, 0x57), (F12, 0x58)] {
            assert_eq!(packet(key, true).unwrap().bytes, [code]);
            assert_eq!(packet(key, false).unwrap().bytes, [code | 0x80]);
        }
        let keys = [
            NumpadEnter,
            ControlRight,
            NumpadDivide,
            AltRight,
            Home,
            ArrowUp,
            PageUp,
            ArrowLeft,
            ArrowRight,
            End,
            ArrowDown,
            PageDown,
            Insert,
            Delete,
            SuperLeft,
            SuperRight,
            ContextMenu,
        ];
        let codes = [
            0x1c, 0x1d, 0x35, 0x38, 0x47, 0x48, 0x49, 0x4b, 0x4d, 0x4f, 0x50, 0x51, 0x52, 0x53,
            0x5b, 0x5c, 0x5d,
        ];
        for (key, code) in keys.into_iter().zip(codes) {
            assert_eq!(packet(key, true).unwrap().bytes, [0xe0, code]);
            assert_eq!(packet(key, false).unwrap().bytes, [0xe0, code | 0x80]);
        }
        assert_eq!(
            packet(PrintScreen, true).unwrap().bytes,
            [0xe0, 0x2a, 0xe0, 0x37]
        );
        assert_eq!(
            packet(PrintScreen, false).unwrap().bytes,
            [0xe0, 0xb7, 0xe0, 0xaa]
        );
        assert_eq!(
            packet(Pause, true).unwrap().bytes,
            [0xe1, 0x1d, 0x45, 0xe1, 0x9d, 0xc5]
        );
        assert!(packet(Pause, false).unwrap().bytes.is_empty());
        assert!(packet(KeyCode::F24, true).is_none());
    }
    #[test]
    fn modifier_added_during_repeat_preserves_guest_release() {
        for (key, modifier) in [
            (KeyCode::Enter, ModifiersState::ALT),
            (KeyCode::F10, ModifiersState::CONTROL),
        ] {
            let mut hotkeys = Hotkeys::default();
            let mut keyboard = Keyboard::default();
            assert_eq!(
                hotkeys.event(key, true, false, ModifiersState::empty()),
                KeyAction::Forward
            );
            assert!(keyboard.event(key, true).unwrap().down);
            assert_eq!(hotkeys.event(key, true, true, modifier), KeyAction::Forward);
            keyboard.event(key, true);
            assert_eq!(
                hotkeys.event(key, false, false, modifier),
                KeyAction::Forward
            );
            assert!(!keyboard.event(key, false).unwrap().down);
            assert!(keyboard.release_all().is_empty());
        }
    }
    #[test]
    fn initial_hotkeys_consume_repeat_and_release() {
        for (key, modifier) in [
            (KeyCode::Enter, ModifiersState::ALT),
            (KeyCode::F10, ModifiersState::CONTROL),
            (KeyCode::F11, ModifiersState::empty()),
            (KeyCode::F12, ModifiersState::empty()),
        ] {
            let mut hotkeys = Hotkeys::default();
            assert_eq!(hotkeys.event(key, true, false, modifier), KeyAction::Hotkey);
            assert_eq!(hotkeys.event(key, true, true, modifier), KeyAction::Consume);
            assert_eq!(
                hotkeys.event(key, false, false, ModifiersState::empty()),
                KeyAction::Consume
            );
            assert_eq!(hotkeys.event(key, true, false, modifier), KeyAction::Hotkey);
            hotkeys.clear();
            assert_eq!(
                hotkeys.event(key, false, false, modifier),
                KeyAction::Forward
            );
        }
    }
    #[test]
    fn menu_key_is_never_forwarded_to_the_guest() {
        // Mirrors the frontend's routing: only `Forward` reaches the keyboard.
        let mut hotkeys = Hotkeys::default();
        let mut keyboard = Keyboard::default();
        let mut guest = Vec::new();
        for modifiers in [
            ModifiersState::empty(),
            ModifiersState::SHIFT,
            ModifiersState::CONTROL,
            ModifiersState::ALT,
        ] {
            for (down, repeat) in [(true, false), (true, true), (true, true), (false, false)] {
                let action = hotkeys.event(KeyCode::F12, down, repeat, modifiers);
                assert_ne!(action, KeyAction::Forward, "{down} {repeat} {modifiers:?}");
                if action == KeyAction::Forward {
                    guest.extend(keyboard.event(KeyCode::F12, down));
                }
            }
        }
        assert!(guest.is_empty());
        assert!(keyboard.release_all().is_empty());
    }
    #[test]
    fn repeat_and_focus_loss() {
        let mut keyboard = Keyboard::default();
        assert!(keyboard.event(KeyCode::KeyA, false).is_none());
        for _ in 0..3 {
            assert_eq!(keyboard.event(KeyCode::KeyA, true).unwrap().bytes, [0x1e]);
        }
        keyboard.event(KeyCode::ArrowUp, true);
        let released = keyboard.release_all();
        assert_eq!(released.len(), 2);
        assert!(released.iter().any(|p| p.bytes == [0x9e]));
        assert!(released.iter().any(|p| p.bytes == [0xe0, 0xc8]));
        assert!(keyboard.release_all().is_empty());
    }
}
