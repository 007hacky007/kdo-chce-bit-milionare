//! `GameLoop` 0x405190 and the question round (F1.4, F2, F4).
//!
//! ```text
//! game_loop                 GameLoop 0x405190
//!   init                    level 500, q 0, money 1,000,000 for players 0..remaining,
//!                           special-round schedule (ShuffleDraw 3 + 17, F5.5)
//!   with_art                0x405a70 question-screen assets (freed by 0x405c60)
//!     per question          reset (buzz timer 164, answer -1, ...), unless the previous
//!                           question ended with the chicken lifeline:
//!       infobox::slide_in   event 0x27(1); InfoBoxesSlideIn 0x405d10 (q++, event 2)
//!       infobox::host_intro HostIntro 0x405e30
//!       infobox::slide_out  InfoBoxesSlideOut 0x405fe0
//!       phases::qs_bg_intro Qs_Bg 1..8 (0x4064a0)
//!     select::level_for     SetLevelByQuestionNo 0x406590
//!     select::pick_question PickQuestion 0x405420
//!     phases::*             Qs_Bg 10..21 (0x4065f0), question read (0x4066c0),
//!                           answer panel 22..35 (0x406790), RevealAnswer 0..3
//!                           (0x406860), AnswerPhase (0x406a20, lifelines)
//!     resolve::pick_opponent  0x406bb0 (PickVictimScreen 0x403960 for 3-4 players)
//!     resolve::resolve        ResolveAnswer 0x406c20
//!     resolve::single_player_loss 0x406360
//!   1 player, not correct -> PlayAgainScreen, end
//!   nobody buzzed -> next question; else BeatUpRound (crate::beatup); no champion -> next
//!   question; else WinScreen 0x4060f0 (assets reloaded) -> PlayAgainScreen
//! ```
//!
//! State: [`Session`] (in [`Game::session`]) holds what the original keeps in static data
//! for the whole program run (question orders, host and Qs_PnBox anim tables, the button
//! tables the code edits in place); [`Round`] (in [`Game::round`]) holds the GameLoop
//! globals, reset per game and per question exactly where the original resets them.

pub mod host_anim;
pub mod infobox;
pub mod lifelines;
pub mod phases;
pub mod question;
pub mod resolve;
pub mod saves;
pub mod select;

#[cfg(test)]
mod tests;

use crate::anim::Anim;
pub use crate::beatup::BeatUpOutcome;
use crate::beatup::beat_up_round;
pub use crate::beatup::schedule::special_schedule;
use crate::button_tables::{TABLE_41E5D0, TABLE_41E8A0, TABLE_41ED20, TABLE_41EF60};
use crate::buttons::{ButtonDef, ButtonGroup};
use crate::ctx::{Abort, Ctx};
use crate::screens::play_again;
use crate::sound_events::sound_event;
use crate::state::{Game, Sprite};

use host_anim::HostAnim;
use infobox::PnBox;
use question::Question;
use select::{EASY, Selector};

/// Lifeline menu result ids (table 0x41ed20).
pub const FIFTY_FIFTY: i32 = 600;
pub const PASS_THE_BUCK: i32 = 601;
pub const MAGIC_8_BALL: i32 = 602;
pub const CHICKEN: i32 = 603;
/// The lifeline button 'N' of the answer group (table 0x41e5d0).
pub const ID_LIFELINE_BUTTON: i32 = 0x130;
/// Fresh buzz / answer timer (ticks).
pub const TIMER_TICKS: i32 = 164;
/// Answer timer after a buzz that interrupts the question read (F2.5, F11.11 KEEP).
pub const TIMER_TICKS_INTERRUPT: i32 = 89;
/// Starting money (F4.1).
pub const START_MONEY: i32 = 1_000_000;

/// Static data the original keeps for the whole program run.
#[derive(Debug, Clone)]
pub struct Session {
    pub select: Selector,
    pub host: HostAnim,
    pub pnbox: PnBox,
    /// Button tables 0x41e5d0 (answers), 0x41ed20 (lifeline menu), 0x41e8a0 (victim
    /// picker), 0x41ef60 (pass-the-buck target picker). The original edits states and
    /// anim ranges in place, and later groups start from the edited tables.
    pub answer_table: Vec<ButtonDef>,
    pub lifeline_table: Vec<ButtonDef>,
    pub victim_table: Vec<ButtonDef>,
    pub target_table: Vec<ButtonDef>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            select: Selector::new(),
            host: HostAnim::new(),
            pnbox: PnBox::new(),
            answer_table: TABLE_41E5D0.to_vec(),
            lifeline_table: TABLE_41ED20.to_vec(),
            victim_table: TABLE_41E8A0.to_vec(),
            target_table: TABLE_41EF60.to_vec(),
        }
    }
}

