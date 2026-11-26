//! Info boxes, the host intro and the current-player box (F1.4, F4.2, F6.2).
//!
//! - `DrawInfoBox` 0x407e90: player p's box Mm_IbN frame 41 - money / 25000 at slide
//!   step s (table 0x41fc10): (s-164, 0), (640-s, 0), (s-164, 288), (640-s, 288); head
//!   Mm_Heads frame millionaire*10 + min(level, 9) + 1 at offsets (7,4) (58,5) (9,62)
//!   (58,60); Ms_Small frame level + 1 at (14,146) (59,146) (14,27) (59,27); level =
//!   `MoneyToDamageLevel` 0x404740. Only players 0..players-1; a step outside 0..15 is
//!   clamped and reported.
//! - `InfoBoxesSlideIn` 0x405d10: q++, Mm_Bg frame 1, event 2; steps 0, 1, ... until a
//!   clamp, then hold step 15 until the voice queue is empty.
//! - `HostIntro` 0x405e30: lip-sync on; event 0x2e(remaining + 1) if the last beaten
//!   player has no money left, else 0x39 after a question nobody buzzed, else 0x2f on
//!   every 4th question in a close race (0x405cc0), else 0x2d(q, players-1); boxes at
//!   step 15 until the voice ends. On question 1 a pumped key other than 'C' plays 0x19
//!   and 0x3c once.
//! - `InfoBoxesSlideOut` 0x405fe0: steps 15, 14, ... until a clamp, then until the voice
//!   is idle.
//! - Qs_PnBox 0x4080a0 (multiplayer): the answerer's box at (20,0): intro 1..6 / 10..15 /
//!   19..24 / 28..33 until it has looped once, then hold 8 / 17 / 26 / 35. The table
//!   entries keep their loop counts for the program run, so from the second time a
//!   player answers the intro shows one frame.

use super::host_anim::{self, HostAnim};
use super::{Art, Bg, set_scene};
use crate::anim::Anim;
use crate::ctx::{Abort, Ctx};
use crate::screens::pause;
use crate::sound_events::{sound_event, sound_event3};
use crate::state::Game;

/// Slide offsets (0x41fc10).
pub const SLIDE: [i32; 16] = [
    0, 16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 164, 156, 148, 156, 164,
];
const HEAD_OFFSETS: [(i32, i32); 4] = [(7, 4), (58, 5), (9, 62), (58, 60)];
const SMALL_OFFSETS: [(i32, i32); 4] = [(14, 146), (59, 146), (14, 27), (59, 27)];
/// Qs_PnBox anims 0x41fd28: intro per player, then hold per player.
pub const PNBOX_ANIMS: [(i32, i32); 8] = [
    (1, 6),
    (10, 15),
    (19, 24),
    (28, 33),
    (8, 8),
    (17, 17),
    (26, 26),
    (35, 35),
];
pub const PNBOX_POS: (i32, i32) = (20, 0);

/// `MoneyToDamageLevel` 0x404740.
pub fn damage_level(money: i32) -> i32 {
    const STEPS: [i32; 9] = [
        900_000, 800_000, 700_000, 600_000, 500_000, 400_000, 300_000, 200_000, 100_000,
    ];
    match STEPS.iter().position(|&t| money > t) {
        Some(i) => i as i32,
        None if money < 1 => 10,
        None => 9,
    }
}

/// 0x405cc0 (multiplayer only): max - min money of the players still in is <= 100000.
pub fn close_race(game: &Game) -> bool {
    if game.player_count <= 1 {
        return false;
    }
    let (mut lo, mut hi) = (1_000_000, 0);
    for p in &game.players {
        if p.millionaire >= 0 && p.money > 0 {
            lo = lo.min(p.money);
            hi = hi.max(p.money);
        }
    }
    hi - lo <= 100_000
}

/// `DrawInfoBox` 0x407e90; returns true when `step` was clamped.
pub fn draw_info_box(ctx: &mut Ctx, game: &Game, art: &Art, p: usize, step: i32) -> bool {
    if p >= game.player_count {
        return false;
    }
    let clamped = !(0..16).contains(&step);
    let step = step.clamp(0, 15) as usize;
    let player = &game.players[p];
    let level = damage_level(player.money);
    let s = SLIDE[step];
    let x = if p.is_multiple_of(2) {
        s - 164
    } else {
        640 - s
    };
    let y = if p < 2 { 0 } else { 288 };
    art.ib[p].draw(ctx, 41 - player.money / 25_000, x, y);
    let (hx, hy) = HEAD_OFFSETS[p];
    art.heads[p].draw(
        ctx,
        level.min(9) + 1 + player.millionaire * 10,
        x + hx,
        y + hy,
    );
    let (mx, my) = SMALL_OFFSETS[p];
    art.ms_small.draw(ctx, level + 1, x + mx, y + my);
    clamped
}

fn draw_boxes(ctx: &mut Ctx, game: &Game, art: &Art, step: i32) -> bool {
    let mut any = false;
    for p in 0..4 {
        any |= draw_info_box(ctx, game, art, p, step);
    }
    any
}

fn host(ctx: &mut Ctx, game: &mut Game, art: &Art) {
    let loud = host_anim::loud(ctx, game.lip_sync);
    let h: &mut HostAnim = &mut game.session.host;
    h.tick(ctx, art.regis, loud);
}

