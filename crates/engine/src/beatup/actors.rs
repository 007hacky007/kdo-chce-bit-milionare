//! The beat-up actors (F5.1, F5.3a): sprite table 0x41d760, the actor list 0x41de68, the
//! anim helpers 0x40da70 / 0x40db00 / 0x40da30, the z-ordered draw list 0x40ecd0 and the
//! three small actors GetReady 0x401d70, crowd 0x401eb0 and MStones 0x401c10.

use crate::anim::Anim;
use crate::ctx::Ctx;
use crate::game::infobox::damage_level;
use crate::sound_events::{sound_event, sound_event3};
use crate::state::{Game, Sprite};

use super::Fight;

/// Actor indices (the original's structs 0x41d270, 0x41d418, 0x41d4d0, 0x41d5d0,
/// 0x41d6b8).
pub const FIST: usize = 0;
pub const MILL: usize = 1;
pub const STONES: usize = 2;
pub const READY: usize = 3;
pub const CROWD: usize = 4;

/// Slots of the actor list 0x41de68 (-1 terminated).
pub const SLOTS: usize = 7;
/// The list as shipped: GetReady and the crowd are registered statically.
pub const STATIC_SLOTS: [Option<usize>; SLOTS] =
    [Some(READY), Some(CROWD), None, None, None, None, None];

/// Draw list capacity (0x40ee00).
pub const DRAW_LIST_MAX: usize = 200;

/// Sprite table 0x41d760 (24 entries, 1-based ids), all in `Beat/`. Entries 15 and 16
/// are both FistM5B (F11.2 KEEP).
pub const SPRITE_FILES: [&str; 24] = [
    "Fsts_All.Smk",
    "FistM1A.Smk",
    "FistM1B.Smk",
    "FistM1C.Smk",
    "FistM2C.Smk",
    "FistM2A.Smk",
    "FistM2B.Smk",
    "FistM3C.Smk",
    "FistM3A.Smk",
    "FistM3B.Smk",
    "FistM4A.Smk",
    "FistM4C.Smk",
    "FistM4B.Smk",
    "FistM5A.Smk",
    "FistM5B.Smk",
    "FistM5B.Smk",
    "Mill_01.Smk",
    "Mill_02.Smk",
    "Mill_03.Smk",
    "Mill_04.Smk",
    "Mill_05.Smk",
    "MStones.Smk",
    "GetReady.Smk",
    "fx.Smk",
];
pub const SPRITE_MSTONES: i32 = 22;
pub const SPRITE_GETREADY: i32 = 23;
pub const SPRITE_FX: i32 = 24;

/// Fist anims 0x41d1c8 (every fist sprite shares them): idle, Right, Left, Up, Down,
/// Shift+Up, Shift+Down.
pub const FIST_ANIMS: [(i32, i32); 7] = [
    (1, 3),
    (5, 6),
    (8, 9),
    (11, 12),
    (14, 15),
    (17, 18),
    (20, 22),
];
/// Millionaire anims 0x41d2f8 as shipped. Entries 0 (idle) and 8..10 (blocks) are
/// rewritten by 0x401b90 for the damage level; 1 (after the KO) has no frame.
pub const MILL_ANIMS: [(i32, i32); 12] = [
    (-1, -1),
    (-1, -1),
    (1, 3),
    (5, 7),
    (9, 11),
    (13, 15),
    (17, 19),
    (21, 23),
    (-1, -1),
    (-1, -1),
    (-1, -1),
    (76, 98),
];
/// MStones anims 0x41d4a0; entry 1 is set to the damage level + 1 on each drop-in.
pub const STONES_ANIMS: [(i32, i32); 2] = [(-1, -1), (-1, -1)];
/// GetReady anims 0x41d558: frame = beater + 1.
pub const READY_ANIMS: [(i32, i32); 5] = [(-1, -1), (1, 1), (2, 2), (3, 3), (4, 4)];
/// Crowd anims 0x41d658.
pub const CROWD_ANIMS: [(i32, i32); 4] = [(-1, -1), (1, 3), (5, 9), (11, 13)];

/// MStones y path (stack table of 0x401c10), 30 entries (0x41de88).
pub const STONES_PATH: [i32; 30] = [
    0, 50, 100, 150, 170, 173, 175, 173, 175, 175, 175, 175, 175, 175, 175, 175, 175, 175, 175,
    175, 175, 175, 173, 175, 173, 170, 150, 100, 50, 0,
];
/// GetReady drop-in y path in state 0 (8 entries, 0x41de8c).
pub const READY_IN: [i32; 8] = [0, 100, 200, 300, 400, 450, 420, 450];
/// GetReady exit y path by counter 1..5 (0x41de90 = 5); index 0 is never read.
pub const READY_OUT: [i32; 6] = [0, 0, 100, 200, 300, 400];

