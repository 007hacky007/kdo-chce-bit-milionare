//! Menu screens and the top of the state machine (F1.4).
//!
//! ```text
//! run                      PlayLogosThenMainLoop 0x402510: Logos.smk, then MainMenuLoop
//! main_menu_loop           MainMenuLoop 0x405070, repeated while a pause or play-again
//!                          choice is "again" (0x322)
//!   opening                0x402540: stop music, Opening.smk, event 3
//!   player_count::run      0x402570
//!   pick_millionaire::run  0x402fa0 (one pass per player)
//!   keys::run              0x402a40: KeysScreen (2-4 players) then BeatKeyScreen
//!   MM_Open.smk            the show intro
//!   game::game_loop        GameLoop 0x405190 (the question round, crate::game)
//! ```
//!
//! Every screen takes `(ctx, game)`, loads its overlays with
//! [`with_sprites`](crate::state::with_sprites) (released on every exit) and calls
//! [`pause::on_esc`] where the original polls `PauseOnEsc` 0x404810.
//!
//! Pumping: the original reads keys from `PumpMessages` at specific points of each loop,
//! not at every present. Screens call [`Ctx::pump`] exactly there and read
//! [`Ctx::last_key`]; keys that arrive while a loop does not pump stay queued for the next
//! pump, as with the Windows message queue.

pub mod cutscene;
pub mod keys;
pub mod pause;
pub mod pick_millionaire;
pub mod play_again;
pub mod player_count;

use crate::anim::Anim;
use crate::audio_types::{AudioCmd, Channel};
use crate::ctx::{Abort, Ctx};
use crate::sound_events::sound_event;
use crate::state::{Game, ID_AGAIN, Sprite};

/// The whole program flow after startup: Logos, then the main menu loop. Returns
/// `Err(Abort::Quit)` when the window is closed or the credits end the game, `Ok` when
/// the main menu loop ends without a restart.
pub fn run(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    ctx.enter_screen("Logos");
    cutscene::play(ctx, game, "Cuts/Logos.smk", 0)?;
    main_menu_loop(ctx, game)
}

/// `MainMenuLoop` 0x405070.
pub fn main_menu_loop(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    loop {
        for p in &mut game.players {
            p.millionaire = -1;
            p.money = 0;
        }
        ctx.clear_new_game();
        game.pause_choice = None;
        game.play_again_choice = None;
        match main_menu_pass(ctx, game) {
            Ok(()) | Err(Abort::NewGame) => {}
            Err(Abort::Quit) => return Err(Abort::Quit),
        }
        if game.pause_choice != Some(ID_AGAIN) && game.play_again_choice != Some(ID_AGAIN) {
            return Ok(());
        }
    }
}

fn main_menu_pass(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    opening(ctx, game)?;
    player_count::run(ctx, game)?;
    game.remaining_opponents = game.player_count as i32 - 1;
    sound_event(ctx, 0x01, game.player_count as i32 - 1);
    pick_millionaire::run(ctx, game)?;
    keys::run(ctx, game)?;
    ctx.enter_screen("Intro");
    cutscene::play(ctx, game, "Cuts/MM_Open.smk", 0)?;
    if game.player_count == 1 {
        // Forced in 0x405070: no pass the buck with one player.
        let p = &mut game.players[0];
        p.lifelines = 3;
        p.fifty_fifty = true;
        p.pass_the_buck = false;
        p.magic_8_ball = true;
        p.chicken = true;
    }
    crate::game::game_loop(ctx, game)
}

/// 0x402540: stop the music, play Opening.smk, start MUSI0010 (event 3).
fn opening(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    ctx.enter_screen("Opening");
    ctx.audio(AudioCmd::StopChannel(Channel::Music));
    cutscene::play(ctx, game, "Menus/Opening.smk", 0)?;
    sound_event(ctx, 0x03, 0);
    Ok(())
}

/// The anim loop every simple screen phase uses (0x402620, 0x402850, 0x402970,
/// 0x403150, 0x403610): draw `start`, advance, pump; then present until the anim wraps.
/// Presents `end - start + 1` ticks, the last one showing `end`; no pause check.
pub(crate) fn play_range(ctx: &mut Ctx, sprite: Sprite, start: i32, end: i32) -> Result<(), Abort> {
    let mut a = Anim::new(start, end);
    sprite.draw(ctx, a.cur, 0, 0);
    a.advance();
    ctx.pump();
    loop {
        ctx.present()?;
        ctx.pump_voice();
        if a.at_start() {
            return Ok(());
        }
        sprite.draw(ctx, a.cur, 0, 0);
        a.advance();
        ctx.pump();
    }
}
