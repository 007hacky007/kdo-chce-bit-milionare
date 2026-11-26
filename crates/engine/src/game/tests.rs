//! Unit tests of the question round (synthetic data only; no game text).

use std::path::Path;
use std::sync::Arc;

use formats::questions::{Question as DbQuestion, encode_question_db};
use formats::wav::Pcm;
use gamedata::GameData;

use super::lifelines::fifty_fifty_states;
use super::phases::buzz_in;
use super::question::Question;
use super::saves::{read_sav, sav_path};
use super::select::{EASY, HARD, MEDIUM, ORDER_LEN, Selector, level_for, pick_question};
use crate::assets::Assets;
use crate::audio_types::{AudioCmd, Channel, Prefix, SoundId};
use crate::ctx::Ctx;
use crate::headless::{HeadlessHost, ScriptEvent};
use crate::input::Vk;
use crate::rng::Lcg;
use crate::state::{Game, Player};

/// Invented records: record n has text "q{n}" and voice id `voice(n)`.
fn write_db(dir: &Path, file: &str, n: usize, voice: impl Fn(usize) -> u16) {
    let qs: Vec<DbQuestion> = (1..=n)
        .map(|i| DbQuestion {
            id: i as u32,
            text: format!("q{i}"),
            correct: format!("c{i}"),
            wrong: [format!("w{i}a"), format!("w{i}b"), format!("w{i}c")],
            voice_id: voice(i),
        })
        .collect();
    let db = dir.join("game").join("Db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join(file), encode_question_db(&qs)).unwrap();
}

/// A data dir with all three DBs (one record more than the order arrays, as shipped).
fn data_dir(voice: impl Fn(usize) -> u16 + Copy) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (i, f) in ["Mill_1.Dat", "Mill_2.Dat", "Mill_3.Dat"]
        .iter()
        .enumerate()
    {
        write_db(dir.path(), f, ORDER_LEN[i] + 1, voice);
    }
    dir
}

fn record_no(q: &Question) -> usize {
    q.text.strip_prefix('q').unwrap().parse().unwrap()
}

#[test]
fn level_thresholds() {
    assert_eq!(level_for(1, 2), EASY);
    assert_eq!(level_for(1, 3), MEDIUM);
    assert_eq!(level_for(1, 6), MEDIUM);
    assert_eq!(level_for(1, 7), HARD);
    for players in 2..=4 {
        assert_eq!(level_for(players, 4), EASY);
        assert_eq!(level_for(players, 5), MEDIUM);
        assert_eq!(level_for(players, 12), MEDIUM);
        assert_eq!(level_for(players, 13), HARD);
    }
}

/// The orders `PickQuestion` builds on its first call: every difficulty without a
/// `.sav` (`have[i]` false) is shuffled, easy first.
fn reference_orders(rng: &mut Lcg, have: [bool; 3]) -> [Vec<i32>; 3] {
    std::array::from_fn(|i| {
        let n = ORDER_LEN[i];
        let mut order: Vec<i32> = (1..=n as i32).collect();
        if !have[i] {
            rng.shuffle_draw(&mut order, n, None);
        }
        order
    })
}

