//! Physical keys and their XT (set 1) scancodes, as the original's
//! `KeyMap.cs` / `ConsoleKeyHandler.cs`.
//!
//! Keys are named by position, after the W3C UI Events `code` values the
//! window toolkits report (`KeyA` is the key labelled A on a US keyboard,
//! wherever the local layout puts letters). The guest receives the
//! scancode of that position and applies **its own** layout, so a US, ABNT2
//! or any other keyboard types what its key caps show as long as the guest
//! uses the same layout.
//!
//! Scancodes are in QEMU's form, as the original sent them: keys with the
//! `E0` prefix have the high bit set (`E0 53`, Delete, is `0xD3`, the
//! original's `DEL_SCAN = 211`). Each key also has the US keysym, used only
//! with servers that do not take scancodes (text consoles of PV guests).

macro_rules! keys {
    ($( $name:ident = $scan:expr, $sym:expr, $shifted:expr; )*) => {
        /// A key by physical position.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum KeyCode { $($name),* }

        impl KeyCode {
            /// Every key.
            pub const ALL: &'static [KeyCode] = &[$(KeyCode::$name),*];

            /// The XT scancode in QEMU's form.
            pub fn scancode(self) -> u32 {
                match self { $(KeyCode::$name => $scan),* }
            }

            /// The US-layout keysym (with Shift held or not).
            pub fn keysym(self, shift: bool) -> u32 {
                match self { $(KeyCode::$name => if shift { $shifted } else { $sym }),* }
            }

            /// The W3C `code` name.
            pub fn name(self) -> &'static str {
                match self { $(KeyCode::$name => stringify!($name)),* }
            }
        }
    };
}

const fn ch(c: char) -> u32 {
    c as u32
}

