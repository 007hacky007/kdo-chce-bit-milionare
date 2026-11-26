//! `PickMillionaireScreen` 0x402fa0 and its per-player pass 0x403210 (F1.4, F4.1).
//!
//! Entry: events 3 and 0x20, Pm.smk intro 1..49 (0x403150). Then one pass per player
//! (0x403210) over background frame 51: button group 0x41e270 (keys only): Z previous,
//! D next (both event 0x10; browsing skips millionaires taken by earlier players and
//! wraps 1..5, 0x4035b0), P pick (event 0x44), I bio (Bios.smk frame = millionaire,
//! events 0x45 and 0x0e) and, in the bio only, Z back (event 0x0e, StopVoice). The
//! portrait is Pm_Mill.smk frame = millionaire at (178,63); players 2-4 get their
//! Pm_Plyrs.smk banner at (45,8) (1..11 then hold 13, 15..24 hold 26, 28..37 hold 39;
//! the anim switches to the hold frame when it reaches the end frame, which is never
//! shown). Event 0x43(player) on the pass's 12th tick. After a pick the pass waits for
//! the voice to end; any key cuts it short (event 0x19, StopVoice). Exit: Pm_End.smk 1..7.

use crate::anim::Anim;
use crate::audio_types::AudioCmd;
use crate::button_tables::TABLE_41E270;
use crate::buttons::{ButtonGroup, Mode};
use crate::ctx::{Abort, Ctx};
use crate::screens::{pause, play_range};
use crate::sound_events::sound_event;
use crate::state::{Game, Player, Sprite, with_sprites};

pub const INTRO: (i32, i32) = (1, 49);
pub const IDLE: i32 = 51;
pub const END: (i32, i32) = (1, 7);
/// Pm_Plyrs.smk banner per player: (start, end, hold); player 1 has none.
pub const BANNER: [Option<(i32, i32, i32)>; 4] = [
    None,
    Some((1, 11, 13)),
    Some((15, 24, 26)),
    Some((28, 37, 39)),
];
/// Tick of a pass at which the "player N, choose" line plays.
pub const NAME_TICK: u32 = 12;

const ID_PREV: i32 = 201;
const ID_NEXT: i32 = 202;
const ID_PICK: i32 = 203;
const ID_BIO: i32 = 204;
const ID_BACK: i32 = 205;
/// Indices in table 0x41e270.
const BROWSE_BUTTONS: [usize; 4] = [1, 2, 3, 4];
const BACK_BUTTON: usize = 5;

/// `BrowseMillionaire` 0x4035b0: step from `m` (1..5) by `step` (0 = first free from
/// `m`), wrapping 1..5 and skipping millionaires another player has.
pub fn browse(game: &Game, m: i32, step: i32) -> i32 {
    let (mut m, mut step) = (m, step);
    for _ in 0..=5 {
        m += step;
        if m < 1 {
            m = 5;
        } else if m > 5 {
            m = 1;
        }
        if step == 0 {
            step = 1;
        }
        if !game.millionaire_taken(m - 1) {
            break;
        }
    }
    m
}

struct Art {
    pm: Sprite,
    bios: Sprite,
    mill: Sprite,
    clk: Sprite,
    plyrs: Sprite,
}

pub fn run(ctx: &mut Ctx, game: &mut Game) -> Result<(), Abort> {
    ctx.enter_screen("PickMillionaire");
    with_sprites(
        game,
        [
            ("Menus/Pm.smk", false),
            ("Menus/Pm_End.smk", false),
            ("Menus/Bios.smk", false),
            ("Menus/Pm_Mill.smk", true),
            ("Menus/Pm_Clk.smk", true),
            ("Menus/Pm_Plyrs.smk", true),
        ],
        |game, [pm, pm_end, bios, mill, clk, plyrs]| {
            let art = Art {
                pm,
                bios,
                mill,
                clk,
                plyrs,
            };
            sound_event(ctx, 0x03, 0);
            sound_event(ctx, 0x20, 0);
            play_range(ctx, pm, INTRO.0, INTRO.1)?;
            for p in 0..4 {
                pick_one(ctx, game, &art, p)?;
            }
            play_range(ctx, pm_end, END.0, END.1)
        },
    )
}