#[test]
fn selection_order_and_sav() {
    let dir = data_dir(|n| n as u16);
    let data = GameData::open(dir.path()).unwrap();
    let mut errors = Vec::new();
    for (li, level) in [EASY, MEDIUM, HARD].into_iter().enumerate() {
        let saves_dir = tempfile::tempdir().unwrap();
        let saves = saves_dir.path().join("saves");
        let n = ORDER_LEN[li];
        let seed = 77 + li as u32;
        let mut rng = Lcg::new(seed);
        let mut sel = Selector::new();
        // Reference: no .sav, so all three orders are ShuffleDraw(1..N), then the answers.
        let mut want_rng = Lcg::new(seed);
        let orders = reference_orders(&mut want_rng, [false; 3]);
        let order = orders[li].clone();
        let (perm, _, correct) = want_rng.shuffle_with_inverse(4);

        let q = pick_question(&mut rng, &mut sel, &data, Some(&saves), level, &mut errors);
        assert_eq!(record_no(&q) as i32, order[0]);
        assert_eq!(q.correct_slot, correct);
        assert_eq!(q.perm.to_vec(), perm);
        assert_eq!(q.states, [1; 4]);
        assert_eq!(rng.next(), want_rng.next(), "same RNG consumption");
        let path = sav_path(&saves, li);
        assert_eq!(read_sav(&path, n), Some((1, order.clone())));
        for other in (0..3).filter(|&o| o != li) {
            assert!(!sav_path(&saves, other).exists());
        }
        // A fresh selector (program restart) continues from the .sav without drawing
        // for this level; the other two are shuffled again.
        let mut sel2 = Selector::new();
        let mut rng2 = Lcg::new(5);
        let mut probe = Lcg::new(5);
        let mut have = [false; 3];
        have[li] = true;
        reference_orders(&mut probe, have);
        probe.shuffle_with_inverse(4);
        let q2 = pick_question(
            &mut rng2,
            &mut sel2,
            &data,
            Some(&saves),
            level,
            &mut errors,
        );
        assert_eq!(record_no(&q2) as i32, order[1]);
        assert_eq!(
            rng2.next(),
            probe.next(),
            "only the other orders and answers drew"
        );
        assert_eq!(read_sav(&path, n), Some((2, order.clone())));
        // Take the rest: at N the .sav is deleted, the next use reshuffles.
        for &want in &order[2..] {
            let q = pick_question(
                &mut rng2,
                &mut sel2,
                &data,
                Some(&saves),
                level,
                &mut errors,
            );
            assert_eq!(record_no(&q) as i32, want);
        }
        assert!(!path.exists());
        assert_eq!(sel2.cursors[li], -1);
        let mut before = rng2.clone();
        let q = pick_question(
            &mut rng2,
            &mut sel2,
            &data,
            Some(&saves),
            level,
            &mut errors,
        );
        let mut reorder: Vec<i32> = (1..=n as i32).collect();
        before.shuffle_draw(&mut reorder, n, None);
        assert_eq!(record_no(&q) as i32, reorder[0]);
        assert_eq!(read_sav(&path, n), Some((1, reorder)));
        // The last record of each DB is never reachable (F11.1 KEEP).
        assert!(order.iter().all(|&r| r as usize <= n));
    }
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn repeat_voice_avoided() {
    // Records 1 and 2 share a voice id: after 1, record 2 is skipped (still consumed).
    let dir = data_dir(|n| if n == 2 { 1 } else { n as u16 });
    let data = GameData::open(dir.path()).unwrap();
    let saves = dir.path().join("saves");
    let n = ORDER_LEN[0];
    let order: Vec<i32> = (1..=n as i32).collect();
    std::fs::create_dir_all(&saves).unwrap();
    let mut bytes = 0i32.to_le_bytes().to_vec();
    for id in &order {
        bytes.extend(id.to_le_bytes());
    }
    std::fs::write(sav_path(&saves, 0), bytes).unwrap();
    let mut rng = Lcg::new(1);
    let mut sel = Selector::new();
    let mut errors = Vec::new();
    let q1 = pick_question(&mut rng, &mut sel, &data, Some(&saves), EASY, &mut errors);
    let q2 = pick_question(&mut rng, &mut sel, &data, Some(&saves), EASY, &mut errors);
    assert_eq!((record_no(&q1), record_no(&q2)), (1, 3));
    assert_eq!(read_sav(&sav_path(&saves, 0), n), Some((3, order)));
}

#[test]
fn truncated_sav_reshuffles() {
    let dir = data_dir(|n| n as u16);
    let data = GameData::open(dir.path()).unwrap();
    let saves = dir.path().join("saves");
    std::fs::create_dir_all(&saves).unwrap();
    let path = sav_path(&saves, 0);
    std::fs::write(&path, [1u8; 10]).unwrap();
    let n = ORDER_LEN[0];
    let mut rng = Lcg::new(9);
    let mut want = Lcg::new(9);
    let order = reference_orders(&mut want, [false; 3])[0].clone();
    let mut sel = Selector::new();
    let mut errors = Vec::new();
    let q = pick_question(&mut rng, &mut sel, &data, Some(&saves), EASY, &mut errors);
    assert_eq!(record_no(&q) as i32, order[0], "fresh shuffle");
    assert_eq!(read_sav(&path, n), Some((1, order)), "valid file rewritten");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn fifty_fifty_distribution() {
    let mut rng = Lcg::new(2024);
    let trials = 100_000;
    let mut kept = [0usize; 4];
    for _ in 0..trials {
        let mut q = Question::synthetic([3, 0, 2, 1]);
        let orig = fifty_fifty_states(&mut rng, &mut q);
        kept[orig] += 1;
        // Exactly the correct slot and the kept wrong one stay visible.
        let shown: Vec<usize> = (0..4).filter(|&s| q.states[s] == 1).collect();
        assert_eq!(shown.len(), 2);
        assert!(shown.contains(&q.correct_slot));
        assert!(shown.contains(&q.inv[orig]));
        assert!((0..4).all(|s| q.states[s] == 1 || q.states[s] == -1));
    }
    assert_eq!(kept[0], 0);
    let p1 = kept[1] as f64 / trials as f64;
    assert!((p1 - 0.8333).abs() < 0.005, "p(answer 1) = {p1}");
}

fn one_second(id: SoundId) -> Option<Arc<Pcm>> {
    (id == SoundId::with_suffix(Prefix::Char, 42, 'q')).then(|| {
        Arc::new(Pcm {
            sample_rate: 22_050,
            channels: 1,
            bits_per_sample: 16,
            samples: vec![100; 22_050],
        })
    })
}

#[test]
fn answer_timer_89_vs_164() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("game")).unwrap();
    let mut game = Game::new(Assets::new(GameData::open(dir.path()).unwrap()));
    game.player_count = 2;
    for (i, p) in game.players.iter_mut().take(2).enumerate() {
        *p = Player {
            millionaire: i as i32,
            money: 1_000_000,
            ..Player::EMPTY
        };
    }
    game.round.question.voice_id = 42;
    let script = vec![(0, ScriptEvent::Tap(Vk::Q)), (1, ScriptEvent::Tap(Vk::P))];
    let mut host = HeadlessHost::new(script, Box::new(one_second));
    let mut ctx = Ctx::new(&mut host, 1);
    // The buzz lands while CHAR0042q is still playing: 89 ticks, event 0x3d.
    ctx.audio(AudioCmd::Play {
        id: SoundId::with_suffix(Prefix::Char, 42, 'q'),
        channel: Channel::Voice,
        looped: false,
    });
    ctx.present().unwrap();
    ctx.pump();
    buzz_in(&mut ctx, &mut game);
    assert_eq!(game.round.answerer, Some(0));
    assert_eq!(game.round.answer_timer, 89);
    // No buzz while someone is answering.
    ctx.present().unwrap();
    ctx.pump();
    buzz_in(&mut ctx, &mut game);
    assert_eq!(game.round.answerer, Some(0));
    // After the read: 164 ticks, event 0x3e.
    game.round.answerer = None;
    ctx.audio(AudioCmd::StopVoice);
    let script_p = ctx.last_key();
    assert_eq!(script_p, Some(Vk::P));
    buzz_in(&mut ctx, &mut game);
    assert_eq!(game.round.answerer, Some(1));
    assert_eq!(game.round.answer_timer, 164);
    assert_eq!(game.round.timeouts, 0);
    // A knocked-out player cannot buzz.
    game.round.answerer = None;
    game.players[1].money = 0;
    buzz_in(&mut ctx, &mut game);
    assert_eq!(game.round.answerer, None);
}

