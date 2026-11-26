use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use gamedata::GameData;

use super::*;
use crate::assets::Assets;
use crate::audio_types::{AudioCmd, Channel, Prefix, SoundId};
use crate::headless::HeadlessHost;
use crate::host::{Host, Pacing, Quit};
use crate::input::InputState;
use render::Frame;

/// Records commands; nothing ever plays.
#[derive(Default)]
struct RecHost {
    log: Vec<AudioCmd>,
}

impl Host for RecHost {
    fn present(&mut self, _frame: Frame, _pacing: Pacing) -> Result<InputState, Quit> {
        Ok(InputState::default())
    }
    fn audio(&mut self, cmd: AudioCmd) {
        self.log.push(cmd);
    }
    fn voice_busy(&self) -> bool {
        false
    }
    fn voice_envelope(&self) -> u8 {
        0
    }
    fn sound_playing(&self, _id: SoundId) -> bool {
        false
    }
    fn flush_input(&mut self) {}
    fn pump_voice(&mut self) {}
    fn now_ms(&self) -> u64 {
        0
    }
}

fn advanced(rng: &Lcg, n: usize) -> Lcg {
    let mut r = rng.clone();
    for _ in 0..n {
        r.next();
    }
    r
}

fn shuffled(rng: &mut Lcg, items: &[i32]) -> Vec<u16> {
    let mut v = items.to_vec();
    rng.shuffle_swap1000(&mut v);
    v.into_iter().map(|x| x as u16).collect()
}

#[test]
fn first_draw_shuffles_like_the_original() {
    // Every pool starts with count = index = 0 in the exe, so the first draw computes the
    // count, finds the pool "exhausted" and shuffles before taking item 0.
    let mut pool = Pool::new(&[10, 20, 30]);
    let mut rng = Lcg::new(99);
    let mut reference = rng.clone();
    let order = shuffled(&mut reference, &[10, 20, 30]);
    assert_eq!(pool.draw(&mut rng), order[0]);
    assert_eq!(
        rng, reference,
        "first draw consumes exactly 2000 rand calls"
    );
}

#[test]
fn pool_wraps_with_shuffle() {
    let mut pool = Pool::new(&[10, 20, 30]);
    let mut rng = Lcg::new(12345);
    pool.draw(&mut rng); // initial shuffle, see above
    let order: Vec<u16> = pool.items[..3].iter().map(|&x| x as u16).collect();
    // The current order is emitted in order without touching the RNG ...
    let before = rng.clone();
    assert_eq!(pool.draw(&mut rng), order[1]);
    assert_eq!(pool.draw(&mut rng), order[2]);
    assert_eq!(rng, before);
    // ... then the wrap shuffles the pool with exactly 2000 rand calls and continues with a
    // permutation of the same ids.
    let mut reference = before.clone();
    let next = {
        let cur: Vec<i32> = order.iter().map(|&x| i32::from(x)).collect();
        shuffled(&mut reference, &cur)
    };
    let got = [
        pool.draw(&mut rng),
        pool.draw(&mut rng),
        pool.draw(&mut rng),
    ];
    assert_eq!(rng, advanced(&before, 2000));
    assert_eq!(got.to_vec(), next);
    let mut sorted = got.to_vec();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![10, 20, 30]);
}

#[test]
fn single_item_pool_never_shuffles() {
    let mut pool = Pool::new(&[290]);
    let mut rng = Lcg::new(5);
    let before = rng.clone();
    for _ in 0..5 {
        assert_eq!(pool.draw(&mut rng), 290);
    }
    assert_eq!(rng, before);
}

#[test]
fn pools_start_in_exe_order() {
    let p = SoundPools::new();
    assert_eq!(p.items(P_420240), &[13, 14, 15]);
    assert_eq!(p.items(PA_421B80[4]), &[456, 457, 458]);
    assert_eq!(PA_421B80[4], P_421D30);
    assert_eq!(p.items(PA_420320[3]), &[232, 233, 234, 235]);
}

fn voice_ids(log: &[AudioCmd]) -> Vec<SoundId> {
    log.iter()
        .filter_map(|c| match c {
            AudioCmd::QueueVoice(id) => Some(*id),
            AudioCmd::Play {
                id,
                channel: Channel::Voice,
                ..
            } => Some(*id),
            _ => None,
        })
        .collect()
}

#[test]
fn event_0x33_variant_probabilities() {
    let mut host = RecHost::default();
    let mut ctx = Ctx::new(&mut host, 20261126);
    let trials = 100_000;
    let mut counts = [0u32; 3];
    for _ in 0..trials {
        sound_event(&mut ctx, 0x33, 0);
    }
    drop(ctx);
    let ids = voice_ids(&host.log);
    assert_eq!(ids.len(), trials);
    for id in ids {
        assert_eq!((id.prefix, id.suffix), (Prefix::Char, None));
        counts[usize::from(id.number - 236)] += 1;
    }
    let pct = |n: u32| f64::from(n) * 100.0 / trials as f64;
    for (v, want) in [(0, 90.25), (1, 4.75), (2, 5.0)] {
        let got = pct(counts[v]);
        assert!((got - want).abs() < 0.5, "v={v}: {got}% vs {want}%");
    }
}

