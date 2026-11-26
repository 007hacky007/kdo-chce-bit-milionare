//! Corpus tests: the menu flow (F1.4) driven by a headless script.

use std::path::Path;
use std::sync::{Arc, Mutex};

use engine::screens::{pause, player_count};
use engine::{
    Abort, Assets, AudioCmd, Channel, Ctx, DebugState, DrawOp, Frame, Game, HeadlessHost, Pacing,
    Responder, ScriptEvent, Vk,
};
use gamedata::GameData;

const LOGOS: u64 = 158;
const OPENING: u64 = 251;
const MM_OPEN: u64 = 98;
const CREDITS: u64 = 148;
/// Present index of the first player-count idle frame.
const NP_IDLE_AT: u64 = LOGOS + OPENING + 25;

struct Run {
    result: Result<(), Abort>,
    frames: Vec<Frame>,
    pacings: Vec<Pacing>,
    audio: Vec<(u64, AudioCmd)>,
    debug: DebugState,
    game: Game,
}

fn corpus() -> Option<Game> {
    let Ok(root) = std::env::var("MILIONAR_DATA") else {
        println!("skipped: MILIONAR_DATA not set");
        return None;
    };
    Some(Game::new(Assets::new(
        GameData::open(Path::new(&root)).unwrap(),
    )))
}

fn run(game: Game, script: Vec<(u64, ScriptEvent)>) -> Run {
    run_with(game, script, None)
}

