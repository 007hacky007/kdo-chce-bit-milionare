//! The per-question screens on Qs_Bg (F1.4, F2.5, F2.6, F6.2).
//!
//! Every loop has the original's shape: the scene background is drawn and messages are
//! pumped at the end of an iteration, the overlays at the top of the next one, then the
//! present, `PauseOnEsc`, `VoiceQueuePump` and the exit test.
//!
//! - `qs_bg_intro` 0x4064a0: frames 1..8, then hold 8 until the voice queue is empty.
//! - `question_intro` 0x4065f0: event 5, frames 10..21.
//! - `question_read` 0x4066c0: hold 21, event 0x31 (CHAR{id}q), until the voice ends;
//!   buzzing is polled, the question text shown, Qs_PnBox drawn.
//! - `answer_panel` 0x406790: frames 22..35, buzzing polled.
//! - `reveal` 0x406860 (F2.6): per slot, unless an answer was chosen: frame 35, the four
//!   answer buttons' anims cleared; if nobody has buzzed or in 1-player games event 0x1a
//!   and 0x33(slot); then until the voice queue is empty, an answer is chosen or (in
//!   multiplayer) someone has buzzed. If an answer was already chosen the voice stops and
//!   the slot lasts one tick.
//! - `answer_phase` 0x406a20: buzz timer while nobody has buzzed, answer timer after;
//!   'N' opens the lifeline menu; a chosen answer plays event 0x3b and waits for it.
//!
//! Answer group 0x424580 (table 0x41e5d0, keys A-D and N, mouse ignored: +0x24 bit 0 is
//! set as in every menu): `answer_buttons` 0x4036d0 pumps (or keeps, once an answer is
//! chosen), `take_answer` 0x407a60 turns the pressed key into the chosen slot (only a
//! slot in state 1) and `draw_answer_buttons` 0x4038b0 draws. The 'N' button is enabled
//! while someone has buzzed, no answer is chosen, the answerer has lifelines left and the
//! 8-ball is not running.

use super::question::{draw_answer, draw_answers, draw_question, draw_timer};
use super::{
    Art, Bg, ID_LIFELINE_BUTTON, TIMER_TICKS, TIMER_TICKS_INTERRUPT, draw_scene, lifelines,
    set_scene, set_scene_frames, wait_voice_frozen,
};
use crate::audio_types::{AudioCmd, Prefix, SoundId};
use crate::buttons::{DISABLED, Mode, NORMAL};
use crate::ctx::{Abort, Ctx};
use crate::input::Vk;
use crate::screens::pause;
use crate::sound_events::{sound_event, sound_event3};
use crate::state::Game;

use super::infobox::draw_pnbox;

/// Buzz keys by player count (0x41fc60; -1 = none). The 1-player row is never read.
pub const BUZZ_KEYS: [[i32; 4]; 4] = [
    [81, -1, -1, -1],
    [81, 80, -1, -1],
    [81, 86, 80, -1],
    [81, 86, 77, 80],
];
const LIFELINE_BUTTON: usize = 4;
/// Answer anims of a chosen slot / the correct slot after the resolution.
pub const CHOSEN_ANIM: (i32, i32) = (1, 1);

/// The present, pause and voice-pump tail every loop shares.
fn tick(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    ctx.present()?;
    pause::on_esc(ctx, game)?;
    ctx.pump_voice();
    Ok(())
}

fn scene_and_pump(ctx: &mut Ctx, game: &mut Game, art: &Art) {
    draw_scene(ctx, game, art);
    ctx.pump();
}

fn scene_done(game: &Game) -> bool {
    let a = &game.round.scene.anim;
    a.cur == a.start
}

