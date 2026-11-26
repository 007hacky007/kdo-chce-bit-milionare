//! `KeysScreen` 0x402a60 (2-4 players) and `BeatKeyScreen` 0x402db0, run in sequence by
//! 0x402a40 (F1.4).
//!
//! Both are three-phase anims over one full-screen overlay: intro, idle until Space
//! (pumped `g_lastKey`), exit. Keys{n}.smk: events 3, 0x1d(n-2), 0x49; Space: StopVoice,
//! 0x0e. BeatKey.smk: events 3, 0x1e, 0x4a; Space: stop music, StopVoice, 0x0e, 0x1f.
//! The pause menu is polled every tick.

use crate::anim::Anim;
use crate::audio_types::{AudioCmd, Channel};
use crate::ctx::{Abort, Ctx};
use crate::input::Vk;
use crate::screens::pause;
use crate::sound_events::sound_event;
use crate::state::{Game, Sprite, with_sprites};

/// A help screen: its file and the frames of its three phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelpScreen {
    pub file: &'static str,
    pub intro: (i32, i32),
    pub idle: i32,
    pub exit: (i32, i32),
}

const fn help(file: &'static str, intro: (i32, i32), idle: i32, exit: (i32, i32)) -> HelpScreen {
    HelpScreen {
        file,
        intro,
        idle,
        exit,
    }
}

/// Keys{n}.smk for 2..4 players (locals of 0x402a60).
pub const KEYS: [HelpScreen; 3] = [
    help("Menus/Keys2.smk", (1, 62), 62, (63, 78)),
    help("Menus/Keys3.smk", (1, 80), 80, (81, 96)),
    help("Menus/Keys4.smk", (1, 99), 99, (100, 114)),
];
/// BeatKey.smk (locals of 0x402db0).
pub const BEAT_KEY: HelpScreen = help("Menus/BeatKey.smk", (1, 36), 36, (37, 80));

pub fn run(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    keys_screen(ctx, game)?;
    beat_key_screen(ctx, game)
}

fn keys_screen(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    if game.player_count < 2 {
        return Ok(());
    }
    ctx.enter_screen("Keys");
    let n = game.player_count - 2;
    let h = KEYS[n];
    with_sprites(game, [(h.file, false)], |game, [s]| {
        if s.id.is_none() {
            return Ok(());
        }
        sound_event(ctx, 0x03, 0);
        sound_event(ctx, 0x1d, n as i32);
        sound_event(ctx, 0x49, 0);
        three_phase(ctx, game, s, h, |ctx| {
            ctx.audio(AudioCmd::StopVoice);
            sound_event(ctx, 0x0e, 0);
        })
    })
}

fn beat_key_screen(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    ctx.enter_screen("BeatKeys");
    let h = BEAT_KEY;
    with_sprites(game, [(h.file, false)], |game, [s]| {
        sound_event(ctx, 0x03, 0);
        sound_event(ctx, 0x1e, 0);
        sound_event(ctx, 0x4a, 0);
        three_phase(ctx, game, s, h, |ctx| {
            ctx.audio(AudioCmd::StopChannel(Channel::Music));
            ctx.audio(AudioCmd::StopVoice);
            sound_event(ctx, 0x0e, 0);
            sound_event(ctx, 0x1f, 0);
        })
    })
}

/// The shared loop of 0x402a60 / 0x402db0: intro until it wraps, idle frame until a
/// pumped Space (then `on_space`), exit until it wraps. Presents (intro length) + idle
/// ticks + (exit length).
fn three_phase(
    ctx: &mut Ctx,
    game: &mut Game,
    s: Sprite,
    h: HelpScreen,
    on_space: impl Fn(&mut Ctx),
) -> Result<(), Abort> {
    let mut anims = [
        Anim::new(h.intro.0, h.intro.1),
        Anim::new(h.idle, h.idle),
        Anim::new(h.exit.0, h.exit.1),
    ];
    let mut phase = 0;
    s.draw(ctx, anims[0].cur, 0, 0);
    anims[0].advance();
    ctx.pump();
    loop {
        ctx.present()?;
        pause::on_esc(ctx, game)?;
        ctx.pump_voice();
        match phase {
            0 => {
                if anims[0].at_start() {
                    phase = 1;
                    anims[1].restart();
                }
            }
            1 => {
                if ctx.last_key() == Some(Vk::SPACE) {
                    on_space(ctx);
                    phase = 2;
                    anims[2].restart();
                }
            }
            _ => {
                if anims[2].at_start() {
                    return Ok(());
                }
            }
        }
        s.draw(ctx, anims[phase].cur, 0, 0);
        anims[phase].advance();
        ctx.pump();
    }
}
