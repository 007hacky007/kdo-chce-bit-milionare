//! Button groups (F6.2; E3 0x40d370, 0x40d040, 0x40d150, 0x40d2d0 and its helpers
//! 0x40d520, 0x40d3a0, 0x40d1b0, 0x40d440, 0x40d480).
//!
//! A group is a static table of buttons (generated into [`crate::button_tables`]) plus
//! a small state record. [`ButtonGroup::update`] is `ButtonGroupUpdate` 0x40d2d0:
//!
//! - mode 1 pumps messages ([`Ctx::pump`]) and clears the result; a hotkey equal to the
//!   pumped key presses the first matching button that is not disabled (result = its id);
//! - mode 2 keeps the result and neither pumps nor reads keys;
//! - mode 0 clears the result, flushes input and only draws.
//!
//! Without a result the cursor (when the mouse is enabled) hovers a button whose rect
//! contains it (`PtInRect`, right/bottom exclusive) and a pumped left click presses it.
//! Drawing blits every visible button's current-state anim frame and advances that anim.

use crate::anim::Anim;
use crate::ctx::Ctx;
use crate::input::{MouseButton, Vk};
use crate::state::Sprite;

/// Button states (+0x24).
pub const NORMAL: i32 = 0;
pub const HOVER: i32 = 1;
pub const PRESSED: i32 = 2;
pub const DISABLED: i32 = 3;
pub const STATIC: i32 = 4;
pub const HIDDEN: i32 = -1;

/// One 0x90-byte table entry (numbers only, from the exe's tables).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonDef {
    /// Address of the overlay buffer the button draws from (identifies the sprite).
    pub smk: u32,
    pub x: i32,
    pub y: i32,
    /// Hit rect l, t, r, b; -1 = derived from the sprite size, -3 = none.
    pub rect: [i32; 4],
    /// Hotkey VK (0 = none).
    pub key: u8,
    /// Result id.
    pub id: i32,
    pub state: i32,
    /// Start/end frames of the normal, hover, pressed and disabled anims (-1 = none).
    pub anims: [[i32; 2]; 4],
}

#[derive(Debug, Clone)]
pub struct Button {
    pub def: ButtonDef,
    pub sprite: Sprite,
    pub rect: [i32; 4],
    pub state: i32,
    pub anims: [Anim; 4],
}

impl Button {
    fn reset_anim(&mut self, which: usize) {
        let [s, e] = self.def.anims[which];
        self.anims[which].set(s, e);
    }
}

/// Mode argument of [`ButtonGroup::update`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 0: clear the result, flush input, draw only.
    Flush = 0,
    /// 1: pump messages and take a hotkey.
    Pump = 1,
    /// 2: keep the result, no pump.
    Keep = 2,
}

#[derive(Debug, Clone)]
pub struct ButtonGroup {
    pub buttons: Vec<Button>,
    /// Hovered or pressed button (+0x08/+0x0c).
    pub current: Option<usize>,
    /// Ticks the same button has stayed hovered (+0x10) and the previous one (+0x14).
    pub hover_ticks: i32,
    prev_hover: Option<usize>,
    /// Result id (+0x18), `None` = -1.
    pub result: Option<i32>,
    /// +0x1c: 0 makes every update a flush.
    pub enabled: bool,
    /// +0x20 bit 0: a press animation is running; set by screens that wait for it.
    pub pressing: bool,
    /// +0x24 bit 0: ignore the mouse (the menu screens set it).
    pub mouse_disabled: bool,
}

impl ButtonGroup {
    /// `ButtonGroupInit` 0x40d370 + 0x40d040 (auto rects) + 0x40d150 (every visible
    /// button back to normal), as every menu does on entry. `sprite_for` maps a table's
    /// overlay buffer address to the loaded sprite.
    pub fn new(defs: &[ButtonDef], sprite_for: impl Fn(u32) -> Sprite) -> Self {
        let buttons = defs
            .iter()
            .map(|d| {
                let sprite = sprite_for(d.smk);
                let rect = if d.rect[0] == -1 {
                    [d.x, d.y, sprite.width - 1 + d.x, sprite.height - 1 + d.y]
                } else {
                    d.rect
                };
                let anims = d.anims.map(|[s, e]| Anim::new(s, e));
                Button {
                    def: *d,
                    sprite,
                    rect,
                    state: d.state,
                    anims,
                }
            })
            .collect();
        let mut g = Self {
            buttons,
            current: None,
            hover_ticks: 0,
            prev_hover: None,
            result: None,
            enabled: true,
            pressing: false,
            mouse_disabled: false,
        };
        for i in (0..g.buttons.len()).rev() {
            g.set_normal(i);
        }
        g
    }

