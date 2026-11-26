//! Opponent, round resolution and the end screens (F4.4, F1.4; E3 0x406bb0, 0x403960,
//! 0x406c20, 0x406360, 0x4060f0).
//!
//! - `pick_opponent` 0x406bb0, by opponents remaining (0x41fca4): none -> player 0 (the
//!   one player beats their own millionaire); one -> the last other player with money;
//!   more -> `PickVictimScreen` 0x403960 if an answer was chosen.
//! - `PickVictimScreen` over the frozen question screen: event 0x48, Qs_Bbox 1..2; then
//!   frame 4 with table 0x41e8a0 (portraits Qs_BbMl, buttons '1'..'4' Qs_BbPl, players
//!   without money or the answerer disabled), Ms_Small per player; a choice plays 0x0f
//!   and ends the menu; then Qs_Bbox 6..8.
//! - `resolve` 0x406c20: with a chosen answer, event 0x35(slot) if the answerer is at
//!   damage level 9 else 0x34(slot); when the voice ends the verdict: the correct slot's
//!   button anim 3..4 (and a wrong pick's 1..1), events 0x14 / 0x15 and the reaction
//!   (after 50:50 0x5a / 0x5b, pass the buck 0x60 / 0x61, 8-ball 0x68 / 0x69, otherwise
//!   0x36 / 0x37, or 0x38 for the first wrong answer of a multiplayer game); then until
//!   the voice ends. Correct: beater answerer, beaten opponent, result 1. Wrong: beater
//!   opponent, beaten answerer, result 0. A special round follows if q equals schedule
//!   slot k < players. No answer: result 2 (after a timeout the drawn beater beats the
//!   answerer).
//! - `single_player_loss` 0x406360 (1 player, result != 1): Mm_Bg with the host; events
//!   8 and 0x28; when 0x28 ends 0x40(q); when the voice ends 0x30; then until it ends.
//! - `win_screen` 0x4060f0: Mm_Bg with the host, Win_Mill.smk at (392,56) frame
//!   millionaire*2 + 1 + (money < 100000) in multiplayer only (Win_Conf.smk is loaded and
//!   its anim 1..21 stepped but never drawn, F11.10 KEEP); events 8, 0x28, 0x2b; then
//!   0x3f(champion, money < 100000), 0x30; event 0x2b(1) at the end. No pause check on
//!   either end screen.

use render::DrawOp;

use super::infobox::{damage_level, draw_pnbox};
use super::lifelines::{disable_unavailable, draw_portrait, push_ops};
use super::phases::{answer_buttons, draw_answer_buttons};
use super::question::{draw_answers, draw_question};
use super::{
    Art, Bg, FIFTY_FIFTY, MAGIC_8_BALL, PASS_THE_BUCK, draw_scene, host_anim, set_scene,
    set_scene_frames,
};
use crate::anim::Anim;
use crate::buttons::{ButtonGroup, Mode};
use crate::ctx::{Abort, Ctx};
use crate::screens::pause;
use crate::sound_events::{sound_event, sound_event3};
use crate::state::{Game, Sprite, with_sprites};

/// Qs_Bbox frames: intro, idle, exit.
pub const BBOX_INTRO: (i32, i32) = (1, 2);
pub const BBOX_IDLE: i32 = 4;
pub const BBOX_EXIT: (i32, i32) = (6, 8);
pub const WIN_MILL_POS: (i32, i32) = (392, 56);

/// `PickOpponent` 0x406bb0.
pub fn pick_opponent(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    let (answerer, chosen) = (game.round.answerer, game.round.chosen);
    match game.remaining_opponents {
        0 => game.round.opponent = Some(0),
        1 => {
            for (i, p) in game.players.iter().enumerate() {
                if p.money > 0 && answerer != Some(i) {
                    game.round.opponent = Some(i);
                }
            }
        }
        _ => {
            if answerer.is_some() && chosen != -1 {
                game.round.opponent = pick_victim(ctx, game)?;
            }
        }
    }
    Ok(())
}

/// `PickVictimScreen` 0x403960.
fn pick_victim(ctx: &mut Ctx, game: &mut Game) -> Result<Option<usize>, Abort> {
    ctx.enter_screen("PickVictim");
    let frozen = ctx.last_presented().ops.clone();
    with_sprites(
        game,
        [
            ("Menus/Qs_Bbox.smk", true),
            ("Menus/Qs_BbMl.smk", true),
            ("Menus/Qs_BbPl.smk", true),
            ("Menus/Ms_Small.smk", true),
        ],
        |game, [bbox, bbml, bbpl, small]| {
            sound_event(ctx, 0x48, 0);
            bbox_anim(ctx, &frozen, bbox, BBOX_INTRO, false)?;
            let pick = victim_menu(ctx, game, &frozen, bbox, bbml, bbpl, small)?;
            bbox_anim(ctx, &frozen, bbox, BBOX_EXIT, true)?;
            Ok(pick)
        },
    )
}

