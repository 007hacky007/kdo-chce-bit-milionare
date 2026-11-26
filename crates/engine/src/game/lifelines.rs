//! Lifelines (F4.3; E3 0x407070, 0x403f90, 0x407280, 0x4073a0, 0x4074a0, 0x407580; E2
//! 0x404310, 0x4075a0, 0x404460, 0x404680, 0x4046f0).
//!
//! `run` 0x407070 (the answerer pressed 'N'): every normal slot greyed, event 4, the
//! chosen answer cleared; the question screen is composed once and the menu 0x403f90 is
//! drawn over it. Menu (table 0x41ed20, keys only): S 50:50, P pass the buck, K magic
//! 8-ball, V chicken; anims S 1..7 / P 11..17 / K 21..27 / V 33..39, switched to the hold
//! frame 9 / 19 / 29 / 41 when they reach their end frame (so the end frame itself is
//! never shown). S needs the 50:50 flag (a 50:50 already used on this question only
//! sets S's disabled anim to 31, as the original does); P needs multiplayer and the flag;
//! K the flag; V one player and the flag. Event 0x57(lifelines left; 4 for two left after
//! a 50:50 on this question). A choice plays 0x12 and ends the menu one tick later. The
//! menu canvas is not cleared between ticks (the button frames pile up on it).
//!
//! Lifeline anims are full-frame transparent overlays (Ll_*.smk):
//! - `intro` 0x404310: over the frozen menu canvas, frames start..end then the hold frame
//!   until the voice queue is empty (Space: event 0x19, StopVoice);
//! - `hold` 0x4075a0: over the live question screen; a single frame is held until an
//!   answer is chosen (answer buttons live), the answer timer ran out, or (`min` ticks
//!   passed and) the voice is idle; a range plays once. Space ends it when `min` != 0.
//!   Every exit stops the voice. No pause check (as in the original).
//!
//! 50:50 0x407280: Ll_5050 1..11 hold 11 (event 0x58); event 0x1b; all slots hidden but
//! the correct one and one wrong: `rand() % 100 + 1 < 76` keeps DB answer 1, else answer
//! `1 + rand() % 3`; hold 13; then 15 (event 0x59) and 15..19. Pass the buck 0x4073a0:
//! Ll_Ptb 1..9 hold 9 (0x5c); the target picker 0x404460 over Ll_Ptb frame 11 (table
//! 0x41ef60; event 0x5d, a choice 0x0e and 0x5f(target), then until the voice ends); the
//! target buzzes in on the next BuzzIn; then 13..17 (0x5e). Magic 8-ball 0x4074a0:
//! Ll_M8b 1..11 hold 11 (0x62); slots restored; hold 13 (0x63) until an answer is asked
//! (no timer runs); 15 (0x65) until the voice ends; 17 + 0x66 if the asked answer is
//! correct, else 19 + 0x67, for at least 90 ticks and the voice (Space skips); then 23
//! (event 100 = 0x64) and 23..27. Chicken 0x407580: Ll_Chick 1..11 hold 11 (0x6a), then
//! 13..17; the question is dropped (GameLoop asks a new one without intro).
//! Each lifeline clears its flag, counts down the lifelines and resets the answer timer.

use render::DrawOp;

use super::infobox::{damage_level, draw_pnbox};
use super::phases::{answer_buttons, answer_timer_tick, draw_answer_buttons, take_answer};
use super::question::{Question, draw_answers, draw_question, draw_timer};
use super::{
    Art, CHICKEN, FIFTY_FIFTY, MAGIC_8_BALL, PASS_THE_BUCK, TIMER_TICKS, draw_scene,
    set_scene_frames,
};
use crate::anim::Anim;
use crate::audio_types::AudioCmd;
use crate::buttons::{ButtonGroup, Mode};
use crate::ctx::{Abort, Ctx};
use crate::input::Vk;
use crate::rng::Lcg;
use crate::screens::pause;
use crate::sound_events::sound_event;
use crate::state::{Game, Sprite, with_sprites};

