//! Input model: virtual keys, the per-tick [`InputState`] and the [`InputAccumulator`]
//! that hosts feed raw events into (F1.2, F5.2).

use std::collections::HashSet;

/// A Windows virtual-key code, the vocabulary the original's input code uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Vk(pub u8);

impl Vk {
    pub const RETURN: Vk = Vk(0x0D);
    pub const SHIFT: Vk = Vk(0x10);
    pub const ESCAPE: Vk = Vk(0x1B);
    pub const SPACE: Vk = Vk(0x20);
    pub const LEFT: Vk = Vk(0x25);
    pub const UP: Vk = Vk(0x26);
    pub const RIGHT: Vk = Vk(0x27);
    pub const DOWN: Vk = Vk(0x28);
    pub const F11: Vk = Vk(0x7A);
    pub const F12: Vk = Vk(0x7B);

    pub const D0: Vk = Vk(b'0');
    pub const D1: Vk = Vk(b'1');
    pub const D2: Vk = Vk(b'2');
    pub const D3: Vk = Vk(b'3');
    pub const D4: Vk = Vk(b'4');
    pub const D5: Vk = Vk(b'5');
    pub const D6: Vk = Vk(b'6');
    pub const D7: Vk = Vk(b'7');
    pub const D8: Vk = Vk(b'8');
    pub const D9: Vk = Vk(b'9');

    pub const A: Vk = Vk(b'A');
    pub const B: Vk = Vk(b'B');
    pub const C: Vk = Vk(b'C');
    pub const D: Vk = Vk(b'D');
    pub const E: Vk = Vk(b'E');
    pub const F: Vk = Vk(b'F');
    pub const G: Vk = Vk(b'G');
    pub const H: Vk = Vk(b'H');
    pub const I: Vk = Vk(b'I');
    pub const J: Vk = Vk(b'J');
    pub const K: Vk = Vk(b'K');
    pub const L: Vk = Vk(b'L');
    pub const M: Vk = Vk(b'M');
    pub const N: Vk = Vk(b'N');
    pub const O: Vk = Vk(b'O');
    pub const P: Vk = Vk(b'P');
    pub const Q: Vk = Vk(b'Q');
    pub const R: Vk = Vk(b'R');
    pub const S: Vk = Vk(b'S');
    pub const T: Vk = Vk(b'T');
    pub const U: Vk = Vk(b'U');
    pub const V: Vk = Vk(b'V');
    pub const W: Vk = Vk(b'W');
    pub const X: Vk = Vk(b'X');
    pub const Y: Vk = Vk(b'Y');
    pub const Z: Vk = Vk(b'Z');

    /// Numpad 0..9 (0x60..0x69).
    pub const NUMPAD0: Vk = Vk(0x60);
    pub const NUMPAD9: Vk = Vk(0x69);

    /// VK of an ASCII letter (either case) or digit.
    pub fn from_char(c: char) -> Option<Vk> {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' => Some(Vk(c.to_ascii_uppercase() as u8)),
            _ => None,
        }
    }

    /// WndProc's remap of numpad digits to '0'..'9' (F1.2).
    pub const fn remap_numpad(self) -> Vk {
        if self.0 >= Self::NUMPAD0.0 && self.0 <= Self::NUMPAD9.0 {
            Vk(self.0 - Self::NUMPAD0.0 + b'0')
        } else {
            self
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
}

/// Input as seen by one tick of an engine loop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputState {
    /// `g_lastKey`: the last key-down message of the tick (auto-repeat included).
    pub last_key: Option<Vk>,
    /// Latched polling view (`GetAsyncKeyState`): keys down at the tick boundary OR
    /// pressed at any time since the previous tick, so a tap shorter than a tick is seen
    /// for exactly one tick (Review Focus 1). Numpad digits appear as the digit ('0'..'9').
    pub held: HashSet<Vk>,
    /// Keys physically down at the tick boundary, without latched taps. Needed by
    /// `Ctx::flush_input`, which drops the latched presses.
    pub physically_held: HashSet<Vk>,
    /// Every key that went down (a transition, not an auto-repeat) since the previous
    /// tick, even if already released.
    pub pressed_since_last_tick: HashSet<Vk>,
    /// Cursor in 640x480 coordinates, clamped.
    pub mouse: (i32, i32),
    /// The mouse button pressed during the tick: left if any left press, else the last.
    pub mouse_button: Option<MouseButton>,
}

impl InputState {
    /// Convenience over `held` (the latched view) for arrows, Shift and cheat keys.
    pub fn is_down(&self, vk: Vk) -> bool {
        self.held.contains(&vk)
    }