/// 0x403a50 (`until_loop` false: ends when the anim is back at its start) and 0x403d10
/// (true: ends once it has looped): Qs_Bbox frames over the frozen canvas, no pause.
fn bbox_anim(
    ctx: &mut Ctx,
    frozen: &[DrawOp],
    bbox: Sprite,
    (start, end): (i32, i32),
    until_loop: bool,
) -> Result<(), Abort> {
    let mut a = Anim::new(start, end);
    push_ops(ctx, frozen);
    bbox.draw(ctx, a.cur, 0, 0);
    a.advance();
    ctx.pump();
    loop {
        ctx.present()?;
        ctx.pump_voice();
        let done = if until_loop {
            a.loops != 0
        } else {
            a.at_start()
        };
        if done {
            return Ok(());
        }
        push_ops(ctx, frozen);
        bbox.draw(ctx, a.cur, 0, 0);
        a.advance();
        ctx.pump();
    }
}

/// 0x403b60.
fn victim_menu(
    ctx: &mut Ctx,
    game: &mut Game,
    frozen: &[DrawOp],
    bbox: Sprite,
    bbml: Sprite,
    bbpl: Sprite,
    small: Sprite,
) -> Result<Option<usize>, Abort> {
    let mut g = ButtonGroup::new(&game.session.victim_table, |smk| {
        if smk == 0x424708 { bbml } else { bbpl }
    });
    for p in 0..4 {
        disable_unavailable(&mut g, 4 + p, game, p);
    }
    g.mouse_disabled = true;
    let r = (|| -> Result<Option<usize>, Abort> {
        push_ops(ctx, frozen);
        bbox.draw(ctx, BBOX_IDLE, 0, 0);
        g.update(ctx, Mode::Pump, true);
        loop {
            if g.result.is_some() {
                sound_event(ctx, 0x0f, 0);
            }
            for p in 0..4 {
                draw_portrait(ctx, &mut g, game, p, small);
            }
            ctx.present()?;
            pause::on_esc(ctx, game)?;
            ctx.pump_voice();
            if let Some(id) = g.result {
                return Ok(usize::try_from(id - 400).ok().filter(|&p| p < 4));
            }
            push_ops(ctx, frozen);
            bbox.draw(ctx, BBOX_IDLE, 0, 0);
            g.update(ctx, Mode::Pump, true);
        }
    })();
    game.session.victim_table = g.defs();
    r
}

/// `ResolveAnswer` 0x406c20.
pub fn resolve(ctx: &mut Ctx, game: &mut Game, art: &mut Art) -> Result<(), Abort> {
    ctx.enter_screen("Resolve");
    super::sync_debug(ctx, game);
    let chosen = game.round.chosen;
    if let Some(a) = game.round.answerer
        && chosen != -1
    {
        set_scene_frames(game, 35, 35);
        let ev = if damage_level(game.players[a].money) == 9 {
            0x35
        } else {
            0x34
        };
        sound_event(ctx, ev, chosen);
        draw_scene(ctx, game, art);
        answer_buttons(ctx, game, art);
        draw_answer_buttons(ctx, game, art);
        let mut shown = false;
        loop {
            let q = game.round.question.clone();
            draw_question(ctx, &q);
            draw_answers(ctx, &q);
            draw_pnbox(ctx, game, art);
            ctx.present()?;
            pause::on_esc(ctx, game)?;
            ctx.pump_voice();
            if shown {
                if !ctx.voice_busy() {
                    break;
                }
            } else if !ctx.voice_busy() {
                shown = true;
                verdict(ctx, game, art);
            }
            draw_scene(ctx, game, art);
            answer_buttons(ctx, game, art);
            draw_answer_buttons(ctx, game, art);
        }
    }
    let r = &mut game.round;
    if r.chosen != -1 {
        if r.chosen == r.question.correct_slot as i32 {
            r.beater = r.answerer;
            r.beaten = r.opponent;
            r.result = 1;
        } else {
            r.beater = r.opponent;
            r.beaten = r.answerer;
            r.result = 0;
        }
        r.check_special(game.player_count);
    } else {
        if let Some(b) = r.random_beater {
            r.beater = Some(b);
            r.beaten = r.answerer;
            r.opponent = Some(b);
        }
        r.result = 2;
    }
    Ok(())
}

