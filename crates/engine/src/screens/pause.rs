//! `PauseOnEsc` 0x404810, `LogoSlideIn` 0x404cf0 and `PauseMenu` 0x404910 (F1.4).
//!
//! When the pumped `g_lastKey` is ESC: the voice channel pauses, event 9(50) halves the
//! music, LogoStill.smk slides down over the caller's last presented frame (event 0x2a,
//! 13 ticks at y = 0, 100, ..., 479, bottom-centre anchor at x 320), then the menu runs:
//! Pause.smk at (99,341) frames 1..7 intro, then frame 7 with button group 0x41f400 (keys
//! only): P resume, N new game, K end. Event 0x4c every 899 ticks. A choice plays event
//! 0x0e and ends the menu 3 ticks later (4 counting the tick the key was pumped), then
//! event 9(100): P stops the 0x4c line, resumes the voice and shows LogoStill alone for
//! one tick; N stops the voice and unwinds with `Abort::NewGame`; K stops the voice,
//! plays Credits.smk and quits (WM_CLOSE).

use crate::anim::Anim;
use crate::audio_types::{AudioCmd, Channel, Prefix, SoundId};
use crate::button_tables::TABLE_41F400;
use crate::buttons::{ButtonGroup, Mode};
use crate::ctx::{Abort, Ctx};
use crate::input::Vk;
use crate::screens::cutscene;
use crate::sound_events::sound_event;
use crate::state::{Game, ID_AGAIN, ID_END, ID_RESUME, Sprite, with_sprites};
use render::{AssetId, DrawOp, Frame};

/// y of the LogoStill bottom edge per slide-in tick (locals of 0x404cf0).
pub const SLIDE_Y: [i32; 13] = [
    0, 100, 200, 300, 400, 450, 470, 479, 470, 465, 470, 475, 479,
];
pub const SLIDE_X: i32 = 320;
pub const MENU_POS: (i32, i32) = (99, 341);
/// Pause.smk: intro 1..7, idle 7, exit 7.
pub const ANIMS: [(i32, i32); 3] = [(1, 7), (7, 7), (7, 7)];
/// Ticks from the choice to the end of the menu.
pub const EXIT_TICKS: i32 = 4;
/// Event 0x4c (an idle remark) every this many ticks.
pub const IDLE_LINE_TICKS: u32 = 899;

/// Poll for ESC (the pumped `g_lastKey`) and run the pause menu.
pub fn on_esc(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    if ctx.last_key() != Some(Vk::ESCAPE) {
        return Ok(());
    }
    let caller = ctx.enter_screen("Pause");
    ctx.audio(AudioCmd::PauseChannel(Channel::Voice, true));
    sound_event(ctx, 0x09, 50);
    let background = ctx.last_presented().clone();
    let pins = pin_background(game, &background);
    let r = with_sprites(
        game,
        [
            ("Menus/LogoStill.smk", false),
            ("Menus/Pause.smk", true),
            ("Menus/PauseClk.smk", true),
        ],
        |game, [logo, menu, clk]| {
            slide_in(ctx, &background, logo)?;
            pause_menu(ctx, game, logo, menu, clk)
        },
    );
    unpin(game, pins);
    if r.is_ok() {
        ctx.enter_screen(caller);
        // The caller's canvas still holds its own picture (the menu drew on a copy);
        // screens that reuse "the last frame" as a frozen canvas must see that one.
        ctx.set_last_presented(background);
    }
    r
}

/// Take a reference on every overlay the frozen `background` shows, so it keeps
/// drawing while the menu runs even if the caller released those overlays after
/// presenting (the original copied the canvas pixels; here the frame stays a display
/// list). Handles already collected are skipped.
pub fn pin_background(game: &mut Game, background: &Frame) -> Vec<AssetId> {
    let mut pins = Vec::new();
    for op in &background.ops {
        if let DrawOp::Smk { asset, .. } = op
            && !pins.contains(asset)
            && game.assets.retain(*asset)
        {
            pins.push(*asset);
        }
    }
    pins
}

/// Release what [`pin_background`] took.
pub fn unpin(game: &mut Game, pins: Vec<AssetId>) {
    for id in pins {
        game.assets.release(id);
    }
}

