//! Unit tests of the beat-up round (no game data: every sprite is absent and every
//! sound silent, so only the logic runs).

use std::sync::{Arc, Mutex};

use gamedata::GameData;

use super::actors::{FIST, MILL};
use super::schedule::{damage, round_ticks};
use super::{Fight, beat_up_round};
use crate::assets::Assets;
use crate::ctx::Ctx;
use crate::headless::{HeadlessHost, ScriptEvent};
use crate::input::Vk;
use crate::state::{Game, Player};

fn empty_game(players: usize) -> (Game, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("game")).unwrap();
    let mut game = Game::new(Assets::new(GameData::open(dir.path()).unwrap()));
    game.player_count = players;
    for (i, p) in game.players.iter_mut().enumerate().take(players) {
        *p = Player {
            millionaire: i as i32,
            money: 1_000_000,
            ..Player::EMPTY
        };
    }
    game.remaining_opponents = players as i32 - 1;
    let r = &mut game.round;
    r.beater = Some(0);
    r.beaten = Some(players - 1);
    r.answerer = Some(0);
    (game, dir)
}

/// A fight in state 1 (as after the intro voice).
fn fighting(ctx: &mut Ctx, game: &mut Game) -> Fight {
    let mut f = Fight::new(game);
    f.update(ctx, game);
    f.draw(ctx);
    f.state = 1;
    f
}

fn tick(f: &mut Fight, ctx: &mut Ctx, game: &mut Game) {
    f.update(ctx, game);
    f.draw(ctx);
    ctx.frame.ops.clear();
}

#[test]
fn damage_by_players() {
    let normal: Vec<i32> = (1..=4).map(|p| damage(p, false)).collect();
    assert_eq!(normal, [5000, 5000, 8000, 10000]);
    let special: Vec<i32> = (1..=4).map(|p| damage(p, true)).collect();
    assert_eq!(special, [7500, 7500, 12000, 15000]);
}

#[test]
fn hit_only_on_listed_frame() {
    let (mut game, _d) = empty_game(2);
    let mut host = HeadlessHost::with_script(vec![]);
    let mut ctx = Ctx::new(&mut host, 1);
    let mut f = fighting(&mut ctx, &mut game);
    // Right punch (anim 1, frames 5..6): frame 6 does nothing.
    f.set_anim(FIST, 1);
    f.actors[FIST].anim.cur = 6;
    tick(&mut f, &mut ctx, &mut game);
    assert_eq!(game.players[1].money, 1_000_000);
    assert_eq!(f.actors[MILL].anim_id, 0);
    // Shift+Down (anim 6, frames 20..22) hits on 21 only.
    f.set_anim(FIST, 6);
    tick(&mut f, &mut ctx, &mut game);
    assert_eq!(game.players[1].money, 1_000_000, "frame 20");
    tick(&mut f, &mut ctx, &mut game);
    assert_eq!(game.players[1].money, 1_000_000 - 5000, "frame 21");
    assert_eq!(f.actors[MILL].anim_id, 7);
    tick(&mut f, &mut ctx, &mut game);
    assert_eq!(game.players[1].money, 1_000_000 - 5000, "frame 22");
    assert_eq!(f.hits, 1);
}

#[test]
fn three_same_punches_trigger_block() {
    let (mut game, _d) = empty_game(2);
    let mut host = HeadlessHost::with_script(vec![]);
    let mut ctx = Ctx::new(&mut host, 1);
    let mut f = fighting(&mut ctx, &mut game);
    // Hold Right: punches land whenever the millionaire is idle.
    let mut landed = Vec::new();
    for t in 0..40 {
        f.dir = 0x66;
        let before = game.players[1].money;
        tick(&mut f, &mut ctx, &mut game);
        if game.players[1].money != before {
            landed.push(t);
        }
        if landed.len() == 3 {
            break;
        }
    }
    assert_eq!(landed.len(), 3, "{landed:?}");
    assert_eq!(f.repeat, 3);
    // The next tick forces the block anim of group 8 (Right).
    f.dir = 0;
    tick(&mut f, &mut ctx, &mut game);
    assert_eq!(f.actors[MILL].anim_id, 8);
    // Further Right punches are blocked; Up (group 9) lands.
    let money = game.players[1].money;
    for _ in 0..12 {
        f.dir = 0x66;
        tick(&mut f, &mut ctx, &mut game);
    }
    assert_eq!(game.players[1].money, money);
    assert_eq!(f.actors[MILL].anim_id, 8);
    for _ in 0..4 {
        f.dir = 0x68;
        tick(&mut f, &mut ctx, &mut game);
    }
    assert_eq!(game.players[1].money, money - 5000);
    assert_eq!(f.repeat, 1);
}