    /// 0x40d180: back to normal (hidden buttons stay hidden).
    pub fn set_normal(&mut self, i: usize) {
        let b = &mut self.buttons[i];
        if b.state != HIDDEN {
            b.state = NORMAL;
            b.reset_anim(0);
        }
    }

    /// 0x40d290: disabled (`true`) or hidden (`false`).
    pub fn set_disabled(&mut self, i: usize, disabled: bool) {
        let b = &mut self.buttons[i];
        b.state = if disabled { DISABLED } else { HIDDEN };
        b.reset_anim(3);
    }

    /// 0x40d090 / 0x40d0b0 / 0x40d0d0 / 0x40d0f0: replace anim `which` (0 normal, 1 hover,
    /// 2 pressed, 3 disabled) with `start..end` at `start`. The original edits its static
    /// table in place, so the new range is also what later resets (and [`Self::defs`])
    /// use.
    pub fn set_anim(&mut self, i: usize, which: usize, start: i32, end: i32) {
        let b = &mut self.buttons[i];
        b.def.anims[which] = [start, end];
        b.anims[which].set(start, end);
    }

    /// 0x40d110: all four anims of button `i` set to `start..end`.
    pub fn set_all_anims(&mut self, i: usize, start: i32, end: i32) {
        for which in 0..4 {
            self.set_anim(i, which, start, end);
        }
    }

    /// The table as the original leaves it in memory: each entry with its current state
    /// and anim ranges (persist these to rebuild the group later).
    pub fn defs(&self) -> Vec<ButtonDef> {
        self.buttons
            .iter()
            .map(|b| ButtonDef {
                state: b.state,
                ..b.def
            })
            .collect()
    }

    /// 0x40d260: start the press anim unless already pressed or static.
    pub fn press(&mut self, i: usize) {
        let b = &mut self.buttons[i];
        if b.state != PRESSED && b.state != STATIC {
            b.state = PRESSED;
            b.reset_anim(2);
        }
    }

    /// `ButtonGroupUpdate` 0x40d2d0; draws when `draw` is set.
    pub fn update(&mut self, ctx: &mut Ctx, mode: Mode, draw: bool) {
        let mut mode = mode;
        if !self.pressing {
            match mode {
                Mode::Pump => {
                    ctx.pump();
                    self.result = None;
                }
                Mode::Flush => {
                    self.result = None;
                    ctx.flush_input();
                }
                Mode::Keep => {}
            }
        } else {
            mode = Mode::Keep;
        }
        if mode != Mode::Flush {
            if !self.enabled {
                ctx.flush_input();
            } else {
                self.take_key(ctx.last_key(), mode);
                if !self.pressing && self.result.is_none() {
                    self.hover(ctx.input.mouse);
                    self.update_states();
                    self.click(ctx.last_mouse());
                }
            }
        }
        if draw {
            self.draw(ctx);
        }
    }

    /// 0x40d520.
    fn take_key(&mut self, key: Option<Vk>, mode: Mode) {
        if !self.pressing {
            let Some(Vk(k)) = key else { return };
            if mode != Mode::Pump || k == 0 {
                return;
            }
            if let Some(i) = self
                .buttons
                .iter()
                .position(|b| b.state != DISABLED && b.def.key == k)
            {
                self.press(i);
                self.current = Some(i);
                self.result = Some(self.buttons[i].def.id);
            }
        } else if let Some(i) = self.current {
            let a = &self.buttons[i].anims[2];
            if a.cur == a.end {
                self.pressing = false;
            }
        }
    }

    /// 0x40d3a0: the button under the cursor, unless the mouse is ignored.
    fn hover(&mut self, (x, y): (i32, i32)) {
        self.current = None;
        if self.mouse_disabled {
            return;
        }
        self.current = self.buttons.iter().position(|b| {
            let [l, t, r, bt] = b.rect;
            l <= x && x < r && t <= y && y < bt && b.state != DISABLED && b.state != STATIC
        });
    }

    /// 0x40d1b0: un-hover the others, hover the current one, count hover ticks.
    fn update_states(&mut self) {
        for i in (0..self.buttons.len()).rev() {
            if Some(i) != self.current && self.buttons[i].state == HOVER {
                self.set_normal(i);
            }
        }
        if let Some(c) = self.current
            && self.buttons[c].state == NORMAL
        {
            self.buttons[c].state = HOVER;
            self.buttons[c].reset_anim(1);
        }
        let state = self.current.map(|c| self.buttons[c].state);
        if self.current.is_none()
            || self.current != self.prev_hover
            || state == Some(DISABLED)
            || state == Some(HIDDEN)
        {
            self.hover_ticks = 0;
            self.prev_hover = self.current;
        } else {
            self.hover_ticks += 1;
        }
    }