/// One actor struct (0x41d270 and siblings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    pub sprite: Sprite,
    /// +0x08 == 1.
    pub active: bool,
    /// +0x0c / +0x10: bottom-centre anchor.
    pub x: i32,
    pub y: i32,
    /// +0x44.
    pub z: i32,
    /// +0x50 / +0x58: current anim index, -1 after a reset.
    pub anim_id: i32,
    /// +0x60..+0x6c.
    pub anim: Anim,
}

impl Actor {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self {
            sprite: Sprite::ABSENT,
            active: true,
            x,
            y,
            z,
            anim_id: -1,
            anim: Anim::new(-1, -1),
        }
    }

    /// 0x40da30: anim -1 with range -1..-1 (draws nothing), loops 0.
    pub fn reset(&mut self) {
        self.anim_id = -1;
        self.anim = Anim::new(-1, -1);
    }

    /// 0x40db00.
    pub fn anim_done(&self) -> bool {
        self.anim.loops != 0 && self.anim.cur == self.anim.start
    }
}

/// One entry of the draw list (0x431910, 0x34 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawItem {
    pub sprite: Sprite,
    pub frame: i32,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// 0x40ecd0: insert keeping the list sorted by (z, y), exactly as the original walks
/// its linked list (an equal key goes before the head and before the first equal entry
/// it meets, but after equal-z entries passed while skipping lower y).
pub fn insert_sorted(list: &mut Vec<DrawItem>, it: DrawItem) {
    if list.len() >= DRAW_LIST_MAX {
        return;
    }
    let before = |n: &DrawItem| it.z < n.z || (it.z == n.z && it.y <= n.y);
    if list.first().is_none_or(before) {
        list.insert(0, it);
        return;
    }
    let mut cur = 0;
    while let Some(next) = list.get(cur + 1) {
        if before(next) {
            break;
        }
        if it.z == next.z {
            cur += 1;
            while let Some(n) = list.get(cur + 1) {
                if it.z != n.z || it.y < n.y {
                    break;
                }
                cur += 1;
            }
            break;
        }
        cur += 1;
    }
    list.insert(cur + 1, it);
}

impl Fight {
    fn table(&self, actor: usize) -> &[(i32, i32)] {
        match actor {
            FIST => &FIST_ANIMS,
            MILL => &self.mill_anims,
            STONES => &self.stones_anims,
            READY => &READY_ANIMS,
            _ => &CROWD_ANIMS,
        }
    }

    /// `SetAnim` 0x40da70.
    pub fn set_anim(&mut self, actor: usize, n: i32) {
        if self.actors[actor].anim_id == n && n != -1 {
            return;
        }
        let (start, end) = usize::try_from(n)
            .ok()
            .and_then(|i| self.table(actor).get(i).copied())
            .unwrap_or((-1, -1));
        let a = &mut self.actors[actor];
        a.anim.loops = 0;
        a.anim_id = n;
        if a.anim.start != start || a.anim.end != end {
            a.anim.set(start, end);
        }
    }