/// The scene background `0x425e50` points at: Mm_Bg (scene 0, host and info boxes) or
/// Qs_Bg (scene 1, the question studio), with its frame anim (+0x104).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bg {
    Main,
    Question,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scene {
    pub bg: Bg,
    pub anim: Anim,
}

/// GameLoop globals (addresses in the original).
#[derive(Debug, Clone)]
pub struct Round {
    /// 0x425e60: question number, incremented by the info-box slide-in.
    pub q: i32,
    /// 0x41fc50: 500 easy, 501 medium, 502 hard (also the beat-up length).
    pub level: i32,
    /// 0x41fcb4: consecutive questions nobody buzzed (-1 at game start).
    pub timeouts: i32,
    /// 0x41fcbc: the player who has knocked out every opponent.
    pub champion: Option<usize>,
    /// 0x425e64: wrong answers this game.
    pub wrong_answers: i32,
    /// 0x423dc0..: special-round question numbers, slot k used when k < players (F5.5).
    pub schedule: [i32; 4],
    /// 0x423db8 / 0x423e18: beater and beaten player of the round just resolved.
    pub beater: Option<usize>,
    pub beaten: Option<usize>,
    /// 0x425e54: the player answering (buzzed in).
    pub answerer: Option<usize>,
    /// 0x425e58: the opponent of the answerer.
    pub opponent: Option<usize>,
    /// 0x425e5c: pass-the-buck target, buzzes in on the next BuzzIn.
    pub pending: Option<usize>,
    /// 0x41fca8 / 0x41fcac.
    pub buzz_timer: i32,
    pub answer_timer: i32,
    /// 0x41fcb0: chosen display slot 0..3, 4 = the lifeline button, -1 = none.
    pub chosen: i32,
    /// 0x41fc54 == 1: the magic 8-ball is running.
    pub eight_ball: bool,
    /// 0x41fc5c == 1: 50:50 used on this question.
    pub fifty_used: bool,
    /// 0x41fc58: the answer asked of the 8-ball (-1 = none).
    pub asked: i32,
    /// 0x4244c4: the next beat-up is a special round.
    pub special: bool,
    /// 0x425e6c: 1 correct, 0 wrong, 2 timeout / nobody answered.
    pub result: i32,
    /// 0x41fcc0: beater drawn when the answer time ran out.
    pub random_beater: Option<usize>,
    /// 0x4245e8: lifeline chosen on this question (600..603, -1 = none).
    pub lifeline: i32,
    /// `g_curQuestion` 0x425380.
    pub question: Question,
    pub scene: Scene,
}

impl Default for Round {
    fn default() -> Self {
        Self {
            q: 0,
            level: EASY,
            timeouts: -1,
            champion: None,
            wrong_answers: 0,
            schedule: [0; 4],
            beater: None,
            beaten: None,
            answerer: None,
            opponent: None,
            pending: None,
            buzz_timer: TIMER_TICKS,
            answer_timer: 0,
            chosen: -1,
            eight_ball: false,
            fifty_used: false,
            asked: -1,
            special: false,
            result: 0,
            random_beater: None,
            lifeline: -1,
            question: Question::default(),
            scene: Scene {
                bg: Bg::Main,
                anim: Anim::new(1, 1),
            },
        }
    }
}

impl Round {
    /// The per-question reset at the top of GameLoop's question loop.
    fn reset_question(&mut self) {
        self.answerer = None;
        self.opponent = None;
        self.pending = None;
        self.buzz_timer = TIMER_TICKS;
        self.chosen = -1;
        self.eight_ball = false;
        self.fifty_used = false;
        self.special = false;
        self.result = 0;
        self.asked = -1;
        self.random_beater = None;
        self.lifeline = -1;
    }

    /// Special round after question q if q equals schedule slot k < players (0x406c20).
    fn check_special(&mut self, players: usize) {
        for k in 0..players.min(4) {
            if self.q == self.schedule[k] {
                self.special = true;
            }
        }
    }
}

