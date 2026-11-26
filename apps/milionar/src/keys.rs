//! SDL key events to Windows virtual-key codes (the engine's input vocabulary).
//!
//! Letters map by the letter the layout produces (`keycode`), so a Czech QWERTZ user
//! presses the Z printed on the key. Digits map by the physical top-row key
//! (`scancode`), because Czech layouts produce accented letters there without Shift
//! (SDL reports e.g. 'ě' as the keycode). SDL3's default keycode options
//! (`latin_letters`) already turn letters of non-Latin layouts into Latin keycodes; if a
//! layout still produces no Latin letter, the scancode's US letter is used.

use engine::Vk;
use sdl3::keyboard::{Keycode, Scancode};

/// The VK for one key event, `None` for keys the game never reads.
pub fn vk_for(keycode: Option<Keycode>, scancode: Option<Scancode>) -> Option<Vk> {
    if let Some(sc) = scancode {
        if let Some(d) = top_row_digit(sc) {
            return Some(Vk(b'0' + d));
        }
        if let Some(d) = keypad_digit(sc) {
            return Some(Vk(Vk::NUMPAD0.0 + d));
        }
    }
    if let Some(kc) = keycode {
        let c = kc as u32;
        if (u32::from(b'a')..=u32::from(b'z')).contains(&c) {
            return Some(Vk(c as u8 - b'a' + b'A'));
        }
        if (u32::from(b'0')..=u32::from(b'9')).contains(&c) {
            return Some(Vk(c as u8));
        }
        let special = match kc {
            Keycode::Left => Some(Vk::LEFT),
            Keycode::Right => Some(Vk::RIGHT),
            Keycode::Up => Some(Vk::UP),
            Keycode::Down => Some(Vk::DOWN),
            Keycode::LShift | Keycode::RShift => Some(Vk::SHIFT),
            Keycode::Space => Some(Vk::SPACE),
            Keycode::Return | Keycode::KpEnter | Keycode::Return2 => Some(Vk::RETURN),
            Keycode::Escape => Some(Vk::ESCAPE),
            Keycode::F11 => Some(Vk::F11),
            Keycode::F12 => Some(Vk::F12),
            _ => None,
        };
        if special.is_some() {
            return special;
        }
    }
    scancode.and_then(scancode_fallback)
}

fn top_row_digit(sc: Scancode) -> Option<u8> {
    Some(match sc {
        Scancode::_0 => 0,
        Scancode::_1 => 1,
        Scancode::_2 => 2,
        Scancode::_3 => 3,
        Scancode::_4 => 4,
        Scancode::_5 => 5,
        Scancode::_6 => 6,
        Scancode::_7 => 7,
        Scancode::_8 => 8,
        Scancode::_9 => 9,
        _ => return None,
    })
}

fn keypad_digit(sc: Scancode) -> Option<u8> {
    Some(match sc {
        Scancode::Kp0 => 0,
        Scancode::Kp1 => 1,
        Scancode::Kp2 => 2,
        Scancode::Kp3 => 3,
        Scancode::Kp4 => 4,
        Scancode::Kp5 => 5,
        Scancode::Kp6 => 6,
        Scancode::Kp7 => 7,
        Scancode::Kp8 => 8,
        Scancode::Kp9 => 9,
        _ => return None,
    })
}

/// Physical position when the keycode gave nothing usable (US letter, or a special key
/// whose keycode was remapped).
fn scancode_fallback(sc: Scancode) -> Option<Vk> {
    let letters = [
        Scancode::A,
        Scancode::B,
        Scancode::C,
        Scancode::D,
        Scancode::E,
        Scancode::F,
        Scancode::G,
        Scancode::H,
        Scancode::I,
        Scancode::J,
        Scancode::K,
        Scancode::L,
        Scancode::M,
        Scancode::N,
        Scancode::O,
        Scancode::P,
        Scancode::Q,
        Scancode::R,
        Scancode::S,
        Scancode::T,
        Scancode::U,
        Scancode::V,
        Scancode::W,
        Scancode::X,
        Scancode::Y,
        Scancode::Z,
    ];
    if let Some(i) = letters.iter().position(|&l| l == sc) {
        return Some(Vk(b'A' + i as u8));
    }
    match sc {
        Scancode::Left => Some(Vk::LEFT),
        Scancode::Right => Some(Vk::RIGHT),
        Scancode::Up => Some(Vk::UP),
        Scancode::Down => Some(Vk::DOWN),
        Scancode::LShift | Scancode::RShift => Some(Vk::SHIFT),
        Scancode::Space => Some(Vk::SPACE),
        Scancode::Return | Scancode::KpEnter => Some(Vk::RETURN),
        Scancode::Escape => Some(Vk::ESCAPE),
        Scancode::F11 => Some(Vk::F11),
        Scancode::F12 => Some(Vk::F12),
        _ => None,
    }
}

