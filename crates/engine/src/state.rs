//! Game state shared by every screen: assets, the player records and the menu choices.
//!
//! Screens take `(ctx: &mut Ctx, game: &mut Game)`. Assets are reached through
//! `game.assets`; a screen loads its overlays with [`with_sprites`], which releases them on
//! every exit path (normal return, `Quit`, `NewGame`), and draws them through the
//! [`Sprite`] handles it receives.

use std::path::PathBuf;

use render::{AssetId, DrawOp};

use crate::assets::Assets;
use crate::ctx::{Abort, Ctx};
use crate::game::{Round, Session};

/// Pause / play-again result ids (tables 0x41f400, 0x41f5b0).
pub const ID_END: i32 = 0x320;
pub const ID_RESUME: i32 = 0x321;
pub const ID_AGAIN: i32 = 0x322;

/// One entry of `g_players` 0x41fb90 (F4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Player {
    /// Millionaire 0..4, -1 = no player.
    pub millionaire: i32,
    pub money: i32,
    pub lifelines: i32,
    pub fifty_fifty: bool,
    pub pass_the_buck: bool,
    pub magic_8_ball: bool,
    pub chicken: bool,
}

impl Player {
    /// `MainMenuLoop`'s reset: char -1, money 0 (the other fields are left alone there).
    pub const EMPTY: Player = Player {
        millionaire: -1,
        money: 0,
        lifelines: 0,
        fifty_fifty: false,
        pass_the_buck: false,
        magic_8_ball: false,
        chicken: false,
    };
}

pub struct Game {
    pub assets: Assets,
    pub players: [Player; 4],
    /// Number of players 1..4 (`DAT_0041fca0` + 1).
    pub player_count: usize,
    /// Pause menu result (`0x4245c0`), `None` = -1.
    pub pause_choice: Option<i32>,
    /// Play-again result (`0x424660`), `None` = -1.
    pub play_again_choice: Option<i32>,
    /// Assets that failed to load or decode. The original draws nothing for a missing
    /// file and goes on; so do we, keeping the errors for the host to report.
    pub asset_errors: Vec<String>,
    /// Directory of the `.sav` files (`<data_dir>/saves`, F3); `None` = no files are read
    /// or written (every order is shuffled afresh).
    pub saves_dir: Option<PathBuf>,
    /// Opponents left to knock out (`0x41fca4`): players-1 at the menu, decremented by
    /// each KO in the beat-up round.
    pub remaining_opponents: i32,
    /// Lip-sync flag `0x427fa0` (0x40b510): set on host-talking screens; the host only
    /// gestures while it is set (envelopes exist only for clips loaded meanwhile).
    pub lip_sync: bool,
    /// Enable the original's GetAsyncKeyState cheats in the beat-up round (S+W: special
    /// round, K+O: knockout). Off unless the app is started with `--debug-cheats`.
    pub debug_cheats: bool,
    /// State the original keeps in static data for the whole program run.
    pub session: Session,
    /// GameLoop globals: per game and per question.
    pub round: Round,
}

impl Game {
    pub fn new(assets: Assets) -> Self {
        Self {
            assets,
            players: [Player::EMPTY; 4],
            player_count: 1,
            pause_choice: None,
            play_again_choice: None,
            asset_errors: Vec::new(),
            saves_dir: None,
            remaining_opponents: 0,
            lip_sync: false,
            debug_cheats: false,
            session: Session::new(),
            round: Round::default(),
        }
    }

    /// Read and write the `.sav` files in `dir` (normally `<data_dir>/saves`).
    pub fn with_saves_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.saves_dir = Some(dir.into());
        self
    }

    /// Record a load error (kept to a bounded list).
    pub fn note_error(&mut self, e: impl std::fmt::Display) {
        if self.asset_errors.len() < 64 {
            self.asset_errors.push(e.to_string());
        }
    }

    /// Load an overlay; a failure gives an absent sprite that draws nothing.
    pub fn load(&mut self, name: &str, transparent: bool) -> Sprite {
        match self.assets.smk(name) {
            Ok(id) => {
                self.assets.note_blit(id, transparent);
                let (w, h, frames) = self.assets.info(id).unwrap_or((0, 0, 0));
                Sprite {
                    id: Some(id),
                    transparent,
                    width: w as i32,
                    height: h as i32,
                    frames,
                }
            }
            Err(e) => {
                self.note_error(e);
                Sprite::ABSENT
            }
        }
    }

    pub fn release(&mut self, s: Sprite) {
        if let Some(id) = s.id {
            self.assets.release(id);
        }
    }

    /// `true` if any player other than an empty slot has millionaire `m` (0-based).
    pub fn millionaire_taken(&self, m: i32) -> bool {
        self.players.iter().any(|p| p.millionaire == m)
    }
}

/// A loaded overlay (`UsagDefn` with usage 4 opaque or 5 transparent, F6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sprite {
    pub id: Option<AssetId>,
    pub transparent: bool,
    pub width: i32,
    pub height: i32,
    pub frames: u32,
}

impl Sprite {
    pub const ABSENT: Sprite = Sprite {
        id: None,
        transparent: false,
        width: 0,
        height: 0,
        frames: 0,
    };

    /// `SmkBlitFrame` 0x40e070 at top-left (x, y). Frames below 1 (the tables' -1 "no
    /// frame") and past the end draw nothing, as does an absent sprite.
    pub fn draw(&self, ctx: &mut Ctx, frame: i32, x: i32, y: i32) {
        let Some(asset) = self.id else { return };
        let Ok(frame) = u32::try_from(frame) else {
            return;
        };
        if frame == 0 || frame > self.frames {
            return;
        }
        ctx.draw(DrawOp::Smk {
            asset,
            frame,
            x,
            y,
            transparent: self.transparent,
        });
    }

    /// Anchor mode 1 (bottom-centre, F6.1): x -= w/2, y += 1 - h.
    pub fn draw_anchored(&self, ctx: &mut Ctx, frame: i32, x: i32, y: i32) {
        self.draw(ctx, frame, x - (self.width >> 1), y + 1 - self.height);
    }
}

/// Load `specs` (`(file, transparent)`), run `f` with the sprites, then release them
/// whatever `f` returned.
pub fn with_sprites<const N: usize, T>(
    game: &mut Game,
    specs: [(&str, bool); N],
    f: impl FnOnce(&mut Game, [Sprite; N]) -> Result<T, Abort>,
) -> Result<T, Abort> {
    let sprites = specs.map(|(name, t)| game.load(name, t));
    let r = f(game, sprites);
    for s in sprites {
        game.release(s);
    }
    r
}