/// Menu anims (start, end, hold) for S, P, K, V.
pub const MENU_ANIMS: [(i32, i32, i32); 4] = [(1, 7, 9), (11, 17, 19), (21, 27, 29), (33, 39, 41)];
/// Disabled anim given to S when a 50:50 was already used on this question.
pub const FIFTY_USED_FRAME: i32 = 31;
/// Ms_Small positions in the pickers (0x41f3e0).
pub const PICKER_SMALL_POS: [(i32, i32); 4] = [(93, 179), (216, 179), (336, 179), (457, 179)];
/// Ll_Ptb frame behind the target picker.
pub const TARGET_BG_FRAME: i32 = 11;
/// Minimum ticks the 8-ball verdict stays up.
pub const VERDICT_TICKS: i32 = 90;

pub(crate) fn push_ops(ctx: &mut Ctx, ops: &[DrawOp]) {
    ctx.frame.ops.extend_from_slice(ops);
}

/// Drop ops appended after `n` that are already on the canvas below them: redrawing the
/// same frame at the same place over a canvas that keeps its pixels changes nothing.
fn dedup_from(ops: &mut Vec<DrawOp>, n: usize) {
    let mut i = n;
    while i < ops.len() {
        if ops[..n].contains(&ops[i]) {
            ops.remove(i);
        } else {
            i += 1;
        }
    }
}

/// The 50:50 draw of 0x407280; returns the DB answer (1..3) kept with the correct one.
pub fn fifty_fifty_states(rng: &mut Lcg, q: &mut Question) -> usize {
    q.states = [-1; 4];
    q.states[q.inv[0]] = 1;
    let kept = if rng.next() % 100 + 1 < 76 {
        1
    } else {
        1 + rng.next() as usize % 3
    };
    q.states[q.inv[kept]] = 1;
    kept
}

/// Greyed slots back to normal.
fn restore_slots(q: &mut Question) {
    for s in &mut q.states {
        if *s == 0 {
            *s = 1;
        }
    }
}

/// `LifelineMenuAndRun` 0x407070.
pub fn run(ctx: &mut Ctx, game: &mut Game, art: &mut Art) -> Result<(), Abort> {
    ctx.enter_screen("LifelineMenu");
    for s in &mut game.round.question.states {
        if *s == 1 {
            *s = 0;
        }
    }
    sound_event(ctx, 0x04, 0);
    game.round.chosen = -1;
    set_scene_frames(game, 35, 35);
    draw_scene(ctx, game, art);
    let q = game.round.question.clone();
    draw_question(ctx, &q);
    draw_answers(ctx, &q);
    draw_pnbox(ctx, game, art);
    with_sprites(
        game,
        [
            ("Menus/Qs_Ll.smk", true),
            ("Menus/Qs_BbMl.smk", true),
            ("Menus/Qs_BbPl.smk", true),
        ],
        |game, [ll, bbml, bbpl]| {
            let canvas = menu(ctx, game, ll)?;
            match game.round.lifeline {
                FIFTY_FIFTY => with_sprites(game, [("Menus/Ll_5050.smk", true)], |game, [s]| {
                    ctx.enter_screen("Lifeline5050");
                    intro(ctx, game, &canvas, s, 0x58, 1, 11, 11)?;
                    fifty_fifty(ctx, game, art, s)?;
                    hold(ctx, game, art, s, 0x59, 15, 15, true, 0, 0)?;
                    hold(ctx, game, art, s, -1, 15, 19, true, 0, 0)
                }),
                PASS_THE_BUCK => with_sprites(game, [("Menus/Ll_Ptb.smk", true)], |game, [s]| {
                    ctx.enter_screen("LifelinePassBuck");
                    intro(ctx, game, &canvas, s, 0x5c, 1, 9, 9)?;
                    pass_the_buck(ctx, game, art, s, bbml, bbpl)?;
                    hold(ctx, game, art, s, 0x5e, 13, 17, true, 0, 0)
                }),
                MAGIC_8_BALL => with_sprites(game, [("Menus/Ll_M8b.smk", true)], |game, [s]| {
                    ctx.enter_screen("LifelineMagic8Ball");
                    game.round.eight_ball = true;
                    intro(ctx, game, &canvas, s, 0x62, 1, 11, 11)?;
                    restore_slots(&mut game.round.question);
                    magic_8_ball(ctx, game, art, s)?;
                    hold(ctx, game, art, s, 0x64, 23, 23, true, 0, 0)?;
                    hold(ctx, game, art, s, -1, 23, 27, true, 0, 0)?;
                    game.round.eight_ball = false;
                    Ok(())
                }),
                CHICKEN => with_sprites(game, [("Menus/Ll_Chick.smk", true)], |game, [s]| {
                    ctx.enter_screen("LifelineChicken");
                    intro(ctx, game, &canvas, s, 0x6a, 1, 11, 11)?;
                    game.round.answer_timer = TIMER_TICKS;
                    if let Some(a) = game.round.answerer {
                        let p = &mut game.players[a];
                        p.chicken = false;
                        p.lifelines -= 1;
                    }
                    hold(ctx, game, art, s, -1, 13, 17, true, 0, 0)
                }),
                _ => Ok(()),
            }
        },
    )
}