    /// `FlushInput` 0x40fb00: forget the tick's key/mouse messages and latched taps.
    pub fn flush(&mut self) {
        self.last_key = None;
        self.mouse_button = None;
        self.pressed_since_last_tick.clear();
        self.held = self.physically_held.clone();
    }
}

/// Accumulates raw key/mouse events between ticks and produces one [`InputState`]
/// per tick. Shared by the real and the headless host.
#[derive(Debug, Clone, Default)]
pub struct InputAccumulator {
    held: HashSet<Vk>,
    pressed: HashSet<Vk>,
    last_key: Option<Vk>,
    mouse: (i32, i32),
    mouse_button: Option<MouseButton>,
    quit: bool,
}

impl InputAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// A WM_KEYDOWN (auto-repeats included). Shift+F12 requests quit (F1.2). Numpad
    /// digits are remapped to '0'..'9' for every view, so they also set the digit in `held`.
    pub fn key_down(&mut self, vk: Vk) {
        let vk = vk.remap_numpad();
        if vk == Vk::F12 && self.held.contains(&Vk::SHIFT) {
            self.quit = true;
        }
        if self.held.insert(vk) {
            self.pressed.insert(vk);
        }
        self.last_key = Some(vk);
    }

    pub fn key_up(&mut self, vk: Vk) {
        self.held.remove(&vk.remap_numpad());
    }

    /// Cursor position already mapped to 640x480; clamped like 0x40fbb0.
    pub fn mouse_move(&mut self, x: i32, y: i32) {
        self.mouse = (x.clamp(0, 639), y.clamp(0, 479));
    }

    /// A button press. Within one tick a left press wins over any other, as
    /// `PumpMessages` 0x40fa10 reports WM_LBUTTONDOWN in preference to later messages.
    pub fn mouse_button(&mut self, button: MouseButton) {
        if self.mouse_button != Some(MouseButton::Left) {
            self.mouse_button = Some(button);
        }
    }

    /// The window was closed (WM_CLOSE).
    pub fn request_quit(&mut self) {
        self.quit = true;
    }

    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    /// `FlushInput` 0x40fb00: discard pending key/mouse messages and latched taps.
    pub fn flush(&mut self) {
        self.pressed.clear();
        self.last_key = None;
        self.mouse_button = None;
    }

    /// Snapshot for the tick that just ended and reset the per-tick parts.
    pub fn take_tick(&mut self) -> InputState {
        let pressed = std::mem::take(&mut self.pressed);
        InputState {
            last_key: self.last_key.take(),
            held: self.held.union(&pressed).copied().collect(),
            physically_held: self.held.clone(),
            pressed_since_last_tick: pressed,
            mouse: self.mouse,
            mouse_button: self.mouse_button.take(),
        }
    }
}

