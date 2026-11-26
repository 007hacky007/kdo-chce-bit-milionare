//! `HostAnim` 0x408160 (F6.2, F7.5): the host Mm_Regis.smk at (178,65) on the Mm_Bg
//! screens (info-box slides, host intro, win and loss).
//!
//! The anim table 0x41fdf0 has 19 entries (start, end) and starts with every `cur` at 0;
//! entries keep their `cur`/`loops` for the whole program run. The state (an entry index)
//! starts at 0 each game. Each tick, with `loud` = the voice envelope at the play cursor
//! is >= 100 (0x40b840) and `done` = the current entry's `cur == start`:
//!
//! - end anims 6 / 10 / 14 / 18: keep playing until `done`, then idle (2, frame 13);
//! - loud: a start anim 3 / 7 / 11 / 15 moves to its loop 4 / 8 / 12 / 16 when `done`;
//!   idle (2) or state 0 picks a gesture from pool 0x41ffb8 {1, 3, 7, 11, 15} (pool draw
//!   as the sound pools: first draw and each wrap run `ShuffleSwap1000` on the shared
//!   RNG); every other state keeps playing;
//! - quiet: a loop 4 / 8 / 12 / 16 moves to its end anim 6 / 10 / 14 / 18; every other
//!   state drops to idle (2);
//!
//! then the current entry's `cur` frame is drawn and the entry advanced. Entering a state
//! restarts its entry (`cur = start`).

use crate::anim::Anim;
use crate::ctx::Ctx;
use crate::sound_events::Pool;
use crate::state::Sprite;

/// Anim table 0x41fdf0 (start, end).
pub const HOST_ANIMS: [(i32, i32); 19] = [
    (-1, -1),
    (1, 11),
    (13, 13),
    (15, 16),
    (18, 27),
    (29, 29),
    (31, 33),
    (35, 36),
    (38, 47),
    (49, 49),
    (51, 53),
    (55, 56),
    (58, 67),
    (69, 69),
    (71, 73),
    (75, 76),
    (78, 87),
    (89, 89),
    (91, 92),
];
/// Gesture pool 0x41ffb8.
pub const GESTURES: [u16; 5] = [1, 3, 7, 11, 15];
pub const POS: (i32, i32) = (178, 65);
/// Envelope threshold of 0x40b840.
pub const LOUD: u8 = 100;
const IDLE: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAnim {
    pub anims: [Anim; 19],
    /// 0x425e68.
    pub state: usize,
    pool: Pool,
}

impl Default for HostAnim {
    fn default() -> Self {
        Self::new()
    }
}

impl HostAnim {
    pub fn new() -> Self {
        Self {
            anims: HOST_ANIMS.map(|(start, end)| Anim {
                start,
                end,
                cur: 0,
                loops: 0,
            }),
            state: 0,
            pool: Pool::new(&GESTURES),
        }
    }

    /// GameLoop's `0x425e68 = 0` at the start of a game.
    pub fn reset_state(&mut self) {
        self.state = 0;
    }

    fn enter(&mut self, state: usize) {
        self.state = state;
        self.anims[state].restart();
    }

    /// One tick of 0x408160; draws the host with `sprite`.
    pub fn tick(&mut self, ctx: &mut Ctx, sprite: Sprite, loud: bool) {
        let s = self.state;
        let done = self.anims[s].cur == self.anims[s].start;
        match s {
            6 | 10 | 14 | 18 => {
                if done {
                    self.enter(IDLE);
                }
            }
            _ if loud => match s {
                3 | 7 | 11 | 15 => {
                    if done {
                        self.enter(s + 1);
                    }
                }
                0 | IDLE => {
                    let g = self.pool.draw(&mut ctx.rng) as usize;
                    // An empty pool would give 0; the original then stays in state 0.
                    self.enter(g.min(HOST_ANIMS.len() - 1));
                }
                _ => {}
            },
            4 | 8 | 12 | 16 => self.enter(s + 2),
            _ => self.enter(IDLE),
        }
        let a = &mut self.anims[self.state];
        sprite.draw(ctx, a.cur, POS.0, POS.1);
        a.advance();
    }
}

/// 0x40b840 as the host sees it: the lip-sync flag was on when the clip was loaded and
/// the envelope at the voice play cursor is loud.
pub fn loud(ctx: &Ctx, lip_sync: bool) -> bool {
    lip_sync && ctx.voice_envelope() >= LOUD
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::HeadlessHost;
    use render::AssetId;

    #[test]
    fn gesture_cycle_follows_loudness() {
        let mut host = HeadlessHost::with_script(vec![]);
        let mut ctx = Ctx::new(&mut host, 3);
        let sprite = Sprite {
            id: Some(AssetId(1)),
            transparent: true,
            width: 10,
            height: 10,
            frames: 92,
        };
        let mut h = HostAnim::new();
        h.tick(&mut ctx, sprite, false);
        assert_eq!(h.state, 2);
        // The first loud tick shuffles the pool (2000 draws) and takes its first item.
        let mut probe = ctx.rng.clone();
        let mut items: Vec<i32> = GESTURES.iter().map(|&g| i32::from(g)).collect();
        probe.shuffle_swap1000(&mut items);
        h.tick(&mut ctx, sprite, true);
        assert_eq!(h.state as i32, items[0]);
        assert_eq!(ctx.rng, probe);
        // A start anim moves to its loop once it has wrapped; quiet ends it.
        h.state = 3;
        h.anims[3].restart();
        h.tick(&mut ctx, sprite, true);
        assert_eq!(h.state, 4);
        h.tick(&mut ctx, sprite, false);
        assert_eq!(h.state, 6);
        for _ in 0..3 {
            h.tick(&mut ctx, sprite, false);
        }
        assert_eq!(h.state, 2);
    }
}
