//! Keyboard for the console: egui key events → physical keys
//! ([`KeyCode`], hence XT scancodes), the keys held, and the console's own
//! shortcuts.
//!
//! egui reports each key twice: `key` (logical, after the local layout) and
//! `physical_key` (the position). The console uses the position, so the
//! guest's layout decides the character: an ABNT2 keyboard types `ç` into an
//! ABNT2 guest because the `ç` key's position (`Semicolon`, scancode `0x27`)
//! is what goes out. egui-winit has no name for the ABNT2/JIS key left of
//! right Shift (`IntlRo`) and reports it with only its logical key, `/` or
//! `?`, which no other key does: that is mapped back to `IntlRo`.

use std::collections::BTreeMap;

use egui::Key;
use xen_vnc::{Input, KeyCode};

/// The position `key` names, for keys egui knows by position.
fn by_position(key: Key) -> Option<KeyCode> {
    use KeyCode as K;
    Some(match key {
        Key::ArrowDown => K::ArrowDown,
        Key::ArrowLeft => K::ArrowLeft,
        Key::ArrowRight => K::ArrowRight,
        Key::ArrowUp => K::ArrowUp,
        Key::Escape => K::Escape,
        Key::Tab => K::Tab,
        Key::Backspace => K::Backspace,
        Key::Enter => K::Enter,
        Key::Space => K::Space,
        Key::Insert => K::Insert,
        Key::Delete => K::Delete,
        Key::Home => K::Home,
        Key::End => K::End,
        Key::PageUp => K::PageUp,
        Key::PageDown => K::PageDown,
        Key::Comma => K::Comma,
        Key::Backslash => K::Backslash,
        Key::Slash => K::Slash,
        Key::OpenBracket => K::BracketLeft,
        Key::CloseBracket => K::BracketRight,
        Key::Backtick => K::Backquote,
        Key::Minus => K::Minus,
        Key::Period => K::Period,
        Key::Plus => K::NumpadAdd,
        Key::Equals => K::Equal,
        Key::Semicolon => K::Semicolon,
        Key::Quote => K::Quote,
        // Shifted symbols only come as logical keys: their unshifted key.
        Key::Colon => K::Semicolon,
        Key::Pipe => K::Backslash,
        Key::Questionmark => K::Slash,
        Key::Exclamationmark => K::Digit1,
        Key::OpenCurlyBracket => K::BracketLeft,
        Key::CloseCurlyBracket => K::BracketRight,
        Key::Num0 => K::Digit0,
        Key::Num1 => K::Digit1,
        Key::Num2 => K::Digit2,
        Key::Num3 => K::Digit3,
        Key::Num4 => K::Digit4,
        Key::Num5 => K::Digit5,
        Key::Num6 => K::Digit6,
        Key::Num7 => K::Digit7,
        Key::Num8 => K::Digit8,
        Key::Num9 => K::Digit9,
        Key::A => K::KeyA,
        Key::B => K::KeyB,
        Key::C => K::KeyC,
        Key::D => K::KeyD,
        Key::E => K::KeyE,
        Key::F => K::KeyF,
        Key::G => K::KeyG,
        Key::H => K::KeyH,
        Key::I => K::KeyI,
        Key::J => K::KeyJ,
        Key::K => K::KeyK,
        Key::L => K::KeyL,
        Key::M => K::KeyM,
        Key::N => K::KeyN,
        Key::O => K::KeyO,
        Key::P => K::KeyP,
        Key::Q => K::KeyQ,
        Key::R => K::KeyR,
        Key::S => K::KeyS,
        Key::T => K::KeyT,
        Key::U => K::KeyU,
        Key::V => K::KeyV,
        Key::W => K::KeyW,
        Key::X => K::KeyX,
        Key::Y => K::KeyY,
        Key::Z => K::KeyZ,
        Key::F1 => K::F1,
        Key::F2 => K::F2,
        Key::F3 => K::F3,
        Key::F4 => K::F4,
        Key::F5 => K::F5,
        Key::F6 => K::F6,
        Key::F7 => K::F7,
        Key::F8 => K::F8,
        Key::F9 => K::F9,
        Key::F10 => K::F10,
        Key::F11 => K::F11,
        Key::F12 => K::F12,
        Key::ShiftLeft => K::ShiftLeft,
        Key::ShiftRight => K::ShiftRight,
        Key::ControlLeft => K::ControlLeft,
        Key::ControlRight => K::ControlRight,
        Key::AltLeft => K::AltLeft,
        Key::AltRight => K::AltRight,
        Key::SuperLeft => K::MetaLeft,
        Key::SuperRight => K::MetaRight,
        Key::IntlBackslash => K::IntlBackslash,
        Key::CapsLock => K::CapsLock,
        Key::NumLock => K::NumLock,
        Key::ScrollLock => K::ScrollLock,
        Key::PrintScreen => K::PrintScreen,
        Key::Pause => K::Pause,
        Key::Menu => K::ContextMenu,
        _ => return None,
    })
}