/// `LogoSlideIn` 0x404cf0: 13 presents of LogoStill sliding over `background`.
pub(crate) fn slide_in(ctx: &mut Ctx, background: &Frame, logo: Sprite) -> Result<(), Abort> {
    sound_event(ctx, 0x2a, 0);
    for (i, y) in SLIDE_Y.iter().enumerate() {
        if i > 0 {
            ctx.present()?;
        }
        for op in &background.ops {
            ctx.draw(op.clone());
        }
        logo.draw_anchored(ctx, 1, SLIDE_X, *y);
        ctx.pump();
    }
    ctx.present()
}

/// The full-screen still everything in the menu is drawn over (blitted at 0,0).
pub(crate) fn draw_logo(ctx: &mut Ctx, logo: Sprite) {
    logo.draw(ctx, 1, 0, 0);
}

fn pause_menu(
    ctx: &mut Ctx,
    game: &mut Game,
    logo: Sprite,
    menu: Sprite,
    clk: Sprite,
) -> Result<(), Abort> {
    let mut group = ButtonGroup::new(&TABLE_41F400, |_| clk);
    group.mouse_disabled = true;
    let mut anims = ANIMS.map(|(s, e)| Anim::new(s, e));
    let mut phase = 0usize;
    let mut countdown = EXIT_TICKS;
    let mut ticks = 0u32;
    let mut idle_line = 0u32;

    draw_logo(ctx, logo);
    menu.draw(ctx, anims[0].cur, MENU_POS.0, MENU_POS.1);
    anims[0].advance();
    group.update(ctx, Mode::Keep, true);
    loop {
        ticks += 1;
        if ticks.is_multiple_of(IDLE_LINE_TICKS) {
            idle_line = sound_event(ctx, 0x4c, 0);
        }
        if phase == 0 && anims[0].at_start() {
            phase = 1;
        }
        if phase == 1 {
            match group.result {
                Some(ID_END) | Some(ID_AGAIN) => {
                    phase = 2;
                    ctx.audio(AudioCmd::StopVoice);
                    // The original paused buffers, not the channel; unpause so the
                    // voice queue of the next screen runs.
                    ctx.audio(AudioCmd::PauseChannel(Channel::Voice, false));
                    sound_event(ctx, 0x0e, 0);
                }
                Some(ID_RESUME) => {
                    phase = 2;
                    if let Ok(n) = u16::try_from(idle_line)
                        && n != 0
                    {
                        ctx.audio(AudioCmd::StopSound(SoundId::new(Prefix::Char, n)));
                    }
                    ctx.audio(AudioCmd::PauseChannel(Channel::Voice, false));
                    sound_event(ctx, 0x0e, 0);
                }
                _ => {}
            }
        }
        if phase == 2 {
            countdown -= 1;
            if countdown == 0 {
                sound_event(ctx, 0x09, 100);
                game.pause_choice = group.result;
                return match group.result {
                    Some(ID_AGAIN) => {
                        ctx.frame.ops.clear();
                        ctx.request_abort(Abort::NewGame);
                        Err(Abort::NewGame)
                    }
                    Some(ID_RESUME) => {
                        // 0x40e450 on the LogoStill buffer: the still alone, not the
                        // composed menu.
                        ctx.frame.ops.clear();
                        draw_logo(ctx, logo);
                        ctx.present()
                    }
                    _ => credits_then_quit(ctx, game),
                };
            }
        }
        ctx.present()?;
        draw_logo(ctx, logo);
        menu.draw(ctx, anims[phase].cur, MENU_POS.0, MENU_POS.1);
        anims[phase].advance();
        let mode = if phase == 1 { Mode::Pump } else { Mode::Keep };
        group.update(ctx, mode, true);
    }
}

/// K / N on the end screens: Credits.smk, then WM_CLOSE.
pub(crate) fn credits_then_quit(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    ctx.enter_screen("Credits");
    cutscene::play(ctx, game, "Cuts/Credits.smk", 0)?;
    ctx.request_abort(Abort::Quit);
    Err(Abort::Quit)
}