/// 0x4064a0: Qs_Bg 1..8, hold 8 while the voice talks.
pub fn qs_bg_intro(ctx: &mut Ctx, game: &mut Game, art: &Art) -> Result<(), Abort> {
    ctx.enter_screen("QsBgIntro");
    set_scene(game, Bg::Question, 1, 8);
    scene_and_pump(ctx, game, art);
    loop {
        tick(ctx, game)?;
        if scene_done(game) {
            set_scene_frames(game, 8, 8);
            if !ctx.voice_busy() {
                return Ok(());
            }
        }
        scene_and_pump(ctx, game, art);
    }
}

/// 0x4065f0: event 5, frames 10..21.
pub fn question_intro(ctx: &mut Ctx, game: &mut Game, art: &Art) -> Result<(), Abort> {
    ctx.enter_screen("QuestionIntro");
    set_scene_frames(game, 10, 21);
    sound_event(ctx, 0x05, 0);
    scene_and_pump(ctx, game, art);
    loop {
        tick(ctx, game)?;
        if scene_done(game) {
            return Ok(());
        }
        scene_and_pump(ctx, game, art);
    }
}

/// 0x4066c0: the question voice; ends when the voice queue is empty.
pub fn question_read(ctx: &mut Ctx, game: &mut Game, art: &Art) -> Result<(), Abort> {
    ctx.enter_screen("QuestionRead");
    set_scene_frames(game, 21, 21);
    sound_event(ctx, 0x31, game.round.question.voice_id);
    scene_and_pump(ctx, game, art);
    loop {
        buzz_in(ctx, game);
        let q = game.round.question.clone();
        draw_question(ctx, &q);
        draw_pnbox(ctx, game, art);
        tick(ctx, game)?;
        if !ctx.voice_busy() {
            return Ok(());
        }
        scene_and_pump(ctx, game, art);
    }
}

/// 0x406790: the answer panel, frames 22..35.
pub fn answer_panel(ctx: &mut Ctx, game: &mut Game, art: &Art) -> Result<(), Abort> {
    ctx.enter_screen("AnswerPanel");
    set_scene_frames(game, 22, 35);
    scene_and_pump(ctx, game, art);
    loop {
        buzz_in(ctx, game);
        let q = game.round.question.clone();
        draw_question(ctx, &q);
        draw_pnbox(ctx, game, art);
        tick(ctx, game)?;
        if scene_done(game) {
            return Ok(());
        }
        scene_and_pump(ctx, game, art);
    }
}

/// `RevealAnswer` 0x406860 for display slot `slot`.
pub fn reveal(ctx: &mut Ctx, game: &mut Game, art: &mut Art, slot: usize) -> Result<(), Abort> {
    ctx.enter_screen("Reveal");
    if game.round.chosen == -1 {
        set_scene_frames(game, 35, 35);
        for b in 0..4 {
            art.buttons.set_all_anims(b, -1, -1);
        }
        if game.round.answerer.is_none() || game.player_count == 1 {
            sound_event(ctx, 0x1a, 0);
            let q = &game.round.question;
            let (voice, orig) = (q.voice_id, q.perm[slot] as i32);
            sound_event3(ctx, 0x33, slot as i32, voice, orig);
        }
    } else {
        ctx.audio(AudioCmd::StopVoice);
    }
    draw_scene(ctx, game, art);
    answer_buttons(ctx, game, art);
    buzz_in(ctx, game);
    loop {
        take_answer(ctx, game, art);
        draw_answer_buttons(ctx, game, art);
        let q = game.round.question.clone();
        draw_question(ctx, &q);
        for s in 0..=slot {
            draw_answer(ctx, &q, s);
        }
        draw_pnbox(ctx, game, art);
        tick(ctx, game)?;
        if !ctx.voice_busy()
            || game.round.chosen != -1
            || (game.player_count > 1 && game.round.answerer.is_some())
        {
            return Ok(());
        }
        draw_scene(ctx, game, art);
        answer_buttons(ctx, game, art);
        buzz_in(ctx, game);
    }
}