/// `LifelineMenu` 0x403f90; returns the last presented menu canvas and sets
/// `game.round.lifeline`.
fn menu(ctx: &mut Ctx, game: &mut Game, ll: Sprite) -> Result<Vec<DrawOp>, Abort> {
    let mut defs = game.session.lifeline_table.clone();
    for (d, &(s, e, _)) in defs.iter_mut().zip(&MENU_ANIMS) {
        d.anims[0] = [s, e];
    }
    let mut g = ButtonGroup::new(&defs, |_| ll);
    g.mouse_disabled = true;
    let Some(a) = game.round.answerer else {
        return Ok(Vec::new());
    };
    let p = game.players[a];
    if !p.fifty_fifty {
        g.set_disabled(0, true);
    } else if game.round.fifty_used {
        g.set_anim(0, 3, FIFTY_USED_FRAME, FIFTY_USED_FRAME);
    }
    if game.player_count == 1 || !p.pass_the_buck {
        g.set_disabled(1, true);
    }
    if !p.magic_8_ball {
        g.set_disabled(2, true);
    }
    if game.player_count != 1 {
        g.set_disabled(3, true);
    } else {
        g.set_disabled(1, true);
        if !p.chicken {
            g.set_disabled(3, true);
        }
    }
    match p.lifelines {
        1 => sound_event(ctx, 0x57, 1),
        2 => sound_event(ctx, 0x57, if game.round.fifty_used { 4 } else { 2 }),
        3 => sound_event(ctx, 0x57, 3),
        _ => 0,
    };
    g.update(ctx, Mode::Pump, true);
    let r = (|| loop {
        if g.result.is_some() {
            sound_event(ctx, 0x12, 0);
        }
        let canvas = ctx.frame.ops.clone();
        ctx.present()?;
        pause::on_esc(ctx, game)?;
        for (i, &(_, _, h)) in MENU_ANIMS.iter().enumerate() {
            let an = g.buttons[i].anims[0];
            if an.cur == an.end {
                g.set_anim(i, 0, h, h);
            }
        }
        if g.result.is_some() {
            return Ok(canvas);
        }
        let n = canvas.len();
        ctx.frame.ops = canvas;
        g.update(ctx, Mode::Pump, true);
        dedup_from(&mut ctx.frame.ops, n);
    })();
    game.session.lifeline_table = g.defs();
    game.round.lifeline = g.result.unwrap_or(-1);
    r
}

/// 0x404310: the lifeline anim over the frozen menu canvas.
#[allow(clippy::too_many_arguments)]
fn intro(
    ctx: &mut Ctx,
    game: &mut Game,
    canvas: &[DrawOp],
    s: Sprite,
    ev: u8,
    start: i32,
    end: i32,
    hold_frame: i32,
) -> Result<(), Abort> {
    let mut a = Anim::new(start, end);
    sound_event(ctx, ev, 0);
    loop {
        push_ops(ctx, canvas);
        s.draw(ctx, a.cur, 0, 0);
        a.advance();
        if a.at_start() {
            a = Anim::new(hold_frame, hold_frame);
        }
        ctx.pump();
        ctx.present()?;
        pause::on_esc(ctx, game)?;
        ctx.pump_voice();
        if ctx.last_key() == Some(Vk::SPACE) {
            sound_event(ctx, 0x19, 0);
            ctx.audio(AudioCmd::StopVoice);
            return Ok(());
        }
        if !ctx.voice_busy() {
            return Ok(());
        }
    }
}

