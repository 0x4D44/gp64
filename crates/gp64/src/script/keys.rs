//! Script key names to XT set-1 scancodes, and text to keystrokes.
use gp64_hw::keyboard::Keyboard;
use gp64_types::util::ascii_needs_shift;

const LEFT_SHIFT: KeyDef = KeyDef {
    scancode: 0x2A,
    extended: false,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyDef {
    pub scancode: u8,
    /// Sent with the E0 prefix.
    pub extended: bool,
}
impl KeyDef {
    /// The make or break bytes, and the index the keyboard tracks it by.
    pub fn bytes(self, down: bool) -> (Vec<u8>, usize) {
        let byte = self.scancode | if down { 0 } else { 0x80 };
        let bytes = if self.extended {
            vec![0xE0, byte]
        } else {
            vec![byte]
        };
        (bytes, usize::from(self.scancode))
    }
}

/// Looks up a key by name, ignoring case.
pub fn lookup(name: &str) -> Option<KeyDef> {
    let (scancode, extended) = match name.to_ascii_lowercase().as_str() {
        "a" => (0x1E, false),
        "b" => (0x30, false),
        "c" => (0x2E, false),
        "d" => (0x20, false),
        "e" => (0x12, false),
        "f" => (0x21, false),
        "g" => (0x22, false),
        "h" => (0x23, false),
        "i" => (0x17, false),
        "j" => (0x24, false),
        "k" => (0x25, false),
        "l" => (0x26, false),
        "m" => (0x32, false),
        "n" => (0x31, false),
        "o" => (0x18, false),
        "p" => (0x19, false),
        "q" => (0x10, false),
        "r" => (0x13, false),
        "s" => (0x1F, false),
        "t" => (0x14, false),
        "u" => (0x16, false),
        "v" => (0x2F, false),
        "w" => (0x11, false),
        "x" => (0x2D, false),
        "y" => (0x15, false),
        "z" => (0x2C, false),
        "0" => (0x0B, false),
        "1" => (0x02, false),
        "2" => (0x03, false),
        "3" => (0x04, false),
        "4" => (0x05, false),
        "5" => (0x06, false),
        "6" => (0x07, false),
        "7" => (0x08, false),
        "8" => (0x09, false),
        "9" => (0x0A, false),
        "enter" | "return" => (0x1C, false),
        "escape" | "esc" => (0x01, false),
        "space" | "spacebar" => (0x39, false),
        "backspace" | "bs" => (0x0E, false),
        "tab" => (0x0F, false),
        "capslock" => (0x3A, false),
        "numlock" => (0x45, false),
        "scrolllock" => (0x46, false),
        "minus" => (0x0C, false),
        "equal" | "equals" => (0x0D, false),
        "leftbracket" => (0x1A, false),
        "rightbracket" => (0x1B, false),
        "semicolon" => (0x27, false),
        "quote" | "apostrophe" => (0x28, false),
        "backquote" | "grave" | "tilde" => (0x29, false),
        "backslash" => (0x2B, false),
        "comma" => (0x33, false),
        "period" | "dot" => (0x34, false),
        "slash" => (0x35, false),
        "f1" => (0x3B, false),
        "f2" => (0x3C, false),
        "f3" => (0x3D, false),
        "f4" => (0x3E, false),
        "f5" => (0x3F, false),
        "f6" => (0x40, false),
        "f7" => (0x41, false),
        "f8" => (0x42, false),
        "f9" => (0x43, false),
        "f10" => (0x44, false),
        "f11" => (0x57, false),
        "f12" => (0x58, false),
        "shift" | "lshift" | "leftshift" => (0x2A, false),
        "rshift" | "rightshift" => (0x36, false),
        "ctrl" | "lctrl" | "leftctrl" | "control" => (0x1D, false),
        "rctrl" | "rightctrl" => (0x1D, true),
        "alt" | "lalt" | "leftalt" => (0x38, false),
        "ralt" | "rightalt" | "altgr" => (0x38, true),
        "up" | "uparrow" => (0x48, true),
        "down" | "downarrow" => (0x50, true),
        "left" | "leftarrow" => (0x4B, true),
        "right" | "rightarrow" => (0x4D, true),
        "home" => (0x47, true),
        "end" => (0x4F, true),
        "pageup" | "pgup" => (0x49, true),
        "pagedown" | "pgdn" => (0x51, true),
        "insert" | "ins" => (0x52, true),
        "delete" | "del" => (0x53, true),
        "break" | "cancel" => (0x46, true),
        "numpad0" => (0x52, false),
        "numpad1" => (0x4F, false),
        "numpad2" => (0x50, false),
        "numpad3" => (0x51, false),
        "numpad4" => (0x4B, false),
        "numpad5" => (0x4C, false),
        "numpad6" => (0x4D, false),
        "numpad7" => (0x47, false),
        "numpad8" => (0x48, false),
        "numpad9" => (0x49, false),
        "numpadenter" => (0x1C, true),
        "numpadplus" => (0x4E, false),
        "numpadminus" => (0x4A, false),
        "numpadmultiply" => (0x37, false),
        "numpaddivide" => (0x35, true),
        "numpaddot" => (0x53, false),
        _ => return None,
    };
    Some(KeyDef { scancode, extended })
}

/// The press and release transitions that type one character on a US
/// keyboard, with Left Shift around shifted characters. `None` for a
/// character with no key.
pub fn type_char(ch: char) -> Option<Vec<(KeyDef, bool)>> {
    let byte = u8::try_from(ch).ok().filter(u8::is_ascii)?;
    let (base, shift) = match ascii_needs_shift(byte) {
        Some(base) => (base, true),
        None => (byte, false),
    };
    let scancode = Keyboard::scancode_for_ascii(base);
    if scancode == 0 {
        return None;
    }
    let key = KeyDef {
        scancode,
        extended: false,
    };
    let mut strokes = Vec::with_capacity(4);
    if shift {
        strokes.push((LEFT_SHIFT, true));
    }
    strokes.extend([(key, true), (key, false)]);
    if shift {
        strokes.push((LEFT_SHIFT, false));
    }
    Some(strokes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scancode: u8, extended: bool) -> KeyDef {
        KeyDef { scancode, extended }
    }

    #[test]
    fn names_ignore_case_and_mark_extended_keys() {
        assert_eq!(lookup("ESCAPE"), Some(key(0x01, false)));
        assert_eq!(lookup("esc"), Some(key(0x01, false)));
        assert_eq!(lookup("Enter"), Some(key(0x1C, false)));
        assert_eq!(lookup("NumpadEnter"), Some(key(0x1C, true)));
        assert_eq!(lookup("Down"), Some(key(0x50, true)));
        assert_eq!(lookup("Numpad2"), Some(key(0x50, false)));
        assert_eq!(lookup("L"), Some(key(0x26, false)));
        assert_eq!(lookup("F12"), Some(key(0x58, false)));
        assert_eq!(lookup("RCtrl"), Some(key(0x1D, true)));
        assert_eq!(lookup("Hyper"), None);
    }

    #[test]
    fn bytes_carry_the_prefix_and_break_bit() {
        assert_eq!(key(0x50, true).bytes(true), (vec![0xE0, 0x50], 0x50));
        assert_eq!(key(0x50, true).bytes(false), (vec![0xE0, 0xD0], 0x50));
        assert_eq!(key(0x1C, false).bytes(false), (vec![0x9C], 0x1C));
    }

    #[test]
    fn typing_wraps_shifted_characters_in_left_shift() {
        let a = key(0x1E, false);
        assert_eq!(type_char('a'), Some(vec![(a, true), (a, false)]));
        assert_eq!(
            type_char('A'),
            Some(vec![
                (LEFT_SHIFT, true),
                (a, true),
                (a, false),
                (LEFT_SHIFT, false)
            ])
        );
        let one = key(0x02, false);
        assert_eq!(
            type_char('!'),
            Some(vec![
                (LEFT_SHIFT, true),
                (one, true),
                (one, false),
                (LEFT_SHIFT, false)
            ])
        );
        let enter = key(0x1C, false);
        assert_eq!(type_char('\n'), Some(vec![(enter, true), (enter, false)]));
        assert_eq!(type_char('é'), None);
        assert_eq!(type_char('\u{7}'), None);
    }
}
