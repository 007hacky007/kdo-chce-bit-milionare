//! The beat-up minigame (F5): `BeatUpRound` 0x401000 and `BeatUpLoop` 0x401330.
//!
//! ```text
//! beat_up_round            BeatUpRound 0x401000
//!   special round?         0x4244c4 (schedule F5.5) or the S+W debug cheat
//!     weapon::run          WeaponScreen 0x403e10, weapon w = rand() % 3 + 1 + char * 3
//!   load                   0x401280: Bs_Bg, GetReady, fx; fist (entry 1 or w + 1) and
//!                          millionaire (17 + char) registered (0x40d790) and reset
//!   events 6, 0x4f         MUSI0019; the beater's name
//!   intro                  0x401f10: Bs_Bg 1, 2
//!   fight                  BeatUpLoop 0x401330 (states 0..5), event 0x27(1) at the end
//!   KO (state 5)           Cuts/End_0{char + 1}.smk, not skippable; wait for the voice
//!   outro                  0x401fc0: Bs_Bg 6, 7, 8
//! ```
//!
//! [`Fight`] holds the actors and the globals their callbacks share; each tick runs the
//! callbacks ([`Fight::update`], 0x4014d0) and draws the z-ordered actors
//! ([`Fight::draw`], 0x401580).

pub mod actors;
pub mod fist;
pub mod millionaire;
pub mod schedule;
pub mod weapon;

#[cfg(test)]
mod tests;

use crate::anim::Anim;
use crate::audio_types::{AudioCmd, Channel};
use crate::ctx::{Abort, Ctx};
use crate::game::{sync_debug, wait_voice_frozen};
use crate::input::read_arrow_dir;
use crate::screens::{cutscene, pause};
use crate::sound_events::sound_event;
use crate::state::{Game, Sprite};

use actors::{
    Actor, CROWD, FIST, MILL, MILL_ANIMS, READY, SLOTS, SPRITE_FX, SPRITE_GETREADY, SPRITE_MSTONES,
    STATIC_SLOTS, STONES, STONES_ANIMS, sprite_file,
};

/// What a beat-up round leaves for GameLoop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BeatUpOutcome {
    /// The beaten millionaire was knocked out (money 0, End_0N played).
    pub knocked_out: bool,
    /// 0x41fcbc: set when the last opponent was knocked out.
    pub champion: Option<usize>,
}

/// BeatUpLoop states (0x4244bc).
pub const WAIT_INTRO: i32 = 0;
pub const FIGHT: i32 = 1;
pub const TIME_UP: i32 = 2;
pub const RESULT: i32 = 3;
pub const KO: i32 = 4;
pub const KO_DONE: i32 = 5;

/// Bs_Bg frame ranges: intro 0x401f10, fight 0x401330, outro 0x401fc0.
pub const BG_INTRO: (i32, i32) = (1, 2);
pub const BG_FIGHT: i32 = 4;
pub const BG_OUTRO: (i32, i32) = (6, 8);

/// The actors and the globals the callbacks share (addresses in the original).
#[derive(Debug, Clone)]
pub struct Fight {
    /// Fist, millionaire, MStones, GetReady, crowd (see [`actors`]).
    pub actors: [Actor; 5],
    /// The actor list 0x41de68.
    pub slots: [Option<usize>; SLOTS],
    /// 0x4244bc.
    pub state: i32,
    /// 0x41d75c: fight ticks left.
    pub timer: i32,
    /// 0x4244c0: hits landed.
    pub hits: i32,
    /// 0x434578: the `ReadArrowDir` code polled before the last present.
    pub dir: u8,
    /// 0x423e10: millionaire ticks.
    pub tick: i32,
    /// 0x423dbc: tick of the last landed hit.
    pub last_hit_tick: i32,
    /// 0x423db0: last hit anim (2..7).
    pub last_hit: i32,
    /// 0x423e14: same hit repeated.
    pub repeat: i32,
    /// 0x423e1c: block group of the last hit.
    pub last_group: i32,
    /// 0x423e20 / 0x423db4: path counters of MStones and GetReady.
    pub stones_counter: i32,
    pub ready_counter: i32,
    /// Anim tables 0x41d2f8 and 0x41d4a0, rewritten during the round.
    pub mill_anims: [(i32, i32); 12],
    pub stones_anims: [(i32, i32); 2],
    /// 0x423db8 / 0x423e18 as player indices.
    pub beater: usize,
    pub beaten: usize,
    /// 0x41fcbc.
    pub champion: Option<usize>,
}

impl Fight {
    /// The round's setup in 0x401000 without the asset loads: actor list as shipped
    /// plus fist and millionaire, every actor reset, timer by level, state 0.
    pub fn new(game: &Game) -> Self {
        let beater = game.round.beater.unwrap_or(0).min(3);
        let beaten = game.round.beaten.unwrap_or(0).min(3);
        let mut f = Self {
            actors: [
                Actor::new(320, 479, 300),
                Actor::new(320, 479, 200),
                Actor::new(0, 0, 301),
                Actor::new(320, 460, 301),
                Actor::new(317, 183, 300),
            ],
            slots: STATIC_SLOTS,
            state: WAIT_INTRO,
            timer: schedule::round_ticks(game.round.level),
            hits: 0,
            dir: 0,
            tick: 0,
            last_hit_tick: -99,
            last_hit: 0,
            last_group: 0,
            repeat: 0,
            stones_counter: 0,
            ready_counter: 0,
            mill_anims: MILL_ANIMS,
            stones_anims: STONES_ANIMS,
            beater,
            beaten,
            champion: None,
        };
        f.actors[STONES].active = false;
        f.register(FIST);
        f.actors[FIST].reset();
        f.register(MILL);
        f.actors[MILL].reset();
        f.actors[READY].reset();
        f.actors[CROWD].reset();
        f
    }

