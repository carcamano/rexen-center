//! US and ABNT2 typing through scancodes.
//!
//! The client sends the scancode of each physical key; the guest's keyboard
//! layout turns it into a character. These tests check the scancode of
//! every key position the two layouts use against the guest side:
//!
//! * the reference layouts are Linux console keymaps (`us.map` and
//!   `br-abnt2.map` from kbd: Linux keycode → plain / Shift / AltGr);
//! * Linux keycodes come from XT set-1 scancodes through the kernel's atkbd
//!   translation: identity for 1–88, and `0x73` → 89 (`KEY_RO`, the ABNT2
//!   `/?` key);
//! * physical keys are named by their W3C `code`, which is what window
//!   toolkits report for a key position whatever the local layout (on an
//!   ABNT2 keyboard the `ç` key is `Semicolon` and the `/?` key `IntlRo`).
//!
//! A simulated ABNT2 guest then types back what an ABNT2 keyboard sent,
//! dead keys included.

use std::collections::HashMap;

use xen_vnc::KeyCode;

/// atkbd's set-1 scancode for a Linux keycode (the keys these layouts use).
fn scancode_of_linux_keycode(keycode: u32) -> u32 {
    match keycode {
        1..=88 => keycode,
        89 => 0x73,
        other => panic!("keycode {other} not in the table"),
    }
}

fn key(name: &str) -> KeyCode {
    *KeyCode::ALL
        .iter()
        .find(|k| k.name() == name)
        .unwrap_or_else(|| panic!("no key {name}"))
}

/// (W3C code of the position, Linux keycode, plain, shift, altgr) — `None`
/// for no symbol; dead keys as `dead_*` names.
type Entry = (&'static str, u32, &'static str, &'static str, &'static str);

/// kbd `us.map` (printable keys).
const US: &[Entry] = &[
    ("Backquote", 41, "`", "~", ""),
    ("Digit1", 2, "1", "!", ""),
    ("Digit2", 3, "2", "@", ""),
    ("Digit3", 4, "3", "#", ""),
    ("Digit4", 5, "4", "$", ""),
    ("Digit5", 6, "5", "%", ""),
    ("Digit6", 7, "6", "^", ""),
    ("Digit7", 8, "7", "&", ""),
    ("Digit8", 9, "8", "*", ""),
    ("Digit9", 10, "9", "(", ""),
    ("Digit0", 11, "0", ")", ""),
    ("Minus", 12, "-", "_", ""),
    ("Equal", 13, "=", "+", ""),
    ("KeyQ", 16, "q", "Q", ""),
    ("KeyW", 17, "w", "W", ""),
    ("KeyE", 18, "e", "E", ""),
    ("KeyR", 19, "r", "R", ""),
    ("KeyT", 20, "t", "T", ""),
    ("KeyY", 21, "y", "Y", ""),
    ("KeyU", 22, "u", "U", ""),
    ("KeyI", 23, "i", "I", ""),
    ("KeyO", 24, "o", "O", ""),
    ("KeyP", 25, "p", "P", ""),
    ("BracketLeft", 26, "[", "{", ""),
    ("BracketRight", 27, "]", "}", ""),
    ("KeyA", 30, "a", "A", ""),
    ("KeyS", 31, "s", "S", ""),
    ("KeyD", 32, "d", "D", ""),
    ("KeyF", 33, "f", "F", ""),
    ("KeyG", 34, "g", "G", ""),
    ("KeyH", 35, "h", "H", ""),
    ("KeyJ", 36, "j", "J", ""),
    ("KeyK", 37, "k", "K", ""),
    ("KeyL", 38, "l", "L", ""),
    ("Semicolon", 39, ";", ":", ""),
    ("Quote", 40, "'", "\"", ""),
    ("Backslash", 43, "\\", "|", ""),
    ("KeyZ", 44, "z", "Z", ""),
    ("KeyX", 45, "x", "X", ""),
    ("KeyC", 46, "c", "C", ""),
    ("KeyV", 47, "v", "V", ""),
    ("KeyB", 48, "b", "B", ""),
    ("KeyN", 49, "n", "N", ""),
    ("KeyM", 50, "m", "M", ""),
    ("Comma", 51, ",", "<", ""),
    ("Period", 52, ".", ">", ""),
    ("Slash", 53, "/", "?", ""),
    ("Space", 57, " ", " ", ""),
];