/// `AnswerPhase` 0x406a20.
pub fn answer_phase(ctx: &mut Ctx, game: &mut Game, art: &mut Art) -> Result<(), Abort> {
    ctx.enter_screen("AnswerPhase");
    set_scene_frames(game, 35, 35);
    draw_scene(ctx, game, art);
    answer_buttons(ctx, game, art);
    buzz_in(ctx, game);
    take_answer(ctx, game, art);
    loop {
        draw_answer_buttons(ctx, game, art);
        let r = &game.round;
        if r.chosen == -1 {
            match r.answerer {
                None => draw_timer(ctx, r.buzz_timer),
                Some(_) => draw_timer(ctx, r.answer_timer),
            }
        }
        let q = game.round.question.clone();
        draw_question(ctx, &q);
        draw_answers(ctx, &q);
        draw_pnbox(ctx, game, art);
        tick(ctx, game)?;
        if art.buttons.result == Some(ID_LIFELINE_BUTTON) {
            lifelines::run(ctx, game, art)?;
            ctx.enter_screen("AnswerPhase");
            if game.round.lifeline == super::CHICKEN {
                return Ok(());
            }
        } else {
            if !buzz_timer_tick(ctx, game) {
                return Ok(());
            }
            if game.round.answerer.is_some() {
                if !answer_timer_tick(ctx, game)? {
                    return Ok(());
                }
                if game.round.chosen != -1 {
                    sound_event(ctx, 0x3b, 0);
                    return wait_voice_frozen(ctx);
                }
            }
        }
        draw_scene(ctx, game, art);
        answer_buttons(ctx, game, art);
        buzz_in(ctx, game);
        take_answer(ctx, game, art);
    }
}

/// `BuzzIn` 0x407910 (F2.5): reads the pumped `g_lastKey`.
pub fn buzz_in(ctx: &mut Ctx, game: &mut Game) {
    let r = &mut game.round;
    if r.answerer.is_some() || r.chosen != -1 {
        return;
    }
    if game.player_count <= 1 {
        r.answerer = Some(0);
        r.answer_timer = TIMER_TICKS;
        return;
    }
    if let Some(p) = r.pending.take() {
        r.answerer = Some(p);
    } else {
        let key = ctx.last_key().map_or(0, |Vk(k)| i32::from(k));
        let row = BUZZ_KEYS[game.player_count.min(4) - 1];
        for (i, &k) in row.iter().enumerate() {
            if key == k {
                r.answerer = Some(i);
            }
        }
        let Some(a) = r.answerer else { return };
        let p = &game.players[a];
        if p.millionaire == -1 || p.money < 1 {
            r.answerer = None;
            return;
        }
    }
    let Some(a) = r.answerer else { return };
    let voice = SoundId::with_suffix(Prefix::Char, r.question.voice_id as u16, 'q');
    let ev = if ctx.sound_playing(voice) {
        r.answer_timer = TIMER_TICKS_INTERRUPT;
        0x3d
    } else {
        r.answer_timer = TIMER_TICKS;
        0x3e
    };
    sound_event(ctx, ev, a as i32);
    sound_event(ctx, 0x13, 0);
    game.round.timeouts = 0;
}

/// Event 0x1c when `t / 15` changes on the way to `t - 1`.
fn countdown(ctx: &mut Ctx, t: i32) {
    let next = (t - 1) / 15;
    if t / 15 != next {
        sound_event(ctx, 0x1c, next);
    }
}

/// `BuzzTimerTick` 0x407770: false when the buzz time ran out (event 0x16).
pub fn buzz_timer_tick(ctx: &mut Ctx, game: &mut Game) -> bool {
    let r = &mut game.round;
    if r.answerer.is_some() {
        return true;
    }
    countdown(ctx, r.buzz_timer);
    let r = &mut game.round;
    r.buzz_timer -= 1;
    if r.buzz_timer > 0 {
        return true;
    }
    sound_event(ctx, 0x16, 0);
    let r = &mut game.round;
    r.timeouts = r.timeouts.max(0) + 1;
    false
}

