//! Corpus tests: the question round (F1.4, F2, F4) driven by a headless script that
//! reacts to the engine's debug state.

use std::path::Path;
use std::sync::{Arc, Mutex};

use engine::game::saves::{read_sav, sav_path};
use engine::game::select::ORDER_LEN;
use engine::{
    Abort, Assets, Ctx, DebugState, DrawOp, Frame, Game, HeadlessHost, ScriptEvent, TextLayout, Vk,
};
use gamedata::GameData;

#[derive(Clone, Copy)]
enum Key {
    Fixed(char),
    Correct,
    Wrong,
}

/// Tap `key` once, `delay` presents after the `entry`-th entry into `screen`.
#[derive(Clone, Copy)]
struct Rule {
    screen: &'static str,
    entry: usize,
    delay: u64,
    key: Key,
}

const fn rule(screen: &'static str, entry: usize, delay: u64, key: Key) -> Rule {
    Rule {
        screen,
        entry,
        delay,
        key,
    }
}

#[derive(Default)]
struct Driver {
    rules: Vec<(Rule, bool)>,
    /// Quit once this screen has been entered.
    stop_at: &'static str,
    screen: &'static str,
    entered_at: u64,
    entries: Vec<(&'static str, usize)>,
    /// Keys the driver sent: (tick, key).
    sent: Vec<(u64, char)>,
}

impl Driver {
    fn step(&mut self, tick: u64, d: &DebugState) -> Vec<ScriptEvent> {
        if d.screen != self.screen {
            self.screen = d.screen;
            self.entered_at = tick;
            match self.entries.iter_mut().find(|(s, _)| *s == d.screen) {
                Some((_, n)) => *n += 1,
                None => self.entries.push((d.screen, 1)),
            }
        }
        if d.screen == self.stop_at {
            return vec![ScriptEvent::Quit];
        }
        let entry = self
            .entries
            .iter()
            .find(|(s, _)| *s == d.screen)
            .map_or(0, |e| e.1);
        let mut out = Vec::new();
        for (r, done) in &mut self.rules {
            if *done || r.screen != d.screen || r.entry != entry || tick < self.entered_at + r.delay
            {
                continue;
            }
            let c = match r.key {
                Key::Fixed(c) => c,
                Key::Correct => match d.correct_slot {
                    Some(s) => (b'A' + s as u8) as char,
                    None => continue,
                },
                Key::Wrong => match d.correct_slot {
                    Some(s) => (b'A' + ((s as u8 + 1) % 4)) as char,
                    None => continue,
                },
            };
            *done = true;
            self.sent.push((tick, c));
            let vk = if c == ' ' {
                Vk::SPACE
            } else {
                Vk::from_char(c).unwrap()
            };
            out.push(ScriptEvent::Tap(vk));
        }
        out
    }
}

struct Run {
    result: Result<(), Abort>,
    frames: Vec<Frame>,
    debug: DebugState,
    game: Game,
    sent: Vec<(u64, char)>,
    _saves: tempfile::TempDir,
}

fn corpus() -> Option<(Game, tempfile::TempDir)> {
    let Ok(root) = std::env::var("MILIONAR_DATA") else {
        println!("skipped: MILIONAR_DATA not set");
        return None;
    };
    let saves = tempfile::tempdir().unwrap();
    let game = Game::new(Assets::new(GameData::open(Path::new(&root)).unwrap()))
        .with_saves_dir(saves.path().join("saves"));
    Some((game, saves))
}

fn run(game: Game, saves: tempfile::TempDir, rules: &[Rule], stop_at: &'static str) -> Run {
    let mut game = game;
    let driver = Arc::new(Mutex::new(Driver {
        rules: rules.iter().map(|&r| (r, false)).collect(),
        stop_at,
        ..Driver::default()
    }));
    let d2 = driver.clone();
    let mut host = HeadlessHost::with_script(vec![(30_000, ScriptEvent::Quit)]);
    host.set_responder(Box::new(move |t, s| d2.lock().unwrap().step(t, s)));
    host.keep_frames();
    let (result, debug) = {
        let mut ctx = Ctx::new(&mut host, 7);
        let r = engine::run(&mut ctx, &mut game);
        (r, ctx.debug.clone())
    };
    let sent = driver.lock().unwrap().sent.clone();
    Run {
        result,
        frames: host.frames().to_vec(),
        debug,
        game,
        sent,
        _saves: saves,
    }
}

fn entered(debug: &DebugState, screen: &str) -> Vec<u64> {
    debug
        .screen_log
        .iter()
        .filter(|(_, s)| *s == screen)
        .map(|(t, _)| *t)
        .collect()
}

/// (file, frame) of every Smacker op in a frame.
fn smks(game: &Game, f: &Frame) -> Vec<(String, u32)> {
    f.ops
        .iter()
        .filter_map(|op| match op {
            DrawOp::Smk { asset, frame, .. } => Some((
                game.assets.asset_name(*asset).unwrap_or("?").to_string(),
                *frame,
            )),
            _ => None,
        })
        .collect()
}

fn texts(f: &Frame, layout: TextLayout) -> Vec<String> {
    f.ops
        .iter()
        .filter_map(|op| match op {
            DrawOp::Text {
                text, layout: l, ..
            } if *l == layout => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Menus for one player: '1', pick the first millionaire, Space on BeatKey.
const ONE_PLAYER: [Rule; 3] = [
    rule("PlayerCount", 1, 30, Key::Fixed('1')),
    rule("PickMillionaire", 1, 100, Key::Fixed('P')),
    rule("BeatKeys", 1, 45, Key::Fixed(' ')),
];

fn one_player(answer: Key) -> Vec<Rule> {
    let mut r = ONE_PLAYER.to_vec();
    r.push(rule("AnswerPhase", 1, 3, answer));
    r
}

fn assert_question_screens(r: &Run) {
    let q = &r.game.round.question;
    assert!(!q.text.is_empty());
    let with_q: Vec<&Frame> = r
        .frames
        .iter()
        .filter(|f| !texts(f, TextLayout::QuestionCentered).is_empty())
        .collect();
    assert!(!with_q.is_empty());
    let last = with_q.last().unwrap();
    assert_eq!(
        texts(last, TextLayout::QuestionCentered),
        std::slice::from_ref(&q.text)
    );
    let answers = texts(last, TextLayout::AnswerCentered);
    assert_eq!(answers.len(), 4);
    for (slot, a) in answers.iter().enumerate() {
        let want = format!(
            "{}{}",
            ["A. ", "B. ", "C. ", "D. "][slot],
            q.answers[q.perm[slot]]
        );
        assert_eq!(*a, want);
    }
    // The question studio and the info boxes were shown.
    let names: Vec<String> = r
        .frames
        .iter()
        .flat_map(|f| smks(&r.game, f))
        .map(|(n, _)| n)
        .collect();
    for n in [
        "QuesAnsw/Mm_Bg.Smk",
        "QuesAnsw/Qs_Bg.Smk",
        "QuesAnsw/Mm_Ib1.Smk",
        "QuesAnsw/Mm_Regis.Smk",
    ] {
        assert!(names.iter().any(|x| x == n), "{n} never drawn");
    }
}

#[test]
fn one_player_correct_answer_reaches_beatup() {
    let Some((game, saves)) = corpus() else {
        return;
    };
    let r = run(game, saves, &one_player(Key::Correct), "BeatUpRound");
    assert_eq!(r.result, Err(Abort::Quit));
    for s in [
        "GameLoop",
        "InfoBoxesIn",
        "HostIntro",
        "InfoBoxesOut",
        "QsBgIntro",
        "QuestionIntro",
        "QuestionRead",
        "AnswerPanel",
        "AnswerPhase",
        "Resolve",
        "BeatUpRound",
    ] {
        assert_eq!(entered(&r.debug, s).len(), 1, "{s}");
    }
    assert_eq!(entered(&r.debug, "Reveal").len(), 4);
    assert!(entered(&r.debug, "SinglePlayerLoss").is_empty());
    let round = &r.game.round;
    assert_eq!((round.q, round.result), (1, 1));
    assert_eq!((round.beater, round.beaten), (Some(0), Some(0)));
    assert_eq!(round.chosen, round.question.correct_slot as i32);
    let letter = r.sent.last().unwrap().1;
    assert_eq!(letter, (b'A' + round.question.correct_slot as u8) as char);
    assert_eq!(r.debug.money[0], 1_000_000);
    assert_question_screens(&r);
    // Easy question 1 was taken from the shuffled order and the cursor saved.
    let saves = r.game.saves_dir.clone().unwrap();
    let (cursor, order) = read_sav(&sav_path(&saves, 0), ORDER_LEN[0]).unwrap();
    assert_eq!(cursor, 1);
    assert_eq!(r.game.session.select.orders[0], order);
}

#[test]
fn one_player_wrong_answer_ends() {
    let Some((game, saves)) = corpus() else {
        return;
    };
    let r = run(game, saves, &one_player(Key::Wrong), "PlayAgain");
    assert_eq!(r.result, Err(Abort::Quit));
    let round = &r.game.round;
    assert_eq!(round.result, 0);
    assert_eq!((round.beater, round.beaten), (Some(0), Some(0)));
    assert_ne!(round.chosen, round.question.correct_slot as i32);
    let loss = entered(&r.debug, "SinglePlayerLoss");
    let again = entered(&r.debug, "PlayAgain");
    assert_eq!((loss.len(), again.len()), (1, 1));
    assert!(loss[0] < again[0]);
    assert!(entered(&r.debug, "BeatUpRound").is_empty());
    assert_question_screens(&r);
}

#[test]
fn one_player_timeout_ends() {
    let Some((game, saves)) = corpus() else {
        return;
    };
    let r = run(game, saves, &ONE_PLAYER, "PlayAgain");
    assert_eq!(r.result, Err(Abort::Quit));
    assert_eq!(r.game.round.result, 2);
    assert_eq!(r.game.round.answer_timer, 0);
    assert_eq!(entered(&r.debug, "SinglePlayerLoss").len(), 1);
}

#[test]
fn one_player_fifty_fifty_then_correct() {
    let Some((game, saves)) = corpus() else {
        return;
    };
    let mut rules = ONE_PLAYER.to_vec();
    rules.push(rule("AnswerPhase", 1, 3, Key::Fixed('N')));
    rules.push(rule("LifelineMenu", 1, 10, Key::Fixed('S')));
    rules.push(rule("AnswerPhase", 2, 3, Key::Correct));
    let r = run(game, saves, &rules, "BeatUpRound");
    assert_eq!(r.result, Err(Abort::Quit));
    assert_eq!(entered(&r.debug, "Lifeline5050").len(), 1);
    let round = &r.game.round;
    assert_eq!((round.lifeline, round.result), (600, 1));
    assert!(round.fifty_used);
    let hidden = round.question.states.iter().filter(|&&s| s == -1).count();
    assert_eq!(hidden, 2);
    assert_eq!(round.question.states[round.question.correct_slot], 1);
    let p = r.game.players[0];
    assert!(!p.fifty_fifty && p.magic_8_ball && p.chicken);
    assert_eq!(p.lifelines, 2);
    // The menu (Qs_Ll) and the 50:50 overlay were shown: the intro is cut short when the
    // (silent) voice ends, then hold 13, hold 15 and 15..19.
    let ll: Vec<u32> = r
        .frames
        .iter()
        .flat_map(|f| smks(&r.game, f))
        .filter(|(n, _)| n == "Menus/Ll_5050.smk")
        .map(|(_, f)| f)
        .collect();
    for f in [1, 13, 15, 16, 19] {
        assert!(ll.contains(&f), "Ll_5050 frame {f}");
    }
    assert!(
        r.frames
            .iter()
            .any(|f| { smks(&r.game, f).iter().any(|(n, _)| n == "Menus/Qs_Ll.smk") })
    );
}

#[test]
fn one_player_chicken_asks_a_new_question() {
    let Some((game, saves)) = corpus() else {
        return;
    };
    let mut rules = ONE_PLAYER.to_vec();
    rules.push(rule("AnswerPhase", 1, 3, Key::Fixed('N')));
    rules.push(rule("LifelineMenu", 1, 10, Key::Fixed('V')));
    rules.push(rule("AnswerPhase", 2, 3, Key::Correct));
    let r = run(game, saves, &rules, "BeatUpRound");
    assert_eq!(r.result, Err(Abort::Quit));
    // No new intro and no q++ for the replacement question.
    assert_eq!(entered(&r.debug, "HostIntro").len(), 1);
    assert_eq!(entered(&r.debug, "QuestionRead").len(), 2);
    assert_eq!(r.game.round.q, 1);
    assert!(!r.game.players[0].chicken);
    assert_eq!(r.game.round.result, 1);
    assert_eq!(r.game.session.select.cursors[0], 2);
}

#[test]
fn three_players_buzz_answer_and_pick_victim() {
    let Some((game, saves)) = corpus() else {
        return;
    };
    let rules = [
        rule("PlayerCount", 1, 30, Key::Fixed('3')),
        rule("PickMillionaire", 1, 100, Key::Fixed('P')),
        rule("PickMillionaire", 1, 220, Key::Fixed('P')),
        rule("PickMillionaire", 1, 340, Key::Fixed('P')),
        rule("Keys", 1, 90, Key::Fixed(' ')),
        rule("BeatKeys", 1, 45, Key::Fixed(' ')),
        // Player 2 of 3 buzzes with V while the answer panel opens.
        rule("AnswerPanel", 1, 2, Key::Fixed('V')),
        rule("AnswerPhase", 1, 3, Key::Correct),
        rule("PickVictim", 1, 6, Key::Fixed('3')),
    ];
    let r = run(game, saves, &rules, "BeatUpRound");
    assert_eq!(r.result, Err(Abort::Quit));
    assert_eq!(r.game.player_count, 3);
    let m: Vec<i32> = r.game.players[..3].iter().map(|p| p.millionaire).collect();
    assert!(m.iter().all(|&x| x >= 0));
    let round = &r.game.round;
    assert_eq!(round.answerer, Some(1));
    assert!(round.answer_timer < 164);
    assert_eq!(round.result, 1);
    assert_eq!(round.opponent, Some(2));
    assert_eq!((round.beater, round.beaten), (Some(1), Some(2)));
    assert_eq!(entered(&r.debug, "PickVictim").len(), 1);
    // The current-player box of player 2 (Qs_PnBox intro 10..15) was shown.
    let pn: Vec<u32> = r
        .frames
        .iter()
        .flat_map(|f| smks(&r.game, f))
        .filter(|(n, _)| n == "QuesAnsw/Qs_PnBox.Smk")
        .map(|(_, f)| f)
        .collect();
    assert!(pn.contains(&10) && pn.contains(&17), "{pn:?}");
}

#[test]
fn one_player_with_real_audio_reaches_beatup() {
    let Some((mut game, saves)) = corpus() else {
        return;
    };
    let root = std::env::var("MILIONAR_DATA").unwrap();
    let source = Assets::new(GameData::open(Path::new(&root)).unwrap());
    let driver = Arc::new(Mutex::new(Driver {
        rules: one_player(Key::Correct)
            .into_iter()
            .map(|r| (r, false))
            .collect(),
        stop_at: "BeatUpRound",
        ..Driver::default()
    }));
    let d2 = driver.clone();
    let mut host = HeadlessHost::new(vec![(60_000, ScriptEvent::Quit)], Box::new(source));
    host.set_responder(Box::new(move |t, s| d2.lock().unwrap().step(t, s)));
    host.keep_frames();
    let (result, debug) = {
        let mut ctx = Ctx::new(&mut host, 11);
        let r = engine::run(&mut ctx, &mut game);
        (r, ctx.debug.clone())
    };
    assert_eq!(result, Err(Abort::Quit));
    assert_eq!(game.round.result, 1);
    let read = entered(&debug, "QuestionRead")[0];
    let panel = entered(&debug, "AnswerPanel")[0];
    // The question voice takes seconds, not a tick.
    assert!(
        panel - read > 15,
        "question read lasted {} ticks",
        panel - read
    );
    // The host gestured while talking (frames other than the idle 13).
    let regis: Vec<u32> = host
        .frames()
        .iter()
        .flat_map(|f| smks(&game, f))
        .filter(|(n, _)| n == "QuesAnsw/Mm_Regis.Smk")
        .map(|(_, f)| f)
        .collect();
    assert!(regis.iter().any(|&f| f != 13), "host never moved");
    let _ = saves;
}

#[test]
fn one_player_magic_8_ball_then_correct() {
    let Some((game, saves)) = corpus() else {
        return;
    };
    let mut rules = ONE_PLAYER.to_vec();
    rules.push(rule("AnswerPhase", 1, 3, Key::Fixed('N')));
    rules.push(rule("LifelineMenu", 1, 10, Key::Fixed('K')));
    // Ask the 8-ball about the correct answer, then answer it.
    rules.push(rule("LifelineMagic8Ball", 1, 8, Key::Correct));
    rules.push(rule("AnswerPhase", 2, 3, Key::Correct));
    let r = run(game, saves, &rules, "BeatUpRound");
    assert_eq!(r.result, Err(Abort::Quit));
    let round = &r.game.round;
    assert_eq!((round.lifeline, round.result), (602, 1));
    assert_eq!(round.asked, round.question.correct_slot as i32);
    assert!(!round.eight_ball);
    let p = r.game.players[0];
    assert!(p.fifty_fifty && !p.magic_8_ball && p.chicken);
    assert_eq!(p.lifelines, 2);
    // Verdict frame 17 (the asked answer is correct) was shown, 19 was not.
    let m8: Vec<u32> = r
        .frames
        .iter()
        .flat_map(|f| smks(&r.game, f))
        .filter(|(n, _)| n == "Menus/Ll_M8b.smk")
        .map(|(_, f)| f)
        .collect();
    assert!(m8.contains(&13) && m8.contains(&17) && m8.contains(&23));
    assert!(!m8.contains(&19));
}

#[test]
fn two_players_pass_the_buck() {
    let Some((game, saves)) = corpus() else {
        return;
    };
    let rules = [
        rule("PlayerCount", 1, 30, Key::Fixed('2')),
        rule("PickMillionaire", 1, 100, Key::Fixed('P')),
        rule("PickMillionaire", 1, 220, Key::Fixed('P')),
        rule("Keys", 1, 90, Key::Fixed(' ')),
        rule("BeatKeys", 1, 45, Key::Fixed(' ')),
        rule("AnswerPanel", 1, 2, Key::Fixed('Q')),
        rule("AnswerPhase", 1, 3, Key::Fixed('N')),
        rule("LifelineMenu", 1, 10, Key::Fixed('P')),
        rule("PickPassTarget", 1, 5, Key::Fixed('2')),
        rule("AnswerPhase", 2, 3, Key::Correct),
    ];
    let r = run(game, saves, &rules, "BeatUpRound");
    assert_eq!(r.result, Err(Abort::Quit));
    let round = &r.game.round;
    assert_eq!(round.lifeline, 601);
    assert_eq!(round.answerer, Some(1));
    assert_eq!(round.result, 1);
    assert_eq!((round.beater, round.beaten), (Some(1), Some(0)));
    assert!(!r.game.players[0].pass_the_buck);
    assert!(r.game.players[1].pass_the_buck);
    assert_eq!(r.game.players[0].lifelines, 2);
    assert_eq!(entered(&r.debug, "PickPassTarget").len(), 1);
}

#[test]
fn one_player_beatup_returns_to_the_next_question() {
    let Some((game, saves)) = corpus() else {
        return;
    };
    // Correct answer, a beat-up without punches, then the next question times out.
    let r = run(game, saves, &one_player(Key::Correct), "PlayAgain");
    assert_eq!(r.result, Err(Abort::Quit));
    for (s, n) in [
        ("BeatUpRound", 1),
        ("BeatUpLoop", 1),
        ("InfoBoxesIn", 2),
        ("SinglePlayerLoss", 1),
    ] {
        assert_eq!(entered(&r.debug, s).len(), n, "{s}");
    }
    assert!(entered(&r.debug, "BeatUpEnd").is_empty());
    assert_eq!((r.game.round.q, r.game.round.result), (2, 2));
    assert_eq!(r.game.players[0].money, 1_000_000);
    assert_eq!(r.game.remaining_opponents, 0);
}