/// kbd `br-abnt2.map` (printable keys; letters as in US).
const ABNT2: &[Entry] = &[
    ("Backquote", 41, "'", "\"", ""),
    ("Digit1", 2, "1", "!", "¹"),
    ("Digit2", 3, "2", "@", "²"),
    ("Digit3", 4, "3", "#", "³"),
    ("Digit4", 5, "4", "$", "£"),
    ("Digit5", 6, "5", "%", "¢"),
    ("Digit6", 7, "6", "dead_diaeresis", "¬"),
    ("Digit7", 8, "7", "&", ""),
    ("Digit8", 9, "8", "*", ""),
    ("Digit9", 10, "9", "(", ""),
    ("Digit0", 11, "0", ")", ""),
    ("Minus", 12, "-", "_", ""),
    ("Equal", 13, "=", "+", "§"),
    ("KeyQ", 16, "q", "Q", "/"),
    ("KeyW", 17, "w", "W", "?"),
    ("KeyE", 18, "e", "E", "°"),
    ("KeyR", 19, "r", "R", ""),
    ("KeyT", 20, "t", "T", ""),
    ("KeyY", 21, "y", "Y", ""),
    ("KeyU", 22, "u", "U", ""),
    ("KeyI", 23, "i", "I", ""),
    ("KeyO", 24, "o", "O", ""),
    ("KeyP", 25, "p", "P", ""),
    ("BracketLeft", 26, "dead_acute", "dead_grave", ""),
    ("BracketRight", 27, "[", "{", "ª"),
    ("KeyA", 30, "a", "A", ""),
    ("KeyS", 31, "s", "S", ""),
    ("KeyD", 32, "d", "D", ""),
    ("KeyF", 33, "f", "F", ""),
    ("KeyG", 34, "g", "G", ""),
    ("KeyH", 35, "h", "H", ""),
    ("KeyJ", 36, "j", "J", ""),
    ("KeyK", 37, "k", "K", ""),
    ("KeyL", 38, "l", "L", ""),
    ("Semicolon", 39, "ç", "Ç", ""),
    ("Quote", 40, "dead_tilde", "dead_circumflex", ""),
    ("Backslash", 43, "]", "}", "º"),
    ("IntlBackslash", 86, "\\", "|", ""),
    ("KeyZ", 44, "z", "Z", ""),
    ("KeyX", 45, "x", "X", ""),
    ("KeyC", 46, "c", "C", ""),
    ("KeyV", 47, "v", "V", ""),
    ("KeyB", 48, "b", "B", ""),
    ("KeyN", 49, "n", "N", ""),
    ("KeyM", 50, "m", "M", ""),
    ("Comma", 51, ",", "<", ""),
    ("Period", 52, ".", ">", ""),
    ("Slash", 53, ";", ":", ""),
    ("IntlRo", 89, "/", "?", "°"),
    ("Space", 57, " ", " ", ""),
];

#[test]
fn every_us_and_abnt2_key_position_sends_the_scancode_the_guest_layout_expects() {
    for (layout, table) in [("us", US), ("br-abnt2", ABNT2)] {
        for &(code, keycode, ..) in table {
            assert_eq!(
                key(code).scancode(),
                scancode_of_linux_keycode(keycode),
                "{layout}: {code} (Linux keycode {keycode})"
            );
        }
    }
    // The modifiers the guest needs for the second and third levels.
    assert_eq!(KeyCode::ShiftLeft.scancode(), 0x2a);
    assert_eq!(KeyCode::ShiftRight.scancode(), 0x36);
    // AltGr is the right Alt: E0 38.
    assert_eq!(KeyCode::AltRight.scancode(), 0x80 | 0x38);
}