/// 0x4075a0: lifeline frames `start..end` over the live question screen. `buttons`: the
/// answer buttons take keys (param 4); `timer` == 1: the answer timer runs (param 5);
/// `min`: ticks before the voice may end a single-frame hold, and Space skips (param 6).
#[allow(clippy::too_many_arguments)]
pub(crate) fn hold(
    ctx: &mut Ctx,
    game: &mut Game,
    art: &mut Art,
    s: Sprite,
    ev: i32,
    start: i32,
    end: i32,
    buttons: bool,
    timer: i32,
    min: i32,
) -> Result<(), Abort> {
    set_scene_frames(game, 35, 35);
    let mut a = Anim::new(start, end);
    let mut n = 0;
    if ev != -1 {
        sound_event(ctx, ev as u8, 0);
    }
    // Whatever was composed before is covered by the opaque scene drawn first.
    ctx.frame.ops.clear();
    loop {
        draw_scene(ctx, game, art);
        n += 1;
        if buttons {
            answer_buttons(ctx, game, art);
            take_answer(ctx, game, art);
            draw_answer_buttons(ctx, game, art);
        } else {
            ctx.pump();
            art.buttons.draw(ctx);
        }
        if timer == 1 && game.round.answerer.is_some() && game.round.chosen == -1 {
            draw_timer(ctx, game.round.answer_timer);
        }
        let q = game.round.question.clone();
        draw_question(ctx, &q);
        draw_answers(ctx, &q);
        draw_pnbox(ctx, game, art);
        s.draw(ctx, a.cur, 0, 0);
        a.advance();
        ctx.present()?;
        ctx.pump_voice();
        if min != 0 && ctx.last_key() == Some(Vk::SPACE) {
            break;
        }
        if start != end {
            if a.loops == 0 {
                continue;
            }
            break;
        }
        if (buttons && game.round.chosen != -1)
            || (timer == 1 && !answer_timer_tick(ctx, game)?)
            || (min <= n && timer == 0 && !ctx.voice_busy())
        {
            break;
        }
    }
    ctx.audio(AudioCmd::StopVoice);
    Ok(())
}

/// `Lifeline5050` 0x407280.
fn fifty_fifty(ctx: &mut Ctx, game: &mut Game, art: &mut Art, s: Sprite) -> Result<(), Abort> {
    sound_event(ctx, 0x1b, 0);
    set_scene_frames(game, 35, 35);
    draw_scene(ctx, game, art);
    fifty_fifty_states(&mut ctx.rng, &mut game.round.question);
    let q = game.round.question.clone();
    draw_question(ctx, &q);
    draw_answers(ctx, &q);
    draw_pnbox(ctx, game, art);
    hold(ctx, game, art, s, -1, 13, 13, true, 0, 0)?;
    game.round.answer_timer = TIMER_TICKS;
    if let Some(a) = game.round.answerer {
        let p = &mut game.players[a];
        p.fifty_fifty = false;
        p.lifelines -= 1;
    }
    game.round.fifty_used = true;
    Ok(())
}

/// `LifelinePassBuck` 0x4073a0.
fn pass_the_buck(
    ctx: &mut Ctx,
    game: &mut Game,
    art: &mut Art,
    s: Sprite,
    bbml: Sprite,
    bbpl: Sprite,
) -> Result<(), Abort> {
    set_scene_frames(game, 35, 35);
    draw_scene(ctx, game, art);
    let q = game.round.question.clone();
    draw_question(ctx, &q);
    draw_answers(ctx, &q);
    let base = std::mem::take(&mut ctx.frame.ops);
    let target = target_picker(ctx, game, &base, s, bbml, bbpl)?;
    game.round.answer_timer = TIMER_TICKS;
    if let Some(a) = game.round.answerer {
        let p = &mut game.players[a];
        p.pass_the_buck = false;
        p.lifelines -= 1;
    }
    game.round.pending = target;
    game.round.answerer = None;
    restore_slots(&mut game.round.question);
    Ok(())
}

