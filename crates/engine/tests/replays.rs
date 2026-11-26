//! Corpus replay tests: whole games driven by the JSON input scripts in `tests/replays/`
//! (ticks and keys only; answers come from the engine debug hook).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use engine::replay::{self, Script};
use engine::{Abort, Assets, AudioCmd, DebugState, Game, SilentSource, SoundSource};
use gamedata::GameData;

fn root() -> Option<String> {
    let r = std::env::var("MILIONAR_DATA").ok();
    if r.is_none() {
        println!("skipped: MILIONAR_DATA not set");
    }
    r
}

fn script(name: &str) -> Script {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/replays")
        .join(format!("{name}.json"));
    Script::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

struct Run {
    result: Result<(), Abort>,
    debug: DebugState,
    game: Game,
    /// Debug state seen before every present.
    seen: Vec<DebugState>,
    hashes: Vec<u64>,
    audio: Vec<(u64, AudioCmd)>,
    ticks: u64,
    _saves: tempfile::TempDir,
}

/// Run `s` on the corpus; `real_audio` plays the shipped sounds (voice clips last their
/// real length) instead of one-tick silent clips.
fn run(root: &str, s: &Script, real_audio: bool) -> Run {
    let saves = tempfile::tempdir().unwrap();
    let mut game = Game::new(Assets::new(GameData::open(Path::new(root)).unwrap()))
        .with_saves_dir(saves.path().join("saves"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let source: Box<dyn SoundSource + Send> = if real_audio {
        Box::new(Assets::new(GameData::open(Path::new(root)).unwrap()))
    } else {
        Box::new(SilentSource)
    };
    let (mut host, _driver) = replay::host_observed(s, source, move |_, d| {
        let mut d = d.clone();
        d.screen_log.clear();
        s2.lock().unwrap().push(d);
    });
    let (result, debug) = replay::run(&mut host, &mut game, s.seed);
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    Run {
        result,
        debug,
        game,
        seen,
        hashes: host.frame_hashes().to_vec(),
        audio: host.audio_log().to_vec(),
        ticks: host.ticks(),
        _saves: saves,
    }
}

/// The silent-audio run of `name`, shared by the tests that need it (each script runs
/// once per test binary).
fn cached(root: &str, name: &'static str) -> &'static Run {
    static SINGLE: OnceLock<Run> = OnceLock::new();
    static FOUR: OnceLock<Run> = OnceLock::new();
    let cell = match name {
        "single_player_win" => &SINGLE,
        "four_player_hotseat" => &FOUR,
        _ => panic!("no cache for {name}"),
    };
    cell.get_or_init(|| run(root, &script(name), false))
}

/// Screen entries in order, from the per-present debug states.
fn entries(seen: &[DebugState]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for d in seen {
        if out.last() != Some(&d.screen) {
            out.push(d.screen);
        }
    }
    out
}

fn count(e: &[&str], screen: &str) -> usize {
    e.iter().filter(|s| **s == screen).count()
}

#[test]
fn full_single_player_game_to_win() {
    let Some(root) = root() else { return };
    let s = script("single_player_win");
    let t0 = Instant::now();
    let r = cached(&root, "single_player_win");
    println!(
        "single player: {} presents ({:.0} s of game time) in {:.2?}",
        r.ticks,
        r.ticks as f64 * 0.066,
        t0.elapsed()
    );
    // Ended by the script's stop on the play-again menu, not by the tick bound.
    assert_eq!(r.result, Err(Abort::Quit));
    assert!(r.ticks < s.quit_at);
    let e = entries(&r.seen);
    let rounds = count(&e, "BeatUpLoop");
    println!(
        "beat-up rounds: {rounds}, questions: {}",
        count(&e, "AnswerPhase")
    );
    assert!(rounds > 1);
    assert_eq!(count(&e, "SinglePlayerLoss"), 0);
    assert_eq!(count(&e, "BeatUpEnd"), 1);
    assert_eq!(count(&e, "Win"), 1);
    let win = e.iter().position(|s| *s == "Win").unwrap();
    let end = e.iter().position(|s| *s == "BeatUpEnd").unwrap();
    assert!(end < win);
    assert_eq!(e.last(), Some(&"PlayAgain"));
    // Every answer was right, the millionaire's money only went down, to 0.
    assert!(r.seen.iter().all(|d| d.money.iter().all(|&m| m >= 0)));
    let money: Vec<i32> = r.seen.iter().map(|d| d.money[0]).collect();
    assert!(money.windows(2).all(|w| w[1] <= w[0] || w[0] == 0));
    assert_eq!(r.debug.money[0], 0);
    assert_eq!(r.game.round.champion, Some(0));
    assert_eq!(r.game.round.result, 1);
    assert!(r.game.asset_errors.is_empty(), "{:?}", r.game.asset_errors);
}

#[test]
fn four_player_hotseat_game() {
    let Some(root) = root() else { return };
    let s = script("four_player_hotseat");
    let t0 = Instant::now();
    let r = cached(&root, "four_player_hotseat");
    let e = entries(&r.seen);
    println!(
        "four players: {} presents in {:.2?}; questions {}, beat-ups {}, victim picks {}, \
         pass the buck {}, KOs {}, win {}",
        r.ticks,
        t0.elapsed(),
        count(&e, "AnswerPhase"),
        count(&e, "BeatUpLoop"),
        count(&e, "PickVictim"),
        count(&e, "PickPassTarget"),
        count(&e, "BeatUpEnd"),
        count(&e, "Win"),
    );
    assert_eq!(r.result, Err(Abort::Quit));
    assert_eq!(r.game.player_count, 4);
    assert!(count(&e, "PickVictim") >= 1);
    assert!(count(&e, "PickPassTarget") >= 1);
    // The game ends with a champion before the tick bound (a stall would hit it).
    assert!(r.ticks < s.quit_at, "hit the tick bound");
    assert_eq!(count(&e, "Win"), 1);
    assert_eq!(count(&e, "BeatUpEnd"), 3);
    let champ = r.game.round.champion.unwrap();
    assert!(r.debug.money[champ] > 0);
    let alive = r.debug.money.iter().filter(|&&m| m > 0).count();
    assert_eq!(alive, 1, "{:?}", r.debug.money);
    assert_eq!(e.last(), Some(&"PlayAgain"));
    // Money never negative and never rises once the game runs; a player knocked out
    // stays out.
    // GameLoop itself never presents; its first screen is the info boxes.
    let game_start = r
        .seen
        .iter()
        .position(|d| d.screen == "InfoBoxesIn")
        .unwrap();
    let in_game = &r.seen[game_start..];
    for d in in_game {
        assert!(d.money.iter().all(|&m| m >= 0), "{:?}", d.money);
    }
    for w in in_game.windows(2) {
        for p in 0..4 {
            assert!(w[1].money[p] <= w[0].money[p], "player {p} money rose");
        }
    }
    // Eliminated players never act: whoever answers, beats or is beaten has money.
    let mut beatups = 0;
    for w in in_game.windows(2) {
        if w[1].screen == "BeatUpRound" && w[0].screen != "BeatUpRound" {
            beatups += 1;
            let d = &w[1];
            let (a, b) = (d.beater.unwrap(), d.beaten.unwrap());
            assert_ne!(a, b);
            assert!(d.money[a] > 0 && d.money[b] > 0, "{:?} {a} {b}", d.money);
            if let Some(x) = d.answerer {
                assert!(d.money[x] > 0);
            }
        }
    }
    assert!(beatups >= 1);
    // Every answer comes from a player with a millionaire and money (synced at Resolve).
    let mut answered = 0;
    for w in in_game.windows(2) {
        if w[1].screen == "Resolve"
            && w[0].screen != "Resolve"
            && let Some(x) = w[1].answerer
        {
            answered += 1;
            assert!(w[1].millionaire[x] >= 0, "{:?}", w[1].millionaire);
            assert!(w[1].money[x] > 0, "{:?} {x}", w[1].money);
        }
    }
    assert!(answered >= 1);
    assert!(r.game.asset_errors.is_empty(), "{:?}", r.game.asset_errors);
}

#[test]
fn determinism() {
    let Some(root) = root() else { return };
    let s = script("single_player_win");
    let t0 = Instant::now();
    let a = run(&root, &s, true);
    println!(
        "determinism: {} presents per run, first run {:.2?}",
        a.ticks,
        t0.elapsed()
    );
    let b = run(&root, &s, true);
    assert_eq!(a.ticks, b.ticks);
    assert_eq!(a.hashes, b.hashes);
    assert_eq!(a.audio, b.audio);
    assert!(a.hashes.len() > 1000);
    // The run with real voice lengths still reaches the play-again menu.
    assert_eq!(entries(&a.seen).last(), Some(&"PlayAgain"));
    assert!(a.ticks < s.quit_at);
}

/// `.smk` files the replays never draw, with the `transparent` flag of their load site
/// (`None`: the engine never loads the file). Checked against `upscale::blit_mode` like
/// the drawn ones; a file missing from both makes the test fail.
const NOT_REPLAYED: &[(&str, Option<bool>)] = &[
    // beatup/mod.rs: fist and millionaire sprites of SPRITE_FILES, loaded transparent.
    ("beat/fistm1a.smk", Some(true)),
    ("beat/fistm1c.smk", Some(true)),
    ("beat/fistm2b.smk", Some(true)),
    ("beat/fistm2c.smk", Some(true)),
    ("beat/fistm3b.smk", Some(true)),
    ("beat/fistm3c.smk", Some(true)),
    ("beat/fistm4a.smk", Some(true)),
    ("beat/fistm4b.smk", Some(true)),
    ("beat/fistm4c.smk", Some(true)),
    ("beat/fistm5a.smk", Some(true)),
    ("beat/fistm5b.smk", Some(true)),
    ("beat/mill_05.smk", Some(true)),
    // cutscene::play (pause.rs credits_then_quit, beatup End_0N KO): opaque.
    ("cuts/credits.smk", Some(false)),
    ("cuts/end_03.smk", Some(false)),
    ("cuts/end_05.smk", Some(false)),
    // keys.rs: help screens for 2 and 3 players, opaque.
    ("menus/keys2.smk", Some(false)),
    ("menus/keys3.smk", Some(false)),
    // lifelines.rs.
    ("menus/ll_5050.smk", Some(true)),
    ("menus/ll_chick.smk", Some(true)),
    ("menus/ll_m8b.smk", Some(true)),
    // pause.rs (LogoStill is reached through the play-again menu).
    ("menus/pause.smk", Some(true)),
    ("menus/pauseclk.smk", Some(true)),
    // Not referenced by the original (analysis/findings.md 6.1).
    ("menus/mm_dbut.smk", None),
    ("menus/mm_dpl.smk", None),
    ("quesansw/qs_abcs.smk", None),
];

/// The upscale pack stores one blit mode per file (`upscale::blit_mode`); the runtime
/// falls back on a mismatch, but every load site must agree with the rule or its art
/// never shows upscaled. Every (file, transparent) pair the engine used in both replays
/// is compared, and every other `.smk` of the import must be in [`NOT_REPLAYED`].
#[test]
fn blit_modes_match_upscale_rule() {
    let Some(root) = root() else { return };
    let mut used: BTreeMap<String, BTreeSet<bool>> = BTreeMap::new();
    for name in ["single_player_win", "four_player_hotseat"] {
        for (file, t) in cached(&root, name).game.assets.blit_uses() {
            used.entry(file.clone()).or_default().insert(*t);
        }
    }
    let rule = |file: &str| upscale::blit_mode(file) == upscale::BlitMode::Transparent;
    let mut bad = Vec::new();
    for (file, modes) in &used {
        if modes.len() != 1 || !modes.contains(&rule(file)) {
            bad.push(format!(
                "{file}: drawn transparent={modes:?}, rule {}",
                rule(file)
            ));
        }
    }
    let data = GameData::open(Path::new(&root)).unwrap();
    let all: BTreeSet<String> = data
        .files()
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("smk")))
        .map(|p| {
            let rel = p.strip_prefix(data.game_dir()).unwrap();
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            upscale::FrameKey::new(&parts.join("/"), 1).file
        })
        .collect();
    let listed: BTreeMap<&str, Option<bool>> = NOT_REPLAYED.iter().copied().collect();
    for file in &all {
        match (used.contains_key(file), listed.get(file.as_str())) {
            (true, Some(_)) => bad.push(format!("{file}: replayed now, drop it from NOT_REPLAYED")),
            (true, None) => {}
            (false, Some(Some(t))) if *t != rule(file) => bad.push(format!(
                "{file}: loaded transparent={t}, rule {}",
                rule(file)
            )),
            (false, Some(_)) => {}
            (false, None) => bad.push(format!(
                "{file}: not drawn by the replays; add it to NOT_REPLAYED with its load flag"
            )),
        }
    }
    for f in listed.keys() {
        if !all.contains(*f) {
            bad.push(format!("{f}: in NOT_REPLAYED but not in the import"));
        }
    }
    println!(
        "{} files drawn by the replays, {} listed",
        used.len(),
        listed.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