/// The question-screen assets (0x405a70; the answer buttons of 0x4036d0).
pub struct Art {
    pub mm_bg: Sprite,
    pub qs_bg: Sprite,
    /// Info boxes by player: Mm_Ib1, Mm_Ib3, Mm_Ib2, Mm_Ib4 (0x4259f0 + p * 0x38).
    pub ib: [Sprite; 4],
    pub heads: [Sprite; 4],
    pub pnbox: Sprite,
    pub regis: Sprite,
    pub ms_small: Sprite,
    /// Answer group 0x424580 over table 0x41e5d0 (Ans1..4Selc, Qs_LlBut), keys only.
    pub buttons: ButtonGroup,
    sprites: Vec<Sprite>,
}

const ART_FILES: [(&str, bool); 16] = [
    ("QuesAnsw/Mm_Bg.Smk", false),
    ("QuesAnsw/Qs_Bg.Smk", false),
    ("QuesAnsw/Mm_Ib1.Smk", true),
    ("QuesAnsw/Mm_Ib3.Smk", true),
    ("QuesAnsw/Mm_Ib2.Smk", true),
    ("QuesAnsw/Mm_Ib4.Smk", true),
    ("QuesAnsw/Mm_Heads.Smk", true),
    ("QuesAnsw/Mm_Heads.Smk", true),
    ("QuesAnsw/Mm_Heads.Smk", true),
    ("QuesAnsw/Mm_Heads.Smk", true),
    ("QuesAnsw/Qs_PnBox.Smk", true),
    ("QuesAnsw/Mm_Regis.Smk", true),
    ("Menus/Ms_Small.Smk", true),
    ("Menus/Ans1Selc.Smk", true),
    ("Menus/Ans2Selc.Smk", true),
    ("Menus/Ans3Selc.Smk", true),
];
const ART_EXTRA: [(&str, bool); 2] = [("Menus/Ans4Selc.Smk", true), ("Menus/Qs_LlBut.Smk", true)];

/// Load the question-screen assets, run `f`, release them on every path and keep the
/// answer table as the group left it.
pub fn with_art<T>(
    game: &mut Game,
    f: impl FnOnce(&mut Game, &mut Art) -> Result<T, Abort>,
) -> Result<T, Abort> {
    let s: Vec<Sprite> = ART_FILES
        .iter()
        .chain(ART_EXTRA.iter())
        .map(|&(name, t)| game.load(name, t))
        .collect();
    let ans = [s[13], s[14], s[15], s[16], s[17]];
    let mut buttons = ButtonGroup::new(&game.session.answer_table, |smk| match smk {
        0x424938 => ans[0],
        0x424970 => ans[1],
        0x4249a8 => ans[2],
        0x4249e0 => ans[3],
        _ => ans[4],
    });
    buttons.mouse_disabled = true;
    let mut art = Art {
        mm_bg: s[0],
        qs_bg: s[1],
        ib: [s[2], s[3], s[4], s[5]],
        heads: [s[6], s[7], s[8], s[9]],
        pnbox: s[10],
        regis: s[11],
        ms_small: s[12],
        buttons,
        sprites: s,
    };
    let r = f(game, &mut art);
    game.session.answer_table = art.buttons.defs();
    for sp in art.sprites {
        game.release(sp);
    }
    r
}

/// Point the scene at `bg` with frames `start..end` (the original's four stores).
pub(crate) fn set_scene(game: &mut Game, bg: Bg, start: i32, end: i32) {
    game.round.scene = Scene {
        bg,
        anim: Anim::new(start, end),
    };
}

/// Only the frame range (the scene stays what it is).
pub(crate) fn set_scene_frames(game: &mut Game, start: i32, end: i32) {
    game.round.scene.anim = Anim::new(start, end);
}

/// 0x40bfa0 + 0x40c1e0: blit the scene background at its current frame and advance.
pub(crate) fn draw_scene(ctx: &mut Ctx, game: &mut Game, art: &Art) {
    let scene = &mut game.round.scene;
    let sprite = match scene.bg {
        Bg::Main => art.mm_bg,
        Bg::Question => art.qs_bg,
    };
    sprite.draw(ctx, scene.anim.cur, 0, 0);
    if scene.anim.start != 0 {
        scene.anim.advance();
    }
}