/// 0x4046f0: a picker button for player `p` is disabled unless the player has a
/// millionaire, money left and is not the answerer.
pub(crate) fn disable_unavailable(g: &mut ButtonGroup, button: usize, game: &Game, p: usize) {
    let pl = &game.players[p];
    let ok = pl.millionaire != -1 && pl.money >= 1 && game.round.answerer != Some(p);
    if !ok {
        g.set_disabled(button, true);
    }
}

/// 0x404680: player p's portrait (static button p shows millionaire + 1) and Ms_Small
/// at the damage level.
pub(crate) fn draw_portrait(
    ctx: &mut Ctx,
    g: &mut ButtonGroup,
    game: &Game,
    p: usize,
    small: Sprite,
) {
    let m = game.players[p].millionaire;
    if m == -1 {
        return;
    }
    g.set_all_anims(p, -1, -1);
    g.set_all_anims(p, m + 1, m + 1);
    let (x, y) = PICKER_SMALL_POS[p];
    small.draw(ctx, damage_level(game.players[p].money) + 1, x, y);
}

/// `PickPassTarget` 0x404460; returns the target player.
fn target_picker(
    ctx: &mut Ctx,
    game: &mut Game,
    base: &[DrawOp],
    s: Sprite,
    bbml: Sprite,
    bbpl: Sprite,
) -> Result<Option<usize>, Abort> {
    ctx.enter_screen("PickPassTarget");
    with_sprites(game, [("Menus/Ms_Small.smk", true)], |game, [small]| {
        let mut g = ButtonGroup::new(&game.session.target_table, |smk| {
            if smk == 0x424740 { bbml } else { bbpl }
        });
        for p in 0..4 {
            disable_unavailable(&mut g, 4 + p, game, p);
        }
        g.mouse_disabled = true;
        sound_event(ctx, 0x5d, game.round.answerer.map_or(0, |a| a as i32));
        let r = (|| {
            push_ops(ctx, base);
            s.draw(ctx, TARGET_BG_FRAME, 0, 0);
            g.update(ctx, Mode::Pump, true);
            let mut chosen = false;
            let mut mode = Mode::Pump;
            loop {
                if !chosen && g.result.is_some() {
                    sound_event(ctx, 0x0e, 0);
                }
                for p in 0..4 {
                    draw_portrait(ctx, &mut g, game, p, small);
                }
                ctx.present()?;
                pause::on_esc(ctx, game)?;
                ctx.pump_voice();
                if chosen {
                    if !ctx.voice_busy() {
                        break;
                    }
                } else if let Some(id) = g.result {
                    mode = Mode::Keep;
                    chosen = true;
                    sound_event(ctx, 0x5f, id - 700);
                    if !ctx.voice_busy() {
                        break;
                    }
                }
                push_ops(ctx, base);
                s.draw(ctx, TARGET_BG_FRAME, 0, 0);
                g.update(ctx, mode, true);
            }
            Ok(g.result
                .and_then(|id| usize::try_from(id - 700).ok())
                .filter(|&t| t < 4))
        })();
        game.session.target_table = g.defs();
        r
    })
}

/// `LifelineMagic8Ball` 0x4074a0.
fn magic_8_ball(ctx: &mut Ctx, game: &mut Game, art: &mut Art, s: Sprite) -> Result<(), Abort> {
    game.round.answer_timer = TIMER_TICKS;
    hold(ctx, game, art, s, 0x63, 13, 13, true, -1, 0)?;
    if game.round.chosen != -1 {
        game.round.asked = game.round.chosen;
        hold(ctx, game, art, s, 0x65, 15, 15, false, 0, 0)?;
        let (ev, f) = if game.round.chosen == game.round.question.correct_slot as i32 {
            (0x66, 17)
        } else {
            (0x67, 19)
        };
        hold(ctx, game, art, s, ev, f, f, false, 0, VERDICT_TICKS)?;
    }
    game.round.answer_timer = TIMER_TICKS;
    game.round.chosen = -1;
    if let Some(a) = game.round.answerer {
        let p = &mut game.players[a];
        p.magic_8_ball = false;
        p.lifelines -= 1;
    }
    for b in 0..4 {
        art.buttons.set_all_anims(b, -1, -1);
    }
    Ok(())
}