/// Round outcome, beat-up state per present, screen log.
type RoundRun = (
    super::BeatUpOutcome,
    Vec<Option<u8>>,
    Vec<(u64, &'static str)>,
);

/// Present indices at which the debug state showed beat-up state `s`.
fn run_round(game: &mut Game, script: Vec<(u64, ScriptEvent)>) -> RoundRun {
    let states = Arc::new(Mutex::new(Vec::new()));
    let s2 = states.clone();
    let mut host = HeadlessHost::with_script(script);
    host.set_responder(Box::new(move |_, d| {
        s2.lock().unwrap().push(d.beat_up_state);
        vec![]
    }));
    let mut ctx = Ctx::new(&mut host, 3);
    let out = beat_up_round(&mut ctx, game).unwrap();
    let log = ctx.debug.screen_log.clone();
    let states = states.lock().unwrap().clone();
    (out, states, log)
}

#[test]
fn round_length_by_level() {
    assert_eq!(round_ticks(500), 119);
    for (level, ticks) in [(500, 119), (501, 150), (502, 180)] {
        let (mut game, _d) = empty_game(2);
        game.round.level = level;
        let (out, states, _) = run_round(&mut game, vec![]);
        assert_eq!(out, super::BeatUpOutcome::default());
        let fight = states.iter().filter(|&&s| s == Some(1)).count();
        assert_eq!(fight, ticks, "level {level}");
        assert_eq!(game.players[1].money, 1_000_000);
        assert_eq!(game.remaining_opponents, 1);
    }
}

#[test]
fn ko_at_zero_money_plays_end_cutscene() {
    let (mut game, _d) = empty_game(2);
    game.players[1].money = 5000;
    // Right held from the start: the first punch in state 1 lands.
    let (out, states, log) = run_round(&mut game, vec![(0, ScriptEvent::KeyDown(Vk::RIGHT))]);
    assert!(out.knocked_out);
    assert_eq!(out.champion, Some(0));
    assert_eq!(game.players[1].money, 0);
    assert_eq!(game.remaining_opponents, 0);
    assert!(states.contains(&Some(4)));
    assert!(states.contains(&Some(5)));
    // The beaten millionaire is Mill 2: End_02 (missing here, so it is reported).
    assert!(log.iter().any(|(_, s)| *s == "BeatUpEnd"));
    assert!(
        game.asset_errors.iter().any(|e| e.contains("End_02")),
        "{:?}",
        game.asset_errors
    );
}

#[test]
fn cheats_only_with_debug_option() {
    for cheats in [false, true] {
        let (mut game, _d) = empty_game(2);
        game.debug_cheats = cheats;
        let mut host = HeadlessHost::with_script(vec![]);
        let mut ctx = Ctx::new(&mut host, 1);
        let mut f = fighting(&mut ctx, &mut game);
        ctx.input.held.extend([Vk::K, Vk::O, Vk::S, Vk::W]);
        assert_eq!(super::schedule::cheat_special(&ctx, &game), cheats);
        f.set_anim(FIST, 1);
        tick(&mut f, &mut ctx, &mut game);
        let money = game.players[1].money;
        if cheats {
            assert_eq!((money, f.hits, f.state), (0, 31, 4));
        } else {
            assert_eq!((money, f.hits, f.state), (995_000, 1, 1));
        }
    }
}