    /// 0x40d790: put the actor into the first free slot (also when it is already
    /// listed) and activate it.
    pub fn register(&mut self, actor: usize) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.is_none()) {
            *slot = Some(actor);
            self.actors[actor].active = true;
        }
    }

    /// 0x40d7e0: clear the first slot holding the actor and deactivate it.
    pub fn unregister(&mut self, actor: usize) {
        if let Some(slot) = self.slots.iter_mut().find(|s| **s == Some(actor)) {
            *slot = None;
        }
        self.actors[actor].active = false;
    }

    /// The per-tick callback pass of 0x4014d0, in slot order. The loop re-reads the
    /// list, so an actor registered into a later slot runs in the same pass.
    pub fn update(&mut self, ctx: &mut Ctx, game: &mut Game) {
        for slot in 0..SLOTS {
            let Some(a) = self.slots[slot] else { continue };
            if !self.actors[a].active {
                continue;
            }
            match a {
                FIST => self.fist_update(ctx),
                MILL => self.mill_update(ctx, game),
                STONES => self.stones_update(ctx, game),
                READY => self.ready_update(),
                _ => self.crowd_update(ctx),
            }
        }
    }

    /// 0x401580: every active actor into the draw list with its current frame, then its
    /// anim advances; the list is drawn bottom-centre anchored (F6.1).
    pub fn draw(&mut self, ctx: &mut Ctx) {
        let mut list = Vec::with_capacity(SLOTS);
        for slot in 0..SLOTS {
            let Some(i) = self.slots[slot] else { continue };
            let a = &mut self.actors[i];
            if !a.active {
                continue;
            }
            insert_sorted(
                &mut list,
                DrawItem {
                    sprite: a.sprite,
                    frame: a.anim.cur,
                    x: a.x,
                    y: a.y,
                    z: a.z,
                },
            );
            a.anim.advance();
        }
        for it in list {
            it.sprite.draw_anchored(ctx, it.frame, it.x, it.y);
        }
    }

    /// `GetReady_Update` 0x401d70: the beater's "get ready" sign drops in during state 0
    /// and leaves upwards once the fight starts.
    fn ready_update(&mut self) {
        let a = self.actors[READY].anim_id;
        if a == -1 {
            if self.beater < 4 {
                self.set_anim(READY, self.beater as i32 + 1);
            }
            self.actors[READY].y = 0;
            self.ready_counter = 0;
            return;
        }
        if a == 0 {
            return;
        }
        if self.state == 0 {
            if let Some(&y) = READY_IN.get(self.ready_counter as usize) {
                self.actors[READY].y = y;
                self.ready_counter += 1;
            }
            return;
        }
        if self.state < 0 {
            return;
        }
        self.ready_counter = self.ready_counter.min(5);
        if self.ready_counter == 0 {
            self.set_anim(READY, 0);
            return;
        }
        self.actors[READY].y = READY_OUT[self.ready_counter as usize];
        self.ready_counter -= 1;
    }

    /// `Fx_Update` 0x401eb0: after time-up with >= 10 hits the crowd cheers (one of three
    /// loops, kept until the round ends).
    fn crowd_update(&mut self, ctx: &mut Ctx) {
        let a = self.actors[CROWD].anim_id;
        if a == -1 {
            self.set_anim(CROWD, 0);
            return;
        }
        if a != 0 || self.state != 2 || self.hits < 10 {
            return;
        }
        let n = (ctx.rng.next() % 3) as i32 + 1;
        self.set_anim(CROWD, n);
        sound_event3(ctx, 0x27, 0, n - 1, 0);
    }

    /// `MStones_Update` 0x401c10: the damage-level stones drop in and leave again over
    /// 30 ticks, then the actor unregisters itself.
    fn stones_update(&mut self, ctx: &mut Ctx, game: &Game) {
        if self.actors[STONES].anim_id == -1 {
            let s = &mut self.actors[STONES];
            s.x = 320;
            s.y = 0;
            self.stones_counter = 0;
            let frame = damage_level(game.players[self.beaten].money) + 1;
            self.stones_anims[1] = (frame, frame);
            self.set_anim(STONES, 1);
            sound_event(ctx, 0x18, 0);
            if frame != 11 {
                sound_event(ctx, 0x56, frame - 2);
            }
            return;
        }
        self.stones_counter += 1;
        if self.stones_counter >= STONES_PATH.len() as i32 {
            self.stones_counter = 0;
            self.unregister(STONES);
            return;
        }
        self.actors[STONES].y = STONES_PATH[self.stones_counter as usize];
    }
}

/// Load `Beat/<entry>` (1-based sprite id) as a transparent overlay (usage 5).
pub fn sprite_file(id: i32) -> String {
    let i = usize::try_from(id - 1)
        .ok()
        .filter(|&i| i < SPRITE_FILES.len())
        .unwrap_or(0);
    format!("Beat/{}", SPRITE_FILES[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(z: i32, y: i32, frame: i32) -> DrawItem {
        DrawItem {
            sprite: Sprite::ABSENT,
            frame,
            x: 0,
            y,
            z,
        }
    }

    #[test]
    fn draw_list_sorts_by_z_then_y() {
        let mut l = Vec::new();
        for (z, y, f) in [
            (301, 450, 1),
            (300, 183, 2),
            (300, 479, 3),
            (200, 479, 4),
            (301, 0, 5),
        ] {
            insert_sorted(&mut l, item(z, y, f));
        }
        let order: Vec<i32> = l.iter().map(|i| i.frame).collect();
        assert_eq!(order, [4, 2, 3, 5, 1]);
        // Equal keys: before the head, but after equal-z entries skipped by y.
        let mut l = vec![item(300, 10, 1)];
        insert_sorted(&mut l, item(300, 10, 2));
        assert_eq!(l.iter().map(|i| i.frame).collect::<Vec<_>>(), [2, 1]);
        let mut l = vec![item(100, 0, 1), item(300, 5, 2), item(300, 10, 3)];
        insert_sorted(&mut l, item(300, 10, 4));
        assert_eq!(l.iter().map(|i| i.frame).collect::<Vec<_>>(), [1, 2, 3, 4]);
    }
}