fn run_with(mut game: Game, script: Vec<(u64, ScriptEvent)>, responder: Option<Responder>) -> Run {
    let mut host = HeadlessHost::with_script(script);
    if let Some(r) = responder {
        host.set_responder(r);
    }
    host.keep_frames();
    let (result, debug) = {
        let mut ctx = Ctx::new(&mut host, 1);
        let r = engine::run(&mut ctx, &mut game);
        (r, ctx.debug.clone())
    };
    Run {
        result,
        frames: host.frames().to_vec(),
        pacings: host.pacings().to_vec(),
        audio: host.audio_log().to_vec(),
        debug,
        game,
    }
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

/// (file, frame) of the first op of each frame (the background).
fn backgrounds(r: &Run) -> Vec<(String, u32)> {
    r.frames
        .iter()
        .map(|f| {
            smks(&r.game, f)
                .into_iter()
                .next()
                .unwrap_or(("-".to_string(), 0))
        })
        .collect()
}

fn range(name: &str, from: u32, to: u32) -> Vec<(String, u32)> {
    (from..=to).map(|n| (name.to_string(), n)).collect()
}

fn hold(name: &str, frame: u32, n: usize) -> Vec<(String, u32)> {
    vec![(name.to_string(), frame); n]
}

/// Index of the first frame at or after `from` whose background is `(name, frame)`.
fn find(bg: &[(String, u32)], from: usize, name: &str, frame: u32) -> usize {
    (from..bg.len())
        .find(|&i| bg[i].0 == name && bg[i].1 == frame)
        .unwrap_or_else(|| panic!("{name} frame {frame} not shown after {from}"))
}

fn entered(debug: &DebugState, screen: &str) -> Vec<u64> {
    debug
        .screen_log
        .iter()
        .filter(|(_, s)| *s == screen)
        .map(|(t, _)| *t)
        .collect()
}

fn tap(t: u64, c: char) -> (u64, ScriptEvent) {
    (t, ScriptEvent::Tap(Vk::from_char(c).unwrap()))
}

#[test]
fn boot_to_player_count() {
    let Some(game) = corpus() else { return };
    let r = run(game, vec![(NP_IDLE_AT + 5, ScriptEvent::Quit)]);
    assert_eq!(r.result, Err(Abort::Quit));
    let bg = backgrounds(&r);
    let l = LOGOS as usize;
    let o = l + OPENING as usize;
    assert_eq!(bg[..l], range("Cuts/Logos.smk", 1, 158)[..]);
    assert_eq!(bg[l..o], range("Menus/Opening.smk", 1, 251)[..]);
    // Np intro 1..25 (F1.4 frame ranges), then the idle frame 27.
    assert_eq!(bg[o..o + 25], range("Menus/Np.smk", 1, 25)[..]);
    assert_eq!(bg[o + 25..], hold("Menus/Np.smk", 27, 6)[..]);
    // Cutscenes at their native 83.33 ms per frame on an absolute schedule.
    let logo_ms: u32 = r.pacings[..l].iter().map(|p| p.duration_ms()).sum();
    assert_eq!(logo_ms, 13_166);
    assert!(matches!(r.pacings[0], Pacing::Cutscene { min_ms: 83 }));
    assert_eq!(r.pacings[o], Pacing::Tick66);
    assert_eq!(entered(&r.debug, "PlayerCount"), [o as u64]);
    // Opening: music stopped first, MUSI0010 started after it.
    assert!(
        r.audio
            .iter()
            .any(|(t, c)| *t as usize == l && *c == AudioCmd::StopChannel(Channel::Music))
    );
}

#[test]
fn cutscene_skips_on_space_and_click() {
    let Some(game) = corpus() else { return };
    // Space during the 10th Logos frame, a click during the 3rd Opening frame.
    let r = run(
        game,
        vec![
            (9, ScriptEvent::Tap(Vk::SPACE)),
            (12, ScriptEvent::MouseDown(engine::MouseButton::Right)),
            (40, ScriptEvent::Quit),
        ],
    );
    let bg = backgrounds(&r);
    assert_eq!(bg[..10], range("Cuts/Logos.smk", 1, 10)[..]);
    assert_eq!(bg[10..13], range("Menus/Opening.smk", 1, 3)[..]);
    assert_eq!(bg[13], ("Menus/Np.smk".to_string(), 1));
}

#[test]
fn pick_one_player_and_start() {
    let Some(game) = corpus() else { return };
    let script = vec![
        tap(NP_IDLE_AT + 3, '1'),
        tap(560, 'D'),
        tap(570, 'I'),
        tap(580, 'Z'),
        tap(600, 'P'),
        (700, ScriptEvent::Tap(Vk::SPACE)),
        (1000, ScriptEvent::Quit),
    ];
    let r = run(game, script);
    assert_eq!(r.result, Err(Abort::Quit));
    let bg = backgrounds(&r);
    let np = "Menus/Np.smk";
    // Exit for one player: 29..38 then 74..82, right after the idle frames.
    let ex = find(&bg, 0, np, 29);
    assert_eq!(bg[ex - 1], (np.to_string(), 27));
    let mut want = range(np, 29, 38);
    want.extend(range(np, 74, 82));
    assert_eq!(bg[ex..ex + 19], want[..]);
    // Pick: Pm intro 1..49, then frame 51.
    let pm = "Menus/Pm.smk";
    let pi = ex + 19;
    assert_eq!(bg[pi..pi + 49], range(pm, 1, 49)[..]);
    assert_eq!(bg[pi + 49], (pm.to_string(), 51));
    // The portrait: first free millionaire (1), D moved to 2, the bio showed Bios 2.
    let frame_smks = |i: usize| smks(&r.game, &r.frames[i]);
    assert!(frame_smks(pi + 49).contains(&("Menus/Pm_Mill.smk".to_string(), 1)));
    let bio = find(&bg, pi, "Menus/Bios.smk", 2);
    assert!(frame_smks(bio - 1).contains(&("Menus/Pm_Mill.smk".to_string(), 2)));
    let back = find(&bg, bio, pm, 51);
    assert!(back > bio);
    // Player 1 has no Pm_Plyrs banner.
    assert!(r.frames[pi..back].iter().all(|f| {
        !smks(&r.game, f)
            .iter()
            .any(|(n, _)| n == "Menus/Pm_Plyrs.smk")
    }));
    let end = find(&bg, back, "Menus/Pm_End.smk", 1);
    assert_eq!(bg[end..end + 7], range("Menus/Pm_End.smk", 1, 7)[..]);
    // One player: no Keys screen; BeatKey 1..36, idle 36, Space, exit 37..80.
    let bk = "Menus/BeatKey.smk";
    let b = end + 7;
    assert_eq!(bg[b..b + 36], range(bk, 1, 36)[..]);
    let bx = find(&bg, b, bk, 37);
    assert!(bg[b + 36..bx].iter().all(|f| *f == (bk.to_string(), 36)));
    assert_eq!(bg[bx..bx + 44], range(bk, 37, 80)[..]);
    // The show intro, then GameLoop.
    let mo = bx + 44;
    assert_eq!(
        bg[mo..mo + MM_OPEN as usize],
        range("Cuts/MM_Open.smk", 1, 98)[..]
    );
    let gl = entered(&r.debug, "GameLoop");
    assert_eq!(gl, [(mo + MM_OPEN as usize) as u64]);
    assert!(entered(&r.debug, "Keys").is_empty());
    // Player record (F4.1, 1 player forced lifelines).
    let p = r.game.players[0];
    assert_eq!((p.millionaire, p.money, p.lifelines), (1, 1_000_000, 3));
    assert!(p.fifty_fifty && !p.pass_the_buck && p.magic_8_ball && p.chicken);
    assert_eq!(r.game.player_count, 1);
    // Then the question round starts with the info boxes.
    assert_eq!(entered(&r.debug, "InfoBoxesIn"), gl);
}

#[test]
fn pause_new_game_restarts() {
    let Some(game) = corpus() else { return };
    let esc = NP_IDLE_AT + 3;
    let script = vec![
        (esc, ScriptEvent::Tap(Vk::ESCAPE)),
        tap(esc + 30, 'N'),
        (esc + 60, ScriptEvent::Quit),
    ];
    let r = run(game, script);
    assert_eq!(r.result, Err(Abort::Quit));
    let bg = backgrounds(&r);
    // The ESC tap is pumped at the end of tick esc + 1 and seen after the next present.
    let pause_at = entered(&r.debug, "Pause")[0] as usize;
    assert_eq!(pause_at as u64, esc + 2);
    // Slide-in: 13 frames over the frozen Np frame, LogoStill bottom edge at SLIDE_Y.
    for (i, y) in pause::SLIDE_Y.iter().enumerate() {
        let f = &r.frames[pause_at + i];
        let s = smks(&r.game, f);
        assert_eq!(s[0], ("Menus/Np.smk".to_string(), 27));
        let logo = f
            .ops
            .iter()
            .rev()
            .find_map(|op| match op {
                DrawOp::Smk { x, y, .. } => Some((*x, *y)),
                _ => None,
            })
            .unwrap();
        assert_eq!(logo, (0, y - 479));
    }
    // Menu: Pause.smk 1..7 over LogoStill.
    let m = pause_at + 13;
    for i in 0..7 {
        let s = smks(&r.game, &r.frames[m + i]);
        assert_eq!(s[0], ("Menus/LogoStill.smk".to_string(), 1));
        assert_eq!(s[1], ("Menus/Pause.smk".to_string(), i as u32 + 1));
    }
    // N arrives with present esc + 30 and is pumped after it; the exit lasts 4 ticks
    // counting that one (3 more presents), then Opening starts again.
    let reopen = find(&bg, m, "Menus/Opening.smk", 1);
    assert_eq!(reopen, (esc + 30 + 4) as usize);
    assert_eq!(entered(&r.debug, "Opening").len(), 2);
    // Voice paused on entry, unpaused on the way out; music halved and restored.
    let cmds: Vec<&AudioCmd> = r.audio.iter().map(|(_, c)| c).collect();
    let paused = cmds
        .iter()
        .position(|c| **c == AudioCmd::PauseChannel(Channel::Voice, true))
        .unwrap();
    assert!(
        cmds[paused..]
            .iter()
            .any(|c| **c == AudioCmd::PauseChannel(Channel::Voice, false))
    );
}

#[test]
fn credits_then_quit() {
    let Some(game) = corpus() else { return };
    let esc = NP_IDLE_AT + 3;
    let script = vec![(esc, ScriptEvent::Tap(Vk::ESCAPE)), tap(esc + 30, 'K')];
    let r = run(game, script);
    // No Quit in the script: the credits end the program.
    assert_eq!(r.result, Err(Abort::Quit));
    let bg = backgrounds(&r);
    let c = bg.len() - CREDITS as usize;
    assert_eq!(bg[c..], range("Cuts/Credits.smk", 1, 148)[..]);
    assert_eq!(c, (esc + 30 + 4) as usize);
    assert_eq!(entered(&r.debug, "Credits"), [c as u64]);
    assert_eq!(r.game.pause_choice, Some(0x320));
    // Every screen released its assets.
    assert_eq!(r.game.assets.live_handles(), 0);
}

#[test]
fn pause_resume_returns_to_caller() {
    let Some(game) = corpus() else { return };
    let esc = NP_IDLE_AT + 3;
    let script = vec![
        (esc, ScriptEvent::Tap(Vk::ESCAPE)),
        tap(esc + 30, 'P'),
        (esc + 50, ScriptEvent::Quit),
    ];
    let r = run(game, script);
    let bg = backgrounds(&r);
    // 3 exit presents after the key's, one LogoStill tick, then the caller's next frame.
    let back = (esc + 30 + 4 + 1) as usize;
    // One tick of LogoStill alone, then the player-count idle frame again.
    let s = smks(&r.game, &r.frames[back - 1]);
    assert_eq!(s.len(), 1);
    assert_eq!(bg[back], ("Menus/Np.smk".to_string(), 27));
    assert_eq!(r.debug.screen, "PlayerCount");
    assert_eq!(r.game.pause_choice, Some(0x321));
}

#[test]
fn play_again_restarts_main_menu() {
    let Some(game) = corpus() else { return };
    // One player who never answers: the answer time runs out, the 1-player loss screen
    // plays, then the play-again menu; 'A' is tapped 40 ticks into it.
    let script = vec![
        tap(NP_IDLE_AT + 3, '1'),
        tap(600, 'P'),
        (700, ScriptEvent::Tap(Vk::SPACE)),
    ];
    let a_at = Arc::new(Mutex::new(None::<u64>));
    let a2 = a_at.clone();
    let mut seen = None;
    let responder: Responder = Box::new(move |t, d| {
        if d.screen == "PlayAgain" && seen.is_none() {
            seen = Some(t);
        }
        let Some(s) = seen else { return vec![] };
        if t == s + 40 {
            *a2.lock().unwrap() = Some(t);
            return vec![tap(0, 'A').1];
        }
        if t == s + 140 {
            return vec![ScriptEvent::Quit];
        }
        vec![]
    });
    let r = run_with(game, script, Some(responder));
    assert_eq!(r.result, Err(Abort::Quit));
    let a = a_at.lock().unwrap().unwrap() as usize;
    let pa = entered(&r.debug, "PlayAgain")[0] as usize;
    let bg = backgrounds(&r);
    // 13 slide-in ticks, MM_Pa 1..21 then 22.
    let m = pa + 13;
    for i in 0..21 {
        assert_eq!(smks(&r.game, &r.frames[m + i])[1].1, i as u32 + 1);
    }
    assert_eq!(smks(&r.game, &r.frames[m + 21])[1].1, 22);
    let again = find(&bg, m, "Menus/Opening.smk", 1);
    // 'A' arrives with present `a`; 3 exit presents follow (4 ticks with the key's),
    // then one tick of LogoStill alone.
    assert_eq!(again, a + 5);
    assert_eq!(smks(&r.game, &r.frames[a + 4]).len(), 1);
    // (MainMenuLoop resets the choice when it starts over.)
    assert_eq!(entered(&r.debug, "Opening").len(), 2);
    assert!(entered(&r.debug, "SinglePlayerLoss")[0] < pa as u64);
    // Players were reset for the new game.
    assert_eq!(r.game.players[0].millionaire, -1);
}

#[test]
fn player_count_screen_alone() {
    let Some(mut game) = corpus() else { return };
    let mut host = HeadlessHost::with_script(vec![tap(30, '3')]);
    let mut ctx = Ctx::new(&mut host, 1);
    player_count::run(&mut ctx, &mut game).unwrap();
    assert_eq!(game.player_count, 3);
    drop(ctx);
    // Intro 25; idle presents 25..=32: the key arrives with present 30 and is pumped
    // (button pressed), events 0x0e/0x42 go out before present 31, the pump after
    // present 32 finds the (silent, one-render) voice line over; exit 51..60 + 74..82.
    assert_eq!(host.ticks(), 25 + 8 + 10 + 9);
    assert_eq!(game.assets.live_handles(), 0);
}

#[test]
fn two_players_skip_taken_and_show_keys() {
    let Some(game) = corpus() else { return };
    let script = vec![
        tap(NP_IDLE_AT + 3, '2'),
        tap(530, 'P'),
        tap(600, 'P'),
        (700, ScriptEvent::Tap(Vk::SPACE)),
        (800, ScriptEvent::Tap(Vk::SPACE)),
        (1000, ScriptEvent::Quit),
    ];
    let r = run(game, script);
    assert_eq!(r.result, Err(Abort::Quit));
    let bg = backgrounds(&r);
    let np = "Menus/Np.smk";
    let ex = find(&bg, 0, np, 40);
    let mut want = range(np, 40, 49);
    want.extend(range(np, 74, 82));
    assert_eq!(bg[ex..ex + 19], want[..]);
    assert_eq!(r.game.player_count, 2);
    // Player 1 took millionaire 1; player 2's pass starts on 2 and shows the banner
    // 1..10 (frame 11 is never shown) then holds 13.
    assert_eq!(r.game.players[0].millionaire, 0);
    assert_eq!(r.game.players[1].millionaire, 1);
    assert!(r.game.players[1].pass_the_buck);
    let mill = "Menus/Pm_Mill.smk".to_string();
    let plyrs = "Menus/Pm_Plyrs.smk".to_string();
    let second = r
        .frames
        .iter()
        .position(|f| smks(&r.game, f).iter().any(|s| s.0 == plyrs))
        .unwrap();
    let banner: Vec<u32> = r.frames[second..second + 14]
        .iter()
        .map(|f| {
            smks(&r.game, f)
                .into_iter()
                .find(|s| s.0 == plyrs)
                .unwrap()
                .1
        })
        .collect();
    assert_eq!(banner, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 13, 13, 13]);
    assert!(smks(&r.game, &r.frames[second]).contains(&(mill, 2)));
    // Keys2: 1..62, idle 62, Space, 63..78; then BeatKey.
    let k = "Menus/Keys2.smk";
    let ks = find(&bg, 0, k, 1);
    assert_eq!(bg[ks..ks + 62], range(k, 1, 62)[..]);
    let kx = find(&bg, ks, k, 63);
    assert!(bg[ks + 62..kx].iter().all(|f| *f == (k.to_string(), 62)));
    assert_eq!(bg[kx..kx + 16], range(k, 63, 78)[..]);
    assert_eq!(bg[kx + 16], ("Menus/BeatKey.smk".to_string(), 1));
    assert_eq!(entered(&r.debug, "Keys"), [ks as u64]);
    assert_eq!(entered(&r.debug, "GameLoop").len(), 1);
}