keys! {
    Escape = 0x01, 0xff1b, 0xff1b;
    Digit1 = 0x02, ch('1'), ch('!');
    Digit2 = 0x03, ch('2'), ch('@');
    Digit3 = 0x04, ch('3'), ch('#');
    Digit4 = 0x05, ch('4'), ch('$');
    Digit5 = 0x06, ch('5'), ch('%');
    Digit6 = 0x07, ch('6'), ch('^');
    Digit7 = 0x08, ch('7'), ch('&');
    Digit8 = 0x09, ch('8'), ch('*');
    Digit9 = 0x0a, ch('9'), ch('(');
    Digit0 = 0x0b, ch('0'), ch(')');
    Minus = 0x0c, ch('-'), ch('_');
    Equal = 0x0d, ch('='), ch('+');
    Backspace = 0x0e, 0xff08, 0xff08;
    Tab = 0x0f, 0xff09, 0xff09;
    KeyQ = 0x10, ch('q'), ch('Q');
    KeyW = 0x11, ch('w'), ch('W');
    KeyE = 0x12, ch('e'), ch('E');
    KeyR = 0x13, ch('r'), ch('R');
    KeyT = 0x14, ch('t'), ch('T');
    KeyY = 0x15, ch('y'), ch('Y');
    KeyU = 0x16, ch('u'), ch('U');
    KeyI = 0x17, ch('i'), ch('I');
    KeyO = 0x18, ch('o'), ch('O');
    KeyP = 0x19, ch('p'), ch('P');
    BracketLeft = 0x1a, ch('['), ch('{');
    BracketRight = 0x1b, ch(']'), ch('}');
    Enter = 0x1c, 0xff0d, 0xff0d;
    ControlLeft = 0x1d, 0xffe3, 0xffe3;
    KeyA = 0x1e, ch('a'), ch('A');
    KeyS = 0x1f, ch('s'), ch('S');
    KeyD = 0x20, ch('d'), ch('D');
    KeyF = 0x21, ch('f'), ch('F');
    KeyG = 0x22, ch('g'), ch('G');
    KeyH = 0x23, ch('h'), ch('H');
    KeyJ = 0x24, ch('j'), ch('J');
    KeyK = 0x25, ch('k'), ch('K');
    KeyL = 0x26, ch('l'), ch('L');
    Semicolon = 0x27, ch(';'), ch(':');
    Quote = 0x28, ch('\''), ch('"');
    Backquote = 0x29, ch('`'), ch('~');
    ShiftLeft = 0x2a, 0xffe1, 0xffe1;
    Backslash = 0x2b, ch('\\'), ch('|');
    KeyZ = 0x2c, ch('z'), ch('Z');
    KeyX = 0x2d, ch('x'), ch('X');
    KeyC = 0x2e, ch('c'), ch('C');
    KeyV = 0x2f, ch('v'), ch('V');
    KeyB = 0x30, ch('b'), ch('B');
    KeyN = 0x31, ch('n'), ch('N');
    KeyM = 0x32, ch('m'), ch('M');
    Comma = 0x33, ch(','), ch('<');
    Period = 0x34, ch('.'), ch('>');
    Slash = 0x35, ch('/'), ch('?');
    ShiftRight = 0x36, 0xffe2, 0xffe2;
    NumpadMultiply = 0x37, 0xffaa, 0xffaa;
    AltLeft = 0x38, 0xffe9, 0xffe9;
    Space = 0x39, ch(' '), ch(' ');
    CapsLock = 0x3a, 0xffe5, 0xffe5;
    F1 = 0x3b, 0xffbe, 0xffbe;
    F2 = 0x3c, 0xffbf, 0xffbf;
    F3 = 0x3d, 0xffc0, 0xffc0;
    F4 = 0x3e, 0xffc1, 0xffc1;
    F5 = 0x3f, 0xffc2, 0xffc2;
    F6 = 0x40, 0xffc3, 0xffc3;
    F7 = 0x41, 0xffc4, 0xffc4;
    F8 = 0x42, 0xffc5, 0xffc5;
    F9 = 0x43, 0xffc6, 0xffc6;
    F10 = 0x44, 0xffc7, 0xffc7;
    NumLock = 0x45, 0xff7f, 0xff7f;
    ScrollLock = 0x46, 0xff14, 0xff14;
    Numpad7 = 0x47, 0xffb7, 0xffb7;
    Numpad8 = 0x48, 0xffb8, 0xffb8;
    Numpad9 = 0x49, 0xffb9, 0xffb9;
    NumpadSubtract = 0x4a, 0xffad, 0xffad;
    Numpad4 = 0x4b, 0xffb4, 0xffb4;
    Numpad5 = 0x4c, 0xffb5, 0xffb5;
    Numpad6 = 0x4d, 0xffb6, 0xffb6;
    NumpadAdd = 0x4e, 0xffab, 0xffab;
    Numpad1 = 0x4f, 0xffb1, 0xffb1;
    Numpad2 = 0x50, 0xffb2, 0xffb2;
    Numpad3 = 0x51, 0xffb3, 0xffb3;
    Numpad0 = 0x52, 0xffb0, 0xffb0;
    NumpadDecimal = 0x53, 0xffae, 0xffae;
    // The ISO key between left Shift and Z (`\|` on ABNT2, `<>` on many
    // European layouts).
    IntlBackslash = 0x56, ch('<'), ch('>');
    F11 = 0x57, 0xffc8, 0xffc8;
    F12 = 0x58, 0xffc9, 0xffc9;
    // The key left of right Shift on ABNT2 (`/?`) and JIS (`\_`) keyboards.
    IntlRo = 0x73, ch('/'), ch('?');
    NumpadEnter = 0x9c, 0xff8d, 0xff8d;
    ControlRight = 0x9d, 0xffe4, 0xffe4;
    NumpadDivide = 0xb5, 0xffaf, 0xffaf;
    PrintScreen = 0xb7, 0xff61, 0xff61;
    // AltGr on international layouts.
    AltRight = 0xb8, 0xffea, 0xffea;
    Pause = 0xc6, 0xff13, 0xff13;
    Home = 0xc7, 0xff50, 0xff50;
    ArrowUp = 0xc8, 0xff52, 0xff52;
    PageUp = 0xc9, 0xff55, 0xff55;
    ArrowLeft = 0xcb, 0xff51, 0xff51;
    ArrowRight = 0xcd, 0xff53, 0xff53;
    End = 0xcf, 0xff57, 0xff57;
    ArrowDown = 0xd0, 0xff54, 0xff54;
    PageDown = 0xd1, 0xff56, 0xff56;
    Insert = 0xd2, 0xff63, 0xff63;
    Delete = 0xd3, 0xffff, 0xffff;
    MetaLeft = 0xdb, 0xffeb, 0xffeb;
    MetaRight = 0xdc, 0xffec, 0xffec;
    ContextMenu = 0xdd, 0xff67, 0xff67;
}