/// The original's busy-waits on the voice queue without presenting (after the answer,
/// 0x406a20, and after a timeout, 0x407800): `pump_voice; pump; while busy { pump_voice;
/// pump }`. Time only passes here by presenting, so the frozen screen is re-presented
/// each tick (no pause check, as in the original).
pub(crate) fn wait_voice_frozen(ctx: &mut Ctx) -> Result<(), Abort> {
    ctx.pump_voice();
    ctx.pump();
    while ctx.voice_busy() {
        ctx.frame = ctx.last_presented().clone();
        ctx.present()?;
        ctx.pump_voice();
        ctx.pump();
    }
    Ok(())
}

/// Copy the money of every player and the round's roles into the debug state.
pub(crate) fn sync_debug(ctx: &mut Ctx, game: &Game) {
    for (d, p) in ctx.debug.money.iter_mut().zip(&game.players) {
        *d = p.money;
    }
    for (d, p) in ctx.debug.millionaire.iter_mut().zip(&game.players) {
        *d = p.millionaire;
    }
    ctx.debug.answerer = game.round.answerer;
    ctx.debug.beater = game.round.beater;
    ctx.debug.beaten = game.round.beaten;
}

enum Step {
    /// The game is over (play-again shown).
    Done,
    /// Go on: beat-up or the next question.
    Next,
}

/// `GameLoop` 0x405190.
pub fn game_loop(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    ctx.enter_screen("GameLoop");
    let remaining = game.remaining_opponents;
    game.round = Round::default();
    game.session.host.reset_state();
    for (i, p) in game.players.iter_mut().enumerate() {
        p.money = if i as i32 <= remaining {
            START_MONEY
        } else {
            0
        };
    }
    game.round.schedule = special_schedule(&mut ctx.rng);
    sync_debug(ctx, game);
    loop {
        let step = with_art(game, |game, art| questions(ctx, game, art))?;
        if let Step::Done = step {
            return Ok(());
        }
        if game.round.answerer.is_none() {
            continue;
        }
        let outcome = beat_up_round(ctx, game)?;
        game.round.champion = outcome.champion;
        sync_debug(ctx, game);
        if game.round.champion.is_none() {
            continue;
        }
        return with_art(game, |game, art| {
            resolve::win_screen(ctx, game, art)?;
            play_again::run(ctx, game)
        });
    }
}

/// From LAB_0040521f to the free at LAB_004053b8: the questions (repeated after a
/// chicken), the opponent, the resolution and the 1-player end.
fn questions(ctx: &mut Ctx, game: &mut Game, art: &mut Art) -> Result<Step, Abort> {
    loop {
        let intro = game.round.lifeline != CHICKEN;
        game.round.reset_question();
        ctx.debug.correct_slot = None;
        if intro {
            sound_event(ctx, 0x27, 1);
            infobox::slide_in(ctx, game, art)?;
            infobox::host_intro(ctx, game, art)?;
            infobox::slide_out(ctx, game, art)?;
            phases::qs_bg_intro(ctx, game, art)?;
        }
        game.round.level = select::level_for(game.player_count, game.round.q);
        let mut errors = std::mem::take(&mut game.asset_errors);
        let q = select::pick_question(
            &mut ctx.rng,
            &mut game.session.select,
            game.assets.game_data(),
            game.saves_dir.as_deref(),
            game.round.level,
            &mut errors,
        );
        game.asset_errors = errors;
        ctx.debug.correct_slot = Some(q.correct_slot);
        game.round.question = q;
        phases::question_intro(ctx, game, art)?;
        phases::question_read(ctx, game, art)?;
        phases::answer_panel(ctx, game, art)?;
        for slot in 0..4 {
            phases::reveal(ctx, game, art, slot)?;
        }
        phases::answer_phase(ctx, game, art)?;
        if game.round.lifeline != CHICKEN {
            break;
        }
    }
    resolve::pick_opponent(ctx, game)?;
    resolve::resolve(ctx, game, art)?;
    ctx.debug.correct_slot = None;
    resolve::single_player_loss(ctx, game, art)?;
    sync_debug(ctx, game);
    if game.player_count == 1 && game.round.result != 1 {
        play_again::run(ctx, game)?;
        return Ok(Step::Done);
    }
    Ok(Step::Next)
}