    /// One BeatUpLoop tick after the present (0x401330): the state machine. Returns
    /// `false` when the loop ends.
    fn step_state(&mut self, ctx: &mut Ctx) -> bool {
        match self.state {
            WAIT_INTRO => {
                if !ctx.voice_busy() {
                    self.state = FIGHT;
                }
            }
            FIGHT => {
                self.timer -= 1;
                if self.timer == 0 {
                    sound_event(ctx, 0x29, 0);
                    self.state = TIME_UP;
                    ctx.audio(AudioCmd::StopChannel(Channel::Music));
                }
            }
            TIME_UP => {
                if sound_event(ctx, 0x29, 1) == 0 {
                    sound_event(ctx, 0x51, self.hits);
                    self.state = RESULT;
                }
            }
            RESULT => return ctx.voice_busy(),
            KO => {}
            _ => return false,
        }
        true
    }
}

/// `BeatUpRound` 0x401000. Reads `game.round` (beater, beaten, answerer, special,
/// level); lowers the beaten player's money as hits land, decrements
/// `game.remaining_opponents` on a KO and names the beater champion when none remain.
pub fn beat_up_round(ctx: &mut Ctx, game: &mut Game) -> Result<BeatUpOutcome, Abort> {
    ctx.enter_screen("BeatUpRound");
    sync_debug(ctx, game);
    let (Some(beater), Some(beaten)) = (game.round.beater, game.round.beaten) else {
        return Ok(BeatUpOutcome::default());
    };
    let char_ = game.players[beaten].millionaire.clamp(0, 4);
    if schedule::cheat_special(ctx, game) {
        game.round.special = true;
    }
    let fist_sprite = if game.round.special {
        let w = schedule::pick_weapon(&mut ctx.rng, char_);
        weapon::run(ctx, game, w)?;
        ctx.enter_screen("BeatUpRound");
        w + 1
    } else {
        1
    };
    let specs = [
        ("Beat/Bs_Bg.Smk".to_string(), false),
        (sprite_file(SPRITE_GETREADY), true),
        (sprite_file(SPRITE_FX), true),
        (sprite_file(fist_sprite), true),
        (sprite_file(17 + char_), true),
        (sprite_file(SPRITE_MSTONES), true),
    ];
    let sprites: Vec<Sprite> = specs.iter().map(|(n, t)| game.load(n, *t)).collect();
    let r = round(ctx, game, beater, char_, &sprites);
    for s in sprites {
        game.release(s);
    }
    ctx.debug.beat_up_state = None;
    ctx.debug.fist_idle = false;
    sync_debug(ctx, game);
    r
}

fn round(
    ctx: &mut Ctx,
    game: &mut Game,
    beater: usize,
    char_: i32,
    s: &[Sprite],
) -> Result<BeatUpOutcome, Abort> {
    let bg = s[0];
    let mut f = Fight::new(game);
    f.actors[READY].sprite = s[1];
    f.actors[CROWD].sprite = s[2];
    f.actors[FIST].sprite = s[3];
    f.actors[MILL].sprite = s[4];
    f.actors[STONES].sprite = s[5];
    let answerer = game.round.answerer;
    sound_event(ctx, 0x06, 0);
    crate::sound_events::sound_event3(
        ctx,
        0x4f,
        beater as i32,
        i32::from(Some(beater) != answerer),
        0,
    );
    bg_range(ctx, bg, BG_INTRO)?;
    fight_loop(ctx, game, &mut f, bg)?;
    let outcome = BeatUpOutcome {
        knocked_out: f.state == KO_DONE,
        champion: f.champion,
    };
    if outcome.knocked_out {
        ctx.enter_screen("BeatUpEnd");
        let name = format!("Cuts/End_0{}.smk", char_ + 1);
        cutscene::play(ctx, game, &name, cutscene::NOT_SKIPPABLE)?;
        wait_voice_frozen(ctx)?;
    }
    bg_range(ctx, bg, BG_OUTRO)?;
    Ok(outcome)
}

/// 0x401f10 / 0x401fc0: Bs_Bg frames `start..=end`, one present each; no pump, pause
/// check or voice pump.
fn bg_range(ctx: &mut Ctx, bg: Sprite, (start, end): (i32, i32)) -> Result<(), Abort> {
    let mut a = Anim::new(start, end);
    loop {
        bg.draw(ctx, a.cur, 0, 0);
        a.advance();
        ctx.present()?;
        if a.at_start() {
            return Ok(());
        }
    }
}

/// `BeatUpLoop` 0x401330.
fn fight_loop(ctx: &mut Ctx, game: &mut Game, f: &mut Fight, bg: Sprite) -> Result<(), Abort> {
    ctx.enter_screen("BeatUpLoop");
    let frame = |ctx: &mut Ctx, game: &mut Game, f: &mut Fight| {
        bg.draw(ctx, BG_FIGHT, 0, 0);
        f.update(ctx, game);
        f.draw(ctx);
        f.dir = read_arrow_dir(&ctx.input);
        ctx.debug.beat_up_state = Some(f.state as u8);
        ctx.debug.fist_idle = f.actors[FIST].anim_id == 0;
        ctx.pump();
    };
    frame(ctx, game, f);
    loop {
        ctx.present()?;
        pause::on_esc(ctx, game)?;
        ctx.pump_voice();
        if !f.step_state(ctx) {
            break;
        }
        frame(ctx, game, f);
    }
    sound_event(ctx, 0x27, 1);
    Ok(())
}