/// The verdict events of 0x406c20 once the "final answer" line has ended.
fn verdict(ctx: &mut Ctx, game: &mut Game, art: &mut Art) {
    let r = &mut game.round;
    let chosen = r.chosen;
    let correct = r.question.correct_slot;
    art.buttons.set_all_anims(correct, 3, 4);
    let opponent = r.opponent.map_or(0, |o| o as i32);
    if chosen == correct as i32 {
        sound_event(ctx, 0x14, 0);
        match r.lifeline {
            FIFTY_FIFTY => sound_event(ctx, 0x5a, chosen),
            PASS_THE_BUCK => sound_event(ctx, 0x60, chosen),
            MAGIC_8_BALL => sound_event3(ctx, 0x68, chosen, r.asked, 0),
            _ => sound_event(ctx, 0x36, chosen),
        };
    } else {
        if let Ok(slot) = usize::try_from(chosen) {
            art.buttons.set_all_anims(slot, 1, 1);
        }
        sound_event(ctx, 0x15, 0);
        let r = &mut game.round;
        r.wrong_answers += 1;
        let c = correct as i32;
        match r.lifeline {
            FIFTY_FIFTY => sound_event3(ctx, 0x5b, chosen, c, 0),
            PASS_THE_BUCK => sound_event3(ctx, 0x61, chosen, c, 0),
            MAGIC_8_BALL => sound_event3(ctx, 0x69, chosen, c, r.asked),
            _ if game.player_count == 1 || r.wrong_answers != 1 => {
                sound_event3(ctx, 0x37, chosen, c, opponent)
            }
            _ => sound_event3(ctx, 0x38, chosen, c, opponent),
        };
    }
}

fn host(ctx: &mut Ctx, game: &mut Game, art: &Art) {
    let loud = host_anim::loud(ctx, game.lip_sync);
    game.session.host.tick(ctx, art.regis, loud);
}

/// `SinglePlayerLoss` 0x406360.
pub fn single_player_loss(ctx: &mut Ctx, game: &mut Game, art: &Art) -> Result<(), Abort> {
    if game.player_count != 1 || game.round.result == 1 {
        return Ok(());
    }
    ctx.enter_screen("SinglePlayerLoss");
    set_scene(game, Bg::Main, 1, 1);
    sound_event(ctx, 0x08, 0);
    sound_event(ctx, 0x28, 0);
    game.lip_sync = true;
    let r = end_talk(ctx, game, art, None, |ctx, game| {
        sound_event(ctx, 0x40, game.round.q);
    });
    game.lip_sync = false;
    r
}

/// The talk sequence both end screens share: wait for sfx 0x28, then `first`, then
/// event 0x30, then the voice.
fn end_talk(
    ctx: &mut Ctx,
    game: &mut Game,
    art: &Art,
    mut win: Option<(Sprite, i32, &mut Anim)>,
    first: impl Fn(&mut Ctx, &mut Game),
) -> Result<(), Abort> {
    let mut phase = 0;
    draw_scene(ctx, game, art);
    ctx.pump();
    loop {
        host(ctx, game, art);
        if let Some((mill, frame, conf)) = win.as_mut() {
            if game.player_count > 1 {
                mill.draw(ctx, *frame, WIN_MILL_POS.0, WIN_MILL_POS.1);
            }
            if phase < 2 || conf.cur != conf.start {
                conf.advance();
            }
        }
        ctx.present()?;
        ctx.pump_voice();
        match phase {
            0 => {
                if sound_event(ctx, 0x28, 1) == 0 {
                    first(ctx, game);
                    phase = 1;
                }
            }
            1 => {
                if !ctx.voice_busy() {
                    sound_event(ctx, 0x30, 0);
                    phase = 2;
                }
            }
            _ => {
                if !ctx.voice_busy() {
                    return Ok(());
                }
            }
        }
        draw_scene(ctx, game, art);
        ctx.pump();
    }
}

/// `WinScreen` 0x4060f0.
pub fn win_screen(ctx: &mut Ctx, game: &mut Game, art: &Art) -> Result<(), Abort> {
    ctx.enter_screen("Win");
    let champ = game.round.champion.unwrap_or(0);
    let (m, money) = (game.players[champ].millionaire, game.players[champ].money);
    let poor = i32::from(money < 100_000);
    set_scene(game, Bg::Main, 1, 1);
    with_sprites(
        game,
        [
            ("QuesAnsw/Win_Mill.Smk", true),
            ("QuesAnsw/Win_Conf.Smk", true),
        ],
        |game, [mill, _conf]| {
            let frame = poor + 1 + m * 2;
            let mut conf = Anim::new(1, 21);
            sound_event(ctx, 0x08, 0);
            sound_event(ctx, 0x28, 0);
            sound_event(ctx, 0x2b, 0);
            game.lip_sync = true;
            let r = end_talk(ctx, game, art, Some((mill, frame, &mut conf)), |ctx, _| {
                sound_event3(ctx, 0x3f, champ as i32, poor, 0);
            });
            game.lip_sync = false;
            r?;
            sound_event(ctx, 0x2b, 1);
            Ok(())
        },
    )
}