/// A guest with a console keymap: turns key events (scancodes) into text,
/// with Shift, AltGr and dead keys.
struct Guest {
    keymap: HashMap<u32, [&'static str; 3]>,
    shift: bool,
    altgr: bool,
    dead: Option<&'static str>,
    text: String,
}

impl Guest {
    fn new(table: &[Entry]) -> Self {
        Self {
            keymap: table
                .iter()
                .map(|&(_, keycode, a, b, c)| (scancode_of_linux_keycode(keycode), [a, b, c]))
                .collect(),
            shift: false,
            altgr: false,
            dead: None,
            text: String::new(),
        }
    }

    fn key(&mut self, scancode: u32, down: bool) {
        match scancode {
            0x2a | 0x36 => self.shift = down,
            0xb8 => self.altgr = down,
            _ if !down => {}
            _ => {
                let level = if self.altgr {
                    2
                } else {
                    usize::from(self.shift)
                };
                let sym = self.keymap[&scancode][level];
                if sym.starts_with("dead_") {
                    self.dead = Some(sym);
                    return;
                }
                let composed = match (self.dead.take(), sym) {
                    (None, s) => s.to_owned(),
                    (Some("dead_acute"), "a") => "á".into(),
                    (Some("dead_acute"), "e") => "é".into(),
                    (Some("dead_tilde"), "a") => "ã".into(),
                    (Some("dead_tilde"), "o") => "õ".into(),
                    (Some("dead_circumflex"), "e") => "ê".into(),
                    (Some(d), s) => panic!("no composition {d} + {s}"),
                };
                self.text.push_str(&composed);
            }
        }
    }
}

/// What a keyboard with `table`'s key caps sends to type `text`:
/// `(scancode, down)` events, as the console view produces them from the
/// physical keys (modifiers pressed around the key).
fn type_on(table: &[Entry], text: &str) -> Vec<(u32, bool)> {
    let mut caps: HashMap<&str, (&str, usize)> = HashMap::new();
    for &(code, _, a, b, c) in table {
        for (level, sym) in [a, b, c].into_iter().enumerate() {
            if !sym.is_empty() {
                caps.entry(sym).or_insert((code, level));
            }
        }
    }
    let dead_for = |c: char| -> Option<(&str, &str)> {
        Some(match c {
            'á' => ("dead_acute", "a"),
            'é' => ("dead_acute", "e"),
            'ã' => ("dead_tilde", "a"),
            'õ' => ("dead_tilde", "o"),
            'ê' => ("dead_circumflex", "e"),
            _ => return None,
        })
    };
    let mut events = Vec::new();
    let mut press = |sym: &str| {
        let (code, level) = caps[sym];
        let modifier = match level {
            1 => Some(KeyCode::ShiftLeft),
            2 => Some(KeyCode::AltRight),
            _ => None,
        };
        if let Some(m) = modifier {
            events.push((m.scancode(), true));
        }
        events.push((key(code).scancode(), true));
        events.push((key(code).scancode(), false));
        if let Some(m) = modifier {
            events.push((m.scancode(), false));
        }
    };
    for c in text.chars() {
        match dead_for(c) {
            Some((dead, base)) => {
                press(dead);
                press(base);
            }
            None => press(&c.to_string()),
        }
    }
    events
}

#[test]
fn an_abnt2_keyboard_types_into_an_abnt2_guest() {
    let text = "Ação: é çÇ [ok]; olá/? \\| ª º ° § ¹²³ £ ¢ ¬ \"x\" 'y' {z}";
    let mut guest = Guest::new(ABNT2);
    for (scancode, down) in type_on(ABNT2, text) {
        guest.key(scancode, down);
    }
    assert_eq!(guest.text, text);
}

#[test]
fn a_us_keyboard_types_into_a_us_guest() {
    let text = "Hello, World! ~`@#$%^&*()_+-=[]{}\\|;:'\",.<>/? 0123456789";
    let mut guest = Guest::new(US);
    for (scancode, down) in type_on(US, text) {
        guest.key(scancode, down);
    }
    assert_eq!(guest.text, text);
    // And the helper the tests use to type at a US prompt agrees.
    for c in text.chars() {
        let (k, shift) = KeyCode::for_us_char(c).unwrap();
        let entry = US.iter().find(|e| e.0 == k.name()).unwrap();
        let sym = if shift { entry.3 } else { entry.2 };
        assert_eq!(sym, c.to_string());
    }
}