/// Mm_Bg at frame 1 (the four stores the Mm_Bg screens begin with).
fn main_scene(game: &mut Game) {
    set_scene(game, Bg::Main, 1, 1);
}

/// The body both slides share: `step` moves by `dir` until a box clamps, then the
/// voice queue must drain.
fn slide(ctx: &mut Ctx, game: &mut Game, art: &Art, mut step: i32, dir: i32) -> Result<(), Abort> {
    let mut done = false;
    super::draw_scene(ctx, game, art);
    ctx.pump();
    loop {
        host(ctx, game, art);
        let clamped = draw_boxes(ctx, game, art, step);
        ctx.present()?;
        pause::on_esc(ctx, game)?;
        ctx.pump_voice();
        if !done && clamped {
            if dir > 0 {
                step -= 1;
            }
            done = true;
        } else if !done {
            step += dir;
        }
        if done && !ctx.voice_busy() {
            return Ok(());
        }
        super::draw_scene(ctx, game, art);
        ctx.pump();
    }
}

/// `InfoBoxesSlideIn` 0x405d10.
pub fn slide_in(ctx: &mut Ctx, game: &mut Game, art: &Art) -> Result<(), Abort> {
    ctx.enter_screen("InfoBoxesIn");
    game.round.q += 1;
    main_scene(game);
    sound_event(ctx, 0x02, game.player_count as i32 - 1);
    slide(ctx, game, art, 0, 1)
}

/// `InfoBoxesSlideOut` 0x405fe0.
pub fn slide_out(ctx: &mut Ctx, game: &mut Game, art: &Art) -> Result<(), Abort> {
    ctx.enter_screen("InfoBoxesOut");
    main_scene(game);
    slide(ctx, game, art, 15, -1)
}

/// `HostIntro` 0x405e30.
pub fn host_intro(ctx: &mut Ctx, game: &mut Game, art: &Art) -> Result<(), Abort> {
    ctx.enter_screen("HostIntro");
    main_scene(game);
    game.lip_sync = true;
    let r = &game.round;
    let beaten_broke = r.beaten.is_some_and(|b| game.players[b].money == 0);
    let (ev, a, b) = if beaten_broke {
        (0x2e, game.remaining_opponents + 1, 0)
    } else if r.timeouts >= 1 {
        (0x39, 0, 0)
    } else if r.q % 4 == 0 && close_race(game) {
        (0x2f, 0, 0)
    } else {
        (0x2d, r.q, game.player_count as i32 - 1)
    };
    sound_event3(ctx, ev, a, b, 0);
    super::draw_scene(ctx, game, art);
    ctx.pump();
    let mut interrupted = false;
    loop {
        host(ctx, game, art);
        draw_boxes(ctx, game, art, 15);
        ctx.present()?;
        pause::on_esc(ctx, game)?;
        ctx.pump_voice();
        if game.round.q == 1
            && !interrupted
            && ctx.last_key().is_some_and(|k| k.0 != 0 && k.0 != b'C')
        {
            sound_event(ctx, 0x19, 0);
            sound_event(ctx, 0x3c, 0);
            interrupted = true;
        }
        if !ctx.voice_busy() {
            game.lip_sync = false;
            return Ok(());
        }
        super::draw_scene(ctx, game, art);
        ctx.pump();
    }
}

/// Qs_PnBox state (0x41fde8 last player, 0x425e70 state, the anim table 0x41fd28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnBox {
    pub anims: [Anim; 8],
    last: i32,
    /// 0 none, 1 intro, 2 hold.
    state: u8,
    cur: usize,
}

impl Default for PnBox {
    fn default() -> Self {
        Self::new()
    }
}

impl PnBox {
    pub fn new() -> Self {
        Self {
            anims: PNBOX_ANIMS.map(|(start, end)| Anim {
                start,
                end,
                cur: 0,
                loops: 0,
            }),
            last: -1,
            state: 0,
            cur: 0,
        }
    }
}

/// 0x4080a0.
pub fn draw_pnbox(ctx: &mut Ctx, game: &mut Game, art: &Art) {
    let b = &mut game.session.pnbox;
    let Some(a) = game.round.answerer else {
        b.state = 0;
        return;
    };
    if game.player_count <= 1 {
        return;
    }
    if b.last == a as i32 && b.state != 0 {
        if b.state == 1 && b.anims[b.cur].loops == 1 {
            b.state = 2;
            b.cur = a + 4;
            b.anims[b.cur].restart();
        }
    } else {
        b.last = a as i32;
        b.state = 1;
        b.cur = a;
        b.anims[b.cur].restart();
    }
    let anim = &mut b.anims[b.cur];
    art.pnbox.draw(ctx, anim.cur, PNBOX_POS.0, PNBOX_POS.1);
    anim.advance();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_levels() {
        let cases = [
            (1_000_000, 0),
            (900_001, 0),
            (900_000, 1),
            (500_001, 4),
            (100_001, 8),
            (100_000, 9),
            (1, 9),
            (0, 10),
            (-5, 10),
        ];
        for (money, level) in cases {
            assert_eq!(damage_level(money), level, "{money}");
        }
    }
}