/// A host shortcut that toggles fullscreen and is not passed to the game: Alt+Enter,
/// Cmd+Ctrl+F (macOS), F11.
pub fn is_fullscreen_toggle(vk: Option<Vk>, alt: bool, ctrl: bool, gui: bool) -> bool {
    match vk {
        Some(Vk::RETURN) => alt,
        Some(Vk::F) => ctrl && gui,
        Some(Vk::F11) => true,
        _ => false,
    }
}

/// Keys currently down, by physical key. Several keys can produce the same VK (both
/// Shifts; a numpad digit and the top-row digit, which the engine folds together), so a
/// VK is released only when no other held key still produces it.
#[derive(Debug, Default)]
pub struct HeldKeys {
    keys: Vec<(Scancode, Vk)>,
}

impl HeldKeys {
    /// Records a key-down.
    pub fn press(&mut self, sc: Scancode, vk: Vk) {
        if !self.keys.iter().any(|&(s, _)| s == sc) {
            self.keys.push((sc, vk));
        }
    }

    /// Records a key-up; returns the VK to release, if no other held key produces it.
    pub fn release(&mut self, sc: Scancode) -> Option<Vk> {
        let i = self.keys.iter().position(|&(s, _)| s == sc)?;
        let (_, vk) = self.keys.remove(i);
        let folded = vk.remap_numpad();
        let still = self.keys.iter().any(|&(_, v)| v.remap_numpad() == folded);
        (!still).then_some(vk)
    }

