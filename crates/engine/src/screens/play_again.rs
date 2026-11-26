//! `PlayAgainScreen` 0x404c10 / `PlayAgainMenu` 0x404e10 (F1.4).
//!
//! LogoStill.smk slides over the caller's last presented frame (as the pause menu,
//! 0x404cf0), then MM_Pa.smk at (145,305) plays 1..21 and holds 22 with button group
//! 0x41f5b0 (keys only): A again, N no. Event 0x4b on menu tick 75 if the intro is over,
//! event 0x4c every 899 ticks. A choice stops the voice, plays event 0x0e and ends the
//! menu 3 ticks later: A shows LogoStill alone for one tick and returns with
//! `game.play_again_choice = Some(ID_AGAIN)` (MainMenuLoop then starts over); N plays
//! Credits.smk and quits.

use crate::anim::Anim;
use crate::audio_types::AudioCmd;
use crate::button_tables::TABLE_41F5B0;
use crate::buttons::{ButtonGroup, Mode};
use crate::ctx::{Abort, Ctx};
use crate::screens::pause::{credits_then_quit, draw_logo, pin_background, slide_in, unpin};
use crate::sound_events::sound_event;
use crate::state::{Game, ID_AGAIN, ID_END, Sprite, with_sprites};

pub const MENU_POS: (i32, i32) = (145, 305);
/// MM_Pa.smk: intro 1..21, idle 22, exit 22.
pub const ANIMS: [(i32, i32); 3] = [(1, 21), (22, 22), (22, 22)];
/// Menu tick of the "play again?" question (event 0x4b).
pub const QUESTION_TICK: u32 = 75;
pub const EXIT_TICKS: i32 = 4;
pub const IDLE_LINE_TICKS: u32 = 899;

pub fn run(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    ctx.enter_screen("PlayAgain");
    let background = ctx.last_presented().clone();
    let pins = pin_background(game, &background);
    let r = with_sprites(
        game,
        [
            ("Menus/LogoStill.smk", false),
            ("Menus/MM_Pa.smk", true),
            ("Menus/MM_PaClk.smk", true),
        ],
        |game, [logo, menu, clk]| {
            slide_in(ctx, &background, logo)?;
            play_again_menu(ctx, game, logo, menu, clk)
        },
    );
    unpin(game, pins);
    r
}

fn play_again_menu(
    ctx: &mut Ctx,
    game: &mut Game,
    logo: Sprite,
    menu: Sprite,
    clk: Sprite,
) -> Result<(), Abort> {
    let mut group = ButtonGroup::new(&TABLE_41F5B0, |_| clk);
    group.mouse_disabled = true;
    let mut anims = ANIMS.map(|(s, e)| Anim::new(s, e));
    let mut phase = 0usize;
    let mut countdown = EXIT_TICKS;
    let mut ticks = 0u32;
    let mut credits = false;
    loop {
        draw_logo(ctx, logo);
        menu.draw(ctx, anims[phase].cur, MENU_POS.0, MENU_POS.1);
        anims[phase].advance();
        ticks += 1;
        if ticks == QUESTION_TICK && phase == 1 {
            sound_event(ctx, 0x4b, 0);
        }
        if ticks.is_multiple_of(IDLE_LINE_TICKS) {
            sound_event(ctx, 0x4c, 0);
        }
        let mode = if phase == 1 { Mode::Pump } else { Mode::Keep };
        group.update(ctx, mode, true);
        if phase == 0 && anims[0].at_start() {
            phase = 1;
        }
        if phase == 1 {
            if group.result == Some(ID_AGAIN) {
                phase = 2;
                ctx.audio(AudioCmd::StopVoice);
                sound_event(ctx, 0x0e, 0);
            }
            if group.result == Some(ID_END) {
                credits = true;
                phase = 2;
                ctx.audio(AudioCmd::StopVoice);
                sound_event(ctx, 0x0e, 0);
            }
        }
        if phase == 2 {
            countdown -= 1;
            if countdown == 0 {
                game.play_again_choice = group.result;
                if credits {
                    return credits_then_quit(ctx, game);
                }
                // The composed menu frame is replaced by LogoStill alone (0x40e450 on the
                // LogoStill buffer).
                ctx.frame.ops.clear();
                draw_logo(ctx, logo);
                return ctx.present();
            }
        }
        ctx.present()?;
        ctx.pump_voice();
    }
}
