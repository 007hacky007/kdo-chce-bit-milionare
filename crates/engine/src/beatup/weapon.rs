//! `WeaponScreen` 0x403e10 (F1.4, F5.1): the special round's weapon.
//!
//! Sp_BG frames 1..24 loop (fully opaque, so the never-cleared canvas does not show
//! through); events 7 (MUSI0020) and 0x54. When the voice queue is empty Sp_Weap frame w
//! appears at (326,82) with event 0x55(w - 1); then the screen ends on Space or when the
//! voice queue is empty again. Pump before the present, voice pump and PauseOnEsc after.

use crate::anim::Anim;
use crate::ctx::{Abort, Ctx};
use crate::input::Vk;
use crate::screens::pause;
use crate::sound_events::sound_event;
use crate::state::{Game, with_sprites};

pub const WEAPON_POS: (i32, i32) = (326, 82);

pub fn run(ctx: &mut Ctx, game: &mut Game, w: i32) -> Result<(), Abort> {
    ctx.enter_screen("Weapon");
    with_sprites(
        game,
        [("Beat/Sp_BG.Smk", true), ("Beat/Sp_Weap.Smk", true)],
        |game, [bg, weapon]| {
            let mut a = Anim::new(1, 24);
            let mut shown = false;
            sound_event(ctx, 0x07, 0);
            sound_event(ctx, 0x54, 0);
            loop {
                bg.draw(ctx, a.cur, 0, 0);
                a.advance();
                if shown {
                    weapon.draw(ctx, w, WEAPON_POS.0, WEAPON_POS.1);
                }
                ctx.pump();
                ctx.present()?;
                ctx.pump_voice();
                pause::on_esc(ctx, game)?;
                if shown {
                    if ctx.last_key() == Some(Vk::SPACE) || !ctx.voice_busy() {
                        return Ok(());
                    }
                } else if !ctx.voice_busy() {
                    shown = true;
                    sound_event(ctx, 0x55, w - 1);
                }
            }
        },
    )
}