/// `ReadArrowDir` 0x40fda0 (F5.2): numpad-style direction code from the polled arrows.
/// UP beats DOWN and RIGHT beats LEFT; opposing keys do not cancel. The 0x40fcb0 filter
/// (bytes at 0x422b48, all 1 in the shipped data) is a no-op and is omitted.
pub fn read_arrow_dir(input: &InputState) -> u8 {
    let u = input.is_down(Vk::UP);
    let d = input.is_down(Vk::DOWN);
    let l = input.is_down(Vk::LEFT);
    let r = input.is_down(Vk::RIGHT);
    if u {
        if r {
            0x69
        } else if l {
            0x67
        } else {
            0x68
        }
    } else if d {
        if r {
            0x63
        } else if l {
            0x61
        } else {
            0x62
        }
    } else if r {
        0x66
    } else if l {
        0x64
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_between_ticks_is_seen_once() {
        let mut l = InputAccumulator::new();
        assert!(l.take_tick().held.is_empty());
        l.key_down(Vk::LEFT);
        l.key_up(Vk::LEFT);
        let t = l.take_tick();
        assert!(t.held.contains(&Vk::LEFT));
        assert!(t.is_down(Vk::LEFT));
        assert!(!t.physically_held.contains(&Vk::LEFT));
        assert_eq!(t.last_key, Some(Vk::LEFT));
        let t2 = l.take_tick();
        assert!(!t2.held.contains(&Vk::LEFT));
        assert_eq!(t2.last_key, None);
    }

    #[test]
    fn flush_drops_latched_taps_but_keeps_physical_keys() {
        let mut l = InputAccumulator::new();
        l.key_down(Vk::SHIFT);
        l.key_down(Vk::A);
        l.key_up(Vk::A);
        l.mouse_button(MouseButton::Right);
        let mut t = l.take_tick();
        t.flush();
        assert_eq!(t.last_key, None);
        assert_eq!(t.mouse_button, None);
        assert!(t.pressed_since_last_tick.is_empty());
        assert_eq!(t.held, HashSet::from([Vk::SHIFT]));

        l.key_down(Vk::B);
        l.key_up(Vk::B);
        l.mouse_button(MouseButton::Left);
        l.flush();
        let t = l.take_tick();
        assert_eq!(t.last_key, None);
        assert_eq!(t.mouse_button, None);
        assert_eq!(t.held, HashSet::from([Vk::SHIFT]));
    }

    #[test]
    fn held_key_stays_held_without_repeating_press() {
        let mut l = InputAccumulator::new();
        l.key_down(Vk::SHIFT);
        assert!(l.take_tick().pressed_since_last_tick.contains(&Vk::SHIFT));
        let t = l.take_tick();
        assert!(t.held.contains(&Vk::SHIFT));
        assert!(t.pressed_since_last_tick.is_empty());
    }

    #[test]
    fn numpad_maps_to_digits() {
        let mut l = InputAccumulator::new();
        l.key_down(Vk(0x63));
        let t = l.take_tick();
        assert_eq!(t.last_key, Some(Vk::D3));
        assert!(t.held.contains(&Vk::D3));
        l.key_up(Vk(0x63));
        assert!(l.take_tick().held.is_empty());
    }

    #[test]
    fn shift_f12_quits() {
        let mut l = InputAccumulator::new();
        l.key_down(Vk::F12);
        assert!(!l.quit_requested());
        l.key_down(Vk::SHIFT);
        l.key_down(Vk::F12);
        assert!(l.quit_requested());
    }

    #[test]
    fn mouse_is_clamped() {
        let mut l = InputAccumulator::new();
        l.mouse_move(700, -5);
        l.mouse_button(MouseButton::Left);
        let t = l.take_tick();
        assert_eq!(t.mouse, (639, 0));
        assert_eq!(t.mouse_button, Some(MouseButton::Left));
        assert_eq!(l.take_tick().mouse_button, None);
    }

    #[test]
    fn last_key_is_last_down() {
        let mut l = InputAccumulator::new();
        l.key_down(Vk::A);
        l.key_down(Vk::B);
        l.key_up(Vk::B);
        assert_eq!(l.take_tick().last_key, Some(Vk::B));
        assert_eq!(l.take_tick().last_key, None);
    }

    fn dir(keys: &[Vk]) -> u8 {
        let mut l = InputAccumulator::new();
        for k in keys {
            l.key_down(*k);
        }
        read_arrow_dir(&l.take_tick())
    }

    #[test]
    fn arrow_codes() {
        assert_eq!(dir(&[]), 0);
        assert_eq!(dir(&[Vk::UP]), 0x68);
        assert_eq!(dir(&[Vk::UP, Vk::LEFT]), 0x67);
        assert_eq!(dir(&[Vk::UP, Vk::RIGHT]), 0x69);
        assert_eq!(dir(&[Vk::DOWN]), 0x62);
        assert_eq!(dir(&[Vk::DOWN, Vk::LEFT]), 0x61);
        assert_eq!(dir(&[Vk::DOWN, Vk::RIGHT]), 0x63);
        assert_eq!(dir(&[Vk::RIGHT]), 0x66);
        assert_eq!(dir(&[Vk::LEFT]), 0x64);
        assert_eq!(dir(&[Vk::UP, Vk::DOWN]), 0x68);
        assert_eq!(dir(&[Vk::LEFT, Vk::RIGHT]), 0x66);
        assert_eq!(dir(&[Vk::UP, Vk::DOWN, Vk::LEFT]), 0x67);
        assert_eq!(dir(&[Vk::UP, Vk::LEFT, Vk::RIGHT]), 0x69);
        assert_eq!(dir(&[Vk::DOWN, Vk::LEFT, Vk::RIGHT]), 0x63);
        assert_eq!(dir(&[Vk::UP, Vk::DOWN, Vk::LEFT, Vk::RIGHT]), 0x69);
    }

    #[test]
    fn arrow_dir_sees_latched_tap() {
        let mut l = InputAccumulator::new();
        l.key_down(Vk::RIGHT);
        l.key_up(Vk::RIGHT);
        assert_eq!(read_arrow_dir(&l.take_tick()), 0x66);
        assert_eq!(read_arrow_dir(&l.take_tick()), 0);
    }
}