impl KeyCode {
    /// Shift, Control, Alt or Meta (either side).
    pub fn is_modifier(self) -> bool {
        matches!(
            self,
            Self::ShiftLeft
                | Self::ShiftRight
                | Self::ControlLeft
                | Self::ControlRight
                | Self::AltLeft
                | Self::AltRight
                | Self::MetaLeft
                | Self::MetaRight
        )
    }

    /// The key (and whether Shift is needed) that types `c` with a US
    /// layout in the guest. Numpad keys are never chosen.
    pub fn for_us_char(c: char) -> Option<(KeyCode, bool)> {
        let wanted = match c {
            '\n' | '\r' => return Some((Self::Enter, false)),
            '\t' => return Some((Self::Tab, false)),
            c if (' '..='~').contains(&c) => u32::from(c),
            _ => return None,
        };
        Self::ALL
            .iter()
            .filter(|k| !k.name().starts_with("Numpad") && !k.name().starts_with("Intl"))
            .find_map(|&k| {
                if k.keysym(false) == wanted {
                    Some((k, false))
                } else if k.keysym(true) == wanted {
                    Some((k, true))
                } else {
                    None
                }
            })
    }
}

/// The key presses (scancode, down) of Ctrl+Alt+Del, as the original's
/// `SendCAD`: Ctrl, Alt, Del down, then the three up.
pub const CTRL_ALT_DEL: [(KeyCode, bool); 6] = [
    (KeyCode::ControlLeft, true),
    (KeyCode::AltLeft, true),
    (KeyCode::Delete, true),
    (KeyCode::ControlLeft, false),
    (KeyCode::AltLeft, false),
    (KeyCode::Delete, false),
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn scancodes_are_unique_and_match_the_original_constants() {
        let codes: HashSet<u32> = KeyCode::ALL.iter().map(|k| k.scancode()).collect();
        assert_eq!(codes.len(), KeyCode::ALL.len());
        // ConsoleKeyHandler.cs: CTRL_SCAN 29, ALT_SCAN 56, CTRL2_SCAN 157,
        // ALT2_SCAN 184, DEL_SCAN 211, INS_SCAN 210, shifts 0x2A/0x36, F11 87.
        assert_eq!(KeyCode::ControlLeft.scancode(), 29);
        assert_eq!(KeyCode::AltLeft.scancode(), 56);
        assert_eq!(KeyCode::ControlRight.scancode(), 157);
        assert_eq!(KeyCode::AltRight.scancode(), 184);
        assert_eq!(KeyCode::Delete.scancode(), 211);
        assert_eq!(KeyCode::Insert.scancode(), 210);
        assert_eq!(KeyCode::ShiftLeft.scancode(), 0x2a);
        assert_eq!(KeyCode::ShiftRight.scancode(), 0x36);
        assert_eq!(KeyCode::F11.scancode(), 87);
    }

    #[test]
    fn us_characters_map_to_keys() {
        assert_eq!(KeyCode::for_us_char('a'), Some((KeyCode::KeyA, false)));
        assert_eq!(KeyCode::for_us_char('A'), Some((KeyCode::KeyA, true)));
        assert_eq!(KeyCode::for_us_char('/'), Some((KeyCode::Slash, false)));
        assert_eq!(KeyCode::for_us_char('?'), Some((KeyCode::Slash, true)));
        assert_eq!(KeyCode::for_us_char('0'), Some((KeyCode::Digit0, false)));
        assert_eq!(KeyCode::for_us_char('|'), Some((KeyCode::Backslash, true)));
        assert_eq!(KeyCode::for_us_char('\n'), Some((KeyCode::Enter, false)));
        assert_eq!(KeyCode::for_us_char('ç'), None);
        for c in ' '..='~' {
            assert!(KeyCode::for_us_char(c).is_some(), "{c:?}");
        }
    }
}