#[test]
fn event_0x33_later_slots_reuse_the_variant() {
    let mut host = RecHost::default();
    let mut ctx = Ctx::new(&mut host, 1);
    for _ in 0..200 {
        let before = ctx.rng.clone();
        sound_event(&mut ctx, 0x33, 0);
        let rolls = if advanced(&before, 1) == ctx.rng {
            1
        } else {
            2
        };
        assert_eq!(advanced(&before, rolls), ctx.rng);
        let before = ctx.rng.clone();
        sound_event(&mut ctx, 0x33, 1);
        sound_event(&mut ctx, 0x33, 2);
        assert_eq!(ctx.rng, before, "slots B and C roll nothing");
    }
    drop(ctx);
    let ids: Vec<u16> = voice_ids(&host.log).iter().map(|i| i.number).collect();
    for t in ids.chunks(3) {
        let v = t[0] - 236;
        assert_eq!(t[1], 239 + v);
        assert_eq!(t[2], 242 + v);
    }
}

#[test]
fn event_0x31_extra_line_ten_percent() {
    let mut host = RecHost::default();
    let mut ctx = Ctx::new(&mut host, 7);
    let trials = 20_000;
    for _ in 0..trials {
        sound_event(&mut ctx, 0x31, 1103);
    }
    drop(ctx);
    let q = SoundId::with_suffix(Prefix::Char, 1103, 'q');
    let plays = host
        .log
        .iter()
        .filter(|c| matches!(c, AudioCmd::Play { id, channel: Channel::Voice, .. } if *id == q))
        .count();
    let queued = host
        .log
        .iter()
        .filter(|c| matches!(c, AudioCmd::QueueVoice(id) if *id == q))
        .count();
    assert_eq!(plays + queued, trials);
    let pct = queued as f64 * 100.0 / trials as f64;
    assert!((pct - 10.0).abs() < 0.7, "{pct}%");
    // Each extra line comes from pool 0x420ac0 right after the question.
    let extra = voice_ids(&host.log)
        .windows(2)
        .filter(|w| w[0] == q && w[1] != q)
        .map(|w| w[1].number)
        .collect::<BTreeSet<_>>();
    assert_eq!(extra, [248, 249, 250, 251, 252].into_iter().collect());
}

#[test]
fn name_pool_follows_player_count() {
    let mut host = RecHost::default();
    let mut ctx = Ctx::new(&mut host, 3);
    sound_event(&mut ctx, 0x01, 0); // 1 player
    sound_event(&mut ctx, 0x5d, 2);
    sound_event(&mut ctx, 0x01, 2); // 3 players
    sound_event(&mut ctx, 0x5d, 2);
    drop(ctx);
    let ids: Vec<u16> = voice_ids(&host.log).iter().map(|i| i.number).collect();
    assert_eq!(ids.len(), 4);
    assert!((445..=451).contains(&ids[0]));
    assert_eq!(ids[1], 405);
    assert!((228..=231).contains(&ids[2]));
    assert_eq!(ids[3], 405);
}

#[test]
fn music_and_loops() {
    let mut host = RecHost::default();
    let mut ctx = Ctx::new(&mut host, 3);
    sound_event(&mut ctx, 0x03, 0);
    sound_event3(&mut ctx, 0x27, 0, 2, 0);
    sound_event(&mut ctx, 0x27, 1);
    sound_event(&mut ctx, 0x27, 1);
    drop(ctx);
    assert_eq!(
        host.log,
        vec![
            AudioCmd::Play {
                id: SoundId::new(Prefix::Musi, 10),
                channel: Channel::Music,
                looped: true
            },
            AudioCmd::Play {
                id: SoundId::new(Prefix::Sfex, 4188),
                channel: Channel::Sfx,
                looped: true
            },
            AudioCmd::StopSound(SoundId::new(Prefix::Sfex, 4188)),
        ]
    );
}

// ---- corpus ----------------------------------------------------------------------------

fn data_root() -> Option<String> {
    match std::env::var("MILIONAR_DATA") {
        Ok(r) => Some(r),
        Err(_) => {
            println!("skipped: MILIONAR_DATA not set");
            None
        }
    }
}