    /// Forgets every key (focus lost); returns the VKs to release.
    pub fn release_all(&mut self) -> Vec<Vk> {
        let mut out: Vec<Vk> = Vec::new();
        for (_, vk) in self.keys.drain(..) {
            if !out.iter().any(|v| v.remap_numpad() == vk.remap_numpad()) {
                out.push(vk);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vks_release_with_the_last_key() {
        let mut h = HeldKeys::default();
        h.press(Scancode::LShift, Vk::SHIFT);
        h.press(Scancode::RShift, Vk::SHIFT);
        assert_eq!(h.release(Scancode::LShift), None);
        assert_eq!(h.release(Scancode::RShift), Some(Vk::SHIFT));
        assert_eq!(h.release(Scancode::RShift), None, "not held any more");
        // Numpad 1 and top-row 1 are the same key for the engine.
        h.press(Scancode::Kp1, Vk(0x61));
        h.press(Scancode::_1, Vk::D1);
        assert_eq!(h.release(Scancode::Kp1), None);
        assert_eq!(h.release(Scancode::_1), Some(Vk::D1));
        h.press(Scancode::_1, Vk::D1);
        h.press(Scancode::Kp1, Vk(0x61));
        assert_eq!(h.release(Scancode::_1), None);
        assert_eq!(h.release(Scancode::Kp1), Some(Vk(0x61)));
        // Auto-repeat does not stack.
        h.press(Scancode::A, Vk::A);
        h.press(Scancode::A, Vk::A);
        assert_eq!(h.release(Scancode::A), Some(Vk::A));
        h.press(Scancode::A, Vk::A);
        h.press(Scancode::Up, Vk::UP);
        let mut all = h.release_all();
        all.sort();
        assert_eq!(all, vec![Vk::UP, Vk::A]);
        assert!(h.release_all().is_empty());
    }

    #[test]
    fn letters_follow_the_layout() {
        // QWERTZ: the key at US-Y produces 'z'.
        assert_eq!(vk_for(Some(Keycode::Z), Some(Scancode::Y)), Some(Vk::Z));
        assert_eq!(vk_for(Some(Keycode::Y), Some(Scancode::Z)), Some(Vk::Y));
        assert_eq!(vk_for(Some(Keycode::P), Some(Scancode::P)), Some(Vk::P));
        // No Latin keycode at all: fall back to the physical letter.
        assert_eq!(vk_for(None, Some(Scancode::K)), Some(Vk::K));
    }

    #[test]
    fn digits_follow_the_physical_key() {
        // Czech layout: top-row 2 produces 'ě', which SDL reports as a non-enum keycode.
        assert_eq!(vk_for(None, Some(Scancode::_2)), Some(Vk::D2));
        // A keycode that is some other symbol on that key still gives the digit.
        assert_eq!(
            vk_for(Some(Keycode::Plus), Some(Scancode::_1)),
            Some(Vk::D1)
        );
        assert_eq!(vk_for(Some(Keycode::_0), Some(Scancode::_0)), Some(Vk::D0));
        // AZERTY-like: the digit key reports '&' but is still 1.
        assert_eq!(
            vk_for(Some(Keycode::Ampersand), Some(Scancode::_1)),
            Some(Vk::D1)
        );
    }

    #[test]
    fn specials_and_numpad() {
        assert_eq!(
            vk_for(Some(Keycode::Left), Some(Scancode::Left)),
            Some(Vk::LEFT)
        );
        assert_eq!(vk_for(Some(Keycode::Up), Some(Scancode::Up)), Some(Vk::UP));
        assert_eq!(
            vk_for(Some(Keycode::Right), Some(Scancode::Right)),
            Some(Vk::RIGHT)
        );
        assert_eq!(
            vk_for(Some(Keycode::Down), Some(Scancode::Down)),
            Some(Vk::DOWN)
        );
        assert_eq!(
            vk_for(Some(Keycode::LShift), Some(Scancode::LShift)),
            Some(Vk::SHIFT)
        );
        assert_eq!(
            vk_for(Some(Keycode::RShift), Some(Scancode::RShift)),
            Some(Vk::SHIFT)
        );
        assert_eq!(
            vk_for(Some(Keycode::Space), Some(Scancode::Space)),
            Some(Vk::SPACE)
        );
        assert_eq!(
            vk_for(Some(Keycode::Return), Some(Scancode::Return)),
            Some(Vk::RETURN)
        );
        assert_eq!(
            vk_for(Some(Keycode::KpEnter), Some(Scancode::KpEnter)),
            Some(Vk::RETURN)
        );
        assert_eq!(
            vk_for(Some(Keycode::Escape), Some(Scancode::Escape)),
            Some(Vk::ESCAPE)
        );
        assert_eq!(
            vk_for(Some(Keycode::F12), Some(Scancode::F12)),
            Some(Vk::F12)
        );
        assert_eq!(
            vk_for(Some(Keycode::F11), Some(Scancode::F11)),
            Some(Vk::F11)
        );
        // Numpad digits whatever NumLock makes of the keycode.
        assert_eq!(
            vk_for(Some(Keycode::Kp3), Some(Scancode::Kp3)),
            Some(Vk(0x63))
        );
        assert_eq!(
            vk_for(Some(Keycode::End), Some(Scancode::Kp1)),
            Some(Vk(0x61))
        );
        assert_eq!(
            vk_for(Some(Keycode::Kp0), Some(Scancode::Kp0)),
            Some(Vk::NUMPAD0)
        );
        assert_eq!(vk_for(Some(Keycode::Tab), Some(Scancode::Tab)), None);
        assert_eq!(vk_for(None, None), None);
    }

    #[test]
    fn fullscreen_shortcuts() {
        assert!(is_fullscreen_toggle(Some(Vk::RETURN), true, false, false));
        assert!(is_fullscreen_toggle(Some(Vk::F), false, true, true));
        assert!(is_fullscreen_toggle(Some(Vk::F11), false, false, false));
        assert!(!is_fullscreen_toggle(Some(Vk::RETURN), false, false, false));
        assert!(!is_fullscreen_toggle(Some(Vk::F), false, true, false));
        assert!(!is_fullscreen_toggle(Some(Vk::F), false, false, false));
        assert!(!is_fullscreen_toggle(None, true, true, true));
    }
}
