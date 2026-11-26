//! `PlayerCountScreen` 0x402570 (F1.4): Np.smk with the Np_Ro.smk buttons '1'..'4'.
//!
//! Event 3 on entry. Intro (0x402620): event 0x41, Np frames 1..25. Idle (0x4026f0):
//! frame 27 with button group 0x41e030 (keys only); a key '1'..'4' plays events 0x0e and
//! 0x42(n-1) and presses its button, whose anim then loops until the voice line ends.
//! Exit (0x402850, 0x402970): frames 29..38 / 40..49 / 51..60 / 62..72 for 1..4 players,
//! then 74..82.

use crate::button_tables::TABLE_41E030;
use crate::buttons::{ButtonGroup, Mode};
use crate::ctx::{Abort, Ctx};
use crate::input::Vk;
use crate::screens::{pause, play_range};
use crate::sound_events::sound_event;
use crate::state::{Game, with_sprites};

pub const INTRO: (i32, i32) = (1, 25);
pub const IDLE: i32 = 27;
pub const EXIT: [(i32, i32); 4] = [(29, 38), (40, 49), (51, 60), (62, 72)];
pub const EXIT_TAIL: (i32, i32) = (74, 82);

fn digit(key: Option<Vk>) -> Option<usize> {
    match key {
        Some(Vk(k @ b'1'..=b'4')) => Some(usize::from(k - b'0')),
        _ => None,
    }
}

/// Sets `game.player_count`.
pub fn run(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    ctx.enter_screen("PlayerCount");
    with_sprites(
        game,
        [("Menus/Np.smk", false), ("Menus/Np_Ro.smk", true)],
        |game, [np, np_ro]| {
            sound_event(ctx, 0x03, 0);
            sound_event(ctx, 0x41, 0);
            play_range(ctx, np, INTRO.0, INTRO.1)?;

            let mut group = ButtonGroup::new(&TABLE_41E030, |_| np_ro);
            group.mouse_disabled = true;
            let mut chosen = false;
            np.draw(ctx, IDLE, 0, 0);
            group.update(ctx, Mode::Pump, true);
            loop {
                if !chosen && let Some(n) = digit(ctx.last_key()) {
                    sound_event(ctx, 0x0e, 0);
                    sound_event(ctx, 0x42, n as i32 - 1);
                    game.player_count = n;
                }
                ctx.present()?;
                pause::on_esc(ctx, game)?;
                ctx.pump_voice();
                if chosen {
                    ctx.pump();
                    if !ctx.voice_busy() {
                        break;
                    }
                } else if digit(ctx.last_key()).is_some() {
                    chosen = true;
                }
                np.draw(ctx, IDLE, 0, 0);
                let mode = if chosen { Mode::Keep } else { Mode::Pump };
                group.update(ctx, mode, true);
            }

            let (s, e) = EXIT[game.player_count - 1];
            play_range(ctx, np, s, e)?;
            play_range(ctx, np, EXIT_TAIL.0, EXIT_TAIL.1)
        },
    )
}