#[test]
fn degenerate_dbs_do_not_panic() {
    // Empty DBs (missing files), a short DB and a DB where every voice id is equal.
    let empty = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(empty.path().join("game")).unwrap();
    let short = tempfile::tempdir().unwrap();
    for f in ["Mill_1.Dat", "Mill_2.Dat", "Mill_3.Dat"] {
        write_db(short.path(), f, 3, |n| n as u16);
    }
    let same = data_dir(|_| 9);
    for (dir, saves) in [
        (&empty, true),
        (&short, true),
        (&same, true),
        (&same, false),
    ] {
        let data = GameData::open(dir.path()).unwrap();
        let saves_dir = dir.path().join("saves");
        let saves = saves.then_some(saves_dir.as_path());
        let mut rng = Lcg::new(3);
        let mut sel = Selector::new();
        let mut errors = Vec::new();
        for level in [EASY, EASY, MEDIUM, HARD, HARD] {
            pick_question(&mut rng, &mut sel, &data, saves, level, &mut errors);
            for li in 0..3 {
                let c = sel.cursors[li];
                assert!(c == -1 || (0..ORDER_LEN[li] as i32).contains(&c));
            }
        }
    }
    // A missing DB consumes one order entry per pick, not the whole order.
    let data = GameData::open(empty.path()).unwrap();
    let mut sel = Selector::new();
    let mut errors = Vec::new();
    let saves = empty.path().join("saves2");
    let mut rng = Lcg::new(4);
    pick_question(&mut rng, &mut sel, &data, Some(&saves), EASY, &mut errors);
    pick_question(&mut rng, &mut sel, &data, Some(&saves), EASY, &mut errors);
    assert_eq!(sel.cursors[0], 2);
    assert!(!errors.is_empty(), "the missing DB is reported");
}

#[test]
fn timeout_beater_skips_eliminated_players() {
    // F11.3: players 1 (money 0) and 3 (no millionaire) are never drawn; player 0 answers.
    for seed in 1..40 {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("game")).unwrap();
        let mut game = Game::new(Assets::new(GameData::open(dir.path()).unwrap()));
        game.player_count = 4;
        for (i, p) in game.players.iter_mut().take(4).enumerate() {
            *p = Player {
                millionaire: if i == 3 { -1 } else { i as i32 },
                money: if i == 1 { 0 } else { 1_000_000 },
                ..Player::EMPTY
            };
        }
        game.round.answerer = Some(0);
        game.round.chosen = -1;
        game.round.answer_timer = 1;
        let mut host = HeadlessHost::new(Vec::new(), Box::new(|_| None));
        let mut ctx = Ctx::new(&mut host, seed);
        let alive = super::phases::answer_timer_tick(&mut ctx, &mut game).unwrap();
        assert!(!alive);
        assert_eq!(game.round.random_beater, Some(2), "seed {seed}");
    }
}