/// The physical key of an egui key event.
pub fn key_code(key: Key, physical_key: Option<Key>) -> Option<KeyCode> {
    match physical_key {
        Some(p) => by_position(p),
        // The ABNT2/JIS key egui-winit cannot name (see the module docs).
        None if matches!(key, Key::Slash | Key::Questionmark) => Some(KeyCode::IntlRo),
        None => by_position(key),
    }
}

/// What the console does with a key event besides (or instead of) sending
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// Ctrl+Alt+Insert: send Ctrl+Alt+Del (the original's default).
    CtrlAltDel,
    /// Ctrl+Alt+F: full screen on or off.
    Fullscreen,
    /// Alt+Shift+U: undock or redock.
    Dock,
    /// Right Ctrl pressed and released alone: release the keyboard (the
    /// original's default "uncapture" key).
    Release,
}

/// The keys held in the guest, and modifier tracking for the shortcuts.
#[derive(Debug, Default)]
pub struct Keyboard {
    /// Keys sent down and not yet up, with the keysym they went with.
    held: BTreeMap<KeyCode, u32>,
    /// Right Ctrl went down and nothing else since.
    right_ctrl_alone: bool,
}

impl Keyboard {
    fn held(&self, k: KeyCode) -> bool {
        self.held.contains_key(&k)
    }

    fn ctrl(&self) -> bool {
        self.held(KeyCode::ControlLeft) || self.held(KeyCode::ControlRight)
    }

    fn alt(&self) -> bool {
        self.held(KeyCode::AltLeft) || self.held(KeyCode::AltRight)
    }

    fn shift(&self) -> bool {
        self.held(KeyCode::ShiftLeft) || self.held(KeyCode::ShiftRight)
    }

    /// Handles one key event: the inputs for the guest, and a shortcut if
    /// the key is one (then the key itself is not sent).
    pub fn key(&mut self, code: KeyCode, pressed: bool) -> (Vec<Input>, Option<Shortcut>) {
        let mut out = Vec::new();
        if pressed {
            self.right_ctrl_alone = code == KeyCode::ControlRight;
            let shortcut = match code {
                KeyCode::Insert if self.ctrl() && self.alt() => Some(Shortcut::CtrlAltDel),
                KeyCode::KeyF if self.ctrl() && self.alt() => Some(Shortcut::Fullscreen),
                KeyCode::KeyU if self.alt() && self.shift() && !self.ctrl() => Some(Shortcut::Dock),
                _ => None,
            };
            if shortcut.is_some() {
                return (out, shortcut);
            }
            let keysym = code.keysym(self.shift());
            self.held.insert(code, keysym);
            out.push(Input::Key {
                down: true,
                scancode: code.scancode(),
                keysym,
            });
            (out, None)
        } else {
            let release = code == KeyCode::ControlRight && self.right_ctrl_alone;
            self.right_ctrl_alone = false;
            // Keys whose press was a shortcut were never sent.
            if let Some(keysym) = self.held.remove(&code) {
                out.push(Input::Key {
                    down: false,
                    scancode: code.scancode(),
                    keysym,
                });
            }
            (out, release.then_some(Shortcut::Release))
        }
    }

    /// Releases every key still held (focus lost).
    pub fn release_all(&mut self) -> Vec<Input> {
        self.right_ctrl_alone = false;
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|(code, keysym)| Input::Key {
                down: false,
                scancode: code.scancode(),
                keysym,
            })
            .collect()
    }

    /// Whether a key that types a character goes as a key event in keysym
    /// mode: only with Ctrl or Alt held (Ctrl+C…); otherwise the character
    /// comes from the text event, after the local layout.
    pub fn send_in_keysym_mode(&self, code: KeyCode) -> bool {
        let printable = (0x20..0x100).contains(&code.keysym(false));
        !printable || self.ctrl() || self.alt()
    }

    /// Whether `code` is held down in the guest.
    #[cfg(test)]
    pub fn is_held(&self, code: KeyCode) -> bool {
        self.held(code)
    }
}

