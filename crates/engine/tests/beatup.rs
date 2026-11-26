//! Corpus tests: the beat-up round (F5) with the shipped art, driven by a headless
//! script that reacts to the beat-up state.

use std::path::Path;
use std::sync::{Arc, Mutex};

use engine::beatup::{BeatUpOutcome, beat_up_round};
use engine::{Assets, Ctx, DebugState, DrawOp, Frame, Game, HeadlessHost, Player, ScriptEvent, Vk};
use gamedata::GameData;

fn root() -> Option<String> {
    let r = std::env::var("MILIONAR_DATA").ok();
    if r.is_none() {
        println!("skipped: MILIONAR_DATA not set");
    }
    r
}

fn assets(root: &str) -> Assets {
    Assets::new(GameData::open(Path::new(root)).unwrap())
}

/// Two players: player 0 (Mill 1) beats player 1 (Mill 2).
fn game(root: &str) -> Game {
    let mut game = Game::new(assets(root));
    game.player_count = 2;
    for (i, p) in game.players.iter_mut().enumerate().take(2) {
        *p = Player {
            millionaire: i as i32,
            money: 1_000_000,
            ..Player::EMPTY
        };
    }
    game.remaining_opponents = 1;
    game.round.beater = Some(0);
    game.round.beaten = Some(1);
    game.round.answerer = Some(0);
    game
}

struct Run {
    outcome: BeatUpOutcome,
    frames: Vec<Frame>,
    /// Debug state seen before each present.
    seen: Vec<DebugState>,
    /// (present index, key) of every tap.
    taps: Vec<(u64, Vk)>,
}

/// Run one round. `react(present, state, taps so far)` returns a key to tap (down and up
/// before the coming present, so shorter than a tick).
fn run(
    game: &mut Game,
    real_audio: Option<&str>,
    mut react: impl FnMut(u64, &DebugState, usize) -> Option<Vk> + Send + 'static,
) -> Run {
    let seen = Arc::new(Mutex::new((Vec::new(), Vec::new())));
    let s2 = seen.clone();
    let script = vec![(20_000, ScriptEvent::Quit)];
    let mut host = match real_audio {
        Some(r) => HeadlessHost::new(script, Box::new(assets(r))),
        None => HeadlessHost::with_script(script),
    };
    host.set_responder(Box::new(move |t, d| {
        let mut s = s2.lock().unwrap();
        s.0.push(d.clone());
        let n = s.1.len();
        match react(t, d, n) {
            Some(vk) => {
                s.1.push((t, vk));
                vec![ScriptEvent::Tap(vk)]
            }
            None => vec![],
        }
    }));
    host.keep_frames();
    let outcome = {
        let mut ctx = Ctx::new(&mut host, 5);
        beat_up_round(&mut ctx, game).unwrap()
    };
    let (seen, taps) = seen.lock().unwrap().clone();
    Run {
        outcome,
        frames: host.frames().to_vec(),
        seen,
        taps,
    }
}

fn smks(game: &Game, f: &Frame) -> Vec<(String, u32, i32, i32)> {
    f.ops
        .iter()
        .filter_map(|op| match op {
            DrawOp::Smk {
                asset, frame, x, y, ..
            } => Some((
                game.assets.asset_name(*asset).unwrap_or("?").to_string(),
                *frame,
                *x,
                *y,
            )),
            _ => None,
        })
        .collect()
}

fn names(game: &Game, f: &Frame) -> Vec<String> {
    smks(game, f).into_iter().map(|s| s.0).collect()
}

/// Tap Right every 12 presents while fighting, up to `n` taps.
fn tap_right(n: usize) -> impl FnMut(u64, &DebugState, usize) -> Option<Vk> + Send + 'static {
    let mut last = 0;
    move |t, d, taps| {
        if d.beat_up_state == Some(1) && taps < n && t >= last + 12 {
            last = t;
            Some(Vk::RIGHT)
        } else {
            None
        }
    }
}

