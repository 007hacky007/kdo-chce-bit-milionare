//! `Fist_Update` 0x401650 (F5.2): the beater's fist.
//!
//! The arrow code ([`Fight::dir`], `0x434578`) is polled by `ReadArrowDir` after the
//! previous tick's draw, before its present; Shift is polled here (GetAsyncKeyState), so
//! both read the latched `held` view and a tap shorter than a tick counts once. Input is
//! read only in state 1 while the fist is idle; diagonals do nothing.

use crate::ctx::Ctx;
use crate::input::Vk;

use super::Fight;
use super::actors::FIST;

/// `ReadArrowDir` codes.
pub const DIR_RIGHT: u8 = 0x66;
pub const DIR_LEFT: u8 = 0x64;
pub const DIR_UP: u8 = 0x68;
pub const DIR_DOWN: u8 = 0x62;

impl Fight {
    pub(crate) fn fist_update(&mut self, ctx: &Ctx) {
        let a = self.actors[FIST].anim_id;
        if a == -1 {
            self.set_anim(FIST, 0);
            return;
        }
        if a != 0 {
            if self.actors[FIST].anim_done() {
                self.set_anim(FIST, 0);
            }
            return;
        }
        if self.state != 1 {
            return;
        }
        // Each test reads the code again, as the original does; at most one matches.
        match self.dir {
            DIR_RIGHT => self.set_anim(FIST, 1),
            DIR_LEFT => self.set_anim(FIST, 2),
            DIR_UP => self.set_anim(FIST, 3),
            DIR_DOWN => self.set_anim(FIST, 4),
            _ => {}
        }
        if ctx.input.is_down(Vk::SHIFT) {
            match self.dir {
                DIR_UP => self.set_anim(FIST, 5),
                DIR_DOWN => self.set_anim(FIST, 6),
                _ => {}
            }
        }
    }
}