    /// 0x40d440: a pumped left click on the hovered button presses it.
    fn click(&mut self, mouse: Option<MouseButton>) {
        if mouse != Some(MouseButton::Left) {
            return;
        }
        if let Some(c) = self.current {
            let s = self.buttons[c].state;
            if s != PRESSED && s != STATIC {
                self.result = Some(self.buttons[c].def.id);
                self.press(c);
            }
        }
    }

    /// 0x40d480: last button first; static buttons draw their normal anim.
    pub fn draw(&mut self, ctx: &mut Ctx) {
        for b in self.buttons.iter_mut().rev() {
            if b.sprite.id.is_none() || b.state == HIDDEN {
                continue;
            }
            let which = if b.state == STATIC {
                0
            } else {
                b.state as usize
            };
            let a = &mut b.anims[which];
            b.sprite.draw(ctx, a.cur, b.def.x, b.def.y);
            a.advance();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::button_tables::TABLE_41E030;
    use crate::headless::{HeadlessHost, ScriptEvent};
    use render::{AssetId, DrawOp};

    fn sprite() -> Sprite {
        Sprite {
            id: Some(AssetId(1)),
            transparent: true,
            width: 84,
            height: 84,
            frames: 39,
        }
    }

    fn smk_frames(ctx: &Ctx) -> Vec<(u32, i32, i32)> {
        ctx.last_presented()
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Smk { frame, x, y, .. } => Some((*frame, *x, *y)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn auto_rect_from_sprite_size() {
        let g = ButtonGroup::new(&TABLE_41E030, |_| sprite());
        assert_eq!(g.buttons[0].rect, [133, 169, 216, 252]);
        assert!(g.buttons.iter().all(|b| b.state == NORMAL));
    }

    #[test]
    fn hotkey_presses_and_press_anim_runs_in_keep_mode() {
        let mut host = HeadlessHost::with_script(vec![(0, ScriptEvent::Tap(Vk::D3))]);
        let mut ctx = Ctx::new(&mut host, 1);
        let mut g = ButtonGroup::new(&TABLE_41E030, |_| sprite());
        g.mouse_disabled = true;
        ctx.present().unwrap();
        g.update(&mut ctx, Mode::Pump, true);
        assert_eq!(g.result, Some(102));
        assert_eq!(g.buttons[2].state, PRESSED);
        ctx.present().unwrap();
        assert_eq!(smk_frames(&ctx), [(21, 135, 261)]);
        // Keep mode: result stays, anim advances, no pump.
        g.update(&mut ctx, Mode::Keep, true);
        ctx.present().unwrap();
        assert_eq!(g.result, Some(102));
        assert_eq!(smk_frames(&ctx), [(22, 135, 261)]);
        // Pump mode without a key clears the result; the button stays pressed.
        g.update(&mut ctx, Mode::Pump, true);
        assert_eq!(g.result, None);
    }

    #[test]
    fn key_queued_across_unpumped_ticks_is_seen_by_next_pump() {
        let mut host = HeadlessHost::with_script(vec![(0, ScriptEvent::Tap(Vk::D1))]);
        let mut ctx = Ctx::new(&mut host, 1);
        let mut g = ButtonGroup::new(&TABLE_41E030, |_| sprite());
        g.mouse_disabled = true;
        for _ in 0..3 {
            ctx.present().unwrap();
            g.update(&mut ctx, Mode::Keep, false);
        }
        g.update(&mut ctx, Mode::Pump, false);
        assert_eq!(g.result, Some(100));
    }

    #[test]
    fn disabled_button_skipped_for_shared_hotkey() {
        use crate::button_tables::TABLE_41E270;
        let mut host = HeadlessHost::with_script(vec![(0, ScriptEvent::Tap(Vk::Z))]);
        let mut ctx = Ctx::new(&mut host, 1);
        let mut g = ButtonGroup::new(&TABLE_41E270, |_| sprite());
        g.set_disabled(1, true);
        ctx.present().unwrap();
        g.update(&mut ctx, Mode::Pump, false);
        assert_eq!(g.result, Some(205));
    }

    #[test]
    fn mouse_click_presses_hovered_button_unless_disabled() {
        let script = vec![
            (0, ScriptEvent::MouseMove(140, 175)),
            (0, ScriptEvent::MouseDown(MouseButton::Left)),
        ];
        for disabled in [false, true] {
            let mut host = HeadlessHost::with_script(script.clone());
            let mut ctx = Ctx::new(&mut host, 1);
            let mut g = ButtonGroup::new(&TABLE_41E030, |_| sprite());
            g.mouse_disabled = disabled;
            ctx.present().unwrap();
            g.update(&mut ctx, Mode::Pump, false);
            let want = if disabled { None } else { Some(100) };
            assert_eq!(g.result, want);
        }
    }
}