#[test]
fn normal_round_taps_land_once_each() {
    let Some(root) = root() else { return };
    let mut game = game(&root);
    let r = run(&mut game, None, tap_right(5));
    assert_eq!(r.outcome, BeatUpOutcome::default());
    assert_eq!(r.taps.len(), 5);
    let fighting = r.seen.iter().filter(|d| d.beat_up_state == Some(1)).count();
    assert_eq!(fighting, 119);
    // Intro Bs_Bg 1, 2; the fight on frame 4 with the actors; outro 6, 7, 8.
    let bg: Vec<u32> = r
        .frames
        .iter()
        .filter_map(|f| smks(&game, f).first().cloned())
        .filter(|s| s.0 == "Beat/Bs_Bg.Smk")
        .map(|s| s.1)
        .collect();
    assert_eq!(&bg[..3], &[1, 2, 4]);
    assert_eq!(&bg[bg.len() - 3..], &[6, 7, 8]);
    assert!(bg[2..bg.len() - 3].iter().all(|&f| f == 4));
    let fight: Vec<&Frame> = r
        .frames
        .iter()
        .filter(|f| names(&game, f).iter().any(|n| n == "Beat/Mill_02.Smk"))
        .collect();
    assert!(fight.len() >= 119);
    // Draw order: millionaire (z 200), fist (z 300) and GetReady (z 301) above it.
    let first = smks(&game, fight[0]);
    let order: Vec<&str> = first.iter().map(|s| s.0.as_str()).collect();
    assert_eq!(
        order,
        [
            "Beat/Bs_Bg.Smk",
            "Beat/Mill_02.Smk",
            "Beat/Fsts_All.Smk",
            "Beat/GetReady.Smk"
        ]
    );
    // Idle frame 25 at damage level 0; Fsts_All 640x476 anchored at (320,479).
    assert_eq!(first[1].1, 25);
    assert_eq!((first[2].1, first[2].2, first[2].3), (1, 0, 4));
    // GetReady shows the beater (frame 1).
    assert_eq!(first[3].1, 1);
    // The money went down: no stones before a level change, none here (25000 lost).
    assert!(
        !r.frames
            .iter()
            .any(|f| names(&game, f).iter().any(|n| n == "Beat/MStones.Smk"))
    );
    // Every tap is shorter than a tick and starts exactly one Right punch (frames 5, 6;
    // Review Focus 1). A punch lands (hit anim 1..3, 5000) unless the millionaire
    // blocks it (block 8 = 45 at level 0, after a recent hit on a 35th tick).
    let frames_of = |file: &str| -> Vec<u32> {
        fight
            .iter()
            .flat_map(|f| smks(&game, f))
            .filter(|s| s.0 == file)
            .map(|s| s.1)
            .collect()
    };
    let fist = frames_of("Beat/Fsts_All.Smk");
    assert_eq!(fist.iter().filter(|&&f| f == 5).count(), 5);
    assert_eq!(fist.iter().filter(|&&f| f == 6).count(), 5);
    let mill = frames_of("Beat/Mill_02.Smk");
    let landed = mill.iter().filter(|&&f| f == 1).count() as i32;
    assert!(landed >= 1);
    assert_eq!(game.players[1].money, 1_000_000 - landed * 5000);
    assert!(
        mill.iter().all(|f| [1, 2, 3, 25, 45, 55, 65].contains(f)),
        "{mill:?}"
    );
    assert!(fight.iter().all(|f| {
        let n = names(&game, f);
        !n.iter().any(|x| x.starts_with("Beat/Sp_"))
    }));
}

#[test]
fn level_change_drops_stones() {
    let Some(root) = root() else { return };
    let mut game = game(&root);
    game.players[1].money = 905_000;
    let r = run(&mut game, None, tap_right(1));
    assert_eq!(game.players[1].money, 900_000);
    let ys: Vec<(u32, i32)> = r
        .frames
        .iter()
        .flat_map(|f| smks(&game, f))
        .filter(|s| s.0 == "Beat/MStones.Smk")
        .map(|s| (s.1, s.3))
        .collect();
    // 30 ticks on the y path (bottom edge), frame = level + 1 = 2; MStones is 564x100.
    assert_eq!(ys.len(), 30);
    assert!(ys.iter().all(|&(f, _)| f == 2));
    let path: Vec<i32> = ys.iter().map(|&(_, y)| y + 100 - 1).collect();
    assert_eq!(&path[..8], &[0, 50, 100, 150, 170, 173, 175, 173]);
    assert_eq!(*path.last().unwrap(), 0);
}

#[test]
fn special_round_shows_weapon_and_hits_harder() {
    let Some(root) = root() else { return };
    let mut game = game(&root);
    game.round.special = true;
    game.player_count = 4;
    for (i, p) in game.players.iter_mut().enumerate() {
        p.millionaire = i as i32;
        p.money = 1_000_000;
    }
    let r = run(&mut game, None, tap_right(2));
    assert_eq!(game.players[1].money, 1_000_000 - 2 * 15000);
    let weapon: Vec<u32> = r
        .frames
        .iter()
        .flat_map(|f| smks(&game, f))
        .filter(|s| s.0 == "Beat/Sp_Weap.Smk")
        .map(|s| s.1)
        .collect();
    assert!(!weapon.is_empty());
    let w = weapon[0];
    assert!((4..=6).contains(&w), "Mill 2 weapons are 4..6, got {w}");
    let fist = ["FistM2C.Smk", "FistM2A.Smk", "FistM2B.Smk"][w as usize - 4];
    assert!(
        r.frames
            .iter()
            .any(|f| names(&game, f).iter().any(|n| *n == format!("Beat/{fist}")))
    );
    assert!(r.seen.iter().any(|d| d.screen == "Weapon"));
}

#[test]
fn knockout_plays_end_cutscene_with_real_audio() {
    let Some(root) = root() else { return };
    let mut game = game(&root);
    game.players[1].money = 5000;
    let r = run(&mut game, Some(&root), tap_right(1));
    assert!(r.outcome.knocked_out);
    assert_eq!(r.outcome.champion, Some(0));
    assert_eq!((game.players[1].money, game.remaining_opponents), (0, 0));
    assert!(game.asset_errors.is_empty(), "{:?}", game.asset_errors);
    // KO anim 76..98 of Mill_02, then the End_02 cutscene, then the outro.
    let mill: Vec<u32> = r
        .frames
        .iter()
        .flat_map(|f| smks(&game, f))
        .filter(|s| s.0 == "Beat/Mill_02.Smk")
        .map(|s| s.1)
        .collect();
    let ko: Vec<u32> = mill.iter().copied().filter(|&f| f >= 76).collect();
    assert_eq!(ko, (76..=98).collect::<Vec<_>>());
    let end = r
        .frames
        .iter()
        .filter(|f| names(&game, f).iter().any(|n| n == "Cuts/End_02.smk"))
        .count();
    assert!(end > 10, "End_02 frames: {end}");
    let last = smks(&game, r.frames.last().unwrap());
    assert_eq!((last[0].0.as_str(), last[0].1), ("Beat/Bs_Bg.Smk", 8));
}
