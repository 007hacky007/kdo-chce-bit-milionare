//! `Millionaire_Update` 0x401730 and `SetupMillAnims` 0x401b90 (F5.3, F5.3a): blocks,
//! hit test, damage, damage-level stones and the knockout.
//!
//! Hits are tick exact (F11.8 KEEP): a punch lands only on the tick the fist shows its
//! listed frame, and only while the millionaire is idle or blocking.

use crate::audio_types::{AudioCmd, Channel};
use crate::ctx::Ctx;
use crate::game::infobox::damage_level;
use crate::game::sync_debug;
use crate::sound_events::sound_event;
use crate::state::Game;

use super::Fight;
use super::actors::{FIST, MILL, STONES};
use super::schedule::{apply_damage, cheat_knockout};

/// (hit anim of the millionaire, block group, punch sfx event).
pub type HitEffect = (i32, i32, u8);

/// (fist anim, fist frame) -> [`HitEffect`].
pub const HITS: [((i32, i32), HitEffect); 6] = [
    ((1, 5), (2, 8, 0x0b)),
    ((2, 8), (3, 8, 0x0b)),
    ((3, 11), (4, 9, 0x0b)),
    ((4, 14), (5, 10, 0x0a)),
    ((5, 17), (6, 9, 0x0b)),
    ((6, 21), (7, 8, 0x0b)),
];

/// Block anims (8 + rand() % 3 or the group of a repeated punch).
const BLOCKS: std::ops::RangeInclusive<i32> = 8..=10;
/// The block check runs on every 35th tick and reacts to a hit in the last 15 ticks.
const BLOCK_PERIOD: i32 = 35;
const BLOCK_RECENT: i32 = 15;
/// A block lasts at least this many ticks (loops of its single frame).
const BLOCK_TICKS: i32 = 30;
const KO_ANIM: i32 = 11;
const AFTER_KO_ANIM: i32 = 1;

impl Fight {
    /// 0x401b90: idle frame 25 + 2 * level, blocks 45 / 55 / 65 + level.
    pub(crate) fn setup_mill_anims(&mut self, game: &Game) {
        let l = damage_level(game.players[self.beaten].money);
        let t = &mut self.mill_anims;
        t[0] = (25 + 2 * l, 25 + 2 * l);
        t[8] = (45 + l, 45 + l);
        t[9] = (55 + l, 55 + l);
        t[10] = (65 + l, 65 + l);
    }

    pub(crate) fn mill_update(&mut self, ctx: &mut Ctx, game: &mut Game) {
        let char_ = game.players[self.beaten].millionaire;
        if self.actors[MILL].anim_id == -1 {
            self.setup_mill_anims(game);
            self.last_hit_tick = -99;
            self.last_hit = 0;
            self.last_group = 0;
            self.repeat = 0;
            self.tick = 0;
            self.set_anim(MILL, 0);
            return;
        }
        self.tick += 1;
        if self.state == 1 {
            let anim = self.actors[MILL].anim_id;
            if self.repeat >= 3 {
                self.set_anim(MILL, self.last_group);
            } else if self.tick % BLOCK_PERIOD == 0 {
                if self.tick - self.last_hit_tick < BLOCK_RECENT && !BLOCKS.contains(&anim) {
                    let n = 8 + (ctx.rng.next() % 3) as i32;
                    self.set_anim(MILL, n);
                }
            } else if BLOCKS.contains(&anim) && self.actors[MILL].anim.loops >= BLOCK_TICKS {
                self.setup_mill_anims(game);
                self.set_anim(MILL, 0);
            }
        }
        let anim = self.actors[MILL].anim_id;
        if anim == AFTER_KO_ANIM {
            if sound_event(ctx, 0x26, 1) != 0 {
                return;
            }
            self.actors[MILL].reset();
            self.state = 5;
            sound_event(ctx, 0x52, char_);
            game.remaining_opponents -= 1;
            if game.remaining_opponents <= 0 {
                self.champion = Some(self.beater);
            }
            return;
        }
        if anim == KO_ANIM {
            if self.actors[MILL].anim_done() {
                self.set_anim(MILL, AFTER_KO_ANIM);
            }
            return;
        }
        if self.state != 1 && BLOCKS.contains(&anim) {
            self.set_anim(MILL, 0);
            return;
        }
        if (2..=7).contains(&anim) {
            if !self.actors[MILL].anim_done() {
                return;
            }
            self.setup_mill_anims(game);
            if game.players[self.beaten].money != 0 {
                self.set_anim(MILL, 0);
                return;
            }
            self.set_anim(MILL, KO_ANIM);
            ctx.audio(AudioCmd::StopChannel(Channel::Music));
            sound_event(ctx, 0x26, 0);
            return;
        }
        // Idle or blocking: does the fist show a hit frame this tick?
        let fist = &self.actors[FIST];
        let Some(&(_, (hit, group, sfx))) = HITS
            .iter()
            .find(|(k, _)| *k == (fist.anim_id, fist.anim.cur))
        else {
            return;
        };
        if BLOCKS.contains(&anim) && anim == group {
            sound_event(ctx, 0x0d, char_);
            return;
        }
        if self.last_hit == hit {
            self.repeat += 1;
        } else {
            self.repeat = 1;
        }
        self.last_hit = hit;
        self.last_group = group;
        self.last_hit_tick = self.tick;
        self.set_anim(MILL, hit);
        sound_event(ctx, sfx, 0);
        sound_event(ctx, 0x0c, char_);
        let p = self.beaten;
        let before = damage_level(game.players[p].money);
        let special = game.round.special;
        game.players[p].money = apply_damage(game.players[p].money, game.player_count, special);
        if cheat_knockout(ctx, game) {
            game.players[p].money = 0;
            self.hits = 30;
        }
        if damage_level(game.players[p].money) != before {
            self.register(STONES);
            self.actors[STONES].reset();
        }
        if game.players[p].money <= 0 {
            game.players[p].money = 0;
            self.hits += 1;
            self.state = 4;
        } else {
            if self.hits % 5 == 2 {
                sound_event(ctx, 0x50, 0);
            }
            self.hits += 1;
        }
        sync_debug(ctx, game);
    }
}
