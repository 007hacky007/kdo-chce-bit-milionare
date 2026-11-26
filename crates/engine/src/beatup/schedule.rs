//! Special rounds, weapons, round length and damage (F5.1, F5.3, F5.5).

use crate::ctx::Ctx;
use crate::game::select::level_index;
use crate::input::Vk;
use crate::rng::Lcg;
use crate::state::Game;

/// Beat-up length in fight ticks by level (0x41d740).
pub const ROUND_TICKS: [i32; 3] = [119, 150, 180];
/// Damage per hit by players - 1 (0x41d74c).
pub const DAMAGE: [i32; 4] = [5000, 5000, 8000, 10000];
/// Special-round factor (double at 0x41b230), added to the money and truncated.
pub const SPECIAL_FACTOR: f64 = -1.5;

/// GameLoop start: the special-round schedule (0x405190): `ShuffleDraw(a, 3, 3)` then
/// `ShuffleDraw(a + 1, 17, 6)` over one array, so slot 0 is one of 3..5 and slots 1..3
/// the first three of a shuffle of 6..22 (F11.12 KEEP).
pub fn special_schedule(rng: &mut Lcg) -> [i32; 4] {
    let mut a = [0i32; 18];
    rng.shuffle_draw(&mut a[0..3], 3, Some(3));
    rng.shuffle_draw(&mut a[1..18], 17, Some(6));
    [a[0], a[1], a[2], a[3]]
}

/// Fight ticks for level 500..502 (`*(0x41cf70 + level * 4)`).
pub fn round_ticks(level: i32) -> i32 {
    level_index(level).map_or(ROUND_TICKS[0], |i| ROUND_TICKS[i])
}

/// Money a landed hit takes (positive), for 1..4 players.
pub fn damage(players: usize, special: bool) -> i32 {
    let d = DAMAGE[players.clamp(1, 4) - 1];
    if special {
        -((f64::from(d) * SPECIAL_FACTOR) as i32)
    } else {
        d
    }
}

/// Apply one hit to `money` as 0x401730 does (the special round adds the truncated
/// product, the normal one subtracts).
pub fn apply_damage(money: i32, players: usize, special: bool) -> i32 {
    let d = DAMAGE[players.clamp(1, 4) - 1];
    if special {
        money.saturating_add((f64::from(d) * SPECIAL_FACTOR) as i32)
    } else {
        money.saturating_sub(d)
    }
}

/// Debug cheat S+W at the start of BeatUpRound (GetAsyncKeyState): force a special
/// round. Only with [`Game::debug_cheats`].
pub fn cheat_special(ctx: &Ctx, game: &Game) -> bool {
    game.debug_cheats && ctx.input.is_down(Vk::S) && ctx.input.is_down(Vk::W)
}

/// Debug cheat K+O when a hit lands: knock out at once. Only with
/// [`Game::debug_cheats`].
pub fn cheat_knockout(ctx: &Ctx, game: &Game) -> bool {
    game.debug_cheats && ctx.input.is_down(Vk::K) && ctx.input.is_down(Vk::O)
}

/// The weapon of a special round: `rand() % 3 + 1 + millionaire * 3` (1..15); the
/// fist sprite is entry w + 1.
pub fn pick_weapon(rng: &mut Lcg, millionaire: i32) -> i32 {
    (rng.next() % 3) as i32 + 1 + millionaire * 3
}