/// One player's pass (0x403210); skipped for players beyond the count.
fn pick_one(ctx: &mut Ctx, game: &mut Game, art: &Art, p: usize) -> Result<(), Abort> {
    if p >= game.player_count {
        return Ok(());
    }
    let mut group = ButtonGroup::new(&TABLE_41E270, |smk| {
        if smk == TABLE_41E270[0].smk {
            art.mill
        } else {
            art.clk
        }
    });
    group.set_disabled(BACK_BUTTON, true);
    group.mouse_disabled = true;
    let mut banner = BANNER[p].map(|(s, e, hold)| (Anim::new(s, e), e, hold));
    let mut m = browse(game, 1, 0);
    let mut bio = 0;
    let mut picked = false;
    let mut ticks = 0u32;
    loop {
        if bio == 0 {
            art.pm.draw(ctx, IDLE, 0, 0);
        } else {
            art.bios.draw(ctx, bio, 0, 0);
        }
        ticks += 1;
        if ticks == NAME_TICK {
            sound_event(ctx, 0x43, p as i32);
        }
        group.update(ctx, if picked { Mode::Flush } else { Mode::Pump }, true);
        if group.result.is_some()
            && let Some(c) = group.current
        {
            group.set_normal(c);
        }
        match group.result {
            Some(ID_PREV) => {
                m = browse(game, m, -1);
                sound_event(ctx, 0x10, 0);
            }
            Some(ID_NEXT) => {
                m = browse(game, m, 1);
                sound_event(ctx, 0x10, 0);
            }
            _ => {}
        }
        if bio == 0 {
            if let Some((a, end, hold)) = &mut banner {
                art.plyrs.draw(ctx, a.cur, 45, 8);
                a.advance();
                if a.cur == *end {
                    *a = Anim::new(*hold, *hold);
                }
            }
            art.mill.draw(ctx, m, 178, 63);
        }
        ctx.present()?;
        if group.result == Some(ID_BIO) {
            for b in BROWSE_BUTTONS {
                group.set_disabled(b, true);
            }
            group.set_normal(BACK_BUTTON);
            sound_event(ctx, 0x45, m - 1);
            sound_event(ctx, 0x0e, 0);
            ctx.clear_last_key();
            bio = m;
        }
        if group.result == Some(ID_BACK) {
            for b in BROWSE_BUTTONS {
                group.set_normal(b);
            }
            group.set_disabled(BACK_BUTTON, true);
            sound_event(ctx, 0x0e, 0);
            bio = 0;
            ctx.audio(AudioCmd::StopVoice);
            ctx.clear_last_key();
        }
        pause::on_esc(ctx, game)?;
        ctx.pump_voice();
        if picked {
            ctx.pump();
            if ctx.last_key().is_some() {
                sound_event(ctx, 0x19, 0);
                ctx.audio(AudioCmd::StopVoice);
                return Ok(());
            }
            if !ctx.voice_busy() {
                return Ok(());
            }
            continue;
        }
        if group.result == Some(ID_PICK) {
            game.players[p] = Player {
                millionaire: m - 1,
                money: 1_000_000,
                lifelines: 3,
                fifty_fifty: true,
                pass_the_buck: true,
                magic_8_ball: true,
                chicken: true,
            };
            picked = true;
            sound_event(ctx, 0x44, m - 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Player;

    fn game_with(taken: &[i32]) -> Game {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("game")).unwrap();
        std::fs::write(dir.path().join("game/x.txt"), b"x").unwrap();
        let data = gamedata::GameData::open(dir.path()).unwrap();
        let mut g = Game::new(crate::assets::Assets::new(data));
        for (i, &m) in taken.iter().enumerate() {
            g.players[i] = Player {
                millionaire: m,
                ..Player::EMPTY
            };
        }
        g
    }

    #[test]
    fn browse_skips_taken_and_wraps() {
        let g = game_with(&[0, 2]);
        assert_eq!(browse(&g, 1, 0), 2);
        assert_eq!(browse(&g, 2, 1), 4);
        assert_eq!(browse(&g, 4, 1), 5);
        assert_eq!(browse(&g, 5, 1), 2);
        assert_eq!(browse(&g, 2, -1), 5);
        let free = game_with(&[]);
        assert_eq!(browse(&free, 1, 0), 1);
        assert_eq!(browse(&free, 1, -1), 5);
    }
}