#[test]
fn released_caller_overlay_stays_drawable_under_menu() {
    let Some(mut game) = corpus() else { return };
    let bg = game.load("QuesAnsw/Mm_Bg.smk", false);
    let id = bg.id.unwrap();
    let mut host = HeadlessHost::with_script(vec![]);
    let mut ctx = Ctx::new(&mut host, 1);
    bg.draw(&mut ctx, 1, 0, 0);
    ctx.present().unwrap();
    // The caller frees its overlays after presenting, before opening the menu.
    game.release(bg);
    let frame = ctx.last_presented().clone();
    let pins = pause::pin_background(&mut game, &frame);
    assert_eq!(pins, [id]);
    // The menu's own loads collect released assets; the pinned one survives.
    let logo = game.load("Menus/LogoStill.smk", false);
    assert!(game.assets.frame(id, 1).is_ok());
    pause::unpin(&mut game, pins);
    game.release(logo);
    let other = game.load("Menus/Pause.smk", true);
    assert!(game.assets.frame(id, 1).is_err());
    game.release(other);
}

#[test]
fn pause_idle_line_plays_and_paused_voice_resumes() {
    let Some(game) = corpus() else { return };
    let root = std::env::var("MILIONAR_DATA").unwrap();
    let sounds = Assets::new(GameData::open(Path::new(&root)).unwrap());
    let esc = NP_IDLE_AT + 3;
    let pause_at = esc + 2;
    let menu_at = pause_at + 13;
    let p_at = menu_at + 960;
    let script = vec![
        (esc, ScriptEvent::Tap(Vk::ESCAPE)),
        tap(p_at, 'P'),
        (p_at + 6, ScriptEvent::Quit),
    ];
    let mut host = HeadlessHost::new(script, Box::new(sounds));
    let mut game = game;
    {
        let mut ctx = Ctx::new(&mut host, 1);
        assert_eq!(engine::run(&mut ctx, &mut game), Err(Abort::Quit));
    }
    let log = host.audio_log().to_vec();
    let pause = log
        .iter()
        .position(|(_, c)| *c == AudioCmd::PauseChannel(Channel::Voice, true))
        .unwrap();
    assert_eq!(log[pause].0, pause_at);
    // The voice clip that was playing when the menu opened (Np's 0x41 line).
    let paused_clip = log[..pause].iter().rev().find_map(|(_, c)| match c {
        AudioCmd::Play {
            id,
            channel: Channel::Voice,
            ..
        } => Some(*id),
        _ => None,
    });
    // Event 0x4c on menu tick 899: a voice line played without StopVoice.
    let line = log[pause..]
        .iter()
        .find_map(|(t, c)| match c {
            AudioCmd::Play {
                id,
                channel: Channel::Voice,
                ..
            } => Some((*t, *id)),
            _ => None,
        })
        .unwrap();
    assert_eq!(line.0, menu_at + 898);
    let after_line = &log[pause..];
    assert!(
        !after_line
            .iter()
            .any(|(t, c)| *t < p_at && *c == AudioCmd::StopVoice)
    );
    // P stops that line and resumes the voice channel.
    let stop = after_line
        .iter()
        .position(|(_, c)| *c == AudioCmd::StopSound(line.1))
        .unwrap();
    assert!(
        after_line[stop..]
            .iter()
            .any(|(_, c)| *c == AudioCmd::PauseChannel(Channel::Voice, false))
    );
    // The suspended clip continues after P: it had time left, which the menu froze.
    let id = paused_clip.unwrap();
    let started = log[..pause]
        .iter()
        .rev()
        .find(|(_, c)| matches!(c, AudioCmd::Play { id: i, .. } if *i == id))
        .unwrap()
        .0;
    let played_ms = (pause_at - started) * 66;
    let pcm = Assets::new(GameData::open(Path::new(&root)).unwrap())
        .sound(id)
        .unwrap();
    let len_ms =
        pcm.samples.len() as u64 * 1000 / (u64::from(pcm.sample_rate) * u64::from(pcm.channels));
    assert!(
        len_ms > played_ms + 8 * 66,
        "{id}: {len_ms} ms, {played_ms} played"
    );
    assert!(engine::Host::sound_playing(&host, id), "{id} resumed");
}