/// The keysym of a typed character (X11: Latin-1 as is, other Unicode
/// characters as `0x01000000 + code point`).
pub fn char_keysym(c: char) -> u32 {
    let cp = u32::from(c);
    if (0x20..0x100).contains(&cp) {
        cp
    } else {
        0x0100_0000 | cp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(code: KeyCode) -> Input {
        Input::Key {
            down: true,
            scancode: code.scancode(),
            keysym: code.keysym(false),
        }
    }

    #[test]
    fn the_physical_key_wins_over_the_layout() {
        // AZERTY: the key labelled A sits where US has Q.
        assert_eq!(key_code(Key::A, Some(Key::Q)), Some(KeyCode::KeyQ));
        // ABNT2: `ç` has no egui name; its position is Semicolon.
        assert_eq!(
            key_code(Key::Semicolon, Some(Key::Semicolon)),
            Some(KeyCode::Semicolon)
        );
        // ABNT2 `\|` (the ISO key) and the `/?` key egui-winit cannot name.
        assert_eq!(
            key_code(Key::Backslash, Some(Key::IntlBackslash)),
            Some(KeyCode::IntlBackslash)
        );
        assert_eq!(key_code(Key::Slash, None), Some(KeyCode::IntlRo));
        assert_eq!(key_code(Key::Questionmark, None), Some(KeyCode::IntlRo));
        // US `/` has its position.
        assert_eq!(key_code(Key::Slash, Some(Key::Slash)), Some(KeyCode::Slash));
        // No physical key (tests, some platforms): the logical one.
        assert_eq!(key_code(Key::Enter, None), Some(KeyCode::Enter));
        assert_eq!(key_code(Key::Colon, None), Some(KeyCode::Semicolon));
        assert_eq!(key_code(Key::Copy, None), None);
    }

    #[test]
    fn every_egui_key_with_a_position_maps() {
        let unmapped: Vec<Key> = Key::ALL
            .iter()
            .copied()
            .filter(|k| by_position(*k).is_none())
            .collect();
        // Only keys a PC keyboard does not send to a guest.
        for k in &unmapped {
            let name = k.name();
            assert!(
                name.starts_with('F') && name.len() == 3 && name > "F12"
                    || name.starts_with("Browser")
                    || name.starts_with("Media")
                    || name.starts_with("Audio")
                    || name.starts_with("Launch")
                    || [
                        "Copy", "Cut", "Paste", "Fn", "Eject", "Help", "Power", "Sleep", "Clear"
                    ]
                    .contains(&name),
                "{name} has no position"
            );
        }
    }

    #[test]
    fn keys_are_held_released_and_shortcuts_are_not_sent() {
        let mut kb = Keyboard::default();
        let (out, s) = kb.key(KeyCode::ControlLeft, true);
        assert_eq!((out, s), (vec![down(KeyCode::ControlLeft)], None));
        kb.key(KeyCode::AltLeft, true);
        // Ctrl+Alt+Insert → Ctrl+Alt+Del, Insert itself not sent.
        assert_eq!(
            kb.key(KeyCode::Insert, true),
            (vec![], Some(Shortcut::CtrlAltDel))
        );
        assert_eq!(kb.key(KeyCode::Insert, false), (vec![], None));
        assert_eq!(
            kb.key(KeyCode::KeyF, true),
            (vec![], Some(Shortcut::Fullscreen))
        );
        // Releasing on focus loss lets go of Ctrl and Alt.
        let released = kb.release_all();
        assert_eq!(released.len(), 2);
        assert!(
            released
                .iter()
                .all(|i| matches!(i, Input::Key { down: false, .. }))
        );
        assert!(!kb.is_held(KeyCode::ControlLeft));

        // Shift changes the keysym (for keysym-mode servers), not the scancode.
        kb.key(KeyCode::ShiftLeft, true);
        let (out, _) = kb.key(KeyCode::KeyA, true);
        assert_eq!(
            out,
            [Input::Key {
                down: true,
                scancode: 0x1e,
                keysym: u32::from('A')
            }]
        );
        kb.release_all();

        // Right Ctrl alone releases the keyboard; with another key it does not.
        kb.key(KeyCode::ControlRight, true);
        let (out, s) = kb.key(KeyCode::ControlRight, false);
        assert_eq!(s, Some(Shortcut::Release));
        assert_eq!(out.len(), 1);
        kb.key(KeyCode::ControlRight, true);
        kb.key(KeyCode::KeyC, true);
        kb.key(KeyCode::KeyC, false);
        assert_eq!(kb.key(KeyCode::ControlRight, false).1, None);

        // Alt+Shift+U docks.
        kb.key(KeyCode::AltLeft, true);
        kb.key(KeyCode::ShiftLeft, true);
        assert_eq!(kb.key(KeyCode::KeyU, true).1, Some(Shortcut::Dock));
    }

    #[test]
    fn keysym_mode_leaves_characters_to_text_events() {
        let mut kb = Keyboard::default();
        assert!(!kb.send_in_keysym_mode(KeyCode::KeyA));
        assert!(kb.send_in_keysym_mode(KeyCode::Enter));
        kb.key(KeyCode::ControlLeft, true);
        assert!(kb.send_in_keysym_mode(KeyCode::KeyC));
        assert_eq!(char_keysym('a'), 0x61);
        assert_eq!(char_keysym('ç'), 0xe7);
        assert_eq!(char_keysym('€'), 0x0100_20ac);
    }
}