/// Every argument combination each event accepts in the original's call sites.
fn arg_sets(event: u8) -> Vec<(i32, i32, i32)> {
    let r = |n: i32| 0..n;
    let one = |v: &[i32]| v.iter().map(|&a| (a, 0, 0)).collect::<Vec<_>>();
    let mut out = Vec::new();
    match event {
        0x01 => out = one(&[0, 1, 2, 3]),
        0x09 => out = one(&[50, 100]),
        0x0c | 0x0d | 0x44 | 0x45 | 0x52 => out = r(5).map(|a| (a, 0, 0)).collect(),
        0x1c => out = r(10).map(|a| (a, 0, 0)).collect(),
        0x1d => out = r(3).map(|a| (a, 0, 0)).collect(),
        0x26 | 0x28 | 0x29 | 0x2b => out = one(&[0, 1]),
        0x27 => {
            out = r(3).map(|b| (0, b, 0)).collect();
            out.push((1, 0, 0));
        }
        0x2d => {
            for a in [1, 2, 5] {
                for b in r(4) {
                    out.push((a, b, 0));
                }
            }
        }
        0x2e => out = one(&[2, 3]),
        0x33 | 0x34 | 0x35 | 0x36 | 0x3d | 0x3e | 0x42 | 0x43 | 0x5a | 0x5d | 0x60 => {
            out = r(4).map(|a| (a, 0, 0)).collect()
        }
        0x37 | 0x5b | 0x61 | 0x68 => {
            for a in r(4) {
                for b in r(4) {
                    out.push((a, b, 0));
                }
            }
        }
        0x38 | 0x3a | 0x69 => {
            for a in r(4) {
                for b in r(4) {
                    for c in r(4) {
                        out.push((a, b, c));
                    }
                }
            }
        }
        0x3f | 0x4f => {
            for a in r(4) {
                for b in r(2) {
                    out.push((a, b, 0));
                }
            }
        }
        0x40 => out = (1..=20).map(|a| (a, 0, 0)).collect(),
        0x51 => out = one(&[0, 2, 3, 14, 15, 19, 20, 30]),
        0x55 => out = r(15).map(|a| (a, 0, 0)).collect(),
        0x56 => out = r(9).map(|a| (a, 0, 0)).collect(),
        0x57 => out = one(&[1, 2, 3, 4]),
        // 0x31 is driven by the question voice ids below; 0x32 (`/quesonly` only) asks for
        // CHAR{id}a..d clips that do not exist (F7.2, F11 item 13).
        0x31 | 0x32 => {}
        _ => out.push((0, 0, 0)),
    }
    out
}

fn emitted(log: &[(u64, AudioCmd)]) -> BTreeSet<SoundId> {
    log.iter()
        .filter_map(|(_, c)| match c {
            AudioCmd::Play { id, .. } | AudioCmd::QueueVoice(id) => Some(*id),
            _ => None,
        })
        .collect()
}

fn settle(ctx: &mut Ctx) {
    for _ in 0..16 {
        if !ctx.voice_busy() {
            break;
        }
        ctx.present().unwrap();
        ctx.pump_voice();
    }
}

fn question_voice_ids(root: &str) -> Vec<u16> {
    let data = GameData::open(Path::new(root)).unwrap();
    let mut ids = Vec::new();
    for n in 1..=3 {
        let bytes = data.read(&format!("Db/Mill_{n}.dat")).unwrap();
        for q in formats::questions::decode_question_db(&bytes).unwrap() {
            ids.push(q.voice_id);
        }
    }
    ids
}

#[test]
fn every_event_resolves_existing_sounds() {
    let Some(root) = data_root() else { return };
    let mut host = HeadlessHost::with_script(vec![]);
    let mut ctx = Ctx::new(&mut host, 4242);
    for players in [0, 1, 3] {
        sound_event(&mut ctx, 0x01, players);
        for event in 0u8..=0x6f {
            let reps = if event == 0x33 { 400 } else { 60 };
            for (a, b, c) in arg_sets(event) {
                for _ in 0..reps {
                    if event == 0x35 {
                        sound_event(&mut ctx, 0x01, players); // re-arm the once-per-game line
                    }
                    sound_event3(&mut ctx, event, a, b, c);
                    settle(&mut ctx);
                }
            }
        }
        sound_event(&mut ctx, 0x01, players);
    }
    let qids = question_voice_ids(&root);
    for &id in &qids {
        for _ in 0..3 {
            sound_event(&mut ctx, 0x31, i32::from(id));
            settle(&mut ctx);
        }
    }
    drop(ctx);
    let ids = emitted(host.audio_log());

    // Every pool item was emitted (so the run above reached every pool entirely) ...
    let numbers: HashSet<u16> = ids.iter().map(|i| i.number).collect();
    for (p, items) in POOL_ITEMS.iter().enumerate() {
        for n in *items {
            assert!(
                numbers.contains(n),
                "pool 0x{:06x}: {n} never emitted",
                POOL_VA[p]
            );
        }
    }
    // ... and every emitted id exists in the archives. Reopen the assets in chunks so the
    // decoded PCM cache stays small.
    let mut missing = Vec::new();
    for chunk in ids.iter().collect::<Vec<_>>().chunks(100) {
        let assets = Assets::new(GameData::open(Path::new(&root)).unwrap());
        for id in chunk {
            if assets.sound(**id).is_err() {
                missing.push(id.to_string());
            }
        }
    }
    // The shipped archives lack these five (F7.4); the original's load fails and nothing
    // plays, as with the port's silent placeholder.
    assert_eq!(
        missing,
        ["SFEX4119", "CHAR0431", "CHAR0444", "CHAR0463", "CHAR0491"],
        "missing sounds"
    );
    assert!(ids.len() > 600, "{} ids", ids.len());
    assert!(qids.len() > 700);
}