/// `AnswerTimerTick` 0x407800: false when the answer time ran out. In multiplayer a
/// random beater is drawn: `rand() % players`, re-rolled while it is the answerer or
/// (F11.3 FIX) a player without a millionaire or money; events 0x16 and 0x3a, then the
/// frozen wait for the voice.
pub fn answer_timer_tick(ctx: &mut Ctx, game: &mut Game) -> Result<bool, Abort> {
    if game.round.chosen != -1 {
        return Ok(true);
    }
    countdown(ctx, game.round.answer_timer);
    game.round.answer_timer -= 1;
    if game.round.answer_timer > 0 {
        return Ok(true);
    }
    let n = game.player_count.max(1);
    let a = game.round.answerer;
    if n > 1
        && game.players[..n]
            .iter()
            .enumerate()
            .any(|(i, p)| Some(i) != a && p.millionaire != -1 && p.money >= 1)
    {
        loop {
            let b = ctx.rng.next() as usize % n;
            let p = &game.players[b];
            if Some(b) != a && p.millionaire != -1 && p.money >= 1 {
                game.round.random_beater = Some(b);
                break;
            }
        }
    }
    sound_event(ctx, 0x16, 0);
    let beater = game.round.random_beater.map_or(-1, |b| b as i32);
    sound_event3(ctx, 0x3a, a.map_or(0, |a| a as i32), n as i32 - 1, beater);
    wait_voice_frozen(ctx)?;
    Ok(false)
}

/// The 'N' button's state, shared by 0x4036d0 and 0x4038b0.
fn lifeline_button(game: &Game, art: &mut Art) {
    let r = &game.round;
    let enabled = !r.eight_ball
        && r.chosen == -1
        && r.answerer.is_some_and(|a| game.players[a].lifelines != 0);
    let g = &mut art.buttons;
    if enabled {
        g.buttons[LIFELINE_BUTTON].state = NORMAL;
        let a = g.buttons[LIFELINE_BUTTON].anims[0];
        if a.cur == a.end {
            g.set_anim(LIFELINE_BUTTON, 0, 22, 22);
        }
    } else {
        g.set_disabled(LIFELINE_BUTTON, true);
    }
}

/// 0x4036d0: pump (no answer yet) or keep, without drawing.
pub fn answer_buttons(ctx: &mut Ctx, game: &mut Game, art: &mut Art) {
    lifeline_button(game, art);
    let mode = if game.round.chosen != -1 {
        Mode::Keep
    } else {
        Mode::Pump
    };
    art.buttons.update(ctx, mode, false);
}

/// 0x4038b0: keep mode, draw.
pub fn draw_answer_buttons(ctx: &mut Ctx, game: &mut Game, art: &mut Art) {
    lifeline_button(game, art);
    art.buttons.update(ctx, Mode::Keep, true);
}

/// 0x407a60: a pressed A-D (slot in state 1) or N becomes the chosen answer; events
/// 0x0e and 4.
pub fn take_answer(ctx: &mut Ctx, game: &mut Game, art: &mut Art) {
    let r = &mut game.round;
    if r.answerer.is_none() || r.chosen != -1 {
        return;
    }
    let Some(res) = art.buttons.result else {
        return;
    };
    if res == ID_LIFELINE_BUTTON {
        r.chosen = 4;
    } else {
        let Some(slot) = art.buttons.current else {
            return;
        };
        if r.question.state(slot as i32) != 1 {
            return;
        }
        r.chosen = slot as i32;
        art.buttons
            .set_all_anims(slot, CHOSEN_ANIM.0, CHOSEN_ANIM.1);
    }
    sound_event(ctx, 0x0e, 0);
    sound_event(ctx, 0x04, 0);
}

/// Whether the 'N' button can be pressed (for tests).
pub fn lifeline_button_enabled(art: &Art) -> bool {
    art.buttons.buttons[LIFELINE_BUTTON].state != DISABLED
}
